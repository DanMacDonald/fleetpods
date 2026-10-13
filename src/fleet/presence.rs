//! Fleet presence.
//!
//! Version 2 is public: a big-endian unix timestamp, the sender address, and a
//! UTF-8 name of at most 64 bytes. Versions 3 and 4 wrap that body, and version
//! 4 also carries a 16-byte routing session before the name. The seal is the
//! shared fleet key.

use zeroize::Zeroize;

use super::crypto::{open_fleet, seal_fleet};
use super::nearby::FRESH_MS;
use super::wire::PRESENCE_DOMAIN;
use crate::types::{valid_name, Address, UnixMs};

pub const VERSION: u8 = 2;
/// Public setup hint; no fleet identity or key is exposed.
pub const SETUP: u8 = 5;
pub const SEALED: u8 = 3;
pub const SEALED_SESSION: u8 = 4;
pub const MAX_NAME: usize = 64;
pub const MAX_ADVERTISEMENT: usize = 123;
pub const SKEW_MS: u64 = 5_000;
pub const REFRESH_MS: u64 = 3_000;
const MAX_BODY: usize = 128;

pub struct FleetKeys {
    pub id: [u8; 16],
    pub key: [u8; 32],
}

impl Drop for FleetKeys {
    fn drop(&mut self) {
        self.key.zeroize();
    }
}

pub struct Sight {
    pub address: Address,
    pub name: String,
    pub sent: UnixMs,
    pub authenticated: bool,
    pub session: Option<[u8; 16]>,
    pub enrolled: Option<bool>,
}

/// One public presence frame, or nothing when the name cannot be sent.
pub fn advertise(address: Address, name: &str, sent: UnixMs) -> Option<Vec<u8>> {
    let body = clear_body(address, name, sent, None)?;
    let mut frame = Vec::with_capacity(1 + body.len());
    frame.push(VERSION);
    frame.extend(body);
    Some(frame)
}

pub fn advertise_setup(
    address: Address,
    name: &str,
    sent: UnixMs,
    enrolled: Option<bool>,
) -> Option<Vec<u8>> {
    let body = clear_body(address, name, sent, None)?;
    let flag = match enrolled {
        None => 0,
        Some(false) => 1,
        Some(true) => 2,
    };
    let mut frame = vec![SETUP, flag];
    frame.extend(body);
    Some(frame)
}

pub fn is_public(data: &[u8]) -> bool {
    matches!(data.first(), Some(&VERSION) | Some(&SETUP))
}

/// Version 3, or version 4 when `session` is present. Nothing without a fleet key.
pub fn advertise_sealed(
    address: Address,
    name: &str,
    sent: UnixMs,
    keys: &FleetKeys,
    session: Option<[u8; 16]>,
) -> Option<Vec<u8>> {
    let body = clear_body(address, name, sent, session)?;
    let sealed = seal_fleet(PRESENCE_DOMAIN, &keys.id, &keys.key, &body)?;
    let mut frame = Vec::with_capacity(1 + sealed.len());
    frame.push(if session.is_some() { SEALED_SESSION } else { SEALED });
    frame.extend(sealed);
    (frame.len() <= MAX_ADVERTISEMENT).then_some(frame)
}

/// A fresh announcement from someone else. Sealed versions open only with `keys`.
pub fn observe(
    data: &[u8],
    local: Option<Address>,
    now: UnixMs,
    keys: Option<&FleetKeys>,
) -> Option<Sight> {
    if data.is_empty() || data.len() > MAX_ADVERTISEMENT {
        return None;
    }
    let version = data[0];
    if version != VERSION && version != SEALED && version != SEALED_SESSION && version != SETUP {
        return None;
    }
    let authenticated = !is_public(data);
    let enrolled = if version == SETUP {
        match *data.get(1)? {
            0 => None,
            1 => Some(false),
            2 => Some(true),
            _ => return None,
        }
    } else if authenticated {
        Some(true)
    } else {
        None
    };
    let body = if authenticated {
        let keys = keys?;
        open_fleet(PRESENCE_DOMAIN, &keys.id, &keys.key, &data[1..])?
    } else {
        data[if version == SETUP { 2 } else { 1 }..].to_vec()
    };
    let name_at = if version == SEALED_SESSION { 30 } else { 14 };
    if body.len() < name_at + 1 || body.len() > name_at + MAX_NAME {
        return None;
    }
    let sent = UnixMs(u64::from_be_bytes(body[..8].try_into().ok()?));
    if sent.0 > now.0.saturating_add(SKEW_MS) || sent.0.saturating_add(FRESH_MS) < now.0 {
        return None;
    }
    let address = Address::new(body[8..14].try_into().ok()?)?;
    if local == Some(address) {
        return None;
    }
    let session = (version == SEALED_SESSION).then(|| body[14..30].try_into().ok()).flatten();
    let name_bytes = &body[name_at..];
    let name = std::str::from_utf8(name_bytes).ok()?;
    if !valid_name(name) {
        return None;
    }
    Some(Sight { address, name: name.to_string(), sent, authenticated, session, enrolled })
}

fn clear_body(
    address: Address,
    name: &str,
    sent: UnixMs,
    session: Option<[u8; 16]>,
) -> Option<Vec<u8>> {
    let encoded = name.as_bytes();
    if !valid_name(name) || encoded.is_empty() || encoded.len() > MAX_NAME {
        return None;
    }
    let mut body = Vec::with_capacity(30 + encoded.len());
    body.extend_from_slice(&sent.0.to_be_bytes());
    body.extend_from_slice(&address.bytes());
    if let Some(session) = session {
        body.extend_from_slice(&session);
    }
    body.extend_from_slice(encoded);
    (body.len() <= MAX_BODY).then_some(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(byte: u8) -> Address {
        Address::new([byte, 0, 0, 0, 0, 1]).unwrap()
    }

    #[test]
    fn public_setup_hint_is_optional_and_cannot_authenticate_membership() {
        let now = UnixMs(1000);
        for enrolled in [None, Some(false), Some(true)] {
            let bytes = advertise_setup(addr(2), "gram", now, enrolled).unwrap();
            let sight = observe(&bytes, Some(addr(1)), now, None).unwrap();
            assert_eq!(sight.enrolled, enrolled);
            assert!(!sight.authenticated);
            assert!(sight.session.is_none());
            assert_eq!(sight.name, "gram");
            assert!(observe(&bytes, Some(addr(1)), UnixMs(32000), None).is_none());
        }
        let legacy = advertise(addr(2), "old-host", now).unwrap();
        assert!(observe(&legacy, None, now, None).unwrap().enrolled.is_none());
        assert!(observe(&[SETUP], None, now, None).is_none());
        let mut invalid = advertise_setup(addr(2), "gram", now, Some(true)).unwrap();
        invalid[1] = 255;
        assert!(observe(&invalid, None, now, None).is_none());
    }

    #[test]
    fn a_public_frame_round_trips_and_the_wrong_ones_do_not() {
        let now = UnixMs(1_700_000_000_000);
        let frame = advertise(addr(2), "xps", now).unwrap();
        let sight = observe(&frame, Some(addr(1)), now, None).unwrap();
        assert_eq!(sight.address, addr(2));
        assert_eq!(sight.name, "xps");
        assert_eq!(sight.sent, now);
        assert!(!sight.authenticated);
        assert!(observe(&frame, Some(addr(2)), now, None).is_none());
        assert!(observe(&frame, Some(addr(1)), UnixMs(now.0 + FRESH_MS + 1), None).is_none());
        assert!(observe(&frame, Some(addr(1)), UnixMs(now.0 - SKEW_MS - 1), None).is_none());
        assert!(advertise(addr(2), "", now).is_none());
        assert!(advertise(addr(2), &"n".repeat(MAX_NAME + 1), now).is_none());
    }

    #[test]
    fn a_sealed_frame_opens_with_the_fleet_key_and_a_session_round_trips() {
        let now = UnixMs(1_700_000_000_000);
        let keys = FleetKeys { id: [3; 16], key: [0x11; 32] };
        let other = FleetKeys { id: [3; 16], key: [0x22; 32] };
        let frame = advertise_sealed(addr(2), "xps", now, &keys, None).unwrap();
        assert_eq!(frame[0], SEALED);
        let sight = observe(&frame, Some(addr(1)), now, Some(&keys)).unwrap();
        assert!(sight.authenticated);
        assert_eq!(sight.name, "xps");
        assert!(sight.session.is_none());
        assert!(observe(&frame, Some(addr(1)), now, Some(&other)).is_none());
        assert!(observe(&frame, Some(addr(1)), now, None).is_none());
        let mut tampered = frame.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        assert!(observe(&tampered, Some(addr(1)), now, Some(&keys)).is_none());

        let session = [9u8; 16];
        let with_session = advertise_sealed(addr(2), "xps", now, &keys, Some(session)).unwrap();
        assert_eq!(with_session[0], SEALED_SESSION);
        let sight = observe(&with_session, Some(addr(1)), now, Some(&keys)).unwrap();
        assert_eq!(sight.session, Some(session));
        assert_eq!(sight.name, "xps");
    }
}
