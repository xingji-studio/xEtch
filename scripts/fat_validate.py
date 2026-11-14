#!/usr/bin/env python3
"""Independent FAT12/16/32 directory/LFN regressions, Linux or Windows from WSL."""
import argparse
import json
from pathlib import Path

from extended_validate import command, digest
from format_validate import Backend


def validate(binary, directory):
    directory.mkdir(parents=True, exist_ok=False)
    backend = Backend(binary, Path("artifacts/windows/qemu"))
    def cli(*args, **kwargs):
        return backend.cli(*args, experimental=False, **kwargs)
    results = []
    for bits, mib in [(12, 2), (16, 32), (32, 64)]:
        root = directory / f"fat{bits}"
        root.mkdir()
        source = root / "source.img"
        with source.open("xb") as stream:
            stream.truncate(mib << 20)
        command("mkfs.fat", "-F", str(bits), source)
        original = digest(source)
        ws = root / "session"
        cli("workspace", "create", source, "--workspace", ws)
        payload = root / "payload.bin"
        payload.write_bytes("native FAT Unicode 中文\n".encode())
        def edit(op, *args, fail=False):
            before = (ws / "workspace.json").read_bytes()
            entries = set(ws.iterdir())
            result = cli("file", op, "--workspace", ws, *args, fail=fail)
            if fail:
                assert (ws / "workspace.json").read_bytes() == before and set(ws.iterdir()) == entries
            return result
        def export(name):
            image = root / name
            cli("apply", "--workspace", ws, "--output", image)
            command("fsck.fat", "-n", image)
            return image
        edit("mkdir", "--path", "/parent")
        edit("mkdir", "--path", "/parent/child")
        edit("put", "--source", payload, "--destination", "/parent/child/内容.txt")
        edit("put", "--source", payload, "--destination", "/parent/child/unsupported-😀.txt", fail=True)
        first = export("created.img")
        assert command("mcopy", "-i", first, "::/parent/child/内容.txt", "-") == payload.read_bytes()
        edit("rename", "--source", "/parent/child/内容.txt", "--destination", "/parent/renamed.txt")
        edit("rename", "--source", "/parent/child", "--destination", "/parent/renamed-dir")
        edit("rename", "--source", "/parent/renamed-dir", "--destination", "/moved", fail=True)
        edit("remove", "--path", "/parent", fail=True)
        saved = export("saved.img")
        assert command("mcopy", "-i", saved, "::/parent/renamed.txt", "-") == payload.read_bytes()
        for path in ["/parent/renamed.txt", "/parent/renamed-dir", "/parent"]:
            edit("remove", "--path", path)
        export("deleted.img")
        assert cli("file", "list", "--workspace", ws)["entries"] == []
        assert digest(source) == original
        results.append({"filesystem": f"FAT{bits}", "status": "pass", "checks": [
            "root_and_nested_dot_entries_independent_fsck", "unicode_lfn_independent_mtools_readback",
            "file_move_and_directory_rename", "rejected_operations_preserve_checkpoint",
            "delete_directory_tree_independent_fsck", "source_hash_unchanged",
            "unsupported_non_bmp_name_preserves_checkpoint"]})
        print(f"PASS FAT{bits} directory, LFN and rollback", flush=True)
    return {"schema_version": 1, "status": "pass", "producer": "native Windows x64" if backend.windows else "Linux",
            "checker": "Linux fsck.fat and mtools", "filesystems": results}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/release/xetch"))
    parser.add_argument("--directory", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    report = validate(args.binary, args.directory.resolve())
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2)+"\n", encoding="utf-8")
