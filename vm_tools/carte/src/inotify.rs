//! Waiting for changes in the applications directories.

use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::desktop;

const TREE_MASK: u32 = libc::IN_CREATE
    | libc::IN_DELETE
    | libc::IN_MOVED_FROM
    | libc::IN_MOVED_TO
    | libc::IN_CLOSE_WRITE
    | libc::IN_DELETE_SELF
    | libc::IN_MOVE_SELF
    | libc::IN_ONLYDIR;
/// For the parent of a directory that doesn't exist yet.
const PARENT_MASK: u32 = libc::IN_CREATE | libc::IN_MOVED_TO | libc::IN_ONLYDIR;
/// Keeps a directory that never stops changing from blocking updates.
const MAX_SETTLE: Duration = Duration::from_secs(10);

pub struct Watcher(OwnedFd);

impl Watcher {
    pub fn new() -> io::Result<Self> {
        let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Watcher(unsafe { OwnedFd::from_raw_fd(fd) }))
    }

    /// Watches `dir` and the directories below it. If `dir` doesn't exist,
    /// watches its closest existing ancestor instead, to notice it appear.
    /// Make a new Watcher after a change: watches aren't updated.
    pub fn watch_tree(&self, dir: &Path) {
        if dir.is_dir() {
            for sub in desktop::subdirectories(dir) {
                self.add(&sub, TREE_MASK);
            }
        } else if let Some(parent) = dir.ancestors().skip(1).find(|p| p.is_dir()) {
            self.add(parent, PARENT_MASK);
        }
    }

    fn add(&self, dir: &Path, mask: u32) {
        let Ok(path) = CString::new(dir.as_os_str().as_bytes()) else { return };
        if unsafe { libc::inotify_add_watch(self.0.as_raw_fd(), path.as_ptr(), mask) } < 0 {
            eprintln!("carte-agent: watching {}: {}", dir.display(), io::Error::last_os_error());
        }
    }

    /// Blocks until something changes, then until nothing has changed for
    /// `settle` (at most 10 s), so a package install is one update.
    pub fn wait(&self, settle: Duration) -> io::Result<()> {
        self.read()?;
        let start = Instant::now();
        while start.elapsed() < MAX_SETTLE && self.poll(settle)? {
            self.read()?;
        }
        Ok(())
    }

    fn read(&self) -> io::Result<()> {
        // The events themselves don't matter, only that there were some.
        let mut buf = [0u8; 4096];
        loop {
            let n = unsafe { libc::read(self.0.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
            if n >= 0 {
                return Ok(());
            }
            let err = io::Error::last_os_error();
            if err.kind() != io::ErrorKind::Interrupted {
                return Err(err);
            }
        }
    }

    /// Whether there are events within `timeout`.
    fn poll(&self, timeout: Duration) -> io::Result<bool> {
        let mut fds = libc::pollfd { fd: self.0.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        loop {
            let n = unsafe { libc::poll(&mut fds, 1, timeout.as_millis() as libc::c_int) };
            if n >= 0 {
                return Ok(n > 0);
            }
            let err = io::Error::last_os_error();
            if err.kind() != io::ErrorKind::Interrupted {
                return Err(err);
            }
        }
    }
}
