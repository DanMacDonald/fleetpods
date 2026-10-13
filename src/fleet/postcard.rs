//! Bytes on the fleet radio. `Letter` stays inside this codec.
//!
//! A frame is `pf1\n` plus one JSON object. Garbage, a status document, and a
//! truncated SPAKE2 fragment all fail the decode and change nothing. This is
//! the in-process frame the test world forwards. It is not a captured C++
//! advertisement.

use serde_json::{json, Value};

use crate::types::{Address, FleetId, HostId, InvitationId, MonoMs, SessionId, UnixMs};

use super::letter::Letter;
use super::members::{GroupKey, Member, Proposal};
use super::roster::RosterReport;

const PREFIX: &[u8] = b"pf1\n";

pub enum Postcard {
    Letter { from: Address, to: Address, body: Letter },
    Announce { from: Address, name: String, seated: bool, roster: Option<RosterReport> },
}

pub fn encode_letter(from: Address, to: Address, body: &Letter) -> Vec<u8> {
    frame(json!({
        "k": "letter",
        "from": from.display(),
        "to": to.display(),
        "body": letter_json(body),
    }))
}

pub fn encode_announce(
    from: Address,
    name: &str,
    seated: bool,
    roster: Option<&RosterReport>,
) -> Vec<u8> {
    frame(json!({
        "k": "announce",
        "from": from.display(),
        "name": name,
        "seated": seated,
        "roster": roster.map(roster_json),
    }))
}

pub fn decode(bytes: &[u8]) -> Option<Postcard> {
    let json = bytes.strip_prefix(PREFIX)?;
    let value: Value = serde_json::from_slice(json).ok()?;
    match value.get("k")?.as_str()? {
        "letter" => Some(Postcard::Letter {
            from: address(&value, "from")?,
            to: address(&value, "to")?,
            body: decode_letter(value.get("body")?)?,
        }),
        "announce" => Some(Postcard::Announce {
            from: address(&value, "from")?,
            name: value.get("name")?.as_str()?.to_string(),
            seated: value.get("seated")?.as_bool()?,
            roster: match value.get("roster") {
                None | Some(Value::Null) => None,
                Some(roster) => Some(decode_roster(roster)?),
            },
        }),
        _ => None,
    }
}

fn frame(value: Value) -> Vec<u8> {
    let mut out = PREFIX.to_vec();
    if let Ok(body) = serde_json::to_vec(&value) {
        out.extend(body);
    }
    out
}

fn letter_json(body: &Letter) -> Value {
    match body {
        Letter::JoinRequest { id, name } => {
            json!({"t": "join_request", "id": id.hex(), "name": name})
        }
        Letter::Hello { id, name } => json!({"t": "hello", "id": id.hex(), "name": name}),
        Letter::Ack { id } => json!({"t": "ack", "id": id.hex()}),
        Letter::Offer { id, attempt, public } => json!({
            "t": "offer",
            "id": id.hex(),
            "attempt": attempt,
            "public": hex(public),
        }),
        Letter::Response { id, attempt, public, confirm } => json!({
            "t": "response",
            "id": id.hex(),
            "attempt": attempt,
            "public": hex(public),
            "confirm": hex(confirm),
        }),
        Letter::Grant { id, confirm, proposal } => json!({
            "t": "grant",
            "id": id.hex(),
            "confirm": hex(confirm),
            "proposal": {
                "id": proposal.id.hex(),
                "key": hex(proposal.key.as_bytes()),
                "members": proposal.members.iter().map(|member| json!({
                    "address": member.address.display(),
                    "label": member.label,
                })).collect::<Vec<_>>(),
            },
        }),
        Letter::Saved { id, .. } => json!({"t": "saved", "id": id.hex()}),
        Letter::Commit { id } => json!({"t": "commit", "id": id.hex()}),
        Letter::Done { id } => json!({"t": "done", "id": id.hex()}),
        Letter::Cancel { id } => json!({"t": "cancel", "id": id.hex()}),
        Letter::Rejected { id, attempt } => {
            json!({"t": "rejected", "id": id.hex(), "attempt": attempt})
        }
    }
}

fn decode_letter(body: &Value) -> Option<Letter> {
    let id = InvitationId::parse_hex(body.get("id")?.as_str()?)?;
    match body.get("t")?.as_str()? {
        "join_request" => {
            Some(Letter::JoinRequest { id, name: body.get("name")?.as_str()?.to_string() })
        }
        "hello" => Some(Letter::Hello { id, name: body.get("name")?.as_str()?.to_string() }),
        "ack" => Some(Letter::Ack { id }),
        "offer" => Some(Letter::Offer {
            id,
            attempt: small(body.get("attempt")?)?,
            public: unhex(body.get("public")?.as_str()?, 1)?,
        }),
        "response" => Some(Letter::Response {
            id,
            attempt: small(body.get("attempt")?)?,
            public: unhex(body.get("public")?.as_str()?, 1)?,
            confirm: fixed(body.get("confirm")?.as_str()?)?,
        }),
        "grant" => Some(Letter::Grant {
            id,
            confirm: fixed(body.get("confirm")?.as_str()?)?,
            proposal: decode_proposal(body.get("proposal")?)?,
        }),
        "saved" => Some(Letter::Saved { id, members: Vec::new() }),
        "commit" => Some(Letter::Commit { id }),
        "done" => Some(Letter::Done { id }),
        "cancel" => Some(Letter::Cancel { id }),
        "rejected" => Some(Letter::Rejected { id, attempt: small(body.get("attempt")?)? }),
        _ => None,
    }
}

fn decode_proposal(value: &Value) -> Option<Proposal> {
    let mut members = Vec::new();
    for item in value.get("members")?.as_array()? {
        members.push(Member {
            address: Address::parse(item.get("address")?.as_str()?)?,
            label: item.get("label")?.as_str()?.to_string(),
        });
    }
    Some(Proposal {
        id: FleetId::parse_hex(value.get("id")?.as_str()?)?,
        key: GroupKey::new(fixed(value.get("key")?.as_str()?)?),
        members,
    })
}

fn roster_json(report: &RosterReport) -> Value {
    json!({
        "reporter": hex(&report.reporter.bytes()),
        "reporter_address": report.reporter_address.display(),
        "accessory": report.accessory.display(),
        "session": report.session.0,
        "seq": report.seq,
        "at": report.at.0,
        "sent": report.sent.0,
        "peers": report.peers.iter().map(|peer| peer.display()).collect::<Vec<_>>(),
        "audio_source": report.audio_source.map(|address| address.display()),
        "pending": report.pending,
        "audio_idle": report.audio_idle,
    })
}

fn decode_roster(value: &Value) -> Option<RosterReport> {
    let mut peers = Vec::new();
    for peer in value.get("peers")?.as_array()? {
        peers.push(Address::parse(peer.as_str()?)?);
    }
    let audio_source = match value.get("audio_source") {
        None | Some(Value::Null) => None,
        Some(source) => Some(Address::parse(source.as_str()?)?),
    };
    Some(RosterReport {
        reporter: HostId::new(fixed(value.get("reporter")?.as_str()?)?),
        reporter_address: Address::parse(value.get("reporter_address")?.as_str()?)?,
        accessory: Address::parse(value.get("accessory")?.as_str()?)?,
        session: SessionId(value.get("session")?.as_u64()?),
        seq: value.get("seq")?.as_u64()?,
        at: MonoMs(value.get("at")?.as_u64()?),
        sent: UnixMs(value.get("sent")?.as_u64()?),
        peers,
        audio_source,
        pending: value.get("pending")?.as_bool()?,
        audio_idle: value.get("audio_idle")?.as_bool()?,
        wire_session: None,
        last_use: UnixMs(value.get("last_use").and_then(|item| item.as_u64()).unwrap_or(0)),
    })
}

fn address(value: &Value, key: &str) -> Option<Address> {
    Address::parse(value.get(key)?.as_str()?)
}

fn small(value: &Value) -> Option<u8> {
    u8::try_from(value.as_u64()?).ok()
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

fn unhex(text: &str, min: usize) -> Option<Vec<u8>> {
    if text.len() < min * 2 || !text.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(text.len() / 2);
    let bytes = text.as_bytes();
    for pair in bytes.chunks(2) {
        let hi = from_hex(pair[0])?;
        let lo = from_hex(pair[1])?;
        out.push((hi << 4) | lo);
    }
    Some(out)
}

fn fixed<const N: usize>(text: &str) -> Option<[u8; N]> {
    let bytes = unhex(text, N)?;
    if bytes.len() != N {
        return None;
    }
    let mut out = [0u8; N];
    out.copy_from_slice(&bytes);
    Some(out)
}

fn from_hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gram() -> Address {
        Address::parse("10:00:00:00:00:01").unwrap()
    }

    fn xps() -> Address {
        Address::parse("20:00:00:00:00:01").unwrap()
    }

    #[test]
    fn a_letter_and_an_announcement_round_trip_and_garbage_does_not() {
        let id = InvitationId::new([0xab; 16]);
        let grant = Letter::Grant {
            id,
            confirm: [0x11; 32],
            proposal: Proposal {
                id: FleetId::new([0x22; 16]),
                key: GroupKey::new([0x33; 32]),
                members: vec![
                    Member { address: gram(), label: "gram".into() },
                    Member { address: xps(), label: "xps".into() },
                ],
            },
        };
        let bytes = encode_letter(gram(), xps(), &grant);
        assert!(bytes.starts_with(PREFIX));
        let Postcard::Letter { from, to, body } = decode(&bytes).unwrap() else {
            panic!("grant");
        };
        assert_eq!(from, gram());
        assert_eq!(to, xps());
        match body {
            Letter::Grant { id: got, confirm, proposal } => {
                assert_eq!(got, id);
                assert_eq!(confirm, [0x11; 32]);
                assert_eq!(proposal.key.as_bytes(), &[0x33; 32]);
                assert_eq!(proposal.members.len(), 2);
            }
            other => panic!("{other:?}"),
        }

        let roster = RosterReport {
            reporter: HostId::new({
                let mut bytes = [0; 16];
                bytes[..6].copy_from_slice(&gram().bytes());
                bytes
            }),
            reporter_address: gram(),
            accessory: Address::parse("A0:00:00:00:00:03").unwrap(),
            session: SessionId(1),
            seq: 4,
            at: MonoMs(9),
            sent: UnixMs(1_700_000_000_000),
            peers: vec![gram()],
            audio_source: Some(xps()),
            pending: false,
            audio_idle: false,
            wire_session: None,
            last_use: UnixMs(0),
        };
        let card = encode_announce(gram(), "gram", true, Some(&roster));
        let Postcard::Announce { from, name, seated, roster: got } = decode(&card).unwrap() else {
            panic!("announce");
        };
        assert_eq!((from, name.as_str(), seated), (gram(), "gram", true));
        assert_eq!(got.unwrap().seq, 4);

        assert!(decode(b"{\"schema_version\":1}").is_none());
        assert!(decode(&[0xff, 0x00, 0x01]).is_none());
        assert!(decode(b"pf1\n{").is_none());
        assert!(decode(b"pf1\n{\"k\":\"letter\"}").is_none());
    }
}
