//! Peer identity and playback relayed through the paired accessory's control connection.

use crate::types::Address;

const RELAY: [u8; 6] = [4, 0, 4, 0, 0x11, 0];
const MAX_PACKET: usize = 2048;

#[derive(Debug, PartialEq, Eq)]
pub struct Report {
    pub sender: Address,
    pub name: Option<String>,
    pub playing: Option<bool>,
}

#[cfg(test)]
pub fn name(packet: &[u8]) -> Option<(Address, String)> {
    let report = parse(packet)?;
    Some((report.sender, report.name?))
}

pub fn parse(packet: &[u8]) -> Option<Report> {
    if packet.len() < 16 || packet.len() > MAX_PACKET || !packet.starts_with(&RELAY) {
        return None;
    }
    let mut bytes: [u8; 6] = packet[6..12].try_into().ok()?;
    bytes.reverse();
    if bytes[0] & 1 != 0 {
        return None;
    }
    let sender = Address::new(bytes)?;
    let length = usize::from(u16::from_le_bytes([packet[12], packet[13]]));
    if packet.len() != 14 + length || packet[14] != 1 {
        return None;
    }
    let fields = packet[15].checked_sub(0xe0).filter(|n| *n <= 14)?;
    let mut body = &packet[16..];
    let mut name = None;
    let mut app = None;
    let mut streaming = None;
    let mut keys = std::collections::BTreeSet::new();
    for _ in 0..fields {
        let key = string(&mut body)?;
        if !keys.insert(key) {
            return None;
        }
        match key {
            "btName" => name = Some(crate::devices::valid_label(string(&mut body)?)?.to_owned()),
            "playingApp" => {
                let value = string(&mut body)?;
                if value != "Unknown" {
                    app = Some(!value.is_empty() && value != "NA");
                }
            }
            "hostStreamingState" => {
                streaming = match string(&mut body)? {
                    "YES" => Some(true),
                    "NO" => Some(false),
                    _ => None,
                }
            }
            _ => skip_value(&mut body)?,
        }
    }
    if !body.is_empty() {
        return None;
    }
    // Apple can report a playing app with streaming=NO. The app is authoritative.
    Some(Report { sender, name, playing: app.or(streaming) })
}

fn take<'a>(body: &mut &'a [u8], length: usize) -> Option<&'a [u8]> {
    let (value, rest) = body.split_at_checked(length)?;
    *body = rest;
    Some(value)
}

fn string<'a>(body: &mut &'a [u8]) -> Option<&'a str> {
    let marker = take(body, 1)?[0];
    let length = match marker {
        0x40..=0x60 => usize::from(marker - 0x40),
        0x61 => usize::from(take(body, 1)?[0]),
        0x62 => usize::from(u16::from_le_bytes(take(body, 2)?.try_into().ok()?)),
        _ => return None,
    };
    std::str::from_utf8(take(body, length)?).ok()
}

fn skip_value(body: &mut &[u8]) -> Option<()> {
    let marker = *body.first()?;
    match marker {
        0x40..=0x62 => {
            string(body)?;
        }
        0x01..=0x04 | 0x08..=0x2f => {
            take(body, 1)?;
        }
        0x30..=0x33 => {
            take(body, 1 + (1 << (marker - 0x30)))?;
        }
        _ => return None,
    }
    Some(())
}

fn put_string(body: &mut Vec<u8>, text: &str) {
    let bytes = text.as_bytes();
    if bytes.len() <= 32 {
        body.push(0x40 + bytes.len() as u8);
    } else {
        body.push(0x61);
        body.push(bytes.len() as u8);
    }
    body.extend_from_slice(bytes);
}

fn envelope(target: Address, body: Vec<u8>) -> Vec<u8> {
    let mut packet = vec![4, 0, 4, 0, 0x10, 0];
    packet.extend(target.bytes().into_iter().rev());
    packet.extend((body.len() as u16).to_le_bytes());
    packet.extend(body);
    packet
}

/// Current desktop playback, independently of the Bluetooth source category.
pub fn media_information(local: Address, peer: Address, host: &str, playing: bool) -> Vec<u8> {
    let host: String = host.chars().filter(|c| !c.is_control()).take(48).collect();
    let label = format!("Omarchy - {host}");
    let mut media = vec![1, 0xe5];
    for (key, value) in [
        ("playingApp", if playing { "Omarchy" } else { "NA" }),
        ("hostStreamingState", if playing { "YES" } else { "NO" }),
        ("btAddress", local.display().as_str()),
        ("btName", label.as_str()),
    ] {
        put_string(&mut media, key);
        put_string(&mut media, value);
    }
    put_string(&mut media, "otherDeviceAudioCategory");
    media.extend_from_slice(if playing { &[0x31, 0x2d, 1] } else { &[0x30, 100] });
    envelope(peer, media)
}

/// Introduce this host once when a peer joins. No audio ownership request.
pub fn introduction(local: Address, peer: Address, host: &str, playing: bool) -> Vec<Vec<u8>> {
    if local == peer {
        return Vec::new();
    }
    let host: String = host.chars().filter(|c| !c.is_control()).take(48).collect();
    let label = format!("Omarchy - {host}");
    let mut join = vec![1, 0xe5];
    put_string(&mut join, "idleTime");
    join.push(8);
    put_string(&mut join, "newTipi");
    join.push(1);
    put_string(&mut join, "btAddress");
    put_string(&mut join, &local.display());
    put_string(&mut join, "btName");
    put_string(&mut join, &label);
    put_string(&mut join, "nearbyAudioScore");
    join.push(0x0e);
    vec![media_information(local, peer, &host, playing), envelope(peer, join)]
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn report(sender: Address, label: &str) -> Vec<u8> {
        let mut body = vec![1, 0xe3];
        put_string(&mut body, "newTipi");
        body.push(1);
        put_string(&mut body, "btName");
        put_string(&mut body, label);
        put_string(&mut body, "nearbyAudioScore");
        body.extend([0x31, 0x2d, 1]);
        let mut packet = envelope(sender, body);
        packet[4] = 0x11;
        packet
    }

    #[test]
    fn relay_names_bind_to_the_sender_and_reject_malformed_frames() {
        let sender = Address::parse("F0:2F:4B:14:DE:29").unwrap();
        for label in
            ["MacBook Pro", "Dan’s iPhone", "A MacBook Pro name longer than thirty two bytes"]
        {
            let packet = report(sender, label);
            assert_eq!(name(&packet), Some((sender, label.into())));
            for end in 0..packet.len() {
                assert!(name(&packet[..end]).is_none());
            }
            let mut invalid = packet.clone();
            invalid.push(0);
            assert!(name(&invalid).is_none());
            invalid = packet;
            invalid[15] += 1;
            assert!(name(&invalid).is_none());
        }
        for label in ["", "Unknown", "Apple device", "F0:2F:4B:14:DE:29", "bad\nname"] {
            assert!(name(&report(sender, label)).is_none());
        }
    }

    #[test]
    fn introductions_are_bounded_and_carry_the_local_name() {
        let local = Address::parse("A8:72:4D:0C:7C:EE").unwrap();
        let peer = Address::parse("F0:2F:4B:14:DE:29").unwrap();
        assert!(introduction(local, local, "xps", false).is_empty());
        for mut packet in introduction(local, peer, "xps", false) {
            assert_eq!(packet[4], 0x10);
            packet[4] = 0x11;
            packet[6..12].copy_from_slice(&local.bytes().into_iter().rev().collect::<Vec<_>>());
            assert_eq!(name(&packet), Some((local, "Omarchy - xps".into())));
        }
    }
    #[test]
    fn playback_relays_bind_sender_and_prefer_the_app_over_streaming() {
        let sender = Address::parse("D0:C6:37:2F:D0:9F").unwrap();
        for (app, stream, expected) in
            [("NA", "NO", false), ("com.brave.Browser", "NO", true), ("Unknown", "YES", true)]
        {
            let mut body = vec![1, 0xe2];
            put_string(&mut body, "playingApp");
            put_string(&mut body, app);
            put_string(&mut body, "hostStreamingState");
            put_string(&mut body, stream);
            let mut packet = envelope(sender, body);
            packet[4] = 0x11;
            assert_eq!(
                parse(&packet),
                Some(Report { sender, name: None, playing: Some(expected) })
            );
            for end in 0..packet.len() {
                assert!(parse(&packet[..end]).is_none());
            }
            let mut duplicate = packet.clone();
            duplicate[15] = 0xe3;
            put_string(&mut duplicate, "playingApp");
            put_string(&mut duplicate, "NA");
            let length = (duplicate.len() - 14) as u16;
            duplicate[12..14].copy_from_slice(&length.to_le_bytes());
            assert!(parse(&duplicate).is_none());
        }
    }
}
