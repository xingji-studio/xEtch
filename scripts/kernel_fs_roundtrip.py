#!/usr/bin/env python3
"""Privileged Linux driver checks restricted to a marked disposable image directory.

Run with sudo (or WSL --user root) and a JSON request from
deep_filesystems_validate.py. Never accepts a block device or a pre-existing mount.
"""
import argparse
import hashlib
import json
import os
import re
from pathlib import Path
import shutil
import stat
import struct
import subprocess
import tempfile


def run(*args):
    result = subprocess.run(list(map(str, args)), capture_output=True, text=True, timeout=180)
    if result.returncode:
        raise RuntimeError(f"{args}: {result.stdout}\n{result.stderr}")
    return result.stdout


def relative(root, name):
    path = Path(name)
    if path.is_absolute() or not path.parts or any(p in (".", "..") for p in path.parts):
        raise ValueError("Expected a relative guest path")
    resolved = (root / path).resolve()
    if not resolved.is_relative_to(root):
        raise ValueError("Guest path escapes the mounted image")
    return resolved


def crc32c(data, crc=0xffffffff):
    for value in data:
        crc ^= value
        for _ in range(8):
            crc = (crc >> 1) ^ (0x82f63b78 if crc & 1 else 0)
    return crc


def journal_info(image):
    with image.open("rb") as stream:
        stream.seek(1024); sb=stream.read(1024)
        bs=1024 << struct.unpack_from("<I",sb,24)[0]
        physical=int(run("debugfs","-R","bmap <8> 0",image).strip())
        stream.seek(physical*bs); data=stream.read(bs)
    features=struct.unpack_from(">I",data,40)[0]
    return {"block_size":bs,"journal_block":physical,"features":features,
            "checksum_version":3 if features&0x10 else 2 if features&8 else 0,
            "journal_64bit":bool(features&2)}


def prepare_checksum_v2(image):
    info=journal_info(image)
    with image.open("r+b") as stream:
        stream.seek(info["journal_block"]*info["block_size"])
        block=bytearray(stream.read(info["block_size"]))
        assert struct.unpack_from(">I",block,28)[0]==0
        assert info["checksum_version"]==3
        # A clean empty log can be switched without translating old records:
        # s_start=0 means stale transactions are not candidates for replay.
        features=(info["features"]&~0x10)|8
        struct.pack_into(">I",block,40,features)
        block[252:256]=bytes(4)
        struct.pack_into(">I",block,252,crc32c(block[:1024]))
        stream.seek(info["journal_block"]*info["block_size"]); stream.write(block)
        stream.flush(); os.fsync(stream.fileno())
    assert journal_info(image)["checksum_version"]==2


def fragment_fixture(mount, unit=64<<10):
    """Fill with interleaved durable allocations, delete every other file.

    Keeping the tail below 2 MiB forces the later 8 MiB file to use holes.
    The caller independently measures its physical allocation after unmount.
    """
    if unit not in (4096,64<<10): raise ValueError("Unsupported fragmentation allocation unit")
    anchors=mount/"anchors"; holes=mount/"holes"
    anchors.mkdir(); holes.mkdir(); (mount/"pool").mkdir()
    expected={}
    count=0
    while count<(8192 if unit==4096 else 1024):
        stats=os.statvfs(mount)
        if stats.f_bavail*stats.f_frsize < (1<<20)+unit*2: break
        for directory,color in [(anchors,(count*17+3)%256),(holes,(count*29+7)%256)]:
            path=directory/f"{count:04d}.bin"
            payload=bytes([color])*unit
            with path.open("xb") as stream:
                stream.write(payload); stream.flush(); os.fsync(stream.fileno())
            if directory==anchors:
                expected[f"anchors/{count:04d}.bin"]={"size":unit,"sha256":hashlib.sha256(payload).hexdigest()}
        count+=1
    assert count>=32, "Fixture did not produce enough interleaved allocations"
    for path in holes.iterdir(): path.unlink()
    holes.rmdir()
    payload=bytes(range(256))*4096
    digest=hashlib.sha256()
    with (mount/"fragmented.bin").open("xb") as stream:
        for _ in range(8): stream.write(payload); digest.update(payload)
        stream.flush(); os.fsync(stream.fileno())
    expected["fragmented.bin"]={"size":8<<20,"sha256":digest.hexdigest()}
    return {"anchors":count,"unit_size":unit,"expected":expected}


def inode_disk_offset(image, inode):
    """Locate an inode in a private test image, without mounting it."""
    with image.open("rb") as stream:
        stream.seek(1024); sb=stream.read(1024)
        assert struct.unpack_from("<H",sb,56)[0]==0xef53
        bs=1024 << struct.unpack_from("<I",sb,24)[0]
        count=struct.unpack_from("<I",sb,0)[0]
        per_group=struct.unpack_from("<I",sb,40)[0]
        size=struct.unpack_from("<H",sb,88)[0]
        assert 0<inode<=count and per_group and 128<=size<=bs
        group,index=divmod(inode-1,per_group)
        wide=bool(struct.unpack_from("<I",sb,96)[0]&0x80)
        desc_size=struct.unpack_from("<H",sb,254)[0] if wide else 32
        assert 32<=desc_size<=bs
        stream.seek((2 if bs==1024 else 1)*bs+group*desc_size)
        desc=stream.read(desc_size)
        table=struct.unpack_from("<I",desc,8)[0]
        if wide: table|=struct.unpack_from("<I",desc,40)[0]<<32
        offset=table*bs+index*size
        assert offset+size<=image.stat().st_size
        return offset,bs,size


def force_journal_replay(image, source, root, corrupt_tag=False, restore_inode=None):
    source = Path(source).resolve(strict=True)
    if not source.is_relative_to(root) or not source.is_file():
        raise ValueError("Replay reference must be a private image")
    def block(inode, logical):
        text = run("debugfs", "-R", f"bmap <{inode}> {logical}", image).strip()
        return int(text)
    with image.open("r+b") as stream:
        stream.seek(1024); sb = bytearray(stream.read(1024))
        bs = 1024 << struct.unpack_from("<I", sb, 24)[0]
        journal_block = block(8, 0)
        stream.seek(journal_block*bs); jsb = bytearray(stream.read(bs))
        first = struct.unpack_from(">I", jsb, 20)[0]
        stream.seek(block(8, first)*bs); descriptor = stream.read(bs)
        assert descriptor[:8] == bytes.fromhex("c03b399800000001")
        tid = struct.unpack_from(">I", descriptor, 8)[0]
        if corrupt_tag:
            descriptor=bytearray(descriptor)
            features=struct.unpack_from(">I",jsb,40)[0]
            assert features&0x18
            offset=24 if features&0x10 else 16
            descriptor[offset]^=1
            descriptor[-4:]=bytes(4)
            seed=crc32c(jsb[48:64])
            struct.pack_into(">I",descriptor,bs-4,crc32c(descriptor,seed))
            stream.seek(block(8,first)*bs); stream.write(descriptor)
        struct.pack_into(">II", jsb, 24, tid, first)
        if struct.unpack_from(">I", jsb, 40)[0] & 0x18:
            jsb[252:256] = bytes(4)
            struct.pack_into(">I", jsb, 252, crc32c(jsb[:1024]))
        stream.seek(journal_block*bs); stream.write(jsb)
        # Roll back home metadata. Correct names/inode fields must then be
        # recovered by the real kernel replaying the backend's last journal.
        if restore_inode:
            if not re.fullmatch(r"[A-Za-z0-9_-]+\.bin",restore_inode): raise ValueError("Invalid replay file")
            text=run("debugfs","-R",f"stat /{restore_inode}",image)
            inode=int(re.search(r"Inode:\s*(\d+)",text).group(1))
            offset,inode_bs,_=inode_disk_offset(image,inode)
            assert inode_bs==bs and inode_disk_offset(source,inode)[0]==offset
            root_block=offset//bs
        else:
            root_block = block(2, 0)
        with source.open("rb") as original:
            original.seek(root_block*bs); old = original.read(bs)
        stream.seek(root_block*bs); stream.write(old)
        features = struct.unpack_from("<I", sb, 96)[0] | 4
        struct.pack_into("<I", sb, 96, features)
        if struct.unpack_from("<I", sb, 100)[0] & 0x400:
            struct.pack_into("<I", sb, 1020, crc32c(sb[:1020]))
        stream.seek(1024); stream.write(sb)
        stream.flush(); os.fsync(stream.fileno())


def validate(request):
    root = Path(request["root"]).resolve(strict=True)
    marker = json.loads((root / ".xetch-fs-validation.json").read_text())
    if marker != {"purpose": "xetch-filesystem-validation-v1", "token": request["token"]}:
        raise ValueError("Missing or mismatched validation marker")
    image = Path(request["image"]).resolve(strict=True)
    if not image.is_relative_to(root) or not stat.S_ISREG(image.stat().st_mode):
        raise ValueError("Only regular image files inside the marked test directory are allowed")
    filesystem = request["filesystem"]
    if filesystem not in ("exfat", "ext4") or os.geteuid() != 0:
        raise ValueError("Linux root with exfat/ext4 is required")
    if request.get("format"):
        if filesystem == "ext4":
            bs=request.get("ext_block_size",4096)
            if bs not in (1024,2048,4096): raise ValueError("Unsupported test block size")
            options=["-O","^64bit"] if request.get("journal_64bit") is False else []
            run("mkfs.ext4", "-q", "-F", "-b",bs,*options,"-E", "lazy_itable_init=0,lazy_journal_init=0", image)
        else:
            tool = request.get("mkfs_exfat") or shutil.which("mkfs.exfat")
            if not tool:
                raise RuntimeError("Install exfatprogs or pass mkfs_exfat")
            run(tool, image)
    if filesystem == "exfat":
        run("modprobe", "exfat")
    if request.get("force_replay_source"):
        if filesystem != "ext4":
            raise ValueError("JBD2 replay applies only to ext4")
        force_journal_replay(image, request["force_replay_source"], root,request.get("corrupt_journal_tag",False),request.get("force_replay_inode_table"))
        missing=request.get("force_replay_expect_missing")
        if missing:
            result=subprocess.run(["debugfs","-R",f"stat /{missing}",str(image)],capture_output=True,text=True,timeout=30)
            assert "Inode:" not in result.stdout and "not found" in result.stderr
    journal_before=journal_info(image) if filesystem=="ext4" else None
    mount = Path(tempfile.mkdtemp(prefix="xetch-kernel-mount-"))
    mounted = False
    checks = []
    if request.get("force_replay_source"):
        checks.append("kernel_replays_backend_journal_after_home_metadata_rollback")
    try:
        run("mount", "-t", filesystem, "-o", "loop,nosuid,nodev,noexec,noatime", image, mount)
        mounted = True
        fixture=fragment_fixture(mount,request.get("fragment_unit",64<<10)) if request.get("fragment_fixture") else None
        for name, expected in request.get("expect", {}).items():
            path = relative(mount, name)
            with path.open("rb") as stream:
                digest = hashlib.sha256()
                for block in iter(lambda: stream.read(1 << 20), b""):
                    digest.update(block)
            if path.stat().st_size != expected["size"] or digest.hexdigest() != expected["sha256"]:
                raise AssertionError(f"Kernel readback differs: {name}")
            checks.append(f"kernel_read:{name}")
        if "expect_directories" in request:
            actual_dirs=set(); actual_files=set()
            for parent,dirs,files in os.walk(mount):
                parent=Path(parent)
                actual_dirs.update((parent/name).relative_to(mount).as_posix() for name in dirs)
                actual_files.update((parent/name).relative_to(mount).as_posix() for name in files)
            assert actual_dirs==set(request["expect_directories"]),(actual_dirs^set(request["expect_directories"]))
            assert actual_files==set(request.get("expect",{})),(actual_files^set(request.get("expect",{})))
            checks.append("kernel_exact_full_directory_and_file_namespace")
        for name, content in request.get("write", {}).items():
            path = relative(mount, name)
            path.parent.mkdir(parents=True, exist_ok=True)
            with path.open("wb") as stream:
                stream.write(bytes.fromhex(content))
                stream.flush()
                os.fsync(stream.fileno())
            checks.append(f"kernel_write:{name}")
        for old, new in request.get("rename", []):
            relative(mount, old).rename(relative(mount, new))
            checks.append(f"kernel_rename:{old}")
        for old,new in request.get("links",[]):
            os.link(relative(mount,old),relative(mount,new))
            checks.append(f"kernel_hardlink:{old}")
        for name,metadata in request.get("metadata",{}).items():
            path=relative(mount,name)
            if "mode" in metadata: path.chmod(metadata["mode"])
            if "uid" in metadata or "gid" in metadata:
                os.chown(path,metadata.get("uid",-1),metadata.get("gid",-1))
            checks.append(f"kernel_metadata:{name}")
        for name in request.get("unlink",[]):
            relative(mount,name).unlink()
            checks.append(f"kernel_unlink:{name}")
        run("sync", "-f", mount)
    finally:
        if mounted:
            # Never recursively delete a mountpoint when unmount failed.
            run("umount", mount)
        mount.rmdir()
    journal_after=journal_info(image) if filesystem=="ext4" else None
    if request.get("prepare_checksum_v2"):
        if filesystem!="ext4": raise ValueError("JBD2 only applies to ext4")
        prepare_checksum_v2(image)
    result={"status": "pass", "driver": f"Linux {os.uname().release} {filesystem}", "checks": checks}
    if fixture: result["fragment_fixture"]=fixture
    if filesystem=="ext4":
        result.update(journal_before_mount=journal_before,journal_after_unmount=journal_after,
                      final_journal=journal_info(image))
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--request", type=Path, required=True)
    args = parser.parse_args()
    print(json.dumps(validate(json.loads(args.request.read_text()))))
