#!/usr/bin/env python3
"""Exercise the shipped CLI/RPC and independently check saved FAT images (Linux)."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import signal
import tempfile


def validate(binary):
    checks = []
    def cli(*args, success=True):
        proc = subprocess.Popen([str(binary), "--json", *map(str, args)], stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
        try:
            stdout, stderr = proc.communicate(timeout=120)
        except subprocess.TimeoutExpired:
            os.killpg(proc.pid, signal.SIGKILL)
            proc.communicate()
            raise
        result = subprocess.CompletedProcess(proc.args, proc.returncode, stdout, stderr)
        if success:
            assert result.returncode == 0, result.stderr.decode()
            return json.loads(result.stdout)
        assert result.returncode != 0
        return result
    with tempfile.TemporaryDirectory(prefix="xetch-cli-") as tmp:
        root = Path(tmp)
        image = root / "source.img"
        workspace = root / "session"
        cli("create", "--output", image, "--size", "64MiB", "--table", "none")
        cli("inspect", image)
        cli("workspace", "create", image, "--workspace", workspace)
        payload = root / "payload.txt"
        payload.write_bytes("xEtch 中文 CLI\n".encode())
        cli("file", "put", "--workspace", workspace, "--source", payload, "--destination", "/hello.txt")
        assert cli("file", "list", "--workspace", workspace)["entries"][0]["name"] == "hello.txt"
        cli("file", "get", "--workspace", workspace, "--source", "/hello.txt", "--destination", root / "extracted.txt")
        assert (root / "extracted.txt").read_bytes() == payload.read_bytes()
        assert cli("plan", "--workspace", workspace)["manifest"]["changes"][0]["operation"] == "file.put"
        output = root / "saved.img"
        cli("apply", "--workspace", workspace, "--output", output)
        # Independent implementations inspect the bytes emitted by our Rust writer.
        subprocess.run(["fsck.fat", "-n", str(output)], check=True, capture_output=True, timeout=30)
        subprocess.run(["mcopy", "-i", str(output), "::/hello.txt", str(root / "mtools.txt")], check=True, capture_output=True, timeout=30)
        assert (root / "mtools.txt").read_bytes() == payload.read_bytes()
        checks.append("CLI FAT import/export, workspace resume, fsck.fat and mtools verification")
        cli("convert", output, "--output", root / "saved.vmdk")
        cli("file", "get", root / "saved.vmdk", "--source", "/hello.txt", "--destination", root / "vmdk.txt")
        assert (root / "vmdk.txt").read_bytes() == payload.read_bytes()
        checks.append("CLI VMDK conversion and file readback")
        # Fill the child overlay, then fail: committed workspace must be intact.
        small = root / "small-fat12.img"
        with small.open("wb") as f:
            f.truncate(2 * 1024 * 1024)
        subprocess.run(["mkfs.fat", "-F", "12", str(small)], check=True, capture_output=True, timeout=30)
        small_workspace = root / "small-session"
        cli("workspace", "create", small, "--workspace", small_workspace)
        huge = root / "too-large.bin"
        with huge.open("wb") as f:
            f.truncate(3 * 1024 * 1024)
        before = (small_workspace / "workspace.json").read_bytes()
        cli("file", "put", "--workspace", small_workspace, "--source", huge, "--destination", "/huge.bin", success=False)
        assert (small_workspace / "workspace.json").read_bytes() == before
        assert cli("file", "list", "--workspace", small_workspace)["entries"] == []
        checks.append("filesystem out-of-space rolls back partial writes")
        # Exercise real RPC transport and a mutation through the same application core.
        requests = [
            {"jsonrpc": "2.0", "id": 1, "method": "system.capabilities"},
            {"jsonrpc": "2.0", "id": 2, "method": "file.list", "params": {"image": str(output)}},
            {"jsonrpc": "2.0", "id": 3, "method": "file.mkdir", "params": {"workspace": str(workspace), "path": "/rpc-dir"}},
            {"jsonrpc": "2.0", "id": 4, "method": "file.list", "params": {"workspace": str(workspace)}},
        ]
        result = subprocess.run([str(binary), "serve", "--stdio"], input="\n".join(map(json.dumps, requests))+"\n",
                                text=True, capture_output=True, check=True, timeout=120)
        replies = list(map(json.loads, result.stdout.splitlines()))
        assert [r["id"] for r in replies] == [1, 2, 3, 4]
        assert all("result" in r for r in replies), replies
        assert "rpc-dir" in [e["name"] for e in replies[3]["result"]["entries"]]
        checks.append("JSON-RPC stdio capabilities, file browsing and editing")
    return checks


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/debug/xetch"))
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    checks = validate(args.binary.resolve())
    report = {"status": "pass", "checks": checks}
    text = json.dumps(report, ensure_ascii=False, indent=2)+"\n"
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(text, encoding="utf-8")
    print(text)
