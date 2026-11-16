#!/usr/bin/env python3
"""Build a pinned, independent e2fsck oracle; never link it into the BSD worker."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
VERSION = "1.47.4"
SOURCE = "https://www.kernel.org/pub/linux/kernel/people/tytso/e2fsprogs/v1.47.4/e2fsprogs-1.47.4.tar.gz"
SHA256 = "da274408bebbfd13a5a2fc3cfc66e3ffff17c48534673aa67f88d49b99123b96"

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output",type=Path,default=ROOT/"artifacts/tools/e2fsprogs")
    parser.add_argument("--offline",action="store_true")
    args=parser.parse_args()
    archive=ROOT/"artifacts/downloads"/SOURCE.rsplit("/",1)[-1]
    archive.parent.mkdir(parents=True,exist_ok=True)
    if not archive.exists():
        if args.offline:raise RuntimeError("Pinned e2fsprogs archive is not cached")
        urllib.request.urlretrieve(SOURCE,archive)
    assert hashlib.sha256(archive.read_bytes()).hexdigest()==SHA256,"e2fsprogs archive hash mismatch"
    output=args.output.resolve();output.mkdir(parents=True,exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="xetch-e2fsck-build-") as td:
        build=Path(td)
        with tarfile.open(archive) as tar:
            assert all(not p.name.startswith("/") and ".." not in Path(p.name).parts for p in tar.getmembers())
            tar.extractall(build)
        source=build/("e2fsprogs-"+VERSION)
        options=["./configure","--disable-nls","--disable-fuse2fs","--disable-uuidd"]
        subprocess.run(options,cwd=source,check=True)
        subprocess.run(["make","-j4"],cwd=source,check=True)
        (output/"sbin").mkdir(exist_ok=True)
        shutil.copy2(source/"e2fsck/e2fsck",output/"sbin/e2fsck")
    binary=output/"sbin/e2fsck"
    (output/"validation-tool-profile.json").write_text(json.dumps({"version":VERSION,"source":SOURCE,"source_sha256":SHA256,
        "binary_sha256":hashlib.sha256(binary.read_bytes()).hexdigest(),"role":"independent test oracle; excluded from application packages"},indent=2)+"\n")
    subprocess.run([str(binary),"-V"],check=True)
    print(binary)

if __name__=="__main__":main()
