//! What this user's VMs share: vsock CIDs. Networks come from gerant-net
//! (see `net`), which shares them between all users.

use std::collections::BTreeSet;
use std::net::Ipv4Addr;

use crate::config::DaemonConfig;

pub struct Pool {
    config: DaemonConfig,
    cids: BTreeSet<u32>,
}

impl Pool {
    pub fn new(config: DaemonConfig) -> Pool {
        Pool { config, cids: BTreeSet::new() }
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cids() {
        let mut p = Pool::new(DaemonConfig { cids: 3..=5, ..DaemonConfig::default() });
        assert_eq!(p.alloc_cid(), Ok(3));
        assert_eq!(p.alloc_cid(), Ok(4));
        assert_eq!(p.alloc_cid(), Ok(5));
        assert!(p.alloc_cid().unwrap_err().contains("3-5"));
        p.free_cid(4);
        assert_eq!(p.alloc_cid(), Ok(4));
    }
}
