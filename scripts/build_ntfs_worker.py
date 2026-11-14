#!/usr/bin/env python3
"""Build the optional GPL NTFS-3G worker on Linux/macOS; cross-build Windows with MinGW.

Only the upstream static library and this separate executable use GPL code.
No FUSE driver, privileged mount, system install or NTFS recovery is involved.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import shlex
import subprocess
import tarfile
import urllib.request

VERSION = "2026.9.28"
SHA256 = "350d9415c59f7c3fa74e23985ad2d56423c6a168337368f672e2e362d8f295c8"
URL = f"https://www.tuxera.com/opensource/ntfs-3g_ntfsprogs-{VERSION}.tgz"
ROOT = Path(__file__).resolve().parents[1]


def run(args, cwd, log):
    with log.open("wb") as output:
        result = subprocess.run(list(map(str, args)), cwd=cwd, stdout=output, stderr=subprocess.STDOUT)
    if result.returncode:
        raise RuntimeError(f"Build failed: {args}; see {log}\n{log.read_text(errors='replace')[-6000:]}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", choices=["native", "windows"], default="native")
    parser.add_argument("--output", type=Path)
    parser.add_argument("--jobs", type=int, default=min(os.cpu_count() or 2, 8))
    args = parser.parse_args()
    windows = args.target == "windows"
    output = (args.output or ROOT / "target" / ("windows/release" if windows else "release")).resolve()
    archive = ROOT / "artifacts/downloads" / URL.rsplit("/", 1)[1]
    archive.parent.mkdir(parents=True, exist_ok=True)
    if not archive.exists():
        temporary = archive.with_suffix(".download")
        urllib.request.urlretrieve(URL, temporary)
        if hashlib.sha256(temporary.read_bytes()).hexdigest() != SHA256:
            raise RuntimeError("NTFS-3G archive checksum mismatch")
        temporary.replace(archive)
    if hashlib.sha256(archive.read_bytes()).hexdigest() != SHA256:
        raise RuntimeError("NTFS-3G archive checksum mismatch")
    source = ROOT / "artifacts/native-src" / f"ntfs-3g-{VERSION}"
    if not source.exists():
        with tarfile.open(archive) as tar:
            tar.extractall(source.parent, filter="data")
    # Upstream's legacy WINDOWS block removes every GCC attribute, including
    # transparent_union. Modern MinGW needs these attributes just like GCC.
    compat = source / "include/ntfs-3g/compat.h"
    text = compat.read_text()
    old = "#define __attribute__(X)\t/*nothing*/"
    if "#ifndef __GNUC__\n" + old not in text:
        if old not in text:
            raise RuntimeError("Unexpected upstream compat.h; refuse an unverified patch")
        compat.write_text(text.replace(old, "#ifndef __GNUC__\n" + old + "\n#endif"))
    build = ROOT / "artifacts" / ("ntfs3g-windows" if windows else "ntfs3g-linux")
    build.mkdir(parents=True, exist_ok=True)
    shims = ROOT / "native/ntfs-worker/windows-compat"
    compiler = "x86_64-w64-mingw32-gcc" if windows else os.environ.get("CC", "cc")
    cflags = ["-O2", "-D_FILE_OFFSET_BITS=64"]
    if windows:
        cflags += ["-D__timespec_defined=1", f"-I{shims}", "-include", str(shims / "compat.h")]
    configure = [source / "configure", "--disable-ntfs-3g", "--disable-ntfsprogs", "--disable-shared",
                 "--enable-static", "--disable-ldconfig", "--disable-posix-acls", "--disable-xattr-mappings",
                 "--disable-plugins", "--disable-device-default-io-ops", "--without-uuid"]
    if windows:
        configure += ["--host=x86_64-w64-mingw32"]
    stamp = build / "xetch-configure.json"
    settings = json.dumps({"configure": list(map(str, configure)), "cflags": cflags})
    if not (build / "Makefile").exists() or not stamp.exists() or stamp.read_text() != settings:
        if (build / "Makefile").exists():
            run(["make", "-C", "libntfs-3g", "clean"], build, build / "clean.log")
        run(configure, build, build / "configure.log")
        stamp.write_text(settings)
    run(["make", f"-j{args.jobs}", "-C", "libntfs-3g", "CFLAGS=" + shlex.join(cflags)], build, build / "build.log")
    output.mkdir(parents=True, exist_ok=True)
    binary = output / ("xetch-ntfs-worker.exe" if windows else "xetch-ntfs-worker")
    command = [compiler, "-std=c11", *cflags, "-DHAVE_CONFIG_H", f"-I{build}",
               f"-I{source / 'include/ntfs-3g'}", ROOT / "native/ntfs-worker/worker.c"]
    if windows:
        command += [shims / "accounts.c", "-static"]
    command += [build / "libntfs-3g/.libs/libntfs-3g.a"]
    if not windows:
        command += ["-lpthread"]
        if os.uname().sysname == "Linux":
            command += ["-ldl"]
    command += ["-o", binary]
    run(command, ROOT, build / "link.log")
    notices = output / "ntfs-worker-licenses"
    notices.mkdir(exist_ok=True)
    shutil.copyfile(source / "COPYING", notices / "NTFS-3G-COPYING")
    shutil.copyfile(source / "COPYING.LIB", notices / "NTFS-3G-COPYING.LIB")
    shutil.copyfile(ROOT / "native/ntfs-worker/README.md", notices / "README.md")
    corresponding = output / "ntfs-worker-source"
    corresponding.mkdir(exist_ok=True)
    (corresponding / "artifacts/downloads").mkdir(parents=True, exist_ok=True)
    (corresponding / "scripts").mkdir(exist_ok=True)
    shutil.copyfile(archive, corresponding / "artifacts/downloads" / archive.name)
    shutil.copytree(ROOT / "native/ntfs-worker", corresponding / "native/ntfs-worker", dirs_exist_ok=True)
    shutil.copyfile(Path(__file__), corresponding / "scripts/build_ntfs_worker.py")
    shutil.copyfile(ROOT / "LICENSE", corresponding / "LICENSE")
    profile = {"engine": "ntfs-3g", "version": VERSION, "protocol": 1, "target": args.target,
               "source": URL, "source_sha256": SHA256,
               "worker_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
               "configure": list(map(str, configure[1:])), "compiler": compiler, "cflags": cflags,
               "adaptations": ["GCC attributes retained on MinGW", "Windows POSIX vocabulary and CRT system RNG; no host SID mapping"] if windows else []}
    (output / "ntfs-worker-profile.json").write_text(json.dumps(profile, indent=2) + "\n")
    print(binary)


if __name__ == "__main__":
    main()
