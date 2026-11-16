use crate::{
    extended,
    nbd::{Disk, Window},
    ntfs, partition, Error, Result,
};
use serde::Serialize;
use std::io::{Read, Seek, Write};

fn volume(disk: &mut Disk, index: Option<u32>) -> Result<partition::Partition> {
    partition::select(&partition::read(disk, disk.size)?, index)
}

pub fn check_write(disk: &mut Disk, index: Option<u32>, experimental: bool) -> Result<String> {
    let p = volume(disk, index)?;
    if p.filesystem == "NTFS" {
        if !ntfs::available() {
            return Err(Error::Unsupported("NTFS writes require the optional NTFS-3G worker; build it or set XETCH_NTFS_WORKER".into()));
        }
        return Ok(p.filesystem);
    }
    if extended::supported(&p.filesystem) {
        if !experimental {
            return Err(Error::Unsupported(format!(
                "{} writes are experimental; opt in with --experimental-filesystems",
                p.filesystem
            )));
        }
    } else if !matches!(p.filesystem.as_str(), "FAT12" | "FAT16" | "FAT32") {
        return Err(Error::Unsupported(format!("{} filesystem", p.filesystem)));
    }
    Ok(p.filesystem)
}

#[derive(Debug, Serialize, serde::Deserialize)]
pub struct Entry {
    pub name: String,
    pub is_directory: bool,
    pub size: u64,
}

pub fn normalize(path: &str, allow_root: bool) -> Result<String> {
    if path.len() > 4096
        || path.contains('\\')
        || path
            .chars()
            .any(|c| c.is_control() || ":*?\"<>|".contains(c))
    {
        return Err(Error::Invalid("invalid image path".into()));
    }
    let path = path.trim_matches('/');
    if path.is_empty() {
        return if allow_root {
            Ok(String::new())
        } else {
            Err(Error::Invalid("operation requires a non-root path".into()))
        };
    }
    if path.split('/').any(|p| {
        p.is_empty()
            || p == "."
            || p == ".."
            || p.encode_utf16().count() > 255
            || p.ends_with('.')
            || p.ends_with(' ')
    }) {
        return Err(Error::Invalid("path contains invalid components".into()));
    }
    Ok(path.into())
}

pub fn with_fat<T, F>(disk: &mut Disk, index: Option<u32>, write: bool, operation: F) -> Result<T>
where
    F: FnOnce(&fatfs::FileSystem<Window<&mut Disk>>) -> Result<T>,
{
    let layout = partition::read(disk, disk.size)?;
    let p = partition::select(&layout, index)?;
    if !matches!(p.filesystem.as_str(), "FAT12" | "FAT16" | "FAT32") {
        return Err(Error::Unsupported(format!(
            "file operations are unavailable for {} filesystem",
            p.filesystem
        )));
    }
    let fs = fatfs::FileSystem::new(
        Window::new(disk, p.offset, p.size),
        fatfs::FsOptions::new().update_accessed_date(false),
    )?;
    if write {
        let status = fs.read_status_flags()?;
        if status.dirty() || status.io_error() {
            return Err(Error::Conflict(
                "FAT volume is dirty; check a copy with fsck.fat before editing".into(),
            ));
        }
    }
    let result = operation(&fs)?;
    fs.unmount()?;
    Ok(result)
}

pub fn list(disk: &mut Disk, index: Option<u32>, path: &str) -> Result<Vec<Entry>> {
    let path = normalize(path, true)?;
    let p = volume(disk, index)?;
    if extended::supported(&p.filesystem) {
        return extended::list(disk, &p, &path);
    }
    with_fat(disk, index, false, |fs| {
        let root = fs.root_dir();
        let dir = if path.is_empty() {
            root
        } else {
            root.open_dir(&path)?
        };
        let mut entries = Vec::new();
        for item in dir.iter() {
            let e = item?;
            let name = e.file_name();
            if name == "." || name == ".." {
                continue;
            }
            entries.push(Entry {
                name,
                is_directory: e.is_dir(),
                size: e.len(),
            });
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    })
}

pub fn get<W: Write>(disk: &mut Disk, index: Option<u32>, path: &str, to: &mut W) -> Result<u64> {
    let path = normalize(path, false)?;
    let p = volume(disk, index)?;
    if extended::supported(&p.filesystem) {
        return extended::get(disk, &p, &path, to);
    }
    with_fat(disk, index, false, |fs| {
        Ok(std::io::copy(&mut fs.root_dir().open_file(&path)?, to)?)
    })
}

pub fn put<R: Read + Seek>(
    disk: &mut Disk,
    index: Option<u32>,
    path: &str,
    from: &mut R,
    replace: bool,
) -> Result<()> {
    let path = normalize(path, false)?;
    let p = volume(disk, index)?;
    if p.filesystem == "NTFS" {
        return ntfs::mutate(disk, &p, "put", &path, None, Some(from), replace);
    }
    if extended::supported(&p.filesystem) {
        return extended::put(disk, &p, &path, from, replace);
    }
    with_fat(disk, index, true, |fs| {
        let root = fs.root_dir();
        match root.open_file(&path) {
            Ok(_) if !replace => {
                return Err(Error::Conflict(
                    "destination file exists; specify --replace".into(),
                ))
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let mut file = root.create_file(&path)?;
        file.truncate()?;
        std::io::copy(from, &mut file)?;
        file.flush()?;
        Ok(())
    })
}

pub fn mkdir(disk: &mut Disk, index: Option<u32>, path: &str) -> Result<()> {
    let path = normalize(path, false)?;
    let p = volume(disk, index)?;
    if p.filesystem == "NTFS" {
        return ntfs::mutate(disk, &p, "mkdir", &path, None, None, false);
    }
    if extended::supported(&p.filesystem) {
        return extended::mkdir(disk, &p, &path);
    }
    with_fat(disk, index, true, |fs| {
        fs.root_dir().create_dir(&path)?;
        Ok(())
    })
}
pub fn remove(disk: &mut Disk, index: Option<u32>, path: &str) -> Result<()> {
    let path = normalize(path, false)?;
    let p = volume(disk, index)?;
    if p.filesystem == "NTFS" {
        return ntfs::mutate(disk, &p, "remove", &path, None, None, false);
    }
    if extended::supported(&p.filesystem) {
        return extended::remove(disk, &p, &path);
    }
    with_fat(disk, index, true, |fs| {
        fs.root_dir().remove(&path)?;
        Ok(())
    })
}
pub fn rename(disk: &mut Disk, index: Option<u32>, source: &str, destination: &str) -> Result<()> {
    let source = normalize(source, false)?;
    let destination = normalize(destination, false)?;
    let p = volume(disk, index)?;
    let descendant = if p.filesystem == "ext4" {
        destination.starts_with(&format!("{source}/"))
    } else if p.filesystem == "exFAT" {
        false
    }
    // Checked by cluster ancestry and the volume up-case table.
    else {
        destination
            .to_uppercase()
            .starts_with(&format!("{}/", source.to_uppercase()))
    };
    if descendant {
        return Err(Error::Invalid("cannot move a directory into itself".into()));
    }
    if p.filesystem == "NTFS" {
        return ntfs::mutate(disk, &p, "rename", &source, Some(&destination), None, false);
    }
    if extended::supported(&p.filesystem) {
        return extended::rename(disk, &p, &source, &destination);
    }
    with_fat(disk, index, true, |fs| {
        let root = fs.root_dir();
        if root.open_dir(&source).is_ok() {
            let source_parent = source.rsplit_once('/').map_or("", |(parent, _)| parent);
            let destination_parent = destination
                .rsplit_once('/')
                .map_or("", |(parent, _)| parent);
            if source_parent.to_uppercase() != destination_parent.to_uppercase() {
                // fatfs 0.3.6 does not update a moved directory's '..' entry.
                return Err(Error::Unsupported(
                    "moving directories between parents is not yet supported; rename within the same parent".into(),
                ));
            }
        }
        root.rename(&source, &root, &destination)?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn path_validation_preserves_unicode_and_blocks_traversal() {
        assert_eq!(normalize("/中文/文件.txt", false).unwrap(), "中文/文件.txt");
        assert_eq!(normalize("/", true).unwrap(), "");
        for p in [
            "/../secret",
            "a/./b",
            "a//b",
            "a\\b",
            "a:stream",
            "/",
            "trailing.",
        ] {
            assert!(normalize(p, false).is_err(), "{p}");
        }
    }
}
