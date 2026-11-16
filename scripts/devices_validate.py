#!/usr/bin/env python3
"""D0 discovery and D1 tests on newly owned loop/VHD fixtures only. Never accepts a physical disk."""
from __future__ import annotations
import argparse
import contextlib
import errno
import hashlib
import json
import os
from pathlib import Path
import platform
import queue
import shutil
import subprocess
import sys
import threading
import time
import uuid

REPO = Path(__file__).resolve().parents[1]
PURPOSE = "xetch-disposable-device-validation-v1"
WINDOWS = os.name == "nt"
for stream in (sys.stdout, sys.stderr):
    if hasattr(stream, "reconfigure"):
        stream.reconfigure(encoding="utf-8", errors="backslashreplace")


def sha(path, offset=0, length=None):
    h = hashlib.sha256()
    with Path(path).open("rb") as f:
        f.seek(offset)
        while length is None or length > 0:
            b = f.read(1024 * 1024 if length is None else min(1024 * 1024, length))
            if not b:
                if length:
                    raise AssertionError("short hash input")
                break
            h.update(b)
            if length is not None:
                length -= len(b)
    return h.hexdigest()


def loop_backing(path):
    try:
        return path.read_text().strip()
    except OSError as e:
        if e.errno in (errno.ENOENT, errno.ENODEV, errno.ENXIO):
            return ""
        raise


def write_report(path, report):
    temporary = path.with_name(path.name + ".tmp")
    temporary.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    os.replace(temporary, path)


def run(args, *, env=None, fail=False, timeout=60):
    p = subprocess.run([str(x) for x in args], capture_output=True, encoding="utf-8", errors="replace", cwd=REPO, env=env, timeout=timeout)
    if (p.returncode != 0) != fail:
        raise AssertionError(f"unexpected exit {p.returncode}: {args}\n{p.stdout}\n{p.stderr}")
    return p


class Client:
    def __init__(self, factory, extra=None):
        env = dict(factory.env)
        for key in ("XETCH_VALIDATION_POINT", "XETCH_VALIDATION_MODE", "XETCH_VALIDATION_RECORD", "XETCH_DEVICE_TEST_FAULT", "XETCH_DEVICE_TEST_CONFIRM_TTL_MS"):
            env.pop(key, None)
        env.update(extra or {})
        self.p = subprocess.Popen(factory.command("serve"), cwd=REPO, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, encoding="utf-8", bufsize=1)
        self.lines = queue.Queue()
        self.errors = []
        self.id = 0
        threading.Thread(target=self._read, daemon=True).start()
        threading.Thread(target=lambda: self.errors.append(self.p.stderr.read()), daemon=True).start()
        if WINDOWS and not getattr(factory, "baseline_ready", False):
            try:
                factory.initialize_windows(self)
            except BaseException:
                self.kill()
                raise

    def _read(self):
        for line in self.p.stdout:
            self.lines.put(line)
        self.lines.put(None)

    def send(self, method, params=None):
        self.id += 1
        self.p.stdin.write(json.dumps({"jsonrpc": "2.0", "id": self.id, "method": "validation.device." + method, "params": params or {}}, ensure_ascii=False) + "\n")
        self.p.stdin.flush()

    def receive(self, error=False):
        line = self.lines.get(timeout=45)
        if line is None:
            raise AssertionError(f"test helper exited: {self.p.wait(timeout=5)} {''.join(self.errors)}")
        d = json.loads(line)
        assert d["id"] == self.id and d["jsonrpc"] == "2.0", d
        assert ("error" in d) == error, d
        return d.get("error") if error else d["result"]

    def call(self, method, params=None, error=False):
        self.send(method, params)
        return self.receive(error)

    def close(self):
        if self.p.poll() is None:
            self.p.stdin.close()
            try:
                self.p.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.p.kill()
                self.p.wait(timeout=10)
                raise AssertionError("helper did not finish a safe terminal state after frontend EOF")

    def kill(self):
        if self.p.poll() is None:
            self.p.kill()
        self.p.wait(timeout=10)

    def __enter__(self):
        return self

    def __exit__(self, *args):
        self.close()


class Factory:
    def __init__(self, args, name):
        self.args = args
        self.root = args.directory / name
        self.env = dict(os.environ)
        self.device = None
        self.attached = False
        self.marker = None
        self.created = None
        self.payload_sha = None
        self.env.update(args.runtime_env)

    def command(self, command):
        args = [self.args.probe, command, "--root", self.root]
        if self.device:
            args += ["--loop", self.device]
        return [str(x) for x in args]

    def windows(self, action, *extra):
        result = run([os.environ.get("SystemRoot", r"C:\Windows") + r"\System32\WindowsPowerShell\v1.0\powershell.exe", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", REPO / "scripts/windows_device_fixture.ps1", "-Action", action, "-Root", self.root, *extra], env=self.env, timeout=150)
        (self.root / ("windows-" + action + ".stdout.log")).write_text(result.stdout, encoding="utf-8")
        try:
            return json.loads(result.stdout)
        except json.JSONDecodeError as e:
            raise AssertionError(f"Native fixture {action} returned invalid JSON: stdout={result.stdout!r}, stderr={result.stderr!r}") from e

    def __enter__(self):
        self.created = json.loads(run(self.command("create"), env=self.env).stdout)
        self.marker = json.loads((self.root / ".xetch-device-validation.json").read_text(encoding="utf-8"))
        assert self.marker["purpose"] == PURPOSE
        payload = self.root / "payload.bin"
        payload.write_bytes(bytes(range(256)) * 256 + "xEtch D1 native filesystem roundtrip\n".encode())
        self.payload_sha = sha(payload)
        self.factory_source_sha = sha(self.root / "original.img")
        self.source_path = self.root / "original.img"
        self.source_sha = self.factory_source_sha
        self.baseline_ready = not WINDOWS
        if WINDOWS:
            self.attached = True
            self.association = self.windows("Attach")
        else:
            self.build_plan()
            self.device = run(["losetup", "--find", "--show", "--partscan", self.root / "fixture.img"]).stdout.strip()
            assert self.device.startswith("/dev/loop") and self.device[9:].isdigit()
        self.attached = True
        return self

    def build_plan(self):
        workspace = self.root / "workspace"
        self.backend("workspace", "create", self.source_path, "--workspace", workspace)
        self.backend("file", "put", "--workspace", workspace, "--source", self.root / "payload.bin", "--destination", "/planned.txt")
        self.backend("apply", "--workspace", workspace, "--output", self.root / "planned.img")
        self.old_sha = sha(self.source_path, self.marker["partition_offset"], self.marker["partition_size"])
        self.new_sha = sha(self.root / "planned.img", self.marker["partition_offset"], self.marker["partition_size"])
        assert self.old_sha != self.new_sha

    def initialize_windows(self, client):
        # Capture the native-prepared baseline under the very same offline,
        # exclusive lease used for planning and commit; never remount in between.
        snapshot = client.call("snapshot")
        assert snapshot["target_write_calls"] == 0
        self.source_path = self.root / "baseline.img"
        self.source_sha = sha(self.source_path)
        self.build_plan()
        assert snapshot["partition_sha256"] == self.old_sha
        self.baseline_ready = True

    def backend(self, *args):
        return json.loads(run([self.args.binary, "--json", *args], env=self.env).stdout)

    def clean(self):
        if self.attached:
            if WINDOWS:
                assert self.windows("Detach")["detached"]
            else:
                mount = self.root / "mount"
                if mount.exists() and os.path.ismount(mount):
                    run(["umount", mount])
                backing = Path("/sys/class/block") / Path(self.device).name / "loop/backing_file"
                expected = str(self.root / "fixture.img")
                current = loop_backing(backing)
                if current:
                    assert current == expected, "refusing to detach a loop reassigned to another backing file"
                    run(["losetup", "--detach", self.device])
                    deadline = time.monotonic() + 10
                    while loop_backing(backing) == expected:
                        assert time.monotonic() < deadline, "owned loop backing remains attached"
                        time.sleep(.02)
            self.attached = False
        if self.created:
            if hasattr(self, "factory_source_sha"):
                assert sha(self.root / "original.img") == self.factory_source_sha, "factory seed image was modified"
            if hasattr(self, "source_sha"):
                assert sha(self.source_path) == self.source_sha, "original source baseline was modified"
            marker = json.loads((self.root / ".xetch-device-validation.json").read_text(encoding="utf-8"))
            assert marker["purpose"] == PURPOSE and marker["token"] == self.marker["token"]
            if not self.args.keep and not getattr(self,"failed",False):
                shutil.rmtree(self.root)

    def __exit__(self, *args):
        if args and args[0] is not None:
            self.failed = True
        self.clean()

    def status(self, client, expected=None):
        state = client.call("status", {"hash": True})
        self.last_state = state
        assert state["exclusive"] and state["test_only"]
        if expected:
            assert state["partition_sha256"] == expected, state
        return state

    def prepare(self, client):
        return client.call("plan", {"image": "planned.img"})

    def consent(self, client, plan):
        auth = client.call("confirm", {"plan_id": plan["plan_id"], "phrase": plan["phrase"]})
        return {"plan_id": plan["plan_id"], "authorization": auth["authorization"]}

    def driver_check(self):
        if WINDOWS:
            assert self.windows("DriverCheck", "-ExpectedSha256", self.payload_sha)["driver_check"]
        else:
            mount = self.root / "mount"
            mount.mkdir(exist_ok=True)
            run(["mount", "-t", "vfat", self.device + "p1", mount])
            try:
                assert sha(mount / "planned.txt") == self.payload_sha
                with (mount / "driver-feedback.txt").open("xb") as out:
                    out.write(b"native Linux FAT32 feedback\n")
                    out.flush()
                    os.fsync(out.fileno())
            finally:
                run(["umount", mount])
            run(["fsck.fat", "-n", self.device + "p1"])
        # Raw snapshot is read from the owned fixture only after native driver feedback.
        returned = self.root / "returned.img"
        if WINDOWS:
            self.windows("Detach")
            self.attached = False
            qemu = self.env["XETCH_QEMU_IMG"]
            run([qemu, "convert", "-f", "vpc", "-O", "raw", self.root / "fixture.vhd", returned], env=self.env)
        else:
            shutil.copyfile(self.root / "fixture.img", returned)
        output = self.root / "feedback.txt"
        self.backend("file", "get", returned, "--source", "/driver-feedback.txt", "--destination", output)
        assert b"FAT32 feedback" in output.read_bytes()


def wait_record(record, client):
    deadline = time.monotonic() + 40
    while not record.exists():
        if client.p.poll() is not None:
            raise AssertionError("helper exited before the interruption boundary: " + "".join(client.errors))
        if time.monotonic() >= deadline:
            raise AssertionError("test boundary timed out")
        time.sleep(.02)
    while True:
        try:
            event = json.loads(record.read_text(encoding="utf-8"))
            assert event["pid"] == client.p.pid
            return event
        except json.JSONDecodeError:
            if time.monotonic() >= deadline:
                raise AssertionError("interruption record is incomplete")
            time.sleep(.01)


def interrupt(factory, point="device.wrote_block", extra=None):
    record = factory.root / ("interrupt-" + uuid.uuid4().hex + ".json")
    env = {"XETCH_VALIDATION_POINT": point, "XETCH_VALIDATION_MODE": "pause", "XETCH_VALIDATION_RECORD": str(record)}
    env.update(extra or {})
    client = Client(factory, env)
    try:
        plan = factory.prepare(client)
        consent = factory.consent(client, plan)
        client.send("commit", consent)
        event = wait_record(record, client)
        assert event["point"] == point
    finally:
        client.kill()


def inventory(args):
    d = json.loads(run([args.binary, "--json", "device", "list"], env={**os.environ, **args.runtime_env, "XETCH_QEMU_IMG": "missing-qemu", "XETCH_QEMU_STORAGE_DAEMON": "missing-qsd"}).stdout)
    assert d["read_only"] and not d["write_enabled"] and d["devices"]
    assert all(not node["protection"]["write_allowed"] for node in d["devices"])
    protected = [node for node in d["devices"] if node["protection"]["hard_protected"]]
    assert any(set(n["roles"]) & {"system", "boot"} for n in protected), "current OS storage was not identified"
    one = json.loads(run([args.binary, "--json", "device", "inspect", d["devices"][0]["id"]], env={**os.environ, **args.runtime_env}).stdout)
    assert not one["device"]["protection"]["write_allowed"]
    requests = [{"jsonrpc": "2.0", "id": i, "method": method, "params": params} for i, (method, params) in enumerate([
        ("device.write", {"id": d["devices"][0]["id"], "force": True, "confirmed": True}),
        ("device.commit", {"authorization": "forged"}),
        ("device.confirm", {"phrase": "yes"}),
        ("device.list", {"write": True}),
        ("validation.device.plan", {"image": "anything"}),
    ], 1)]
    p = subprocess.run([str(args.binary), "serve", "--stdio"], input="".join(json.dumps(x) + "\n" for x in requests), capture_output=True, encoding="utf-8", cwd=REPO, timeout=45)
    replies = [json.loads(line) for line in p.stdout.splitlines()]
    assert p.returncode == 0 and len(replies) == len(requests) and all("error" in r for r in replies)
    return {"status": "pass", "devices": len(d["devices"]), "hard_protected": len(protected), "roles": sorted({r for n in protected for r in n["roles"]}), "warnings": d["warnings"], "checks": ["live_native_inventory_without_qemu", "current_system_backing_protected", "all_physical_writes_disabled", "inspect_uses_current_id", "forged_rpc_and_force_rejected", "validation_api_not_exposed"]}


def consent_case(f):
    with Client(f) as c:
        baseline = f.status(c, f.old_sha)
        assert baseline["target_write_calls"] == 0
        plan = f.prepare(c)
        assert f.status(c, f.old_sha)["target_write_calls"] == 0
        c.call("commit", {"plan_id": plan["plan_id"], "authorization": "forged"}, error=True)
        c.call("confirm", {"plan_id": plan["plan_id"], "phrase": ""}, error=True)
        c.call("confirm", {"plan_id": plan["plan_id"], "phrase": plan["phrase"], "force": True}, error=True)
        consent = f.consent(c, plan)
        c.call("cancel")
        c.call("commit", consent, error=True)
        assert f.status(c, f.old_sha)["target_write_calls"] == 0
        plan = f.prepare(c)
        stale = f.consent(c, plan)
        f.prepare(c)
        c.call("commit", stale, error=True)
        plan = f.prepare(c)
        consent = f.consent(c, plan)
        # The backend owns captured changes; mutating the input image cannot replace the accepted plan.
        with (f.root / "planned.img").open("r+b") as out:
            out.seek(f.marker["partition_offset"] + 96 * 1024 * 1024)
            out.write(b"changed after preview")
        c.call("commit", consent)
        c.call("commit", consent, error=True)
        assert f.status(c, f.new_sha)["target_write_calls"] > 0
    f.driver_check()
    return ["zero_writes_before_consent", "empty_wrong_and_forced_confirmation_rejected", "cancel_invalidates_authorization", "replacement_plan_invalidates_nonce", "captured_plan_immutable", "one_use_authorization", "full_partition_hash_matches", "native_driver_read_and_feedback", "independent_filesystem_check", "backend_reads_driver_feedback"]


def busy_case(f):
    if WINDOWS:
        handle = open(f.association["volume_path"] + "busy.txt", "xb")
        try:
            handle.write(b"owned native busy file");handle.flush();os.fsync(handle.fileno())
            run(f.command("claim"), env=f.env, fail=True)
        finally:
            handle.close()
    else:
        mount = f.root / "mount";mount.mkdir()
        run(["mount", "-t", "vfat", f.device + "p1", mount])
        try:
            run(f.command("claim"), env=f.env, fail=True)
        finally:
            run(["umount", mount])
    with Client(f) as c:
        assert c.call("status")["target_write_calls"] == 0
        run(f.command("claim"), env=f.env, fail=True)
    run(f.command("claim"), env=f.env)
    return ["busy_native_filesystem_refuses_lease", "second_exclusive_claim_refused", "lease_can_be_reacquired_after_close"]


def expiry_case(f):
    with Client(f, {"XETCH_DEVICE_TEST_CONFIRM_TTL_MS": "500"}) as c:
        p = f.prepare(c);auth = f.consent(c, p);time.sleep(.7)
        c.call("commit", auth, error=True)
        assert f.status(c, f.old_sha)["target_write_calls"] == 0
    return ["confirmation_expires_before_commit", "expired_plan_zero_writes"]


def identity_case(f):
    with Client(f) as c:
        plan = f.prepare(c);auth = f.consent(c, plan)
        p = f.root / ".xetch-device-validation.json";original = p.read_bytes()
        changed = json.loads(original);changed["image_identity"] += "-changed"
        try:
            p.write_text(json.dumps(changed), encoding="utf-8")
            c.call("commit", auth, error=True)
        finally:
            p.write_bytes(original)
        assert f.status(c, f.old_sha)["target_write_calls"] == 0
        c.call("commit", auth, error=True)
    return ["identity_change_revokes_consent", "failed_identity_check_burns_nonce", "target_unchanged"]


def boundary_case(f, point, mode):
    if mode == "kill":
        interrupt(f, point)
        with Client(f) as c:
            if point == "device.commit_recorded":
                assert not f.status(c, f.new_sha)["pending_recovery"]
            else:
                state = c.call("status")
                assert state["editing_blocked"] and len(state["pending_recovery"]) == 1
                f.prepare_error = c.call("plan", {"image": "planned.img"}, error=True)
                pending = state["pending_recovery"][0]
                c.call("recover", {"transaction_id": pending["transaction_id"], "phrase": "wrong"}, error=True)
                c.call("recover", {"transaction_id": pending["transaction_id"], "phrase": pending["phrase"]})
                assert not f.status(c, f.old_sha)["editing_blocked"]
        return ["actual_helper_process_killed_at_" + point, "new_session_scans_durable_journal", "unfinished_edits_blocked", "explicit_recovery_to_old_or_recorded_commit_to_new", "full_partition_hash"]
    record = f.root / "fault.json"
    with Client(f, {"XETCH_VALIDATION_POINT": point, "XETCH_VALIDATION_MODE": "error", "XETCH_VALIDATION_RECORD": str(record)}) as c:
        p = f.prepare(c);auth = f.consent(c, p)
        c.call("commit", auth, error=True)
        state = f.status(c, f.new_sha if point == "device.commit_recorded" else f.old_sha)
        assert not state["editing_blocked"]
        assert record.is_file()
        if point in ("device.journal_durable", "device.before_write"):
            assert state["target_write_calls"] == 0
    return ["returned_error_at_" + point, "automatic_verified_rollback_or_commit_uncertain", "full_partition_hash"]


def io_case(f, fault):
    with Client(f, {"XETCH_DEVICE_TEST_FAULT": fault}) as c:
        p = f.prepare(c);auth = f.consent(c, p)
        c.call("commit", auth, error=True)
        state = f.status(c, f.old_sha)
        if fault == "journal_enospc":
            assert state["editing_blocked"] and state["recovery_error"] and state["target_write_calls"] == 0
            c.call("plan", {"image": "planned.img"}, error=True)
        else:
            assert not state["editing_blocked"]
    return ["injected_" + fault, "verified_original_partition_hash", "fail_closed_or_verified_undo"]


def recovery_case(f, point=None, corrupt=False):
    interrupt(f)
    saved = None
    if corrupt:
        journal = next((f.root / "recovery").glob("*/journal.json"))
        saved = journal.read_bytes();changed = bytearray(saved);changed[len(changed)//2] ^= 1;journal.write_bytes(changed)
        with Client(f) as c:
            state = c.call("status")
            assert state["editing_blocked"] and state["recovery_error"]
            c.call("plan", {"image": "planned.img"}, error=True)
            assert state["target_write_calls"] == 0
        journal.write_bytes(saved)  # restore the controller's exact backup, not an automatic repair claim
    elif point:
        record = f.root / "recovery-interrupt.json"
        c = Client(f, {"XETCH_VALIDATION_POINT": point, "XETCH_VALIDATION_MODE": "pause", "XETCH_VALIDATION_RECORD": str(record)})
        try:
            pending = c.call("status")["pending_recovery"][0]
            c.send("recover", {"transaction_id": pending["transaction_id"], "phrase": pending["phrase"]})
            wait_record(record, c)
        finally:
            c.kill()
    else:
        with Client(f, {"XETCH_DEVICE_TEST_FAULT": "recovery_write_eio"}) as c:
            pending = c.call("status")["pending_recovery"][0]
            c.call("recover", {"transaction_id": pending["transaction_id"], "phrase": pending["phrase"]}, error=True)
            assert c.call("status")["editing_blocked"]
    with Client(f) as c:
        pending = c.call("status")["pending_recovery"][0]
        c.call("recover", {"transaction_id": pending["transaction_id"], "phrase": pending["phrase"]})
        assert not f.status(c, f.old_sha)["editing_blocked"]
    return ["interrupted_or_corrupt_recovery_refuses_new_edits", "zero_automatic_writes_with_corrupt_journal" if corrupt else "recovery_is_restartable", "explicit_recovery_full_old_hash"]


def frontend_case(f):
    record = f.root / "gate.json"
    c = Client(f, {"XETCH_VALIDATION_POINT": "device.wrote_block", "XETCH_VALIDATION_MODE": "gate", "XETCH_VALIDATION_RECORD": str(record)})
    try:
        p = f.prepare(c);c.send("commit", f.consent(c, p))
        wait_record(record, c)
        c.p.stdin.close()
        record.with_suffix(".continue").write_text("continue", encoding="utf-8")
        c.receive()
        c.p.wait(timeout=20)
    finally:
        c.kill()
    with Client(f) as resumed:
        assert not f.status(resumed, f.new_sha)["editing_blocked"]
    return ["frontend_eof_during_commit", "helper_keeps_lease_until_terminal_state", "committed_full_hash_after_reopen"]


def gui_case(f):
    with Client(f) as seed:
        f.status(seed, f.old_sha)
    report = f.args.output.with_suffix(".gui.json")
    config = f.root / "gui.json"
    config.write_text(json.dumps({"device_fixture": True, "root": str(f.root), "probe": str(f.args.probe), "report": str(report), "expected_sha256": f.new_sha}), encoding="utf-8")
    run([f.args.gui, "--smoke-test", config], env={**f.env, "XETCH_BACKEND": str(f.args.binary)}, timeout=150)
    result = json.loads(report.read_text(encoding="utf-8"))
    assert result["status"] == "pass", result
    f.driver_check()
    return result["checks"] + ["native_driver_verifies_gui_committed_file", "driver_feedback_read_by_backend"]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=REPO / ("target/windows/release/xetch.exe" if WINDOWS else "target/release/xetch"))
    parser.add_argument("--probe", type=Path, default=REPO / ("target/windows/release/examples/device_validation_probe.exe" if WINDOWS else "target/release/examples/device_validation_probe"))
    parser.add_argument("--directory", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--inventory-only", action="store_true")
    parser.add_argument("--suite", choices=["smoke", "all"], default="all")
    parser.add_argument("--keep", action="store_true")
    parser.add_argument("--gui", type=Path, help="Optional self-contained Windows GUI integration variant")
    parser.add_argument("--case", action="append", default=[], help="Run selected named cases; result records partial coverage")
    args = parser.parse_args()
    args.directory = args.directory.resolve();args.output = args.output.resolve();args.binary = args.binary.resolve();args.probe = args.probe.resolve()
    args.runtime_env = {}
    if WINDOWS:
        args.runtime_env = {"XETCH_QEMU_IMG": str(REPO / "artifacts/windows/qemu/qemu-img.exe"), "XETCH_QEMU_STORAGE_DAEMON": str(REPO / "artifacts/windows/qemu/qemu-storage-daemon.exe")}
    report = {"schema_version": 1, "status": "running", "platform": "native Windows x64" if WINDOWS else "Linux x64", "host": platform.platform(), "backend_sha256": sha(args.binary), "cases": [], "limits": ["No real physical disk writes; only newly factory-created 128 MiB loop/VHD images.", "Injected returned failures and killed helper processes do not emulate actual hardware power loss.", "Recovery journals are on the host filesystem, outside the virtual disk; different real physical failure domains are not verified.", "Physical device editing remains disabled in the production backend.", "GUI controller tests do not substitute for keyboard, picker and accessibility acceptance."]}
    active = None
    try:
        report["inventory"] = inventory(args)
        if not args.inventory_only:
            if WINDOWS:
                import ctypes
                assert ctypes.windll.shell32.IsUserAnAdmin(), "Run only the disposable VHD tests as Administrator."
            else:
                assert os.geteuid() == 0, "Root is required only to create and mount disposable loop devices."
            args.directory.mkdir()
            (args.directory / ".xetch-physical-validation.json").write_text(json.dumps({"purpose": "xetch-device-test-controller-v1", "token": str(uuid.uuid4())}), encoding="utf-8")
            runtime = args.directory / "runtime";runtime.mkdir()
            for field in ("binary", "probe"):
                source = getattr(args, field);destination = runtime / source.name;shutil.copy2(source, destination);setattr(args, field, destination)
            report["probe_sha256"] = sha(args.probe)
            source_paths = sorted((REPO / "crates/xetch-core/src/devices").glob("*")) + sorted((REPO / "crates/xetch-core/src/device_validation").glob("*"))
            source_paths += [REPO / "crates/xetch-core/examples/device_validation_probe.rs", REPO / "scripts/devices_validate.py", REPO / "scripts/windows_device_fixture.ps1"]
            report["source_inputs"] = [{"path": str(p.relative_to(REPO)).replace("\\", "/"), "sha256": sha(p)} for p in source_paths if p.is_file()]
            cases = [("consent-and-native-driver", consent_case), ("exclusive-native-access", busy_case), ("expired-consent", expiry_case), ("changed-device-identity", identity_case), ("frontend-disconnect", frontend_case)]
            if args.suite == "all":
                for mode in ("error", "kill"):
                    for point in ("device.journal_durable", "device.before_write", "device.wrote_block", "device.before_write.2", "device.wrote_block.2", "device.target_synced", "device.commit_recorded"):
                        cases.append((mode + "-" + point.replace(".", "-"), lambda f, p=point, m=mode: boundary_case(f, p, m)))
                for fault in ("write_eio", "write_enospc", "torn_write", "sync_eio", "journal_enospc"):
                    cases.append(("io-" + fault, lambda f, fault=fault: io_case(f, fault)))
                cases += [("recovery-write-eio", recovery_case), ("recovery-corrupt-journal", lambda f: recovery_case(f, corrupt=True))]
                for point in ("device.recovery_wrote_block", "device.recovery_synced"):
                    cases.append((point.replace(".", "-"), lambda f, p=point: recovery_case(f, p)))
            if args.gui:
                assert WINDOWS, "GUI D1 acceptance currently supports Windows only"
                args.gui = args.gui.resolve()
                cases.append(("windows-gui-consent", gui_case))
            available = {name for name, _ in cases}
            selected = list(args.case)
            selection_file = args.output.with_name(args.output.name + ".selection.json")
            if selection_file.exists():
                selection = json.loads(selection_file.read_text(encoding="utf-8"))
                assert isinstance(selection, dict) and set(selection) == {"cases"} and isinstance(selection["cases"], list)
                assert all(isinstance(name, str) for name in selection["cases"])
                selected += selection["cases"]
                report["selection_file_sha256"] = sha(selection_file)
            assert not (set(selected) - available), "unknown selected test case"
            report["available_cases"] = sorted(available)
            report["full_matrix"] = not selected
            report["selected_cases"] = selected
            if selected:
                cases = [(name, action) for name, action in cases if name in selected]
            for name, action in cases:
                active = Factory(args, name)
                started = time.monotonic()
                try:
                    with active as f:
                        checks = action(f)
                        evidence = {"fixture_token": f.marker["token"], "partition_offset": f.marker["partition_offset"], "partition_size": f.marker["partition_size"], "old_partition_sha256": f.old_sha, "planned_partition_sha256": f.new_sha, "source_image": f.source_path.name, "original_source_sha256": f.source_sha, "factory_seed_sha256": f.factory_source_sha, "payload_sha256": f.payload_sha, "last_verified_state": getattr(f, "last_state", None)}
                        evidence["journals"] = [{"sha256": sha(j), "committed": (j.parent / "committed").exists(), "rolled_back": (j.parent / "rolled-back").exists()} for j in sorted((f.root / "recovery").glob("*/journal.json"))]
                    report["cases"].append({"case": name, "status": "pass", "checks": checks, "evidence": evidence, "seconds": round(time.monotonic()-started, 3), "fixture_detached": not active.attached})
                    print("PASS " + name, flush=True)
                except Exception as e:
                    with contextlib.suppress(Exception):
                        active.clean()
                    report["cases"].append({"case": name, "status": "fail", "error": repr(e), "seconds": round(time.monotonic()-started, 3), "fixture_detached": not active.attached})
                    print("FAIL " + name + ": " + repr(e), flush=True)
                    if active.attached:
                        raise
                finally:
                    args.output.parent.mkdir(parents=True, exist_ok=True)
                    write_report(args.output, report)
        failures = [case for case in report["cases"] if case["status"] != "pass"]
        if failures:
            raise AssertionError(f"{len(failures)} independent device cases failed; see their preserved diagnostics")
        report["status"] = "pass"
    except Exception as e:
        report["status"] = "fail";report["error"] = repr(e)
        raise
    finally:
        report["cleanup"] = {"active_owned_devices": 1 if active and active.attached else 0}
        report["finished_at_unix"] = time.time()
        args.output.parent.mkdir(parents=True, exist_ok=True)
        write_report(args.output, report)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
