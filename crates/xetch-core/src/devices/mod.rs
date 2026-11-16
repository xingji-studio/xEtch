//! Read-only host device discovery. No device handle is opened for writing here.
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
#[cfg(any(target_os = "linux", windows))]
use sha2::{Digest, Sha256};
#[cfg(any(target_os = "linux", windows, test))]
use std::collections::BTreeMap;
use std::collections::BTreeSet;
#[cfg(any(target_os = "linux", windows))]
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(any(target_os = "linux", windows))]
mod process;
#[cfg(windows)]
mod windows;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Device {
    pub id: String,
    pub os_path: String,
    pub name: String,
    pub kind: String,
    pub size: u64,
    pub offset: Option<u64>,
    pub logical_sector_size: u32,
    pub physical_sector_size: u32,
    pub read_only: Option<bool>,
    pub removable: bool,
    pub model: Option<String>,
    pub serial: Option<String>,
    pub stable_identity: Option<String>,
    pub disk_number: Option<u32>,
    pub partition_number: Option<u32>,
    pub partition_table: Option<String>,
    pub partition_type: Option<String>,
    pub partition_uuid: Option<String>,
    pub filesystem: Option<String>,
    pub label: Option<String>,
    pub mount_points: Vec<String>,
    pub parents: Vec<String>,
    pub roles: BTreeSet<String>,
    pub metadata_complete: bool,
    #[serde(default)]
    pub protection: Protection,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Protection {
    pub write_allowed: bool,
    pub hard_protected: bool,
    pub reasons: Vec<Reason>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Reason {
    pub code: String,
    pub message: String,
}
#[derive(Debug, Serialize)]
pub struct Inventory {
    pub schema_version: u32,
    pub platform: &'static str,
    pub snapshot_id: String,
    pub captured_at_unix_ms: u128,
    pub read_only: bool,
    pub write_enabled: bool,
    pub complete: bool,
    pub warnings: Vec<String>,
    pub devices: Vec<Device>,
}

pub fn enumerate() -> Result<Inventory> {
    #[cfg(target_os = "linux")]
    let (mut devices, mut warnings) = linux::enumerate()?;
    #[cfg(windows)]
    let (mut devices, mut warnings) = windows::enumerate()?;
    #[cfg(not(any(target_os = "linux", windows)))]
    return Err(Error::Unsupported(
        "host device discovery supports Windows and Linux".into(),
    ));
    #[cfg(any(target_os = "linux", windows))]
    {
        protect(&mut devices, &mut warnings);
        devices.sort_by(|a, b| a.id.cmp(&b.id));
        let captured_at_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let snapshot_id = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&(
                &devices,
                &warnings,
                captured_at_unix_ms
            ))?)
        );
        Ok(Inventory {
            schema_version: 1,
            platform: std::env::consts::OS,
            snapshot_id,
            captured_at_unix_ms,
            read_only: true,
            write_enabled: false,
            complete: warnings.is_empty() && devices.iter().all(|d| d.metadata_complete),
            warnings,
            devices,
        })
    }
}

pub fn inspect(id: &str) -> Result<serde_json::Value> {
    let inventory = enumerate()?;
    let device = inventory
        .devices
        .iter()
        .find(|d| d.id == id)
        .ok_or_else(|| {
            Error::Invalid("device id is absent from the current inventory; enumerate again".into())
        })?;
    Ok(
        serde_json::json!({"schema_version":1,"snapshot_id":inventory.snapshot_id,
        "read_only":true,"write_enabled":false,"device":device,"warnings":inventory.warnings}),
    )
}

#[cfg(any(target_os = "linux", windows, test))]
fn reason(device: &mut Device, code: &str, message: &str) {
    if !device.protection.reasons.iter().any(|r| r.code == code) {
        device.protection.reasons.push(Reason {
            code: code.into(),
            message: message.into(),
        });
    }
}
#[cfg(any(target_os = "linux", windows, test))]
fn classify(device: &mut Device) {
    let p = device
        .partition_type
        .as_deref()
        .unwrap_or("")
        .trim_matches(['{', '}'])
        .to_ascii_lowercase();
    let role = match p.as_str() {
        "c12a7328-f81f-11d2-ba4b-00a0c93ec93b" | "0xef" => Some("efi"),
        "e3c9e316-0b5c-4db8-817d-f92df00215ae" => Some("reserved"),
        "de94bba4-06d1-4d40-a16a-bfd50179d6ac" | "0x27" | "0x12" | "0xde" => Some("recovery"),
        "21686148-6449-6e6f-744e-656564454649" => Some("boot"),
        "0657fd6d-a4ab-43c4-84e5-0933c84b4f4f" | "0x82" => Some("swap"),
        _ => None,
    };
    if let Some(role) = role {
        device.roles.insert(role.into());
    }
    if device.kind != "disk" && device.kind != "partition" && device.kind != "loop" {
        device.roles.insert("complex_storage".into());
    }
    if let Some(fs) = &device.filesystem {
        if matches!(
            fs.to_ascii_lowercase().as_str(),
            "crypto_luks" | "bitlocker" | "lvm2_member" | "linux_raid_member" | "zfs_member"
        ) {
            device.roles.insert("complex_storage".into());
        }
    }
}
#[cfg(any(target_os = "linux", windows, test))]
fn critical(device: &Device) -> bool {
    [
        "system",
        "boot",
        "efi",
        "reserved",
        "recovery",
        "active_mbr",
        "swap",
        "pagefile",
        "hibernation",
        "crash_dump",
        "runtime",
        "workspace",
        "recovery_storage",
    ]
    .iter()
    .any(|r| device.roles.contains(*r))
}

/// Pure policy shared by CLI/RPC. All current host devices remain read-only.
#[cfg(any(target_os = "linux", windows, test))]
fn protect(devices: &mut [Device], warnings: &mut Vec<String>) {
    for d in devices.iter_mut() {
        d.protection = Protection::default();
        classify(d);
    }
    let indexes: BTreeMap<String, usize> = devices
        .iter()
        .enumerate()
        .map(|(i, d)| (d.id.clone(), i))
        .collect();
    if indexes.len() != devices.len() {
        warnings
            .push("duplicate operating-system device identifiers; inventory is ambiguous".into());
        for d in devices.iter_mut() {
            d.metadata_complete = false;
        }
    }
    for d in devices.iter_mut() {
        if d.parents
            .iter()
            .any(|p| !indexes.contains_key(p) || p == &d.id)
        {
            d.metadata_complete = false;
            warnings.push(format!("{} has an unresolved storage parent", d.id));
        }
    }
    let mut remaining = vec![0usize; devices.len()];
    let mut children = vec![Vec::new(); devices.len()];
    for (i, d) in devices.iter().enumerate() {
        for parent in &d.parents {
            if let Some(&p) = indexes.get(parent) {
                remaining[i] += 1;
                children[p].push(i);
            }
        }
    }
    let mut ready: Vec<_> = remaining
        .iter()
        .enumerate()
        .filter_map(|(i, &n)| (n == 0).then_some(i))
        .collect();
    while let Some(i) = ready.pop() {
        for &child in &children[i] {
            remaining[child] -= 1;
            if remaining[child] == 0 {
                ready.push(child);
            }
        }
    }
    for (i, &n) in remaining.iter().enumerate() {
        if n != 0 {
            devices[i].metadata_complete = false;
            warnings.push(format!(
                "{} has a cyclic or unresolved backing graph",
                devices[i].id
            ));
        }
    }
    let mut identities = BTreeMap::<String, Vec<usize>>::new();
    for (i, d) in devices.iter().enumerate() {
        if let Some(identity) = d.stable_identity.as_ref().filter(|s| !s.trim().is_empty()) {
            identities
                .entry(format!(
                    "{}:{}",
                    d.kind,
                    identity
                        .trim()
                        .trim_matches(['{', '}'])
                        .to_ascii_lowercase()
                ))
                .or_default()
                .push(i);
        }
    }
    for group in identities.values().filter(|g| g.len() > 1) {
        for &i in group {
            devices[i].metadata_complete = false;
            devices[i].roles.insert("ambiguous_identity".into());
        }
    }
    let mut protected: BTreeSet<String> = devices
        .iter()
        .filter(|d| critical(d) || !d.metadata_complete || d.roles.contains("complex_storage"))
        .map(|d| d.id.clone())
        .collect();
    // Protect the backing graph and the rest of a protected disk. Iterate to a fixed point.
    loop {
        let before = protected.len();
        for d in devices.iter() {
            if protected.contains(&d.id) {
                protected.extend(d.parents.iter().cloned());
            }
        }
        for d in devices.iter() {
            if d.parents.iter().any(|p| protected.contains(p)) {
                protected.insert(d.id.clone());
            }
        }
        if before == protected.len() {
            break;
        }
    }
    for d in devices.iter_mut() {
        let roles: Vec<_> = d.roles.iter().cloned().collect();
        for role in roles {
            let message = match role.as_str() {
                "system" => "Operating-system root or Windows boot volume.",
                "boot" => "Boot files or the operating-system boot chain.",
                "efi" => "EFI System Partition.",
                "reserved" => "Microsoft reserved partition.",
                "recovery" => "Recovery or OEM reserved partition.",
                "active_mbr" => "Active MBR boot partition.",
                "swap" | "pagefile" => "Active swap/page file or a reserved swap partition.",
                "hibernation" => "Hibernation storage.",
                "crash_dump" => "Crash dump storage.",
                "runtime" => "Contains the running application or temporary data.",
                "workspace" => "Contains the current workspace.",
                "recovery_storage" => "Contains recovery records.",
                "complex_storage" => "Encrypted, composite or unsupported storage topology.",
                "ambiguous_identity" => {
                    "Device identity is duplicated; refresh cannot establish a unique target."
                }
                _ => continue,
            };
            reason(d, &role, message);
        }
        if protected.contains(&d.id) {
            d.protection.hard_protected = true;
            if !critical(d) {
                reason(d,"protected_backing_or_same_disk","Shares a backing device with protected system, boot, active application or unresolved storage.");
            }
        }
        if !d.mount_points.is_empty() {
            reason(
                d,
                "mounted",
                "Mounted or assigned volume paths; no exclusive lease has been acquired.",
            );
        }
        if d.read_only != Some(false) {
            reason(
                d,
                "read_only_or_unknown",
                "The device is read-only or its writable state is unknown.",
            );
        }
        if !d.metadata_complete {
            reason(
                d,
                "metadata_incomplete",
                "Discovery is incomplete; protection cannot be relaxed.",
            );
        }
        if d.stable_identity.as_ref().is_none_or(|s| s.is_empty()) {
            reason(
                d,
                "identity_unverified",
                "A stable device identity is unavailable.",
            );
        }
        if d.logical_sector_size != 512
            || d.physical_sector_size < 512
            || !d.physical_sector_size.is_power_of_two()
        {
            reason(d,"unsupported_sector_geometry","Only verified 512-byte logical-sector geometry is admitted by the first device design.");
        }
        if d.offset.is_some_and(|o| {
            d.physical_sector_size == 0 || !o.is_multiple_of(u64::from(d.physical_sector_size))
        }) {
            reason(
                d,
                "unaligned_partition",
                "Partition start is not aligned to a verified physical sector.",
            );
        }
        if d.kind == "partition"
            && (d.physical_sector_size == 0
                || !d.size.is_multiple_of(u64::from(d.physical_sector_size)))
        {
            reason(
                d,
                "unaligned_partition_end",
                "Partition size is not aligned to a verified physical sector.",
            );
        }
        reason(d,"volume_state_unverified","Filesystem clean state, encryption and exclusive access require a separate validated session.");
        reason(
            d,
            "device_write_disabled",
            "This release implements discovery only; physical-device writes are disabled.",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn data(id: &str, parents: &[&str]) -> Device {
        Device {
            id: id.into(),
            kind: if parents.is_empty() {
                "disk"
            } else {
                "partition"
            }
            .into(),
            size: 64 << 20,
            logical_sector_size: 512,
            physical_sector_size: 4096,
            read_only: Some(false),
            metadata_complete: true,
            stable_identity: Some(id.into()),
            parents: parents.iter().map(|p| p.to_string()).collect(),
            ..Default::default()
        }
    }
    #[test]
    fn protected_types_backing_graph_and_siblings_cannot_be_overridden() {
        for (ty, role) in [
            ("c12a7328-f81f-11d2-ba4b-00a0c93ec93b", "efi"),
            ("e3c9e316-0b5c-4db8-817d-f92df00215ae", "reserved"),
            ("de94bba4-06d1-4d40-a16a-bfd50179d6ac", "recovery"),
            ("21686148-6449-6e6f-744e-656564454649", "boot"),
        ] {
            let mut nodes = vec![
                data("disk", &[]),
                data("boot", &["disk"]),
                data("data", &["disk"]),
                data("other", &[]),
            ];
            nodes[1].partition_type = Some(ty.into());
            protect(&mut nodes, &mut vec![]);
            assert!(nodes[1].roles.contains(role));
            assert!(nodes[..3].iter().all(|d| d.protection.hard_protected));
            assert!(!nodes[3].protection.hard_protected);
            assert!(nodes.iter().all(|d| !d.protection.write_allowed));
        }
    }
    #[test]
    fn root_swap_runtime_and_unknown_topology_fail_closed() {
        for role in [
            "system",
            "boot",
            "active_mbr",
            "swap",
            "pagefile",
            "runtime",
            "workspace",
        ] {
            let mut nodes = vec![data("disk", &[]), data("part", &["disk"])];
            nodes[1].roles.insert(role.into());
            protect(&mut nodes, &mut vec![]);
            assert!(nodes.iter().all(|d| d.protection.hard_protected));
        }
        let mut nodes = vec![
            data("part", &["missing"]),
            data("duplicate1", &[]),
            data("duplicate2", &[]),
        ];
        nodes[2].stable_identity = nodes[1].stable_identity.clone();
        protect(&mut nodes, &mut vec![]);
        assert!(nodes
            .iter()
            .all(|d| !d.metadata_complete && !d.protection.write_allowed));
    }
    #[test]
    fn raw_methods_remain_disabled_even_with_forged_confirmation() {
        for method in [
            "device.open",
            "device.write",
            "device.commit",
            "device.confirm",
            "device.recover",
        ] {
            let e = crate::execute(
                method,
                serde_json::json!({"id":"windows:disk:0", "force":true, "confirmed":true}),
            )
            .unwrap_err();
            assert!(matches!(e, Error::Unsupported(_)));
        }
        assert!(matches!(
            crate::execute("device.list", serde_json::json!({"write":true})),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            crate::execute(
                "device.inspect",
                serde_json::json!({"id":"x", "force":true})
            ),
            Err(Error::Invalid(_))
        ));
    }
    #[test]
    fn encrypted_storage_and_geometry_are_never_write_eligible() {
        let mut nodes = vec![data("disk", &[]), data("crypt", &["disk"])];
        nodes[1].filesystem = Some("crypto_LUKS".into());
        nodes[1].logical_sector_size = 4096;
        nodes[1].offset = Some(512);
        nodes[1].size = 8193;
        protect(&mut nodes, &mut vec![]);
        assert!(nodes
            .iter()
            .all(|d| d.protection.hard_protected && !d.protection.write_allowed));
        let codes = nodes[1]
            .protection
            .reasons
            .iter()
            .map(|r| r.code.as_str())
            .collect::<BTreeSet<_>>();
        assert!(
            codes.contains("complex_storage")
                && codes.contains("unsupported_sector_geometry")
                && codes.contains("unaligned_partition")
                && codes.contains("unaligned_partition_end")
        );
    }
    #[test]
    fn cyclic_topology_and_case_variant_clone_identity_are_denied() {
        let mut nodes = vec![
            data("a", &["b"]),
            data("b", &["a"]),
            data("clone1", &[]),
            data("clone2", &[]),
        ];
        nodes[2].stable_identity = Some("{ABCD}".into());
        nodes[3].stable_identity = Some("abcd".into());
        let mut warnings = Vec::new();
        protect(&mut nodes, &mut warnings);
        assert!(!warnings.is_empty());
        assert!(nodes.iter().all(|n| !n.metadata_complete
            && n.protection.hard_protected
            && !n.protection.write_allowed));
    }
}
