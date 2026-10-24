//! Authenticated pod telemetry, separate from direct observations and routing.
//! Only the connected seat owner publishes. Receivers project a fresh report
//! into panel state; they never publish it again or use it to acquire a seat.

use std::collections::HashMap;

use super::crypto::{open_fleet, seal_fleet};
use super::presence::{FleetKeys, SKEW_MS};
use crate::pods::reports::{Accessory, Placement, Pod, REPORT_TTL_MS};
use crate::types::{Address, Battery, Clock, Evidence, LidState, MonoMs, UnixMs};

pub const VERSION: u8 = 10;
pub const TTL_MS: u64 = 10_000;
const REFRESH_MS: u64 = 3_000;
const CHANGE_MS: u64 = 1_000;
const DOMAIN: &[u8] = b"fleetpods-pod-state-v1";
const BODY_LEN: usize = 104;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Reading<T> {
    value: T,
    observed: UnixMs,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct State {
    left: Option<Reading<Battery>>,
    right: Option<Reading<Battery>>,
    left_place: Option<Reading<Placement>>,
    right_place: Option<Reading<Placement>>,
    case: Option<Reading<Battery>>,
    lid: Option<Reading<LidState>>,
}

fn reading<T: Copy>(evidence: &Evidence<T>, now: Clock) -> Option<Reading<T>> {
    Some(Reading { value: *evidence.as_fresh()?, observed: now.unix_at(evidence.at()?) })
}

impl State {
    fn capture(direct: &Accessory, now: Clock) -> Self {
        let battery = |pod: &Evidence<Pod>| {
            reading(pod, now).map(|r| Reading { value: r.value.battery, observed: r.observed })
        };
        Self {
            left: battery(&direct.left),
            right: battery(&direct.right),
            left_place: reading(&direct.left_placement, now),
            right_place: reading(&direct.right_placement, now),
            case: reading(&direct.case_battery, now),
            lid: reading(&direct.lid.latest, now)
                .filter(|r| now.unix.0.saturating_sub(r.observed.0) <= REPORT_TTL_MS),
        }
    }

    fn same_values(&self, other: &Self) -> bool {
        fn value<T: Copy>(r: Option<Reading<T>>) -> Option<T> {
            r.map(|r| r.value)
        }
        value(self.left) == value(other.left)
            && value(self.right) == value(other.right)
            && value(self.left_place) == value(other.left_place)
            && value(self.right_place) == value(other.right_place)
            && value(self.case) == value(other.case)
            && value(self.lid) == value(other.lid)
    }

    /// Disconnected hosts prefer the owner's bud reports. A live control
    /// session keeps its own unless the owner observed them later: a link
    /// can go silent for its 20 s supervision timeout when the buds swap
    /// roles (gram, 2026-10-24 11:19). Lid/case are compared by observation
    /// time, since a follower may have heard a newer case advertisement.
    fn apply(&self, direct: &mut Accessory, linked: bool, now: Clock) -> Vec<&'static str> {
        let mut used = Vec::new();
        let later = |observed: UnixMs, prior: Option<MonoMs>| {
            !linked || prior.is_none_or(|at| observed > now.unix_at(at))
        };
        for (right, battery, placement, label) in [
            (false, self.left, self.left_place, "left"),
            (true, self.right, self.right_place, "right"),
        ] {
            let (pod_at, place_at) = if right {
                (direct.right.at(), direct.right_placement.at())
            } else {
                (direct.left.at(), direct.left_placement.at())
            };
            let battery = battery.filter(|r| later(r.observed, pod_at));
            let placement = placement.filter(|r| later(r.observed, place_at));
            let slot = if right { &mut direct.right } else { &mut direct.left };
            // A linked host keeps its own placement flags unless the owner's are newer.
            let held = slot.known().copied().filter(|_| linked);
            if let Some(r) = battery {
                *slot = Evidence::fresh(
                    Pod {
                        battery: r.value,
                        in_ear: placement.map_or(held.and_then(|p| p.in_ear), |p| p.value.in_ear),
                        in_case: placement
                            .map_or(held.and_then(|p| p.in_case), |p| p.value.in_case),
                    },
                    mono_at(now, r.observed),
                );
            } else if let (Some(p), Some(pod), Some(at)) = (placement, held, pod_at) {
                *slot = Evidence::fresh(
                    Pod { in_ear: p.value.in_ear, in_case: p.value.in_case, ..pod },
                    at,
                );
            }
            if let Some(r) = placement {
                direct.note_placement(
                    right,
                    r.value.in_ear,
                    r.value.in_case,
                    mono_at(now, r.observed),
                );
            }
            if battery.is_some() || placement.is_some() {
                used.push(label);
            }
        }
        let newer = |observed: UnixMs, prior: Option<MonoMs>| {
            now.unix.0.saturating_sub(observed.0) <= REPORT_TTL_MS
                && prior.is_none_or(|at| observed > now.unix_at(at))
        };
        if let Some(r) = self.case.filter(|r| newer(r.observed, direct.case_battery.at())) {
            direct.note_case(r.value, mono_at(now, r.observed));
            used.push("case");
        }
        if let Some(r) = self.lid.filter(|r| newer(r.observed, direct.lid.latest.at())) {
            direct.note_lid(r.value, mono_at(now, r.observed));
            used.push("lid");
        }
        used
    }
}

fn mono_at(now: Clock, observed: UnixMs) -> MonoMs {
    MonoMs(now.mono.0.saturating_sub(now.unix.0.saturating_sub(observed.0)))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity {
    pub reporter: Address,
    pub accessory: Address,
    pub session: [u8; 16],
}

#[derive(Clone, Debug)]
pub struct Packet {
    pub identity: Identity,
    pub sequence: u64,
    pub sent: UnixMs,
    state: State,
}

fn push<T>(bytes: &mut Vec<u8>, r: Option<Reading<T>>, encode: impl Fn(T) -> [u8; 2]) {
    let (value, at) = r.map(|r| (encode(r.value), r.observed.0)).unwrap_or(([255; 2], 0));
    bytes.extend(value);
    bytes.extend(at.to_be_bytes());
}

fn battery(value: Battery) -> [u8; 2] {
    [value.level(), u8::from(value.charging)]
}
fn placement(value: Placement) -> [u8; 2] {
    let bit = |v: Option<bool>| v.map(u8::from).unwrap_or(255);
    [bit(value.in_ear), bit(value.in_case)]
}

fn seal(packet: &Packet, keys: &FleetKeys) -> Option<Vec<u8>> {
    let mut body = Vec::with_capacity(BODY_LEN);
    body.extend(packet.identity.reporter.bytes());
    body.extend(packet.identity.accessory.bytes());
    body.extend(packet.identity.session);
    body.extend(packet.sequence.to_be_bytes());
    body.extend(packet.sent.0.to_be_bytes());
    let s = packet.state;
    push(&mut body, s.left, battery);
    push(&mut body, s.right, battery);
    push(&mut body, s.left_place, placement);
    push(&mut body, s.right_place, placement);
    push(&mut body, s.case, battery);
    push(&mut body, s.lid, |v| [v.wire() as u8, 255]);
    let mut frame = vec![VERSION];
    frame.extend(seal_fleet(DOMAIN, &keys.id, &keys.key, &body)?);
    Some(frame)
}

fn parse_reading<T>(
    bytes: &[u8],
    sent: UnixMs,
    decode: impl Fn([u8; 2]) -> Option<T>,
) -> Option<Option<Reading<T>>> {
    let value = [bytes[0], bytes[1]];
    let at = u64::from_be_bytes(bytes[2..10].try_into().ok()?);
    if at == 0 {
        return (value == [255; 2]).then_some(None);
    }
    if at > sent.0.saturating_add(SKEW_MS) {
        return None;
    }
    Some(Some(Reading { value: decode(value)?, observed: UnixMs(at) }))
}

fn read(bytes: &[u8], keys: &FleetKeys, now: UnixMs) -> Option<Packet> {
    if bytes.len() != BODY_LEN + 29 || bytes.first() != Some(&VERSION) {
        return None;
    }
    let b = open_fleet(DOMAIN, &keys.id, &keys.key, &bytes[1..])?;
    if b.len() != BODY_LEN {
        return None;
    }
    let identity = Identity {
        reporter: Address::new(b[0..6].try_into().ok()?)?,
        accessory: Address::new(b[6..12].try_into().ok()?)?,
        session: b[12..28].try_into().ok()?,
    };
    let sequence = u64::from_be_bytes(b[28..36].try_into().ok()?);
    let sent = UnixMs(u64::from_be_bytes(b[36..44].try_into().ok()?));
    if identity.reporter == identity.accessory
        || identity.session == [0; 16]
        || sequence == 0
        || !fresh(sent, now)
    {
        return None;
    }
    let cell = |v: [u8; 2]| (v[1] <= 1).then(|| Battery::new(v[0], v[1] == 1)).flatten();
    let bit = |v| match v {
        0 => Some(Some(false)),
        1 => Some(Some(true)),
        255 => Some(None),
        _ => None,
    };
    let place = |v: [u8; 2]| Some(Placement { in_ear: bit(v[0])?, in_case: bit(v[1])? });
    let lid = |v: [u8; 2]| match v {
        [0, 255] => Some(LidState::Open),
        [1, 255] => Some(LidState::Closed),
        _ => None,
    };
    Some(Packet {
        identity,
        sequence,
        sent,
        state: State {
            left: parse_reading(&b[44..54], sent, cell)?,
            right: parse_reading(&b[54..64], sent, cell)?,
            left_place: parse_reading(&b[64..74], sent, place)?,
            right_place: parse_reading(&b[74..84], sent, place)?,
            case: parse_reading(&b[84..94], sent, cell)?,
            lid: parse_reading(&b[94..104], sent, lid)?,
        },
    })
}

fn fresh(sent: UnixMs, now: UnixMs) -> bool {
    sent.0 != 0 && sent.0 <= now.0.saturating_add(SKEW_MS) && now.0 < sent.0.saturating_add(TTL_MS)
}

#[derive(Default)]
pub struct Sharing {
    scope: Option<([u8; 16], Address)>,
    offered: Option<(Identity, State, MonoMs)>,
    sequence: u64,
    received: HashMap<Address, (Packet, MonoMs)>,
}

impl Sharing {
    pub fn scope(&mut self, scope: Option<([u8; 16], Address)>) {
        if self.scope != scope {
            *self = Self { scope, ..Self::default() };
        }
    }

    pub fn stop_publishing(&mut self) {
        self.offered = None;
    }

    /// Coalesce changes to at most one slot/second, refresh every three seconds.
    /// Observation timestamps do not count as changes and are never refreshed here.
    pub fn offer(
        &mut self,
        identity: Identity,
        direct: &Accessory,
        now: Clock,
        keys: &FleetKeys,
    ) -> Option<Vec<u8>> {
        self.scope(Some((keys.id, identity.accessory)));
        let state = State::capture(direct, now);
        if state == State::default() && self.offered.is_none() {
            return None;
        }
        if let Some((old_id, old, at)) = self.offered {
            let elapsed = now.mono.saturating_since(at);
            if elapsed < CHANGE_MS
                || (old_id == identity && old.same_values(&state) && elapsed < REFRESH_MS)
            {
                return None;
            }
        }
        self.sequence = self.sequence.saturating_add(1);
        let packet = Packet { identity, sequence: self.sequence, sent: now.unix, state };
        let frame = seal(&packet, keys)?;
        self.offered = Some((identity, state, now.mono));
        Some(frame)
    }

    pub fn hear(
        &mut self,
        bytes: &[u8],
        keys: &FleetKeys,
        accessory: Address,
        now: Clock,
        member: impl Fn(Address) -> bool,
    ) -> Option<Address> {
        self.scope(Some((keys.id, accessory)));
        self.received.retain(|reporter, _| member(*reporter));
        let packet = read(bytes, keys, now.unix)?;
        let id = packet.identity;
        if id.accessory != accessory || !member(id.reporter) {
            return None;
        }
        if let Some((previous, _)) = self.received.get(&id.reporter) {
            if (previous.identity.session == id.session && previous.sequence >= packet.sequence)
                || (previous.identity.session != id.session && previous.sent >= packet.sent)
                || previous.sent > packet.sent
            {
                return None;
            }
        }
        self.received.insert(id.reporter, (packet, now.mono));
        Some(id.reporter)
    }

    /// A matching fresh roster must independently identify this reporter's
    /// live control session and seat. A telemetry packet cannot elect itself.
    pub fn current(&self, keys: [u8; 16], identity: Identity, now: Clock) -> Option<&Packet> {
        if self.scope != Some((keys, identity.accessory)) {
            return None;
        }
        let (packet, at) = self.received.get(&identity.reporter)?;
        (packet.identity == identity
            && fresh(packet.sent, now.unix)
            && now.mono.saturating_since(*at) < TTL_MS)
            .then_some(packet)
    }

    pub fn project(
        packet: &Packet,
        direct: &Accessory,
        linked: bool,
        now: Clock,
    ) -> (Accessory, Vec<&'static str>) {
        let mut resolved = direct.clone();
        let used = packet.state.apply(&mut resolved, linked, now);
        (resolved, used)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(n: u8) -> Address {
        Address::new([2, 0, 0, 0, 0, n]).unwrap()
    }
    fn keys() -> FleetKeys {
        FleetKeys { id: [3; 16], key: [7; 32] }
    }
    fn identity() -> Identity {
        Identity { reporter: addr(1), accessory: addr(9), session: [2; 16] }
    }
    fn clock(ms: u64) -> Clock {
        Clock::new(ms, ms + 1_000_000)
    }
    fn direct(now: Clock) -> Accessory {
        let mut a = Accessory::new();
        a.note_pod(
            false,
            Pod {
                battery: Battery::new(65, false).unwrap(),
                in_ear: Some(true),
                in_case: Some(false),
            },
            now.mono,
        );
        a.note_placement(true, None, Some(true), now.mono);
        a.note_case(Battery::new(80, true).unwrap(), now.mono);
        a.note_lid(LidState::Open, now.mono);
        a
    }
    fn packet(now: Clock) -> Packet {
        Packet {
            identity: identity(),
            sequence: 1,
            sent: now.unix,
            state: State::capture(&direct(now), now),
        }
    }

    #[test]
    fn round_trip_preserves_unknowns_and_original_observation_times() {
        let now = clock(1000);
        let mut p = packet(now);
        p.sent = clock(5000).unix;
        let frame = seal(&p, &keys()).unwrap();
        assert_eq!(frame.len(), 133);
        let decoded = read(&frame, &keys(), p.sent).unwrap();
        assert_eq!(decoded.state, p.state);
        assert_eq!(decoded.state.left.unwrap().observed, now.unix);
        assert_eq!(decoded.state.right, None);
        assert_eq!(decoded.state.right_place.unwrap().value.in_ear, None);
    }

    #[test]
    fn rejects_tampering_invalid_values_expiry_and_future_envelopes() {
        let now = clock(1000);
        let p = packet(now);
        let frame = seal(&p, &keys()).unwrap();
        for i in [0, 12, frame.len() - 1] {
            let mut altered = frame.clone();
            altered[i] ^= 1;
            assert!(read(&altered, &keys(), now.unix).is_none());
        }
        assert!(read(&frame, &keys(), clock(1000 + TTL_MS).unix).is_none());
        assert!(read(&frame, &keys(), UnixMs(now.unix.0 - SKEW_MS - 1)).is_none());
        let body = open_fleet(DOMAIN, &keys().id, &keys().key, &frame[1..]).unwrap();
        for (offset, value) in [(44, 101), (45, 2), (64, 3), (94, 2), (95, 0)] {
            let mut invalid = body.clone();
            invalid[offset] = value;
            let mut frame = vec![VERSION];
            frame.extend(seal_fleet(DOMAIN, &keys().id, &keys().key, &invalid).unwrap());
            assert!(read(&frame, &keys(), now.unix).is_none(), "offset {offset}");
        }
    }

    #[test]
    fn membership_scope_replay_and_session_bound_the_cache() {
        let now = clock(1000);
        let p = packet(now);
        let frame = seal(&p, &keys()).unwrap();
        let mut sharing = Sharing::default();
        assert!(sharing.hear(&frame, &keys(), addr(8), now, |_| true).is_none());
        assert!(sharing.hear(&frame, &keys(), addr(9), now, |_| false).is_none());
        assert_eq!(sharing.hear(&frame, &keys(), addr(9), now, |_| true), Some(addr(1)));
        assert!(sharing.hear(&frame, &keys(), addr(9), clock(1100), |_| true).is_none());
        assert!(sharing.current(keys().id, identity(), now).is_some());
        let mut id = identity();
        id.session = [4; 16];
        assert!(sharing.current(keys().id, id, now).is_none());
        let mut newer = p.clone();
        newer.identity = id;
        newer.sent = clock(1500).unix;
        assert!(sharing
            .hear(&seal(&newer, &keys()).unwrap(), &keys(), addr(9), clock(1500), |_| true)
            .is_some());
        assert!(sharing.hear(&frame, &keys(), addr(9), clock(1600), |_| true).is_none());
        assert!(sharing.current(keys().id, id, clock(1500 + TTL_MS)).is_none());
        sharing.scope(None);
        assert!(sharing.current(keys().id, id, clock(1600)).is_none());
    }

    #[test]
    fn changes_coalesce_while_unchanged_reports_only_refresh() {
        let now = clock(1000);
        let mut a = direct(now);
        let mut sharing = Sharing::default();
        assert!(sharing.offer(identity(), &a, now, &keys()).is_some());
        a.note_placement(false, Some(false), Some(true), clock(1100).mono);
        assert!(sharing.offer(identity(), &a, clock(1100), &keys()).is_none());
        a.note_placement(false, Some(true), Some(false), clock(1200).mono);
        assert!(sharing.offer(identity(), &a, clock(2000), &keys()).is_none());
        a.note_placement(false, Some(false), Some(true), clock(2100).mono);
        let changed = sharing.offer(identity(), &a, clock(2100), &keys()).unwrap();
        assert_eq!(read(&changed, &keys(), clock(2100).unix).unwrap().sequence, 2);
        // A repeat observation is not another state change.
        a.note_placement(false, Some(false), Some(true), clock(3100).mono);
        assert!(sharing.offer(identity(), &a, clock(3100), &keys()).is_none());
        assert!(sharing.offer(identity(), &a, clock(5100), &keys()).is_some());
    }

    #[test]
    fn owner_buds_win_without_overwriting_direct_observations_or_newer_case_data() {
        let now = clock(2000);
        let p = packet(clock(1000));
        let mut follower = direct(now);
        follower.note_placement(false, Some(false), Some(true), now.mono);
        follower.note_lid(LidState::Closed, now.mono);
        follower.note_case(Battery::new(90, true).unwrap(), now.mono);
        let (resolved, used) = Sharing::project(&p, &follower, false, now);
        assert_eq!(resolved.left_placement.known().unwrap().in_ear, Some(true));
        assert_eq!(follower.left_placement.known().unwrap().in_ear, Some(false));
        assert_eq!(resolved.lid.last_known, Some(LidState::Closed));
        assert_eq!(resolved.case_battery.known().unwrap().level(), 90);
        assert_eq!(used, vec!["left", "right"]);
        let (linked, used) = Sharing::project(&p, &follower, true, now);
        assert_eq!(linked.left_placement, follower.left_placement);
        assert!(used.is_empty());
    }

    #[test]
    fn a_linked_host_takes_the_owners_newer_bud_readings() {
        // gram, 2026-10-24 11:19: a pod went into the case, the buds swapped
        // roles, and the gram's still "ready" link said nothing for 20 s
        // while the owner reported the change.
        let mut follower = direct(clock(1000));
        let mut owner = direct(clock(1000));
        owner.note_pod(
            false,
            Pod {
                battery: Battery::new(66, true).unwrap(),
                in_ear: Some(false),
                in_case: Some(true),
            },
            clock(1500).mono,
        );
        let now = clock(2000);
        let p = Packet {
            identity: identity(),
            sequence: 2,
            sent: now.unix,
            state: State::capture(&owner, now),
        };
        let (resolved, used) = Sharing::project(&p, &follower, true, now);
        assert_eq!(used, vec!["left"]);
        let place = resolved.left_placement.known().unwrap();
        assert_eq!((place.in_ear, place.in_case), (Some(false), Some(true)));
        let pod = resolved.left.known().unwrap();
        assert_eq!((pod.battery.level(), pod.in_ear), (66, Some(false)));

        // A newer local observation still wins on a live link.
        follower.note_placement(false, Some(true), Some(false), clock(1800).mono);
        let (resolved, used) = Sharing::project(&p, &follower, true, now);
        assert_eq!(resolved.left_placement.known().unwrap().in_ear, Some(true));
        assert_eq!(resolved.left.known().unwrap().battery.level(), 66);
        assert_eq!(resolved.left.known().unwrap().in_ear, Some(true));
        assert_eq!(used, vec!["left"]);
    }

    #[test]
    fn a_refresh_does_not_make_old_case_observations_fresh() {
        let mut p = packet(clock(1000));
        let now = clock(1000 + REPORT_TTL_MS + 1);
        p.sent = now.unix;
        let (resolved, used) = Sharing::project(&p, &Accessory::new(), false, now);
        assert!(resolved.lid.latest.is_unknown());
        assert!(resolved.case_battery.is_unknown());
        assert!(!used.contains(&"lid"));
        // Unchanged bud facts remain valid while the reporting control link lives.
        assert_eq!(resolved.left_placement.known().unwrap().in_ear, Some(true));
    }
    #[test]
    fn withdrawn_readings_replace_a_previous_snapshot_with_unknowns() {
        let mut sharing = Sharing::default();
        let now = clock(1000);
        assert!(sharing.offer(identity(), &direct(now), now, &keys()).is_some());
        let later = clock(2000);
        let bytes = sharing.offer(identity(), &Accessory::new(), later, &keys()).unwrap();
        let packet = read(&bytes, &keys(), later.unix).unwrap();
        assert_eq!(packet.state, State::default());
        let local = direct(later);
        let (resolved, used) = Sharing::project(&packet, &local, false, later);
        assert!(used.is_empty());
        assert_eq!(resolved.left_placement, local.left_placement);
    }
}
