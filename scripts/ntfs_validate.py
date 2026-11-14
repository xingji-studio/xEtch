#!/usr/bin/env python3
"""Independent NTFS-3G worker validation on Linux, also driving native Windows .exe from WSL.

All fixtures are disposable, independently formatted by mkntfs; checks use the
system ntfsprogs, not the application's filesystem reader. No volume is mounted.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import struct
import subprocess


def run(*args):
    p = subprocess.run(list(map(str, args)), capture_output=True, timeout=180)
    if p.returncode:
        raise RuntimeError(f"{args}: {p.stdout.decode(errors='replace')}\n{p.stderr.decode(errors='replace')}")
    return p.stdout


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def validate(binary, directory, large_volume=False):
    directory.mkdir(parents=True, exist_ok=False)
    windows = binary.suffix.lower() == ".exe"
    env = os.environ.copy()

    def host(path):
        return run("wslpath", "-w", path).decode().strip() if windows else str(path)

    if windows:
        qemu = Path("artifacts/windows/qemu").resolve()
        env["XETCH_QEMU_IMG"] = host(qemu / "qemu-img.exe")
        env["XETCH_QEMU_STORAGE_DAEMON"] = host(qemu / "qemu-storage-daemon.exe")
        env["WSLENV"] = ":".join(filter(None, [env.get("WSLENV", ""), "XETCH_QEMU_IMG", "XETCH_QEMU_STORAGE_DAEMON", "XETCH_NTFS_WORKER"]))
    checks = []

    def cli(*args, fail=False):
        p = subprocess.run([str(binary), "--json", *(host(a) if isinstance(a, Path) else str(a) for a in args)],
                           env=env, capture_output=True, timeout=180)
        if fail:
            assert p.returncode, args
            return json.loads(p.stderr)["error"]
        if p.returncode:
            raise RuntimeError(f"xetch {args}: {p.stderr.decode(errors='replace')}")
        return json.loads(p.stdout)

    def passed(name):
        checks.append(name)
        print(f"PASS {name}", flush=True)

    assert cli("doctor")["capabilities"]["ntfs_write"]
    image = directory / "独立 NTFS.img"
    with image.open("xb") as stream:
        stream.truncate(64 * 1024 * 1024)
    run("mkntfs", "-F", "-Q", image)
    payload = directory / "payload.bin"
    payload.write_bytes(b"original resident data\n")
    run("ntfscp", image, payload, "/seed.txt")
    ads = b"alternate stream must survive replacement\n"
    payload.write_bytes(ads)
    run("ntfscp", "-N", "xetch-test", image, payload, "/seed.txt")
    name, value = b"xetch", b"extended attribute must survive replacement"
    ea = struct.pack("<IBBH", 0, 0, len(name), len(value)) + name + b"\0" + value
    payload.write_bytes(ea)
    run("ntfscp", "-a", "0xe0", image, payload, "/seed.txt")
    descriptor = run("ntfscat", "-a", "0x50", image, "/seed.txt")
    info = run("ntfsinfo", "-F", "/seed.txt", image).decode()
    inode = re.search(r"Dumping Inode (\d+)", info).group(1)
    run("ntfsfix", "-n", image)
    source_hash = digest(image)
    passed("independent_mkntfs_seed_acl_ads_ea")
    workspace = directory / "会话 workspace"
    cli("workspace", "create", image, "--workspace", workspace)
    manifest = workspace / "workspace.json"

    def mutation(operation, *args, fail=False):
        before = manifest.read_bytes()
        files = set(workspace.iterdir())
        result = cli("file", operation, "--workspace", workspace, *args, fail=fail)
        if fail:
            assert manifest.read_bytes() == before
            assert set(workspace.iterdir()) == files
        return result

    # Resident -> non-resident -> empty -> resident, always retaining the MFT.
    for data in [bytes(range(256)) * 8192, b"", "small 中文 replacement\n".encode()]:
        payload.write_bytes(data)
        mutation("put", "--source", payload, "--destination", "/seed.txt", "--replace")
        readback = directory / f"readback-{len(data)}.bin"
        cli("file", "get", "--workspace", workspace, "--source", "/seed.txt", "--destination", readback)
        assert readback.read_bytes() == data
    mutation("mkdir", "--path", "/资料😀")
    payload.write_bytes(bytes(range(256)) * 2048 + b"unicode name payload\n")
    expected = payload.read_bytes()
    mutation("put", "--source", payload, "--destination", "/资料😀/内容.txt")
    mutation("rename", "--source", "/资料😀/内容.txt", "--destination", "/moved.txt")
    mutation("rename", "--source", "/moved.txt", "--destination", "/资料😀/内容.txt")
    passed("new_files_unicode_non_bmp_mkdir_file_move")
    for op, params in [
        ("put", ("--source", payload, "--destination", "/seed.txt")),
        ("remove", ("--path", "/资料😀")),
        ("rename", ("--source", "/资料😀", "--destination", "/dir-renamed")),
        ("rename", ("--source", "/seed.txt", "--destination", "/资料😀/内容.txt")),
        ("remove", ("--path", "/$Bitmap")),
    ]:
        mutation(op, *params, fail=True)
    passed("conflict_nonempty_directory_system_path_rollback")
    with payload.open("wb") as stream:
        stream.truncate(80 * 1024 * 1024)
    mutation("put", "--source", payload, "--destination", "/too-large.bin", fail=True)
    passed("out_of_space_rollback")
    output = directory / "saved.img"
    cli("apply", "--workspace", workspace, "--output", output)
    run("ntfsfix", "-n", output)
    assert run("ntfscat", output, "/seed.txt") == "small 中文 replacement\n".encode()
    assert run("ntfscat", output, "/资料😀/内容.txt") == expected
    assert run("ntfscat", "-n", "xetch-test", output, "/seed.txt") == ads
    assert run("ntfscat", "-a", "0x50", output, "/seed.txt") == descriptor
    assert run("ntfscat", "-a", "0xe0", output, "/seed.txt") == ea
    assert re.search(rb"Dumping Inode (\d+)", run("ntfsinfo", "-F", "/seed.txt", output)).group(1).decode() == inode
    passed("independent_raw_content_acl_ads_ea_mft_preserved")
    vmdk = directory / "saved.vmdk"
    cli("apply", "--workspace", workspace, "--output", vmdk)
    readback = directory / "vmdk-read.bin"
    cli("file", "get", vmdk, "--source", "/资料😀/内容.txt", "--destination", readback)
    assert readback.read_bytes() == expected
    passed("vmdk_export_readback")
    # Exercise an NTFS volume as a VMDK source too, not just as an output.
    vmdk_session = directory / "vmdk-session"
    cli("workspace", "create", vmdk, "--workspace", vmdk_session)
    cli("file", "mkdir", "--workspace", vmdk_session, "--path", "/from-vmdk")
    cli("apply", "--workspace", vmdk_session, "--output", directory / "from-vmdk.img")
    run("ntfsfix", "-n", directory / "from-vmdk.img")
    passed("vmdk_source_edit_independent_check")
    mutation("remove", "--path", "/资料😀/内容.txt")
    mutation("remove", "--path", "/资料😀")
    cli("apply", "--workspace", workspace, "--output", directory / "after-delete.img")
    run("ntfsfix", "-n", directory / "after-delete.img")
    assert digest(image) == source_hash
    passed("file_and_empty_directory_delete_source_unchanged")
    # Simulate a worker dying AFTER a successful block write, not just failing
    # preflight. The source and active checkpoint must remain untouched.
    compiler = "x86_64-w64-mingw32-gcc" if windows else "cc"
    fault = directory / ("fault-worker.exe" if windows else "fault-worker")
    run(compiler, Path(__file__).resolve().parents[1] / "native/ntfs-worker/fail_after_write_test.c", "-o", fault)
    old_worker = env.get("XETCH_NTFS_WORKER")
    env["XETCH_NTFS_WORKER"] = host(fault)
    error = mutation("mkdir", "--path", "/must-rollback-sector", fail=True)
    assert error["code"] == "dependency_failure", error
    if old_worker is None:
        del env["XETCH_NTFS_WORKER"]
    else:
        env["XETCH_NTFS_WORKER"] = old_worker
    cli("apply", "--workspace", workspace, "--output", directory / "after-crash.img")
    run("ntfsfix", "-n", directory / "after-crash.img")
    assert digest(directory / "after-crash.img") == digest(directory / "after-delete.img")
    passed("worker_exit_after_sector_write_checkpoint_rollback")
    env["XETCH_NTFS_WORKER"] = host(directory / "missing-worker")
    assert not cli("doctor")["capabilities"]["ntfs_write"]
    assert mutation("mkdir", "--path", "/no-worker", fail=True)["code"] == "unsupported"
    if old_worker is None:
        del env["XETCH_NTFS_WORKER"]
    else:
        env["XETCH_NTFS_WORKER"] = old_worker
    passed("missing_worker_falls_back_to_read_only")
    for kind in ["dirty", "hibernated", "invalid-log"]:
        unsafe = directory / f"{kind}.img"
        shutil.copyfile(image, unsafe)
        if kind == "dirty":
            # ntfsfix without -d deliberately leaves the dirty bit set.
            run("ntfsfix", unsafe)
        elif kind == "hibernated":
            payload.write_bytes(b"hibr" + bytes(4092))
            run("ntfscp", unsafe, payload, "/hiberfil.sys")
        else:
            # Independently replace only this disposable fixture's log with
            # invalid restart records. The product must never force recovery.
            payload.write_bytes(b"X" * (4 * 1024 * 1024))
            run("ntfscp", unsafe, payload, "/$LogFile")
        before = digest(unsafe)
        ws = directory / f"{kind}-session"
        cli("workspace", "create", unsafe, "--workspace", ws)
        prior = (ws / "workspace.json").read_bytes()
        error = cli("file", "mkdir", "--workspace", ws, "--path", "/must-not-exist", fail=True)
        assert error["code"] == "conflict", error
        assert (ws / "workspace.json").read_bytes() == prior
        assert digest(unsafe) == before
        passed(f"{kind}_volume_refused_without_recovery")
    if large_volume:
        large = directory / "large-5GiB.img"
        with large.open("xb") as stream:
            stream.truncate(5 * 1024**3)
        run("mkntfs", "-F", "-Q", large)
        large_workspace = directory / "large-session"
        cli("workspace", "create", large, "--workspace", large_workspace)
        cli("file", "mkdir", "--workspace", large_workspace, "--path", "/large-volume")
        large_saved = directory / "large-saved.img"
        cli("apply", "--workspace", large_workspace, "--output", large_saved)
        run("ntfsfix", "-n", large_saved)
        assert b"DIRECTORY" in run("ntfsinfo", "-F", "/large-volume", large_saved)
        passed("five_gib_ntfs_volume_64_bit_capacity_and_mft_mirror")
    return {"schema_version": 1, "status": "pass", "platform": "native Windows x86_64" if windows else "Linux",
            "engine": "NTFS-3G 2026.9.28", "checks": checks, "source_sha256": source_hash,
            "limits": ["offline clean NTFS only", "no directory rename or compressed/EFS/reparse writes", "ntfsfix -n is not Windows chkdsk"]}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/release/xetch"))
    parser.add_argument("--directory", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--large-volume", action="store_true")
    args = parser.parse_args()
    report = validate(args.binary.resolve(), args.directory.resolve(), args.large_volume)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n")
