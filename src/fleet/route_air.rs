//! Sealed routing advertisement. Version 5 carries one admission message.
//! The retain address is six zero bytes when the sender has not chosen who stays.

use super::crypto::{open_fleet, seal_fleet};
use super::presence::{FleetKeys, SKEW_MS};
use super::wire::ROUTE_DOMAIN;
use crate::types::{Address, UnixMs};

pub const VERSION: u8 = 5;
pub const PACKET_LEN: usize = 111;
pub const LIFETIME_MS: u64 = 30_000;
pub const RETRY_MS: u64 = 1_500;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    Request = 1,
    Admission = 2,
    Connection = 3,
    Complete = 4,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Answer {
    Ok = 0,
    Busy = 1,
    Unavailable = 2,
    RosterChanged = 3,
    WriteFailed = 4,
    TimedOut = 5,
    ConnectionFailed = 6,
    VerifyConnection = 7,
}

impl Answer {
    pub fn text(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Busy => "peer_busy",
            Self::Unavailable => "control_unavailable",
            Self::RosterChanged => "roster_changed",
            Self::WriteFailed => "priority_write_failed",
            Self::TimedOut => "timed_out",
            Self::ConnectionFailed => "connection_failed",
            Self::VerifyConnection => "verify_connection",
        }
    }
}

#[derive(Clone)]
pub struct Message {
    pub kind: Kind,
    pub result: Answer,
    pub source: Address,
    pub target: Address,
    pub source_session: [u8; 16],
    pub target_session: [u8; 16],
    pub sequence: u64,
    pub transaction: [u8; 8],
    pub sent: UnixMs,
    pub accessory: Address,
    pub retain: Option<Address>,
}

pub fn advertise(message: &Message, keys: &FleetKeys) -> Option<Vec<u8>> {
    let body = encode_body(message)?;
    let sealed = seal_fleet(ROUTE_DOMAIN, &keys.id, &keys.key, &body)?;
    let mut frame = Vec::with_capacity(1 + sealed.len());
    frame.push(VERSION);
    frame.extend(sealed);
    (frame.len() == PACKET_LEN).then_some(frame)
}

pub fn read(packet: &[u8], keys: &FleetKeys) -> Option<Message> {
    if packet.len() != PACKET_LEN || packet.first() != Some(&VERSION) {
        return None;
    }
    let body = open_fleet(ROUTE_DOMAIN, &keys.id, &keys.key, &packet[1..])?;
    if body.len() != PACKET_LEN - 29 {
        return None;
    }
    let message = Message {
        kind: kind_at(body.first().copied()?)?,
        result: answer_at(body.get(1).copied()?)?,
        source: address_at(&body, 2)?,
        target: address_at(&body, 8)?,
        source_session: body.get(14..30)?.try_into().ok()?,
        target_session: body.get(30..46)?.try_into().ok()?,
        sequence: u64::from_be_bytes(body.get(46..54)?.try_into().ok()?),
        transaction: body.get(54..62)?.try_into().ok()?,
        sent: UnixMs(u64::from_be_bytes(body.get(62..70)?.try_into().ok()?)),
        accessory: address_at(&body, 70)?,
        retain: optional_address(&body, 76)?,
    };
    valid(&message).then_some(message)
}

pub fn fresh(sent: UnixMs, now: UnixMs) -> bool {
    sent.0 != 0
        && sent.0 <= now.0.saturating_add(SKEW_MS)
        && sent.0.saturating_add(LIFETIME_MS) >= now.0
}

fn encode_body(message: &Message) -> Option<Vec<u8>> {
    if !valid(message) {
        return None;
    }
    let mut body = Vec::with_capacity(82);
    body.push(message.kind as u8);
    body.push(message.result as u8);
    body.extend_from_slice(&message.source.bytes());
    body.extend_from_slice(&message.target.bytes());
    body.extend_from_slice(&message.source_session);
    body.extend_from_slice(&message.target_session);
    body.extend_from_slice(&message.sequence.to_be_bytes());
    body.extend_from_slice(&message.transaction);
    body.extend_from_slice(&message.sent.0.to_be_bytes());
    body.extend_from_slice(&message.accessory.bytes());
    body.extend_from_slice(&message.retain.map(|address| address.bytes()).unwrap_or([0; 6]));
    Some(body)
}

fn valid(message: &Message) -> bool {
    if message.source == message.target
        || message.accessory == message.source
        || message.accessory == message.target
        || message.sequence == 0
        || message.sent.0 == 0
    {
        return false;
    }
    if message.result == Answer::VerifyConnection && message.kind != Kind::Admission {
        return false;
    }
    if message.kind == Kind::Request && message.result != Answer::Ok {
        return false;
    }
    let automatic_ok = message.retain.is_none()
        && (message.kind == Kind::Request
            || (message.kind == Kind::Admission
                && message.result != Answer::Ok
                && message.result != Answer::VerifyConnection));
    if message.retain.is_none() && !automatic_ok {
        return false;
    }
    if let Some(retain) = message.retain {
        if retain == message.accessory {
            return false;
        }
        let blocked = if message.kind == Kind::Request || message.kind == Kind::Connection {
            message.source
        } else {
            message.target
        };
        if retain == blocked {
            return false;
        }
    }
    true
}

fn kind_at(byte: u8) -> Option<Kind> {
    Some(match byte {
        1 => Kind::Request,
        2 => Kind::Admission,
        3 => Kind::Connection,
        4 => Kind::Complete,
        _ => return None,
    })
}

fn answer_at(byte: u8) -> Option<Answer> {
    Some(match byte {
        0 => Answer::Ok,
        1 => Answer::Busy,
        2 => Answer::Unavailable,
        3 => Answer::RosterChanged,
        4 => Answer::WriteFailed,
        5 => Answer::TimedOut,
        6 => Answer::ConnectionFailed,
        7 => Answer::VerifyConnection,
        _ => return None,
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(byte: u8) -> Address {
        Address::new([byte, 0, 0, 0, 0, 1]).unwrap()
    }

    fn request() -> Message {
        Message {
            kind: Kind::Request,
            result: Answer::Ok,
            source: addr(1),
            target: addr(2),
            source_session: [4; 16],
            target_session: [5; 16],
            sequence: 3,
            transaction: 9u64.to_be_bytes(),
            sent: UnixMs(1_700_000_000_000),
            accessory: addr(3),
            retain: None,
        }
    }

    #[test]
    fn a_request_round_trips_and_an_ok_admission_names_who_stays() {
        let keys = FleetKeys { id: [3; 16], key: [0x11; 32] };
        let other = FleetKeys { id: [3; 16], key: [0x22; 32] };
        let frame = advertise(&request(), &keys).unwrap();
        assert_eq!(frame.len(), PACKET_LEN);
        assert_eq!(frame[0], VERSION);
        let got = read(&frame, &keys).unwrap();
        assert_eq!(got.kind, Kind::Request);
        assert_eq!(got.target, addr(2));
        assert!(got.retain.is_none());
        assert!(read(&frame, &other).is_none());
        let mut bad = frame.clone();
        let last = bad.len() - 1;
        bad[last] ^= 1;
        assert!(read(&bad, &keys).is_none());

        let mut admission = request();
        admission.kind = Kind::Admission;
        admission.result = Answer::Ok;
        admission.source = addr(2);
        admission.target = addr(1);
        admission.retain = Some(addr(2));
        let frame = advertise(&admission, &keys).unwrap();
        assert_eq!(read(&frame, &keys).unwrap().retain, Some(addr(2)));
        admission.retain = None;
        assert!(advertise(&admission, &keys).is_none());
    }
}
