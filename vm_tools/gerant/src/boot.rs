//! Booting the kernel that's in the VM's image, so that guests can update
//! it like on real hardware.
//!
//! Unless `[Boot] Kernel=` is set, gerantd runs gerant-boot on the image
//! before every boot (also when the guest reboots). gerant-boot reads the
//! image's EFI system partition (and XBOOTLDR partition, if there is one),
//! picks the newest kernel there, a unified kernel image or a type 1 boot
//! loader entry, and sends the kernel and initrd to standard output.
//! gerantd puts them in the VM's runtime directory, for crosvm. What the
//! image says about the kernel command line is ignored: gerantd's own goes
//! on it as before.
//!
//! The image comes from the guest, so the parsing happens in gerant-boot,
//! which can do nothing but read the image it's given on standard input
//! and write to standard output and standard error (see
//! src/bin/gerant-boot.rs). gerantd checks the sizes it sends and that
//! the kernel is one.
//!
//! An image with no kernel in it boots the files mkosi puts next to it,
//! `<image>.vmlinuz` and `<image>.initrd`, as before.

pub mod entries;
pub mod fat;
pub mod gpt;
pub mod pe;
pub mod version;

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::time::timeout;

use crate::config::VmConfig;

pub const HELPER: &str = "gerant-boot";
/// gerant-boot's exit status when the image has no kernel.
pub const EXIT_NONE: i32 = 2;
/// What gerant-boot's output starts with.
pub const MAGIC: [u8; 8] = *b"GBOOT\0\0\x01";

/// The biggest kernel and initrd (all of them together) we boot.
pub const MAX_KERNEL: u64 = 64 << 20;
pub const MAX_INITRD: u64 = 512 << 20;
/// The longest name and version gerant-boot may send.
const MAX_NAME: u32 = 4096;
/// How long gerant-boot may take.
const TIMEOUT: Duration = Duration::from_secs(60);
/// How much of gerant-boot's standard error we read.
const MAX_STDERR: u64 = 64 << 10;

/// The files in the VM's runtime directory.
const KERNEL: &str = "vmlinuz";
const INITRD: &str = "initrd";

/// Reads a disk.
pub trait ReadAt {
    /// Fills `buf` from `offset`, or fails.
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> Result<(), String>;
}

impl ReadAt for Vec<u8> {
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> Result<(), String> {
        let data = usize::try_from(offset)
            .ok()
            .and_then(|start| self.get(start..start.checked_add(buf.len())?))
            .ok_or_else(|| format!("read past the end of the disk at {offset}"))?;
        buf.copy_from_slice(data);
        Ok(())
    }
}

impl ReadAt for File {
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> Result<(), String> {
        FileExt::read_exact_at(self, buf, offset)
            .map_err(|e| format!("reading the disk at {offset}: {e}"))
    }
}

pub(crate) fn le_u16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(b[at..at + 2].try_into().unwrap())
}

pub(crate) fn le_u32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

pub(crate) fn le_u64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

/// Sends `plan` the way gerantd reads it: [`MAGIC`], the name and the
/// version (a u32 length each, then the bytes), the length of the kernel
/// and of the initrd (u64), the kernel and the initrds, each padded to 4
/// bytes. Numbers are little-endian.
pub fn send(plan: &entries::Plan, out: &mut dyn Write) -> Result<(), String> {
    let failed = |e: io::Error| format!("writing: {e}");
    let mut header = MAGIC.to_vec();
    for s in [plan.name.as_str(), plan.version.as_deref().unwrap_or("")] {
        let s = &s.as_bytes()[..s.len().min(MAX_NAME as usize)];
        header.extend((s.len() as u32).to_le_bytes());
        header.extend(s);
    }
    header.extend(plan.kernel.len.to_le_bytes());
    header.extend(plan.initrd_len().to_le_bytes());
    out.write_all(&header).map_err(failed)?;
    let mut buf = vec![0u8; 1 << 20];
    let pieces =
        std::iter::once((&plan.kernel, false)).chain(plan.initrds.iter().map(|p| (p, true)));
    for (piece, pad) in pieces {
        let mut done = 0;
        while done < piece.len {
            let n = (piece.len - done).min(buf.len() as u64) as usize;
            piece.read_at(&mut buf[..n], done)?;
            out.write_all(&buf[..n]).map_err(failed)?;
            done += n as u64;
        }
        if pad {
            let padding = piece.len.next_multiple_of(4) - piece.len;
            out.write_all(&[0u8; 4][..padding as usize])
                .map_err(failed)?;
        }
    }
    out.flush().map_err(failed)
}

/// What gerant-boot found.
#[derive(Debug, PartialEq)]
struct Found {
    name: String,
    version: Option<String>,
    initrd: bool,
}

/// Fills `buf`, or as much of it as there is before the end. Returns how
/// much.
async fn read_full(r: &mut (impl AsyncRead + Unpin), buf: &mut [u8]) -> io::Result<usize> {
    let mut done = 0;
    while done < buf.len() {
        match r.read(&mut buf[done..]).await? {
            0 => break,
            n => done += n,
        }
    }
    Ok(done)
}

async fn read_exact(r: &mut (impl AsyncRead + Unpin), buf: &mut [u8]) -> Result<(), String> {
    match read_full(r, buf).await {
        Ok(n) if n == buf.len() => Ok(()),
        Ok(_) => Err("its output ends too soon".into()),
        Err(e) => Err(format!("reading its output: {e}")),
    }
}

async fn read_string(r: &mut (impl AsyncRead + Unpin)) -> Result<String, String> {
    let mut len = [0u8; 4];
    read_exact(r, &mut len).await?;
    let len = u32::from_le_bytes(len);
    if len > MAX_NAME {
        return Err("it sent a name that's too long".into());
    }
    let mut s = vec![0u8; len as usize];
    read_exact(r, &mut s).await?;
    Ok(String::from_utf8_lossy(&s).into_owned())
}

/// Copies `len` bytes from `r` to `path` (through a temporary file next to
/// it), checking the start with `check`.
async fn receive_file(
    r: &mut (impl AsyncRead + Unpin),
    len: u64,
    path: &Path,
    check: impl FnOnce(&[u8]) -> Result<(), String>,
) -> Result<(), String> {
    let part = path.with_extension("part");
    let failed = |e: io::Error| format!("{}: {e}", part.display());
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&part)
        .map_err(failed)?;
    let mut buf = vec![0u8; 1 << 20];
    let mut done = 0;
    let mut check = Some(check);
    while done < len {
        let want = (len - done).min(buf.len() as u64) as usize;
        read_exact(r, &mut buf[..want]).await?;
        if let Some(check) = check.take() {
            check(&buf[..want])?;
        }
        // To tmpfs, a megabyte at a time: quick enough not to bother the
        // other VMs.
        file.write_all(&buf[..want]).map_err(failed)?;
        done += want as u64;
    }
    drop(file);
    fs::rename(&part, path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Checks that `start` is the start of a kernel crosvm can boot.
fn check_kernel(start: &[u8]) -> Result<(), String> {
    // The bzImage setup header's boot flag and magic.
    if start.len() >= 0x206
        && start[0x1fe..0x200] == [0x55, 0xaa]
        && &start[0x202..0x206] == b"HdrS"
    {
        Ok(())
    } else {
        Err("it sent something that isn't a kernel".into())
    }
}

/// Reads what gerant-boot sends into `dir`; None if it sent nothing.
async fn receive(r: &mut (impl AsyncRead + Unpin), dir: &Path) -> Result<Option<Found>, String> {
    let mut magic = [0u8; 8];
    match read_full(r, &mut magic).await {
        Ok(0) => return Ok(None),
        Ok(8) if magic == MAGIC => {}
        Ok(_) => return Err("unexpected output".into()),
        Err(e) => return Err(format!("reading its output: {e}")),
    }
    let name = read_string(r).await?;
    let version = Some(read_string(r).await?).filter(|v| !v.is_empty());
    let mut lens = [0u8; 16];
    read_exact(r, &mut lens).await?;
    let (kernel_len, initrd_len) = (le_u64(&lens, 0), le_u64(&lens, 8));
    if kernel_len > MAX_KERNEL || initrd_len > MAX_INITRD {
        return Err(format!(
            "{name} is too big (kernel {} MiB, initrd {} MiB)",
            kernel_len >> 20,
            initrd_len >> 20
        ));
    }
    if kernel_len < 0x400 {
        return Err(format!("{name}: the kernel is too small"));
    }
    receive_file(r, kernel_len, &dir.join(KERNEL), check_kernel).await?;
    if initrd_len > 0 {
        receive_file(r, initrd_len, &dir.join(INITRD), |_| Ok(())).await?;
    } else {
        let _ = fs::remove_file(dir.join(INITRD));
    }
    let mut more = [0u8; 1];
    if read_full(r, &mut more)
        .await
        .map_err(|e| format!("reading its output: {e}"))?
        != 0
    {
        return Err("it sent more than it said".into());
    }
    Ok(Some(Found {
        name,
        version,
        initrd: initrd_len > 0,
    }))
}

/// gerant-boot next to gerantd, or else in $PATH.
fn find_helper() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok();
    let beside = exe
        .as_deref()
        .and_then(Path::parent)
        .map(|dir| dir.join(HELPER));
    let path = std::env::var_os("PATH").unwrap_or_default();
    beside
        .into_iter()
        .chain(std::env::split_paths(&path).map(|dir| dir.join(HELPER)))
        .find(|p| p.is_file())
}

fn describe(status: ExitStatus) -> String {
    match (status.code(), status.signal()) {
        (Some(code), _) => format!("{HELPER} exited with status {code}"),
        (None, Some(libc::SIGSYS)) => format!("{HELPER} was killed by its seccomp filter"),
        (None, Some(sig)) => format!("{HELPER} was killed by signal {sig}"),
        (None, None) => format!("{HELPER} failed"),
    }
}

/// Runs gerant-boot on `image`, with the kernel and initrd it finds going
/// to `dir`. Its warnings go to `notes`.
async fn extract(
    image: &Path,
    dir: &Path,
    notes: &mut Vec<String>,
) -> Result<Option<Found>, String> {
    let helper = find_helper().ok_or_else(|| format!("{HELPER} isn't installed"))?;
    let disk = File::open(image).map_err(|e| format!("{}: {e}", image.display()))?;
    let mut child = tokio::process::Command::new(&helper)
        .env_clear()
        .stdin(Stdio::from(disk))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("{}: {e}", helper.display()))?;
    let mut stdout = child.stdout.take().expect("piped");
    let stderr = child.stderr.take().expect("piped");

    let run = async {
        let errors = async {
            let mut buf = Vec::new();
            let _ = stderr.take(MAX_STDERR).read_to_end(&mut buf).await;
            buf
        };
        let (found, errors) = tokio::join!(receive(&mut stdout, dir), errors);
        if found.is_err() {
            // It may be stuck writing.
            let _ = child.start_kill();
        }
        (found, errors, child.wait().await)
    };
    let Ok((found, errors, status)) = timeout(TIMEOUT, run).await else {
        let _ = child.start_kill();
        return Err(format!("{HELPER} took more than {}s", TIMEOUT.as_secs()));
    };
    let lines: Vec<String> = String::from_utf8_lossy(&errors)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect();
    let status = status.map_err(|e| format!("waiting for {HELPER}: {e}"))?;
    match (found, status.code()) {
        (Ok(Some(found)), Some(0)) => {
            notes.extend(lines);
            Ok(Some(found))
        }
        (Ok(None), Some(EXIT_NONE)) => {
            notes.extend(lines);
            Ok(None)
        }
        // Its last words say why; the ones before are warnings.
        (Ok(_), _) => {
            let last = lines.last().cloned();
            notes.extend(lines.into_iter().rev().skip(1).rev());
            Err(last.unwrap_or_else(|| describe(status)))
        }
        (Err(e), _) => {
            notes.extend(lines);
            Err(format!("{HELPER}: {e}"))
        }
    }
}

/// What a VM boots.
#[derive(Debug, Clone, PartialEq)]
pub struct Boot {
    pub kernel: PathBuf,
    pub initrd: Option<PathBuf>,
    /// Where the kernel comes from, for people.
    pub source: String,
    /// Warnings along the way.
    pub notes: Vec<String>,
}

/// Gets the kernel and initrd ready for a VM whose runtime directory is
/// `dir`: `[Boot] Kernel=` if set, else the newest kernel in the image's
/// boot partitions, else the kernel next to the image. `[Boot] Initrd=`
/// replaces the initrd if set.
pub async fn prepare(config: &VmConfig, dir: &Path) -> Result<Boot, String> {
    let beside_initrd = || Some(config.image.with_extension("initrd")).filter(|p| p.is_file());
    if let Some(kernel) = &config.kernel {
        return Ok(Boot {
            kernel: kernel.clone(),
            initrd: config.initrd.clone().or_else(beside_initrd),
            source: kernel.display().to_string(),
            notes: Vec::new(),
        });
    }
    let mut notes = Vec::new();
    let failure = match extract(&config.image, dir, &mut notes).await {
        Ok(Some(found)) => {
            let what = match &found.version {
                Some(v) => format!("Linux {v} ({})", found.name),
                None => found.name,
            };
            return Ok(Boot {
                kernel: dir.join(KERNEL),
                initrd: config
                    .initrd
                    .clone()
                    .or_else(|| found.initrd.then(|| dir.join(INITRD))),
                source: format!("{what} from the image"),
                notes,
            });
        }
        Ok(None) => None,
        Err(e) => Some(e),
    };
    let kernel = config.image.with_extension("vmlinuz");
    if !kernel.is_file() {
        return Err(match failure {
            Some(e) => format!(
                "can't boot the kernel in the image ({e}), and there's no {} \
                 next to it; set [Boot] Kernel=",
                kernel.display()
            ),
            None => format!(
                "there's no kernel in the image's boot partitions, nor a {} \
                 next to it; set [Boot] Kernel=",
                kernel.display()
            ),
        });
    }
    notes.push(match failure {
        Some(e) => format!("can't boot the kernel in the image: {e}"),
        None => "there's no kernel in the image's boot partitions".to_owned(),
    });
    Ok(Boot {
        initrd: config.initrd.clone().or_else(beside_initrd),
        source: kernel.display().to_string(),
        kernel,
        notes,
    })
}

#[cfg(test)]
mod tests {
    use super::entries::Disk;
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("gerant-boot-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// The disk entries::tests builds, with one unified kernel image.
    fn plan_bytes(initrd: &[u8]) -> Vec<u8> {
        let disk = entries::tests::esp_disk(|b| {
            b.put(
                "/EFI/Linux/a.efi",
                &entries::tests::uki("6.1.2", true, initrd),
            )
        });
        let mut warnings = Vec::new();
        let d = Disk::open(&disk, disk.len() as u64, &mut warnings).unwrap();
        let plans = d.plans(&mut warnings);
        let mut out = Vec::new();
        send(&plans[0], &mut out).unwrap();
        out
    }

    #[tokio::test]
    async fn round_trip() {
        let d = dir("round-trip");
        let bytes = plan_bytes(b"initrd!");
        let found = receive(&mut &bytes[..], &d).await.unwrap().unwrap();
        assert_eq!(
            found,
            Found {
                name: "ESP:/EFI/Linux/a.efi".into(),
                version: Some("6.1.2".into()),
                initrd: true,
            }
        );
        assert_eq!(
            fs::read(d.join(KERNEL)).unwrap(),
            entries::tests::bzimage("6.1.2", 0x600)
        );
        assert_eq!(fs::read(d.join(INITRD)).unwrap(), b"initrd!\0");
        assert!(!d.join("vmlinuz.part").exists());

        // A boot without an initrd removes the old one.
        let bytes = plan_bytes(b"");
        let found = receive(&mut &bytes[..], &d).await.unwrap().unwrap();
        assert!(!found.initrd);
        assert!(!d.join(INITRD).exists());
        fs::remove_dir_all(&d).unwrap();
    }

    #[tokio::test]
    async fn bad_output() {
        let d = dir("bad-output");
        assert_eq!(receive(&mut &b""[..], &d).await, Ok(None));
        assert!(receive(&mut &b"junk"[..], &d).await.is_err());
        let good = plan_bytes(b"initrd!");
        // Cut short, or too long.
        assert!(receive(&mut &good[..good.len() - 1], &d).await.is_err());
        let mut long = good.clone();
        long.push(0);
        assert!(receive(&mut &long[..], &d).await.is_err());
        // Not a kernel.
        let mut not_kernel = good.clone();
        let header = 8 + 4 + "ESP:/EFI/Linux/a.efi".len() + 4 + "6.1.2".len() + 16;
        not_kernel[header + 0x202] = b'X';
        assert!(receive(&mut &not_kernel[..], &d).await.is_err());
        // Bigger than allowed.
        let mut huge = good.clone();
        huge[header - 8..header].copy_from_slice(&(MAX_INITRD + 1).to_le_bytes());
        assert!(receive(&mut &huge[..], &d).await.is_err());
        fs::remove_dir_all(&d).unwrap();
    }
}
