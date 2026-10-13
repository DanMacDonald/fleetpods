//! Acceptance scenarios from REQUIREMENTS.md section 13.
//!
//! `cargo xtask test` runs these in parallel. Each test builds its own world.
//! Platform events are queued and not applied until the fake BlueZ, radio,
//! audio, and keyring loop is connected, so the scenarios fail closed.

#[path = "scenarios/mod.rs"]
mod scenarios;
#[path = "support/mod.rs"]
mod support;
