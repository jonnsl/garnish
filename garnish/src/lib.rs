//! garnish: a small take on ChromeOS's garcon for crosvm guests.
//!
//! `garnish-agent` runs in the guest's user session. It finds the installed
//! applications (.desktop files), hands their list and icons to the host and
//! launches them on request. `garnish` runs on the host: it turns the list
//! into .desktop files in the host's application menu that launch the guest's
//! applications. Both sides talk over vsock, see [`proto`].

pub mod desktop;
pub mod icons;
pub mod inotify;
pub mod menu;
pub mod proto;
pub mod vsock;
pub mod xdg;
