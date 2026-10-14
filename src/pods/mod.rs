//! This machine's AirPods.
//!
//! `link` is the only Bluetooth retry loop. `reports` stores what the pods
//! confirmed. `prefs` stores what the user asked for before that confirmation.

pub mod air;
pub mod link;
pub mod nearby;
pub mod observe;
pub mod peer;
pub mod prefs;
pub mod reconnect;
pub mod reports;
pub mod setup;
