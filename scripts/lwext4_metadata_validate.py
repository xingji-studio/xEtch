"""Independent kernel/xattr API checks for the BSD lwext4 implementation."""
from pathlib import Path
import json
import re
import shutil
import struct
import subprocess
from deep_filesystems_validate import ROOT,sha
from kernel_fs_roundtrip import crc32c

def acl(entries):
    return struct.pack('<I',2)+b''.join(struct.pack('<HHI',tag,perm,identity) for tag,perm,identity in entries)

ENTRIES=[(1,7,0xffffffff),(2,7,1001),(4,5,0xffffffff),(8,5,1002),(16,7,0xffffffff),(32,0,0xffffffff)]
DEFAULT=acl(ENTRIES)
def inherited(mode):
    return acl([(tag,perm&((mode>>({1:6,16:3,32:0}[tag]))&7) if tag in (1,16,32) else perm,identity) for tag,perm,identity in ENTRIES])

def external(self,image,path):
    info=self.command('debugfs','-R','stat /'+path,image).decode()
    return int(re.search(r'File ACL:\s*(\d+)',info).group(1))

def metadata_cases(self):
    probe=ROOT/('target/windows/release/examples/xetch-ext4-probe.exe' if self.backend.windows else 'target/release/examples/xetch-ext4-probe')
    frozen=self.root/'runtime'/probe.name;shutil.copy2(probe,frozen)
    self.report['xattr_probe_sha256']=sha(frozen)
    def api(operation,image,path,key='',value=None,fail=False):
        cmd=[str(frozen),operation,self.backend.host(image),path,key]
        if value is not None:cmd.append(value.hex())
        result=subprocess.run(cmd,capture_output=True,env=self.backend.env,timeout=120)
        if fail:assert result.returncode,(cmd,result.stdout)
        else:assert result.returncode==0,(cmd,result.stderr)
        return result.stdout.decode().strip()
    results=[]
    for bs,inode_size,csum in ((1024,256,True),(2048,256,True),(4096,256,True),(4096,128,True),(4096,256,False)):
        folder=self.root/f'metadata-{bs}-{inode_size}-{int(csum)}';folder.mkdir();source=folder/'source.img'
        with source.open('xb') as f:f.truncate(128<<20)
        binary=bytes(range(256))+b'\0binary\xff';large=bytes([77])*(bs-256)
        small=bytes([9,0,255,23,1]);contents=b'original inode payload'
        attrs={'file.bin':{'user.small':small.hex(),'user.binary':binary.hex(),'user.数据':binary.hex(),'system.posix_acl_access':DEFAULT.hex()},
               'a.bin':{'user.shared':large.hex()},'b.bin':{'user.shared':large.hex()},
               'secure':{'system.posix_acl_default':DEFAULT.hex()}}
        fixture=self.kernel('ext4',source,format=True,ext_block_size=bs,ext_inode_size=inode_size,metadata_checksum=csum,
                write={'file.bin':contents.hex(),'a.bin':b'a'.hex(),'b.bin':b'b'.hex(),'secure/seed':b'seed'.hex()},
                links=[['file.bin','hardlink.bin']],metadata={'file.bin':{'uid':70000,'gid':80000,'mode':0o770},'secure':{'gid':80000,'mode':0o2770}},xattrs=attrs)
        if inode_size==256:
            self.command('debugfs','-w','-R','write /dev/null /legacy-extra0.bin',source)
            self.command('debugfs','-w','-R','sif /legacy-extra0.bin extra_isize 0',source)
        self.consistency('ext4',source);original=sha(source)
        shared=external(self,source,'a.bin');assert shared and shared==external(self,source,'b.bin')
        with source.open('rb') as f:f.seek(shared*bs+4);references=struct.unpack('<I',f.read(4))[0]
        assert references==2,references
        # Legacy x86 producers used signed bytes in name hashes. Construct the
        # legacy representation independently and require both the modern
        # oracle and the BSD reader to accept it without changing the value.
        legacy=folder/'legacy-signed.img';shutil.copyfile(source,legacy)
        physical=external(self,legacy,'file.bin')
        with legacy.open('r+b') as f:
            f.seek(1024+104);uuid=f.read(16)
            f.seek(physical*bs);block=bytearray(f.read(bs));pos=32;block_hash=0;changed=False
            while struct.unpack_from('<I',block,pos)[0]:
                length,index,offset,inum,value_size,stored=struct.unpack_from('<BBHIII',block,pos)
                name=block[pos+16:pos+16+length];value=block[offset:offset+value_size]
                if any(b>=128 for b in name):
                    h=0
                    for byte in name:
                        signed=byte-256 if byte>=128 else byte
                        h=((h<<5)^(h>>27)^signed)&0xffffffff
                    for start in range(0,len(value),4):
                        h=((h<<16)^(h>>16)^int.from_bytes(value[start:start+4].ljust(4,b'\0'),'little'))&0xffffffff
                    struct.pack_into('<I',block,pos+12,h);stored=h;changed=True
                block_hash=((block_hash<<16)^(block_hash>>16)^stored)&0xffffffff
                pos+=(16+length+3)&~3
            assert changed
            struct.pack_into('<I',block,12,block_hash)
            if csum:
                struct.pack_into('<I',block,16,0)
                seed=crc32c(struct.pack('<Q',physical),crc32c(uuid))
                struct.pack_into('<I',block,16,crc32c(block,seed))
            f.seek(physical*bs);f.write(block)
        self.consistency('ext4',legacy)
        assert bytes.fromhex(api('get',legacy,'/file.bin','user.binary'))==binary
        api('set',legacy,'/file.bin','user.small',small)
        self.consistency('ext4',legacy)
        self.kernel('ext4',legacy,expect_xattrs={'file.bin':{'user.数据':binary.hex(),'user.small':small.hex()}})
        # Direct calls exercise generic BSD set/get/list/remove and COW, beyond
        # the shipping file editor's preservation/default-ACL paths.
        modified=folder/'api.img';shutil.copyfile(source,modified)
        assert bytes.fromhex(api('get',modified,'/file.bin','user.small'))==small
        names=set(api('list',modified,'/file.bin').splitlines());assert {'user.small','user.binary','system.posix_acl_access'}<=names
        replacement=bytes([33])*(bs-300)
        api('set',modified,'/a.bin','user.shared',replacement)
        assert external(self,modified,'a.bin')!=shared
        assert bytes.fromhex(api('get',modified,'/b.bin','user.shared'))==large
        api('set',modified,'/file.bin','user.empty',b'')
        assert api('get',modified,'/file.bin','user.empty')==''
        api('set',modified,'/file.bin','user.small',bytes(80))
        api('remove',modified,'/file.bin','user.binary')
        api('remove',modified,'/file.bin','user.empty')
        api('set',modified,'/file.bin','user.too_large',bytes(4096),fail=True)
        assert bytes.fromhex(api('get',modified,'/file.bin','user.small'))==bytes(80)
        self.consistency('ext4',modified)
        self.kernel('ext4',modified,expect_xattrs={'a.bin':{'user.shared':replacement.hex()},'b.bin':{'user.shared':large.hex()},'file.bin':{'user.small':bytes(80).hex(),'system.posix_acl_access':DEFAULT.hex()}})
        self.consistency('ext4',modified)
        if inode_size==256:
            api('set',modified,'/legacy-extra0.bin','user.old',small)
            self.consistency('ext4',modified)
            self.kernel('ext4',modified,expect_xattrs={'legacy-extra0.bin':{'user.old':small.hex()}})
        # The shipping worker replaces/moves original inodes and inherits ACLs.
        ws=folder/'session';self.backend.cli('workspace','create',source,'--workspace',ws)
        payload=folder/'payload.bin';data=bytes(range(256))*257+b'tail';payload.write_bytes(data)
        self.backend.cli('file','put','--workspace',ws,'--source',payload,'--destination','/file.bin','--replace')
        self.backend.cli('file','rename','--workspace',ws,'--source','/file.bin','--destination','/renamed.bin')
        self.backend.cli('file','put','--workspace',ws,'--source',payload,'--destination','/secure/new.bin')
        self.backend.cli('file','mkdir','--workspace',ws,'--path','/secure/subdir')
        self.backend.cli('file','put','--workspace',ws,'--source',payload,'--destination','/secure/subdir/nested.bin')
        self.backend.cli('file','mkdir','--workspace',ws,'--path','/secure/empty')
        self.backend.cli('file','remove','--workspace',ws,'--path','/secure/empty')
        self.backend.cli('file','remove','--workspace',ws,'--path','/a.bin')
        saved=folder/'saved.img';self.backend.cli('apply','--workspace',ws,'--output',saved);self.consistency('ext4',saved)
        expected_attrs={'renamed.bin':attrs['file.bin'],'hardlink.bin':attrs['file.bin'],'b.bin':attrs['b.bin'],
                'secure/new.bin':{'system.posix_acl_access':inherited(0o644).hex()},
                'secure/subdir':{'system.posix_acl_access':inherited(0o755).hex(),'system.posix_acl_default':DEFAULT.hex()},
                'secure/subdir/nested.bin':{'system.posix_acl_access':inherited(0o644).hex()}}
        self.kernel('ext4',saved,expect={p:self.expected(data) for p in ('renamed.bin','hardlink.bin','secure/new.bin','secure/subdir/nested.bin')},
                expect_xattrs=expected_attrs,expect_metadata={'renamed.bin':{'uid':70000,'gid':80000,'mode':0o770,'links':2},
                    'secure/new.bin':{'gid':80000,'mode':0o640},'secure/subdir':{'gid':80000,'mode':0o2750},'secure/subdir/nested.bin':{'gid':80000,'mode':0o640}})
        self.consistency('ext4',saved)
        with saved.open('rb') as f:f.seek(shared*bs+4);assert struct.unpack('<I',f.read(4))[0]==1
        self.backend.cli('file','remove','--workspace',ws,'--path','/b.bin')
        deleted=folder/'deleted.img';self.backend.cli('apply','--workspace',ws,'--output',deleted);self.consistency('ext4',deleted)
        if csum:
            corrupt=folder/'bad-crc.img';shutil.copyfile(source,corrupt)
            with corrupt.open('r+b') as f:f.seek(shared*bs+16);x=f.read(1);f.seek(-1,1);f.write(bytes([x[0]^1]))
            broken=folder/'bad-session';self.backend.cli('workspace','create',corrupt,'--workspace',broken)
            before=(broken/'workspace.json').read_bytes();fingerprint=sha(corrupt)
            error=self.backend.cli('file','put','--workspace',broken,'--source',payload,'--destination','/a.bin','--replace',fail=True)
            assert error['code'] in ('invalid_argument','io_error') and (broken/'workspace.json').read_bytes()==before and sha(corrupt)==fingerprint,error
        malformed=folder/'bad-acl.img';shutil.copyfile(source,malformed)
        api('set',malformed,'/secure','system.posix_acl_default',bytes.fromhex('02000000'))
        rejected=folder/'acl-session';self.backend.cli('workspace','create',malformed,'--workspace',rejected)
        before=(rejected/'workspace.json').read_bytes();fingerprint=sha(malformed)
        error=self.backend.cli('file','put','--workspace',rejected,'--source',payload,'--destination','/secure/rejected.bin',fail=True)
        assert error['code']=='invalid_argument' and (rejected/'workspace.json').read_bytes()==before and sha(malformed)==fingerprint,error
        assert sha(source)==original
        checks=['legacy_signed_hash_read_and_modern_unsigned_hash_write','utf8_external_attribute_names_and_binary_values','kernel_creates_inline_external_binary_xattrs_and_acls','kernel_generates_shared_external_block',
                'bsd_get_and_aligned_list_api','bsd_set_replace_remove_empty_values','bsd_external_block_copy_on_write',
                'out_of_space_preserves_existing_attributes','kernel_reads_bsd_attribute_mutations','overwrite_rename_preserve_xattrs_acls_hardlinks_owners',
                'new_file_access_acl_mode_filter','directory_access_and_default_acl_inheritance','nested_default_acl_and_setgid_group_inheritance',
                'last_link_deletion_decrements_shared_block_reference','unique_external_block_and_acl_directory_cleanup','independent_fsck_after_each_phase','original_source_hash_unchanged','malformed_default_acl_rejected_without_checkpoint_change']
        if csum:checks.append('bad_xattr_crc_rejected_without_checkpoint_or_source_change')
        if inode_size==256:checks.append('legacy_zero_extra_isize_inode_uses_external_attrs_and_short_checksum')
        results.append({'suite':'bsd-xattrs-posix-acls','status':'pass','block_size':bs,'inode_size':inode_size,'metadata_checksum':csum,'source_shared_xattr_refcount':references,'driver':fixture['driver'],'checks':checks})
        print(f'PASS BSD xattr/ACL {bs}-byte blocks, {inode_size}-byte inodes, metadata_csum={csum}',flush=True)
    folder=self.root/'sparse-large-flag';folder.mkdir();source=folder/'source.img'
    self.backend.cli('create','--output',source,'--size','64MiB','--table','none','--filesystem','ext4')
    with source.open('rb') as f:f.seek(1024+100);assert not struct.unpack('<I',f.read(4))[0]&2
    original=sha(source);image=folder/'sparse.img';shutil.copyfile(source,image)
    api('sparse',image,'/huge-sparse.bin')
    self.consistency('ext4',image)
    with image.open('rb') as f:f.seek(1024+100);assert struct.unpack('<I',f.read(4))[0]&2
    size=(4<<30)+(2<<20)+137
    entries=self.backend.cli('file','list',image)['entries'];assert any(e['name']=='huge-sparse.bin' and e['size']==size for e in entries)
    samples=[]
    for offset in (0,(2<<30)-16,(4<<30)-16):samples.append({'offset':offset,**self.expected(bytes(32))})
    samples.append({'offset':size-1,**self.expected(b'Z')})
    self.kernel('ext4',image,expect_ranges={'huge-sparse.bin':samples})
    self.consistency('ext4',image);assert sha(source)==original
    results.append({'suite':'sparse-large-file-feature','status':'pass','logical_size':size,'checks':['large_file_feature_initially_absent','bsd_tail_write_above_4gib_sets_large_file_feature','u64_file_list_size','kernel_zero_holes_across_2gib_and_4gib','kernel_reads_allocated_tail','independent_fsck','source_hash_unchanged']})
    return results


def metadata_faults(self):
    folder=self.root/'metadata-faults';folder.mkdir();source=folder/'source.img'
    with source.open('xb') as stream:stream.truncate(128<<20)
    shared=bytes([71])*3072
    self.kernel('ext4',source,format=True,write={'a.bin':b'a'.hex(),'b.bin':b'b'.hex(),'secure/seed':b'seed'.hex()},
        metadata={'secure':{'mode':0o2770,'gid':80000}},
        xattrs={'a.bin':{'user.shared':shared.hex()},'b.bin':{'user.shared':shared.hex()},'secure':{'system.posix_acl_default':DEFAULT.hex()}})
    self.consistency('ext4',source);original=sha(source);checks=[]
    for operation in ('mkdir','remove'):
        for point,new in (('edit.layer_synced',False),('manifest.replaced',True)):
            trial=folder/(operation+'-'+point);trial.mkdir();ws=trial/'session'
            self.backend.cli('workspace','create',source,'--workspace',ws)
            before=(ws/'workspace.json').read_bytes();env=self.injected.env.copy()
            env.update(XETCH_VALIDATION_POINT=point,XETCH_VALIDATION_MODE='error',XETCH_VALIDATION_RECORD=self.injected.host(trial/'record.json'))
            if self.backend.windows:env['WSLENV']=env.get('WSLENV','')+':XETCH_VALIDATION_POINT:XETCH_VALIDATION_MODE:XETCH_VALIDATION_RECORD'
            path='/secure/child' if operation=='mkdir' else '/a.bin'
            error=self.injected.cli('file',operation,'--workspace',ws,'--path',path,env=env,fail=True)
            assert error['code']==('commit_uncertain' if new else 'io_error'),error
            self.backend.cli('workspace','info','--workspace',ws)
            assert ((ws/'workspace.json').read_bytes()!=before)==new
            entries=self.backend.cli('file','list','--workspace',ws,'--path','/secure' if operation=='mkdir' else '/')['entries']
            names={e['name'] for e in entries}
            assert (('child' in names)==new) if operation=='mkdir' else (('a.bin' in names)==(not new))
            image=trial/'saved.img';self.backend.cli('apply','--workspace',ws,'--output',image);self.consistency('ext4',image)
            attributes={'b.bin':{'user.shared':shared.hex()}}
            if operation!='remove' or not new:attributes['a.bin']={'user.shared':shared.hex()}
            if operation=='mkdir' and new:
                attributes['secure/child']={'system.posix_acl_access':inherited(0o755).hex(),'system.posix_acl_default':DEFAULT.hex()}
            self.kernel('ext4',image,expect_xattrs=attributes)
            self.consistency('ext4',image);assert sha(source)==original
            checks.append({'operation':operation,'point':point,'manifest':'new' if new else 'old','business_code':error['code'],'status':'pass'})
    return {'suite':'xattr-acl-commit-faults','status':'pass','checks':checks+[
        'default_acl_external_allocation_survives_commit_or_rolls_back',
        'shared_xattr_release_survives_commit_or_rolls_back',
        'real_kernel_verifies_attributes_after_each_boundary',
        'independent_fsck_after_each_boundary','original_source_unchanged']}
