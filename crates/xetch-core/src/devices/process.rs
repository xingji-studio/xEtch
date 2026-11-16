//! Bounded, timed discovery helpers; never invoke a command shell.
use crate::{Error, Result};
use std::{
    io::Read,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
pub(super) fn output(command: &mut Command) -> Result<Vec<u8>> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = crate::child::spawn(command)?;
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    std::thread::scope(|s| {
        let out = s.spawn(move || {
            let mut data = Vec::new();
            stdout
                .take(8 * 1024 * 1024 + 1)
                .read_to_end(&mut data)
                .map(|_| data)
        });
        let err = s.spawn(move || {
            let mut data = Vec::new();
            stderr
                .take(64 * 1024 + 1)
                .read_to_end(&mut data)
                .map(|_| data)
        });
        let deadline = Instant::now() + Duration::from_secs(30);
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {}
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(e.into());
                }
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::Dependency(
                    "device discovery timed out after 30 seconds".into(),
                ));
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let out = out
            .join()
            .map_err(|_| Error::Dependency("discovery stdout reader failed".into()))??;
        let err = err
            .join()
            .map_err(|_| Error::Dependency("discovery stderr reader failed".into()))??;
        if out.len() > 8 * 1024 * 1024 || err.len() > 64 * 1024 {
            return Err(Error::Dependency(
                "discovery output exceeded its bound".into(),
            ));
        }
        if !status.success() {
            return Err(Error::Dependency(format!(
                "read-only device discovery failed: {}",
                String::from_utf8_lossy(&err)
            )));
        }
        Ok(out)
    })
}
