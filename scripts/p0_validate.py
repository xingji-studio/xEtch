#!/usr/bin/env python3
"""Rootless P0 experiment. Creates only disposable images in a temporary directory."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import secrets
import shutil
import socket
import struct
import subprocess
import tempfile
import time


def run(*args):
    return subprocess.run([str(a) for a in args], check=True, capture_output=True, timeout=120)


def exact(sock, n):
    data = bytearray()
    while len(data) < n:
        chunk = sock.recv(n - len(data))
        if not chunk:
            raise EOFError("NBD connection closed")
        data.extend(chunk)
    return data


class Nbd:
    def __init__(self, path, fmt, writable=False):
        self.log = tempfile.TemporaryFile()
        self.proc = None
        self.sock = None
        reservation = socket.socket()
        reservation.bind(("127.0.0.1", 0))
        port = reservation.getsockname()[1]
        reservation.close()
        token = secrets.token_hex(16)
        node = {"driver": fmt, "node-name": "disk", "read-only": not writable,
                "file": {"driver": "file", "filename": str(path)}}
        self.proc = subprocess.Popen([
            "qemu-storage-daemon", "--blockdev", json.dumps(node),
            "--nbd-server", f"addr.type=inet,addr.host=127.0.0.1,addr.port={port},max-connections=1",
            "--export", f"nbd,id=disk,node-name=disk,name={token},writable={'on' if writable else 'off'}"
        ], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=self.log)
        try:
            deadline = time.monotonic() + 10
            while True:
                if self.proc.poll() is not None:
                    self.log.seek(0)
                    raise RuntimeError(self.log.read().decode())
                try:
                    self.sock = socket.create_connection(("127.0.0.1", port), timeout=2)
                    break
                except OSError:
                    if time.monotonic() > deadline:
                        raise
                    time.sleep(0.02)
            self.sock.settimeout(10)
            magic, opts, flags = struct.unpack(">QQH", exact(self.sock, 18))
            assert magic == 0x4E42444D41474943 and opts == 0x49484156454F5054
            assert flags & 1
            self.sock.sendall(struct.pack(">I", 3 if flags & 2 else 1))
            encoded = token.encode()
            self.sock.sendall(struct.pack(">QII", opts, 1, len(encoded)) + encoded)
            self.size, self.flags = struct.unpack(">QH", exact(self.sock, 10))
            if not flags & 2:
                exact(self.sock, 124)
        except BaseException:
            self.close()
            raise

    def request(self, cmd, offset=0, data=b"", length=0):
        if cmd == 1:
            length = len(data)
        self.sock.sendall(struct.pack(">IHHQQI", 0x25609513, 0, cmd, 1, offset, length) + data)
        magic, error, handle = struct.unpack(">IIQ", exact(self.sock, 16))
        assert magic == 0x67446698 and handle == 1
        if error:
            raise OSError(error, "NBD request failed")
        return bytes(exact(self.sock, length)) if cmd == 0 else None

    def close(self):
        if self.sock:
            try:
                self.sock.sendall(struct.pack(">IHHQQI", 0x25609513, 0, 2, 1, 0, 0))
            except OSError:
                pass
            self.sock.close()
            self.sock = None
        if self.proc:
            self.proc.terminate()
            self.proc.wait(timeout=10)
            self.proc = None
        self.log.close()

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()


def digest(path):
    with open(path, "rb") as f:
        return hashlib.file_digest(f, "sha256").hexdigest()


def validate():
    results = []
    required = ["qemu-img", "qemu-storage-daemon", "mkfs.fat", "mcopy", "fsck.fat",
                "mkfs.ext4", "debugfs", "e2fsck", "mkntfs", "ntfscp", "ntfscat"]
    missing = [name for name in required if not shutil.which(name)]
    if missing:
        raise RuntimeError(f"Missing P0 experiment dependencies: {missing}")
    with tempfile.TemporaryDirectory(prefix="xetch-p0-") as tmp:
        root = Path(tmp)
        raw = root / "fat.img"
        with raw.open("wb") as f:
            f.truncate(64 * 1024 * 1024)
        run("mkfs.fat", "-F", "32", raw)
        payload = root / "payload.txt"
        payload.write_bytes("xEtch P0 中文文件内容\n".encode())
        run("mcopy", "-i", raw, payload, "::/probe.txt")
        run("fsck.fat", "-n", raw)
        results.append({"test": "fat32_offline_write_and_check", "status": "pass"})
        for sub in ["monolithicSparse", "monolithicFlat", "twoGbMaxExtentSparse", "twoGbMaxExtentFlat", "streamOptimized"]:
            image_dir = root / sub
            image_dir.mkdir()
            vmdk = image_dir / "disk.vmdk"
            run("qemu-img", "convert", "-f", "raw", "-O", "vmdk", "-o", f"subformat={sub}", raw, vmdk)
            with Nbd(vmdk, "vmdk") as nbd:
                assert nbd.size == raw.stat().st_size
                with raw.open("rb") as f:
                    assert nbd.request(0, length=4096) == f.read(4096)
            results.append({"test": f"vmdk_{sub}_nbd_read", "status": "pass"})
            # All edits go to a persistent qcow2 overlay, including streamOptimized.
            before = {p.name: digest(p) for p in image_dir.iterdir()}
            overlay = root / f"{sub}.qcow2"
            run("qemu-img", "create", "-f", "qcow2", "-F", "vmdk", "-b", vmdk, overlay)
            block = b"xEtch" + bytes(507)
            with Nbd(overlay, "qcow2", True) as nbd:
                nbd.request(1, 24 * 1024 * 1024, block)
                nbd.request(3)
                assert nbd.request(0, 24 * 1024 * 1024, length=512) == block
            with Nbd(overlay, "qcow2") as nbd:
                assert nbd.request(0, 24 * 1024 * 1024, length=512) == block
            assert before == {p.name: digest(p) for p in image_dir.iterdir()}
            output = root / f"{sub}-edited.vmdk"
            run("qemu-img", "convert", "-f", "qcow2", "-O", "vmdk", overlay, output)
            with Nbd(output, "vmdk") as nbd:
                assert nbd.request(0, 24 * 1024 * 1024, length=512) == block
            results.append({"test": f"vmdk_{sub}_overlay_flush_reopen_export_source_unchanged", "status": "pass"})
        # Sparse 5 GiB image tests actual split extents and 64-bit offsets
        # without allocating 5 GiB of host storage.
        large_raw = root / "large.img"
        with large_raw.open("wb") as f:
            f.truncate(5 * 1024**3)
        split_dir = root / "large-split"
        split_dir.mkdir()
        split = split_dir / "disk.vmdk"
        run("qemu-img", "convert", "-f", "raw", "-O", "vmdk", "-o", "subformat=twoGbMaxExtentSparse", large_raw, split)
        before = {p.name: digest(p) for p in split_dir.iterdir()}
        overlay = root / "large-overlay.qcow2"
        run("qemu-img", "create", "-f", "qcow2", "-F", "vmdk", "-b", split, overlay)
        offsets = [2 * 1024**3 - 512, 2 * 1024**3, 4 * 1024**3 + 4096]
        with Nbd(overlay, "qcow2", True) as nbd:
            assert nbd.size == 5 * 1024**3
            for i, offset in enumerate(offsets):
                nbd.request(1, offset, bytes([i + 1]) * 512)
            nbd.request(3)
        saved = root / "large-saved.vmdk"
        run("qemu-img", "convert", "-f", "qcow2", "-O", "vmdk", overlay, saved)
        with Nbd(saved, "vmdk") as nbd:
            for i, offset in enumerate(offsets):
                assert nbd.request(0, offset, length=512) == bytes([i + 1]) * 512
        assert before == {p.name: digest(p) for p in split_dir.iterdir()}
        results.append({"test": "5GiB_sparse_split_vmdk_2GiB_boundary_and_64bit_offsets", "status": "pass"})
        ext = root / "ext4.img"
        with ext.open("wb") as f:
            f.truncate(64 * 1024 * 1024)
        run("mkfs.ext4", "-q", "-F", ext)
        run("debugfs", "-w", "-R", f"write {payload} /probe.txt", ext)
        assert run("debugfs", "-R", "cat /probe.txt", ext).stdout == payload.read_bytes()
        run("e2fsck", "-fn", ext)
        results.append({"test": "ext4_offline_tools_write_read_check", "status": "pass", "product_support": False})
        ntfs = root / "ntfs.img"
        with ntfs.open("wb") as f:
            f.truncate(64 * 1024 * 1024)
        run("mkntfs", "-F", "-Q", ntfs)
        run("ntfscp", "-f", ntfs, payload, "/probe.txt")
        assert run("ntfscat", ntfs, "/probe.txt").stdout == payload.read_bytes()
        results.append({"test": "ntfs_offline_tools_write_read", "status": "pass", "product_support": False})
    return {
        "schema_version": 1, "host": platform.platform(),
        "qemu": run("qemu-img", "--version").stdout.decode().splitlines()[0],
        "rootless": os.geteuid() != 0 if hasattr(os, "geteuid") else None,
        "results": results,
        "unverified": ["Windows runtime and QEMU distribution", "macOS runtime and signing",
                       "exFAT implementation", "cross-platform NTFS/ext4 library writes",
                       "power-loss durability", "large real-world and snapshot fixtures"],
    }


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    report = validate()
    text = json.dumps(report, ensure_ascii=False, indent=2) + "\n"
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(text, encoding="utf-8")
    print(text)
