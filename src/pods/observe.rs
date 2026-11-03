//! One accessory packet, interpreted once.
//!
//! A control packet can carry a relayed peer report, a priority
//! acknowledgement, a peer-reconnect, and a setup report; each used to
//! be parsed by a different part of the step loop, and the daemon's
//! tests spoke a second, private vocabulary for the same facts. Here
//! the packet becomes typed observations in one place. Production
//! packets and tests both arrive as these observations; nobody
//! re-parses the packet downstream.

use crate::pods::setup::{FollowUp, Place, Setup};
use crate::types::{Address, MonoMs, NoiseMode};

/// One fact an accessory packet stated.
#[derive(Clone, Debug, PartialEq)]
pub enum Observation {
    Bud { bud: crate::pods::setup::Bud, level: u8, charging: bool, place: Option<Place> },
    HeadWear { primary: Option<bool>, secondary: Option<bool> },
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
    setup_time: Option<MonoMs>,
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
    let (follow, events) = setup.observe(packet, local, setup_time);
    observations.extend(events);
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
        let (_, observations) = translate(&mut setup, &packet, None, Some(MonoMs(0)));
        assert_eq!(observations, vec![Observation::Ownership(true)]);
    }

    #[test]
    fn a_priority_acknowledgement_is_an_observation() {
        let mut setup = Setup::new();
        let packet = [0x04, 0x00, 0x04, 0x00, 0x40, 0x00, 0x14, 0x00, 0x00, 0x00, 0x00, 0x00];
        let (_, observations) = translate(&mut setup, &packet, None, Some(MonoMs(0)));
        assert_eq!(observations, vec![Observation::PriorityAcknowledged]);
    }

    #[test]
    fn source_reports_normalize_known_local_and_unknown_hosts_once() {
        let address = Address::parse("11:22:33:44:55:66").unwrap();
        let other = Address::parse("AA:BB:CC:DD:EE:FF").unwrap();
        let packet = [4, 0, 4, 0, 0x0e, 0, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11, 2];
        for local in [Some(address), Some(other), None] {
            let (_, reports) = translate(&mut Setup::new(), &packet, local, None);
            let mine = local == Some(address);
            assert_eq!(
                reports,
                vec![Observation::Source {
                    local: mine,
                    kind: 2,
                    remote: (!mine).then_some(address)
                }]
            );
        }
    }

    #[test]
    fn unknown_packets_state_nothing() {
        let mut setup = Setup::new();
        let (_, observations) = translate(&mut setup, &[0x01, 0x02, 0x03], None, Some(MonoMs(0)));
        assert!(observations.is_empty());
    }
}
