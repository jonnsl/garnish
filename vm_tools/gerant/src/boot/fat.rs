//! A read-only FAT12/16/32 reader, for the boot partitions. Everything in
//! them comes from the guest, so every number is checked before it's used,
//! cluster chains can't loop, and directories and files have size limits.

use std::cell::RefCell;

use super::{ReadAt, le_u16, le_u32};

/// The most of a directory we read.
const MAX_DIR_BYTES: u64 = 1 << 20;
/// The FAT is read in blocks of this size and the last one is kept.
const FAT_BLOCK: u64 = 64 << 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Fat12,
    Fat16,
    Fat32,
}

#[derive(Debug, Clone, Copy)]
enum Root {
    /// FAT12/16: a fixed area before the data.
    Fixed { offset: u64, len: u64 },
    /// FAT32: a cluster chain like any directory.
    Cluster(u32),
}

/// A file system on part of a disk.
pub struct Fat<'a> {
    disk: &'a dyn ReadAt,
    /// Where it starts on the disk.
    base: u64,
    kind: Kind,
    cluster_size: u64,
    /// Offsets from `base`.
    fat_offset: u64,
    data_offset: u64,
    /// Data clusters, numbered 2..clusters + 2.
    clusters: u32,
    root: Root,
    /// The FAT block read last: its offset and contents.
    cache: RefCell<(u64, Vec<u8>)>,
}

/// A directory entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub dir: bool,
    cluster: u32,
    pub size: u64,
}

/// A file's clusters, to read any part of it.
pub struct File<'f, 'a> {
    fat: &'f Fat<'a>,
    clusters: Vec<u32>,
    pub size: u64,
}

impl<'a> Fat<'a> {
    /// Reads the boot sector of the file system at `base`, `len` bytes long.
    pub fn open(disk: &'a dyn ReadAt, base: u64, len: u64) -> Result<Fat<'a>, String> {
        let mut b = [0u8; 512];
        if len < 512 {
            return Err("too small for FAT".into());
        }
        disk.read_exact_at(&mut b, base)?;
        if b[510..512] != [0x55, 0xaa] {
            return Err("not a FAT file system (no boot signature)".into());
        }
        let bytes_per_sector = u64::from(le_u16(&b, 11));
        let sectors_per_cluster = u64::from(b[13]);
        let reserved = u64::from(le_u16(&b, 14));
        let fats = u64::from(b[16]);
        let root_entries = u64::from(le_u16(&b, 17));
        let total = match le_u16(&b, 19) {
            0 => u64::from(le_u32(&b, 32)),
            n => u64::from(n),
        };
        let fat_size = match le_u16(&b, 22) {
            0 => u64::from(le_u32(&b, 36)),
            n => u64::from(n),
        };
        if ![512, 1024, 2048, 4096].contains(&bytes_per_sector)
            || !sectors_per_cluster.is_power_of_two()
            || sectors_per_cluster * bytes_per_sector > 64 << 10
            || reserved == 0
            || !(1..=4).contains(&fats)
            || fat_size == 0
        {
            return Err("not a FAT file system (bad BIOS parameter block)".into());
        }
        if total * bytes_per_sector > len {
            return Err("the FAT file system is bigger than its partition".into());
        }
        let root_sectors = (root_entries * 32).div_ceil(bytes_per_sector);
        let first_data = reserved + fats * fat_size + root_sectors;
        if first_data >= total {
            return Err("FAT: no room for data".into());
        }
        let clusters = (total - first_data) / sectors_per_cluster;
        let kind = match clusters {
            0..4085 => Kind::Fat12,
            4085..65525 => Kind::Fat16,
            _ => Kind::Fat32,
        };
        let clusters = u32::try_from(clusters)
            .ok()
            .filter(|&c| c <= 0x0fff_fff5)
            .ok_or("FAT: too many clusters")?;
        let entry_bits = match kind {
            Kind::Fat12 => 12,
            Kind::Fat16 => 16,
            Kind::Fat32 => 32,
        };
        if fat_size * bytes_per_sector * 8 / entry_bits < u64::from(clusters) + 2 {
            return Err("FAT: the table is too small for the clusters".into());
        }
        let root = match kind {
            Kind::Fat32 => {
                if root_entries != 0 {
                    return Err("FAT32 with a fixed root directory".into());
                }
                Root::Cluster(le_u32(&b, 44))
            }
            _ => Root::Fixed {
                offset: (reserved + fats * fat_size) * bytes_per_sector,
                len: root_entries * 32,
            },
        };
        Ok(Fat {
            disk,
            base,
            kind,
            cluster_size: sectors_per_cluster * bytes_per_sector,
            fat_offset: reserved * bytes_per_sector,
            data_offset: first_data * bytes_per_sector,
            clusters,
            root,
            cache: RefCell::new((u64::MAX, Vec::new())),
        })
    }

    fn valid_cluster(&self, c: u32) -> bool {
        c >= 2 && c - 2 < self.clusters
    }

    /// `n` bytes of the FAT at `offset`, from the cache if they're there.
    fn fat_bytes(&self, offset: u64, n: usize) -> Result<[u8; 4], String> {
        let mut out = [0u8; 4];
        let block = offset / FAT_BLOCK * FAT_BLOCK;
        let within = (offset - block) as usize;
        let mut cache = self.cache.borrow_mut();
        if cache.0 != block {
            let mut buf = vec![0u8; FAT_BLOCK as usize];
            // The last block may run past the end of the partition; the
            // table itself was checked to fit.
            let fat_end = self.data_offset;
            let len = FAT_BLOCK.min(fat_end.saturating_sub(self.fat_offset + block)) as usize;
            buf.truncate(len);
            self.disk
                .read_exact_at(&mut buf, self.base + self.fat_offset + block)?;
            *cache = (block, buf);
        }
        match cache.1.get(within..within + n) {
            Some(bytes) => out[..n].copy_from_slice(bytes),
            None => {
                // Straddles two blocks (FAT12).
                self.disk
                    .read_exact_at(&mut out[..n], self.base + self.fat_offset + offset)?;
            }
        }
        Ok(out)
    }

    /// The cluster after `c`, or None at the end of the chain.
    fn next(&self, c: u32) -> Result<Option<u32>, String> {
        let (value, end) = match self.kind {
            Kind::Fat12 => {
                let b = self.fat_bytes(u64::from(c) + u64::from(c / 2), 2)?;
                let v = u32::from(u16::from_le_bytes([b[0], b[1]]));
                (if c % 2 == 1 { v >> 4 } else { v & 0xfff }, 0xff8)
            }
            Kind::Fat16 => {
                let b = self.fat_bytes(u64::from(c) * 2, 2)?;
                (u32::from(u16::from_le_bytes([b[0], b[1]])), 0xfff8)
            }
            Kind::Fat32 => {
                let b = self.fat_bytes(u64::from(c) * 4, 4)?;
                (u32::from_le_bytes(b) & 0x0fff_ffff, 0x0fff_fff8)
            }
        };
        if value >= end {
            Ok(None)
        } else if self.valid_cluster(value) {
            Ok(Some(value))
        } else {
            Err(format!(
                "FAT: broken cluster chain (cluster {c} -> {value:#x})"
            ))
        }
    }

    /// The chain from `first`: exactly `count` clusters, or up to `count`
    /// if `exact` is false.
    fn chain(&self, first: u32, count: u64, exact: bool) -> Result<Vec<u32>, String> {
        let mut chain = Vec::new();
        let mut c = Some(first);
        while (chain.len() as u64) < count {
            let Some(cluster) = c else {
                if exact {
                    return Err("FAT: a file's cluster chain is shorter than the file".into());
                }
                break;
            };
            if !self.valid_cluster(cluster) {
                return Err(format!("FAT: invalid cluster {cluster:#x}"));
            }
            chain.push(cluster);
            c = self.next(cluster)?;
        }
        if !exact && c.is_some() {
            return Err("FAT: a directory is too big".into());
        }
        Ok(chain)
    }

    fn cluster_offset(&self, c: u32) -> u64 {
        self.base + self.data_offset + u64::from(c - 2) * self.cluster_size
    }

    fn read_dir_bytes(&self, root: Option<Root>, dir: Option<&Entry>) -> Result<Vec<u8>, String> {
        let first = match (root, dir) {
            (Some(Root::Fixed { offset, len }), _) => {
                let mut buf = vec![0u8; len as usize];
                self.disk.read_exact_at(&mut buf, self.base + offset)?;
                return Ok(buf);
            }
            (Some(Root::Cluster(c)), _) => c,
            (None, Some(e)) => e.cluster,
            (None, None) => unreachable!(),
        };
        let max = (MAX_DIR_BYTES / self.cluster_size).max(1);
        let mut buf = Vec::new();
        for c in self.chain(first, max, false)? {
            let start = buf.len();
            buf.resize(start + self.cluster_size as usize, 0);
            self.disk
                .read_exact_at(&mut buf[start..], self.cluster_offset(c))?;
        }
        Ok(buf)
    }

    /// The root directory's entries.
    pub fn root(&self) -> Result<Vec<Entry>, String> {
        Ok(parse_dir(&self.read_dir_bytes(Some(self.root), None)?))
    }

    /// The entries of the directory `dir`.
    pub fn list(&self, dir: &Entry) -> Result<Vec<Entry>, String> {
        if !dir.dir {
            return Err(format!("{} isn't a directory", dir.name));
        }
        if dir.cluster == 0 {
            // ".." of a top-level directory; we skip those anyway.
            return self.root();
        }
        Ok(parse_dir(&self.read_dir_bytes(None, Some(dir))?))
    }

    /// The entry at `path` (from the root, `/`-separated, any case), if
    /// there is one.
    pub fn lookup(&self, path: &str) -> Result<Option<Entry>, String> {
        let mut entries = self.root()?;
        let mut parts = path.split('/').filter(|p| !p.is_empty()).peekable();
        while let Some(part) = parts.next() {
            if part == "." || part == ".." {
                return Err(format!("{path}: . and .. aren't allowed"));
            }
            let Some(entry) = entries.into_iter().find(|e| same_name(&e.name, part)) else {
                return Ok(None);
            };
            if parts.peek().is_none() {
                return Ok(Some(entry));
            }
            if !entry.dir {
                return Ok(None);
            }
            entries = self.list(&entry)?;
        }
        Ok(None)
    }

    /// The entries of the directory at `path`; none if it doesn't exist.
    pub fn list_path(&self, path: &str) -> Result<Vec<Entry>, String> {
        match self.lookup(path)? {
            Some(dir) if dir.dir => self.list(&dir),
            _ => Ok(Vec::new()),
        }
    }

    /// A file, to read from. `max` bounds its size.
    pub fn file(&self, entry: &Entry, max: u64) -> Result<File<'_, 'a>, String> {
        if entry.dir {
            return Err(format!("{} is a directory", entry.name));
        }
        if entry.size > max {
            return Err(format!(
                "{} is too big ({} MiB, at most {} MiB)",
                entry.name,
                entry.size >> 20,
                max >> 20
            ));
        }
        let count = entry.size.div_ceil(self.cluster_size);
        let clusters = if count == 0 {
            Vec::new()
        } else {
            self.chain(entry.cluster, count, true)?
        };
        Ok(File {
            fat: self,
            clusters,
            size: entry.size,
        })
    }
}

impl File<'_, '_> {
    /// Fills `buf` from `offset` in the file.
    pub fn read_at(&self, buf: &mut [u8], offset: u64) -> Result<(), String> {
        let end = offset
            .checked_add(buf.len() as u64)
            .filter(|&end| end <= self.size)
            .ok_or("read past the end of a file")?;
        let cs = self.fat.cluster_size;
        let mut pos = offset;
        let mut done = 0;
        while pos < end {
            let index = (pos / cs) as usize;
            let within = pos % cs;
            // Read contiguous clusters at once.
            let mut run = 1;
            while index + run < self.clusters.len()
                && self.clusters[index + run] == self.clusters[index] + run as u32
                && pos - within + run as u64 * cs < end
            {
                run += 1;
            }
            let n = (run as u64 * cs - within).min(end - pos) as usize;
            self.fat.disk.read_exact_at(
                &mut buf[done..done + n],
                self.fat.cluster_offset(self.clusters[index]) + within,
            )?;
            pos += n as u64;
            done += n;
        }
        Ok(())
    }

    /// The whole file, which has to be small.
    pub fn read_all(&self) -> Result<Vec<u8>, String> {
        let mut buf = vec![0u8; self.size as usize];
        self.read_at(&mut buf, 0)?;
        Ok(buf)
    }
}

/// FAT names compare without case.
pub fn same_name(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b) || (!a.is_ascii() && a.to_lowercase() == b.to_lowercase())
}

/// The checksum of an 8.3 name that its long name entries carry.
fn short_checksum(name: &[u8]) -> u8 {
    name.iter()
        .fold(0u8, |sum, &c| sum.rotate_right(1).wrapping_add(c))
}

fn short_name(e: &[u8]) -> String {
    let lower_base = e[12] & 0x08 != 0;
    let lower_ext = e[12] & 0x10 != 0;
    let part = |bytes: &[u8], lower: bool| -> String {
        let s: String = bytes
            .iter()
            .enumerate()
            .map(|(i, &c)| if i == 0 && c == 0x05 { 0xe5 } else { c })
            .map(char::from)
            .collect::<String>()
            .trim_end_matches(' ')
            .to_owned();
        if lower { s.to_ascii_lowercase() } else { s }
    };
    let base = part(&e[0..8], lower_base);
    let ext = part(&e[8..11], lower_ext);
    if ext.is_empty() {
        base
    } else {
        format!("{base}.{ext}")
    }
}

/// The entries in a directory's bytes, without `.`, `..` and the volume
/// label.
fn parse_dir(bytes: &[u8]) -> Vec<Entry> {
    let mut entries = Vec::new();
    // The long name being put together: its parts by number, and the
    // checksum they carry.
    let mut long: Vec<Option<[u16; 13]>> = Vec::new();
    let mut checksum = 0u8;
    for e in bytes.as_chunks::<32>().0 {
        match e[0] {
            0 => break,
            0xe5 => {
                long.clear();
                continue;
            }
            _ => {}
        }
        let attr = e[11];
        if attr & 0x3f == 0x0f {
            let ord = e[0] & 0x3f;
            if e[0] & 0x40 != 0 {
                long = vec![None; usize::from(ord)];
                checksum = e[13];
            }
            let index = usize::from(ord).wrapping_sub(1);
            if ord == 0 || index >= long.len() || e[13] != checksum {
                long.clear();
                continue;
            }
            let mut units = [0u16; 13];
            for (i, off) in (1..11)
                .step_by(2)
                .chain((14..26).step_by(2))
                .chain((28..32).step_by(2))
                .enumerate()
            {
                units[i] = le_u16(e, off);
            }
            long[index] = Some(units);
            continue;
        }
        let name = if !long.is_empty()
            && long.iter().all(Option::is_some)
            && short_checksum(&e[0..11]) == checksum
        {
            let units: Vec<u16> = long
                .iter()
                .flatten()
                .flatten()
                .copied()
                .take_while(|&u| u != 0)
                .collect();
            String::from_utf16_lossy(&units)
        } else {
            short_name(e)
        };
        long.clear();
        if attr & 0x08 != 0 || name == "." || name == ".." || name.is_empty() {
            continue;
        }
        entries.push(Entry {
            name,
            dir: attr & 0x10 != 0,
            cluster: (u32::from(le_u16(e, 20)) << 16) | u32::from(le_u16(e, 26)),
            size: u64::from(le_u32(e, 28)),
        });
    }
    entries
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// Builds small FAT16 file systems for the tests: 512-byte sectors and
    /// clusters, one FAT, a fixed root directory. Files go in contiguous
    /// clusters unless told otherwise.
    pub struct Builder {
        pub image: Vec<u8>,
        next_cluster: u32,
        /// For unique 8.3 names.
        names: u32,
        /// Directory clusters' fill level, by cluster (0 for the root).
        dir_used: Vec<(u32, usize)>,
    }

    const SECTORS: u64 = 8192;
    const FAT_SECTORS: u64 = 32;
    const ROOT_ENTRIES: u64 = 64;
    pub const DATA: usize = (1 + FAT_SECTORS as usize + 4) * 512;

    impl Default for Builder {
        fn default() -> Builder {
            Builder::new()
        }
    }

    impl Builder {
        pub fn new() -> Builder {
            let mut b = vec![0u8; (SECTORS * 512) as usize];
            b[11..13].copy_from_slice(&512u16.to_le_bytes());
            b[13] = 1;
            b[14..16].copy_from_slice(&1u16.to_le_bytes());
            b[16] = 1;
            b[17..19].copy_from_slice(&(ROOT_ENTRIES as u16).to_le_bytes());
            b[19..21].copy_from_slice(&(SECTORS as u16).to_le_bytes());
            b[22..24].copy_from_slice(&(FAT_SECTORS as u16).to_le_bytes());
            b[510] = 0x55;
            b[511] = 0xaa;
            Builder {
                image: b,
                next_cluster: 2,
                names: 0,
                dir_used: vec![(0, 0)],
            }
        }

        fn cluster_at(c: u32) -> usize {
            DATA + (c as usize - 2) * 512
        }

        pub fn set_fat(&mut self, c: u32, value: u16) {
            let off = 512 + c as usize * 2;
            self.image[off..off + 2].copy_from_slice(&value.to_le_bytes());
        }

        fn add_entry(&mut self, dir: u32, raw: [u8; 32]) {
            let (base, cap) = if dir == 0 {
                ((1 + FAT_SECTORS as usize) * 512, ROOT_ENTRIES as usize * 32)
            } else {
                (Self::cluster_at(dir), 512)
            };
            let slot = self.dir_used.iter_mut().find(|(c, _)| *c == dir).unwrap();
            assert!(slot.1 + 32 <= cap, "directory full");
            let off = base + slot.1;
            slot.1 += 32;
            self.image[off..off + 32].copy_from_slice(&raw);
        }

        /// Adds a long name entry set and its 8.3 entry for `name`.
        fn add_named(&mut self, dir: u32, name: &str, attr: u8, cluster: u32, size: u32) {
            self.names += 1;
            let mut short = [b' '; 11];
            short[..8].copy_from_slice(format!("~{:07}", self.names).as_bytes());
            let units: Vec<u16> = name.encode_utf16().collect();
            let parts = units.len().div_ceil(13);
            let sum = short_checksum(&short);
            for p in (0..parts).rev() {
                let mut e = [0u8; 32];
                e[0] = (p + 1) as u8 | if p + 1 == parts { 0x40 } else { 0 };
                e[11] = 0x0f;
                e[13] = sum;
                let offs: Vec<usize> = (1..11)
                    .step_by(2)
                    .chain((14..26).step_by(2))
                    .chain((28..32).step_by(2))
                    .collect();
                for (i, off) in offs.into_iter().enumerate() {
                    let u = match units.get(p * 13 + i) {
                        Some(&u) => u,
                        None if p * 13 + i == units.len() => 0,
                        None => 0xffff,
                    };
                    e[off..off + 2].copy_from_slice(&u.to_le_bytes());
                }
                self.add_entry(dir, e);
            }
            let mut e = [0u8; 32];
            e[..11].copy_from_slice(&short);
            e[11] = attr;
            e[20..22].copy_from_slice(&((cluster >> 16) as u16).to_le_bytes());
            e[26..28].copy_from_slice(&(cluster as u16).to_le_bytes());
            e[28..32].copy_from_slice(&size.to_le_bytes());
            self.add_entry(dir, e);
        }

        fn alloc(&mut self, n: u32) -> u32 {
            let first = self.next_cluster;
            self.next_cluster += n;
            for c in first..first + n {
                self.set_fat(
                    c,
                    if c + 1 < first + n {
                        (c + 1) as u16
                    } else {
                        0xffff
                    },
                );
            }
            first
        }

        /// Makes a directory in `dir` (0 for the root); returns its cluster.
        pub fn mkdir(&mut self, dir: u32, name: &str) -> u32 {
            let c = self.alloc(1);
            self.add_named(dir, name, 0x10, c, 0);
            self.dir_used.push((c, 0));
            let mut dot = [0u8; 32];
            dot[..11].copy_from_slice(b".          ");
            dot[11] = 0x10;
            self.add_entry(c, dot);
            c
        }

        /// Makes a file in `dir`; returns its first cluster.
        pub fn file(&mut self, dir: u32, name: &str, data: &[u8]) -> u32 {
            let n = data.len().div_ceil(512) as u32;
            let c = if n == 0 { 0 } else { self.alloc(n) };
            if n > 0 {
                let at = Self::cluster_at(c);
                self.image[at..at + data.len()].copy_from_slice(data);
            }
            self.add_named(dir, name, 0x20, c, data.len() as u32);
            c
        }

        /// Makes `path` (from the root) with its directories.
        pub fn put(&mut self, path: &str, data: &[u8]) {
            let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
            let mut dir = 0;
            for part in &parts[..parts.len() - 1] {
                dir = match self.find_dir(dir, part) {
                    Some(c) => c,
                    None => self.mkdir(dir, part),
                };
            }
            self.file(dir, parts[parts.len() - 1], data);
        }

        fn find_dir(&self, dir: u32, name: &str) -> Option<u32> {
            let fat = Fat::open(&self.image, 0, self.image.len() as u64).unwrap();
            let entries = if dir == 0 {
                fat.root().unwrap()
            } else {
                fat.list(&Entry {
                    name: String::new(),
                    dir: true,
                    cluster: dir,
                    size: 0,
                })
                .unwrap()
            };
            entries
                .into_iter()
                .find(|e| e.dir && e.name == name)
                .map(|e| e.cluster)
        }
    }

    #[test]
    fn reads_files_by_path() {
        let mut b = Builder::new();
        let big: Vec<u8> = (0..2000u32).map(|i| i as u8).collect();
        b.put("/EFI/Linux/a-very-long-file-name.efi", &big);
        b.put("/loader/entries/x.conf", b"linux /x\n");
        let fat = Fat::open(&b.image, 0, b.image.len() as u64).unwrap();
        let e = fat
            .lookup("/efi/LINUX/A-Very-Long-File-Name.EFI")
            .unwrap()
            .unwrap();
        assert_eq!(e.name, "a-very-long-file-name.efi");
        let f = fat.file(&e, 1 << 20).unwrap();
        assert_eq!(f.read_all().unwrap(), big);
        let mut part = [0u8; 700];
        f.read_at(&mut part, 1000).unwrap();
        assert_eq!(&part[..], &big[1000..1700]);
        assert!(f.read_at(&mut part, 1500).is_err());
        assert!(fat.file(&e, 1000).is_err());
        let names: Vec<String> = fat
            .list_path("/loader/entries")
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, ["x.conf"]);
        assert_eq!(fat.lookup("/nope/x").unwrap(), None);
        assert!(fat.lookup("/EFI/../EFI").is_err());
    }

    #[test]
    fn cluster_loops_end() {
        // A file's chain that loops is only followed for the file's size.
        let mut b = Builder::new();
        b.put("/f", &[1u8; 1500]);
        b.set_fat(4, 2);
        let fat = Fat::open(&b.image, 0, b.image.len() as u64).unwrap();
        let e = fat.lookup("/f").unwrap().unwrap();
        assert_eq!(
            fat.file(&e, 1 << 20).unwrap().read_all().unwrap(),
            [1u8; 1500]
        );
        // A directory's has no size: it's an error.
        let mut b = Builder::new();
        let d = b.mkdir(0, "d");
        b.set_fat(d, d as u16);
        let fat = Fat::open(&b.image, 0, b.image.len() as u64).unwrap();
        assert!(fat.lookup("/d/x").is_err());
    }

    #[test]
    fn short_chains_and_bad_clusters_are_errors() {
        let mut b = Builder::new();
        b.put("/f", &[1u8; 1500]);
        b.set_fat(3, 0xffff);
        let fat = Fat::open(&b.image, 0, b.image.len() as u64).unwrap();
        let e = fat.lookup("/f").unwrap().unwrap();
        assert!(fat.file(&e, 1 << 20).is_err());
        b.set_fat(3, 0x7fff);
        let fat = Fat::open(&b.image, 0, b.image.len() as u64).unwrap();
        assert!(fat.file(&e, 1 << 20).is_err());
    }

    #[test]
    fn rejects_bad_boot_sectors() {
        let b = Builder::new();
        let mut img = b.image.clone();
        img[510] = 0;
        assert!(Fat::open(&img, 0, img.len() as u64).is_err());
        let mut img = b.image.clone();
        img[13] = 3;
        assert!(Fat::open(&img, 0, img.len() as u64).is_err());
        // Bigger than its partition.
        assert!(Fat::open(&b.image, 0, b.image.len() as u64 - 512).is_err());
    }

    #[test]
    fn short_names() {
        let mut e = [0u8; 32];
        e[..11].copy_from_slice(b"BOOTX64 EFI");
        assert_eq!(short_name(&e), "BOOTX64.EFI");
        e[12] = 0x18;
        assert_eq!(short_name(&e), "bootx64.efi");
    }
}
