//! Native user-space adapters for clean exFAT, ext4 and NTFS volumes.
//! Writes are experimental and only run inside a transactional child overlay.
use crate::{
    ext4_worker,
    files::Entry,
    nbd::{Disk, Window},
    partition::Partition,
    Error, Result,
};
use fstool::{
    block::BlockDevice,
    fs::{EntryKind, FileMeta, Filesystem, SetAttrs, XattrPair},
};
use std::{
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};

pub fn supported(name: &str) -> bool {
    matches!(name, "exFAT" | "ext4" | "NTFS")
}
pub fn map_error(error: fstool::Error) -> Error {
    match error {
        fstool::Error::Io(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            Error::Conflict(e.to_string())
        }
        fstool::Error::Io(e) => Error::Io(e),
        fstool::Error::Unsupported(s) => Error::Unsupported(s),
        other => Error::Invalid(other.to_string()),
    }
}

impl<T: Read + Write + Seek + Send> BlockDevice for Window<T> {
    fn block_size(&self) -> u32 {
        512
    }
    fn total_size(&self) -> u64 {
        self.len()
    }
    fn sync(&mut self) -> fstool::Result<()> {
        self.flush().map_err(Into::into)
    }
}

fn open(dev: &mut dyn BlockDevice, name: &str) -> Result<Box<dyn Filesystem>> {
    Ok(match name {
        "exFAT" => Box::new(fstool::fs::exfat::Exfat::open(dev).map_err(map_error)?),
        "ext4" => {
            let ext = fstool::fs::ext::Ext::open(dev).map_err(map_error)?;
            check_journal(&ext, dev)?;
            Box::new(ext)
        }
        "NTFS" => Box::new(fstool::fs::ntfs::Ntfs::open(dev).map_err(map_error)?),
        _ => return Err(Error::Unsupported(format!("{name} filesystem"))),
    })
}

pub fn check_clean(dev: &mut dyn BlockDevice, name: &str) -> Result<()> {
    if name == "exFAT" {
        let mut count = [0];
        dev.read_at(110, &mut count).map_err(map_error)?;
        if count[0] != 1 {
            return Err(Error::Unsupported(
                "exFAT writes require a single FAT; TexFAT is not supported".into(),
            ));
        }
        let mut flags = [0; 2];
        dev.read_at(106, &mut flags).map_err(map_error)?;
        if u16::from_le_bytes(flags) & 6 != 0 {
            return Err(Error::Conflict(
                "exFAT is dirty or has a media error; check a copy before editing".into(),
            ));
        }
    } else if name == "ext4" {
        let mut sb = [0; 104];
        dev.read_at(1024, &mut sb).map_err(map_error)?;
        if u16::from_le_bytes(sb[58..60].try_into().unwrap()) != 1
            || u32::from_le_bytes(sb[96..100].try_into().unwrap()) & 4 != 0
        {
            return Err(Error::Conflict(
                "ext4 is not clean or requires journal replay; check a copy before access".into(),
            ));
        }
    }
    Ok(())
}

/// Share admission policy with the P0 engine; filesystem mutations themselves
/// are performed by lwext4. Excluded xattr semantics must never be silently lost.
pub(crate) fn worker_preflight(
    dev: &mut dyn BlockDevice,
    write: bool,
    paths: &[&str],
) -> Result<()> {
    check_clean(dev, "ext4")?;
    let ext = fstool::fs::ext::Ext::open(dev).map_err(map_error)?;
    check_journal(&ext, dev)?;
    if !write {
        return Ok(());
    }
    ext.check_writable().map_err(map_error)?;
    for path in paths {
        if path.split('/').any(|part| part.len() > 255) {
            return Err(Error::Invalid(
                "ext4 file names cannot exceed 255 UTF-8 bytes".into(),
            ));
        }
        check_ext_path(&ext, dev, path, false)?;
        let mut inode = 2;
        let parts: Vec<_> = path.split('/').filter(|p| !p.is_empty()).collect();
        for step in 0..=parts.len() {
            if !ext.read_xattrs(dev, inode).map_err(map_error)?.is_empty() {
                return Err(Error::Unsupported("lwext4 BSD P0 protects paths with xattrs or default ACLs; use the fstool engine".into()));
            }
            if step == parts.len() {
                break;
            }
            match ext
                .lookup_dir_entry(dev, inode, parts[step].as_bytes())
                .map_err(map_error)?
            {
                Some(next) => inode = next,
                None if step + 1 == parts.len() => break,
                None => {
                    return Err(Error::Invalid(
                        "ext4 parent directory does not exist".into(),
                    ))
                }
            }
        }
    }
    Ok(())
}

fn with_fs<T>(
    disk: &mut Disk,
    p: &Partition,
    write: bool,
    paths: &[&str],
    removing: bool,
    operation: impl FnOnce(&mut dyn Filesystem, &mut dyn BlockDevice) -> Result<T>,
) -> Result<T> {
    if write && p.filesystem == "NTFS" {
        return Err(Error::Unsupported(
            "NTFS writes must use the NTFS-3G worker".into(),
        ));
    }
    let mut dev = Window::new(disk, p.offset, p.size);
    if p.filesystem == "ext4"
        && paths
            .iter()
            .any(|path| path.split('/').any(|part| part.len() > 255))
    {
        return Err(Error::Invalid(
            "ext4 file names cannot exceed 255 UTF-8 bytes".into(),
        ));
    }
    // Do not replay an ext4 journal on a read-only handle either.
    if write || p.filesystem == "ext4" {
        check_clean(&mut dev, &p.filesystem)?;
    }
    let mut fs = if write && p.filesystem == "ext4" {
        let ext = fstool::fs::ext::Ext::open(&mut dev).map_err(map_error)?;
        check_journal(&ext, &mut dev)?;
        ext.check_writable().map_err(map_error)?;
        for path in paths {
            check_ext_path(&ext, &mut dev, path, removing)?;
        }
        Box::new(ext) as Box<dyn Filesystem>
    } else {
        open(&mut dev, &p.filesystem)?
    };
    let result = operation(fs.as_mut(), &mut dev)?;
    if write {
        fs.flush(&mut dev).map_err(map_error)?;
        dev.sync().map_err(map_error)?;
    }
    Ok(result)
}

pub fn list(disk: &mut Disk, p: &Partition, path: &str) -> Result<Vec<Entry>> {
    if ext4_worker::selected(&p.filesystem)? {
        return ext4_worker::list(disk, p, path);
    }
    with_fs(disk, p, false, &[], false, |fs, dev| {
        let entries = fs
            .list(dev, Path::new(&format!("/{path}")))
            .map_err(map_error)?;
        let mut result: Vec<_> = entries
            .into_iter()
            .filter(|e| e.name != "." && e.name != "..")
            .map(|e| Entry {
                name: e.name,
                is_directory: e.kind == EntryKind::Dir,
                size: e.size,
            })
            .collect();
        result.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(result)
    })
}
pub fn get<W: Write>(disk: &mut Disk, p: &Partition, path: &str, to: &mut W) -> Result<u64> {
    if ext4_worker::selected(&p.filesystem)? {
        return ext4_worker::get(disk, p, path, to);
    }
    with_fs(disk, p, false, &[], false, |fs, dev| {
        let path = format!("/{path}");
        let path = Path::new(&path);
        let attr = fs.getattr(dev, path).map_err(map_error)?;
        if attr.kind != EntryKind::Regular {
            return Err(Error::Unsupported("export requires a regular file".into()));
        }
        let mut reader = fs.read_file(dev, path).map_err(map_error)?;
        Ok(std::io::copy(&mut reader, to)?)
    })
}
pub fn put<R: Read + Seek>(
    disk: &mut Disk,
    p: &Partition,
    path: &str,
    from: &mut R,
    replace: bool,
) -> Result<()> {
    if ext4_worker::selected(&p.filesystem)? {
        return ext4_worker::put(disk, p, path, from, replace);
    }
    with_fs(disk, p, true, &[path], false, |fs, dev| {
        let path = format!("/{path}");
        let path = Path::new(&path);
        let parent = path.parent().unwrap_or(Path::new("/"));
        // Let the filesystem resolve names: Unicode uppercasing is not the
        // one-code-unit mapping stored in an exFAT volume's up-case table.
        let existing = optional_attrs(fs, dev, path)?;
        if let Some(entry) = &existing {
            if !replace {
                return Err(Error::Conflict(
                    "destination file exists; specify --replace".into(),
                ));
            }
            if entry.kind != EntryKind::Regular {
                return Err(Error::Unsupported(
                    "replacement requires a regular file".into(),
                ));
            }
            // Preserve existing inode/MFT metadata by truncating an open handle.
            let mut file = fs
                .open_file_rw(
                    dev,
                    path,
                    fstool::fs::OpenFlags {
                        truncate: true,
                        ..Default::default()
                    },
                    None,
                )
                .map_err(map_error)?;
            std::io::copy(from, &mut file)?;
            file.sync().map_err(map_error)?;
        } else {
            let (meta, xattrs) = inherited_meta(fs, dev, parent, false, &p.filesystem)?;
            let position = from.stream_position()?;
            let end = from.seek(SeekFrom::End(0))?;
            from.seek(SeekFrom::Start(position))?;
            fs.create_file_streaming(dev, path, from, end - position, meta)
                .map_err(map_error)?;
            if !xattrs.is_empty() {
                fs.set_xattrs(dev, path, &xattrs).map_err(map_error)?;
            }
        }
        fs.set_attrs(
            dev,
            path,
            SetAttrs {
                mtime: Some(now()),
                ctime: if p.filesystem == "ext4" {
                    Some(now())
                } else {
                    None
                },
                ..Default::default()
            },
        )
        .map_err(map_error)?;
        if existing.is_none() {
            update_parent(fs, dev, parent, &p.filesystem)?;
        }
        Ok(())
    })
}
pub fn mkdir(disk: &mut Disk, p: &Partition, path: &str) -> Result<()> {
    if ext4_worker::selected(&p.filesystem)? {
        return ext4_worker::mutate(disk, p, "mkdir", path, None);
    }
    with_fs(disk, p, true, &[path], false, |fs, dev| {
        let path = format!("/{path}");
        let path = Path::new(&path);
        if optional_attrs(fs, dev, path)?.is_some() {
            return Err(Error::Conflict("directory destination exists".into()));
        }
        let (meta, xattrs) = inherited_meta(fs, dev, path.parent().unwrap(), true, &p.filesystem)?;
        fs.create_dir(dev, path, meta).map_err(map_error)?;
        if !xattrs.is_empty() {
            fs.set_xattrs(dev, path, &xattrs).map_err(map_error)?;
        }
        update_parent(fs, dev, path.parent().unwrap(), &p.filesystem)
    })
}
pub fn remove(disk: &mut Disk, p: &Partition, path: &str) -> Result<()> {
    if ext4_worker::selected(&p.filesystem)? {
        return ext4_worker::mutate(disk, p, "remove", path, None);
    }
    with_fs(disk, p, true, &[path], true, |fs, dev| {
        let path = format!("/{path}");
        let path = Path::new(&path);
        fs.remove(dev, path).map_err(map_error)?;
        update_parent(fs, dev, path.parent().unwrap(), &p.filesystem)
    })
}
pub fn rename(disk: &mut Disk, p: &Partition, source: &str, destination: &str) -> Result<()> {
    if ext4_worker::selected(&p.filesystem)? {
        return ext4_worker::mutate(disk, p, "rename", source, Some(destination));
    }
    with_fs(disk, p, true, &[source, destination], false, |fs, dev| {
        let source = format!("/{source}");
        let destination = format!("/{destination}");
        if p.filesystem != "exFAT" && optional_attrs(fs, dev, Path::new(&destination))?.is_some() {
            return Err(Error::Conflict("rename destination already exists".into()));
        }
        fs.rename(dev, Path::new(&source), Path::new(&destination))
            .map_err(map_error)?;
        if p.filesystem == "ext4" {
            fs.set_attrs(
                dev,
                Path::new(&destination),
                SetAttrs {
                    ctime: Some(now()),
                    ..Default::default()
                },
            )
            .map_err(map_error)?;
        }
        update_parent(fs, dev, Path::new(&source).parent().unwrap(), &p.filesystem)?;
        update_parent(
            fs,
            dev,
            Path::new(&destination).parent().unwrap(),
            &p.filesystem,
        )
    })
}

fn now() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .min(u64::from(u32::MAX)) as u32
}
fn file_meta(directory: bool) -> FileMeta {
    let now = now();
    FileMeta {
        mode: if directory { 0o755 } else { 0o644 },
        mtime: now,
        atime: now,
        ctime: now,
        ..Default::default()
    }
}

fn inherited_meta(
    fs: &mut dyn Filesystem,
    dev: &mut dyn BlockDevice,
    parent: &Path,
    directory: bool,
    name: &str,
) -> Result<(FileMeta, Vec<XattrPair>)> {
    let mut meta = file_meta(directory);
    if name != "ext4" {
        return Ok((meta, Vec::new()));
    }
    let attrs = fs.getattr(dev, parent).map_err(map_error)?;
    if attrs.mode & 0o2000 != 0 {
        meta.gid = attrs.gid;
        if directory {
            meta.mode |= 0o2000;
        }
    }
    let inherited = fs
        .list_xattrs(dev, parent)
        .map_err(map_error)?
        .into_iter()
        .find(|attr| attr.name == "system.posix_acl_default");
    let Some(default) = inherited else {
        return Ok((meta, Vec::new()));
    };
    let (access, mode) = crate::ext_acl::inherit(&default.value, meta.mode)?;
    meta.mode = mode;
    let mut xattrs = vec![XattrPair {
        name: "system.posix_acl_access".into(),
        value: access,
    }];
    if directory {
        xattrs.push(default);
    }
    Ok((meta, xattrs))
}

pub fn format_exfat<T: Read + Write + Seek + Send>(
    volume: &mut Window<T>,
    offset: u64,
) -> Result<()> {
    // This formatter writes the complete recommended Unicode up-case table.
    let sectors = volume.len() / 512;
    let base = offset / 512;
    let driver = SectorVolume(volume, base);
    let fs = fstool::fs::exfat::Volume::<_, 512>::format_at(
        driver,
        base,
        sectors,
        &fstool::fs::exfat::VolumeFormatOpts {
            label: "XETCH",
            volume_serial: u32::from_le_bytes(
                uuid::Uuid::new_v4().as_bytes()[..4].try_into().unwrap(),
            ),
            ..Default::default()
        },
    )
    .map_err(map_format_error)?;
    fs.unmount().map_err(map_format_error)?;
    Ok(())
}

/// Only for a NEW zero-filled temporary image, never an existing filesystem.
/// Use a fixed, interoperable ext4 profile rather than exposing untested flags.
pub fn format_ext4<T: Read + Write + Seek + Send>(volume: &mut Window<T>) -> Result<()> {
    use fstool::fs::ext::{layout, Ext, FormatOpts, FsKind};
    let mut blocks = u32::try_from(volume.len() / 4096 / 8 * 8).map_err(|_| {
        Error::Unsupported("ext4 formatting is limited to fewer than 2^32 blocks".into())
    })?;
    let mut opts = FormatOpts {
        kind: FsKind::Ext4,
        block_size: 4096,
        inode_size: 256,
        uuid: uuid::Uuid::new_v4().into_bytes(),
        volume_label: *b"XETCH\0\0\0\0\0\0\0\0\0\0\0",
        mtime: now(),
        sparse_super: true,
        prezeroed: true,
        ..Default::default()
    };
    // A tiny final group cannot hold its bitmaps and inode table. Drop that
    // partial group and replan; the image/partition keeps its requested size.
    loop {
        opts.blocks_count = blocks;
        opts.inodes_count = blocks / 4; // one inode per 16 KiB
        opts.journal_blocks = (blocks / 32).clamp(1024, 32768);
        let plan = layout::plan_layout_sized(
            4096,
            blocks,
            opts.inodes_count,
            layout::SparseSuperMode::Classic,
            0,
            false,
            256,
        )
        .map_err(map_error)?;
        if let Some(last) = plan
            .groups
            .last()
            .filter(|g| g.data_start > g.end_block + 1)
        {
            blocks = last.start_block;
            continue;
        }
        break;
    }
    let ext = Ext::format_with(volume, &opts).map_err(map_error)?;
    ext.check_writable().map_err(map_error)?;
    check_clean(volume, "ext4")?;
    check_journal(&ext, volume)?;
    volume.sync().map_err(map_error)
}

fn map_format_error(error: fstool::fs::exfat::Error<std::io::Error>) -> Error {
    match error {
        fstool::fs::exfat::Error::Io(e) => Error::Io(e),
        other => Error::Invalid(format!("exFAT format: {other:?}")),
    }
}
struct SectorVolume<'a, T>(&'a mut Window<T>, u64);
impl<T: Read + Write + Seek> fstool::device::SectorDriver for SectorVolume<'_, T> {
    type Error = std::io::Error;
    fn sector_size(&self) -> u32 {
        512
    }
    fn sector_count(&self) -> u64 {
        self.1 + self.0.len() / 512
    }
    fn read_sectors(&mut self, lba: u64, bytes: &mut [u8]) -> std::io::Result<()> {
        self.0.seek(SeekFrom::Start(
            lba.checked_sub(self.1)
                .and_then(|lba| lba.checked_mul(512))
                .ok_or(std::io::ErrorKind::InvalidInput)?,
        ))?;
        self.0.read_exact(bytes)
    }
    fn write_sectors(&mut self, lba: u64, bytes: &[u8]) -> std::io::Result<()> {
        self.0.seek(SeekFrom::Start(
            lba.checked_sub(self.1)
                .and_then(|lba| lba.checked_mul(512))
                .ok_or(std::io::ErrorKind::InvalidInput)?,
        ))?;
        self.0.write_all(bytes)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

fn optional_attrs(
    fs: &mut dyn Filesystem,
    dev: &mut dyn BlockDevice,
    path: &Path,
) -> Result<Option<fstool::fs::FileAttrs>> {
    match fs.getattr(dev, path) {
        Ok(attrs) => Ok(Some(attrs)),
        Err(fstool::Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(map_error(e)),
    }
}
fn update_parent(
    fs: &mut dyn Filesystem,
    dev: &mut dyn BlockDevice,
    parent: &Path,
    name: &str,
) -> Result<()> {
    if name == "exFAT" && parent == Path::new("/") {
        return Ok(());
    }
    fs.set_attrs(
        dev,
        parent,
        SetAttrs {
            mtime: Some(now()),
            ctime: if name == "ext4" { Some(now()) } else { None },
            ..Default::default()
        },
    )
    .map_err(map_error)
}

/// Respect inode restrictions that the generic writer does not enforce. Keep
/// shared external EA blocks away from its unconditional last-link free path.
fn check_ext_path(
    ext: &fstool::fs::ext::Ext,
    dev: &mut dyn BlockDevice,
    path: &str,
    removing: bool,
) -> Result<()> {
    let parts: Vec<_> = path.split('/').filter(|p| !p.is_empty()).collect();
    let mut inode = 2;
    for step in 0..=parts.len() {
        let node = ext.read_inode(dev, inode).map_err(map_error)?;
        if node.flags & (0x10 | 0x20 | 0x800 | 0x10_0000 | 0x4000_0000) != 0 {
            return Err(Error::Unsupported(
                "ext4 immutable, append-only, encrypted, verity or casefold inode is protected"
                    .into(),
            ));
        }
        if step == parts.len() {
            if removing
                && node.file_acl != 0
                && (node.links_count <= 1 || node.mode & 0xf000 == 0x4000)
            {
                let mut log = [0; 4];
                dev.read_at(1024 + 24, &mut log).map_err(map_error)?;
                let shift = u32::from_le_bytes(log);
                if shift > 6 {
                    return Err(Error::Invalid("invalid ext4 block size".into()));
                }
                let size = 1024u64 << shift;
                let mut header = [0; 8];
                dev.read_at(u64::from(node.file_acl) * size, &mut header)
                    .map_err(map_error)?;
                if u32::from_le_bytes(header[4..8].try_into().unwrap()) > 1 {
                    return Err(Error::Unsupported("removing the final link to an inode with a shared external xattr block is not supported".into()));
                }
            }
            return Ok(());
        }
        match ext
            .lookup_dir_entry(dev, inode, parts[step].as_bytes())
            .map_err(map_error)?
        {
            Some(next) => inode = next,
            None if step + 1 == parts.len() => return Ok(()),
            None => {
                return Err(Error::Invalid(
                    "ext4 parent directory does not exist".into(),
                ))
            }
        }
    }
    Ok(())
}

fn check_journal(ext: &fstool::fs::ext::Ext, dev: &mut dyn BlockDevice) -> Result<()> {
    let mut sb = [0; 232];
    dev.read_at(1024, &mut sb).map_err(map_error)?;
    if u32::from_le_bytes(sb[92..96].try_into().unwrap()) & 4 == 0 {
        return Ok(());
    }
    let inode = u32::from_le_bytes(sb[224..228].try_into().unwrap());
    if inode == 0 || u32::from_le_bytes(sb[228..232].try_into().unwrap()) != 0 {
        return Err(Error::Unsupported(
            "external ext4 journals are not supported".into(),
        ));
    }
    // JBD2's checksum covers its full 1024-byte superblock, including
    // fields beyond the 64-byte header used by the original guard.
    let mut bytes = [0; 1024];
    ext.open_file_reader(dev, inode)
        .map_err(map_error)?
        .read_exact(&mut bytes)?;
    let journal = fstool::fs::ext::jbd2::JournalSuperblock::decode(&bytes).map_err(map_error)?;
    if journal.start != 0 || u32::from_be_bytes(bytes[32..36].try_into().unwrap()) != 0 {
        return Err(Error::Conflict(
            "ext4 journal has pending recovery or a recorded error; automatic replay is disabled"
                .into(),
        ));
    }
    if journal.feature_incompat & !0x1b != 0 {
        return Err(Error::Unsupported(
            "unsupported ext4 journal features (including fast commit)".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn journal_last_tag_ends_each_descriptor_in_a_transaction() {
        use fstool::fs::ext::jbd2::{encode_descriptor_block, JournalBlock, JBD2_FLAG_LAST_TAG};
        // Kernel recovery stops each descriptor's tag list at LAST_TAG,
        // including a descriptor followed by another descriptor.
        let block = JournalBlock {
            fs_block: 42,
            bytes: vec![0; 1024],
        };
        let descriptor = encode_descriptor_block(1024, 7, &[block], &[0x42; 16], true, false);
        let flags = u16::from_be_bytes(descriptor[18..20].try_into().unwrap());
        assert_ne!(flags & JBD2_FLAG_LAST_TAG, 0);
    }
    #[test]
    fn kernel_journal_checksum_is_checked_and_preserved_on_updates() {
        // Clean superblock produced by Linux JBD2 (64-bit + checksum v3).
        // Validate with the simple bitwise CRC, independently of the writer's
        // table-based CRC implementation, and cover the former 64-byte guard.
        let mut bytes = *include_bytes!("../tests/fixtures/jbd2-csum-v3.bin");
        let decode = fstool::fs::ext::jbd2::JournalSuperblock::decode;
        assert_eq!(decode(&bytes).unwrap().feature_incompat, 0x12);
        assert!(decode(&bytes[..64]).is_err());
        fstool::fs::ext::jbd2::set_sequence(&mut bytes, 123);
        fstool::fs::ext::jbd2::set_start(&mut bytes, 1);
        let stored = u32::from_be_bytes(bytes[252..256].try_into().unwrap());
        let mut copy = bytes;
        copy[252..256].fill(0);
        let mut crc = !0u32;
        for byte in copy {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = (crc >> 1) ^ if crc & 1 != 0 { 0x82f6_3b78 } else { 0 };
            }
        }
        assert_eq!(stored, crc);
        let journal = decode(&bytes).unwrap();
        assert_eq!((journal.sequence, journal.start), (123, 1));
        bytes[20] ^= 1;
        assert!(decode(&bytes).is_err());
    }
    #[test]
    fn ext4_format_last_group_does_not_overflow_block_limit() {
        let blocks = u32::MAX - 7;
        let plan = fstool::fs::ext::layout::plan_layout_sized(
            4096,
            blocks,
            blocks / 4,
            fstool::fs::ext::layout::SparseSuperMode::Classic,
            0,
            false,
            256,
        )
        .unwrap();
        let last = plan.groups.last().unwrap();
        assert_eq!(last.end_block, blocks - 1);
        assert!(last.data_start <= last.end_block);
    }
    #[test]
    fn dirty_volumes_are_rejected_before_mount() {
        let mut bytes = vec![0; 2048];
        bytes[106] = 2;
        bytes[110] = 1;
        let mut dev = Window::new(std::io::Cursor::new(bytes), 0, 2048);
        assert!(matches!(
            check_clean(&mut dev, "exFAT"),
            Err(Error::Conflict(_))
        ));
        dev.inner.get_mut()[106] = 0;
        assert!(check_clean(&mut dev, "exFAT").is_ok());
        assert!(matches!(
            check_clean(&mut dev, "ext4"),
            Err(Error::Conflict(_))
        ));
        dev.inner.get_mut()[1024 + 58] = 1;
        assert!(check_clean(&mut dev, "ext4").is_ok());
        dev.inner.get_mut()[1024 + 96] = 4;
        assert!(matches!(
            check_clean(&mut dev, "ext4"),
            Err(Error::Conflict(_))
        ));
    }
}
