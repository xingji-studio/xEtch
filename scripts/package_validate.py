#!/usr/bin/env python3
"""Validate a relocated package without source-tree tools or PATH dependencies."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys

from xetch_build import ROOT, ensure_replaceable, powershell, windows_path


def validate_posix(package, directory):
    directory.mkdir(parents=True, exist_ok=False)
    relocated = directory / "portable folder 中文"
    shutil.copytree(package, relocated)
    manifest = json.loads((relocated / "xetch-package.json").read_text())
    env = os.environ.copy()
    for key in ["XETCH_EXT4_ENGINE", "XETCH_EXT4_WORKER", "XETCH_GUI", "XETCH_QEMU_IMG", "XETCH_QEMU_STORAGE_DAEMON", "XETCH_NTFS_WORKER", "XETCH_BACKEND", "QEMU_MODULE_DIR", "LD_LIBRARY_PATH", "DYLD_LIBRARY_PATH"]:
        env.pop(key, None)
    env["PATH"] = ""
    binary = relocated / "xetch"
    def cli(*args):
        p = subprocess.run([str(binary), "--json", *map(str, args)], cwd=directory, env=env,
                           capture_output=True, timeout=120)
        if p.returncode: raise RuntimeError(p.stderr.decode(errors="replace"))
        return json.loads(p.stdout)
    capabilities = cli("doctor")
    assert all(t["available"] for t in capabilities["tools"])
    assert capabilities["capabilities"]["ntfs_write"] == manifest["ntfs_worker"]
    devices = cli("device", "list")
    assert devices["read_only"] and not devices["write_enabled"]
    assert all(not node["protection"]["write_allowed"] for node in devices["devices"])
    no_gui = subprocess.run([str(binary)], cwd=directory, env=env, capture_output=True, timeout=10)
    assert no_gui.returncode == 4 and b"GUI was not found" in no_gui.stderr
    rpc = subprocess.run([str(binary), "serve", "--stdio"], cwd=directory, env=env,
                         input=b'{"jsonrpc":"2.0","id":74,"method":"system.capabilities"}\n',
                         capture_output=True, timeout=30)
    assert rpc.returncode == 0 and not rpc.stderr
    response = json.loads(rpc.stdout)
    assert response["id"] == 74 and response["result"]["protocol_version"] == 1
    image, ws, payload = directory/"source.img", directory/"session", directory/"payload.txt"
    payload.write_bytes("portable package 中文\n".encode())
    cli("--experimental-filesystems", "create", "--output", image, "--size", "64MiB", "--table", "gpt", "--filesystem", "ext4")
    cli("workspace", "create", image, "--workspace", ws)
    if manifest.get("ext4_worker"):
        env["XETCH_EXT4_ENGINE"] = "lwext4"
    cli("--experimental-filesystems", "file", "put", "--workspace", ws, "--source", payload, "--destination", "/配置.txt")
    saved = directory/"edited.vmdk"
    cli("apply", "--workspace", ws, "--output", saved)
    readback = directory/"readback.txt"
    cli("file", "get", saved, "--source", "/配置.txt", "--destination", readback)
    assert readback.read_bytes() == payload.read_bytes()
    check_env = env.copy(); check_env["XETCH_PACKAGE_CHECK"] = "isolated"
    subprocess.run(["/bin/sh", str(relocated/"check-deps.sh")], cwd=directory, env=check_env, check=True, timeout=60)
    if manifest.get("ext4_worker"):
        worker = relocated/"xetch-ext4-worker"
        hidden = relocated/"xetch-ext4-worker.hidden"
        worker.rename(hidden)
        try:
            p = subprocess.run(["/bin/sh",str(relocated/"check-deps.sh")],cwd=directory,env=check_env,capture_output=True,timeout=60)
            assert p.returncode != 0 and b"xetch-ext4-worker" in p.stdout
        finally:hidden.rename(worker)
    tool = relocated/"qemu/qemu-img"
    missing = relocated/"qemu/qemu-img.hidden"
    tool.rename(missing)
    try:
        p = subprocess.run(["/bin/sh", str(relocated/"check-deps.sh")], cwd=directory, env=check_env, capture_output=True, timeout=60)
        assert p.returncode != 0 and b"qemu-img" in p.stdout
    finally: missing.rename(tool)
    ensure_replaceable(relocated)
    sentinel = relocated/"personal-image.img";sentinel.write_bytes(b"keep personal data")
    try:
        try: ensure_replaceable(relocated)
        except RuntimeError: pass
        else: raise AssertionError("Package replacement must refuse user files")
        assert sentinel.read_bytes() == b"keep personal data"
    finally: sentinel.unlink()
    return {"schema_version":1, "status":"pass", "platform":manifest["platform"], "checks":[
        "relocated_read_only_devices_without_development_environment", "relocated_unicode_and_space_path", "empty_PATH_no_development_overrides", "bundled_qemu_and_ntfs",
        "create_ext4_and_edit_workspace", "vmdk_export_and_readback", "runtime_check_without_python",
        "missing_dependency_is_reported", "package_replacement_preserves_personal_files",
        "backend_only_no_args_reports_missing_gui", "stdio_rpc_does_not_launch_frontend", *(["bundled_bsd_ext4_worker_selected_without_PATH", "missing_bsd_ext4_worker_reported"] if manifest.get("ext4_worker") else [])]}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package", type=Path, required=True)
    parser.add_argument("--directory", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--gui-test-build", type=Path)
    args = parser.parse_args()
    package, directory = args.package.resolve(), args.directory.resolve()
    manifest = json.loads((package/"xetch-package.json").read_text())
    if manifest["platform"] == "windows":
        arguments = ["-Package", windows_path(package), "-Directory", windows_path(directory), "-Output", windows_path(args.output)]
        if args.gui_test_build: arguments += ["-GuiTestBuild", windows_path(args.gui_test_build)]
        powershell("windows_package_validate.ps1", *arguments)
    else:
        result = validate_posix(package, directory)
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(result, indent=2)+"\n")
        print(json.dumps(result, indent=2))
