//! NTFS-3G is a separate GPL worker, with all block I/O bounded by the parent.
use crate::{
    nbd::{Disk, Window},
    partition::Partition,
    tools, Error, Result,
};
use serde_json::Value;
use std::{
    ffi::OsString,
    io::{Read, Seek, SeekFrom, Write},
    process::{Child, Stdio},
};
pub trait Upload: Read + Seek {}
impl<T: Read + Seek> Upload for T {}

pub fn program() -> OsString {
    if let Some(path) = std::env::var_os("XETCH_NTFS_WORKER") {
        return path;
    }
    let name = if cfg!(windows) {
        "xetch-ntfs-worker.exe"
    } else {
        "xetch-ntfs-worker"
    };
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let path = parent.join(name);
            if path.is_file() {
                return path.into_os_string();
            }
        }
    }
    name.into()
}
pub fn available() -> bool {
    let Ok(output) = tools::new_command(program()).arg("--version").output() else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    serde_json::from_slice::<Value>(&output.stdout).is_ok_and(|v| {
        v["engine"] == "ntfs-3g" && v["protocol"] == 1 && v["version"] == "2026.9.28"
    })
}
fn hex(value: &str) -> String {
    value
        .as_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
struct Worker(Child);
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub fn mutate(
    disk: &mut Disk,
    p: &Partition,
    operation: &str,
    path: &str,
    destination: Option<&str>,
    mut upload: Option<&mut dyn Upload>,
    replace: bool,
) -> Result<()> {
    if [Some(path), destination]
        .into_iter()
        .flatten()
        .any(|path| path.split('/').any(|part| part.starts_with('$')))
    {
        return Err(Error::Unsupported(
            "NTFS system metadata paths are protected".into(),
        ));
    }
    let upload_size = match upload.as_mut() {
        Some(file) => {
            let n = file.seek(SeekFrom::End(0))?;
            file.seek(SeekFrom::Start(0))?;
            n
        }
        None => 0,
    };
    let log = tempfile::tempfile()?;
    let child = tools::new_command(program())
        .args([
            operation.to_string(),
            p.size.to_string(),
            hex(&format!("/{path}")),
            hex(&format!("/{}", destination.unwrap_or(""))),
            upload_size.to_string(),
            if replace { "1" } else { "0" }.to_string(),
            "1".to_string(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(log.try_clone()?)
        .spawn()
        .map_err(|e| {
            Error::Dependency(format!(
                "NTFS-3G worker: {e}; build it or set XETCH_NTFS_WORKER"
            ))
        })?;
    let mut worker = Worker(child);
    let mut input = worker
        .0
        .stdin
        .take()
        .ok_or_else(|| Error::Dependency("missing NTFS stdin".into()))?;
    let mut output = worker
        .0
        .stdout
        .take()
        .ok_or_else(|| Error::Dependency("missing NTFS stdout".into()))?;
    let mut volume = Window::new(disk, p.offset, p.size);
    loop {
        let mut header = [0u8; 24];
        if let Err(e) = output.read_exact(&mut header) {
            let mut log = log;
            let mut message = Vec::new();
            log.seek(SeekFrom::Start(0))?;
            log.take(8192).read_to_end(&mut message)?;
            return Err(Error::Dependency(format!(
                "NTFS worker protocol: {e}; {}",
                String::from_utf8_lossy(&message)
            )));
        }
        if &header[..4] != b"XNT1" || header[5..8] != [0, 0, 0] {
            return Err(Error::Dependency("invalid NTFS worker frame".into()));
        }
        let offset = u64::from_le_bytes(header[8..16].try_into().unwrap());
        let length = u64::from_le_bytes(header[16..24].try_into().unwrap());
        if length > 1024 * 1024 {
            return Err(Error::Dependency("NTFS worker frame exceeds 1 MiB".into()));
        }
        let mut buffer = vec![0; length as usize];
        match header[4] {
            b'R' | b'W' | b'F' | b'U' => {
                if header[4] == b'W' {
                    output.read_exact(&mut buffer)?;
                }
                let io = (|| -> std::io::Result<()> {
                    match header[4] {
                        b'R' => {
                            if offset.checked_add(length).is_none_or(|end| end > p.size) {
                                return Err(std::io::ErrorKind::InvalidInput.into());
                            }
                            volume.seek(SeekFrom::Start(offset))?;
                            volume.read_exact(&mut buffer)?;
                        }
                        b'W' => {
                            if offset.checked_add(length).is_none_or(|end| end > p.size) {
                                return Err(std::io::ErrorKind::InvalidInput.into());
                            }
                            volume.seek(SeekFrom::Start(offset))?;
                            volume.write_all(&buffer)?;
                            buffer.clear();
                        }
                        b'F' => {
                            if length != 0 || offset != 0 {
                                return Err(std::io::ErrorKind::InvalidInput.into());
                            }
                            volume.flush()?;
                        }
                        _ => {
                            if offset
                                .checked_add(length)
                                .is_none_or(|end| end > upload_size)
                            {
                                return Err(std::io::ErrorKind::InvalidInput.into());
                            }
                            let file = upload.as_mut().ok_or(std::io::ErrorKind::InvalidInput)?;
                            file.seek(SeekFrom::Start(offset))?;
                            file.read_exact(&mut buffer)?;
                        }
                    }
                    Ok(())
                })();
                input.write_all(&(if io.is_ok() { 0u32 } else { 1u32 }).to_le_bytes())?;
                if io.is_err() {
                    buffer.clear();
                }
                input.write_all(&(buffer.len() as u32).to_le_bytes())?;
                input.write_all(&buffer)?;
                input.flush()?;
                io?;
            }
            b'J' => {
                output.read_exact(&mut buffer)?;
                let _: Value = serde_json::from_slice(&buffer)?;
                drop(input);
                if !worker.0.wait()?.success() {
                    return Err(Error::Dependency(
                        "NTFS worker exited unsuccessfully".into(),
                    ));
                }
                volume.flush()?;
                return Ok(());
            }
            b'E' => {
                output.read_exact(&mut buffer)?;
                let mut message = String::from_utf8_lossy(&buffer).into_owned();
                let mut diagnostic = String::new();
                let mut reader = log.try_clone()?;
                reader.seek(SeekFrom::Start(0))?;
                reader.take(8192).read_to_string(&mut diagnostic).ok();
                if !diagnostic.trim().is_empty() {
                    message.push_str(&format!("; {}", diagnostic.trim()));
                }
                return Err(match offset {
                    1 => Error::Invalid(message),
                    2 => Error::Unsupported(message),
                    3 => Error::Conflict(message),
                    _ => Error::Io(std::io::Error::other(message)),
                });
            }
            _ => return Err(Error::Dependency("unknown NTFS worker operation".into())),
        }
    }
}
