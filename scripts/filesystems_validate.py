#!/usr/bin/env python3
"""Linux interoperability checks, with independent filesystem formatters/checkers.

The generated fixtures can be passed to windows_validate.ps1 for native tests.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess


def run(*args):
    result = subprocess.run(list(map(str, args)), capture_output=True, timeout=180)
    if result.returncode:
        raise RuntimeError(f"{args}: {result.stdout.decode(errors='replace')}\n{result.stderr.decode(errors='replace')}")
    return result.stdout


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def validate(binary, fixtures, results_dir, mkfs_exfat, fsck_exfat):
    fixtures.mkdir(parents=True, exist_ok=True)
    results_dir.mkdir(parents=True, exist_ok=True)
    seed = fixtures / "seed.txt"
    seed.write_bytes("Independent filesystem fixture / 中文\n".encode())
    checks = []

    def cli(*args, fail=False):
        result = subprocess.run([str(binary), "--json", *map(str, args)], capture_output=True, timeout=180)
        if fail:
            assert result.returncode, args
            return json.loads(result.stderr)["error"]
        if result.returncode:
            raise RuntimeError(f"xetch {args}: {result.stderr.decode(errors='replace')}")
        return json.loads(result.stdout)

    for fs in ["exfat", "ext4", "ntfs"]:
        image = fixtures / f"{fs}.img"
        if image.exists():
            raise RuntimeError(f"Refusing to overwrite fixture {image}")
        with image.open("xb") as stream:
            stream.truncate(64 * 1024 * 1024)
        if fs == "ext4":
            run("mkfs.ext4", "-q", "-F", "-E", "lazy_itable_init=0,lazy_journal_init=0", image)
            run("debugfs", "-w", "-R", f"write {seed} /seed.txt", image)
            run("e2fsck", "-fn", image)
        elif fs == "ntfs":
            run("mkntfs", "-F", "-Q", image)
            run("ntfscp", "-f", image, seed, "/seed.txt")
            run("ntfsfix", "-n", image)
        else:
            run(mkfs_exfat, image)
            run(fsck_exfat, "-n", image)
        before = digest(image)
        cli("inspect", image)
        cli("file", "list", image)
        if fs != "exfat":
            target = results_dir / f"{fs}-seed.txt"
            cli("file", "get", image, "--source", "/seed.txt", "--destination", target)
            assert target.read_bytes() == seed.read_bytes()
        workspace = results_dir / f"{fs}-session"
        cli("workspace", "create", image, "--workspace", workspace)
        original = (workspace / "workspace.json").read_bytes()
        if fs != "ntfs" or not cli("doctor")["capabilities"]["ntfs_write"]:
            error = cli("file", "mkdir", "--workspace", workspace, "--path", "/new-dir", fail=True)
            assert error["code"] == "unsupported"
            assert original == (workspace / "workspace.json").read_bytes()
        if fs == "ntfs" and not cli("doctor")["capabilities"]["ntfs_write"]:
            assert digest(image) == before
            checks.append({"filesystem": fs, "status": "pass", "read_only": True, "write_blocker": "optional NTFS-3G worker not installed"})
            print("PASS ntfs read-only fallback", flush=True)
            continue
        opt_in = [] if fs == "ntfs" else ["--experimental-filesystems"]
        cli(*opt_in, "file", "mkdir", "--workspace", workspace, "--path", "/new-dir")
        payload = results_dir / f"{fs}-payload.bin"
        payload.write_bytes(bytes(range(256)) * 512 + "中文".encode())
        cli(*opt_in, "file", "put", "--workspace", workspace, "--source", payload, "--destination", "/new-dir/data.bin")
        cli("file", "get", "--workspace", workspace, "--source", "/new-dir/data.bin", "--destination", results_dir / f"{fs}-read.bin")
        assert (results_dir / f"{fs}-read.bin").read_bytes() == payload.read_bytes()
        # Exercise replacement without unlink/recreate, including resident/non-resident transitions.
        payload.write_bytes(b"short replacement\n")
        cli(*opt_in, "file", "put", "--workspace", workspace, "--source", payload, "--destination", "/new-dir/data.bin", "--replace")
        manifest_before = (workspace / "workspace.json").read_bytes()
        cli(*opt_in, "file", "remove", "--workspace", workspace, "--path", "/new-dir", fail=True)
        assert manifest_before == (workspace / "workspace.json").read_bytes()
        output = results_dir / f"{fs}-saved.img"
        cli("apply", "--workspace", workspace, "--output", output)
        if fs == "ext4":
            run("e2fsck", "-fn", output)
            assert run("debugfs", "-R", "cat /new-dir/data.bin", output) == payload.read_bytes()
        elif fs == "ntfs":
            run("ntfsfix", "-n", output)
            assert run("ntfscat", output, "/new-dir/data.bin") == payload.read_bytes()
        else:
            run(fsck_exfat, "-n", output)
        saved_vmdk = results_dir / f"{fs}-saved.vmdk"
        cli("apply", "--workspace", workspace, "--output", saved_vmdk)
        vmdk_read = results_dir / f"{fs}-vmdk-read.bin"
        cli("file", "get", saved_vmdk, "--source", "/new-dir/data.bin", "--destination", vmdk_read)
        assert vmdk_read.read_bytes() == payload.read_bytes()
        cli(*opt_in, "file", "remove", "--workspace", workspace, "--path", "/new-dir/data.bin")
        cli(*opt_in, "file", "remove", "--workspace", workspace, "--path", "/new-dir")
        assert digest(image) == before
        checks.append({"filesystem": fs, "status": "pass", "experimental_write": fs != "ntfs", "checks": ["independent_format", "native_read", "mkdir_put_replace_remove", "failed_operation_rollback", "independent_consistency_check", "vmdk_export_readback", "source_unchanged"]})
        print(f"PASS {fs}", flush=True)
    exfat = results_dir / "created-exfat.img"
    cli("--experimental-filesystems", "create", "--output", exfat, "--size", "64MiB", "--table", "none", "--filesystem", "exfat")
    run(fsck_exfat, "-n", exfat)
    return {"status": "pass", "platform": "linux", "filesystems": checks, "exfat_formatter_independent_check": "pass"}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/release/xetch"))
    parser.add_argument("--fixtures", type=Path, required=True)
    parser.add_argument("--results-dir", type=Path, required=True)
    parser.add_argument("--mkfs-exfat", default=os.environ.get("XETCH_MKFS_EXFAT", "mkfs.exfat"))
    parser.add_argument("--fsck-exfat", default=os.environ.get("XETCH_FSCK_EXFAT", "fsck.exfat"))
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    result = validate(args.binary.resolve(), args.fixtures.resolve(), args.results_dir.resolve(), args.mkfs_exfat, args.fsck_exfat)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2)+"\n", encoding="utf-8")
