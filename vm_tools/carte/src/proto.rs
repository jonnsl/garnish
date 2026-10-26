//! The protocol between `carte` (host) and `carte-agent` (guest).
//!
//! The host connects to the agent on vsock port [`AGENT_PORT`]. Every message
//! is one line of JSON. The host sends a [`Request`] and the agent answers:
//!
//! - `watch`: an [`AppList`] now and again every time it changes. Nothing else
//!   can be sent on the connection after this.
//! - `icon`: a [`Reply`] with `format` and `len`, followed by `len` bytes of
//!   image data, or a [`Reply`] with `error`.
//! - `launch`: a [`Reply`], with `error` set if it failed.
//!
//! Several `icon` and `launch` requests can share a connection.

use std::collections::BTreeMap;
use std::io::{self, BufRead, Read, Write};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub const AGENT_PORT: u32 = 8890;

/// Upper bound for a single message line.
const MAX_LINE: u64 = 16 << 20;
/// Upper bound for icon data.
pub const MAX_ICON: u64 = 4 << 20;

/// Localized strings keyed by locale ("de", "pt_BR", ...), "" for the default.
pub type Localized = BTreeMap<String, String>;

#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Watch,
    Icon { id: String, size: u32 },
    /// `vm` must match the agent's VM name, so a stale CID can't start an
    /// application in a different VM.
    Launch { vm: String, id: String },
}

#[derive(Serialize, Deserialize, Debug)]
pub struct AppList {
    /// The VM's name: sommelier's --vm-identifier, which is also part of the
    /// app_id of every window that comes from this VM.
    pub vm: String,
    pub apps: Vec<App>,
}

/// One application, from the [Desktop Entry] keys of the same name.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct App {
    /// Desktop file ID without ".desktop", e.g. "org.gnome.Calculator".
    pub id: String,
    pub name: Localized,
    #[serde(default)]
    pub generic_name: Localized,
    #[serde(default)]
    pub comment: Localized,
    #[serde(default)]
    pub keywords: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub startup_wm_class: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Default)]
pub struct Reply {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Icon replies: "png" or "svg".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    /// Icon replies: number of bytes of image data after this line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub len: Option<u64>,
}

impl Reply {
    pub fn error(message: impl Into<String>) -> Self {
        Reply { error: Some(message.into()), ..Default::default() }
    }
}

pub fn send<T: Serialize>(w: &mut impl Write, msg: &T) -> io::Result<()> {
    let mut line = serde_json::to_vec(msg)?;
    line.push(b'\n');
    w.write_all(&line)
}

/// Reads one message. `Ok(None)` means the other side closed the connection.
pub fn recv<T: DeserializeOwned>(r: &mut impl BufRead) -> io::Result<Option<T>> {
    let mut line = Vec::new();
    if (&mut *r).take(MAX_LINE).read_until(b'\n', &mut line)? == 0 {
        return Ok(None);
    }
    if line.last() != Some(&b'\n') {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "message too long or cut off"));
    }
    serde_json::from_slice(&line)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Desktop file IDs that are safe to use in host file names, unit names and
/// Exec= lines. Real ones are reverse-DNS names or plain words, so this only
/// loses exotic entries.
pub fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 200
        && !id.starts_with('.')
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

/// Like [`is_valid_id`] but without '-': the host names files
/// carte-<vm>-<id>.desktop, and a '-' in <vm> would make that ambiguous.
pub fn is_valid_vm(vm: &str) -> bool {
    !vm.is_empty()
        && vm.len() <= 64
        && !vm.starts_with('.')
        && vm.bytes().all(|b| b.is_ascii_alphanumeric() || b"._".contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_format() {
        let line = serde_json::to_string(&Request::Icon { id: "a".into(), size: 64 }).unwrap();
        assert_eq!(line, r#"{"type":"icon","id":"a","size":64}"#);
        let watch: Request = serde_json::from_str(r#"{"type":"watch"}"#).unwrap();
        assert_eq!(watch, Request::Watch);
    }

    #[test]
    fn round_trip() {
        let mut buf = Vec::new();
        send(&mut buf, &Reply::error("nope")).unwrap();
        send(&mut buf, &Reply { format: Some("png".into()), len: Some(3), error: None }).unwrap();
        let mut r = &buf[..];
        let a: Reply = recv(&mut r).unwrap().unwrap();
        let b: Reply = recv(&mut r).unwrap().unwrap();
        assert_eq!(a.error.as_deref(), Some("nope"));
        assert_eq!((b.format.as_deref(), b.len), (Some("png"), Some(3)));
        assert!(recv::<Reply>(&mut r).unwrap().is_none());
    }

    #[test]
    fn cut_off_message() {
        let mut r = &b"{\"error\":"[..];
        assert!(recv::<Reply>(&mut r).is_err());
    }

    #[test]
    fn names() {
        assert!(is_valid_id("org.gnome.Calculator"));
        assert!(is_valid_id("kde4-kate"));
        assert!(!is_valid_id("../x"));
        assert!(!is_valid_id("a b"));
        assert!(!is_valid_id(".hidden"));
        assert!(is_valid_vm("debian"));
        assert!(!is_valid_vm("debian-2"));
    }
}
