//! The other computers.
//!
//! A nearby host is not a member. An invitation is not a saved fleet.
//! A route may ask `pods::link` to connect. It does not start its own retry.

pub mod activity;
pub mod airtime;
pub mod commit;
pub mod cppmsg;
pub mod crypto;
pub mod enroll;
pub mod letter;
pub mod members;
pub mod nearby;
pub mod postcard;
pub mod presence;
pub mod radio;
pub mod roster;
pub mod roster_air;
pub mod route;
pub mod route_air;
pub mod wire;
