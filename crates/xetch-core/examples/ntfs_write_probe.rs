//! Reproduce the candidate's NTFS write limitation on a DISPOSABLE COPY.
//! Input must be an independently formatted standalone NTFS volume containing
//! /seed.txt (scripts/filesystems_validate.py generates one).
use fstool::{
    block::FileBackend,
    fs::{Filesystem, OpenFlags},
};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Write},
    path::Path,
};

fn hash(path: &Path) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let source = std::env::args_os()
        .nth(1)
        .ok_or("usage: ntfs_write_probe STANDALONE_NTFS_IMAGE")?;
    let source = Path::new(&source);
    if !source.is_file() {
        return Err("input must be a regular image file".into());
    }
    let before = hash(source)?;
    let mut snapshot = tempfile::NamedTempFile::new()?;
    std::io::copy(&mut File::open(source)?, snapshot.as_file_mut())?;
    snapshot.flush()?;
    let mut device = FileBackend::open(snapshot.path())?;
    let mut ntfs = fstool::fs::ntfs::Ntfs::open(&mut device)?;
    let reason = match ntfs.open_file_rw(
        &mut device,
        Path::new("/seed.txt"),
        OpenFlags {
            truncate: true,
            ..Default::default()
        },
        None,
    ) {
        Err(fstool::Error::Unsupported(reason)) if reason.contains("non-LFS-shaped $LogFile") => {
            reason
        }
        Err(other) => return Err(format!("unexpected probe failure: {other}").into()),
        Ok(_) => {
            return Err("candidate behavior changed; revisit the write compatibility gate".into())
        }
    };
    if hash(source)? != before {
        return Err("source changed during probe".into());
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "probe":"ntfs_candidate_existing_file_write", "candidate":"fstool 0.5.0",
            "platform":std::env::consts::OS,"verdict":"write_blocked","reason":reason,
            "disposable_copy":true,"source_unchanged":true,"source_sha256":before,
        }))?
    );
    Ok(())
}
