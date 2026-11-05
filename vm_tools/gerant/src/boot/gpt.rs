//! The GPT partition table: where the EFI system partition and the
//! extended boot loader partition (XBOOTLDR) are.

use super::{ReadAt, le_u32, le_u64};

/// The partition types of the boot partitions, as stored on disk (the
/// first three fields little-endian).
const ESP: [u8; 16] = guid(
    0xc12a7328,
    0xf81f,
    0x11d2,
    [0xba, 0x4b, 0x00, 0xa0, 0xc9, 0x3e, 0xc9, 0x3b],
);
const XBOOTLDR: [u8; 16] = guid(
    0xbc13c2ff,
    0x59e6,
    0x4262,
    [0xa3, 0x52, 0xb2, 0x75, 0xfd, 0x6f, 0x71, 0x72],
);

/// Bounds on what we read: the table may come from a hostile guest.
const MAX_ENTRIES: u32 = 1024;
const MAX_ENTRY_SIZE: u32 = 4096;
const MAX_TABLE_BYTES: u64 = 1 << 20;

const fn guid(a: u32, b: u16, c: u16, d: [u8; 8]) -> [u8; 16] {
    let a = a.to_le_bytes();
    let b = b.to_le_bytes();
    let c = c.to_le_bytes();
    [
        a[0], a[1], a[2], a[3], b[0], b[1], c[0], c[1], d[0], d[1], d[2], d[3], d[4], d[5], d[6],
        d[7],
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Esp,
    Xbootldr,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Esp => "ESP",
            Kind::Xbootldr => "XBOOTLDR",
        }
    }
}

/// A boot partition, in bytes from the start of the disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Partition {
    pub kind: Kind,
    pub start: u64,
    pub len: u64,
}

/// The disk's boot partitions, in table order; none if it has no GPT.
pub fn boot_partitions(disk: &dyn ReadAt, disk_len: u64) -> Result<Vec<Partition>, String> {
    // The header is in the second sector, which is 512 or 4096 bytes.
    let mut found = None;
    for sector in [512u64, 4096] {
        if sector + 92 > disk_len {
            continue;
        }
        let mut header = [0u8; 92];
        disk.read_exact_at(&mut header, sector)?;
        if &header[0..8] == b"EFI PART" {
            found = Some((sector, header));
            break;
        }
    }
    let Some((sector, header)) = found else {
        return Ok(Vec::new());
    };
    let table_lba = le_u64(&header, 72);
    let count = le_u32(&header, 80);
    let entry_size = le_u32(&header, 84);
    if count > MAX_ENTRIES
        || !(128..=MAX_ENTRY_SIZE).contains(&entry_size)
        || !entry_size.is_multiple_of(8)
    {
        return Err(format!(
            "GPT: unexpected table ({count} entries of {entry_size} bytes)"
        ));
    }
    let table_len = u64::from(count) * u64::from(entry_size);
    let table_start = table_lba
        .checked_mul(sector)
        .filter(|start| table_len <= MAX_TABLE_BYTES && start + table_len <= disk_len)
        .ok_or("GPT: the partition table is outside the disk")?;
    let mut table = vec![0u8; table_len as usize];
    disk.read_exact_at(&mut table, table_start)?;

    let mut partitions = Vec::new();
    for entry in table.chunks_exact(entry_size as usize) {
        let kind = match &entry[0..16] {
            t if t == ESP => Kind::Esp,
            t if t == XBOOTLDR => Kind::Xbootldr,
            _ => continue,
        };
        let (first, last) = (le_u64(entry, 32), le_u64(entry, 40));
        let range = (first <= last)
            .then(|| {
                let len = (last - first).checked_add(1)?.checked_mul(sector)?;
                Some((first.checked_mul(sector)?, len))
            })
            .flatten()
            .filter(|(start, len)| start.checked_add(*len).is_some_and(|end| end <= disk_len));
        match range {
            Some((start, len)) => partitions.push(Partition { kind, start, len }),
            None => return Err(format!("GPT: the {} is outside the disk", kind.as_str())),
        }
    }
    Ok(partitions)
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A disk with 512-byte sectors and the given partitions (type, first
    /// and last sector).
    pub fn disk(size: usize, parts: &[(Kind, u64, u64)]) -> Vec<u8> {
        let mut d = vec![0u8; size];
        d[512..520].copy_from_slice(b"EFI PART");
        d[512 + 72..512 + 80].copy_from_slice(&2u64.to_le_bytes());
        d[512 + 80..512 + 84].copy_from_slice(&128u32.to_le_bytes());
        d[512 + 84..512 + 88].copy_from_slice(&128u32.to_le_bytes());
        for (i, (kind, first, last)) in parts.iter().enumerate() {
            let e = 1024 + i * 128;
            let t = match kind {
                Kind::Esp => ESP,
                Kind::Xbootldr => XBOOTLDR,
            };
            d[e..e + 16].copy_from_slice(&t);
            d[e + 32..e + 40].copy_from_slice(&first.to_le_bytes());
            d[e + 40..e + 48].copy_from_slice(&last.to_le_bytes());
        }
        d
    }

    #[test]
    fn finds_boot_partitions() {
        let d = disk(1 << 20, &[(Kind::Esp, 64, 127), (Kind::Xbootldr, 128, 255)]);
        let parts = boot_partitions(&d, d.len() as u64).unwrap();
        assert_eq!(
            parts,
            [
                Partition {
                    kind: Kind::Esp,
                    start: 64 * 512,
                    len: 64 * 512
                },
                Partition {
                    kind: Kind::Xbootldr,
                    start: 128 * 512,
                    len: 128 * 512
                },
            ]
        );
    }

    #[test]
    fn no_gpt() {
        let d = vec![0u8; 8192];
        assert_eq!(boot_partitions(&d, d.len() as u64).unwrap(), []);
    }

    #[test]
    fn rejects_partitions_outside_the_disk() {
        let d = disk(1 << 20, &[(Kind::Esp, 64, 1 << 40)]);
        assert!(boot_partitions(&d, d.len() as u64).is_err());
        let d = disk(1 << 20, &[(Kind::Esp, u64::MAX / 2, u64::MAX)]);
        assert!(boot_partitions(&d, d.len() as u64).is_err());
    }
}
