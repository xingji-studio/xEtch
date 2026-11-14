use crate::{Error, Result};
use serde::Serialize;
use std::io::{Read, Seek, SeekFrom, Write};

pub const SECTOR: u64 = 512;
#[derive(Debug, Clone, Serialize)]
pub struct Partition {
    pub index: u32,
    pub offset: u64,
    pub size: u64,
    pub name: String,
    pub filesystem: String,
}
#[derive(Debug, Serialize)]
pub struct Layout {
    pub table: String,
    pub partitions: Vec<Partition>,
}

fn u32le(b: &[u8]) -> u32 {
    u32::from_le_bytes(b[..4].try_into().unwrap())
}
fn u64le(b: &[u8]) -> u64 {
    u64::from_le_bytes(b[..8].try_into().unwrap())
}
fn bad(message: &str) -> Error {
    Error::Invalid(format!("invalid partition table: {message}"))
}

pub fn filesystem<T: Read + Seek>(disk: &mut T, offset: u64, size: u64) -> Result<String> {
    if size < 512 {
        return Ok("unknown".into());
    }
    disk.seek(SeekFrom::Start(offset))?;
    let mut boot = [0; 512];
    disk.read_exact(&mut boot)?;
    let fs = if &boot[3..11] == b"NTFS    " {
        "NTFS"
    } else if &boot[3..11] == b"EXFAT   " {
        "exFAT"
    } else if &boot[82..90] == b"FAT32   " {
        "FAT32"
    } else if &boot[54..62] == b"FAT16   " {
        "FAT16"
    } else if &boot[54..62] == b"FAT12   " {
        "FAT12"
    } else {
        "unknown"
    };
    if fs != "unknown" {
        return Ok(fs.into());
    }
    if size >= 2048 {
        disk.seek(SeekFrom::Start(offset + 1024))?;
        let mut sb = [0; 104];
        disk.read_exact(&mut sb)?;
        if sb[56..58] == [0x53, 0xef] {
            // Extents distinguish the usual ext4 format from ext2/ext3.
            return Ok(if u32le(&sb[96..100]) & 0x40 != 0 {
                "ext4"
            } else {
                "ext2/ext3"
            }
            .into());
        }
    }
    Ok("unknown".into())
}

pub fn read<T: Read + Seek>(disk: &mut T, size: u64) -> Result<Layout> {
    if size < 512 {
        return Err(bad("image smaller than one sector"));
    }
    let fs = filesystem(disk, 0, size)?;
    if fs != "unknown" {
        return Ok(Layout {
            table: "none".into(),
            partitions: vec![Partition {
                index: 0,
                offset: 0,
                size,
                name: "volume".into(),
                filesystem: fs,
            }],
        });
    }
    disk.seek(SeekFrom::Start(0))?;
    let mut mbr = [0; 512];
    disk.read_exact(&mut mbr)?;
    if mbr[510..] != [0x55, 0xaa] {
        return Ok(Layout {
            table: "none".into(),
            partitions: vec![],
        });
    }
    let protective = mbr[446..510].chunks_exact(16).any(|e| e[4] == 0xee);
    let mut parts = Vec::new();
    let table;
    if protective {
        table = "GPT";
        if mbr[446..510]
            .chunks_exact(16)
            .any(|e| e[4] != 0 && e[4] != 0xee)
        {
            return Err(bad("hybrid MBR is unsupported"));
        }
        let (header, entries) = gpt_header(disk, 1, size)?;
        let backup_lba = u64le(&header[32..40]);
        if backup_lba != size / SECTOR - 1 {
            return Err(bad("GPT backup is not at end of disk"));
        }
        let (backup, backup_entries) = gpt_header(disk, backup_lba, size)?;
        if u64le(&backup[32..40]) != 1
            || header[40..72] != backup[40..72]
            || entries != backup_entries
        {
            return Err(bad("primary and backup GPT disagree"));
        }
        let entry_size = u32le(&header[84..88]) as usize;
        let first = u64le(&header[40..48]);
        let last = u64le(&header[48..56]);
        for (i, entry) in entries.chunks_exact(entry_size).enumerate() {
            if entry[..16] == [0; 16] {
                continue;
            }
            let start = u64le(&entry[32..40]);
            let end = u64le(&entry[40..48]);
            if start < first || end > last || end < start {
                return Err(bad("GPT partition outside usable range"));
            }
            let name: Vec<u16> = entry[56..128]
                .chunks_exact(2)
                .map(|b| u16::from_le_bytes([b[0], b[1]]))
                .take_while(|c| *c != 0)
                .collect();
            parts.push(Partition {
                index: i as u32 + 1,
                offset: start * SECTOR,
                size: (end - start + 1) * SECTOR,
                name: String::from_utf16_lossy(&name),
                filesystem: "unknown".into(),
            });
        }
    } else {
        table = "MBR";
        for (i, e) in mbr[446..510].chunks_exact(16).enumerate() {
            if e[4] == 0 {
                continue;
            }
            if matches!(e[4], 0x05 | 0x0f | 0x85) {
                return Err(Error::Unsupported(
                    "extended MBR partitions are not yet supported".into(),
                ));
            }
            let start = u64::from(u32le(&e[8..12])) * SECTOR;
            let length = u64::from(u32le(&e[12..16])) * SECTOR;
            if start < SECTOR
                || length == 0
                || start.checked_add(length).is_none_or(|end| end > size)
            {
                return Err(bad("MBR partition outside image"));
            }
            parts.push(Partition {
                index: i as u32 + 1,
                offset: start,
                size: length,
                name: format!("partition {}", i + 1),
                filesystem: "unknown".into(),
            });
        }
    }
    let mut ranges: Vec<_> = parts
        .iter()
        .map(|p| (p.offset, p.offset + p.size))
        .collect();
    ranges.sort_unstable();
    if ranges.windows(2).any(|p| p[0].1 > p[1].0) {
        return Err(bad("overlapping partitions"));
    }
    for p in &mut parts {
        p.filesystem = filesystem(disk, p.offset, p.size)?;
    }
    Ok(Layout {
        table: table.into(),
        partitions: parts,
    })
}

fn gpt_header<T: Read + Seek>(disk: &mut T, lba: u64, size: u64) -> Result<(Vec<u8>, Vec<u8>)> {
    disk.seek(SeekFrom::Start(
        lba.checked_mul(SECTOR).ok_or_else(|| bad("GPT overflow"))?,
    ))?;
    let mut h = vec![0; 512];
    disk.read_exact(&mut h)?;
    if &h[..8] != b"EFI PART" || u32le(&h[8..12]) != 0x10000 {
        return Err(bad("GPT signature/version"));
    }
    let length = u32le(&h[12..16]) as usize;
    if !(92..=512).contains(&length) || u64le(&h[24..32]) != lba {
        return Err(bad("GPT header size/location"));
    }
    let checksum = u32le(&h[16..20]);
    h[16..20].fill(0);
    if crc32fast::hash(&h[..length]) != checksum {
        return Err(bad("GPT header CRC"));
    }
    let entry_lba = u64le(&h[72..80]);
    let count = u32le(&h[80..84]) as usize;
    let entry_size = u32le(&h[84..88]) as usize;
    if entry_size < 128 || !entry_size.is_multiple_of(128) || count == 0 {
        return Err(bad("GPT entry geometry"));
    }
    let bytes = count
        .checked_mul(entry_size)
        .filter(|n| *n <= 1024 * 1024)
        .ok_or_else(|| bad("GPT entry array exceeds 1 MiB limit"))?;
    let offset = entry_lba
        .checked_mul(SECTOR)
        .ok_or_else(|| bad("GPT entries overflow"))?;
    if offset
        .checked_add(bytes as u64)
        .is_none_or(|end| end > size)
    {
        return Err(bad("GPT entries outside image"));
    }
    let first = u64le(&h[40..48]);
    let last = u64le(&h[48..56]);
    if first < 34 || last < first || last >= size / SECTOR - 1 {
        return Err(bad("GPT usable range"));
    }
    let entries_end = offset + bytes as u64;
    if (offset < (last + 1) * SECTOR && entries_end > first * SECTOR)
        || (offset <= lba * SECTOR && entries_end > lba * SECTOR)
    {
        return Err(bad("GPT metadata overlaps usable space or header"));
    }
    disk.seek(SeekFrom::Start(offset))?;
    let mut entries = vec![0; bytes];
    disk.read_exact(&mut entries)?;
    if crc32fast::hash(&entries) != u32le(&h[88..92]) {
        return Err(bad("GPT entries CRC"));
    }
    Ok((h, entries))
}

pub fn select(layout: &Layout, index: Option<u32>) -> Result<Partition> {
    match index {
        Some(i) => layout
            .partitions
            .iter()
            .find(|p| p.index == i)
            .cloned()
            .ok_or_else(|| Error::Invalid(format!("partition {i} not found"))),
        None if layout.partitions.len() == 1 => Ok(layout.partitions[0].clone()),
        _ => Err(Error::Invalid(
            "specify --partition when image does not contain exactly one volume".into(),
        )),
    }
}

/// Create a single aligned FAT32 partition on a newly created image only.
pub fn write_table<T: Write + Seek>(
    disk: &mut T,
    size: u64,
    table: &str,
    filesystem: &str,
) -> Result<(u64, u64)> {
    let (mbr_type, gpt_type) = match filesystem {
        "fat32" => (0x0c, fstool::device::gpt::BASIC_DATA),
        "exfat" => (0x07, fstool::device::gpt::BASIC_DATA),
        "ext4" => (0x83, fstool::device::gpt::LINUX_FS),
        _ => {
            return Err(Error::Unsupported(
                "unsupported new partition filesystem".into(),
            ))
        }
    };
    if size < 64 * 1024 * 1024 || !size.is_multiple_of(SECTOR) {
        return Err(Error::Invalid(
            "new partitioned images must be at least 64 MiB and sector aligned".into(),
        ));
    }
    if table == "none" {
        return Ok((0, size));
    }
    let sectors = size / SECTOR;
    let start = 2048u64;
    let end = if table == "gpt" {
        sectors - 34
    } else {
        sectors - 1
    };
    let mut mbr = [0u8; 512];
    mbr[510..].copy_from_slice(&[0x55, 0xaa]);
    let entry = &mut mbr[446..462];
    entry[1..4].copy_from_slice(&[0xfe, 0xff, 0xff]);
    entry[5..8].copy_from_slice(&[0xfe, 0xff, 0xff]);
    if table == "mbr" {
        if sectors > u64::from(u32::MAX) {
            return Err(Error::Invalid(
                "MBR exceeds 512-byte LBA limit; use GPT".into(),
            ));
        }
        entry[4] = mbr_type;
        entry[8..12].copy_from_slice(&(start as u32).to_le_bytes());
        entry[12..16].copy_from_slice(&((end - start + 1) as u32).to_le_bytes());
    } else if table == "gpt" {
        entry[4] = 0xee;
        entry[8..12].copy_from_slice(&1u32.to_le_bytes());
        entry[12..16]
            .copy_from_slice(&((sectors - 1).min(u64::from(u32::MAX)) as u32).to_le_bytes());
        let mut entries = vec![0u8; 128 * 128];
        entries[..16].copy_from_slice(&gpt_type.0);
        entries[16..32].copy_from_slice(&uuid::Uuid::new_v4().to_bytes_le());
        let disk_guid = uuid::Uuid::new_v4().to_bytes_le();
        entries[32..40].copy_from_slice(&start.to_le_bytes());
        entries[40..48].copy_from_slice(&end.to_le_bytes());
        for (i, c) in "xEtch".encode_utf16().enumerate() {
            entries[56 + i * 2..58 + i * 2].copy_from_slice(&c.to_le_bytes());
        }
        for (lba, other, array) in [(1, sectors - 1, 2), (sectors - 1, 1, sectors - 33)] {
            let mut h = [0u8; 512];
            h[..8].copy_from_slice(b"EFI PART");
            h[8..12].copy_from_slice(&0x10000u32.to_le_bytes());
            h[12..16].copy_from_slice(&92u32.to_le_bytes());
            h[24..32].copy_from_slice(&lba.to_le_bytes());
            h[32..40].copy_from_slice(&other.to_le_bytes());
            h[40..48].copy_from_slice(&34u64.to_le_bytes());
            h[48..56].copy_from_slice(&end.to_le_bytes());
            h[56..72].copy_from_slice(&disk_guid);
            h[72..80].copy_from_slice(&array.to_le_bytes());
            h[80..84].copy_from_slice(&128u32.to_le_bytes());
            h[84..88].copy_from_slice(&128u32.to_le_bytes());
            h[88..92].copy_from_slice(&crc32fast::hash(&entries).to_le_bytes());
            let crc = crc32fast::hash(&h[..92]);
            h[16..20].copy_from_slice(&crc.to_le_bytes());
            disk.seek(SeekFrom::Start(array * SECTOR))?;
            disk.write_all(&entries)?;
            disk.seek(SeekFrom::Start(lba * SECTOR))?;
            disk.write_all(&h)?;
        }
    } else {
        return Err(Error::Invalid(
            "partition table must be none, mbr, or gpt".into(),
        ));
    }
    disk.seek(SeekFrom::Start(0))?;
    disk.write_all(&mbr)?;
    Ok((start * SECTOR, (end - start + 1) * SECTOR))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    #[test]
    fn gpt_crc_and_backup_are_validated() {
        let size = 64 * 1024 * 1024;
        let mut disk = Cursor::new(vec![0u8; size]);
        write_table(&mut disk, size as u64, "gpt", "fat32").unwrap();
        let layout = read(&mut disk, size as u64).unwrap();
        assert_eq!(layout.table, "GPT");
        assert_eq!(layout.partitions[0].offset, 1048576);
        disk.get_mut()[512 + 16] ^= 1;
        assert!(read(&mut disk, size as u64).is_err());
        disk.get_mut()[512 + 16] ^= 1;
        disk.get_mut()[size - 512 + 16] ^= 1;
        assert!(read(&mut disk, size as u64).is_err());
    }
    #[test]
    fn overlapping_mbr_and_extended_partitions_are_rejected() {
        let size = 64 * 1024 * 1024;
        let mut disk = Cursor::new(vec![0; size]);
        write_table(&mut disk, size as u64, "mbr", "fat32").unwrap();
        let first: Vec<u8> = disk.get_ref()[446..462].to_vec();
        disk.get_mut()[462..478].copy_from_slice(&first);
        assert!(read(&mut disk, size as u64).is_err());
        disk.get_mut()[462..478].fill(0);
        disk.get_mut()[450] = 0x0f;
        assert!(matches!(
            read(&mut disk, size as u64),
            Err(Error::Unsupported(_))
        ));
    }
    #[test]
    fn malformed_small_inputs_never_panic() {
        for size in [0, 1, 511, 512, 1024, 4096] {
            let mut input = Cursor::new(vec![0xff; size]);
            let _ = read(&mut input, size as u64);
        }
    }
}
