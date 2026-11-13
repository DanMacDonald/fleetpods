//! Why this laptop is not reopening its control link on its own.
//!
//! One record owns every reason, so the events that end a hold (a ready
//! link, an accessory switch) clear them together and each opening path asks
//! one question. Explicit Connect and Play never consult it.

use super::REOPEN_HOLD_MS;
use crate::types::MonoMs;

#[derive(Debug, Default)]
pub(super) struct ReopenHold {
    /// An Apple device took the slot this laptop had been sitting in.
    /// A failed route, a closed case, and an idle link do not set this.
    /// It has no deadline: only input, a case cycle or a new link ends it.
    pub(super) displaced: bool,
    /// A slot was yielded or lost; the incoming device gets until then to
    /// finish connecting without being displaced.
    pub(super) slot_until: Option<MonoMs>,
    /// The fleet laptop that owned the pair's audio reconnects first. The
    /// grace window starts at the first held attempt.
    pub(super) owner_first: bool,
    pub(super) owner_until: Option<MonoMs>,
}

impl ReopenHold {
    /// A ready link or another accessory supersedes every earlier reason.
    pub(super) fn clear(&mut self) {
        *self = Self::default();
    }

    /// This laptop is deliberately opening again beside the device that
    /// took its slot. The owner-first order is a separate agreement.
    pub(super) fn clear_slot_loss(&mut self) {
        self.displaced = false;
        self.slot_until = None;
    }

    /// Holds only ever extend; a shorter one cannot cut a longer one short.
    pub(super) fn hold_slot(&mut self, until: MonoMs) {
        if self.slot_until.is_none_or(|held| held.0 < until.0) {
            self.slot_until = Some(until);
        }
    }

    pub(super) fn wait_for_owner(&mut self) {
        self.owner_first = true;
        self.owner_until = None;
    }

    pub(super) fn owner_landed(&mut self) {
        self.owner_first = false;
        self.owner_until = None;
    }

    pub(super) fn slot_held(&mut self, now: MonoMs) -> bool {
        if self.slot_until.is_some_and(|until| now.0 < until.0) {
            return true;
        }
        self.slot_until = None;
        false
    }

    /// After the grace window this laptop may attach anyway: the owner may
    /// be gone.
    pub(super) fn owner_blocked(&mut self, now: MonoMs) -> bool {
        if !self.owner_first {
            return false;
        }
        let until = *self.owner_until.get_or_insert_with(|| now.saturating_add(REOPEN_HOLD_MS));
        if now.0 < until.0 {
            return true;
        }
        self.owner_landed();
        false
    }

    /// Openings nobody asked for: worn advertisements and ACL attachment.
    /// Losing a slot beside an Apple device is not a new availability event.
    /// Repeated worn advertisements can outlive the settling delay while two
    /// laptops each remember themselves as preferred, so displacement waits
    /// for input instead of evicting the other laptop again.
    pub(super) fn passive_blocked(&mut self, now: MonoMs) -> bool {
        self.displaced || self.slot_held(now) || self.owner_blocked(now)
    }
}
