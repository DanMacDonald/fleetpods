//! Sealed roster advertisement.
//!
//! Version 7 is the plain snapshot. Version 8 adds one flag byte: `1` while a
//! pair is pending, `2` when the audio source is only the idle owner. Version 6
//! is the older snapshot without a last-use time. The seal is the fleet key.

use super::crypto::{open_fleet, seal_fleet};
use super::presence::{FleetKeys, SKEW_MS};
use super::wire::ROSTER_DOMAIN;
use crate::types::{Address, UnixMs};

pub const VERSION: u8 = 7;
pub const FLAGGED: u8 = 8;
pub const LEGACY: u8 = 6;
pub const LIFETIME_MS: u64 = 15_000;
const PLAIN: usize = 100;
const WITH_FLAG: usize = 101;
const LEGACY_LEN: usize = 92;
const PENDING: u8 = 1;
const IDLE: u8 = 2;

pub struct Snapshot {
    pub reporter: Address,
    pub session: [u8; 16],
    pub sequence: u64,
    pub sent: UnixMs,
    pub accessory: Address,
    pub peers: Vec<Address>,
    pub audio_source: Option<Address>,
    pub last_use: UnixMs,
    pub pending: bool,
    pub audio_idle: bool,
}

pub fn advertise(snapshot: &Snapshot, keys: &FleetKeys) -> Option<Vec<u8>> {
    let body = encode_body(snapshot)?;
    let sealed = seal_fleet(ROSTER_DOMAIN, &keys.id, &keys.key, &body)?;
    let version = if snapshot.pending || snapshot.audio_idle { FLAGGED } else { VERSION };
    let mut frame = Vec::with_capacity(1 + sealed.len());
    frame.push(version);
    frame.extend(sealed);
    Some(frame)
}

pub fn read(packet: &[u8], keys: &FleetKeys) -> Option<Snapshot> {
    let legacy = packet.len() == LEGACY_LEN && packet.first() == Some(&LEGACY);
    let flagged = packet.len() == WITH_FLAG && packet.first() == Some(&FLAGGED);
    let plain = packet.len() == PLAIN && packet.first() == Some(&VERSION);
    if !legacy && !flagged && !plain {
        return None;
    }
    let body = open_fleet(ROSTER_DOMAIN, &keys.id, &keys.key, &packet[1..])?;
    if body.len() != packet.len() - 29 || body.get(44).copied().unwrap_or(3) > 2 {
        return None;
    }
    let flag = if flagged { body.get(71).copied() } else { None };
    if flagged && flag != Some(PENDING) && flag != Some(IDLE) {
        return None;
    }
    let snapshot = Snapshot {
        reporter: address_at(&body, 0)?,
        session: body.get(6..22)?.try_into().ok()?,
        sequence: u64::from_be_bytes(body.get(22..30)?.try_into().ok()?),
        sent: UnixMs(u64::from_be_bytes(body.get(30..38)?.try_into().ok()?)),
        accessory: address_at(&body, 38)?,
        peers: peers_at(&body)?,
        audio_source: optional_address(&body, 57)?,
        last_use: if legacy {
            UnixMs(0)
        } else {
            UnixMs(u64::from_be_bytes(body.get(63..71)?.try_into().ok()?))
        },
        pending: flag == Some(PENDING),
        audio_idle: flag == Some(IDLE),
    };
    valid(&snapshot).then_some(snapshot)
}

/// A roster timestamp that is still inside the advertisement lifetime.
pub fn fresh(sent: UnixMs, now: UnixMs) -> bool {
    sent.0 != 0
        && sent.0 <= now.0.saturating_add(SKEW_MS)
        && sent.0.saturating_add(LIFETIME_MS) > now.0
}

fn encode_body(snapshot: &Snapshot) -> Option<Vec<u8>> {
    if !valid(snapshot) {
        return None;
    }
    let mut body = Vec::with_capacity(72);
    body.extend_from_slice(&snapshot.reporter.bytes());
    body.extend_from_slice(&snapshot.session);
    body.extend_from_slice(&snapshot.sequence.to_be_bytes());
    body.extend_from_slice(&snapshot.sent.0.to_be_bytes());
    body.extend_from_slice(&snapshot.accessory.bytes());
    body.push(snapshot.peers.len() as u8);
    for slot in 0..2 {
        match snapshot.peers.get(slot) {
            Some(peer) => body.extend_from_slice(&peer.bytes()),
            None => body.extend_from_slice(&[0; 6]),
        }
    }
    match snapshot.audio_source {
        Some(source) => body.extend_from_slice(&source.bytes()),
        None => body.extend_from_slice(&[0; 6]),
    }
    body.extend_from_slice(&snapshot.last_use.0.to_be_bytes());
    if snapshot.pending {
        body.push(PENDING);
    } else if snapshot.audio_idle {
        body.push(IDLE);
    }
    Some(body)
}

fn valid(snapshot: &Snapshot) -> bool {
    if snapshot.pending && (snapshot.peers.len() != 2 || snapshot.audio_source.is_some()) {
        return false;
    }
    if snapshot.audio_idle && (snapshot.pending || snapshot.audio_source.is_none()) {
        return false;
    }
    if snapshot.reporter == snapshot.accessory || snapshot.sequence == 0 || snapshot.sent.0 == 0 {
        return false;
    }
    if snapshot.last_use.0 > snapshot.sent.0 || snapshot.peers.len() > 2 {
        return false;
    }
    if !snapshot.peers.is_empty() && !snapshot.peers.contains(&snapshot.reporter) {
        return false;
    }
    let mut seen = Vec::new();
    for peer in &snapshot.peers {
        if *peer == snapshot.accessory || seen.contains(peer) {
            return false;
        }
        seen.push(*peer);
    }
    snapshot.audio_source.is_none_or(|source| snapshot.peers.contains(&source))
}

fn address_at(body: &[u8], at: usize) -> Option<Address> {
    Address::new(body.get(at..at + 6)?.try_into().ok()?)
}

fn optional_address(body: &[u8], at: usize) -> Option<Option<Address>> {
    let bytes: [u8; 6] = body.get(at..at + 6)?.try_into().ok()?;
    if bytes == [0; 6] {
        Some(None)
    } else {
        Some(Some(Address::new(bytes)?))
    }
}

fn peers_at(body: &[u8]) -> Option<Vec<Address>> {
    let count = *body.get(44)? as usize;
    if count > 2 {
        return None;
    }
    let mut peers = Vec::with_capacity(count);
    for slot in 0..2 {
        let bytes: [u8; 6] = body.get(45 + slot * 6..51 + slot * 6)?.try_into().ok()?;
        if slot < count {
            peers.push(Address::new(bytes)?);
        } else if bytes != [0; 6] {
            return None;
        }
    }
    Some(peers)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(byte: u8) -> Address {
        Address::new([byte, 0, 0, 0, 0, 1]).unwrap()
    }

    fn sample(pending: bool, idle: bool) -> Snapshot {
        Snapshot {
            reporter: addr(1),
            session: [9; 16],
            sequence: 4,
            sent: UnixMs(1_700_000_000_000),
            accessory: addr(3),
            peers: if pending { vec![addr(1), addr(2)] } else { vec![addr(1)] },
            audio_source: if pending { None } else { Some(addr(1)) },
            last_use: UnixMs(1_700_000_000_000),
            pending,
            audio_idle: idle && !pending,
        }
    }

    #[test]
    fn a_roster_round_trips_and_the_wrong_key_or_a_changed_byte_does_not() {
        let keys = FleetKeys { id: [3; 16], key: [0x11; 32] };
        let other = FleetKeys { id: [3; 16], key: [0x22; 32] };
        let frame = advertise(&sample(false, true), &keys).unwrap();
        assert_eq!(frame[0], FLAGGED);
        assert_eq!(frame.len(), WITH_FLAG);
        let got = read(&frame, &keys).unwrap();
        assert_eq!(got.reporter, addr(1));
        assert_eq!(got.sequence, 4);
        assert!(got.audio_idle);
        assert!(!got.pending);
        assert!(read(&frame, &other).is_none());
        let mut bad = frame.clone();
        let last = bad.len() - 1;
        bad[last] ^= 1;
        assert!(read(&bad, &keys).is_none());

        let pending = advertise(&sample(true, false), &keys).unwrap();
        assert_eq!(pending[0], FLAGGED);
        let got = read(&pending, &keys).unwrap();
        assert!(got.pending);
        assert_eq!(got.peers.len(), 2);
    }

    #[test]
    fn an_unflagged_roster_is_version_7_and_a_lie_does_not_encode() {
        let keys = FleetKeys { id: [3; 16], key: [0x11; 32] };
        let mut plain = sample(false, false);
        plain.audio_source = None;
        plain.audio_idle = false;
        let frame = advertise(&plain, &keys).unwrap();
        assert_eq!(frame[0], VERSION);
        assert_eq!(frame.len(), PLAIN);

        plain.sequence = 0;
        assert!(advertise(&plain, &keys).is_none());
        plain.sequence = 4;
        plain.peers = vec![addr(2)];
        assert!(advertise(&plain, &keys).is_none());
    }
}
