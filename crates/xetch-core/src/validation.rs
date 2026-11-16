//! Deterministic interruption points, compiled only in an opt-in test build.
use crate::Result;
use std::path::Path;

#[cfg(not(feature = "validation-hooks"))]
#[inline(always)]
pub(crate) fn checkpoint(_point: &str, _path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(feature = "validation-hooks")]
pub(crate) fn checkpoint(point: &str, path: &Path) -> Result<()> {
    use std::{
        fs::OpenOptions,
        io::Write,
        time::{Duration, Instant},
    };
    if std::env::var("XETCH_VALIDATION_POINT").ok().as_deref() != Some(point) {
        return Ok(());
    }
    let record = std::env::var_os("XETCH_VALIDATION_RECORD")
        .ok_or_else(|| std::io::Error::other("missing validation record path"))?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(record)?;
    serde_json::to_writer(
        &mut file,
        &serde_json::json!({
            "pid": std::process::id(), "point": point, "path": path,
        }),
    )?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    drop(file);
    match std::env::var("XETCH_VALIDATION_MODE").as_deref() {
        Ok("error") => {
            Err(std::io::Error::other(format!("injected boundary failure: {point}")).into())
        }
        Ok("pause") => {
            let deadline = Instant::now() + Duration::from_secs(60);
            while Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(
                std::io::Error::other("validation controller did not interrupt paused backend")
                    .into(),
            )
        }
        _ => Err(std::io::Error::other("unknown validation mode").into()),
    }
}
