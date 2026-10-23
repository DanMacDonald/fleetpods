//! Airtime bookkeeping for presence and roster announcements.
//!
//! The daemon says what the announcement is; this module remembers what
//! last aired and decides whether the next one is due, so cadence and
//! change detection live in one place instead of beside the radio schedule.

use crate::types::{Address, FleetId, MonoMs};

/// What the last presence slot actually sealed. A fleet id or session change
/// reseals on the next free slot.
#[derive(Clone, PartialEq, Eq)]
pub struct PresenceMark {
    pub address: Address,
    pub name: String,
    pub fleet: Option<FleetId>,
    pub session: Option<[u8; 16]>,
    pub handoff: bool,
}

#[derive(Default)]
pub struct PresenceAir {
    mark: Option<PresenceMark>,
    aired: Option<MonoMs>,
}

impl PresenceAir {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a presence announcement is due, and if so whether the sealed
    /// form should air instead of the public one. Only a host with no fleet
    /// airs the public form: an enrolled host never broadcasts its address,
    /// name, or enrollment in cleartext, so unenrolled scanners cannot see
    /// it. A keyring-locked host has no fleet loaded and airs public with an
    /// unknown enrollment flag, like an unenrolled host.
    pub fn due(&self, mark: &PresenceMark, now: MonoMs, refresh_ms: u64) -> Option<bool> {
        let changed = self.mark.as_ref() != Some(mark);
        let aged = self.aired.is_none_or(|at| now.saturating_since(at) >= refresh_ms);
        if !changed && !aged {
            return None;
        }
        Some(mark.fleet.is_some())
    }

    pub fn note_aired(&mut self, mark: PresenceMark, now: MonoMs) {
        self.aired = Some(now);
        self.mark = Some(mark);
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }
}

/// What the last roster slot actually sealed.
#[derive(Clone, PartialEq, Eq)]
pub struct RosterMark {
    pub fleet: FleetId,
    pub session: [u8; 16],
    pub accessory: Address,
    pub peers: Vec<Address>,
    pub audio_source: Option<Address>,
    pub pending: bool,
    pub audio_idle: bool,
}

/// How a planned roster compares to the one that last aired.
#[derive(Clone, Copy)]
pub struct Assessment {
    pub changed: bool,
    pub peers_changed: bool,
    pub aged: bool,
}

#[derive(Default)]
pub struct RosterAir {
    mark: Option<RosterMark>,
    aired: Option<MonoMs>,
    seq: u64,
}

impl RosterAir {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn assess(&self, mark: &RosterMark, now: MonoMs, refresh_ms: u64) -> Assessment {
        Assessment {
            changed: self.mark.as_ref() != Some(mark),
            peers_changed: self.mark.as_ref().is_none_or(|previous| previous.peers != mark.peers),
            aged: self.aired.is_none_or(|at| now.saturating_since(at) >= refresh_ms),
        }
    }

    pub fn has_mark(&self) -> bool {
        self.mark.is_some()
    }

    pub fn next_seq(&mut self) -> u64 {
        self.seq = self.seq.saturating_add(1);
        self.seq
    }

    pub fn seq(&self) -> u64 {
        self.seq
    }

    pub fn note_queued(&mut self, mark: RosterMark) {
        self.mark = Some(mark);
    }

    pub fn note_aired(&mut self, now: MonoMs) {
        self.aired = Some(now);
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }
}
