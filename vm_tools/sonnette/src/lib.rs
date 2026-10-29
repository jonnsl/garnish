//! sonnette: desktop notifications from crosvm guests on the host, a small
//! take on ChromeOS's notificationd.
//!
//! `sonnette-agent` runs in the guest's user session. It is the guest's
//! notification server (`org.freedesktop.Notifications` on the session bus)
//! and hands every notification to the host. `sonnette` runs on the host: it
//! shows them through the host's own notification server, and passes clicks
//! and dismissals back. Both sides talk over vsock, see [`proto`].

pub mod base64;
pub mod markup;
pub mod proto;
pub mod vsock;
