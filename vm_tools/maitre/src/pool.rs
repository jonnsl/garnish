//! The resources VMs share: vsock CIDs and tap devices with their addresses.

use std::collections::BTreeSet;
use std::fs;
use std::net::Ipv4Addr;

use crate::config::DaemonConfig;

/// One tap device and the /30 network on it.
#[derive(Debug, Clone, PartialEq)]
pub struct NetSlot {
    pub index: u32,
    pub tap: String,
    pub host: Ipv4Addr,
    pub guest: Ipv4Addr,
}

impl NetSlot {
    pub fn netmask(&self) -> Ipv4Addr {
        Ipv4Addr::new(255, 255, 255, 252)
    }
}

/// Whether a tap can be used by this user: it has to exist (only root can
/// create it) and belong to us.
pub trait TapCheck {
    fn usable(&self, tap: &str) -> Result<(), String>;
}

/// Looks at /sys/class/net.
pub struct SysfsTaps;

impl TapCheck for SysfsTaps {
    fn usable(&self, tap: &str) -> Result<(), String> {
        let dir = format!("/sys/class/net/{tap}");
        if fs::metadata(&dir).is_err() {
            return Err(format!("{tap} doesn't exist"));
        }
        // -1 means no owner: then only root can attach to it.
        let owner = fs::read_to_string(format!("{dir}/owner")).map_err(|_| format!("{tap} is not a tap device"))?;
        let uid = unsafe { libc::getuid() };
        if owner.trim().parse::<i64>() != Ok(i64::from(uid)) {
            return Err(format!("{tap} belongs to uid {}, not {uid}", owner.trim()));
        }
        Ok(())
    }
}

pub struct Pool {
    config: DaemonConfig,
    cids: BTreeSet<u32>,
    slots: BTreeSet<u32>,
}

impl Pool {
    pub fn new(config: DaemonConfig) -> Pool {
        Pool { config, cids: BTreeSet::new(), slots: BTreeSet::new() }
    }

    pub fn dns(&self) -> &[Ipv4Addr] {
        &self.config.dns
    }

    /// The lowest free CID.
    pub fn alloc_cid(&mut self) -> Result<u32, String> {
        let cid = self
            .config
            .cids
            .clone()
            .find(|cid| !self.cids.contains(cid))
            .ok_or_else(|| format!("all CIDs ({}-{}) are in use", self.config.cids.start(), self.config.cids.end()))?;
        self.cids.insert(cid);
        Ok(cid)
    }

    pub fn free_cid(&mut self, cid: u32) {
        self.cids.remove(&cid);
    }

    pub fn slot(&self, index: u32) -> NetSlot {
        let base = u32::from(self.config.subnet) + 4 * index;
        NetSlot {
            index,
            tap: format!("{}{index}", self.config.tap_prefix),
            host: Ipv4Addr::from(base + 1),
            guest: Ipv4Addr::from(base + 2),
        }
    }

    /// The lowest free slot whose tap is usable.
    pub fn alloc_net(&mut self, taps: &dyn TapCheck) -> Result<NetSlot, String> {
        let mut first_problem = None;
        for index in 0..self.config.net_slots() {
            if self.slots.contains(&index) {
                continue;
            }
            let slot = self.slot(index);
            match taps.usable(&slot.tap) {
                Ok(()) => {
                    self.slots.insert(index);
                    return Ok(slot);
                }
                Err(e) => {
                    first_problem.get_or_insert(e);
                    // setup-host-net.sh numbers its taps from 0 without gaps.
                    break;
                }
            }
        }
        let busy = self.slots.len();
        Err(match first_problem {
            Some(e) if busy == 0 => format!("no usable tap ({e}): run ./setup-host-net.sh up"),
            Some(e) => format!("all {busy} taps are in use ({e}): create more with ./setup-host-net.sh up"),
            None => format!("all {busy} networks in the subnet are in use"),
        })
    }

    pub fn free_net(&mut self, slot: &NetSlot) {
        self.slots.remove(&slot.index);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeTaps(u32);

    impl TapCheck for FakeTaps {
        fn usable(&self, tap: &str) -> Result<(), String> {
            let n: u32 = tap.trim_start_matches("crosvm_tap").parse().unwrap();
            if n < self.0 { Ok(()) } else { Err(format!("{tap} doesn't exist")) }
        }
    }

    fn pool() -> Pool {
        Pool::new(DaemonConfig { cids: 3..=5, ..DaemonConfig::default() })
    }

    #[test]
    fn cids() {
        let mut p = pool();
        assert_eq!(p.alloc_cid(), Ok(3));
        assert_eq!(p.alloc_cid(), Ok(4));
        assert_eq!(p.alloc_cid(), Ok(5));
        assert!(p.alloc_cid().unwrap_err().contains("3-5"));
        p.free_cid(4);
        assert_eq!(p.alloc_cid(), Ok(4));
    }

    #[test]
    fn nets() {
        let mut p = pool();
        let taps = FakeTaps(2);
        let a = p.alloc_net(&taps).unwrap();
        assert_eq!(a.tap, "crosvm_tap0");
        assert_eq!((a.host, a.guest), (Ipv4Addr::new(192, 168, 10, 1), Ipv4Addr::new(192, 168, 10, 2)));
        let b = p.alloc_net(&taps).unwrap();
        assert_eq!(b.tap, "crosvm_tap1");
        assert_eq!((b.host, b.guest), (Ipv4Addr::new(192, 168, 10, 5), Ipv4Addr::new(192, 168, 10, 6)));
        assert!(p.alloc_net(&taps).unwrap_err().contains("all 2 taps are in use"));
        p.free_net(&a);
        assert_eq!(p.alloc_net(&taps).unwrap(), a);
        assert!(pool().alloc_net(&FakeTaps(0)).unwrap_err().contains("setup-host-net.sh up"));
    }
}
