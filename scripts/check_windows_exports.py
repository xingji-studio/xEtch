#!/usr/bin/env python3
"""Independently check native-Windows exports from a Linux host."""
import argparse
import json
from pathlib import Path
import subprocess

from extended_validate import ExfatReader
from format_validate import partition_bounds


def command(*args):
    result = subprocess.run(list(map(str, args)), capture_output=True, timeout=60)
    if result.returncode:
        raise RuntimeError(result.stdout.decode(errors="replace") + result.stderr.decode(errors="replace"))
    return result.stdout


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--directory", required=True, type=Path)
    parser.add_argument("--fsck-exfat", default="fsck.exfat")
    parser.add_argument("--gui-directory", type=Path,
                        help="GUI exports under ntfs/, exfat/ and ext4/ subdirectories")
    parser.add_argument("--gui-create-directory", type=Path,
                        help="GUI-created exFAT/ext4 images and reports under exfat/ and ext4/")
    parser.add_argument("--gui-create-fat32", action="store_true",
                        help="Also require and check fat32/ under --gui-create-directory")
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    command(args.fsck_exfat, "-n", args.directory / "exfat-saved.img")
    assert ExfatReader(args.directory / "exfat-saved.img").file("/new-dir/data.txt") == (args.directory / "exfat-payload.txt").read_bytes()
    command("e2fsck", "-fn", args.directory / "ext4-saved.img")
    contents = command("debugfs", "-R", "cat /new-dir/data.txt", args.directory / "ext4-saved.img")
    assert contents == (args.directory / "ext4-payload.txt").read_bytes()
    command("ntfsfix", "-n", args.directory / "ntfs-saved.img")
    assert command("ntfscat", args.directory / "ntfs-saved.img", "/new-dir/data.txt") == (args.directory / "ntfs-payload.txt").read_bytes()
    checks = ["exFAT: exfatprogs consistency and independent file readback", "ext4: e2fsck consistency check", "ext4: independent debugfs file readback", "NTFS: ntfsfix -n check and ntfscat content readback"]
    if (args.directory / "gui-saved.img").exists():
        command("ntfsfix", "-n", args.directory / "gui-saved.img")
        assert command("ntfscat", args.directory / "gui-saved.img", "/gui-created/renamed.txt") == (args.directory / "payload.txt").read_bytes()
        checks.append("WinUI NTFS output: independent consistency and file readback")
    if args.gui_directory:
        for fs in ("exfat", "ext4", "ntfs"):
            folder = args.gui_directory / fs
            image = folder / "gui-saved.img"
            expected = (folder / "payload.txt").read_bytes()
            path = "/gui-created/renamed.txt"
            if fs == "exfat":
                command(args.fsck_exfat, "-n", image)
                actual = ExfatReader(image).file(path)
            elif fs == "ext4":
                command("e2fsck", "-fn", image)
                actual = command("debugfs", "-R", f"cat {path}", image)
            else:
                command("ntfsfix", "-n", image)
                actual = command("ntfscat", image, path)
            assert actual == expected, fs
            checks.append(f"WinUI {fs} output: independent consistency and file readback")
    if args.gui_create_directory:
        for fs in ("exfat", "ext4", *(["fat32"] if args.gui_create_fat32 else [])):
            folder = args.gui_create_directory / fs
            report = json.loads((folder / "gui-results.json").read_text(encoding="utf-8-sig"))
            assert report["status"] == "pass" and report["source_unchanged"] and report["created_image"]
            for filename in ("gui-created.img", "gui-saved.img"):
                image = folder / filename
                offset, length = partition_bounds(image, image.stat().st_size, report["table"], fs)
                volume = folder / (filename+"-volume.img")
                with image.open("rb") as src, volume.open("wb") as dst:
                    src.seek(offset)
                    # GPT's trailer belongs to the image, outside the filesystem.
                    remaining = length
                    while remaining:
                        data = src.read(min(1 << 20, remaining));assert data
                        dst.write(data);remaining -= len(data)
                command(*([args.fsck_exfat, "-n"] if fs == "exfat" else ["fsck.fat", "-n"] if fs == "fat32" else ["e2fsck", "-fn"]), volume)
                if filename == "gui-saved.img":
                    path = "/gui-created/renamed.txt"
                    data = ExfatReader(volume).file(path) if fs == "exfat" else command("mcopy", "-i", volume, "::"+path, "-") if fs == "fat32" else command("debugfs", "-R", f"cat {path}", volume)
                    assert data == (folder / "payload.txt").read_bytes()
            checks.append(f"WinUI new {fs} image: partition type, fresh/edited consistency and file readback")
    report = {"status": "pass", "producer": "native Windows xEtch", "checker_host": "Linux",
              "checks": checks}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2)+"\n", encoding="utf-8")
    print(json.dumps(report, indent=2))
