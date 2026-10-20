//! Accessory setup once the control socket is open.
//!
//! The socket is asked for a handshake, then features, then notifications.
//! Readiness is battery plus ear placement. Unknown packets are ignored.

use crate::types::{Address, NoiseMode};

const HANDSHAKE: &[u8] = &[
    0x00, 0x00, 0x04, 0x00, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];
const FEATURES: &[u8] =
    &[0x04, 0x00, 0x04, 0x00, 0x4d, 0x00, 0xd7, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
const NOTIFICATIONS: &[u8] = &[0x04, 0x00, 0x04, 0x00, 0x0f, 0x00, 0xff, 0xff, 0xff, 0xff];

/// What the daemon just wrote on the control socket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outgoing {
    Handshake,
    Features,
    Notifications,
}

/// The packet the daemon writes after a reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FollowUp {
    Features,
    Notifications,
}

impl FollowUp {
    pub fn bytes(self) -> &'static [u8] {
        match self {
            Self::Features => FEATURES,
            Self::Notifications => NOTIFICATIONS,
        }
    }

    pub fn step(self) -> crate::pods::link::OpenStep {
        match self {
            Self::Features => crate::pods::link::OpenStep::Features,
            Self::Notifications => crate::pods::link::OpenStep::Notifications,
        }
    }
}

pub fn classify_outgoing(bytes: &[u8]) -> Option<Outgoing> {
    if bytes.starts_with(HANDSHAKE) {
        Some(Outgoing::Handshake)
    } else if bytes.starts_with(FEATURES) {
        Some(Outgoing::Features)
    } else if bytes.starts_with(NOTIFICATIONS) {
        Some(Outgoing::Notifications)
    } else {
        None
    }
}

/// Ask the accessory for its identity-resolving and advertisement keys.
pub fn request_keys() -> &'static [u8] {
    &[0x04, 0x00, 0x04, 0x00, 0x30, 0x00, 0x05, 0x00]
}

/// A control write. `value` 1 is on, 2 is off, except noise mode which uses 1–4.
pub fn setting(id: u8, value: u8) -> Vec<u8> {
    vec![0x04, 0x00, 0x04, 0x00, 0x09, 0x00, id, value, 0x00, 0x00, 0x00]
}

pub fn noise_mode(mode: NoiseMode) -> Vec<u8> {
    let value = match mode {
        NoiseMode::Off => 0x01,
        NoiseMode::Anc => 0x02,
        NoiseMode::Transparency => 0x03,
        NoiseMode::Adaptive => 0x04,
    };
    setting(0x0d, value)
}

pub fn conversation(enabled: bool) -> Vec<u8> {
    setting(0x28, if enabled { 0x01 } else { 0x02 })
}

/// Audio ownership uses 0 for release, unlike the on/off settings.
pub fn ownership(claim: bool) -> Vec<u8> {
    setting(0x06, u8::from(claim))
}

pub fn one_bud(enabled: bool) -> Vec<u8> {
    setting(0x1b, if enabled { 0x01 } else { 0x02 })
}

pub fn adaptive_level(level: u8) -> Vec<u8> {
    setting(0x2e, level.min(100))
}

/// Connection-priority write. Addresses go on the wire least-significant octet first.
/// `second` absent is the one-entry form that names only this host.
pub fn priority(
    first: crate::types::Address,
    second: Option<crate::types::Address>,
) -> Option<Vec<u8>> {
    if second == Some(first) {
        return None;
    }
    let mut packet = vec![0x04, 0x00, 0x04, 0x00, 0x14, 0x00, if second.is_some() { 2 } else { 1 }];
    packet.extend(first.bytes().into_iter().rev());
    if let Some(second) = second {
        packet.extend(second.bytes().into_iter().rev());
    }
    Some(packet)
}

pub fn priority_acknowledged(packet: &[u8]) -> bool {
    packet == [0x04, 0x00, 0x04, 0x00, 0x40, 0x00, 0x14, 0x00, 0x00, 0x00, 0x00, 0x00]
}

/// The pods relay `omarchyReconnect` with the requester in the envelope.
/// Byte 4 is `0x11` on the way in; the request we send uses `0x10`.
pub fn peer_reconnect(packet: &[u8]) -> Option<crate::types::Address> {
    const RELAYED: &[u8] = &[0x04, 0x00, 0x04, 0x00, 0x11, 0x00];
    if packet.len() < 12 || !packet.starts_with(RELAYED) {
        return None;
    }
    let marker = b"omarchyReconnect";
    let at = packet.windows(marker.len()).position(|window| window == marker)?;
    if packet.get(at + marker.len()) != Some(&0x01) {
        return None;
    }
    crate::types::Address::new([packet[11], packet[10], packet[9], packet[8], packet[7], packet[6]])
}

/// Ask the accessory to relay a reconnect to one fleet peer, then let an Apple device take the slot.
/// The two addresses must differ. The peer is the envelope target. This host is named inside the body.
pub fn reconnect_request(
    peer: crate::types::Address,
    local: crate::types::Address,
) -> Option<Vec<u8>> {
    if peer == local {
        return None;
    }
    let mut body = vec![0x01, 0xe7];
    push_key(&mut body, "localscore")?;
    body.extend_from_slice(&[0x30, 0x64]);
    push_key(&mut body, "reason")?;
    push_key(&mut body, "Hijackv2")?;
    push_key(&mut body, "audioRoutingScore")?;
    body.extend_from_slice(&[0x31, 0x2d, 0x01]);
    push_key(&mut body, "audioRoutingSetOwnershipToFalse")?;
    body.push(0x01);
    push_key(&mut body, "remotescore")?;
    body.push(0xa5);
    push_key(&mut body, "btAddress")?;
    push_key(&mut body, &local.display())?;
    push_key(&mut body, "omarchyReconnect")?;
    body.push(0x01);
    let mut packet = vec![0x04, 0x00, 0x04, 0x00, 0x10, 0x00];
    packet.extend(peer.bytes().into_iter().rev());
    let len = u16::try_from(body.len()).ok()?;
    packet.extend(len.to_le_bytes());
    packet.extend(body);
    Some(packet)
}

fn push_key(out: &mut Vec<u8>, text: &str) -> Option<()> {
    let bytes = text.as_bytes();
    let len = u8::try_from(bytes.len()).ok()?;
    if !(1..=32).contains(&len) {
        return None;
    }
    out.push(0x40 + len);
    out.extend_from_slice(bytes);
    Some(())
}

const HANDSHAKE_ACK: &[u8] = &[0x01, 0x00, 0x04, 0x00];
const FEATURES_ACK: &[u8] = &[0x04, 0x00, 0x04, 0x00, 0x2b, 0x00];
const BATTERY: &[u8] = &[0x04, 0x00, 0x04, 0x00, 0x04, 0x00];
const EARS: &[u8] = &[0x04, 0x00, 0x04, 0x00, 0x06, 0x00];
const OWNERSHIP: &[u8] = &[0x04, 0x00, 0x04, 0x00, 0x09, 0x00, 0x06];
const SOURCE: &[u8] = &[0x04, 0x00, 0x04, 0x00, 0x0e, 0x00];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Handshake,
    Features,
    Notifications,
    Live,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bud {
    Left,
    Right,
    Headset,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    InEar,
    Out,
    InCase,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    BudReport {
        bud: Bud,
        level: u8,
        charging: bool,
        place: Option<Place>,
    },
    Case {
        level: u8,
        charging: bool,
    },
    Ownership(bool),
    Source {
        address: Address,
        kind: u8,
    },
    Noise(NoiseMode),
    Identity {
        name: String,
        model: String,
    },
    Keys {
        irk: [u8; 16],
        encryption: [u8; 16],
    },
    /// Who the pods say is connected. This replaces a held fleet roster.
    PodPeers {
        peers: Vec<Address>,
        owner: Option<Address>,
    },
    Ready,
}

#[derive(Clone, Copy, Debug)]
struct Half {
    bud: Bud,
    level: u8,
    charging: bool,
    place: Option<Place>,
}

pub struct Setup {
    phase: Phase,
    primary: Option<Bud>,
    secondary: Option<Bud>,
    pending_ears: Option<(u8, u8)>,
    saw_battery: bool,
    saw_ears: bool,
    saw_roster: bool,
    ready: bool,
    halves: [Option<Half>; 2],
}

impl Setup {
    pub fn new() -> Self {
        Self {
            phase: Phase::Handshake,
            primary: None,
            secondary: None,
            pending_ears: None,
            saw_battery: false,
            saw_ears: false,
            saw_roster: false,
            ready: false,
            halves: [None, None],
        }
    }

    pub fn start(&mut self) -> &'static [u8] {
        *self = Self::new();
        HANDSHAKE
    }

    /// A setup reply advances the handshake. Anything else is a report, parsed
    /// the same way before and after the link is ready.
    pub fn observe(&mut self, packet: &[u8]) -> (Option<FollowUp>, Vec<Event>) {
        if let Some(follow) = self.take_reply(packet) {
            return (Some(follow), Vec::new());
        }
        let mut events = Vec::new();
        if let Some(report) = parse_report(packet) {
            self.note_report(report, &mut events);
        }
        // Pods playing for another computer report battery and who is
        // connected at once, but may hold ears until something changes. That
        // would stall a takeover here until the setup times out.
        if self.saw_battery
            && (self.saw_ears || self.saw_roster)
            && !self.ready
            && matches!(self.phase, Phase::Notifications | Phase::Live)
        {
            self.ready = true;
            self.phase = Phase::Live;
            events.push(Event::Ready);
        }
        (None, events)
    }

    fn take_reply(&mut self, packet: &[u8]) -> Option<FollowUp> {
        if packet.starts_with(HANDSHAKE_ACK)
            && self.phase == Phase::Handshake
            && (packet.len() < 6 || packet[4..6] == [0, 0])
        {
            self.phase = Phase::Features;
            return Some(FollowUp::Features);
        }
        if packet.starts_with(FEATURES_ACK) && self.phase == Phase::Features {
            self.phase = Phase::Notifications;
            return Some(FollowUp::Notifications);
        }
        None
    }

    fn note_report(&mut self, report: Report, events: &mut Vec<Event>) {
        match report {
            Report::Battery(cells) => {
                self.saw_battery = true;
                let buds: Vec<Bud> = cells
                    .iter()
                    .filter_map(|cell| match cell.part {
                        Part::Bud(bud) => Some(bud),
                        Part::Case => None,
                    })
                    .collect();
                self.primary = buds.first().copied();
                self.secondary = buds.get(1).copied();
                for cell in cells {
                    self.store(cell, events);
                }
                if let Some((primary, secondary)) = self.pending_ears.take() {
                    self.apply_ears(primary, secondary, events);
                }
            }
            Report::Ears { primary, secondary } => self.apply_ears(primary, secondary, events),
            Report::Ownership(local) => events.push(Event::Ownership(local)),
            Report::Source { address, kind } => events.push(Event::Source { address, kind }),
            Report::Noise(mode) => events.push(Event::Noise(mode)),
            Report::Identity { name, model } => events.push(Event::Identity { name, model }),
            Report::Keys { irk, encryption } => events.push(Event::Keys { irk, encryption }),
            Report::PodPeers { peers, owner } => {
                if matches!(self.phase, Phase::Notifications | Phase::Live) {
                    self.saw_roster = true;
                }
                events.push(Event::PodPeers { peers, owner });
            }
        }
    }

    fn store(&mut self, cell: Cell, events: &mut Vec<Event>) {
        let Part::Bud(bud) = cell.part else {
            events.push(Event::Case { level: cell.level, charging: cell.charging });
            return;
        };
        let place = self
            .halves
            .iter()
            .find_map(|half| half.filter(|half| half.bud == bud).and_then(|half| half.place));
        let half = Half { bud, level: cell.level, charging: cell.charging, place };
        if let Some(slot) =
            self.halves.iter_mut().find(|slot| slot.is_some_and(|half| half.bud == bud))
        {
            *slot = Some(half);
        } else if let Some(slot) = self.halves.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(half);
        }
        events.push(Event::BudReport { bud, level: cell.level, charging: cell.charging, place });
    }

    fn apply_ears(&mut self, primary: u8, secondary: u8, events: &mut Vec<Event>) {
        let (Some(primary_bud),) = (self.primary,) else {
            self.pending_ears = Some((primary, secondary));
            return;
        };
        self.saw_ears = true;
        // The report lists the pods by role, and the order comes from the last
        // battery report. Taking a pod out or docking it can hand the primary
        // role to the other pod before a battery report says so. Read with the
        // old order, both pods would appear to move at once (XPS, 2026-10-19:
        // left in ear / right out, then left in case / right in ear, 0.27 s
        // apart). When the reversed order moves fewer pods, the roles swapped.
        if let Some(secondary_bud) = self.secondary {
            let moved = |bud, byte| {
                let place = place_of(byte);
                self.halves
                    .iter()
                    .flatten()
                    .find(|half| half.bud == bud)
                    .and_then(|half| half.place)
                    .is_some_and(|old| place.is_some_and(|new| new != old)) as u8
            };
            let as_listed = moved(primary_bud, primary) + moved(secondary_bud, secondary);
            let swapped = moved(secondary_bud, primary) + moved(primary_bud, secondary);
            if swapped < as_listed {
                eprintln!("fleetpodsd: ear report reads as a primary/secondary role swap");
                self.primary = Some(secondary_bud);
                self.secondary = Some(primary_bud);
                return self.apply_ears(primary, secondary, events);
            }
        }
        self.place(primary_bud, primary, events);
        if let Some(bud) = self.secondary {
            self.place(bud, secondary, events);
        }
    }

    fn place(&mut self, bud: Bud, byte: u8, events: &mut Vec<Event>) {
        let place = place_of(byte);
        let slot = self.halves.iter_mut().find(|half| half.is_some_and(|half| half.bud == bud));
        let Some(slot) = slot else {
            return;
        };
        let Some(half) = slot.as_mut() else {
            return;
        };
        half.place = place;
        events.push(Event::BudReport { bud, level: half.level, charging: half.charging, place });
    }
}

fn place_of(byte: u8) -> Option<Place> {
    match byte {
        0x00 => Some(Place::InEar),
        0x01 => Some(Place::Out),
        0x02 => Some(Place::InCase),
        _ => None,
    }
}

impl Default for Setup {
    fn default() -> Self {
        Self::new()
    }
}

/// One accessory report. Independent of the handshake phase.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Report {
    Battery(Vec<Cell>),
    Ears {
        primary: u8,
        secondary: u8,
    },
    Ownership(bool),
    Source {
        address: Address,
        kind: u8,
    },
    Noise(NoiseMode),
    Identity {
        name: String,
        model: String,
    },
    Keys {
        irk: [u8; 16],
        encryption: [u8; 16],
    },
    /// Connected addresses from a TiPi device list, in packet order.
    PodPeers {
        peers: Vec<Address>,
        owner: Option<Address>,
    },
}

pub fn parse_report(packet: &[u8]) -> Option<Report> {
    if let Some(cells) = parse_battery(packet) {
        return Some(Report::Battery(cells));
    }
    if packet.len() == 8 && packet.starts_with(EARS) {
        return Some(Report::Ears { primary: packet[6], secondary: packet[7] });
    }
    if packet.len() == 11 && packet.starts_with(OWNERSHIP) && matches!(packet[7], 0 | 1) {
        return Some(Report::Ownership(packet[7] == 1));
    }
    if let Some(Event::Source { address, kind }) = parse_source(packet) {
        return Some(Report::Source { address, kind });
    }
    if let Some(mode) = parse_noise(packet) {
        return Some(Report::Noise(mode));
    }
    if let Some(Event::Identity { name, model }) = parse_metadata(packet) {
        return Some(Report::Identity { name, model });
    }
    if let Some(Event::Keys { irk, encryption }) = parse_keys(packet) {
        return Some(Report::Keys { irk, encryption });
    }
    if let Some((peers, owner)) = connected_roster(packet) {
        return Some(Report::PodPeers { peers, owner });
    }
    None
}

/// Connected devices in a TiPi list (`04 00 04 00 2e 00`).
///
/// A malformed list is `None`, so a bad packet does not wipe a good one.
/// The status byte beside each address is `2` while that device is connected.
pub fn connected_peers(packet: &[u8]) -> Option<Vec<Address>> {
    connected_roster(packet).map(|(peers, _)| peers)
}

/// In each eight-byte entry, byte six is connection state and bit 0x02
/// of byte seven identifies the audio owner. Disconnected entries cannot own
/// audio; ambiguous owner flags must not manufacture an ownership report.
fn connected_roster(packet: &[u8]) -> Option<(Vec<Address>, Option<Address>)> {
    const HEADER: &[u8] = &[0x04, 0x00, 0x04, 0x00, 0x2e, 0x00];
    if packet.len() < 9 || !packet.starts_with(HEADER) {
        return None;
    }
    let count = packet[8] as usize;
    if packet.len() != 9 + count * 8 {
        return None;
    }
    let mut seen = Vec::with_capacity(count);
    let mut connected = Vec::new();
    let mut owners = Vec::new();
    for index in 0..count {
        let at = 9 + index * 8;
        let bytes: [u8; 6] = packet.get(at..at + 6)?.try_into().ok()?;
        if bytes == [0; 6] || bytes[0] & 1 != 0 || seen.contains(&bytes) {
            return None;
        }
        seen.push(bytes);
        if packet.get(15 + index * 8).copied() == Some(2) {
            let address = Address::new(bytes)?;
            connected.push(address);
            if packet[16 + index * 8] & 0x02 != 0 {
                owners.push(address);
            }
        }
    }
    let owner = if owners.len() == 1 { owners.first().copied() } else { None };
    Some((connected, owner))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    part: Part,
    level: u8,
    charging: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    Bud(Bud),
    Case,
}

fn parse_battery(packet: &[u8]) -> Option<Vec<Cell>> {
    if packet.len() < 7 || !packet.starts_with(BATTERY) {
        return None;
    }
    let count = packet[6] as usize;
    if count > 3 || packet.len() != 7 + 5 * count {
        return None;
    }
    let mut cells = Vec::with_capacity(count);
    for index in 0..count {
        let offset = 7 + 5 * index;
        if packet[offset + 1] != 0x01 || packet[offset + 4] != 0x01 {
            return None;
        }
        let part = match packet[offset] {
            0x01 => Part::Bud(Bud::Headset),
            0x02 => Part::Bud(Bud::Right),
            0x04 => Part::Bud(Bud::Left),
            0x08 => Part::Case,
            _ => return None,
        };
        if packet[offset + 2] > 100 {
            return None;
        }
        let status = packet[offset + 3];
        if status == 0x04 {
            continue;
        }
        // Pro 3 also reports 0x05 with valid levels and pod ordering. An
        // unfamiliar charge state must not discard the whole battery report
        // and prevent setup. Only 0x01 positively reports charging.
        let charging = status == 0x01;
        cells.push(Cell { part, level: packet[offset + 2], charging });
    }
    Some(cells)
}

fn parse_source(packet: &[u8]) -> Option<Event> {
    if packet.len() != 13 || !packet.starts_with(SOURCE) || packet[12] > 2 {
        return None;
    }
    let raw = [packet[11], packet[10], packet[9], packet[8], packet[7], packet[6]];
    let address = Address::new(raw)?;
    Some(Event::Source { address, kind: packet[12] })
}

fn parse_metadata(packet: &[u8]) -> Option<Event> {
    const HEADER: &[u8] = &[0x04, 0x00, 0x04, 0x00, 0x1d];
    if !packet.starts_with(HEADER) || packet.len() < HEADER.len() + 6 {
        return None;
    }
    let mut pos = HEADER.len() + 6;
    let name = c_string(packet, &mut pos)?;
    let model = c_string(packet, &mut pos)?;
    let _manufacturer = c_string(packet, &mut pos)?;
    Some(Event::Identity { name, model })
}

fn c_string(packet: &[u8], pos: &mut usize) -> Option<String> {
    if *pos > packet.len() {
        return None;
    }
    let start = *pos;
    while *pos < packet.len() && packet[*pos] != 0 {
        *pos += 1;
    }
    let text = String::from_utf8(packet[start..*pos].to_vec()).ok()?;
    if *pos < packet.len() {
        *pos += 1;
    }
    Some(text)
}

fn parse_keys(packet: &[u8]) -> Option<Event> {
    const HEADER: &[u8] = &[0x04, 0x00, 0x04, 0x00, 0x31, 0x00, 0x02];
    if packet.len() < 47 || !packet.starts_with(HEADER) {
        return None;
    }
    let mut index = HEADER.len();
    if packet[index] != 0x01 {
        return None;
    }
    index += 1;
    let len = u16::from_be_bytes([packet[index], packet[index + 1]]);
    if len != 16 {
        return None;
    }
    index += 3;
    let mut irk = [0u8; 16];
    irk.copy_from_slice(&packet[index..index + 16]);
    index += 16;
    if packet[index] != 0x04 {
        return None;
    }
    index += 1;
    let len = u16::from_be_bytes([packet[index], packet[index + 1]]);
    if len != 16 {
        return None;
    }
    index += 3;
    let mut encryption = [0u8; 16];
    encryption.copy_from_slice(&packet[index..index + 16]);
    Some(Event::Keys { irk, encryption })
}

fn parse_noise(packet: &[u8]) -> Option<NoiseMode> {
    if packet.len() != 11 || !packet.starts_with(&[0x04, 0x00, 0x04, 0x00, 0x09, 0x00, 0x0d]) {
        return None;
    }
    Some(match packet[7] {
        0x01 => NoiseMode::Off,
        0x02 => NoiseMode::Anc,
        0x03 => NoiseMode::Transparency,
        0x04 => NoiseMode::Adaptive,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Address;

    fn handshake_ack() -> Vec<u8> {
        vec![0x01, 0x00, 0x04, 0x00, 0x01]
    }

    fn features_ack() -> Vec<u8> {
        vec![0x04, 0x00, 0x04, 0x00, 0x2b, 0x00, 0x01]
    }

    fn sample_ears() -> Vec<u8> {
        vec![0x04, 0x00, 0x04, 0x00, 0x06, 0x00, 0x00, 0x02]
    }

    fn sample_battery() -> Vec<u8> {
        let mut packet = vec![0x04, 0x00, 0x04, 0x00, 0x04, 0x00, 0x03];
        packet.extend_from_slice(&[0x04, 0x01, 80, 0x02, 0x01]);
        packet.extend_from_slice(&[0x02, 0x01, 60, 0x01, 0x01]);
        packet.extend_from_slice(&[0x08, 0x01, 40, 0x02, 0x01]);
        packet
    }

    #[test]
    fn captured_pro_3_charge_states_do_not_block_setup() {
        // XPS, 2026-10-12: right 80%, left 79%, case 52%; both buds docked.
        let battery = [4, 0, 4, 0, 4, 0, 3, 2, 1, 80, 5, 1, 4, 1, 79, 5, 1, 8, 1, 52, 2, 1];
        let ears = [4, 0, 4, 0, 6, 0, 2, 2];
        for ears_first in [false, true] {
            let mut setup = Setup::new();
            setup.start();
            setup.observe(&handshake_ack());
            setup.observe(&features_ack());
            let (first, last): (&[u8], &[u8]) =
                if ears_first { (&ears, &battery) } else { (&battery, &ears) };
            assert!(!setup.observe(first).1.contains(&Event::Ready));
            let events = setup.observe(last).1;
            assert!(events.contains(&Event::Ready));
            for (bud, level) in [(Bud::Right, 80), (Bud::Left, 79)] {
                assert!(events.contains(&Event::BudReport {
                    bud,
                    level,
                    charging: false,
                    place: Some(Place::InCase),
                }));
            }
            assert!(!setup.observe(&ears).1.contains(&Event::Ready));
            let mut malformed = battery;
            malformed[8] = 0;
            assert!(parse_report(&malformed).is_none());
        }
    }

    #[test]
    fn battery_and_the_connected_list_complete_setup_when_ears_never_arrive() {
        // XPS, 2026-10-19: a laptop not playing to the pods got the connected
        // list and battery within a second of subscribing, but no ears report.
        let roster = [0x04, 0x00, 0x04, 0x00, 0x2e, 0x00, 0x01, 0x02, 0x00];
        for subscribed in [true, false] {
            let mut setup = Setup::new();
            setup.start();
            setup.observe(&handshake_ack());
            if subscribed {
                setup.observe(&features_ack());
            }
            assert!(!setup.observe(&roster).1.contains(&Event::Ready));
            let events = setup.observe(&sample_battery()).1;
            assert_eq!(events.contains(&Event::Ready), subscribed, "only after subscribing");
            assert!(!setup.observe(&sample_ears()).1.contains(&Event::Ready), "ready once");
        }
    }

    #[test]
    fn an_ear_report_after_a_role_swap_does_not_move_both_pods() {
        let mut setup = Setup::new();
        setup.start();
        setup.observe(&handshake_ack());
        setup.observe(&features_ack());
        setup.observe(&sample_battery());
        // Primary in an ear, secondary docked.
        setup.observe(&[0x04, 0x00, 0x04, 0x00, 0x06, 0x00, 0x00, 0x02]);
        let placed = |setup: &Setup| {
            setup.halves.iter().flatten().map(|half| (half.bud, half.place)).collect::<Vec<_>>()
        };
        let before = placed(&setup);
        // The docked pod became primary: the same two places, listed the other way.
        let events = setup.observe(&[0x04, 0x00, 0x04, 0x00, 0x06, 0x00, 0x02, 0x00]).1;
        assert_eq!(placed(&setup), before, "neither pod moved");
        assert!(events.iter().all(|event| match event {
            Event::BudReport { bud, place, .. } => before.contains(&(*bud, *place)),
            _ => true,
        }));
        // A real single move under the new roles still lands on the right pod.
        let worn = before.iter().find(|(_, place)| *place == Some(Place::InEar)).unwrap().0;
        setup.observe(&[0x04, 0x00, 0x04, 0x00, 0x06, 0x00, 0x02, 0x01]);
        assert!(placed(&setup).contains(&(worn, Some(Place::Out))), "the worn pod came out");
    }

    #[test]
    fn a_refused_handshake_does_not_start_feature_negotiation() {
        let mut setup = Setup::new();
        setup.start();
        assert_eq!(setup.observe(&[1, 0, 4, 0, 0x85, 0]), (None, vec![]));
        assert_eq!(setup.observe(&[1, 0, 4, 0, 0, 0]).0, Some(FollowUp::Features));
    }

    #[test]
    fn setup_waits_for_battery_and_ears_before_ready() {
        let mut setup = Setup::new();
        assert_eq!(setup.start(), HANDSHAKE);
        let (next, events) = setup.observe(&handshake_ack());
        assert_eq!(next, Some(FollowUp::Features));
        assert!(events.is_empty());
        let (next, _) = setup.observe(&features_ack());
        assert_eq!(next, Some(FollowUp::Notifications));
        let (_, events) = setup.observe(&sample_battery());
        assert!(events
            .iter()
            .any(|event| matches!(event, Event::BudReport { bud: Bud::Left, level: 80, .. })));
        assert!(events.iter().any(|event| matches!(event, Event::Case { level: 40, .. })));
        assert!(!events.iter().any(|event| matches!(event, Event::Ready)));
        let (_, events) = setup.observe(&sample_ears());
        assert!(events.iter().any(|event| matches!(
            event,
            Event::BudReport { bud: Bud::Left, place: Some(Place::InEar), .. }
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            Event::BudReport { bud: Bud::Right, place: Some(Place::InCase), .. }
        )));
        assert!(events.iter().any(|event| matches!(event, Event::Ready)));
    }

    #[test]
    fn ownership_and_noise_are_not_a_second_setup() {
        let mut setup = Setup::new();
        let _ = setup.start();
        let (_, events) =
            setup.observe(&[0x04, 0x00, 0x04, 0x00, 0x09, 0x00, 0x06, 0x00, 0x00, 0x00, 0x00]);
        assert_eq!(events, vec![Event::Ownership(false)]);
        let (_, events) =
            setup.observe(&[0x04, 0x00, 0x04, 0x00, 0x09, 0x00, 0x0d, 0x02, 0x00, 0x00, 0x00]);
        assert_eq!(events, vec![Event::Noise(NoiseMode::Anc)]);
        let (_, events) = setup.observe(&[0xff, 0x00]);
        assert!(events.is_empty());
    }

    #[test]
    fn metadata_names_the_model_and_the_keys_stay_paired() {
        let mut setup = Setup::new();
        let mut packet = vec![0x04, 0x00, 0x04, 0x00, 0x1d, 0, 0, 0, 0, 0, 0];
        packet.extend_from_slice(b"AirPods Pro\0A3064\0Apple Inc.\0");
        let (_, events) = setup.observe(&packet);
        assert_eq!(
            events,
            vec![Event::Identity { name: "AirPods Pro".into(), model: "A3064".into() }]
        );

        let mut keys = vec![0x04, 0x00, 0x04, 0x00, 0x31, 0x00, 0x02, 0x01, 0x00, 0x10, 0x00];
        keys.extend_from_slice(&[0x11; 16]);
        keys.extend_from_slice(&[0x04, 0x00, 0x10, 0x00]);
        keys.extend_from_slice(&[0x22; 16]);
        let (_, events) = setup.observe(&keys);
        match &events[..] {
            [Event::Keys { irk, encryption }] => {
                assert_eq!(irk, &[0x11; 16]);
                assert_eq!(encryption, &[0x22; 16]);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_tipi_list_keeps_only_the_connected_addresses() {
        let frame = [
            0x04, 0x00, 0x04, 0x00, 0x2e, 0x00, 0x01, 0x02, 0x03, 0x02, 0x00, 0x00, 0x00, 0x00,
            0x01, 0x00, 0x05, 0x02, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x01, 0x02, 0x00, 0x00,
            0x00, 0x00, 0x03, 0x02, 0x02,
        ];
        let peers = connected_peers(&frame).unwrap();
        assert_eq!(peers, vec![Address::parse("02:00:00:00:00:03").unwrap()]);
        assert!(connected_peers(&frame[..frame.len() - 1]).is_none());
        assert_eq!(
            connected_peers(&[0x04, 0x00, 0x04, 0x00, 0x2e, 0x00, 0x01, 0x02, 0x00]),
            Some(vec![])
        );
    }

    #[test]
    fn a_phone_only_list_leaves_the_mac_and_this_laptop_out() {
        // Captured layout: Mac disconnected, phone connected, this laptop still connecting.
        let packet = [
            0x04, 0x00, 0x04, 0x00, 0x2e, 0x00, 0x01, 0x02, 0x03, 0x02, 0x00, 0x00, 0x00, 0x00,
            0x03, 0x00, 0x01, 0x02, 0x00, 0x00, 0x00, 0x00, 0x02, 0x02, 0x17, 0x02, 0x00, 0x00,
            0x00, 0x00, 0x01, 0x01, 0x00,
        ];
        let phone = Address::parse("02:00:00:00:00:02").unwrap();
        assert_eq!(connected_peers(&packet), Some(vec![phone]));
        assert!(connected_peers(&packet[..packet.len() - 1]).is_none());
    }

    #[test]
    fn roster_owner_requires_one_connected_owner_and_a_valid_whole_packet() {
        let first = Address::parse("02:00:00:00:00:01").unwrap();
        let second = Address::parse("02:00:00:00:00:02").unwrap();
        for (first_state, first_flags, second_flags, expected) in [
            (2, 3, 1, Some(first)),
            (2, 1, 0x17, Some(second)),
            (0, 3, 1, None),
            (2, 1, 1, None),
            (2, 3, 3, None),
        ] {
            let mut packet = vec![4, 0, 4, 0, 0x2e, 0, 1, 2, 2];
            packet.extend(first.bytes());
            packet.extend([first_state, first_flags]);
            packet.extend(second.bytes());
            packet.extend([2, second_flags]);
            let (_, events) = Setup::new().observe(&packet);
            assert!(matches!(&events[..], [Event::PodPeers { owner, .. }] if *owner == expected));
            packet.pop();
            assert!(Setup::new().observe(&packet).1.is_empty());
        }
    }

    #[test]
    fn priority_names_the_two_addresses_least_significant_octet_first() {
        let local = Address::parse("02:00:00:00:00:01").unwrap();
        let remote = Address::parse("02:00:00:00:00:02").unwrap();
        assert_eq!(
            priority(remote, Some(local)).unwrap(),
            [
                0x04, 0x00, 0x04, 0x00, 0x14, 0x00, 0x02, 0x02, 0x00, 0x00, 0x00, 0x00, 0x02, 0x01,
                0x00, 0x00, 0x00, 0x00, 0x02,
            ]
        );
        assert_eq!(
            priority(local, None).unwrap(),
            [0x04, 0x00, 0x04, 0x00, 0x14, 0x00, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x02]
        );
        assert!(priority(local, Some(local)).is_none());
        assert!(priority_acknowledged(&[
            0x04, 0x00, 0x04, 0x00, 0x40, 0x00, 0x14, 0x00, 0x00, 0x00, 0x00, 0x00
        ]));
        assert!(!priority_acknowledged(&[
            0x04, 0x00, 0x04, 0x00, 0x40, 0x00, 0x14, 0x01, 0x00, 0x00, 0x00, 0x00
        ]));
    }

    #[test]
    fn a_reconnect_request_names_this_host_and_the_peer() {
        let local = Address::parse("02:00:00:00:00:01").unwrap();
        let peer = Address::parse("02:00:00:00:00:02").unwrap();
        assert!(reconnect_request(local, local).is_none());
        let packet = reconnect_request(peer, local).unwrap();
        assert_eq!(&packet[..6], &[0x04, 0x00, 0x04, 0x00, 0x10, 0x00]);
        assert_eq!(&packet[6..12], &peer.bytes().into_iter().rev().collect::<Vec<_>>());
        assert!(packet.windows(16).any(|window| window == b"omarchyReconnect"));
        assert!(packet.windows(9).any(|window| window == b"btAddress"));
        assert_ne!(reconnect_request(local, peer).unwrap(), packet);
        assert!(peer_reconnect(&packet).is_none());
        let mut relayed = packet.clone();
        relayed[4] = 0x11;
        relayed[6..12].copy_from_slice(&local.bytes().into_iter().rev().collect::<Vec<_>>());
        assert_eq!(peer_reconnect(&relayed), Some(local));
    }
}
