#!/usr/bin/env python3
"""Exercise actual QSD bind failures, bounded retry and non-retryable startup errors."""
import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import shutil

from deep_filesystems_validate import ROOT, Runner, sha


def validate(args):
    root = args.directory.resolve()
    root.mkdir(parents=True, exist_ok=False)
    (root / ".xetch-startup-validation.json").write_text(
        json.dumps({"purpose": "xetch-nbd-startup-validation-v1"}) + "\n")
    runtime = root / "runtime"
    runtime.mkdir()
    binary = runtime / args.binary.name
    shutil.copy2(args.binary, binary)
    backend = Runner(binary, args.qemu_directory)
    proxy_source = args.fault_proxy or ROOT / (
        "target/windows/release/examples/qsd_fault_proxy.exe" if backend.windows
        else "target/release/examples/qsd_fault_proxy")
    proxy = runtime / proxy_source.name
    shutil.copy2(proxy_source, proxy)
    real = (args.qemu_directory.resolve() / "qemu-storage-daemon.exe" if backend.windows
            else Path(shutil.which("qemu-storage-daemon")))
    source = root / "source.img"
    backend.cli("create", "--output", source, "--size", "64MiB", "--table", "none")
    original = sha(source)
    cases = []
    for mode, launches in (("bind-once", 2), ("bind-always", 5), ("startup-error", 1)):
        trace = root / (mode + ".jsonl")
        env = backend.env.copy()
        injected = {
            "XETCH_QEMU_STORAGE_DAEMON": backend.host(proxy),
            "XETCH_TEST_QSD_REAL": backend.host(real),
            "XETCH_TEST_QSD_FAULT": mode,
            "XETCH_TEST_QSD_STARTUP_TRACE": backend.host(trace),
            "XETCH_TEST_QSD_COLLISION_MARKER": backend.host(root / "collision-once"),
        }
        env.update(injected)
        if backend.windows and os.name != "nt":
            env["WSLENV"] = ":".join(filter(None, [env.get("WSLENV", ""), *injected]))
        result = backend.cli("file", "list", source, env=env, fail=mode != "bind-once")
        attempts = [json.loads(line) for line in trace.read_text().splitlines()]
        assert len(attempts) == launches, (mode, attempts, result)
        if mode != "bind-once":
            assert result["code"] == "dependency_failure", result
            assert ("Failed to find an available port" in result["message"]) == (mode == "bind-always"), result
        assert sha(source) == original
        cases.append({"mode": mode, "status": "pass", "launches": len(attempts),
                      "ports": [attempt["port"] for attempt in attempts],
                      "checks": ["actual_qemu_startup_diagnostic", "expected_retry_count", "source_unchanged"]})
    report = {"schema_version": 1, "status": "pass", "validated_at": datetime.now(timezone.utc).isoformat(),
              "platform": "native Windows x64" if backend.windows else "Linux x64",
              "backend_sha256": sha(binary), "fault_proxy_sha256": sha(proxy), "cases": cases}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    print("PASS: real QSD port collision recovery, five-attempt bound, unrelated startup error not retried")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/xetch")
    parser.add_argument("--fault-proxy", type=Path)
    parser.add_argument("--qemu-directory", type=Path, default=ROOT / "artifacts/windows/qemu")
    parser.add_argument("--directory", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    validate(parser.parse_args())
