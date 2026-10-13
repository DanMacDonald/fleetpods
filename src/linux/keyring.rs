//! The fleet record in the Secret Service.
//!
//! Lookup attributes live in [`crate::fleet::wire`]. The secret is compact JSON:
//! `id` is 32 hex characters, `key` is standard base64, `members` are address
//! and label. A missing item is not a locked keyring.

use std::collections::HashMap;
use std::time::Duration;

use dbus::arg::{RefArg, Variant};
use dbus::blocking::Connection;
use dbus::Path;
use serde_json::{json, Value};

use crate::fleet::members::{Fleet, GroupKey, Member};
use crate::fleet::wire::{KEYRING_APP, KEYRING_SCHEMA};
use crate::types::{Address, FleetId};
const ITEM: &str = "fleet-v1";
pub const IRK_ITEM: &str = "magic-acc-irk";
pub const ENC_ITEM: &str = "magic-acc-enc-key";

pub enum Load {
    Missing,
    Open(Fleet),
    Locked,
}

pub fn encode(fleet: &Fleet) -> Vec<u8> {
    let members: Vec<_> = fleet
        .members
        .iter()
        .map(|member| json!({"address": member.address.display(), "label": member.label}))
        .collect();
    serde_json::to_vec(&json!({
        "id": fleet.id.hex(),
        "key": base64_encode(fleet.key.as_bytes()),
        "members": members,
    }))
    .unwrap_or_default()
}

pub fn decode(bytes: &[u8]) -> Option<Fleet> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    let id = FleetId::parse_hex(value.get("id")?.as_str()?)?;
    let key_bytes = base64_decode(value.get("key")?.as_str()?)?;
    if key_bytes.len() != 32 {
        return None;
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&key_bytes);
    let mut members = Vec::new();
    for member in value.get("members")?.as_array()? {
        if members.len() == 32 {
            return None;
        }
        members.push(Member {
            address: Address::parse(member.get("address")?.as_str()?)?,
            label: member.get("label")?.as_str()?.to_string(),
        });
    }
    Some(Fleet { id, key: GroupKey::new(key), members })
}

type Secret = (Path<'static>, Vec<u8>, Vec<u8>, String);

pub fn load() -> Result<Load, String> {
    let bus = Connection::new_session().map_err(|err| err.to_string())?;
    let session = open_session(&bus)?;
    let (unlocked, locked) = search(&bus, ITEM)?;
    if unlocked.is_empty() {
        return Ok(if locked.is_empty() { Load::Missing } else { Load::Locked });
    }
    let secret = get_secret(&bus, &unlocked[0], &session)?;
    if secret.is_empty() {
        Ok(Load::Missing)
    } else {
        decode(&secret).map(Load::Open).ok_or_else(|| "fleet keyring item is invalid".into())
    }
}

pub fn store(fleet: &Fleet) -> Result<(), String> {
    let bus = Connection::new_session().map_err(|err| err.to_string())?;
    let session = open_session(&bus)?;
    let collection = bus.with_proxy(
        "org.freedesktop.secrets",
        "/org/freedesktop/secrets/aliases/default",
        Duration::from_secs(5),
    );
    let secret: Secret = (session, Vec::new(), encode(fleet), "application/octet-stream".into());
    let (item, prompt): (Path<'static>, Path<'static>) = collection
        .method_call(
            "org.freedesktop.Secret.Collection",
            "CreateItem",
            (properties(ITEM), secret, true),
        )
        .map_err(|err| err.to_string())?;
    if item.as_str() == Some("/") && prompt.as_str() != Some("/") {
        return Err("keyring is locked".into());
    }
    Ok(())
}

/// Delete only the fleet record, without opening a secret session or reading
/// any secret. The caller must serialize this with all enrollment writes.
pub fn clear_fleet() -> Result<(), String> {
    let bus = Connection::new_session().map_err(|err| err.to_string())?;
    let (unlocked, locked) = search(&bus, ITEM)?;
    if !locked.is_empty() {
        return Err("fleet keyring item is locked; unlock the keyring and retry".into());
    }
    for item in unlocked {
        let proxy = bus.with_proxy("org.freedesktop.secrets", item, Duration::from_secs(5));
        let (prompt,): (Path<'static>,) = proxy
            .method_call("org.freedesktop.Secret.Item", "Delete", ())
            .map_err(|err| err.to_string())?;
        if prompt.as_str() != Some("/") {
            return Err("fleet deletion requires keyring confirmation; unlock and retry".into());
        }
    }
    let (unlocked, locked) = search(&bus, ITEM)?;
    if !unlocked.is_empty() || !locked.is_empty() {
        return Err("fleet keyring item still exists; retry the reset".into());
    }
    Ok(())
}

fn open_session(bus: &Connection) -> Result<Path<'static>, String> {
    let proxy = bus.with_proxy(
        "org.freedesktop.secrets",
        "/org/freedesktop/secrets",
        Duration::from_secs(5),
    );
    let input: Variant<String> = Variant(String::new());
    let (_output, session): (Variant<Box<dyn RefArg>>, Path<'static>) = proxy
        .method_call("org.freedesktop.Secret.Service", "OpenSession", ("plain", input))
        .map_err(|err| err.to_string())?;
    Ok(session)
}

pub fn load_secret(name: &str) -> Result<Option<Vec<u8>>, String> {
    let bus = Connection::new_session().map_err(|err| err.to_string())?;
    let session = open_session(&bus)?;
    let (unlocked, locked) = search(&bus, name)?;
    if unlocked.is_empty() {
        return if locked.is_empty() { Ok(None) } else { Err("keyring is locked".into()) };
    }
    let secret = get_secret(&bus, &unlocked[0], &session)?;
    Ok(if secret.is_empty() { None } else { Some(secret) })
}

pub fn store_secret(name: &str, bytes: &[u8]) -> Result<(), String> {
    let bus = Connection::new_session().map_err(|err| err.to_string())?;
    let session = open_session(&bus)?;
    let collection = bus.with_proxy(
        "org.freedesktop.secrets",
        "/org/freedesktop/secrets/aliases/default",
        Duration::from_secs(5),
    );
    let secret: Secret = (session, Vec::new(), bytes.to_vec(), "application/octet-stream".into());
    let (item, prompt): (Path<'static>, Path<'static>) = collection
        .method_call(
            "org.freedesktop.Secret.Collection",
            "CreateItem",
            (properties(name), secret, true),
        )
        .map_err(|err| err.to_string())?;
    if item.as_str() == Some("/") && prompt.as_str() != Some("/") {
        return Err("keyring is locked".into());
    }
    Ok(())
}

fn search(
    bus: &Connection,
    name: &str,
) -> Result<(Vec<Path<'static>>, Vec<Path<'static>>), String> {
    let proxy = bus.with_proxy(
        "org.freedesktop.secrets",
        "/org/freedesktop/secrets",
        Duration::from_secs(5),
    );
    proxy
        .method_call("org.freedesktop.Secret.Service", "SearchItems", (attributes(name),))
        .map_err(|err| err.to_string())
}

fn get_secret(
    bus: &Connection,
    item: &Path<'static>,
    session: &Path<'static>,
) -> Result<Vec<u8>, String> {
    let proxy = bus.with_proxy("org.freedesktop.secrets", item, Duration::from_secs(5));
    // GetSecret returns one struct, not four separate arguments.
    let (secret,): (Secret,) = proxy
        .method_call("org.freedesktop.Secret.Item", "GetSecret", (session,))
        .map_err(|err| err.to_string())?;
    Ok(secret.2)
}

fn attributes(name: &str) -> HashMap<String, String> {
    HashMap::from([
        ("application".into(), KEYRING_APP.into()),
        ("name".into(), name.to_string()),
        ("xdg:schema".into(), KEYRING_SCHEMA.into()),
    ])
}

fn properties(name: &str) -> HashMap<String, Variant<Box<dyn RefArg>>> {
    let mut properties = HashMap::new();
    properties.insert(
        "org.freedesktop.Secret.Item.Label".into(),
        Variant(Box::new(format!("FleetPods {name}")) as Box<dyn RefArg>),
    );
    properties.insert(
        "org.freedesktop.Secret.Item.Attributes".into(),
        Variant(Box::new(attributes(name)) as Box<dyn RefArg>),
    );
    properties
}

fn base64_encode(bytes: &[u8]) -> String {
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

fn base64_decode(text: &str) -> Option<Vec<u8>> {
    fn val(byte: u8) -> Option<u8> {
        match byte {
            b'A'..=b'Z' => Some(byte - b'A'),
            b'a'..=b'z' => Some(byte - b'a' + 26),
            b'0'..=b'9' => Some(byte - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::new();
    for chunk in bytes.chunks(4) {
        let pad = chunk.iter().filter(|byte| **byte == b'=').count();
        let a = val(chunk[0])?;
        let b = val(chunk[1])?;
        let c = if chunk[2] == b'=' { 0 } else { val(chunk[2])? };
        let d = if chunk[3] == b'=' { 0 } else { val(chunk[3])? };
        let n = (u32::from(a) << 18) | (u32::from(b) << 12) | (u32::from(c) << 6) | u32::from(d);
        out.push((n >> 16) as u8);
        if pad < 2 {
            out.push((n >> 8) as u8);
        }
        if pad < 1 {
            out.push(n as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fleet_deletion_attributes_cannot_match_accessory_keys() {
        let fleet = attributes(ITEM);
        assert_eq!(fleet.get("application").map(String::as_str), Some(KEYRING_APP));
        assert_eq!(fleet.get("name").map(String::as_str), Some("fleet-v1"));
        assert_eq!(fleet.get("xdg:schema").map(String::as_str), Some(KEYRING_SCHEMA));
        for name in [IRK_ITEM, ENC_ITEM] {
            let keys = attributes(name);
            assert!(!fleet.iter().all(|(key, value)| keys.get(key) == Some(value)));
        }
    }

    #[test]
    fn the_fleet_record_round_trips_without_the_secret_service() {
        let fleet = Fleet {
            id: FleetId::new([0xab; 16]),
            key: GroupKey::new([0x11; 32]),
            members: vec![Member {
                address: Address::parse("10:00:00:00:00:01").unwrap(),
                label: "gram".into(),
            }],
        };
        let bytes = encode(&fleet);
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(text.contains("\"id\":\"abababababababababababababababab\""));
        assert!(!text.contains("111111"));
        assert_eq!(decode(&bytes).unwrap().members[0].label, "gram");
        assert!(decode(br#"{"id":"zz","key":"","members":[]}"#).is_none());
    }
}
