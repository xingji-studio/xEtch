#!/usr/bin/env python3
"""Real filesystem-driver roundtrips, >4 GiB files and transaction fault recovery.

All inputs and mount requests are restricted to a newly marked test directory.
Kernel checks need sudo or --wsl-distribution; the backend itself runs normally.
"""
import argparse
import csv
from datetime import datetime, timezone
import hashlib
import json
import os
import platform
from pathlib import Path
import shutil
import signal
import subprocess
import struct
import sys
import time
import uuid

from format_validate import Backend

ROOT = Path(__file__).resolve().parents[1]


def sha(path):
    with path.open("rb") as stream:
        digest = hashlib.sha256()
        for data in iter(lambda: stream.read(1 << 20), b""):
            digest.update(data)
        return digest.hexdigest()


class Runner(Backend):
    def cli(self, *args, experimental=True, fail=False, env=None):
        argv = [str(self.binary), "--json", *(["--experimental-filesystems"] if experimental else []),
                *(self.host(a) if isinstance(a, Path) else str(a) for a in args)]
        started = time.monotonic()
        result = subprocess.run(argv, capture_output=True, env=env or self.env, timeout=3600)
        if fail:
            if not result.returncode:
                raise AssertionError(f"Expected failure: {args}")
            return json.loads(result.stderr)["error"]
        if result.returncode:
            raise RuntimeError(f"{args}: {result.stderr.decode(errors='replace')}")
        print(f"  {args[0]} {args[1] if len(args)>1 else ''}: {time.monotonic()-started:.2f}s", flush=True)
        return json.loads(result.stdout)


class Validation:
    def __init__(self, args):
        self.args = args
        self.root = args.directory.resolve()
        self.root.mkdir(parents=True, exist_ok=False)
        self.token = str(uuid.uuid4())
        (self.root / ".xetch-fs-validation.json").write_text(json.dumps({
            "purpose": "xetch-filesystem-validation-v1", "token": self.token}))
        runtime = self.root / "runtime"
        runtime.mkdir()
        frozen = runtime / ("xetch.exe" if args.binary.suffix.lower() == ".exe" else "xetch")
        shutil.copy2(args.binary.resolve(), frozen)
        self.backend = Runner(frozen, args.qemu_directory)
        self.fault_proxy = None
        if args.suite in ("fault", "all"):
            source_proxy = args.fault_proxy or (ROOT/"target/windows/release/examples/qsd_fault_proxy.exe" if self.backend.windows else ROOT/"target/release/examples/qsd_fault_proxy")
            self.fault_proxy = runtime / source_proxy.name
            shutil.copy2(source_proxy.resolve(), self.fault_proxy)
        self.report = {"schema_version": 1, "status": "running",
                       "backend": "native Windows x64" if self.backend.windows else "Linux x64",
                       "backend_sha256": sha(frozen),
                       "started_at": datetime.now(timezone.utc).isoformat(),
                       "validation_host": platform.platform(),
                       "cases": [], "limits": ["Fault injection does not simulate physical power loss."]}
        if self.fault_proxy:
            self.report["fault_proxy_sha256"] = sha(self.fault_proxy)

    def save(self):
        self.args.output.parent.mkdir(parents=True, exist_ok=True)
        self.args.output.write_text(json.dumps(self.report, indent=2)+"\n")

    def kernel(self, filesystem, image, **actions):
        request = {"root": str(self.root), "token": self.token, "filesystem": filesystem,
                   "image": str(image.resolve()), "mkfs_exfat": str(self.args.mkfs_exfat.resolve()), **actions}
        request_path = self.root / ("kernel-request-"+uuid.uuid4().hex+".json")
        request_path.write_text(json.dumps(request))
        helper = ROOT / "scripts/kernel_fs_roundtrip.py"
        if self.args.wsl_distribution:
            command = [str(self.args.wsl), "-d", self.args.wsl_distribution, "-u", "root", "--exec",
                       "python3", str(helper), "--request", str(request_path)]
        elif os.geteuid() == 0:
            command = [sys.executable, str(helper), "--request", str(request_path)]
        else:
            command = ["sudo", "-n", sys.executable, str(helper), "--request", str(request_path)]
        result = subprocess.run(command, capture_output=True, timeout=600)
        if result.returncode:
            raise RuntimeError(result.stdout.decode(errors="replace")+result.stderr.decode(errors="replace"))
        data = json.loads(result.stdout)
        assert data["status"] == "pass"
        return data

    @staticmethod
    def expected(data):
        return {"size": len(data), "sha256": hashlib.sha256(data).hexdigest()}

    def consistency(self, filesystem, image):
        command = [str(self.args.fsck_exfat), "-n", str(image)] if filesystem == "exfat" else ["e2fsck", "-fn", str(image)]
        result = subprocess.run(command, capture_output=True, timeout=180)
        if result.returncode:
            raise RuntimeError(result.stdout.decode(errors="replace")+result.stderr.decode(errors="replace"))

    def system(self, filesystem):
        folder = self.root / (filesystem+"-system")
        folder.mkdir()
        source = folder / "system-source.img"
        source.touch(); source.open("r+b").truncate(128 << 20)
        initial = b"kernel driver seed\n" + bytes(range(256))*4096
        seed = self.kernel(filesystem, source, format=True, write={
            "from-system.bin": initial.hex(), "directory/Unicode-内容.txt": b"system file".hex()})
        self.consistency(filesystem, source)
        original = sha(source)
        readback = folder / "initial.bin"
        self.backend.cli("file", "get", source, "--source", "/from-system.bin", "--destination", readback)
        assert readback.read_bytes() == initial
        workspace = folder / "session"
        self.backend.cli("workspace", "create", source, "--workspace", workspace)
        changed = b"xEtch modification checked by actual kernel\n"+bytes(range(255,-1,-1))*2048
        payload = folder / "payload.bin"; payload.write_bytes(changed)
        self.backend.cli("file", "put", "--workspace", workspace, "--source", payload, "--destination", "/from-system.bin", "--replace")
        self.backend.cli("file", "rename", "--workspace", workspace, "--source", "/directory", "--destination", "/renamed-directory")
        saved = folder / "saved.img"
        self.backend.cli("apply", "--workspace", workspace, "--output", saved)
        self.consistency(filesystem, saved)
        reply = b"second native-driver write after xEtch export\n"
        if filesystem == "ext4":
            recovery = folder / "journal-recovery.img"
            shutil.copyfile(saved, recovery)
            self.kernel(filesystem, recovery, force_replay_source=str(source),
                        expect={"renamed-directory/Unicode-内容.txt": self.expected(b"system file")})
            self.consistency(filesystem, recovery)
        verify = self.kernel(filesystem, saved, expect={"from-system.bin": self.expected(changed),
                           "renamed-directory/Unicode-内容.txt": self.expected(b"system file")},
                           write={"returned-by-driver.txt": reply.hex()})
        self.consistency(filesystem, saved)
        returned = folder / "returned.txt"
        self.backend.cli("file", "get", saved, "--source", "/returned-by-driver.txt", "--destination", returned)
        assert returned.read_bytes() == reply and sha(source) == original
        # Also mount an xEtch-formatted filesystem with the real kernel.
        fresh = folder / "native-created.img"
        self.backend.cli("create", "--output", fresh, "--filesystem", filesystem, "--table", "none", "--size", "128MiB")
        own = self.kernel(filesystem, fresh, write={"kernel-on-native.txt": reply.hex()})
        self.consistency(filesystem, fresh)
        native_read = folder / "native-read.txt"
        self.backend.cli("file", "get", fresh, "--source", "/kernel-on-native.txt", "--destination", native_read)
        assert native_read.read_bytes() == reply
        result = {"filesystem": filesystem, "suite": "linux-kernel-roundtrip", "status": "pass",
                "checks": ["system_format_and_kernel_write", "backend_reads_system_file", "backend_replaces_file_and_moves_directory",
                           "kernel_reads_export_and_writes_again", "backend_reads_returned_file", "kernel_mounts_backend_formatted_image",
                           "independent_fsck_after_each_phase", "source_hash_unchanged"], "driver": seed["driver"]}
        if filesystem == "ext4":
            result["checks"].append("kernel_replays_actual_backend_journal_after_directory_rollback")
        return result

    def large_payload(self):
        if self.args.payload:
            path = self.args.payload.resolve(strict=True)
            if path.stat().st_size <= 0xffffffff:
                raise ValueError("The reusable payload must be larger than 4 GiB")
            if not hasattr(self, "payload_hash"):
                self.payload_hash = sha(path)
            return path, self.payload_hash
        path = self.root / "large-payload.bin"
        if path.exists():
            return path, self.payload_hash
        size = (4 << 30) + (2 << 20) + 137
        if shutil.disk_usage(self.root).free < size * 5 + (6 << 30):
            raise RuntimeError("Large-file tests need at least 27 GiB free for their disposable files.")
        digest = hashlib.sha256()
        block = bytearray(bytes(range(256)) * 4096)
        with path.open("xb") as stream:
            for offset in range(0, size, len(block)):
                struct.pack_into("<Q", block, 0, offset)
                data = memoryview(block)[:min(len(block), size-offset)]
                stream.write(data); digest.update(data)
            stream.flush(); os.fsync(stream.fileno())
        self.payload_hash = digest.hexdigest()
        return path, self.payload_hash

    def large(self, filesystem):
        payload, expected_hash = self.large_payload()
        size = payload.stat().st_size
        folder = self.root / (filesystem+"-large")
        folder.mkdir()
        source = folder / "source.vmdk"
        cli = self.backend.cli
        print(f"START {filesystem}: full {size}-byte file, SHA-256 {expected_hash}", flush=True)
        cli("create", "--output", source, "--size", "6GiB", "--table", "none", "--filesystem", filesystem)
        original = sha(source)
        workspace = folder / "session"
        cli("workspace", "create", source, "--workspace", workspace)
        cli("file", "put", "--workspace", workspace, "--source", payload, "--destination", "/large.bin")
        entries = cli("file", "list", "--workspace", workspace)
        assert any(entry["name"] == "large.bin" and entry["size"] == size for entry in entries["entries"])
        first = folder / "first.vmdk"
        cli("apply", "--workspace", workspace, "--output", first)
        readback = folder / "readback.bin"
        cli("file", "get", first, "--source", "/large.bin", "--destination", readback)
        assert readback.stat().st_size == size and sha(readback) == expected_hash
        readback.unlink()
        # Exercise both 64-bit creation and the separate existing-file writer.
        small = folder / "small.bin"; small.write_bytes(b"truncate a file formerly larger than 4 GiB\n")
        cli("file", "put", "--workspace", workspace, "--source", small, "--destination", "/large.bin", "--replace")
        cli("file", "get", "--workspace", workspace, "--source", "/large.bin", "--destination", readback)
        assert readback.read_bytes() == small.read_bytes()
        readback.unlink()
        cli("file", "put", "--workspace", workspace, "--source", payload, "--destination", "/large.bin", "--replace")
        cli("file", "rename", "--workspace", workspace, "--source", "/large.bin", "--destination", "/moved.bin")
        saved = folder / "saved.img"
        cli("apply", "--workspace", workspace, "--output", saved)
        self.consistency(filesystem, saved)
        reply = b"kernel read every byte of the large exported file\n"
        driver = self.kernel(filesystem, saved,
                             expect={"moved.bin": {"size": size, "sha256": expected_hash}},
                             write={"kernel-large-feedback.txt": reply.hex()})
        self.consistency(filesystem, saved)
        cli("file", "get", saved, "--source", "/moved.bin", "--destination", readback)
        assert readback.stat().st_size == size and sha(readback) == expected_hash
        readback.unlink()
        cli("file", "get", saved, "--source", "/kernel-large-feedback.txt", "--destination", readback)
        assert readback.read_bytes() == reply and sha(source) == original
        result = {"filesystem": filesystem, "suite": "large-file", "status": "pass", "file_size": size,
                  "sha256": expected_hash, "driver": driver["driver"], "checks": [
                      "full_nonzero_payload_above_4gib", "create_and_list_exact_u64_size", "vmdk_full_sha256_readback",
                      "large_to_small_truncation", "small_to_large_existing_inode_overwrite", "rename_large_file",
                      "kernel_full_sha256_read_and_write", "backend_full_sha256_after_kernel_roundtrip",
                      "independent_fsck", "source_hash_unchanged"]}
        if not self.args.keep_artifacts:
            shutil.rmtree(folder)
        return result

    def fault(self, filesystem):
        folder = self.root / (filesystem+"-fault")
        folder.mkdir()
        cli = self.backend.cli
        source = folder / "source.vmdk"
        cli("create", "--output", source, "--size", "128MiB", "--table", "none", "--filesystem", filesystem)
        original = sha(source)
        workspace = folder / "session"
        cli("workspace", "create", source, "--workspace", workspace)
        old = folder / "old.bin"; old.write_bytes(b"last committed file must remain readable\n"*8192)
        cli("file", "put", "--workspace", workspace, "--source", old, "--destination", "/safe.bin")
        sentinel = workspace / "edit-user-content.qcow2"
        sentinel.write_bytes(b"unrelated user data: never sweep by filename pattern")
        payload = folder / "new.bin"
        with payload.open("xb") as stream:
            for _ in range(32): stream.write(bytes(range(256))*4096)
        manifest = workspace / "workspace.json"
        checks = []
        for mode in ["eio", "enospc", "flush", "qsd-kill", "backend-kill"]:
            before = manifest.read_bytes(); state = json.loads(before)
            head = workspace / state["head"]; head_hash = sha(head)
            expected_files = {p.name for p in workspace.iterdir()}
            evidence = folder / (mode+"-process.json")
            env = self.backend.env.copy()
            env["XETCH_TEST_QSD_REAL"] = self.backend.host(self.args.qemu_directory.resolve()/"qemu-storage-daemon.exe") if self.backend.windows else shutil.which("qemu-storage-daemon")
            env["XETCH_TEST_QSD_FAULT"] = "observe" if mode.endswith("kill") else mode
            env["XETCH_TEST_QSD_RECORD"] = self.backend.host(evidence)
            proxy = self.fault_proxy
            env["XETCH_QEMU_STORAGE_DAEMON"] = self.backend.host(proxy.resolve())
            if self.backend.windows and os.name != "nt":
                names = ["XETCH_TEST_QSD_REAL", "XETCH_TEST_QSD_FAULT", "XETCH_TEST_QSD_RECORD"]
                env["WSLENV"] = ":".join(filter(None,[env.get("WSLENV",""), *names]))
            argv = [str(self.backend.binary), "--json", "--experimental-filesystems", "file", "put", "--workspace", self.backend.host(workspace), "--source", self.backend.host(payload), "--destination", "/safe.bin", "--replace"]
            process = subprocess.Popen(argv, env=env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            try:
                if mode.endswith("kill"):
                    deadline = time.monotonic()+60
                    info = None
                    while time.monotonic() < deadline:
                        if process.poll() is not None:
                            raise RuntimeError("Mutation finished before deterministic crash injection")
                        try:
                            info = json.loads(evidence.read_text())
                            private = next(p for p in workspace.glob("edit-*.qcow2") if p.name not in expected_files)
                            if private.stat().st_size > (2 << 20): break
                        except (OSError, ValueError, StopIteration): pass
                        time.sleep(.005)
                    else: raise RuntimeError("No partial child-overlay writes observed before crash injection")
                    target = info["qsd_pid"] if mode == "qsd-kill" else info["parent_pid"]
                    if self.backend.windows:
                        taskkill = str(Path(os.environ["SystemRoot"])/"System32/taskkill.exe") if os.name == "nt" else "/mnt/c/Windows/System32/taskkill.exe"
                        result = subprocess.run([taskkill, "/PID", str(target), "/F"],capture_output=True)
                        if result.returncode: raise RuntimeError(result.stdout.decode(errors="replace"))
                    else:
                        if mode == "backend-kill": assert target == process.pid
                        os.kill(target, signal.SIGKILL)
                stdout, stderr = process.communicate(timeout=90)
                assert process.returncode, (mode,stdout,stderr)
                if mode in ("eio", "enospc", "flush"):
                    message = json.loads(stderr)["error"]
                    assert message["code"] == "io_error" and "NBD error" in message["message"], message
            finally:
                if process.poll() is None: process.kill(); process.communicate()
            info = json.loads(evidence.read_text())
            deadline = time.monotonic()+10
            for pid in (info["proxy_pid"], info["qsd_pid"]):
                while True:
                    if self.backend.windows:
                        tasklist = str(Path(os.environ["SystemRoot"])/"System32/tasklist.exe") if os.name == "nt" else "/mnt/c/Windows/System32/tasklist.exe"
                        result = subprocess.run([tasklist,"/FI",f"PID eq {pid}","/FO","CSV","/NH"],capture_output=True)
                        rows = csv.reader(result.stdout.decode(errors="replace").splitlines())
                        alive = any(len(row)>1 and row[1] == str(pid) for row in rows)
                    else:
                        try: alive = Path(f"/proc/{pid}/stat").read_text().split(") ",1)[1][0] != "Z"
                        except FileNotFoundError: alive = False
                    if not alive: break
                    if time.monotonic() >= deadline: raise AssertionError(f"Orphan helper still running: {pid}")
                    time.sleep(.05)
            assert manifest.read_bytes() == before and sha(head) == head_hash and sha(source) == original
            cli("workspace", "info", "--workspace", workspace)
            assert {p.name for p in workspace.iterdir()} == expected_files
            assert sentinel.read_bytes().startswith(b"unrelated user data")
            readback = folder / (mode+"-readback.bin")
            cli("file", "get", "--workspace", workspace, "--source", "/safe.bin", "--destination", readback)
            assert sha(readback) == sha(old)
            exported = folder / (mode+"-recovered.img")
            cli("apply", "--workspace", workspace, "--output", exported)
            self.consistency(filesystem, exported)
            checks.append(mode+": original/checkpoint/data preserved, helpers stopped, abandoned edit reclaimed, independent fsck")
            print(f"PASS {filesystem} fault {mode}", flush=True)
        # Simulate a crash after atomic manifest rename but before removing
        # the pending marker: the committed layer must never be deleted.
        head_name = json.loads(manifest.read_text())["head"]
        (workspace/".pending-edit.json").write_text(json.dumps({"schema_version":1,"layer":head_name}))
        cli("workspace", "info", "--workspace", workspace)
        assert (workspace/head_name).is_file() and not (workspace/".pending-edit.json").exists()
        cli("file", "mkdir", "--workspace", workspace, "--path", "/after-recovery")
        final = folder/"continued.img"
        cli("apply", "--workspace", workspace, "--output", final)
        self.consistency(filesystem, final)
        assert sha(source) == original
        checks += ["committed_layer_survives_stale_pending_marker", "new_edit_and_export_succeed_after_all_recoveries"]
        return {"filesystem":filesystem,"suite":"fault-recovery","status":"pass","checks":checks}

    def run(self):
        try:
            for filesystem in self.args.filesystems:
                self.report["active"] = filesystem
                if self.args.suite in ("system", "all"):
                    self.report["cases"].append(self.system(filesystem)); self.save()
                if self.args.suite in ("large", "all"):
                    self.report["cases"].append(self.large(filesystem)); self.save()
                if self.args.suite in ("fault", "all"):
                    self.report["cases"].append(self.fault(filesystem)); self.save()
            if not self.report["cases"]:
                raise RuntimeError("No validation cases were executed.")
            self.report.pop("active", None)
            self.report["status"] = "pass"
            if not self.args.keep_artifacts:
                (self.root / "large-payload.bin").unlink(missing_ok=True)
        except Exception as error:
            self.report["status"] = "fail"; self.report["error"] = str(error)
            raise
        finally:
            self.report["finished_at"] = datetime.now(timezone.utc).isoformat()
            self.save()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT/"target/release/xetch")
    parser.add_argument("--directory", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--suite", choices=["system", "large", "fault", "all"], default="system")
    parser.add_argument("--filesystems", nargs="+", choices=["exfat", "ext4"], default=["exfat", "ext4"])
    parser.add_argument("--qemu-directory", type=Path, default=ROOT/"artifacts/windows/qemu")
    parser.add_argument("--mkfs-exfat", type=Path, default=Path(shutil.which("mkfs.exfat") or ROOT/"artifacts/tools/exfatprogs/usr/sbin/mkfs.exfat"))
    parser.add_argument("--fsck-exfat", type=Path, default=Path(shutil.which("fsck.exfat") or ROOT/"artifacts/tools/exfatprogs/usr/sbin/fsck.exfat"))
    parser.add_argument("--wsl-distribution")
    parser.add_argument("--keep-artifacts", action="store_true", help="Keep multi-gigabyte successful test outputs")
    parser.add_argument("--payload", type=Path, help="Reuse a read-only existing >4 GiB payload")
    parser.add_argument("--fault-proxy", type=Path)
    parser.add_argument("--wsl", type=Path, default=Path("/mnt/c/Windows/system32/wsl.exe"))
    Validation(parser.parse_args()).run()
