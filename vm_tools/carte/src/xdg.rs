//! XDG base directories.

use std::env;
use std::path::{Path, PathBuf};

pub fn home() -> PathBuf {
    env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

fn absolute_var(name: &str) -> Option<PathBuf> {
    env::var_os(name).map(PathBuf::from).filter(|p| p.is_absolute())
}

/// $XDG_DATA_HOME, by default ~/.local/share.
pub fn data_home() -> PathBuf {
    absolute_var("XDG_DATA_HOME").unwrap_or_else(|| home().join(".local/share"))
}

/// $XDG_CONFIG_HOME, by default ~/.config.
pub fn config_home() -> PathBuf {
    absolute_var("XDG_CONFIG_HOME").unwrap_or_else(|| home().join(".config"))
}

/// $XDG_DATA_HOME followed by $XDG_DATA_DIRS, most important first.
pub fn data_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![data_home()];
    let system = env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    for dir in system.split(':').map(Path::new) {
        if dir.is_absolute() && !dirs.iter().any(|d| d == dir) {
            dirs.push(dir.to_owned());
        }
    }
    dirs
}

/// $XDG_RUNTIME_DIR, by default /run/user/<uid>.
pub fn runtime_dir() -> PathBuf {
    absolute_var("XDG_RUNTIME_DIR")
        .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() })))
}
