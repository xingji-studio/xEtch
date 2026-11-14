#!/usr/bin/env python3
"""Create/edit exFAT and ext4 with the native backend; independently check bytes.

Linux can check its own output or Windows output from WSL. On Windows CI use
--produce-only, then transfer the new directory and use --check-directory on
Linux. No mounts, root, or host filesystem formatting.
"""
import argparse
import json
import os
from pathlib import Path
import struct
import subprocess
import tempfile
import uuid
import zlib

from extended_validate import ExfatReader, command, debug, digest


class Backend:
    def __init__(self, binary, qemu_directory):
        self.binary = binary.resolve()
        self.windows = self.binary.suffix.lower() == ".exe"
        self.env = os.environ.copy()
        if self.windows:
            qemu = qemu_directory.resolve()
            self.env["XETCH_QEMU_IMG"] = self.host(qemu / "qemu-img.exe")
            self.env["XETCH_QEMU_STORAGE_DAEMON"] = self.host(qemu / "qemu-storage-daemon.exe")
            if os.name != "nt":
                self.env["WSLENV"] = ":".join(filter(None, [self.env.get("WSLENV", ""),
                    "XETCH_QEMU_IMG", "XETCH_QEMU_STORAGE_DAEMON"]))

    def host(self, value):
        return command("wslpath", "-w", value).decode().strip() if self.windows and os.name != "nt" else str(value)

    def cli(self, *args, experimental=True, fail=False, env=None):
        argv = [str(self.binary), "--json", *(["--experimental-filesystems"] if experimental else []),
                *(self.host(a) if isinstance(a, Path) else str(a) for a in args)]
        result = subprocess.run(argv, capture_output=True, env=env or self.env, timeout=180)
        if fail:
            assert result.returncode, args
            return json.loads(result.stderr)["error"]["code"]
        if result.returncode:
            raise RuntimeError(f"{args}: {result.stderr.decode(errors='replace')}")
        return json.loads(result.stdout)


def produce(backend, directory, large):
    directory.mkdir(parents=True, exist_ok=False)
    cases = []
    for table in ("none", "mbr", "gpt"):
        cases.append((f"64m-{table}", 64 << 20, table, "img", True))
    cases += [("sector-tail", (64 << 20)+512, "none", "img", False),
              ("tiny-last-group", (128 << 20)+32768, "none", "img", False),
              ("multi-group", 258 << 20, "gpt", "img", False),
              ("direct-vmdk", 64 << 20, "gpt", "vmdk", True)]
    if large:
        cases.append(("5g-vmdk", 5 << 30, "gpt", "vmdk", False))
    records = []
    for fs in ("exfat", "ext4"):
        guard = directory / f"{fs}-guard.img"
        before = set(directory.iterdir())
        for args, code in [(("--size", "64MiB"), "unsupported"),
                           (("--size", "512"), "invalid_argument"),
                           (("--size", str((64 << 20)+1)), "invalid_argument")]:
            assert backend.cli("create", "--output", guard, "--filesystem", fs, *args,
                               experimental=(code != "unsupported"), fail=True) == code
            assert set(directory.iterdir()) == before
        guard.write_bytes(b"existing output must survive")
        original = guard.read_bytes()
        backend.cli("create", "--output", guard, "--size", "64MiB", "--filesystem", fs, fail=True)
        assert guard.read_bytes() == original
        guard.unlink()
        broken = backend.env.copy()
        broken["XETCH_QEMU_IMG"] = backend.host(directory / "missing-qemu-img.exe")
        backend.cli("create", "--output", guard, "--size", "64MiB", "--filesystem", fs,
                    fail=True, env=broken)
        assert set(directory.iterdir()) == before
        for name, size, table, extension, complete in cases:
            root = directory / f"{fs}-{name}"
            root.mkdir()
            source = root / f"source.{extension}"
            created = backend.cli("create", "--output", source, "--size", str(size),
                                  "--table", table, "--filesystem", fs)
            assert created["filesystem"].lower() == fs
            info = backend.cli("inspect", source)
            partition = info["layout"]["partitions"][0]
            assert info["image"]["virtual_size"] == size and partition["filesystem"].lower() == fs
            assert partition["index"] == (0 if table == "none" else 1)
            source_hash = digest(source)
            # Keep independently checkable raw bytes even for direct VMDK creation.
            raw = source
            if extension == "vmdk":
                raw = root / "source-raw.img"
                backend.cli("convert", source, "--output", raw)
            session = root / "session"
            backend.cli("workspace", "create", source, "--workspace", session)
            manifest = session / "workspace.json"
            def edit(op, *args, fail=False):
                old = manifest.read_bytes()
                entries = set(session.iterdir())
                result = backend.cli("file", op, "--workspace", session, *args, fail=fail)
                if fail:
                    assert manifest.read_bytes() == old and set(session.iterdir()) == entries
                return result
            payload = root / "payload.bin"
            payload.write_bytes(b"native format read/write\n" * 8192)
            backend.cli("file", "list", source)
            edit("mkdir", "--path", "/tree")
            edit("put", "--source", payload, "--destination", "/tree/data.bin")
            payload.write_bytes(b"final contents\n")
            edit("put", "--source", payload, "--destination", "/tree/data.bin", "--replace")
            if complete:
                edit("mkdir", "--path", "/parent")
                edit("rename", "--source", "/tree", "--destination", "/parent/moved")
                edit("rename", "--source", "/parent", "--destination", "/parent/moved/cycle", fail=True)
                edit("remove", "--path", "/parent/moved", fail=True)
                edit("rename", "--source", "/parent/moved/data.bin", "--destination", "/parent/moved/内容😀.txt")
                edit("rename", "--source", "/parent/moved", "--destination", "/tree")
                edit("rename", "--source", "/tree/内容😀.txt", "--destination", "/tree/data.bin")
                edit("remove", "--path", "/parent")
                edit("put", "--source", payload, "--destination", "/tree/data.bin", fail=True)
            saved = root / "saved.img"
            backend.cli("apply", "--workspace", session, "--output", saved)
            readback = root / "readback.bin"
            backend.cli("file", "get", saved, "--source", "/tree/data.bin", "--destination", readback)
            assert readback.read_bytes() == payload.read_bytes()
            if complete:
                vmdk = root / "saved.vmdk"
                backend.cli("apply", "--workspace", session, "--output", vmdk)
                readback2 = root / "vmdk-readback.bin"
                backend.cli("file", "get", vmdk, "--source", "/tree/data.bin", "--destination", readback2)
                assert readback2.read_bytes() == payload.read_bytes()
            edit("remove", "--path", "/tree/data.bin")
            edit("remove", "--path", "/tree")
            deleted = root / "deleted.img"
            backend.cli("apply", "--workspace", session, "--output", deleted)
            assert digest(source) == source_hash
            records.append({"case": root.name, "filesystem": fs, "table": table, "size": size,
                "raw_source": str(raw.relative_to(directory)).replace("\\", "/"),
                "saved": str(saved.relative_to(directory)).replace("\\", "/"),
                "deleted": str(deleted.relative_to(directory)).replace("\\", "/"),
                "payload": str(payload.relative_to(directory)).replace("\\", "/"),
                "source_sha256": source_hash, "full_edit_sequence": complete})
            print(f"PASS native creation/edit {root.name}", flush=True)
    report = {"schema_version": 1, "status": "pass", "producer": "native Windows x64" if backend.windows else "Linux",
              "guards": ["explicit opt-in", "minimum size", "sector alignment", "existing output unchanged",
                         "dependency failure leaves no output or temporary files"], "cases": records}
    (directory / "producer.json").write_text(json.dumps(report, indent=2)+"\n", encoding="utf-8")
    return report


def partition_bounds(image, size, table, fs):
    assert image.stat().st_size == size
    with image.open("rb") as stream:
        mbr = stream.read(512)
        if table == "none":
            return 0, size
        assert mbr[510:512] == b"\x55\xaa"
        if table == "mbr":
            assert mbr[450] == {"ext4": 0x83, "exfat": 0x07, "fat32": 0x0c}[fs]
            start, sectors = struct.unpack_from("<II", mbr, 454)
            assert start == 2048 and start+sectors == size//512
            return start*512, sectors*512
        assert mbr[450] == 0xee
        headers = []
        for sector in (1, size//512-1):
            stream.seek(sector*512)
            h = stream.read(512)
            assert h[:8] == b"EFI PART"
            length, expected = struct.unpack_from("<II", h, 12)
            raw = bytearray(h[:length]);raw[16:20] = bytes(4)
            assert zlib.crc32(raw) == expected
            current, alternate = struct.unpack_from("<QQ", h, 24)
            assert current == sector and alternate == (size//512-1 if sector == 1 else 1)
            array, count, width, crc = struct.unpack_from("<QIII", h, 72)
            stream.seek(array*512);entries = stream.read(count*width)
            assert zlib.crc32(entries) == crc
            headers.append((h[56:72], entries))
        assert headers[0] == headers[1]
        entry = headers[0][1]
        expected = "0fc63daf-8483-4772-8e79-3d69d8477de4" if fs == "ext4" else "ebd0a0a2-b9e5-4433-87c0-68b6b72699c7"
        assert uuid.UUID(bytes_le=entry[:16]) == uuid.UUID(expected)
        assert uuid.UUID(bytes_le=entry[16:32]).version == 4
        assert uuid.UUID(bytes_le=headers[0][0]).version == 4
        start, end = struct.unpack_from("<QQ", entry, 32)
        assert start == 2048 and end == size//512-34
        return start*512, (end-start+1)*512


def verify(directory, fsck_exfat, scratch):
    producer = json.loads((directory / "producer.json").read_text())
    identities = set()
    results = []
    for item in producer["cases"]:
        fs, size, table = item["filesystem"], item["size"], item["table"]
        checks = []
        for key in ("raw_source", "saved", "deleted"):
            image = directory / item[key]
            offset, length = partition_bounds(image, size, table, fs)
            volume = scratch / "volume.img"
            with image.open("rb") as src, volume.open("wb") as dst:
                src.seek(offset)
                remaining = length
                while remaining:
                    data = src.read(min(1 << 20, remaining))
                    assert data
                    if data == bytes(len(data)):
                        dst.seek(len(data), 1)
                    else:
                        dst.write(data)
                    remaining -= len(data)
                dst.truncate(length)
            if fs == "ext4":
                command("e2fsck", "-fn", volume)
                with volume.open("rb") as stream:
                    stream.seek(1024);sb = stream.read(1024)
                    assert sb[56:58] == b"\x53\xef" and struct.unpack_from("<I", sb, 24)[0] == 2
                    blocks = struct.unpack_from("<I", sb, 4)[0]
                    assert blocks*4096 <= length and struct.unpack_from("<H", sb, 88)[0] == 256
                    compat, incompat, ro = struct.unpack_from("<III", sb, 92)
                    assert compat & 4 and incompat & 0x40 and ro & 0x400
                    identity = uuid.UUID(bytes=sb[104:120])
                    assert identity.version == 4
                    journal = int(debug(volume, "bmap <8> 0").strip())
                    stream.seek(journal*4096);jsb = stream.read(64)
                    assert struct.unpack_from(">I", jsb)[0] == 0xc03b3998
                    assert jsb[48:64] == sb[104:120] and jsb[28:36] == bytes(8)
                if blocks > 32768:
                    command("e2fsck", "-fn", "-b", "32768", "-B", "4096", volume)
                assert b"Mode:  0700" in debug(volume, "stat /lost+found")
                if key == "saved":
                    assert debug(volume, "cat /tree/data.bin") == (directory / item["payload"]).read_bytes()
                else:
                    assert b"tree" not in debug(volume, "ls -p /")
            else:
                command(fsck_exfat, "-n", volume)
                reader = ExfatReader(volume)
                assert reader.upcase() == (0xe619d30d, 5836)
                boot = reader.read(0, 24*512)
                assert struct.unpack_from("<Q", boot, 64)[0] == offset//512
                identity = struct.unpack_from("<I", boot, 100)[0]
                for start in (0, 12*512):
                    value = 0
                    for i, byte in enumerate(boot[start:start+11*512]):
                        if i not in (106, 107, 112):
                            value = (((value >> 1) | (value << 31))+byte) & 0xffffffff
                    assert boot[start+11*512:start+12*512] == struct.pack("<I", value)*128
                if key == "saved":
                    assert reader.file("/tree/data.bin") == (directory / item["payload"]).read_bytes()
                else:
                    assert "tree" not in reader.entries(reader.root)
            if key == "raw_source":
                assert (fs, identity) not in identities
                identities.add((fs, identity))
            checks.append(f"{key}: table/type, consistency, identity, metadata and content")
        results.append({"case": item["case"], "status": "pass", "checks": checks})
        print(f"PASS independent format check {item['case']}", flush=True)
    return {"schema_version": 1, "status": "pass", "producer": producer["producer"], "checker": "Linux independent tools",
            "guards": producer["guards"], "cases": results}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/release/xetch"))
    parser.add_argument("--directory", type=Path)
    parser.add_argument("--check-directory", type=Path)
    parser.add_argument("--qemu-directory", type=Path, default=Path("artifacts/windows/qemu"))
    parser.add_argument("--fsck-exfat", default=os.environ.get("XETCH_FSCK_EXFAT", "fsck.exfat"))
    parser.add_argument("--produce-only", action="store_true")
    parser.add_argument("--large-volume", action="store_true")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if bool(args.directory) == bool(args.check_directory):
        parser.error("supply exactly one of --directory or --check-directory")
    if args.directory:
        report = produce(Backend(args.binary, args.qemu_directory), args.directory.resolve(), args.large_volume)
    if not args.produce_only:
        with tempfile.TemporaryDirectory(prefix="xetch-format-check-") as scratch:
            report = verify((args.check_directory or args.directory).resolve(), args.fsck_exfat, Path(scratch))
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2)+"\n", encoding="utf-8")
