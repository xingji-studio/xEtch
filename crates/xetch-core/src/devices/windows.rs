use super::Device;
use crate::{Error, Result};
use serde::Deserialize;
use std::{path::PathBuf, process::Command};
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Discovery {
    devices: Vec<Device>,
    warnings: Vec<String>,
}
fn encoded_script(script: &str) -> String {
    const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes: Vec<_> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut output = String::new();
    for chunk in bytes.chunks(3) {
        let a = chunk[0];
        let b = chunk.get(1).copied().unwrap_or(0);
        let c = chunk.get(2).copied().unwrap_or(0);
        output.push(B64[(a >> 2) as usize] as char);
        output.push(B64[(((a & 3) << 4) | (b >> 4)) as usize] as char);
        output.push(if chunk.len() > 1 {
            B64[(((b & 15) << 2) | (c >> 6)) as usize] as char
        } else {
            '='
        });
        output.push(if chunk.len() > 2 {
            B64[(c & 63) as usize] as char
        } else {
            '='
        });
    }
    output
}
pub(super) fn enumerate() -> Result<(Vec<Device>, Vec<String>)> {
    let root = std::env::var_os("SystemRoot")
        .ok_or_else(|| Error::Dependency("SystemRoot is unavailable".into()))?;
    let shell = PathBuf::from(root).join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let paths = serde_json::json!({"runtime":std::env::current_exe().ok(),"workspace":std::env::current_dir().ok(),"temporary":std::env::temp_dir()});
    let data = super::process::output(
        Command::new(shell)
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-EncodedCommand",
                &encoded_script(include_str!("windows_inventory.ps1")),
            ])
            .env("XETCH_DISCOVERY_PATHS", paths.to_string()),
    )?;
    let d: Discovery = serde_json::from_slice(&data)
        .map_err(|e| Error::Dependency(format!("invalid native storage inventory: {e}")))?;
    if d.devices.len() > 65536
        || d.devices
            .iter()
            .any(|d| d.id.is_empty() || d.os_path.is_empty())
    {
        return Err(Error::Dependency(
            "invalid device inventory size or identity".into(),
        ));
    }
    Ok((d.devices, d.warnings))
}
