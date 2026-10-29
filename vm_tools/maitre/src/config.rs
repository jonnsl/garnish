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

    /// A name for people, e.g. "Debian (work)"; UIs show it instead of the
    /// VM's name.
    pub title: Option<String>,
    pub cpus: u32,
    pub memory_mib: u64,
    pub network: NetworkMode,
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

/// `[Network] Mode=`: how the guest is connected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkMode {
    /// No network device.
    None,
    /// A tap of its own, with a /30 and NAT to the outside, from maitre-net.
    Nat,
}

impl NetworkMode {
    pub fn parse(s: &str) -> Option<NetworkMode> {
        match s {
            "none" => Some(NetworkMode::None),
            "nat" => Some(NetworkMode::Nat),
            _ => None,
        }
    }
}

/// VM names: also the guest's hostname, and used in file names and socket
/// paths, so short and plain: a hostname label (letters, digits and '-', not
/// at either end).
pub fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && !name.starts_with('-')
        && !name.ends_with('-')
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
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
        let mut title = None;
        let mut cpus = 4;
        let mut memory = 4u64 << 30;
        let mut network = NetworkMode::Nat;
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

                ("Machine", "Title") => title = (!empty).then(|| e.value.clone()),
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
                ("Machine", "Network") => {
                    return Err(e.error("replaced by [Network] Mode= (nat or none)"));
                }
                ("Machine", "SharedHome") => shared_home = (!empty).then(|| path(e)),
                ("Machine", "Audio") => audio = !empty && bool_value(e)?,
                ("Machine", "Microphone") => microphone = !empty && bool_value(e)?,
                ("Machine", "Wayland") => wayland = empty || bool_value(e)?,
                ("Machine", "GpuContextTypes") => {
                    gpu_context_types = if empty { vec!["cross-domain".to_owned()] } else { words(e)? };
                }
                ("Machine", "ExtraArguments") if empty => extra_args.clear(),
                ("Machine", "ExtraArguments") => extra_args.extend(words(e)?),

                ("Network", "Mode") => {
                    network = if empty {
                        NetworkMode::Nat
                    } else {
                        NetworkMode::parse(&e.value).ok_or_else(|| e.error("expected nat or none"))?
                    };
                }

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
            title,
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
    /// Given to guests with a network.
    pub dns: Vec<Ipv4Addr>,
    /// How long a guest gets to shut down before crosvm is stopped.
    pub stop_timeout_secs: u64,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        DaemonConfig {
            cids: 3..=99,
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
                ("Pool", "Subnet" | "TapPrefix") => {
                    return Err(e.error(format!("the guest networks are set in {} now", crate::net::CONFIG_PATH)));
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
        assert_eq!(c.title, None);
        assert_eq!((c.cpus, c.memory_mib, c.network, c.audio, c.wayland), (4, 4096, NetworkMode::Nat, false, true));
        assert_eq!((c.user.as_str(), c.uid, c.gid), ("me", 1000, 1000));
        assert_eq!(c.gpu_context_types, ["cross-domain"]);
    }

    #[test]
    fn settings() {
        let c = vm("[Boot]\nImage=~/d.raw\nKernel=/k\nKernelCommandLineExtra=a b\nKernelCommandLineExtra=\n\
                    KernelCommandLineExtra=c \"d e\"\n[Machine]\nTitle=Débian (work)\nCPUs=2\nMemory=8G\nAudio=yes\n\
                    SharedHome=~/vm/home\nGpuContextTypes=cross-domain virgl2\nExtraArguments=--x\n\
                    [Network]\nMode=none\n[Guest]\nUser=alice\nUID=1001\nAppMenu=off\n")
        .unwrap();
        assert_eq!(c.image, Path::new("/home/me/d.raw"));
        assert_eq!(c.kernel, Path::new("/k"));
        assert_eq!(c.cmdline_extra, ["c", "d e"]);
        assert_eq!(c.title.as_deref(), Some("Débian (work)"));
        assert_eq!((c.cpus, c.memory_mib, c.network, c.audio), (2, 8192, NetworkMode::None, true));
        assert_eq!(c.shared_home.as_deref(), Some(Path::new("/home/me/vm/home")));
        assert_eq!(c.gpu_context_types, ["cross-domain", "virgl2"]);
        assert_eq!(c.extra_args, ["--x"]);
        assert_eq!((c.user.as_str(), c.uid, c.gid, c.app_menu), ("alice", 1001, 1001, false));
    }

    #[test]
    fn errors() {
        assert!(vm("[Machine]\nCPUs=2\n").unwrap_err().contains("Image="));
        assert!(vm("[Boot]\nImage=a\nImgae=b\n").unwrap_err().contains("unknown setting [Boot] Imgae="));
        assert!(vm("[Boot]\nImage=a\n[Machine]\nMemory=4096\n").unwrap_err().contains("128M"));
        assert!(vm("[Boot]\nImage=a\n[Machine]\nAudio=maybe\n").unwrap_err().contains("test.conf:4"));
        assert!(vm("[Boot]\nImage=a\n[Network]\nMode=bridge\n").unwrap_err().contains("nat or none"));
        assert!(vm("[Boot]\nImage=a\n[Machine]\nNetwork=yes\n").unwrap_err().contains("[Network] Mode="));
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
        assert!(is_valid_name("fedora-44-work"));
        assert!(!is_valid_name(""));
        assert!(!is_valid_name(".hidden"));
        assert!(!is_valid_name("work_1"));
        assert!(!is_valid_name("fedora.work"));
        assert!(!is_valid_name("-x") && !is_valid_name("x-"));
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
        let d = daemon("[Pool]\nCIDs=10-20\nDNS=10.0.0.1\n").unwrap();
        assert_eq!(d.cids, 10..=20);
        assert_eq!(d.dns, [Ipv4Addr::new(10, 0, 0, 1)]);
        assert!(daemon("[Pool]\nDNS=\n").unwrap().dns.is_empty());
        assert!(daemon("[Pool]\nCIDs=2-5\n").is_err());
        assert!(daemon("[Pool]\nSubnet=10.0.0.0/24\n").unwrap_err().contains("/etc/garnish/network.conf"));
    }
}
