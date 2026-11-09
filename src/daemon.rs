//! Composition root. [`crate::step::Input`] is the platform path. Each input
//! reaches one module, then [`Daemon::step`] returns the effects that input caused.
//!
//! A keyring write is not success until [`Daemon::persist_enrollment`] is called
//! with the result of that write. The in-memory store is the record of what was
//! persisted, not a substitute for the keyring.

use serde_json::Value;

use crate::audio::mic::Microphone;
#[cfg(test)]
use crate::audio::play::StreamSnap;
use crate::audio::play::{Audio, PlayReply, Profile};
use crate::audio::seat::{Holder, Inputs as SeatInputs, Resolved, Seat};
use crate::fleet::crypto::Pake;
use crate::fleet::enroll::Enrollment;
use crate::fleet::letter::{Letter, Outbound};
use crate::fleet::members::{AccessoryKeys, Fleet, Membership, Proposal, Reject};
use crate::fleet::nearby::Discovery;
use crate::fleet::roster::{Ingest, Peers, RosterReport};
use crate::fleet::route::{Phase, Routing, Start};
use crate::linux::os;
use crate::panel::ipc::{Command, FleetCommand, Response};
use crate::panel::status::{self, Snapshot};
use crate::pods::link::{Connection, FailReason, Stage};
use crate::pods::prefs::Config;
use crate::pods::reports::{Accessory, Pod};
use crate::types::{
    code, Address, Clock, Code, FleetId, Generation, HostId, InvitationId, LidState, Model, MonoMs,
    NoiseMode, UnixMs,
};

/// One bud's advertised placement. A docked bud's stale view must not
/// overrule the other bud's fresh worn report.
fn placement(
    sight: &crate::pods::air::Frame,
    right: bool,
    prior: Option<crate::pods::reports::Placement>,
) -> (Option<bool>, Option<bool>) {
    let worn = prior.is_some_and(|prior| prior.in_ear == Some(true));
    let sender = right != sight.primary_left;
    if sender && sight.this_in_case {
        return (Some(false), Some(true));
    }
    if sight.both_in_case {
        if worn {
            return (Some(true), Some(false));
        }
        return (Some(false), Some(true));
    }
    let in_ear = if right { sight.right_in_ear } else { sight.left_in_ear };
    let in_case = if in_ear {
        Some(false)
    } else if sender {
        // The sending pod is not in the case, or this_in_case would say so.
        Some(false)
    } else {
        None
    };
    (Some(in_ear), in_case)
}

/// How long a pod's advertisement about itself outranks the other pod's
/// advertisements about it.
const ADVERTISED_SELF_MS: u64 = 1_500;

/// Effects queued by handlers and drained once per step by the harvest
/// sweep in `step`. Handlers write here and nowhere else for anything
/// that must leave the process; harvest is the only reader, so an
/// effect's cause is always the handler that queued it.
#[derive(Default)]
pub(crate) struct PendingEffects {
    pub control_out: Vec<(Generation, Vec<u8>)>,
    pub secret_out: Vec<(String, Vec<u8>)>,
    pub drop_link: bool,
    pub remove_device: Option<Address>,
    /// Selection changed: the platform must follow (and persist) it.
    pub select_accessory: Option<Option<Address>>,
    /// Disconnect this specific accessory (the outgoing one on a switch),
    /// distinct from `drop_link`, which means the selected accessory.
    pub disconnect_accessory: Option<Address>,
    pub pause_players: Vec<String>,
    pub keyring_dirty: bool,
    pub follow_link: bool,
    pub announce_dirty: bool,
}

mod session;

pub struct Daemon {
    session: session::Session,
    pub(crate) local_name: String,
    pub(crate) local_address: Option<Address>,
    pub(crate) config: Config,
    pub(crate) bluetooth_bonded: Option<bool>,
    pub(crate) connection: Connection,
    pub(crate) accessory: Accessory,
    /// Reports for known accessories that are not the selected one.
    /// They are tracked from advertisements only; connection, lifecycle,
    /// and seat stay with the selected accessory until Phase B/C.
    pub(crate) tracked: std::collections::BTreeMap<Address, Accessory>,
    pub(crate) nearby_pods: crate::pods::nearby::Nearby,
    pub(crate) audio: Audio,
    pub(crate) discovery: Discovery,
    pub(crate) peers: Peers,
    pub(crate) membership: Membership,
    pub(crate) enrollment: Enrollment,
    pub(crate) routing: Routing,
    pub(crate) microphone: Microphone,
    pub(crate) mic_path: std::sync::Arc<crate::audio::queue::MicPath>,
    mic_decoder: Option<crate::audio::eld::Decoder>,
    mic_started: bool,
    mic_source: Option<(Address, u32, String)>,
    pub(crate) outbox: Vec<Outbound>,
    pub(crate) pake: Option<Pake>,
    pub(crate) offer: Option<Vec<u8>>,
    /// Also holds off enrollment after a failed startup read, until a retry succeeds.
    pub(crate) keyring_locked: bool,
    pub(crate) roster_seq: u64,
    pub(crate) effects: PendingEffects,
    pub(crate) commit: crate::fleet::commit::Commit,
    priority_until: Option<MonoMs>,
    priority_generation: u64,
    /// The only seat record. Claim and step-back are its writers. Status reads it.
    pub(crate) seat: Seat,
    /// Hold passive reconnects after yielding or losing a slot so the
    /// incoming device can finish connecting without being displaced.
    slot_hold_until: Option<MonoMs>,
    /// Who owned the audio while this laptop shared the pods with a fleet
    /// laptop, and whether this laptop is holding its automatic reconnect
    /// so that owner can come back first.
    pair_audio_owner: Option<Address>,
    reopen_wait: bool,
    reopen_hold_until: Option<MonoMs>,
    pub(crate) lifecycle: crate::pods::lifecycle::Lifecycle,
    pub(crate) staged_controls: Vec<Vec<u8>>,
    pub(crate) peers_seated: Vec<Address>,
    pub(crate) output: crate::audio::route::Router,
    pub(crate) last_graph: Option<crate::audio::route::Graph>,
    pub(crate) output_health: crate::audio::health::Health,
    /// Whether the accessory's A2DP transport is streaming.
    pub(crate) transport_active: bool,
    /// When each pod (left, right) last advertised while sending the frame.
    pub(crate) advertised_self_at: [Option<MonoMs>; 2],
    pub(crate) radio_inbox: crate::fleet::radio::Inbox,
    /// Letter frames waiting to be split. The daemon chooses each fragment.
    pub(crate) outbound: crate::fleet::radio::Schedule,
    pub(crate) presence_air: crate::fleet::airtime::PresenceAir,
    /// Discovery has been asked to stay up.
    pub(crate) listening: bool,
    pub(crate) suspending: bool,
    pub(crate) adapter_down: bool,
    pub(crate) radio_silenced: bool,
    air_session: [u8; 16],
    session_fleet: Option<FleetId>,
    roster_air: crate::fleet::airtime::RosterAir,
    roster_change_queued: Option<MonoMs>,
    roster_dirty: bool,
    /// Unix time of the latest local source report. Sealing a roster does not move it.
    local_use: Option<UnixMs>,
    /// Last non-empty peer list from the other daemon, and who sent it.
    /// The seat keeps that list while the sender is still nearby. A missed
    /// roster refresh must not move them to the tray.
    held_peers: Vec<Address>,
    held_reporter: Option<Address>,
    /// Connected devices from the pods' own list. Known stays set after an empty list.
    pod_peers: Vec<Address>,
    pod_peers_known: bool,
    /// The pods' list has arrived at least once. After that, a dropped link
    /// resets the saved roster instead of reviving remembered seat members.
    pod_list_seen: bool,
    pod_peers_at: Option<crate::types::UnixMs>,
    /// The pods' devices in the order they joined, oldest first, kept while
    /// this laptop's Bluetooth link stays up. Every panel draws the Omarchy
    /// seat from it, so all of them put the newest laptop on the right.
    pod_order: Vec<Address>,
    /// This laptop saw the order form. One that restarted onto a live link
    /// follows the other laptop's roster instead.
    pod_order_seen: bool,
    acl_up_at: Option<crate::types::UnixMs>,
    /// A conflicting authenticated roster asks us to verify this socket, not
    /// to trust the peer over a working local connection.
    roster_probe: Option<(Generation, MonoMs)>,
    roster_probe_after: Option<MonoMs>,
    /// An Apple device took the slot this laptop had been sitting in.
    /// A failed route, a closed case, and an idle link do not set this.
    displaced: bool,
    case_opened_at: Option<MonoMs>,
    apple_labels: std::collections::HashMap<Address, String>,
    apple_labels_dirty: bool,
    introduced_peers: (Generation, Vec<Address>),
    peer_media_sent: std::collections::HashMap<Address, bool>,
    peer_playback: std::collections::HashMap<Address, (bool, MonoMs)>,
    idle_claimed: Option<(Address, MonoMs)>,
    activity_at: Option<MonoMs>,
    /// Whether a bud is in an ear, and since when.
    worn: Option<(bool, MonoMs)>,
    /// When the pods last went from out of both ears into one.
    inserted_at: Option<MonoMs>,
    pub(crate) activity_preference: crate::fleet::activity::Preference,
    pod_state: crate::fleet::pod_state::Sharing,
    route_seq: u64,
    route_air_tx: Option<u64>,
    route_air_at: Option<MonoMs>,
    route_wait_from: Option<MonoMs>,
    route_water: Vec<RouteMark>,
    route_seen: Vec<SeenRoute>,
    route_reply: Option<SavedRoute>,
    /// Admission waits for the pair reservation or confirmed local link release.
    pending_admission: Option<PendingAdmission>,
    /// The request packet for this transaction. A retry sends these same bytes.
    route_packet: Option<Vec<u8>>,
    /// Why the current request has no packet yet. Logged once per reason.
    route_block: Option<&'static str>,
    /// The connection packet. A retry sends these same bytes.
    route_connection: Option<Vec<u8>>,
}

struct PendingAdmission {
    request: crate::fleet::route_air::Message,
    until: MonoMs,
    release: Option<Generation>,
}

struct RouteMark {
    source: Address,
    session: [u8; 16],
    sequence: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct SeenRoute {
    source: Address,
    session: [u8; 16],
    tx: u64,
    kind: crate::fleet::route_air::Kind,
    sequence: u64,
}

struct SavedRoute {
    source: Address,
    session: [u8; 16],
    sequence: u64,
    packet: Vec<u8>,
}

struct PlannedRoster {
    fleet: FleetId,
    session: [u8; 16],
    accessory: Address,
    peers: Vec<Address>,
    audio_source: Option<Address>,
    pending: bool,
    audio_idle: bool,
    intent: bool,
    air: crate::fleet::roster_air::Snapshot,
}

#[derive(Clone, Copy)]
enum ActivityTarget {
    None,
    One(Address),
    Several,
}

/// How long a yielded slot stays yielded: long enough for the Apple device
/// to take it, short enough that the laptop rejoins on its own afterwards.
const SLOT_HOLD_MS: u64 = 30_000;

/// An established link disappearing may be another device taking its slot.
/// Desktop activity must give that connection time to settle before reopening.
const DISPLACEMENT_HOLD_MS: u64 = 5_000;
const ROSTER_PROBE_MS: u64 = 5_000;
const ROSTER_PROBE_INTERVAL_MS: u64 = 15_000;

/// How long the non-owner of a dropped laptop pair holds its automatic
/// reconnect so the owner can land first, before it may attach anyway.
const REOPEN_HOLD_MS: u64 = 15_000;

/// An Apple device reconnecting as the buds go back in can reset this
/// laptop's link while it plays (XPS, 2026-10-24 10:03). The pods then hold
/// both, so let that connection settle and rejoin alongside it.
const REJOIN_DELAY_MS: u64 = 2_000;
/// One automatic rejoin per window. A second reset means the other device
/// insists, and the next play is the user's call.
const REJOIN_WINDOW_MS: u64 = 20_000;

#[cfg(test)]
use crate::pods::lifecycle::{
    RETRY_MS as TRANSPORT_RETRY_MS, RETRY_WINDOW_MS as TRANSPORT_RETRY_WINDOW_MS,
};

/// A first device list this soon after the Bluetooth link came up shows
/// who was on the pods before this laptop joined.
const JOIN_WITNESS_MS: u64 = 30_000;

/// An incoming invitation held aside across a fleet reset (see
/// [`Daemon::take_invitation`]).
pub(crate) struct InvitationSave {
    enrollment: Enrollment,
    offer: Option<Vec<u8>>,
    pake: Option<Pake>,
}

impl Daemon {
    pub fn load_apple_names(&mut self) {
        self.apple_labels = crate::devices::load();
    }

    pub fn new(local_name: impl Into<String>) -> Self {
        Self {
            session: session::Session::default(),
            local_name: local_name.into(),
            local_address: None,
            config: Config::new(),
            bluetooth_bonded: None,
            connection: Connection::new(),
            accessory: Accessory::new(),
            tracked: std::collections::BTreeMap::new(),
            nearby_pods: crate::pods::nearby::Nearby::default(),
            audio: Audio::new(),
            discovery: Discovery::new(),
            peers: Peers::new(),
            membership: Membership::new(),
            enrollment: Enrollment::new(),
            routing: Routing::new(),
            microphone: Microphone::new(),
            mic_path: std::sync::Arc::new(crate::audio::queue::MicPath::new()),
            mic_decoder: None,
            mic_started: false,
            mic_source: None,
            outbox: Vec::new(),
            pake: None,
            offer: None,
            keyring_locked: false,
            roster_seq: 0,
            effects: PendingEffects { announce_dirty: true, ..Default::default() },
            commit: crate::fleet::commit::Commit::new(),
            priority_until: None,
            priority_generation: 0,
            seat: Seat::new(),
            slot_hold_until: None,
            pair_audio_owner: None,
            reopen_wait: false,
            reopen_hold_until: None,
            lifecycle: crate::pods::lifecycle::Lifecycle::default(),
            staged_controls: Vec::new(),
            peers_seated: Vec::new(),
            output: crate::audio::route::Router::new(),
            last_graph: None,
            output_health: crate::audio::health::Health::default(),
            transport_active: false,
            advertised_self_at: [None, None],
            radio_inbox: crate::fleet::radio::Inbox::new(),
            outbound: crate::fleet::radio::Schedule::new(),
            presence_air: crate::fleet::airtime::PresenceAir::new(),
            listening: false,
            suspending: false,
            adapter_down: false,
            radio_silenced: false,
            air_session: {
                let mut bytes = [0u8; 16];
                let _ = os::random_bytes(&mut bytes);
                bytes
            },
            session_fleet: None,
            roster_air: crate::fleet::airtime::RosterAir::new(),
            roster_change_queued: None,
            roster_dirty: false,
            local_use: None,
            held_peers: Vec::new(),
            held_reporter: None,
            pod_peers: Vec::new(),
            pod_peers_known: false,
            pod_list_seen: false,
            pod_peers_at: None,
            pod_order: Vec::new(),
            pod_order_seen: false,
            acl_up_at: None,
            roster_probe: None,
            roster_probe_after: None,
            displaced: false,
            case_opened_at: None,
            apple_labels: std::collections::HashMap::new(),
            apple_labels_dirty: false,
            introduced_peers: (Generation(0), Vec::new()),
            peer_media_sent: std::collections::HashMap::new(),
            peer_playback: std::collections::HashMap::new(),
            idle_claimed: None,
            activity_at: None,
            worn: None,
            inserted_at: None,
            activity_preference: crate::fleet::activity::Preference::default(),
            pod_state: crate::fleet::pod_state::Sharing::default(),
            route_seq: 0,
            route_air_tx: None,
            route_air_at: None,
            route_wait_from: None,
            route_water: Vec::new(),
            route_seen: Vec::new(),
            route_reply: None,
            pending_admission: None,
            route_packet: None,
            route_block: None,
            route_connection: None,
        }
    }

    pub fn local_name(&self) -> &str {
        &self.local_name
    }

    pub fn select_accessory(&mut self, address: Option<Address>) {
        if self.config.accessory != address {
            self.bluetooth_bonded = None;
            self.activity_preference.select_accessory(address);
            self.pod_state.scope(None);
            self.outbound.clear_pod_state();
            self.effects.select_accessory = Some(address);
        }
        self.config.accessory = address;
    }

    /// Make another known accessory the selected one: its tracked report
    /// and saved prefs become live, the outgoing pair's report and prefs
    /// are kept, and the platform is told to disconnect the outgoing pair
    /// and follow the new selection.
    pub(crate) fn activate_accessory(&mut self, address: Address, now: Clock) {
        if self.config.accessory == Some(address) {
            return;
        }
        let switched = self.config.accessory.is_some();
        if let Some(old) = self.config.accessory {
            let prefs = self.config.live_prefs();
            self.config.saved_prefs.insert(old, prefs);
            let mut report = std::mem::take(&mut self.accessory);
            report.drop_control_reports();
            self.tracked.insert(old, report);
            if self.connection.stage() != Stage::Idle && self.connection.stage() != Stage::Waiting {
                self.effects.disconnect_accessory = Some(old);
            }
        }
        if let Some(report) = self.tracked.remove(&address) {
            self.accessory = report;
        } else if let Some(report) = self.nearby_pods.report_for_pairing(address, now.mono) {
            self.accessory = report;
        }
        match self.config.saved_prefs.remove(&address) {
            Some(prefs) => self.config.apply_prefs(prefs),
            // Switching between pairs: a pair never configured starts
            // from defaults, not from the outgoing pair's choices.
            // The first-ever selection keeps whatever was set before it.
            None if switched => self.config.apply_prefs(crate::pods::prefs::PairPrefs::new()),
            None => {}
        }
        self.accessory.pairing_at = None;
        // The switch gesture supersedes holds earned against the previous
        // pair and slot: the newly selected pair may pre-connect right
        // away instead of waiting out the old pair's holds.
        self.slot_hold_until = None;
        self.reopen_wait = false;
        self.reopen_hold_until = None;
        self.select_accessory(Some(address));
    }

    pub fn set_local_address(&mut self, address: Option<Address>) {
        self.local_address = address;
        self.note_radio_source();
    }

    /// Split any letter that still has its original bytes. A missing id leaves it whole.
    pub(crate) fn note_radio_source(&mut self) {
        let Some(source) = self.local_address else {
            return;
        };
        self.outbound.note_source(source, || {
            let mut id = [0u8; 8];
            os::random_bytes(&mut id).ok().map(|_| id)
        });
    }

    pub fn local_address(&self) -> Option<Address> {
        self.local_address
    }

    pub fn set_identity(&mut self, name: impl Into<String>, sku: &str) {
        self.accessory.set_identity(name, sku);
    }

    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    pub fn enrollment(&self) -> &Enrollment {
        &self.enrollment
    }

    pub fn membership(&self) -> &Membership {
        &self.membership
    }

    pub fn routing(&self) -> &Routing {
        &self.routing
    }

    pub fn accessory(&self) -> &Accessory {
        &self.accessory
    }

    pub fn audio(&self) -> &Audio {
        &self.audio
    }

    pub fn microphone(&self) -> &Microphone {
        &self.microphone
    }

    pub fn status_value(&self, now: Clock) -> Value {
        let resolved = self.resolved(now);
        let telemetry = self.fleet_pod_state(now);
        let projected = telemetry.map(|packet| {
            crate::fleet::pod_state::Sharing::project(
                packet,
                &self.accessory,
                self.connection.stage() == Stage::Ready,
                now,
            )
        });
        let mut snapshot = self.snapshot(now, &resolved);
        if let Some((accessory, _)) = &projected {
            snapshot.accessory = accessory;
        }
        let mut value = status::project(&snapshot);
        value["fleet_pod_state"] = telemetry
            .map(|packet| {
                serde_json::json!({
                    "reporter": packet.identity.reporter.display(),
                    "sequence": packet.sequence,
                    "sent_ms": packet.sent.0,
                    "age_ms": now.unix.0.saturating_sub(packet.sent.0),
                    "used_fields": projected.as_ref().map(|(_, used)| used),
                })
            })
            .unwrap_or(Value::Null);
        value["fleet_preference"] = self
            .preferred_connector()
            .map(|record| {
                serde_json::json!({
                    "device": record.owner.display(), "activity_ms": record.at.0,
                    "local": Some(record.owner) == self.local_address,
                })
            })
            .unwrap_or(Value::Null);
        value["connection_lifecycle"] = status::lifecycle(&self.lifecycle.progress());
        value["audio_transport_active"] = serde_json::json!(self.transport_active);
        value["nearby_airpods"] = self.nearby_pods.status(now.mono);
        value["audio_output_health"] = self.output_health_status();
        value["pairing_candidates"] = serde_json::json!(if self.config.accessory.is_none() {
            self.nearby_pods.pairing_count(now.mono)
        } else {
            0
        });
        // Every known accessory, selected first in spirit: the selected
        // entry carries `selected: true`; the flat fields above remain
        // its legacy projection for existing consumers.
        let mut accessories = Vec::new();
        if let Some(address) = self.config.accessory {
            accessories.push(status::accessory_entry(
                address,
                &self.accessory,
                true,
                self.membership.accessory_keys_ready(address),
                now,
            ));
        }
        for (address, report) in &self.tracked {
            accessories.push(status::accessory_entry(
                *address,
                report,
                false,
                self.membership.accessory_keys_ready(*address),
                now,
            ));
        }
        // Accessories we hold keys for but have not heard from yet are
        // still known: they appear with empty telemetry.
        for address in self.membership.accessory_key_map().keys() {
            let known =
                self.config.accessory == Some(*address) || self.tracked.contains_key(address);
            if !known {
                accessories.push(status::accessory_entry(
                    *address,
                    &Accessory::new(),
                    false,
                    true,
                    now,
                ));
            }
        }
        value["accessories"] = serde_json::json!(accessories);
        value
    }

    pub fn status_line(&self, now: Clock) -> String {
        let mut text = self.status_value(now).to_string();
        text.push('\n');
        text
    }

    pub fn handle(&mut self, command: Command, now: Clock) -> Response {
        match command {
            Command::Status => Response::line(self.status_line(now)),
            // The platform must confirm deletion before clearing the core.
            Command::FleetReset => Response::line("error: fleet reset requires the keyring worker"),
            Command::Activity => self.activity(None, now),
            Command::ActivityAt(at) => self.activity(Some(at), now),
            Command::Connect => self.connect(now),
            Command::AudioReset => self.reset_audio_output(),
            Command::ReconnectPeer => self.reconnect_peer(now),
            Command::HandoffRelease => self.release(now),
            Command::HandoffClaim => self.claim_handoff(now),
            Command::Noise(mode) => {
                self.request_noise(mode);
                Response::none()
            }
            Command::Adaptive(level) => {
                self.request_adaptive(level);
                Response::none()
            }
            Command::Conversation(enabled) => {
                self.request_conversation(enabled);
                Response::none()
            }
            Command::OneBud(enabled) => {
                self.request_one_bud(enabled);
                Response::none()
            }
            Command::Ear(behavior) => {
                self.config.ear = behavior;
                Response::none()
            }
            Command::Fleet(Ok(command)) => self.fleet(command, now),
            Command::Fleet(Err(())) => Response::line("error: invalid fleet JSON"),
            Command::Playback(Ok(event)) => self.playback(&event, now),
            Command::Playback(Err(())) => Response::line("invalid_json"),
            Command::Reopen => Response::line("error: this daemon runs headless and has no window"),
            Command::ScanPause | Command::ScanResume => {
                Response::line("error: discovery control is not available")
            }
            Command::Disconnect => {
                self.disconnect_session(now);
                Response::line("ok")
            }
            Command::Forget => self.forget(now),
            Command::Select(address) => {
                if self.config.accessory == Some(address) {
                    Response::line("ok")
                } else if self.tracked.contains_key(&address)
                    || self.membership.accessory_keys_ready(address)
                {
                    self.activate_accessory(address, now);
                    Response::line("ok")
                } else {
                    Response::line("error: unknown accessory")
                }
            }
            Command::NoiseCycle => {
                self.cycle_noise();
                Response::none()
            }
            Command::Solo => self.priority_solo(now),
            Command::Priority(peer) => self.priority_peer(&peer, false, now),
            Command::Incoming(Ok(request)) => self.incoming_route(&request, now),
            Command::Incoming(Err(())) => self.incoming_reply(false, "invalid_json", 0),
            Command::Unknown => Response::none(),
        }
    }

    pub fn note_bluez(&mut self, up: bool) {
        self.connection.note_bluez(up);
    }

    /// Join order lives as long as the Bluetooth link. A link that comes up
    /// while this daemon watches marks the moment this laptop joined.
    pub(crate) fn note_acl(&mut self, up: bool, now: crate::types::UnixMs) {
        if !up {
            self.pod_order.clear();
            self.pod_order_seen = false;
            self.acl_up_at = None;
        } else if self.connection.bluez() == Some(false) {
            self.acl_up_at = Some(now);
        }
    }

    /// Put a device list from the pods in join order. Devices already there
    /// when this laptop joined are older; later arrivals go last.
    fn join_order(&mut self, peers: &[Address], now: crate::types::UnixMs) -> Vec<Address> {
        self.pod_order.retain(|peer| peers.contains(peer));
        if self.pod_order.is_empty() {
            let joined =
                self.acl_up_at.is_some_and(|at| now.0.saturating_sub(at.0) <= JOIN_WITNESS_MS);
            let local = self.local_address.filter(|local| joined && peers.contains(local));
            self.pod_order = peers.iter().copied().filter(|peer| Some(*peer) != local).collect();
            if let Some(local) = local {
                self.pod_order.push(local);
            } else {
                // Restarted onto a live link: an order both laptops compute
                // alike until the other laptop's roster says otherwise.
                self.pod_order.sort_by_key(|peer| peer.bytes());
            }
            self.pod_order_seen = local.is_some() || peers.len() < 2;
        } else {
            if self.pod_order.len() < 2 {
                self.pod_order_seen = true;
            }
            for peer in peers {
                if !self.pod_order.contains(peer) {
                    self.pod_order.push(*peer);
                }
            }
        }
        self.pod_order.clone()
    }

    /// A laptop that did not see the order form takes the other laptop's.
    fn follow_roster_order(&mut self, reported: &[Address]) {
        let same = reported.len() == self.pod_order.len()
            && reported.iter().all(|peer| self.pod_order.contains(peer));
        if self.pod_order_seen || !same || reported == self.pod_order.as_slice() {
            return;
        }
        self.pod_order = reported.to_vec();
        if self.pod_peers.len() == reported.len()
            && reported.iter().all(|peer| self.pod_peers.contains(peer))
        {
            self.pod_peers = reported.to_vec();
            self.roster_dirty = true;
            self.effects.announce_dirty = true;
        }
    }

    pub fn note_profile_pending(&mut self, pending: bool) {
        self.connection.note_profile_pending(pending);
    }

    /// The other device sharing the pods with this laptop, when the pods
    /// named exactly two and this laptop is one of them.
    fn pod_pair_other(&self) -> Option<Address> {
        let local = self.local_address?;
        if !self.pod_peers_known || self.pod_peers.len() != 2 || !self.pod_peers.contains(&local) {
            return None;
        }
        self.pod_peers.iter().copied().find(|peer| *peer != local)
    }

    /// The pods ended the link while this laptop and one Apple device were the pair.
    /// A fleet laptop in that pair is a handoff, not a playback wait.
    fn consider_apple_displacement(&mut self) {
        if self.displaced || self.seat.held() || self.routing.phase().busy() {
            return;
        }
        let Some(other) = self.pod_pair_other() else {
            return;
        };
        if !self.membership.contains(other) {
            self.displaced = true;
        }
    }

    /// The link dropped while this laptop shared the pods with a fleet
    /// laptop that owned the audio. When the pods come back (case opens,
    /// wake), both laptops would race back on; the non-owner holds its
    /// automatic reconnect so the owner lands first.
    fn consider_reopen_order(&mut self) {
        let Some(other) = self.pod_pair_other() else {
            return;
        };
        if self.membership.contains(other) && self.pair_audio_owner == Some(other) {
            self.reopen_wait = true;
            self.reopen_hold_until = None;
        }
    }

    /// Whether the automatic reconnect is currently held for the pair's
    /// audio owner. The grace window starts at the first held attempt;
    /// after it, this laptop may attach anyway (the owner may be gone).
    fn reopen_blocked(&mut self, now: MonoMs) -> bool {
        if !self.reopen_wait {
            return false;
        }
        let until =
            *self.reopen_hold_until.get_or_insert_with(|| now.saturating_add(REOPEN_HOLD_MS));
        if now.0 < until.0 {
            true
        } else {
            self.reopen_wait = false;
            self.reopen_hold_until = None;
            false
        }
    }

    /// Passive ACL attachment and desktop activity share the same slot hold.
    /// An explicit Connect or a new Play request can still reconnect immediately.
    fn background_reopen_blocked(&mut self, now: MonoMs) -> bool {
        if self.slot_hold_until.is_some_and(|until| now.0 < until.0) {
            return true;
        }
        self.slot_hold_until = None;
        self.reopen_blocked(now)
    }

    /// Start the wake grace when the controller can be used again.
    /// Returns whether a reconnect is waiting. Otherwise the thread may attach now.
    pub(crate) fn wake_link(&mut self, at: MonoMs) -> bool {
        if self.radio_quiet() {
            return false;
        }
        self.connection.arm_resume(at.saturating_add(crate::pods::link::RESUME_GRACE_MS))
    }

    /// A controller or bluetoothd came back. Reconnect when this link was up.
    /// Otherwise let the thread attach a socket that is already connected.
    pub(crate) fn note_link_returned(&mut self, at: MonoMs) {
        if self.radio_quiet() || self.connection.reason() == "link_lost" || self.wake_link(at) {
            return;
        }
        self.effects.follow_link = true;
    }

    pub(crate) fn radio_quiet(&self) -> bool {
        self.suspending || self.adapter_down
    }

    pub fn note_ownership(&mut self, local: bool, at: MonoMs) {
        self.accessory.note_ownership(local, at);
        // Remember who owned the audio in a two-laptop pair, so a later
        // drop can let that owner reconnect first.
        if let (Some(local_address), Some(other)) = (self.local_address, self.pod_pair_other()) {
            if self.membership.contains(other) {
                self.pair_audio_owner = Some(if local { local_address } else { other });
            }
        }
        if local && self.idle_claimed.is_some_and(|(_, requested)| at >= requested) {
            self.idle_claimed = None;
            self.audio.clear_yield();
            self.seat.mark_local_owned();
        }
    }

    pub fn note_source(&mut self, local: bool, kind: u8, at: MonoMs) {
        self.idle_claimed = None;
        self.accessory.note_source(local, kind, at);
        // Source categories can become active while the desktop remains
        // paused. Refresh our self-report so peers do not mistake that
        // category for a new playback event after an older pause report.
        if local {
            self.peer_media_sent.clear();
        }
    }

    pub fn note_remote(&mut self, address: Address, at: MonoMs) {
        self.accessory.note_remote_address(address, at);
    }

    pub fn note_pod(&mut self, right: bool, pod: Pod, at: MonoMs) {
        let was_docked = self.accessory.both_docked();
        self.accessory.note_pod(right, pod, at);
        if self.accessory.both_docked() != was_docked {
            self.effects.announce_dirty = true;
        }
        self.note_ears(at);
        self.enforce_connection_admission(at);
    }

    /// An insertion is a bud going in after both were out for a second. The
    /// first reading after a restart or a new link is not one, and neither is
    /// a pod briefly misreporting the other (XPS, 2026-10-19: four flips in 0.4 s).
    fn note_wearing(&mut self, at: MonoMs) {
        const OUT_SETTLE_MS: u64 = 1_000;
        let Some(worn) = self.accessory.wearing() else { return };
        if !worn {
            self.inserted_at = None;
        }
        match self.worn {
            Some((previous, _)) if previous == worn => {}
            Some((false, since)) if at.saturating_since(since) >= OUT_SETTLE_MS => {
                self.inserted_at = Some(at);
                self.worn = Some((true, at));
            }
            _ => self.worn = Some((worn, at)),
        }
    }

    pub(crate) fn pairing_elsewhere(&self, now: MonoMs) -> bool {
        self.config.accessory.is_some_and(|address| self.membership.accessory_keys_ready(address))
            && self.accessory.pairing_at.is_some_and(|at| now.saturating_since(at) < 3_000)
    }

    pub fn note_noise_report(&mut self, mode: NoiseMode, at: MonoMs) {
        self.accessory.note_noise(mode, at);
    }

    pub fn note_roster(&mut self, report: RosterReport) -> Ingest {
        self.peers.ingest(report, self.config.accessory)
    }

    pub fn observe_nearby(
        &mut self,
        host: HostId,
        address: Address,
        name: impl Into<String>,
        sent: crate::types::UnixMs,
        authenticated: bool,
        session: Option<[u8; 16]>,
    ) {
        self.discovery.observe(host, address, name, sent, authenticated, session);
    }

    pub(crate) fn read_presence(
        &self,
        bytes: &[u8],
        now: crate::types::UnixMs,
    ) -> Option<crate::fleet::presence::Sight> {
        let fleet = self.membership.fleet().map(|fleet| crate::fleet::presence::FleetKeys {
            id: fleet.id.bytes(),
            key: *fleet.key.as_bytes(),
        });
        crate::fleet::presence::observe(bytes, self.local_address, now, fleet.as_ref())
    }

    /// A fleet letter from an unenrolled peer is also a sighting: the peer is
    /// demonstrably nearby and talking, even when its presence advertisements
    /// are not being heard. Without this, such a peer never reaches the tray.
    pub(crate) fn note_letter_peer(&mut self, from: Address, name: Option<String>, now: Clock) {
        if self.local_address == Some(from) || self.membership.contains(from) {
            return;
        }
        let name = name
            .filter(|name| crate::types::valid_name(name))
            .or_else(|| {
                self.discovery
                    .hosts()
                    .iter()
                    .find(|host| host.address == from)
                    .map(|host| host.name.clone())
            })
            .or_else(|| {
                let (peer, peer_name) = self.enrollment.peer()?;
                (peer == from).then(|| peer_name.to_string())
            });
        let Some(name) = name.filter(|name| !name.is_empty()) else {
            return;
        };
        self.observe_nearby(host_id(Some(from)), from, name, now.unix, false, None);
    }

    /// Keep a letter peer nearby while its fragments keep arriving. The
    /// inbox dedupes the completed letter, so without this the sighting
    /// lapses 30 s after the first assembly even though the peer is still
    /// transmitting the same packet.
    pub(crate) fn refresh_letter_peer(&mut self, from: Address, now: Clock) {
        if self.local_address == Some(from) || self.membership.contains(from) {
            return;
        }
        let Some(name) = self
            .discovery
            .hosts()
            .iter()
            .find(|host| host.address == from)
            .map(|host| host.name.clone())
        else {
            return;
        };
        self.observe_nearby(host_id(Some(from)), from, name, now.unix, false, None);
    }

    /// A saved member may advertise that it has reset its enrollment. Keep
    /// that public UI hint separate from membership; fresh sealed presence
    /// still takes precedence in discovery.
    pub(crate) fn hear_presence(&mut self, sight: crate::fleet::presence::Sight) {
        if !sight.authenticated
            && sight.enrolled != Some(false)
            && self.membership.contains(sight.address)
        {
            return;
        }
        let host = host_id(Some(sight.address));
        self.discovery.observe_presence(sight, host);
    }

    /// Queue one presence frame when the refresh interval has passed.
    /// An enrolled host airs only sealed presence, on the three-second
    /// cadence, handoff or idle. Only a host with no fleet loaded airs the
    /// public setup frame, so fleet members stay invisible to unenrolled
    /// scanners. Completion replies use the same cadence. Losing a presence
    /// packet must not make the peer wait for a later cycle to validate us.
    pub(crate) fn offer_presence(&mut self, now: Clock) {
        let Some(address) = self.local_address else {
            self.outbound.clear_presence();
            return;
        };
        let mark = self.presence_plan(address, now.mono);
        let Some(private) =
            self.presence_air.due(&mark, now.mono, crate::fleet::presence::REFRESH_MS)
        else {
            return;
        };
        let frame = match self.membership.fleet() {
            Some(fleet) if private => {
                let keys = crate::fleet::presence::FleetKeys {
                    id: fleet.id.bytes(),
                    key: *fleet.key.as_bytes(),
                };
                crate::fleet::presence::advertise_sealed(
                    address,
                    &self.local_name,
                    now.unix,
                    &keys,
                    mark.session,
                )
            }
            _ => crate::fleet::presence::advertise_setup(
                address,
                &self.local_name,
                now.unix,
                if self.keyring_locked { None } else { Some(self.membership.fleet().is_some()) },
            ),
        };
        match frame {
            Some(frame) if mark.handoff => self.outbound.offer_handoff_presence(frame),
            Some(frame) => self.outbound.offer(frame),
            None => self.outbound.clear_presence(),
        }
    }

    fn presence_plan(
        &mut self,
        address: Address,
        now: MonoMs,
    ) -> crate::fleet::airtime::PresenceMark {
        let fleet = self.membership.fleet().map(|fleet| fleet.id);
        let session = fleet.map(|id| self.routing_session(Some(id)));
        let handoff = self.routing.phase().busy() || self.outbound.has_live_route(now);
        crate::fleet::airtime::PresenceMark {
            address,
            name: self.local_name.clone(),
            fleet,
            session,
            handoff,
        }
    }

    fn routing_session(&mut self, fleet: Option<FleetId>) -> [u8; 16] {
        if self.session_fleet != fleet {
            self.session_fleet = fleet;
            let mut bytes = [0u8; 16];
            if os::random_bytes(&mut bytes).is_err() {
                bytes = [0; 16];
            }
            self.air_session = bytes;
        }
        self.air_session
    }

    /// Queue one roster when the fleet, the accessory, and the refresh allow it.
    /// Changed rosters get the next non-pairing slot and a short repeat burst.
    pub(crate) fn offer_roster(&mut self, now: Clock) {
        let Some(snapshot) = self.roster_snapshot(now) else {
            self.outbound.clear_roster();
            return;
        };
        let mark = crate::fleet::airtime::RosterMark {
            fleet: snapshot.fleet,
            session: snapshot.session,
            accessory: snapshot.accessory,
            peers: snapshot.peers.clone(),
            audio_source: snapshot.audio_source,
            pending: snapshot.pending,
            audio_idle: snapshot.audio_idle,
            intent: snapshot.intent,
        };
        let assessment =
            self.roster_air.assess(&mark, now.mono, crate::fleet::presence::REFRESH_MS);
        let peers_changed = assessment.peers_changed;
        let changed = assessment.changed;
        if !changed && (!assessment.aged || self.outbound.has_roster()) {
            return;
        }
        let mut air = snapshot.air;
        air.sequence = self.roster_air.next_seq();
        let Some(fleet) = self.membership.fleet() else {
            self.outbound.clear_roster();
            return;
        };
        let keys =
            crate::fleet::presence::FleetKeys { id: fleet.id.bytes(), key: *fleet.key.as_bytes() };
        match crate::fleet::roster_air::advertise(&air, &keys) {
            Some(frame) => {
                if peers_changed {
                    let names = snapshot
                        .peers
                        .iter()
                        .map(|address| address.display())
                        .collect::<Vec<_>>()
                        .join(" ");
                    eprintln!("fleetpodsd: telling the fleet the roster is {names}");
                }
                if changed && self.roster_air.has_mark() {
                    self.roster_change_queued = Some(now.mono);
                    if snapshot.intent {
                        self.outbound.offer_roster_intent(frame);
                    } else {
                        self.outbound.offer_roster_change(frame);
                    }
                } else {
                    self.outbound.offer_roster(frame);
                }
                self.roster_air.note_queued(mark);
            }
            None => self.outbound.clear_roster(),
        }
    }

    fn roster_snapshot(&mut self, now: Clock) -> Option<PlannedRoster> {
        let reporter = self.local_address?;
        let fleet = self.membership.fleet()?.id;
        let accessory = self.config.accessory?;
        let session = self.routing_session(Some(fleet));
        if session == [0; 16] || now.unix.0 == 0 {
            return None;
        }
        let peers = self.roster_peers(now);
        let (audio_source, audio_idle) = self.roster_audio(now);
        let audio_source = audio_source.filter(|owner| peers.contains(owner));
        let audio_idle = audio_idle && audio_source.is_some();
        if let (Some(report), Some(at)) =
            (self.accessory.source.known(), self.accessory.source.at())
        {
            if report.local {
                let observed = now.unix_at(at);
                if self.local_use.is_none_or(|prev| observed > prev) {
                    self.local_use = Some(observed);
                }
            }
        }
        let last_use = UnixMs(self.local_use.unwrap_or(UnixMs(0)).0.min(now.unix.0));
        // Intent airs while this laptop wants the accessory's audio and the
        // pods have not granted it: the current owner may step back before
        // the claim itself can arrive on a control link.
        let intent = self.roster_intent(now) && audio_source != Some(reporter);
        Some(PlannedRoster {
            fleet,
            session,
            accessory,
            peers: peers.clone(),
            audio_source,
            pending: false,
            audio_idle,
            intent,
            air: crate::fleet::roster_air::Snapshot {
                reporter,
                session,
                sequence: 0,
                sent: now.unix,
                accessory,
                peers,
                audio_source,
                last_use,
                pending: false,
                audio_idle,
                intent,
            },
        })
    }

    pub(crate) fn note_roster_aired(&mut self, now: MonoMs) {
        self.roster_air.note_aired(now);
        if let Some(queued) = self.roster_change_queued.take() {
            eprintln!(
                "fleetpodsd: changed roster published sequence={} queue_ms={}",
                self.roster_air.seq(),
                now.saturating_since(queued)
            );
        }
    }

    pub(crate) fn read_roster(&self, bytes: &[u8]) -> Option<crate::fleet::roster_air::Snapshot> {
        let fleet = self.membership.fleet()?;
        let keys =
            crate::fleet::presence::FleetKeys { id: fleet.id.bytes(), key: *fleet.key.as_bytes() };
        crate::fleet::roster_air::read(bytes, &keys)
    }

    pub(crate) fn hear_roster(&mut self, snapshot: crate::fleet::roster_air::Snapshot, now: Clock) {
        if Some(snapshot.reporter) == self.local_address
            || !crate::fleet::roster_air::fresh(snapshot.sent, now.unix)
            || !self.membership.contains(snapshot.reporter)
        {
            return;
        }
        let seated =
            self.discovery.refresh_session(snapshot.reporter, snapshot.session, snapshot.sent);
        if !seated {
            return;
        }
        let remembered = snapshot.peers.clone();
        let outcome = self.peers.ingest_air(
            RosterReport {
                reporter: host_id(Some(snapshot.reporter)),
                reporter_address: snapshot.reporter,
                accessory: snapshot.accessory,
                session: crate::types::SessionId(0),
                seq: snapshot.sequence,
                at: now.mono,
                sent: snapshot.sent,
                peers: snapshot.peers,
                audio_source: snapshot.audio_source,
                pending: snapshot.pending,
                audio_idle: snapshot.audio_idle,
                intent: snapshot.intent,
                wire_session: Some(snapshot.session),
                last_use: snapshot.last_use,
            },
            self.config.accessory,
        );
        if outcome == Ingest::Stored {
            eprintln!(
                "fleetpodsd: accepted fleet roster reporter={} age_ms={} pending={} peers={remembered:?}",
                snapshot.reporter,
                now.unix.0.saturating_sub(snapshot.sent.0),
                snapshot.pending
            );
            // Empty is an explicit disconnection, not a missing update. Keeping
            // the last nonempty list resurrects its laptop on our next setup.
            // Only accepted reports may update this cache; late/replayed or
            // other-accessory packets must not restore an obsolete seat.
            if !remembered.is_empty() {
                self.follow_roster_order(&remembered);
                self.held_peers = remembered.clone();
                self.held_reporter = Some(snapshot.reporter);
            } else if self.held_reporter == Some(snapshot.reporter) {
                self.held_peers.clear();
                self.held_reporter = None;
            }
            if !remembered.contains(&snapshot.reporter) {
                self.peers_seated.retain(|peer| *peer != snapshot.reporter);
            }
            self.probe_roster_conflict(snapshot.reporter, &remembered, snapshot.pending, now);
            // Only a stored report acts: the ingest watermark has already
            // dropped verbatim replays and stale sessions, so an intent
            // frame cannot step this laptop back twice.
            if snapshot.intent {
                self.note_fleet_intent(snapshot.reporter, now);
            }
        }
        if outcome != Ingest::Replay {
            self.roster_dirty = true;
        }
    }

    fn probe_roster_conflict(
        &mut self,
        reporter: Address,
        peers: &[Address],
        pending: bool,
        now: Clock,
    ) {
        let Some(local) = self.local_address else { return };
        if pending
            || self.connection.stage() != Stage::Ready
            || self.connection.apple_reconnect()
            || !self.pod_peers_known
            || self.pod_peers.len() != 2
            || !self.pod_peers.contains(&local)
            || peers.len() != 2
            || !peers.contains(&reporter)
            || peers.contains(&local)
            || self.roster_probe.is_some()
            || self.roster_probe_after.is_some_and(|at| now.mono < at)
        {
            return;
        }
        let generation = self.connection.generation();
        self.roster_probe = Some((generation, now.mono.saturating_add(ROSTER_PROBE_MS)));
        self.roster_probe_after = Some(now.mono.saturating_add(ROSTER_PROBE_INTERVAL_MS));
        // This existing read-only setup query elicits a control response without
        // claiming audio, changing settings, or restarting the handshake.
        self.effects.control_out.push((generation, crate::pods::setup::request_keys().to_vec()));
        eprintln!(
            "fleetpodsd: checking control liveness generation={} conflicting_reporter={reporter}",
            generation.0
        );
    }

    pub(crate) fn note_control_response(&mut self, generation: Generation) {
        if self.roster_probe.is_some_and(|(expected, _)| generation == expected) {
            self.roster_probe = None;
            eprintln!("fleetpodsd: control liveness confirmed generation={}", generation.0);
        }
    }

    fn expire_roster_probe(&mut self, now: Clock) {
        let Some((generation, deadline)) = self.roster_probe else { return };
        if generation != self.connection.generation() || self.connection.stage() != Stage::Ready {
            self.roster_probe = None;
            return;
        }
        if now.mono < deadline {
            return;
        }
        eprintln!(
            "fleetpodsd: silent control connection after roster conflict generation={}",
            generation.0
        );
        // Follow the ordinary lost-socket path: retire its generation, clear
        // local reports, and hold background reconnects while the peer settles.
        let was_busy = self.routing.phase().busy();
        self.note_socket(false, generation, now.mono);
        if !was_busy {
            self.step_back(now);
        }
        self.effects.drop_link = true;
    }

    pub(crate) fn import_roster_watermark(&mut self, bytes: &[u8]) {
        self.peers.import_air(bytes);
    }

    /// Keep one clock for the whole handoff, and repeat a request without changing it.
    pub(crate) fn queue_outgoing_route(&mut self, now: Clock) {
        self.expire_route(now);
        match self.routing.phase() {
            crate::fleet::route::Phase::WaitingAdmission => self.queue_request(now),
            crate::fleet::route::Phase::Connecting => self.queue_connection(now),
            crate::fleet::route::Phase::WaitingCompletion => self.resend_connection(now),
            _ => {}
        }
    }

    fn offer_route_frame(&mut self, frame: Vec<u8>, now: Clock) {
        let Some(message) = self.read_route(&frame) else {
            return;
        };
        if !crate::fleet::route_air::fresh(message.sent, now.unix) {
            return;
        }
        let remaining = message
            .sent
            .0
            .saturating_add(crate::fleet::route_air::LIFETIME_MS)
            .saturating_sub(now.unix.0);
        self.outbound.offer_route(frame, now.mono.saturating_add(remaining));
    }

    fn expire_route(&mut self, now: Clock) {
        if !self.routing.phase().busy() {
            self.route_wait_from = None;
            self.route_air_tx = None;
            self.route_air_at = None;
            self.route_packet = None;
            self.route_block = None;
            self.route_connection = None;
            return;
        }
        let tx = self.routing.tx();
        if self.route_air_tx != Some(tx) {
            self.route_air_tx = Some(tx);
            self.route_wait_from = Some(now.mono);
            self.route_air_at = None;
            self.route_packet = None;
            self.route_block = None;
            self.route_connection = None;
        }
        if self
            .route_wait_from
            .is_some_and(|at| now.mono.saturating_since(at) >= crate::fleet::route_air::LIFETIME_MS)
        {
            let _ = self.routing.fail(tx, "timed_out", now.unix);
            self.outbound.clear_route();
        }
    }

    fn queue_request(&mut self, now: Clock) {
        let due = self
            .route_air_at
            .is_none_or(|at| now.mono.saturating_since(at) >= crate::fleet::route_air::RETRY_MS);
        if !due {
            return;
        }
        if self.route_packet.is_none() {
            match self.seal_request(now, self.routing.tx()) {
                Ok(frame) => {
                    let peer =
                        self.routing.peer().map(|address| address.display()).unwrap_or_default();
                    eprintln!("fleetpodsd: asking {peer} for the AirPods");
                    self.route_block = None;
                    self.route_packet = Some(frame);
                }
                Err(reason) => {
                    if self.route_block != Some(reason) {
                        eprintln!("fleetpodsd: AirPods request not sent: {reason}");
                        self.route_block = Some(reason);
                    }
                    return;
                }
            }
        }
        let Some(frame) = self.route_packet.clone() else {
            return;
        };
        self.route_air_at = Some(now.mono);
        self.offer_route_frame(frame, now);
    }

    fn queue_connection(&mut self, now: Clock) {
        if self.connection.stage() != Stage::Ready {
            return;
        }
        if self.route_connection.is_none() {
            self.route_connection = self.seal_followup(
                crate::fleet::route_air::Kind::Connection,
                crate::fleet::route_air::Answer::Ok,
                now,
            );
        }
        let Some(frame) = self.route_connection.clone() else {
            return;
        };
        if self.routing.note_connecting(self.routing.tx()) {
            self.route_air_at = Some(now.mono);
            self.offer_route_frame(frame, now);
        }
    }

    fn resend_connection(&mut self, now: Clock) {
        let due = self
            .route_air_at
            .is_none_or(|at| now.mono.saturating_since(at) >= crate::fleet::route_air::RETRY_MS);
        let Some(frame) = self.route_connection.clone() else {
            return;
        };
        if !due {
            return;
        }
        self.route_air_at = Some(now.mono);
        self.offer_route_frame(frame, now);
    }

    /// Fleet members currently on the configured accessory.
    ///
    /// The audio source is not the whole seat. Another laptop that announced
    /// itself seated stays beside this one when playback starts here.
    fn seated_members(&self) -> Vec<Address> {
        let Some(reporter) = self.local_address else {
            return Vec::new();
        };
        let Some(accessory) = self.config.accessory else {
            return Vec::new();
        };
        let mut peers = Vec::new();
        let add = |peers: &mut Vec<Address>, peer: Address| {
            if peer != accessory
                && self.membership.contains(peer)
                && !peers.contains(&peer)
                && peers.len() < 2
            {
                peers.push(peer);
            }
        };
        if self.connection.stage() == Stage::Ready {
            add(&mut peers, reporter);
        }
        for peer in &self.peers_seated {
            add(&mut peers, *peer);
        }
        for peer in &self.held_peers {
            add(&mut peers, *peer);
        }
        if let Some(remote) = self.accessory.remote_address.as_fresh().copied() {
            add(&mut peers, remote);
        }
        peers
    }

    /// Who this laptop tells the fleet is on the pods.
    ///
    /// Once the pods have sent their device list, that list is the roster.
    /// A laptop we used to hear is not kept on it. Before the first list, the
    /// seated members stand in so a playback roster can still name a neighbor.
    fn roster_peers(&self, now: Clock) -> Vec<Address> {
        // Once the pods' list has been seen, a drop resets the saved
        // roster: the remembered seated members stand in only before the
        // first list, never after one was dropped.
        if self.pod_list_seen {
            return self.pod_broadcast(now);
        }
        if !self.pod_peers_known {
            return self.seated_members();
        }
        self.pod_broadcast(now)
    }

    /// The pods' connected devices, published only while this laptop is one of
    /// at most two. Any other shape is an empty roster, matching the previous daemon.
    fn pod_broadcast(&self, now: Clock) -> Vec<Address> {
        let Some(local) = self.local_address else {
            return Vec::new();
        };
        if self.connection.stage() != Stage::Ready
            || !self.pod_peers.contains(&local)
            || self.pod_peers.len() > 2
        {
            return Vec::new();
        }
        // Two laptops connecting at once can each be told they are alone.
        // The other laptop's fresh roster completes the pair until the
        // pods' own list catches up (or supersedes this).
        let report =
            self.config.accessory.zip(self.held_reporter).and_then(|(accessory, reporter)| {
                self.peers
                    .fresh_from(now, accessory, reporter)
                    .map(|report| report.peers.as_slice())
            });
        crate::fleet::roster::complete_solo_list(
            &self.pod_peers,
            local,
            self.held_reporter,
            report.unwrap_or(&[]),
        )
    }

    pub(crate) fn note_pod_peers(&mut self, peers: Vec<Address>, now: crate::types::UnixMs) {
        let same = self.pod_peers.len() == peers.len()
            && peers.iter().all(|peer| self.pod_peers.contains(peer));
        if self.pod_peers_known && same {
            return;
        }
        let peers = self.join_order(&peers, now);
        self.pod_peers_known = true;
        self.pod_list_seen = true;
        self.peer_playback.retain(|peer, _| peers.contains(peer));
        self.pod_peers = peers;
        self.pod_peers_at = Some(now);
    }

    pub(crate) fn note_peer_playback(&mut self, peer: Address, playing: bool, now: Clock) {
        if self.local_address == Some(peer) {
            return;
        }
        // Accept media only from a device on this control session's peer list.
        if !self.pod_peers.contains(&peer) {
            return;
        }
        if self.peer_playback.get(&peer).is_none_or(|old| old.0 != playing) {
            eprintln!("fleetpodsd: peer {} media playing={playing}", peer.display());
            self.idle_claimed = None;
        }
        self.peer_playback.insert(peer, (playing, now.mono));
        self.contest_started_outside_playback(peer, playing, now);
        self.note_peer_stem_relay(peer, playing, now);
        self.effects.announce_dirty = true;
    }

    /// A peer's own playback state can update an older accessory source report.
    /// Third-party roster summaries cannot turn someone else's playback off.
    fn remote_playback(&self, peer: Address, now: Clock) -> Option<(bool, MonoMs)> {
        // The pods report a peer's playback when it changes, not on a timer, and these reports
        // are cleared when the link drops. On a live link the last one stands until the next.
        let linked = self.connection.stage() == Stage::Ready;
        let relay = self.peer_playback.get(&peer).copied().filter(|(_, at)| {
            linked || now.mono.saturating_since(*at) <= crate::pods::reports::REPORT_TTL_MS
        });
        let roster = self
            .config
            .accessory
            .and_then(|a| self.peers.fresh(now, a))
            .filter(|r| !r.pending && r.reporter_address == peer && r.audio_source == Some(peer))
            .map(|r| (!r.audio_idle, r.at));
        [relay, roster]
            .into_iter()
            .flatten()
            .max_by_key(|(_, at)| at.0)
            .filter(|(_, at)| self.accessory.source.at().is_none_or(|source| *at >= source))
    }

    fn roster_audio(&self, now: Clock) -> (Option<Address>, bool) {
        let resolved = self.resolved(now);
        // Publish accessory observations, not an inferred peer report under
        // our own name. Otherwise two laptops can keep a guessed owner alive.
        if resolved.source != crate::audio::seat::Source::Direct {
            return (None, false);
        }
        match resolved.holder {
            Holder::Local { playing } => (self.local_address, !playing),
            Holder::Remote { address, playing } => (address, !playing),
            Holder::Unknown => (None, false),
        }
    }

    pub(crate) fn learn_apple_name(&mut self, address: Address, label: &str) {
        if self.local_address == Some(address)
            || self.config.accessory == Some(address)
            || self.membership.contains(address)
        {
            return;
        }
        // A laptop on the pods announces `Omarchy - <host>`; that peer is
        // never an Apple device, and a name learned before this rule is dropped.
        if crate::devices::omarchy_label(label) {
            if self.apple_labels.remove(&address).is_some() {
                self.apple_labels_dirty = true;
            }
            return;
        }
        let Some(label) = crate::devices::valid_label(label) else {
            return;
        };
        if self.apple_labels.get(&address).is_some_and(|old| {
            old == label
                || (crate::devices::generic_label(label) && !crate::devices::generic_label(old))
        }) {
            return;
        }
        self.apple_labels.insert(address, label.to_owned());
        self.apple_labels_dirty = true;
    }

    pub(crate) fn take_apple_names(
        &mut self,
    ) -> Option<std::collections::HashMap<Address, String>> {
        std::mem::take(&mut self.apple_labels_dirty).then(|| self.apple_labels.clone())
    }

    pub(crate) fn introduce_peers(&mut self) {
        if self.connection.stage() != Stage::Ready || !self.pod_peers_known {
            return;
        }
        let Some(local) = self.local_address else {
            return;
        };
        if !self.pod_peers.contains(&local) {
            return;
        }
        let generation = self.connection.generation();
        if self.introduced_peers.0 != generation {
            self.introduced_peers = (generation, Vec::new());
            self.peer_media_sent.clear();
        }
        self.peer_media_sent.retain(|peer, _| self.pod_peers.contains(peer));
        self.introduced_peers.1.retain(|peer| self.pod_peers.contains(peer));
        let playing = self.audio.local_playing() && !self.accessory.playback_blocked();
        for peer in &self.pod_peers {
            if *peer == local {
                continue;
            }
            if !self.introduced_peers.1.contains(peer) {
                for packet in
                    crate::pods::peer::introduction(local, *peer, &self.local_name, playing)
                {
                    self.effects.control_out.push((generation, packet));
                }
                self.introduced_peers.1.push(*peer);
            } else if self.peer_media_sent.get(peer) != Some(&playing) {
                let packet =
                    crate::pods::peer::media_information(local, *peer, &self.local_name, playing);
                self.effects.control_out.push((generation, packet));
            }
            self.peer_media_sent.insert(*peer, playing);
        }
    }

    fn seal_request(&mut self, now: Clock, tx: u64) -> Result<Vec<u8>, &'static str> {
        let source = self.local_address.ok_or("no local address")?;
        let target = self.routing.peer().ok_or("no peer")?;
        let fleet = self.membership.fleet().ok_or("no fleet")?;
        let accessory = self.config.accessory.ok_or("no accessory")?;
        if !self.membership.contains(target) {
            return Err("peer is not a fleet member");
        }
        let fleet_id = fleet.id;
        let keys =
            crate::fleet::presence::FleetKeys { id: fleet.id.bytes(), key: *fleet.key.as_bytes() };
        let Some(target_session) = self.discovery.hosts().iter().find_map(|host| {
            (host.address == target && host.authenticated).then_some(host.session).flatten()
        }) else {
            return Err("peer has no routing session");
        };
        let source_session = self.routing_session(Some(fleet_id));
        if source_session == [0; 16] {
            return Err("local routing session is empty");
        }
        if now.unix.0 == 0 {
            return Err("clock is unset");
        }
        self.route_seq = self.route_seq.saturating_add(1);
        if self.route_seq == 0 {
            return Err("route sequence wrapped");
        }
        let message = crate::fleet::route_air::Message {
            kind: crate::fleet::route_air::Kind::Request,
            result: crate::fleet::route_air::Answer::Ok,
            source,
            target,
            source_session,
            target_session,
            sequence: self.route_seq,
            transaction: tx.to_be_bytes(),
            sent: now.unix,
            accessory,
            retain: None,
        };
        crate::fleet::route_air::advertise(&message, &keys).ok_or("route packet did not seal")
    }

    fn seal_followup(
        &mut self,
        kind: crate::fleet::route_air::Kind,
        result: crate::fleet::route_air::Answer,
        now: Clock,
    ) -> Option<Vec<u8>> {
        let source = self.local_address?;
        let target = self.routing.peer()?;
        let fleet = self.membership.fleet()?;
        let accessory = self.config.accessory?;
        let fleet_id = fleet.id;
        let keys =
            crate::fleet::presence::FleetKeys { id: fleet.id.bytes(), key: *fleet.key.as_bytes() };
        let target_session = self.discovery.hosts().iter().find_map(|host| {
            (host.address == target && host.authenticated).then_some(host.session).flatten()
        })?;
        let source_session = self.routing_session(Some(fleet_id));
        let retain = self.routing.retain();
        if source_session == [0; 16] || now.unix.0 == 0 || retain.is_none() {
            return None;
        }
        self.route_seq = self.route_seq.saturating_add(1);
        if self.route_seq == 0 {
            return None;
        }
        let message = crate::fleet::route_air::Message {
            kind,
            result,
            source,
            target,
            source_session,
            target_session,
            sequence: self.route_seq,
            transaction: self.routing.tx().to_be_bytes(),
            sent: now.unix,
            accessory,
            retain,
        };
        crate::fleet::route_air::advertise(&message, &keys)
    }

    pub(crate) fn read_route(&self, bytes: &[u8]) -> Option<crate::fleet::route_air::Message> {
        let fleet = self.membership.fleet()?;
        let keys =
            crate::fleet::presence::FleetKeys { id: fleet.id.bytes(), key: *fleet.key.as_bytes() };
        crate::fleet::route_air::read(bytes, &keys)
    }

    pub(crate) fn hear_route(&mut self, message: crate::fleet::route_air::Message, now: Clock) {
        use crate::fleet::route_air::{Answer, Kind};
        let Some(local) = self.local_address else {
            return;
        };
        if message.target != local || !crate::fleet::route_air::fresh(message.sent, now.unix) {
            return;
        }
        let Some(fleet) = self.membership.fleet().map(|fleet| fleet.id) else {
            return;
        };
        if message.target_session != self.routing_session(Some(fleet)) {
            eprintln!(
                "fleetpodsd: route from {} ignored: session does not match",
                message.source.display()
            );
            return;
        }
        if !self.membership.contains(message.source) {
            eprintln!(
                "fleetpodsd: route from {} ignored: not a fleet member",
                message.source.display()
            );
            return;
        }
        let known =
            self.discovery.refresh_session(message.source, message.source_session, message.sent);
        if !known {
            eprintln!(
                "fleetpodsd: route from {} ignored: sender session is not the one heard",
                message.source.display()
            );
            return;
        }
        if message.kind == Kind::Request {
            self.hear_request(message, now);
            return;
        }
        let tx = u64::from_be_bytes(message.transaction);
        if self.routing.tx() != tx || self.routing.peer() != Some(message.source) {
            return;
        }
        // Admissions, connections, and completions get no replay
        // protection from the request watermark, so remember each one
        // exactly: a captured sealed frame replayed verbatim inside its
        // freshness window must not flip an admitted handoff back to
        // failed. This is exact-frame dedupe, not a sequence watermark —
        // BLE advertisements genuinely reorder, and an older admission
        // can legitimately arrive after a newer busy answer.
        let seen = SeenRoute {
            source: message.source,
            session: message.source_session,
            tx,
            kind: message.kind,
            sequence: message.sequence,
        };
        if self.route_seen.contains(&seen) {
            return;
        }
        self.route_seen.push(seen);
        if self.route_seen.len() > 128 {
            self.route_seen.remove(0);
        }
        match (message.kind, message.result) {
            (Kind::Admission, Answer::Ok | Answer::VerifyConnection) => {
                eprintln!(
                    "fleetpodsd: {} admitted the AirPods request ({})",
                    message.source.display(),
                    message.result.text()
                );
                if self.routing.admit(tx, true, message.retain) == crate::fleet::route::Admit::Stale
                {
                    return;
                }
                // The peer dropped the link. Open ours. An outstanding
                // explicit switch claims on the new link; otherwise audio
                // stays unclaimed.
                if self.seat.user_intent_fresh(now.mono) {
                    self.claim_seat_user(now);
                    eprintln!("fleetpodsd: finishing an explicit user switch after admission");
                }
                self.open_connection(now.mono);
            }
            (Kind::Admission, Answer::Busy) => {
                let _ = self.routing.peer_busy(tx, now.mono);
            }
            (Kind::Admission, other) => {
                let _ = self.routing.fail(tx, other.text(), now.unix);
                self.background_admission_failed(other == Answer::Unavailable, now);
            }
            (Kind::Connection, _) => self.hear_connection(&message, now),
            (Kind::Complete, _) => {
                let alongside = self.routing.retain() == Some(message.source);
                let _ = self.routing.complete(tx, alongside, now.unix);
            }
            (Kind::Request, _) => {}
        }
    }

    /// A full seat can still admit a fleet laptop when this laptop holds
    /// one slot and the other is an idle device: this laptop stays, the
    /// priority pair becomes (this laptop, the requester), and the idle
    /// device gives up its slot. A playing remote device is never the one
    /// displaced, and a requester already seated needs no admission.
    fn full_seat_retain(
        seated: &[Address],
        local: Option<Address>,
        source: Address,
        holder: &Holder,
    ) -> Option<Address> {
        let local = local?;
        if seated.len() != 2 || !seated.contains(&local) || seated.contains(&source) {
            return None;
        }
        if matches!(holder, Holder::Remote { playing: true, .. }) {
            return None;
        }
        Some(local)
    }

    fn hear_request(&mut self, message: crate::fleet::route_air::Message, now: Clock) {
        use crate::fleet::route_air::Answer;
        let key = (message.source, message.source_session);
        if self.route_water.iter().any(|mark| {
            mark.source == key.0 && mark.session == key.1 && message.sequence <= mark.sequence
        }) {
            if let Some(saved) = &self.route_reply {
                if saved.source == key.0
                    && saved.session == key.1
                    && saved.sequence == message.sequence
                {
                    self.offer_route_frame(saved.packet.clone(), now);
                }
            }
            return;
        }
        // A laptop asking for a seat is not on the pods, whatever older
        // evidence (a remote source report, a held peer) still names it.
        let mut seated = self.seated_members();
        seated.retain(|peer| *peer != message.source);
        eprintln!(
            "fleetpodsd: fleet seat request from {} control={}",
            message.source,
            self.connection.stage().wire()
        );
        let (answer, retain, take) = if self.routing.phase().busy()
            || self.priority_until.is_some()
            || self.connection.apple_reconnect()
        {
            (Answer::Busy, None, false)
        } else if self.connection.stage() != Stage::Ready {
            (Answer::Unavailable, None, false)
        } else if let Holder::Remote { address: Some(peer), playing: true } =
            self.resolved(now).holder
        {
            if self.pod_peers_known
                && self.pod_peers.contains(&peer)
                && Some(peer) != self.local_address
                && peer != message.source
            {
                (Answer::Ok, Some(peer), true)
            } else {
                (Answer::RosterChanged, None, false)
            }
        } else if seated.len() == 1 {
            (Answer::Ok, Some(seated[0]), true)
        } else if let Some(local) = Self::full_seat_retain(
            &seated,
            self.local_address,
            message.source,
            &self.resolved(now).holder,
        ) {
            (Answer::Ok, Some(local), true)
        } else {
            (Answer::RosterChanged, None, false)
        };
        if !take {
            eprintln!(
                "fleetpodsd: fleet seat request from {} refused answer={answer:?} holder={:?} seated={seated:?} pod_peers={:?} known={}",
                message.source,
                self.resolved(now).holder,
                self.pod_peers,
                self.pod_peers_known
            );
        }
        if take {
            let tx = u64::from_be_bytes(message.transaction);
            if !self.routing.accept_request(message.source, tx) || !self.routing.grant(tx, retain) {
                if self.reply_admission(&message, Answer::Busy, None, now) {
                    self.remember_route(&message);
                }
                return;
            }
            if let Some(stay) = retain {
                if Some(stay) != self.local_address {
                    self.step_back(now);
                    self.effects.drop_link = true;
                    self.pending_admission = Some(PendingAdmission {
                        request: message.clone(),
                        until: now.mono.saturating_add(8_000),
                        release: Some(self.connection.generation()),
                    });
                    self.remember_route(&message);
                    eprintln!(
                        "fleetpodsd: waiting for Bluetooth release before admitting {}",
                        message.source
                    );
                    return;
                } else if self.pod_peers_known
                    && self.pod_peers.len() == 2
                    && !self.pod_peers.contains(&message.source)
                {
                    let packet = crate::pods::setup::priority(stay, Some(message.source)).unwrap();
                    self.queue_priority(packet, now);
                    self.pending_admission = Some(PendingAdmission {
                        request: message.clone(),
                        until: now.mono.saturating_add(5_000),
                        release: None,
                    });
                    self.remember_route(&message);
                    eprintln!(
                        "fleetpodsd: reserving pair {stay} {} before fleet admission",
                        message.source
                    );
                    return;
                }
            }
        }
        // Admission grants a connection slot. Only the peer's explicit
        // roster playback intent can make the current audio owner step back.
        if self.reply_admission(&message, answer, retain, now) {
            self.remember_route(&message);
        }
    }

    fn remember_route(&mut self, message: &crate::fleet::route_air::Message) {
        self.route_water.push(RouteMark {
            source: message.source,
            session: message.source_session,
            sequence: message.sequence,
        });
        if self.route_water.len() > 128 {
            self.route_water.remove(0);
        }
    }

    pub(crate) fn finish_admission(&mut self, result: crate::fleet::route_air::Answer, now: Clock) {
        let Some(pending) = self.pending_admission.take() else {
            return;
        };
        let request = pending.request;
        self.priority_until = None;
        let tx = u64::from_be_bytes(request.transaction);
        if self.routing.tx() != tx
            || self.routing.phase() != crate::fleet::route::Phase::WaitingConnection
        {
            return;
        }
        let retain = self.routing.retain();
        if result != crate::fleet::route_air::Answer::Ok {
            self.routing.fail(tx, result.text(), now.unix);
        }
        eprintln!(
            "fleetpodsd: fleet admission prerequisite {} for {}",
            result.text(),
            request.source
        );
        self.reply_admission(&request, result, retain, now);
    }

    pub(crate) fn note_link_released(
        &mut self,
        accessory: Address,
        generation: Generation,
        ok: bool,
        now: Clock,
    ) {
        if self.pending_admission.as_ref().is_some_and(|pending| {
            pending.release == Some(generation) && pending.request.accessory == accessory
        }) {
            let result = if ok {
                crate::fleet::route_air::Answer::Ok
            } else {
                crate::fleet::route_air::Answer::ConnectionFailed
            };
            self.finish_admission(result, now);
        }
    }

    pub(crate) fn note_pair_reserved(&mut self, now: Clock) {
        if self.pending_admission.as_ref().is_some_and(|pending| pending.release.is_none()) {
            self.finish_admission(crate::fleet::route_air::Answer::Ok, now);
        }
    }

    fn hear_connection(&mut self, message: &crate::fleet::route_air::Message, now: Clock) {
        if self.pending_admission.is_some() {
            return;
        }
        if self.routing.phase() != crate::fleet::route::Phase::WaitingConnection {
            if let Some(saved) = &self.route_reply {
                if saved.source == message.source
                    && saved.session == message.source_session
                    && saved.sequence == message.sequence
                {
                    self.offer_route_frame(saved.packet.clone(), now);
                }
            }
            return;
        }
        let tx = u64::from_be_bytes(message.transaction);
        if self.routing.tx() != tx || self.routing.peer() != Some(message.source) {
            return;
        }
        self.reply_followup(message, crate::fleet::route_air::Kind::Complete, now);
        // Retaining the Apple player replaces this laptop, not a join alongside it.
        let alongside = self.routing.retain() == Some(message.target);
        let _ = self.routing.complete(tx, alongside, now.unix);
    }

    fn reply_followup(
        &mut self,
        incoming: &crate::fleet::route_air::Message,
        kind: crate::fleet::route_air::Kind,
        now: Clock,
    ) {
        let Some(fleet) = self.membership.fleet() else {
            return;
        };
        let fleet_id = fleet.id;
        let keys =
            crate::fleet::presence::FleetKeys { id: fleet.id.bytes(), key: *fleet.key.as_bytes() };
        let source_session = self.routing_session(Some(fleet_id));
        self.route_seq = self.route_seq.saturating_add(1);
        if self.route_seq == 0 || now.unix.0 == 0 {
            return;
        }
        let message = crate::fleet::route_air::Message {
            kind,
            result: crate::fleet::route_air::Answer::Ok,
            source: incoming.target,
            target: incoming.source,
            source_session,
            target_session: incoming.source_session,
            sequence: self.route_seq,
            transaction: incoming.transaction,
            sent: now.unix,
            accessory: incoming.accessory,
            retain: incoming.retain,
        };
        let Some(packet) = crate::fleet::route_air::advertise(&message, &keys) else {
            return;
        };
        self.route_reply = Some(SavedRoute {
            source: incoming.source,
            session: incoming.source_session,
            sequence: incoming.sequence,
            packet: packet.clone(),
        });
        self.offer_route_frame(packet, now);
    }

    fn reply_admission(
        &mut self,
        request: &crate::fleet::route_air::Message,
        result: crate::fleet::route_air::Answer,
        retain: Option<Address>,
        now: Clock,
    ) -> bool {
        let Some(fleet) = self.membership.fleet() else {
            return false;
        };
        let fleet_id = fleet.id;
        let keys =
            crate::fleet::presence::FleetKeys { id: fleet.id.bytes(), key: *fleet.key.as_bytes() };
        let source_session = self.routing_session(Some(fleet_id));
        self.route_seq = self.route_seq.saturating_add(1);
        if self.route_seq == 0 || now.unix.0 == 0 {
            return false;
        }
        let message = crate::fleet::route_air::Message {
            kind: crate::fleet::route_air::Kind::Admission,
            result,
            source: request.target,
            target: request.source,
            source_session,
            target_session: request.source_session,
            sequence: self.route_seq,
            transaction: request.transaction,
            sent: now.unix,
            accessory: request.accessory,
            retain,
        };
        let Some(packet) = crate::fleet::route_air::advertise(&message, &keys) else {
            return false;
        };
        self.route_reply = Some(SavedRoute {
            source: request.source,
            session: request.source_session,
            sequence: request.sequence,
            packet: packet.clone(),
        });
        self.offer_route_frame(packet, now);
        true
    }

    pub(crate) fn take_roster_watermark(&mut self) -> Option<Vec<u8>> {
        if !self.roster_dirty {
            return None;
        }
        self.roster_dirty = false;
        Some(self.peers.export_air())
    }

    pub(crate) fn note_presence_aired(&mut self, now: MonoMs) {
        let Some(address) = self.local_address else {
            return;
        };
        let mark = self.presence_plan(address, now);
        self.presence_air.note_aired(mark, now);
    }

    pub fn set_profiles(&mut self, profiles: Vec<Profile>) {
        self.audio.set_offers(profiles);
    }

    pub fn activate_profile(&mut self) -> Option<&str> {
        let selected = self.audio.activate();
        selected
    }

    pub fn invite(
        &mut self,
        peer: Address,
        name: &str,
        code: Code,
        id: InvitationId,
        now: Clock,
    ) -> bool {
        let Some(local) = self.local_address else {
            return false;
        };
        self.enrollment.begin_invite(local, peer, name, code, id, now.mono)
    }

    pub fn deliver_invite(&mut self, id: InvitationId) -> bool {
        self.enrollment.mark_delivered(id)
    }

    pub fn receive_invite(
        &mut self,
        peer: Address,
        name: &str,
        id: InvitationId,
        now: Clock,
    ) -> bool {
        let Some(local) = self.local_address else {
            return false;
        };
        self.enrollment.receive_invite(local, peer, name, id, now.mono)
    }

    pub fn auth_rejected(&mut self, id: InvitationId, attempt: u8) -> bool {
        self.enrollment.auth_rejected(id, attempt)
    }

    pub fn auth_accepted(&mut self, id: InvitationId, proposal: Proposal) -> bool {
        self.enrollment.auth_accepted(id, proposal)
    }

    /// Call after the keyring write has returned. A `true` return is not a
    /// successful enrollment; the confirmation message still has to arrive.
    pub fn persist_enrollment(&mut self, id: InvitationId) -> Result<(), Reject> {
        let Some(proposal) = self.enrollment.proposal().cloned() else {
            return Err(Reject::Empty);
        };
        self.membership.apply(proposal)?;
        if !self.enrollment.note_persisted(id) {
            return Err(Reject::Empty);
        }
        Ok(())
    }

    pub fn confirm_enrollment(&mut self, id: InvitationId) -> bool {
        self.enrollment.note_confirmed(id)
    }

    pub fn set_accessory_keys(&mut self, address: Address, keys: AccessoryKeys) {
        self.membership.set_accessory_keys(address, keys);
    }

    /// Load the whole stored key map (startup / keyring unlock).
    pub fn set_accessory_key_map(
        &mut self,
        keys: std::collections::BTreeMap<Address, AccessoryKeys>,
    ) {
        for (address, keys) in keys {
            self.membership.set_accessory_keys(address, keys);
        }
    }

    /// The selected accessory's keys, if it has any.
    pub(crate) fn selected_keys(&self) -> Option<&AccessoryKeys> {
        self.config.accessory.and_then(|address| self.membership.accessory_keys(address))
    }

    pub fn mark_secrets_unavailable(&mut self) {
        self.membership.mark_unavailable();
    }

    pub fn start_route(&mut self, peer: Address) -> Start {
        self.routing.start(peer)
    }

    pub fn take_letters(&mut self) -> Vec<Outbound> {
        std::mem::take(&mut self.outbox)
    }

    pub fn set_keyring_locked(&mut self, locked: bool) {
        self.keyring_locked = locked;
    }

    pub fn keyring_locked(&self) -> bool {
        self.keyring_locked
    }

    pub(crate) fn queue_accessory_keys(&mut self) {
        if self.membership.any_accessory_keys() {
            let bytes = crate::linux::keyring::encode_keys(self.membership.accessory_key_map());
            self.effects.secret_out.push((crate::linux::keyring::KEYS_ITEM.into(), bytes));
        }
    }

    /// The keyring opened after a locked start: load what it holds, and save keys learned meanwhile.
    pub(crate) fn keyring_unlocked(
        &mut self,
        fleet: Option<Fleet>,
        keys: Option<std::collections::BTreeMap<Address, AccessoryKeys>>,
    ) {
        if !self.keyring_locked {
            return;
        }
        self.keyring_locked = false;
        self.membership.mark_unlocked();
        let restored = fleet.is_some();
        if let Some(fleet) = fleet {
            self.restore_fleet(fleet);
        }
        match keys {
            Some(keys) if !self.membership.any_accessory_keys() => self.set_accessory_key_map(keys),
            Some(_) => {}
            None => self.queue_accessory_keys(),
        }
        eprintln!(
            "fleetpodsd: the keyring is unlocked; {}",
            if restored { "the fleet is restored" } else { "it holds no fleet" }
        );
    }

    pub fn saved_fleet(&self) -> Option<Fleet> {
        self.membership.fleet().cloned()
    }

    pub(crate) fn fleet_without(&self, peer: Address) -> Result<Option<Fleet>, &'static str> {
        if self.local_address.is_none() {
            return Err("Bluetooth adapter is not ready");
        }
        if self.local_address == Some(peer) {
            return Err("use fleet reset to remove this machine from its fleet");
        }
        if self.keyring_locked {
            return Err("unlock the keyring before removing a fleet member");
        }
        if self.enrollment.phase().active()
            || self.commit.is_staged()
            || self.effects.keyring_dirty
            || self.routing.phase().busy()
        {
            return Err("finish or cancel the current fleet operation, then retry");
        }
        let Some(mut fleet) = self.saved_fleet().filter(|_| self.membership.contains(peer)) else {
            return Ok(None);
        };
        fleet.members.retain(|member| member.address != peer);
        if fleet.members.is_empty() {
            return Err("use fleet reset to clear the last saved fleet member");
        }
        Ok(Some(fleet))
    }

    /// The reduced membership is already durable. Keep direct AirPods observations.
    pub(crate) fn remove_fleet_member(&mut self, peer: Address) {
        if self.preferred_connector().is_some_and(|r| r.owner == peer) {
            self.activity_preference.clear();
        }
        self.membership.remove(peer);
        self.discovery.forget(peer);
        self.peers.forget(peer);
        self.peers_seated.retain(|address| *address != peer);
        self.held_peers.retain(|address| *address != peer);
        if self.held_reporter == Some(peer) {
            self.held_reporter = None;
            self.held_peers.clear();
        }
        self.peer_playback.remove(&peer);
        self.peer_media_sent.remove(&peer);
        self.introduced_peers.1.retain(|address| *address != peer);
        self.roster_dirty = true;
        self.effects.announce_dirty = true;
    }

    /// An incoming invitation the adapter is holding aside across a local
    /// fleet reset. Which invitation may be held, and putting it back, are
    /// decided here — the adapter only runs the keyring deletion between.
    pub(crate) fn take_invitation(
        &mut self,
        id: InvitationId,
    ) -> Result<InvitationSave, &'static str> {
        if self.enrollment.id() != Some(id)
            || self.enrollment.phase() != crate::fleet::enroll::Phase::Incoming
            || self.commit.is_staged()
        {
            return Err("error: this invitation is no longer awaiting a code\n");
        }
        Ok(InvitationSave {
            enrollment: std::mem::take(&mut self.enrollment),
            offer: self.offer.take(),
            pake: self.pake.take(),
        })
    }

    pub(crate) fn restore_invitation(&mut self, save: InvitationSave) {
        self.enrollment = save.enrollment;
        self.offer = save.offer;
        self.pake = save.pake;
    }

    /// Whether this machine may drop its fleet and immediately invite `peer`.
    pub(crate) fn check_rejoin(&self, peer: Address) -> Result<(), &'static str> {
        if self.enrollment.phase().active()
            || self.local_address.is_none()
            || self.local_address == Some(peer)
        {
            return Err(
                "error: finish the current fleet operation and enable Bluetooth before leaving\n",
            );
        }
        Ok(())
    }

    /// Called only after the serialized keyring worker confirms deletion.
    /// Forget fleet credentials, exchanges, cached rosters and queued fleet
    /// traffic. Direct AirPods observations, pairing and local audio survive.
    pub(crate) fn reset_fleet(&mut self) {
        self.membership.mark_unlocked();
        self.membership.mark_confirmed_empty();
        self.keyring_locked = false;
        self.effects.keyring_dirty = false;
        self.commit.clear();
        self.enrollment = Enrollment::new();
        self.pake = None;
        self.offer = None;
        self.outbox.clear();
        self.radio_inbox = crate::fleet::radio::Inbox::new();
        self.outbound = crate::fleet::radio::Schedule::new();
        self.discovery = Discovery::new();
        self.activity_preference.clear();
        self.pod_state.scope(None);
        self.peers.invalidate();
        self.roster_dirty = true;
        self.held_peers.clear();
        self.held_reporter = None;
        self.peers_seated.clear();
        self.routing = Routing::new();
        if self.output.wants_output() {
            self.routing.claim_audio();
        }
        self.route_seq = 0;
        self.route_air_tx = None;
        self.route_air_at = None;
        self.route_wait_from = None;
        self.route_water.clear();
        self.route_seen.clear();
        self.route_reply = None;
        self.pending_admission = None;
        self.route_packet = None;
        self.route_block = None;
        self.route_connection = None;
        self.routing_session(None);
        self.roster_seq = 0;
        self.roster_air.reset();
        self.roster_change_queued = None;
        self.presence_air.reset();
        self.effects.announce_dirty = true;
        eprintln!("fleetpodsd: local fleet reset; AirPods pairing preserved");
    }

    pub fn restore_fleet(&mut self, fleet: Fleet) {
        let proposal = Proposal { id: fleet.id, key: fleet.key, members: fleet.members };
        let _ = self.membership.apply(proposal);
    }

    pub fn announcement(&self) -> Announcement {
        Announcement {
            host: host_id(self.local_address),
            address: self.local_address,
            name: self.local_name.clone(),
            seated: self.connection.stage() == Stage::Ready,
        }
    }

    pub fn audio_roster(&mut self, now: Clock) -> Option<RosterReport> {
        self.accessory.source.as_fresh()?;
        let (audio_source, audio_idle) = self.roster_audio(now);
        let reporter = self.local_address?;
        let accessory = self.config.accessory?;
        self.roster_seq = self.roster_seq.saturating_add(1);
        Some(RosterReport {
            reporter: host_id(Some(reporter)),
            reporter_address: reporter,
            accessory,
            session: self.routing.session(),
            seq: self.roster_seq,
            at: now.mono,
            sent: now.unix,
            peers: self.roster_peers(now),
            audio_source,
            pending: false,
            audio_idle,
            intent: self.roster_intent(now),
            wire_session: None,
            last_use: UnixMs(0),
        })
    }

    pub fn note_lid(&mut self, open: bool, now: MonoMs) {
        let state = if open { LidState::Open } else { LidState::Closed };
        if open && self.accessory.lid.last_known != Some(LidState::Open) {
            let out = |pod: &crate::types::Evidence<Pod>| {
                pod.known()
                    .is_some_and(|pod| pod.in_ear == Some(true) || pod.in_case == Some(false))
            };
            if !out(&self.accessory.left) && !out(&self.accessory.right) {
                self.case_opened_at = Some(now);
            }
        } else if !open {
            self.case_opened_at = None;
        }
        self.accessory.note_lid(state, now);
        self.enforce_connection_admission(now);
        if !open && self.displaced {
            let worn = |pod: &crate::types::Evidence<crate::pods::reports::Pod>| {
                pod.known().is_some_and(|pod| pod.in_ear == Some(true))
            };
            if !worn(&self.accessory.left) && !worn(&self.accessory.right) {
                self.displaced = false;
            }
        }
        // Availability is evaluated in reconnect_preferred after all evidence
        // for this input arrives. Only the remembered fleet host may react.
    }

    /// A proximity advertisement. The control link keeps bud reports once it is ready.
    /// Case battery and the lid still follow the advertisement.
    pub(crate) fn hear_proximity(&mut self, address: Address, payload: &[u8], now: Clock) {
        // Unpaired advertisements are observations of separate candidates. They
        // cannot drive connection, audio, or overwrite another candidate's state.
        if self.config.accessory.is_none() {
            if self.nearby_pods.hear(address, payload, now.mono) {
                if let Some(report) = self.nearby_pods.selected_report(now.mono) {
                    self.accessory = report;
                }
            }
            return;
        }
        // Which known accessory is speaking? The selected one by its direct
        // address, or any keyed accessory whose IRK resolves the advertiser,
        // or a tracked accessory seen at its identity address.
        let selected = self.config.accessory;
        let owner = if selected == Some(address) {
            selected
        } else if let Some(found) =
            self.membership.accessory_for_advertiser(address, crate::pods::air::resolves)
        {
            Some(found)
        } else if self.tracked.contains_key(&address) {
            Some(address)
        } else {
            None
        };
        if crate::pods::air::pairing_model(payload).is_some() {
            match owner {
                Some(owner) if Some(owner) == selected => {
                    if self.membership.accessory_keys_ready(owner) {
                        if !self.pairing_elsewhere(now.mono) {
                            eprintln!(
                                "fleetpodsd: known AirPods entered pairing mode; holding reconnects"
                            );
                        }
                        self.accessory.pairing_at = Some(now.mono);
                        self.enforce_connection_admission(now.mono);
                    }
                }
                Some(owner) => {
                    self.tracked.entry(owner).or_default().pairing_at = Some(now.mono);
                }
                None => {}
            }
            return;
        }
        let Some(sight) = crate::pods::air::parse(payload) else {
            return;
        };
        let Some(owner) = owner else {
            return;
        };
        if Some(owner) != selected {
            let was_worn =
                self.tracked.get(&owner).is_some_and(|report| report.wearing() == Some(true));
            self.track_other(owner, address, &sight, now);
            let is_worn =
                self.tracked.get(&owner).is_some_and(|report| report.wearing() == Some(true));
            if !was_worn && is_worn {
                self.maybe_promote(owner, now);
            }
            return;
        }
        let keys = self.selected_keys().cloned();
        let irk_known = keys.is_some();
        let irk_match =
            keys.as_ref().is_some_and(|keys| crate::pods::air::resolves(&keys.irk, address));
        // The placement, lid, and case fields are plaintext. Until the
        // stored IRK can resolve the advertiser, only the configured
        // accessory's own address may speak for it — otherwise any nearby
        // radio with a recognized model byte could drive this accessory's
        // lid, placement, and stowed-connection decisions.
        if !irk_match && self.config.accessory != Some(address) {
            return;
        }
        let recognized = sight.model != Model::Unknown;
        let model = if recognized { sight.model } else { self.accessory.model };
        let Some(decision) =
            crate::pods::air::decide(irk_match, recognized, model.is_headset(), irk_known)
        else {
            return;
        };
        // Normal placement reports can interleave with pairing advertisements.
        // They do not cancel a fresh pairing indication; a closed lid does.
        if sight.lid == Some(LidState::Closed) {
            self.accessory.pairing_at = None;
        }
        self.accessory.note_advertisement(now.mono, sight.connection);
        if decision.adopt_model {
            self.accessory.note_model(sight.model);
        }
        if let Some(worn) = sight.head_worn {
            let previous = self.accessory.head_worn();
            self.accessory.note_head_worn(worn, now.mono);
            let changed = previous != self.accessory.head_worn();
            self.note_ears(now.mono);
            if changed {
                eprintln!("fleetpodsd: advertised head wear address={owner} worn={worn}");
            }
        }
        let link_up = self.connection.stage() == Stage::Ready;
        if !link_up && !model.is_headset() {
            let previous = (
                self.accessory.left_placement.known().copied(),
                self.accessory.right_placement.known().copied(),
            );
            // Each pod reports itself first. The other pod's view of it lags
            // (XPS, 2026-10-19: the left pod said "out" while the docked right
            // pod still said "worn", four flips in 0.4 s), so a pod that spoke
            // for itself recently is not overruled by the other pod.
            let sender_right = !sight.primary_left;
            self.advertised_self_at[usize::from(sender_right)] = Some(now.mono);
            let decode = |daemon: &Self, right: bool| {
                let prior = if right {
                    daemon.accessory.right_placement.as_fresh().copied()
                } else {
                    daemon.accessory.left_placement.as_fresh().copied()
                };
                let spoke = daemon.advertised_self_at[usize::from(right)]
                    .is_some_and(|at| now.mono.saturating_since(at) < ADVERTISED_SELF_MS);
                match prior {
                    Some(prior) if right != sender_right && spoke => (prior.in_ear, prior.in_case),
                    _ => placement(&sight, right, prior),
                }
            };
            let (left_ear, left_case) = decode(self, false);
            let (right_ear, right_case) = decode(self, true);
            self.accessory.note_placement(false, left_ear, left_case, now.mono);
            self.accessory.note_placement(true, right_ear, right_case, now.mono);
            self.note_ears(now.mono);
            if decision.encrypted {
                if let Some(keys) = &keys {
                    let plain = crate::pods::air::open_block(&keys.encryption, &sight.encrypted);
                    let levels = crate::pods::air::levels(
                        &plain,
                        sight.primary_left,
                        sight.this_in_case,
                        false,
                    );
                    self.paint_bud(false, levels.left, left_ear, left_case, now.mono);
                    self.paint_bud(true, levels.right, right_ear, right_case, now.mono);
                    if let Some(cell) = levels.case {
                        self.note_case_battery(cell.level, cell.charging, now.mono);
                    }
                }
            }
            let placement = (
                self.accessory.left_placement.as_fresh().copied(),
                self.accessory.right_placement.as_fresh().copied(),
            );
            if placement != previous {
                eprintln!(
                    "fleetpodsd: advertised placement without control stage={:?} left={:?} right={:?} frame(primary_left={} this_in_case={} both_in_case={} left_in_ear={} right_in_ear={})",
                    self.connection.stage(), placement.0, placement.1, sight.primary_left,
                    sight.this_in_case, sight.both_in_case, sight.left_in_ear, sight.right_in_ear
                );
            }
        } else if !link_up && decision.encrypted {
            if let Some(keys) = &keys {
                let plain = crate::pods::air::open_block(&keys.encryption, &sight.encrypted);
                if let Some(cell) =
                    crate::pods::air::levels(&plain, sight.primary_left, sight.this_in_case, true)
                        .headset
                {
                    if let Some(battery) = crate::types::Battery::new(cell.level, cell.charging) {
                        self.accessory.note_headset(battery, now.mono);
                    }
                }
            }
        }
        if decision.case_battery {
            if let Some(level) = sight.case_level {
                self.note_case_battery(level, sight.case_charging, now.mono);
            }
        }
        if let Some(state) = sight.lid {
            self.note_lid(state == LidState::Open, now.mono);
        }
    }

    /// Wear promotion: a tracked pair that just went into an ear becomes
    /// the selected pair, unless the selected pair is in use. Putting a
    /// pair in is the user's switch gesture; a pair that is playing (or
    /// being worn) is never taken over.
    fn maybe_promote(&mut self, candidate: Address, now: Clock) {
        if self.config.accessory == Some(candidate) {
            return;
        }
        // Advertisement paths need pairing keys to trust the candidate's
        // identity; a platform-reported connected headset is already
        // bond-verified, and selecting it is how its keys get learned.
        let bond_verified =
            self.tracked.get(&candidate).is_some_and(|report| report.bluez_connected);
        if !self.membership.accessory_keys_ready(candidate) && !bond_verified {
            return;
        }
        if self.accessory.wearing() == Some(true) {
            return;
        }
        if self.audio.local_playing() && self.transport_active {
            return;
        }
        eprintln!("fleetpodsd: tracked accessory {candidate} worn; making it the selected pair");
        self.activate_accessory(candidate, now);
    }

    /// Track a non-selected known accessory from its advertisements.
    /// Telemetry only: it must not drive connection, seat, ears policy,
    /// or fleet state, which stay with the selected accessory.
    fn track_other(
        &mut self,
        owner: Address,
        advertiser: Address,
        sight: &crate::pods::air::Frame,
        now: Clock,
    ) {
        let keys = self.membership.accessory_keys(owner).cloned();
        let irk_match =
            keys.as_ref().is_some_and(|keys| crate::pods::air::resolves(&keys.irk, advertiser));
        let report = self.tracked.entry(owner).or_default();
        let recognized = sight.model != Model::Unknown;
        let model = if recognized { sight.model } else { report.model };
        let Some(decision) =
            crate::pods::air::decide(irk_match, recognized, model.is_headset(), keys.is_some())
        else {
            return;
        };
        if sight.lid == Some(LidState::Closed) {
            report.pairing_at = None;
        }
        report.note_advertisement(now.mono, sight.connection);
        if decision.adopt_model {
            report.note_model(sight.model);
        }
        if let Some(worn) = sight.head_worn {
            report.note_head_worn(worn, now.mono);
        }
        if !model.is_headset() {
            let mut placements = [(None, None); 2];
            for right in [false, true] {
                let prior = if right {
                    report.right_placement.known().copied()
                } else {
                    report.left_placement.known().copied()
                };
                placements[usize::from(right)] = placement(sight, right, prior);
                let (in_ear, in_case) = placements[usize::from(right)];
                report.note_placement(right, in_ear, in_case, now.mono);
            }
            if decision.encrypted {
                if let Some(keys) = &keys {
                    let plain = crate::pods::air::open_block(&keys.encryption, &sight.encrypted);
                    let levels = crate::pods::air::levels(
                        &plain,
                        sight.primary_left,
                        sight.this_in_case,
                        false,
                    );
                    for (right, cell) in [(false, levels.left), (true, levels.right)] {
                        let (in_ear, in_case) = placements[usize::from(right)];
                        let prior = if right {
                            report.right.as_fresh().copied()
                        } else {
                            report.left.as_fresh().copied()
                        };
                        let battery = match cell {
                            Some(cell) => crate::types::Battery::new(cell.level, cell.charging),
                            None => prior.map(|pod| pod.battery),
                        };
                        if let Some(battery) = battery {
                            report.note_pod(
                                right,
                                Pod {
                                    battery,
                                    in_ear,
                                    in_case: in_case.or_else(|| {
                                        cell.filter(|cell| cell.charging && in_ear != Some(true))
                                            .map(|_| true)
                                    }),
                                },
                                now.mono,
                            );
                        }
                    }
                    if let Some(cell) = levels.case {
                        if let Some(battery) = crate::types::Battery::new(cell.level, cell.charging)
                        {
                            report.note_case(battery, now.mono);
                        }
                    }
                }
            }
        } else if decision.encrypted {
            if let Some(keys) = &keys {
                let plain = crate::pods::air::open_block(&keys.encryption, &sight.encrypted);
                if let Some(cell) =
                    crate::pods::air::levels(&plain, sight.primary_left, sight.this_in_case, true)
                        .headset
                {
                    if let Some(battery) = crate::types::Battery::new(cell.level, cell.charging) {
                        report.note_headset(battery, now.mono);
                    }
                }
            }
        }
        if decision.case_battery {
            if let Some(level) = sight.case_level {
                if let Some(battery) = crate::types::Battery::new(level, sight.case_charging) {
                    report.note_case(battery, now.mono);
                }
            }
        }
        if let Some(state) = sight.lid {
            report.note_lid(state, now.mono);
        }
    }

    fn paint_bud(
        &mut self,
        right: bool,
        cell: Option<crate::pods::air::Cell>,
        in_ear: Option<bool>,
        in_case: Option<bool>,
        now: MonoMs,
    ) {
        let prior = if right {
            self.accessory.right.as_fresh().copied()
        } else {
            self.accessory.left.as_fresh().copied()
        };
        let battery = match cell {
            Some(cell) => crate::types::Battery::new(cell.level, cell.charging),
            None => prior.map(|pod| pod.battery),
        };
        let Some(battery) = battery else {
            return;
        };
        self.note_pod(
            right,
            Pod {
                battery,
                in_ear,
                // An advertisement replaces control-link placement. Reusing
                // the old case flag here could keep a returned bud "out" (or
                // a removed bud "docked") indefinitely. Fresh charging proves
                // docking; a cached battery or not charging proves neither.
                in_case: in_case.or_else(|| {
                    cell.filter(|cell| cell.charging && in_ear != Some(true)).map(|_| true)
                }),
            },
            now,
        );
    }

    pub fn note_case_battery(&mut self, level: u8, charging: bool, now: MonoMs) {
        if let Some(battery) = crate::types::Battery::new(level, charging) {
            self.accessory.note_case(battery, now);
        }
    }

    pub fn note_ears(&mut self, now: MonoMs) {
        let (left, right) = self.accessory.wear_state();
        let action =
            self.audio.ears(left, right, self.config.ear, self.accessory.model.is_headset(), now);
        self.apply_ear_action(action);
        self.note_wearing(now);
    }

    /// Settle a removal or reinsertion that has waited out its debounce
    /// without a newer report.
    pub(crate) fn tick_ears(&mut self, now: MonoMs) {
        let action = self.audio.ears_tick(self.config.ear, now);
        self.apply_ear_action(action);
    }

    fn apply_ear_action(&mut self, action: crate::audio::play::EarAction) {
        use crate::audio::play::EarAction;
        match action {
            EarAction::Hold => {}
            EarAction::Pause => {
                let players = self.audio.ear_paused().to_vec();
                eprintln!(
                    "fleetpodsd: {} removed; pausing {players:?}",
                    self.accessory.model.display_name()
                );
                self.effects.pause_players.extend(players);
            }
            EarAction::Resume => {
                let players = self.audio.take_ear_paused();
                eprintln!(
                    "fleetpodsd: {} worn again; resuming {players:?}",
                    self.accessory.model.display_name()
                );
                self.queue_resume(players);
            }
        }
    }

    pub fn note_remote_player(&mut self, address: Address, now: MonoMs) {
        self.note_ownership(false, now);
        self.note_source(false, 2, now);
        self.note_remote(address, now);
    }

    pub fn request_route(&mut self, peer: Address) {
        if !self.routing.phase().busy() {
            let _ = self.routing.start(peer);
        }
    }

    pub fn note_alongside(&mut self, peer: Address, at: crate::types::UnixMs) {
        self.routing.note_alongside(peer, at);
    }

    pub fn set_microphone(&mut self, streaming: bool, muted: bool, now: Clock) {
        if self.connection.stage() == Stage::Ready {
            self.microphone.set_control_ready(true);
        }
        let available = self.connection.stage() == Stage::Ready
            && self.connection.socket_up() == Some(true)
            && crate::audio::mic::format_for_sku(&self.accessory.sku).is_some();
        let live = available && streaming && !muted;
        match self.microphone.note_live(live) {
            crate::audio::mic::Motion::Start => self.begin_capture(now.mono),
            crate::audio::mic::Motion::Stop => self.end_capture(),
            crate::audio::mic::Motion::None => {}
        }
    }

    fn begin_capture(&mut self, now: MonoMs) {
        let on = self.accessory.conversation.as_fresh() == Some(&true);
        self.audio.suspend_conversation(on);
        if on {
            self.queue_setting(crate::pods::setup::conversation(false));
        }
        self.mic_decoder = crate::audio::mic::format_for_sku(&self.accessory.sku)
            .and_then(|format| crate::audio::eld::Decoder::open(&format));
        if self.mic_decoder.is_none() {
            eprintln!("fleetpodsd: microphone decoder did not open");
        }
        self.mic_started = true;
        self.mic_path
            .delivering
            .store(self.mic_decoder.is_some(), std::sync::atomic::Ordering::Relaxed);
        self.mic_path.discard.store(true, std::sync::atomic::Ordering::Relaxed);
        self.queue_setting(crate::audio::mic::start_command());
        self.microphone.arm(now);
    }

    fn end_capture(&mut self) {
        let started = self.mic_started;
        self.mic_started = false;
        self.mic_path.delivering.store(false, std::sync::atomic::Ordering::Relaxed);
        self.mic_decoder = None;
        self.mic_path.discard.store(true, std::sync::atomic::Ordering::Relaxed);
        if started {
            self.queue_setting(crate::audio::mic::stop_command());
        }
        if self.audio.resume_conversation() == Some(true) {
            self.queue_setting(crate::pods::setup::conversation(true));
        }
    }

    fn consider_capture_stall(&mut self, now: MonoMs) {
        if !self.microphone.capturing() {
            return;
        }
        let Some(quiet) = self.microphone.quiet_for(now) else {
            return;
        };
        if quiet < crate::audio::mic::STALL_MS {
            return;
        }
        let phase = self.microphone.stalled();
        if phase == crate::audio::mic::Phase::Capturing {
            self.end_capture();
            self.begin_capture(now);
        } else if phase == crate::audio::mic::Phase::NeedsReopen {
            self.end_capture();
        }
    }

    pub(crate) fn hear_microphone(&mut self, packet: &[u8], now: MonoMs) -> bool {
        use std::sync::atomic::Ordering;
        match crate::audio::mic::audio_units(packet) {
            None => false,
            Some(Err(())) => {
                if self.microphone.capturing() {
                    self.microphone.note_malformed();
                }
                true
            }
            Some(Ok(units)) => {
                if self.microphone.capturing() {
                    self.microphone.note_frames(units.len() as u32, now);
                    for unit in units {
                        match self.mic_decoder.as_mut().and_then(|decoder| decoder.decode(unit)) {
                            Some(mut pcm) => {
                                if !self.mic_path.format_ok.load(Ordering::Relaxed) {
                                    continue;
                                }
                                crate::audio::mic::amplify(&mut pcm);
                                if !self.mic_path.queue.push(&pcm) {
                                    self.mic_path.dropped.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                            None => {
                                self.mic_path.decode_errors.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    }
                }
                true
            }
        }
    }

    /// The name shown for the microphone. A blank accessory name uses AirPods.
    fn microphone_label(name: &str) -> String {
        let name = name.trim();
        if name.is_empty() {
            "AirPods".to_string()
        } else {
            name.to_string()
        }
    }

    pub(crate) fn pace_microphone(&mut self) -> Option<crate::step::Effect> {
        let format = crate::audio::mic::format_for_sku(&self.accessory.sku);
        let ready =
            self.connection.stage() == Stage::Ready && self.connection.socket_up() == Some(true);
        let next = match (ready, format, self.config.accessory) {
            (true, Some(format), Some(address)) => {
                Some((address, format.delivery_hz, Self::microphone_label(&self.accessory.name)))
            }
            _ => None,
        };
        if next == self.mic_source {
            return None;
        }
        if let Some((_, rate, _)) = &next {
            self.mic_path.rate.store(*rate, std::sync::atomic::Ordering::Relaxed);
        }
        let effect = match next {
            Some((address, rate, description)) => {
                self.mic_source = Some((address, rate, description.clone()));
                crate::step::Effect::ShowMicrophone { address, rate, description }
            }
            None => {
                self.mic_source = None;
                crate::step::Effect::HideMicrophone
            }
        };
        Some(effect)
    }

    /// Bytes that are not a `pf1` postcard are ignored.
    /// A replay or a malformed fragment must not change membership.
    pub fn observe_radio(&mut self, _bytes: &[u8]) {}

    /// A physical input or new Play records activity even while stowed. A
    /// notification, idle tick, radio packet or reconnection never counts.
    pub(crate) fn remember_activity(&mut self, now: Clock) {
        let (Some(fleet), Some(accessory), Some(local)) =
            (self.membership.fleet(), self.config.accessory, self.local_address)
        else {
            return;
        };
        self.activity_preference.note_local(fleet.id.bytes(), accessory, local, now.unix);
        if self.connection.reason() == "disconnect" {
            // Fresh input can replace an explicit disconnect even while stowed;
            // case telemetry and radio refreshes cannot.
            self.connection.prepare();
        }
    }

    pub(crate) fn preferred_connector(&self) -> Option<crate::fleet::activity::Record> {
        let fleet = self.membership.fleet()?;
        let record = self.activity_preference.record(fleet.id.bytes(), self.config.accessory?)?;
        (Some(record.owner) == self.local_address || self.membership.contains(record.owner))
            .then_some(record)
    }

    pub(crate) fn preferred_connection_allowed(&self) -> bool {
        // A standalone installation has no fleet election. An enrolled fleet
        // without a remembered owner waits for genuine input instead of racing.
        !self.keyring_locked
            && (self.membership.fleet().is_none()
                || self.preferred_connector().is_some_and(|r| Some(r.owner) == self.local_address))
    }

    pub(crate) fn hear_activity(&mut self, bytes: &[u8], now: Clock) {
        let (Some(fleet), Some(accessory)) = (self.membership.fleet(), self.config.accessory)
        else {
            return;
        };
        let keys =
            crate::fleet::presence::FleetKeys { id: fleet.id.bytes(), key: *fleet.key.as_bytes() };
        let local = self.local_address;
        let members = &fleet.members;
        if self.activity_preference.hear(bytes, &keys, accessory, now.unix, |address| {
            Some(address) == local || members.iter().any(|m| m.address == address)
        }) {
            if let Some(record) = self.preferred_connector() {
                eprintln!("fleetpodsd: remembered active fleet device {}", record.owner);
            }
        }
    }

    pub(crate) fn offer_activity(&mut self, now: Clock) {
        let (Some(record), Some(local), Some(fleet)) =
            (self.preferred_connector(), self.local_address, self.membership.fleet())
        else {
            self.outbound.clear_activity();
            return;
        };
        let keys =
            crate::fleet::presence::FleetKeys { id: fleet.id.bytes(), key: *fleet.key.as_bytes() };
        if let Some(offer) = self.activity_preference.offer(record, local, now, &keys) {
            self.outbound.offer_activity(offer);
        }
    }

    /// Telemetry follows the live seat, not the remembered reconnect preference.
    /// A ready link alone does not entitle a second connected laptop to publish.
    pub(crate) fn offer_pod_state(&mut self, now: Clock) {
        let scope = self
            .membership
            .fleet()
            .zip(self.config.accessory)
            .map(|(fleet, accessory)| (fleet.id.bytes(), accessory));
        self.pod_state.scope(scope);
        let eligible = !self.keyring_locked
            && self.connection.stage() == Stage::Ready
            && self.local_address.is_some()
            && self.roster_audio(now).0 == self.local_address;
        let (Some((_, accessory)), Some(local)) = (scope, self.local_address) else {
            self.pod_state.stop_publishing();
            self.outbound.clear_pod_state();
            return;
        };
        if !eligible {
            self.pod_state.stop_publishing();
            self.outbound.clear_pod_state();
            return;
        }
        let fleet_id = self.membership.fleet().unwrap().id;
        let session = self.routing_session(Some(fleet_id));
        if session == [0; 16] {
            return;
        }
        let fleet = self.membership.fleet().unwrap();
        let keys =
            crate::fleet::presence::FleetKeys { id: fleet.id.bytes(), key: *fleet.key.as_bytes() };
        let identity = crate::fleet::pod_state::Identity { reporter: local, accessory, session };
        if let Some(frame) = self.pod_state.offer(identity, &self.accessory, now, &keys) {
            self.outbound.offer_pod_state(frame);
        }
    }

    pub(crate) fn hear_pod_state(&mut self, bytes: &[u8], now: Clock) {
        if self.keyring_locked {
            return;
        }
        let (Some(fleet), Some(accessory)) = (self.membership.fleet(), self.config.accessory)
        else {
            return;
        };
        let keys =
            crate::fleet::presence::FleetKeys { id: fleet.id.bytes(), key: *fleet.key.as_bytes() };
        let local = self.local_address;
        self.pod_state.hear(bytes, &keys, accessory, now, |reporter| {
            Some(reporter) != local && fleet.members.iter().any(|m| m.address == reporter)
        });
    }

    fn fleet_pod_state(&self, now: Clock) -> Option<&crate::fleet::pod_state::Packet> {
        if self.keyring_locked {
            return None;
        }
        let fleet = self.membership.fleet()?;
        let accessory = self.config.accessory?;
        let Holder::Remote { address: Some(address), .. } = self.resolved(now).holder else {
            return None;
        };
        if !self.membership.contains(address) {
            return None;
        }
        let report = self.peers.fresh_from(now, accessory, address)?;
        if report.pending
            || report.audio_source != Some(address)
            || !report.peers.contains(&address)
        {
            return None;
        }
        self.pod_state.current(
            fleet.id.bytes(),
            crate::fleet::pod_state::Identity {
                reporter: address,
                accessory,
                session: report.wire_session?,
            },
            now,
        )
    }

    /// A phone or Mac (not a fleet laptop) plays on the pods, by this laptop's
    /// own observation or by the latest fleet report.
    fn outside_device_playing(&self, now: Clock) -> bool {
        let outside = |address: Address| {
            Some(address) != self.local_address && !self.membership.contains(address)
        };
        if let Holder::Remote { address: Some(address), playing: true } = self.resolved(now).holder
        {
            if outside(address) {
                return true;
            }
        }
        self.config.accessory.and_then(|accessory| self.peers.fresh(now, accessory)).is_some_and(
            |report| {
                !report.pending && !report.audio_idle && report.audio_source.is_some_and(outside)
            },
        )
    }

    /// Keep fresh input through the peer's pause debounce, never through a later session.
    pub(crate) fn claim_idle_on_activity(&mut self, now: Clock) -> bool {
        const ACTIVITY_GRACE_MS: u64 = 5_000;
        if !self.automatic_playback_allowed() {
            return false;
        }
        let Some(at) = self.activity_at else {
            return false;
        };
        if now.mono.saturating_since(at) > ACTIVITY_GRACE_MS {
            self.activity_at = None;
            return false;
        }
        if self.connection.stage() != Stage::Ready
            || self.routing.phase().busy()
            || self.connection.apple_reconnect()
            || self.accessory.playback_blocked()
            || self.audio.local_playing()
        {
            return false;
        }
        let holder = self.resolved(now).holder;
        if matches!(holder, Holder::Local { .. }) {
            // Input while we already own audio is satisfied, not a claim to
            // replay if another laptop takes ownership a moment later.
            self.activity_at = None;
            return false;
        }
        if let Holder::Remote { address, playing: false } = holder {
            // An ownership-loss notification need not name the new owner.
            // With both fleet laptops still connected, the other laptop's
            // authenticated self-report can supply that missing identity.
            let peer = address.or_else(|| {
                let local = self.local_address?;
                let report = self.peers.fresh(now, self.config.accessory?)?;
                let peer = report.reporter_address;
                (peer != local
                    && self.membership.contains(peer)
                    && !report.pending
                    && report.audio_source == Some(peer)
                    && report.audio_idle
                    && self.pod_peers.len() == 2
                    && self.pod_peers.contains(&local)
                    && self.pod_peers.contains(&peer)
                    && report.peers.contains(&local)
                    && report.peers.contains(&peer))
                .then_some(peer)
            });
            let Some(peer) = peer else { return false };
            if let Some((false, reported_at)) = self.remote_playback(peer, now) {
                // A device outside the fleet that only just reported not
                // playing is still joining, not idle: desktop activity is
                // permission to be connected, not to interrupt its join.
                // Fleet members report deliberately and stay on the fast path.
                const OUTSIDE_JOIN_IDLE_MS: u64 = 10_000;
                if !self.membership.contains(peer)
                    && now.mono.saturating_since(reported_at) < OUTSIDE_JOIN_IDLE_MS
                {
                    return false;
                }
                if self.idle_claimed.is_none_or(|(previous, requested)| {
                    previous != peer || now.mono.saturating_since(requested) >= 2_000
                }) {
                    self.activity_at = None;
                    self.idle_claimed = Some((peer, now.mono));
                    self.emit_ownership_claim(session::ClaimReason::IdleActivity);
                    return true;
                }
            }
        }
        false
    }

    fn activity_target(&self, now: Clock) -> ActivityTarget {
        let mut found = None;
        for host in self.discovery.hosts() {
            if Some(host.address) == self.local_address
                || !host.authenticated
                || host.session.is_none()
                || !self.membership.contains(host.address)
            {
                continue;
            }
            if self.config.accessory.is_some_and(|accessory| {
                self.peers.fresh_from(now, accessory, host.address).is_some_and(|report| {
                    (report.wire_session.is_none() || report.wire_session == host.session)
                        && !report.pending
                        && !report.peers.contains(&host.address)
                })
            }) {
                continue;
            }
            if found.is_some() {
                return ActivityTarget::Several;
            }
            found = Some(host.address);
        }
        match found {
            Some(peer) => ActivityTarget::One(peer),
            None => ActivityTarget::None,
        }
    }

    pub(crate) fn seat_audio_failure(&mut self, reason: &'static str, now: Clock) {
        self.seat.note_failure(reason, now);
    }

    fn reconnect_peers(&self) -> Vec<Address> {
        let peers = if self.pod_peers_known { &self.pod_peers } else { &self.peers_seated };
        peers
            .iter()
            .copied()
            .filter(|peer| Some(*peer) != self.local_address && self.membership.contains(*peer))
            .collect()
    }

    fn send_reconnect_requests(&mut self) {
        let Some(local) = self.local_address else {
            return;
        };
        for peer in self.reconnect_peers() {
            if let Some(packet) = crate::pods::setup::reconnect_request(peer, local) {
                eprintln!(
                    "fleetpodsd: asking seated fleet peer {peer} to disconnect for Apple reconnect"
                );
                self.queue_setting(packet);
            }
        }
    }

    /// The accessory's card and its active A2DP playback profile.
    pub(crate) fn playback_card(&self) -> Option<(String, String)> {
        let address = self.config.accessory?;
        let card =
            self.last_graph.as_ref()?.cards.iter().find(|card| card.address == Some(address))?;
        let profile = card.active.as_deref().filter(|profile| profile.starts_with("a2dp-sink"))?;
        Some((card.name.clone(), profile.to_string()))
    }

    fn fleet(&mut self, command: FleetCommand, now: Clock) -> Response {
        if self.routing.phase().busy() && matches!(command, FleetCommand::Invite { .. }) {
            return Response::line("error: fleet request could not be completed");
        }
        let ok = match command {
            FleetCommand::LeaveInvitation { .. } => {
                return Response::line("error: leaving a fleet requires the keyring worker")
            }
            FleetCommand::Remove { .. } => {
                return Response::line("error: removing a fleet member requires the keyring worker")
            }
            FleetCommand::Invite { address, name } => self.invite_os(address, &name, now),
            FleetCommand::Join { address, name, leave: false } => self.join_os(address, &name, now),
            FleetCommand::Join { leave: true, .. } => {
                return Response::line("error: leaving a fleet requires the keyring worker")
            }
            FleetCommand::Submit { id, code } => {
                let accepted = self.enrollment.submit_code(id, code);
                if accepted {
                    self.answer_submit();
                }
                accepted
            }
            FleetCommand::Cancel { id } => {
                let peer = self.enrollment.peer().map(|(address, _)| address);
                let accepted = self.enrollment.cancel(id);
                if accepted {
                    self.pake = None;
                    self.offer = None;
                    if let Some(peer) = peer {
                        self.send(peer, Letter::Cancel { id });
                    }
                }
                accepted
            }
            FleetCommand::Bad => false,
        };
        if ok {
            Response::line("ok")
        } else {
            Response::line("error: fleet request could not be completed")
        }
    }

    pub(crate) fn join_os(&mut self, peer: Address, name: &str, now: Clock) -> bool {
        if self.keyring_locked
            || self.membership.fleet().is_some()
            || self.enrollment.phase().active()
        {
            return false;
        }
        let Some(local) = self.local_address else { return false };
        let mut bytes = [0u8; 16];
        if os::random_bytes(&mut bytes).is_err() {
            return false;
        }
        let id = InvitationId::new(bytes);
        if !self.enrollment.receive_invite(local, peer, name, id, now.mono) {
            return false;
        }
        self.pake = None;
        self.offer = None;
        self.send(peer, Letter::JoinRequest { id, name: self.local_name.clone() });
        true
    }

    fn invite_os(&mut self, peer: Address, name: &str, now: Clock) -> bool {
        if self.keyring_locked {
            return false;
        }
        let Ok(code) = code::from_os() else {
            return false;
        };
        let mut bytes = [0u8; 16];
        if os::random_bytes(&mut bytes).is_err() {
            return false;
        }
        let id = InvitationId::new(bytes);
        let previous = self.enrollment.id().zip(self.enrollment.peer().map(|(address, _)| address));
        let previous_active = self.enrollment.phase().active();
        let started = self.invite(peer, name, code, id, now);
        if started {
            if previous_active {
                if let Some((old_id, old_peer)) = previous {
                    self.send(old_peer, Letter::Cancel { id: old_id });
                }
            }
            self.pake = None;
            self.offer = None;
            self.send(peer, Letter::Hello { id, name: self.local_name.clone() });
        }
        started
    }

    /// Putting the pods in while the speakers play is play intent for that
    /// playback. Wait for the link and an ownership report; never take the
    /// pods from a device playing on them, or from one whose state is unknown.
    pub(crate) fn follow_insertion(&mut self, now: Clock) {
        const INSERTION_GRACE_MS: u64 = 15_000;
        let Some(at) = self.inserted_at else {
            return;
        };
        if now.mono.saturating_since(at) > INSERTION_GRACE_MS || self.output.wants_output() {
            self.inserted_at = None;
            return;
        }
        if self.connection.stage() != Stage::Ready
            || self.routing.phase().busy()
            || self.connection.apple_reconnect()
            || self.accessory.playback_blocked()
        {
            return;
        }
        let Some(graph) = self.last_graph.as_ref() else {
            return;
        };
        if !crate::audio::route::playing_on_speakers(graph, self.output.source_sink()) {
            return;
        }
        // Taking the pods from an idle device needs a player to confirm the
        // claim; a stream-only player would hold the speakers muted for nothing.
        let free = match self.resolved(now).holder {
            Holder::Local { .. } => true,
            Holder::Remote { address: Some(peer), playing: false } => {
                self.audio.local_playing()
                    && matches!(self.remote_playback(peer, now), Some((false, _)))
            }
            Holder::Remote { .. } | Holder::Unknown => false,
        };
        if !free || self.outside_device_playing(now) {
            return;
        }
        self.inserted_at = None;
        eprintln!(
            "fleetpodsd: pods put in while the speakers play; moving playback to the AirPods"
        );
        self.remember_activity(now);
        self.claim_playback(now);
    }

    pub(crate) fn playback(&mut self, event: &crate::types::PlaybackEvent, now: Clock) -> Response {
        let reply = self.audio.player_event(event, now.unix);
        if matches!(event.kind, crate::types::PlayKind::Play | crate::types::PlayKind::Pause) {
            // Do not include the observer token or arbitrary player identifier.
            eprintln!("fleetpodsd: media event source=observer application={:?} kind={:?} playing={} result={reply:?} mono_ms={}", event.application, event.kind, event.playing, now.mono.0);
        }
        if let PlayReply::Accepted { claim: true } = reply {
            self.remember_activity(now);
            self.claim_playback(now);
        }
        Response::line(match reply {
            PlayReply::Ok => "ok",
            PlayReply::Accepted { .. } => "accepted",
            PlayReply::InvalidOrStale => "invalid_or_stale",
            PlayReply::InvalidEvent => "invalid_event",
            PlayReply::Duplicate => "duplicate",
        })
    }

    fn cycle_noise(&mut self) {
        let current = self
            .accessory
            .noise
            .as_fresh()
            .copied()
            .or(self.config.noise)
            .unwrap_or(NoiseMode::Off);
        let order = [NoiseMode::Off, NoiseMode::Anc, NoiseMode::Transparency, NoiseMode::Adaptive];
        let start = order.iter().position(|mode| *mode == current).unwrap_or(0);
        for step in 1..order.len() {
            let next = order[(start + step) % order.len()];
            if self.accessory.model == Model::Unknown || self.accessory.model.allows(next) {
                self.request_noise(next);
                return;
            }
        }
    }

    fn forget(&mut self, now: Clock) -> Response {
        let Some(address) = self.config.accessory else {
            return Response::line("error: no accessory");
        };
        self.effects.remove_device = Some(address);
        self.unpaired(address, now);
        Response::line("ok")
    }

    /// A paired AirPods the platform knows about but that is not
    /// selected. Tracking it (even before its keys are harvested) makes
    /// it visible in status and selectable in the panel.
    pub(crate) fn note_known_accessory(
        &mut self,
        address: Address,
        name: &str,
        connected: bool,
        now: Clock,
    ) {
        if self.config.accessory == Some(address) {
            return;
        }
        let was_connected = self.tracked.get(&address).is_some_and(|report| report.bluez_connected);
        let report = self.tracked.entry(address).or_default();
        if !name.is_empty() && report.name != name {
            report.name = name.to_string();
        }
        report.bluez_connected = connected;
        // BlueZ connecting is a ready signal with a bond-verified
        // identity, so unlike an advertisement-resolved pair it needs no
        // keys to promote; selecting it is how its keys get learned. The
        // shared gates still protect a worn or playing selected pair.
        if connected && !was_connected {
            self.maybe_promote(address, now);
        }
    }

    pub(crate) fn unpaired(&mut self, address: Address, now: Clock) {
        if self.config.accessory != Some(address) {
            return;
        }
        self.interrupt("disconnect", now);
        self.connection.wait_for_request();
        self.connection.note_bluez(false);
        self.select_accessory(None);
        self.effects.follow_link = false;
        self.effects.drop_link = false;
        self.effects.control_out.clear();
        self.displaced = false;
        self.held_peers.clear();
        self.held_reporter = None;
        self.accessory = self.nearby_pods.selected_report(now.mono).unwrap_or_default();
        eprintln!("fleetpodsd: Bluetooth pairing removed; waiting for manual pairing");
    }

    fn priority_solo(&mut self, now: Clock) -> Response {
        let Some(local) = self.local_address else {
            return Response::line("control_unavailable");
        };
        if let Some(reason) = self.priority_blocked() {
            return Response::line(reason);
        }
        let Some(packet) = crate::pods::setup::priority(local, None) else {
            return Response::line("invalid_priority_pair");
        };
        self.queue_priority(packet, now);
        Response::line("written_waiting_for_ack")
    }

    fn priority_peer(&mut self, text: &str, incoming: bool, now: Clock) -> Response {
        let Some(peer) = Address::parse(text.trim()) else {
            return Response::line("invalid_address");
        };
        let Some(local) = self.local_address else {
            return Response::line("control_unavailable");
        };
        if peer == local {
            return Response::line("invalid_priority_pair");
        }
        if let Some(reason) = self.priority_blocked() {
            return Response::line(reason);
        }
        let packet = if incoming {
            crate::pods::setup::priority(local, Some(peer))
        } else {
            crate::pods::setup::priority(peer, Some(local))
        };
        let Some(packet) = packet else {
            return Response::line("invalid_priority_pair");
        };
        self.queue_priority(packet, now);
        Response::line("written_waiting_for_ack")
    }

    fn incoming_route(&mut self, request: &serde_json::Value, now: Clock) -> Response {
        let Some(peer) =
            request.get("peer").and_then(|value| value.as_str()).and_then(Address::parse)
        else {
            return self.incoming_reply(false, "invalid_peer", 0);
        };
        if self.local_address == Some(peer) {
            return self.incoming_reply(false, "invalid_peer", 0);
        }
        let yield_seat = request.get("yield").and_then(|value| value.as_bool()).unwrap_or(true);
        if let Some(reason) = self.priority_blocked() {
            return self.incoming_reply(false, reason, 0);
        }
        let Some(local) = self.local_address else {
            return self.incoming_reply(false, "control_unavailable", 0);
        };
        let packet = if yield_seat {
            crate::pods::setup::priority(peer, Some(local))
        } else {
            crate::pods::setup::priority(local, Some(peer))
        };
        let Some(packet) = packet else {
            return self.incoming_reply(false, "invalid_priority_pair", 0);
        };
        if yield_seat {
            self.step_back(now);
        }
        let id = self.queue_priority(packet, now);
        self.incoming_reply(true, "written_waiting_for_ack", id)
    }

    fn incoming_reply(&self, ok: bool, reason: &str, id: u64) -> Response {
        Response::line(format!("{{\"ok\":{ok},\"reason\":\"{reason}\",\"request_id\":{id}}}"))
    }

    fn priority_blocked(&self) -> Option<&'static str> {
        if self.routing.phase().busy() {
            return Some("fleet_routing_busy");
        }
        if self.connection.stage() != Stage::Ready || self.connection.socket_up() != Some(true) {
            return Some("control_unavailable");
        }
        if self.priority_until.is_some() {
            return Some("request_pending");
        }
        None
    }

    fn queue_priority(&mut self, packet: Vec<u8>, now: Clock) -> u64 {
        self.priority_generation = self.priority_generation.saturating_add(1);
        self.priority_until = Some(now.mono.saturating_add(5_000));
        self.effects.control_out.push((self.connection.generation(), packet));
        self.priority_generation
    }

    pub(crate) fn note_priority_ack(&mut self) {
        self.priority_until = None;
    }

    fn request_noise(&mut self, mode: NoiseMode) {
        if self.accessory.model != Model::Unknown && !self.accessory.model.allows(mode) {
            return;
        }
        self.config.noise = Some(mode);
        self.queue_setting(crate::pods::setup::noise_mode(mode));
    }

    fn request_adaptive(&mut self, level: u8) {
        if self.accessory.model != Model::Unknown && !self.accessory.model.supports_adaptive() {
            return;
        }
        self.config.adaptive = Some(level);
        self.queue_setting(crate::pods::setup::adaptive_level(level));
    }

    fn request_conversation(&mut self, enabled: bool) {
        if self.accessory.model != Model::Unknown
            && !self.accessory.model.supports_conversation_awareness()
        {
            return;
        }
        self.config.conversation = Some(enabled);
        self.queue_setting(crate::pods::setup::conversation(enabled));
    }

    fn request_one_bud(&mut self, enabled: bool) {
        if self.accessory.model != Model::Unknown && !self.accessory.model.supports_one_bud_anc() {
            return;
        }
        self.config.one_bud = Some(enabled);
        self.queue_setting(crate::pods::setup::one_bud(enabled));
    }

    fn queue_setting(&mut self, bytes: Vec<u8>) {
        if self.connection.socket_up() == Some(true) && self.connection.stage() == Stage::Ready {
            self.effects.control_out.push((self.connection.generation(), bytes));
        } else {
            self.staged_controls.push(bytes);
        }
    }

    pub(crate) fn flush_settings(&mut self) {
        let generation = self.connection.generation();
        for bytes in std::mem::take(&mut self.staged_controls) {
            self.effects.control_out.push((generation, bytes));
        }
    }

    fn resolved(&self, now: Clock) -> Resolved {
        let fleet = self.config.accessory.and_then(|address| self.peers.fresh(now, address));
        self.seat.resolve(
            SeatInputs {
                ownership: &self.accessory.ownership,
                source_report: &self.accessory.source,
                remote: &self.accessory.remote_address,
                fleet,
                local_address: self.local_address,
                local_playing: self.audio.local_playing(),
                routed_here: self.audio.routed(),
                observed_local: self.audio.observed_playback(),
                playback_blocked: self.accessory.playback_blocked(),
            },
            &|peer| self.remote_playback(peer, now).map(|(observed, _)| observed),
        )
    }

    fn snapshot<'a>(&'a self, now: Clock, resolved: &'a Resolved) -> Snapshot<'a> {
        Snapshot {
            clock: now,
            local_name: &self.local_name,
            local_address: self.local_address,
            config: &self.config,
            bluetooth_bonded: self.bluetooth_bonded,
            connection: &self.connection,
            accessory: &self.accessory,
            discovery: &self.discovery,
            peers: &self.peers,
            membership: &self.membership,
            enrollment: &self.enrollment,
            routing: &self.routing,
            microphone: &self.microphone,
            mic_path: &self.mic_path,
            handoff_yielded: self.seat.yielded(),
            handoff_phase: self.seat.phase(),
            handoff_failure: self.seat.failure(),
            handoff_updated: self.seat.updated(),
            audio_seen: self.last_graph.is_some(),
            playing: self.audio.local_playing(),
            routed: self.audio.routed(),
            resolved,
            seated: &self.peers_seated,
            held_peers: &self.held_peers,
            held_reporter: self.held_reporter,
            pod_peers: &self.pod_peers,
            pod_peers_known: self.pod_peers_known,
            pod_peers_at: self.pod_peers_at,
            displaced: self.displaced,
            apple_labels: &self.apple_labels,
        }
    }
}

pub struct Announcement {
    pub host: HostId,
    pub address: Option<Address>,
    pub name: String,
    pub seated: bool,
}

pub(crate) fn host_id(address: Option<Address>) -> HostId {
    let mut bytes = [0u8; 16];
    if let Some(address) = address {
        bytes[..6].copy_from_slice(&address.bytes());
    }
    HostId::new(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::members::{GroupKey, Member, Store};
    use crate::panel::ipc::push;
    use crate::types::{Battery, FleetId, UnixMs};

    pub(super) fn clock() -> Clock {
        Clock::new(1_000, 1_700_000_000_000)
    }

    pub(super) fn addr(n: u8) -> Address {
        Address::new([n, 2, 3, 4, 5, 6]).unwrap()
    }

    #[test]
    fn fleet_reset_cancels_enrollment_and_allows_a_different_fleet_without_unpairing() {
        use crate::step::{Effect, Input};
        let mut daemon = Daemon::new("xps");
        let now = clock();
        daemon.set_local_address(Some(addr(1)));
        ready_link(&mut daemon, now);
        let stage = daemon.connection.stage();
        let generation = daemon.connection.generation();
        let keys = AccessoryKeys { irk: [1; 16], encryption: [2; 16] };
        daemon.set_accessory_keys(addr(3), keys.clone());
        daemon.config.noise = Some(NoiseMode::Anc);
        daemon.output.claim();
        daemon.apple_labels.insert(addr(4), "Phone".into());
        daemon.restore_fleet(Fleet {
            id: FleetId::new([3; 16]),
            key: GroupKey::new([4; 32]),
            members: vec![Member { address: addr(2), label: "gram".into() }],
        });
        let id = InvitationId::new([5; 16]);
        assert!(daemon.enrollment.begin_invite(
            addr(1),
            addr(2),
            "gram",
            Code::parse("123456").unwrap(),
            id,
            now.mono
        ));
        daemon.commit.stage_saved(
            addr(2),
            id,
            Proposal {
                id: FleetId::new([3; 16]),
                key: GroupKey::new([4; 32]),
                members: vec![Member { address: addr(2), label: "gram".into() }],
            },
        );
        daemon.effects.keyring_dirty = true;
        daemon.routing.start(addr(2));
        daemon.held_peers.push(addr(2));
        daemon.held_reporter = Some(addr(2));
        daemon.offer_presence(now);
        daemon.reset_fleet();
        daemon.reset_fleet(); // already-empty is a supported reset
        let outcome = daemon.step(Input::KeyringStored, now);
        assert!(daemon.saved_fleet().is_none());
        let status = daemon.status_value(now);
        assert_eq!(status["fleet"]["enrolled"], false);
        assert_eq!(status["fleet"]["phase"], "idle");
        assert_eq!(status["fleet"]["error"], "");
        assert_eq!(status["pairing_keys_ready"], true);
        assert_eq!(daemon.membership.accessory_keys(addr(3)), Some(&keys));
        assert_eq!(daemon.connection.stage(), stage);
        assert_eq!(daemon.connection.generation(), generation);
        assert_eq!(daemon.config.accessory, Some(addr(3)));
        assert_eq!(daemon.config.noise, Some(NoiseMode::Anc));
        assert_eq!(daemon.apple_labels[&addr(4)], "Phone");
        assert!(!daemon.routing.phase().busy());
        assert!(daemon.output.wants_output());
        assert!(daemon.held_peers.is_empty());
        assert!(!daemon.commit.is_staged());
        assert!(!outcome.effects.iter().any(|effect| matches!(
            effect,
            Effect::WriteKeyring(_)
                | Effect::WriteSecret { .. }
                | Effect::RemoveDevice { .. }
                | Effect::Disconnect { .. }
        )));
        daemon.restore_fleet(Fleet {
            id: FleetId::new([6; 16]),
            key: GroupKey::new([7; 32]),
            members: vec![Member { address: addr(2), label: "gram".into() }],
        });
        assert_eq!(daemon.saved_fleet().unwrap().id, FleetId::new([6; 16]));
    }

    #[test]
    fn a_closed_case_is_not_a_connected_seat_and_unknown_battery_is_omitted() {
        let daemon = Daemon::new("gram");
        let status = daemon.status_value(clock());
        assert_eq!(status["schema_version"], 1);
        assert_eq!(status["connected"], false);
        assert_eq!(status["bluez_connected"], false);
        assert_eq!(status["noise_mode"], -1);
        assert_eq!(status["lid_state"], 2);
        assert_eq!(status["pairing_keys_ready"], false);
        assert_eq!(status["control_connection"]["stage"], "idle");
        assert_eq!(status["fleet"]["phase"], "idle");
        assert_eq!(status["ownership_intent"], "none");
        assert_eq!(status["want_omarchy_seat"], false);
        assert!(status.get("left").is_none());
        assert!(status.get("case").is_none());
        assert_eq!(daemon.connection().bluez(), None);
        assert_eq!(status["ble_connection_state"], 255);
        assert_eq!(status["last_advertisement_ms"], 0);
    }

    #[test]
    fn a_proximity_advertisement_names_this_accessory_and_leaves_a_foreign_one() {
        use crate::step::Input;

        let mut daemon = Daemon::new("gram");
        let now = clock();
        daemon.step(Input::Proximity { address: rpa(), payload: vec![0x07, 0x19, 0x01] }, now);
        assert_eq!(daemon.status_value(now)["lid_state"], 2);

        daemon.step(Input::Proximity { address: rpa(), payload: captured_case(0x39) }, now);
        let seen = daemon.status_value(now);
        assert_eq!(seen["lid_state"], 1);
        assert_eq!(seen["last_known_lid_state"], 1);
        assert_eq!(seen["model_int"], 11);
        assert_eq!(seen["case"]["level"], 10);
        assert_eq!(seen["case"]["charging"], false);
        assert!(seen["left"].get("level").is_none());
        assert_eq!(seen["left"]["in_case"], true);
        assert_eq!(seen["ble_connection_state"], 4);
        assert_eq!(seen["last_advertisement_ms"], 1_700_000_000_000_u64);
        assert_eq!(seen["lid_updated_ms"], 1_700_000_000_000_u64);

        daemon
            .set_accessory_keys(addr(3), AccessoryKeys { irk: [0x11; 16], encryption: [0x22; 16] });
        daemon.select_accessory(Some(addr(3)));
        daemon.step(Input::Proximity { address: rpa(), payload: captured_case(0x31) }, now);
        assert_eq!(daemon.status_value(now)["lid_state"], 1);
        assert_eq!(daemon.status_value(now)["last_advertisement_ms"], 1_700_000_000_000_u64);

        daemon.set_accessory_keys(
            addr(3),
            AccessoryKeys {
                irk: (0u8..16).collect::<Vec<_>>().try_into().unwrap(),
                encryption: [0x22; 16],
            },
        );
        daemon.step(Input::Proximity { address: rpa(), payload: encrypted_case() }, now);
        let decoded = daemon.status_value(now);
        assert_eq!(decoded["left"]["level"], 83);
        assert_eq!(decoded["left"]["charging"], true);
        assert_eq!(decoded["left"]["in_case"], true);
        assert_eq!(decoded["right"]["level"], 60);
        assert_eq!(decoded["case"]["level"], 40);
        assert_eq!(decoded["case"]["charging"], true);

        ready_link(&mut daemon, now);
        assert_eq!(daemon.status_value(now)["left"]["level"], 80);
        let mut open = encrypted_case();
        open[7] = 0x02;
        open[8] = 0x00;
        daemon.step(Input::Proximity { address: rpa(), payload: open }, now);
        let held = daemon.status_value(now);
        assert_eq!(held["left"]["level"], 80);
        assert_eq!(held["case"]["level"], 20);
        assert_eq!(held["lid_state"], 0);
        assert_eq!(daemon.connection().stage(), Stage::Ready);
    }

    #[test]
    fn a_second_known_accessory_is_tracked_without_touching_the_selected_one() {
        use crate::step::Input;

        fn rpa_for(irk: &[u8; 16]) -> Address {
            use aes::cipher::{BlockEncrypt, KeyInit};
            let prand = [0x40u8, 0x22, 0x33];
            let mut padded = [0u8; 16];
            padded[0] = prand[2];
            padded[1] = prand[1];
            padded[2] = prand[0];
            let mut secret = *irk;
            secret.reverse();
            let mut block = padded;
            block.reverse();
            let cipher = aes::Aes128::new(&secret.into());
            let mut out = aes::cipher::generic_array::GenericArray::from(block);
            cipher.encrypt_block(&mut out);
            let mut hashed: [u8; 16] = out.into();
            hashed.reverse();
            Address::new([prand[0], prand[1], prand[2], hashed[2], hashed[1], hashed[0]]).unwrap()
        }

        let mut daemon = Daemon::new("xps");
        let now = clock();
        let first = addr(3);
        let second = addr(9);
        daemon.select_accessory(Some(first));
        daemon.set_accessory_keys(
            first,
            AccessoryKeys {
                irk: (0u8..16).collect::<Vec<_>>().try_into().unwrap(),
                encryption: [0x22; 16],
            },
        );
        let second_irk: [u8; 16] = (16u8..32).collect::<Vec<_>>().try_into().unwrap();
        daemon
            .set_accessory_keys(second, AccessoryKeys { irk: second_irk, encryption: [0x33; 16] });

        daemon.step(Input::Proximity { address: rpa(), payload: captured_case(0x39) }, now);
        daemon.step(
            Input::Proximity { address: rpa_for(&second_irk), payload: captured_case(0x31) },
            now,
        );

        let status = daemon.status_value(now);
        assert_eq!(status["lid_state"], 1, "flat fields stay the selected pair");
        let accessories = status["accessories"].as_array().unwrap();
        assert_eq!(accessories.len(), 2);
        let entry = |address: Address| {
            accessories
                .iter()
                .find(|entry| entry["address"] == address.display())
                .unwrap_or_else(|| panic!("no status entry for {address}"))
        };
        assert_eq!(entry(first)["selected"], true);
        assert_eq!(entry(first)["lid_state"], 1);
        assert_eq!(entry(second)["selected"], false);
        assert_eq!(entry(second)["keys_ready"], true);
        assert_eq!(entry(second)["lid_state"], 0);
        assert_eq!(entry(second)["case"]["level"], 10);
        assert_eq!(entry(second)["left"]["in_case"], true);
        assert!(daemon.tracked.contains_key(&second));

        // Selecting the second pair keeps the first pair's report tracked.
        daemon.step(Input::Command(Command::Select(second)), now);
        daemon.step(Input::Paired { address: second, name: "AirPods Pro".into() }, now);
        assert_eq!(daemon.config.accessory, Some(second));
        assert!(daemon.tracked.contains_key(&first));
        let status = daemon.status_value(now);
        let accessories = status["accessories"].as_array().unwrap();
        assert_eq!(accessories.len(), 2);
        let entry = |address: Address| {
            accessories
                .iter()
                .find(|entry| entry["address"] == address.display())
                .unwrap_or_else(|| panic!("no status entry for {address}"))
        };
        assert_eq!(entry(second)["selected"], true);
        assert_eq!(entry(second)["lid_state"], 0);
        assert_eq!(entry(first)["selected"], false);
        assert_eq!(entry(first)["lid_state"], 1);
    }

    fn rpa_for(irk: &[u8; 16]) -> Address {
        use aes::cipher::{BlockEncrypt, KeyInit};
        let prand = [0x40u8, 0x22, 0x33];
        let mut padded = [0u8; 16];
        padded[0] = prand[2];
        padded[1] = prand[1];
        padded[2] = prand[0];
        let mut secret = *irk;
        secret.reverse();
        let mut block = padded;
        block.reverse();
        let cipher = aes::Aes128::new(&secret.into());
        let mut out = aes::cipher::generic_array::GenericArray::from(block);
        cipher.encrypt_block(&mut out);
        let mut hashed: [u8; 16] = out.into();
        hashed.reverse();
        Address::new([prand[0], prand[1], prand[2], hashed[2], hashed[1], hashed[0]]).unwrap()
    }

    fn worn_frame() -> Vec<u8> {
        let mut frame = captured_case(0x31);
        frame[5] = 0x2a; // primary left, both buds in ear, neither in case
        frame
    }

    fn max2_frame(status: u8) -> Vec<u8> {
        let mut frame: Vec<u8> =
            "07 19 01 2d 20 23 f9 8f 06 13 04 74 0f 4d 64 71 ed 5b b0 11 06 6e 42 36 72 eb 01"
                .split_whitespace()
                .map(|byte| u8::from_str_radix(byte, 16).unwrap())
                .collect();
        frame[5] = status;
        frame
    }

    #[test]
    fn max2_wear_promotes_only_a_trusted_candidate_and_preserves_selected_use() {
        use crate::step::{Effect, Input};
        for selected_worn in [false, true] {
            let (mut daemon, first, max, irk) = two_pair_daemon();
            let now = clock();
            let at = |ms| Clock::new(now.mono.0 + ms, now.unix.0 + ms);
            daemon.step(
                Input::Proximity {
                    address: rpa(),
                    payload: if selected_worn { worn_frame() } else { captured_case(0x39) },
                },
                now,
            );
            let advertiser = rpa_for(&irk);
            for status in [0x20, 0x21] {
                daemon.step(
                    Input::Proximity { address: advertiser, payload: max2_frame(status) },
                    now,
                );
                assert_eq!(daemon.config.accessory, Some(first));
                assert_eq!(daemon.tracked[&max].wearing(), Some(false));
            }
            daemon
                .step(Input::Proximity { address: addr(77), payload: max2_frame(0x23) }, at(1000));
            assert_eq!(
                daemon.tracked[&max].wearing(),
                Some(false),
                "untrusted transmitter cannot promote"
            );
            let result = daemon.step(
                Input::Proximity { address: advertiser, payload: max2_frame(0x23) },
                at(2000),
            );
            assert_eq!(daemon.config.accessory, Some(if selected_worn { first } else { max }));
            assert!(!result
                .effects
                .iter()
                .any(|effect| matches!(effect, Effect::PausePlayers(_) | Effect::MuteStreams(_))));
            let report = if selected_worn { &daemon.tracked[&max] } else { &daemon.accessory };
            assert_eq!(report.wearing(), Some(true));
            assert!(report.left_placement.is_unknown() && report.right_placement.is_unknown());
            assert!(!report.stowed() && !report.both_docked());
            assert!(daemon.status_value(at(2000))["accessories"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["headset"]["worn"] == true));
        }
    }

    #[test]
    fn removing_max2_cancels_pending_insertion_and_cached_lid_cannot_make_it_available() {
        use crate::step::Input;
        let mut daemon = Daemon::new("xps");
        let now = clock();
        let at = |ms| Clock::new(now.mono.0 + ms, now.unix.0 + ms);
        daemon.select_accessory(Some(addr(3)));
        // Older decoders could leave an unrelated lid interpretation behind.
        daemon.accessory.note_lid(LidState::Open, now.mono);
        daemon.step(Input::Proximity { address: addr(3), payload: max2_frame(0x20) }, now);
        assert!(!daemon.accessory.connection_available(now.mono));
        daemon.step(Input::Proximity { address: addr(3), payload: max2_frame(0x23) }, at(1500));
        assert!(daemon.inserted_at.is_some());
        daemon.step(Input::Proximity { address: addr(3), payload: max2_frame(0x21) }, at(2000));
        assert!(daemon.inserted_at.is_none());
        assert!(!daemon.accessory.connection_available(at(2000).mono));
    }

    #[test]
    fn waiting_status_uses_the_same_availability_as_connection_admission() {
        for model in [Model::AirPodsPro3, Model::AirPodsMax2] {
            for available in [false, true] {
                let mut daemon = Daemon::new("xps");
                let now = clock();
                daemon.select_accessory(Some(addr(3)));
                daemon.connection.wait_for_request();
                daemon.accessory.note_model(model);
                if model.is_headset() {
                    // Legacy case data and a nearby connected phone cannot
                    // override an unworn headset in either policy or status.
                    daemon.accessory.note_lid(LidState::Open, now.mono);
                    daemon.accessory.note_advertisement(now.mono, 4);
                    daemon.accessory.note_head_worn(available, now.mono);
                } else {
                    daemon.accessory.note_placement(
                        false,
                        Some(available),
                        Some(!available),
                        now.mono,
                    );
                }
                for elapsed in [0, 11_000] {
                    let at = Clock::new(now.mono.0 + elapsed, now.unix.0 + elapsed);
                    let expected = available && elapsed == 0;
                    assert_eq!(daemon.accessory.connection_available(at.mono), expected);
                    assert_eq!(
                        daemon.status_value(at)["control_connection"]["stage"],
                        if expected { "idle" } else { "waiting" }
                    );
                }
            }
        }
    }

    #[test]
    fn max2_availability_wakes_only_the_preferred_host_and_never_claims_audio() {
        use crate::fleet::members::{Fleet, Member};
        use crate::step::{Effect, Input};
        for local_preferred in [true, false] {
            let mut daemon = Daemon::new("xps");
            let now = clock();
            let at = |ms| Clock::new(now.mono.0 + ms, now.unix.0 + ms);
            let (local, gram, max) = (addr(1), addr(2), addr(3));
            daemon.set_local_address(Some(local));
            daemon.select_accessory(Some(max));
            daemon.restore_fleet(Fleet {
                id: FleetId::new([3; 16]),
                key: GroupKey::new([0x11; 32]),
                members: vec![
                    Member { address: local, label: "xps".into() },
                    Member { address: gram, label: "gram".into() },
                ],
            });
            daemon.activity_preference.note_local(
                [3; 16],
                max,
                if local_preferred { local } else { gram },
                now.unix,
            );
            daemon.step(Input::Proximity { address: max, payload: max2_frame(0x20) }, now);
            let off = daemon.step(Input::Tick, at(3000));
            assert!(!off.effects.iter().any(|effect| matches!(effect, Effect::OpenLink { .. })));
            daemon.step(Input::Proximity { address: max, payload: max2_frame(0x23) }, at(4000));
            let on = daemon.step(Input::Tick, at(6000));
            assert_eq!(
                on.effects.iter().any(|effect| matches!(effect, Effect::OpenLink { .. })),
                local_preferred
            );
            assert!(!daemon.output.wants_output());
            assert!(!on
                .effects
                .iter()
                .any(|effect| matches!(effect, Effect::MuteStreams(_) | Effect::PausePlayers(_))));
            assert!(daemon.accessory.playback_available(at(6000).mono));
            assert!(
                !daemon.accessory.playback_available(at(15000).mono),
                "a stale worn report is not availability"
            );
            daemon.accessory.expire(at(130000).mono, true);
            assert_eq!(
                daemon.accessory.wearing(),
                None,
                "missing advertisements do not invent removal"
            );
        }
    }

    fn two_pair_daemon() -> (Daemon, Address, Address, [u8; 16]) {
        let mut daemon = Daemon::new("xps");
        let first = addr(3);
        let second = addr(9);
        daemon.select_accessory(Some(first));
        daemon.set_accessory_keys(
            first,
            AccessoryKeys {
                irk: (0u8..16).collect::<Vec<_>>().try_into().unwrap(),
                encryption: [0x22; 16],
            },
        );
        let second_irk: [u8; 16] = (16u8..32).collect::<Vec<_>>().try_into().unwrap();
        daemon
            .set_accessory_keys(second, AccessoryKeys { irk: second_irk, encryption: [0x33; 16] });
        (daemon, first, second, second_irk)
    }

    #[test]
    fn accessory_status_keeps_last_seen_times_separate() {
        let (mut daemon, first, second, _) = two_pair_daemon();
        let now = Clock::new(10_000, 100_000);
        daemon.accessory.note_advertisement(MonoMs(9_000), 0);
        let mut max = Accessory::new();
        max.set_identity("AirPods Max", "A3454");
        max.note_advertisement(MonoMs(8_000), 0);
        daemon.tracked.insert(second, max);
        let status = daemon.status_value(now);
        let entries = status["accessories"].as_array().unwrap();
        let selected = entries.iter().find(|a| a["address"] == first.display()).unwrap();
        let tracked = entries.iter().find(|a| a["address"] == second.display()).unwrap();
        assert_eq!(selected["last_advertisement_ms"], 99_000);
        assert_eq!(tracked["last_advertisement_ms"], 98_000);
        assert_eq!(tracked["is_headset"], true);
        daemon.tracked.remove(&second);
        let status = daemon.status_value(now);
        let unheard = status["accessories"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["address"] == second.display())
            .unwrap();
        assert_eq!(unheard["last_advertisement_ms"], 0);
    }

    #[test]
    fn wearing_a_tracked_pair_promotes_it_and_keeps_both_pairs() {
        use crate::step::{Effect, Input};
        let (mut daemon, first, second, second_irk) = two_pair_daemon();
        let now = clock();
        daemon.config.noise = Some(NoiseMode::Anc);
        // Selected pair sits docked in its case.
        daemon.step(Input::Proximity { address: rpa(), payload: captured_case(0x39) }, now);
        assert_eq!(daemon.config.accessory, Some(first));

        // The tracked pair goes into the user's ears: it becomes selected.
        let outcome = daemon
            .step(Input::Proximity { address: rpa_for(&second_irk), payload: worn_frame() }, now);
        assert_eq!(daemon.config.accessory, Some(second));
        assert!(daemon.tracked.contains_key(&first), "the first pair stays tracked");
        assert!(daemon.accessory.wearing() == Some(true), "the promoted pair's report is live");
        assert!(outcome.effects.iter().any(|effect| matches!(
            effect,
            Effect::SelectAccessory { accessory: Some(address) } if *address == second
        )));
        // Per-pair prefs: the first pair's Anc choice is saved, not inherited.
        assert_eq!(daemon.config.noise, None);
        assert_eq!(
            daemon.config.saved_prefs.get(&first).and_then(|prefs| prefs.noise),
            Some(NoiseMode::Anc)
        );
        // Selecting back restores the first pair and its prefs.
        let back = daemon.step(Input::Command(Command::Select(first)), now);
        assert_eq!(back.reply.as_deref(), Some("ok\n"));
        assert_eq!(daemon.config.accessory, Some(first));
        assert_eq!(daemon.config.noise, Some(NoiseMode::Anc));
        assert!(daemon.tracked.contains_key(&second));
    }

    #[test]
    fn delayed_pairing_reports_cannot_undo_wear_or_manual_selection() {
        use crate::step::{Effect, Input};
        let (mut daemon, first, second, second_irk) = two_pair_daemon();
        let now = clock();
        daemon.set_identity("AirPods Max", "A3454");
        daemon.accessory.note_head_worn(false, now.mono);
        daemon.step(Input::Proximity { address: rpa_for(&second_irk), payload: worn_frame() }, now);
        assert_eq!(daemon.config.accessory, Some(second));
        assert_eq!(daemon.accessory.wearing(), Some(true));

        // The radio's acknowledgement of its old selection arrives after wear
        // has selected the buds. It must not enqueue another selection of Max.
        for address in [first, second, first, second] {
            let report = daemon.step(Input::Paired { address, name: "Radio name".into() }, now);
            assert_eq!(daemon.config.accessory, Some(second));
            assert_eq!(daemon.accessory.wearing(), Some(true));
            assert_eq!(daemon.status_value(now)["is_headset"], false);
            assert!(!report.effects.iter().any(|effect| matches!(
                effect,
                Effect::SelectAccessory { .. } | Effect::Disconnect { .. }
            )));
        }

        // An explicit selection still wins, even over worn buds; their queued
        // acknowledgement must likewise not undo that decision.
        let picked = daemon.step(Input::Command(Command::Select(first)), now);
        assert_eq!(picked.reply.as_deref(), Some("ok\n"));
        for address in [second, first] {
            let report = daemon.step(Input::Paired { address, name: "Radio name".into() }, now);
            assert_eq!(daemon.config.accessory, Some(first));
            assert_eq!(daemon.status_value(now)["is_headset"], true);
            assert!(!report.effects.iter().any(|effect| matches!(
                effect,
                Effect::SelectAccessory { .. } | Effect::Disconnect { .. }
            )));
        }
        assert!(daemon.tracked.contains_key(&second));
    }

    #[test]
    fn switching_pairs_releases_the_slot_and_reopen_holds() {
        use crate::step::Input;
        let (mut daemon, first, second, second_irk) = two_pair_daemon();
        let now = clock();
        daemon.step(Input::Proximity { address: rpa(), payload: captured_case(0x39) }, now);
        assert_eq!(daemon.config.accessory, Some(first));
        // A yielded slot and a pending owner-reconnect hold are in force.
        daemon.slot_hold_until = Some(now.mono.saturating_add(60_000));
        daemon.reopen_wait = true;
        daemon.reopen_hold_until = Some(now.mono.saturating_add(60_000));
        // Putting the tracked pair in is the switch gesture: the holds were
        // earned against the previous pair and must not delay this one.
        daemon.step(Input::Proximity { address: rpa_for(&second_irk), payload: worn_frame() }, now);
        assert_eq!(daemon.config.accessory, Some(second));
        assert_eq!(daemon.slot_hold_until, None, "the switch supersedes the slot hold");
        assert!(!daemon.reopen_wait, "the switch supersedes the reopen hold");
        assert_eq!(daemon.reopen_hold_until, None);
    }

    #[test]
    fn a_reported_paired_accessory_is_named_and_selectable() {
        use crate::step::Input;
        let (mut daemon, first, second, _irk) = two_pair_daemon();
        let now = clock();
        daemon.step(
            Input::KnownAccessory { address: second, name: "AirPods Pro".into(), connected: false },
            now,
        );
        let status = daemon.status_value(now);
        let accessories = status["accessories"].as_array().unwrap();
        let entry = accessories
            .iter()
            .find(|entry| entry["address"] == second.display())
            .expect("known pair listed");
        assert_eq!(entry["device_name"], "AirPods Pro");
        assert_eq!(entry["selected"], false);
        assert_eq!(daemon.config.accessory, Some(first), "reporting does not select it");
        // It is selectable so its keys can be harvested on connection.
        let picked = daemon.step(Input::Command(Command::Select(second)), now);
        assert_eq!(picked.reply.as_deref(), Some("ok\n"));
        assert_eq!(daemon.config.accessory, Some(second));
    }

    #[test]
    fn a_connected_accessory_promotes_when_the_selected_pair_is_not_worn() {
        use crate::step::Input;
        for (scenario, selected_worn, connected) in [
            ("accessory connects, selected in case", false, true),
            ("selected pair worn", true, true),
            ("accessory not connected", false, false),
        ] {
            let mut daemon = Daemon::new("xps");
            let now = clock();
            let first = addr(3);
            let max = addr(9);
            daemon.select_accessory(Some(first));
            if selected_worn {
                daemon.note_pod(
                    false,
                    Pod {
                        battery: Battery::new(80, false).unwrap(),
                        in_ear: Some(true),
                        in_case: Some(false),
                    },
                    now.mono,
                );
            } else {
                for right in [false, true] {
                    daemon.note_pod(
                        right,
                        Pod {
                            battery: Battery::new(80, true).unwrap(),
                            in_ear: Some(false),
                            in_case: Some(true),
                        },
                        now.mono,
                    );
                }
            }
            daemon.step(
                Input::KnownAccessory { address: max, name: "AirPods Max".into(), connected },
                now,
            );
            assert_eq!(
                daemon.config.accessory,
                Some(if connected && !selected_worn { max } else { first }),
                "{scenario}"
            );
        }
    }

    #[test]
    fn pressing_play_promotes_a_worn_tracked_pair() {
        use crate::step::Input;
        let (mut daemon, first, second, second_irk) = two_pair_daemon();
        let now = clock();
        // Selected pair worn first, so the tracked pair's wear cannot promote.
        daemon.step(Input::Proximity { address: rpa(), payload: worn_frame() }, now);
        daemon.step(Input::Proximity { address: rpa_for(&second_irk), payload: worn_frame() }, now);
        assert_eq!(daemon.config.accessory, Some(first));
        // Both reports age out; only the tracked pair is then seen worn
        // again (as after a daemon restart, where no transition fires).
        let later = Clock::new(now.mono.0 + 121_000, now.unix.0 + 121_000);
        daemon.step(Input::Tick, later);
        assert_ne!(daemon.accessory.wearing(), Some(true));
        daemon.tracked.get_mut(&second).unwrap().note_placement(
            false,
            Some(true),
            Some(false),
            later.mono,
        );
        daemon.claim_playback(later);
        assert_eq!(daemon.config.accessory, Some(second));
    }

    #[test]
    fn a_dropped_link_clears_the_transport_so_play_can_promote_the_worn_pair() {
        use crate::step::Input;
        let (mut daemon, first, second, second_irk) = two_pair_daemon();
        let now = clock();
        // Selected pair docked but still streaming to its sink: a worn
        // tracked pair can promote neither on wear nor on play.
        daemon.step(Input::Proximity { address: rpa(), payload: captured_case(0x39) }, now);
        daemon.audio.set_local_playing_for_test(true);
        daemon.step(Input::Transport { active: true }, now);
        daemon.step(Input::Proximity { address: rpa_for(&second_irk), payload: worn_frame() }, now);
        assert_eq!(daemon.config.accessory, Some(first));
        daemon.claim_playback(now);
        assert_eq!(daemon.config.accessory, Some(first), "in use: no promotion");

        // The selected pair's link drops. Its transport is gone with it;
        // a stale active flag must not keep the promotion gate shut.
        daemon.step(Input::Bluez(false), now);
        assert!(!daemon.transport_active, "no link, no active transport");
        daemon.claim_playback(now);
        assert_eq!(daemon.config.accessory, Some(second));
    }

    #[test]
    fn wearing_a_tracked_pair_does_not_steal_from_a_worn_selected_pair() {
        use crate::step::Input;
        let (mut daemon, first, second, second_irk) = two_pair_daemon();
        let now = clock();
        // Selected pair is in the user's ears.
        daemon.step(Input::Proximity { address: rpa(), payload: worn_frame() }, now);
        daemon.step(Input::Proximity { address: rpa_for(&second_irk), payload: worn_frame() }, now);
        assert_eq!(
            daemon.config.accessory,
            Some(first),
            "no steal while the selected pair is worn"
        );
        assert!(daemon.tracked.contains_key(&second), "it is still tracked");
    }

    #[test]
    fn disconnected_advertisements_replace_old_control_placement() {
        use crate::step::{Effect, Input};

        // Both laptops can lose their control slots; neither needs a seated
        // fleet peer to keep receiving placement and lid telemetry.
        for name in ["xps", "gram"] {
            for previously_docked in [[false, false], [true, false], [false, true], [true, true]] {
                let mut daemon = Daemon::new(name);
                let now = clock();
                ready_link(&mut daemon, now);
                daemon.set_accessory_keys(
                    addr(3),
                    AccessoryKeys {
                        irk: (0u8..16).collect::<Vec<_>>().try_into().unwrap(),
                        encryption: [0x22; 16],
                    },
                );
                for right in [false, true] {
                    let docked = previously_docked[usize::from(right)];
                    daemon.note_pod(
                        right,
                        Pod {
                            battery: Battery::new(90, docked).unwrap(),
                            in_ear: Some(!docked),
                            in_case: Some(docked),
                        },
                        now.mono,
                    );
                }
                let generation = daemon.connection.generation();
                daemon.step(Input::ControlSocket { up: false, generation }, now);
                assert_ne!(daemon.connection.stage(), Stage::Ready);

                // Left is charging in the case, right is no longer in an ear.
                // The right pod's case placement is unknown, not its old value.
                let mut frame = encrypted_case();
                frame[5] = 0x60;
                frame[8] = 0;
                let next = Clock::new(now.mono.0 + 1000, now.unix.0 + 1000);
                let outcome =
                    daemon.step(Input::Proximity { address: rpa(), payload: frame.clone() }, next);
                let status = daemon.status_value(next);
                assert_eq!(status["connected"], false);
                assert_eq!(status["left"]["in_ear"], false);
                assert_eq!(status["left"]["in_case"], true);
                assert_eq!(status["left"]["level"], 83);
                assert_eq!(status["right"]["in_ear"], false);
                assert!(status["right"].get("in_case").is_none(), "{status}");
                assert_eq!(status["lid_state"], 0);
                assert!(!outcome.effects.iter().any(|effect| matches!(
                    effect,
                    Effect::OpenLink { .. } | Effect::WriteControl { .. }
                )));

                // Returning both buds and closing the case replaces placement
                // again even though the control link has not returned.
                frame[5] = 0x64;
                frame[8] = 8;
                daemon.step(Input::Proximity { address: rpa(), payload: frame }, next);
                let status = daemon.status_value(next);
                assert_eq!(status["left"]["in_case"], true);
                assert_eq!(status["right"]["in_case"], true);
                assert_eq!(status["case_confirmed_docked"], true);
                assert_eq!(status["lid_state"], 1);
            }
        }
    }

    #[test]
    fn a_docked_pods_both_in_case_frame_does_not_dock_the_worn_pod() {
        use crate::step::Input;
        use aes::cipher::{BlockEncrypt, KeyInit};
        // XPS, 2026-10-19 20:53:15: with the left pod worn, the right pod docked
        // and sent "both in case, neither in an ear", then 0.27 s later "left
        // in ear". The panel briefly showed the left pod entering the case.
        let mut daemon = Daemon::new("xps");
        let now = clock();
        ready_link(&mut daemon, now);
        daemon.set_accessory_keys(
            addr(3),
            AccessoryKeys {
                irk: (0u8..16).collect::<Vec<_>>().try_into().unwrap(),
                encryption: [0x22; 16],
            },
        );
        let generation = daemon.connection.generation();
        daemon.step(Input::ControlSocket { up: false, generation }, now);
        daemon.step(Input::Bluez(false), now);
        let mut plain = [0u8; 16];
        plain[1] = 81;
        plain[2] = 84;
        plain[3] = 80;
        let mut block = aes::cipher::generic_array::GenericArray::from(plain);
        aes::Aes128::new((&[0x22; 16]).into()).encrypt_block(&mut block);
        // Status bytes, all sent by the right pod: both worn; docked claiming
        // both docked; docked with the left in an ear.
        for (index, (flags, left_ear, right_case)) in
            [(0x0b, true, false), (0x44, true, true), (0x42, true, true)].into_iter().enumerate()
        {
            let mut frame = vec![0x07, 0x19, 0x01, 0x27, 0x20, flags, 0x88, 0x8f, 0x11, 0, 8];
            frame.extend_from_slice(&block);
            let at = Clock::new(now.mono.0 + 1000 + index as u64 * 300, now.unix.0 + 1000);
            daemon.step(Input::Proximity { address: rpa(), payload: frame }, at);
            let status = daemon.status_value(at);
            assert_eq!(status["left"]["in_ear"], left_ear, "frame {flags:#04x}: {status}");
            assert_ne!(status["left"]["in_case"], true, "frame {flags:#04x}: {status}");
            assert_eq!(status["right"]["in_case"], right_case, "frame {flags:#04x}: {status}");
        }
    }

    #[test]
    fn a_pods_own_advertisement_outranks_the_other_pods_view_of_it() {
        use crate::step::Input;
        use aes::cipher::{BlockEncrypt, KeyInit};
        // XPS, 2026-10-19 20:55:58: the left pod advertised itself out of the
        // ear while the docked right pod, interleaved, still said "worn".
        let mut daemon = Daemon::new("xps");
        let now = clock();
        ready_link(&mut daemon, now);
        daemon.set_accessory_keys(
            addr(3),
            AccessoryKeys {
                irk: (0u8..16).collect::<Vec<_>>().try_into().unwrap(),
                encryption: [0x22; 16],
            },
        );
        let generation = daemon.connection.generation();
        daemon.step(Input::ControlSocket { up: false, generation }, now);
        daemon.step(Input::Bluez(false), now);
        let mut block = aes::cipher::generic_array::GenericArray::from([0u8; 16]);
        aes::Aes128::new((&[0x22; 16]).into()).encrypt_block(&mut block);
        let send = |daemon: &mut Daemon, flags: u8, ms: u64| {
            let mut frame = vec![0x07, 0x19, 0x01, 0x27, 0x20, flags, 0x88, 0x8f, 0x11, 0, 8];
            frame.extend_from_slice(&block);
            let at = Clock::new(now.mono.0 + ms, now.unix.0 + ms);
            daemon.step(Input::Proximity { address: rpa(), payload: frame }, at);
            daemon.status_value(at)
        };
        // Right pod docked, left worn; then the left pod says it is out; then
        // the docked right pod repeats its older "left worn".
        send(&mut daemon, 0x42, 1_000);
        assert_eq!(send(&mut daemon, 0x20, 1_100)["left"]["in_ear"], false);
        assert_eq!(
            send(&mut daemon, 0x42, 1_150)["left"]["in_ear"],
            false,
            "the left pod spoke for itself"
        );
        // Once the left pod is quiet for a while, the other pod's view counts again.
        assert_eq!(send(&mut daemon, 0x42, 2_700)["left"]["in_ear"], true);
    }

    #[test]
    fn removing_pods_after_eviction_updates_without_activity_or_reconnecting() {
        use crate::step::{Effect, Input};
        use aes::cipher::{BlockEncrypt, KeyInit};

        let mut daemon = Daemon::new("xps");
        let now = clock();
        ready_link(&mut daemon, now);
        daemon.set_accessory_keys(
            addr(3),
            AccessoryKeys {
                irk: (0u8..16).collect::<Vec<_>>().try_into().unwrap(),
                encryption: [0x22; 16],
            },
        );
        for right in [false, true] {
            daemon.note_pod(
                right,
                Pod {
                    battery: Battery::new(84, !right).unwrap(),
                    in_ear: Some(right),
                    in_case: Some(!right),
                },
                now.mono,
            );
        }
        let generation = daemon.connection.generation();
        daemon.step(Input::ControlSocket { up: false, generation }, now);
        daemon.step(Input::Bluez(false), now);

        // Observed Pro 3 headers: right worn first, then both worn. Use test
        // keys and non-charging battery values, never captured secret material.
        for (index, flags) in [0x03, 0x0b].into_iter().enumerate() {
            let mut frame = vec![0x07, 0x19, 0x01, 0x27, 0x20, flags, 0x88, 0x8f, 0x11, 0, 8];
            let mut plain = [0u8; 16];
            plain[1] = 81;
            plain[2] = 84;
            plain[3] = 80;
            let mut block = aes::cipher::generic_array::GenericArray::from(plain);
            aes::Aes128::new((&[0x22; 16]).into()).encrypt_block(&mut block);
            frame.extend_from_slice(&block);
            let at = Clock::new(
                now.mono.0 + 1000 + index as u64 * 1000,
                now.unix.0 + 1000 + index as u64 * 1000,
            );
            let result = daemon.step(Input::Proximity { address: rpa(), payload: frame }, at);
            let status = daemon.status_value(at);
            assert_eq!(status["connected"], false);
            assert_eq!(status["right"]["in_ear"], true);
            assert_eq!(status["right"]["in_case"], false);
            assert_eq!(status["left"]["in_ear"], index == 1);
            if index == 0 {
                assert!(
                    status["left"].get("in_case").is_none(),
                    "old docking must not survive: {status}"
                );
            } else {
                assert_eq!(status["left"]["in_case"], false);
            }
            assert!(!result.effects.iter().any(|effect| matches!(
                effect,
                Effect::OpenLink { .. } | Effect::WriteControl { .. }
            )));
        }
        let later = Clock::new(now.mono.0 + 20_000, now.unix.0 + 20_000);
        let idle = daemon.step(Input::Tick, later);
        assert_eq!(daemon.status_value(later)["connected"], false);
        assert!(!idle.effects.iter().any(|effect| matches!(effect, Effect::OpenLink { .. })));
    }

    #[test]
    fn missing_advertised_battery_does_not_restore_old_docking() {
        let mut daemon = Daemon::new("gram");
        let now = clock();
        daemon.note_pod(
            false,
            Pod {
                battery: Battery::new(90, true).unwrap(),
                in_ear: Some(false),
                in_case: Some(true),
            },
            now.mono,
        );
        daemon.accessory.note_placement(false, Some(false), None, now.mono);
        daemon.paint_bud(false, None, Some(false), None, now.mono);
        let status = daemon.status_value(now);
        assert_eq!(status["left"]["level"], 90);
        assert_eq!(status["left"]["in_ear"], false);
        assert!(status["left"].get("in_case").is_none());
    }

    #[test]
    fn expired_placement_does_not_return_through_cached_battery() {
        let mut daemon = Daemon::new("gram");
        let now = clock();
        daemon.note_pod(
            false,
            Pod {
                battery: Battery::new(90, false).unwrap(),
                in_ear: Some(true),
                in_case: Some(false),
            },
            now.mono,
        );
        let later = Clock::new(now.mono.0 + 121_000, now.unix.0 + 121_000);
        daemon.accessory.expire(later.mono, false);
        let status = daemon.status_value(later);
        assert_eq!(status["left"]["level"], 90);
        assert!(status["left"].get("in_ear").is_none());
        assert!(status["left"].get("in_case").is_none());
    }

    #[test]
    fn unpaired_advertisements_report_placement_without_inventing_batteries() {
        use crate::step::Input;
        let mut daemon = Daemon::new("xps");
        let mut frame = captured_case(0x39);
        for (index, status_byte, lid_byte, lid, left_ear, right_ear, docked) in [
            (0, 0x55, 0x39, 1, false, false, true),
            (1, 0x55, 0x31, 0, false, false, true),
            (2, 0x22, 0x31, 0, true, false, false),
            (3, 0x2a, 0x31, 0, true, true, false),
            (4, 0x55, 0x31, 0, false, false, true),
            (5, 0x55, 0x39, 1, false, false, true),
        ] {
            let now = Clock::new(1000 + index * 1000, 1_700_000_000_000 + index * 1000);
            frame[5] = status_byte;
            frame[8] = lid_byte;
            let turn =
                daemon.step(Input::Proximity { address: rpa(), payload: frame.clone() }, now);
            assert!(!turn
                .effects
                .iter()
                .any(|effect| matches!(effect, crate::step::Effect::OpenLink { .. })));
            assert!(!matches!(daemon.connection.stage(), Stage::Connecting | Stage::Retrying));
            let status = daemon.status_value(now);
            assert_eq!(status["left"]["in_ear"], left_ear);
            assert_eq!(status["right"]["in_ear"], right_ear);
            assert_eq!(status["lid_state"], lid);
            assert_eq!(status["case_confirmed_docked"], docked);
            assert_eq!(status["ear_reported"], true);
            assert_eq!(status["connected"], false);
            assert_eq!(status["pairing_keys_ready"], false);
            for side in ["left", "right"] {
                assert!(status[side].get("level").is_none());
                assert!(status[side].get("charging").is_none());
                if docked {
                    assert_eq!(status[side]["in_case"], true);
                }
            }
        }
        daemon.accessory.expire(MonoMs(200_000), false);
        assert!(!daemon.accessory.both_docked());
        let stale = daemon.status_value(Clock::new(200_000, 1_700_000_200_000));
        assert!(stale.get("left").is_none());
        assert!(stale.get("right").is_none());
    }

    #[test]
    fn pairing_indicator_survives_interleaved_telemetry_but_expires_or_closes() {
        let mut daemon = Daemon::new("xps");
        let mut frame = captured_case(0x31);
        frame[2] = 7;
        daemon.hear_proximity(rpa(), &frame, Clock::new(1000, 1000));
        assert_eq!(daemon.status_value(Clock::new(1000, 1000))["pairing_mode"], true);
        assert_eq!(daemon.status_value(Clock::new(4000, 4000))["pairing_mode"], false);
        assert!(daemon.accessory.left_placement.is_unknown());
        assert!(daemon.accessory.left.is_unknown());
        daemon.hear_proximity(rpa(), &captured_case(0x31), Clock::new(1500, 1500));
        assert_eq!(daemon.status_value(Clock::new(1500, 1500))["pairing_mode"], true);
        daemon.hear_proximity(rpa(), &captured_case(0x31), Clock::new(3900, 3900));
        assert_eq!(daemon.status_value(Clock::new(3900, 3900))["pairing_mode"], true);
        assert_eq!(daemon.status_value(Clock::new(4000, 4000))["pairing_mode"], false);
        daemon.hear_proximity(rpa(), &frame, Clock::new(4100, 4100));
        assert_eq!(daemon.status_value(Clock::new(4100, 4100))["pairing_mode"], true);
        daemon.hear_proximity(rpa(), &captured_case(0x39), Clock::new(4200, 4200));
        assert_eq!(daemon.status_value(Clock::new(4200, 4200))["pairing_mode"], false);
        daemon.hear_proximity(rpa(), &frame, Clock::new(4300, 4300));
        daemon.connection.note_bluez(true);
        assert_eq!(daemon.status_value(Clock::new(4300, 4300))["pairing_mode"], false);
        daemon.set_accessory_keys(addr(3), AccessoryKeys { irk: [1; 16], encryption: [2; 16] });
        daemon.hear_proximity(rpa(), &frame, Clock::new(4400, 4400));
        assert_eq!(daemon.status_value(Clock::new(4400, 4400))["pairing_mode"], false);
    }

    #[test]
    fn known_pairing_mode_cancels_reconnects_and_holds_until_reports_expire() {
        use crate::step::{Effect, Input};
        // Cover the case-open grace, an in-flight open, and its retry timer.
        for phase in 0..3 {
            let mut daemon = Daemon::new("gram");
            let pods = addr(9);
            daemon.select_accessory(Some(pods));
            daemon.set_accessory_keys(
                addr(9),
                AccessoryKeys {
                    irk: (0u8..16).collect::<Vec<_>>().try_into().unwrap(),
                    encryption: [0x22; 16],
                },
            );
            daemon.connection.prepare();
            daemon.step(
                Input::Proximity { address: rpa(), payload: captured_case(0x31) },
                Clock::new(1000, 1000),
            );
            daemon.step(Input::Command(Command::Activity), Clock::new(1000, 1000));
            if phase > 0 {
                let opened = daemon.step(Input::Tick, Clock::new(3000, 3000));
                assert!(opened.effects.iter().any(|e| matches!(e, Effect::OpenLink { .. })));
            }
            let old = daemon.connection.generation();
            if phase == 2 {
                daemon.step(
                    Input::LinkFailed { generation: old, busy: false },
                    Clock::new(3000, 3000),
                );
                assert_eq!(daemon.connection.stage(), Stage::Retrying);
            }
            let mut pairing = captured_case(0x31);
            pairing[2] = 7;
            let at = Clock::new(3100, 3100);
            let held =
                daemon.step(Input::Proximity { address: pods, payload: pairing.clone() }, at);
            assert_eq!(daemon.connection.stage(), Stage::Waiting);
            assert_ne!(daemon.connection.generation(), old);
            assert_eq!(
                held.effects.iter().any(|e| matches!(e, Effect::Disconnect { .. })),
                phase > 0
            );
            // Late callbacks from a canceled open cannot resurrect it.
            daemon.step(Input::ControlSocket { up: true, generation: old }, at);
            for time in [4000, 6000, 8000] {
                let at = Clock::new(time, time);
                daemon.step(Input::Proximity { address: pods, payload: pairing.clone() }, at);
                for input in [
                    Input::Proximity { address: rpa(), payload: captured_case(0x31) },
                    Input::Command(Command::Activity),
                    Input::Command(Command::Connect),
                    Input::Tick,
                ] {
                    let held = daemon.step(input, at);
                    assert!(!held.effects.iter().any(|e| matches!(e, Effect::OpenLink { .. })));
                    assert_eq!(daemon.connection.stage(), Stage::Waiting);
                }
                daemon.claim_playback(at);
                daemon.attach_available(daemon.connection.generation(), at.mono);
                assert_eq!(daemon.connection.stage(), Stage::Waiting);
            }
            let expired = Clock::new(11_000, 11_000);
            let quiet = daemon.step(Input::Tick, expired);
            assert!(!quiet.effects.iter().any(|e| matches!(e, Effect::OpenLink { .. })));
            let activity = daemon.step(Input::Command(Command::Activity), expired);
            assert!(activity.effects.iter().any(|e| matches!(e, Effect::OpenLink { .. })));
        }
    }

    #[test]
    fn pairing_hold_requires_known_identity_and_preserves_ready_links_and_first_setup() {
        use crate::step::{Effect, Input};
        let at = Clock::new(1000, 1000);
        let mut pairing = captured_case(0x31);
        pairing[2] = 7;
        let mut daemon = Daemon::new("gram");
        ready_link(&mut daemon, at);
        daemon.set_accessory_keys(
            addr(3),
            AccessoryKeys {
                irk: (0u8..16).collect::<Vec<_>>().try_into().unwrap(),
                encryption: [0x22; 16],
            },
        );
        daemon.step(Input::Proximity { address: addr(8), payload: pairing.clone() }, at);
        assert!(!daemon.pairing_elsewhere(at.mono), "another pair of the same model is unrelated");
        for address in [addr(3), rpa()] {
            let held = daemon.step(Input::Proximity { address, payload: pairing.clone() }, at);
            assert!(daemon.pairing_elsewhere(at.mono));
            assert_eq!(daemon.connection.stage(), Stage::Ready);
            assert!(!held.effects.iter().any(|e| matches!(e, Effect::Disconnect { .. })));
        }
        daemon.hear_proximity(rpa(), &captured_case(0x39), at);
        assert!(!daemon.pairing_elsewhere(at.mono), "verified closed lid ends pairing mode");

        let mut fresh = Daemon::new("xps");
        fresh.step(Input::Proximity { address: addr(3), payload: pairing.clone() }, at);
        fresh.step(Input::Paired { address: addr(3), name: "Chosen AirPods".into() }, at);
        fresh.step(Input::Proximity { address: addr(3), payload: pairing }, at);
        assert!(!fresh.pairing_elsewhere(at.mono), "first setup must be allowed to retrieve keys");
        let opened = fresh.step(Input::Command(Command::Connect), at);
        assert!(opened.effects.iter().any(|e| matches!(e, Effect::OpenLink { .. })));
    }

    #[test]
    fn nearby_pairs_keep_separate_state_and_bluetooth_selection_wins() {
        use crate::step::{Effect, Input};
        let mut daemon = Daemon::new("xps");
        let mut pair = captured_case(0x31);
        pair[2] = 7;
        let mut worn = captured_case(0x31);
        worn[3] = 0x0e; // another model, in the ears
        worn[5] = 0x2a;
        let a = addr(1);
        let b = addr(2);
        let now = Clock::new(1000, 1000);
        daemon.step(Input::Proximity { address: b, payload: worn.clone() }, now);
        assert_eq!(daemon.accessory.model, Model::AirPodsPro);
        daemon.step(Input::Proximity { address: a, payload: pair.clone() }, now);
        for address in [a, b] {
            daemon.step(
                Input::NearbyMetadata { address, name: "Same name".into(), rssi: Some(-50) },
                now,
            );
        }
        for (time, payload) in [(1100, worn), (1200, captured_case(0x39))] {
            let at = Clock::new(time, time);
            let turn = daemon.step(Input::Proximity { address: b, payload }, at);
            assert!(!turn.effects.iter().any(|effect| matches!(effect, Effect::OpenLink { .. })));
            let status = daemon.status_value(at);
            assert_eq!(status["pairing_mode"], true);
            assert_eq!(status["model_int"], 11);
            assert_eq!(status["lid_state"], 2); // not the OTHER closed case
            assert_eq!(status["nearby_airpods"].as_array().unwrap().len(), 2);
            assert_eq!(daemon.accessory.name, "Same name");
        }
        let later = Clock::new(1500, 1500);
        daemon.step(Input::Proximity { address: b, payload: pair }, later);
        let status = daemon.status_value(later);
        assert_eq!(status["pairing_candidates"], 2);
        let records = status["nearby_airpods"].as_array().unwrap();
        assert!(records.iter().any(|c| c["address"] == a.display() && c["selected"] == true));
        // Closing the first pair clears only its own indicator and retains its artwork.
        daemon.step(Input::Proximity { address: a, payload: captured_case(0x39) }, later);
        assert_eq!(daemon.status_value(later)["pairing_mode"], false);
        assert_eq!(daemon.status_value(later)["pairing_candidates"], 1);
        assert_eq!(daemon.status_value(later)["lid_state"], 1);
        // User selects the second pair, not the one that got focus first.
        daemon.step(Input::Proximity { address: b, payload: captured_case(0x31) }, later);
        daemon.step(Input::Paired { address: b, name: "Chosen AirPods".into() }, later);
        assert_eq!(daemon.config.accessory, Some(b));
        assert_eq!(daemon.accessory.name, "Chosen AirPods");
        assert_eq!(daemon.status_value(later)["lid_state"], 0);
        assert_eq!(daemon.status_value(later)["pairing_candidates"], 0);
        assert_eq!(daemon.status_value(later)["pairing_mode"], false);
    }

    #[test]
    fn a_new_links_battery_report_keeps_the_buds_in_the_ears() {
        use crate::step::Input;
        let mut daemon = Daemon::new("xps");
        let now = clock();
        daemon.select_accessory(Some(addr(3)));
        // Known worn from before this link, as an advertisement or the last link reported.
        let worn = Pod {
            battery: crate::types::Battery::new(60, false).unwrap(),
            in_ear: Some(true),
            in_case: Some(false),
        };
        daemon.note_pod(false, worn, now.mono);
        daemon.note_pod(true, worn, now.mono);
        daemon.step(Input::Command(Command::Connect), now);
        let generation = daemon.connection().generation();
        daemon.step(Input::ControlSocket { up: true, generation }, now);
        daemon
            .step(Input::Accessory { generation, packet: vec![0x01, 0x00, 0x04, 0x00, 0x01] }, now);
        daemon.step(
            Input::Accessory { generation, packet: vec![0x04, 0x00, 0x04, 0x00, 0x2b, 0x00, 0x01] },
            now,
        );
        // The new link reports batteries before the ears. That is not the buds leaving.
        let mut battery = vec![0x04, 0x00, 0x04, 0x00, 0x04, 0x00, 0x02];
        battery.extend_from_slice(&[0x04, 0x01, 61, 0x02, 0x01]);
        battery.extend_from_slice(&[0x02, 0x01, 52, 0x02, 0x01]);
        daemon.step(Input::Accessory { generation, packet: battery }, now);
        let status = daemon.status_value(now);
        assert_eq!(status["left"]["level"], 61);
        assert_eq!(status["left"]["in_ear"], true);
        assert_eq!(status["right"]["in_ear"], true);
    }

    fn key_packet(irk: u8, encryption: u8) -> Vec<u8> {
        let mut keys = vec![0x04, 0x00, 0x04, 0x00, 0x31, 0x00, 0x02, 0x01, 0x00, 0x10, 0x00];
        keys.extend_from_slice(&[irk; 16]);
        keys.extend_from_slice(&[0x04, 0x00, 0x10, 0x00]);
        keys.extend_from_slice(&[encryption; 16]);
        keys
    }

    fn secret_writes(effects: &[crate::step::Effect]) -> usize {
        effects.iter().filter(|e| matches!(e, crate::step::Effect::WriteSecret { .. })).count()
    }

    #[test]
    fn unchanged_accessory_keys_are_not_written_again() {
        use crate::step::Input;
        let mut daemon = Daemon::new("xps");
        let now = clock();
        ready_link(&mut daemon, now);
        let generation = daemon.connection().generation();
        let first =
            daemon.step(Input::Accessory { generation, packet: key_packet(0x11, 0x22) }, now);
        assert_eq!(secret_writes(&first.effects), 1, "one key-map item holds every accessory");
        // Every connection reports the keys; the keyring is not asked to replace the same items.
        let again =
            daemon.step(Input::Accessory { generation, packet: key_packet(0x11, 0x22) }, now);
        assert_eq!(secret_writes(&again.effects), 0);
        let changed =
            daemon.step(Input::Accessory { generation, packet: key_packet(0x33, 0x22) }, now);
        assert_eq!(secret_writes(&changed.effects), 1);
    }

    #[test]
    fn a_keyring_unlocked_after_a_locked_start_restores_the_fleet() {
        use crate::fleet::members::Fleet;
        use crate::step::Input;
        let mut daemon = Daemon::new("xps");
        let now = clock();
        daemon.set_local_address(Some(addr(1)));
        daemon.set_keyring_locked(true);
        daemon.mark_secrets_unavailable();
        let fleet = Fleet {
            id: FleetId::new([3; 16]),
            key: GroupKey::new([0x11; 32]),
            members: vec![
                Member { address: addr(1), label: "xps".into() },
                Member { address: addr(2), label: "gram".into() },
            ],
        };
        daemon.restore_fleet(fleet.clone());
        assert!(daemon.saved_fleet().is_none(), "a locked keyring holds the fleet back");
        // Keys learned from the pods while locked could not be saved then.
        ready_link(&mut daemon, now);
        let generation = daemon.connection().generation();
        daemon.step(Input::Accessory { generation, packet: key_packet(0x11, 0x22) }, now);

        let opened =
            daemon.step(Input::KeyringUnlocked { fleet: Some(fleet.clone()), keys: None }, now);
        assert!(!daemon.keyring_locked());
        assert_eq!(daemon.saved_fleet(), Some(fleet.clone()));
        assert_eq!(secret_writes(&opened.effects), 1, "keys learned while locked are saved");
        // A late duplicate does nothing.
        let late = daemon.step(Input::KeyringUnlocked { fleet: Some(fleet), keys: None }, now);
        assert_eq!(secret_writes(&late.effects), 0);
    }

    #[test]
    fn sleep_drops_the_seat_and_wake_waits_before_reconnecting() {
        use crate::step::{Effect, Input};

        let mut daemon = Daemon::new("gram");
        daemon.set_local_address(Some(addr(1)));
        let now = clock();
        ready_link(&mut daemon, now);
        daemon.note_ownership(true, now.mono);
        daemon.note_source(true, 2, now.mono);
        daemon.output.claim();
        assert!(matches!(daemon.start_route(addr(2)), crate::fleet::route::Start::Sent { .. }));
        assert_eq!(daemon.status_value(now)["reported_ownership"], 1);

        let slept = daemon.step(Input::Suspend, now);
        assert_eq!(daemon.connection().stage(), Stage::Interrupted);
        assert_eq!(daemon.connection().reason(), "suspend");
        assert_eq!(daemon.status_value(now)["reported_ownership"], -1);
        assert_eq!(daemon.status_value(now)["audio_source_local"], -1);
        assert_eq!(daemon.routing().phase(), crate::fleet::route::Phase::Failed);
        assert_eq!(daemon.routing().error(), "interrupted");
        assert!(daemon.output.wants_output());
        assert!(slept.effects.iter().any(|effect| matches!(effect, Effect::StopListen)));
        assert!(slept.effects.iter().any(|effect| matches!(effect, Effect::ReleaseAir)));
        assert!(slept.effects.iter().all(|effect| !matches!(effect, Effect::OpenLink { .. })));

        let held = daemon.step(Input::Tick, now);
        assert!(held.effects.iter().all(|effect| !matches!(effect, Effect::OpenLink { .. })));

        let resumed = daemon.step(Input::Resume, now);
        assert!(resumed.effects.iter().all(|effect| !matches!(effect, Effect::FollowLink)));
        daemon.step(Input::BluezLost, now);
        assert_eq!(daemon.connection().reason(), "bluez");
        assert!(daemon.output.wants_output());
        let early = daemon.step(Input::Tick, Clock::new(2_999, now.unix.0 + 1_999));
        assert!(early.effects.iter().all(|effect| !matches!(effect, Effect::OpenLink { .. })));
        let woken = daemon.step(Input::Tick, Clock::new(3_000, now.unix.0 + 2_000));
        assert!(woken.effects.iter().any(|effect| matches!(effect, Effect::OpenLink { .. })));
        assert!(daemon.connection().stage().is_opening());
    }

    #[test]
    fn a_user_disconnect_stays_down_across_wake_and_an_idle_sleep_does_not_connect() {
        use crate::step::{Effect, Input};

        let mut daemon = Daemon::new("gram");
        let now = clock();
        ready_link(&mut daemon, now);
        daemon.step(Input::Command(Command::Disconnect), now);
        assert_eq!(daemon.connection().reason(), "disconnect");
        daemon.step(Input::Resume, now);
        let later = daemon.step(Input::Tick, Clock::new(20_000, now.unix.0 + 20_000));
        assert!(later.effects.iter().all(|effect| !matches!(effect, Effect::OpenLink { .. })));
        assert_eq!(daemon.connection().stage(), Stage::Interrupted);

        let mut idle = Daemon::new("gram");
        idle.set_local_address(Some(addr(1)));
        idle.step(Input::Suspend, now);
        assert_eq!(idle.connection().stage(), Stage::Idle);
        let back = idle.step(Input::Resume, now);
        assert!(back.effects.iter().any(|effect| matches!(effect, Effect::FollowLink)));
        let quiet = idle.step(Input::Tick, Clock::new(20_000, now.unix.0 + 20_000));
        assert!(quiet.effects.iter().all(|effect| !matches!(effect, Effect::OpenLink { .. })));
    }

    #[test]
    fn an_idle_seated_laptop_restores_control_when_bluetooth_rejoins_after_wake() {
        use crate::step::{Effect, Input};
        let mut daemon = Daemon::new("gram");
        daemon.set_local_address(Some(addr(1)));
        let now = clock();
        ready_link(&mut daemon, now);
        daemon.note_ownership(true, now.mono);
        daemon.note_source(true, 2, now.mono);
        assert!(!daemon.output.wants_output());
        let old = daemon.connection.generation();
        daemon.step(Input::Suspend, now);
        // The adapter's local close carries the old generation. It must not
        // replace suspend with link_lost or yield the laptop's playback state.
        let closed = daemon.step(Input::ControlSocket { up: false, generation: old }, now);
        assert_eq!(daemon.connection.reason(), "suspend");
        assert!(!daemon.audio.yielded());
        assert!(closed.effects.iter().all(|e| !matches!(e, Effect::PausePlayers(_))));
        daemon.step(Input::Bluez(false), now);
        daemon.step(Input::Resume, now);
        daemon.step(Input::Bluez(true), now);
        let early = daemon.step(Input::Tick, Clock::new(2_999, now.unix.0 + 1_999));
        assert!(early.effects.iter().all(|e| !matches!(e, Effect::OpenLink { .. })));
        let restored = daemon.step(Input::Tick, Clock::new(3_000, now.unix.0 + 2_000));
        assert!(restored.effects.iter().any(|e| matches!(e, Effect::OpenLink { .. })));
        assert_eq!(daemon.connection.stage(), Stage::Connecting);
        assert!(!daemon.output.wants_output(), "restoring control is not a playback claim");
        assert!(restored.effects.iter().all(
            |e| !matches!(e, Effect::WriteControl { bytes, .. } if bytes == &crate::pods::setup::ownership(true))
        ));
    }

    #[test]
    fn bluetoothd_leaving_clears_the_seat_and_its_return_waits() {
        use crate::step::{Effect, Input};

        let mut daemon = Daemon::new("gram");
        let now = clock();
        ready_link(&mut daemon, now);
        daemon.note_ownership(true, now.mono);
        daemon.step(Input::BluezLost, now);
        assert_eq!(daemon.connection().stage(), Stage::Interrupted);
        assert_eq!(daemon.connection().reason(), "bluez");
        assert_eq!(daemon.status_value(now)["reported_ownership"], -1);
        daemon.step(Input::BluezBack, now);
        let early = daemon.step(Input::Tick, Clock::new(2_999, now.unix.0));
        assert!(early.effects.iter().all(|effect| !matches!(effect, Effect::OpenLink { .. })));
        let back = daemon.step(Input::Tick, Clock::new(3_000, now.unix.0));
        assert!(back.effects.iter().any(|effect| matches!(effect, Effect::OpenLink { .. })));
    }

    #[test]
    fn a_dropped_background_seat_waits_for_new_activity_or_play() {
        use crate::step::{Effect, Input};
        for play in [false, true] {
            let mut daemon = Daemon::new("xps");
            let now = clock();
            daemon.set_local_address(Some(addr(1)));
            ready_link(&mut daemon, now);
            daemon.note_pod_peers(vec![addr(1), addr(4)], now.unix);
            daemon.note_source(false, 2, now.mono);
            daemon.note_remote(addr(4), now.mono);
            let generation = daemon.connection.generation();
            daemon.step(Input::ControlSocket { up: false, generation }, now);
            assert_eq!(daemon.connection.reason(), "link_lost");
            for elapsed in [1_000, 5_000, 30_000, 120_000] {
                let at = Clock::new(now.mono.0 + elapsed, now.unix.0 + elapsed);
                for input in [
                    Input::Bluez(true),
                    Input::BluezBack,
                    Input::AttachAvailable { generation: daemon.connection.generation() },
                    Input::Tick,
                ] {
                    let result = daemon.step(input, at);
                    assert!(
                        !result.effects.iter().any(|e| matches!(
                            e,
                            Effect::OpenLink { .. }
                                | Effect::FollowLink
                                | Effect::ConnectTransport { .. }
                        )),
                        "a lost seat cannot reuse old activity or a BlueZ update"
                    );
                }
            }
            let later = Clock::new(now.mono.0 + 121_000, now.unix.0 + 121_000);
            daemon.note_pod(
                false,
                Pod {
                    battery: Battery::new(70, false).unwrap(),
                    in_case: Some(false),
                    in_ear: Some(true),
                },
                later.mono,
            );
            let input = if play {
                daemon.audio.startup_players(&[]);
                Input::Player {
                    service: "org.mpris.MediaPlayer2.brave".into(),
                    playing: Some(true),
                }
            } else {
                Input::Command(Command::Activity)
            };
            let requested = daemon.step(input, later);
            assert!(
                requested.effects.iter().any(|e| matches!(e, Effect::OpenLink { .. })),
                "fresh activity or Play must open a new connection"
            );
        }
    }

    #[test]
    fn a_socket_drop_clears_the_report_and_keeps_the_claim_and_the_route() {
        let mut daemon = Daemon::new("gram");
        let now = clock();
        ready_link(&mut daemon, now);
        let generation = daemon.connection().generation();
        daemon.note_ownership(true, now.mono);
        daemon.output.claim();
        assert!(matches!(daemon.start_route(addr(2)), crate::fleet::route::Start::Sent { .. }));
        daemon.note_socket(false, generation, now.mono);
        assert_eq!(daemon.connection().stage(), Stage::Interrupted);
        assert_eq!(daemon.connection().reason(), "link_lost");
        assert_eq!(daemon.status_value(now)["reported_ownership"], -1);
        assert!(daemon.output.wants_output());
        assert_eq!(daemon.routing().phase(), crate::fleet::route::Phase::WaitingAdmission);
    }

    #[test]
    fn the_microphone_node_is_named_after_the_accessory() {
        use crate::step::{Effect, Input};

        let mut daemon = Daemon::new("gram");
        daemon.set_identity("Dan\u{2019}s AirPods Pro #3", "A3063");
        let now = clock();
        ready_link(&mut daemon, now);
        assert_eq!(
            daemon.mic_source.as_ref().map(|(_, _, name)| name.as_str()),
            Some("Dan\u{2019}s AirPods Pro #3")
        );
        let same = daemon.step(Input::Tick, now);
        assert!(same.effects.iter().all(|effect| !matches!(effect, Effect::ShowMicrophone { .. })));
        daemon.set_identity("Other Buds", "A3063");
        let renamed = daemon.step(Input::Tick, now);
        assert!(renamed.effects.iter().any(|effect| matches!(
            effect,
            Effect::ShowMicrophone { description, .. } if description == "Other Buds"
        )));
        daemon.set_identity("  ", "A3063");
        let blank = daemon.step(Input::Tick, now);
        assert!(blank.effects.iter().any(|effect| matches!(
            effect,
            Effect::ShowMicrophone { description, .. } if description == "AirPods"
        )));
    }

    #[test]
    fn capture_starts_the_microphone_counts_frames_and_stops_after_two_restarts() {
        use crate::step::{Effect, Input};

        for sku in ["A3064", "A2698", "A3454"] {
            let mut daemon = Daemon::new("gram");
            daemon.set_identity("Pods", sku);
            daemon.set_local_address(Some(addr(1)));
            let now = clock();
            ready_link(&mut daemon, now);
            assert_eq!(
                daemon.mic_source.as_ref().map(|(_, rate, name)| (*rate, name.as_str())),
                Some((64_000, "Pods"))
            );
            daemon.accessory.note_conversation(true, now.mono);
            let started = daemon.step(Input::Microphone { streaming: true, muted: false }, now);
            let start = crate::audio::mic::start_command();
            let off = crate::pods::setup::conversation(false);
            assert!(started.effects.iter().any(|effect| matches!(
                effect,
                Effect::WriteControl { bytes, .. } if bytes == &off
            )));
            assert!(started.effects.iter().any(|effect| matches!(
                effect,
                Effect::WriteControl { bytes, .. } if bytes == &start
            )));
            assert!(started.effects.iter().all(|effect| !matches!(effect, Effect::StopListen)));
            let generation = daemon.connection().generation();
            let mut audio = vec![0x04, 0x00, 0x04, 0x00, 0x58, 0x00, 0x01, 0x00];
            audio.extend(std::iter::repeat_n(0, 14));
            audio.extend_from_slice(&[1, 0, 0, 0, 1, 0x11]);
            daemon.step(Input::Accessory { generation, packet: audio.clone() }, now);
            daemon.step(
                Input::Accessory { generation, packet: audio },
                Clock::new(1_400, now.unix.0 + 400),
            );
            let status = daemon.status_value(Clock::new(1_400, now.unix.0 + 400));
            assert_eq!(status["microphone"]["phase"], "capturing");
            assert_eq!(status["microphone"]["frames"], 2);
            assert_eq!(status["microphone"]["max_gap_ms"], 400);
            daemon.step(
                Input::Accessory {
                    generation,
                    packet: vec![0x04, 0x00, 0x04, 0x00, 0x58, 0x00, 0x01, 0x00],
                },
                now,
            );
            assert_eq!(daemon.microphone().malformed(), 1);

            let again = daemon.step(Input::Tick, Clock::new(3_400, now.unix.0 + 2_400));
            let stop = crate::audio::mic::stop_command();
            assert!(again.effects.iter().any(|effect| matches!(
                effect,
                Effect::WriteControl { bytes, .. } if bytes == &stop
            )));
            assert!(again.effects.iter().any(|effect| matches!(
                effect,
                Effect::WriteControl { bytes, .. } if bytes == &start
            )));
            daemon.step(Input::Tick, Clock::new(5_400, now.unix.0 + 4_400));
            let stopped = daemon.step(Input::Tick, Clock::new(7_400, now.unix.0 + 6_400));
            let on = crate::pods::setup::conversation(true);
            assert!(stopped.effects.iter().any(|effect| matches!(
                effect,
                Effect::WriteControl { bytes, .. } if bytes == &stop
            )));
            assert!(stopped.effects.iter().all(|effect| !matches!(
                effect,
                Effect::WriteControl { bytes, .. } if bytes == &start
            )));
            assert!(stopped.effects.iter().any(|effect| matches!(
                effect,
                Effect::WriteControl { bytes, .. } if bytes == &on
            )));
            assert_eq!(daemon.status_value(now)["microphone"]["phase"], "needs_reopen");
            let held = daemon.step(Input::Microphone { streaming: true, muted: false }, now);
            assert!(held.effects.iter().all(|effect| !matches!(
                effect,
                Effect::WriteControl { bytes, .. } if bytes == &start
            )));
            assert_eq!(daemon.status_value(now)["microphone"]["phase"], "needs_reopen");
            daemon.step(Input::Microphone { streaming: false, muted: false }, now);
            assert_eq!(daemon.status_value(now)["microphone"]["phase"], "idle");
            let reopened = daemon.step(Input::Microphone { streaming: true, muted: false }, now);
            assert!(reopened.effects.iter().any(|effect| matches!(
                effect,
                Effect::WriteControl { bytes, .. } if bytes == &start
            )));
            assert_eq!(daemon.status_value(now)["microphone"]["phase"], "capturing");
        }
    }

    #[test]
    fn a_link_drop_stops_the_microphone_and_a_down_source_does_not_play() {
        use std::sync::atomic::Ordering;

        use crate::step::Input;

        for sku in ["A3064", "A2698", "A3454"] {
            let mut daemon = Daemon::new("gram");
            daemon.set_identity("Pods", sku);
            let now = clock();
            ready_link(&mut daemon, now);
            daemon.accessory.note_conversation(true, now.mono);
            daemon.step(Input::Microphone { streaming: true, muted: false }, now);
            assert!(daemon.mic_path.delivering.load(Ordering::Relaxed));
            let generation = daemon.connection().generation();
            daemon.step(Input::ControlSocket { up: false, generation }, now);
            assert!(!daemon.mic_path.delivering.load(Ordering::Relaxed));
            assert!(!daemon.mic_started);
            assert_eq!(daemon.status_value(now)["microphone"]["phase"], "down");
            let stop = crate::audio::mic::stop_command();
            let on = crate::pods::setup::conversation(true);
            assert!(daemon.staged_controls.iter().any(|bytes| bytes == &stop));
            assert!(daemon.staged_controls.iter().any(|bytes| bytes == &on));
            let ignored = daemon.step(Input::Microphone { streaming: true, muted: false }, now);
            assert!(ignored.effects.iter().all(|effect| !matches!(
            effect,
            crate::step::Effect::WriteControl { bytes, .. } if bytes == &crate::audio::mic::start_command()
        )));
            assert_eq!(daemon.status_value(now)["microphone"]["phase"], "down");
        }
    }

    #[test]
    fn bluetooth_forget_stops_retries_and_restores_setup_with_saved_keys() {
        use crate::step::{Effect, Input};
        for established in [false, true] {
            let mut daemon = Daemon::new("xps");
            let now = Clock::new(1000, 1000);
            let pods = addr(3);
            let keys = AccessoryKeys { irk: [0x11; 16], encryption: [0x22; 16] };
            daemon.set_accessory_keys(pods, keys.clone());
            if established {
                ready_link(&mut daemon, now);
            } else {
                daemon.select_accessory(Some(pods));
                daemon.step(Input::Command(Command::Connect), now);
                daemon.step(
                    Input::LinkFailed { generation: daemon.connection.generation(), busy: false },
                    now,
                );
                assert_eq!(daemon.connection.stage(), Stage::Retrying);
            }
            let old = daemon.connection.generation();
            daemon.step(Input::Unpaired { address: addr(8) }, now);
            assert_eq!(daemon.config.accessory, Some(pods));
            let forgotten = daemon.step(Input::Unpaired { address: pods }, now);
            assert_eq!(daemon.config.accessory, None);
            assert_eq!(daemon.membership.accessory_keys(pods), Some(&keys));
            assert!(!forgotten.effects.iter().any(|effect| matches!(
                effect,
                Effect::OpenLink { .. } | Effect::RemoveDevice { .. } | Effect::WriteSecret { .. }
            )));
            let status = daemon.status_value(now);
            assert_eq!(status["bluetooth_paired"], false);
            assert_eq!(status["pairing_keys_ready"], true);
            assert_eq!(status["connected"], false);
            assert_eq!(status["control_connection"]["stage"], "waiting");
            daemon.step(Input::ControlSocket { up: true, generation: old }, now);
            let later = Clock::new(10_000, 10_000);
            for input in
                [Input::Tick, Input::Command(Command::Activity), Input::Command(Command::Connect)]
            {
                let held = daemon.step(input, later);
                assert!(!held.effects.iter().any(|e| matches!(e, Effect::OpenLink { .. })));
            }
            daemon.step(Input::Proximity { address: pods, payload: captured_case(0x31) }, later);
            let mut pair = captured_case(0x31);
            pair[2] = 7;
            daemon.step(Input::Proximity { address: pods, payload: pair }, later);
            assert_eq!(daemon.status_value(later)["pairing_mode"], true);
            assert_eq!(daemon.status_value(later)["model_int"], 11);
            daemon.step(Input::Paired { address: pods, name: "My AirPods".into() }, later);
            assert_eq!(daemon.status_value(later)["bluetooth_paired"], true);
            assert!(!daemon.pairing_elsewhere(later.mono));
            let reconnected = daemon.step(Input::Command(Command::Connect), later);
            assert!(reconnected.effects.iter().any(|e| matches!(e, Effect::OpenLink { .. })));
        }
    }

    #[test]
    fn noise_cycle_skips_a_mode_the_model_rejects_and_forget_removes_the_accessory() {
        use crate::step::{Effect, Input};

        let mut daemon = Daemon::new("gram");
        daemon.set_identity("Pods", "A3064");
        daemon.note_noise_report(NoiseMode::Anc, MonoMs(1_000));
        daemon.handle(Command::NoiseCycle, clock());
        assert_eq!(daemon.config.noise, Some(NoiseMode::Transparency));
        daemon.note_noise_report(NoiseMode::Adaptive, MonoMs(1_000));
        daemon.handle(Command::NoiseCycle, clock());
        assert_eq!(daemon.config.noise, Some(NoiseMode::Anc));

        assert_eq!(
            daemon.handle(Command::Forget, clock()).body.as_deref(),
            Some("error: no accessory\n")
        );
        daemon.select_accessory(Some(addr(3)));
        daemon.output.claim();
        let forgotten = daemon.step(Input::Command(Command::Forget), clock());
        assert_eq!(forgotten.reply.as_deref(), Some("ok\n"));
        assert!(forgotten.effects.iter().any(|effect| matches!(
            effect,
            Effect::RemoveDevice { accessory } if *accessory == addr(3)
        )));
        assert!(daemon.config.accessory.is_none());
        assert!(!daemon.output.wants_output());
        assert_eq!(daemon.connection().stage(), Stage::Waiting);
        assert_eq!(daemon.status_value(clock())["bluetooth_paired"], false);
    }

    #[test]
    fn solo_and_incoming_priority_wait_for_one_acknowledgement() {
        use crate::step::{Effect, Input};

        let mut daemon = Daemon::new("gram");
        let now = clock();
        let local = Address::parse("02:00:00:00:00:01").unwrap();
        let peer = Address::parse("02:00:00:00:00:02").unwrap();
        daemon.set_local_address(Some(local));
        assert_eq!(
            daemon.handle(Command::Solo, now).body.as_deref(),
            Some("control_unavailable\n")
        );
        ready_link(&mut daemon, now);
        daemon.set_local_address(Some(local));
        let solo = daemon.step(Input::Command(Command::Solo), now);
        assert_eq!(solo.reply.as_deref(), Some("written_waiting_for_ack\n"));
        let packet = crate::pods::setup::priority(local, None).unwrap();
        assert!(solo.effects.iter().any(|effect| matches!(
            effect,
            Effect::WriteControl { bytes, .. } if bytes == &packet
        )));
        assert_eq!(daemon.handle(Command::Solo, now).body.as_deref(), Some("request_pending\n"));
        let generation = daemon.connection().generation();
        daemon.step(
            Input::Accessory {
                generation,
                packet: vec![
                    0x04, 0x00, 0x04, 0x00, 0x40, 0x00, 0x14, 0x00, 0x00, 0x00, 0x00, 0x00,
                ],
            },
            now,
        );
        let ordered = daemon.step(Input::Command(Command::Priority(peer.display())), now);
        let both = crate::pods::setup::priority(peer, Some(local)).unwrap();
        assert_eq!(ordered.reply.as_deref(), Some("written_waiting_for_ack\n"));
        assert!(ordered.effects.iter().any(|effect| matches!(
            effect,
            Effect::WriteControl { bytes, .. } if bytes == &both
        )));
        daemon.step(Input::Tick, Clock::new(now.mono.0 + 5_000, now.unix.0 + 5_000));
        assert!(matches!(daemon.start_route(addr(9)), crate::fleet::route::Start::Sent { .. }));
        assert_eq!(daemon.handle(Command::Solo, now).body.as_deref(), Some("fleet_routing_busy\n"));

        let mut waking = Daemon::new("gram");
        waking.set_local_address(Some(local));
        ready_link(&mut waking, now);
        waking.set_local_address(Some(local));
        waking.audio.startup_players(&[crate::audio::play::PlayerSeen {
            service: "org.mpris.MediaPlayer2.spotify".into(),
            playing: true,
        }]);
        let blocked = serde_json::json!({"peer": peer.display(), "yield": true});
        let yielded = waking.step(Input::Command(Command::Incoming(Ok(blocked))), now);
        let yielded_body: serde_json::Value =
            serde_json::from_str(yielded.reply.unwrap().trim()).unwrap();
        assert_eq!(yielded_body["ok"], true);
        assert_eq!(yielded_body["reason"], "written_waiting_for_ack");
        let peer_first = crate::pods::setup::priority(peer, Some(local)).unwrap();
        assert!(yielded.effects.iter().any(|effect| matches!(
            effect,
            Effect::WriteControl { bytes, .. } if bytes == &peer_first
        )));
        assert!(yielded.effects.iter().any(|effect| matches!(
            effect,
            Effect::PausePlayers(names) if names == &["org.mpris.MediaPlayer2.spotify".to_string()]
        )));
        assert!(!waking.output.wants_output());
        assert_eq!(waking.status_value(now)["native_handoff"]["yielded"], true);
        let blocked_claim = waking.handle(Command::HandoffClaim, now);
        assert_eq!(blocked_claim.body.as_deref(), Some("request_pending\n"));
        assert!(!waking.output.wants_output());
        assert!(waking.audio.yielded());
        let generation = waking.connection().generation();
        waking.step(
            Input::Accessory {
                generation,
                packet: vec![
                    0x04, 0x00, 0x04, 0x00, 0x40, 0x00, 0x14, 0x00, 0x00, 0x00, 0x00, 0x00,
                ],
            },
            now,
        );
        let call = serde_json::json!({"peer": peer.display(), "yield": false});
        let admitted = waking.step(Input::Command(Command::Incoming(Ok(call))), now);
        let body: serde_json::Value = serde_json::from_str(admitted.reply.unwrap().trim()).unwrap();
        assert_eq!(body["ok"], true);
        assert_eq!(body["reason"], "written_waiting_for_ack");
        let first = crate::pods::setup::priority(local, Some(peer)).unwrap();
        assert!(admitted.effects.iter().any(|effect| matches!(
            effect,
            Effect::WriteControl { bytes, .. } if bytes == &first
        )));
        assert_eq!(
            waking.handle(Command::Incoming(Err(())), now).body.unwrap().trim(),
            r#"{"ok":false,"reason":"invalid_json","request_id":0}"#
        );
    }

    #[test]
    fn releasing_or_reconnecting_mutes_playing_streams_before_pausing() {
        use crate::audio::play::PlayerSeen;
        use crate::audio::route::{Graph, LiveStream, Sink};
        use crate::step::{Effect, Input};
        // XPS, 2026-10-19 16:01:39: Reconnect paused Brave and its stream
        // reached the speakers 10 ms later, still playing.
        for command in [Command::ReconnectPeer, Command::HandoffRelease] {
            let mut daemon = Daemon::new("xps");
            let now = clock();
            ready_link(&mut daemon, now);
            daemon.note_bluez(true);
            daemon.audio.startup_players(&[PlayerSeen {
                service: "org.mpris.MediaPlayer2.brave".into(),
                playing: true,
            }]);
            daemon.output.claim();
            daemon.step(
                Input::Audio(Graph {
                    default_sink: "pods".into(),
                    sinks: vec![
                        Sink {
                            name: "pods".into(),
                            address: Some(addr(3)),
                            profile: Some("a2dp-sink".into()),
                        },
                        Sink { name: "speakers".into(), address: None, profile: None },
                    ],
                    cards: Vec::new(),
                    streams: vec![LiveStream {
                        id: 1,
                        generation: 10,
                        sink: "pods".into(),
                        role: String::new(),
                        node: "Brave".into(),
                        mute: false,
                        temporarily_muted: false,
                        volume: 1.0,
                        corked: false,
                    }],
                }),
                now,
            );
            let outcome = daemon.step(Input::Command(command.clone()), now);
            let mute = outcome.effects.iter().position(|e| matches!(e, Effect::MuteStreams(_)));
            let pause = outcome.effects.iter().position(|e| matches!(e, Effect::PausePlayers(_)));
            assert!(
                matches!((mute, pause), (Some(m), Some(p)) if m < p),
                "{command:?}: mute {mute:?} must come before pause {pause:?}"
            );
        }
    }

    #[test]
    fn reconnect_refuses_a_temporary_pairing_without_releasing_either_laptop() {
        use crate::step::{Effect, Input};
        let mut daemon = Daemon::new("xps");
        let now = clock();
        ready_link(&mut daemon, now);
        daemon.note_bluez(true);
        daemon.step(Input::Bonded { address: addr(8), saved: false }, now);
        assert_eq!(daemon.status_value(now)["bluetooth_bonded"], serde_json::Value::Null);
        daemon.step(Input::Bonded { address: addr(3), saved: false }, now);
        assert_eq!(daemon.status_value(now)["bluetooth_bonded"], false);
        let rejected = daemon.step(Input::Command(Command::ReconnectPeer), now);
        assert!(rejected.reply.unwrap().contains("Bluetooth pairing was not saved"));
        assert!(!daemon.connection.apple_reconnect());
        assert_eq!(daemon.connection.stage(), Stage::Ready);
        assert!(!rejected.effects.iter().any(|e| matches!(
            e,
            Effect::Disconnect { .. }
                | Effect::RemoveDevice { .. }
                | Effect::PausePlayers(_)
                | Effect::WriteControl { .. }
        )));
        daemon.step(Input::Bonded { address: addr(3), saved: true }, now);
        assert_eq!(daemon.status_value(now)["bluetooth_bonded"], true);
        let accepted = daemon.step(Input::Command(Command::ReconnectPeer), now);
        assert_eq!(accepted.reply.as_deref(), Some("accepted\n"));
        assert!(daemon.connection.apple_reconnect());
        daemon.step(Input::Unpaired { address: addr(3) }, now);
        assert_eq!(daemon.status_value(now)["bluetooth_bonded"], serde_json::Value::Null);
    }

    #[test]
    fn apple_reconnect_keeps_local_link_when_a_rostered_peer_does_not_leave() {
        use crate::fleet::members::{Fleet, Member};
        use crate::step::{Effect, Input};
        let mut daemon = Daemon::new("xps");
        let now = clock();
        let local = addr(1);
        let peer = addr(2);
        ready_link(&mut daemon, now);
        daemon.set_local_address(Some(local));
        daemon.note_bluez(true);
        daemon.restore_fleet(Fleet {
            id: FleetId::new([7; 16]),
            key: GroupKey::new([9; 32]),
            members: vec![
                Member { address: local, label: "xps".into() },
                Member { address: peer, label: "gram".into() },
            ],
        });
        use crate::pods::reconnect::{PEER_LEAVE_MS, PEER_STAYED};
        daemon.note_pod_peers(vec![local, peer], now.unix);
        daemon.routing.claim_audio();
        daemon.step(Input::Command(Command::ReconnectPeer), now);
        assert_eq!(daemon.routing.intent(), crate::fleet::route::Intent::Release);
        for elapsed in [1, 1000, PEER_LEAVE_MS + 1] {
            let step =
                daemon.step(Input::Tick, Clock::new(now.mono.0 + elapsed, now.unix.0 + elapsed));
            assert!(!step.effects.iter().any(|effect| matches!(effect, Effect::Disconnect { .. })));
        }
        assert_eq!(daemon.connection.stage(), Stage::Ready);
        assert_eq!(
            daemon.status_value(now)["control_connection"]["peer_reconnect_error"],
            PEER_STAYED
        );
    }

    #[test]
    fn a_relayed_reconnect_drops_this_link() {
        use crate::step::{Effect, Input};

        let mut daemon = Daemon::new("xps");
        let now = clock();
        let local = Address::parse("02:00:00:00:00:02").unwrap();
        let peer = Address::parse("02:00:00:00:00:01").unwrap();
        ready_link(&mut daemon, now);
        daemon.set_local_address(Some(local));
        daemon.peers_seated.push(peer);
        let mut packet = crate::pods::setup::reconnect_request(local, peer).unwrap();
        packet[4] = 0x11;
        packet[6..12].copy_from_slice(&peer.bytes().into_iter().rev().collect::<Vec<_>>());
        let generation = daemon.connection().generation();
        let dropped = daemon.step(Input::Accessory { generation, packet }, now);
        assert!(dropped.effects.iter().any(|effect| matches!(effect, Effect::Disconnect { .. })));
        assert!(!daemon.routing().want_seat());
    }

    #[test]
    fn a_full_seat_with_an_idle_partner_admits_a_fleet_laptop() {
        let local = addr(1);
        let phone = addr(4);
        let xps = addr(2);
        let seated = vec![local, phone];
        // The idle phone gives up its slot; this laptop is retained.
        assert_eq!(
            Daemon::full_seat_retain(
                &seated,
                Some(local),
                xps,
                &Holder::Remote { address: Some(phone), playing: false },
            ),
            Some(local)
        );
        // A playing phone is never the device displaced.
        assert_eq!(
            Daemon::full_seat_retain(
                &seated,
                Some(local),
                xps,
                &Holder::Remote { address: Some(phone), playing: true },
            ),
            None
        );
        // An unnamed playing source is protected too.
        assert_eq!(
            Daemon::full_seat_retain(
                &seated,
                Some(local),
                xps,
                &Holder::Remote { address: None, playing: true },
            ),
            None
        );
        // A requester already in the seat needs no admission.
        assert_eq!(Daemon::full_seat_retain(&seated, Some(local), phone, &Holder::Unknown), None);
        // Without this laptop in the seat there is no pair to reserve.
        assert_eq!(Daemon::full_seat_retain(&seated, Some(xps), addr(5), &Holder::Unknown), None);
        // A single occupant is admitted by the existing branch, not this one.
        assert_eq!(
            Daemon::full_seat_retain(&seated[..1], Some(local), xps, &Holder::Unknown),
            None
        );
    }

    #[test]
    fn a_lost_slot_holds_activity_and_acl_attach_without_extending_the_grace() {
        use crate::step::{Effect, Input};
        let mut daemon = Daemon::new("xps");
        let now = clock();
        ready_link(&mut daemon, now);
        let generation = daemon.connection.generation();
        daemon.step(Input::ControlSocket { up: false, generation }, now);
        let generation = daemon.connection.generation();
        let until = now.mono.saturating_add(DISPLACEMENT_HOLD_MS);
        assert_eq!(daemon.slot_hold_until, Some(until));
        for delta in [0, 1_000, DISPLACEMENT_HOLD_MS - 1] {
            let at = Clock::new(now.mono.0 + delta, now.unix.0 + delta);
            // Duplicate close notifications must not keep pushing the hold out.
            daemon.step(Input::ControlSocket { up: false, generation }, at);
            daemon.step(Input::Bluez(true), at);
            for input in [
                Input::Command(Command::Activity),
                Input::AttachAvailable { generation },
                Input::Tick,
            ] {
                let outcome = daemon.step(input, at);
                assert!(!outcome.effects.iter().any(|e| matches!(e, Effect::OpenLink { .. })));
            }
            assert_eq!(daemon.slot_hold_until, Some(until));
            assert_eq!(daemon.connection.generation(), generation);
            // (The rebuild-on-silence count is asserted inside the grant arm above.)
        }
        let settled = Clock::new(until.0, now.unix.0 + DISPLACEMENT_HOLD_MS);
        let outcome = daemon.step(Input::Command(Command::Activity), settled);
        assert!(outcome.effects.iter().any(|e| matches!(e, Effect::OpenLink { .. })));
    }

    #[test]
    fn an_old_socket_close_cannot_hold_or_drop_a_new_ready_link() {
        use crate::step::Input;
        let mut daemon = Daemon::new("xps");
        let now = clock();
        ready_link(&mut daemon, now);
        let generation = daemon.connection.generation();
        daemon.step(
            Input::ControlSocket { up: false, generation: Generation(generation.0 - 1) },
            now,
        );
        assert_eq!(daemon.connection.stage(), Stage::Ready);
        assert_eq!(daemon.slot_hold_until, None);
    }

    #[test]
    fn playback_keeps_a_seated_laptop_beside_this_one() {
        use crate::fleet::members::{Fleet, Member};
        use crate::types::FleetId;

        let mut daemon = Daemon::new("omarchy-xps");
        let now = clock();
        let local = addr(1);
        let gram = addr(2);
        ready_link(&mut daemon, now);
        daemon.set_local_address(Some(local));
        daemon.restore_fleet(Fleet {
            id: FleetId::new([3; 16]),
            key: GroupKey::new([0x11; 32]),
            members: vec![
                Member { address: local, label: "omarchy-xps".into() },
                Member { address: gram, label: "lg-gram".into() },
            ],
        });
        daemon.peers_seated.push(gram);
        daemon.note_source(true, 2, now.mono);
        let aired = daemon.audio_roster(now).expect("roster");
        assert!(aired.peers.contains(&local));
        assert!(aired.peers.contains(&gram));
        let listed = daemon.status_value(now)["connected_peers"].as_array().unwrap().clone();
        assert!(listed.iter().any(|value| value.as_str() == Some(gram.display().as_str())));
    }

    #[test]
    fn an_empty_roster_from_the_old_peer_does_not_clear_the_seat() {
        use crate::fleet::members::{Fleet, Member};
        use crate::types::FleetId;

        let mut daemon = Daemon::new("omarchy-xps");
        let now = clock();
        let local = addr(1);
        let gram = addr(2);
        ready_link(&mut daemon, now);
        daemon.set_local_address(Some(local));
        daemon.restore_fleet(Fleet {
            id: FleetId::new([3; 16]),
            key: GroupKey::new([0x11; 32]),
            members: vec![
                Member { address: local, label: "omarchy-xps".into() },
                Member { address: gram, label: "lg-gram".into() },
            ],
        });
        daemon.held_peers = vec![gram, local];
        daemon.held_reporter = Some(gram);
        daemon.observe_nearby(host_id(Some(gram)), gram, "lg-gram", now.unix, true, Some([1; 16]));
        daemon.note_source(true, 2, now.mono);
        let aired = daemon.audio_roster(now).expect("roster");
        assert!(aired.peers.contains(&local) && aired.peers.contains(&gram));
        let listed = daemon.status_value(now)["connected_peers"].as_array().unwrap().clone();
        assert!(listed.iter().any(|value| value.as_str() == Some(gram.display().as_str())));
    }

    #[test]
    fn an_admitted_join_does_not_reseat_its_released_helper_from_cached_rosters() {
        use crate::fleet::members::Fleet;
        use crate::fleet::roster_air::Snapshot;
        use crate::fleet::route::{Admit, Start};
        use crate::pods::link::{reach_stage, Stage};
        let now = clock();
        let local = addr(2);
        let gram = addr(4);
        let phone = addr(5);
        let pods = addr(9);
        for retained in [phone, gram] {
            let mut daemon = Daemon::new("omarchy-xps");
            daemon.select_accessory(Some(pods));
            daemon.set_local_address(Some(local));
            daemon.restore_fleet(Fleet {
                id: FleetId::new([3; 16]),
                key: GroupKey::new([0x11; 32]),
                members: vec![
                    Member { address: local, label: "omarchy-xps".into() },
                    Member { address: gram, label: "lg-gram".into() },
                ],
            });
            daemon.observe_nearby(
                host_id(Some(gram)),
                gram,
                "lg-gram",
                now.unix,
                true,
                Some([1; 16]),
            );
            daemon.hear_roster(
                Snapshot {
                    reporter: gram,
                    session: [1; 16],
                    sequence: 1,
                    sent: now.unix,
                    accessory: pods,
                    peers: vec![phone, gram],
                    audio_source: Some(retained),
                    last_use: UnixMs(0),
                    pending: false,
                    audio_idle: false,
                    intent: false,
                },
                now,
            );
            daemon.peers_seated.push(gram);
            let Start::Sent { tx } = daemon.start_route(gram) else { panic!("route") };
            assert_eq!(daemon.routing.admit(tx, true, Some(retained)), Admit::Beside);
            daemon.open_connection(now.mono);
            let generation = daemon.connection.generation();
            // This laptop's Bluetooth link comes up as it joins beside the gram.
            daemon.note_bluez(false);
            daemon.note_acl(true, now.unix);
            daemon.note_bluez(true);
            daemon.note_socket(true, generation, now.mono);
            // Gram's empty roster has not arrived yet. Neither a fresh cached
            // roster nor its held fallback may put a released helper back.
            for elapsed in [0, 16_000] {
                let at = Clock::new(now.mono.0 + elapsed, now.unix.0 + elapsed);
                daemon.observe_nearby(
                    host_id(Some(gram)),
                    gram,
                    "lg-gram",
                    at.unix,
                    true,
                    Some([1; 16]),
                );
                let peers = daemon.status_value(at)["connected_peers"].as_array().unwrap().clone();
                assert_eq!(peers.contains(&serde_json::json!(gram.display())), retained == gram,
                    "released helper must stay in the tray (retained={retained}, elapsed={elapsed})");
            }
            // The pods' fresh roster is authoritative even if it reports that
            // the helper actually remained or rejoined alongside this laptop.
            daemon.note_pod_peers(vec![gram, local], now.unix);
            assert!(reach_stage(
                &mut daemon.connection,
                Stage::Notifications,
                generation,
                now.mono
            ));
            assert!(daemon.connection.mark_ready(generation));
            assert_eq!(
                daemon.status_value(now)["connected_peers"],
                serde_json::json!([gram.display(), local.display()])
            );
        }
    }

    #[test]
    fn both_laptops_list_the_pods_in_join_order_with_the_newest_last() {
        use crate::fleet::members::Fleet;
        use crate::fleet::roster_air::Snapshot;
        // Addresses sort the other way round, so address order cannot pass for join order.
        let xps = addr(4);
        let gram = addr(2);
        let now = clock();
        let peers = |daemon: &Daemon| daemon.status_value(now)["connected_peers"].clone();
        let laptop = |name: &str, local: Address, joins: bool| {
            let mut daemon = Daemon::new(name);
            daemon.set_local_address(Some(local));
            daemon.restore_fleet(Fleet {
                id: FleetId::new([3; 16]),
                key: GroupKey::new([0x11; 32]),
                members: vec![
                    Member { address: xps, label: "omarchy-xps".into() },
                    Member { address: gram, label: "lg-gram".into() },
                ],
            });
            if joins {
                daemon.note_bluez(false);
                daemon.note_acl(true, now.unix);
                daemon.note_bluez(true);
            }
            ready_link(&mut daemon, now);
            daemon
        };
        let roster = |reporter: Address, listed: Vec<Address>| Snapshot {
            reporter,
            session: [1; 16],
            sequence: 1,
            sent: now.unix,
            accessory: addr(3),
            peers: listed,
            audio_source: None,
            last_use: UnixMs(0),
            pending: false,
            audio_idle: true,
            intent: false,
        };
        let expected = serde_json::json!([xps.display(), gram.display()]);

        // The XPS was alone; the gram joins and the pods list it first.
        let mut first = laptop("omarchy-xps", xps, true);
        first.note_pod_peers(vec![xps], now.unix);
        first.note_pod_peers(vec![gram, xps], now.unix);
        assert_eq!(peers(&first), expected);
        // The gram finds the XPS already there, whatever order the pods use.
        let mut joiner = laptop("lg-gram", gram, true);
        joiner.note_pod_peers(vec![gram, xps], now.unix);
        assert_eq!(peers(&joiner), expected);

        // A control socket reset on a live Bluetooth link keeps the order.
        let generation = first.connection().generation();
        first.note_socket(false, generation, now.mono);
        ready_link(&mut first, now);
        first.note_pod_peers(vec![gram, xps], now.unix);
        assert_eq!(peers(&first), expected);
        // It saw the gram join, so a disagreeing roster does not move it.
        first.observe_nearby(host_id(Some(gram)), gram, "lg-gram", now.unix, true, Some([1; 16]));
        first.hear_roster(roster(gram, vec![gram, xps]), now);
        assert_eq!(peers(&first), expected);

        // Restarted onto the live link: address order until the gram's roster.
        let mut restarted = laptop("omarchy-xps", xps, false);
        restarted.note_pod_peers(vec![xps, gram], now.unix);
        assert_eq!(peers(&restarted), serde_json::json!([gram.display(), xps.display()]));
        restarted.observe_nearby(
            host_id(Some(gram)),
            gram,
            "lg-gram",
            now.unix,
            true,
            Some([1; 16]),
        );
        restarted.hear_roster(roster(gram, vec![xps, gram]), now);
        assert_eq!(peers(&restarted), expected);
        assert_eq!(restarted.roster_peers(now), vec![xps, gram]);
    }

    #[test]
    fn a_solo_pod_list_is_completed_by_the_other_laptops_fresh_roster() {
        use crate::fleet::members::Fleet;
        use crate::fleet::roster_air::Snapshot;
        let mut daemon = Daemon::new("omarchy-xps");
        let now = clock();
        let local = addr(2);
        let gram = addr(4);
        daemon.set_local_address(Some(local));
        daemon.restore_fleet(Fleet {
            id: FleetId::new([3; 16]),
            key: GroupKey::new([0x11; 32]),
            members: vec![
                Member { address: local, label: "omarchy-xps".into() },
                Member { address: gram, label: "lg-gram".into() },
            ],
        });
        ready_link(&mut daemon, now);
        // Both laptops connected at once; the pods' list named only us.
        daemon.note_pod_peers(vec![local], now.unix);
        assert_eq!(daemon.roster_peers(now), vec![local]);
        daemon.observe_nearby(host_id(Some(gram)), gram, "lg-gram", now.unix, true, Some([1; 16]));
        daemon.hear_roster(
            Snapshot {
                reporter: gram,
                session: [1; 16],
                sequence: 1,
                sent: now.unix,
                accessory: addr(3),
                peers: vec![gram],
                audio_source: None,
                last_use: UnixMs(0),
                pending: false,
                audio_idle: true,
                intent: false,
            },
            now,
        );
        // The gram's roster does not name us yet, so it joined first.
        assert_eq!(daemon.roster_peers(now), vec![gram, local]);
        assert_eq!(
            daemon.status_value(now)["connected_peers"],
            serde_json::json!([gram.display(), local.display()])
        );
        // The other laptop explicitly leaving removes it again.
        daemon.hear_roster(
            Snapshot {
                reporter: gram,
                session: [1; 16],
                sequence: 2,
                sent: UnixMs(now.unix.0 + 1_000),
                accessory: addr(3),
                peers: vec![],
                audio_source: None,
                last_use: UnixMs(0),
                pending: false,
                audio_idle: true,
                intent: false,
            },
            Clock::new(now.mono.0 + 1_000, now.unix.0 + 1_000),
        );
        assert_eq!(daemon.roster_peers(now), vec![local]);
    }

    #[test]
    fn a_dropped_link_resets_the_saved_roster_instead_of_repeating_it() {
        use crate::fleet::members::Fleet;
        let mut daemon = Daemon::new("lg-gram");
        let now = clock();
        let local = addr(1);
        let peer = addr(2);
        daemon.set_local_address(Some(local));
        daemon.restore_fleet(Fleet {
            id: FleetId::new([3; 16]),
            key: GroupKey::new([0x11; 32]),
            members: vec![
                Member { address: local, label: "lg-gram".into() },
                Member { address: peer, label: "omarchy-xps".into() },
            ],
        });
        ready_link(&mut daemon, now);
        daemon.set_local_address(Some(local));
        daemon.peers_seated.push(peer);
        daemon.note_pod_peers(vec![local, peer], now.unix);
        assert_eq!(daemon.roster_peers(now), vec![local, peer]);
        // The pods drop this laptop (an Apple device took its seat). The
        // saved roster must reset: repeating the old pair from remembered
        // seated members resurrects a device that is no longer connected.
        let generation = daemon.connection().generation();
        daemon.note_socket(false, generation, now.mono);
        assert_eq!(daemon.roster_peers(now), Vec::<Address>::new());
    }

    pub(super) fn conflicting_roster_setup() -> Daemon {
        let mut daemon = Daemon::new("xps");
        let now = clock();
        daemon.set_local_address(Some(addr(1)));
        daemon.restore_fleet(Fleet {
            id: FleetId::new([3; 16]),
            key: GroupKey::new([0x11; 32]),
            members: vec![
                Member { address: addr(1), label: "xps".into() },
                Member { address: addr(2), label: "gram".into() },
            ],
        });
        ready_link(&mut daemon, now);
        daemon.note_pod_peers(vec![addr(1), addr(4)], now.unix);
        daemon.observe_nearby(
            host_id(Some(addr(2))),
            addr(2),
            "gram",
            now.unix,
            true,
            Some([1; 16]),
        );
        daemon
    }

    #[test]
    fn activity_does_not_connect_while_a_phone_plays_on_the_pods() {
        use crate::step::Input;
        // gram, 2026-10-19 16:11: input while disconnected asked the xps for a
        // seat, and the pair reservation dropped the playing iPhone.
        for phone_playing in [true, false] {
            let mut daemon = Daemon::new("gram");
            let now = clock();
            daemon.set_local_address(Some(addr(2)));
            daemon.restore_fleet(Fleet {
                id: FleetId::new([3; 16]),
                key: GroupKey::new([0x11; 32]),
                members: vec![
                    Member { address: addr(1), label: "xps".into() },
                    Member { address: addr(2), label: "gram".into() },
                ],
            });
            daemon.select_accessory(Some(addr(3)));
            daemon.observe_nearby(
                host_id(Some(addr(1))),
                addr(1),
                "xps",
                now.unix,
                true,
                Some([1; 16]),
            );
            daemon.hear_roster(
                crate::fleet::roster_air::Snapshot {
                    reporter: addr(1),
                    session: [1; 16],
                    sequence: 1,
                    sent: now.unix,
                    accessory: addr(3),
                    peers: vec![addr(1), addr(4)],
                    audio_source: Some(addr(4)),
                    last_use: now.unix,
                    pending: false,
                    audio_idle: !phone_playing,
                    intent: false,
                },
                now,
            );
            let reply = daemon.step(Input::Command(Command::Activity), now).reply;
            assert_eq!(
                reply.as_deref() == Some("ignored\n"),
                phone_playing,
                "phone playing={phone_playing}: {reply:?}"
            );
        }
    }

    pub(super) fn conflicting_roster(
        at: Clock,
        sequence: u64,
    ) -> crate::fleet::roster_air::Snapshot {
        crate::fleet::roster_air::Snapshot {
            reporter: addr(2),
            session: [1; 16],
            sequence,
            sent: at.unix,
            accessory: addr(3),
            peers: vec![addr(2), addr(4)],
            audio_source: Some(addr(4)),
            last_use: at.unix,
            pending: false,
            audio_idle: false,
            intent: false,
        }
    }

    #[test]
    fn conflicting_roster_probes_and_retires_a_silent_control_connection() {
        use crate::step::{Effect, Input};
        let mut daemon = conflicting_roster_setup();
        let now = clock();
        let generation = daemon.connection.generation();
        daemon.hear_roster(conflicting_roster(now, 1), now);
        let result = daemon.step(Input::Tick, now);
        assert!(result.effects.iter().any(|e| matches!(e, Effect::WriteControl { bytes, .. }
            if bytes == crate::pods::setup::request_keys())));
        assert_eq!(daemon.connection.stage(), Stage::Ready, "a peer's claim alone cannot drop us");
        let later = Clock::new(now.mono.0 + 4_000, now.unix.0 + 4_000);
        // Repeat broadcasts must not keep moving the liveness deadline.
        daemon.hear_roster(conflicting_roster(later, 2), later);
        let result = daemon.step(Input::Tick, later);
        assert!(!result.effects.iter().any(|e| matches!(e, Effect::WriteControl { .. })));
        // Neither a malformed packet nor a stale socket's reply proves liveness.
        daemon.step(Input::Accessory { generation, packet: vec![0xff] }, later);
        daemon.step(
            Input::Accessory { generation: Generation(generation.0 - 1), packet: key_packet(1, 2) },
            later,
        );
        let deadline = Clock::new(now.mono.0 + 5_001, now.unix.0 + 5_001);
        let result = daemon.step(Input::Tick, deadline);
        assert!(result.effects.iter().any(|e| matches!(e, Effect::Disconnect { .. })));
        assert!(!daemon.status_value(deadline)["connected"].as_bool().unwrap());
        assert!(daemon.roster_peers(deadline).is_empty());
        assert_ne!(daemon.connection.generation(), generation);
        daemon.step(Input::Accessory { generation, packet: key_packet(1, 2) }, deadline);
        assert_eq!(daemon.connection.stage(), Stage::Interrupted);
        assert_eq!(
            daemon.step(Input::Command(Command::Activity), deadline).reply.unwrap(),
            "ignored\n"
        );
    }

    #[test]
    fn live_control_response_survives_a_conflicting_peer_roster() {
        use crate::step::{Effect, Input};
        let mut daemon = conflicting_roster_setup();
        let now = clock();
        daemon.hear_roster(conflicting_roster(now, 1), now);
        daemon.step(Input::Tick, now);
        let generation = daemon.connection.generation();
        let reply = Clock::new(now.mono.0 + 100, now.unix.0 + 100);
        daemon.step(Input::Accessory { generation, packet: key_packet(1, 2) }, reply);
        let later = Clock::new(now.mono.0 + 6_000, now.unix.0 + 6_000);
        daemon.hear_roster(conflicting_roster(later, 2), later);
        let result = daemon.step(Input::Tick, later);
        assert_eq!(daemon.connection.stage(), Stage::Ready);
        assert!(!result
            .effects
            .iter()
            .any(|e| matches!(e, Effect::Disconnect { .. } | Effect::WriteControl { .. })));
        // A successful probe is not a permanent exemption if this link dies later.
        let again = Clock::new(now.mono.0 + 16_000, now.unix.0 + 16_000);
        daemon.hear_roster(conflicting_roster(again, 3), again);
        let result = daemon.step(Input::Tick, again);
        assert!(result.effects.iter().any(|e| matches!(e, Effect::WriteControl { bytes, .. }
            if bytes == crate::pods::setup::request_keys())));
    }

    #[test]
    fn only_accepted_conflicting_full_rosters_probe_the_connection() {
        use crate::step::{Effect, Input};
        for variant in 0..8 {
            let mut daemon = conflicting_roster_setup();
            let now = clock();
            let mut report = conflicting_roster(now, 1);
            match variant {
                0 => report.pending = true,
                1 => report.accessory = addr(9),
                2 => report.reporter = addr(8),
                3 => report.session = [9; 16],
                4 => report.peers = vec![addr(1), addr(2)],
                5 => report.peers = vec![addr(2)],
                6 => report.peers.clear(),
                _ => report.sent = UnixMs(now.unix.0.saturating_sub(16_000)),
            }
            daemon.hear_roster(report, now);
            let result = daemon.step(Input::Tick, now);
            assert!(
                !result.effects.iter().any(|e| matches!(e, Effect::WriteControl { bytes, .. }
                if bytes == crate::pods::setup::request_keys())),
                "variant {variant}"
            );
            assert_eq!(daemon.connection.stage(), Stage::Ready);
        }
    }

    #[test]
    fn an_empty_fleet_roster_cannot_resurrect_a_tray_device_during_setup() {
        use crate::fleet::members::Fleet;
        use crate::fleet::roster_air::Snapshot;
        use crate::step::Input;
        let mut daemon = Daemon::new("omarchy-xps");
        let now = clock();
        let local = addr(2);
        let gram = addr(4);
        let pods = addr(9);
        daemon.select_accessory(Some(pods));
        daemon.set_local_address(Some(local));
        daemon.restore_fleet(Fleet {
            id: FleetId::new([3; 16]),
            key: GroupKey::new([0x11; 32]),
            members: vec![
                Member { address: local, label: "omarchy-xps".into() },
                Member { address: gram, label: "lg-gram".into() },
            ],
        });
        daemon.observe_nearby(host_id(Some(gram)), gram, "lg-gram", now.unix, true, Some([1; 16]));
        let report = |sequence, peers| Snapshot {
            reporter: gram,
            session: [1; 16],
            sequence,
            sent: UnixMs(now.unix.0 + sequence),
            accessory: pods,
            peers,
            audio_source: None,
            last_use: UnixMs(0),
            pending: false,
            audio_idle: true,
            intent: false,
        };
        daemon.hear_roster(report(1, vec![gram]), now);
        daemon.peers_seated.push(gram);
        daemon.hear_roster(report(2, vec![]), now);
        // Opening the local control socket used to revive the last nonempty
        // list, despite Gram explicitly reporting that it was disconnected.
        daemon.step(Input::Command(Command::Connect), now);
        let generation = daemon.connection.generation();
        daemon.step(Input::ControlSocket { up: true, generation }, now);
        let peers = |d: &Daemon, at| d.status_value(at)["connected_peers"].clone();
        assert_eq!(peers(&daemon, now), serde_json::json!([]));
        assert!(!daemon.roster_peers(now).contains(&gram));
        // Neither a delayed older packet nor one for different AirPods may
        // overwrite the accepted disconnection in the remembered roster.
        daemon.hear_roster(report(1, vec![gram]), now);
        let mut foreign = report(3, vec![gram]);
        foreign.accessory = addr(8);
        daemon.hear_roster(foreign, now);
        assert_eq!(peers(&daemon, now), serde_json::json!([]));
        let later = Clock::new(now.mono.0 + 16_000, now.unix.0 + 16_000);
        daemon.observe_nearby(
            host_id(Some(gram)),
            gram,
            "lg-gram",
            later.unix,
            true,
            Some([1; 16]),
        );
        assert_eq!(peers(&daemon, later), serde_json::json!([]));
        // A new confirmed connection may seat Gram again.
        let mut joined = report(4, vec![gram]);
        joined.sent = later.unix;
        daemon.hear_roster(joined, later);
        assert_eq!(peers(&daemon, later), serde_json::json!([gram.display()]));
    }

    #[test]
    fn an_open_socket_in_setup_does_not_pass_the_fleet_roster_off_as_the_pods_list() {
        use crate::step::Input;
        let now = clock();
        let gram = addr(2);
        let pods = addr(3);
        let mut daemon = Daemon::new("omarchy-xps");
        daemon.set_local_address(Some(addr(1)));
        daemon.select_accessory(Some(pods));
        daemon.note_roster(RosterReport {
            reporter: host_id(Some(gram)),
            reporter_address: gram,
            accessory: pods,
            session: crate::types::SessionId(0),
            seq: 1,
            at: now.mono,
            sent: now.unix,
            peers: vec![gram, addr(4)],
            audio_source: Some(addr(4)),
            pending: false,
            audio_idle: false,
            intent: false,
            wire_session: None,
            last_use: UnixMs(0),
        });
        // An observer off the pods still dates the roster it draws.
        assert_eq!(daemon.status_value(now)["peer_list_updated_ms"], now.unix.0);
        daemon.step(Input::Command(Command::Connect), now);
        let generation = daemon.connection().generation();
        daemon.step(Input::ControlSocket { up: true, generation }, now);
        let status = daemon.status_value(now);
        assert_eq!(status["connected"], true);
        assert_ne!(status["control_connection"]["stage"], "ready");
        assert_eq!(status["peer_list_updated_ms"], 0);
    }

    #[test]
    fn the_pods_list_drops_a_held_laptop_from_the_roster() {
        use crate::fleet::members::Fleet;
        use crate::fleet::presence::FleetKeys;
        use crate::fleet::roster_air;
        use crate::step::{Effect, Input};

        let mut daemon = Daemon::new("omarchy-xps");
        let now = clock();
        let local = addr(2);
        let gram = addr(4);
        let phone = Address::parse("28:02:2E:BF:7F:7D").unwrap();
        ready_link(&mut daemon, now);
        daemon.set_local_address(Some(local));
        daemon.restore_fleet(Fleet {
            id: FleetId::new([3; 16]),
            key: GroupKey::new([0x11; 32]),
            members: vec![
                Member { address: local, label: "omarchy-xps".into() },
                Member { address: gram, label: "lg-gram".into() },
            ],
        });
        daemon.held_peers = vec![gram, local];
        daemon.held_reporter = Some(gram);
        daemon.observe_nearby(host_id(Some(gram)), gram, "lg-gram", now.unix, true, Some([1; 16]));
        let mut packet = vec![0x04, 0x00, 0x04, 0x00, 0x2e, 0x00, 0x01, 0x02, 0x02];
        packet.extend_from_slice(&local.bytes());
        packet.extend_from_slice(&[0x02, 0x00]);
        packet.extend_from_slice(&phone.bytes());
        packet.extend_from_slice(&[0x02, 0x01]);
        let generation = daemon.connection().generation();
        daemon.step(Input::Accessory { generation, packet }, now);
        daemon.note_source(true, 2, now.mono);

        let aired = daemon.audio_roster(now).expect("roster");
        assert_eq!(aired.peers, vec![local, phone]);
        let listed = daemon.status_value(now)["connected_peers"].as_array().unwrap().clone();
        let texts: Vec<_> = listed.iter().filter_map(|value| value.as_str()).collect();
        let local_text = local.display();
        let phone_text = phone.display();
        let gram_text = gram.display();
        assert!(texts.contains(&local_text.as_str()));
        assert!(texts.contains(&phone_text.as_str()));
        assert!(!texts.contains(&gram_text.as_str()));

        let first = daemon.step(Input::Tick, now);
        let second = daemon.step(Input::Tick, Clock::new(now.mono.0 + 500, now.unix.0 + 500));
        let keys = FleetKeys { id: [3; 16], key: [0x11; 32] };
        let sent = first
            .effects
            .iter()
            .chain(second.effects.iter())
            .find_map(|effect| match effect {
                Effect::Advertise(bytes) => roster_air::read(bytes, &keys),
                _ => None,
            })
            .expect("a roster advertisement");
        assert_eq!(sent.peers, vec![local, phone]);
    }

    #[test]
    fn the_codec_is_selected_on_the_reported_card_and_a2dp_connects_when_down() {
        use crate::audio::play::Profile;
        use crate::audio::route::{Card, Graph};
        use crate::step::{Effect, Input};

        let mut daemon = Daemon::new("gram");
        let now = clock();
        let pods = addr(3);
        daemon.select_accessory(Some(pods));
        ready(&mut daemon, now.mono);
        daemon.note_ownership(true, now.mono);
        daemon.output.claim();
        daemon.set_profiles(vec![Profile {
            name: "a2dp-sink-sbc_xq".into(),
            description: "High Fidelity Playback (A2DP Sink, codec SBC-XQ)".into(),
            priority: 10,
            available: true,
            sinks: 1,
            sources: 0,
        }]);
        daemon.activate_profile();
        let graph = Graph {
            default_sink: String::new(),
            sinks: Vec::new(),
            streams: Vec::new(),
            cards: vec![Card {
                name: "bluez_card.reported".into(),
                address: Some(pods),
                profiles: vec!["a2dp-sink-sbc_xq".into(), "a2dp-sink-sbc".into()],
                active: Some("off".into()),
            }],
        };
        let chosen = daemon.step(Input::Audio(graph), now);
        assert!(chosen.effects.iter().any(|effect| matches!(
            effect,
            Effect::SelectProfile { card, profile, .. }
                if card == "bluez_card.reported" && profile == "a2dp-sink-sbc_xq"
        )));
        assert!(chosen.effects.iter().any(|effect| matches!(
            effect,
            Effect::ConnectTransport { accessory, .. } if *accessory == pods
        )));
        let again = daemon.step(Input::Tick, now);
        assert!(again.effects.iter().all(|effect| !matches!(
            effect,
            Effect::SelectProfile { .. } | Effect::ConnectTransport { .. }
        )));
        let id = chosen
            .effects
            .iter()
            .find_map(|e| match e {
                Effect::SelectProfile { id, .. } => Some(*id),
                _ => None,
            })
            .unwrap();
        let failed = daemon.step(Input::ProfileResult { id, ok: false }, now);
        assert_eq!(
            daemon.status_value(now)["native_handoff"]["failure"],
            "profile was not selected"
        );
        assert!(!failed
            .effects
            .iter()
            .any(|effect| matches!(effect, Effect::SelectProfile { .. })));
        assert_eq!(daemon.status_value(now)["connection_lifecycle"]["codec"], "retry_wait");
        let early = daemon.step(Input::Tick, Clock::new(now.mono.0 + 999, now.unix.0 + 999));
        assert!(!early.effects.iter().any(|effect| matches!(effect, Effect::SelectProfile { .. })));
        let retry = daemon.step(Input::Tick, Clock::new(now.mono.0 + 1_000, now.unix.0 + 1_000));
        assert!(retry.effects.iter().any(|effect| matches!(effect,
            Effect::SelectProfile { id: next, card, profile }
                if *next != id && card == "bluez_card.reported" && profile == "a2dp-sink-sbc_xq"
        )));
    }

    #[test]
    fn a_dead_audio_server_forgets_stream_serials_and_keeps_the_claim() {
        use crate::audio::route::{Graph, LiveStream, Sink};
        use crate::step::{Effect, Input};

        let mut daemon = Daemon::new("gram");
        daemon.select_accessory(Some(addr(3)));
        daemon.output.claim();
        daemon.step(
            Input::Audio(Graph {
                default_sink: "speakers".into(),
                sinks: vec![Sink { name: "speakers".into(), address: None, profile: None }],
                streams: vec![LiveStream {
                    id: 7,
                    generation: 3,
                    sink: "speakers".into(),
                    role: String::new(),
                    node: "Brave".into(),
                    mute: false,
                    temporarily_muted: false,
                    volume: 0.4,
                    corked: false,
                }],
                cards: vec![],
            }),
            clock(),
        );
        assert_eq!(daemon.output.held_streams().len(), 1);
        daemon.step(Input::AudioLost, clock());
        assert!(daemon.output.wants_output());
        assert!(daemon.output.held_streams().is_empty());
        let tick =
            daemon.step(Input::Tick, Clock::new(clock().mono.0 + 2_000, clock().unix.0 + 2_000));
        assert!(tick.effects.iter().all(|effect| !matches!(
            effect,
            Effect::MuteStreams(_)
                | Effect::Restore(_)
                | Effect::MoveStreams(_)
                | Effect::SetDefaultSink { .. }
        )));
        assert!(
            daemon.output.held_streams().is_empty(),
            "a tick must not replan the dead server's graph"
        );
    }

    fn rpa() -> Address {
        Address::parse("40:11:22:83:AC:CD").unwrap()
    }

    fn captured_case(lid: u8) -> Vec<u8> {
        let mut frame = vec![0x07, 0x19, 0x01, 0x27, 0x20, 0x55, 0x87, 0xb1, lid, 0x00, 0x04];
        frame.extend_from_slice(&[0; 16]);
        frame
    }

    fn encrypted_case() -> Vec<u8> {
        let mut frame = vec![0x07, 0x19, 0x01, 0x27, 0x20, 0x64, 0xff, 0x0f, 0x08, 0x00, 0x04];
        frame.extend_from_slice(&[
            0x55, 0xde, 0xd4, 0x4f, 0xf0, 0x65, 0x42, 0xdc, 0x10, 0xa8, 0x03, 0xc1, 0xdb, 0x77,
            0x45, 0x85,
        ]);
        frame
    }

    pub(super) fn ready_link(daemon: &mut Daemon, now: Clock) {
        use crate::step::Input;
        daemon.select_accessory(Some(addr(3)));
        daemon.step(Input::Command(Command::Connect), now);
        let generation = daemon.connection().generation();
        set_up_link(daemon, generation, now);
    }

    /// The pods answer a control socket through to readiness.
    fn set_up_link(
        daemon: &mut Daemon,
        generation: Generation,
        now: Clock,
    ) -> Vec<crate::step::Effect> {
        use crate::step::Input;
        let mut battery = vec![0x04, 0x00, 0x04, 0x00, 0x04, 0x00, 0x03];
        battery.extend_from_slice(&[0x04, 0x01, 80, 0x02, 0x01]);
        battery.extend_from_slice(&[0x02, 0x01, 60, 0x01, 0x01]);
        battery.extend_from_slice(&[0x08, 0x01, 40, 0x02, 0x01]);
        if daemon.accessory.model.is_headset() {
            battery = vec![4, 0, 4, 0, 4, 0, 1, 1, 1, 80, 2, 1];
        }
        let mut effects = daemon.step(Input::ControlSocket { up: true, generation }, now).effects;
        for packet in [
            vec![0x01, 0x00, 0x04, 0x00, 0x01],
            vec![0x04, 0x00, 0x04, 0x00, 0x2b, 0x00, 0x01],
            battery,
            vec![
                0x04,
                0x00,
                0x04,
                0x00,
                0x06,
                0x00,
                0x00,
                if daemon.accessory.model.is_headset() { 0 } else { 2 },
            ],
        ] {
            effects.extend(daemon.step(Input::Accessory { generation, packet }, now).effects);
        }
        effects
    }

    #[test]
    fn activity_reconnects_in_the_background_when_the_nearby_helper_is_disconnected() {
        use crate::fleet::members::{Fleet, Member};
        use crate::fleet::route_air::{Answer, Kind, Message};
        use crate::step::{Effect, Input};
        for roster_known in [true, false] {
            let now = clock();
            let local = addr(1);
            let gram = addr(2);
            let pods = addr(9);
            let mut daemon = Daemon::new("xps");
            daemon.set_local_address(Some(local));
            daemon.select_accessory(Some(pods));
            daemon.restore_fleet(Fleet {
                id: FleetId::new([3; 16]),
                key: GroupKey::new([0x11; 32]),
                members: vec![
                    Member { address: local, label: "xps".into() },
                    Member { address: gram, label: "gram".into() },
                ],
            });
            daemon.observe_nearby(host_id(Some(gram)), gram, "gram", now.unix, true, Some([4; 16]));
            if roster_known {
                daemon.note_roster(RosterReport {
                    reporter: host_id(Some(gram)),
                    reporter_address: gram,
                    accessory: pods,
                    session: crate::types::SessionId(0),
                    seq: 1,
                    at: now.mono,
                    sent: now.unix,
                    peers: Vec::new(),
                    audio_source: None,
                    pending: false,
                    audio_idle: false,
                    intent: false,
                    wire_session: Some([4; 16]),
                    last_use: UnixMs(0),
                });
            }
            daemon.step_back(now);
            daemon.displaced = true;
            daemon.note_pod(
                false,
                Pod {
                    battery: crate::types::Battery::new(60, false).unwrap(),
                    in_ear: Some(true),
                    in_case: Some(false),
                },
                now.mono,
            );
            let mut attempt = daemon.step(Input::Command(Command::Activity), now);
            if !roster_known {
                assert_eq!(daemon.routing.phase(), Phase::WaitingAdmission);
                let target_session = daemon.routing_session(Some(FleetId::new([3; 16])));
                daemon.hear_route(
                    Message {
                        kind: Kind::Admission,
                        result: Answer::Unavailable,
                        source: gram,
                        target: local,
                        source_session: [4; 16],
                        target_session,
                        sequence: 1,
                        transaction: daemon.routing.tx().to_be_bytes(),
                        sent: now.unix,
                        accessory: pods,
                        retain: None,
                    },
                    now,
                );
                attempt = daemon.step(Input::Tick, now);
            }
            assert!(attempt.effects.iter().any(|e| matches!(e, Effect::OpenLink { .. })));
            assert!(!daemon.routing.phase().busy(), "a disconnected helper cannot admit us");
            assert_eq!(daemon.routing.error(), "");
            assert!(!daemon.output.wants_output());
            assert!(!attempt.effects.iter().any(|e| matches!(
                e,
                Effect::ConnectTransport { .. } | Effect::WriteControl { .. }
            )));
            let repeated = daemon.step(Input::Command(Command::Activity), now);
            assert!(!repeated.effects.iter().any(|e| matches!(e, Effect::OpenLink { .. })));
            let generation = daemon.connection.generation();
            daemon.note_setup_failed(generation, FailReason::Transport, now.mono);
            assert_eq!(daemon.connection.stage(), Stage::Retrying);
            let retry =
                daemon.step(Input::Tick, Clock::new(now.mono.0 + 2_000, now.unix.0 + 2_000));
            assert!(retry.effects.iter().any(|e| matches!(e, Effect::OpenLink { .. })));
            assert!(!retry.effects.iter().any(|e| matches!(e, Effect::ConnectTransport { .. })));
        }
    }

    #[test]
    fn activity_does_not_claim_audio() {
        let mut daemon = Daemon::new("gram");
        daemon.select_accessory(Some(addr(9)));
        let reply = daemon.handle(Command::Activity, clock());
        assert_eq!(reply.body.as_deref(), Some("accepted\n"));
        assert_eq!(daemon.routing().intent(), crate::fleet::route::Intent::None);
        assert!(!daemon.routing().want_seat());
        assert_eq!(daemon.connection().stage(), Stage::Waiting);
        assert_eq!(daemon.status_value(clock())["waiting_for_playback"], false);
        assert_eq!(daemon.status_value(clock())["connected"], false);
        assert!(!daemon.output.wants_output());
    }

    #[test]
    fn relayed_peer_names_update_status_and_save_only_when_changed() {
        use crate::step::{Effect, Input};
        let mut daemon = Daemon::new("xps");
        let now = clock();
        let local = addr(2);
        let phone = Address::parse("28:02:2E:BF:7F:7D").unwrap();
        let mac = Address::parse("F0:2F:4B:14:DE:29").unwrap();
        daemon.set_local_address(Some(local));
        daemon.apple_labels.insert(mac, "MacBook Pro".into());
        ready_link(&mut daemon, now);
        daemon.note_pod_peers(vec![local, phone], now.unix);
        let generation = daemon.connection.generation();
        let introduction = daemon.step(Input::Tick, now);
        assert_eq!(
            introduction
                .effects
                .iter()
                .filter(|e| matches!(e,
                    Effect::WriteControl { bytes, .. } if bytes.starts_with(&[4, 0, 4, 0, 0x10, 0])
                ))
                .count(),
            2
        );
        assert!(!daemon.step(Input::Tick, now).effects.iter().any(|e| matches!(e,
            Effect::WriteControl { bytes, .. } if bytes.starts_with(&[4, 0, 4, 0, 0x10, 0])
        )));
        let report = |label: &str| {
            let mut body = vec![1, 0xe1, 0x46];
            body.extend(b"btName");
            body.push(0x40 + label.len() as u8);
            body.extend(label.as_bytes());
            let mut packet = vec![4, 0, 4, 0, 0x11, 0];
            packet.extend(phone.bytes().into_iter().rev());
            packet.extend((body.len() as u16).to_le_bytes());
            packet.extend(body);
            packet
        };
        for label in ["Dan’s iPhone", "Work iPhone"] {
            let learned = daemon.step(Input::Accessory { generation, packet: report(label) }, now);
            assert!(learned.effects.iter().any(|e| matches!(e,
                Effect::StoreDevices(names) if names.get(&phone).map(String::as_str) == Some(label)
                    && names.get(&mac).map(String::as_str) == Some("MacBook Pro")
            )));
            assert!(daemon.status_value(now)["devices"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["address"] == phone.display() && row["label"] == label));
            let duplicate =
                daemon.step(Input::Accessory { generation, packet: report(label) }, now);
            assert!(!duplicate.effects.iter().any(|e| matches!(e, Effect::StoreDevices(_))));
        }
        for label in ["", "Unknown", "bad\nname", "iPhone"] {
            daemon.step(Input::Accessory { generation, packet: report(label) }, now);
            assert_eq!(daemon.apple_labels[&phone], "Work iPhone");
        }
        daemon.step(
            Input::Accessory { generation: Generation(generation.0 + 1), packet: report("stale") },
            now,
        );
        assert_eq!(daemon.apple_labels[&phone], "Work iPhone");
        daemon.learn_apple_name(local, "not an Apple device");
        assert!(!daemon.apple_labels.contains_key(&local));
        // A laptop attached to the pods announces itself as Omarchy; it is
        // never learned as an Apple device, and an earlier bad name is dropped.
        daemon.learn_apple_name(phone, "Omarchy - lg-gram");
        assert!(!daemon.apple_labels.contains_key(&phone));
    }

    #[test]
    fn saved_apple_devices_remain_available_when_another_peer_is_connected() {
        let mut daemon = Daemon::new("xps");
        let now = clock();
        let local = addr(2);
        let mac = Address::parse("F0:2F:4B:14:DE:29").unwrap();
        let phone = Address::parse("28:02:2E:BF:7F:7D").unwrap();
        daemon.set_local_address(Some(local));
        daemon.apple_labels.insert(mac, "MacBook Pro".into());
        daemon.apple_labels.insert(phone, "iPhone".into());
        ready_link(&mut daemon, now);
        for (peer, label) in [(mac, "MacBook Pro"), (phone, "iPhone")] {
            daemon.note_pod_peers(vec![local, peer], now.unix);
            let status = daemon.status_value(now);
            assert_eq!(
                status["connected_peers"],
                serde_json::json!([local.display(), peer.display()])
            );
            let devices = status["devices"].as_array().unwrap();
            let apples: Vec<_> = devices.iter().filter(|row| row["kind"] == "apple").collect();
            assert_eq!(apples.len(), 2);
            let connected = apples.iter().find(|row| row["address"] == peer.display()).unwrap();
            assert_eq!(connected["label"], label);
            let waiting = apples.iter().find(|row| row["address"] != peer.display()).unwrap();
            assert_eq!(waiting["nearby"], false);
            assert!(!status["connected_peers"].as_array().unwrap().contains(&waiting["address"]));
        }
        daemon.note_socket(false, daemon.connection.generation(), now.mono);
        let status = daemon.status_value(now);
        assert_eq!(status["connected_peers"], serde_json::json!([]));
        assert_eq!(status["devices"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn a_retry_keeps_its_history_and_ignores_the_old_socket_closing() {
        use crate::step::Input;
        let mut daemon = Daemon::new("xps");
        let now = clock();
        daemon.select_accessory(Some(addr(9)));
        daemon.step(Input::Command(Command::Connect), now);
        let old = daemon.connection.generation();
        daemon.step(Input::LinkFailed { generation: old, busy: false }, now);
        let later = Clock::new(now.mono.0 + 2_000, now.unix.0 + 2_000);
        daemon.step(Input::Tick, later);
        let generation = daemon.connection.generation();
        assert_ne!(old, generation);
        assert_eq!(daemon.status_value(later)["control_connection"]["recovering"], true);
        assert_eq!(daemon.status_value(later)["control_connection"]["retry"], 2);
        daemon.step(Input::ControlSocket { up: true, generation }, later);
        daemon.step(Input::Accessory { generation, packet: vec![1, 0, 4, 0, 0, 0] }, later);
        daemon.step(Input::ControlSocket { up: false, generation: old }, later);
        for packet in [
            vec![4, 0, 4, 0, 0x2b, 0],
            vec![4, 0, 4, 0, 4, 0, 3, 2, 1, 80, 5, 1, 4, 1, 79, 5, 1, 8, 1, 52, 2, 1],
            vec![4, 0, 4, 0, 6, 0, 2, 2],
        ] {
            daemon.step(Input::Accessory { generation, packet }, later);
        }
        assert_eq!(daemon.connection.stage(), Stage::Ready);
        assert!(!daemon.connection.recovering());
    }

    #[test]
    fn closing_the_docked_case_cancels_attempts_until_it_opens_again() {
        use crate::step::{Effect, Input};

        for retrying in [false, true] {
            let mut daemon = Daemon::new("gram");
            let now = clock();
            daemon.select_accessory(Some(addr(9)));
            daemon.set_accessory_keys(
                addr(9),
                AccessoryKeys {
                    irk: (0u8..16).collect::<Vec<_>>().try_into().unwrap(),
                    encryption: [0x22; 16],
                },
            );
            daemon.step(Input::Command(Command::Activity), now);
            let mut open = encrypted_case();
            open[8] = 0;
            let opening =
                daemon.step(Input::Proximity { address: rpa(), payload: open.clone() }, now);
            assert!(!opening.effects.iter().any(|e| matches!(e, Effect::OpenLink { .. })));
            daemon.step(Input::Command(Command::Activity), now);
            let now = Clock::new(now.mono.0 + 2_000, now.unix.0 + 2_000);
            let started = daemon.step(Input::Tick, now);
            assert!(started.effects.iter().any(|e| matches!(e, Effect::OpenLink { .. })));
            let generation = daemon.connection.generation();
            if retrying {
                daemon.step(Input::LinkFailed { generation, busy: false }, now);
                assert_eq!(daemon.connection.stage(), Stage::Retrying);
            }
            let closed =
                daemon.step(Input::Proximity { address: rpa(), payload: encrypted_case() }, now);
            assert_eq!(daemon.connection.stage(), Stage::Waiting);
            assert!(closed.effects.iter().any(|e| matches!(e, Effect::Disconnect { .. })));
            assert!(!closed.effects.iter().any(|e| matches!(e, Effect::OpenLink { .. })));

            // Completion from the cancelled attempt must not start a handshake.
            let late = daemon.step(Input::ControlSocket { up: true, generation }, now);
            assert!(!late.effects.iter().any(|e| matches!(e, Effect::WriteControl { .. })));
            daemon.step(Input::LinkFailed { generation, busy: false }, now);
            assert_eq!(daemon.connection.stage(), Stage::Waiting);

            // Silence after closure ages telemetry, not the connection hold.
            let later = Clock::new(now.mono.0 + 600_000, now.unix.0 + 600_000);
            for input in [
                Input::Tick,
                Input::Command(Command::Activity),
                Input::Command(Command::Connect),
                Input::Tick,
            ] {
                let held = daemon.step(input, later);
                assert!(!held.effects.iter().any(|e| matches!(e, Effect::OpenLink { .. })));
                assert_eq!(daemon.connection.stage(), Stage::Waiting);
            }
            assert_eq!(daemon.status_value(later)["lid_state"], 2);
            assert_eq!(daemon.status_value(later)["last_known_lid_state"], 1);
            let reopened = daemon.step(Input::Proximity { address: rpa(), payload: open }, later);
            assert!(!reopened.effects.iter().any(|e| matches!(e, Effect::OpenLink { .. })));
            let settled = Clock::new(later.mono.0 + 2_000, later.unix.0 + 2_000);
            let idle = daemon.step(Input::Tick, settled);
            assert!(!idle.effects.iter().any(|e| matches!(e, Effect::OpenLink { .. })));
            let started = daemon.step(Input::Command(Command::Activity), settled);
            assert!(started.effects.iter().any(|e| matches!(e, Effect::OpenLink { .. })));
            assert_eq!(daemon.connection.stage(), Stage::Connecting);
        }
    }

    #[test]
    fn closing_or_disconnecting_during_case_wake_cancels_the_deferred_open() {
        use crate::step::{Effect, Input};
        for disconnect in [false, true] {
            let mut daemon = Daemon::new("xps");
            daemon.select_accessory(Some(addr(9)));
            let now = clock();
            let pod = Pod {
                battery: Battery::new(70, true).unwrap(),
                in_case: Some(true),
                in_ear: Some(false),
            };
            daemon.note_pod(false, pod, now.mono);
            daemon.note_pod(true, pod, now.mono);
            daemon.connection.prepare();
            let opening = daemon.step(Input::Lid { open: true }, now);
            assert!(!opening.effects.iter().any(|e| matches!(e, Effect::OpenLink { .. })));
            daemon.step(Input::Command(Command::Activity), now);
            let cancel = if disconnect {
                Input::Command(Command::Disconnect)
            } else {
                Input::Lid { open: false }
            };
            daemon.step(cancel, now);
            let later = Clock::new(now.mono.0 + 120_000, now.unix.0 + 120_000);
            let generation = daemon.connection.generation();
            for input in [Input::Bluez(true), Input::AttachAvailable { generation }, Input::Tick] {
                let result = daemon.step(input, later);
                assert!(!result.effects.iter().any(|e| matches!(e, Effect::OpenLink { .. })));
            }
        }
    }

    #[test]
    fn a_closed_empty_case_does_not_block_buds_in_use() {
        use crate::step::{Effect, Input};

        let mut daemon = Daemon::new("gram");
        let now = clock();
        daemon.select_accessory(Some(addr(9)));
        let pod = Pod {
            battery: Battery::new(50, false).unwrap(),
            in_ear: Some(true),
            in_case: Some(false),
        };
        daemon.note_pod(false, pod, now.mono);
        daemon.note_pod(true, pod, now.mono);
        daemon.note_lid(false, now.mono);
        let started = daemon.step(Input::Command(Command::Connect), now);
        assert!(started.effects.iter().any(|e| matches!(e, Effect::OpenLink { .. })));
    }

    #[test]
    fn playback_with_pods_stowed_in_a_closed_case_does_not_claim_the_seat() {
        let mut daemon = Daemon::new("omarchy-xps");
        let now = clock();
        daemon.select_accessory(Some(addr(9)));
        let docked = Pod {
            battery: Battery::new(50, false).unwrap(),
            in_ear: Some(false),
            in_case: Some(true),
        };
        daemon.note_pod(false, docked, now.mono);
        daemon.note_pod(true, docked, now.mono);
        daemon.note_lid(false, now.mono);
        assert!(daemon.accessory.stowed());
        daemon.claim_playback(now);
        // No claim: the speaker streams must never be soft-muted waiting
        // for a sink that cannot appear.
        assert!(!daemon.output.wants_output());
        assert_eq!(daemon.status_value(now)["want_omarchy_seat"], false);
    }

    #[test]
    fn background_control_readiness_does_not_take_docked_playback_from_speakers() {
        use crate::audio::route::{Graph, LiveStream, Sink};
        use crate::step::{Effect, Input};

        let mut daemon = Daemon::new("gram");
        daemon.set_local_address(Some(addr(1)));
        let now = Clock::new(4_000, clock().unix.0);
        let at = |ms| Clock::new(now.mono.0 + ms, now.unix.0 + ms);
        daemon.select_accessory(Some(addr(3)));
        daemon.connection.note_bluez(false);
        let docked = Pod {
            battery: Battery::new(75, true).unwrap(),
            in_ear: Some(false),
            in_case: Some(true),
        };
        daemon.note_pod(false, docked, now.mono);
        daemon.note_pod(true, docked, now.mono);
        daemon.note_lid(true, MonoMs(1));
        let graph = Graph {
            default_sink: "speakers".into(),
            sinks: vec![Sink { name: "speakers".into(), address: None, profile: None }],
            cards: vec![],
            streams: vec![LiveStream {
                id: 1,
                generation: 10,
                sink: "speakers".into(),
                role: String::new(),
                node: "Brave".into(),
                mute: false,
                temporarily_muted: false,
                volume: 1.0,
                corked: false,
            }],
        };
        daemon.step(Input::Audio(graph.clone()), now);
        daemon.step(Input::Players(Vec::new()), now);
        let activity = daemon.step(Input::Command(Command::Activity), now);
        assert!(activity.effects.iter().any(|e| matches!(e, Effect::OpenLink { .. })));
        let generation = daemon.connection.generation();
        let mut effects = daemon
            .step(
                Input::Player {
                    service: "org.mpris.MediaPlayer2.brave".into(),
                    playing: Some(true),
                },
                at(100),
            )
            .effects;
        assert!(!daemon.output.wants_output());

        // Gram, 2026-10-23: setup's battery-only reports replaced the docked
        // placement with unknown. A ready control link then took speaker audio.
        daemon.step(Input::ControlSocket { up: true, generation }, at(200));
        for packet in [
            vec![1, 0, 4, 0, 1],
            vec![4, 0, 4, 0, 0x2b, 0, 1],
            vec![4, 0, 4, 0, 0x2e, 0, 1, 2, 0],
            vec![4, 0, 4, 0, 4, 0, 2, 4, 1, 75, 1, 1, 2, 1, 76, 1, 1],
        ] {
            effects.extend(daemon.step(Input::Accessory { generation, packet }, at(300)).effects);
        }
        assert_eq!(daemon.connection.stage(), Stage::Ready);
        assert!(!daemon.accessory.playback_available(at(300).mono));
        assert!(!daemon.accessory.both_docked());
        for ms in [500, 2_000, 9_000] {
            effects.extend(daemon.step(Input::Tick, at(ms)).effects);
            effects.extend(daemon.step(Input::Audio(graph.clone()), at(ms)).effects);
            assert!(!daemon.output.wants_output(), "control readiness is not bud-out evidence");
            assert!(daemon.output.held_streams().is_empty());
        }
        assert!(
            !effects.iter().any(|e| matches!(
                e,
                Effect::MuteStreams(_) | Effect::MoveStreams(_) | Effect::ConnectTransport { .. }
            )),
            "{effects:?}"
        );
        daemon.accessory.note_placement(false, Some(true), Some(false), at(10_000).mono);
        daemon.step(Input::Tick, at(10_000));
        assert!(daemon.output.wants_output(), "a bud-out report permits the pending takeover");
    }

    #[test]
    fn unavailable_play_returns_an_unclaimed_airpods_default_to_speakers() {
        use crate::audio::route::{Graph, LiveStream, Sink};
        use crate::step::{Effect, Input};

        for (headset, mpris) in [(false, true), (false, false), (true, true), (true, false)] {
            let mut daemon = Daemon::new("gram");
            let now = clock();
            daemon.select_accessory(Some(addr(3)));
            if headset {
                daemon.set_identity("Max", "A3454");
                daemon.accessory.note_control_head_wear(Some(false), Some(false), now.mono);
            } else {
                for right in [false, true] {
                    daemon.accessory.note_placement(right, Some(false), Some(true), now.mono);
                }
            }
            let mut graph = Graph {
                default_sink: "pods".into(),
                sinks: vec![
                    Sink { name: "speakers".into(), address: None, profile: None },
                    Sink {
                        name: "pods".into(),
                        address: Some(addr(3)),
                        profile: Some("a2dp-sink".into()),
                    },
                ],
                cards: vec![],
                streams: vec![],
            };
            daemon.step(Input::Audio(graph.clone()), now);
            daemon.step(Input::Players(Vec::new()), now);
            assert!(!daemon.output.wants_output());
            let mut effects = Vec::new();
            if mpris {
                effects.extend(
                    daemon
                        .step(
                            Input::Player {
                                service: "org.mpris.MediaPlayer2.brave".into(),
                                playing: Some(true),
                            },
                            now,
                        )
                        .effects,
                );
            }
            graph.streams.push(LiveStream {
                id: 1,
                generation: 10,
                sink: "pods".into(),
                role: String::new(),
                node: "Brave".into(),
                mute: false,
                temporarily_muted: false,
                volume: 1.0,
                corked: false,
            });
            effects.extend(daemon.step(Input::Audio(graph), now).effects);
            assert!(!daemon.output.wants_output());
            assert!(!daemon.roster_intent(now), "speaker playback cannot ask peers to yield");
            assert!(
                effects.iter().any(|e| matches!(e,
                    Effect::SetDefaultSink { name } if name == "speakers"
                )),
                "headset={headset} mpris={mpris}: {effects:?}"
            );
            assert!(
                effects.iter().any(|e| matches!(e,
                    Effect::MoveStreams(moved) if moved.sink == "speakers"
                )),
                "headset={headset} mpris={mpris}: {effects:?}"
            );
            assert!(!effects.iter().any(|e| matches!(e, Effect::MuteStreams(_))));
        }
    }

    #[test]
    fn unavailable_pods_leave_speaker_playback_audible_through_connection_retries() {
        use crate::audio::route::{Graph, LiveStream, Sink};
        use crate::step::{Effect, Input};

        for scenario in ["old sighting", "incomplete closed case", "open docked case"] {
            let mut daemon = Daemon::new("xps");
            let before = clock();
            let now = Clock::new(before.mono.0 + 20_000, before.unix.0 + 20_000);
            daemon.select_accessory(Some(addr(9)));
            daemon.connection.note_bluez(false);
            daemon.accessory.note_advertisement(before.mono, 0);
            if scenario == "old sighting" {
                daemon.accessory.note_placement(false, Some(true), Some(false), before.mono);
            } else {
                daemon.accessory.note_placement(false, Some(false), Some(true), now.mono);
                daemon.accessory.note_placement(
                    true,
                    Some(false),
                    (scenario == "open docked case").then_some(true),
                    now.mono,
                );
                daemon.note_lid(scenario == "open docked case", now.mono);
            }
            let graph = Graph {
                default_sink: "speakers".into(),
                sinks: vec![Sink { name: "speakers".into(), address: None, profile: None }],
                cards: Vec::new(),
                streams: vec![LiveStream {
                    id: 1,
                    generation: 10,
                    sink: "speakers".into(),
                    role: String::new(),
                    node: "Brave".into(),
                    mute: false,
                    temporarily_muted: false,
                    volume: 0.5,
                    corked: false,
                }],
            };
            daemon.step(Input::Audio(graph.clone()), now);
            daemon.step(Input::Players(Vec::new()), now);
            let playing = daemon.step(
                Input::Player {
                    service: "org.mpris.MediaPlayer2.brave".into(),
                    playing: Some(true),
                },
                now,
            );
            assert!(!daemon.output.wants_output(), "{scenario}");
            assert!(!playing.effects.iter().any(|e| matches!(e, Effect::MuteStreams(_))));
            let generation = daemon.connection.generation();
            daemon.step(Input::LinkFailed { generation, busy: false }, now);
            for elapsed in [1_000, 6_000, 9_000] {
                let at = Clock::new(now.mono.0 + elapsed, now.unix.0 + elapsed);
                for input in [Input::Tick, Input::Audio(graph.clone())] {
                    let result = daemon.step(input, at);
                    assert!(!daemon.output.wants_output(), "{scenario}: {elapsed}");
                    assert!(daemon.output.held_streams().is_empty());
                    assert!(!result.effects.iter().any(|e| matches!(e, Effect::MuteStreams(_))));
                }
            }
            // Taking a bud out supplies the missing evidence and permits takeover.
            let at = Clock::new(now.mono.0 + 10_000, now.unix.0 + 10_000);
            daemon.accessory.note_placement(false, Some(true), Some(false), at.mono);
            daemon.step(Input::Tick, at);
            assert!(daemon.output.wants_output(), "{scenario}");
        }
    }

    #[test]
    fn buds_in_the_ear_are_not_a_wait_for_the_case() {
        let mut daemon = Daemon::new("omarchy-xps");
        let now = clock();
        daemon.select_accessory(Some(addr(9)));
        daemon.handle(Command::Activity, now);
        let pod = Pod {
            battery: Battery::new(49, false).unwrap(),
            in_ear: Some(true),
            in_case: Some(false),
        };
        daemon.note_pod(false, pod, now.mono);
        daemon.note_pod(true, Pod { battery: Battery::new(52, false).unwrap(), ..pod }, now.mono);
        let status = daemon.status_value(now);
        assert_eq!(status["control_connection"]["stage"], "idle");
        assert_eq!(status["waiting_for_playback"], false);
    }

    #[test]
    fn wear_removal_pauses_and_reinsertion_resumes_owned_playback() {
        for signal in [
            "buds",
            "advertisement",
            "headset-left",
            "headset-right",
            "headset-both",
            "headset-legacy",
        ] {
            use crate::audio::route::{Graph, LiveStream, Sink};
            use crate::step::{Effect, Input};
            let brave = "org.mpris.MediaPlayer2.brave";
            let mut daemon = Daemon::new("xps");
            let now = clock();
            let at = |ms: u64| Clock::new(now.mono.0 + ms, now.unix.0 + ms);
            if signal.starts_with("headset-") {
                daemon.set_identity(
                    "Max",
                    if signal == "headset-legacy" { "A2096" } else { "A3454" },
                );
            }
            ready_link(&mut daemon, now);
            let generation = daemon.connection().generation();
            let wear = |removed: bool| {
                if signal == "advertisement" {
                    Input::Proximity {
                        address: addr(3),
                        payload: max2_frame(if removed { 0x21 } else { 0x23 }),
                    }
                } else {
                    Input::Accessory {
                        generation,
                        packet: vec![
                            4,
                            0,
                            4,
                            0,
                            6,
                            0,
                            u8::from(removed && signal != "headset-right"),
                            if signal == "headset-legacy" {
                                2
                            } else {
                                u8::from(
                                    removed && matches!(signal, "headset-right" | "headset-both"),
                                )
                            },
                        ],
                    }
                }
            };
            if signal == "advertisement" {
                daemon.step(wear(false), now);
            }
            daemon
                .step(Input::Observation(crate::pods::observe::Observation::Ownership(true)), now);
            daemon.step(Input::Player { service: brave.into(), playing: Some(false) }, now);
            daemon.step(Input::Player { service: brave.into(), playing: Some(true) }, now);
            daemon.step(
                Input::Audio(Graph {
                    default_sink: "pods".into(),
                    sinks: vec![Sink {
                        name: "pods".into(),
                        address: Some(addr(3)),
                        profile: Some("a2dp-sink".into()),
                    }],
                    cards: vec![],
                    streams: vec![LiveStream {
                        id: 9,
                        generation: 9,
                        sink: "pods".into(),
                        role: "".into(),
                        node: "Brave".into(),
                        mute: false,
                        temporarily_muted: false,
                        volume: 1.0,
                        corked: false,
                    }],
                }),
                now,
            );
            assert!(daemon.audio.routed());
            let mut effects = Vec::new();
            let mut feed = |daemon: &mut Daemon, input: Input, clock: Clock| {
                effects.extend(daemon.step(input, clock).effects);
            };
            feed(&mut daemon, wear(false), at(100));
            feed(&mut daemon, Input::Tick, at(600));
            feed(&mut daemon, wear(true), at(1_000));
            if signal.starts_with("headset-") && signal != "headset-legacy" {
                // A lagging advertisement must not cancel the control removal.
                feed(
                    &mut daemon,
                    Input::Proximity { address: addr(3), payload: max2_frame(0x23) },
                    at(1_100),
                );
            }
            // Either a BLE or control report uses the same removal debounce.
            feed(&mut daemon, Input::Tick, at(1_500));
            let paused: Vec<_> =
                effects.iter().filter(|e| matches!(e, Effect::PausePlayers(_))).cloned().collect();
            assert_eq!(paused.len(), 1, "{effects:?}");
            assert!(
                matches!(&paused[0], Effect::PausePlayers(players) if players == &[brave.to_string()])
            );
            if signal.starts_with("headset-") {
                assert_eq!(
                    daemon.accessory.head_worn(),
                    Some(!matches!(signal, "headset-both" | "headset-legacy"))
                );
                assert_eq!(
                    daemon.accessory.playback_blocked(),
                    matches!(signal, "headset-both" | "headset-legacy"),
                    "lifting one cup must not enable speaker fallback"
                );
            }
            effects.clear();
            let mut feed = |daemon: &mut Daemon, input: Input, clock: Clock| {
                effects.extend(daemon.step(input, clock).effects);
            };
            feed(
                &mut daemon,
                Input::Player { service: brave.into(), playing: Some(false) },
                at(1_600),
            );
            if signal == "headset-both" {
                feed(
                    &mut daemon,
                    Input::Accessory { generation, packet: vec![4, 0, 4, 0, 6, 0, 0, 1] },
                    at(2_000),
                );
                feed(&mut daemon, Input::Tick, at(2_500));
                assert!(
                    !effects.iter().any(|e| matches!(e, Effect::ResumePlayers(_))),
                    "replacing only one cup must not resume a two-cup pause"
                );
            }
            let mut feed = |daemon: &mut Daemon, input: Input, clock: Clock| {
                effects.extend(daemon.step(input, clock).effects);
            };
            feed(&mut daemon, wear(false), at(3_000));
            feed(&mut daemon, Input::Tick, at(3_500));
            let resumed = effects
            .iter()
            .filter(
                |e| matches!(e, Effect::ResumePlayers(players) if players == &[brave.to_string()]),
            )
            .count();
            assert_eq!(resumed, 1, "{effects:?}");
            // Our own resume is not a new play that claims again.
            let claim = [4, 0, 4, 0, 9, 0, 6, 1, 0, 0, 0];
            let replayed = daemon
                .step(Input::Player { service: brave.into(), playing: Some(true) }, at(3_600));
            assert!(!replayed.effects.iter().any(|effect| {
                matches!(effect, Effect::WriteControl { bytes, .. } if bytes == &claim)
                    || matches!(effect, Effect::MuteStreams(_))
            }));
            assert!(daemon.audio.local_playing());
        }
    }

    #[test]
    fn play_after_docking_or_headset_removal_uses_speakers_until_worn_again() {
        use crate::audio::route::{Graph, LiveStream, Sink};
        use crate::step::{Effect, Input};
        // XPS, 2026-10-23 14:47: playing on the pods, both docked with the lid
        // open, then play. The pods stayed connected for 23 s and Brave
        // played into the case.
        let brave = "org.mpris.MediaPlayer2.brave";
        let ears = |left: u8, right: u8| vec![0x04, 0x00, 0x04, 0x00, 0x06, 0x00, left, right];
        let graph = |sink: &str, corked: bool| Graph {
            default_sink: sink.into(),
            sinks: vec![
                Sink { name: "omarchy_speaker_tuning".into(), address: None, profile: None },
                Sink {
                    name: "pods".into(),
                    address: Some(addr(3)),
                    profile: Some("a2dp-sink".into()),
                },
            ],
            cards: vec![],
            streams: vec![LiveStream {
                id: 9,
                generation: 7554,
                sink: sink.into(),
                role: "".into(),
                node: "Brave".into(),
                mute: false,
                temporarily_muted: false,
                volume: 1.0,
                corked,
            }],
        };
        for (headset, lifted) in
            [(false, None), (true, None), (true, Some(false)), (true, Some(true))]
        {
            let mut daemon = Daemon::new("xps");
            let now = clock();
            let at = |ms: u64| Clock::new(now.mono.0 + ms, now.unix.0 + ms);
            if headset {
                daemon.set_identity("Max", "A3454");
            }
            ready_link(&mut daemon, now);
            let generation = daemon.connection().generation();
            daemon.step(Input::Accessory { generation, packet: ears(0x00, 0x00) }, now);
            daemon
                .step(Input::Observation(crate::pods::observe::Observation::Ownership(true)), now);
            daemon.step(Input::Audio(graph("omarchy_speaker_tuning", false)), now);
            daemon.step(Input::Players(Vec::new()), now);
            daemon.step(Input::Player { service: brave.into(), playing: Some(true) }, now);
            daemon.step(Input::Audio(graph("pods", false)), at(100));
            assert!(daemon.output.wants_output());
            // Removal pauses Brave without rerouting or starting output setup.
            daemon.step(
                Input::Accessory {
                    generation,
                    packet: match lifted {
                        Some(right) => ears(u8::from(!right), u8::from(right)),
                        None if headset => ears(1, 1),
                        None => ears(2, 2),
                    },
                },
                at(1_000),
            );
            if lifted.is_none() {
                assert!(!daemon.can_repair_output(), "removal must disable output repair");
                assert_eq!(
                    daemon.status_value(at(1_000))["audio_owner"]["playing"],
                    false,
                    "removal overrides a stale playing report without losing the owner"
                );
            }
            let docked = daemon.step(Input::Tick, at(1_500));
            assert!(docked.effects.iter().any(|e| matches!(e, Effect::PausePlayers(_))));
            assert!(!docked.effects.iter().any(|e| matches!(e, Effect::SetDefaultSink { .. })));
            daemon.step(Input::Player { service: brave.into(), playing: Some(false) }, at(1_600));
            daemon.step(Input::Audio(graph("pods", true)), at(1_700));
            // Manual Play stays on a partially worn Max, otherwise uses speakers.
            let mut effects = daemon
                .step(Input::Player { service: brave.into(), playing: Some(true) }, at(3_000))
                .effects;
            effects.extend(daemon.step(Input::Audio(graph("pods", false)), at(3_100)).effects);
            if lifted.is_some() {
                assert!(daemon.output.wants_output(), "one cup still worn retains pods output");
                assert!(!effects.iter().any(|e| matches!(e,
                    Effect::SetDefaultSink { name } if name == "omarchy_speaker_tuning")));
                assert!(!effects.iter().any(|e| matches!(e,
                    Effect::MoveStreams(moved) if moved.sink == "omarchy_speaker_tuning")));
                assert!(!effects.iter().any(|e| matches!(e, Effect::PausePlayers(_))));
                let settled = daemon.step(Input::Tick, at(4_500));
                assert!(daemon.output.wants_output());
                assert!(!settled.effects.iter().any(|e| matches!(e, Effect::PausePlayers(_))));
                continue;
            }
            assert!(!daemon.output.wants_output());
            assert!(
                effects.iter().any(|e| matches!(e,
                Effect::SetDefaultSink { name } if name == "omarchy_speaker_tuning")),
                "{effects:?}"
            );
            assert!(
                effects.iter().any(|e| matches!(e,
                Effect::MoveStreams(moved) if moved.sink == "omarchy_speaker_tuning"
                    && moved.streams.iter().any(|s| s.generation == 7554))),
                "{effects:?}"
            );
            assert!(!effects.iter().any(|e| matches!(e, Effect::MuteStreams(_))), "{effects:?}");
            daemon.step(Input::Audio(graph("omarchy_speaker_tuning", false)), at(3_200));
            let settled = daemon.step(Input::Tick, at(4_500));
            assert!(!settled.effects.iter().any(|e| matches!(e, Effect::MoveStreams(_))));
            assert!(!daemon.output.wants_output(), "ticks cannot reclaim off-head playback");
            if headset {
                // XPS, 2026-11-03: Max on the table reports off/unknown after
                // off/off. Seeking in Brave emits Paused -> Playing; that is
                // not new evidence that the headset can take speaker audio.
                daemon.step(
                    Input::Observation(crate::pods::observe::Observation::HeadWear {
                        primary: Some(false),
                        secondary: None,
                    }),
                    at(4_600),
                );
                daemon
                    .step(Input::Player { service: brave.into(), playing: Some(false) }, at(4_700));
                let mut seek = daemon
                    .step(Input::Player { service: brave.into(), playing: Some(true) }, at(4_701))
                    .effects;
                seek.extend(
                    daemon
                        .step(Input::Audio(graph("omarchy_speaker_tuning", true)), at(4_800))
                        .effects,
                );
                seek.extend(
                    daemon
                        .step(Input::Audio(graph("omarchy_speaker_tuning", false)), at(4_900))
                        .effects,
                );
                seek.extend(daemon.step(Input::Tick, at(5_000)).effects);
                assert!(
                    !daemon.output.wants_output(),
                    "seek must keep playing on speakers: {seek:?}"
                );
                assert!(!seek.iter().any(|e| matches!(e, Effect::MuteStreams(_))));
                assert!(!seek.iter().any(|e| matches!(e,
                    Effect::MoveStreams(moved) if moved.sink == "pods")));
            }
            // Re-wearing takes the pods back for the waiting play.
            daemon.step(
                Input::Accessory { generation, packet: ears(0x00, if headset { 0 } else { 2 }) },
                at(6_000),
            );
            daemon.step(Input::Tick, at(6_500));
            assert!(daemon.output.wants_output());
        }
    }

    #[test]
    fn putting_the_pods_in_moves_speaker_playback_unless_a_phone_plays() {
        use crate::audio::route::{Graph, LiveStream, Sink};
        use crate::pods::observe::Observation;
        use crate::step::{Effect, Input};
        // XPS, 2026-10-23 14:31: Brave kept playing on the speakers after
        // both pods went in, until a pause and play moved it.
        let claim = [4, 0, 4, 0, 9, 0, 6, 1, 0, 0, 0];
        let ears = |left: u8, right: u8| vec![0x04, 0x00, 0x04, 0x00, 0x06, 0x00, left, right];
        let owners = [
            "local",
            "idle phone",
            "playing phone",
            "already worn",
            "silent speakers",
            "no player",
        ];
        for owner in owners {
            let mut daemon = Daemon::new("omarchy-xps");
            let now = clock();
            let at = |ms: u64| Clock::new(now.mono.0 + ms, now.unix.0 + ms);
            let local = addr(2);
            let phone = Address::parse("28:02:2E:BF:7F:7D").unwrap();
            daemon.set_local_address(Some(local));
            ready_link(&mut daemon, now);
            let generation = daemon.connection().generation();
            // Brave was already playing when the daemon first looked.
            let players = if owner == "no player" {
                Vec::new()
            } else {
                vec![crate::audio::play::PlayerSeen {
                    service: "org.mpris.MediaPlayer2.brave".into(),
                    playing: true,
                }]
            };
            daemon.step(Input::Players(players), now);
            daemon.step(
                Input::Audio(Graph {
                    default_sink: "speakers".into(),
                    sinks: vec![
                        Sink { name: "speakers".into(), address: None, profile: None },
                        Sink {
                            name: "pods".into(),
                            address: Some(addr(3)),
                            profile: Some("a2dp-sink".into()),
                        },
                    ],
                    cards: Vec::new(),
                    streams: vec![LiveStream {
                        id: 7,
                        generation: 7260,
                        sink: "speakers".into(),
                        role: String::new(),
                        node: "Brave".into(),
                        mute: false,
                        temporarily_muted: false,
                        volume: 1.0,
                        corked: owner == "silent speakers",
                    }],
                }),
                now,
            );
            if owner == "local" {
                daemon.step(Input::Observation(Observation::Ownership(true)), now);
            } else {
                daemon.note_pod_peers(vec![local, phone], now.unix);
                daemon.step(
                    Input::Observation(Observation::Source {
                        local: false,
                        kind: 2,
                        remote: Some(phone),
                    }),
                    now,
                );
                daemon.step(Input::Observation(Observation::Ownership(false)), now);
                daemon.note_peer_playback(phone, owner == "playing phone", at(1));
            }
            if owner != "already worn" {
                daemon.step(Input::Accessory { generation, packet: ears(0x02, 0x02) }, at(10));
            }
            assert!(!daemon.output.wants_output(), "{owner}: claimed before the pods went in");
            let worn =
                daemon.step(Input::Accessory { generation, packet: ears(0x00, 0x00) }, at(2_000));
            let tick = daemon.step(Input::Tick, at(2_100));
            let moved = owner == "local" || owner == "idle phone";
            assert_eq!(daemon.output.wants_output(), moved, "{owner}");
            let wrote_claim = worn.effects.iter().chain(&tick.effects).any(
                |effect| matches!(effect, Effect::WriteControl { bytes, .. } if bytes == &claim),
            );
            assert_eq!(wrote_claim, moved, "{owner}");
            // One insertion is one claim, not a standing order.
            if moved {
                daemon.step(Input::Command(Command::HandoffRelease), at(3_000));
                daemon.step(Input::Tick, at(3_100));
                assert!(!daemon.output.wants_output(), "{owner}: reclaimed after a release");
            }
        }
    }

    #[test]
    fn an_apple_device_taking_the_slot_waits_for_fresh_activity_or_playback() {
        let mut daemon = Daemon::new("omarchy-xps");
        let now = clock();
        let local = addr(2);
        let phone = Address::parse("28:02:2E:BF:7F:7D").unwrap();
        ready_link(&mut daemon, now);
        daemon.set_local_address(Some(local));
        daemon.note_pod_peers(vec![local, phone], now.unix);
        assert_eq!(daemon.status_value(now)["waiting_for_playback"], false);
        let generation = daemon.connection().generation();
        daemon.note_socket(false, generation, now.mono);
        assert_eq!(daemon.status_value(now)["waiting_for_playback"], true);
        let passive = daemon.step(crate::step::Input::Tick, now);
        assert!(!passive.effects.iter().any(|e| matches!(e, crate::step::Effect::OpenLink { .. })));
        assert_eq!(daemon.handle(Command::Activity, now).body.as_deref(), Some("ignored\n"));
        let settled =
            Clock::new(now.mono.0 + DISPLACEMENT_HOLD_MS, now.unix.0 + DISPLACEMENT_HOLD_MS);
        assert_eq!(daemon.handle(Command::Activity, settled).body.as_deref(), Some("accepted\n"));
        assert_eq!(daemon.status_value(settled)["waiting_for_playback"], false);
        assert!(!daemon.output.wants_output());
        let away = Pod {
            battery: Battery::new(49, false).unwrap(),
            in_ear: Some(false),
            in_case: Some(true),
        };
        daemon.note_pod(false, away, now.mono);
        daemon.note_pod(true, away, now.mono);
        daemon.note_lid(false, now.mono);
        assert_eq!(daemon.status_value(now)["waiting_for_playback"], false);
    }

    #[test]
    fn a_fleet_laptop_or_a_failed_route_does_not_wait_for_playback() {
        let mut daemon = Daemon::new("omarchy-xps");
        let now = clock();
        let local = addr(2);
        let gram = addr(4);
        ready_link(&mut daemon, now);
        daemon.set_local_address(Some(local));
        daemon.restore_fleet(crate::fleet::members::Fleet {
            id: FleetId::new([3; 16]),
            key: GroupKey::new([0x11; 32]),
            members: vec![
                Member { address: local, label: "omarchy-xps".into() },
                Member { address: gram, label: "lg-gram".into() },
            ],
        });
        daemon.note_pod_peers(vec![local, gram], now.unix);
        let generation = daemon.connection().generation();
        daemon.note_socket(false, generation, now.mono);
        assert_eq!(daemon.status_value(now)["waiting_for_playback"], false);

        let mut idle = Daemon::new("gram");
        idle.select_accessory(Some(addr(9)));
        assert_eq!(idle.handle(Command::Activity, now).body.as_deref(), Some("accepted\n"));
        assert_eq!(idle.status_value(now)["waiting_for_playback"], false);
    }

    #[test]
    fn an_explicit_connect_ends_the_playback_wait() {
        let mut daemon = Daemon::new("omarchy-xps");
        let now = clock();
        let local = addr(2);
        let phone = Address::parse("28:02:2E:BF:7F:7D").unwrap();
        ready_link(&mut daemon, now);
        daemon.set_local_address(Some(local));
        daemon.note_pod_peers(vec![local, phone], now.unix);
        daemon.note_socket(false, daemon.connection().generation(), now.mono);
        assert_eq!(daemon.status_value(now)["waiting_for_playback"], true);
        assert_eq!(daemon.handle(Command::Connect, now).body.as_deref(), Some("accepted\n"));
        assert_eq!(daemon.status_value(now)["waiting_for_playback"], false);
    }

    /// Drop a playing laptop's ready link the way the pods reset it, then
    /// tick past the rejoin delay. Returns every effect along the way.
    fn evict_and_wait(daemon: &mut Daemon, from: Clock, ms: u64) -> Vec<crate::step::Effect> {
        use crate::step::Input;
        let generation = daemon.connection().generation();
        let mut effects = daemon.step(Input::ControlSocket { up: false, generation }, from).effects;
        effects.extend(daemon.step(Input::Bluez(false), from).effects);
        for elapsed in (100..=ms).step_by(100) {
            let at = Clock::new(from.mono.0 + elapsed, from.unix.0 + elapsed);
            effects.extend(daemon.step(Input::Tick, at).effects);
        }
        effects
    }

    #[test]
    fn the_pods_dropping_this_laptop_mid_playback_rejoin_alongside_the_phone() {
        use crate::step::{Effect, Input};
        // XPS, 2026-10-24 10:03: the pods listed only this laptop, the buds
        // went back in, Brave resumed, and the pods reset the link as the
        // phone reconnected. Afterwards they held both.
        let brave = "org.mpris.MediaPlayer2.brave";
        let opens =
            |effects: &[Effect]| effects.iter().any(|e| matches!(e, Effect::OpenLink { .. }));
        let mut daemon = Daemon::new("omarchy-xps");
        let now = clock();
        let at = |ms: u64| Clock::new(now.mono.0 + ms, now.unix.0 + ms);
        let local = addr(2);
        let phone = Address::parse("28:02:2E:BF:7F:7D").unwrap();
        ready_link(&mut daemon, now);
        daemon.set_local_address(Some(local));
        daemon.audio.startup_players(&[]);
        daemon.step(Input::Player { service: brave.into(), playing: Some(true) }, now);
        assert!(daemon.output.wants_output());
        daemon.note_pod_peers(vec![local], now.unix);

        let waiting = evict_and_wait(&mut daemon, at(100), REJOIN_DELAY_MS - 100);
        assert!(waiting.iter().any(
            |e| matches!(e, Effect::PausePlayers(players) if players == &[brave.to_string()])
        ));
        assert!(!opens(&waiting), "the newcomer settles first: {waiting:?}");
        assert!(!daemon.output.wants_output(), "playback stays paused until the link is back");
        // The browser acknowledges our pause before reconnection completes.
        // Without this report the test incorrectly leaves local_playing true.
        daemon.step(
            Input::Player { service: brave.into(), playing: Some(false) },
            at(REJOIN_DELAY_MS),
        );

        let mut rejoin = Vec::new();
        for elapsed in [REJOIN_DELAY_MS + 100, REJOIN_DELAY_MS + 200, REJOIN_DELAY_MS + 300] {
            rejoin.extend(daemon.step(Input::Tick, at(elapsed)).effects);
        }
        assert!(opens(&rejoin), "{rejoin:?}");
        assert!(!rejoin.iter().any(|e| matches!(e, Effect::ResumePlayers(_))));

        let ready = at(REJOIN_DELAY_MS + 1_500);
        daemon.step(Input::Bluez(true), ready);
        let generation = daemon.connection().generation();
        daemon.step(Input::Observation(crate::pods::observe::Observation::Ownership(false)), ready);
        let setup = set_up_link(&mut daemon, generation, ready);
        assert!(daemon.output.wants_output());
        assert!(setup.iter().any(|e| matches!(e,
            Effect::WriteControl { bytes, .. } if *bytes == crate::pods::setup::ownership(true)
        )));
        assert!(
            setup.iter().any(
                |e| matches!(e, Effect::ResumePlayers(players) if players == &[brave.to_string()])
            ),
            "{setup:?}"
        );
        // Our own resume is the claim already made, not a new play.
        daemon.step(Input::Player { service: brave.into(), playing: Some(true) }, ready);

        assert!(daemon.audio.local_playing());
        // XPS, 2026-11-01: the opening roster still named the iPhone 101 ms
        // after our resume. The pending resume must already have sent CLAIM.
        let stale = daemon.step(
            Input::Observation(crate::pods::observe::Observation::Ownership(false)),
            at(REJOIN_DELAY_MS + 1_601),
        );
        assert!(!stale.effects.iter().any(|e| matches!(e, Effect::PausePlayers(_))));
        assert!(daemon.output.wants_output());
        daemon.step(
            Input::Observation(crate::pods::observe::Observation::Ownership(true)),
            at(REJOIN_DELAY_MS + 1_700),
        );
        // A second reset inside the window is the phone insisting.
        daemon.note_pod_peers(vec![local, phone], ready.unix);
        let again = evict_and_wait(&mut daemon, at(REJOIN_DELAY_MS + 5_000), 6_000);
        assert!(again.iter().any(|e| matches!(e, Effect::PausePlayers(_))));
        assert!(!opens(&again), "{again:?}");
        assert!(!daemon.output.wants_output());
    }

    #[test]
    fn no_rejoin_for_a_fleet_handoff_a_paused_player_or_a_disconnect() {
        use crate::fleet::members::{Fleet, Member};
        use crate::step::{Effect, Input};
        let brave = "org.mpris.MediaPlayer2.brave";
        for case in ["alone", "gram_listed", "paused", "disconnect"] {
            let mut daemon = Daemon::new("omarchy-xps");
            let now = clock();
            let local = addr(2);
            let gram = addr(4);
            ready_link(&mut daemon, now);
            daemon.set_local_address(Some(local));
            daemon.restore_fleet(Fleet {
                id: FleetId::new([3; 16]),
                key: GroupKey::new([0x11; 32]),
                members: vec![
                    Member { address: local, label: "omarchy-xps".into() },
                    Member { address: gram, label: "lg-gram".into() },
                ],
            });
            daemon.audio.startup_players(&[]);
            daemon.step(Input::Player { service: brave.into(), playing: Some(true) }, now);
            assert!(daemon.output.wants_output(), "{case}");
            if case == "paused" {
                daemon.step(Input::Player { service: brave.into(), playing: Some(false) }, now);
            }
            daemon.note_pod_peers(
                if case == "gram_listed" { vec![local, gram] } else { vec![local] },
                now.unix,
            );
            let generation = daemon.connection().generation();
            let mut effects =
                daemon.step(Input::ControlSocket { up: false, generation }, now).effects;
            if case == "disconnect" {
                effects.extend(daemon.step(Input::Command(Command::Disconnect), now).effects);
            }
            for elapsed in (100..=REJOIN_DELAY_MS + 1_000).step_by(100) {
                let at = Clock::new(now.mono.0 + elapsed, now.unix.0 + elapsed);
                effects.extend(daemon.step(Input::Tick, at).effects);
            }
            let opened = effects.iter().any(|e| matches!(e, Effect::OpenLink { .. }));
            assert_eq!(opened, case == "alone", "{case}: {effects:?}");
        }
    }

    #[test]
    fn typing_asks_the_one_nearby_laptop_for_the_seat() {
        use crate::fleet::members::{Fleet, Member};
        use crate::types::FleetId;

        let mut daemon = Daemon::new("omarchy-xps");
        let now = clock();
        let local = addr(1);
        let gram = addr(2);
        daemon.select_accessory(Some(addr(9)));
        daemon.set_local_address(Some(local));
        daemon.restore_fleet(Fleet {
            id: FleetId::new([3; 16]),
            key: GroupKey::new([0x11; 32]),
            members: vec![
                Member { address: local, label: "omarchy-xps".into() },
                Member { address: gram, label: "lg-gram".into() },
            ],
        });
        daemon.observe_nearby(host_id(Some(gram)), gram, "lg-gram", now.unix, true, Some([4; 16]));
        let reply = daemon.handle(Command::Activity, now);
        assert_eq!(reply.body.as_deref(), Some("accepted\n"));
        assert_eq!(daemon.routing().peer(), Some(gram));
        assert_eq!(daemon.routing().phase(), crate::fleet::route::Phase::WaitingAdmission);
        assert_eq!(daemon.routing().intent(), crate::fleet::route::Intent::None);
        assert!(!daemon.output.wants_output());
    }

    #[test]
    fn activity_on_a_ready_link_does_not_ask_a_nearby_laptop_for_it() {
        use crate::fleet::members::{Fleet, Member};
        use crate::types::FleetId;

        let mut daemon = Daemon::new("omarchy-xps");
        let now = clock();
        let local = addr(1);
        let gram = addr(2);
        daemon.set_local_address(Some(local));
        daemon.restore_fleet(Fleet {
            id: FleetId::new([3; 16]),
            key: GroupKey::new([0x11; 32]),
            members: vec![
                Member { address: local, label: "omarchy-xps".into() },
                Member { address: gram, label: "lg-gram".into() },
            ],
        });
        daemon.observe_nearby(host_id(Some(gram)), gram, "lg-gram", now.unix, true, Some([4; 16]));
        ready_link(&mut daemon, now);
        daemon.routing.release_audio();
        let generation = daemon.connection.generation();

        for _ in 0..3 {
            let reply = daemon.handle(Command::Activity, now);
            assert_eq!(reply.body.as_deref(), Some("ignored\n"));
            assert_eq!(daemon.routing.phase(), Phase::Idle);
            assert_eq!(daemon.routing.intent(), crate::fleet::route::Intent::Release);
            assert_eq!(daemon.connection.generation(), generation);
            // (The rebuild-on-silence count is asserted inside the grant arm above.)
            assert!(!daemon.output.wants_output());
        }
    }

    #[test]
    fn a_play_on_a_ready_link_routes_audio_during_a_fleet_request() {
        use crate::audio::route::{Graph, Sink};
        use crate::step::{Effect, Input};

        let mut daemon = Daemon::new("omarchy-xps");
        let now = clock();
        ready_link(&mut daemon, now);
        daemon.routing.release_audio();
        daemon.step(Input::Players(Vec::new()), now);
        let pods = addr(3);
        let pods_sink = format!("bluez_output.{}.1", pods.display().replace(':', "_"));
        daemon.step(
            Input::Audio(Graph {
                default_sink: "speakers".into(),
                sinks: vec![
                    Sink { name: "speakers".into(), address: None, profile: None },
                    Sink {
                        name: pods_sink.clone(),
                        address: Some(pods),
                        profile: Some("a2dp-sink".into()),
                    },
                ],
                cards: Vec::new(),
                streams: Vec::new(),
            }),
            now,
        );
        daemon.routing.start(addr(2));
        assert!(daemon.routing.phase().busy());
        let generation = daemon.connection.generation();

        let claimed = daemon.step(
            Input::Player { service: "org.mpris.MediaPlayer2.brave".into(), playing: Some(true) },
            now,
        );

        assert!(daemon.output.wants_output(), "a fleet request must not consume the play edge");
        assert_eq!(daemon.routing.intent(), crate::fleet::route::Intent::Claim);
        assert_eq!(daemon.connection.generation(), generation, "use the existing connection");
        assert!(
            claimed.effects.iter().any(|effect| matches!(
                effect, Effect::WriteControl { bytes, generation: written, .. }
                    if *written == generation && bytes == &[4, 0, 4, 0, 9, 0, 6, 1, 0, 0, 0]
            )),
            "send CLAIM on the same step as Play"
        );
        assert_eq!(daemon.accessory.ownership.as_fresh(), None, "wait for the accessory's report");
        assert!(claimed
            .effects
            .iter()
            .all(|effect| !matches!(effect, Effect::OpenLink { .. } | Effect::Disconnect { .. })));
        assert!(claimed.effects.iter().any(|effect| matches!(
            effect, Effect::SetDefaultSink { name } if name == &pods_sink
        )));
    }

    #[test]
    fn losing_the_pods_releases_the_output_once_paused_players_leave_it() {
        use crate::audio::health::Output;
        use crate::audio::play::PlayerSeen;
        use crate::pods::observe::Observation;
        use crate::step::{Effect, Input};
        let output = |state| Output {
            name: "pods".into(),
            index: 7,
            address: Some(addr(3)),
            profile: Some("a2dp-sink".into()),
            state,
            muted: false,
            audible_volume: true,
        };
        let released = |effects: &[Effect]| {
            effects.iter().any(|e| matches!(e, Effect::SuspendOutput { name } if name == "pods"))
        };
        for streaming in [true, false] {
            let mut daemon = Daemon::new("gram");
            let now = clock();
            let at = |ms: u64| Clock::new(now.mono.0 + ms, now.unix.0 + ms);
            ready_link(&mut daemon, now);
            daemon.audio.startup_players(&[PlayerSeen {
                service: "org.mpris.MediaPlayer2.brave".into(),
                playing: true,
            }]);
            daemon.output.claim();
            daemon.step(Input::Transport { active: streaming }, now);
            daemon.step(Input::Outputs(vec![output(0)]), now);
            let lost = daemon.step(Input::Observation(Observation::Ownership(false)), now);
            assert!(!released(&lost.effects), "players still on the output");
            let idle = daemon.step(Input::Outputs(vec![output(1)]), at(30));
            assert_eq!(released(&idle.effects), streaming);
            let again = daemon.step(Input::Outputs(vec![output(1)]), at(60));
            assert!(!released(&again.effects), "released once");
        }
    }

    #[test]
    fn a_handoff_mute_is_lifted_even_if_a_player_keeps_playing_or_is_handed_to_a_new_claim() {
        use crate::audio::play::PlayerSeen;
        use crate::audio::route::{Graph, LiveStream, Sink};
        use crate::pods::observe::Observation;
        use crate::step::{Effect, Input};
        let graph = Graph {
            default_sink: "pods".into(),
            sinks: vec![
                Sink {
                    name: "pods".into(),
                    address: Some(addr(3)),
                    profile: Some("a2dp-sink".into()),
                },
                Sink { name: "speakers".into(), address: None, profile: None },
            ],
            cards: Vec::new(),
            streams: vec![LiveStream {
                id: 1,
                generation: 11,
                sink: "pods".into(),
                role: String::new(),
                node: "Brave".into(),
                mute: false,
                temporarily_muted: false,
                volume: 0.8,
                corked: false,
            }],
        };
        for reclaim in [false, true] {
            let mut daemon = Daemon::new("xps");
            let now = clock();
            let at = |ms: u64| Clock::new(now.mono.0 + ms, now.unix.0 + ms);
            ready_link(&mut daemon, now);
            daemon.audio.startup_players(&[PlayerSeen {
                service: "org.mpris.MediaPlayer2.brave".into(),
                playing: true,
            }]);
            daemon.output.claim();
            daemon.step(Input::Audio(graph.clone()), now);
            daemon.step(Input::Observation(Observation::Ownership(false)), now);

            if reclaim {
                // A new claim restores the stream itself, with its original state.
                daemon.output.claim();
                daemon.step(Input::Tick, at(10));

                assert!(daemon
                    .output
                    .held_streams()
                    .iter()
                    .any(|s| s.id == 1 && !s.mute && s.volume == 0.8));
                continue;
            }
            // The player never reports a pause.
            daemon.audio.set_local_playing_for_test(true);
            let early = daemon.step(Input::Tick, at(900));
            assert!(!early.effects.iter().any(|e| matches!(e, Effect::Restore(_))));
            let late = daemon.step(Input::Tick, at(1_000));
            assert!(late.effects.iter().any(|e| matches!(e, Effect::Restore(s) if s.len() == 1)));
        }
    }

    #[test]
    fn a_play_waiting_for_the_link_claims_over_the_owner_the_setup_reports_describe() {
        use crate::audio::play::PlayerSeen;
        use crate::pods::observe::Observation;
        use crate::step::{Effect, Input};
        let claimed = |effects: &[Effect]| {
            effects.iter().any(|e| {
                matches!(e,
                Effect::WriteControl { bytes, .. } if *bytes == crate::pods::setup::ownership(true))
            })
        };
        let paused =
            |effects: &[Effect]| effects.iter().any(|e| matches!(e, Effect::PausePlayers(_)));
        let mut daemon = Daemon::new("xps");
        daemon.set_identity("AirPods", "A3064");
        let now = clock();
        let at = |ms: u64| Clock::new(now.mono.0 + ms, now.unix.0 + ms);
        daemon.select_accessory(Some(addr(3)));
        daemon.audio.startup_players(&[PlayerSeen {
            service: "org.mpris.MediaPlayer2.brave".into(),
            playing: false,
        }]);
        daemon.step(Input::Command(Command::Connect), now);
        let generation = daemon.connection().generation();
        daemon.step(Input::ControlSocket { up: true, generation }, now);
        // The play arrives while the link is still being set up.
        daemon.step(
            Input::Player { service: "org.mpris.MediaPlayer2.brave".into(), playing: Some(true) },
            at(10),
        );
        assert!(daemon.output.wants_output());
        let feed = |daemon: &mut Daemon, packet: Vec<u8>, ms: u64| {
            daemon.step(Input::Accessory { generation, packet }, at(ms))
        };
        feed(&mut daemon, vec![0x01, 0x00, 0x04, 0x00, 0x01], 20);
        feed(&mut daemon, vec![0x04, 0x00, 0x04, 0x00, 0x2b, 0x00, 0x01], 30);
        // The opening reports say another device owns the audio.
        let report = feed(
            &mut daemon,
            vec![0x04, 0x00, 0x04, 0x00, 0x09, 0x00, 0x06, 0x00, 0x00, 0x00, 0x00],
            40,
        );
        assert!(!paused(&report.effects));
        let mut battery = vec![0x04, 0x00, 0x04, 0x00, 0x04, 0x00, 0x03];
        battery.extend_from_slice(&[0x04, 0x01, 80, 0x02, 0x01]);
        battery.extend_from_slice(&[0x02, 0x01, 60, 0x01, 0x01]);
        battery.extend_from_slice(&[0x08, 0x01, 40, 0x02, 0x01]);
        feed(&mut daemon, battery, 50);
        let ready = feed(&mut daemon, vec![0x04, 0x00, 0x04, 0x00, 0x06, 0x00, 0x00, 0x02], 60);
        assert!(claimed(&ready.effects), "the waiting play asks for the audio");
        let remote = Observation::Source { local: false, kind: 2, remote: Some(addr(9)) };
        let stale = daemon.step(Input::Observation(remote.clone()), at(100));
        assert!(!paused(&stale.effects), "a report from before the claim is not a takeover");
        assert!(daemon.output.wants_output());
        // Without an answer to the claim, a later report is a real takeover.
        let later = daemon.step(Input::Observation(remote), at(1_700));
        assert!(paused(&later.effects));
    }

    #[test]
    fn a_remote_report_during_setup_settle_is_decided_when_the_window_closes() {
        use crate::audio::play::PlayerSeen;
        use crate::pods::observe::Observation;
        use crate::step::{Effect, Input};
        let paused =
            |effects: &[Effect]| effects.iter().any(|e| matches!(e, Effect::PausePlayers(_)));
        let resumed =
            |effects: &[Effect]| effects.iter().any(|e| matches!(e, Effect::ResumePlayers(_)));
        let mut daemon = Daemon::new("xps");
        daemon.set_identity("AirPods", "A3064");
        let now = clock();
        let at = |ms: u64| Clock::new(now.mono.0 + ms, now.unix.0 + ms);
        daemon.select_accessory(Some(addr(3)));
        daemon.audio.startup_players(&[PlayerSeen {
            service: "org.mpris.MediaPlayer2.brave".into(),
            playing: false,
        }]);
        daemon.step(Input::Command(Command::Connect), now);
        let generation = daemon.connection().generation();
        daemon.step(Input::ControlSocket { up: true, generation }, now);
        daemon.step(
            Input::Player { service: "org.mpris.MediaPlayer2.brave".into(), playing: Some(true) },
            at(10),
        );
        let feed = |daemon: &mut Daemon, packet: Vec<u8>, ms: u64| {
            daemon.step(Input::Accessory { generation, packet }, at(ms))
        };
        feed(&mut daemon, vec![0x01, 0x00, 0x04, 0x00, 0x01], 20);
        feed(&mut daemon, vec![0x04, 0x00, 0x04, 0x00, 0x2b, 0x00, 0x01], 30);
        feed(
            &mut daemon,
            vec![0x04, 0x00, 0x04, 0x00, 0x09, 0x00, 0x06, 0x00, 0x00, 0x00, 0x00],
            40,
        );
        let mut battery = vec![0x04, 0x00, 0x04, 0x00, 0x04, 0x00, 0x03];
        battery.extend_from_slice(&[0x04, 0x01, 80, 0x02, 0x01]);
        battery.extend_from_slice(&[0x02, 0x01, 60, 0x01, 0x01]);
        battery.extend_from_slice(&[0x08, 0x01, 40, 0x02, 0x01]);
        feed(&mut daemon, battery, 50);
        feed(&mut daemon, vec![0x04, 0x00, 0x04, 0x00, 0x06, 0x00, 0x00, 0x02], 60);
        assert!(daemon.output.wants_output());

        // One remote report lands inside the setup-settle window and no
        // duplicate arrives after it. It must be deferred, not dropped.
        let remote = Observation::Source { local: false, kind: 2, remote: Some(addr(9)) };
        let deferred = daemon.step(Input::Observation(remote), at(100));
        assert!(!paused(&deferred.effects));
        assert!(daemon.output.wants_output());
        daemon.queue_resume(vec!["org.mpris.MediaPlayer2.brave".into()]);
        let settling = daemon.step(Input::Tick, at(1_000));
        assert!(!paused(&settling.effects));
        assert!(!resumed(&settling.effects), "a deferred report fences queued resumes");
        assert!(daemon.output.wants_output());

        let decided = daemon.step(Input::Tick, at(1_600));
        assert!(paused(&decided.effects), "the deferred remote report must yield at settle");
        assert!(!resumed(&decided.effects));
        assert!(!daemon.output.wants_output());
        assert!(daemon.audio.yielded());
    }

    #[test]
    fn a_takeover_timeout_while_the_link_connects_defers_the_play_until_it_lands() {
        use crate::audio::play::PlayerSeen;
        use crate::audio::route::{Graph, LiveStream, Sink};
        use crate::step::{Effect, Input};
        let mut daemon = Daemon::new("xps");
        daemon.set_identity("AirPods", "A3064");
        let now = clock();
        let at = |ms: u64| Clock::new(now.mono.0 + ms, now.unix.0 + ms);
        daemon.select_accessory(Some(addr(3)));
        daemon.audio.startup_players(&[PlayerSeen {
            service: "org.mpris.MediaPlayer2.brave".into(),
            playing: false,
        }]);
        daemon.step(Input::Command(Command::Connect), now);
        let generation = daemon.connection().generation();
        daemon.step(Input::ControlSocket { up: true, generation }, now);
        let feed = |daemon: &mut Daemon, packet: Vec<u8>, ms: u64| {
            daemon.step(Input::Accessory { generation, packet }, at(ms))
        };
        feed(&mut daemon, vec![0x01, 0x00, 0x04, 0x00, 0x01], 20);
        feed(&mut daemon, vec![0x04, 0x00, 0x04, 0x00, 0x2b, 0x00, 0x01], 30);
        feed(
            &mut daemon,
            vec![0x04, 0x00, 0x04, 0x00, 0x09, 0x00, 0x06, 0x00, 0x00, 0x00, 0x00],
            40,
        );
        let mut battery = vec![0x04, 0x00, 0x04, 0x00, 0x04, 0x00, 0x03];
        battery.extend_from_slice(&[0x04, 0x01, 80, 0x02, 0x01]);
        battery.extend_from_slice(&[0x02, 0x01, 60, 0x01, 0x01]);
        battery.extend_from_slice(&[0x08, 0x01, 40, 0x02, 0x01]);
        feed(&mut daemon, battery.clone(), 50);
        let graph = Graph {
            default_sink: "speakers".into(),
            sinks: vec![Sink { name: "speakers".into(), address: None, profile: None }],
            cards: Vec::new(),
            streams: vec![LiveStream {
                id: 1,
                generation: 10,
                sink: "speakers".into(),
                role: String::new(),
                node: "Brave".into(),
                mute: false,
                temporarily_muted: false,
                volume: 1.0,
                corked: false,
            }],
        };
        daemon.step(Input::Audio(graph.clone()), at(60));
        daemon.step(
            Input::Player { service: "org.mpris.MediaPlayer2.brave".into(), playing: Some(true) },
            at(70),
        );
        assert!(daemon.output.wants_output(), "the play claims while the link opens");
        // The socket attempt hangs past the takeover hold: the speakers get
        // their stream back instead of staying muted forever.
        feed(&mut daemon, battery, 11_900);
        let expired = daemon.step(Input::Audio(graph.clone()), at(12_100));
        assert!(
            expired.effects.iter().any(|e| matches!(e, Effect::Restore(_))),
            "the expired hold restores the speakers"
        );
        assert!(!daemon.output.wants_output());
        // The link lands moments later; the still-playing stream must be
        // claimed then, not left on the speakers until the next play edge.
        feed(&mut daemon, vec![0x04, 0x00, 0x04, 0x00, 0x06, 0x00, 0x00, 0x02], 12_200);
        let landed = daemon.step(Input::Tick, at(12_300));
        assert!(daemon.output.wants_output(), "the landed link claims the waiting play");
        assert!(landed.effects.iter().any(|e| matches!(e,
            Effect::WriteControl { bytes, .. } if *bytes == crate::pods::setup::ownership(true))));
    }

    #[test]
    fn remote_takeover_mutes_streams_only_until_paused_players_drain() {
        use crate::audio::play::PlayerSeen;
        use crate::audio::route::{Graph, LiveStream, Sink};
        use crate::step::{Effect, Input};
        let mut daemon = Daemon::new("xps");
        let now = clock();
        ready_link(&mut daemon, now);
        daemon.audio.startup_players(&[PlayerSeen {
            service: "org.mpris.MediaPlayer2.brave".into(),
            playing: true,
        }]);
        daemon.output.claim();
        daemon.output.on_graph(
            Some(addr(3)),
            &Graph {
                default_sink: "speakers".into(),
                sinks: vec![],
                streams: vec![],
                cards: vec![],
            },
            true,
            now.mono,
        );
        let graph = Graph {
            default_sink: "pods".into(),
            sinks: vec![
                Sink {
                    name: "pods".into(),
                    address: Some(addr(3)),
                    profile: Some("a2dp-sink".into()),
                },
                Sink { name: "speakers".into(), address: None, profile: None },
            ],
            cards: Vec::new(),
            streams: (1..=2)
                .map(|id| LiveStream {
                    id,
                    generation: id as u64 + 10,
                    sink: "pods".into(),
                    role: String::new(),
                    node: "Brave".into(),
                    mute: false,
                    temporarily_muted: false,
                    volume: 1.0,
                    corked: false,
                })
                .collect(),
        };
        daemon.step(Input::Audio(graph.clone()), now);
        let yielded = daemon
            .step(Input::Observation(crate::pods::observe::Observation::Ownership(false)), now);
        // Muted ahead of the pause, so audio a player sends before it stops
        // does not follow the default to the speakers.
        assert!(
            matches!(yielded.effects.first(), Some(Effect::MuteStreams(streams)) if streams.len() == 2)
        );
        assert!(yielded
            .effects
            .iter()
            .any(|e| matches!(e, Effect::PausePlayers(names) if names.len() == 1)));
        assert!(yielded
            .effects
            .iter()
            .any(|e| matches!(e, Effect::SetDefaultSink { name } if name == "speakers")));
        assert!(daemon.output.held_streams().is_empty());
        daemon.step(
            Input::Player { service: "org.mpris.MediaPlayer2.brave".into(), playing: Some(false) },
            now,
        );
        let mut speakers = graph;
        speakers.default_sink = "speakers".into();
        for stream in &mut speakers.streams {
            stream.sink = "speakers".into();
        }
        let idle = daemon.step(Input::Audio(speakers.clone()), now);
        assert!(!idle
            .effects
            .iter()
            .any(|e| matches!(e, Effect::MuteStreams(_) | Effect::Restore(_))));
        // Uncontrolled streams are unmuted once paused players have drained.
        let draining = daemon.step(Input::Tick, Clock::new(now.mono.0 + 400, now.unix.0 + 400));
        assert!(!draining.effects.iter().any(|e| matches!(e, Effect::Restore(_))));
        let drained = daemon.step(Input::Tick, Clock::new(now.mono.0 + 500, now.unix.0 + 500));
        assert!(drained.effects.iter().any(|e| matches!(e,
            Effect::Restore(streams) if streams.len() == 2 && streams.iter().all(|s| !s.mute))));
        assert!(daemon.output.held_streams().is_empty());
        assert!(!daemon.output.wants_output());
        let peer = addr(4);
        daemon.note_pod_peers(vec![peer], now.unix);
        daemon.step(
            Input::Observation(crate::pods::observe::Observation::Source {
                local: false,
                kind: 2,
                remote: Some(peer),
            }),
            now,
        );
        daemon.note_peer_playback(peer, true, now);
        for elapsed in [100, 1_000, 4_000] {
            let at = Clock::new(now.mono.0 + elapsed, now.unix.0 + elapsed);
            for input in
                [Input::Command(Command::Activity), Input::Audio(speakers.clone()), Input::Tick]
            {
                let outcome = daemon.step(input, at);
                assert!(!outcome.effects.iter().any(|effect| matches!(effect,
                    Effect::WriteControl { bytes, .. } if *bytes == crate::pods::setup::ownership(true)
                )), "desktop activity and a continuing speaker stream cannot interrupt remote playback");
                assert!(!daemon.output.wants_output());
                assert!(daemon.audio.yielded());
            }
        }
        // The loop above left fresh desktop input on record, so the next
        // Play is a press at this keyboard, not the player resuming itself:
        // it takes over even inside the rebound window. (A play with no
        // input before it is re-paused; see the reclaim test.)
        let play = daemon.step(
            Input::Player { service: "org.mpris.MediaPlayer2.brave".into(), playing: Some(true) },
            Clock::new(now.mono.0 + 4_100, now.unix.0 + 4_100),
        );
        assert!(
            daemon.output.wants_output(),
            "a subsequent Play can take over while the peer is playing"
        );
        assert!(play.effects.iter().any(|effect| matches!(effect,
            Effect::WriteControl { bytes, .. } if *bytes == crate::pods::setup::ownership(true)
        )));
    }

    #[test]
    fn a_rearm_refreshes_the_known_card_instead_of_reconnecting_bluetooth() {
        use crate::audio::route::{Card, Graph, LiveStream, Sink};
        use crate::step::{Effect, Input};
        for card_known in [true, false] {
            let mut daemon = Daemon::new("xps");
            let now = clock();
            daemon.set_local_address(Some(addr(2)));
            ready_link(&mut daemon, now);
            daemon.step(Input::Players(Vec::new()), now);
            let mut graph = Graph {
                default_sink: "speakers".into(),
                sinks: vec![
                    Sink {
                        name: "pods".into(),
                        address: Some(addr(3)),
                        profile: Some("a2dp-sink".into()),
                    },
                    Sink { name: "speakers".into(), address: None, profile: None },
                ],
                cards: Vec::new(),
                streams: vec![LiveStream {
                    id: 1,
                    generation: 10,
                    sink: "speakers".into(),
                    role: String::new(),
                    node: "Brave".into(),
                    mute: false,
                    temporarily_muted: false,
                    volume: 1.0,
                    corked: false,
                }],
            };
            if card_known {
                graph.cards.push(Card {
                    name: "bluez_card.pods".into(),
                    address: Some(addr(3)),
                    profiles: vec!["a2dp-sink".into()],
                    active: Some("a2dp-sink".into()),
                });
            }
            daemon.step(Input::Audio(graph.clone()), now);
            daemon
                .step(Input::Observation(crate::pods::observe::Observation::Ownership(false)), now);
            daemon.step(
                Input::Player {
                    service: "org.mpris.MediaPlayer2.brave".into(),
                    playing: Some(true),
                },
                now,
            );
            daemon
                .step(Input::Observation(crate::pods::observe::Observation::Ownership(true)), now);
            daemon.audio.set_local_playing_for_test(true);
            daemon.step(Input::Tick, Clock::new(now.mono.0 + 100, now.unix.0 + 100));
            daemon.step(Input::Tick, Clock::new(now.mono.0 + 2_600, now.unix.0 + 2_600));
            let mut muted = graph;
            muted.streams[0].temporarily_muted = true;
            let reset = daemon.step(Input::Audio(muted), now);
            let card = reset.effects.iter().find_map(|effect| match effect {
                Effect::RestartOutput { card, .. } => Some(card.clone()),
                _ => None,
            });
            let expected =
                card_known.then(|| ("bluez_card.pods".to_string(), "a2dp-sink".to_string()));
            assert_eq!(card, Some(expected));
            assert_eq!(daemon.status_value(now)["connection_lifecycle"]["state"], "routing");
            assert_eq!(
                daemon.status_value(now)["connection_lifecycle"]["waiting_for"],
                "stream_route"
            );
        }
    }

    #[test]
    fn reclaim_waits_for_ownership_before_moving_streams_onto_the_sink() {
        use crate::audio::route::{Graph, LiveStream, Sink};
        use crate::step::{Effect, Input};
        let mut daemon = Daemon::new("xps");
        let now = clock();
        daemon.set_local_address(Some(addr(2)));
        ready_link(&mut daemon, now);
        daemon.step(Input::Players(Vec::new()), now);
        let graph = Graph {
            default_sink: "speakers".into(),
            sinks: vec![
                Sink {
                    name: "pods".into(),
                    address: Some(addr(3)),
                    profile: Some("a2dp-sink".into()),
                },
                Sink { name: "speakers".into(), address: None, profile: None },
            ],
            cards: Vec::new(),
            streams: vec![LiveStream {
                id: 1,
                generation: 10,
                sink: "speakers".into(),
                role: String::new(),
                node: "Brave".into(),
                mute: false,
                temporarily_muted: false,
                volume: 1.0,
                corked: false,
            }],
        };
        daemon.step(Input::Audio(graph.clone()), now);
        daemon.step(Input::Observation(crate::pods::observe::Observation::Ownership(false)), now);
        let play = daemon.step(
            Input::Player { service: "org.mpris.MediaPlayer2.brave".into(), playing: Some(true) },
            now,
        );
        let updated = daemon.step(Input::Audio(graph.clone()), now);
        assert!(
            play.effects.iter().any(|effect| matches!(effect, Effect::MuteStreams(_))),
            "silence speakers as soon as playback claims ownership"
        );
        let mute_at =
            play.effects.iter().position(|e| matches!(e, Effect::MuteStreams(_))).unwrap();
        let claim_at = play.effects.iter().position(|e| matches!(e,
            Effect::WriteControl { bytes, .. } if *bytes == crate::pods::setup::ownership(true)
        )).unwrap();
        assert!(mute_at < claim_at, "request the mute before starting the takeover");
        for outcome in [play, updated] {
            assert!(
                !outcome.effects.iter().any(|effect| matches!(
                    effect,
                    Effect::MoveStreams(_) | Effect::SetDefaultSink { .. }
                )),
                "normal routing must not race the upcoming sink recovery"
            );
        }
        let grant = daemon
            .step(Input::Observation(crate::pods::observe::Observation::Ownership(true)), now);
        assert!(!grant.effects.iter().any(|effect| matches!(effect, Effect::RestartOutput { .. })));
        assert!(!grant.effects.iter().any(|effect| matches!(effect, Effect::MoveStreams(_))));
        let mut muted = graph;
        muted.streams[0].temporarily_muted = true;
        daemon.step(Input::Tick, Clock::new(now.mono.0 + 100, now.unix.0 + 100));
        daemon.step(Input::Tick, Clock::new(now.mono.0 + 2_600, now.unix.0 + 2_600));
        let reset = daemon.step(Input::Audio(muted), now);
        assert!(reset.effects.iter().any(|effect| matches!(effect, Effect::RestartOutput { .. })));
        assert!(!daemon.output.held_streams()[0].mute, "save the speaker stream before muting it");
    }

    #[test]
    fn an_ownership_grant_rearms_a_yielded_sink_once_without_reconnecting() {
        use crate::audio::route::{Graph, Sink};
        use crate::step::{Effect, Input};
        for completion in ["grant", "source", "cancel"] {
            let mut daemon = Daemon::new("xps");
            let now = clock();
            let local = addr(2);
            let peer = addr(4);
            daemon.set_local_address(Some(local));
            ready_link(&mut daemon, now);
            let generation = daemon.connection.generation();
            daemon.step(Input::Players(Vec::new()), now);
            daemon.step(
                Input::Audio(Graph {
                    default_sink: "pods".into(),
                    sinks: vec![
                        Sink {
                            name: "pods".into(),
                            address: Some(addr(3)),
                            profile: Some("a2dp-sink".into()),
                        },
                        Sink { name: "speakers".into(), address: None, profile: None },
                    ],
                    cards: Vec::new(),
                    streams: Vec::new(),
                }),
                now,
            );
            daemon.step(
                Input::Observation(crate::pods::observe::Observation::Source {
                    local: false,
                    kind: 2,
                    remote: Some(peer),
                }),
                now,
            );
            daemon
                .step(Input::Observation(crate::pods::observe::Observation::Ownership(false)), now);
            let play = daemon.step(
                Input::Player {
                    service: "org.mpris.MediaPlayer2.brave".into(),
                    playing: Some(true),
                },
                now,
            );
            assert!(play.effects.iter().any(|effect| matches!(effect,
                Effect::WriteControl { bytes, .. } if bytes == &[4, 0, 4, 0, 9, 0, 6, 1, 0, 0, 0]
            )));
            assert!(!play
                .effects
                .iter()
                .any(|effect| matches!(effect, Effect::RestartOutput { .. })));
            assert_eq!(daemon.accessory.ownership.as_fresh(), Some(&false));
            if completion == "source" {
                daemon.step(
                    Input::Observation(crate::pods::observe::Observation::Source {
                        local: true,
                        kind: 2,
                        remote: None,
                    }),
                    now,
                );
            } else if completion == "cancel" {
                daemon.step(Input::Command(Command::HandoffRelease), now);
            }
            let granted = daemon.step(
                Input::Accessory { generation, packet: vec![4, 0, 4, 0, 9, 0, 6, 1, 0, 0, 0] },
                now,
            );
            assert!(
                !granted
                    .effects
                    .iter()
                    .any(|effect| matches!(effect, Effect::RestartOutput { .. })),
                "a grant now routes directly; the restart waits for a silent transport"
            );
            if completion == "grant" {
                daemon.audio.set_local_playing_for_test(true);
                let parked = Graph {
                    default_sink: "pods".into(),
                    sinks: vec![
                        Sink {
                            name: "pods".into(),
                            address: Some(addr(3)),
                            profile: Some("a2dp-sink".into()),
                        },
                        Sink { name: "speakers".into(), address: None, profile: None },
                    ],
                    cards: Vec::new(),
                    streams: vec![crate::audio::route::LiveStream {
                        id: 1,
                        generation: 10,
                        sink: "speakers".into(),
                        role: String::new(),
                        node: "Brave".into(),
                        mute: true,
                        temporarily_muted: false,
                        volume: 1.0,
                        corked: false,
                    }],
                };
                // Two snapshots: the first lets the rebuilt output observe
                // the parked stream, the second starts it.
                let tick_fx =
                    daemon.step(Input::Tick, Clock::new(now.mono.0 + 2_650, now.unix.0 + 2_650));
                let rebuilt_at = |fx: &[Effect]| {
                    fx.iter().filter(|e| matches!(e, Effect::RestartOutput { .. })).count()
                };
                let rebuilt = daemon.step(
                    Input::Audio(parked.clone()),
                    Clock::new(now.mono.0 + 2_700, now.unix.0 + 2_700),
                );

                let rebuilt2 = daemon.step(
                    Input::Audio(parked.clone()),
                    Clock::new(now.mono.0 + 2_800, now.unix.0 + 2_800),
                );
                let rebuilt3 = daemon
                    .step(Input::Audio(parked), Clock::new(now.mono.0 + 2_900, now.unix.0 + 2_900));
                let restarts = rebuilt_at(&tick_fx.effects)
                    + rebuilt_at(&rebuilt.effects)
                    + rebuilt_at(&rebuilt2.effects)
                    + rebuilt_at(&rebuilt3.effects);
                assert_eq!(restarts, 1, "a silent transport after the grant rebuilds once");
            }
            assert!(!granted.effects.iter().any(|effect| matches!(
                effect,
                Effect::OpenLink { .. } | Effect::Disconnect { .. } | Effect::SelectProfile { .. }
            )));
            assert_eq!(daemon.connection.generation(), generation);
            // (The rebuild-on-silence count is asserted inside the grant arm above.)
            let repeated = daemon
                .step(Input::Observation(crate::pods::observe::Observation::Ownership(true)), now);
            assert!(!repeated
                .effects
                .iter()
                .any(|effect| matches!(effect, Effect::RestartOutput { .. })));
        }
    }

    #[test]
    fn a_remote_report_just_before_the_grant_does_not_end_the_claim() {
        use crate::step::{Effect, Input};
        // XPS, 2026-10-19 15:56:16: ownership false, then true 1 ms later.
        let mut daemon = Daemon::new("xps");
        let now = clock();
        let at = |ms: u64| Clock::new(now.mono.0 + ms, now.unix.0 + ms);
        daemon.set_local_address(Some(addr(2)));
        ready_link(&mut daemon, now);
        let generation = daemon.connection.generation();
        let service = "org.mpris.MediaPlayer2.brave";
        daemon.step(Input::Players(Vec::new()), now);
        daemon.step(Input::Player { service: service.into(), playing: Some(true) }, now);
        let mut effects = Vec::new();
        for (ms, owned) in [(100, 0), (101, 1)] {
            let packet = vec![4, 0, 4, 0, 9, 0, 6, owned, 0, 0, 0];
            effects.extend(daemon.step(Input::Accessory { generation, packet }, at(ms)).effects);
        }
        effects.extend(daemon.step(Input::Tick, at(500)).effects);
        assert!(!effects.iter().any(|e| matches!(e, Effect::PausePlayers(_))));
        assert!(daemon.output.wants_output());
        assert!(!daemon.audio.yielded());
    }

    #[test]
    fn real_remote_reports_pause_local_media_once_and_a_fresh_play_can_reclaim() {
        use crate::step::{Effect, Input};
        let local = addr(2);
        let gram = addr(4);
        let mut source = vec![4, 0, 4, 0, 0x0e, 0];
        source.extend(gram.bytes().into_iter().rev());
        source.push(2);
        let ownership = vec![4, 0, 4, 0, 9, 0, 6, 0, 0, 0, 0];

        for packet in [ownership, source] {
            let mut daemon = Daemon::new("xps");
            let now = clock();
            daemon.set_local_address(Some(local));
            ready_link(&mut daemon, now);
            let generation = daemon.connection.generation();
            let service = "org.mpris.MediaPlayer2.brave";
            daemon.step(Input::Players(Vec::new()), now);
            daemon.note_pod_peers(vec![local, gram], now.unix);
            daemon.step(Input::Player { service: service.into(), playing: Some(true) }, now);
            assert!(daemon.output.wants_output());
            let remote_at = Clock::new(now.mono.0 + 1, now.unix.0 + 1);
            let reported =
                daemon.step(Input::Accessory { generation, packet: packet.clone() }, remote_at);
            // The pods may still grant the claim just sent; give them a moment.
            assert!(!reported.effects.iter().any(|e| matches!(e, Effect::PausePlayers(_))));
            assert!(daemon.output.wants_output());
            let remote_at = Clock::new(now.mono.0 + 301, now.unix.0 + 301);
            let yielded = daemon.step(Input::Tick, remote_at);
            assert!(
                yielded.effects.iter().any(|effect| matches!(
                    effect, Effect::PausePlayers(names) if names == &[service.to_string()]
                )),
                "a real remote report must pause the player"
            );
            assert!(daemon.audio.yielded());
            assert!(!daemon.output.wants_output());
            assert!(
                !daemon.wants_local_confirm(now.mono),
                "a pending pause is not new play intent"
            );
            assert_eq!(daemon.connection.stage(), Stage::Ready);
            assert_eq!(daemon.connection.generation(), generation);
            // (The rebuild-on-silence count is asserted inside the grant arm above.)

            let duplicate =
                daemon.step(Input::Accessory { generation, packet: packet.clone() }, remote_at);
            assert!(!duplicate
                .effects
                .iter()
                .any(|effect| matches!(effect, Effect::PausePlayers(_))));
            daemon.step(Input::Player { service: service.into(), playing: Some(true) }, remote_at);
            assert!(!daemon.output.wants_output(), "buffered playing feedback must not reclaim");
            // A play that returns right after the yield, while the remote
            // source is playing, is a rebound rather than intent: re-pause
            // it once instead of starting a claim war.
            daemon.step(Input::Player { service: service.into(), playing: Some(false) }, remote_at);
            daemon.step(
                Input::Accessory { generation, packet: peer_media(gram, local, true) },
                remote_at,
            );
            let rebound = daemon.step(
                Input::Player { service: service.into(), playing: Some(true) },
                Clock::new(remote_at.mono.0 + 2_000, remote_at.unix.0 + 2_000),
            );
            assert!(
                rebound.effects.iter().any(|effect| matches!(
                    effect, Effect::PausePlayers(names) if names == &[service.to_string()]
                )),
                "a rebound right after a yield must be re-paused"
            );
            assert!(!daemon.output.wants_output(), "a rebound must not reclaim");
            // Brave can rebound more than once after a yield; every replay
            // inside the window is re-paused, not just the first.
            daemon.step(Input::Player { service: service.into(), playing: Some(false) }, remote_at);
            let rebound2 = daemon.step(
                Input::Player { service: service.into(), playing: Some(true) },
                Clock::new(remote_at.mono.0 + 4_000, remote_at.unix.0 + 4_000),
            );
            assert!(
                rebound2.effects.iter().any(|effect| matches!(
                    effect, Effect::PausePlayers(names) if names == &[service.to_string()]
                )),
                "a second rebound inside the window must be re-paused too"
            );
            assert!(!daemon.output.wants_output(), "a second rebound must not reclaim");
            // A press preceded by fresh desktop input is intent even inside
            // the window: the shell reports the input before the key press
            // reaches the player, so no mouse click is needed to take over.
            daemon.step(Input::Player { service: service.into(), playing: Some(false) }, remote_at);
            daemon.step(
                Input::Command(crate::panel::ipc::Command::Activity),
                Clock::new(remote_at.mono.0 + 5_000, remote_at.unix.0 + 5_000),
            );
            let press = daemon.step(
                Input::Player { service: service.into(), playing: Some(true) },
                Clock::new(remote_at.mono.0 + 6_000, remote_at.unix.0 + 6_000),
            );
            assert!(
                !press.effects.iter().any(|effect| matches!(effect, Effect::PausePlayers(_))),
                "a play after desktop input must not be re-paused"
            );
            assert!(daemon.output.wants_output(), "a play after desktop input takes audio back");
            assert!(!daemon.audio.yielded());
        }
    }

    #[test]
    fn a_fresh_fleet_intent_pauses_local_players_ahead_of_the_claim_and_a_replay_cannot_repeat_it()
    {
        use crate::fleet::members::{Fleet, Member};
        use crate::fleet::roster_air::Snapshot;
        use crate::pods::observe::Observation;
        use crate::step::{Effect, Input};
        let local = addr(2);
        let gram = addr(4);
        let pods = addr(3);
        let mut daemon = Daemon::new("xps");
        let now = clock();
        let at = |ms: u64| Clock::new(now.mono.0 + ms, now.unix.0 + ms);
        daemon.set_local_address(Some(local));
        daemon.restore_fleet(Fleet {
            id: FleetId::new([3; 16]),
            key: GroupKey::new([0x11; 32]),
            members: vec![
                Member { address: local, label: "xps".into() },
                Member { address: gram, label: "gram".into() },
            ],
        });
        ready_link(&mut daemon, now);
        let service = "org.mpris.MediaPlayer2.brave";
        daemon.step(Input::Players(Vec::new()), now);
        daemon.step(Input::Player { service: service.into(), playing: Some(true) }, now);
        daemon.step(Input::Observation(Observation::Ownership(true)), now);
        assert!(daemon.output.wants_output());
        daemon.observe_nearby(host_id(Some(gram)), gram, "lg-gram", now.unix, true, Some([7; 16]));
        let intent = |sequence: u64, sent: UnixMs| Snapshot {
            reporter: gram,
            session: [7; 16],
            sequence,
            sent,
            accessory: pods,
            peers: vec![gram, local],
            audio_source: Some(local),
            last_use: UnixMs(0),
            pending: false,
            audio_idle: false,
            intent: true,
        };
        daemon.hear_roster(intent(1, now.unix), now);
        let yielded = daemon.step(Input::Tick, at(10));
        assert!(
            yielded.effects.iter().any(|effect| matches!(
                effect, Effect::PausePlayers(names) if names == &[service.to_string()]
            )),
            "a fresh intent must pause local players ahead of the claim"
        );
        assert!(!daemon.output.wants_output());
        let hold_until = daemon.session.intent_until;
        assert!(hold_until.is_some(), "the speculative yield must be bounded");

        // The same sealed frame replayed verbatim is not a new intent: it
        // must neither pause again nor extend the hold.
        daemon.hear_roster(intent(1, now.unix), at(20));
        let replayed = daemon.step(Input::Tick, at(30));
        assert!(!replayed
            .effects
            .iter()
            .any(|effect| matches!(effect, Effect::PausePlayers(_) | Effect::ResumePlayers(_))));
        assert_eq!(daemon.session.intent_until, hold_until);

        // No grant lands inside the hold: the paused player resumes and the
        // seat is this laptop's again.
        let resumed = daemon.step(Input::Tick, at(12_100));
        assert!(
            resumed.effects.iter().any(|effect| matches!(
                effect, Effect::ResumePlayers(names) if names == &[service.to_string()]
            )),
            "an intent that never becomes a claim must give playback back"
        );
        assert_eq!(daemon.session.intent_until, None);
    }

    #[test]
    fn a_stem_toggle_on_a_yielded_device_is_repaused_but_a_desktop_press_takes_over() {
        use crate::step::{Effect, Input};
        let local = addr(2);
        let gram = addr(4);
        let mut source = vec![4, 0, 4, 0, 0x0e, 0];
        source.extend(gram.bytes().into_iter().rev());
        source.push(2);
        for desktop_press in [false, true] {
            let mut daemon = Daemon::new("xps");
            daemon.set_identity("AirPods", "A3064");
            let now = clock();
            let at = |ms: u64| Clock::new(now.mono.0 + ms, now.unix.0 + ms);
            daemon.set_local_address(Some(local));
            ready_link(&mut daemon, now);
            let generation = daemon.connection.generation();
            let service = "org.mpris.MediaPlayer2.brave";
            daemon.step(Input::Players(Vec::new()), now);
            daemon.note_pod_peers(vec![local, gram], now.unix);
            daemon.step(Input::Player { service: service.into(), playing: Some(true) }, now);
            assert!(daemon.output.wants_output());
            daemon.step(Input::Accessory { generation, packet: source.clone() }, at(1));
            let yielded = daemon.step(Input::Tick, at(301));
            assert!(yielded.effects.iter().any(|effect| matches!(
                effect, Effect::PausePlayers(names) if names == &[service.to_string()]
            )));
            assert!(!daemon.output.wants_output());

            // Long past the rebound window, a stem click toggles this
            // laptop's paused player back to Playing. With no desktop
            // input it is the pods' pause/resume key talking, not a
            // takeover press: repeated toggles must stay paused.
            if desktop_press {
                daemon.step(Input::Command(Command::Activity), at(60_000));
            }
            for toggle in 0..8 {
                daemon.step(
                    Input::Player { service: service.into(), playing: Some(false) },
                    at(60_000 + toggle * 1_000),
                );
                let replayed = daemon.step(
                    Input::Player { service: service.into(), playing: Some(true) },
                    at(60_500 + toggle * 1_000),
                );
                if desktop_press {
                    assert!(
                        !replayed
                            .effects
                            .iter()
                            .any(|effect| matches!(effect, Effect::PausePlayers(_))),
                        "a desktop press is a takeover, not a stem toggle"
                    );
                    assert!(daemon.output.wants_output());
                    break;
                }
                assert!(
                    replayed.effects.iter().any(|effect| matches!(
                        effect, Effect::PausePlayers(names) if names == &[service.to_string()]
                    )),
                    "stem toggle {toggle} on a yielded device must be re-paused"
                );
                assert!(!daemon.output.wants_output(), "a stem toggle must not claim");
            }
        }
    }

    #[test]
    fn delayed_activity_keeps_its_event_time_across_a_yield() {
        use crate::step::{Effect, Input};
        let local = addr(2);
        let gram = addr(4);
        let mut source = vec![4, 0, 4, 0, 0x0e, 0];
        source.extend(gram.bytes().into_iter().rev());
        source.push(2);
        for (event_ms, delivered_ms, play_ms, takes_over) in [
            (200, 1_000, 1_500, false),   // input before yield, delivered afterwards
            (400, 1_000, 1_500, true),    // actual input after yield
            (400, 30_000, 30_500, false), // expired report
            (2_000, 1_000, 1_500, false), // future timestamp
            (400, 1_000, 30_500, false),  // input expires before playback
        ] {
            let mut daemon = Daemon::new("xps");
            let now = clock();
            let at = |ms: u64| Clock::new(now.mono.0 + ms, now.unix.0 + ms);
            daemon.set_local_address(Some(local));
            ready_link(&mut daemon, now);
            let generation = daemon.connection.generation();
            let service = "org.mpris.MediaPlayer2.brave";
            daemon.step(Input::Players(Vec::new()), now);
            daemon.note_pod_peers(vec![local, gram], now.unix);
            daemon.step(Input::Player { service: service.into(), playing: Some(true) }, now);
            daemon.step(Input::Accessory { generation, packet: source.clone() }, at(1));
            daemon.step(Input::Tick, at(301));
            assert!(!daemon.output.wants_output());
            daemon.step(Input::Command(Command::ActivityAt(at(event_ms).unix)), at(delivered_ms));
            daemon
                .step(Input::Player { service: service.into(), playing: Some(false) }, at(play_ms));
            let replayed = daemon.step(
                Input::Player { service: service.into(), playing: Some(true) },
                at(play_ms + 1),
            );
            assert_eq!(
                daemon.output.wants_output(),
                takes_over,
                "event={event_ms}, delivered={delivered_ms}, play={play_ms}"
            );
            assert_eq!(
                replayed.effects.iter().any(|effect| matches!(effect, Effect::PausePlayers(_))),
                !takes_over
            );
        }
    }

    #[test]
    fn an_outside_started_session_is_contested_but_a_fleet_takeover_is_not() {
        use crate::fleet::members::{Fleet, Member};
        use crate::pods::observe::Observation;
        use crate::step::{Effect, Input};

        for (scenario, fleet_peer) in [("outside phone", false), ("fleet peer", true)] {
            let local = addr(2);
            let peer = addr(7);
            let mut daemon = Daemon::new("xps");
            let now = clock();
            let at = |ms: u64| Clock::new(now.mono.0 + ms, now.unix.0 + ms);
            daemon.set_local_address(Some(local));
            if fleet_peer {
                daemon.restore_fleet(Fleet {
                    id: FleetId::new([3; 16]),
                    key: GroupKey::new([0x11; 32]),
                    members: vec![
                        Member { address: local, label: "xps".into() },
                        Member { address: peer, label: "gram".into() },
                    ],
                });
            }
            ready_link(&mut daemon, now);
            let service = "org.mpris.MediaPlayer2.brave";
            daemon.step(Input::Players(Vec::new()), now);
            daemon.note_pod_peers(vec![local, peer], now.unix);
            daemon.step(Input::Player { service: service.into(), playing: Some(true) }, now);
            daemon.step(Input::Observation(Observation::Ownership(true)), at(10));
            let yielded = daemon.step(
                Input::Observation(Observation::PodPeers {
                    peers: vec![local, peer],
                    owner: Some(peer),
                }),
                at(2_000),
            );
            let paused_for_yield = |effects: &[Effect]| {
                effects.iter().any(|effect| {
                    matches!(effect, Effect::PausePlayers(names) if names == &[service.to_string()])
                })
            };
            let claims_in = |effects: &[Effect]| {
                effects
                    .iter()
                    .filter(|effect| {
                        matches!(effect,
                            Effect::WriteControl { bytes, .. }
                                if *bytes == crate::pods::setup::ownership(true))
                    })
                    .count()
            };
            assert!(paused_for_yield(&yielded.effects), "{scenario}: local playback pauses");
            assert_eq!(
                claims_in(&yielded.effects),
                0,
                "{scenario}: no claim before the peer starts"
            );
            assert!(
                !yielded.effects.iter().any(|effect| matches!(effect, Effect::ResumePlayers(_))),
                "{scenario}: contesting ownership must not resume the paused local player"
            );
            assert!(!daemon.output.wants_output(), "{scenario}: the seat yields first");

            daemon.step(Input::Player { service: service.into(), playing: Some(false) }, at(2_050));
            let started = daemon.step(
                Input::Observation(Observation::PeerPlaying { address: peer, playing: true }),
                at(2_800),
            );
            assert_eq!(
                claims_in(&started.effects),
                usize::from(!fleet_peer),
                "{scenario}: only a started outside session is contested"
            );
            assert!(
                !daemon.output.wants_output(),
                "{scenario}: ownership contest does not reclaim the output"
            );

            if !fleet_peer {
                // The pods grant the contest, then the phone insists once.
                // Answer that started session too, but stop after the window.
                daemon.step(Input::Observation(Observation::Ownership(true)), at(2_900));
                daemon.step(
                    Input::Observation(Observation::PeerPlaying { address: peer, playing: false }),
                    at(3_000),
                );
                let insisted = daemon.step(
                    Input::Observation(Observation::PeerPlaying { address: peer, playing: true }),
                    at(3_100),
                );
                assert_eq!(
                    claims_in(&insisted.effects),
                    1,
                    "insisting started session is contested"
                );
                daemon.step(Input::Observation(Observation::Ownership(true)), at(3_200));
                let late = daemon.step(
                    Input::Observation(Observation::PeerPlaying { address: peer, playing: true }),
                    at(8_000),
                );
                assert_eq!(claims_in(&late.effects), 0, "contest window must expire");
            }
        }
    }

    #[test]
    fn connection_admission_does_not_imply_playback_intent() {
        use crate::fleet::members::{Fleet, Member};
        use crate::fleet::route_air::{Kind, Message};
        use crate::pods::observe::Observation;
        use crate::step::{Effect, Input};
        for peer_on_pods in [false, true] {
            let local = addr(2);
            let gram = addr(1);
            let mut daemon = Daemon::new("xps");
            let now = clock();
            let at = |ms: u64| Clock::new(now.mono.0 + ms, now.unix.0 + ms);
            daemon.set_local_address(Some(local));
            daemon.restore_fleet(Fleet {
                id: FleetId::new([3; 16]),
                key: GroupKey::new([0x11; 32]),
                members: vec![
                    Member { address: local, label: "xps".into() },
                    Member { address: gram, label: "gram".into() },
                ],
            });
            ready_link(&mut daemon, now);
            let service = "org.mpris.MediaPlayer2.brave";
            daemon.step(Input::Players(Vec::new()), now);
            daemon.step(Input::Player { service: service.into(), playing: Some(true) }, now);
            daemon.step(Input::Observation(Observation::Ownership(true)), now);
            assert!(daemon.output.wants_output());
            daemon.note_pod_peers(
                if peer_on_pods { vec![local, gram] } else { vec![local] },
                now.unix,
            );
            daemon.hear_request(
                Message {
                    kind: Kind::Request,
                    result: crate::fleet::route_air::Answer::Ok,
                    source: gram,
                    target: local,
                    source_session: [9; 16],
                    target_session: [0; 16],
                    sequence: 1,
                    transaction: [7; 8],
                    sent: now.unix,
                    accessory: addr(3),
                    retain: None,
                },
                now,
            );
            let stepped = daemon.step(Input::Tick, at(10));
            let reply = daemon.read_route(&daemon.route_reply.as_ref().unwrap().packet).unwrap();
            assert_eq!(reply.result, crate::fleet::route_air::Answer::Ok);
            assert_eq!(reply.retain, Some(local));
            assert!(
                !stepped.effects.iter().any(|effect| matches!(
                    effect,
                    Effect::PausePlayers(_) | Effect::MuteStreams(_) | Effect::Disconnect { .. }
                )),
                "connection admission must not interrupt the owner: {peer_on_pods}"
            );
            assert!(daemon.audio.local_playing());
            assert!(daemon.output.wants_output());
            assert_eq!(daemon.session.intent_until, None);
            let later = daemon.step(Input::Tick, at(13_000));
            assert!(
                !later.effects.iter().any(|effect| matches!(
                    effect,
                    Effect::PausePlayers(_) | Effect::ResumePlayers(_)
                )),
                "a background join must not create a speculative pause/resume hold"
            );

            // Actual playback intent still frees the radio before the claim,
            // including when the requester has not finished connecting yet.
            let intent_at = at(14_000);
            daemon.observe_nearby(
                host_id(Some(gram)),
                gram,
                "gram",
                intent_at.unix,
                true,
                Some([9; 16]),
            );
            daemon.hear_roster(
                crate::fleet::roster_air::Snapshot {
                    reporter: gram,
                    session: [9; 16],
                    sequence: 1,
                    sent: intent_at.unix,
                    accessory: addr(3),
                    peers: if peer_on_pods { vec![local, gram] } else { vec![local] },
                    audio_source: Some(local),
                    last_use: UnixMs(0),
                    pending: false,
                    audio_idle: false,
                    intent: true,
                },
                intent_at,
            );
            let takeover = daemon.step(Input::Tick, at(14_010));
            assert!(
                takeover.effects.iter().any(|effect| matches!(
                    effect, Effect::PausePlayers(names) if names == &[service.to_string()]
                )),
                "explicit playback intent still yields ahead of the peer's claim"
            );
            assert!(daemon.session.intent_until.is_some());
        }
    }

    #[test]
    fn a_deferred_claim_still_airs_intent_while_it_waits_for_the_link() {
        use crate::step::Input;
        let mut daemon = Daemon::new("gram");
        let now = clock();
        daemon.set_local_address(Some(addr(2)));
        daemon.select_accessory(Some(addr(3)));
        // No link and no fresh sighting: the claim defers instead of
        // claiming, but the intent is already worth airing.
        daemon.step(Input::Bluez(false), now);
        daemon.claim_playback(now);
        assert!(daemon.roster_intent(now), "intent while the claim waits");
        let later = Clock::new(now.mono.0 + 16_000, now.unix.0 + 16_000);
        assert!(!daemon.roster_intent(later), "intent does not outlive the wait");
    }

    #[test]
    fn setup_idle_and_local_reports_do_not_pause_a_new_claim() {
        use crate::step::{Effect, Input};
        let mut daemon = Daemon::new("xps");
        daemon.set_identity("AirPods", "A3064");
        let now = clock();
        let local = addr(2);
        daemon.set_local_address(Some(local));
        daemon.select_accessory(Some(addr(3)));
        daemon.step(Input::Players(Vec::new()), now);
        daemon.step(
            Input::Player { service: "org.mpris.MediaPlayer2.brave".into(), playing: Some(true) },
            now,
        );
        let generation = daemon.connection.generation();
        let ownership = vec![4, 0, 4, 0, 9, 0, 6, 0, 0, 0, 0];
        let setup = daemon.step(Input::Accessory { generation, packet: ownership.clone() }, now);
        assert!(!setup.effects.iter().any(|effect| matches!(effect, Effect::PausePlayers(_))));
        assert!(daemon.output.wants_output(), "initial ownership is not a new handoff");
        ready_link(&mut daemon, now);

        for (address, kind) in [(addr(4), 0), (local, 2)] {
            let mut packet = vec![4, 0, 4, 0, 0x0e, 0];
            packet.extend(address.bytes().into_iter().rev());
            packet.push(kind);
            let observed = daemon.step(Input::Accessory { generation, packet }, now);
            assert!(!observed
                .effects
                .iter()
                .any(|effect| matches!(effect, Effect::PausePlayers(_))));
            assert!(daemon.output.wants_output());
        }
        let stale = daemon.step(
            Input::Accessory { generation: Generation(generation.0 + 1), packet: ownership },
            now,
        );
        assert!(!stale.effects.iter().any(|effect| matches!(effect, Effect::PausePlayers(_))));
        assert!(daemon.output.wants_output());
    }

    #[test]
    fn a_real_play_routes_the_accessory_and_a_snapshot_does_not() {
        use crate::audio::play::PlayerSeen;
        use crate::audio::route::{Graph, LiveStream, Sink, StreamTarget};
        use crate::step::{Effect, Input};

        let pods = Address::new([0xA0, 0, 0, 0, 0, 3]).unwrap();
        let mut daemon = Daemon::new("gram");
        daemon.set_identity("AirPods", "A3064");
        daemon.select_accessory(Some(pods));
        let now = clock();
        let service = "org.mpris.MediaPlayer2.spotify";
        daemon
            .step(Input::Players(vec![PlayerSeen { service: service.into(), playing: true }]), now);
        assert!(!daemon.output.wants_output(), "the first sight of a player is a snapshot");
        daemon.step(Input::Player { service: service.into(), playing: Some(false) }, now);
        let claimed =
            daemon.step(Input::Player { service: service.into(), playing: Some(true) }, now);
        assert!(daemon.output.wants_output());
        assert!(claimed
            .effects
            .iter()
            .all(|effect| !matches!(effect, Effect::SetDefaultSink { .. })));

        let speakers = "alsa_output.pci.analog-stereo";
        let pods_sink = "bluez_output.A0_00_00_00_00_03.1";
        let graph = Graph {
            default_sink: speakers.into(),
            sinks: vec![
                Sink { name: speakers.into(), address: None, profile: None },
                Sink {
                    name: pods_sink.into(),
                    address: Some(pods),
                    profile: Some("a2dp-sink".into()),
                },
                Sink { name: "alsa_output.hdmi".into(), address: None, profile: None },
            ],
            cards: Vec::new(),
            streams: vec![
                LiveStream {
                    id: 1,
                    generation: 8,
                    sink: speakers.into(),
                    role: String::new(),
                    node: String::new(),
                    mute: false,
                    temporarily_muted: false,
                    volume: 1.0,
                    corked: false,
                },
                LiveStream {
                    id: 2,
                    generation: 9,
                    sink: "alsa_output.hdmi".into(),
                    role: String::new(),
                    node: String::new(),
                    mute: false,
                    temporarily_muted: false,
                    volume: 0.4,
                    corked: false,
                },
            ],
        };
        let quieted = daemon.step(Input::Audio(graph.clone()), now);
        assert!(quieted.effects.iter().any(|effect| matches!(
            effect,
            Effect::MuteStreams(streams) if streams == &vec![StreamTarget { id: 1, generation: 8 }]
        )));
        let mut muted = graph;
        muted.streams[0].temporarily_muted = true;
        let switching = daemon.step(Input::Audio(muted.clone()), now);
        assert!(switching.effects.iter().any(|effect| matches!(
            effect,
            Effect::SetDefaultSink { name } if name == pods_sink
        )));
        let mut moved_graph = muted;
        moved_graph.default_sink = pods_sink.into();
        let moved = daemon.step(Input::Audio(moved_graph), Clock::new(2_000, 1_700_000_000_000));
        assert!(moved.effects.iter().any(|effect| matches!(
            effect,
            Effect::MoveStreams(streams)
                if streams.sink == pods_sink
                    && streams.streams == vec![StreamTarget { id: 1, generation: 8 }]
        )));
        assert!(daemon.audio.routed());
    }

    #[test]
    fn connect_without_an_accessory_is_an_error_and_a_snapshot_does_not_claim() {
        let mut daemon = Daemon::new("gram");
        let reply = daemon.handle(Command::Connect, clock());
        assert_eq!(reply.body.as_deref(), Some("error: no paired AirPods address\n"));
        let raw = b"playback:{\"kind\":\"snapshot\",\"application\":\"Browser\",\"time\":1700000000000,\"player\":\"tab\",\"token\":\"s\",\"playing\":true}\n";
        let crate::panel::ipc::Incoming::Ready(command) = push(raw) else { panic!("parse") };
        let reply = daemon.handle(command, clock());
        assert_eq!(reply.body.as_deref(), Some("accepted\n"));
        assert_eq!(daemon.routing().intent(), crate::fleet::route::Intent::None);
    }

    #[test]
    fn contradictory_reports_stay_distinct_in_one_snapshot() {
        let mut daemon = Daemon::new("gram");
        let pods = addr(9);
        let phone = addr(4);
        daemon.select_accessory(Some(pods));
        daemon.handle(
            Command::Playback(Ok(crate::types::PlaybackEvent {
                kind: crate::types::PlayKind::Play,
                application: "Browser".into(),
                time: UnixMs(1_700_000_000_000),
                player: "tab".into(),
                token: "play".into(),
                playing: true,
            })),
            clock(),
        );
        daemon.note_ownership(false, MonoMs(2_000));
        daemon.note_source(false, 2, MonoMs(2_100));
        daemon.note_remote(phone, MonoMs(2_100));
        let status = daemon.status_value(clock());
        assert_eq!(status["reported_ownership"], 0);
        assert_eq!(status["audio_source_local"], 0);
        assert_eq!(status["audio_owner"]["holder"], "remote");
        assert_eq!(status["remote_source_address"], phone.display());
        assert_eq!(status["audio_owner"]["playing"], true);
        assert_ne!(status["audio_source_local"], 1);
    }

    #[test]
    fn ongoing_playback_keeps_its_owner_after_the_source_report_ages() {
        use crate::audio::play::PlayerSeen;
        use crate::audio::route::{Graph, LiveStream, Sink};
        use crate::step::Input;
        let mut daemon = Daemon::new("xps");
        let now = clock();
        let local = addr(1);
        daemon.set_local_address(Some(local));
        ready_link(&mut daemon, now);
        // This connection supplied a source notification, but no ownership reply.
        daemon.note_source(true, 2, now.mono);
        daemon.output.claim();
        daemon.audio.startup_players(&[PlayerSeen {
            service: "org.mpris.MediaPlayer2.brave".into(),
            playing: true,
        }]);
        daemon.step(
            Input::Audio(Graph {
                default_sink: "pods".into(),
                sinks: vec![Sink {
                    name: "pods".into(),
                    address: Some(addr(3)),
                    profile: Some("a2dp-sink".into()),
                }],
                streams: vec![LiveStream {
                    id: 1,
                    generation: 1,
                    sink: "pods".into(),
                    role: String::new(),
                    node: "Brave".into(),
                    mute: false,
                    temporarily_muted: false,
                    volume: 1.0,
                    corked: false,
                }],
                cards: Vec::new(),
            }),
            now,
        );
        let elapsed = crate::pods::reports::REPORT_TTL_MS + 1;
        let later = Clock::new(now.mono.0 + elapsed, now.unix.0 + elapsed);
        daemon.step(crate::step::Input::Tick, later);
        // 2026-10-12: the pods send a source report when it changes; on a live link it stands.
        assert!(daemon.accessory.source.as_fresh().is_some());
        let status = daemon.status_value(later);
        assert_eq!(status["native_handoff"]["audio_active"], true);
        assert_eq!(status["audio_owner"]["holder"], "local");
        assert_eq!(status["audio_owner"]["playing"], true);
        assert_eq!(status["roster_audio_owner"], local.display());

        daemon.audio.update_player("org.mpris.MediaPlayer2.brave", Some(false));
        assert_eq!(daemon.status_value(later)["audio_owner"]["playing"], false);
        daemon.note_source(false, 2, later.mono);
        daemon.note_remote(addr(4), later.mono);
        assert_eq!(daemon.status_value(later)["audio_owner"]["holder"], "remote");
        daemon.note_socket(false, daemon.connection.generation(), now.mono);
        assert_eq!(daemon.status_value(later)["audio_owner"]["holder"], "");
    }

    #[test]
    fn docking_in_an_open_case_quiets_playback_without_losing_the_owner() {
        for local in [true, false] {
            let mut daemon = Daemon::new("xps");
            let now = clock();
            daemon.set_local_address(Some(addr(2)));
            ready_link(&mut daemon, now);
            let worn = Pod {
                battery: Battery::new(80, false).unwrap(),
                in_case: Some(false),
                in_ear: Some(true),
            };
            daemon.note_pod(false, worn, now.mono);
            daemon.note_pod(true, worn, now.mono);
            daemon.note_lid(true, now.mono);
            daemon.note_ownership(local, now.mono);
            daemon.note_source(local, 2, now.mono);
            daemon.note_remote(addr(4), now.mono);
            let holder = if local { "local" } else { "remote" };
            assert_eq!(daemon.status_value(now)["audio_owner"]["playing"], true);
            let docked = Pod {
                battery: Battery::new(80, false).unwrap(),
                in_case: Some(true),
                in_ear: Some(false),
            };
            daemon.note_pod(false, docked, now.mono);
            assert_eq!(
                daemon.status_value(now)["audio_owner"]["playing"],
                true,
                "one docked bud does not rule out playback through the other"
            );
            daemon.effects.announce_dirty = false;
            daemon.note_pod(true, docked, now.mono);
            let status = daemon.status_value(now);
            assert_eq!(status["audio_owner"]["holder"], holder);
            assert_eq!(status["audio_owner"]["playing"], false);
            assert_eq!(status["audio_source_type"], 2, "retain the actual accessory report");
            assert_eq!(daemon.connection.stage(), Stage::Ready);
            assert!(daemon.audio_roster(now).unwrap().audio_idle);
            assert!(daemon.effects.announce_dirty, "tell the fleet when the buds are docked");
            daemon.note_pod(
                true,
                Pod { in_case: Some(false), in_ear: Some(true), ..docked },
                now.mono,
            );
            daemon.note_source(local, 2, now.mono);
            assert_eq!(daemon.status_value(now)["audio_owner"]["playing"], true);
            assert!(!daemon.audio_roster(now).unwrap().audio_idle);
        }
    }

    #[test]
    fn a_desired_noise_mode_is_not_published_as_confirmed() {
        let mut daemon = Daemon::new("gram");
        daemon.set_identity("Pods", "A3064");
        daemon.handle(Command::Noise(NoiseMode::Off), clock());
        daemon.handle(Command::Noise(NoiseMode::Anc), clock());
        assert_eq!(daemon.status_value(clock())["noise_mode"], -1);
        assert!(!daemon.accessory().model.supports_noise_off());
        daemon.note_noise_report(NoiseMode::Anc, MonoMs(1_000));
        assert_eq!(daemon.status_value(clock())["noise_mode"], 1);
    }

    #[test]
    fn a_noise_change_is_sent_when_the_link_becomes_ready() {
        use crate::step::{Effect, Input};

        let mut daemon = Daemon::new("gram");
        daemon.select_accessory(Some(addr(3)));
        let now = clock();
        let early = daemon.step(Input::Command(Command::Noise(NoiseMode::Anc)), now);
        assert!(early.effects.iter().all(|effect| !matches!(effect, Effect::WriteControl { .. })));

        daemon.step(Input::Command(Command::Connect), now);
        let generation = daemon.connection().generation();
        daemon.step(Input::ControlSocket { up: true, generation }, now);
        daemon
            .step(Input::Accessory { generation, packet: vec![0x01, 0x00, 0x04, 0x00, 0x01] }, now);
        daemon.step(
            Input::Accessory { generation, packet: vec![0x04, 0x00, 0x04, 0x00, 0x2b, 0x00, 0x01] },
            now,
        );
        let mut battery = vec![0x04, 0x00, 0x04, 0x00, 0x04, 0x00, 0x03];
        battery.extend_from_slice(&[0x04, 0x01, 80, 0x02, 0x01]);
        battery.extend_from_slice(&[0x02, 0x01, 60, 0x01, 0x01]);
        battery.extend_from_slice(&[0x08, 0x01, 40, 0x02, 0x01]);
        daemon.step(Input::Accessory { generation, packet: battery }, now);
        let ready = daemon.step(
            Input::Accessory {
                generation,
                packet: vec![0x04, 0x00, 0x04, 0x00, 0x06, 0x00, 0x00, 0x02],
            },
            now,
        );
        assert_eq!(daemon.connection().stage(), Stage::Ready);
        let noise = crate::pods::setup::noise_mode(NoiseMode::Anc);
        assert!(ready.effects.iter().any(|effect| {
            matches!(effect, Effect::WriteControl { bytes, .. } if bytes == &noise)
        }));
    }

    #[test]
    fn an_existing_fleet_can_invite_an_unknown_peer_without_enrolling_it_yet() {
        let mut daemon = Daemon::new("xps");
        daemon.set_local_address(Some(addr(1)));
        let original = Fleet {
            id: FleetId::new([3; 16]),
            key: GroupKey::new([4; 32]),
            members: vec![Member { address: addr(1), label: "xps".into() }],
        };
        daemon.restore_fleet(original.clone());
        let reply = daemon.handle(
            Command::Fleet(Ok(FleetCommand::Invite { address: addr(2), name: "gram".into() })),
            clock(),
        );
        assert_eq!(reply.body.as_deref(), Some("ok\n"));
        assert_eq!(daemon.saved_fleet(), Some(original));
        assert!(!daemon.membership.contains(addr(2)));
        assert_eq!(daemon.take_letters().len(), 1);
    }

    #[test]
    fn enrollment_persists_both_members_only_after_the_commit_point() {
        let mut daemon = Daemon::new("gram");
        daemon.set_local_address(Some(addr(1)));
        let id = InvitationId::new([7; 16]);
        assert!(daemon.invite(addr(2), "xps", Code::parse("012345").unwrap(), id, clock()));
        assert!(daemon.deliver_invite(id));
        let status = daemon.status_value(clock());
        assert_eq!(status["fleet"]["phase"], "inviting");
        assert_eq!(status["fleet"]["code"], "012345");
        assert!(daemon.auth_accepted(
            id,
            Proposal {
                id: FleetId::new([3; 16]),
                key: GroupKey::new([0x11; 32]),
                members: vec![
                    Member { address: addr(1), label: "gram".into() },
                    Member { address: addr(2), label: "xps".into() },
                ],
            },
        ));
        assert!(!daemon.confirm_enrollment(id));
        assert!(matches!(daemon.membership().store(), Store::Unloaded));
        daemon.persist_enrollment(id).unwrap();
        match daemon.membership().store() {
            Store::Open(fleet) => assert_eq!(fleet.members.len(), 2),
            other => panic!("{other:?}"),
        }
        assert_ne!(daemon.enrollment().phase(), crate::fleet::enroll::Phase::Success);
        assert!(daemon.confirm_enrollment(id));
        let listed = daemon.status_value(clock())["devices"].as_array().unwrap().len();
        assert_eq!(listed, 2);
        assert!(!daemon.status_line(clock()).contains("11111111"));
    }

    #[test]
    fn cancel_then_the_other_direction_does_not_keep_an_empty_fleet() {
        let mut daemon = Daemon::new("gram");
        daemon.set_local_address(Some(addr(1)));
        let id = InvitationId::new([1; 16]);
        assert!(daemon.invite(addr(2), "xps", Code::parse("000111").unwrap(), id, clock()));
        assert!(
            daemon
                .handle(Command::Fleet(Ok(FleetCommand::Cancel { id })), clock(),)
                .body
                .as_deref()
                == Some("ok\n")
        );
        assert!(matches!(daemon.membership().store(), Store::Unloaded));
        assert!(daemon.receive_invite(addr(2), "xps", InvitationId::new([2; 16]), clock()));
        assert_eq!(daemon.enrollment().phase(), crate::fleet::enroll::Phase::WaitingOffer);
    }

    pub(super) fn play(token: &str) -> Command {
        Command::Playback(Ok(crate::types::PlaybackEvent {
            kind: crate::types::PlayKind::Play,
            application: "Browser".into(),
            time: UnixMs(1_700_000_000_000),
            player: "tab".into(),
            token: token.into(),
            playing: true,
        }))
    }

    #[test]
    fn bluez_up_is_not_a_ready_control_socket() {
        let mut daemon = Daemon::new("gram");
        let outcome = daemon.step(crate::step::Input::Bluez(true), clock());
        assert!(outcome.effects.is_empty());
        assert_eq!(daemon.connection().bluez(), Some(true));
        assert_eq!(daemon.connection().socket_up(), None);
        assert_eq!(daemon.connection().stage(), Stage::Idle);
        assert_eq!(daemon.status_value(clock())["bluez_connected"], true);
        assert_eq!(daemon.status_value(clock())["connected"], false);
    }

    #[test]
    fn a_play_does_not_write_ownership_and_a_remote_report_after_it_wins() {
        let now = clock();
        let mut earlier = Daemon::new("gram");
        earlier.set_identity("AirPods", "A3064");
        earlier.select_accessory(Some(addr(9)));
        earlier.note_ownership(false, now.mono);
        earlier.handle(play("earlier"), now);
        assert_eq!(earlier.accessory().ownership.as_fresh(), Some(&false));
        assert!(earlier.wants_local_confirm(now.mono));

        let mut later = Daemon::new("gram");

        later.set_identity("AirPods", "A3064");
        later.select_accessory(Some(addr(9)));
        later.handle(play("later"), now);
        later.note_ownership(false, now.mono);
        assert_eq!(later.accessory().ownership.as_fresh(), Some(&false));
        assert!(!later.wants_local_confirm(now.mono));
    }

    #[test]
    fn a_zero_battery_is_present_and_a_missing_one_is_not() {
        let mut daemon = Daemon::new("gram");
        daemon.note_pod(
            false,
            Pod {
                battery: Battery::new(0, false).unwrap(),
                in_ear: Some(true),
                in_case: Some(false),
            },
            MonoMs(1_000),
        );
        let status = daemon.status_value(clock());
        assert_eq!(status["left"]["level"], 0);
        assert_eq!(status["left"]["available"], true);
        assert!(status.get("right").is_none());
    }

    #[test]
    fn a_letter_takes_the_air_after_one_public_presence_slot() {
        use crate::fleet::letter::{Letter, Outbound};
        use crate::step::{Effect, Input};

        let mut daemon = Daemon::new("gram");
        daemon.set_local_address(Some(addr(1)));
        let presence = daemon.step(Input::Tick, clock());
        let frame = advertised(&presence.effects);
        let sight = crate::fleet::presence::observe(frame, None, clock().unix, None).unwrap();
        assert_eq!(sight.address, addr(1));
        assert_eq!(sight.name, "gram");
        assert_eq!(sight.enrolled, Some(false));
        assert!(presence.effects.iter().any(|effect| matches!(effect, Effect::Announce(_))));

        let dwell = daemon.step(Input::Tick, clock());
        assert!(dwell
            .effects
            .iter()
            .all(|effect| !matches!(effect, Effect::Advertise(_) | Effect::ReleaseAir)));

        let released = daemon.step(Input::Tick, Clock::new(1_500, 1_700_000_000_500));
        assert!(released.effects.iter().any(|effect| matches!(effect, Effect::ReleaseAir)));

        daemon
            .outbox
            .push(Outbound { to: addr(2), body: Letter::Ack { id: InvitationId::new([7; 16]) } });
        let sent = daemon.step(Input::Tick, Clock::new(1_500, 1_700_000_000_500));
        let bytes = advertised(&sent.effects);
        assert!(bytes.len() > crate::fleet::radio::HEADER);
        assert_eq!(bytes[0], 1);
        assert_eq!(&bytes[1..7], &addr(1).bytes());
        assert_eq!(&bytes[7..13], &addr(2).bytes());
        assert_eq!(bytes[21], 0);
        assert_eq!(bytes[22], 1);
        let first = bytes.to_vec();
        assert!(sent
            .effects
            .iter()
            .any(|effect| matches!(effect, Effect::Send { to, .. } if *to == addr(2))));

        // Like the previous daemon, the finished packet cycles again instead
        // of leaving the air after one pass.
        let next = daemon.step(Input::Tick, Clock::new(2_000, 1_700_000_001_000));
        assert_eq!(advertised(&next.effects), first.as_slice());
        let repeated = daemon.step(Input::Tick, Clock::new(2_500, 1_700_000_001_500));
        assert_eq!(advertised(&repeated.effects), first.as_slice());
    }

    #[test]
    fn a_public_presence_is_a_nearby_host_and_a_member_replay_is_not() {
        use crate::fleet::members::{Fleet, GroupKey, Member};
        use crate::fleet::presence;
        use crate::step::Input;
        use crate::types::{FleetId, UnixMs};

        let now = clock();
        let frame = presence::advertise(addr(2), "xps", now.unix).unwrap();
        let mut daemon = Daemon::new("gram");
        daemon.set_local_address(Some(addr(1)));
        daemon.step(Input::Advertisement(frame.clone()), now);
        let devices = daemon.status_value(now)["devices"].clone();
        assert!(devices.as_array().unwrap().iter().any(|row| {
            row["address"] == "02:02:03:04:05:06"
                && row["label"] == "xps"
                && row["discovered"] == true
        }));
        daemon.step(Input::Advertisement(frame.clone()), now);
        assert_eq!(daemon.status_value(now)["devices"].as_array().unwrap().len(), 1);

        let stale = presence::advertise(addr(3), "old", UnixMs(now.unix.0 - 30_001)).unwrap();
        daemon.step(Input::Advertisement(stale), now);
        assert_eq!(daemon.status_value(now)["devices"].as_array().unwrap().len(), 1);

        let mut member = Daemon::new("gram");
        member.restore_fleet(Fleet {
            id: FleetId::new([3; 16]),
            key: GroupKey::new([0x11; 32]),
            members: vec![Member { address: addr(2), label: "xps".into() }],
        });
        member.step(Input::Advertisement(frame), now);
        let status = member.status_value(now);
        let rows = status["devices"].as_array().unwrap();
        assert!(rows.iter().all(|row| row["discovered"] == false && row["nearby"] == false));
        let sealed_member =
            presence::advertise_sealed(addr(2), "xps", now.unix, &keys_for_test(), None).unwrap();
        member.step(Input::Advertisement(sealed_member), now);
        let status = member.status_value(now);
        let rows = status["devices"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["discovered"], false);
        assert_eq!(rows[0]["nearby"], true);

        let mut enrolled = Daemon::new("gram");
        enrolled.set_local_address(Some(addr(1)));
        enrolled.restore_fleet(Fleet {
            id: FleetId::new([3; 16]),
            key: GroupKey::new([0x11; 32]),
            members: vec![Member { address: addr(2), label: "xps".into() }],
        });
        let quiet = enrolled.step(crate::step::Input::Tick, now);
        let frame = advertised(&quiet.effects);
        assert_eq!(frame[0], crate::fleet::presence::SEALED_SESSION);
        let keys = keys_for_test();
        let sight = presence::observe(frame, Some(addr(2)), now.unix, Some(&keys)).unwrap();
        assert!(sight.authenticated);
        assert!(sight.session.is_some());
        assert_eq!(sight.name, "gram");
        assert!(quiet.effects.iter().any(|effect| matches!(effect, crate::step::Effect::Listen)));

        let sealed = presence::advertise_sealed(addr(4), "phone", now.unix, &keys, None).unwrap();
        enrolled.step(crate::step::Input::Advertisement(sealed), now);
        let status = enrolled.status_value(now);
        let rows = status["devices"].as_array().unwrap();
        assert!(rows.iter().any(|row| {
            row["address"] == "04:02:03:04:05:06"
                && row["label"] == "phone"
                && row["discovered"] == true
        }));
    }

    #[test]
    fn a_saved_peer_advertising_unjoined_returns_to_discovery_without_erasing_membership() {
        use crate::fleet::members::{Fleet, GroupKey, Member};
        use crate::fleet::presence;
        use crate::step::Input;
        use crate::types::FleetId;

        let now = clock();
        let mut daemon = Daemon::new("gram");
        daemon.set_local_address(Some(addr(1)));
        daemon.restore_fleet(Fleet {
            id: FleetId::new([3; 16]),
            key: GroupKey::new([0x11; 32]),
            members: vec![Member { address: addr(2), label: "xps".into() }],
        });
        ready_link(&mut daemon, now);
        daemon.note_pod_peers(vec![addr(1), addr(2)], now.unix);
        let unjoined = presence::advertise_setup(addr(2), "xps", now.unix, Some(false)).unwrap();
        daemon.step(Input::Advertisement(unjoined), now);
        let status = daemon.status_value(now);
        let row = status["devices"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["address"] == addr(2).display())
            .unwrap();
        assert_eq!(row["discovered"], true);
        assert_eq!(row["fleet_enrolled"], false);
        assert_eq!(row["nearby"], true);
        assert!(status["connected_peers"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!(addr(2).display())));
        assert!(daemon.membership.contains(addr(2)));

        // Authenticated presence restores the member presentation. A newer
        // public hint cannot displace that evidence while it remains fresh.
        let sealed =
            presence::advertise_sealed(addr(2), "xps", now.unix, &keys_for_test(), None).unwrap();
        daemon.step(Input::Advertisement(sealed), now);
        let later = Clock::new(now.mono.0 + 1, now.unix.0 + 1);
        let unjoined = presence::advertise_setup(addr(2), "xps", later.unix, Some(false)).unwrap();
        daemon.step(Input::Advertisement(unjoined), later);
        let status = daemon.status_value(later);
        let row = status["devices"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["address"] == addr(2).display())
            .unwrap();
        assert_eq!(row["discovered"], false);
        assert_eq!(row["nearby"], true);

        let expired = Clock::new(now.mono.0 + 30_001, now.unix.0 + 30_001);
        daemon.step(Input::Tick, expired);
        let unjoined =
            presence::advertise_setup(addr(2), "xps", expired.unix, Some(false)).unwrap();
        daemon.step(Input::Advertisement(unjoined), expired);
        let status = daemon.status_value(expired);
        let row = status["devices"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["address"] == addr(2).display())
            .unwrap();
        assert_eq!(row["discovered"], true);
        assert_eq!(row["fleet_enrolled"], false);
        assert!(daemon.membership.contains(addr(2)));
    }

    #[test]
    fn a_fleet_letter_from_an_unenrolled_peer_is_a_nearby_sighting() {
        use crate::fleet::cppmsg;
        use crate::fleet::letter::Letter;
        use crate::step::Input;
        use crate::types::InvitationId;

        let now = clock();
        let mut daemon = Daemon::new("xps");
        daemon.set_local_address(Some(addr(1)));
        // The previous daemon's acknowledgement, inside a version-1 fragment.
        let frame = cppmsg::encode(
            addr(2),
            addr(1),
            "lg-gram",
            &Letter::Ack { id: InvitationId::new([7; 16]) },
            None,
        )
        .unwrap();
        for part in crate::fleet::radio::fragments(&frame, addr(2), addr(1), [9; 8]) {
            daemon.step(Input::Advertisement(part), now);
        }
        let rows = daemon.status_value(now)["devices"].clone();
        assert!(
            rows.as_array().unwrap().iter().any(|row| {
                row["address"] == "02:02:03:04:05:06"
                    && row["label"] == "lg-gram"
                    && row["kind"] == "omarchy"
                    && row["discovered"] == true
            }),
            "devices: {rows}"
        );
        // The same packet keeps arriving (and is deduped as a letter); its
        // fragments alone keep the peer in the tray past the 30 s window.
        let later = Clock::new(now.mono.0 + 31_000, now.unix.0 + 31_000);
        for part in crate::fleet::radio::fragments(&frame, addr(2), addr(1), [9; 8]) {
            daemon.step(Input::Advertisement(part), later);
        }
        let rows = daemon.status_value(later)["devices"].clone();
        assert!(
            rows.as_array()
                .unwrap()
                .iter()
                .any(|row| { row["address"] == "02:02:03:04:05:06" && row["discovered"] == true }),
            "devices after refresh: {rows}"
        );
    }

    #[test]
    fn enrolled_presence_is_sealed_only_and_invisible_without_the_fleet_key() {
        use crate::fleet::presence;
        use crate::step::Input;
        let fleet = Fleet {
            id: FleetId::new([3; 16]),
            key: GroupKey::new([0x11; 32]),
            members: vec![
                Member { address: addr(1), label: "xps".into() },
                Member { address: addr(2), label: "gram".into() },
            ],
        };
        let mut sender = Daemon::new("xps");
        sender.set_local_address(Some(addr(1)));
        sender.restore_fleet(fleet.clone());
        let mut unenrolled = Daemon::new("gram");
        unenrolled.set_local_address(Some(addr(2)));
        let mut member = Daemon::new("gram");
        member.set_local_address(Some(addr(2)));
        member.restore_fleet(fleet);
        let mut public = 0;
        let mut private = 0;
        for elapsed in (0..=20_000).step_by(500) {
            let now = Clock::new(clock().mono.0 + elapsed, clock().unix.0 + elapsed);
            let outcome = sender.step(Input::Tick, now);
            for effect in outcome.effects {
                if let crate::step::Effect::Advertise(bytes) = effect {
                    if presence::is_public(&bytes) {
                        public += 1;
                    }
                    if bytes[0] == presence::SEALED_SESSION {
                        private += 1;
                    }
                    unenrolled.step(Input::Advertisement(bytes.clone()), now);
                    member.step(Input::Advertisement(bytes), now);
                }
            }
        }
        assert_eq!(public, 0);
        assert!(private > 0);
        // Without the fleet key the sender does not exist; sighting it
        // sealed still grants the member listener no new membership.
        assert!(unenrolled.saved_fleet().is_none());
        assert!(unenrolled.discovery.hosts().iter().all(|host| host.address != addr(1)));
        let nearby = member.discovery.hosts().iter().find(|host| host.address == addr(1)).unwrap();
        assert_eq!(nearby.name, "xps");
        assert!(nearby.authenticated);
        assert!(nearby.session.is_some());
    }

    #[test]
    fn a_handoff_recovers_lost_presence_and_confirmation_without_the_idle_cycle() {
        use crate::fleet::presence;
        use crate::fleet::route::{Admit, Phase, Start};
        use crate::fleet::route_air::{self, Kind};
        use crate::step::{Effect, Input};

        let initial = clock();
        let keys = keys_for_test();
        let fleet = Fleet {
            id: FleetId::new(keys.id),
            key: GroupKey::new(keys.key),
            members: vec![
                Member { address: addr(1), label: "gram".into() },
                Member { address: addr(2), label: "xps".into() },
            ],
        };
        let mut gram = Daemon::new("gram");
        let mut xps = Daemon::new("xps");
        for (daemon, address) in [(&mut gram, addr(1)), (&mut xps, addr(2))] {
            daemon.set_local_address(Some(address));
            daemon.select_accessory(Some(addr(9)));
            daemon.restore_fleet(fleet.clone());
        }
        let gram_presence = advertised(&gram.step(Input::Tick, initial).effects).to_vec();
        let xps_presence = advertised(&xps.step(Input::Tick, initial).effects).to_vec();
        gram.hear_presence(gram.read_presence(&xps_presence, initial.unix).unwrap());
        xps.hear_presence(xps.read_presence(&gram_presence, initial.unix).unwrap());

        // Reproduce the point after admission: XPS is physically ready, but
        // Gram no longer has XPS's presence record when confirmation arrives.
        let Start::Sent { tx } = xps.start_route(addr(1)) else { panic!("start") };
        assert_eq!(xps.routing.admit(tx, true, Some(addr(4))), Admit::Beside);
        assert!(gram.routing.accept_request(addr(2), tx));
        assert!(gram.routing.grant(tx, Some(addr(4))));
        ready(&mut xps, initial.mono);
        gram.discovery.forget(addr(2));

        let mut lost_presence = false;
        let mut lost_confirmation = false;
        let mut rejected_connection = false;
        let mut completed_at = None;
        for elapsed in (500..=15_000).step_by(100) {
            let now = Clock::new(initial.mono.0 + elapsed, initial.unix.0 + elapsed);
            let mut air = std::collections::VecDeque::new();
            for (from_xps, outcome) in
                [(true, xps.step(Input::Tick, now)), (false, gram.step(Input::Tick, now))]
            {
                for effect in outcome.effects {
                    if let Effect::Advertise(bytes) = effect {
                        air.push_back((from_xps, bytes));
                    }
                }
            }
            while let Some((from_xps, bytes)) = air.pop_front() {
                if from_xps && bytes[0] == presence::SEALED_SESSION && !lost_presence {
                    lost_presence = true;
                    continue;
                }
                let message = route_air::read(&bytes, &keys);
                if !from_xps
                    && message.as_ref().is_some_and(|m| m.kind == Kind::Complete)
                    && !lost_confirmation
                {
                    // Gram has finished, but XPS must relearn Gram's presence
                    // before accepting a repeated Complete. The helper must
                    // keep refreshing presence even after its local success.
                    xps.discovery.forget(addr(1));
                    lost_confirmation = true;
                    continue;
                }
                let receiver = if from_xps { &mut gram } else { &mut xps };
                let unknown_connection = from_xps
                    && message.as_ref().is_some_and(|m| m.kind == Kind::Connection)
                    && receiver.discovery.hosts().is_empty();
                let outcome = receiver.step(Input::Advertisement(bytes), now);
                if unknown_connection {
                    assert_eq!(receiver.routing.phase(), Phase::WaitingConnection);
                    rejected_connection = true;
                }
                for effect in outcome.effects {
                    if let Effect::Advertise(reply) = effect {
                        air.push_back((!from_xps, reply));
                    }
                }
            }
            if gram.routing.phase() == Phase::Success && xps.routing.phase() == Phase::Success {
                completed_at = Some(elapsed);
                break;
            }
        }
        assert!(lost_presence && lost_confirmation && rejected_connection);
        assert!(completed_at.is_some(), "handoff waited for idle presence refresh");

        // Fast private refresh is bounded by the confirmation packet lifetime;
        // enrolled hosts keep airing sealed presence afterward and never
        // resume public discovery.
        for daemon in [&mut gram, &mut xps] {
            let mut public = false;
            let mut private = false;
            let idle_at = crate::fleet::route_air::LIFETIME_MS + 10_000;
            for elapsed in (idle_at..=idle_at + 10_000).step_by(500) {
                let now = Clock::new(initial.mono.0 + elapsed, initial.unix.0 + elapsed);
                for effect in daemon.step(Input::Tick, now).effects {
                    if let Effect::Advertise(bytes) = effect {
                        public |= presence::is_public(&bytes);
                        private |= bytes[0] == presence::SEALED_SESSION;
                    }
                }
            }
            assert!(!public, "enrolled presence must stay sealed after a handoff");
            assert!(private, "sealed presence must continue after a handoff");
        }
    }

    #[test]
    fn a_new_fleet_reseals_on_the_next_slot() {
        use crate::fleet::members::{Fleet, GroupKey, Member};
        use crate::fleet::presence::{self, FleetKeys};
        use crate::step::Input;

        let mut daemon = Daemon::new("gram");
        daemon.set_local_address(Some(addr(1)));
        daemon.restore_fleet(Fleet {
            id: FleetId::new([3; 16]),
            key: GroupKey::new([0x11; 32]),
            members: vec![Member { address: addr(1), label: "gram".into() }],
        });
        let first = daemon.step(Input::Tick, clock());
        let frame = advertised(&first.effects);
        let old = FleetKeys { id: [3; 16], key: [0x11; 32] };
        assert!(presence::observe(frame, Some(addr(2)), clock().unix, Some(&old)).is_some());

        assert!(daemon.membership.mark_confirmed_empty());
        daemon.restore_fleet(Fleet {
            id: FleetId::new([4; 16]),
            key: GroupKey::new([0x22; 32]),
            members: vec![Member { address: addr(1), label: "gram".into() }],
        });
        let second = daemon.step(Input::Tick, Clock::new(1_500, 1_700_000_000_500));
        let frame = advertised(&second.effects);
        let new = FleetKeys { id: [4; 16], key: [0x22; 32] };
        assert!(presence::observe(frame, Some(addr(2)), clock().unix, Some(&new)).is_some());
        assert!(presence::observe(frame, Some(addr(2)), clock().unix, Some(&old)).is_none());
    }

    #[test]
    fn a_roster_advertisement_is_aired_and_a_replay_does_not_replace_it() {
        use crate::fleet::members::{Fleet, GroupKey, Member};
        use crate::fleet::presence::{self, FleetKeys};
        use crate::fleet::roster_air::{self, Snapshot};
        use crate::step::{Effect, Input};

        let now = clock();
        let pods = Address::parse("A0:00:00:00:00:03").unwrap();
        let keys = FleetKeys { id: [3; 16], key: [0x11; 32] };
        let mut daemon = Daemon::new("gram");
        daemon.set_local_address(Some(addr(1)));
        daemon.select_accessory(Some(pods));
        daemon.restore_fleet(Fleet {
            id: FleetId::new([3; 16]),
            key: GroupKey::new([0x11; 32]),
            members: vec![
                Member { address: addr(1), label: "gram".into() },
                Member { address: addr(2), label: "xps".into() },
            ],
        });
        let first = daemon.step(Input::Tick, now);
        let frame = advertised(&first.effects);
        assert_eq!(frame[0], presence::SEALED_SESSION);
        let ours = presence::observe(frame, Some(addr(2)), now.unix, Some(&keys)).unwrap();
        let second = daemon.step(Input::Tick, Clock::new(1_500, now.unix.0 + 500));
        let roster_frame = advertised(&second.effects);
        assert_eq!(roster_frame[0], roster_air::VERSION);
        let aired = roster_air::read(roster_frame, &keys).unwrap();
        assert_eq!(aired.reporter, addr(1));
        assert_eq!(aired.sequence, 1);
        assert_eq!(aired.session, ours.session.unwrap());
        assert!(aired.peers.is_empty());
        assert_eq!(aired.last_use, UnixMs(0));

        let session = [9u8; 16];
        let seen =
            presence::advertise_sealed(addr(2), "xps", now.unix, &keys, Some(session)).unwrap();
        daemon.step(Input::Advertisement(seen), now);
        let snapshot = Snapshot {
            reporter: addr(2),
            session,
            sequence: 3,
            sent: now.unix,
            accessory: pods,
            peers: vec![addr(2)],
            audio_source: Some(addr(2)),
            last_use: now.unix,
            pending: false,
            audio_idle: true,
            intent: false,
        };
        let packet = roster_air::advertise(&snapshot, &keys).unwrap();
        let stored = daemon.step(Input::Advertisement(packet.clone()), now);
        let saved = stored.effects.iter().find_map(|effect| match effect {
            Effect::StoreWatermark(bytes) => Some(bytes.clone()),
            _ => None,
        });
        let Some(saved) = saved else {
            panic!("the watermark should be saved");
        };
        assert_eq!(daemon.status_value(now)["fleet"]["roster"]["reporter"], "02:02:03:04:05:06");
        let replay = daemon.step(Input::Advertisement(packet.clone()), now);
        assert!(replay.effects.iter().all(|effect| !matches!(effect, Effect::StoreWatermark(_))));

        let mut restarted = Daemon::new("other");
        restarted.import_roster_watermark(&saved);
        restarted.set_local_address(Some(addr(1)));
        restarted.select_accessory(Some(pods));
        restarted.restore_fleet(Fleet {
            id: FleetId::new([3; 16]),
            key: GroupKey::new([0x11; 32]),
            members: vec![
                Member { address: addr(1), label: "gram".into() },
                Member { address: addr(2), label: "xps".into() },
            ],
        });
        let again =
            presence::advertise_sealed(addr(2), "xps", now.unix, &keys, Some(session)).unwrap();
        restarted.step(Input::Advertisement(again), now);
        let rejected = restarted.step(Input::Advertisement(packet), now);
        assert!(rejected.effects.iter().all(|effect| !matches!(effect, Effect::StoreWatermark(_))));
        assert!(restarted.status_value(now)["fleet"]["roster"].as_object().unwrap().is_empty());
    }

    #[test]
    fn a_plaintext_announcement_cannot_supply_a_roster_or_seat() {
        use crate::fleet::members::{Fleet, GroupKey, Member};
        use crate::fleet::postcard;
        use crate::step::Input;

        let now = clock();
        let pods = Address::parse("A0:00:00:00:00:03").unwrap();
        let mut daemon = Daemon::new("xps");
        daemon.set_local_address(Some(addr(1)));
        daemon.select_accessory(Some(pods));
        daemon.restore_fleet(Fleet {
            id: FleetId::new([3; 16]),
            key: GroupKey::new([0x11; 32]),
            members: vec![
                Member { address: addr(1), label: "xps".into() },
                Member { address: addr(2), label: "gram".into() },
            ],
        });
        let forged = RosterReport {
            reporter: host_id(Some(addr(2))),
            reporter_address: addr(2),
            accessory: pods,
            session: crate::types::SessionId(99),
            seq: 999,
            at: crate::types::MonoMs(u64::MAX / 2),
            sent: now.unix,
            peers: vec![addr(2)],
            audio_source: Some(addr(2)),
            pending: false,
            audio_idle: false,
            intent: false,
            wire_session: None,
            last_use: UnixMs(0),
        };
        let frame = postcard::encode_announce(addr(2), "gram", true, Some(&forged));
        daemon.step(Input::Radio(frame), now);
        assert!(daemon.peers.fresh(now, pods).is_none());
        assert!(daemon.status_value(now)["fleet"]["roster"].as_object().unwrap().is_empty());
        assert!(!daemon.peers_seated.contains(&addr(2)));
        daemon.peers_seated.push(addr(2));
        let frame = postcard::encode_announce(addr(2), "gram", false, None);
        daemon.step(Input::Radio(frame), now);
        assert!(daemon.peers_seated.contains(&addr(2)));
    }

    #[test]
    fn a_plaintext_letter_cannot_confirm_or_grant_an_enrollment() {
        use crate::fleet::enroll::Phase;
        use crate::step::Input;

        let now = clock();
        let mut daemon = Daemon::new("gram");
        daemon.set_local_address(Some(addr(1)));
        let id = InvitationId::new([7; 16]);
        assert!(daemon.receive_invite(addr(2), "xps", id, now));
        daemon.take_letters();
        // A forged grant would hand us an attacker fleet key; a forged done
        // would mark the enrollment confirmed. Neither may move anything.
        for body in [
            format!(
                "{{\"t\":\"grant\",\"id\":\"{}\",\"confirm\":\"{}\",\"proposal\":{{\"id\":\"{}\",\"key\":\"{}\",\"members\":[{{\"address\":\"{}\",\"label\":\"xps\"}}]}}}}",
                id.hex(),
                "11".repeat(32),
                "22".repeat(16),
                "33".repeat(32),
                addr(2).display(),
            ),
            format!("{{\"t\":\"done\",\"id\":\"{}\"}}", id.hex()),
            format!("{{\"t\":\"commit\",\"id\":\"{}\"}}", id.hex()),
        ] {
            let frame = format!(
                "pf1\n{{\"k\":\"letter\",\"from\":\"{}\",\"to\":\"{}\",\"body\":{body}}}",
                addr(2).display(),
                addr(1).display(),
            );
            let result = daemon.step(Input::Radio(frame.into_bytes()), now);
            assert_eq!(daemon.enrollment.phase(), Phase::WaitingOffer);
            assert!(daemon.saved_fleet().is_none());
            assert!(!result
                .effects
                .iter()
                .any(|effect| matches!(effect, crate::step::Effect::WriteKeyring(_))));
        }
    }

    #[test]
    fn a_withdrawal_clears_the_roster_the_other_laptop_learned() {
        use crate::fleet::members::{Fleet, GroupKey, Member};
        use crate::fleet::presence::{self, FleetKeys};
        use crate::fleet::roster_air::{self, Snapshot};
        use crate::step::Input;

        let now = clock();
        let pods = Address::parse("A0:00:00:00:00:03").unwrap();
        let phone = Address::parse("28:02:2E:BF:7F:7D").unwrap();
        let keys = FleetKeys { id: [3; 16], key: [0x11; 32] };
        let session = [9u8; 16];
        let mut listener = Daemon::new("omarchy-xps");
        listener.set_local_address(Some(addr(2)));
        listener.select_accessory(Some(pods));
        listener.restore_fleet(Fleet {
            id: FleetId::new([3; 16]),
            key: GroupKey::new([0x11; 32]),
            members: vec![
                Member { address: addr(2), label: "xps".into() },
                Member { address: addr(4), label: "gram".into() },
            ],
        });
        let presence =
            presence::advertise_sealed(addr(4), "gram", now.unix, &keys, Some(session)).unwrap();
        listener.step(Input::Advertisement(presence), now);
        let learned = Snapshot {
            reporter: addr(4),
            session,
            sequence: 1,
            sent: now.unix,
            accessory: pods,
            peers: vec![addr(4), phone],
            audio_source: Some(phone),
            last_use: UnixMs(now.unix.0 - 1),
            pending: false,
            audio_idle: false,
            intent: false,
        };
        listener.step(Input::Advertisement(roster_air::advertise(&learned, &keys).unwrap()), now);
        let learned_status = listener.status_value(now);
        assert_eq!(learned_status["fleet"]["roster"]["peers"].as_array().unwrap().len(), 2);

        let mut withdrawal = learned;
        withdrawal.peers.clear();
        withdrawal.audio_source = None;
        withdrawal.sequence = 2;
        withdrawal.sent = UnixMs(now.unix.0 + 1);
        let later = Clock::new(now.mono.0 + 1, now.unix.0 + 1);
        listener
            .step(Input::Advertisement(roster_air::advertise(&withdrawal, &keys).unwrap()), later);
        let roster = &listener.status_value(later)["fleet"]["roster"];
        assert!(roster["peers"].as_array().unwrap().is_empty());
        assert!(listener.status_value(later)["connected_peers"].as_array().unwrap().is_empty());
    }

    #[test]
    fn a_phone_is_not_a_roster_peer_and_last_use_stays_on_the_observation() {
        use crate::fleet::members::{Fleet, GroupKey, Member};
        use crate::fleet::presence::FleetKeys;
        use crate::fleet::roster_air;
        use crate::pods::link::{reach_stage, Claim, Stage};
        use crate::step::Input;

        let now = clock();
        let pods = Address::parse("A0:00:00:00:00:03").unwrap();
        let phone = Address::new([9, 9, 9, 9, 9, 9]).unwrap();
        let keys = FleetKeys { id: [3; 16], key: [0x11; 32] };
        let mut daemon = Daemon::new("gram");
        daemon.set_local_address(Some(addr(1)));
        daemon.select_accessory(Some(pods));
        daemon.restore_fleet(Fleet {
            id: FleetId::new([3; 16]),
            key: GroupKey::new([0x11; 32]),
            members: vec![
                Member { address: addr(1), label: "gram".into() },
                Member { address: addr(2), label: "xps".into() },
            ],
        });
        let Claim::Started(generation) = daemon.connection.claim(now.mono) else {
            panic!("claim");
        };
        daemon.note_socket(true, generation, now.mono);
        assert!(reach_stage(&mut daemon.connection, Stage::Notifications, generation, now.mono));
        assert!(daemon.connection.mark_ready(generation));
        daemon.note_source(true, 2, now.mono);
        daemon.note_remote(phone, now.mono);

        daemon.step(Input::Tick, now);
        let aired = daemon.step(Input::Tick, Clock::new(1_500, now.unix.0 + 500));
        let first = roster_air::read(advertised(&aired.effects), &keys).unwrap();
        assert_eq!(first.peers, vec![addr(1)]);
        assert_eq!(first.audio_source, Some(addr(1)));
        assert!(!first.audio_idle);
        assert_eq!(first.last_use, now.unix);

        let later = Clock::new(6_500, now.unix.0 + 5_500);
        daemon.step(Input::Tick, Clock::new(6_000, now.unix.0 + 5_000));
        let refreshed = daemon.step(Input::Tick, later);
        let second = roster_air::read(advertised(&refreshed.effects), &keys).unwrap();
        assert_eq!(second.last_use, now.unix);
        assert!(second.sent.0 > second.last_use.0);
        assert_eq!(second.peers, first.peers);
    }

    #[test]
    fn a_route_request_is_aired_and_a_busy_peer_answers() {
        use crate::fleet::members::{Fleet, GroupKey, Member};
        use crate::fleet::presence::FleetKeys;
        use crate::fleet::route::Phase;
        use crate::fleet::route_air::{self, Answer, Kind};
        use crate::step::Input;

        for (preserve_apple, idle_apple, timeout) in
            [(false, false, false), (true, false, false), (false, true, false), (false, true, true)]
        {
            let mut now = clock();
            let pods = Address::parse("A0:00:00:00:00:03").unwrap();
            let keys = FleetKeys { id: [3; 16], key: [0x11; 32] };
            let fleet = Fleet {
                id: FleetId::new([3; 16]),
                key: GroupKey::new([0x11; 32]),
                members: vec![
                    Member { address: addr(1), label: "gram".into() },
                    Member { address: addr(2), label: "xps".into() },
                ],
            };
            let mut gram = Daemon::new("gram");
            let mut xps = Daemon::new("xps");
            for (daemon, address) in [(&mut gram, addr(1)), (&mut xps, addr(2))] {
                daemon.set_local_address(Some(address));
                daemon.select_accessory(Some(pods));
                daemon.restore_fleet(fleet.clone());
            }
            let gram_tick = gram.step(Input::Tick, now);
            let xps_tick = xps.step(Input::Tick, now);
            let gram_presence = advertised(&gram_tick.effects).to_vec();
            let xps_presence = advertised(&xps_tick.effects).to_vec();
            gram.step(Input::Advertisement(xps_presence), now);
            xps.step(Input::Advertisement(gram_presence), now);
            ready(&mut xps, now.mono);
            ready(&mut gram, now.mono);
            let retained = if preserve_apple { addr(4) } else { addr(2) };
            if idle_apple {
                xps.note_pod_peers(vec![addr(2), addr(4)], now.unix);
                xps.note_remote(addr(4), now.mono);
                xps.note_source(false, 0, now.mono);
            }
            if preserve_apple {
                xps.note_pod_peers(vec![addr(2), retained], now.unix);
                xps.note_ownership(false, now.mono);
                xps.note_remote(retained, now.mono);
                xps.note_source(false, 2, now.mono);
            }

            assert!(matches!(gram.start_route(addr(2)), crate::fleet::route::Start::Sent { .. }));
            // The newly confirmed roster takes the first slot before the route.
            gram.step(Input::Tick, advance_clock(&mut now, 500));
            let sent = gram.step(Input::Tick, advance_clock(&mut now, 500));
            let request = next_route(&mut gram, &sent.effects, &mut now, &keys);
            assert_eq!(request.kind, Kind::Request);
            assert!(request.retain.is_none());

            let retry = gram.step(Input::Tick, advance_clock(&mut now, 500));
            let again = next_route(&mut gram, &retry.effects, &mut now, &keys);
            assert_eq!(again.sequence, request.sequence);
            assert_eq!(again.transaction, request.transaction);

            let mut helped = xps.step(
                Input::Advertisement(route_frame(&request, &keys)),
                advance_clock(&mut now, 500),
            );
            if idle_apple {
                let priority = crate::pods::setup::priority(addr(2), Some(addr(1))).unwrap();
                assert!(
                    helped.effects.iter().any(|effect| matches!(effect,
                        crate::step::Effect::WriteControl { bytes, .. } if bytes == &priority
                    )),
                    "a full roster must reserve the two laptops before admitting Gram"
                );
                assert!(helped
                    .effects
                    .iter()
                    .all(|effect| !matches!(effect, crate::step::Effect::Disconnect { .. })));
                assert!(
                    !helped.effects.iter().any(|effect| matches!(effect,
                        crate::step::Effect::Advertise(bytes) if route_air::read(bytes, &keys)
                            .is_some_and(|message| message.kind == Kind::Admission)
                    )),
                    "wait for priority acknowledgement before admitting the requester"
                );
                let repeated = xps.step(
                    Input::Advertisement(route_frame(&again, &keys)),
                    advance_clock(&mut now, 500),
                );
                assert!(
                    !repeated.effects.iter().any(|effect| matches!(effect,
                        crate::step::Effect::WriteControl { bytes, .. } if bytes == &priority
                    )),
                    "a repeated request must not repeat the priority write"
                );
                if timeout {
                    let expired = xps.step(Input::Tick, advance_clock(&mut now, 5_000));
                    let refused = next_route(&mut xps, &expired.effects, &mut now, &keys);
                    assert_eq!(refused.result, Answer::TimedOut);
                    assert_eq!(xps.routing.phase(), Phase::Failed);
                    assert!(!expired
                        .effects
                        .iter()
                        .any(|effect| matches!(effect, crate::step::Effect::Disconnect { .. })));
                    xps.step(
                        Input::Accessory {
                            generation: xps.connection.generation(),
                            packet: vec![4, 0, 4, 0, 0x40, 0, 0x14, 0, 0, 0, 0, 0],
                        },
                        advance_clock(&mut now, 500),
                    );
                    assert_eq!(xps.routing.phase(), Phase::Failed);
                    continue;
                }
                helped = xps.step(
                    Input::Accessory {
                        generation: xps.connection.generation(),
                        packet: vec![4, 0, 4, 0, 0x40, 0, 0x14, 0, 0, 0, 0, 0],
                    },
                    advance_clock(&mut now, 500),
                );
            }
            if preserve_apple {
                helped = xps.step(
                    Input::LinkReleased {
                        accessory: pods,
                        generation: xps.connection.generation(),
                        ok: true,
                    },
                    advance_clock(&mut now, 2_000),
                );
            } else if !idle_apple {
                helped = xps.step(Input::Tick, advance_clock(&mut now, 500));
            }
            assert_eq!(xps.routing.phase(), Phase::WaitingConnection);
            let admission = next_route(&mut xps, &helped.effects, &mut now, &keys);
            assert_eq!(admission.result, Answer::Ok);
            assert_eq!(admission.retain, Some(retained));

            let repeated = xps.step(
                Input::Advertisement(route_frame(&again, &keys)),
                advance_clock(&mut now, 500),
            );
            let repeated_answer = next_route(&mut xps, &repeated.effects, &mut now, &keys);
            assert_eq!(repeated_answer.result, Answer::Ok);
            assert_eq!(xps.routing.phase(), Phase::WaitingConnection);

            let mut fresh = request.clone();
            fresh.sequence += 1;
            let busy = xps.step(
                Input::Advertisement(route_frame(&fresh, &keys)),
                advance_clock(&mut now, 500),
            );
            let answer = next_route(&mut xps, &busy.effects, &mut now, &keys);
            assert_eq!(answer.result, Answer::Busy);
            gram.step(
                Input::Advertisement(route_frame(&answer, &keys)),
                advance_clock(&mut now, 500),
            );
            assert_eq!(gram.routing.error(), "peer_busy");
            let deadline = gram.routing.retry_deadline();
            gram.step(Input::Tick, advance_clock(&mut now, 500));
            gram.step(Input::Tick, advance_clock(&mut now, 500));
            let held = gram.step(Input::Tick, advance_clock(&mut now, 500));
            let held_request = next_route(&mut gram, &held.effects, &mut now, &keys);
            assert_eq!(held_request.sequence, request.sequence);
            assert_eq!(gram.routing.retry_deadline(), deadline);

            let joined = gram.step(
                Input::Advertisement(route_frame(&admission, &keys)),
                advance_clock(&mut now, 500),
            );
            assert_eq!(gram.routing.phase(), Phase::WaitingCompletion);
            // A verbatim replay of that admission changes nothing.
            gram.step(
                Input::Advertisement(route_frame(&admission, &keys)),
                advance_clock(&mut now, 500),
            );
            assert_eq!(gram.routing.phase(), Phase::WaitingCompletion);
            let connection = next_route(&mut gram, &joined.effects, &mut now, &keys);
            assert_eq!(connection.kind, Kind::Connection);
            assert_eq!(connection.retain, Some(retained));

            let finished = xps.step(
                Input::Advertisement(route_frame(&connection, &keys)),
                advance_clock(&mut now, 500),
            );
            assert_eq!(xps.routing.phase(), Phase::Success);
            assert_eq!(
                xps.routing.outcome().unwrap().result,
                if preserve_apple { "admitted" } else { "admitted_alongside" }
            );
            let complete = next_route(&mut xps, &finished.effects, &mut now, &keys);
            assert_eq!(complete.kind, Kind::Complete);
            gram.step(
                Input::Advertisement(route_frame(&complete, &keys)),
                advance_clock(&mut now, 5_000),
            );
            assert_eq!(gram.routing.phase(), Phase::Success);
            assert_eq!(
                gram.routing.outcome().unwrap().result,
                if preserve_apple { "joined" } else { "joined_alongside" }
            );

            assert!(matches!(gram.start_route(addr(2)), crate::fleet::route::Start::Sent { .. }));
            gram.step(Input::Tick, advance_clock(&mut now, 500));
            gram.step(Input::Tick, advance_clock(&mut now, 30_000));
            assert_eq!(gram.routing.phase(), Phase::Failed);
            assert_eq!(gram.routing.error(), "timed_out");
        }
    }

    #[test]
    fn a_playing_helper_admits_a_requester_that_older_evidence_still_seats() {
        use crate::fleet::members::{Fleet, GroupKey, Member};
        use crate::fleet::route_air::{Answer, Kind, Message};
        let now = clock();
        let local = addr(1);
        let requester = addr(2);
        let pods = addr(9);
        let mut daemon = Daemon::new("gram");
        daemon.set_local_address(Some(local));
        daemon.select_accessory(Some(pods));
        daemon.restore_fleet(Fleet {
            id: FleetId::new([3; 16]),
            key: GroupKey::new([0x11; 32]),
            members: vec![
                Member { address: local, label: "gram".into() },
                Member { address: requester, label: "xps".into() },
            ],
        });
        ready(&mut daemon, now.mono);
        // The requester was the audio source moments ago and has since
        // dropped off; the pods now list only this laptop, which is playing.
        daemon.note_remote(requester, now.mono);
        daemon.note_pod_peers(vec![local], now.unix);
        daemon.note_ownership(true, now.mono);
        daemon.note_source(true, 2, now.mono);
        daemon.hear_request(
            Message {
                kind: Kind::Request,
                result: Answer::Ok,
                source: requester,
                target: local,
                source_session: [1; 16],
                target_session: [2; 16],
                sequence: 1,
                transaction: 1u64.to_be_bytes(),
                sent: now.unix,
                accessory: pods,
                retain: None,
            },
            now,
        );
        let reply = daemon.read_route(&daemon.route_reply.as_ref().unwrap().packet).unwrap();
        assert_eq!(reply.result, Answer::Ok);
        assert_eq!(reply.retain, Some(local), "the playing laptop keeps its seat");
        assert!(!daemon.effects.drop_link);
    }

    #[test]
    fn background_admission_keeps_the_playing_apple_peer_and_releases_this_laptop() {
        use crate::fleet::members::{Fleet, GroupKey, Member};
        use crate::fleet::route_air::{Answer, Kind, Message};
        use crate::step::Input;
        for released in [Some(true), Some(false), None] {
            let mut now = clock();
            let local = addr(1);
            let requester = addr(2);
            let mac = addr(4);
            let pods = addr(9);
            let mut daemon = Daemon::new("helper");
            daemon.set_local_address(Some(local));
            daemon.select_accessory(Some(pods));
            daemon.restore_fleet(Fleet {
                id: FleetId::new([3; 16]),
                key: GroupKey::new([0x11; 32]),
                members: vec![
                    Member { address: local, label: "helper".into() },
                    Member { address: requester, label: "requester".into() },
                ],
            });
            ready(&mut daemon, now.mono);
            daemon.note_pod_peers(vec![local, mac], now.unix);
            daemon.note_ownership(false, now.mono);
            daemon.note_remote(mac, now.mono);
            daemon.note_source(false, 2, now.mono);
            daemon.hear_request(
                Message {
                    kind: Kind::Request,
                    result: Answer::Ok,
                    source: requester,
                    target: local,
                    source_session: [1; 16],
                    target_session: [2; 16],
                    sequence: 1,
                    transaction: 1u64.to_be_bytes(),
                    sent: now.unix,
                    accessory: pods,
                    retain: None,
                },
                now,
            );
            assert_eq!(daemon.routing.retain(), Some(mac));
            assert!(daemon.effects.drop_link, "the helper must actually free its connection slot");
            assert!(!daemon.output.wants_output());
            let generation = daemon.connection.generation();
            // Closing our control socket and its optimistic down notification do
            // not prove the controller has released the physical connection.
            daemon.step(Input::ControlSocket { up: false, generation }, now);
            daemon.step(Input::Bluez(false), now);
            daemon.note_pair_reserved(now);
            daemon.step(
                Input::LinkReleased {
                    accessory: pods,
                    generation: Generation(generation.0 + 1),
                    ok: true,
                },
                now,
            );
            daemon.step(Input::Tick, advance_clock(&mut now, 2_000));
            assert!(
                daemon.route_reply.is_none(),
                "must not admit before Bluetooth confirms release"
            );
            if let Some(ok) = released {
                daemon.step(Input::LinkReleased { accessory: pods, generation, ok }, now);
            } else {
                daemon.step(Input::Tick, advance_clock(&mut now, 8_000));
            }
            let reply = daemon.read_route(&daemon.route_reply.as_ref().unwrap().packet).unwrap();
            assert_eq!(
                reply.result,
                match released {
                    Some(true) => Answer::Ok,
                    Some(false) => Answer::ConnectionFailed,
                    None => Answer::TimedOut,
                }
            );
            assert_eq!(reply.retain, Some(mac));
            if released != Some(true) {
                // A late success cannot turn a failed release into permission to connect.
                daemon.step(Input::LinkReleased { accessory: pods, generation, ok: true }, now);
                let late = daemon.read_route(&daemon.route_reply.as_ref().unwrap().packet).unwrap();
                assert_eq!(late.result, reply.result);
            }
        }
    }

    #[test]
    fn admitted_background_connection_retries_after_an_earlier_audio_release() {
        use crate::fleet::route::{Admit, Start};
        use crate::step::{Effect, Input};
        let now = clock();
        let mut daemon = Daemon::new("requester");
        daemon.select_accessory(Some(addr(9)));
        daemon.step_back(now);
        let Start::Sent { tx } = daemon.start_route(addr(2)) else { panic!("route") };
        assert_eq!(daemon.routing.admit(tx, true, Some(addr(3))), Admit::Beside);
        daemon.open_connection(now.mono);
        let generation = daemon.connection.generation();
        daemon.note_setup_failed(generation, FailReason::Transport, now.mono);
        assert_eq!(daemon.connection.stage(), Stage::Retrying);
        let retry = daemon.step(Input::Tick, Clock::new(now.mono.0 + 2_000, now.unix.0 + 2_000));
        assert!(retry.effects.iter().any(|effect| matches!(effect, Effect::OpenLink { .. })));
        assert!(!daemon.output.wants_output());
        assert!(!retry.effects.iter().any(|effect| matches!(effect, Effect::WriteControl { .. })));
    }

    #[test]
    fn a_play_during_fleet_admission_waits_muted_and_claims_once_the_link_is_ready() {
        use crate::audio::play::PlayerSeen;
        use crate::audio::route::{Graph, LiveStream, Sink};
        use crate::fleet::route::Start;
        use crate::pods::link::{reach_stage, Claim, Stage};
        use crate::pods::observe::Observation;
        use crate::step::{Effect, Input};
        let now = clock();
        let at = |ms: u64| Clock::new(now.mono.0 + ms, now.unix.0 + ms);
        let mut daemon = Daemon::new("gram");
        daemon.set_identity("AirPods", "A3064");
        daemon.select_accessory(Some(addr(9)));
        daemon.audio.startup_players(&[PlayerSeen {
            service: "org.mpris.MediaPlayer2.brave".into(),
            playing: false,
        }]);
        daemon.step(
            Input::Audio(Graph {
                default_sink: "speakers".into(),
                sinks: vec![Sink { name: "speakers".into(), address: None, profile: None }],
                cards: Vec::new(),
                streams: vec![LiveStream {
                    id: 1,
                    generation: 10,
                    sink: "speakers".into(),
                    role: String::new(),
                    node: "Brave".into(),
                    mute: false,
                    temporarily_muted: false,
                    volume: 1.0,
                    corked: false,
                }],
            }),
            now,
        );
        let Start::Sent { .. } = daemon.start_route(addr(2)) else { panic!("route") };
        let play = daemon.step(
            Input::Player { service: "org.mpris.MediaPlayer2.brave".into(), playing: Some(true) },
            now,
        );
        assert!(daemon.output.wants_output());
        assert!(
            play.effects.iter().any(|e| matches!(e, Effect::MuteStreams(_))),
            "the play waits for the pods instead of starting on the speakers"
        );
        // Admission opens the link; the claim goes out when it is ready.
        let Claim::Started(generation) = daemon.connection.claim(now.mono) else { panic!("claim") };
        daemon.note_socket(true, generation, now.mono);
        assert!(reach_stage(&mut daemon.connection, Stage::Notifications, generation, now.mono));
        let ready = daemon.step(Input::Observation(Observation::Ready), at(5_000));
        assert!(ready.effects.iter().any(|e| matches!(e,
            Effect::WriteControl { bytes, .. } if *bytes == crate::pods::setup::ownership(true))));
        // The rest of the opening reports still name the previous owner.
        let stale = daemon.step(
            Input::Observation(Observation::Source {
                local: false,
                kind: 2,
                remote: Some(addr(4)),
            }),
            at(5_300),
        );
        assert!(!stale.effects.iter().any(|e| matches!(e, Effect::PausePlayers(_))));
        assert!(daemon.output.wants_output());
    }

    #[test]
    fn a_play_during_admission_holds_streams_off_a_present_sink_until_the_grant() {
        use crate::audio::play::PlayerSeen;
        use crate::audio::route::{Graph, LiveStream, Sink};
        use crate::fleet::route::Start;
        use crate::pods::link::{reach_stage, Claim, Stage};
        use crate::pods::observe::Observation;
        use crate::step::{Effect, Input};
        let now = clock();
        let at = |ms: u64| Clock::new(now.mono.0 + ms, now.unix.0 + ms);
        let moved = |effects: &[Effect]| {
            effects.iter().any(|e| matches!(e, Effect::MoveStreams(m) if m.sink == "pods"))
        };
        let released = |effects: &[Effect]| {
            moved(effects)
                || effects
                    .iter()
                    .any(|e| matches!(e, Effect::Restore(snaps) if snaps.iter().any(|s| s.id == 1)))
        };
        let mut daemon = Daemon::new("gram");
        daemon.set_identity("AirPods", "A3064");
        daemon.select_accessory(Some(addr(9)));
        daemon.audio.startup_players(&[PlayerSeen {
            service: "org.mpris.MediaPlayer2.brave".into(),
            playing: false,
        }]);
        // The pods are already connected beside this laptop (dual link), so
        // their sink exists before this laptop owns the audio.
        let graph = Graph {
            default_sink: "speakers".into(),
            sinks: vec![
                Sink { name: "speakers".into(), address: None, profile: None },
                Sink {
                    name: "pods".into(),
                    address: Some(addr(9)),
                    profile: Some("a2dp-sink".into()),
                },
            ],
            cards: Vec::new(),
            streams: vec![LiveStream {
                id: 1,
                generation: 10,
                sink: "speakers".into(),
                role: String::new(),
                node: "Brave".into(),
                mute: false,
                temporarily_muted: false,
                volume: 1.0,
                corked: false,
            }],
        };
        daemon.step(Input::Audio(graph.clone()), now);
        let Start::Sent { .. } = daemon.start_route(addr(2)) else { panic!("route") };
        let play = daemon.step(
            Input::Player { service: "org.mpris.MediaPlayer2.brave".into(), playing: Some(true) },
            now,
        );
        assert!(daemon.output.wants_output());
        assert!(!moved(&play.effects), "admission must not route before the grant");
        // WirePlumber switches the default to the pods and links the
        // stream there; FleetPods may only hold it muted until this
        // laptop owns the audio, never move or release it early.
        let mut switched = graph.clone();
        switched.default_sink = "pods".into();
        switched.streams[0].sink = "pods".into();
        let held = daemon.step(Input::Audio(switched.clone()), at(100));
        assert!(!released(&held.effects), "a present sink is not ownership");
        // Admission opens the link; the claim goes out when it is ready.
        let Claim::Started(generation) = daemon.connection.claim(now.mono) else { panic!("claim") };
        daemon.note_socket(true, generation, now.mono);
        assert!(reach_stage(&mut daemon.connection, Stage::Notifications, generation, now.mono));
        let ready = daemon.step(Input::Observation(Observation::Ready), at(5_000));
        assert!(ready.effects.iter().any(|e| matches!(e,
            Effect::WriteControl { bytes, .. } if *bytes == crate::pods::setup::ownership(true))));
        // The pods answer the claim: local source, then the ownership grant.
        daemon.step(
            Input::Observation(Observation::Source { local: true, kind: 2, remote: None }),
            at(5_100),
        );
        daemon.step(Input::Observation(Observation::Ownership(true)), at(5_150));
        // Held streams were saved as muted/quieted: with the grant in hand
        // the router no longer holds, and routing reports the stream routed
        // onto the pods instead of moving or re-releasing it pre-grant.
        assert!(daemon.audio.routed());
        assert!(daemon.output.wants_output());
    }

    #[test]
    fn an_empty_accessory_does_not_invent_who_stays() {
        use crate::fleet::members::{Fleet, GroupKey, Member};
        use crate::fleet::presence::FleetKeys;
        use crate::fleet::route::Phase;
        use crate::fleet::route_air::Answer;
        use crate::step::Input;

        let mut now = clock();
        let pods = Address::parse("A0:00:00:00:00:03").unwrap();
        let keys = FleetKeys { id: [3; 16], key: [0x11; 32] };
        let mut gram = Daemon::new("gram");
        let mut xps = Daemon::new("xps");
        let fleet = Fleet {
            id: FleetId::new([3; 16]),
            key: GroupKey::new([0x11; 32]),
            members: vec![
                Member { address: addr(1), label: "gram".into() },
                Member { address: addr(2), label: "xps".into() },
            ],
        };
        for (daemon, address) in [(&mut gram, addr(1)), (&mut xps, addr(2))] {
            daemon.set_local_address(Some(address));
            daemon.select_accessory(Some(pods));
            daemon.restore_fleet(fleet.clone());
        }
        let gram_tick = gram.step(Input::Tick, now);
        let xps_tick = xps.step(Input::Tick, now);
        gram.step(Input::Advertisement(advertised(&xps_tick.effects).to_vec()), now);
        xps.step(Input::Advertisement(advertised(&gram_tick.effects).to_vec()), now);
        gram.remember_activity(now);
        gram.start_route(addr(2));
        let sent = gram.step(Input::Tick, advance_clock(&mut now, 500));
        let request = next_route(&mut gram, &sent.effects, &mut now, &keys);
        let reply = xps
            .step(Input::Advertisement(route_frame(&request, &keys)), advance_clock(&mut now, 500));
        let admission = next_route(&mut xps, &reply.effects, &mut now, &keys);
        assert_eq!(admission.result, Answer::Unavailable);
        assert_eq!(xps.routing.phase(), Phase::Idle);
    }

    #[test]
    fn startup_playback_waits_for_max_wear_across_event_orderings() {
        use crate::audio::play::PlayerSeen;
        use crate::audio::route::{Graph, LiveStream, Sink};
        use crate::pods::observe::Observation;
        use crate::step::{Effect, Input};

        for snapshot_first in [false, true] {
            for identity_first in [false, true] {
                let mut daemon = Daemon::new("xps");
                let now = clock();
                let brave = "org.mpris.MediaPlayer2.brave";
                let hold = |daemon: &mut Daemon, input| {
                    let outcome = daemon.step(input, now);
                    assert!(
                        !daemon.output.wants_output(),
                        "{snapshot_first}/{identity_first}: {:?}",
                        outcome.effects
                    );
                    assert!(
                        !outcome.effects.iter().any(|effect| matches!(
                            effect,
                            Effect::MuteStreams(_)
                                | Effect::MoveStreams(_)
                                | Effect::SetDefaultSink { .. }
                        )),
                        "{:?}",
                        outcome.effects
                    );
                };
                hold(
                    &mut daemon,
                    Input::Audio(Graph {
                        default_sink: "speakers".into(),
                        sinks: vec![
                            Sink { name: "speakers".into(), address: None, profile: None },
                            Sink { name: "pods".into(), address: Some(addr(3)), profile: None },
                        ],
                        cards: vec![],
                        streams: vec![LiveStream {
                            id: 1,
                            generation: 1,
                            sink: "speakers".into(),
                            role: String::new(),
                            node: "Brave".into(),
                            mute: false,
                            temporarily_muted: false,
                            volume: 1.0,
                            corked: false,
                        }],
                    }),
                );
                hold(
                    &mut daemon,
                    Input::Players(vec![PlayerSeen {
                        service: brave.into(),
                        playing: snapshot_first,
                    }]),
                );
                hold(&mut daemon, Input::Paired { address: addr(3), name: "Max".into() });
                hold(&mut daemon, Input::Bluez(true));
                if !snapshot_first {
                    hold(&mut daemon, Input::Player { service: brave.into(), playing: Some(true) });
                }
                let identity = Observation::Identity { name: "Max".into(), model: "A3454".into() };
                let wear = Observation::HeadWear { primary: Some(false), secondary: Some(false) };
                for report in if identity_first { [identity, wear] } else { [wear, identity] } {
                    hold(&mut daemon, Input::Observation(report));
                }
                hold(&mut daemon, Input::Tick);
                // Existing play intent survives waiting for evidence. Re-wear
                // claims promptly, without a second Play or another worker.
                daemon.step(
                    Input::Observation(Observation::HeadWear {
                        primary: Some(true),
                        secondary: Some(true),
                    }),
                    now,
                );
                daemon.step(Input::Tick, now);
                assert!(daemon.output.wants_output(), "{snapshot_first}/{identity_first}");
            }
        }
    }

    #[test]
    fn a_pairing_snapshot_claims_existing_playback_only_when_available() {
        use crate::audio::play::PlayerSeen;
        use crate::step::{Effect, Input};

        for worn in [None, Some(false), Some(true)] {
            let mut daemon = Daemon::new("gram");
            let now = clock();
            if let Some(worn) = worn {
                daemon.set_identity("Max", "A3454");
                daemon.accessory.note_head_worn(worn, now.mono);
            }
            daemon.step(
                Input::Players(vec![PlayerSeen {
                    service: "org.mpris.MediaPlayer2.brave".into(),
                    playing: true,
                }]),
                now,
            );
            let found = daemon.step(Input::Paired { address: addr(3), name: "Pods".into() }, now);
            assert_eq!(daemon.output.wants_output(), worn == Some(true), "wear={worn:?}");
            if worn == Some(true) {
                assert!(found
                    .effects
                    .iter()
                    .any(|effect| matches!(effect, Effect::OpenLink { .. })));
            } else {
                assert!(
                    !found.effects.iter().any(|effect| matches!(
                        effect,
                        Effect::MuteStreams(_)
                            | Effect::MoveStreams(_)
                            | Effect::SetDefaultSink { .. }
                    )),
                    "pairing inventory must leave speakers alone: {:?}",
                    found.effects
                );
            }
        }
    }

    pub(super) fn ready(daemon: &mut Daemon, now: MonoMs) {
        use crate::pods::link::{reach_stage, Claim, Stage};
        let Claim::Started(generation) = daemon.connection.claim(now) else {
            panic!("claim");
        };
        daemon.note_socket(true, generation, now);
        assert!(reach_stage(&mut daemon.connection, Stage::Notifications, generation, now));
        assert!(daemon.connection.mark_ready(generation));
    }

    fn route_frame(
        message: &crate::fleet::route_air::Message,
        keys: &crate::fleet::presence::FleetKeys,
    ) -> Vec<u8> {
        crate::fleet::route_air::advertise(message, keys).unwrap()
    }

    fn advance_clock(now: &mut Clock, milliseconds: u64) -> Clock {
        *now = Clock::new(now.mono.0 + milliseconds, now.unix.0 + milliseconds);
        *now
    }

    // Protocol assertions should allow presence/roster broadcasts between
    // routing packets, while still bounding how long the route can wait.
    fn next_route(
        daemon: &mut Daemon,
        effects: &[crate::step::Effect],
        now: &mut Clock,
        keys: &crate::fleet::presence::FleetKeys,
    ) -> crate::fleet::route_air::Message {
        let decode = |effects: &[crate::step::Effect]| {
            effects.iter().find_map(|effect| {
                if let crate::step::Effect::Advertise(bytes) = effect {
                    crate::fleet::route_air::read(bytes, keys)
                } else {
                    None
                }
            })
        };
        if let Some(message) = decode(effects) {
            return message;
        }
        for _ in 0..8 {
            let outcome = daemon.step(crate::step::Input::Tick, advance_clock(now, 500));
            if let Some(message) = decode(&outcome.effects) {
                return message;
            }
        }
        panic!("route advertisement did not get a slot within four seconds");
    }

    fn keys_for_test() -> crate::fleet::presence::FleetKeys {
        crate::fleet::presence::FleetKeys { id: [3; 16], key: [0x11; 32] }
    }

    fn advertised(effects: &[crate::step::Effect]) -> &[u8] {
        effects
            .iter()
            .find_map(|effect| match effect {
                crate::step::Effect::Advertise(bytes) => Some(bytes.as_slice()),
                _ => None,
            })
            .expect("an advertisement")
    }
    fn peer_media(sender: Address, local: Address, playing: bool) -> Vec<u8> {
        let mut packet = crate::pods::peer::media_information(sender, local, "gram", playing);
        packet[4] = 0x11;
        packet[6..12].copy_from_slice(&sender.bytes().into_iter().rev().collect::<Vec<_>>());
        packet
    }

    #[test]
    fn a_peer_pause_quiets_status_and_activity_claims_only_the_idle_owner() {
        use crate::fleet::members::Fleet;
        use crate::step::{Effect, Input};
        let mut daemon = Daemon::new("xps");
        let mut now = clock();
        let local = addr(2);
        let gram = addr(4);
        daemon.set_local_address(Some(local));
        daemon.restore_fleet(Fleet {
            id: FleetId::new([3; 16]),
            key: GroupKey::new([0x11; 32]),
            members: vec![Member { address: gram, label: "lg-gram".into() }],
        });
        ready_link(&mut daemon, now);
        daemon.note_pod_peers(vec![local, gram], now.unix);
        let generation = daemon.connection.generation();
        daemon.step(
            Input::Observation(crate::pods::observe::Observation::Source {
                local: false,
                kind: 2,
                remote: Some(gram),
            }),
            now,
        );
        let claims = |outcome: &crate::step::StepOutcome| {
            outcome
                .effects
                .iter()
                .filter(|effect| {
                    matches!(effect,
            Effect::WriteControl { bytes, .. } if *bytes == crate::pods::setup::ownership(true))
                })
                .count()
        };
        assert_eq!(
            claims(&daemon.step(Input::Command(Command::Activity), now)),
            0,
            "unknown playback cannot be interrupted"
        );
        for playing in [true, false] {
            daemon.activity_at = None;
            now.mono.0 += 10;
            now.unix.0 += 10;
            daemon.step(
                Input::Accessory { generation, packet: peer_media(gram, local, playing) },
                now,
            );
            let status = daemon.status_value(now);
            assert_eq!(status["audio_owner"]["holder"], "remote");
            assert_eq!(status["audio_owner"]["playing"], playing);
            assert_eq!(status["audio_source_type"], 2, "raw source report is untouched");
            assert_eq!(daemon.audio_roster(now).unwrap().audio_idle, !playing);
            let activity = daemon.step(Input::Command(Command::Activity), now);
            assert_eq!(claims(&activity), usize::from(!playing));
            assert_eq!(daemon.connection.generation(), generation);
            // (The rebuild-on-silence count is asserted inside the grant arm above.)
            assert!(!daemon.output.wants_output(), "input alone must not move media");
            assert!(!activity.effects.iter().any(|e| matches!(
                e,
                Effect::PausePlayers(_) | Effect::SetDefaultSink { .. } | Effect::MoveStreams(_)
            )));
        }
        assert_eq!(
            claims(&daemon.step(Input::Command(Command::Activity), now)),
            0,
            "no duplicate claim while waiting"
        );
        assert_eq!(
            daemon.status_value(now)["audio_owner"]["holder"],
            "remote",
            "request is not confirmation"
        );
        now.mono.0 += 10;
        daemon.step(Input::Observation(crate::pods::observe::Observation::Ownership(true)), now);
        assert_eq!(daemon.status_value(now)["audio_owner"]["holder"], "local");
        assert_eq!(daemon.status_value(now)["roster_audio_owner"], local.display());
        let roster = daemon.audio_roster(now).unwrap();
        assert_eq!(roster.audio_source, Some(local));
        assert!(roster.audio_idle);
        assert_eq!(daemon.status_value(now)["audio_owner"]["playing"], false);
        daemon.drop_reports();
        assert!(daemon.peer_playback.is_empty());
    }

    #[test]
    fn activity_does_not_interrupt_an_outside_device_that_just_joined() {
        use crate::step::{Effect, Input};
        let mut daemon = Daemon::new("xps");
        let mut now = clock();
        let local = addr(2);
        let phone = addr(7);
        daemon.set_local_address(Some(local));
        ready_link(&mut daemon, now);
        daemon.note_pod_peers(vec![local, phone], now.unix);
        // The phone just took the roster and has not started playing yet:
        // it is joining, not idle.
        daemon.step(
            Input::Observation(crate::pods::observe::Observation::Source {
                local: false,
                kind: 2,
                remote: Some(phone),
            }),
            now,
        );
        daemon.note_peer_playback(phone, false, now);
        let claims = |outcome: &crate::step::StepOutcome| {
            outcome
                .effects
                .iter()
                .filter(|effect| {
                    matches!(effect,
                        Effect::WriteControl { bytes, .. }
                            if *bytes == crate::pods::setup::ownership(true))
                })
                .count()
        };
        assert_eq!(
            claims(&daemon.step(Input::Command(Command::Activity), now)),
            0,
            "desktop activity must not snatch the pods from a device still joining"
        );
        // Once the phone has been idle well past the join window, activity
        // may claim as before.
        now.mono.0 += 10_001;
        now.unix.0 += 10_001;
        assert_eq!(
            claims(&daemon.step(Input::Command(Command::Activity), now)),
            1,
            "an outside device idle past the join window can be claimed"
        );
    }

    #[test]
    fn a_phone_still_playing_after_two_minutes_still_reads_as_playing() {
        use crate::step::Input;
        let mut daemon = Daemon::new("xps");
        let mut now = clock();
        let local = addr(2);
        let phone = addr(7);
        daemon.set_local_address(Some(local));
        ready_link(&mut daemon, now);
        daemon.note_pod_peers(vec![local, phone], now.unix);
        daemon.step(
            Input::Observation(crate::pods::observe::Observation::Source {
                local: false,
                kind: 2,
                remote: Some(phone),
            }),
            now,
        );
        now.mono.0 += 10;
        daemon.note_peer_playback(phone, true, now);
        assert_eq!(daemon.status_value(now)["audio_owner"]["playing"], true);
        // The pods report playback when it changes. A song that keeps playing sends nothing more.
        let elapsed = crate::pods::reports::REPORT_TTL_MS + 60_000;
        now = Clock::new(now.mono.0 + elapsed, now.unix.0 + elapsed);
        daemon.step(crate::step::Input::Tick, now);
        let status = daemon.status_value(now);
        assert_eq!(status["audio_owner"]["holder"], "remote");
        assert_eq!(status["audio_owner"]["playing"], true);
        assert_eq!(status["remote_playing"], true);
        // A pause is reported, and it stands too.
        daemon.note_peer_playback(phone, false, now);
        assert_eq!(daemon.status_value(now)["audio_owner"]["playing"], false);
    }

    #[test]
    fn a_live_link_keeps_its_readings_and_a_lost_one_lets_them_age() {
        use crate::step::Input;
        let mut daemon = Daemon::new("xps");
        let now = clock();
        let local = addr(2);
        let phone = addr(7);
        daemon.set_local_address(Some(local));
        ready_link(&mut daemon, now);
        daemon.note_pod_peers(vec![local, phone], now.unix);
        daemon.step(
            Input::Observation(crate::pods::observe::Observation::Source {
                local: false,
                kind: 2,
                remote: Some(phone),
            }),
            now,
        );
        let later_by = |ms: u64| Clock::new(now.mono.0 + ms, now.unix.0 + ms);
        let later = later_by(crate::pods::reports::REPORT_TTL_MS * 3);
        daemon.step(crate::step::Input::Tick, later);
        // Nothing changed for six minutes on a live link: the pods had nothing new to say.
        assert!(daemon.accessory.source.as_fresh().is_some());
        assert_eq!(daemon.accessory.remote_address.as_fresh(), Some(&phone));
        assert!(daemon.accessory.left.as_fresh().is_some());
        assert!(daemon.accessory.noise.as_fresh().is_some() || daemon.accessory.noise.is_unknown());
        // Without the link, the battery reading ages out as before.
        daemon.note_socket(false, daemon.connection.generation(), now.mono);
        daemon
            .step(crate::step::Input::Tick, later_by(crate::pods::reports::REPORT_TTL_MS * 3 + 1));
        assert!(daemon.accessory.left.as_fresh().is_none());
        assert!(daemon.accessory.left.known().is_some(), "aged, not forgotten");
    }

    #[test]
    fn peer_playback_must_match_the_owner_and_be_newer_than_its_source() {
        use crate::step::Input;
        let mut daemon = Daemon::new("xps");
        let mut now = clock();
        let local = addr(2);
        let gram = addr(4);
        daemon.set_local_address(Some(local));
        ready_link(&mut daemon, now);
        daemon.note_pod_peers(vec![local, gram], now.unix);
        daemon.note_peer_playback(gram, false, now);
        now.mono.0 += 10;
        daemon.step(
            Input::Observation(crate::pods::observe::Observation::Source {
                local: false,
                kind: 2,
                remote: Some(gram),
            }),
            now,
        );
        assert_eq!(
            daemon.status_value(now)["audio_owner"]["playing"],
            true,
            "old idle cannot silence a new source"
        );
        daemon.note_peer_playback(addr(6), false, now);
        assert!(!daemon.peer_playback.contains_key(&addr(6)));
        assert_eq!(daemon.status_value(now)["audio_owner"]["playing"], true);
        daemon.note_peer_playback(gram, false, now);
        assert_eq!(daemon.status_value(now)["audio_owner"]["playing"], false);
        daemon.note_pod_peers(vec![local], now.unix);
        assert!(daemon.peer_playback.is_empty());
    }

    #[test]
    fn an_idle_source_change_republishes_desktop_pause_to_the_other_laptop() {
        use crate::audio::play::PlayerSeen;
        use crate::step::{Effect, Input};
        let mut xps = Daemon::new("xps");
        let mut gram = Daemon::new("gram");
        let mut now = clock();
        let local = addr(2);
        let peer = addr(4);
        xps.set_local_address(Some(local));
        gram.set_local_address(Some(peer));
        for daemon in [&mut xps, &mut gram] {
            ready_link(daemon, now);
            daemon.note_pod_peers(vec![local, peer], now.unix);
            daemon.step(
                Input::Players(vec![PlayerSeen { service: "browser".into(), playing: false }]),
                now,
            );
        }
        gram.note_peer_playback(local, false, now);
        now.mono.0 += 100;
        now.unix.0 += 100;
        let source = xps.step(
            Input::Observation(crate::pods::observe::Observation::Source {
                local: true,
                kind: 2,
                remote: None,
            }),
            now,
        );
        let refreshed = crate::pods::peer::media_information(local, peer, "xps", false);
        assert!(source.effects.iter().any(|e| matches!(e,
            Effect::WriteControl { bytes, .. } if *bytes == refreshed)));
        gram.step(
            Input::Observation(crate::pods::observe::Observation::Source {
                local: false,
                kind: 2,
                remote: Some(local),
            }),
            now,
        );
        let generation = gram.connection.generation();
        let mut relay = refreshed;
        relay[4] = 0x11;
        relay[6..12].copy_from_slice(&local.bytes().into_iter().rev().collect::<Vec<_>>());
        gram.step(Input::Accessory { generation, packet: relay }, now);
        assert_eq!(gram.status_value(now)["audio_owner"]["playing"], false);
        assert!(gram.audio_roster(now).unwrap().audio_idle);
        // A same-tick ownership notification must supersede the old source.
        gram.step(Input::Observation(crate::pods::observe::Observation::Ownership(true)), now);
        assert_eq!(gram.status_value(now)["audio_owner"]["holder"], "local");
        assert_eq!(gram.status_value(now)["audio_owner"]["playing"], false);
    }

    #[test]
    fn local_pause_updates_rosters_and_relays_without_changing_source_or_owner() {
        use crate::audio::play::PlayerSeen;
        use crate::step::{Effect, Input};
        let mut daemon = Daemon::new("xps");
        let now = clock();
        let local = addr(2);
        let gram = addr(4);
        daemon.set_local_address(Some(local));
        ready_link(&mut daemon, now);
        daemon.note_pod_peers(vec![local, gram], now.unix);
        daemon
            .step(Input::Players(vec![PlayerSeen { service: "brave".into(), playing: true }]), now);
        daemon.step(
            Input::Observation(crate::pods::observe::Observation::Source {
                local: true,
                kind: 2,
                remote: None,
            }),
            now,
        );
        assert_eq!(daemon.status_value(now)["audio_owner"]["playing"], true);
        let paused =
            daemon.step(Input::Player { service: "brave".into(), playing: Some(false) }, now);
        assert_eq!(daemon.status_value(now)["audio_owner"]["holder"], "local");
        assert_eq!(daemon.status_value(now)["audio_owner"]["playing"], false);
        assert_eq!(daemon.status_value(now)["audio_source_type"], 2);
        assert!(daemon.audio_roster(now).unwrap().audio_idle);
        assert!(paused
            .effects
            .iter()
            .any(|effect| matches!(effect, Effect::WriteControl { bytes, .. }
            if *bytes == crate::pods::peer::media_information(local, gram, "xps", false))));
    }
    #[test]
    fn activity_claims_an_idle_fleet_owner_when_ownership_loss_omits_its_address() {
        use crate::fleet::members::Fleet;
        use crate::fleet::roster_air::Snapshot;
        use crate::step::{Effect, Input};

        for name in ["xps", "gram"] {
            for scenario in ["idle", "playing", "pending", "stale", "not_seated", "local_playing"] {
                let mut daemon = Daemon::new(name);
                let mut now = clock();
                let local = addr(2);
                let peer = addr(4);
                daemon.set_local_address(Some(local));
                ready_link(&mut daemon, now);
                daemon.restore_fleet(Fleet {
                    id: FleetId::new([3; 16]),
                    key: GroupKey::new([0x11; 32]),
                    members: vec![Member { address: peer, label: "other laptop".into() }],
                });
                daemon.note_pod_peers(
                    if scenario == "not_seated" { vec![local] } else { vec![local, peer] },
                    now.unix,
                );
                daemon.note_ownership(false, now.mono);
                assert!(daemon.accessory.remote_address.known().is_none());
                daemon.observe_nearby(
                    host_id(Some(peer)),
                    peer,
                    "other laptop",
                    now.unix,
                    true,
                    Some([1; 16]),
                );
                daemon.hear_roster(
                    Snapshot {
                        reporter: peer,
                        session: [1; 16],
                        sequence: 1,
                        sent: now.unix,
                        accessory: addr(3),
                        peers: vec![local, peer],
                        audio_source: Some(peer),
                        last_use: UnixMs(0),
                        pending: scenario == "pending",
                        audio_idle: scenario != "playing",
                        intent: false,
                    },
                    now,
                );
                if scenario == "stale" {
                    now.mono.0 += 30_001;
                    now.unix.0 += 30_001;
                }
                if scenario == "local_playing" {
                    daemon.step(
                        Input::Players(vec![crate::audio::play::PlayerSeen {
                            service: "browser".into(),
                            playing: true,
                        }]),
                        now,
                    );
                }
                let generation = daemon.connection.generation();
                let outcome = daemon.step(Input::Command(Command::Activity), now);
                let claims = outcome.effects.iter().filter(|e| matches!(e,
                    Effect::WriteControl { bytes, .. } if *bytes == crate::pods::setup::ownership(true)
                )).count();
                assert_eq!(claims, usize::from(scenario == "idle"), "{name}: {scenario}");
                assert_eq!(daemon.connection.generation(), generation);
                // (The rebuild-on-silence count is asserted inside the grant arm above.)
                assert!(
                    !outcome.effects.iter().any(|e| matches!(
                        e,
                        Effect::OpenLink { .. }
                            | Effect::PausePlayers(_)
                            | Effect::SetDefaultSink { .. }
                            | Effect::MoveStreams(_)
                    )),
                    "activity must only request ownership on the existing link"
                );
                if scenario == "idle" {
                    assert_eq!(daemon.status_value(now)["audio_owner"]["holder"], "remote");
                    let duplicate = daemon.step(Input::Command(Command::Activity), now);
                    assert!(!duplicate.effects.iter().any(|e| matches!(e,
                        Effect::WriteControl { bytes, .. } if *bytes == crate::pods::setup::ownership(true)
                    )));
                    daemon.step(
                        Input::Observation(crate::pods::observe::Observation::Ownership(true)),
                        now,
                    );
                    assert_eq!(daemon.status_value(now)["audio_owner"]["holder"], "local");
                }
            }
        }
    }

    #[test]
    fn fresh_activity_waits_for_peer_pause_but_expires_without_claiming() {
        use crate::fleet::members::{Fleet, Member};
        use crate::step::{Effect, Input};
        for delay in [3_500, 5_001] {
            let mut daemon = Daemon::new("xps");
            let mut now = clock();
            let local = addr(2);
            let gram = addr(4);
            daemon.set_local_address(Some(local));
            daemon.restore_fleet(Fleet {
                id: FleetId::new([3; 16]),
                key: GroupKey::new([0x11; 32]),
                members: vec![Member { address: gram, label: "lg-gram".into() }],
            });
            ready_link(&mut daemon, now);
            daemon.note_pod_peers(vec![local, gram], now.unix);
            daemon.step(
                Input::Observation(crate::pods::observe::Observation::Source {
                    local: false,
                    kind: 2,
                    remote: Some(gram),
                }),
                now,
            );
            let generation = daemon.connection.generation();
            daemon.note_peer_playback(gram, true, now);
            let activity = daemon.step(Input::Command(Command::Activity), now);
            let claimed = |outcome: &crate::step::StepOutcome| {
                outcome.effects.iter().any(|effect| {
                    matches!(effect,
                Effect::WriteControl { bytes, .. } if *bytes == crate::pods::setup::ownership(true))
                })
            };
            assert!(!claimed(&activity));
            now.mono.0 += delay;
            now.unix.0 += delay;
            let paused = daemon
                .step(Input::Accessory { generation, packet: peer_media(gram, local, false) }, now);
            assert_eq!(claimed(&paused), delay <= 5_000);
            assert!(!daemon.output.wants_output());
            assert!(!claimed(&daemon.step(Input::Tick, now)));
        }
    }

    #[test]
    fn startup_roster_owner_allows_idle_activity_without_replaying_satisfied_input() {
        use crate::fleet::members::Fleet;
        use crate::step::{Effect, Input};
        let local = addr(2);
        let peer = addr(4);
        let mut daemon = Daemon::new("xps");
        let mut now = clock();
        daemon.set_local_address(Some(local));
        daemon.restore_fleet(Fleet {
            id: FleetId::new([3; 16]),
            key: GroupKey::new([0x11; 32]),
            members: vec![Member { address: peer, label: "lg-gram".into() }],
        });
        ready_link(&mut daemon, now);
        let generation = daemon.connection.generation();
        let roster = |owner| {
            let mut packet = vec![4, 0, 4, 0, 0x2e, 0, 1, 2, 2];
            for address in [local, peer] {
                packet.extend(address.bytes());
                packet.extend([2, if address == owner { 3 } else { 1 }]);
            }
            Input::Accessory { generation, packet }
        };
        let claims = |outcome: &crate::step::StepOutcome| {
            outcome.effects.iter().any(|e| {
                matches!(e, Effect::WriteControl { bytes, .. }
                if *bytes == crate::pods::setup::ownership(true))
            })
        };
        // No ownership notification after attaching to an existing idle link.
        daemon.step(
            Input::Observation(crate::pods::observe::Observation::Source {
                local: true,
                kind: 0,
                remote: None,
            }),
            now,
        );
        assert_eq!(daemon.audio_roster(now).unwrap().audio_source, None);
        daemon.step(roster(local), now);
        assert_eq!(daemon.status_value(now)["audio_owner"]["holder"], "local");
        assert_eq!(daemon.status_value(now)["audio_owner"]["playing"], false);
        assert_eq!(daemon.audio_roster(now).unwrap().audio_source, Some(local));
        daemon.note_peer_playback(peer, false, now);
        assert!(!claims(&daemon.step(Input::Command(Command::Activity), now)));
        // Gram claims one second later: old XPS activity must not steal it back.
        now.mono.0 += 1_000;
        now.unix.0 += 1_000;
        assert!(!claims(&daemon.step(roster(peer), now)));
        assert_eq!(daemon.status_value(now)["audio_owner"]["holder"], "remote");
        assert_eq!(daemon.audio_roster(now).unwrap().audio_source, Some(peer));
        // Only new input can claim it, using the existing connection.
        let activity = daemon.step(Input::Command(Command::Activity), now);
        assert!(claims(&activity));
        assert!(!activity.effects.iter().any(|e| matches!(
            e,
            Effect::OpenLink { .. } | Effect::Disconnect { .. } | Effect::MoveStreams(_)
        )));
        daemon.step(roster(local), now);
        assert_eq!(daemon.status_value(now)["audio_owner"]["holder"], "local");
        assert_eq!(daemon.connection.generation(), generation);
    }

    #[test]
    fn fleet_owner_is_not_echoed_back_as_a_new_direct_observation() {
        let now = clock();
        let mut xps = Daemon::new("xps");
        let mut gram = Daemon::new("gram");
        for (daemon, address) in [(&mut xps, addr(2)), (&mut gram, addr(4))] {
            ready_link(daemon, now);
            daemon.set_local_address(Some(address));
            daemon.note_source(true, 0, now.mono);
            daemon.note_pod_peers(vec![addr(2), addr(4)], now.unix);
        }
        gram.note_ownership(true, now.mono);
        xps.peers.ingest(gram.audio_roster(now).unwrap(), Some(addr(3)));
        assert_eq!(xps.status_value(now)["audio_owner"]["holder"], "remote");
        let forwarded = xps.audio_roster(now).unwrap();
        assert_eq!(forwarded.audio_source, None);
        gram.drop_reports();
        gram.peers.ingest(forwarded, Some(addr(3)));
        assert_eq!(gram.resolved(now).holder, Holder::Unknown);
    }
    #[test]
    fn a_second_browser_without_mpris_uses_owned_pods_without_another_claim() {
        use crate::audio::play::PlayerSeen;
        use crate::audio::route::{Graph, LiveStream, Sink};
        use crate::step::{Effect, Input};
        for (local_owner, yielded, expected) in
            [(true, false, true), (false, false, false), (true, true, false)]
        {
            let mut daemon = Daemon::new("xps");
            let now = clock();
            ready_link(&mut daemon, now);
            daemon.step(
                Input::Players(vec![PlayerSeen { service: "brave.first".into(), playing: false }]),
                now,
            );
            daemon.note_ownership(local_owner, now.mono);
            if yielded {
                daemon.step_back(now);
            }
            let generation = daemon.connection.generation();
            let mut graph = Graph {
                default_sink: "pods".into(),
                sinks: vec![
                    Sink {
                        name: "pods".into(),
                        address: Some(addr(3)),
                        profile: Some("a2dp-sink".into()),
                    },
                    Sink { name: "omarchy_speaker_tuning".into(), address: None, profile: None },
                ],
                cards: vec![],
                streams: vec![],
            };
            daemon.step(Input::Audio(graph.clone()), now);
            graph.streams.push(LiveStream {
                id: 55,
                generation: 1234,
                sink: "omarchy_speaker_tuning".into(),
                role: "video".into(),
                node: "Brave second process".into(),
                mute: false,
                temporarily_muted: false,
                volume: 0.6,
                corked: false,
            });
            let started = daemon.step(Input::Audio(graph), now);
            assert_eq!(daemon.output.wants_output(), expected);
            assert_eq!(
                started.effects.iter().any(|effect| matches!(effect, Effect::MoveStreams(m)
                if m.sink == "pods" && m.streams.iter().any(|stream| stream.id == 55))),
                expected
            );
            assert!(!started.effects.iter().any(|effect| matches!(
                effect,
                Effect::OpenLink { .. }
                    | Effect::RestartOutput { .. }
                    | Effect::WriteControl { .. }
            )));
            assert_eq!(daemon.connection.generation(), generation);
            // (The rebuild-on-silence count is asserted inside the grant arm above.)
        }
    }
    mod preferred_connection {
        use super::*;
        use crate::fleet::members::Fleet;
        use crate::step::{Effect, Input};

        fn peer(n: u8, now: Clock) -> Daemon {
            let mut daemon = Daemon::new(if n == 1 { "gram" } else { "xps" });
            daemon.set_local_address(Some(addr(n)));
            daemon.select_accessory(Some(addr(9)));
            daemon.restore_fleet(Fleet {
                id: FleetId::new([3; 16]),
                key: GroupKey::new([0x11; 32]),
                members: vec![
                    Member { address: addr(1), label: "gram".into() },
                    Member { address: addr(2), label: "xps".into() },
                ],
            });
            // Both buds in the wells. In-ear placement is already available, so
            // the preferred host would open while the case is still closed.
            for right in [false, true] {
                daemon.accessory.note_placement(right, Some(false), Some(true), now.mono);
            }
            daemon.step(Input::Lid { open: false }, now);
            daemon
        }

        fn opens(effects: &[Effect]) -> bool {
            effects.iter().any(|e| matches!(e, Effect::OpenLink { .. }))
        }

        fn exchange(a: &mut Daemon, b: &mut Daemon, now: &mut Clock) {
            for _ in 0..12 {
                advance_clock(now, 500);
                let a_tick = a.step(Input::Tick, *now);
                let b_tick = b.step(Input::Tick, *now);
                assert!(!opens(&a_tick.effects) && !opens(&b_tick.effects));
                for (receiver, effects) in [(&mut *b, a_tick.effects), (&mut *a, b_tick.effects)] {
                    for effect in effects {
                        if let Effect::Advertise(bytes) = effect {
                            let heard = receiver.step(Input::Advertisement(bytes), *now);
                            assert!(!opens(&heard.effects));
                        }
                    }
                }
            }
        }

        #[test]
        fn periodic_preference_refreshes_do_not_broadcast_during_a_connection_attempt() {
            let mut now = clock();
            let mut daemon = peer(1, now);
            daemon.step(Input::Command(Command::Activity), now);
            // Let the initial preference and roster announcements finish.
            for _ in 0..5 {
                advance_clock(&mut now, 500);
                daemon.step(Input::Tick, now);
            }
            for right in [false, true] {
                daemon.accessory.note_placement(right, Some(true), Some(false), now.mono);
            }
            daemon.handle(Command::Connect, now);
            let opening = daemon.step(Input::Tick, now);
            assert!(opens(&opening.effects));
            for _ in 0..14 {
                advance_clock(&mut now, 500);
                let tick = daemon.step(Input::Tick, now);
                assert!(
                    !tick.effects.iter().any(|e| matches!(e, Effect::Advertise(_))),
                    "even periodic preference envelopes must defer during paging"
                );
                assert!(daemon.listening);
                assert_eq!(daemon.connection.stage(), Stage::Connecting);
            }
            let connected = daemon.step(Input::Bluez(true), now);
            assert!(connected.effects.iter().any(|e| matches!(e, Effect::Advertise(_))));
        }

        #[test]
        fn case_activity_is_shared_and_only_the_latest_device_reconnects_after_restart() {
            let mut now = clock();
            let mut a = peer(1, now);
            let mut b = peer(2, now);
            let first = a.step(Input::Command(Command::Activity), now);
            assert!(!opens(&first.effects));
            exchange(&mut a, &mut b, &mut now);
            assert_eq!(b.preferred_connector().unwrap().owner, addr(1));
            advance_clock(&mut now, 500);
            let second = b.step(Input::Command(Command::Activity), now);
            let saved = second
                .effects
                .into_iter()
                .find_map(|e| match e {
                    Effect::StoreActivity(bytes) => Some(bytes),
                    _ => None,
                })
                .expect("activity is durable while the case is closed");
            exchange(&mut a, &mut b, &mut now);
            assert_eq!(a.preferred_connector().unwrap().owner, addr(2));
            b = peer(2, now);
            b.activity_preference.restore(&saved);
            for daemon in [&mut a, &mut b] {
                let opened = daemon.step(Input::Lid { open: true }, now);
                assert!(!opens(&opened.effects), "allow the case and fleet radio to settle");
            }
            let mut b_opened = false;
            for _ in 0..8 {
                advance_clock(&mut now, 500);
                assert!(!opens(&a.step(Input::Tick, now).effects));
                b_opened |= opens(&b.step(Input::Tick, now).effects);
            }
            assert!(b_opened, "the remembered device automatically connects");
            assert!(!b.output.wants_output(), "availability alone never claims audio");
            // The preferred device now disappears. No expiry, stale discovery,
            // or Bluetooth auto-attach gives the other laptop permission.
            advance_clock(&mut now, 3_600_000);
            assert!(!opens(&a.step(Input::Tick, now).effects));
            a.note_bluez(true);
            a.attach_available(a.connection.generation(), now.mono);
            let rejected = a.step(Input::Tick, now);
            assert!(!opens(&rejected.effects));
            assert!(rejected.effects.iter().any(|e| matches!(e, Effect::Disconnect { .. })));
            assert_eq!(a.preferred_connector().unwrap().owner, addr(2));
            let explicit = a.step(Input::Command(Command::Connect), now);
            assert!(opens(&explicit.effects));
            assert_eq!(a.preferred_connector().unwrap().owner, addr(1));
        }

        #[test]
        fn startup_preserves_the_record_when_bluez_identifies_the_saved_accessory() {
            let now = clock();
            let mut b = peer(2, now);
            let changed = b.step(Input::Command(Command::Activity), now);
            let saved = changed
                .effects
                .into_iter()
                .find_map(|e| match e {
                    Effect::StoreActivity(bytes) => Some(bytes),
                    _ => None,
                })
                .unwrap();
            let mut a = peer(1, now);
            a.select_accessory(None);
            a.activity_preference.restore(&saved);
            assert!(a.preferred_connector().is_none());
            let discovered = a.step(Input::Paired { address: addr(9), name: "Pods".into() }, now);
            assert_eq!(a.preferred_connector().unwrap().owner, addr(2));
            assert!(!opens(&discovered.effects));
            a.select_accessory(Some(addr(8)));
            assert!(a.preferred_connector().is_none());
            assert_eq!(a.activity_preference.take_save().unwrap(), b"FPAC1");
        }

        #[test]
        fn user_disconnect_survives_case_cycles_until_fresh_activity() {
            let mut now = clock();
            let mut daemon = peer(1, now);
            daemon.step(Input::Command(Command::Activity), now);
            daemon.step(Input::Command(Command::Disconnect), now);
            for open in [true, false, true] {
                daemon.step(Input::Lid { open }, now);
                advance_clock(&mut now, 5_000);
                assert!(!opens(&daemon.step(Input::Tick, now).effects));
            }
            daemon.step(Input::Lid { open: false }, now);
            let activity = daemon.step(Input::Command(Command::Activity), now);
            assert!(!opens(&activity.effects));
            daemon.step(Input::Lid { open: true }, now);
            advance_clock(&mut now, 2_500);
            assert!(opens(&daemon.step(Input::Tick, now).effects));
        }

        #[test]
        fn a_newer_preference_cancels_a_pending_open_and_restores_speaker_playback() {
            let mut now = clock();
            let mut a = peer(1, now);
            a.step(Input::Command(Command::Activity), now);
            a.step(Input::Lid { open: true }, now);
            advance_clock(&mut now, 2_500);
            let opened = a.step(Input::Tick, now);
            assert!(opens(&opened.effects));
            let retired = a.connection.generation();
            // Model the audio claim waiting on the opening connection. Losing
            // the preference releases this claim without pausing the player.
            a.output.claim();
            a.routing.claim_audio();
            a.audio.startup_players(&[crate::audio::play::PlayerSeen {
                service: "org.mpris.MediaPlayer2.test".into(),
                playing: true,
            }]);
            let mut graph = crate::audio::route::Graph {
                default_sink: "speakers".into(),
                sinks: vec![crate::audio::route::Sink {
                    name: "speakers".into(),
                    address: None,
                    profile: None,
                }],
                cards: vec![],
                streams: vec![crate::audio::route::LiveStream {
                    id: 9,
                    generation: 9,
                    sink: "speakers".into(),
                    role: "".into(),
                    node: "test".into(),
                    mute: false,
                    temporarily_muted: false,
                    volume: 0.7,
                    corked: false,
                }],
            };
            let held = a.step(Input::Audio(graph.clone()), now);
            assert!(held.effects.iter().any(|e| matches!(e, Effect::MuteStreams(_))));
            graph.streams[0].temporarily_muted = true;
            a.step(Input::Audio(graph), now);
            let mut b = peer(2, now);
            b.step(Input::Command(Command::Activity), now);
            let record = b.preferred_connector().unwrap();
            advance_clock(&mut now, crate::fleet::activity::REFRESH_MS);
            let frame =
                b.activity_preference.offer(record, addr(2), now, &keys_for_test()).unwrap();
            let heard = a.step(Input::Advertisement(frame.frame), now);
            assert!(heard.effects.iter().any(|e| matches!(e, Effect::Disconnect { .. })));
            assert!(!heard.effects.iter().any(|e| matches!(e, Effect::PausePlayers(_))));
            assert!(!a.output.wants_output());
            assert!(heard.effects.iter().any(|e| matches!(e, Effect::Restore(streams)
                if streams.iter().any(|stream| stream.id == 9 && !stream.mute && stream.volume == 0.7))));
            assert!(a.audio.local_playing());
            assert_ne!(a.connection.generation(), retired);
            let late = a.step(Input::ControlSocket { up: true, generation: retired }, now);
            assert!(!opens(&late.effects));
            advance_clock(&mut now, 120_000);
            assert!(!opens(&a.step(Input::Tick, now).effects));
            assert_eq!(a.preferred_connector().unwrap().owner, addr(2));
        }
    }
    mod shared_pod_states {
        use super::*;
        use crate::fleet::{members::Fleet, pod_state, radio};
        use crate::step::{Effect, Input};

        fn pair(now: Clock) -> (Daemon, Daemon) {
            let mut owner = Daemon::new("xps");
            owner.set_local_address(Some(addr(1)));
            ready_link(&mut owner, now);
            owner.note_ownership(true, now.mono);
            owner.note_source(true, 2, now.mono);
            owner.note_pod_peers(vec![addr(1)], now.unix);
            owner.accessory.note_placement(false, Some(true), Some(false), now.mono);
            owner.accessory.note_lid(LidState::Open, now.mono);
            let mut follower = Daemon::new("gram");
            follower.set_local_address(Some(addr(2)));
            follower.select_accessory(Some(addr(3)));
            follower.accessory.note_placement(false, Some(false), Some(true), now.mono);
            for daemon in [&mut owner, &mut follower] {
                daemon.restore_fleet(Fleet {
                    id: FleetId::new([3; 16]),
                    key: GroupKey::new([0x11; 32]),
                    members: vec![
                        Member { address: addr(1), label: "xps".into() },
                        Member { address: addr(2), label: "gram".into() },
                    ],
                });
            }
            owner.outbound = radio::Schedule::new();
            (owner, follower)
        }

        fn frame(owner: &mut Daemon, now: Clock) -> Vec<u8> {
            owner.offer_pod_state(now);
            match owner.outbound.due(now.mono) {
                radio::Duty::Publish(radio::Item::PodState(bytes)) => bytes,
                other => panic!("expected telemetry: {other:?}"),
            }
        }

        fn roster(owner: &mut Daemon, follower: &mut Daemon, now: Clock) {
            let mut roster = owner.roster_snapshot(now).unwrap().air;
            roster.sequence = 1;
            follower.observe_nearby(
                host_id(Some(addr(1))),
                addr(1),
                "xps",
                now.unix,
                true,
                Some(roster.session),
            );
            follower.hear_roster(roster, now);
        }

        #[test]
        fn a_disconnected_follower_uses_owner_state_then_falls_back_without_echo_or_connect() {
            let mut now = clock();
            let (mut owner, mut follower) = pair(now);
            let bytes = frame(&mut owner, now);
            let received = follower.step(Input::Advertisement(bytes.clone()), now);
            assert!(!received.effects.iter().any(|e| matches!(e, Effect::OpenLink { .. })));
            let generation = follower.connection.generation();
            // Telemetry cannot authorize itself before the matching seat roster arrives.
            assert!(follower.status_value(now)["fleet_pod_state"].is_null());
            roster(&mut owner, &mut follower, now);
            let status = follower.status_value(now);
            assert_eq!(status["left"]["in_ear"], true);
            assert_eq!(status["left"]["in_case"], false);
            assert_eq!(status["fleet_pod_state"]["reporter"], addr(1).display());
            assert_eq!(follower.accessory.left_placement.known().unwrap().in_case, Some(true));
            assert_eq!(follower.connection.generation(), generation);
            assert_ne!(follower.connection.stage(), Stage::Ready);
            // Its newer BLE placement cannot overrule a live owner on the panel.
            follower.accessory.note_placement(
                false,
                Some(false),
                Some(true),
                now.mono.saturating_add(100),
            );
            assert_eq!(follower.status_value(now)["left"]["in_ear"], true);
            follower.outbound = radio::Schedule::new();
            follower.offer_pod_state(now);
            assert_eq!(follower.outbound.due(now.mono), radio::Duty::Hold);
            advance_clock(&mut now, pod_state::TTL_MS);
            let fallback = follower.status_value(now);
            assert!(fallback["fleet_pod_state"].is_null());
            assert_eq!(fallback["left"]["in_case"], true);
            // A replay does not restart the freshness window.
            follower.hear_pod_state(&bytes, now);
            assert!(follower.status_value(now)["fleet_pod_state"].is_null());
        }

        #[test]
        fn normal_radio_rotation_delivers_changes_and_recovers_a_missed_snapshot() {
            let mut now = clock();
            let (mut owner, mut follower) = pair(now);
            let mut packets = 0;
            let mut learned = false;
            for tick in 0..40 {
                advance_clock(&mut now, 500);
                if tick == 20 {
                    owner.accessory.note_placement(false, Some(false), Some(true), now.mono);
                }
                let outbound = owner.step(Input::Tick, now);
                for effect in outbound.effects {
                    if let Effect::Advertise(bytes) = effect {
                        if bytes[0] == pod_state::VERSION {
                            packets += 1;
                            if packets == 1 {
                                continue;
                            } // lose a whole advertising slot
                        }
                        let received = follower.step(Input::Advertisement(bytes), now);
                        assert!(!received
                            .effects
                            .iter()
                            .any(|e| matches!(e, Effect::OpenLink { .. })));
                        assert!(!received.effects.iter().any(|e| matches!(e,
                            Effect::Advertise(bytes) if bytes[0] == pod_state::VERSION)));
                    }
                }
                if tick < 20 && follower.fleet_pod_state(now).is_some() {
                    assert_eq!(follower.status_value(now)["left"]["in_ear"], true);
                    learned = true;
                }
            }
            assert!(learned, "a periodic refresh must recover the missed first snapshot");
            assert!((3..=9).contains(&packets), "bounded telemetry slots: {packets}");
            assert!(follower.fleet_pod_state(now).is_some());
            assert_eq!(follower.status_value(now)["left"]["in_case"], true);
            assert_ne!(follower.connection.stage(), Stage::Ready);
        }

        #[test]
        fn seat_loss_and_explicit_roster_withdrawal_stop_sharing() {
            let mut now = clock();
            let (mut owner, mut follower) = pair(now);
            let bytes = frame(&mut owner, now);
            follower.hear_pod_state(&bytes, now);
            roster(&mut owner, &mut follower, now);
            assert!(follower.fleet_pod_state(now).is_some());
            advance_clock(&mut now, 1000);
            owner.note_ownership(false, now.mono);
            owner.accessory.note_remote_address(addr(2), now.mono);
            owner.outbound = radio::Schedule::new();
            owner.offer_pod_state(now);
            assert_eq!(owner.outbound.due(now.mono), radio::Duty::Hold);
            let mut withdrawn = owner.roster_snapshot(now).unwrap().air;
            withdrawn.sequence = 2;
            withdrawn.peers.clear();
            withdrawn.audio_source = None;
            follower.hear_roster(withdrawn, now);
            assert!(follower.fleet_pod_state(now).is_none());
        }

        #[test]
        fn changing_accessory_or_resetting_fleet_drops_shared_state() {
            for reset in [false, true] {
                let now = clock();
                let (mut owner, mut follower) = pair(now);
                follower.hear_pod_state(&frame(&mut owner, now), now);
                roster(&mut owner, &mut follower, now);
                assert!(follower.fleet_pod_state(now).is_some());
                if reset {
                    follower.reset_fleet();
                } else {
                    follower.select_accessory(Some(addr(9)));
                }
                assert!(follower.fleet_pod_state(now).is_none());
            }
        }
    }
    #[test]
    fn bluetooth_connect_defers_routine_broadcasts_without_blocking_audio_or_recovery() {
        use crate::step::{Effect, Input};
        let later = |ms| Clock::new(clock().mono.0 + ms, clock().unix.0 + ms);
        let advertises =
            |effects: &[Effect]| effects.iter().any(|e| matches!(e, Effect::Advertise(_)));
        for ending in ["connected", "failed", "canceled"] {
            let mut daemon = Daemon::new("xps");
            daemon.set_identity("AirPods", "A3064");
            daemon.set_local_address(Some(addr(1)));
            daemon.select_accessory(Some(addr(3)));
            assert!(advertises(&daemon.step(Input::Tick, clock()).effects));
            daemon.claim_playback(later(600));
            let start = daemon.step(Input::Tick, later(600));
            let release = start.effects.iter().position(|e| matches!(e, Effect::ReleaseAir));
            let open = start.effects.iter().position(|e| matches!(e, Effect::OpenLink { .. }));
            assert!(release.is_some() && open.is_some() && release < open);
            assert!(!advertises(&start.effects));
            assert!(daemon.listening, "incoming observations must keep flowing");
            let waiting = daemon.step(Input::Tick, later(3_600));
            assert!(!advertises(&waiting.effects));
            assert!(!waiting.effects.iter().any(|e| matches!(e, Effect::OpenLink { .. })));
            let generation = daemon.connection.generation();
            let input = match ending {
                "connected" => Input::Bluez(true),
                "failed" => Input::LinkFailed { generation, busy: false },
                _ => Input::Command(Command::Disconnect),
            };
            let ended = daemon.step(input, later(4_000));
            assert!(advertises(&ended.effects), "routine traffic resumes after {ending}");
            if ending == "connected" {
                assert!(ended.effects.iter().any(|e| matches!(e, Effect::ConnectTransport { .. })));
                assert_eq!(daemon.connection.stage(), Stage::Connecting);
            } else if ending == "failed" {
                let retry = daemon.step(Input::Tick, later(5_000));
                assert!(retry.effects.iter().any(|e| matches!(e, Effect::OpenLink { .. })));
                assert!(retry.effects.iter().any(|e| matches!(e, Effect::ReleaseAir)));
                assert!(!advertises(&retry.effects));
            }
        }
    }

    #[test]
    fn lifecycle_joins_profile_reports_and_exposes_its_blocking_gate() {
        use crate::pods::lifecycle::Profiles;
        use crate::step::{Effect, Input, TransportReply};
        let now = clock();
        let mut daemon = Daemon::new("xps");
        daemon.set_identity("AirPods", "A3064");
        daemon.select_accessory(Some(addr(3)));
        daemon.claim_playback(now);
        daemon.step(Input::Tick, now);
        let started = daemon.step(Input::Bluez(true), now);
        let id = started
            .effects
            .iter()
            .find_map(|e| match e {
                Effect::ConnectTransport { id, .. } => Some(*id),
                _ => None,
            })
            .unwrap();
        let mut observed = Profiles {
            accessory: addr(3),
            connected: true,
            services_resolved: false,
            handsfree: true,
            busy: false,
        };
        daemon.step(Input::BluetoothProfiles(observed), now);
        let waiting =
            daemon.step(Input::TransportReply { id, reply: TransportReply::Connected }, now);
        assert!(!waiting.effects.iter().any(|e| matches!(e, Effect::ConnectHandsfree { .. })));
        let status = daemon.status_value(now);
        assert_eq!(status["connection_lifecycle"]["state"], "completing_profiles");
        assert_eq!(status["connection_lifecycle"]["waiting_for"], serde_json::json!("output"));
        assert_eq!(status["connection_lifecycle"]["handsfree"], "waiting");
        observed.services_resolved = true;
        let ready = daemon.step(Input::BluetoothProfiles(observed), now);
        assert!(ready.effects.iter().any(|e| matches!(e, Effect::ConnectHandsfree { accessory, id: request } if *accessory == addr(3) && *request == id)));
        let duplicate = daemon.step(Input::BluetoothProfiles(observed), now);
        assert!(!duplicate.effects.iter().any(|e| matches!(e, Effect::ConnectHandsfree { .. })));
        daemon.step(Input::HandsfreeReply { id, ok: false }, now);
        assert_eq!(
            daemon.status_value(now)["connection_lifecycle"]["state"],
            "completing_profiles"
        );
        daemon.claim_playback(now);
        daemon.step(Input::Tick, now);
        let replacement = daemon.lifecycle.request_id().unwrap();
        assert_ne!(id, replacement);
        let stale =
            daemon.step(Input::TransportReply { id, reply: TransportReply::Connected }, now);
        assert!(!stale.effects.iter().any(|e| matches!(e, Effect::ConnectHandsfree { .. })));
    }

    #[test]
    fn a_play_opens_audio_alongside_an_authorized_control_attempt() {
        use crate::pods::link::{reach_stage, Stage};
        use crate::step::{Effect, Input};
        let now = clock();
        let mut daemon = Daemon::new("xps");
        daemon.set_identity("AirPods", "A3064");
        daemon.select_accessory(Some(addr(3)));
        daemon.claim_playback(now);
        assert_eq!(daemon.connection.stage(), Stage::Connecting);
        let start = daemon.step(Input::Tick, now);
        assert!(start.effects.iter().any(|e| matches!(e, Effect::OpenLink { .. })));
        // Audio waits for the link: paging the pods twice at once collides.
        assert!(!start.effects.iter().any(|e| matches!(e, Effect::ConnectTransport { .. })));
        let linked = daemon.step(Input::Bluez(true), now);
        assert!(linked.effects.iter().any(|e| matches!(e, Effect::ConnectTransport { .. })));
        let generation = daemon.connection.generation();
        daemon.note_socket(true, generation, now.mono);
        for stage in [Stage::Handshake, Stage::Features, Stage::Notifications] {
            assert!(reach_stage(&mut daemon.connection, stage, generation, now.mono));
            let next = daemon.step(Input::Tick, now);
            assert!(!next.effects.iter().any(|e| matches!(e, Effect::ConnectTransport { .. })));
        }
        // Control readiness must preserve an in-flight audio task and its completion ID.
        assert!(daemon.mark_control_ready(generation));
        let request = daemon.lifecycle.request_id();
        let ready = daemon.step(Input::Tick, now);
        assert!(!ready.effects.iter().any(|e| matches!(e, Effect::ConnectTransport { .. })));
        assert_eq!(daemon.lifecycle.request_id(), request);
        assert!(daemon.advance_connection_tasks(now.mono).is_empty());
    }

    #[test]
    fn early_audio_obeys_playback_intent_and_connection_authorization() {
        let now = clock();
        let mut daemon = Daemon::new("xps");
        daemon.select_accessory(Some(addr(3)));
        daemon.open_connection(now.mono);
        daemon.step(crate::step::Input::Tick, now);
        assert!(
            daemon.advance_connection_tasks(now.mono).is_empty(),
            "background control does not open audio"
        );
        daemon.output.claim();
        assert!(
            daemon.advance_connection_tasks(now.mono).is_empty(),
            "audio cannot page alongside the in-flight link"
        );
        daemon.step(crate::step::Input::Tick, now);
        daemon.keyring_locked = true;
        assert!(
            daemon.advance_connection_tasks(now.mono).is_empty(),
            "locked authorization blocks early audio"
        );
        daemon.keyring_locked = false;
        daemon.connection.wait_for_request();
        assert!(
            daemon.advance_connection_tasks(now.mono).is_empty(),
            "a canceled attempt cannot open audio"
        );
    }

    #[test]
    fn an_early_transport_that_arrives_is_not_reopened_at_control_readiness() {
        use crate::audio::route::{Graph, Sink};
        use crate::step::{Effect, Input};
        let now = clock();
        let mut daemon = Daemon::new("xps");
        daemon.set_identity("AirPods", "A3064");
        daemon.select_accessory(Some(addr(3)));
        daemon.claim_playback(now);
        daemon.step(Input::Tick, now);
        daemon.step(Input::Bluez(true), now);
        assert_eq!(
            daemon.lifecycle.progress().state,
            crate::pods::lifecycle::State::PreparingPaths
        );
        daemon.last_graph = Some(Graph {
            default_sink: "pods".into(),
            sinks: vec![Sink {
                name: "pods".into(),
                address: Some(addr(3)),
                profile: Some("a2dp-sink".into()),
            }],
            cards: vec![],
            streams: vec![],
        });
        let generation = daemon.connection.generation();
        daemon.note_socket(true, generation, now.mono);
        crate::pods::link::reach_stage(
            &mut daemon.connection,
            Stage::Notifications,
            generation,
            now.mono,
        );
        assert!(daemon.mark_control_ready(generation));
        assert!(!daemon
            .advance_connection_tasks(now.mono)
            .iter()
            .any(|e| matches!(e, Effect::ConnectTransport { .. })));
        daemon.step(Input::Transport { active: true }, now);
        assert_eq!(daemon.status_value(now)["audio_transport_active"], true);
        daemon.step(Input::Transport { active: false }, now);
        assert_eq!(daemon.status_value(now)["audio_transport_active"], false);
    }
    #[test]
    fn a_busy_audio_profile_is_asked_again_until_the_window_closes() {
        use crate::step::{Effect, Input, TransportReply};
        let asks = |effects: &[Effect]| {
            effects.iter().filter(|e| matches!(e, Effect::ConnectTransport { .. })).count()
        };
        let later = |now: Clock, ms: u64| Clock::new(now.mono.0 + ms, now.unix.0 + ms);
        let mut now = clock();
        let mut daemon = Daemon::new("xps");
        daemon.set_identity("AirPods", "A3064");
        daemon.select_accessory(Some(addr(3)));
        daemon.claim_playback(now);
        daemon.step(Input::Tick, now);
        assert_eq!(asks(&daemon.step(Input::Bluez(true), now).effects), 1);
        let generation = daemon.connection.generation();
        set_up_link(&mut daemon, generation, now);
        assert_eq!(daemon.connection.stage(), Stage::Ready);
        // A busy answer waits a beat, then asks once more.
        daemon.step(
            Input::TransportReply {
                id: daemon.lifecycle.request_id().unwrap(),
                reply: TransportReply::Busy,
            },
            now,
        );
        assert_eq!(asks(&daemon.step(Input::Tick, now).effects), 0);
        now = later(now, TRANSPORT_RETRY_MS);
        assert_eq!(asks(&daemon.step(Input::Tick, now).effects), 1);
        assert_eq!(asks(&daemon.step(Input::Tick, now).effects), 0);
        // So does a request that went unanswered.
        daemon.step(
            Input::TransportReply {
                id: daemon.lifecycle.request_id().unwrap(),
                reply: TransportReply::TimedOut,
            },
            now,
        );
        now = later(now, TRANSPORT_RETRY_MS);
        assert_eq!(asks(&daemon.step(Input::Tick, now).effects), 1);
        // Past the window the claim stops asking.
        now = later(now, TRANSPORT_RETRY_WINDOW_MS);
        daemon.step(
            Input::TransportReply {
                id: daemon.lifecycle.request_id().unwrap(),
                reply: TransportReply::Busy,
            },
            now,
        );
        now = later(now, TRANSPORT_RETRY_MS);
        assert_eq!(asks(&daemon.step(Input::Tick, now).effects), 0);
        // A new play is a new claim with its own window.
        daemon.claim_playback(now);
        assert_eq!(asks(&daemon.step(Input::Tick, now).effects), 1);
    }
}
