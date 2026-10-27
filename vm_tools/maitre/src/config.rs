//! VM and daemon configuration.

use std::net::Ipv4Addr;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};

use crate::ini::{self, Entry};

/// Keep in sync with KernelCommandLine= in mkosi/mkosi.conf.
pub const DEFAULT_CMDLINE: &str = "root=PARTLABEL=root rw console=ttyS0 systemd.firstboot=off video=Virtual-1:d";

/// `<config dir>/vms/<name>.conf`.
#[derive(Debug, Clone, PartialEq)]
pub struct VmConfig {
    pub image: PathBuf,
    pub kernel: PathBuf,
    pub initrd: Option<PathBuf>,
    pub cmdline: String,
    pub cmdline_extra: Vec<String>,

    pub cpus: u32,
    pub memory_mib: u64,
    pub network: bool,
    pub shared_home: Option<PathBuf>,
    pub audio: bool,
    pub microphone: bool,
    pub wayland: bool,
    pub gpu_context_types: Vec<String>,
    pub extra_args: Vec<String>,

    pub user: String,
    pub uid: u32,
    pub gid: u32,
    pub app_menu: bool,
}

/// VM names: also used in file names and socket paths, so short and plain.
pub fn is_valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        && name.len() <= 32
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

fn bool_value(e: &Entry) -> Result<bool, String> {
    ini::parse_bool(&e.value).ok_or_else(|| e.error("expected a boolean"))
}

fn number<T: std::str::FromStr>(e: &Entry) -> Result<T, String> {
    e.value.parse().map_err(|_| e.error("expected a number"))
}

fn words(e: &Entry) -> Result<Vec<String>, String> {
    ini::split_words(&e.value).map_err(|msg| e.error(msg))
}

fn unknown(e: &Entry) -> String {
    format!("{}:{}: unknown setting [{}] {}=", e.file.display(), e.line, e.section, e.key)
}

impl VmConfig {
    /// `user` is the default for [Guest] User=, `home` is what `~` expands to.
    pub fn from_entries(entries: &[Entry], user: &str, home: &Path) -> Result<VmConfig, String> {
        let path = |e: &Entry| ini::resolve_path(&e.value, e.dir(), home);
        let mut image = None;
        let mut kernel = None;
        let mut initrd = None;
        let mut cmdline = None;
        let mut cmdline_extra = Vec::new();
        let mut cpus = 4;
        let mut memory = 4u64 << 30;
        let mut network = true;
        let mut shared_home = None;
        let mut audio = false;
        let mut microphone = false;
        let mut wayland = true;
        let mut gpu_context_types = vec!["cross-domain".to_owned()];
        let mut extra_args = Vec::new();
        let mut guest_user = None;
        let mut uid = 1000;
        let mut gid = None;
        let mut app_menu = true;

        for e in entries {
            // An empty assignment resets the setting to its default.
            let empty = e.value.is_empty();
            match (e.section.as_str(), e.key.as_str()) {
                ("Boot", "Image") => image = (!empty).then(|| path(e)),
                ("Boot", "Kernel") => kernel = (!empty).then(|| path(e)),
                ("Boot", "Initrd") => initrd = (!empty).then(|| path(e)),
                ("Boot", "KernelCommandLine") => cmdline = (!empty).then(|| e.value.clone()),
                ("Boot", "KernelCommandLineExtra") if empty => cmdline_extra.clear(),
                ("Boot", "KernelCommandLineExtra") => cmdline_extra.extend(words(e)?),

                ("Machine", "CPUs") => {
                    cpus = if empty { 4 } else { number(e)? };
                    if cpus == 0 {
                        return Err(e.error("must be at least 1"));
                    }
                }
                ("Machine", "Memory") => {
                    memory = if empty {
                        4 << 30
                    } else {
                        ini::parse_size(&e.value).ok_or_else(|| e.error("expected a size, e.g. 4G or 512M"))?
                    };
                    if memory < 128 << 20 {
                        return Err(e.error("must be at least 128M (a plain number is in bytes)"));
                    }
                }
                ("Machine", "Network") => network = empty || bool_value(e)?,
                ("Machine", "SharedHome") => shared_home = (!empty).then(|| path(e)),
                ("Machine", "Audio") => audio = !empty && bool_value(e)?,
                ("Machine", "Microphone") => microphone = !empty && bool_value(e)?,
                ("Machine", "Wayland") => wayland = empty || bool_value(e)?,
                ("Machine", "GpuContextTypes") => {
                    gpu_context_types = if empty { vec!["cross-domain".to_owned()] } else { words(e)? };
                }
                ("Machine", "ExtraArguments") if empty => extra_args.clear(),
                ("Machine", "ExtraArguments") => extra_args.extend(words(e)?),

                ("Guest", "User") => guest_user = (!empty).then(|| e.value.clone()),
                ("Guest", "UID") => uid = if empty { 1000 } else { number(e)? },
                ("Guest", "GID") => gid = if empty { None } else { Some(number(e)?) },
                ("Guest", "AppMenu") => app_menu = empty || bool_value(e)?,
                _ => return Err(unknown(e)),
            }
        }

        let image = image.ok_or("[Boot] Image= is not set")?;
        // mkosi's SplitArtifacts= puts <name>.vmlinuz and <name>.initrd
        // next to <name>.raw.
        let kernel = kernel.unwrap_or_else(|| image.with_extension("vmlinuz"));
        // Explicitly set, it has to exist; the default is optional.
        let initrd = initrd.or_else(|| Some(image.with_extension("initrd")).filter(|p| p.is_file()));
        let guest_user = guest_user.unwrap_or_else(|| user.to_owned());
        if guest_user.is_empty() || !guest_user.chars().all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c)) {
            return Err(format!("[Guest] User={guest_user}: not a valid user name"));
        }
        Ok(VmConfig {
            image,
            kernel,
            initrd,
            cmdline: cmdline.unwrap_or_else(|| DEFAULT_CMDLINE.to_owned()),
            cmdline_extra,
            cpus,
            memory_mib: memory >> 20,
            network,
            shared_home,
            audio,
            microphone,
            wayland,
            gpu_context_types,
            extra_args,
            user: guest_user,
            uid,
            gid: gid.unwrap_or(uid),
            app_menu,
        })
    }

    /// Checks that the files the VM needs are there, so that a typo is
    /// reported as such and not as a crosvm error.
    pub fn check_files(&self) -> Result<(), String> {
        let mut files = vec![("Image", &self.image), ("Kernel", &self.kernel)];
        if let Some(initrd) = &self.initrd {
            files.push(("Initrd", initrd));
        }
        for (key, file) in files {
            if !file.is_file() {
                return Err(format!("[Boot] {key}={}: no such file", file.display()));
            }
        }
        if let Some(dir) = &self.shared_home
            && !dir.is_dir()
        {
            return Err(format!("[Machine] SharedHome={}: not a directory", dir.display()));
        }
        Ok(())
    }
}

/// `<config dir>/maitred.conf`: the resources shared by all VMs.
#[derive(Debug, Clone, PartialEq)]
pub struct DaemonConfig {
    /// vsock context IDs to hand out. 0-2 are reserved.
    pub cids: RangeInclusive<u32>,
    /// Split into /30 networks, one per tap: the n-th gets tap
    /// `<tap_prefix><n>`, the host its first address and the guest its second.
    pub subnet: Ipv4Addr,
    pub prefix_len: u8,
    pub tap_prefix: String,
    pub dns: Vec<Ipv4Addr>,
    /// How long a guest gets to shut down before crosvm is stopped.
    pub stop_timeout_secs: u64,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        DaemonConfig {
            cids: 3..=99,
            subnet: Ipv4Addr::new(192, 168, 10, 0),
            prefix_len: 24,
            tap_prefix: "crosvm_tap".to_owned(),
            dns: vec![Ipv4Addr::new(1, 1, 1, 1), Ipv4Addr::new(9, 9, 9, 9)],
            stop_timeout_secs: 30,
        }
    }
}

impl DaemonConfig {
    pub fn from_entries(entries: &[Entry]) -> Result<DaemonConfig, String> {
        let mut config = DaemonConfig::default();
        let default = DaemonConfig::default();
        // The first DNS= replaces the default servers, later ones add to it.
        let mut dns: Option<Vec<Ipv4Addr>> = None;
        for e in entries {
            let empty = e.value.is_empty();
            match (e.section.as_str(), e.key.as_str()) {
                ("Pool", "CIDs") if empty => config.cids = default.cids.clone(),
                ("Pool", "CIDs") => {
                    let (lo, hi) = e.value.split_once('-').unwrap_or((&e.value, &e.value));
                    let (lo, hi): (u32, u32) = match (lo.trim().parse(), hi.trim().parse()) {
                        (Ok(lo), Ok(hi)) if 3 <= lo && lo <= hi && hi < u32::MAX => (lo, hi),
                        _ => return Err(e.error("expected a range like 3-99 (CIDs below 3 are reserved)")),
                    };
                    config.cids = lo..=hi;
                }
                ("Pool", "Subnet") if empty => (config.subnet, config.prefix_len) = (default.subnet, default.prefix_len),
                ("Pool", "Subnet") => {
                    let parsed = e.value.split_once('/').and_then(|(addr, len)| {
                        Some((addr.parse::<Ipv4Addr>().ok()?, len.parse::<u8>().ok().filter(|l| *l <= 30)?))
                    });
                    let Some((addr, len)) = parsed else {
                        return Err(e.error("expected an IPv4 network like 192.168.10.0/24, at most /30"));
                    };
                    let mask = u32::MAX.checked_shl(32 - u32::from(len)).unwrap_or(0);
                    if u32::from(addr) & !mask != 0 {
                        return Err(e.error("host bits are set"));
                    }
                    (config.subnet, config.prefix_len) = (addr, len);
                }
                ("Pool", "TapPrefix") => {
                    config.tap_prefix = if empty { default.tap_prefix.clone() } else { e.value.clone() };
                    // Interface names are at most 15 bytes; leave room for the number.
                    if config.tap_prefix.len() > 12 || config.tap_prefix.contains(['/', ' ']) {
                        return Err(e.error("must be at most 12 characters, without '/' or spaces"));
                    }
                }
                ("Pool", "DNS") if empty => dns = Some(Vec::new()),
                ("Pool", "DNS") => {
                    let dns = dns.get_or_insert_with(Vec::new);
                    for word in words(e)? {
                        dns.push(word.parse().map_err(|_| e.error(format!("{word}: not an IPv4 address")))?);
                    }
                    if dns.len() > 2 {
                        return Err(e.error("at most two servers (the ip= kernel parameter has room for two)"));
                    }
                }
                ("Manager", "StopTimeoutSec") => {
                    config.stop_timeout_secs = if empty { default.stop_timeout_secs } else { number(e)? };
                }
                _ => return Err(unknown(e)),
            }
        }
        if let Some(dns) = dns {
            config.dns = dns;
        }
        Ok(config)
    }

    /// Number of /30 networks in the subnet.
    pub fn net_slots(&self) -> u32 {
        1 << (30 - self.prefix_len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vm(text: &str) -> Result<VmConfig, String> {
        let entries = ini::parse(text, Path::new("/cfg/vms/test.conf"))?;
        VmConfig::from_entries(&entries, "me", Path::new("/home/me"))
    }

    #[test]
    fn defaults() {
        let c = vm("[Boot]\nImage=../images/debian.raw\n").unwrap();
        assert_eq!(c.image, Path::new("/cfg/vms/../images/debian.raw"));
        assert_eq!(c.kernel, Path::new("/cfg/vms/../images/debian.vmlinuz"));
        assert_eq!(c.initrd, None);
        assert_eq!(c.cmdline, DEFAULT_CMDLINE);
        assert_eq!((c.cpus, c.memory_mib, c.network, c.audio, c.wayland), (4, 4096, true, false, true));
        assert_eq!((c.user.as_str(), c.uid, c.gid), ("me", 1000, 1000));
        assert_eq!(c.gpu_context_types, ["cross-domain"]);
    }

    #[test]
    fn settings() {
        let c = vm("[Boot]\nImage=~/d.raw\nKernel=/k\nKernelCommandLineExtra=a b\nKernelCommandLineExtra=\n\
                    KernelCommandLineExtra=c \"d e\"\n[Machine]\nCPUs=2\nMemory=8G\nNetwork=no\nAudio=yes\n\
                    SharedHome=~/vm/home\nGpuContextTypes=cross-domain virgl2\nExtraArguments=--x\n\
                    [Guest]\nUser=jonnathan\nUID=1001\nAppMenu=off\n")
        .unwrap();
        assert_eq!(c.image, Path::new("/home/me/d.raw"));
        assert_eq!(c.kernel, Path::new("/k"));
        assert_eq!(c.cmdline_extra, ["c", "d e"]);
        assert_eq!((c.cpus, c.memory_mib, c.network, c.audio), (2, 8192, false, true));
        assert_eq!(c.shared_home.as_deref(), Some(Path::new("/home/me/vm/home")));
        assert_eq!(c.gpu_context_types, ["cross-domain", "virgl2"]);
        assert_eq!(c.extra_args, ["--x"]);
        assert_eq!((c.user.as_str(), c.uid, c.gid, c.app_menu), ("jonnathan", 1001, 1001, false));
    }

    #[test]
    fn errors() {
        assert!(vm("[Machine]\nCPUs=2\n").unwrap_err().contains("Image="));
        assert!(vm("[Boot]\nImage=a\nImgae=b\n").unwrap_err().contains("unknown setting [Boot] Imgae="));
        assert!(vm("[Boot]\nImage=a\n[Machine]\nMemory=4096\n").unwrap_err().contains("128M"));
        assert!(vm("[Boot]\nImage=a\n[Machine]\nAudio=maybe\n").unwrap_err().contains("test.conf:4"));
    }

    #[test]
    fn examples() {
        let c = vm(include_str!("../examples/debian.conf")).unwrap();
        assert_eq!(c.image, Path::new("/home/me/src/mkosi_experiments/mkosi/mkosi.output/debian.raw"));
        let d = daemon(include_str!("../examples/maitred.conf")).unwrap();
        assert_eq!(d, DaemonConfig::default());
    }

    #[test]
    fn names() {
        assert!(is_valid_name("debian"));
        assert!(is_valid_name("fedora-44.work_1"));
        assert!(!is_valid_name(""));
        assert!(!is_valid_name(".hidden"));
        assert!(!is_valid_name("a/b"));
        assert!(!is_valid_name(&"x".repeat(33)));
    }

    fn daemon(text: &str) -> Result<DaemonConfig, String> {
        DaemonConfig::from_entries(&ini::parse(text, Path::new("maitred.conf"))?)
    }

    #[test]
    fn daemon_config() {
        let d = daemon("").unwrap();
        assert_eq!(d, DaemonConfig::default());
        assert_eq!(d.net_slots(), 64);
        let d = daemon("[Pool]\nCIDs=10-20\nSubnet=10.0.0.0/28\nDNS=10.0.0.1\n").unwrap();
        assert_eq!(d.cids, 10..=20);
        assert_eq!(d.net_slots(), 4);
        assert_eq!(d.dns, [Ipv4Addr::new(10, 0, 0, 1)]);
        assert!(daemon("[Pool]\nDNS=\n").unwrap().dns.is_empty());
        assert!(daemon("[Pool]\nCIDs=2-5\n").is_err());
        assert!(daemon("[Pool]\nSubnet=10.0.0.1/24\n").unwrap_err().contains("host bits"));
        assert!(daemon("[Pool]\nSubnet=10.0.0.0/31\n").is_err());
    }
}
