use super::Device;
use crate::{Error, Result};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::Path,
    process::Command,
};

const COLUMNS:&str="NAME,KNAME,PATH,MAJ:MIN,TYPE,SIZE,LOG-SEC,PHY-SEC,RO,RM,MODEL,SERIAL,WWN,PTTYPE,PARTTYPE,PARTUUID,PARTFLAGS,FSTYPE,LABEL,UUID,MOUNTPOINTS,PKNAME";
fn text(v: &Value, k: &str) -> Option<String> {
    v[k].as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}
fn number(v: &Value, k: &str) -> Option<u64> {
    v[k].as_u64().or_else(|| v[k].as_str()?.parse().ok())
}
fn flag(v: &Value, k: &str) -> Option<bool> {
    v[k].as_bool().or_else(|| number(v, k).map(|v| v != 0))
}
fn dev_id(device: u64) -> String {
    format!("linux:{}:{}", libc::major(device), libc::minor(device))
}
fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\'
            && i + 3 < b.len()
            && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c))
        {
            out.push((b[i + 1] - b'0') * 64 + (b[i + 2] - b'0') * 8 + b[i + 3] - b'0');
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}
fn role_for_mount(path: &str) -> Option<&'static str> {
    match path {
        "/" => Some("system"),
        "/boot" | "/boot/efi" | "/efi" => Some("boot"),
        _ => None,
    }
}
fn parse(data: &Value) -> Result<Vec<Device>> {
    let nodes = data["blockdevices"]
        .as_array()
        .ok_or_else(|| Error::Dependency("lsblk did not return blockdevices".into()))?;
    if nodes.len() > 65536 {
        return Err(Error::Dependency("too many block devices".into()));
    }
    let mut devices = Vec::new();
    let names: BTreeMap<_, _> = nodes
        .iter()
        .filter_map(|v| Some((text(v, "kname")?, format!("linux:{}", text(v, "maj:min")?))))
        .collect();
    for v in nodes {
        let name = text(v, "kname")
            .ok_or_else(|| Error::Dependency("lsblk omitted kernel device name".into()))?;
        if name.contains('/') || name == "." || name == ".." {
            return Err(Error::Dependency("invalid kernel device name".into()));
        }
        let major_minor = text(v, "maj:min")
            .ok_or_else(|| Error::Dependency("lsblk omitted device number".into()))?;
        let mut d = Device {
            id: format!("linux:{major_minor}"),
            os_path: text(v, "path").unwrap_or_default(),
            name: name.clone(),
            kind: match text(v, "type").as_deref() {
                Some("part") => "partition".into(),
                Some(s) => s.into(),
                None => "unknown".into(),
            },
            size: number(v, "size").unwrap_or(0),
            logical_sector_size: number(v, "log-sec")
                .and_then(|v| v.try_into().ok())
                .unwrap_or(0),
            physical_sector_size: number(v, "phy-sec")
                .and_then(|v| v.try_into().ok())
                .unwrap_or(0),
            read_only: flag(v, "ro"),
            removable: flag(v, "rm").unwrap_or(false),
            model: text(v, "model"),
            serial: text(v, "serial"),
            partition_table: text(v, "pttype"),
            partition_type: text(v, "parttype"),
            partition_uuid: text(v, "partuuid"),
            filesystem: text(v, "fstype"),
            label: text(v, "label"),
            metadata_complete: true,
            ..Default::default()
        };
        d.stable_identity = d
            .partition_uuid
            .clone()
            .or_else(|| text(v, "wwn"))
            .or_else(|| d.serial.clone());
        if let Some(parent) = text(v, "pkname") {
            if let Some(id) = names.get(&parent) {
                d.parents.push(id.clone());
            } else {
                d.metadata_complete = false;
            }
        }
        if let Some(points) = v["mountpoints"].as_array() {
            for point in points.iter().filter_map(Value::as_str) {
                if point == "[SWAP]" {
                    d.roles.insert("swap".into());
                } else {
                    d.mount_points.push(point.into());
                    if let Some(role) = role_for_mount(point) {
                        d.roles.insert(role.into());
                    }
                }
            }
        }
        if let Some(flags) = text(v, "partflags")
            .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        {
            if d.partition_type
                .as_ref()
                .is_some_and(|t| t.starts_with("0x"))
                && flags & 0x80 != 0
            {
                d.roles.insert("active_mbr".into());
            }
        }
        devices.push(d);
    }
    Ok(devices)
}
pub(super) fn enumerate() -> Result<(Vec<Device>, Vec<String>)> {
    let binary = ["/usr/bin/lsblk", "/bin/lsblk"]
        .iter()
        .find(|p| Path::new(p).is_file())
        .ok_or_else(|| {
            Error::Dependency("read-only device discovery requires util-linux lsblk".into())
        })?;
    let data = super::process::output(
        Command::new(binary)
            .args(["--json", "--bytes", "--list", "--output", COLUMNS])
            .env("LC_ALL", "C"),
    )?;
    let mut devices = parse(&serde_json::from_slice(&data)?)?;
    let mut warnings = Vec::new();
    let ids: BTreeMap<_, _> = devices
        .iter()
        .map(|d| (d.name.clone(), d.id.clone()))
        .collect();
    for d in &mut devices {
        let sys = Path::new("/sys/class/block").join(&d.name);
        if fs::read_to_string(sys.join("dev"))
            .ok()
            .map(|s| format!("linux:{}", s.trim()))
            != Some(d.id.clone())
        {
            d.metadata_complete = false;
            warnings.push(format!(
                "{} changed or its sysfs identity is unavailable",
                d.id
            ));
            continue;
        }
        if d.kind == "partition" {
            d.offset = fs::read_to_string(sys.join("start"))
                .ok()
                .and_then(|s| s.trim().parse::<u64>().ok())
                .and_then(|n| n.checked_mul(512));
            d.partition_number = fs::read_to_string(sys.join("partition"))
                .ok()
                .and_then(|s| s.trim().parse().ok());
            if d.offset.is_none() || d.partition_number.is_none() {
                d.metadata_complete = false;
            }
        }
        match fs::read_dir(sys.join("slaves")) {
            Ok(children) => {
                for child in children {
                    match child.ok().and_then(|e| {
                        ids.get(&e.file_name().to_string_lossy().into_owned())
                            .cloned()
                    }) {
                        Some(parent) => {
                            if !d.parents.contains(&parent) {
                                d.parents.push(parent)
                            }
                        }
                        None => {
                            d.metadata_complete = false;
                            warnings.push(format!("{} has an unresolved slave", d.id));
                        }
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && d.kind == "partition" => {}
            Err(_) => {
                d.metadata_complete = false;
                warnings.push(format!("{} storage dependencies are unreadable", d.id));
            }
        }
        if d.kind == "loop" {
            match fs::read_to_string(sys.join("loop/backing_file"))
                .and_then(|s| fs::metadata(unescape(s.trim())))
            {
                Ok(metadata) => {
                    let backing = dev_id(if metadata.file_type().is_block_device() {
                        metadata.rdev()
                    } else {
                        metadata.dev()
                    });
                    if ids.values().any(|id| id == &backing) {
                        if !d.parents.contains(&backing) {
                            d.parents.push(backing);
                        }
                    } else {
                        d.metadata_complete = false;
                        warnings.push(format!("{} backing-file storage is unresolved", d.id));
                    }
                }
                Err(e) => {
                    d.metadata_complete = false;
                    warnings.push(format!(
                        "{} backing-file metadata is unavailable: {e}",
                        d.id
                    ));
                }
            }
        }
        if fs::read_dir(sys.join("holders")).is_err() {
            d.metadata_complete = false;
            warnings.push(format!("{} holders are unreadable", d.id));
        }
        if d.logical_sector_size == 0 || d.size == 0 || d.os_path.is_empty() {
            d.metadata_complete = false;
        }
    }
    match fs::read_to_string("/proc/self/mountinfo") {
        Ok(mounts) => {
            for line in mounts.lines() {
                let fields: Vec<_> = line.split_whitespace().collect();
                if fields.len() < 6 {
                    warnings.push("malformed mountinfo record".into());
                    continue;
                }
                let id = format!("linux:{}", fields[2]);
                let point = unescape(fields[4]);
                if let Some(d) = devices.iter_mut().find(|d| d.id == id) {
                    if !d.mount_points.contains(&point) {
                        d.mount_points.push(point.clone());
                    }
                    if let Some(role) = role_for_mount(&point) {
                        d.roles.insert(role.into());
                    }
                } else if role_for_mount(&point).is_some() {
                    warnings.push(format!(
                        "protected mount {point} is not resolvable to an enumerated block device"
                    ));
                }
            }
        }
        Err(e) => warnings.push(format!("mountinfo unavailable: {e}")),
    }
    for (role, path) in [
        ("runtime", std::env::current_exe().ok()),
        ("workspace", std::env::current_dir().ok()),
        ("runtime", Some(std::env::temp_dir())),
    ] {
        if let Some(path) = path {
            match fs::metadata(&path) {
                Ok(m) => {
                    let id = dev_id(m.dev());
                    if let Some(d) = devices.iter_mut().find(|d| d.id == id) {
                        d.roles.insert(role.into());
                    } else {
                        warnings.push(format!(
                            "{role} storage is outside the visible block-device topology"
                        ));
                    }
                }
                Err(e) => warnings.push(format!("cannot resolve {role} storage: {e}")),
            }
        }
    }
    match fs::read_to_string("/proc/swaps") {
        Ok(swaps) => {
            for line in swaps.lines().skip(1) {
                let Some(name) = line.split_whitespace().next() else {
                    continue;
                };
                match fs::metadata(unescape(name)) {
                    Ok(m) => {
                        let id = dev_id(if m.file_type().is_block_device() {
                            m.rdev()
                        } else {
                            m.dev()
                        });
                        if let Some(d) = devices.iter_mut().find(|d| d.id == id) {
                            d.roles.insert("swap".into());
                        } else {
                            warnings.push("active swap backing device is unresolved".into());
                        }
                    }
                    Err(e) => warnings.push(format!("active swap metadata is unreadable: {e}")),
                }
            }
        }
        Err(e) => warnings.push(format!("swap inventory unavailable: {e}")),
    }
    warnings.push("Only the current Linux mount namespace is observed; other namespaces, holders and open handles require the later exclusive-access helper.".into());
    for d in &mut devices {
        d.parents.sort();
        d.parents.dedup();
        d.mount_points.sort();
        d.mount_points.dedup();
    }
    Ok((devices, warnings))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_numeric_strings_multiple_mounts_and_mbr_active() {
        let d=parse(&serde_json::json!({"blockdevices":[{"kname":"sda","path":"/dev/sda","maj:min":"8:0","type":"disk","size":"1048576","log-sec":512,"phy-sec":4096,"ro":false,"serial":"id"},{"kname":"sda1","path":"/dev/sda1","maj:min":"8:1","pkname":"sda","type":"part","size":4096,"parttype":"0x83","partflags":"0x80","mountpoints":["/boot","/efi",null]}]})).unwrap();
        assert_eq!(d[0].size, 1048576);
        assert_eq!(d[1].parents, ["linux:8:0"]);
        assert!(d[1].roles.contains("boot") && d[1].roles.contains("active_mbr"));
        assert_eq!(unescape("/path\\040with\\134slash"), "/path with\\slash");
    }
}
