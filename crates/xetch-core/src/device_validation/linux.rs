use super::{conflict, file_hash, invalid, plain_file, Fixture};
use crate::Result;
use std::{
    fs::{self, File, Metadata, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
};
pub(super) const IMAGE_NAME: &str = "fixture.img";
pub(super) fn reparse(m: &Metadata) -> bool {
    m.file_type().is_symlink()
}
pub(super) fn file_identity(f: &File) -> Result<String> {
    let m = f.metadata()?;
    if !m.is_file() || m.nlink() != 1 {
        return Err(invalid(
            "fixture files must be regular and have exactly one hard link",
        ));
    }
    Ok(format!("{}:{}", m.dev(), m.ino()))
}
pub(super) fn create_image(source: &Path, image: &Path, _size: u64) -> Result<()> {
    let mut src = plain_file(source)?;
    let mut dst = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(image)?;
    std::io::copy(&mut src, &mut dst)?;
    dst.sync_all()?;
    Ok(())
}
#[repr(C)]
struct LoopInfo {
    device: u64,
    inode: u64,
    rdevice: u64,
    offset: u64,
    size_limit: u64,
    number: u32,
    encryption: u32,
    key_size: u32,
    flags: u32,
    name: [u8; 64],
    crypt_name: [u8; 64],
    key: [u8; 32],
    init: [u64; 2],
}
fn loop_info(file: &File) -> Result<LoopInfo> {
    // SAFETY: LOOP_GET_STATUS64 writes the fixed-size Linux uapi loop_info64.
    let mut info: LoopInfo = unsafe { std::mem::zeroed() };
    if unsafe { libc::ioctl(file.as_raw_fd(), 0x4C05 as libc::c_ulong, &mut info) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(info)
}
pub(super) struct Lease {
    file: File,
    path: PathBuf,
    device: u64,
    backing: File,
    base: u64,
    length: u64,
}
impl Lease {
    pub(super) fn open(root: &Path, f: &Fixture, device: Option<&Path>) -> Result<Self> {
        let path = device.ok_or_else(|| {
            invalid("Linux D1 requires the exact loop device created by the test controller")
        })?;
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if path.parent() != Some(Path::new("/dev"))
            || !name
                .strip_prefix("loop")
                .is_some_and(|s| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()))
        {
            return Err(invalid(
                "D1 accepts only a whole /dev/loopN device, never a physical disk",
            ));
        }
        let backing = plain_file(&root.join(&f.image_name))?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)?;
        let m = file.metadata()?;
        if !m.file_type().is_block_device() {
            return Err(invalid("loop target is not a block device"));
        }
        let mut lease = Self {
            file,
            path: path.into(),
            device: m.rdev(),
            backing,
            base: f.partition_offset,
            length: f.partition_size,
        };
        lease.verify(root, f)?;
        Ok(lease)
    }
    pub(super) fn verify(&mut self, root: &Path, f: &Fixture) -> Result<()> {
        let info = loop_info(&self.file)?;
        let m = self.backing.metadata()?;
        if info.device != m.dev()
            || info.inode != m.ino()
            || info.offset != 0
            || info.size_limit != 0
            || info.encryption != 0
            || info.flags & 1 != 0
            || file_identity(&self.backing)? != f.image_identity
            || file_identity(&plain_file(&root.join(&f.image_name))?)? != f.image_identity
        {
            return Err(conflict(
                "loop backing identity, offset or mode does not match the owned fixture",
            ));
        }
        let m = fs::symlink_metadata(&self.path)?;
        if !m.file_type().is_block_device() || m.rdev() != self.device {
            return Err(conflict("loop device path was replaced"));
        }
        let mut length = 0u64;
        // SAFETY: BLKGETSIZE64 writes one u64; BLKSSZGET writes one int.
        if unsafe {
            libc::ioctl(
                self.file.as_raw_fd(),
                0x80081272 as libc::c_ulong,
                &mut length,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        let mut logical = 0i32;
        if unsafe { libc::ioctl(self.file.as_raw_fd(), 0x1268 as libc::c_ulong, &mut logical) } != 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        let mut physical = 0i32;
        // SAFETY: BLKPBSZGET writes one int.
        if unsafe {
            libc::ioctl(
                self.file.as_raw_fd(),
                0x127B as libc::c_ulong,
                &mut physical,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        if !(512..=4096).contains(&physical)
            || !(physical as u32).is_power_of_two()
            || !f.partition_offset.is_multiple_of(physical as u64)
            || !f.partition_size.is_multiple_of(physical as u64)
        {
            return Err(conflict("unsupported loop physical-sector geometry"));
        }
        if length != f.size
            || logical != 512
            || file_hash(&mut self.file, 0, f.partition_offset)? != f.prefix_sha256
        {
            return Err(conflict(
                "loop geometry or protected partition-table prefix changed",
            ));
        }
        Ok(())
    }
    fn range(&self, offset: u64, len: usize) -> Result<u64> {
        if !offset.is_multiple_of(512)
            || !len.is_multiple_of(512)
            || offset
                .checked_add(len as u64)
                .is_none_or(|end| end > self.length)
        {
            return Err(invalid("I/O escapes the bounded test partition"));
        }
        self.base
            .checked_add(offset)
            .ok_or_else(|| invalid("I/O offset overflow"))
    }
    pub(super) fn read_at(&mut self, offset: u64, data: &mut [u8]) -> Result<()> {
        let position = self.range(offset, data.len())?;
        self.file.seek(SeekFrom::Start(position))?;
        self.file.read_exact(data)?;
        Ok(())
    }
    pub(super) fn write_at(&mut self, offset: u64, data: &[u8]) -> Result<()> {
        let position = self.range(offset, data.len())?;
        self.file.seek(SeekFrom::Start(position))?;
        self.file.write_all(data)?;
        Ok(())
    }
    pub(super) fn sync(&self) -> Result<()> {
        self.file.sync_all()?;
        Ok(())
    }
    pub(super) fn partition_hash(&mut self, len: u64) -> Result<String> {
        if len != self.length {
            return Err(invalid("hash escapes partition"));
        }
        file_hash(&mut self.file, self.base, len)
    }
}
