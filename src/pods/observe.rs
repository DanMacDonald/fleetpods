//! One accessory packet, interpreted once.
//!
//! A control packet can carry a relayed peer report, a priority
//! acknowledgement, a peer-reconnect, and a setup report; each used to
//! be parsed by a different part of the step loop, and the daemon's
//! tests spoke a second, private vocabulary for the same facts. Here
//! the packet becomes typed observations in one place. Production
//! packets and tests both arrive as these observations; nobody
//! re-parses the packet downstream.

use crate::pods::setup::{Event, FollowUp, Place, Setup};
use crate::types::{Address, NoiseMode};

/// One fact an accessory packet stated.
#[derive(Clone, Debug, PartialEq)]
pub enum Observation {
    Bud { bud: crate::pods::setup::Bud, level: u8, charging: bool, place: Option<Place> },
    CaseBattery { level: u8, charging: bool },
    Ownership(bool),
    Source { local: bool, kind: u8, remote: Option<Address> },
    Noise(NoiseMode),
    Identity { name: String, model: String },
    Keys { irk: [u8; 16], encryption: [u8; 16] },
    PodPeers { peers: Vec<Address>, owner: Option<Address> },
    PeerName { address: Address, label: String },
    PeerPlaying { address: Address, playing: bool },
    PriorityAcknowledged,
    PeerReconnect(Address),
    Ready,
}

/// Interpret one packet against the setup state: the follow-up write
/// the setup sequence asks for, and every fact the packet stated.
pub fn translate(
    setup: &mut Setup,
    packet: &[u8],
    local: Option<Address>,
) -> (Option<FollowUp>, Vec<Observation>) {
    let mut observations = Vec::new();
    if let Some(report) = crate::pods::peer::parse(packet) {
        if let Some(label) = report.name {
            observations.push(Observation::PeerName { address: report.sender, label });
        }
        if let Some(playing) = report.playing {
            observations.push(Observation::PeerPlaying { address: report.sender, playing });
        }
    }
    if crate::pods::setup::priority_acknowledged(packet) {
        observations.push(Observation::PriorityAcknowledged);
    }
    if let Some(peer) = crate::pods::setup::peer_reconnect(packet) {
        observations.push(Observation::PeerReconnect(peer));
    }
    let (follow, events) = setup.observe(packet);
    for event in events {
        observations.push(match event {
            Event::BudReport { bud, level, charging, place } => {
                Observation::Bud { bud, level, charging, place }
            }
            Event::Case { level, charging } => Observation::CaseBattery { level, charging },
            Event::Ownership(locally) => Observation::Ownership(locally),
            Event::Source { address, kind } => {
                let mine = local == Some(address);
                Observation::Source { local: mine, kind, remote: (!mine).then_some(address) }
            }
            Event::Noise(mode) => Observation::Noise(mode),
            Event::Identity { name, model } => Observation::Identity { name, model },
            Event::Keys { irk, encryption } => Observation::Keys { irk, encryption },
            Event::PodPeers { peers, owner } => Observation::PodPeers { peers, owner },
            Event::Ready => Observation::Ready,
        });
    }
    (follow, observations)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pods::setup::Setup;

    #[test]
    fn an_ownership_packet_becomes_one_observation() {
        let mut setup = Setup::new();
        let packet = [0x04, 0x00, 0x04, 0x00, 0x09, 0x00, 0x06, 0x01, 0x00, 0x00, 0x00];
        let (_, observations) = translate(&mut setup, &packet, None);
        assert_eq!(observations, vec![Observation::Ownership(true)]);
    }

    #[test]
    fn a_priority_acknowledgement_is_an_observation() {
        let mut setup = Setup::new();
        let packet = [0x04, 0x00, 0x04, 0x00, 0x40, 0x00, 0x14, 0x00, 0x00, 0x00, 0x00, 0x00];
        let (_, observations) = translate(&mut setup, &packet, None);
        assert_eq!(observations, vec![Observation::PriorityAcknowledged]);
    }

    #[test]
    fn unknown_packets_state_nothing() {
        let mut setup = Setup::new();
        let (_, observations) = translate(&mut setup, &[0x01, 0x02, 0x03], None);
        assert!(observations.is_empty());
    }
}
