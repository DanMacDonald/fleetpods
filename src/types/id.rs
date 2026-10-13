//! Identifiers that must not be substituted for one another.
//! A host is not an accessory, an operation is not a session, and a
//! connection generation is not an enrollment attempt.

use std::fmt;

/// One local connection attempt. Completions for an older id are ignored.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OperationId(pub u64);

/// Bumped when a connection is superseded. In-flight callbacks carry the old value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Generation(pub u64);

/// Routing or presence session. Restarting the daemon mints a new one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionId(pub u64);

/// Stable host identity. Independent of the current LE address.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HostId([u8; 16]);

impl HostId {
    pub fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    pub fn bytes(self) -> [u8; 16] {
        self.0
    }
}

impl fmt::Debug for HostId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HostId({})", hex16(&self.0))
    }
}

/// 16-byte fleet identifier. Hex on the wire is 32 lowercase characters.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct FleetId([u8; 16]);

impl FleetId {
    pub fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    pub fn parse_hex(text: &str) -> Option<Self> {
        parse_hex16(text).map(Self)
    }

    pub fn hex(self) -> String {
        hex16(&self.0)
    }

    pub fn bytes(self) -> [u8; 16] {
        self.0
    }
}

impl fmt::Debug for FleetId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FleetId({})", self.hex())
    }
}

/// Enrollment invitation. 16 bytes, 32 hex characters on the wire.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct InvitationId([u8; 16]);

impl InvitationId {
    pub fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    pub fn parse_hex(text: &str) -> Option<Self> {
        parse_hex16(text).map(Self)
    }

    pub fn hex(self) -> String {
        hex16(&self.0)
    }
}

impl fmt::Debug for InvitationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "InvitationId({})", self.hex())
    }
}

pub fn parse_hex16(text: &str) -> Option<[u8; 16]> {
    if text.len() != 32 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; 16];
    for (slot, pair) in out.iter_mut().zip(text.as_bytes().chunks(2)) {
        let hi = from_hex(pair[0])?;
        let lo = from_hex(pair[1])?;
        *slot = (hi << 4) | lo;
    }
    Some(out)
}

pub fn hex16(bytes: &[u8; 16]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(32);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
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

    #[test]
    fn invitation_hex_round_trips() {
        let id = InvitationId::new([0x0a, 0x1b, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff]);
        assert_eq!(InvitationId::parse_hex(&id.hex()), Some(id));
        assert!(InvitationId::parse_hex("abc").is_none());
        assert!(InvitationId::parse_hex(&"zz".repeat(16)).is_none());
    }
}
