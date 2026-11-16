use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

pub fn program(name: &str) -> OsString {
    let key = match name {
        "qemu-img" => "XETCH_QEMU_IMG",
        _ => "XETCH_QEMU_STORAGE_DAEMON",
    };
    if let Some(configured) = std::env::var_os(key) {
        return configured;
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let file = if cfg!(windows) {
                format!("{name}.exe")
            } else {
                name.into()
            };
            let bundled = parent.join("qemu").join(file);
            if bundled.is_file() {
                return bundled.into_os_string();
            }
        }
    }
    name.into()
}

pub fn new_command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let program = program.as_ref();
    let mut command = Command::new(program);
    // Portable packages keep QEMU's non-system shared libraries next to its
    // binaries. Set the search path only for the child, never the caller.
    #[cfg(unix)]
    if let Some(parent) = Path::new(program)
        .parent()
        .filter(|_| Path::new(program).is_absolute())
    {
        let lib = parent.join("lib");
        if lib.is_dir() {
            let key = if cfg!(target_os = "macos") {
                "DYLD_LIBRARY_PATH"
            } else {
                "LD_LIBRARY_PATH"
            };
            let mut paths = vec![lib];
            if let Some(existing) = std::env::var_os(key) {
                paths.extend(std::env::split_paths(&existing));
            }
            if let Ok(value) = std::env::join_paths(paths) {
                command.env(key, value);
            }
        }
        let modules = parent.join("modules");
        if modules.is_dir() && std::env::var_os("QEMU_MODULE_DIR").is_none() {
            command.env("QEMU_MODULE_DIR", modules);
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW for GUI-owned helpers.
    }
    #[cfg(not(windows))]
    let _ = &mut command;
    command
}

/// Keep JSON command-line arguments ASCII. Some Windows QEMU distributions
/// receive argv in the system codepage, while their JSON parser expects UTF-8.
/// Escaped UTF-16 code units preserve even non-BMP paths across that boundary.
pub fn command_json(value: &Value) -> String {
    let mut result = String::new();
    for c in value.to_string().chars() {
        if c.is_ascii() {
            result.push(c);
        } else {
            for unit in c.encode_utf16(&mut [0; 2]) {
                use std::fmt::Write;
                write!(&mut result, "\\u{unit:04x}").expect("writing into a String");
            }
        }
    }
    result
}

pub fn command(args: &[OsString]) -> Result<Vec<u8>> {
    let mut command = new_command(program("qemu-img"));
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = crate::child::spawn(&mut command).map_err(|e| {
        Error::Dependency(format!("qemu-img: {e}; install QEMU or set XETCH_QEMU_IMG"))
    })?;
    if args.first().is_some_and(|a| a == "convert") {
        crate::validation::checkpoint("qemu.convert_started", Path::new(args.last().unwrap()))?;
    }
    let output = child.output()?;
    if !output.status.success() {
        return Err(Error::Dependency(format!(
            "qemu-img: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output.stdout)
}

// Windows can expose raw storage through DOS/NT aliases. Reject those aliases
// before metadata/QEMU so regular-image APIs never become a device entrypoint.
#[cfg(any(windows, test))]
fn windows_device_namespace(path: &Path) -> bool {
    let value = path
        .as_os_str()
        .to_string_lossy()
        .replace('/', "\\")
        .to_ascii_lowercase();
    if [r"\\.\", r"\\?\globalroot\", r"\device\", r"\??\"]
        .iter()
        .any(|p| value.starts_with(p))
    {
        return true;
    }
    if let Some(rest) = value.strip_prefix(r"\\?\") {
        if rest.starts_with("physicaldrive") {
            return true;
        }
        if rest.starts_with("volume{") {
            return rest
                .find('}')
                .is_none_or(|i| rest[i + 1..].trim_matches('\\').is_empty());
        }
    }
    false
}

pub fn regular(path: &Path) -> Result<PathBuf> {
    #[cfg(windows)]
    if windows_device_namespace(path) {
        return Err(Error::Invalid("raw device namespaces are not accepted by image operations; use read-only device discovery".into()));
    }
    let path = fs::canonicalize(path)?;
    #[cfg(windows)]
    if windows_device_namespace(&path) {
        return Err(Error::Invalid(
            "image path resolves to a raw device namespace".into(),
        ));
    }

    if !fs::metadata(&path)?.is_file() {
        return Err(Error::Invalid(
            "only regular disk-image files are supported".into(),
        ));
    }
    Ok(path)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageInfo {
    pub path: PathBuf,
    pub format: String,
    pub virtual_size: u64,
    pub dependencies: Vec<PathBuf>,
}

pub fn info(path: &Path, allow_overlay: bool) -> Result<ImageInfo> {
    let path = regular(path)?;
    let data: Value = serde_json::from_slice(&command(&[
        "info".into(),
        "--output=json".into(),
        path.as_os_str().into(),
    ])?)?;
    let format = data["format"]
        .as_str()
        .ok_or_else(|| Error::Invalid("missing image format".into()))?;
    if !(matches!(format, "raw" | "vmdk") || allow_overlay && format == "qcow2") {
        return Err(Error::Unsupported(format!(
            "image format {format}; expected raw IMG or VMDK"
        )));
    }
    if data.get("backing-filename").is_some() && !allow_overlay {
        return Err(Error::Unsupported(
            "snapshot/backing chains are not yet accepted as source images; flatten first".into(),
        ));
    }
    if data["encrypted"].as_bool() == Some(true) {
        return Err(Error::Unsupported(
            "encrypted images are not supported".into(),
        ));
    }
    let size = data["virtual-size"]
        .as_u64()
        .ok_or_else(|| Error::Invalid("missing virtual size".into()))?;
    let mut dependencies = vec![path.clone()];
    if let Some(extents) = data
        .pointer("/format-specific/data/extents")
        .and_then(Value::as_array)
    {
        for extent in extents {
            if let Some(name) = extent["filename"].as_str() {
                let dependency = regular(&path.parent().unwrap().join(name))?;
                if !dependencies.contains(&dependency) {
                    dependencies.push(dependency);
                }
            }
        }
    }
    Ok(ImageInfo {
        path,
        format: format.into(),
        virtual_size: size,
        dependencies,
    })
}

pub fn doctor() -> Value {
    let tools: Vec<Value> = ["qemu-img", "qemu-storage-daemon"]
        .into_iter()
        .map(
            |name| match new_command(program(name)).arg("--version").output() {
                Ok(out) => serde_json::json!({"name": name, "available": out.status.success(),
                "version": String::from_utf8_lossy(&out.stdout).lines().next().unwrap_or("")}),
                Err(e) => {
                    serde_json::json!({"name": name, "available": false, "error": e.to_string()})
                }
            },
        )
        .collect();
    let ntfs = crate::ntfs::available();
    serde_json::json!({"protocol_version": 1, "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH, "tools": tools,
        "capabilities": {"source_formats": ["raw", "vmdk"], "output_formats": ["raw", "vmdk:monolithicSparse"],
        "filesystem_read_write": if ntfs {vec!["FAT12", "FAT16", "FAT32", "NTFS"]} else {vec!["FAT12", "FAT16", "FAT32"]},
        "filesystem_read": ["FAT12", "FAT16", "FAT32", "exFAT", "NTFS", "ext4"],
        "filesystem_write_experimental": ["exFAT", "ext4"],
        "filesystem_operations": {"exFAT": ["list", "get", "put", "mkdir", "remove", "rename-file", "rename-directory", "move-file", "move-directory", "create-none", "create-mbr", "create-gpt"],
            "ext4": ["list", "get", "put", "mkdir", "remove", "rename-file", "rename-directory", "move-file", "move-directory", "create-none", "create-mbr", "create-gpt"]},
        "filesystem_read_only": if ntfs {Vec::<&str>::new()}else{vec!["NTFS"]},
        "ntfs_write": ntfs, "ntfs_engine": if ntfs {"ntfs-3g 2026.9.28"}else{"unavailable"},
        "filesystem_detect_only": [],
        "partition_tables_read": ["MBR-primary", "GPT"], "sector_size": 512,
        "in_place_write": false, "physical_disks": false, "physical_disk_discovery": cfg!(any(windows, target_os="linux")),
        "physical_disk_write": false, "physical_disk_methods": if cfg!(any(windows,target_os="linux")){vec!["device.list","device.inspect"]}else{Vec::<&str>::new()}, "rpc_jobs": false}})
}

#[cfg(test)]
mod tests {
    #[test]
    fn qemu_json_preserves_unicode_in_ascii_arguments() {
        let value = serde_json::json!({"filename": "C:\\space 中文\\disk-🦀.img"});
        let encoded = super::command_json(&value);
        assert!(encoded.is_ascii());
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&encoded).unwrap(),
            value
        );
    }
    #[test]
    fn raw_windows_aliases_are_distinct_from_regular_image_paths() {
        for value in [
            r"\\.\PhysicalDrive0",
            r"\\.\C:",
            r"\\?\GLOBALROOT\Device\Harddisk0\Partition1",
            r"\\?\PhysicalDrive0",
            r"\Device\Harddisk0\Partition1",
            r"\??\C:",
            r"\\?\Volume{00000000-0000-0000-0000-000000000001}",
            r"\\?\Volume{00000000-0000-0000-0000-000000000001}\",
        ] {
            assert!(
                super::windows_device_namespace(std::path::Path::new(value)),
                "{value}"
            );
            #[cfg(windows)]
            assert!(matches!(
                super::regular(std::path::Path::new(value)),
                Err(crate::Error::Invalid(_))
            ));
        }
        for value in [
            r"C:\images\disk.img",
            r"\\?\C:\images\disk.img",
            r"\\server\share\disk.img",
            r"\\?\Volume{00000000-0000-0000-0000-000000000001}\images\disk.img",
        ] {
            assert!(
                !super::windows_device_namespace(std::path::Path::new(value)),
                "{value}"
            );
        }
    }
}
