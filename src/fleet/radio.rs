//! Version-1 fleet advertisement fragments.
//!
//! A fragment is version, source, target, an 8-byte message id, part, count,
//! the first 8 bytes of SHA-256, then at most 112 payload bytes. The checksum
//! stops two messages mixing. It is not an authentication tag. A finished
//! payload is the in-process `pf1` frame. Other version bytes are left alone.

use std::collections::{BTreeMap, VecDeque};

use sha2::{Digest, Sha256};

use crate::types::{Address, MonoMs};

pub const CHUNK: usize = 112;
pub const HEADER: usize = 31;
pub const MAX_FRAME: usize = 8192;
pub const MAX_PARTS: usize = 80;
const MAX_PENDING: usize = 8;
const MAX_DONE: usize = 64;
const EXPIRE_MS: u64 = 10_000;

const AIR_MS: u64 = 500;
const MAX_LETTERS: usize = 4;
/// The previous daemon keeps cycling its latest enrollment packet while the
/// session is active. A letter gets the same lifetime so a peer that misses
/// one 500 ms fragment still hears the whole packet.
const REPEAT_MS: u64 = 120_000;

pub fn fragments(frame: &[u8], source: Address, target: Address, id: [u8; 8]) -> Vec<Vec<u8>> {
    if frame.is_empty() || frame.len() > MAX_FRAME {
        return Vec::new();
    }
    let count = frame.len().div_ceil(CHUNK);
    if count > MAX_PARTS {
        return Vec::new();
    }
    let digest = Sha256::digest(frame);
    let mut out = Vec::with_capacity(count);
    for (part, chunk) in frame.chunks(CHUNK).enumerate() {
        let mut bytes = Vec::with_capacity(HEADER + chunk.len());
        bytes.push(1);
        bytes.extend_from_slice(&source.bytes());
        bytes.extend_from_slice(&target.bytes());
        bytes.extend_from_slice(&id);
        bytes.push(part as u8);
        bytes.push(count as u8);
        bytes.extend_from_slice(&digest[..8]);
        bytes.extend_from_slice(chunk);
        out.push(bytes);
    }
    out
}

struct Assembly {
    count: u8,
    digest: [u8; 8],
    parts: BTreeMap<u8, Vec<u8>>,
    seen: MonoMs,
}

impl Default for Assembly {
    fn default() -> Self {
        Self { count: 0, digest: [0; 8], parts: BTreeMap::new(), seen: MonoMs(0) }
    }
}

/// Reassembles a version-1 letter addressed to this host.
pub struct Inbox {
    pending: BTreeMap<Vec<u8>, Assembly>,
    done: VecDeque<Vec<u8>>,
}

impl Inbox {
    pub fn new() -> Self {
        Self { pending: BTreeMap::new(), done: VecDeque::new() }
    }

    /// The payload, once every part has arrived and the checksum matches.
    pub fn accept(
        &mut self,
        data: &[u8],
        local: Option<Address>,
        now: MonoMs,
    ) -> Option<(Address, Vec<u8>)> {
        self.pending.retain(|_, assembly| now.saturating_since(assembly.seen) <= EXPIRE_MS);
        if data.len() <= HEADER || data.len() > HEADER + CHUNK || data[0] != 1 {
            return None;
        }
        let local = local?;
        let target = Address::new(data[7..13].try_into().ok()?)?;
        if target != local {
            return None;
        }
        let source = Address::new(data[1..7].try_into().ok()?)?;
        if source == local {
            return None;
        }
        let mut id = data[1..7].to_vec();
        id.extend_from_slice(&data[13..21]);
        if self.done.iter().any(|seen| seen == &id) {
            return None;
        }
        let part = data[21];
        let count = data[22];
        if count == 0 || count as usize > MAX_PARTS || part >= count {
            return None;
        }
        let mut digest = [0u8; 8];
        digest.copy_from_slice(&data[23..31]);
        if !self.pending.contains_key(&id) && self.pending.len() >= MAX_PENDING {
            self.pending.pop_first();
        }
        if let Some(existing) = self.pending.get(&id) {
            if existing.count != count || existing.digest != digest {
                self.pending.remove(&id);
                return None;
            }
        }
        let assembly = self.pending.entry(id.clone()).or_default();
        assembly.count = count;
        assembly.digest = digest;
        assembly.seen = now;
        assembly.parts.insert(part, data[HEADER..].to_vec());
        if assembly.parts.len() != count as usize {
            return None;
        }
        let mut payload = Vec::new();
        for index in 0..count {
            payload.extend(assembly.parts.get(&index)?.clone());
        }
        self.pending.remove(&id);
        if payload.is_empty()
            || payload.len() > MAX_FRAME
            || Sha256::digest(&payload)[..8] != digest
        {
            return None;
        }
        self.done.push_back(id);
        if self.done.len() > MAX_DONE {
            self.done.pop_front();
        }
        Some((source, payload))
    }
}

impl Default for Inbox {
    fn default() -> Self {
        Self::new()
    }
}

struct Queued {
    to: Address,
    frame: Vec<u8>,
}

/// Letters waiting for a fragment slot. One public-presence frame may wait
/// behind them. `due` returns the next advertisement payload, or asks the
/// radio to stop once the last payload has been up for [`AIR_MS`].
pub struct Schedule {
    waiting: VecDeque<Queued>,
    parts: VecDeque<Vec<Vec<u8>>>,
    part_at: usize,
    repeat: Option<Vec<Vec<u8>>>,
    repeat_at: usize,
    repeat_until: Option<MonoMs>,
    letter_slot: u8,
    aired_at: Option<MonoMs>,
    on_air: bool,
    presence: Option<Vec<u8>>,
    presence_urgent: bool,
    roster: Option<Vec<u8>>,
    roster_remaining: u8,
    roster_urgent: bool,
    /// After presence, the next free slot belongs to a waiting roster.
    roster_next: bool,
    route: Option<Vec<u8>>,
    route_urgent: bool,
    route_until: Option<MonoMs>,
    /// Presence, roster, and route each get one slot per rotation. Empty
    /// announcement slots carry the route, without delaying queued rosters.
    route_slot: u8,
}

impl Schedule {
    pub fn new() -> Self {
        Self {
            waiting: VecDeque::new(),
            parts: VecDeque::new(),
            part_at: 0,
            repeat: None,
            repeat_at: 0,
            repeat_until: None,
            letter_slot: 0,
            aired_at: None,
            on_air: false,
            presence: None,
            presence_urgent: false,
            roster: None,
            roster_remaining: 0,
            roster_urgent: false,
            roster_next: false,
            route: None,
            route_urgent: false,
            route_until: None,
            route_slot: 0,
        }
    }

    /// Keep the latest presence frame until a free slot airs it. A letter goes first.
    pub fn offer(&mut self, frame: Vec<u8>) {
        self.presence = Some(frame);
        self.presence_urgent = false;
    }

    /// Handoff presence must not wait behind repeated routing packets that
    /// the receiver cannot yet authenticate. The caller bounds its cadence.
    pub fn offer_handoff_presence(&mut self, frame: Vec<u8>) {
        self.offer(frame);
        self.presence_urgent = true;
    }

    pub fn clear_presence(&mut self) {
        self.presence = None;
        self.presence_urgent = false;
    }

    /// Keep the latest roster frame. A letter still goes first, then presence
    /// and roster alternate.
    pub fn offer_roster(&mut self, frame: Vec<u8>) {
        self.roster = Some(frame);
        self.roster_remaining = 1;
        self.roster_urgent = false;
    }

    /// A seat change takes the next non-pairing slot, then repeats twice in
    /// ordinary roster slots so a missed BLE advertisement costs less time.
    pub fn offer_roster_change(&mut self, frame: Vec<u8>) {
        self.roster = Some(frame);
        self.roster_remaining = 3;
        self.roster_urgent = true;
    }

    pub fn clear_roster(&mut self) {
        self.roster = None;
        self.roster_remaining = 0;
        self.roster_urgent = false;
    }

    pub fn has_roster(&self) -> bool {
        self.roster.is_some()
    }

    /// Keep a routing packet until it is cleared. A changed packet starts on
    /// the route slot, so its first airing is that packet. Repeating the
    /// same bytes leaves the rotation alone.
    pub fn offer_route(&mut self, frame: Vec<u8>, until: MonoMs) {
        if self.route.as_ref() != Some(&frame) {
            self.route_slot = 2;
            self.route_urgent = true;
        }
        self.route = Some(frame);
        self.route_until = Some(until);
    }

    pub fn clear_route(&mut self) {
        self.route = None;
        self.route_urgent = false;
        self.route_until = None;
        self.route_slot = 0;
    }

    /// Confirmation replies keep repeating after the local transaction ends.
    pub fn has_live_route(&self, now: MonoMs) -> bool {
        self.route.is_some() && self.route_until.is_some_and(|until| now <= until)
    }

    pub fn push(&mut self, to: Address, frame: Vec<u8>) {
        while self.waiting.len() + self.parts.len() >= MAX_LETTERS {
            if self.waiting.pop_front().is_none() {
                self.parts.pop_front();
                self.part_at = 0;
            }
        }
        self.waiting.push_back(Queued { to, frame });
    }

    /// Fragment every letter still waiting. A missing id puts that letter back.
    pub fn note_source(&mut self, source: Address, mut ident: impl FnMut() -> Option<[u8; 8]>) {
        let waiting = std::mem::take(&mut self.waiting);
        for letter in waiting {
            let Some(id) = ident() else {
                self.waiting.push_back(letter);
                continue;
            };
            let parts = fragments(&letter.frame, source, letter.to, id);
            if !parts.is_empty() {
                // A newer letter supersedes the packet being repeated.
                self.repeat = None;
                self.repeat_until = None;
                self.parts.push_back(parts);
            }
        }
    }

    pub fn due(&mut self, now: MonoMs) -> Duty {
        if self.route_until.is_some_and(|until| now.0 > until.0) {
            self.clear_route();
        }
        if self.repeat_until.is_some_and(|until| now.0 > until.0) {
            self.repeat = None;
            self.repeat_until = None;
        }
        if let Some(bytes) = self.take_part(now) {
            return Duty::Publish(Item::Letter(bytes));
        }
        if self.busy() {
            return Duty::Hold;
        }
        if let Some(item) = self.take_idle(now) {
            return Duty::Publish(item);
        }
        if self.on_air && self.aired_at.is_some_and(|at| now.saturating_since(at) >= AIR_MS) {
            self.on_air = false;
            self.aired_at = None;
            return Duty::Release;
        }
        Duty::Hold
    }

    fn busy(&self) -> bool {
        !self.waiting.is_empty() || !self.parts.is_empty()
    }

    fn take_part(&mut self, now: MonoMs) -> Option<Vec<u8>> {
        if self.parts.is_empty() {
            return None;
        }
        if self.aired_at.is_some_and(|at| now.saturating_since(at) < AIR_MS) {
            return None;
        }
        let letter = self.parts.front()?;
        if self.part_at >= letter.len() {
            self.parts.pop_front();
            self.part_at = 0;
            return self.take_part(now);
        }
        let part = letter[self.part_at].clone();
        self.part_at += 1;
        if self.part_at >= letter.len() {
            // Keep cycling this packet like the previous daemon does. A
            // newer letter replaces it in `note_source`.
            self.repeat = self.parts.pop_front();
            self.repeat_at = 0;
            self.repeat_until = Some(MonoMs(now.0.saturating_add(REPEAT_MS)));
            self.part_at = 0;
        }
        self.aired_at = Some(now);
        self.on_air = true;
        Some(part)
    }

    fn take_repeat(&mut self, now: MonoMs) -> Option<Item> {
        if self.aired_at.is_some_and(|at| now.saturating_since(at) < AIR_MS) {
            return None;
        }
        let letter = self.repeat.as_ref()?;
        if letter.is_empty() {
            return None;
        }
        let part = letter[self.repeat_at % letter.len()].clone();
        self.repeat_at = (self.repeat_at + 1) % letter.len();
        self.mark_aired(now);
        Some(Item::Letter(part))
    }

    fn take_idle(&mut self, now: MonoMs) -> Option<Item> {
        if self.aired_at.is_some_and(|at| now.saturating_since(at) < AIR_MS) {
            return None;
        }
        // Start each handshake phase promptly. Re-offering the same packet
        // does not rearm this priority, so roster/presence slots still run.
        if self.route_urgent {
            self.route_urgent = false;
            self.route_slot = 0;
            let bytes = self.route.clone()?;
            self.mark_aired(now);
            return Some(Item::Route(bytes));
        }
        if self.roster_urgent {
            return self.take_roster(now);
        }
        if self.presence_urgent && self.route.is_none() {
            return self.take_presence(now);
        }
        // Previous-daemon rotation: presence, roster, then the repeated
        // enrollment packet. Routing still takes the later slots.
        if self.repeat.is_some() && self.route.is_none() {
            let slot = self.letter_slot % 4;
            self.letter_slot = self.letter_slot.wrapping_add(1);
            if slot == 0 && self.presence.is_some() {
                return self.take_announcement(now);
            }
            if slot == 1 && self.roster.is_some() {
                return self.take_roster(now);
            }
            return self.take_repeat(now);
        }
        if self.route.is_some() {
            let slot = self.route_slot % 3;
            self.route_slot = self.route_slot.wrapping_add(1);
            let announcement = match slot {
                0 => self.take_presence(now),
                1 => self.take_roster(now),
                _ => None,
            };
            if let Some(item) = announcement {
                return Some(item);
            }
            let bytes = self.route.clone()?;
            self.mark_aired(now);
            return Some(Item::Route(bytes));
        }
        self.take_announcement(now)
    }

    fn take_announcement(&mut self, now: MonoMs) -> Option<Item> {
        let roster_first = self.roster_next && self.roster.is_some();
        if roster_first {
            return self.take_roster(now);
        }
        if let Some(item) = self.take_presence(now) {
            return Some(item);
        }
        self.take_roster(now)
    }

    fn take_presence(&mut self, now: MonoMs) -> Option<Item> {
        let bytes = self.presence.take()?;
        self.presence_urgent = false;
        self.roster_next = self.roster.is_some();
        self.mark_aired(now);
        Some(Item::Presence(bytes))
    }

    fn take_roster(&mut self, now: MonoMs) -> Option<Item> {
        let bytes = self.roster.as_ref()?.clone();
        self.roster_remaining = self.roster_remaining.saturating_sub(1);
        if self.roster_remaining == 0 {
            self.roster = None;
        }
        self.roster_urgent = false;
        self.roster_next = false;
        self.mark_aired(now);
        Some(Item::Roster(bytes))
    }

    fn mark_aired(&mut self, now: MonoMs) {
        self.aired_at = Some(now);
        self.on_air = true;
    }
}

impl Default for Schedule {
    fn default() -> Self {
        Self::new()
    }
}

/// Which advertisement `due` chose. The caller does not read the bytes to find out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    Letter(Vec<u8>),
    Presence(Vec<u8>),
    Roster(Vec<u8>),
    Route(Vec<u8>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Duty {
    Hold,
    Publish(Item),
    Release,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(byte: u8) -> Address {
        Address::new([byte, 0, 0, 0, 0, 1]).unwrap()
    }

    #[test]
    fn a_split_frame_returns_once_and_a_replay_does_not() {
        let frame = vec![7u8; 200];
        let parts = fragments(&frame, addr(1), addr(2), [9; 8]);
        assert_eq!(parts.len(), 2);
        let mut inbox = Inbox::new();
        assert!(inbox.accept(&parts[0], Some(addr(2)), MonoMs(0)).is_none());
        let got = inbox.accept(&parts[1], Some(addr(2)), MonoMs(10)).unwrap();
        assert_eq!(got, (addr(1), frame));
        assert!(inbox.accept(&parts[0], Some(addr(2)), MonoMs(20)).is_none());
        assert!(inbox.accept(&parts[1], Some(addr(2)), MonoMs(20)).is_none());
    }

    #[test]
    fn the_wrong_host_a_bad_checksum_and_an_oversized_frame_are_dropped() {
        let frame = b"pf1\n{}".to_vec();
        let mut parts = fragments(&frame, addr(1), addr(2), [3; 8]);
        assert!(Inbox::new().accept(&parts[0], Some(addr(4)), MonoMs(0)).is_none());
        parts[0][23] ^= 0xff;
        assert!(Inbox::new().accept(&parts[0], Some(addr(2)), MonoMs(0)).is_none());
        assert!(fragments(&vec![1u8; MAX_FRAME + 1], addr(1), addr(2), [0; 8]).is_empty());
    }

    #[test]
    fn a_letter_waits_for_the_source_address_and_then_airs_one_piece_at_a_time() {
        let frame = vec![9u8; 200];
        let mut schedule = Schedule::new();
        schedule.push(addr(2), frame.clone());
        assert!(matches!(schedule.due(MonoMs(0)), Duty::Hold));
        schedule.note_source(addr(1), || Some([5; 8]));
        let Duty::Publish(Item::Letter(first)) = schedule.due(MonoMs(0)) else {
            panic!("first fragment should air as soon as the source is known");
        };
        assert!(matches!(schedule.due(MonoMs(400)), Duty::Hold));
        let Duty::Publish(Item::Letter(second)) = schedule.due(MonoMs(500)) else {
            panic!("second fragment should wait 500 ms");
        };
        let mut inbox = Inbox::new();
        assert!(inbox.accept(&first, Some(addr(2)), MonoMs(0)).is_none());
        assert_eq!(inbox.accept(&second, Some(addr(2)), MonoMs(500)).unwrap(), (addr(1), frame));
        assert!(Inbox::new().accept(&first, Some(addr(3)), MonoMs(0)).is_none());
        assert!(matches!(schedule.due(MonoMs(500)), Duty::Hold));
        // The finished packet keeps cycling, as the previous daemon airs it,
        // until its lifetime ends or a newer letter replaces it.
        let Duty::Publish(Item::Letter(repeat_first)) = schedule.due(MonoMs(1_000)) else {
            panic!("finished letter should repeat its first fragment");
        };
        assert_eq!(repeat_first, first);
        let Duty::Publish(Item::Letter(repeat_second)) = schedule.due(MonoMs(1_500)) else {
            panic!("finished letter should repeat its second fragment");
        };
        assert_eq!(repeat_second, second);
        assert!(matches!(schedule.due(MonoMs(121_000)), Duty::Release));
    }

    #[test]
    fn a_letter_airs_before_a_presence_frame_that_is_waiting() {
        let mut schedule = Schedule::new();
        schedule.push(addr(2), vec![9u8; 200]);
        schedule.note_source(addr(1), || Some([5; 8]));
        schedule.offer(vec![2, 1, 2, 3]);
        assert!(matches!(
            schedule.due(MonoMs(0)),
            Duty::Publish(Item::Letter(bytes)) if bytes[0] == 1
        ));
        assert!(matches!(schedule.due(MonoMs(400)), Duty::Hold));
        assert!(matches!(
            schedule.due(MonoMs(500)),
            Duty::Publish(Item::Letter(bytes)) if bytes[0] == 1
        ));
        assert!(matches!(
            schedule.due(MonoMs(1_000)),
            Duty::Publish(Item::Presence(bytes)) if bytes == vec![2, 1, 2, 3]
        ));
        // Presence took its rotation slot; the repeated letter takes the next.
        assert!(matches!(
            schedule.due(MonoMs(1_500)),
            Duty::Publish(Item::Letter(bytes)) if bytes[0] == 1
        ));
    }

    #[test]
    fn a_newer_letter_replaces_the_repeating_packet() {
        let mut schedule = Schedule::new();
        schedule.push(addr(2), vec![9u8; 40]);
        schedule.note_source(addr(1), || Some([5; 8]));
        let Duty::Publish(Item::Letter(first_packet)) = schedule.due(MonoMs(0)) else {
            panic!("first packet should air");
        };
        assert!(matches!(schedule.due(MonoMs(500)), Duty::Publish(Item::Letter(_))));
        schedule.push(addr(2), vec![7u8; 40]);
        schedule.note_source(addr(1), || Some([6; 8]));
        let Duty::Publish(Item::Letter(next_packet)) = schedule.due(MonoMs(1_000)) else {
            panic!("newer packet should replace the repeat");
        };
        assert_ne!(next_packet, first_packet);
    }

    #[test]
    fn presence_and_roster_alternate_after_letters() {
        let mut schedule = Schedule::new();
        schedule.offer(vec![2]);
        schedule.offer_roster(vec![7]);
        assert!(
            matches!(schedule.due(MonoMs(0)), Duty::Publish(Item::Presence(bytes)) if bytes == vec![2])
        );
        assert!(
            matches!(schedule.due(MonoMs(500)), Duty::Publish(Item::Roster(bytes)) if bytes == vec![7])
        );
        assert!(matches!(schedule.due(MonoMs(1_000)), Duty::Release));
    }

    #[test]
    fn a_route_leaves_the_next_slot_for_presence() {
        let mut schedule = Schedule::new();
        schedule.offer(vec![2]);
        schedule.offer_route(vec![5], MonoMs(30_000));
        assert!(matches!(
            schedule.due(MonoMs(0)),
            Duty::Publish(Item::Route(bytes)) if bytes == vec![5]
        ));
        assert!(matches!(
            schedule.due(MonoMs(500)),
            Duty::Publish(Item::Presence(bytes)) if bytes == vec![2]
        ));
        assert!(matches!(
            schedule.due(MonoMs(1_000)),
            Duty::Publish(Item::Route(bytes)) if bytes == vec![5]
        ));
        // The request is still queued after presence has gone out.
        assert!(matches!(
            schedule.due(MonoMs(1_500)),
            Duty::Publish(Item::Route(bytes)) if bytes == vec![5]
        ));
    }

    #[test]
    fn a_changed_roster_gets_the_next_slot_and_repeats_between_route_messages() {
        let mut schedule = Schedule::new();
        schedule.offer_route(vec![5], MonoMs(30_000));
        schedule.due(MonoMs(0));
        schedule.offer(vec![2]);
        schedule.offer_roster_change(vec![7, 1]);
        assert!(matches!(schedule.due(MonoMs(499)), Duty::Hold));
        assert!(
            matches!(schedule.due(MonoMs(500)), Duty::Publish(Item::Roster(bytes)) if bytes == vec![7, 1])
        );
        let mut rosters = 1;
        let mut routes = 0;
        let mut presence = 0;
        for at in (1_000..=4_000).step_by(500) {
            match schedule.due(MonoMs(at)) {
                Duty::Publish(Item::Roster(bytes)) => {
                    assert_eq!(bytes, vec![7, 1]);
                    rosters += 1;
                }
                Duty::Publish(Item::Route(_)) => routes += 1,
                Duty::Publish(Item::Presence(_)) => presence += 1,
                _ => {}
            }
        }
        assert_eq!(rosters, 3);
        assert!(routes > 0);
        assert_eq!(presence, 1);
        schedule.offer_roster_change(vec![7, 2]);
        assert!(
            matches!(schedule.due(MonoMs(4_500)), Duty::Publish(Item::Roster(bytes)) if bytes == vec![7, 2])
        );
        schedule.offer_roster_change(vec![7]); // Disconnection supersedes the old burst.
        assert!(
            matches!(schedule.due(MonoMs(5_000)), Duty::Publish(Item::Roster(bytes)) if bytes == vec![7])
        );
    }

    #[test]
    fn roster_repeats_get_slots_even_with_continuous_presence_and_routing() {
        let mut schedule = Schedule::new();
        schedule.offer_route(vec![5], MonoMs(30_000));
        schedule.offer_roster_change(vec![7]);
        let mut rosters = Vec::new();
        let mut routes = Vec::new();
        let mut presence = Vec::new();
        for at in (0..=4_000).step_by(500) {
            // More presence than the daemon normally queues: it still must
            // not push roster repeats or connection confirmation out of line.
            schedule.offer_handoff_presence(vec![4]);
            schedule.offer_route(vec![5], MonoMs(30_000));
            match schedule.due(MonoMs(at)) {
                Duty::Publish(Item::Roster(_)) => rosters.push(at),
                Duty::Publish(Item::Route(_)) => routes.push(at),
                Duty::Publish(Item::Presence(_)) => presence.push(at),
                _ => panic!("a queued handoff needs a broadcast at each slot"),
            }
        }
        assert_eq!(rosters.len(), 3);
        assert_eq!(routes[0], 0);
        assert_eq!(rosters[0], 500);
        assert!(rosters[1] <= 1_500 && rosters[2] <= 3_000, "rosters: {rosters:?}");
        assert!(routes.len() >= 2 && presence.len() >= 2);
        // The one-off changed roster follows the first route immediately;
        // subsequent routing packets keep their ordinary rotation slots.
        assert!(routes[1] - routes[0] <= 2_000);
        assert!(routes[1..].windows(2).all(|pair| pair[1] - pair[0] <= 1_500));
        assert!(presence.windows(2).all(|pair| pair[1] - pair[0] <= 1_500));
    }

    #[test]
    fn a_new_admission_precedes_disconnection_roster_without_starving_it() {
        let mut schedule = Schedule::new();
        schedule.offer_route(vec![5, 1], MonoMs(30_000));
        assert!(matches!(schedule.due(MonoMs(0)), Duty::Publish(Item::Route(_))));
        schedule.offer_handoff_presence(vec![4]);
        schedule.offer_roster_change(vec![7]);
        schedule.offer_route(vec![5, 2], MonoMs(30_000));
        assert!(matches!(schedule.due(MonoMs(499)), Duty::Hold));
        assert!(matches!(schedule.due(MonoMs(500)),
            Duty::Publish(Item::Route(bytes)) if bytes == vec![5, 2]));
        // Repeated requests cause the helper to offer the same admission.
        // That must not jump ahead of the roster again.
        schedule.offer_route(vec![5, 2], MonoMs(30_000));
        assert!(matches!(schedule.due(MonoMs(1_000)), Duty::Publish(Item::Roster(_))));
        assert!(matches!(schedule.due(MonoMs(1_500)), Duty::Publish(Item::Presence(_))));
        schedule.offer_route(vec![5, 3], MonoMs(30_000));
        schedule.clear_route();
        assert!(matches!(schedule.due(MonoMs(2_000)), Duty::Publish(Item::Roster(_))));
    }

    #[test]
    fn expired_route_packets_stop_competing_with_rosters() {
        let mut schedule = Schedule::new();
        schedule.offer_route(vec![5], MonoMs(30_000));
        schedule.due(MonoMs(0));
        schedule.offer_route(vec![5], MonoMs(30_000));
        schedule.offer_roster(vec![7]);
        assert!(
            matches!(schedule.due(MonoMs(30_001)), Duty::Publish(Item::Roster(bytes)) if bytes == vec![7])
        );
        assert!(matches!(schedule.due(MonoMs(30_501)), Duty::Release));
    }
}
