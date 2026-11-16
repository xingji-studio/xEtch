#!/usr/bin/env python3
"""P0 differential, deep-extent, real-kernel, stress and process-boundary validation."""
import argparse
from datetime import datetime,timezone
import json
import os
from pathlib import Path
import re
import shutil
import signal
import struct
import subprocess
import sys
import time

from advanced_filesystems_validate import Advanced,fragment_runs
from deep_filesystems_validate import ROOT,sha
from kernel_fs_roundtrip import inode_disk_offset


class P0(Advanced):
    def __init__(self,args):
        self.p0_suite=args.p0_suite
        args.suite="all"
        super().__init__(args)
        proxy=args.fault_proxy or ROOT/("target/windows/release/examples/qsd_fault_proxy.exe" if self.backend.windows else "target/release/examples/qsd_fault_proxy")
        self.fault_proxy=self.root/"runtime"/proxy.name
        shutil.copy2(proxy,self.fault_proxy)
        probe=args.protocol_probe or ROOT/("target/windows/release/examples/ext4_protocol_probe.exe" if self.backend.windows else "target/release/examples/ext4_protocol_probe")
        self.probe=self.root/"runtime"/probe.name
        shutil.copy2(probe,self.probe)
        self.report.update(suite=self.p0_suite,fault_proxy_sha256=sha(self.fault_proxy),protocol_probe_sha256=sha(self.probe))

    def control(self,*args,**kwargs):
        env=self.backend.env.copy();env["XETCH_EXT4_ENGINE"]="fstool"
        return self.backend.cli(*args,env=env,**kwargs)

    def command(self,*args):
        result=subprocess.run(list(map(str,args)),capture_output=True)
        if result.returncode: raise RuntimeError(result.stdout.decode(errors="replace")+result.stderr.decode(errors="replace"))
        return result.stdout

    def differential(self):
        folder=self.root/"differential";folder.mkdir()
        source=folder/"source.img"
        with source.open("xb") as f:f.truncate(128<<20)
        old=b"shared inode from Linux\n"+bytes(range(256))*1024
        self.kernel("ext4",source,format=True,write={"shared.bin":old.hex(),"dir/child.txt":b"child".hex()},
                    links=[["shared.bin","link.bin"]],metadata={"shared.bin":{"mode":0o640,"uid":70000,"gid":80000}})
        for name in ("sparse.bin","preallocated-empty.bin"):
            self.command("debugfs","-w","-R",f"write /dev/null /{name}",source)
            self.command("debugfs","-w","-R",f"fallocate /{name} 4 127",source)
        self.command("debugfs","-w","-R","sif /sparse.bin size 3145728",source)
        self.consistency("ext4",source)
        original=sha(source);payload=folder/"payload.bin";payload.write_bytes(bytes(range(255,-1,-1))*2049)
        expected={"shared.bin":self.expected(payload.read_bytes()),"link.bin":self.expected(payload.read_bytes()),"moved/child.txt":self.expected(b"child")}
        expected["sparse.bin"]=self.expected(bytes(3<<20))
        for engine,cli in (("fstool",self.control),("lwext4",self.backend.cli)):
            trial=folder/engine;trial.mkdir();ws=trial/"session"
            cli("workspace","create",source,"--workspace",ws)
            cli("file","put","--workspace",ws,"--source",payload,"--destination","/shared.bin","--replace")
            cli("file","rename","--workspace",ws,"--source","/dir","--destination","/moved")
            before=trial/"sparse-before.bin"
            cli("file","get","--workspace",ws,"--source","/sparse.bin","--destination",before)
            assert self.expected(before.read_bytes())==expected["sparse.bin"]
            cli("file","remove","--workspace",ws,"--path","/preallocated-empty.bin")
            image=trial/"saved.img";cli("apply","--workspace",ws,"--output",image)
            self.consistency("ext4",image)
            self.kernel("ext4",image,expect=expected)
            text=self.command("debugfs","-R","stat /shared.bin",image).decode()
            assert "Mode:  0640" in text and "70000" in text and "80000" in text and "Links: 2" in text,text
            for name in expected:
                for reader in (self.control,self.backend.cli):
                    result=trial/(name.replace("/","_")+"-"+str(time.time_ns())+".bin")
                    reader("file","get",image,"--source","/"+name,"--destination",result)
                    assert self.expected(result.read_bytes())==expected[name]
        assert sha(source)==original
        attributed=folder/"xattrs.img";shutil.copyfile(source,attributed)
        self.command("debugfs","-w","-R","ea_set /shared.bin user.p0 retained",attributed)
        self.consistency("ext4",attributed);attributed_hash=sha(attributed)
        guarded=folder/"xattrs-session";self.backend.cli("workspace","create",attributed,"--workspace",guarded)
        before=(guarded/"workspace.json").read_bytes()
        self.backend.cli("file","put","--workspace",guarded,"--source",payload,"--destination","/shared.bin","--replace")
        assert (guarded/"workspace.json").read_bytes()!=before
        self.control("file","put","--workspace",guarded,"--source",payload,"--destination","/shared.bin","--replace")
        kept=folder/"xattrs-kept.img";self.control("apply","--workspace",guarded,"--output",kept)
        self.consistency("ext4",kept)
        assert b"retained" in self.command("debugfs","-R","ea_get /shared.bin user.p0",kept)
        assert sha(attributed)==attributed_hash
        return {"suite":"differential","status":"pass","checks":["identical_kernel_source_for_both_engines","both_engines_replace_and_move","both_engines_read_both_exports",
                "kernel_verifies_all_contents","hardlink_inode_preserved","32_bit_owner_and_mode_preserved","sparse_holes_and_unwritten_extents_read_zero",
                "empty_preallocated_inode_deletion_frees_all_blocks","bsd_worker_preserves_existing_xattrs",
                "explicit_fstool_control_retains_xattrs","independent_fsck","original_source_unchanged"]}

    def extents(self):
        folder=self.root/"deep-extents";folder.mkdir();source=folder/"source.img"
        with source.open("xb") as f:f.truncate(64<<20)
        fixture=self.kernel("ext4",source,format=True,ext_block_size=1024,fragment_fixture=True,fragment_unit=4096)["fragment_fixture"]
        self.consistency("ext4",source)
        info=self.command("debugfs","-R","stat /fragmented.bin",source).decode()
        blocks=[int(value) for value in re.findall(r"\(ETB\d+\):(\d+)",info)]
        assert blocks,info
        with source.open("rb") as f:
            f.seek(blocks[0]*1024);first=f.read(1024)
        depth=struct.unpack_from("<H",first,6)[0]+1
        assert depth>=2,(depth,info)
        runs=fragment_runs("ext4",source);assert runs>336
        original=sha(source);readback=folder/"readback.bin"
        self.backend.cli("file","get",source,"--source","/fragmented.bin","--destination",readback)
        assert self.expected(readback.read_bytes())==fixture["expected"]["fragmented.bin"]
        corrupt=folder/"bad-crc.img";shutil.copyfile(source,corrupt)
        tail=12+struct.unpack_from("<H",first,4)[0]*12
        with corrupt.open("r+b") as f:
            f.seek(blocks[0]*1024+tail);byte=f.read(1);f.seek(-1,1);f.write(bytes([byte[0]^1]))
        fingerprint=sha(corrupt);destination=folder/"rejected.bin"
        error=self.backend.cli("file","get",corrupt,"--source","/fragmented.bin","--destination",destination,fail=True)
        assert error["code"]=="io_error",error
        assert not destination.exists() and sha(corrupt)==fingerprint
        ws=folder/"session";self.backend.cli("workspace","create",source,"--workspace",ws)
        payload=folder/"payload.bin";data=bytes(range(256))*1025+b"tail";payload.write_bytes(data)
        self.backend.cli("file","put","--workspace",ws,"--source",payload,"--destination","/fragmented.bin","--replace")
        saved=folder/"saved.img";self.backend.cli("apply","--workspace",ws,"--output",saved)
        self.consistency("ext4",saved)
        expected=dict(fixture["expected"]);expected["fragmented.bin"]=self.expected(data)
        self.kernel("ext4",saved,expect=expected)
        self.read_file(saved,"/fragmented.bin",data,folder)
        self.backend.cli("file","remove","--workspace",ws,"--path","/fragmented.bin")
        deleted=folder/"deleted.img";self.backend.cli("apply","--workspace",ws,"--output",deleted)
        self.consistency("ext4",deleted);assert sha(source)==original
        return {"suite":"deep-extents","status":"pass","initial_depth":depth,"physical_runs":runs,"anchors":fixture["anchors"],
                "checks":["kernel_generates_depth_two_or_deeper_tree","candidate_reads_all_fragmented_payload","candidate_rejects_bad_external_node_crc",
                          "rejected_download_not_published","deep_truncate_and_new_allocation","kernel_hashes_all_anchors_and_new_payload","delete_frees_tree_and_data",
                          "independent_fsck_after_mutations","original_source_unchanged"]}

    def worker_faults(self):
        folder=self.root/"worker-faults";folder.mkdir();source=folder/"source.img"
        self.backend.cli("create","--output",source,"--size","64MiB","--table","none","--filesystem","ext4")
        original=sha(source);checks=[]
        for mode in ("oversized","out_of_bounds","write_read_only","wrong_magic","exit","old_profile"):
            env=self.backend.env.copy();env.update(XETCH_EXT4_WORKER=self.backend.host(self.probe),XETCH_EXT4_PROBE=mode)
            if self.backend.windows:env["WSLENV"]=env.get("WSLENV","")+":XETCH_EXT4_PROBE"
            error=self.backend.cli("file","list",source,env=env,fail=True)
            assert error["code"] in ("dependency_failure","io_error"),error
            assert sha(source)==original
            checks.append({"mode":mode,"status":"pass"})
        ws=folder/"session";self.backend.cli("workspace","create",source,"--workspace",ws)
        payload=folder/"payload.bin";payload.write_bytes(bytes(range(256))*1024)
        for point in ("ext4_worker.started","ext4_worker.write_ack"):
            evidence=folder/(point+".json");env=self.injected.env.copy()
            env.update(XETCH_VALIDATION_POINT=point,XETCH_VALIDATION_MODE="gate",XETCH_VALIDATION_RECORD=self.injected.host(evidence))
            if self.backend.windows:env["WSLENV"]=env.get("WSLENV","")+":XETCH_VALIDATION_POINT:XETCH_VALIDATION_MODE:XETCH_VALIDATION_RECORD"
            manifest=(ws/"workspace.json").read_bytes()
            command=[str(self.injected.binary),"--json","--experimental-filesystems","file","put","--workspace",self.injected.host(ws),"--source",self.injected.host(payload),"--destination","/uncommitted.bin"]
            process=subprocess.Popen(command,env=env,stdin=subprocess.DEVNULL,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
            try:
                deadline=time.monotonic()+45
                while time.monotonic()<deadline:
                    if process.poll() is not None:raise RuntimeError(process.communicate())
                    try:record=json.loads(evidence.read_text());break
                    except (OSError,ValueError):time.sleep(.01)
                else:raise RuntimeError("Worker gate not reached")
                pid=int(record["path"])
                if self.backend.windows:self.command("/mnt/c/Windows/System32/taskkill.exe","/PID",str(pid),"/F")
                else:os.kill(pid,signal.SIGKILL)
                evidence.with_suffix(".continue").write_text("resume")
                stdout,stderr=process.communicate(timeout=30)
                assert process.returncode and json.loads(stderr)["error"]["code"] in ("io_error","dependency_failure"),(stdout,stderr)
            finally:
                if process.poll() is None:process.kill();process.communicate()
            self.backend.cli("workspace","info","--workspace",ws)
            assert (ws/"workspace.json").read_bytes()==manifest and sha(source)==original
            assert not (ws/".pending-edit.json").exists()
            checks.append({"mode":"worker_only_kill","point":point,"status":"pass"})
        self.backend.cli("file","put","--workspace",ws,"--source",payload,"--destination","/continued.bin")
        checks.append({"mode":"continued_edit_after_worker_crashes","status":"pass"})
        return {"suite":"worker-faults","status":"pass","checks":checks}

    def journal_escape(self):
        results=[]
        for bs,version in ((2048,2),(4096,3)):
            folder=self.root/f"escape-{bs}-v{version}";folder.mkdir();source=folder/"source.img"
            with source.open("xb") as f:f.truncate(128<<20)
            files={f"f{i}.bin":b"filler".hex() for i in range(6)}
            files["magic.bin"]=b"old size".hex()
            self.kernel("ext4",source,format=True,ext_block_size=bs,prepare_checksum_v2=version==2,
                        write=files,unlink=["f5.bin"])
            text=self.command("debugfs","-R","stat /magic.bin",source).decode()
            assert int(re.search(r"Inode:\s*(\d+)",text).group(1))==18,text
            offset,inode_bs,_=inode_disk_offset(source,17)
            assert inode_bs==bs and offset%bs==0
            # Unallocated inode 17 starts the table block containing inode 18.
            # Its unused bytes can validly begin with the JBD2 magic. Updating
            # inode 18 then journals escaped metadata, not ordered file data.
            with source.open("r+b") as f:
                f.seek(offset+20);dtime=struct.unpack("<I",f.read(4))[0]
                f.seek(offset);f.write(bytes.fromhex("c03b3998"))
                f.flush();os.fsync(f.fileno())
            # Recalculate the unused inode's checksum through the independent
            # tool while preserving its deletion timestamp and free status.
            self.command("debugfs","-n","-w","-R",f"sif <17> dtime {dtime}",source)
            with source.open("rb") as f:
                f.seek(offset);assert f.read(4)==bytes.fromhex("c03b3998")
            self.consistency("ext4",source)
            original=sha(source);ws=folder/"session"
            self.backend.cli("workspace","create",source,"--workspace",ws)
            data=bytes.fromhex("c03b3998")+bytes(range(133));payload=folder/"payload.bin";payload.write_bytes(data)
            self.backend.cli("file","put","--workspace",ws,"--source",payload,"--destination","/magic.bin","--replace")
            saved=folder/"saved.img";self.backend.cli("apply","--workspace",ws,"--output",saved)
            self.consistency("ext4",saved)
            with saved.open("rb") as f:
                f.seek(offset);assert f.read(4)==bytes.fromhex("c03b3998")
            journal=folder/"journal.bin"
            self.command("debugfs","-R",f"dump <8> {journal}",saved)
            log=journal.read_bytes();features=struct.unpack_from(">I",log,40)[0]
            tag_size=16 if features&0x10 else 8+(4 if features&2 else 0)+(2 if features&8 else 0)
            escaped=[];first=struct.unpack_from(">I",log,20)[0];count=len(log)//bs
            first_tid=struct.unpack_from(">I",log,first*bs+8)[0]
            span=(struct.unpack_from(">I",log,24)[0]-first_tid)&0xffffffff
            assert 0<span<(1<<20)
            for index in range(first,count):
                descriptor=log[index*bs:(index+1)*bs]
                if descriptor[:8]!=bytes.fromhex("c03b399800000001"):continue
                # Clean logs retain older kernel transactions beyond the new
                # worker's tail. Only inspect this session's sequence window.
                if ((struct.unpack_from(">I",descriptor,8)[0]-first_tid)&0xffffffff)>=span:continue
                pos=12;payload=index+1
                while pos+tag_size<=bs-4:
                    home=struct.unpack_from(">I",descriptor,pos)[0]
                    flags=struct.unpack_from(">I" if features&0x10 else ">H",descriptor,pos+(4 if features&0x10 else 6))[0]
                    if features&2:home|=struct.unpack_from(">I",descriptor,pos+8)[0]<<32
                    if payload>=count:payload=first
                    if home==offset//bs:
                        assert flags&1 and log[payload*bs:payload*bs+4]==bytes(4)
                        escaped.append({"home_block":home,"journal_logical_block":payload,"tag_flags":flags})
                    pos+=tag_size+(0 if flags&2 else 16)
                    if not flags&4:payload+=1
                    if flags&8:break
            assert escaped,"Expected an escaped inode-table journal payload"
            self.kernel("ext4",saved,force_replay_source=str(source),force_replay_inode_table="magic.bin",
                        expect={"magic.bin":self.expected(data)})
            self.consistency("ext4",saved);assert sha(source)==original
            results.append({"suite":"escaped-journal-metadata","status":"pass","block_size":bs,"checksum_version":version,"escaped_tags":escaped,
                            "checks":["valid_magic_prefixed_inode_table_metadata_is_journaled","actual_kernel_restores_rolled_back_home_inode_from_log",
                                      "escaped_payload_checksum_and_commit_accepted","independent_fsck","original_source_unchanged"]})
        return results

    def run(self):
        try:
            selected=self.p0_suite
            if selected in ("interoperability","all"):
                self.report["cases"].append(self.system("ext4"));self.save()
                self.report["cases"].append(self.differential());self.save()
                self.report["cases"].extend(self.journals());self.save()
            if selected in ("extents","all"):
                self.report["cases"].append(self.extents());self.save()
            if selected in ("journal-escape","all"):
                self.report["cases"].extend(self.journal_escape());self.save()
            if selected in ("boundaries","all"):
                self.report["cases"].append(self.boundaries("ext4"));self.save()
                self.report["cases"].append(self.worker_faults());self.save()
            if selected in ("io-faults","all"):
                self.report["cases"].append(self.fault("ext4"));self.save()
            if selected in ("stress","all"):
                self.report["cases"].extend(self.stress("ext4"));self.save()
            if selected in ("metadata","all"):
                from lwext4_metadata_validate import metadata_cases
                self.report["cases"].extend(metadata_cases(self));self.save()
            if selected in ("metadata-faults","all"):
                from lwext4_metadata_validate import metadata_faults
                self.report["cases"].append(metadata_faults(self));self.save()
            if selected in ("large","all"):
                self.report["cases"].append(self.large("ext4"));self.save()
            self.report["status"]="pass"
        except Exception as error:
            self.report.update(status="fail",error=str(error));raise
        finally:
            self.report["finished_at"]=datetime.now(timezone.utc).isoformat();self.save()


def arguments():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary",type=Path,default=ROOT/"target/release/xetch")
    parser.add_argument("--validation-binary",type=Path)
    parser.add_argument("--ext4-worker",type=Path)
    parser.add_argument("--e2fsck",type=Path)
    parser.add_argument("--ext4-engine",choices=["lwext4"],default="lwext4")
    parser.add_argument("--suite",dest="p0_suite",choices=["interoperability","extents","journal-escape","boundaries","io-faults","stress","metadata","metadata-faults","large","all"],default="all")
    parser.add_argument("--directory",type=Path,required=True);parser.add_argument("--output",type=Path,required=True)
    parser.add_argument("--qemu-directory",type=Path,default=ROOT/"artifacts/windows/qemu")
    parser.add_argument("--mkfs-exfat",type=Path,default=ROOT/"artifacts/tools/exfatprogs/usr/sbin/mkfs.exfat")
    parser.add_argument("--fsck-exfat",type=Path,default=ROOT/"artifacts/tools/exfatprogs/usr/sbin/fsck.exfat")
    parser.add_argument("--fault-proxy",type=Path);parser.add_argument("--protocol-probe",type=Path)
    parser.add_argument("--wsl-distribution");parser.add_argument("--wsl",type=Path,default=Path("/mnt/c/Windows/System32/wsl.exe"))
    parser.add_argument("--operations",type=int,default=1000);parser.add_argument("--seeds",type=lambda s:int(s,0),nargs="+",default=[0x5eed,0xc0ffee])
    parser.add_argument("--checkpoint-every",type=int,choices=range(1,97),default=16)
    parser.add_argument("--payload",type=Path)
    parser.add_argument("--keep-artifacts",action="store_true")
    args=parser.parse_args()
    if args.operations<1:parser.error("operations must be positive")
    return args


if __name__=="__main__":P0(arguments()).run()
