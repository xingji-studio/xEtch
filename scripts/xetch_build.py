#!/usr/bin/env python3
"""xEtch build/package driver. Uses only Python's standard library."""
import sys
if sys.version_info < (3, 12):
    raise SystemExit("Python 3.12+ is required for building and packaging xEtch.")
import argparse
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parents[1]
PLATFORMS = {"windows": "windows", "win32": "windows", "linux": "linux", "darwin": "macos", "macos": "macos"}
ARCHES = {"amd64": "x64", "x86_64": "x64", "x64": "x64", "aarch64": "arm64", "arm64": "arm64"}


def host_platform():
    return PLATFORMS.get(sys.platform, sys.platform)


def host_arch():
    return ARCHES.get(platform.machine().lower(), platform.machine().lower())


def run(argv, *, capture=False, env=None):
    argv = list(map(str, argv))
    result = subprocess.run(argv, cwd=ROOT, env=env, text=True, encoding="utf-8", errors="replace",
                            stdout=subprocess.PIPE if capture else None,
                            stderr=subprocess.PIPE if capture else None)
    if result.returncode:
        detail = (result.stdout + result.stderr).strip() if capture else "see output above"
        raise RuntimeError(f"{Path(argv[0]).name} failed ({result.returncode}): {detail}")
    return result.stdout.strip() if capture else ""


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def windows_path(path):
    return run(["wslpath", "-w", Path(path).resolve()], capture=True) if os.name != "nt" else str(Path(path).resolve())


def powershell_executable():
    fallback = Path(os.environ.get("SystemRoot", "C:/Windows")) / "System32/WindowsPowerShell/v1.0/powershell.exe" if os.name == "nt" else Path("/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe")
    return shutil.which("powershell.exe") or str(fallback)


def powershell(script, *arguments):
    exe = powershell_executable()
    if not Path(exe).is_file():
        raise RuntimeError("Windows builds need a Windows host, or WSL with Windows interoperability enabled.")
    return run([exe, "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", windows_path(ROOT / "scripts" / script), *arguments])


@dataclass
class Config:
    platform: str
    arch: str
    components: str
    profile: str
    ntfs: str
    offline: bool
    ext4: str = "auto"

    @property
    def windows(self):
        return self.platform == "windows"

    @property
    def gui(self):
        return self.components in ("frontend", "all")

    @property
    def backend(self):
        return self.components in ("backend", "all")

    @property
    def directory(self):
        base = Path(os.environ.get("XETCH_BUILD_DIR", ROOT / "target/xetch"))
        return base.resolve() / f"{self.platform}-{self.arch}" / self.profile

    @property
    def exe(self):
        return "xetch.exe" if self.windows else "xetch"


def validate(config):
    if config.components not in ("backend", "frontend", "all") or config.profile not in ("release", "debug") or config.ntfs not in ("auto", "on", "off") or config.ext4 not in ("auto", "on", "off"):
        raise RuntimeError("Invalid XETCH_COMPONENTS, XETCH_PROFILE, XETCH_NTFS or XETCH_EXT4; see ./xetch --help.")
    if config.platform not in ("windows", "linux", "macos") or config.arch not in ("x64", "arm64"):
        raise RuntimeError("Supported platforms: windows/linux/macos; architectures: x64/arm64.")
    if config.gui and (not config.windows or config.arch != "x64"):
        raise RuntimeError("Only the Windows x64 frontend exists. Use XETCH_COMPONENTS=backend for Linux/macOS or arm64.")
    if config.platform != host_platform() and not (config.windows and host_platform() == "linux" and shutil.which("wslpath")):
        raise RuntimeError(f"Build {config.platform} on that OS. Windows builds also work from WSL; this is not a universal cross compiler.")
    if config.arch != host_arch():
        raise RuntimeError("Build on a machine matching XETCH_ARCH; arbitrary architecture cross compilation is not configured.")


def cargo_target(config):
    override = os.environ.get("XETCH_CARGO_TARGET_DIR") or (os.environ.get("CARGO_TARGET_DIR") if not config.windows or os.name == "nt" else None)
    return Path(override or ROOT / ("target/windows" if config.windows else "target")).resolve()


def windows_build(config, action):
    arguments = ["-Action", action, "-Components", config.components, "-Profile", config.profile,
                 "-Architecture", config.arch, "-OutputDirectory", windows_path(config.directory),
                 "-CargoTargetDirectory", windows_path(cargo_target(config))]
    home = os.environ.get("XETCH_WINDOWS_CARGO_HOME")
    if home:
        arguments += ["-CargoHome", windows_path(home)]
    if os.environ.get("XETCH_CARGO"):
        arguments += ["-CargoExecutable", windows_path(os.environ["XETCH_CARGO"])]
    if config.offline:
        arguments += ["-Offline"]
    powershell("windows_build.ps1", *arguments)


def binary_targets(path):
    with path.open("rb") as stream:
        data = stream.read(4096)
    if data[:4] == b"\x7fELF":
        arch = {62:"x64", 183:"arm64"}.get(int.from_bytes(data[18:20], "little" if data[5] == 1 else "big"))
        return {("linux", arch)}
    if data[:2] == b"MZ":
        at = int.from_bytes(data[60:64], "little")
        with path.open("rb") as stream:
            stream.seek(at); header = stream.read(6)
        return {("windows", {0x8664:"x64", 0xaa64:"arm64"}.get(int.from_bytes(header[4:6], "little")))}
    if data[:4] in (b"\xcf\xfa\xed\xfe", b"\xfe\xed\xfa\xcf"):
        cpu = int.from_bytes(data[4:8], "little" if data[0] == 0xcf else "big")
        return {("macos", {0x1000007:"x64", 0x100000c:"arm64"}.get(cpu))}
    if data[:4] in (b"\xca\xfe\xba\xbe", b"\xca\xfe\xba\xbf"):
        width = 20 if data[3] == 0xbe else 32
        count = min(int.from_bytes(data[4:8], "big"), 64)
        return {("macos", {0x1000007:"x64", 0x100000c:"arm64"}.get(int.from_bytes(data[8+i*width:12+i*width], "big"))) for i in range(count)}
    return set()


def worker_sources_match(directory):
    """Reuse only a worker bound to its source snapshot and recorded hash."""
    profile_file = directory / "ntfs-worker-profile.json"
    try:
        profile = json.loads(profile_file.read_text())
        binary = directory / ("xetch-ntfs-worker.exe" if profile["target"] == "windows" else "xetch-ntfs-worker")
        if profile["worker_sha256"] != digest(binary):
            return False
        source = directory / "ntfs-worker-source"
        archive = source / "artifacts/downloads" / profile["source"].rsplit("/", 1)[-1]
        if digest(archive) != profile["source_sha256"]:
            return False
        for file in (ROOT / "native/ntfs-worker").rglob("*"):
            if file.is_file() and file.read_bytes() != (source / file.relative_to(ROOT)).read_bytes():
                return False
        return (ROOT / "scripts/build_ntfs_worker.py").read_bytes() == (source / "scripts/build_ntfs_worker.py").read_bytes()
    except (OSError, ValueError, KeyError, TypeError):
        return False



def worker_starts(binary, engine):
    try:
        result = subprocess.run([str(binary), "--version"], capture_output=True, timeout=15)
        if result.returncode != 0: return False
        body = json.loads(result.stdout)
        if not isinstance(body, dict): return False
        return body.get("engine") == engine and body.get("protocol") == 1
    except (OSError, ValueError, subprocess.TimeoutExpired):
        return False


def include_worker(config):
    destination = config.directory / "backend"
    name = "xetch-ntfs-worker.exe" if config.windows else "xetch-ntfs-worker"
    if config.ntfs == "off":
        for path in [destination / name, destination / "ntfs-worker-profile.json"]:
            path.unlink(missing_ok=True)
        for path in [destination / "ntfs-worker-source", destination / "ntfs-worker-licenses"]:
            if path.is_dir(): shutil.rmtree(path)
        return False
    candidates = [destination, cargo_target(config) / config.profile,
                  ROOT / ("target/windows/release" if config.windows else "target/release")]
    for directory in candidates:
        if (directory / name).is_file() and worker_sources_match(directory) and (config.platform, config.arch) in binary_targets(directory / name) and worker_starts(directory / name, "ntfs-3g"):
            profile = json.loads((directory / "ntfs-worker-profile.json").read_text())
            if (profile["target"] == "windows") != config.windows:
                continue
            if directory != destination:
                for file in (name, "ntfs-worker-profile.json"):
                    shutil.copy2(directory / file, destination / file)
                for folder in ("ntfs-worker-licenses", "ntfs-worker-source"):
                    shutil.copytree(directory / folder, destination / folder, dirs_exist_ok=True)
            print("[OK] Reused NTFS worker with matching source and SHA-256", flush=True)
            return True
    # Remove an obsolete worker before a rebuild or the read-only fallback.
    for path in (destination / name, destination / "ntfs-worker-profile.json"):
        path.unlink(missing_ok=True)
    for path in (destination / "ntfs-worker-source", destination / "ntfs-worker-licenses"):
        if path.is_dir(): shutil.rmtree(path)
    tools = ["x86_64-w64-mingw32-gcc" if config.windows else os.environ.get("CC", "cc"), "make"]
    can_build = os.name != "nt" and all(shutil.which(tool) for tool in tools) and config.arch == "x64"
    # Native arm64 POSIX workers use the host C compiler too.
    if not config.windows and os.name != "nt":
        can_build = all(shutil.which(tool) for tool in tools)
    archive = ROOT / "artifacts/downloads/ntfs-3g_ntfsprogs-2026.9.28.tgz"
    if can_build and not (config.offline and not archive.is_file()):
        argv = [sys.executable, ROOT / "scripts/build_ntfs_worker.py", "--output", destination]
        if config.windows: argv += ["--target", "windows"]
        run(argv)
        return True
    if config.ntfs == "on":
        raise RuntimeError("NTFS worker needs a matching prebuilt worker/source profile or POSIX C + make (MinGW for Windows). Build it in WSL with scripts/build_ntfs_worker.py --target windows, or set XETCH_NTFS=auto/off.")
    print("[NOTE] Optional NTFS worker unavailable: NTFS will be read-only. Set XETCH_NTFS=on to require it.", flush=True)
    return False



def ext4_sources_match(directory):
    """Bind the BSD worker to its complete pinned source/header/build inputs."""
    try:
        profile = json.loads((directory / "ext4-worker-profile.json").read_text())
        binary = directory / ("xetch-ext4-worker.exe" if profile["target"] == "windows" else "xetch-ext4-worker")
        if profile["profile"] != "bsd-xattr-acl-v1" or profile["worker_sha256"] != digest(binary):
            return False
        expected = {"scripts/build_ext4_worker.py", "vendor/lwext4-origin.json", "vendor/lwext4-xetch.patch"}
        inventory = json.loads((ROOT / "vendor/lwext4-origin.json").read_text())
        actual = {p.relative_to(ROOT / "vendor/lwext4").as_posix() for p in (ROOT / "vendor/lwext4").rglob("*") if p.suffix in (".c", ".h")}
        if actual != {item["path"] for item in inventory["files"]}:
            return False
        expected.update("vendor/lwext4/"+item["path"] for item in inventory["files"])
        expected.update("native/ext4-worker/"+name for name in ("extents.c", "xattrs.c", "acls.c", "worker.c", "LICENSE"))
        recorded = {item["path"]: item["sha256"] for item in profile["source_inputs"]}
        if set(recorded) != expected or any(digest(ROOT / name) != value for name,value in recorded.items()):
            return False
        notices = directory / "ext4-worker-licenses"
        expected_notices = {"xetch-ext4-worker-BSD-3-Clause.txt", "lwext4-file-notices.txt"}
        return set(profile["license_files"]) == expected_notices and all(digest(notices/name)==profile["license_files"][name] for name in expected_notices)
    except (OSError, ValueError, KeyError, TypeError):
        return False


def include_ext4_worker(config):
    destination = config.directory / "backend"
    name = "xetch-ext4-worker.exe" if config.windows else "xetch-ext4-worker"
    def clear():
        for filename in (name,"ext4-worker-profile.json"):
            (destination / filename).unlink(missing_ok=True)
        if (destination / "ext4-worker-licenses").is_dir():
            shutil.rmtree(destination / "ext4-worker-licenses")
    if config.ext4 == "off":
        clear();return False
    candidates = [destination, cargo_target(config) / config.profile,
                  ROOT / ("target/windows/release" if config.windows else "target/release")]
    for folder in candidates:
        if (folder / name).is_file() and ext4_sources_match(folder) and (config.platform,config.arch) in binary_targets(folder/name) and worker_starts(folder/name,"lwext4"):
            if folder != destination:
                clear()
                for filename in (name,"ext4-worker-profile.json"):
                    shutil.copy2(folder/filename,destination/filename)
                shutil.copytree(folder/"ext4-worker-licenses",destination/"ext4-worker-licenses")
            print("[OK] Reused BSD ext4 worker with matching source/header/license hashes",flush=True)
            return True
    clear()
    if config.windows and config.arch != "x64" and not os.environ.get("XETCH_EXT4_CC"):
        if config.ext4 == "on":
            raise RuntimeError("Windows ARM64 ext4 requires a matching prebuilt worker or an explicit XETCH_EXT4_CC; the default MinGW builder targets x64.")
        print("[NOTE] No matching Windows ARM64 BSD ext4 worker; default fstool remains available.",flush=True)
        return False
    compiler = os.environ.get("XETCH_EXT4_CC") or ("x86_64-w64-mingw32-gcc" if config.windows else os.environ.get("CC","cc"))
    if shutil.which(compiler):
        argv=[sys.executable,ROOT/"scripts/build_ext4_worker.py","--output",destination]
        if config.windows:argv += ["--target","windows"]
        run(argv)
        if (config.platform,config.arch) not in binary_targets(destination/name) or not worker_starts(destination/name,"lwext4"):
            clear();raise RuntimeError("ext4 compiler produced an unusable OS/CPU/runtime target; correct XETCH_EXT4_CC.")
        return True
    if config.windows and os.name == "nt" and shutil.which("wsl.exe"):
        found=subprocess.run(["wsl.exe","--exec","which","x86_64-w64-mingw32-gcc"],capture_output=True)
        if found.returncode == 0:
            source=run(["wsl.exe","--exec","wslpath","-u",ROOT],capture=True)
            output=run(["wsl.exe","--exec","wslpath","-u",destination],capture=True)
            run(["wsl.exe","--exec","python3",source+"/scripts/build_ext4_worker.py","--target","windows","--output",output])
            if (config.platform,config.arch) not in binary_targets(destination/name) or not worker_starts(destination/name,"lwext4"):
                clear();raise RuntimeError("WSL ext4 compiler did not produce a runnable target for this OS/CPU.")
            return True
    if config.ext4 == "on":
        raise RuntimeError("BSD ext4 worker requires a matching prebuilt worker or a GNU-compatible C compiler (MinGW for Windows). Build it in WSL with scripts/build_ext4_worker.py --target windows, or use XETCH_EXT4=auto/off.")
    print("[NOTE] Optional BSD ext4 worker unavailable; the existing fstool ext4 engine remains usable.",flush=True)
    return False


def build(config):
    print(f"Building {config.platform}/{config.arch} {config.components} ({config.profile})", flush=True)
    config.directory.mkdir(parents=True, exist_ok=True)
    # Remove the success marker before work, so a failed build cannot be
    # packaged later as a successful incremental result.
    (config.directory / "build.json").unlink(missing_ok=True)
    if config.windows:
        windows_build(config, "build")
    else:
        cargo = os.environ.get("XETCH_CARGO", "cargo")
        argv = [cargo, "build", "--locked", "-p", "xetch-cli", "--target-dir", cargo_target(config)]
        if config.profile == "release": argv += ["--release"]
        if config.offline: argv += ["--offline"]
        run(argv)
        backend = config.directory / "backend"
        backend.mkdir(exist_ok=True)
        shutil.copy2(cargo_target(config) / config.profile / "xetch", backend / "xetch")
    worker = include_worker(config) if config.backend else False
    ext4_worker = include_ext4_worker(config) if config.backend else False
    record = {"format_version": 1, "platform": config.platform, "arch": config.arch, "profile": config.profile,
              "components": config.components, "ntfs_policy": config.ntfs, "ntfs_worker": worker,
              "ext4_policy": config.ext4, "ext4_worker": ext4_worker, "gui_self_contained": config.gui}
    (config.directory / "build.json").write_text(json.dumps(record, indent=2)+"\n")
    print(f"[OK] Build: {config.directory}", flush=True)
    return record


def qemu_sources(config, prepare=False):
    explicit = os.environ.get("XETCH_QEMU_DIR")
    directory = Path(explicit).resolve() if explicit else ROOT / "artifacts/windows/qemu" if config.windows else None
    suffix = ".exe" if config.windows else ""
    if prepare and config.windows and not explicit and not config.offline and not all((directory/(name+suffix)).is_file() for name in ("qemu-img", "qemu-storage-daemon")):
        arguments = ["-Destination", windows_path(directory)]
        if os.environ.get("XETCH_7ZIP"): arguments += ["-SevenZip", windows_path(os.environ["XETCH_7ZIP"])]
        powershell("windows_prepare_qemu.ps1", *arguments)
    found = []
    for name, variable in [("qemu-img", "XETCH_QEMU_IMG"), ("qemu-storage-daemon", "XETCH_QEMU_STORAGE_DAEMON")]:
        value = os.environ.get(variable)
        candidate = Path(value) if value else directory / (name+suffix) if directory else Path(shutil.which(name) or "/missing-qemu")
        if not candidate.is_file():
            hint = "Run scripts/windows_prepare_qemu.ps1 or set XETCH_QEMU_DIR." if config.windows else "Install QEMU (qemu-utils + qemu-system-common, or brew install qemu)."
            raise RuntimeError(f"Missing {name}. {hint}")
        if (config.platform, config.arch) not in binary_targets(candidate):
            raise RuntimeError(f"{candidate} is not a {config.platform}/{config.arch} binary.")
        found.append(candidate.resolve())
    if config.windows and found[0].parent != found[1].parent:
        raise RuntimeError("Windows QEMU tools must come from the same directory with their DLLs.")
    return found


SYSTEM_LIBS = re.compile(r"^(ld-linux|ld-musl|lib(c|m|pthread|dl|rt|resolv|util|anl)\.so)")


def bundle_posix_libraries(binaries, destination, system):
    """Copy non-system dependencies, dereferencing aliases for relocation."""
    copied = set()
    queue = list(binaries)
    while queue:
        binary = queue.pop()
        if system == "linux":
            result = subprocess.run(["ldd", str(binary)], capture_output=True, text=True)
            output = result.stdout + result.stderr
            if result.returncode and not any(text in output for text in ("not a dynamic executable", "statically linked")):
                raise RuntimeError(f"Cannot inspect {binary}: {output}")
            if "not found" in output:
                raise RuntimeError(f"Missing shared libraries for {binary}:\n{output}")
            dependencies = [Path(match) for match in re.findall(r"=>\s+(/.+?)\s+\(0x", output)]
        else:
            output = run(["otool", "-L", binary], capture=True)
            dependencies = [Path(line.strip().split(" (",1)[0]) for line in output.splitlines()[1:]
                            if line.strip().startswith("/")]
        for dependency in dependencies:
            if SYSTEM_LIBS.match(dependency.name) or str(dependency).startswith(("/System/", "/usr/lib/")) and system == "macos":
                continue
            if dependency.name not in copied:
                copied.add(dependency.name)
                destination.mkdir(parents=True, exist_ok=True)
                shutil.copy2(dependency, destination / dependency.name)
                queue.append(dependency)


def copy_qemu(config, staging):
    sources = qemu_sources(config, prepare=True)
    target = staging / "qemu"
    target.mkdir()
    notices = staging / "licenses/qemu"
    notices.mkdir(parents=True)
    for source in sources:
        shutil.copy2(source, target / source.name)
    io = sources[0].parent / ("qemu-io.exe" if config.windows else "qemu-io")
    if io.is_file() and (config.platform, config.arch) in binary_targets(io):
        shutil.copy2(io, target / io.name)
        sources.append(io)
    if config.windows:
        for file in sources[0].parent.glob("*.dll"):
            shutil.copy2(file, target / file.name)
        for file in sources[0].parent.iterdir():
            if file.is_file() and (file.name.upper().startswith(("COPYING", "LICENSE")) or file.name == "xetch-dependency-profile.json"):
                shutil.copy2(file, notices / file.name)
    else:
        modules = []
        for folder in [Path("/usr/lib/qemu"), Path("/usr/lib64/qemu"), *Path("/usr/lib").glob("*-linux-gnu/qemu")]:
            if folder.is_dir():
                for pattern in ("block-qcow2.so", "block-vmdk.so", "crypto-*.so"):
                    for file in folder.glob(pattern):
                        (target / "modules").mkdir(exist_ok=True)
                        shutil.copy2(file, target / "modules" / file.name)
                        modules.append(file)
        bundle_posix_libraries([*sources, *modules], target / "lib", config.platform)
        for source in [Path("/usr/share/doc/qemu-utils/copyright"), Path("/usr/share/common-licenses/GPL-2"), Path("/usr/share/common-licenses/LGPL-2.1")]:
            if source.is_file(): shutil.copy2(source, notices / source.name)
    (notices / "SOURCE.txt").write_text("QEMU upstream: https://www.qemu.org/\nSource: https://download.qemu.org/\nWindows distributor/source: https://qemu.weilnetz.de/w64/\nKeep the included dependency profile and license files with this build.\n")


def ensure_replaceable(destination):
    if not destination.exists(): return
    if destination.is_dir() and not any(destination.iterdir()): return
    marker = destination / "xetch-package.json"
    try:
        record = json.loads(marker.read_text())
        if record["application"] != "xetch" or record["format_version"] != 1:
            raise ValueError("not an xEtch package")
        expected = record["files"]
        for path in destination.rglob("*"):
            relative = path.relative_to(destination).as_posix()
            if path.is_symlink(): raise ValueError(f"symlink: {relative}")
            if path.is_file() and relative != "xetch-package.json":
                if relative not in expected or digest(path) != expected[relative]:
                    raise ValueError(f"unlisted or edited file: {relative}")
            elif path.is_dir() and not any(name.startswith(relative+"/") for name in expected):
                raise ValueError(f"unlisted directory: {relative}")
    except (OSError, ValueError, KeyError, TypeError) as error:
        raise RuntimeError(f"Refusing to replace {destination}: {error}. Choose a new --output directory.") from error


def package(config, record, destination):
    if not isinstance(record, dict):
        raise RuntimeError("Invalid build record; rebuild before packaging.")
    if not config.backend:
        raise RuntimeError("A runnable package requires the backend. Use XETCH_COMPONENTS=all or backend.")
    expected = (config.platform, config.arch, config.profile, config.components, config.ntfs, config.ext4)
    if tuple(record.get(k) for k in ("platform", "arch", "profile", "components", "ntfs_policy", "ext4_policy")) != expected:
        raise RuntimeError("Existing build does not match this selection; omit --no-build.")
    if destination.is_relative_to(config.directory) or config.directory.is_relative_to(destination):
        raise RuntimeError("Choose a package directory outside the build output, such as dist/xetch-<platform>-<arch>.")
    if record.get("ext4_worker") and not ext4_sources_match(config.directory / "backend"):
        raise RuntimeError("BSD ext4 worker/source changed after build; rebuild before packaging.")
    ensure_replaceable(destination)
    destination.parent.mkdir(parents=True, exist_ok=True)
    staging = Path(tempfile.mkdtemp(prefix=".xetch-package-", dir=destination.parent))
    try:
        backend = config.directory / "backend"
        shutil.copytree(backend, staging, dirs_exist_ok=True,
                        ignore=lambda directory,names: {"examples"} if Path(directory)==backend else set())
        if config.gui:
            shutil.copytree(config.directory / "gui", staging / "gui")
            if not all((staging / "gui" / f).is_file() for f in ("xEtch.Windows.exe", "xEtch.Windows.pri", "coreclr.dll", "hostfxr.dll")):
                raise RuntimeError("The GUI must be published self-contained; rebuild without --no-build.")
            if (config.directory / "gui-licenses").is_dir():
                shutil.copytree(config.directory / "gui-licenses", staging / "licenses/windows-gui")
            shutil.copy2(ROOT / "scripts/runtime/start-gui.cmd", staging / "start-gui.cmd")
        copy_qemu(config, staging)
        shutil.copy2(ROOT / "LICENSE", staging / "LICENSE")
        for name in ("fatfs", "fstool"):
            source = next((ROOT / "vendor" / name).glob("LICENSE*"))
            folder = staging / "licenses" / name
            folder.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, folder / source.name)
        for file in (["check-deps.cmd", "check-deps.ps1"] if config.windows else ["check-deps.sh"]):
            shutil.copy2(ROOT / "scripts/runtime" / file, staging / file)
        if not config.windows: (staging / "check-deps.sh").chmod(0o755)
        (staging / "README.txt").write_text("xEtch portable folder\n\nRun check-deps.cmd (Windows) or ./check-deps.sh (Linux/macOS).\nCLI: xetch.exe --help / ./xetch --help\nWindows GUI: double-click xetch.exe, run it without arguments, or use start-gui.cmd.\nBackend protocol: xetch.exe serve --stdio / ./xetch serve --stdio\nLinux/macOS GUI is not included yet; no-argument startup reports a missing frontend.\nKeep the entire folder together. No Python or build tools are required to run it.\nNTFS writes require the optional worker; Linux/macOS still require compatible system C libraries.\nDo not store personal images inside this generated application folder.\n")
        names = sorted(p.relative_to(staging).as_posix() for p in staging.rglob("*") if p.is_file())
        (staging / "runtime-files.txt").write_text("\n".join([*names, "runtime-files.txt", "xetch-package.json"])+"\n")
        version = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["package"]["version"]
        manifest = {**record, "application": "xetch", "version": version,
                    "files": {p.relative_to(staging).as_posix(): digest(p) for p in sorted(staging.rglob("*")) if p.is_file()}}
        (staging / "xetch-package.json").write_text(json.dumps(manifest, indent=2)+"\n")
        print("Checking the staged package without development environment overrides...", flush=True)
        if config.windows:
            exe = powershell_executable()
            run([exe, "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", windows_path(staging / "check-deps.ps1"), "-Isolated"])
        else:
            env = os.environ.copy(); env["XETCH_PACKAGE_CHECK"] = "isolated"
            run(["/bin/sh", staging / "check-deps.sh"], env=env)
        if destination.exists():
            backup = Path(tempfile.mkdtemp(prefix=".xetch-previous-", dir=destination.parent))
            backup.rmdir()
            destination.rename(backup)
            try: staging.rename(destination)
            except OSError:
                backup.rename(destination)
                raise
            shutil.rmtree(backup)
        else:
            staging.rename(destination)
        print(f"[OK] Package: {destination}", flush=True)
        print("Run check-deps.cmd" if config.windows else "Run ./check-deps.sh", flush=True)
    finally:
        if staging.exists(): shutil.rmtree(staging)


def doctor(config):
    print(f"Target: {config.platform}/{config.arch}, {config.components}, {config.profile}", flush=True)
    if config.windows:
        windows_build(config, "doctor")
    else:
        print("[OK] " + run([os.environ.get("XETCH_CARGO", "cargo"), "--version"], capture=True))
    try:
        for file in qemu_sources(config): print(f"[OK] QEMU: {file}")
    except RuntimeError as error:
        print(f"[NOTE] {error} This does not prevent compiling; packaging requires QEMU.")
    print(f"[INFO] NTFS worker policy: {config.ntfs} (optional by default)")
    print(f"[INFO] BSD ext4 worker policy: {config.ext4}; select at runtime with XETCH_EXT4_ENGINE=lwext4")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, epilog="Environment: XETCH_PLATFORM, XETCH_ARCH, XETCH_COMPONENTS, XETCH_PROFILE, XETCH_NTFS=auto|on|off, XETCH_EXT4=auto|on|off, XETCH_OFFLINE=1, XETCH_QEMU_DIR.")
    parser.add_argument("action", choices=["build", "package", "doctor"])
    parser.add_argument("--platform", default=os.environ.get("XETCH_PLATFORM", host_platform()))
    parser.add_argument("--arch", default=os.environ.get("XETCH_ARCH", host_arch()))
    parser.add_argument("--components", choices=["auto", "backend", "frontend", "all"], default=os.environ.get("XETCH_COMPONENTS", "auto"))
    parser.add_argument("--profile", choices=["release", "debug"], default=os.environ.get("XETCH_PROFILE", "release"))
    parser.add_argument("--ntfs", choices=["auto", "on", "off"], default=os.environ.get("XETCH_NTFS", "auto"))
    parser.add_argument("--ext4", choices=["auto", "on", "off"], default=os.environ.get("XETCH_EXT4", "auto"))
    parser.add_argument("--offline", action="store_true", default=os.environ.get("XETCH_OFFLINE") == "1")
    parser.add_argument("--no-build", action="store_true", help="Package a successful previous build with exactly this selection")
    parser.add_argument("--output", type=Path, help="Package folder (default dist/xetch-<platform>-<arch>)")
    args = parser.parse_args(argv)
    if sys.version_info < (3,12): parser.error("Python 3.12+ is required for building and packaging.")
    target = PLATFORMS.get(args.platform.lower(), args.platform.lower())
    arch = ARCHES.get(args.arch.lower(), args.arch.lower())
    components = args.components if args.components != "auto" else "all" if target == "windows" and arch == "x64" else "backend"
    config = Config(target, arch, components, args.profile, args.ntfs, args.offline, args.ext4)
    try:
        validate(config)
        if args.action == "doctor": doctor(config)
        elif args.action == "build": build(config)
        else:
            if not config.backend: raise RuntimeError("A runnable package requires backend; select all or backend.")
            record = json.loads((config.directory / "build.json").read_text()) if args.no_build else build(config)
            base = Path(os.environ.get("XETCH_DIST_DIR", ROOT / "dist"))
            suffix = "" if config.profile == "release" else "-debug"
            destination = (args.output or base / f"xetch-{config.platform}-{config.arch}{suffix}").resolve()
            package(config, record, destination)
        return 0
    except (RuntimeError, OSError, ValueError, KeyError) as error:
        print(f"[ERROR] {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
