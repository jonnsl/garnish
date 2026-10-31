//! XDG base directories.

use std::env;
use std::path::PathBuf;

pub fn home() -> PathBuf {
    env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

fn absolute_var(name: &str) -> Option<PathBuf> {
    env::var_os(name).map(PathBuf::from).filter(|p| p.is_absolute())
}

/// $XDG_CONFIG_HOME/garnish, by default ~/.config/garnish.
pub fn config_dir() -> PathBuf {
    absolute_var("XDG_CONFIG_HOME").unwrap_or_else(|| home().join(".config")).join("garnish")
}

/// $XDG_STATE_HOME/garnish, by default ~/.local/state/garnish.
pub fn state_dir() -> PathBuf {
    absolute_var("XDG_STATE_HOME").unwrap_or_else(|| home().join(".local/state")).join("garnish")
}

/// $XDG_RUNTIME_DIR, by default /run/user/<uid>.
pub fn runtime_dir() -> PathBuf {
    absolute_var("XDG_RUNTIME_DIR")
        .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() })))
}

/// The login name of the user running us.
pub fn user_name() -> String {
    env::var("USER").ok().filter(|u| !u.is_empty()).unwrap_or_else(|| {
        let uid = unsafe { libc::getuid() };
        // getpwuid's result is only read before the next call; we make none.
        let pw = unsafe { libc::getpwuid(uid) };
        if pw.is_null() {
            return uid.to_string();
        }
        unsafe { std::ffi::CStr::from_ptr((*pw).pw_name) }.to_string_lossy().into_owned()
    })
}
