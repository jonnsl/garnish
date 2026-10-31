//! The D-Bus API, shared by the daemon and the client.

use serde::{Deserialize, Serialize};
use zvariant::Type;

pub const BUS_NAME: &str = "org.garnish.Gerant";
pub const OBJECT_PATH: &str = "/org/garnish/Gerant";
pub const INTERFACE: &str = "org.garnish.Gerant1";

/// A VM and what it was given. Fields that don't apply are empty or 0.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct VmInfo {
    pub name: String,
    /// "stopped", "starting", "booting", "running" or "stopping". Booting:
    /// crosvm runs, but the guest hasn't said it has booted yet.
    pub state: String,
    pub cid: u32,
    /// The tap device and the guest's address on it.
    pub tap: String,
    pub address: String,
    /// The guest user, for ssh.
    pub user: String,
    /// crosvm's process ID.
    pub pid: u32,
    /// The directory with the VM's logs.
    pub log_dir: String,
}

impl VmInfo {
    pub fn stopped(name: &str) -> VmInfo {
        VmInfo {
            name: name.to_owned(),
            state: "stopped".to_owned(),
            cid: 0,
            tap: String::new(),
            address: String::new(),
            user: String::new(),
            pid: 0,
            log_dir: String::new(),
        }
    }
}

#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.garnish.Gerant1.Error")]
pub enum Error {
    #[zbus(error)]
    ZBus(zbus::Error),
    /// There is no configuration file for the VM.
    NoSuchVm(String),
    /// The configuration file is wrong or refers to missing files.
    InvalidConfig(String),
    AlreadyRunning(String),
    NotRunning(String),
    /// Out of CIDs or tap devices.
    NoResources(String),
    /// crosvm or a helper didn't start.
    StartFailed(String),
    /// The daemon is stopping all VMs and exiting.
    ShuttingDown(String),
}

#[zbus::proxy(
    interface = "org.garnish.Gerant1",
    default_service = "org.garnish.Gerant",
    default_path = "/org/garnish/Gerant"
)]
pub trait Gerant {
    /// Starts the VM configured in `<config dir>/vms/<name>.conf` and
    /// returns once crosvm runs, with the VM booting.
    fn start_vm(&self, name: &str) -> zbus::Result<VmInfo>;
    /// Asks the guest to shut down (ACPI power button). If it is still
    /// running after the daemon's StopTimeoutSec=, crosvm is stopped.
    /// Returns right away; VmStopped follows.
    fn stop_vm(&self, name: &str) -> zbus::Result<()>;
    /// Stops crosvm without waiting for the guest. Returns right away.
    fn kill_vm(&self, name: &str) -> zbus::Result<()>;
    /// Every configured VM, and running VMs whose configuration is gone.
    fn list_vms(&self) -> zbus::Result<Vec<VmInfo>>;

    #[zbus(signal)]
    fn vm_started(&self, info: VmInfo) -> zbus::Result<()>;
    /// `reason` is a human-readable description of how crosvm exited.
    #[zbus(signal)]
    fn vm_stopped(&self, name: &str, reason: &str) -> zbus::Result<()>;
    /// A VM's state changed: starting, booting, running, stopping or stopped (sent
    /// before VmStarted and VmStopped). Also sent with the new PID when
    /// crosvm is started again after a guest reboot.
    #[zbus(signal)]
    fn vm_state_changed(&self, info: VmInfo) -> zbus::Result<()>;
}
