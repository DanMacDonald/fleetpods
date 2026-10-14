//! This machine's sound.
//!
//! `play` chooses the output profile and holds streams. `route` chooses which
//! streams follow a claim. `mic` is capture. `seat` reads the pods and the
//! fleet and answers who is playing.

pub mod eld;
pub mod health;
pub mod mic;
pub mod play;
pub mod queue;
pub mod route;
pub mod seat;
