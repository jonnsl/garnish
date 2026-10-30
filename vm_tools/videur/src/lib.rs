//! videur: a Wayland proxy between a crosvm guest and the host compositor
//! that controls what the guest may copy to the host and paste from it.

pub mod filter;
pub mod protocol;
pub mod proxy;
pub mod wire;
