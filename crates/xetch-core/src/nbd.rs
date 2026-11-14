//! Minimal fixed-newstyle NBD client: sequential simple replies, READ/WRITE/FLUSH.
//! QEMU is a child process and exposes only a random named export on loopback.
use crate::{tools, Error, Result};
use serde_json::json;
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    process::{Child, Stdio},
    thread,
    time::{Duration, Instant},
};

pub struct Disk {
    stream: TcpStream,
    child: Child,
    _log: File,
    _startup: tempfile::TempDir,
    pub size: u64,
    pos: u64,
    writable: bool,
    handle: u64,
}

impl Disk {
    pub fn open(path: &Path, format: &str, writable: bool) -> Result<Self> {
        // Bind to port zero to pick an ephemeral port. The named export also
        // prevents connecting to an unrelated server in the reservation gap.
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        drop(listener);
        let log = tempfile::tempfile()?;
        let startup = tempfile::tempdir()?;
        let pidfile = startup.path().join("qsd.pid");
        let token_file = tempfile::Builder::new().prefix("xetch-nbd-").tempfile()?;
        let token = token_file
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let node = json!({"driver":format, "node-name":"disk", "read-only": !writable,
            "file":{"driver":"file", "filename":path}});
        let mut child = tools::new_command(tools::program("qemu-storage-daemon"))
            .args([
                "--blockdev",
                &tools::command_json(&node),
                "--nbd-server",
                &format!("addr.type=inet,addr.host=127.0.0.1,addr.port={port},max-connections=1"),
                "--export",
                &format!(
                    "nbd,id=disk,node-name=disk,name={token},writable={}",
                    if writable { "on" } else { "off" }
                ),
            ])
            .arg("--pidfile")
            .arg(&pidfile)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log.try_clone()?)
            .spawn()
            .map_err(|e| {
                Error::Dependency(format!(
                    "qemu-storage-daemon: {e}; install QEMU or set XETCH_QEMU_STORAGE_DAEMON"
                ))
            })?;
        let connected = (|| -> Result<TcpStream> {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if child.try_wait()?.is_some() {
                    let mut message = Vec::new();
                    let mut reader = log.try_clone()?;
                    reader.seek(SeekFrom::Start(0))?;
                    reader.take(8192).read_to_end(&mut message)?;
                    return Err(Error::Dependency(format!(
                        "QEMU startup: {}",
                        String::from_utf8_lossy(&message).trim()
                    )));
                }
                // QEMU writes its pidfile only after all exports are ready.
                // Connecting before that can be reset by a local TCP proxy.
                if !pidfile.exists() {
                    if Instant::now() >= deadline {
                        return Err(Error::Dependency("QEMU startup readiness timed out".into()));
                    }
                    thread::sleep(Duration::from_millis(20));
                    continue;
                }
                match TcpStream::connect_timeout(
                    &format!("127.0.0.1:{port}").parse().unwrap(),
                    Duration::from_millis(100),
                ) {
                    Ok(stream) => return Ok(stream),
                    Err(e) if Instant::now() >= deadline => return Err(e.into()),
                    Err(_) => thread::sleep(Duration::from_millis(20)),
                }
            }
        })();
        let handshake = connected.and_then(|mut stream| {
            stream.set_read_timeout(Some(Duration::from_secs(30)))?;
            stream.set_write_timeout(Some(Duration::from_secs(30)))?;
            stream.set_nodelay(true)?;
            let mut greeting = [0; 18];
            stream.read_exact(&mut greeting)?;
            if &greeting[..8] != b"NBDMAGIC"
                || u64::from_be_bytes(greeting[8..16].try_into().unwrap()) != 0x49484156454f5054
            {
                return Err(Error::Dependency("unsupported NBD greeting".into()));
            }
            let flags = u16::from_be_bytes(greeting[16..].try_into().unwrap());
            if flags & 1 == 0 {
                return Err(Error::Unsupported("fixed-newstyle NBD required".into()));
            }
            stream.write_all(&(if flags & 2 != 0 { 3u32 } else { 1 }).to_be_bytes())?;
            stream.write_all(&0x49484156454f5054u64.to_be_bytes())?;
            stream.write_all(&1u32.to_be_bytes())?; // NBD_OPT_EXPORT_NAME
            stream.write_all(&(token.len() as u32).to_be_bytes())?;
            stream.write_all(token.as_bytes())?;
            let mut export = [0; 10];
            stream.read_exact(&mut export)?;
            let size = u64::from_be_bytes(export[..8].try_into().unwrap());
            let transmission = u16::from_be_bytes(export[8..].try_into().unwrap());
            if writable && (transmission & 2 != 0 || transmission & 4 == 0) {
                return Err(Error::Unsupported(
                    "NBD export must support write and flush".into(),
                ));
            }
            if flags & 2 == 0 {
                stream.read_exact(&mut [0; 124])?;
            }
            Ok((stream, size))
        });
        match handshake {
            Ok((stream, size)) => Ok(Self {
                stream,
                child,
                _log: log,
                _startup: startup,
                size,
                pos: 0,
                writable,
                handle: 0,
            }),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                let mut reader = log;
                let mut message = Vec::new();
                let _ = reader.seek(SeekFrom::Start(0));
                let _ = reader.take(8192).read_to_end(&mut message);
                Err(Error::Dependency(format!(
                    "NBD handshake: {e}; QEMU: {}",
                    String::from_utf8_lossy(&message).trim()
                )))
            }
        }
    }

    fn request(&mut self, cmd: u16, offset: u64, length: u32, data: &[u8]) -> io::Result<()> {
        self.handle += 1;
        let mut header = [0u8; 28];
        header[..4].copy_from_slice(&0x25609513u32.to_be_bytes());
        header[6..8].copy_from_slice(&cmd.to_be_bytes());
        header[8..16].copy_from_slice(&self.handle.to_be_bytes());
        header[16..24].copy_from_slice(&offset.to_be_bytes());
        header[24..28].copy_from_slice(&length.to_be_bytes());
        self.stream.write_all(&header)?;
        self.stream.write_all(data)?;
        let mut reply = [0; 16];
        self.stream.read_exact(&mut reply)?;
        if u32::from_be_bytes(reply[..4].try_into().unwrap()) != 0x67446698
            || u64::from_be_bytes(reply[8..].try_into().unwrap()) != self.handle
        {
            return Err(io::Error::other("invalid NBD reply"));
        }
        let error = u32::from_be_bytes(reply[4..8].try_into().unwrap());
        if error != 0 {
            return Err(io::Error::other(format!("NBD error {error}")));
        }
        Ok(())
    }
}

impl Read for Disk {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = buf
            .len()
            .min(1024 * 1024)
            .min(self.size.saturating_sub(self.pos) as usize);
        if n == 0 {
            return Ok(0);
        }
        self.request(0, self.pos, n as u32, &[])?;
        self.stream.read_exact(&mut buf[..n])?;
        self.pos += n as u64;
        Ok(n)
    }
}
impl Write for Disk {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if !self.writable {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let n = buf
            .len()
            .min(1024 * 1024)
            .min(self.size.saturating_sub(self.pos) as usize);
        if n == 0 && !buf.is_empty() {
            return Err(io::ErrorKind::WriteZero.into());
        }
        self.request(1, self.pos, n as u32, &buf[..n])?;
        self.pos += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        if self.writable {
            self.request(3, 0, 0, &[])?;
        }
        Ok(())
    }
}
impl Seek for Disk {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        self.pos = checked_seek(self.pos, self.size, from)?;
        Ok(self.pos)
    }
}
impl Drop for Disk {
    fn drop(&mut self) {
        let _ = self.flush();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn checked_seek(pos: u64, size: u64, from: SeekFrom) -> io::Result<u64> {
    let next = match from {
        SeekFrom::Start(p) => i128::from(p),
        SeekFrom::Current(p) => i128::from(pos) + i128::from(p),
        SeekFrom::End(p) => i128::from(size) + i128::from(p),
    };
    if next < 0 || next > i128::from(size) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    Ok(next as u64)
}

/// Restricts every filesystem access to one validated partition.
pub struct Window<T> {
    pub inner: T,
    start: u64,
    size: u64,
    pos: u64,
}
impl<T: Read + Write + Seek> Window<T> {
    pub fn len(&self) -> u64 {
        self.size
    }
    pub fn new(inner: T, start: u64, size: u64) -> Self {
        Self {
            inner,
            start,
            size,
            pos: 0,
        }
    }
}
impl<T: Read + Write + Seek> Read for Window<T> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = buf.len().min(self.size.saturating_sub(self.pos) as usize);
        self.inner.seek(SeekFrom::Start(self.start + self.pos))?;
        let n = self.inner.read(&mut buf[..n])?;
        self.pos += n as u64;
        Ok(n)
    }
}
impl<T: Read + Write + Seek> Write for Window<T> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.len() as u64 > self.size.saturating_sub(self.pos) {
            return Err(io::ErrorKind::WriteZero.into());
        }
        self.inner.seek(SeekFrom::Start(self.start + self.pos))?;
        let n = self.inner.write(buf)?;
        self.pos += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
impl<T: Read + Write + Seek> Seek for Window<T> {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        self.pos = checked_seek(self.pos, self.size, from)?;
        Ok(self.pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn partition_window_cannot_escape_bounds() {
        let mut window = Window::new(io::Cursor::new(vec![0u8; 16]), 4, 4);
        window.write_all(&[1; 4]).unwrap();
        assert!(window.write_all(&[2]).is_err());
        assert!(window.seek(SeekFrom::Current(-5)).is_err());
        assert!(window.seek(SeekFrom::End(1)).is_err());
        assert_eq!(
            window.inner.into_inner(),
            [vec![0; 4], vec![1; 4], vec![0; 8]].concat()
        );
    }
}
