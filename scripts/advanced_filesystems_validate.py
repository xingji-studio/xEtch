#!/usr/bin/env python3
"""Commit-boundary interruptions, seeded fragmented workloads and journal profiles.

Runs native Linux or Windows backends from a Linux/WSL validation host.
Only newly marked disposable directories are used; normal builds ignore hooks.
"""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import random
import re
import shutil
import signal
import subprocess
import sys
import time
import uuid

from deep_filesystems_validate import Validation, Runner, ROOT, sha
from kernel_fs_roundtrip import journal_info
from extended_validate import ExfatReader, debug


class RpcFailure(RuntimeError):
    def __init__(self, body):
        self.body=body
        super().__init__(str(body))


class Rpc:
    def __init__(self, backend, folder):
        self.backend=backend
        self.stderr=(folder/"rpc-stderr.log").open("wb")
        self.process=subprocess.Popen([str(backend.binary),"serve","--stdio"],env=backend.env,
            stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=self.stderr)
        self.serial=0

    def call(self, method, **params):
        self.serial+=1
        def encode(value):
            if isinstance(value,Path): return self.backend.host(value)
            if isinstance(value,dict): return {k:encode(v) for k,v in value.items()}
            return value
        data={"jsonrpc":"2.0","id":self.serial,"method":method,"params":encode(params)}
        self.process.stdin.write((json.dumps(data)+"\n").encode()); self.process.stdin.flush()
        line=self.process.stdout.readline()
        if not line: raise RuntimeError(f"RPC backend stopped at {method}; see rpc-stderr.log")
        response=json.loads(line)
        assert response["id"]==self.serial
        if "error" in response: raise RpcFailure(response["error"].get("data",response["error"]))
        return response["result"]

    def close(self):
        self.process.stdin.close()
        try: self.process.wait(timeout=30)
        except subprocess.TimeoutExpired: self.process.kill(); self.process.wait()
        self.stderr.close()


class FastExfatReader(ExfatReader):
    def __init__(self,image):
        self.stream=image.open("rb")
        super().__init__(image)
    def read(self,offset,length):
        self.stream.seek(self.offset+offset)
        data=self.stream.read(length)
        assert len(data)==length
        return data
    def close(self): self.stream.close()


def fragment_runs(filesystem,image):
    if filesystem=="exfat":
        reader=FastExfatReader(image)
        try:
            entry=reader.lookup("/fragmented.bin")
            clusters=reader.clusters(entry["first"],entry["flags"],entry["length"])
            return 1+sum(b!=a+1 for a,b in zip(clusters,clusters[1:]))
        finally: reader.close()
    text=debug(image,"stat /fragmented.bin").decode()
    spans=[(int(a),int(b or a),int(c)) for a,b,c in re.findall(r"\((\d+)(?:-(\d+))?\):(\d+)",text)]
    assert spans,text
    runs=1
    for prev,nxt in zip(spans,spans[1:]):
        if prev[2]+prev[1]-prev[0]+1!=nxt[2]: runs+=1
    return runs


def independent_model(filesystem,image,files,directories):
    """Read the exact namespace and every payload without the backend adapter."""
    found_files=set(); found_dirs={"/pool"}
    if filesystem=="exfat":
        reader=FastExfatReader(image)
        try:
            def visit(path):
                entry=reader.lookup(path)
                for name,child in reader.entries(entry["first"],entry["flags"],entry["length"]).items():
                    guest=path+"/"+name
                    if child["directory"]: found_dirs.add(guest); visit(guest)
                    else:
                        found_files.add(guest)
                        assert guest in files and reader.file(guest)==files[guest],guest
            visit("/pool")
        finally: reader.close()
    else:
        def visit(path):
            listing=debug(image,f"ls -l -p {path}").decode()
            for line in listing.splitlines():
                if not line.startswith("/"): continue
                fields=line.split("/")
                # debugfs also displays deleted directory slots; the kernel
                # namespace excludes inode zero regardless of stale name bytes.
                if int(fields[1])==0: continue
                name=fields[5]
                if name in (".",".."): continue
                guest=path+"/"+name
                if int(fields[2],8)&0o170000==0o040000: found_dirs.add(guest); visit(guest)
                else:
                    found_files.add(guest)
                    assert guest in files and debug(image,f"cat {guest}")==files[guest],guest
        visit("/pool")
    assert found_files==set(files),(found_files^set(files))
    assert found_dirs==directories,(found_dirs^directories)


class Advanced(Validation):
    def __init__(self, args):
        suite = args.suite
        args.suite = "system"  # reuse safe directory/runtime setup, no QSD proxy
        super().__init__(args)
        args.suite = suite
        self.report["suite"] = suite
        self.report["limits"] = [
            "Process termination and returned I/O errors do not emulate physical power loss.",
            "Interruption points start after ownership records are durably established.",
        ]
        self.injected = None
        if suite in ("boundaries", "all"):
            source = args.validation_binary or (ROOT/"target/validation-windows/release/xetch.exe" if self.backend.windows else ROOT/"target/validation-linux/release/xetch")
            frozen = self.root/"runtime"/("xetch-validation.exe" if self.backend.windows else "xetch-validation")
            shutil.copy2(source, frozen)
            self.injected = Runner(frozen, args.qemu_directory)
            self.injected.env = self.backend.env.copy()
            self.report["validation_binary_sha256"] = sha(frozen)

    def kill(self, process, pid):
        if self.backend.windows:
            result = subprocess.run(["/mnt/c/Windows/System32/taskkill.exe", "/PID", str(pid), "/F"], capture_output=True)
            if result.returncode: raise RuntimeError(result.stdout.decode(errors="replace"))
        else:
            assert pid == process.pid
            os.kill(pid, signal.SIGKILL)

    def interrupt(self, folder, point, mode, args, paused=None):
        evidence = folder/(point.replace(".", "-")+"-"+mode+".json")
        env = self.injected.env.copy()
        env.update(XETCH_VALIDATION_POINT=point, XETCH_VALIDATION_MODE=mode,
                   XETCH_VALIDATION_RECORD=self.injected.host(evidence))
        if self.backend.windows:
            env["WSLENV"] = ":".join(filter(None,[env.get("WSLENV", ""), "XETCH_VALIDATION_POINT", "XETCH_VALIDATION_MODE", "XETCH_VALIDATION_RECORD"]))
        argv = [str(self.injected.binary), "--json", "--experimental-filesystems",
                *(self.injected.host(a) if isinstance(a, Path) else str(a) for a in args)]
        process = subprocess.Popen(argv, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, stdin=subprocess.DEVNULL)
        try:
            if mode == "pause":
                deadline = time.monotonic()+90
                while time.monotonic() < deadline:
                    if process.poll() is not None:
                        out, err = process.communicate()
                        raise RuntimeError(f"Backend exited before {point}: {out!r} {err!r}")
                    try:
                        record = json.loads(evidence.read_text())
                        break
                    except (OSError, ValueError): time.sleep(.005)
                else: raise RuntimeError(f"Interruption point not observed: {point}")
                if paused: paused(record)
                self.kill(process, record["pid"])
            out, err = process.communicate(timeout=120)
            assert process.returncode, (point, out, err)
            record = json.loads(evidence.read_text())
            assert record["point"] == point
            return json.loads(err)["error"] if mode == "error" else None
        finally:
            if process.poll() is None: process.kill(); process.communicate()

    def read_file(self, image, path, expected, folder):
        result = folder/("read-"+uuid.uuid4().hex+".bin")
        self.backend.cli("file", "get", image, "--source", path, "--destination", result)
        assert result.read_bytes() == expected, path
        result.unlink()

    def boundaries(self, filesystem):
        folder = self.root/(filesystem+"-boundaries"); folder.mkdir()
        cli = self.backend.cli
        initial = folder/"initial.vmdk"
        cli("create", "--output", initial, "--size", "64MiB", "--table", "none", "--filesystem", filesystem)
        base_ws=folder/"base-workspace"
        cli("workspace","create",initial,"--workspace",base_ws)
        old=folder/"old.bin"; old.write_bytes(b"old checkpoint\n"*1024)
        new=folder/"new.bin"; new.write_bytes(b"new committed data\n"*4096)
        cli("file","put","--workspace",base_ws,"--source",old,"--destination","/data.bin")
        source=folder/"baseline.vmdk"
        cli("apply","--workspace",base_ws,"--output",source)
        original=sha(source)
        checks=[]
        points=["edit.pending_durable","edit.layer_synced","edit.layer_kept","manifest.temporary_synced","manifest.replaced","manifest.directory_synced"]
        for point in points:
            for mode in ["error","pause"]:
                trial=folder/(point.replace(".","-")+"-"+mode); trial.mkdir()
                workspace=trial/"session"
                cli("workspace","create",source,"--workspace",workspace)
                manifest=workspace/"workspace.json"; before=manifest.read_bytes()
                head=workspace/json.loads(before)["head"]; head_hash=sha(head)
                sentinel=workspace/"edit-user-owned.qcow2"; sentinel.write_bytes(b"preserve unrelated user content")
                error=self.interrupt(trial,point,mode,["file","put","--workspace",workspace,"--source",new,"--destination","/data.bin","--replace"])
                committed=point in ("manifest.replaced","manifest.directory_synced")
                if error: assert error["code"] == ("commit_uncertain" if committed else "io_error"), error
                cli("workspace","info","--workspace",workspace)
                after=json.loads(manifest.read_text())
                assert len(after["changes"]) == int(committed)
                if not committed: assert manifest.read_bytes()==before
                assert sha(source)==original and sha(head)==head_hash
                assert sentinel.read_bytes()==b"preserve unrelated user content"
                assert not (workspace/".pending-edit.json").exists()
                assert not list(workspace.glob(".manifest-*.json"))
                assert {p.name for p in workspace.glob("edit-*.qcow2")} == {"edit-user-owned.qcow2",*([after['head']] if committed else [])}
                saved=trial/"saved.img"; cli("apply","--workspace",workspace,"--output",saved)
                self.consistency(filesystem,saved)
                self.read_file(saved,"/data.bin",new.read_bytes() if committed else old.read_bytes(),trial)
                cli("file","mkdir","--workspace",workspace,"--path","/continued")
                checks.append({"operation":"edit","point":point,"mode":mode,"result":"new" if committed else "old","status":"pass"})
                print(f"PASS {filesystem} edit {point} {mode}",flush=True)
        operations={
            "create":["output.ready","create.allocated","create.formatted","qemu.convert_started","publish.converted","publish.before_publish","publish.published","publish.directory_synced"],
            "apply":["output.ready","qemu.convert_started","publish.converted","publish.before_publish","publish.published","publish.directory_synced"],
            "file-get":["output.ready","file_get.copied","publish.before_publish","publish.published","publish.directory_synced"],
        }
        for operation, phases in operations.items():
            for point in phases:
                for mode in ["error","pause"]:
                    trial=folder/(operation+"-"+point.replace(".","-")+"-"+mode); trial.mkdir()
                    output=trial/("saved.bin" if operation=="file-get" else "saved.img")
                    args=(["create","--output",output,"--size","64MiB","--table","none","--filesystem",filesystem] if operation=="create" else
                          ["apply","--workspace",base_ws,"--output",output] if operation=="apply" else
                          ["file","get",source,"--source","/data.bin","--destination",output])
                    sentinel=trial/".xetch-stage-user-owned"; sentinel.mkdir(); (sentinel/"note").write_text("user file")
                    error=self.interrupt(trial,point,mode,args)
                    committed=point in ("publish.published","publish.directory_synced")
                    if error: assert error["code"] == ("commit_uncertain" if committed else "io_error"), error
                    assert output.exists()==committed
                    if committed:
                        saved_hash=sha(output)
                        cli(*args,fail=True)
                        assert sha(output)==saved_hash
                    else: cli(*args)
                    assert not [p for p in trial.glob(".xetch-stage-*") if p!=sentinel]
                    assert (sentinel/"note").read_text()=="user file" and sha(source)==original
                    if operation=="file-get": assert output.read_bytes()==old.read_bytes()
                    else:
                        self.consistency(filesystem,output)
                        if operation=="apply": self.read_file(output,"/data.bin",old.read_bytes(),trial)
                        else: assert not cli("file","list",output)["entries"] or filesystem=="ext4"
                    checks.append({"operation":operation,"point":point,"mode":mode,"published_before_retry":committed,"status":"pass"})
                    print(f"PASS {filesystem} {operation} {point} {mode}",flush=True)
        assert sha(source)==original
        # A regular release must ignore the debug-only fault environment.
        trial=folder/"normal-build-hooks-disabled"; trial.mkdir()
        record=trial/"unexpected-hook.json"
        env=self.backend.env.copy()
        env.update(XETCH_VALIDATION_POINT="create.allocated",XETCH_VALIDATION_MODE="error",XETCH_VALIDATION_RECORD=self.backend.host(record))
        if self.backend.windows: env["WSLENV"]=":".join(filter(None,[env.get("WSLENV",""),"XETCH_VALIDATION_POINT","XETCH_VALIDATION_MODE","XETCH_VALIDATION_RECORD"]))
        output=trial/"normal.img"
        cli("create","--output",output,"--size","64MiB","--table","none","--filesystem",filesystem,env=env)
        assert not record.exists(); self.consistency(filesystem,output)
        checks.append({"operation":"normal_build_ignores_hooks","status":"pass"})
        # Concurrent publishers cannot take over a live staging directory.
        trial=folder/"concurrent-output"; trial.mkdir(); output=trial/"saved.img"
        args=["create","--output",output,"--size","64MiB","--table","none","--filesystem",filesystem]
        def busy(_):
            assert cli(*args,fail=True)["code"]=="conflict"
            assert not output.exists()
        self.interrupt(trial,"output.ready","pause",args,paused=busy)
        cli(*args); self.consistency(filesystem,output)
        checks.append({"operation":"concurrent_publisher_is_rejected","status":"pass"})
        trial=folder/"unknown-staging-file"; trial.mkdir(); output=trial/"saved.img"
        args=["create","--output",output,"--size","64MiB","--table","none","--filesystem",filesystem]
        self.interrupt(trial,"create.formatted","pause",args)
        stage=next(trial.glob(".xetch-stage-*")); note=stage/"user-note.bin"; note.write_bytes(b"must survive recovery")
        raw_hash=sha(stage/"raw.img")
        assert cli(*args,fail=True)["code"]=="conflict"
        assert note.read_bytes()==b"must survive recovery" and sha(stage/"raw.img")==raw_hash
        note.unlink(); cli(*args); self.consistency(filesystem,output)
        checks.append({"operation":"unknown_staging_content_preserved","status":"pass"})
        trial=folder/"output-publication-race"; trial.mkdir(); output=trial/"saved.img"
        args=["create","--output",output,"--size","64MiB","--table","none","--filesystem",filesystem]
        def race(_): output.write_bytes(b"user output created before publication")
        self.interrupt(trial,"publish.before_publish","pause",args,paused=race)
        assert cli(*args,fail=True)["code"]=="conflict"
        assert output.read_bytes()==b"user output created before publication"
        assert not list(trial.glob(".xetch-stage-*"))
        checks.append({"operation":"publication_never_clobbers_racing_output","status":"pass"})
        trial=folder/"concurrent-workspace"; trial.mkdir(); workspace=trial/"session"
        cli("workspace","create",source,"--workspace",workspace)
        def workspace_busy(_):
            assert cli("workspace","info","--workspace",workspace,fail=True)["code"]=="conflict"
        self.interrupt(trial,"manifest.temporary_synced","pause",["file","put","--workspace",workspace,"--source",new,"--destination","/data.bin","--replace"],paused=workspace_busy)
        cli("workspace","info","--workspace",workspace)
        assert not (workspace/".pending-edit.json").exists() and not list(workspace.glob(".manifest-*.json"))
        checks.append({"operation":"concurrent_workspace_is_rejected","status":"pass"})
        assert sha(source)==original
        return {"filesystem":filesystem,"suite":"commit-boundaries","status":"pass","checks":checks}

    def run(self):
        try:
            for filesystem in self.args.filesystems:
                if self.args.suite in ("boundaries","all"):
                    self.report["cases"].append(self.boundaries(filesystem)); self.save()
                if self.args.suite in ("journals","all") and filesystem=="ext4":
                    self.report["cases"].extend(self.journals()); self.save()
                if self.args.suite in ("stress","all"):
                    self.report["cases"].extend(self.stress(filesystem)); self.save()
            assert self.report["cases"], "No validation cases executed"
            self.report["status"]="pass"
        except Exception as error:
            self.report["status"]="fail"; self.report["error"]=str(error)
            raise
        finally:
            self.report["finished_at"]=datetime.now(timezone.utc).isoformat(); self.save()

    def journals(self):
        results=[]
        profiles=[(1024,2,True),(2048,2,True),(4096,2,True),(2048,2,False),(2048,3,True)]
        for bs,version,bits64 in profiles:
            name=f"ext4-{bs}-csum{version}-{'64' if bits64 else '32'}"
            folder=self.root/name; folder.mkdir()
            source=folder/"source.img"
            with source.open("xb") as stream: stream.truncate(128<<20)
            initial=b"kernel-written source\n"+bytes(range(256))*1024
            seed=self.kernel("ext4",source,format=True,ext_block_size=bs,journal_64bit=bits64,
                             prepare_checksum_v2=(version==2),write={"system.bin":initial.hex(),"directory/inside.txt":b"retained content".hex()})
            assert seed["final_journal"]["block_size"]==bs
            assert seed["final_journal"]["checksum_version"]==version
            assert seed["final_journal"]["journal_64bit"]==bits64
            self.consistency("ext4",source)
            original=sha(source)
            self.read_file(source,"/system.bin",initial,folder)
            ws=folder/"session"; cli=self.backend.cli
            cli("workspace","create",source,"--workspace",ws)
            changed=b"native backend journal profile\n"+bytes(range(255,-1,-1))*2048
            payload=folder/"payload.bin"; payload.write_bytes(changed)
            cli("file","put","--workspace",ws,"--source",payload,"--destination","/system.bin","--replace")
            cli("file","rename","--workspace",ws,"--source","/directory","--destination","/renamed-directory")
            saved=folder/"saved.img"; cli("apply","--workspace",ws,"--output",saved)
            before=journal_info(saved)
            assert before["checksum_version"]==version and before["block_size"]==bs
            self.consistency("ext4",saved)
            recovery=folder/"kernel-recovery.img"; shutil.copyfile(saved,recovery)
            replay=self.kernel("ext4",recovery,force_replay_source=str(source),force_replay_expect_missing="renamed-directory",
                               expect={"renamed-directory/inside.txt":self.expected(b"retained content")})
            assert replay["journal_before_mount"]["checksum_version"]==version
            assert replay["journal_after_unmount"]["checksum_version"]==3
            self.consistency("ext4",recovery)
            reply=b"actual kernel read and wrote this journal profile\n"
            driver=self.kernel("ext4",saved,expect={"system.bin":self.expected(changed),"renamed-directory/inside.txt":self.expected(b"retained content")},
                               write={"kernel-feedback.txt":reply.hex()})
            self.consistency("ext4",saved)
            self.read_file(saved,"/kernel-feedback.txt",reply,folder)
            # Mount upgrades a clean v2 journal to v3 after loading/replaying it.
            assert driver["journal_before_mount"]["checksum_version"]==version
            assert driver["journal_after_unmount"]["checksum_version"]==3
            returned=folder/"returned-session"; cli("workspace","create",saved,"--workspace",returned)
            cli("file","mkdir","--workspace",returned,"--path","/backend-after-kernel")
            final=folder/"final.img"; cli("apply","--workspace",returned,"--output",final)
            self.consistency("ext4",final)
            assert sha(source)==original
            checks=["kernel_formats_and_writes_profile","exact_block_size_and_journal_feature_verified","independent_initial_fsck",
                    "backend_reads_kernel_file","backend_replaces_and_renames","writer_preserves_requested_journal_mode",
                    "independent_post_backend_fsck","rolled_back_directory_is_missing_before_kernel_replay",
                    "actual_kernel_replays_backend_journal","kernel_full_hash_read_and_feedback_write",
                    "backend_reads_kernel_feedback","backend_edits_returned_kernel_volume","independent_final_fsck","original_source_unchanged"]
            if (bs,version,bits64)==(2048,2,True):
                bad=folder/"corrupt-tag.img"
                # Export the original backend head again; the mounted output
                # has already been upgraded to v3 by the kernel.
                untouched=folder/"untouched-v2.img"; cli("apply","--workspace",ws,"--output",untouched)
                shutil.copyfile(untouched,bad)
                try:
                    self.kernel("ext4",bad,force_replay_source=str(source),corrupt_journal_tag=True,
                                force_replay_expect_missing="renamed-directory",expect={"renamed-directory/inside.txt":self.expected(b"retained content")})
                except RuntimeError as error:
                    assert "('mount', '-t', 'ext4'" in str(error), error
                    checks.append("kernel_rejects_bad_v2_payload_tag_with_valid_descriptor_crc")
                else: raise AssertionError("Kernel accepted a corrupted checksum-v2 transaction")
            results.append({"filesystem":"ext4","suite":"journal-profile","profile":name,"status":"pass","block_size":bs,
                            "journal_checksum_version":version,"journal_64bit":bits64,"driver":driver["driver"],
                            "kernel_upgrade":{"before":version,"after":3},"checks":checks})
            self.report["journal_profile_progress"]=name
            print(f"PASS {name}: real kernel roundtrip and journal replay",flush=True)
        return results

    def stress(self,filesystem):
        folder=self.root/(filesystem+"-stress"); folder.mkdir()
        source=folder/"fragment-source.img"
        with source.open("xb") as stream: stream.truncate(64<<20)
        prepared=self.kernel(filesystem,source,format=True,fragment_fixture=True)
        fixture=prepared["fragment_fixture"]
        self.consistency(filesystem,source)
        runs=fragment_runs(filesystem,source)
        assert runs>1,f"Fixture is contiguous ({runs} runs); physical fragmentation was not achieved"
        source_hash=sha(source)
        print(f"START {filesystem}: {fixture['anchors']} interleaved anchors, fragmented file has {runs} physical runs",flush=True)
        results=[]
        for seed in self.args.seeds:
            trial=folder/f"seed-{seed}"; trial.mkdir()
            rpc=Rpc(self.backend,trial)
            rng=random.Random(seed)
            files={}; directories={"/pool"}; serial=0
            workspace=trial/"session-0000"
            rpc.call("workspace.create",image=source,workspace=workspace)
            current_source=source
            payload=trial/"payload.bin"
            expected_static=dict(fixture["expected"])
            operations={}; methods={}; failures=0; epochs=0
            journal=(trial/"operations.jsonl").open("w")
            try:
                for step in range(1,self.args.operations+1):
                    serial+=1
                    kind=rng.choice(["put","put","replace","remove","mkdir","rename-file","rename-directory","rmdir","reject"])
                    if len(files)>96: kind="remove"
                    # Limit only directory creation. Forcing every operation
                    # to rename at the cap would permanently stop allocation
                    # and deletion churn in a long-running workload.
                    if len(directories)>=24 and kind=="mkdir":
                        kind=rng.choice(["rmdir","rename-directory"])
                    params={"workspace":workspace,"experimental_filesystems":True}
                    method=None; update=lambda:None; expected_failure=False
                    name=f"n{serial:06d}-α-数据"
                    if kind in ("put","replace"):
                        path=(rng.choice(sorted(files)) if kind=="replace" and files else rng.choice(sorted(directories))+"/"+name+".bin")
                        size=rng.choice([0,1,511,512,4095,4096,8193,32769,65537,262147])
                        data=rng.randbytes(size); payload.write_bytes(data)
                        params.update(path=path,source=payload,replace=(path in files))
                        method="file.put"
                        update=lambda p=path,d=data:files.__setitem__(p,d)
                    elif kind=="remove" and files:
                        path=rng.choice(sorted(files)); params["path"]=path; method="file.remove"
                        update=lambda p=path:files.pop(p)
                    elif kind=="mkdir":
                        parents=[d for d in sorted(directories) if d.count("/")<7]
                        path=rng.choice(parents)+"/"+name; params["path"]=path; method="file.mkdir"
                        update=lambda p=path:directories.add(p)
                    elif kind=="rename-file" and files:
                        path=rng.choice(sorted(files))
                        destination=(path.rsplit("/",1)[0]+"/"+path.rsplit("/",1)[1].swapcase() if rng.random()<.3 else rng.choice(sorted(directories))+"/"+name+".bin")
                        params.update(path=path,destination=destination); method="file.rename"
                        update=lambda p=path,d=destination:files.__setitem__(d,files.pop(p))
                    elif kind=="rename-directory" and len(directories)>1:
                        path=rng.choice(sorted(directories-{"/pool"}))
                        parent=rng.choice([d for d in sorted(directories) if d!=path and not d.startswith(path+"/") and d.count("/")<4])
                        destination=parent+"/"+name
                        params.update(path=path,destination=destination); method="file.rename"
                        def update(p=path,d=destination):
                            for key in list(files):
                                if key.startswith(p+"/"): files[d+key[len(p):]]=files.pop(key)
                            for key in list(directories):
                                if key==p or key.startswith(p+"/"): directories.remove(key); directories.add(d+key[len(p):])
                    elif kind=="rmdir":
                        empty=[d for d in sorted(directories-{"/pool"}) if not any(p.startswith(d+"/") for p in set(files)|directories)]
                        if empty:
                            path=rng.choice(empty); params["path"]=path; method="file.remove"
                            update=lambda p=path:directories.remove(p)
                    elif kind=="reject" and files:
                        path=rng.choice(sorted(files)); payload.write_bytes(b"must not replace")
                        params.update(path=path,source=payload,replace=False); method="file.put"; expected_failure=True
                    if method is None:
                        # Empty models still exercise a real read operation.
                        method="file.list"; params={"workspace":workspace,"path":"/pool"}
                    record={"step":step,"kind":kind,"method":method,"params":{k:str(v) if isinstance(v,Path) else v for k,v in params.items()},"expected_failure":expected_failure}
                    if method=="file.put":
                        content=payload.read_bytes()
                        record.update(payload_size=len(content),payload_sha256=hashlib.sha256(content).hexdigest())
                    journal.write(json.dumps(record,ensure_ascii=False)+"\n"); journal.flush()
                    before=(workspace/"workspace.json").read_bytes()
                    try:
                        rpc.call(method,**params)
                    except RpcFailure as error:
                        no_space=any(token in error.body.get("message","").lower() for token in ("space","full","no free"))
                        if not expected_failure and not (method=="file.put" and no_space): raise
                        assert (workspace/"workspace.json").read_bytes()==before
                        failures+=1
                    else:
                        assert not expected_failure,"Expected conflict was accepted"
                        update()
                    operations[kind]=operations.get(kind,0)+1
                    methods[method]=methods.get(method,0)+1
                    assert not (workspace/".pending-edit.json").exists()
                    assert not list(workspace.glob(".manifest-*.json"))
                    if step%self.args.checkpoint_every==0 or step==self.args.operations:
                        epochs+=1
                        snapshot=trial/f"epoch-{epochs:04d}.img"
                        rpc.call("workspace.apply",workspace=workspace,output=snapshot)
                        self.consistency(filesystem,snapshot)
                        independent_model(filesystem,snapshot,files,directories)
                        assert fragment_runs(filesystem,snapshot)==runs
                        expected={**expected_static,**{p.lstrip("/"):self.expected(data) for p,data in files.items()}}
                        reply=f"kernel snapshot seed={seed} step={step}\n".encode()
                        self.kernel(filesystem,snapshot,expect=expected,expect_directories=["anchors",*(["lost+found"] if filesystem=="ext4" else []),*(d.lstrip("/") for d in sorted(directories))],
                                    write={"kernel-feedback.txt":reply.hex()})
                        self.consistency(filesystem,snapshot)
                        expected_static["kernel-feedback.txt"]=self.expected(reply)
                        # Rotate before the 128-layer cap. Reopen a standalone
                        # image containing the kernel's writeback each epoch.
                        compact=trial/f"epoch-{epochs:04d}.vmdk"
                        rpc.call("image.convert",image=snapshot,output=compact)
                        previous_workspace=workspace
                        workspace=trial/f"session-{epochs:04d}"
                        created=rpc.call("workspace.create",image=compact,workspace=workspace)
                        assert len(created["manifest"]["source"]["dependencies"])==1
                        if not self.args.keep_artifacts:
                            shutil.rmtree(previous_workspace)
                            snapshot.unlink()
                            if current_source!=source: current_source.unlink()
                        current_source=compact
                        assert sha(source)==source_hash
                        self.report["active"]={"filesystem":filesystem,"seed":seed,"step":step,"epochs":epochs,"physical_fragment_runs":runs}
                        self.save()
                        print(f"PASS {filesystem} seed {seed}: {step}/{self.args.operations}, full model + fsck + kernel roundtrip, {runs} physical runs",flush=True)
                final=trial/"final-verified.img"
                rpc.call("workspace.apply",workspace=workspace,output=final)
                self.consistency(filesystem,final)
                independent_model(filesystem,final,files,directories)
                self.kernel(filesystem,final,expect={**expected_static,**{p.lstrip("/"):self.expected(data) for p,data in files.items()}},
                            expect_directories=["anchors",*(["lost+found"] if filesystem=="ext4" else []),*(d.lstrip("/") for d in sorted(directories))])
                if self.args.operations>=1000:
                    assert methods.get("file.put",0)>=100 and methods.get("file.remove",0)>=50 and methods.get("file.rename",0)>=50,methods
                results.append({"filesystem":filesystem,"suite":"fragmented-random-stress","status":"pass","workload_version":2,"seed":seed,"operations":self.args.operations,
                    "allocation_churn_guard":"pass" if self.args.operations>=1000 else "not_applicable_to_short_run",
                    "operation_counts":operations,"executed_method_counts":methods,"expected_rejections":failures,"snapshot_roundtrips":epochs,"checkpoint_every":self.args.checkpoint_every,"rpc_requests":rpc.serial,"anchors":fixture["anchors"],"physical_fragment_runs":runs,
                    "checks":["actual_interleaved_allocations_and_hole_reuse","independently_verified_physical_fragmentation","seeded_operation_log","exact_namespace_and_all_payloads_against_reference_model",
                              "rejected_operations_preserve_manifest","all_anchors_and_fragmented_file_hashes_preserved","independent_fsck_each_snapshot","real_kernel_reads_all_files_and_writes_feedback",
                              "continued_backend_edits_after_kernel_writeback","repeated_compaction_and_workspace_reopen","no_pending_manifest_or_edit_leaks","original_source_hash_unchanged"]})
            finally:
                journal.close(); rpc.close()
        self.report.pop("active",None)
        return results


def arguments():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary",type=Path,default=ROOT/"target/release/xetch")
    parser.add_argument("--ext4-engine",choices=["fstool","lwext4"])
    parser.add_argument("--ext4-worker",type=Path)
    parser.add_argument("--validation-binary",type=Path)
    parser.add_argument("--directory",type=Path,required=True)
    parser.add_argument("--output",type=Path,required=True)
    parser.add_argument("--suite",choices=["boundaries","stress","journals","all"],default="boundaries")
    parser.add_argument("--filesystems",nargs="+",choices=["exfat","ext4"],default=["exfat","ext4"])
    parser.add_argument("--qemu-directory",type=Path,default=ROOT/"artifacts/windows/qemu")
    parser.add_argument("--mkfs-exfat",type=Path,default=Path(shutil.which("mkfs.exfat") or ROOT/"artifacts/tools/exfatprogs/usr/sbin/mkfs.exfat"))
    parser.add_argument("--fsck-exfat",type=Path,default=Path(shutil.which("fsck.exfat") or ROOT/"artifacts/tools/exfatprogs/usr/sbin/fsck.exfat"))
    parser.add_argument("--wsl-distribution")
    parser.add_argument("--wsl",type=Path,default=Path("/mnt/c/Windows/System32/wsl.exe"))
    parser.add_argument("--operations",type=int,default=1000)
    parser.add_argument("--seeds",type=lambda s:int(s,0),nargs="+",default=[0x5eed,0xc0ffee])
    parser.add_argument("--checkpoint-every",type=int,choices=range(1,97),default=16)
    parser.add_argument("--keep-artifacts",action="store_true")
    args=parser.parse_args()
    if args.operations<1: parser.error("--operations must be positive")
    return args


if __name__=="__main__": Advanced(arguments()).run()
