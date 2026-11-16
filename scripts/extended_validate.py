#!/usr/bin/env python3
"""Independent exFAT/ext4 integration tests on Linux and native Windows from WSL.

No mounts; mkfs.exfat/mkfs.ext4 create fixtures, fsck.exfat/e2fsck/debugfs and
a separate small exFAT reader verify saved bytes. Use a fresh directory.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import struct
import subprocess


def command(*args, accepted=(0,)):
    p = subprocess.run(list(map(str, args)), capture_output=True, timeout=180)
    if p.returncode not in accepted:
        raise RuntimeError(f"{args}: {p.stdout.decode(errors='replace')}\n{p.stderr.decode(errors='replace')}")
    return p.stdout


def digest(path):
    with path.open("rb") as stream:
        value = hashlib.sha256()
        for data in iter(lambda: stream.read(1 << 20), b""):
            value.update(data)
        return value.hexdigest()


def checksum(data):
    value = 0
    for i, byte in enumerate(data):
        if i not in (2, 3):
            value = (((value >> 1) | (value << 15)) + byte) & 0xffff
    return value


class ExfatReader:
    """Independent directory/stream parser, used only on test artifacts."""
    def __init__(self, image, offset=0):
        self.image, self.offset = image, offset
        boot = self.read(0, 512)
        assert boot[3:11] == b"EXFAT   "
        self.sector = 1 << boot[108]
        self.cluster = self.sector << boot[109]
        self.fat = struct.unpack_from("<I", boot, 80)[0] * self.sector
        self.heap = struct.unpack_from("<I", boot, 88)[0] * self.sector
        self.root = struct.unpack_from("<I", boot, 96)[0]

    def read(self, offset, length):
        with self.image.open("rb") as f:
            f.seek(self.offset + offset)
            result = f.read(length)
        assert len(result) == length
        return result

    def clusters(self, first, flags=0, length=0):
        if first == 0:
            return []
        if flags & 2:
            return list(range(first, first + (length + self.cluster - 1) // self.cluster))
        chain = []
        while first < 0xfffffff8:
            assert first >= 2 and first not in chain
            chain.append(first)
            first = struct.unpack("<I", self.read(self.fat + first * 4, 4))[0]
        return chain

    def contents(self, first, flags=0, length=0):
        return b"".join(self.read(self.heap + (c-2)*self.cluster, self.cluster)
                        for c in self.clusters(first, flags, length))

    def entries(self, first, flags=0, length=0):
        data = self.contents(first, flags, length)
        result, pos = {}, 0
        while pos + 32 <= len(data):
            kind = data[pos]
            if kind == 0:
                break
            if kind != 0x85:
                pos += 32
                continue
            size = (1 + data[pos+1]) * 32
            raw = data[pos:pos+size]
            assert len(raw) == size and checksum(raw) == struct.unpack_from("<H", raw, 2)[0]
            units = raw[35]
            name = b"".join(raw[i+2:i+32] for i in range(64, size, 32) if raw[i] == 0xc1)[:units*2].decode("utf-16-le")
            entry = {"name": name, "raw": raw, "first": struct.unpack_from("<I", raw, 52)[0],
                     "length": struct.unpack_from("<Q", raw, 56)[0], "valid": struct.unpack_from("<Q", raw, 40)[0],
                     "flags": raw[33], "directory": bool(raw[4] & 16),
                     "positions": (first, pos, size)}
            result[name] = entry
            pos += size
        return result

    def lookup(self, path):
        first, flags, length = self.root, 0, 0
        for name in path.strip("/").split("/"):
            entry = self.entries(first, flags, length)[name]
            first, flags, length = entry["first"], entry["flags"], entry["length"]
        return entry

    def file(self, path):
        e = self.lookup(path)
        assert not e["directory"]
        data = self.contents(e["first"], e["flags"], e["length"])[:e["length"]]
        return data[:e["valid"]] + bytes(e["length"] - e["valid"])

    def upcase(self):
        data = self.contents(self.root)
        for i in range(0, len(data), 32):
            if data[i] == 0x82:
                return struct.unpack_from("<I", data, i+4)[0], struct.unpack_from("<Q", data, i+24)[0]
        raise AssertionError("missing up-case table")


def debug(image, request, write=False):
    argv = ["debugfs", *(["-w"] if write else []), "-R", request, str(image)]
    result = subprocess.run(argv, capture_output=True, timeout=180)
    # debugfs can return exit code zero after a command-level failure.
    errors = [line for line in result.stderr.decode(errors="replace").splitlines()
              if line.strip() and not line.startswith("debugfs ")]
    if result.returncode or errors:
        raise RuntimeError(f"{argv}: {result.stdout.decode(errors='replace')}\n" + "\n".join(errors))
    return result.stdout


def ext_stat(image, path):
    output = debug(image, f"stat {path}").decode()
    return {key: re.search(pattern, output).group(1) for key, pattern in {
        "inode": r"Inode:\s+(\d+)", "mode": r"Mode:\s+([0-7]+)", "uid": r"User:\s+(\d+)",
        "gid": r"Group:\s+(\d+)", "links": r"Links:\s+(\d+)", "ea_block": r"File ACL:\s+(\d+)"
    }.items()}


def ext_acl(entries):
    # ext4's on-disk version 1 has short base entries and full named entries;
    # it is not the version 2 value used by the userspace getxattr interface.
    return struct.pack("<I", 1) + b"".join(struct.pack("<HH", tag, perm)
        + (struct.pack("<I", ident) if tag in (2, 8) else b"") for tag, perm, ident in entries)


def validate(binary, directory, mkfs_exfat, fsck_exfat, ext_block_size):
    directory.mkdir(parents=True, exist_ok=False)
    windows = binary.suffix.lower() == ".exe"
    env = os.environ.copy()
    def host(value):
        return command("wslpath", "-w", value).decode().strip() if windows else str(value)
    if windows:
        qemu = Path("artifacts/windows/qemu").resolve()
        env["XETCH_QEMU_IMG"] = host(qemu/"qemu-img.exe")
        env["XETCH_QEMU_STORAGE_DAEMON"] = host(qemu/"qemu-storage-daemon.exe")
        env["WSLENV"] = ":".join(filter(None, [env.get("WSLENV", ""), "XETCH_QEMU_IMG", "XETCH_QEMU_STORAGE_DAEMON"]))
    def cli(*args, fail=False, experimental=True):
        argv = [str(binary), "--json", *(["--experimental-filesystems"] if experimental else []),
                *(host(a) if isinstance(a, Path) else str(a) for a in args)]
        result = subprocess.run(argv, env=env, capture_output=True, timeout=180)
        if fail:
            assert result.returncode, args
            return json.loads(result.stderr)["error"]
        if result.returncode:
            raise RuntimeError(f"{args}: {result.stderr.decode(errors='replace')}")
        return json.loads(result.stdout)
    results = []
    for fs in ["exfat", "ext4"]:
        root = directory/fs
        root.mkdir()
        image = root/"source.img"
        payload = root/"payload.bin"
        initial = b"Independent extended validation\n" * 3000
        payload.write_bytes(initial)
        with image.open("xb") as f:
            f.truncate(64 << 20)
        if fs == "exfat":
            command(mkfs_exfat, image)
            prep = root/"prepare"
            cli("workspace", "create", image, "--workspace", prep)
            cli("file", "put", "--workspace", prep, "--source", payload, "--destination", "/seed.txt")
            cli("file", "mkdir", "--workspace", prep, "--path", "/contiguous")
            cli("file", "put", "--workspace", prep, "--source", payload, "--destination", "/contiguous/inside.txt")
            prepared = root/"prepared.img"
            cli("apply", "--workspace", prep, "--output", prepared)
            # Construct a spec-valid contiguous stream independently; FAT
            # entries become unused, and the valid-data tail must read as zero.
            reader = ExfatReader(prepared)
            seed = reader.lookup("/seed.txt")
            chain = reader.clusters(seed["first"])
            assert chain == list(range(chain[0], chain[0]+len(chain)))
            raw = bytearray(seed["raw"])
            raw[4] |= 2  # hidden bit, preserved by rename/replacement
            raw[33] |= 2
            raw[40:48] = struct.pack("<Q", 64)
            raw[2:4] = struct.pack("<H", checksum(raw))
            with prepared.open("r+b") as f:
                for cluster in chain:
                    f.seek(reader.fat + cluster*4);f.write(bytes(4))
                f.seek(reader.heap + (reader.root-2)*reader.cluster + seed["positions"][1]);f.write(raw)
            reader = ExfatReader(prepared)
            entry = reader.lookup("/contiguous")
            directory_chain = reader.clusters(entry["first"])
            assert directory_chain == list(range(directory_chain[0],directory_chain[0]+len(directory_chain)))
            raw = bytearray(entry["raw"]);raw[33] |= 2
            raw[2:4] = struct.pack("<H",checksum(raw))
            with prepared.open("r+b") as f:
                for cluster in directory_chain:
                    f.seek(reader.fat+cluster*4);f.write(bytes(4))
                f.seek(reader.heap+(reader.root-2)*reader.cluster+entry["positions"][1]);f.write(raw)
            command(fsck_exfat, "-n", prepared)
            image = prepared
        else:
            command("mkfs.ext4", "-q", "-F", "-b", ext_block_size, "-E", "lazy_itable_init=0,lazy_journal_init=0", image)
            debug(image, f"write {payload} /seed.txt", True)
            debug(image, "set_inode_field /seed.txt mode 0100640", True)
            debug(image, "set_inode_field /seed.txt uid 65539", True)
            debug(image, "set_inode_field /seed.txt gid 70000", True)
            debug(image, "link /seed.txt /alias.txt", True)
            debug(image, "set_inode_field /seed.txt links_count 2", True)
            debug(image, "symlink /link.txt /seed.txt", True)
            debug(image, "ea_set /seed.txt user.xetch preserved-value", True)
            (root/"large-ea.bin").write_bytes(b"EA payload" * 80)
            debug(image, f"ea_set -f {root/'large-ea.bin'} /seed.txt user.large", True)
            acl = ext_acl([(1,6,0),(2,4,1001),(4,4,0),(16,4,0),(32,0,0)])
            (root/"acl.bin").write_bytes(acl)
            debug(image, f"ea_set -r -f {root/'acl.bin'} /seed.txt system.posix_acl_access", True)
            default_acl = ext_acl([(1,7,0),(2,7,1001),(4,5,0),(16,7,0),(32,0,0)])
            (root/"default-acl.bin").write_bytes(default_acl)
            debug(image, "mkdir /acl-parent", True)
            debug(image, "set_inode_field /acl-parent mode 042770", True)
            debug(image, "set_inode_field /acl-parent gid 70000", True)
            debug(image, f"ea_set -r -f {root/'default-acl.bin'} /acl-parent system.posix_acl_default", True)
            debug(image, f"ea_set -f {root/'large-ea.bin'} /acl-parent user.large", True)
            debug(image, "mkdir /bad-acl", True)
            invalid_acl = bytearray(default_acl);invalid_acl[0] = 2
            (root/"invalid-acl.bin").write_bytes(invalid_acl)
            debug(image, f"ea_set -r -f {root/'invalid-acl.bin'} /bad-acl system.posix_acl_default", True)
            debug(image, f"write {payload} /locked.txt", True)
            debug(image, "set_inode_field /locked.txt flags 0x80010", True)
            empty = root/"empty";empty.write_bytes(b"")
            batch = root/"debugfs.commands"
            batch.write_text("mkdir /indexed\n" + "".join(f"write {empty} /indexed/entry-{i:03d}-long-name\n" for i in range(300)))
            command("debugfs", "-w", "-f", batch, image)
            command("e2fsck", "-fyD", image, accepted=(0,1))
            command("e2fsck", "-fn", image)
        before = digest(image)
        workspace = root/"session"
        cli("workspace", "create", image, "--workspace", workspace)
        manifest = workspace/"workspace.json"
        checks = []
        def passed(name):
            checks.append(name);print(f"PASS {fs}: {name}", flush=True)
        def mutate(op, *args, fail=False):
            old = manifest.read_bytes();files = set(workspace.iterdir())
            result = cli("file", op, "--workspace", workspace, *args, fail=fail)
            if fail:
                assert manifest.read_bytes() == old and set(workspace.iterdir()) == files
            return result
        def export(name):
            output = root/name
            cli("apply", "--workspace", workspace, "--output", output)
            if output.suffix == ".img":
                command(*([fsck_exfat,"-n"] if fs=="exfat" else ["e2fsck","-fn"]), output)
            return output
        error = cli("file", "mkdir", "--workspace", workspace, "--path", "/guard", fail=True, experimental=False)
        assert error["code"] == "unsupported" and json.loads(manifest.read_text())["changes"] == []
        passed("explicit_write_opt_in_and_checkpoint_guard")
        mutate("rename", "--source", "/seed.txt", "--destination", "/renamed-seed.txt")
        mutate("rename", "--source", "/renamed-seed.txt", "--destination", "/seed.txt")
        renamed = export("rename-only.img")
        if fs == "exfat":
            seed_before, seed_after = ExfatReader(image).lookup("/seed.txt"), ExfatReader(renamed).lookup("/seed.txt")
            assert seed_before["raw"][4:32] == seed_after["raw"][4:32]
            assert seed_before["raw"][32:35] + seed_before["raw"][38:64] == seed_after["raw"][32:35] + seed_after["raw"][38:64]
            readback = root/"unwritten-tail.bin"
            cli("file", "get", "--workspace", workspace, "--source", "/seed.txt", "--destination", readback)
            assert readback.read_bytes() == initial[:64] + bytes(len(initial)-64)
            passed("rename_retains_contiguous_stream_attributes_and_valid_data_length")
            child = root/"contiguous-child.bin"
            cli("file","get","--workspace",workspace,"--source","/contiguous/inside.txt","--destination",child)
            assert child.read_bytes() == initial
            mutate("rename","--source","/contiguous","--destination","/contiguous-renamed")
            mutate("put","--source",payload,"--destination","/contiguous-renamed/new.txt")
            passed("contiguous_directory_read_move_and_safe_fat_materialization")
        else:
            assert ext_stat(image, "/seed.txt") == ext_stat(renamed, "/seed.txt")
            passed("rename_retains_inode_ownership_permissions_and_hardlinks")
        for data in [bytes(range(256))*2000, b"", b"final replacement\n"]:
            payload.write_bytes(data)
            mutate("put", "--source", payload, "--destination", "/seed.txt", "--replace")
        final_data = payload.read_bytes()
        mutate("mkdir", "--path", "/tree")
        mutate("mkdir", "--path", "/tree/subdir")
        mutate("mkdir", "--path", "/other")
        mutate("put", "--source", payload, "--destination", "/tree/subdir/内容😀.txt")
        mutate("rename", "--source", "/tree", "--destination", "/renamed-tree")
        mutate("rename", "--source", "/renamed-tree", "--destination", "/other/tree")
        passed("nonempty_directory_rename_and_cross_parent_move")
        mutate("rename", "--source", "/other", "--destination", "/other/tree/cycle", fail=True)
        mutate("remove", "--path", "/other/tree", fail=True)
        mutate("rename", "--source", "/seed.txt", "--destination", "/other/tree", fail=True)
        mutate("put", "--source", payload, "--destination", "/seed.txt", fail=True)
        passed("descendant_conflict_nonempty_directory_rollback")
        if fs == "exfat":
            for name in ["ß.bin", "SS.bin", "ωmega.txt"]:
                mutate("put", "--source", payload, "--destination", "/"+name)
            mutate("put", "--source", payload, "--destination", "/ΩMEGA.TXT", fail=True)
            mutate("rename", "--source", "/ωmega.txt", "--destination", "/ΩMEGA.TXT")
            mutate("put", "--source", payload, "--destination", "/ss.BIN", "--replace")
            passed("volume_upcase_rules_case_only_rename_and_sharp_s_distinction")
            name = "/other/tree/subdir/内容😀.txt"
            for i in range(12):
                dest = "/other/tree/subdir/" + str(i) + "长名字"*35 + ".bin"
                mutate("rename", "--source", name, "--destination", dest)
                name = dest
            mutate("rename", "--source", name, "--destination", "/other/tree/subdir/内容😀.txt")
            passed("entry_set_growth_and_parent_directory_extension")
        else:
            mutate("mkdir", "--path", "/a")
            mutate("mkdir", "--path", "/A")
            mutate("rename", "--source", "/a", "--destination", "/A/inside")
            mutate("put", "--source", payload, "--destination", "/indexed/new.txt")
            mutate("put", "--source", payload, "--destination", "/link.txt", "--replace", fail=True)
            for op, args in [("put",("--source",payload,"--destination","/locked.txt","--replace")),
                             ("remove",("--path","/locked.txt")),("rename",("--source","/locked.txt","--destination","/unlocked.txt"))]:
                assert mutate(op, *args, fail=True)["code"] == "unsupported"
            mutate("mkdir", "--path", "/"+"中文"*100, fail=True)
            passed("case_sensitive_paths_indexed_directory_symlink_and_immutable_guards")
            mutate("put", "--source", payload, "--destination", "/acl-parent/file.txt")
            mutate("mkdir", "--path", "/acl-parent/child")
            mutate("put", "--source", payload, "--destination", "/acl-parent/child/nested.txt")
            assert mutate("mkdir", "--path", "/bad-acl/rejected", fail=True)["code"] == "invalid_argument"
            passed("default_acl_setgid_inheritance_and_invalid_acl_rollback")
        huge = root/"too-large.bin"
        with huge.open("xb") as f:
            f.truncate(80 << 20)
        mutate("put", "--source", huge, "--destination", "/full.bin", fail=True)
        passed("partial_write_out_of_space_rollback")
        output = export("saved.img")
        if fs == "exfat":
            reader = ExfatReader(output)
            assert reader.file("/seed.txt") == final_data
            assert reader.file("/other/tree/subdir/内容😀.txt") == final_data
            assert reader.file("/contiguous-renamed/inside.txt") == initial
            assert reader.file("/contiguous-renamed/new.txt") == initial
            assert reader.lookup("/ß.bin")["first"] != reader.lookup("/SS.bin")["first"]
        else:
            assert debug(output, "cat /seed.txt") == final_data
            assert debug(output, "cat /alias.txt") == final_data
            assert debug(output, "cat /other/tree/subdir/内容😀.txt") == final_data
            assert ext_stat(image,"/seed.txt") == ext_stat(output,"/seed.txt")
            for attribute in ["user.xetch", "user.large", "system.posix_acl_access"]:
                old, new = root/(attribute+"-old.bin"), root/(attribute+"-new.bin")
                debug(image, f"ea_get -r -f {old} /seed.txt {attribute}")
                debug(output, f"ea_get -r -f {new} /seed.txt {attribute}")
                assert old.read_bytes() == new.read_bytes()
            for path, mode in [("/acl-parent/file.txt", "0640"), ("/acl-parent/child", "02750"),
                               ("/acl-parent/child/nested.txt", "0640")]:
                attrs = ext_stat(output, path)
                assert int(attrs["mode"], 8) == int(mode, 8) and attrs["gid"] == "70000", attrs
                saved_acl = root/(path.rsplit("/",1)[-1]+"-acl.bin")
                debug(output, f"ea_get -r -f {saved_acl} {path} system.posix_acl_access")
                expected = ext_acl([(1,7 if path.endswith("/child") else 6,0),(2,7,1001),
                                    (4,5,0),(16,5 if path.endswith("/child") else 4,0),(32,0,0)])
                assert saved_acl.read_bytes() == expected
            saved_default = root/"inherited-default.bin"
            debug(output, f"ea_get -r -f {saved_default} /acl-parent/child system.posix_acl_default")
            assert saved_default.read_bytes() == (root/"default-acl.bin").read_bytes()
        passed("independent_consistency_content_and_metadata_verification")
        vmdk = export("saved.vmdk")
        readback = root/"vmdk-readback.bin"
        cli("file", "get", vmdk, "--source", "/other/tree/subdir/内容😀.txt", "--destination", readback)
        assert readback.read_bytes() == final_data
        passed("vmdk_export_readback")
        if fs == "ext4":
            mutate("remove", "--path", "/alias.txt")
            for path in ["/acl-parent/child/nested.txt", "/acl-parent/child", "/acl-parent/file.txt"]:
                mutate("remove", "--path", path)
        mutate("remove", "--path", "/other/tree/subdir/内容😀.txt")
        mutate("remove", "--path", "/other/tree/subdir")
        mutate("remove", "--path", "/other/tree")
        after_delete = export("after-delete.img")
        if fs == "ext4":
            assert debug(after_delete, "cat /seed.txt") == final_data
            assert ext_stat(after_delete, "/seed.txt")["links"] == "1"
        assert digest(image) == before
        passed("delete_empty_directories_hardlink_release_and_source_unchanged")
        if fs == "exfat":
            unsafe = root/"invalid-upcase.img";shutil.copyfile(image,unsafe)
            reader = ExfatReader(unsafe);data = reader.contents(reader.root)
            pos = next(i for i in range(0,len(data),32) if data[i]==0x82)
            with unsafe.open("r+b") as f:
                f.seek(reader.heap+(reader.root-2)*reader.cluster+pos+4)
                f.write(struct.pack("<I",0))
            source_hash = digest(unsafe)
            ws = root/"upcase-session";cli("workspace","create",unsafe,"--workspace",ws)
            prior = (ws/"workspace.json").read_bytes()
            cli("file","mkdir","--workspace",ws,"--path","/no-fallback",fail=True)
            cli("file","list",unsafe,fail=True)
            assert (ws/"workspace.json").read_bytes()==prior and digest(unsafe)==source_hash
            passed("invalid_upcase_refused_without_ascii_fallback")
        if fs == "ext4":
            unsafe = root/"corrupt-xattr.img";shutil.copyfile(image,unsafe)
            block = int(ext_stat(unsafe,"/acl-parent")["ea_block"])
            assert block != 0
            with unsafe.open("r+b") as f:
                f.seek(block*ext_block_size+16);old=f.read(1)
                f.seek(block*ext_block_size+16);f.write(bytes([old[0]^1]))
            source_hash = digest(unsafe)
            ws = root/"xattr-session";cli("workspace","create",unsafe,"--workspace",ws)
            prior = (ws/"workspace.json").read_bytes()
            assert cli("file","mkdir","--workspace",ws,"--path","/acl-parent/rejected",fail=True)["code"] == "invalid_argument"
            assert (ws/"workspace.json").read_bytes()==prior and digest(unsafe)==source_hash
            passed("corrupt_external_xattr_checksum_rejected_before_inheritance")
            # A pending journal must be refused even when the ext superblock
            # still advertises clean state and lacks needs_recovery.
            unsafe = root/"pending-journal.img";shutil.copyfile(image,unsafe)
            block = int(debug(unsafe,"bmap <8> 0").strip())
            with unsafe.open("r+b") as f:
                f.seek(block*ext_block_size+28);f.write(struct.pack(">I",1))
            source_hash = digest(unsafe)
            ws = root/"pending-session";cli("workspace","create",unsafe,"--workspace",ws)
            prior = (ws/"workspace.json").read_bytes()
            error = cli("file","mkdir","--workspace",ws,"--path","/no-replay",fail=True)
            assert error["code"] == "conflict"
            assert cli("file","list",unsafe,fail=True)["code"] == "conflict"
            assert (ws/"workspace.json").read_bytes() == prior and digest(unsafe) == source_hash
            passed("pending_journal_refused_without_implicit_replay_on_read_or_write")
        results.append({"filesystem":fs,"status":"pass","checks":checks,"source_sha256":before})
    for table in ["none","mbr","gpt"]:
        image = directory/f"created-exfat-{table}.img"
        cli("create","--output",image,"--size","64MiB","--filesystem","exfat","--table",table)
        partition = cli("inspect",image)["layout"]["partitions"][0]
        offset = partition["offset"]
        reader = ExfatReader(image,offset)
        assert reader.upcase() == (0xe619d30d,5836)
        assert struct.unpack("<Q", reader.read(64,8))[0] == offset//512
        extracted = directory/f"created-{table}-volume.img"
        with image.open("rb") as f:
            f.seek(offset);extracted.write_bytes(f.read(partition["size"]))
        command(fsck_exfat,"-n",extracted)
        ws = directory/f"create-{table}-session";cli("workspace","create",image,"--workspace",ws)
        data = directory/f"create-{table}.bin";data.write_bytes(b"formatted Unicode check\n")
        cli("file","put","--workspace",ws,"--source",data,"--destination","/ωmega.txt")
        assert cli("file","put","--workspace",ws,"--source",data,"--destination","/ΩMEGA.TXT",fail=True)["code"] == "conflict"
        saved = directory/f"created-{table}-saved.img"
        cli("apply","--workspace",ws,"--output",saved)
        assert ExfatReader(saved,offset).file("/ωmega.txt")==data.read_bytes()
        with saved.open("rb") as f:
            f.seek(offset);(directory/f"created-{table}-edited-volume.img").write_bytes(f.read(partition["size"]))
        command(fsck_exfat,"-n",directory/f"created-{table}-edited-volume.img")
        print(f"PASS exfat format {table}: full Unicode table and partition offset",flush=True)
    return {"schema_version":1,"status":"pass","platform":"native Windows x64" if windows else "Linux",
            "ext4_block_size":ext_block_size,"filesystems":results,
            "format_checks":["exFAT none/MBR/GPT independent fsck", "complete Unicode up-case table", "partition offset", "non-ASCII case collision"]}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary",type=Path,default=Path("target/release/xetch"))
    parser.add_argument("--directory",type=Path,required=True)
    parser.add_argument("--output",type=Path,required=True)
    parser.add_argument("--mkfs-exfat",default=os.environ.get("XETCH_MKFS_EXFAT","mkfs.exfat"))
    parser.add_argument("--fsck-exfat",default=os.environ.get("XETCH_FSCK_EXFAT","fsck.exfat"))
    parser.add_argument("--ext-block-size",type=int,choices=[1024,4096],default=4096)
    args=parser.parse_args()
    report=validate(args.binary.resolve(),args.directory.resolve(),args.mkfs_exfat,args.fsck_exfat,args.ext_block_size)
    args.output.parent.mkdir(parents=True,exist_ok=True)
    args.output.write_text(json.dumps(report,indent=2)+"\n")
