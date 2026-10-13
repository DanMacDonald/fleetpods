//! Proximity-pairing advertisements seen while the accessory is not on the control link.
//!
//! The manufacturer payload is Apple company id `0x004C`. Address resolution is the
//! Bluetooth resolvable-private-address check: the stored identity-resolving key is
//! little-endian, and the security function `e` reads that integer most-significant
//! octet first. The last 16 bytes open with AES-128 and a zero IV, which is one block.

use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};

use crate::types::{Address, LidState, Model};

const PROXIMITY: u8 = 0x07;
const PAIRED: u8 = 0x01;
const HEADER: usize = 11;
const BLOCK: usize = 16;

/// Pro 3 broadcasts this envelope repeatedly while its white pairing light flashes.
/// Its body is not the normal plaintext placement/battery layout.
pub fn pairing_model(payload: &[u8]) -> Option<Model> {
    if payload.len() != HEADER + BLOCK
        || payload[0] != PROXIMITY
        || payload[1] != 25
        || payload[2] != 7
    {
        return None;
    }
    let model = Model::from_product(u16::from_be_bytes([payload[3], payload[4]]));
    (model == Model::AirPodsPro3).then_some(model)
}

/// What one accepted advertisement says. Coarse bud nibbles are not a battery report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frame {
    pub model: Model,
    pub primary_left: bool,
    pub this_in_case: bool,
    pub both_in_case: bool,
    pub left_in_ear: bool,
    pub right_in_ear: bool,
    pub case_level: Option<u8>,
    pub case_charging: bool,
    pub lid: Option<LidState>,
    pub connection: u8,
    pub encrypted: [u8; 16],
}

/// Which parts of a frame may change accessory state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Decision {
    pub adopt_model: bool,
    pub case_battery: bool,
    pub encrypted: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub level: u8,
    pub charging: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Levels {
    pub left: Option<Cell>,
    pub right: Option<Cell>,
    pub case: Option<Cell>,
    pub headset: Option<Cell>,
}

pub fn parse(payload: &[u8]) -> Option<Frame> {
    if payload.len() < HEADER + BLOCK || payload[0] != PROXIMITY || payload[2] != PAIRED {
        return None;
    }
    let status = payload[5];
    let primary_left = status & 0x20 != 0;
    let flipped = !primary_left;
    let this_in_case = status & 0x40 != 0;
    let both_in_case = status & 0x04 != 0;
    let swapped = flipped ^ this_in_case;
    let left_in_ear = (status & if swapped { 0x08 } else { 0x02 }) != 0;
    let right_in_ear = (status & if swapped { 0x02 } else { 0x08 }) != 0;
    let flags = payload[7] >> 4;
    let case_nibble = payload[7] & 0x0f;
    let lid = (this_in_case || both_in_case).then(|| {
        if payload[8] & 0x08 == 0 {
            LidState::Open
        } else {
            LidState::Closed
        }
    });
    let encrypted = payload[payload.len() - BLOCK..].try_into().ok()?;
    Some(Frame {
        model: Model::from_product(u16::from_be_bytes([payload[3], payload[4]])),
        primary_left,
        this_in_case,
        both_in_case,
        left_in_ear,
        right_in_ear,
        case_level: (case_nibble != 15).then_some(case_nibble * 10),
        case_charging: flags & 0x04 != 0,
        lid,
        connection: payload[10],
        encrypted,
    })
}

/// A stored key resolves this address, or there is no key and the model is known.
/// Someone else's resolving key never accepts the frame.
pub fn decide(
    irk_match: bool,
    model_recognized: bool,
    headset: bool,
    irk_known: bool,
) -> Option<Decision> {
    if !irk_match && (irk_known || !model_recognized) {
        return None;
    }
    Some(Decision { adopt_model: model_recognized, case_battery: !headset, encrypted: irk_match })
}

pub fn resolves(irk: &[u8; 16], address: Address) -> bool {
    let bytes = address.bytes();
    if bytes[0] & 0xc0 != 0x40 {
        return false;
    }
    let mut padded = [0u8; 16];
    padded[0] = bytes[2];
    padded[1] = bytes[1];
    padded[2] = bytes[0];
    let hashed = security_e(irk, &padded);
    hashed[0] == bytes[5] && hashed[1] == bytes[4] && hashed[2] == bytes[3]
}

pub fn open_block(key: &[u8; 16], block: &[u8; 16]) -> [u8; 16] {
    aes_decrypt(key, block)
}

pub fn levels(plain: &[u8; 16], primary_left: bool, pod_in_case: bool, headset: bool) -> Levels {
    if headset {
        return Levels { left: None, right: None, case: None, headset: cell(plain[1]) };
    }
    let (left, right) = if primary_left { (1, 2) } else { (2, 1) };
    let case = cell(plain[3]).filter(|cell| cell.level > 0 || pod_in_case);
    Levels { left: cell(plain[left]), right: cell(plain[right]), case, headset: None }
}

fn cell(byte: u8) -> Option<Cell> {
    let level = byte & 0x7f;
    if level > 100 {
        None
    } else {
        Some(Cell { level, charging: byte & 0x80 != 0 })
    }
}

fn security_e(key: &[u8; 16], plaintext: &[u8; 16]) -> [u8; 16] {
    let mut secret = *key;
    secret.reverse();
    let mut block = *plaintext;
    block.reverse();
    let mut out = aes_encrypt(&secret, &block);
    out.reverse();
    out
}

fn aes_encrypt(key: &[u8; 16], block: &[u8; 16]) -> [u8; 16] {
    let cipher = aes::Aes128::new(key.into());
    let mut out = aes::cipher::generic_array::GenericArray::from(*block);
    cipher.encrypt_block(&mut out);
    out.into()
}

fn aes_decrypt(key: &[u8; 16], block: &[u8; 16]) -> [u8; 16] {
    let cipher = aes::Aes128::new(key.into());
    let mut out = aes::cipher::generic_array::GenericArray::from(*block);
    cipher.decrypt_block(&mut out);
    out.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_envelope_is_separate_from_placement_and_limited_to_verified_model() {
        let mut frame = header(0x31);
        frame[2] = 7;
        assert_eq!(pairing_model(&frame), Some(Model::AirPodsPro3));
        assert!(parse(&frame).is_none());
        assert!(pairing_model(&frame[..26]).is_none());
        frame[3] = 0x19;
        assert!(pairing_model(&frame).is_none());
        frame[3] = 0x27;
        frame[1] = 24;
        assert!(pairing_model(&frame).is_none());
        assert!(pairing_model(&header(0x31)).is_none());
    }

    fn header(byte8: u8) -> Vec<u8> {
        let mut frame = vec![0x07, 0x19, 0x01, 0x27, 0x20, 0x55, 0x87, 0xb1, byte8, 0x00, 0x04];
        frame.extend_from_slice(&[0; 16]);
        frame
    }

    #[test]
    fn a_captured_case_header_names_the_lid_and_drops_a_short_frame() {
        let closed = parse(&header(0x39)).unwrap();
        assert_eq!(closed.model, Model::AirPodsPro3);
        assert_eq!(closed.lid, Some(LidState::Closed));
        assert_eq!(closed.case_level, Some(10));
        assert!(!closed.case_charging);
        assert_eq!(closed.connection, 0x04);
        assert!(closed.both_in_case);
        assert_eq!(parse(&header(0x31)).unwrap().lid, Some(LidState::Open));
        assert!(parse(&header(0x39)[..26]).is_none());
        let mut pairing = header(0x39);
        pairing[2] = 0x00;
        assert!(parse(&pairing).is_none());
        let mut other = header(0x39);
        other[0] = 0x01;
        assert!(parse(&other).is_none());
    }

    #[test]
    fn the_stored_resolving_key_matches_one_address() {
        let irk: [u8; 16] = (0..16).collect::<Vec<_>>().try_into().unwrap();
        let address = Address::parse("40:11:22:83:AC:CD").unwrap();
        assert!(resolves(&irk, address));
        let mut other = irk;
        other[0] ^= 1;
        assert!(!resolves(&other, address));
        assert!(!resolves(&irk, Address::parse("00:11:22:83:AC:CD").unwrap()));
    }

    #[test]
    fn the_encrypted_block_is_a_battery_and_an_unknown_byte_is_absent() {
        let key = [0x22; 16];
        let cipher = [
            0x55, 0xde, 0xd4, 0x4f, 0xf0, 0x65, 0x42, 0xdc, 0x10, 0xa8, 0x03, 0xc1, 0xdb, 0x77,
            0x45, 0x85,
        ];
        let plain = open_block(&key, &cipher);
        let read = levels(&plain, true, true, false);
        assert_eq!(read.left, Some(Cell { level: 83, charging: true }));
        assert_eq!(read.right, Some(Cell { level: 60, charging: false }));
        assert_eq!(read.case, Some(Cell { level: 40, charging: true }));
        assert!(read.headset.is_none());

        let mut unknown = plain;
        unknown[3] = 0x7f;
        assert!(levels(&unknown, true, false, false).case.is_none());
        unknown[3] = 0x00;
        assert!(levels(&unknown, true, false, false).case.is_none());
        assert_eq!(
            levels(&unknown, true, true, false).case,
            Some(Cell { level: 0, charging: false })
        );
        assert_eq!(
            levels(&plain, true, true, true).headset,
            Some(Cell { level: 83, charging: true })
        );
    }

    #[test]
    fn a_foreign_key_is_rejected_and_a_recognized_model_is_not() {
        assert!(decide(false, true, false, true).is_none());
        assert!(decide(false, false, false, false).is_none());
        let open = decide(false, true, false, false).unwrap();
        assert!(open.adopt_model && open.case_battery && !open.encrypted);
        let matched = decide(true, true, true, true).unwrap();
        assert!(matched.encrypted && !matched.case_battery);
    }
}
