//! The remembered fleet device allowed to reconnect automatically.
//! Activity is independent of the AirPods connection and never expires just
//! because its owner sleeps. Peers gossip the same winning record; a refresh
//! does not manufacture new activity. The record is scoped to a fleet/accessory.

use super::crypto::{open_fleet, seal_fleet};
use super::presence::{FleetKeys, SKEW_MS};
use crate::types::{Address, MonoMs, UnixMs};

pub const VERSION: u8 = 9;
pub const REFRESH_MS: u64 = 3_000;
const DOMAIN: &[u8] = b"fleetpods-activity-v1";
const LIFETIME_MS: u64 = 15_000;
const SETTLE_MS: u64 = 2_000;
const MAGIC: &[u8] = b"FPAC1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Record {
    pub fleet: [u8; 16],
    pub accessory: Address,
    pub owner: Address,
    pub at: UnixMs,
}

#[derive(Default)]
pub struct Preference {
    record: Option<Record>,
    dirty: bool,
    offered_at: Option<MonoMs>,
    available_since: Option<MonoMs>,
    attempted: Option<Record>,
}

impl Preference {
    pub fn record(&self, fleet: [u8; 16], accessory: Address) -> Option<Record> {
        self.record.filter(|r| r.fleet == fleet && r.accessory == accessory)
    }

    pub fn note_local(&mut self, fleet: [u8; 16], accessory: Address, owner: Address, now: UnixMs) {
        // A just-heard event can have the same millisecond or a slightly fast
        // clock. Real subsequent input must win, without refreshing on a tick.
        let previous = self.record(fleet, accessory).map(|r| r.at.0).unwrap_or(0);
        let at = UnixMs(now.0.max(previous.saturating_add(1)));
        self.merge(Record { fleet, accessory, owner, at });
    }

    fn merge(&mut self, record: Record) -> bool {
        if self
            .record(record.fleet, record.accessory)
            .is_some_and(|old| (old.at, old.owner.bytes()) >= (record.at, record.owner.bytes()))
        {
            return false;
        }
        self.record = Some(record);
        self.dirty = true;
        self.offered_at = None;
        true
    }

    pub fn clear(&mut self) {
        *self = Self { dirty: true, ..Self::default() };
    }

    /// Startup restores the record before BlueZ identifies the paired device.
    /// Retain that record when the first matching discovery arrives.
    pub fn select_accessory(&mut self, accessory: Option<Address>) {
        if accessory.is_none() || self.record.is_some_and(|r| Some(r.accessory) != accessory) {
            self.clear();
        }
        self.available_since = None;
        self.attempted = None;
    }

    /// Availability is evidence; it does not itself change the preference.
    pub fn observe_availability(&mut self, available: bool, now: MonoMs) {
        if available {
            self.available_since.get_or_insert(now);
        } else {
            self.available_since = None;
            self.attempted = None;
        }
    }

    pub fn take_wake(&mut self, record: Record, local: Address, now: MonoMs) -> bool {
        if record.owner != local
            || self.attempted == Some(record)
            || self.available_since.is_none_or(|at| now.saturating_since(at) < SETTLE_MS)
        {
            return false;
        }
        self.attempted = Some(record);
        true
    }

    /// One bounded advertisement slot. Legacy presence/roster frames continue
    /// unchanged; old daemons ignore version 9 instead of losing discovery.
    pub fn offer(
        &mut self,
        record: Record,
        sender: Address,
        now: crate::types::Clock,
        keys: &FleetKeys,
    ) -> Option<Vec<u8>> {
        if self.offered_at.is_some_and(|at| now.mono.saturating_since(at) < REFRESH_MS) {
            return None;
        }
        let mut body = Vec::new();
        body.extend(sender.bytes());
        body.extend(record.accessory.bytes());
        body.extend(record.owner.bytes());
        body.extend(record.at.0.to_be_bytes());
        body.extend(now.unix.0.to_be_bytes());
        let mut frame = vec![VERSION];
        frame.extend(seal_fleet(DOMAIN, &keys.id, &keys.key, &body)?);
        self.offered_at = Some(now.mono);
        Some(frame)
    }

    /// Authenticate the envelope and validate both its relay and named owner.
    /// Old/replayed records cannot replace a newer winner, even after restart.
    pub fn hear(
        &mut self,
        bytes: &[u8],
        keys: &FleetKeys,
        accessory: Address,
        now: UnixMs,
        member: impl Fn(Address) -> bool,
    ) -> bool {
        if bytes.len() != 63 || bytes.first() != Some(&VERSION) {
            return false;
        }
        let Some(body) = open_fleet(DOMAIN, &keys.id, &keys.key, &bytes[1..]) else {
            return false;
        };
        if body.len() != 34 {
            return false;
        }
        let address = |offset| Address::new(body[offset..offset + 6].try_into().unwrap());
        let (Some(sender), Some(pods), Some(owner)) = (address(0), address(6), address(12)) else {
            return false;
        };
        let at = UnixMs(u64::from_be_bytes(body[18..26].try_into().unwrap()));
        let sent = u64::from_be_bytes(body[26..34].try_into().unwrap());
        if pods != accessory
            || sender == pods
            || owner == pods
            || !member(sender)
            || !member(owner)
            || sent == 0
            || sent > now.0.saturating_add(SKEW_MS)
            || sent.saturating_add(LIFETIME_MS) <= now.0
            || at.0 == 0
            || at.0 > sent.saturating_add(SKEW_MS)
            || at.0 > now.0.saturating_add(SKEW_MS)
        {
            return false;
        }
        self.merge(Record { fleet: keys.id, accessory, owner, at })
    }

    pub fn take_save(&mut self) -> Option<Vec<u8>> {
        if !std::mem::take(&mut self.dirty) {
            return None;
        }
        let mut bytes = MAGIC.to_vec();
        if let Some(record) = self.record {
            bytes.extend(record.fleet);
            bytes.extend(record.accessory.bytes());
            bytes.extend(record.owner.bytes());
            bytes.extend(record.at.0.to_be_bytes());
        }
        Some(bytes)
    }

    pub fn restore(&mut self, bytes: &[u8]) {
        if bytes == MAGIC {
            *self = Self::default();
            return;
        }
        if bytes.len() != 41 || !bytes.starts_with(MAGIC) {
            return;
        }
        let (Some(accessory), Some(owner)) = (
            Address::new(bytes[21..27].try_into().unwrap()),
            Address::new(bytes[27..33].try_into().unwrap()),
        ) else {
            return;
        };
        let at = UnixMs(u64::from_be_bytes(bytes[33..41].try_into().unwrap()));
        if at.0 == 0 || accessory == owner {
            return;
        }
        self.record =
            Some(Record { fleet: bytes[5..21].try_into().unwrap(), accessory, owner, at });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Clock;

    fn addr(n: u8) -> Address {
        Address::new([2, 0, 0, 0, 0, n]).unwrap()
    }

    fn keys() -> FleetKeys {
        FleetKeys { id: [3; 16], key: [7; 32] }
    }

    fn frame(owner: u8, at: u64, sent: u64) -> Vec<u8> {
        let mut preference = Preference::default();
        preference.note_local(keys().id, addr(9), addr(owner), UnixMs(at));
        let record = preference.record(keys().id, addr(9)).unwrap();
        preference.offer(record, addr(1), Clock::new(sent, sent), &keys()).unwrap()
    }

    #[test]
    fn a_sleeping_owner_survives_relay_restart_and_old_packets() {
        let mut peer = Preference::default();
        assert!(peer.hear(&frame(2, 1_000, 1_000), &keys(), addr(9), UnixMs(1_000), |_| true));
        let saved = peer.take_save().unwrap();
        let mut restarted = Preference::default();
        restarted.restore(&saved);
        let record = restarted.record(keys().id, addr(9)).unwrap();
        assert_eq!(record.owner, addr(2));
        assert!(!restarted.hear(&frame(1, 900, 2_000), &keys(), addr(9), UnixMs(2_000), |_| true));
        // A relay can refresh the envelope days later without refreshing activity.
        let now = Clock::new(200_000_000, 200_000_000);
        let relayed = restarted.offer(record, addr(1), now, &keys()).unwrap();
        let mut newcomer = Preference::default();
        assert!(newcomer.hear(&relayed, &keys(), addr(9), now.unix, |_| true));
        assert_eq!(newcomer.record(keys().id, addr(9)), Some(record));
        newcomer.observe_availability(true, now.mono);
        assert!(!newcomer.take_wake(record, addr(1), MonoMs(now.mono.0 + 60_000)));
        // Actual input, including input on a slightly slow clock, supersedes it.
        newcomer.note_local(keys().id, addr(9), addr(1), UnixMs(999));
        assert_eq!(newcomer.record(keys().id, addr(9)).unwrap().owner, addr(1));
    }

    #[test]
    fn concurrent_activity_converges_in_either_delivery_order() {
        let a = frame(1, 1_000, 1_000);
        let b = frame(2, 1_000, 1_000);
        for messages in [[&a, &b], [&b, &a]] {
            let mut preference = Preference::default();
            for message in messages {
                preference.hear(message, &keys(), addr(9), UnixMs(1_000), |_| true);
            }
            assert_eq!(preference.record(keys().id, addr(9)).unwrap().owner, addr(2));
        }
    }

    #[test]
    fn rejects_unauthenticated_wrong_scope_nonmember_expired_and_future_reports() {
        let valid = frame(2, 1_000, 1_000);
        let mut preference = Preference::default();
        let mut bad = valid.clone();
        bad[20] ^= 1;
        assert!(!preference.hear(&bad, &keys(), addr(9), UnixMs(1_000), |_| true));
        let foreign = FleetKeys { id: [4; 16], key: [7; 32] };
        assert!(!preference.hear(&valid, &foreign, addr(9), UnixMs(1_000), |_| true));
        assert!(!preference.hear(&valid, &keys(), addr(8), UnixMs(1_000), |_| true));
        for excluded in [addr(1), addr(2)] {
            assert!(!preference.hear(&valid, &keys(), addr(9), UnixMs(1_000), |a| a != excluded));
        }
        assert!(!preference.hear(&valid, &keys(), addr(9), UnixMs(16_000), |_| true));
        assert!(
            !preference.hear(&frame(2, 9_000, 9_000), &keys(), addr(9), UnixMs(1_000), |_| true)
        );
        assert!(
            !preference.hear(&frame(2, 9_000, 1_000), &keys(), addr(9), UnixMs(1_000), |_| true)
        );
        assert!(preference.record(keys().id, addr(9)).is_none());
    }

    #[test]
    fn availability_requests_one_wake_per_cycle_without_changing_preference() {
        let mut preference = Preference::default();
        preference.note_local(keys().id, addr(9), addr(1), UnixMs(1_000));
        let record = preference.record(keys().id, addr(9)).unwrap();
        preference.observe_availability(true, MonoMs(1_000));
        assert!(!preference.take_wake(record, addr(1), MonoMs(2_999)));
        assert!(preference.take_wake(record, addr(1), MonoMs(3_000)));
        assert!(!preference.take_wake(record, addr(1), MonoMs(30_000)));
        preference.observe_availability(false, MonoMs(30_000));
        preference.observe_availability(true, MonoMs(40_000));
        assert!(preference.take_wake(record, addr(1), MonoMs(42_000)));
        assert_eq!(preference.record(keys().id, addr(9)), Some(record));
        preference.clear();
        let mut restarted = Preference::default();
        restarted.restore(&preference.take_save().unwrap());
        assert!(restarted.record(keys().id, addr(9)).is_none());
    }
}
