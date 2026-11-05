//! The kernels in a disk's boot partitions, as the Boot Loader
//! Specification lays them out: type 1 entries (`/loader/entries/*.conf`,
//! naming a kernel and initrds) and unified kernel images
//! (`/EFI/Linux/*.efi`), in the EFI system partition and the XBOOTLDR
//! partition. They are ordered newest first, much like systemd-boot does.

use std::cmp::Ordering;
use std::rc::Rc;

use super::fat::{self, Entry, Fat};
use super::gpt::{self, Partition};
use super::{MAX_INITRD, MAX_KERNEL, ReadAt, le_u16, pe, version};

const MAX_CONF: u64 = 64 << 10;
const MAX_SMALL_SECTION: u64 = 4096;
/// Entries looked at per directory.
const MAX_CANDIDATES: usize = 64;
/// The most a unified kernel image may weigh.
const MAX_UKI: u64 = MAX_KERNEL + MAX_INITRD + (16 << 20);

/// garnish is for x86-64 guests only.
const MACHINE: u16 = pe::MACHINE_X86_64;
/// `architecture` in type 1 entries.
const ARCH_NAME: &str = "x64";

/// The boot partitions of a disk that could be read.
pub struct Disk<'a> {
    parts: Vec<(Partition, Fat<'a>)>,
}

/// Part of a file: a whole kernel or initrd, or a section of a unified
/// kernel image.
pub struct Piece<'d, 'a> {
    file: Rc<fat::File<'d, 'a>>,
    offset: u64,
    pub len: u64,
}

impl Piece<'_, '_> {
    pub fn read_at(&self, buf: &mut [u8], offset: u64) -> Result<(), String> {
        if offset + buf.len() as u64 > self.len {
            return Err("read past the end of a kernel or initrd".into());
        }
        self.file.read_at(buf, self.offset + offset)
    }
}

/// A kernel to boot.
pub struct Plan<'d, 'a> {
    /// Where it is, for people: `ESP:/EFI/Linux/arch-linux.efi`.
    pub name: String,
    /// The entry's name without its boot counter, the last thing it's
    /// ordered by.
    pub id: String,
    /// The kernel's version, if known.
    pub version: Option<String>,
    /// Its boot counter ran out: the guest's boot loader wouldn't try it.
    pub bad: bool,
    pub kernel: Piece<'d, 'a>,
    /// Booted one after the other, each padded to 4 bytes.
    pub initrds: Vec<Piece<'d, 'a>>,
}

impl Plan<'_, '_> {
    /// The length of the initrds, as one.
    pub fn initrd_len(&self) -> u64 {
        self.initrds.iter().map(|p| p.len.next_multiple_of(4)).sum()
    }
}

/// What a type 1 entry says.
#[derive(Debug, Default, PartialEq)]
struct Conf {
    version: Option<String>,
    linux: Option<String>,
    initrds: Vec<String>,
    uki: Option<String>,
    architecture: Option<String>,
}

fn parse_conf(text: &str) -> Conf {
    let mut conf = Conf::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line
            .split_once(|c: char| c.is_ascii_whitespace())
            .unwrap_or((line, ""));
        let value = value.trim().to_owned();
        match key {
            "version" => conf.version = Some(value),
            "linux" => conf.linux = Some(value),
            "initrd" => conf
                .initrds
                .extend(value.split_ascii_whitespace().map(str::to_owned)),
            "uki" => conf.uki = Some(value),
            "architecture" => conf.architecture = Some(value),
            _ => {}
        }
    }
    conf
}

/// `name+LEFT-DONE` or `name+LEFT` (boot counting): the name without the
/// counter, and whether no tries are left.
fn split_counter(stem: &str) -> (&str, bool) {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit());
    if let Some((id, counter)) = stem.rsplit_once('+') {
        let (left, done) = counter.split_once('-').unwrap_or((counter, "0"));
        if digits(left) && digits(done) {
            return (id, left.bytes().all(|c| c == b'0'));
        }
    }
    (stem, false)
}

/// Strips `suffix` from `name`, ignoring case.
fn strip_suffix_ci<'n>(name: &'n str, suffix: &str) -> Option<&'n str> {
    let at = name.len().checked_sub(suffix.len())?;
    (name.is_char_boundary(at) && name[at..].eq_ignore_ascii_case(suffix)).then(|| &name[..at])
}

/// Checks that the kernel is one crosvm can boot, and returns its version
/// if it says.
fn check_kernel(kernel: &Piece) -> Result<Option<String>, String> {
    // The bzImage setup header (Documentation/arch/x86/boot.rst).
    let mut h = [0u8; 0x210];
    if kernel.len < h.len() as u64 {
        return Err("the kernel is too small".into());
    }
    kernel.read_at(&mut h, 0)?;
    if h[0x1fe..0x200] != [0x55, 0xaa] || &h[0x202..0x206] != b"HdrS" {
        return Err("the kernel isn't a bzImage".into());
    }
    // kernel_version points to "6.17.1-arch1-1 (builder@host) #1 ...".
    let at = u64::from(le_u16(&h, 0x20e));
    if at == 0 {
        return Ok(None);
    }
    let at = at + 0x200;
    let mut s = [0u8; 128];
    let n = (kernel.len.saturating_sub(at)).min(s.len() as u64) as usize;
    kernel.read_at(&mut s[..n], at)?;
    let release = s[..n]
        .split(|&c| c == 0 || c == b' ')
        .next()
        .unwrap_or_default();
    Ok(
        (!release.is_empty() && release.iter().all(u8::is_ascii_graphic))
            .then(|| String::from_utf8_lossy(release).into_owned()),
    )
}

impl<'a> Disk<'a> {
    /// Finds the boot partitions of `disk` (`len` bytes long). What can't
    /// be read goes to `warnings`.
    pub fn open(
        disk: &'a dyn ReadAt,
        len: u64,
        warnings: &mut Vec<String>,
    ) -> Result<Disk<'a>, String> {
        let mut parts = Vec::new();
        for part in gpt::boot_partitions(disk, len)? {
            match Fat::open(disk, part.start, part.len) {
                Ok(fat) => parts.push((part, fat)),
                Err(e) => warnings.push(format!("{}: {e}", part.kind.as_str())),
            }
        }
        Ok(Disk { parts })
    }

    /// Every kernel that could be booted, the one to boot first. The
    /// broken ones go to `warnings`.
    pub fn plans<'d>(&'d self, warnings: &mut Vec<String>) -> Vec<Plan<'d, 'a>> {
        let mut plans = Vec::new();
        for (part, fat) in &self.parts {
            let kind = part.kind.as_str();
            let mut add = |name: String, plan: Result<Option<Plan<'d, 'a>>, String>| match plan {
                Ok(Some(plan)) => plans.push(plan),
                Ok(None) => {}
                Err(e) => warnings.push(format!("{name}: {e}")),
            };
            for (dir, suffix) in [("/loader/entries", ".conf"), ("/EFI/Linux", ".efi")] {
                let entries = match fat.list_path(dir) {
                    Ok(entries) => entries,
                    Err(e) => {
                        add(format!("{kind}:{dir}"), Err(e));
                        continue;
                    }
                };
                let files: Vec<(Entry, &str)> = entries
                    .iter()
                    .filter(|e| !e.dir && !e.name.starts_with('.'))
                    .filter_map(|e| Some((e.clone(), strip_suffix_ci(&e.name, suffix)?)))
                    .collect();
                if files.len() > MAX_CANDIDATES {
                    add(
                        format!("{kind}:{dir}"),
                        Err(format!(
                            "only the first {MAX_CANDIDATES} entries are looked at"
                        )),
                    );
                }
                for (entry, stem) in files.iter().take(MAX_CANDIDATES) {
                    let name = format!("{kind}:{dir}/{}", entry.name);
                    let (id, bad) = split_counter(stem);
                    let plan = if suffix == ".conf" {
                        Self::type1(fat, entry, &name, id, bad)
                    } else {
                        Self::uki(fat, entry, &name, id, bad).map(Some)
                    };
                    add(name, plan);
                }
            }
        }
        plans.sort_by(order);
        plans
    }

    /// A type 1 entry: its kernel and initrds, or a unified kernel image.
    /// None for entries for other architectures or that boot no Linux.
    fn type1<'d>(
        fat: &'d Fat<'a>,
        entry: &Entry,
        name: &str,
        id: &str,
        bad: bool,
    ) -> Result<Option<Plan<'d, 'a>>, String> {
        let conf = parse_conf(&String::from_utf8_lossy(
            &fat.file(entry, MAX_CONF)?.read_all()?,
        ));
        if conf
            .architecture
            .as_deref()
            .is_some_and(|a| !a.eq_ignore_ascii_case(ARCH_NAME))
        {
            return Ok(None);
        }
        let open = |path: &str, max: u64| -> Result<Rc<fat::File<'d, 'a>>, String> {
            let found = fat
                .lookup(path)?
                .ok_or_else(|| format!("{path} doesn't exist"))?;
            Ok(Rc::new(fat.file(&found, max)?))
        };
        if let Some(uki) = &conf.uki {
            let entry = fat
                .lookup(uki)?
                .ok_or_else(|| format!("{uki} doesn't exist"))?;
            let mut plan = Self::uki(fat, &entry, name, id, bad)?;
            plan.version = conf.version.or(plan.version);
            return Ok(Some(plan));
        }
        let Some(linux) = &conf.linux else {
            return Ok(None);
        };
        let whole = |file: Rc<fat::File<'d, 'a>>| Piece {
            len: file.size,
            file,
            offset: 0,
        };
        let kernel = whole(open(linux, MAX_KERNEL)?);
        let initrds = conf
            .initrds
            .iter()
            .map(|path| open(path, MAX_INITRD).map(whole))
            .collect::<Result<Vec<_>, _>>()?;
        let mut plan = Plan {
            name: name.to_owned(),
            id: id.to_owned(),
            version: None,
            bad,
            kernel,
            initrds,
        };
        if plan.initrd_len() > MAX_INITRD {
            return Err(format!(
                "the initrds are bigger than {} MiB",
                MAX_INITRD >> 20
            ));
        }
        let from_kernel = check_kernel(&plan.kernel)?;
        plan.version = conf.version.filter(|v| !v.is_empty()).or(from_kernel);
        Ok(Some(plan))
    }

    /// A unified kernel image.
    fn uki<'d>(
        fat: &'d Fat<'a>,
        entry: &Entry,
        name: &str,
        id: &str,
        bad: bool,
    ) -> Result<Plan<'d, 'a>, String> {
        let file = Rc::new(fat.file(entry, MAX_UKI)?);
        let (machine, sections) = pe::sections(file.size, &|buf, off| file.read_at(buf, off))?;
        if machine != MACHINE {
            return Err(format!("for another architecture (machine {machine:#06x})"));
        }
        let section = |name: &str, max: u64| -> Result<Option<Piece<'d, 'a>>, String> {
            match sections.iter().find(|s| s.name == name) {
                Some(s) if s.len > max => Err(format!(
                    "{name} is too big ({} MiB, at most {} MiB)",
                    s.len >> 20,
                    max >> 20
                )),
                Some(s) => Ok(Some(Piece {
                    file: Rc::clone(&file),
                    offset: s.offset,
                    len: s.len,
                })),
                None => Ok(None),
            }
        };
        let kernel = section(".linux", MAX_KERNEL)?
            .ok_or("no .linux section: not a unified kernel image")?;
        let initrds = section(".initrd", MAX_INITRD)?.into_iter().collect();
        let uname = match section(".uname", MAX_SMALL_SECTION)? {
            Some(p) => {
                let mut buf = vec![0u8; p.len as usize];
                p.read_at(&mut buf, 0)?;
                Some(
                    String::from_utf8_lossy(&buf)
                        .trim_matches(['\0', ' ', '\n'])
                        .to_owned(),
                )
                .filter(|s| !s.is_empty())
            }
            None => None,
        };
        let from_kernel = check_kernel(&kernel)?;
        Ok(Plan {
            name: name.to_owned(),
            id: id.to_owned(),
            version: uname.or(from_kernel),
            bad,
            kernel,
            initrds,
        })
    }
}

/// Which plan goes first: one that isn't marked bad, then the newest
/// version (a known one before none), then the name that sorts last.
fn order(a: &Plan, b: &Plan) -> Ordering {
    a.bad
        .cmp(&b.bad)
        .then_with(|| match (&a.version, &b.version) {
            (Some(x), Some(y)) => version::compare(y, x),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        })
        .then_with(|| version::compare(&b.id, &a.id))
}

#[cfg(test)]
pub mod tests {
    use super::super::fat::tests::Builder;
    use super::super::gpt::{self, tests::disk};
    use super::super::pe::tests::pe;
    use super::*;

    /// A bzImage that says it's `release`, `size` bytes long.
    pub fn bzimage(release: &str, size: usize) -> Vec<u8> {
        let mut k = vec![0u8; size.max(0x400)];
        k[0x1fe] = 0x55;
        k[0x1ff] = 0xaa;
        k[0x202..0x206].copy_from_slice(b"HdrS");
        k[0x20e..0x210].copy_from_slice(&0x100u16.to_le_bytes());
        let s = format!("{release} (builder@host) #1 SMP\0");
        k[0x300..0x300 + s.len()].copy_from_slice(s.as_bytes());
        k
    }

    pub fn uki(release: &str, uname: bool, initrd: &[u8]) -> Vec<u8> {
        let k = bzimage(release, 0x600);
        let mut sections: Vec<(&str, &[u8])> = vec![(".osrel", b"ID=arch\n"), (".linux", &k)];
        if !initrd.is_empty() {
            sections.push((".initrd", initrd));
        }
        if uname {
            sections.push((".uname", release.as_bytes()));
        }
        pe(MACHINE, &sections)
    }

    /// A disk with an ESP holding what `fill` puts there.
    pub fn esp_disk(fill: impl FnOnce(&mut Builder)) -> Vec<u8> {
        let mut b = Builder::new();
        fill(&mut b);
        let mut d = disk(
            64 << 10,
            &[(gpt::Kind::Esp, 128, 128 + b.image.len() as u64 / 512 - 1)],
        );
        d.extend(b.image);
        d
    }

    /// Each plan's name, version, kernel and initrd length.
    type Summary = Vec<(String, Option<String>, u64, u64)>;

    fn names(d: &Vec<u8>) -> (Summary, Vec<String>) {
        let mut warnings = Vec::new();
        let disk = Disk::open(d, d.len() as u64, &mut warnings).unwrap();
        let plans = disk.plans(&mut warnings);
        let names = plans
            .iter()
            .map(|p| {
                (
                    p.name.clone(),
                    p.version.clone(),
                    p.kernel.len,
                    p.initrd_len(),
                )
            })
            .collect();
        (names, warnings)
    }

    #[test]
    fn newest_first() {
        let d = esp_disk(|b| {
            b.put(
                "/EFI/Linux/arch-7.2.9-arch1-1.efi",
                &uki("7.2.9-arch1-1", true, b"old"),
            );
            b.put(
                "/EFI/Linux/arch-linux.efi",
                &uki("7.2.14-arch1-1", false, b"initrd"),
            );
            b.put("/EFI/Linux/broken+0-3.efi", &uki("9.0.0", true, b""));
            b.put("/EFI/Linux/notes.txt", b"hi");
            b.put("/arch/7.2.10/linux", &bzimage("7.2.10-arch1-1", 0x500));
            b.put("/arch/7.2.10/initrd", b"12345");
            b.put("/arch/7.2.10/microcode", b"ucode");
            b.put(
                "/loader/entries/arch-7.2.10.conf",
                b"title Arch\nversion 7.2.10-arch1-1\nlinux /arch/7.2.10/linux\ninitrd /arch/7.2.10/microcode /arch/7.2.10/initrd\noptions quiet\n",
            );
            b.put(
                "/loader/entries/windows.conf",
                b"title Windows\nefi /EFI/Microsoft/boot.efi\n",
            );
            b.put(
                "/loader/entries/arm.conf",
                b"architecture aa64\nlinux /nope\n",
            );
            b.put("/loader/entries/missing.conf", b"linux /arch/9/linux\n");
        });
        let (names, warnings) = names(&d);
        assert_eq!(
            names,
            [
                (
                    "ESP:/EFI/Linux/arch-linux.efi".into(),
                    Some("7.2.14-arch1-1".into()),
                    0x600,
                    8
                ),
                (
                    "ESP:/loader/entries/arch-7.2.10.conf".into(),
                    Some("7.2.10-arch1-1".into()),
                    0x500,
                    16
                ),
                (
                    "ESP:/EFI/Linux/arch-7.2.9-arch1-1.efi".into(),
                    Some("7.2.9-arch1-1".into()),
                    0x600,
                    4
                ),
                (
                    "ESP:/EFI/Linux/broken+0-3.efi".into(),
                    Some("9.0.0".into()),
                    0x600,
                    0
                ),
            ]
        );
        assert_eq!(
            warnings,
            ["ESP:/loader/entries/missing.conf: /arch/9/linux doesn't exist"]
        );
    }

    #[test]
    fn bad_kernels_are_skipped() {
        let d = esp_disk(|b| {
            b.put("/EFI/Linux/garbage.efi", b"MZ not really");
            b.put(
                "/EFI/Linux/other-arch.efi",
                &pe(0x14c, &[(".linux", &bzimage("1", 0x400))]),
            );
            b.put(
                "/EFI/Linux/not-bzimage.efi",
                &pe(MACHINE, &[(".linux", &[7u8; 0x400])]),
            );
            b.put("/EFI/Linux/good.efi", &uki("6.1", true, b""));
        });
        let (names, warnings) = names(&d);
        assert_eq!(names.len(), 1);
        assert_eq!(names[0].0, "ESP:/EFI/Linux/good.efi");
        assert_eq!(warnings.len(), 3, "{warnings:?}");
    }

    #[test]
    fn reads_the_pieces() {
        let initrd: Vec<u8> = (0..1000u32).map(|i| (i * 7) as u8).collect();
        let d = esp_disk(|b| b.put("/EFI/Linux/a.efi", &uki("6.1", true, &initrd)));
        let mut warnings = Vec::new();
        let disk = Disk::open(&d, d.len() as u64, &mut warnings).unwrap();
        let plans = disk.plans(&mut warnings);
        let mut got = vec![0u8; 1000];
        plans[0].initrds[0].read_at(&mut got, 0).unwrap();
        assert_eq!(got, initrd);
        let mut k = vec![0u8; 0x600];
        plans[0].kernel.read_at(&mut k, 0).unwrap();
        assert_eq!(k, bzimage("6.1", 0x600));
        assert!(plans[0].kernel.read_at(&mut k, 1).is_err());
    }

    #[test]
    fn counters() {
        assert_eq!(split_counter("a+3-1"), ("a", false));
        assert_eq!(split_counter("a+0-3"), ("a", true));
        assert_eq!(split_counter("a+0"), ("a", true));
        assert_eq!(split_counter("a+b"), ("a+b", false));
        assert_eq!(split_counter("6.12+deb13"), ("6.12+deb13", false));
    }

    #[test]
    fn conf() {
        let c = parse_conf("# x\ntitle  T\nversion\t1.2\nlinux /k\ninitrd /a /b\ninitrd /c\nfoo\n");
        assert_eq!(c.version.as_deref(), Some("1.2"));
        assert_eq!(c.linux.as_deref(), Some("/k"));
        assert_eq!(c.initrds, ["/a", "/b", "/c"]);
    }
}
