//! FleetPods keeps one paired AirPods accessory moving between the user's computers.
//!
//! `fleetpodsd` is the process. This library is the decision core it calls.
//! Each fact has one module that changes it. Status JSON is a projection of
//! those modules and is never read back into a decision.

pub mod audio;
pub mod daemon;
mod devices;
pub mod fleet;
mod handshake;
pub mod linux;
pub mod panel;
pub mod pods;
pub mod step;
pub mod types;

pub use daemon::Daemon;
pub use types::time::Clock;
