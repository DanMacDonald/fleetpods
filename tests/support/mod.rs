//! Isolated worlds for the acceptance suite.
//!
//! A test must not touch the process environment, the filesystem, or a socket.
//! Simulated time stays inside the world, which is what lets the suite run on
//! many threads at once.

pub(crate) mod accessory;
mod world;

pub use world::{profile, Slot, World};
