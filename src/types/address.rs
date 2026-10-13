//! A Bluetooth device address. Host identity is a [`super::HostId`], not this value.
//! LE addresses rotate; a host id does not.

use std::fmt;

/// Six-byte Bluetooth address. Display form is uppercase `AA:BB:CC:DD:EE:FF`.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Address([u8; 6]);

impl Address {
    pub fn new(bytes: [u8; 6]) -> Option<Self> {
        if bytes == [0; 6] {
            None
        } else {
            Some(Self(bytes))
        }
    }

    pub fn bytes(self) -> [u8; 6] {
        self.0
    }

    /// Accept `AA:BB:CC:DD:EE:FF` and `AA-BB-CC-DD-EE-FF`.
    pub fn parse(text: &str) -> Option<Self> {
        let sep = if text.contains(':') {
            ':'
        } else if text.contains('-') {
            '-'
        } else {
            return None;
        };
        let mut bytes = [0u8; 6];
        let mut parts = text.split(sep);
        for slot in &mut bytes {
            let part = parts.next()?;
            if part.len() != 2 {
                return None;
            }
            *slot = u8::from_str_radix(part, 16).ok()?;
        }
        if parts.next().is_some() {
            return None;
        }
        Self::new(bytes)
    }

    pub fn display(self) -> String {
        format!(
            "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
            self.0[0], self.0[1], self.0[2], self.0[3], self.0[4], self.0[5]
        )
    }
}

impl fmt::Debug for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.display())
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.display())
    }
}

/// Peer and device names on the fleet wire: printable, 1 to 80 characters.
pub fn valid_name(name: &str) -> bool {
    let count = name.chars().count();
    (1..=80).contains(&count) && name.chars().all(|ch| !ch.is_control())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_both_separators_and_rejects_the_zero_address() {
        let parsed = Address::parse("aa-bb-cc-dd-ee-ff").unwrap();
        assert_eq!(parsed.display(), "AA:BB:CC:DD:EE:FF");
        assert_eq!(Address::parse(&parsed.display()), Some(parsed));
        assert!(Address::parse("00:00:00:00:00:00").is_none());
        assert!(Address::parse("AA:BB").is_none());
    }
}
