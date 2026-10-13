//! The process edge. Adapters call [`crate::Daemon::step`]. They do not keep a
//! second copy of the link or the fleet.
//!
//! BlueZ, the control socket, the Secret Service, and PipeWire are attached.
//! The BlueZ thread writes letter fragments the daemon already chose.

pub mod bluez;
pub mod control;
pub mod keyring;
pub mod micsource;
pub mod os;
pub mod paths;
pub mod pipewire;
pub mod players;
pub mod session;
pub mod wake;
