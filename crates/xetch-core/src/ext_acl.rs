//! ext4 ACLs use version 1, with four-byte base entries and eight-byte named
//! entries. This is different from the version 2 userspace getxattr encoding.
use crate::{Error, Result};

pub fn inherit(default: &[u8], mode: u16) -> Result<(Vec<u8>, u16)> {
    let invalid = || Error::Invalid("invalid ext4 default ACL".into());
    if default.get(..4) != Some(&1u32.to_le_bytes()) {
        return Err(invalid());
    }
    let mut entries = Vec::new();
    let mut pos = 4;
    while pos < default.len() {
        let bytes = default.get(pos..pos + 4).ok_or_else(invalid)?;
        let tag = u16::from_le_bytes(bytes[..2].try_into().unwrap());
        let perm = u16::from_le_bytes(bytes[2..].try_into().unwrap());
        if perm > 7 || !matches!(tag, 1 | 2 | 4 | 8 | 16 | 32) {
            return Err(invalid());
        }
        let id = if matches!(tag, 2 | 8) {
            let bytes = default.get(pos + 4..pos + 8).ok_or_else(invalid)?;
            let id = u32::from_le_bytes(bytes.try_into().unwrap());
            if id == u32::MAX {
                return Err(invalid());
            }
            Some(id)
        } else {
            None
        };
        entries.push((pos, tag, perm, id));
        pos += if id.is_some() { 8 } else { 4 };
    }
    let tags: Vec<_> = entries.iter().map(|e| e.1).collect();
    // Canonical order: owner, named users, group, named groups, mask, other.
    if tags.first() != Some(&1)
        || tags.last() != Some(&32)
        || tags.windows(2).any(|p| p[0] > p[1])
        || [1, 4, 32]
            .iter()
            .any(|tag| tags.iter().filter(|t| *t == tag).count() != 1)
        || tags.iter().filter(|t| **t == 16).count() > 1
        || (tags.iter().any(|t| matches!(t, 2 | 8)) && !tags.contains(&16))
        || entries
            .windows(2)
            .any(|p| p[0].1 == p[1].1 && p[0].3 >= p[1].3)
    {
        return Err(invalid());
    }
    let mut access = default.to_vec();
    let mut permissions = 0;
    for (offset, tag, mut perm, _) in entries {
        let shift = match tag {
            1 => Some(6),
            16 => Some(3),
            4 if !tags.contains(&16) => Some(3),
            32 => Some(0),
            _ => None,
        };
        if let Some(shift) = shift {
            perm &= (mode >> shift) & 7;
            access[offset + 2..offset + 4].copy_from_slice(&perm.to_le_bytes());
            permissions |= perm << shift;
        }
    }
    Ok((access, (mode & !0o777) | permissions))
}

#[cfg(test)]
mod tests {
    use super::*;
    // owner=rwx, named uid 1001=rwx, group=r-x, mask=rwx, other=---.
    const ACL: &[u8] = &[
        1, 0, 0, 0, 1, 0, 7, 0, 2, 0, 7, 0, 233, 3, 0, 0, 4, 0, 5, 0, 16, 0, 7, 0, 32, 0, 0, 0,
    ];
    #[test]
    fn inherited_acl_masks_permissions_and_keeps_named_id_and_setgid() {
        let (access, mode) = inherit(ACL, 0o644).unwrap();
        assert_eq!(mode, 0o640);
        assert_eq!(&access[8..20], &ACL[8..20]);
        assert_eq!(access[6], 6);
        assert_eq!(access[22], 4);
        assert_eq!(inherit(ACL, 0o2755).unwrap().1, 0o2750);
    }
    #[test]
    fn malformed_or_userspace_acl_is_rejected() {
        for len in [0, 3, 7, 15, 27] {
            assert!(inherit(&ACL[..len], 0o644).is_err());
        }
        let mut bad = ACL.to_vec();
        bad[0] = 2;
        assert!(inherit(&bad, 0o644).is_err());
        bad = ACL.to_vec();
        bad[6] = 8;
        assert!(inherit(&bad, 0o644).is_err());
    }
}
