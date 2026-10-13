//! Enrollment JSON the previous daemon puts inside a version-1 fragment.
//!
//! The fragment header is unchanged. The payload is one compact object with
//! `type` and `id`, not a `pf1` postcard. Sealed fields use the enrollment
//! session cipher: a fresh nonce, then ChaCha20-Poly1305.

use serde_json::{json, Value};

use super::crypto::Pake;
use super::letter::Letter;
use super::members::{GroupKey, Member, Proposal};
use crate::types::{Address, FleetId, InvitationId};

pub fn encode(
    from: Address,
    to: Address,
    local_name: &str,
    letter: &Letter,
    pake: Option<&Pake>,
) -> Option<Vec<u8>> {
    let value = match letter {
        Letter::JoinRequest { id, name } => {
            json!({"type": "join_request", "id": id.hex(), "name": name})
        }
        Letter::Hello { id, name } => json!({
            "address": from.display(),
            "id": id.hex(),
            "name": name,
            "target": to.display(),
            "type": "invite",
            "version": 1,
        }),
        Letter::Ack { id } => json!({
            "id": id.hex(),
            "name": local_name,
            "type": "ready",
        }),
        Letter::Offer { id, attempt, public } => json!({
            "attempt": attempt,
            "id": id.hex(),
            "public": b64(public),
            "type": "offer",
        }),
        Letter::Response { id, attempt, public, confirm } => json!({
            "attempt": attempt,
            "confirm": b64(confirm),
            "id": id.hex(),
            "public": b64(public),
            "type": "response",
        }),
        Letter::Grant { id, confirm, proposal } => {
            let sealed = pake?.seal(&fleet_bytes(proposal)?)?;
            json!({
                "confirm": b64(confirm),
                "id": id.hex(),
                "sealed": b64(&sealed),
                "type": "grant",
            })
        }
        Letter::Saved { id, members } => {
            let pake = pake?;
            let stored = pake.seal(b"stored")?;
            let listed = pake.seal(&serde_json::to_vec(&member_json(members)).ok()?)?;
            json!({
                "id": id.hex(),
                "members_sealed": b64(&listed),
                "sealed": b64(&stored),
                "type": "saved",
            })
        }
        Letter::Commit { id } => json!({
            "id": id.hex(),
            "sealed": b64(&pake?.seal(b"commit")?),
            "type": "commit",
        }),
        Letter::Done { id } => json!({
            "id": id.hex(),
            "sealed": b64(&pake?.seal(b"done")?),
            "type": "done",
        }),
        Letter::Cancel { id } => json!({
            "id": id.hex(),
            "type": "cancel",
        }),
        Letter::Rejected { id, attempt } => json!({
            "attempt": attempt,
            "id": id.hex(),
            "type": "code_rejected",
        }),
    };
    serde_json::to_vec(&value).ok()
}

/// The sender's name from an invitation or acknowledgement, when present.
/// Other letters do not carry one this daemon needs.
pub fn sender_name(payload: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(payload).ok()?;
    let name = value.get("name")?.as_str()?;
    crate::types::valid_name(name).then(|| name.to_string())
}

pub fn decode(payload: &[u8], pake: Option<&Pake>) -> Option<Letter> {
    let value: Value = serde_json::from_slice(payload).ok()?;
    let id = InvitationId::parse_hex(value.get("id")?.as_str()?)?;
    match value.get("type")?.as_str()? {
        "join_request" => {
            let name = value.get("name")?.as_str()?;
            crate::types::valid_name(name).then(|| Letter::JoinRequest { id, name: name.into() })
        }
        "invite" => {
            if value.get("version")?.as_i64()? != 1 {
                return None;
            }
            Some(Letter::Hello { id, name: value.get("name")?.as_str()?.to_string() })
        }
        "ready" => Some(Letter::Ack { id }),
        "offer" => Some(Letter::Offer {
            id,
            attempt: attempt(&value),
            public: unb64(value.get("public")?.as_str()?)?,
        }),
        "response" => Some(Letter::Response {
            id,
            attempt: attempt(&value),
            public: unb64(value.get("public")?.as_str()?)?,
            confirm: fixed(&unb64(value.get("confirm")?.as_str()?)?)?,
        }),
        "grant" => {
            let clear = pake?.open(&unb64(value.get("sealed")?.as_str()?)?)?;
            Some(Letter::Grant {
                id,
                confirm: fixed(&unb64(value.get("confirm")?.as_str()?)?)?,
                proposal: proposal(&clear)?,
            })
        }
        "saved" => {
            let stored = pake?.open(&unb64(value.get("sealed")?.as_str()?)?)?;
            if stored != b"stored" {
                return None;
            }
            let members = value
                .get("members_sealed")
                .and_then(Value::as_str)
                .and_then(unb64)
                .and_then(|bytes| pake?.open(&bytes))
                .and_then(|clear| members_of(&clear))
                .unwrap_or_default();
            Some(Letter::Saved { id, members })
        }
        "commit" => opened(pake, &value, b"commit").map(|()| Letter::Commit { id }),
        "done" => opened(pake, &value, b"done").map(|()| Letter::Done { id }),
        "cancel" => Some(Letter::Cancel { id }),
        "code_rejected" => Some(Letter::Rejected { id, attempt: attempt(&value) }),
        _ => None,
    }
}

fn opened(pake: Option<&Pake>, value: &Value, expect: &[u8]) -> Option<()> {
    let clear = pake?.open(&unb64(value.get("sealed")?.as_str()?)?)?;
    (clear == expect).then_some(())
}

fn attempt(value: &Value) -> u8 {
    value.get("attempt").and_then(Value::as_u64).and_then(|n| u8::try_from(n).ok()).unwrap_or(0)
}

fn fleet_bytes(proposal: &Proposal) -> Option<Vec<u8>> {
    serde_json::to_vec(&json!({
        "id": proposal.id.hex(),
        "key": b64(proposal.key.as_bytes()),
        "members": member_json(&proposal.members),
    }))
    .ok()
}

fn member_json(members: &[Member]) -> Vec<Value> {
    members
        .iter()
        .map(|member| json!({"address": member.address.display(), "label": member.label}))
        .collect()
}

fn proposal(bytes: &[u8]) -> Option<Proposal> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    Some(Proposal {
        id: FleetId::parse_hex(value.get("id")?.as_str()?)?,
        key: GroupKey::new(fixed(&unb64(value.get("key")?.as_str()?)?)?),
        members: members_of(&serde_json::to_vec(value.get("members")?).ok()?)?,
    })
}

fn members_of(bytes: &[u8]) -> Option<Vec<Member>> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    let mut members = Vec::new();
    for item in value.as_array()? {
        members.push(Member {
            address: Address::parse(item.get("address")?.as_str()?)?,
            label: item.get("label")?.as_str()?.to_string(),
        });
    }
    Some(members)
}

fn fixed<const N: usize>(bytes: &[u8]) -> Option<[u8; N]> {
    bytes.try_into().ok()
}

fn b64(bytes: &[u8]) -> String {
    const DIGITS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    let mut index = 0;
    while index + 3 <= bytes.len() {
        let n = u32::from_be_bytes([0, bytes[index], bytes[index + 1], bytes[index + 2]]);
        for shift in [18, 12, 6, 0] {
            out.push(DIGITS[((n >> shift) & 0x3f) as usize] as char);
        }
        index += 3;
    }
    if index < bytes.len() {
        let rest = &bytes[index..];
        let n = match rest.len() {
            1 => u32::from(rest[0]) << 16,
            _ => (u32::from(rest[0]) << 16) | (u32::from(rest[1]) << 8),
        };
        out.push(DIGITS[((n >> 18) & 0x3f) as usize] as char);
        out.push(DIGITS[((n >> 12) & 0x3f) as usize] as char);
        if rest.len() == 1 {
            out.push('=');
        } else {
            out.push(DIGITS[((n >> 6) & 0x3f) as usize] as char);
        }
        out.push('=');
    }
    out
}

fn unb64(text: &str) -> Option<Vec<u8>> {
    fn val(byte: u8) -> Option<u8> {
        match byte {
            b'A'..=b'Z' => Some(byte - b'A'),
            b'a'..=b'z' => Some(byte - b'a' + 26),
            b'0'..=b'9' => Some(byte - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            b'=' => Some(0),
            _ => None,
        }
    }
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::new();
    for chunk in bytes.chunks(4) {
        let (a, b, c, d) = (val(chunk[0])?, val(chunk[1])?, val(chunk[2])?, val(chunk[3])?);
        let n = (u32::from(a) << 18) | (u32::from(b) << 12) | (u32::from(c) << 6) | u32::from(d);
        out.push((n >> 16) as u8);
        if chunk[2] != b'=' {
            out.push((n >> 8) as u8);
        }
        if chunk[3] != b'=' {
            out.push(n as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(text: &str) -> Address {
        Address::parse(text).unwrap()
    }

    #[test]
    fn an_invite_is_the_previous_daemons_json() {
        let id = InvitationId::new([1; 16]);
        let bytes = encode(
            addr("A8:72:4D:0C:7C:EE"),
            addr("D0:C6:37:2F:D0:9F"),
            "omarchy-xps",
            &Letter::Hello { id, name: "omarchy-xps".into() },
            None,
        )
        .unwrap();
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(text.contains("\"type\":\"invite\""));
        assert!(text.contains("\"version\":1"));
        assert!(!text.starts_with("pf1"));
        match decode(&bytes, None).unwrap() {
            Letter::Hello { name, .. } => assert_eq!(name, "omarchy-xps"),
            other => panic!("invite decoded as {other:?}"),
        }
    }

    #[test]
    fn a_ready_without_an_attempt_is_an_ack() {
        let id = InvitationId::new([2; 16]);
        let packet = format!("{{\"id\":\"{}\",\"name\":\"lg-gram\",\"type\":\"ready\"}}", id.hex());
        assert!(matches!(decode(packet.as_bytes(), None), Some(Letter::Ack { .. })));
    }
}
