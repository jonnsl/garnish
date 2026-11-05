//! The sections of a PE file, for unified kernel images: the kernel is
//! in `.linux`, the initrd in `.initrd` and the kernel's version in
//! `.uname`.

use super::{le_u16, le_u32};

/// x86-64 (IMAGE_FILE_MACHINE_AMD64).
pub const MACHINE_X86_64: u16 = 0x8664;

const MAX_SECTIONS: u16 = 96;

/// Fills a buffer from an offset in the file.
pub type Read<'r> = dyn Fn(&mut [u8], u64) -> Result<(), String> + 'r;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub name: String,
    /// Where its data is in the file, and how much of it there is.
    pub offset: u64,
    pub len: u64,
}

/// The machine type and sections of the PE file `read` reads, `size` bytes
/// long. Only the first copy of a section counts (a multi-profile image
/// repeats them for each profile).
pub fn sections(size: u64, read: &Read) -> Result<(u16, Vec<Section>), String> {
    let mut dos = [0u8; 64];
    if size < 64 {
        return Err("not a PE file".into());
    }
    read(&mut dos, 0)?;
    if &dos[0..2] != b"MZ" {
        return Err("not a PE file".into());
    }
    let pe = u64::from(le_u32(&dos, 0x3c));
    if pe + 24 > size {
        return Err("PE: the header is past the end".into());
    }
    let mut coff = [0u8; 24];
    read(&mut coff, pe)?;
    if &coff[0..4] != b"PE\0\0" {
        return Err("not a PE file".into());
    }
    let machine = le_u16(&coff, 4);
    let count = le_u16(&coff, 6);
    let optional = u64::from(le_u16(&coff, 20));
    if count > MAX_SECTIONS {
        return Err(format!("PE: {count} sections"));
    }
    let table = pe + 24 + optional;
    let table_len = u64::from(count) * 40;
    if table + table_len > size {
        return Err("PE: the section table is past the end".into());
    }
    let mut buf = vec![0u8; table_len as usize];
    read(&mut buf, table)?;
    let mut sections: Vec<Section> = Vec::new();
    for s in buf.as_chunks::<40>().0 {
        let name = String::from_utf8_lossy(&s[0..8])
            .trim_end_matches('\0')
            .to_owned();
        let virtual_size = u64::from(le_u32(s, 8));
        let raw_size = u64::from(le_u32(s, 16));
        let offset = u64::from(le_u32(s, 20));
        // The raw data is padded to the file alignment; the virtual size
        // is the real one, unless it's 0.
        let len = match virtual_size {
            0 => raw_size,
            v => v.min(raw_size),
        };
        if offset + len > size {
            return Err(format!("PE: section {name} is past the end"));
        }
        if sections.iter().any(|known| known.name == name) {
            continue;
        }
        sections.push(Section { name, offset, len });
    }
    Ok((machine, sections))
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A PE file with these sections.
    pub fn pe(machine: u16, sections: &[(&str, &[u8])]) -> Vec<u8> {
        let mut f = vec![0u8; 0x200];
        f[0..2].copy_from_slice(b"MZ");
        f[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        f[0x80..0x84].copy_from_slice(b"PE\0\0");
        f[0x84..0x86].copy_from_slice(&machine.to_le_bytes());
        f[0x86..0x88].copy_from_slice(&(sections.len() as u16).to_le_bytes());
        f[0x94..0x96].copy_from_slice(&0u16.to_le_bytes());
        let mut data_at = 0x200;
        let mut data = Vec::new();
        for (i, (name, bytes)) in sections.iter().enumerate() {
            let s = 0x98 + i * 40;
            let mut n = [0u8; 8];
            n[..name.len()].copy_from_slice(name.as_bytes());
            f[s..s + 8].copy_from_slice(&n);
            let padded = bytes.len().div_ceil(512) * 512;
            f[s + 8..s + 12].copy_from_slice(&(bytes.len() as u32).to_le_bytes());
            f[s + 16..s + 20].copy_from_slice(&(padded as u32).to_le_bytes());
            f[s + 20..s + 24].copy_from_slice(&(data_at as u32).to_le_bytes());
            data.extend_from_slice(bytes);
            data.resize(data.len() + padded - bytes.len(), 0);
            data_at += padded;
        }
        f.extend(data);
        f
    }

    fn reader(f: &[u8]) -> impl Fn(&mut [u8], u64) -> Result<(), String> + '_ {
        move |buf, off| {
            let off = off as usize;
            buf.copy_from_slice(f.get(off..off + buf.len()).ok_or("past the end")?);
            Ok(())
        }
    }

    #[test]
    fn reads_sections() {
        let f = pe(
            MACHINE_X86_64,
            &[
                (".linux", b"kernel"),
                (".initrd", b"initrd!"),
                (".linux", b"other"),
            ],
        );
        let (machine, s) = sections(f.len() as u64, &reader(&f)).unwrap();
        assert_eq!(machine, MACHINE_X86_64);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].name, ".linux");
        let range = s[1].offset as usize..(s[1].offset + s[1].len) as usize;
        assert_eq!(&f[range], b"initrd!");
    }

    #[test]
    fn rejects_bad_files() {
        let f = pe(MACHINE_X86_64, &[(".linux", b"kernel")]);
        for cut in [10, 0x90, 0xb0, 0x201] {
            assert!(
                sections(cut as u64, &reader(&f[..cut])).is_err(),
                "cut at {cut}"
            );
        }
        let mut bad = f.clone();
        bad[0] = b'X';
        assert!(sections(bad.len() as u64, &reader(&bad)).is_err());
        let mut far = f.clone();
        far[0x3c..0x40].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(sections(far.len() as u64, &reader(&far)).is_err());
    }
}
