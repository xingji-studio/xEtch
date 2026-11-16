//! Optional lwext4 P0 engine. The public CLI/RPC schema and commit policy stay shared.
use crate::{
    child, extended, files::Entry, nbd::Disk, ntfs::Upload, partition::Partition, tools, Error,
    Result,
};
use fstool::block::BlockDevice;
use serde_json::{json, Value};
use std::{
    ffi::OsString,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
    process::Stdio,
};

pub fn selected(filesystem: &str) -> Result<bool> {
    if filesystem != "ext4" {
        return Ok(false);
    }
    match std::env::var_os("XETCH_EXT4_ENGINE").as_deref() {
        None => Ok(false),
        Some(value) if value == "fstool" => Ok(false),
        Some(value) if value == "lwext4" => Ok(true),
        _ => Err(Error::Invalid(
            "XETCH_EXT4_ENGINE must be fstool or lwext4".into(),
        )),
    }
}
pub fn program() -> OsString {
    if let Some(path) = std::env::var_os("XETCH_EXT4_WORKER") {
        return path;
    }
    let name = if cfg!(windows) {
        "xetch-ext4-worker.exe"
    } else {
        "xetch-ext4-worker"
    };
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let sibling = parent.join(name);
            if sibling.is_file() {
                return sibling.into_os_string();
            }
        }
    }
    name.into()
}
fn hex(path: &str) -> String {
    format!("/{path}")
        .as_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
#[allow(clippy::too_many_arguments)]
fn run(
    dev: &mut dyn BlockDevice,
    write: bool,
    operation: &str,
    path: &str,
    destination: Option<&str>,
    mut upload: Option<&mut dyn Upload>,
    mut download: Option<&mut dyn Write>,
    replace: bool,
) -> Result<Value> {
    let upload_size = if let Some(file) = upload.as_mut() {
        let size = file.seek(SeekFrom::End(0))?;
        file.seek(SeekFrom::Start(0))?;
        size
    } else {
        0
    };
    let log = tempfile::tempfile()?;
    let mut command = tools::new_command(program());
    command
        .args([
            operation.into(),
            dev.total_size().to_string(),
            hex(path),
            hex(destination.unwrap_or("")),
            upload_size.to_string(),
            if replace { "1" } else { "0" }.into(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(log.try_clone()?);
    let mut worker = child::spawn(&mut command).map_err(|e| {
        Error::Dependency(format!(
            "lwext4 worker: {e}; build it or set XETCH_EXT4_WORKER"
        ))
    })?;
    let mut input = worker
        .stdin
        .take()
        .ok_or_else(|| Error::Dependency("missing lwext4 stdin".into()))?;
    let mut output = worker
        .stdout
        .take()
        .ok_or_else(|| Error::Dependency("missing lwext4 stdout".into()))?;
    let mut hello = false;
    let mut received = 0u64;
    loop {
        let mut header = [0; 24];
        if let Err(error) = output.read_exact(&mut header) {
            let mut reader = log.try_clone()?;
            reader.seek(SeekFrom::Start(0))?;
            let mut diagnostic = Vec::new();
            reader.take(8192).read_to_end(&mut diagnostic)?;
            return Err(Error::Dependency(format!(
                "lwext4 protocol: {error}; {}",
                String::from_utf8_lossy(&diagnostic)
            )));
        }
        if &header[..4] != b"XEW1" || header[5..8] != [0, 0, 0] {
            return Err(Error::Dependency("invalid lwext4 frame".into()));
        }
        let offset = u64::from_le_bytes(header[8..16].try_into().unwrap());
        let length = u64::from_le_bytes(header[16..24].try_into().unwrap());
        if length > 1024 * 1024 {
            return Err(Error::Dependency("lwext4 frame exceeds 1 MiB".into()));
        }
        let mut buffer = vec![0; length as usize];
        match header[4] {
            b'H' if !hello => {
                output.read_exact(&mut buffer)?;
                let body: Value = serde_json::from_slice(&buffer)?;
                if offset != 0 || body["pid"].as_u64() != Some(u64::from(worker.id())) {
                    return Err(Error::Dependency("invalid lwext4 greeting".into()));
                }
                hello = true;
                crate::validation::checkpoint(
                    "ext4_worker.started",
                    Path::new(&worker.id().to_string()),
                )?;
            }
            b'R' | b'W' | b'F' | b'U' | b'D' if hello => {
                if matches!(header[4], b'W' | b'D') {
                    output.read_exact(&mut buffer)?;
                }
                let io = (|| -> std::io::Result<()> {
                    match header[4] {
                        b'R' | b'W' => {
                            if offset
                                .checked_add(length)
                                .is_none_or(|end| end > dev.total_size())
                                || (header[4] == b'W' && !write)
                            {
                                return Err(std::io::ErrorKind::InvalidInput.into());
                            }
                            dev.seek(SeekFrom::Start(offset))?;
                            if header[4] == b'R' {
                                dev.read_exact(&mut buffer)?;
                            } else {
                                dev.write_all(&buffer)?;
                                // A successful write callback means ordered durable I/O.
                                // Library cache writeback is not itself an NBD FLUSH.
                                dev.flush()?;
                                buffer.clear();
                            }
                        }
                        b'F' => {
                            if offset != 0 || length != 0 {
                                return Err(std::io::ErrorKind::InvalidInput.into());
                            }
                            dev.flush()?;
                        }
                        b'U' => {
                            if offset
                                .checked_add(length)
                                .is_none_or(|end| end > upload_size)
                            {
                                return Err(std::io::ErrorKind::InvalidInput.into());
                            }
                            let source = upload.as_mut().ok_or(std::io::ErrorKind::InvalidInput)?;
                            source.seek(SeekFrom::Start(offset))?;
                            source.read_exact(&mut buffer)?;
                        }
                        _ => {
                            if offset != received {
                                return Err(std::io::ErrorKind::InvalidInput.into());
                            }
                            download
                                .as_mut()
                                .ok_or(std::io::ErrorKind::InvalidInput)?
                                .write_all(&buffer)?;
                            received = received
                                .checked_add(length)
                                .ok_or(std::io::ErrorKind::InvalidInput)?;
                            buffer.clear();
                        }
                    }
                    Ok(())
                })();
                if io.is_err() {
                    buffer.clear();
                }
                if header[4] == b'W' && io.is_ok() {
                    crate::validation::checkpoint(
                        "ext4_worker.write_ack",
                        Path::new(&worker.id().to_string()),
                    )?;
                }
                input.write_all(&(u32::from(io.is_err())).to_le_bytes())?;
                input.write_all(&(buffer.len() as u32).to_le_bytes())?;
                input.write_all(&buffer)?;
                input.flush()?;
                io?;
            }
            b'J' if hello => {
                output.read_exact(&mut buffer)?;
                let result: Value = serde_json::from_slice(&buffer)?;
                if offset != 0
                    || (operation == "get" && result["bytes"].as_u64() != Some(received))
                    || (operation == "put" && result["bytes"].as_u64() != Some(upload_size))
                {
                    return Err(Error::Dependency("invalid lwext4 result".into()));
                }
                drop(input);
                if !worker.wait()?.success() {
                    return Err(Error::Dependency("lwext4 exited unsuccessfully".into()));
                }
                dev.flush()?;
                return Ok(result);
            }
            b'E' => {
                output.read_exact(&mut buffer)?;
                let message = String::from_utf8_lossy(&buffer).into_owned();
                return Err(match offset {
                    1 => Error::Invalid(message),
                    2 => Error::Unsupported(message),
                    3 => Error::Conflict(message),
                    _ => Error::Io(std::io::Error::other(message)),
                });
            }
            _ => return Err(Error::Dependency("unexpected lwext4 operation".into())),
        }
    }
}
#[allow(clippy::too_many_arguments)]
fn execute(
    disk: &mut Disk,
    p: &Partition,
    write: bool,
    operation: &str,
    path: &str,
    destination: Option<&str>,
    upload: Option<&mut dyn Upload>,
    download: Option<&mut dyn Write>,
    replace: bool,
) -> Result<Value> {
    let mut dev = crate::nbd::Window::new(disk, p.offset, p.size);
    extended::worker_preflight(&mut dev, write, &[path, destination.unwrap_or(path)])?;
    run(
        &mut dev,
        write,
        operation,
        path,
        destination,
        upload,
        download,
        replace,
    )
}
pub fn list(disk: &mut Disk, p: &Partition, path: &str) -> Result<Vec<Entry>> {
    let result = execute(disk, p, false, "list", path, None, None, None, false)?;
    let mut entries: Vec<Entry> =
        serde_json::from_value(result.get("entries").cloned().unwrap_or(json!(null)))?;
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(entries)
}
pub fn get<W: Write>(disk: &mut Disk, p: &Partition, path: &str, to: &mut W) -> Result<u64> {
    let result = execute(disk, p, false, "get", path, None, None, Some(to), false)?;
    result["bytes"]
        .as_u64()
        .ok_or_else(|| Error::Dependency("missing lwext4 byte count".into()))
}
pub fn put<R: Read + Seek>(
    disk: &mut Disk,
    p: &Partition,
    path: &str,
    from: &mut R,
    replace: bool,
) -> Result<()> {
    execute(disk, p, true, "put", path, None, Some(from), None, replace).map(|_| ())
}
pub fn mutate(
    disk: &mut Disk,
    p: &Partition,
    operation: &str,
    path: &str,
    destination: Option<&str>,
) -> Result<()> {
    execute(
        disk,
        p,
        true,
        operation,
        path,
        destination,
        None,
        None,
        false,
    )
    .map(|_| ())
}
