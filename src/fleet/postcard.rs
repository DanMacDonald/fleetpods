//! Bytes on the fleet radio.
//!
//! A frame is `pf1\n` plus one JSON object. Garbage, a status document, and a
//! truncated SPAKE2 fragment all fail the decode and change nothing. This is
//! the in-process frame the test world forwards. It is not a captured C++
//! advertisement.
//!
//! Only announcements ride this plaintext frame, and they are nearby hints
//! with no authority. Enrollment letters never do: they travel as `cppmsg`
//! payloads, where the post-authentication letters are sealed with the
//! SPAKE2 session. A plaintext letter channel would let any nearby radio
//! forge `saved`/`commit`/`done`/`grant` from a sniffed invitation id.

use serde_json::{json, Value};

use crate::types::{Address, HostId, MonoMs, SessionId, UnixMs};

use super::roster::RosterReport;

const PREFIX: &[u8] = b"pf1\n";

pub enum Postcard {
    Announce { from: Address, name: String, seated: bool, roster: Option<RosterReport> },
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
    fn an_announcement_round_trips_a_letter_frame_and_garbage_do_not() {
        // A plaintext letter frame must never decode: its grant would carry
        // the fleet key and its done/commit would confirm an enrollment that
        // never proved anything. Even a well-formed one is garbage here.
        let forged = b"pf1\n{\"k\":\"letter\",\"from\":\"10:00:00:00:00:01\",\"to\":\"20:00:00:00:00:01\",\"body\":{\"t\":\"done\",\"id\":\"abababababababababababababababab\"}}";
        assert!(decode(forged).is_none());

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
        let Postcard::Announce { from, name, seated, roster: got } = decode(&card).unwrap();
        assert_eq!((from, name.as_str(), seated), (gram(), "gram", true));
        assert_eq!(got.unwrap().seq, 4);

        assert!(decode(b"{\"schema_version\":1}").is_none());
        assert!(decode(&[0xff, 0x00, 0x01]).is_none());
        assert!(decode(b"pf1\n{").is_none());
        assert!(decode(b"pf1\n{\"k\":\"letter\"}").is_none());
    }
}
