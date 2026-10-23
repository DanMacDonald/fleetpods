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
use crate::pods::setup::Setup;
use crate::types::{
    code, Address, Clock, Code, FleetId, Generation, HostId, InvitationId, LidState, Model, MonoMs,
    NoiseMode, UnixMs,
};

/// One pod's placement from an advertisement, given its current placement.
///
/// A frame comes from one pod: `primary_left` names it, and `this_in_case`
/// says whether that pod is in the case (XPS, 2026-10-19: the left pod
/// docked sent `primary_left` frames, the right pod docked did not). A docked
/// pod also sends frames claiming both pods are docked with neither in an ear,
/// while the other pod is worn; the next frame, 0.27 s later, shows it worn.
/// So that claim never docks a pod known to be in an ear. Otherwise a pod out
/// of an ear has an unknown case placement, never an older one.
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
    pub pause_players: Vec<String>,
    pub keyring_dirty: bool,
    pub follow_link: bool,
    pub announce_dirty: bool,
}

pub struct Daemon {
    pub(crate) local_name: String,
    pub(crate) local_address: Option<Address>,
    pub(crate) config: Config,
    pub(crate) bluetooth_bonded: Option<bool>,
    pub(crate) connection: Connection,
    pub(crate) accessory: Accessory,
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
    pub(crate) link_asked: Option<Generation>,
    pub(crate) effects: PendingEffects,
    pub(crate) commit: crate::fleet::commit::Commit,
    pub(crate) accessory_setup: Setup,
    priority_until: Option<MonoMs>,
    priority_generation: u64,
    /// The only seat record. Claim and step-back are its writers. Status reads it.
    pub(crate) seat: Seat,
    reconnect: crate::pods::reconnect::Reconnect,
    reconnect_retry_at: Option<MonoMs>,
    reconnect_retries: u8,
    /// Hold passive reconnects after yielding or losing a slot so the
    /// incoming device can finish connecting without being displaced.
    slot_hold_until: Option<MonoMs>,
    /// Who owned the audio while this laptop shared the pods with a fleet
    /// laptop, and whether this laptop is holding its automatic reconnect
    /// so that owner can come back first.
    pair_audio_owner: Option<Address>,
    reopen_wait: bool,
    reopen_hold_until: Option<MonoMs>,
    codec_asked: Option<String>,
    transport_asked: bool,
    pub(crate) staged_controls: Vec<Vec<u8>>,
    pub(crate) peers_seated: Vec<Address>,
    pub(crate) output: crate::audio::route::Router,
    pub(crate) last_graph: Option<crate::audio::route::Graph>,
    pub(crate) output_health: crate::audio::health::Health,
    pub(crate) rearm_output: Option<crate::audio::health::Output>,
    pub(crate) resume_output_on_grant: bool,
    /// The pods took ownership away since this laptop last relinked its output.
    pub(crate) rearm_after_loss: bool,
    /// Whether the accessory's A2DP transport is streaming.
    pub(crate) transport_active: bool,
    /// When to check that a rebuilt output actually started.
    pub(crate) rearm_verify_at: Option<MonoMs>,
    pub(crate) rearm_retries: u8,
    /// Until then, a player pausing itself is a side effect of the rebuild.
    pub(crate) rebuild_pause_until: Option<MonoMs>,
    /// Players to play again once the output rebuild is done.
    pub(crate) resume_players: Vec<String>,
    /// Players this daemon just played again; their Playing is not a new claim.
    pub(crate) resumed_players: Vec<String>,
    /// Until then, release the AirPods output once it goes idle after the
    /// pods moved to another computer.
    pub(crate) release_output_until: Option<MonoMs>,
    /// When a claim was sent as a new link became ready.
    pub(crate) setup_claim_at: Option<MonoMs>,
    /// When the control link last became ready.
    pub(crate) link_ready_at: Option<MonoMs>,
    /// When each pod (left, right) last advertised while sending the frame.
    pub(crate) advertised_self_at: [Option<MonoMs>; 2],
    /// When this laptop last sent a claim on the control link.
    pub(crate) claim_sent_at: Option<MonoMs>,
    /// A remote report arrived just after a claim; it ends the claim unless
    /// the pods grant it first.
    pub(crate) yield_pending: bool,
    /// Rebuild again even though Pulse reports the sink running.
    pub(crate) force_rearm: bool,
    pub(crate) output_restart_serial: u64,
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
    /// A conflicting authenticated roster asks us to verify this socket, not
    /// to trust the peer over a working local connection.
    roster_probe: Option<(Generation, MonoMs)>,
    roster_probe_after: Option<MonoMs>,
    /// An Apple device took the slot this laptop had been sitting in.
    /// A failed route, a closed case, and an idle link do not set this.
    displaced: bool,
    pub(crate) background_requested: bool,
    /// A play pressed without evidence the pods can play. The
    /// claim waits until they are available or connect, so the speakers are not
    /// soft-muted for a takeover that may never happen.
    deferred_playback_claim: Option<MonoMs>,
    case_opened_at: Option<MonoMs>,
    apple_labels: std::collections::HashMap<Address, String>,
    apple_labels_dirty: bool,
    introduced_peers: (Generation, Vec<Address>),
    peer_media_sent: std::collections::HashMap<Address, bool>,
    peer_playback: std::collections::HashMap<Address, (bool, MonoMs)>,
    idle_claimed: Option<(Address, MonoMs)>,
    activity_at: Option<MonoMs>,
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
    air: crate::fleet::roster_air::Snapshot,
}

#[derive(Clone, Copy)]
enum ActivityTarget {
    None,
    One(Address),
    Several,
}

/// Delay between automatic retries after a failed Apple reconnect, and how
/// many to attempt before waiting for activity again.
const RECONNECT_RETRY_DELAY_MS: u64 = 3_000;
const RECONNECT_RETRY_LIMIT: u8 = 3;

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
            local_name: local_name.into(),
            local_address: None,
            config: Config::new(),
            bluetooth_bonded: None,
            connection: Connection::new(),
            accessory: Accessory::new(),
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
            link_asked: None,
            effects: PendingEffects { announce_dirty: true, ..Default::default() },
            commit: crate::fleet::commit::Commit::new(),
            accessory_setup: Setup::new(),
            priority_until: None,
            priority_generation: 0,
            seat: Seat::new(),
            reconnect: crate::pods::reconnect::Reconnect::new(),
            reconnect_retry_at: None,
            reconnect_retries: 0,
            slot_hold_until: None,
            pair_audio_owner: None,
            reopen_wait: false,
            reopen_hold_until: None,
            codec_asked: None,
            transport_asked: false,
            staged_controls: Vec::new(),
            peers_seated: Vec::new(),
            output: crate::audio::route::Router::new(),
            last_graph: None,
            output_health: crate::audio::health::Health::default(),
            rearm_output: None,
            resume_output_on_grant: false,
            rearm_after_loss: false,
            transport_active: false,
            rearm_verify_at: None,
            rearm_retries: 0,
            rebuild_pause_until: None,
            resume_players: Vec::new(),
            resumed_players: Vec::new(),
            release_output_until: None,
            setup_claim_at: None,
            link_ready_at: None,
            advertised_self_at: [None, None],
            claim_sent_at: None,
            yield_pending: false,
            force_rearm: false,
            output_restart_serial: 0,
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
            roster_probe: None,
            roster_probe_after: None,
            displaced: false,
            background_requested: false,
            deferred_playback_claim: None,
            case_opened_at: None,
            apple_labels: std::collections::HashMap::new(),
            apple_labels_dirty: false,
            introduced_peers: (Generation(0), Vec::new()),
            peer_media_sent: std::collections::HashMap::new(),
            peer_playback: std::collections::HashMap::new(),
            idle_claimed: None,
            activity_at: None,
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
        }
        self.config.accessory = address;
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
        value["nearby_airpods"] = self.nearby_pods.status(now.mono);
        value["audio_output_health"] = serde_json::json!({
            "state": self.config.accessory.and_then(|a| self.output_health.output(a)).map(|o| o.state),
            "resets": self.output_health.resets,
            "last_reset_ok": self.output_health.last_ok,
        });
        value["pairing_candidates"] = serde_json::json!(if self.config.accessory.is_none() {
            self.nearby_pods.pairing_count(now.mono)
        } else {
            0
        });
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
            Command::Activity => self.activity(now),
            Command::Connect => self.connect(now),
            Command::AudioReset => self.reset_audio_output(now),
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
                self.effects.drop_link = true;
                self.interrupt("disconnect", now);
                Response::line("ok")
            }
            Command::Forget => self.forget(now),
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

    pub fn tick(&mut self, now: Clock) {
        self.expire_roster_probe(now);
        if self.config.accessory.is_none() && self.nearby_pods.refresh(now.mono) {
            if let Some(report) = self.nearby_pods.selected_report(now.mono) {
                self.accessory = report;
            }
        }
        self.accessory.expire(now.mono, self.connection.stage() == Stage::Ready);
        self.discovery.expire(now.unix);
        self.enrollment.on_tick(now.mono);
        if let Some(at) = self.deferred_playback_claim {
            if now.mono.saturating_since(at) > 30_000 || !self.audio.local_playing() {
                self.deferred_playback_claim = None;
            } else if !self.accessory.stowed()
                && !self.accessory.both_docked()
                && self.preferred_connection_allowed()
                && (self.accessory.playback_available(now.mono)
                    || self.connection.stage() == Stage::Ready)
            {
                self.deferred_playback_claim = None;
                self.claim_playback(now);
                // This play predates the link, so its opening reports do too.
                if self.output.wants_output()
                    && self.link_ready_at.is_some_and(|ready| {
                        now.mono.saturating_since(ready) < crate::step::SETUP_CLAIM_SETTLE_MS
                    })
                {
                    self.setup_claim_at = Some(now.mono);
                }
            }
        }
        if let Some(pending) = &self.pending_admission {
            let failure = if pending.release.is_none() && self.connection.stage() != Stage::Ready {
                Some(crate::fleet::route_air::Answer::Unavailable)
            } else if now.mono.0 >= pending.until.0 {
                Some(crate::fleet::route_air::Answer::TimedOut)
            } else {
                None
            };
            if let Some(reason) = failure {
                self.finish_admission(reason, now);
            }
        }
        let intent_valid = self.background_requested
            || self.routing.intent() == crate::fleet::route::Intent::Claim
            || matches!(
                self.routing.phase(),
                crate::fleet::route::Phase::Connecting
                    | crate::fleet::route::Phase::WaitingCompletion
            );
        if !self.hold_pairing_connection(now.mono)
            && !self.hold_stowed_connection()
            && !self.radio_quiet()
        {
            // A previously ready, idle connection may have rejoined at the
            // Bluetooth layer during wake. Restore its control session after
            // the grace without requiring a playback claim or peer admission.
            let resume_existing = self.preferred_connection_allowed()
                && self.connection.bluez() == Some(true)
                && self.connection.stage() == Stage::Interrupted
                && matches!(self.connection.reason(), "suspend" | "bluez" | "adapter");
            self.connection.on_tick(now.mono, intent_valid || resume_existing);
        }
        if self.connection.stage() == Stage::Failed {
            self.background_requested = false;
        }
        self.audio.touch_players(now.unix);
        if self.priority_until.is_some_and(|at| now.mono.0 >= at.0) {
            self.priority_until = None;
        }
        self.consider_capture_stall(now.mono);
        self.advance_reconnect(now);
        self.retry_failed_reconnect(now);
    }

    pub fn note_bluez(&mut self, up: bool) {
        self.connection.note_bluez(up);
    }

    pub fn note_profile_pending(&mut self, pending: bool) {
        self.connection.note_profile_pending(pending);
    }

    pub fn note_socket(&mut self, up: bool, generation: Generation, now: MonoMs) {
        if generation != self.connection.generation() {
            return;
        }
        if !up {
            if self.connection.stage() == Stage::Ready
                && !self.routing.phase().busy()
                && !self.connection.apple_reconnect()
            {
                let until = now.saturating_add(DISPLACEMENT_HOLD_MS);
                self.slot_hold_until = Some(self.slot_hold_until.map_or(until, |held| {
                    if held.0 > until.0 {
                        held
                    } else {
                        until
                    }
                }));
            }
            self.consider_apple_displacement();
            self.consider_reopen_order();
        }
        if self.connection.note_socket(up, generation) {
            self.background_requested = false;
            self.drop_reports();
        }
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

    pub fn mark_control_ready(&mut self, generation: Generation) -> bool {
        let ready = self.connection.mark_ready(generation);
        if ready {
            self.displaced = false;
            self.slot_hold_until = None;
            self.reopen_wait = false;
            self.reopen_hold_until = None;
            self.background_requested = false;
            self.microphone.set_control_ready(true);
        }
        ready
    }

    pub fn note_setup_failed(&mut self, generation: Generation, reason: FailReason, now: MonoMs) {
        let intent_valid = self.background_requested
            || self.routing.intent() == crate::fleet::route::Intent::Claim
            || matches!(
                self.routing.phase(),
                crate::fleet::route::Phase::Connecting
                    | crate::fleet::route::Phase::WaitingCompletion
            );
        let stage = self.connection.stage();
        self.connection.setup_failed(generation, reason, now, intent_valid);
        eprintln!(
            "fleetpodsd: setup failure generation={} stage={} reason={} intent={} next={} attempt={}",
            generation.0, stage.wire(), reason.label(), intent_valid,
            self.connection.stage().wire(), self.connection.retry().0
        );
        if self.connection.stage() == Stage::Failed {
            self.background_requested = false;
        }
    }

    pub fn interrupt(&mut self, reason: &'static str, now: Clock) {
        self.background_requested = false;
        self.connection.interrupt(reason);
        self.drop_reports();
        if reason == "disconnect" {
            self.step_back(now);
        }
        if matches!(reason, "suspend" | "bluez" | "adapter" | "disconnect") {
            self.routing.restart();
            if self.peers.has_air() {
                self.roster_dirty = true;
            }
            self.peers.invalidate();
        }
    }

    /// The link report is stale. The playback claim stays until the user releases it.
    fn drop_reports(&mut self) {
        self.roster_probe = None;
        self.roster_probe_after = None;
        self.resume_output_on_grant = false;
        self.end_capture();
        self.microphone.set_control_ready(false);
        self.accessory.drop_owner();
        self.peer_playback.clear();
        self.idle_claimed = None;
        self.activity_at = None;
        self.pod_peers.clear();
        self.pod_peers_known = false;
        self.pod_peers_at = None;
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
        self.hold_stowed_connection();
    }

    fn hold_stowed_connection(&mut self) -> bool {
        if !self.accessory.stowed() {
            return false;
        }
        self.background_requested = false;
        if self.connection.reason() != "disconnect" && self.connection.wait_for_request() {
            self.effects.drop_link = true;
            self.drop_reports();
        }
        if self.routing.phase().busy() {
            self.routing.restart();
        }
        true
    }

    pub(crate) fn pairing_elsewhere(&self, now: MonoMs) -> bool {
        self.config.accessory.is_some()
            && self.membership.accessory_keys_ready()
            && self.accessory.pairing_at.is_some_and(|at| now.saturating_since(at) < 3_000)
    }

    /// An already paired host must leave the accessory available for a new host.
    /// Retire queued attempts, but do not tear down an established connection.
    fn hold_pairing_connection(&mut self, now: MonoMs) -> bool {
        if !self.pairing_elsewhere(now) {
            return false;
        }
        self.background_requested = false;
        let asked = self.link_asked == Some(self.connection.generation());
        if self.connection.reason() != "disconnect" && self.connection.wait_for_request() {
            self.effects.drop_link |= asked;
            self.drop_reports();
        }
        if self.routing.phase().busy() {
            self.routing.restart();
        }
        true
    }

    fn open_connection(&mut self, now: MonoMs) {
        // A nearby advertisement is not a local Bluetooth pairing.
        if self.config.accessory.is_none() || self.hold_pairing_connection(now) {
            return;
        }
        if !self.hold_stowed_connection() {
            let not_before = self
                .case_opened_at
                .map(|at| at.saturating_add(crate::pods::link::CASE_WAKE_GRACE_MS))
                .unwrap_or(now);
            self.connection.claim_after(now, not_before);
        }
    }

    /// An existing Bluetooth ACL still needs a tracked, retryable control setup.
    /// Property updates must never reopen an established seat that was lost.
    pub(crate) fn attach_available(&mut self, generation: Generation, now: MonoMs) {
        if !self.preferred_connection_allowed() {
            if generation == self.connection.generation()
                && self.connection.bluez() == Some(true)
                && self.connection.stage() != Stage::Ready
            {
                self.effects.drop_link = true;
            }
            return;
        }
        if self.background_reopen_blocked(now) {
            return;
        }
        if generation != self.connection.generation()
            || !matches!(self.connection.stage(), Stage::Idle | Stage::Waiting)
            || self.connection.bluez() != Some(true)
            || self.routing.phase().busy()
            || self.radio_quiet()
            || self.hold_pairing_connection(now)
            || self.hold_stowed_connection()
        {
            return;
        }
        self.background_requested = true;
        self.open_connection(now);
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
                    self.outbound.offer_roster_change(frame);
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
        Some(PlannedRoster {
            fleet,
            session,
            accessory,
            peers: peers.clone(),
            audio_source,
            pending: false,
            audio_idle,
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
        self.accessory_setup = Setup::new();
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
        if self.pod_peers_known && self.pod_peers == peers {
            return;
        }
        self.pod_peers_known = true;
        self.pod_list_seen = true;
        self.peer_playback.retain(|peer, _| peers.contains(peer));
        self.pod_peers = peers;
        self.pod_peers_at = Some(now);
    }

    pub(crate) fn note_peer_playback(&mut self, peer: Address, playing: bool, now: MonoMs) {
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
        self.peer_playback.insert(peer, (playing, now));
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
        let playing = self.audio.local_playing() && !self.accessory.both_docked();
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
                if other == Answer::Unavailable && self.background_requested {
                    self.open_background_connection(now);
                } else {
                    self.background_requested = false;
                }
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

    pub fn set_accessory_keys(&mut self, keys: AccessoryKeys) {
        self.membership.set_accessory_keys(keys);
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
        if let Some(keys) = self.membership.accessory_keys() {
            let irk = keys.irk.to_vec();
            let encryption = keys.encryption.to_vec();
            self.effects.secret_out.push((crate::linux::keyring::IRK_ITEM.into(), irk));
            self.effects.secret_out.push((crate::linux::keyring::ENC_ITEM.into(), encryption));
        }
    }

    /// The keyring opened after a locked start: load what it holds, and save keys learned meanwhile.
    pub(crate) fn keyring_unlocked(&mut self, fleet: Option<Fleet>, keys: Option<AccessoryKeys>) {
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
            Some(keys) if self.membership.accessory_keys().is_none() => {
                self.set_accessory_keys(keys)
            }
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
        self.hold_stowed_connection();
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
        if crate::pods::air::pairing_model(payload).is_some() {
            let ours = self.config.accessory == Some(address)
                || self
                    .membership
                    .accessory_keys()
                    .is_some_and(|keys| crate::pods::air::resolves(&keys.irk, address));
            if ours && self.membership.accessory_keys_ready() {
                if !self.pairing_elsewhere(now.mono) {
                    eprintln!("fleetpodsd: known AirPods entered pairing mode; holding reconnects");
                }
                self.accessory.pairing_at = Some(now.mono);
                self.hold_pairing_connection(now.mono);
            }
            return;
        }
        let Some(sight) = crate::pods::air::parse(payload) else {
            return;
        };
        let keys = self.membership.accessory_keys();
        let irk_known = keys.is_some();
        let irk_match = keys.is_some_and(|keys| crate::pods::air::resolves(&keys.irk, address));
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
            self.note_ears(left_ear, right_ear, now.mono, now.mono);
            if decision.encrypted {
                if let Some(keys) = self.membership.accessory_keys() {
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
            if let Some(keys) = self.membership.accessory_keys() {
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

    pub fn note_ears(&mut self, left: Option<bool>, right: Option<bool>, at: MonoMs, now: MonoMs) {
        self.audio.ears(left, right, self.config.ear, at);
        self.audio.ears_tick(self.config.ear, now);
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
        if let Some(frame) = self.activity_preference.offer(record, local, now, &keys) {
            self.outbound.offer_activity(frame);
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

    /// The remembered host alone reacts to availability. A sleeping owner is
    /// never timed out in favor of a less recently used laptop.
    pub(crate) fn reconnect_preferred(&mut self, now: Clock) {
        let available = !self.accessory.stowed()
            && (self.accessory.lid.latest.as_fresh() == Some(&LidState::Open)
                || self.accessory.playback_available(now.mono));
        self.activity_preference.observe_availability(available, now.mono);
        if self.connection.stage() != Stage::Ready
            && !self.preferred_connection_allowed()
            && !self.connection.apple_reconnect()
        {
            let asked = self.link_asked == Some(self.connection.generation());
            if self.connection.reason() != "disconnect" && self.connection.wait_for_request() {
                self.effects.drop_link |= asked;
            }
            self.background_requested = false;
            self.deferred_playback_claim = None;
            if matches!(
                self.routing.phase(),
                Phase::WaitingAdmission | Phase::Connecting | Phase::WaitingCompletion
            ) {
                self.routing.restart();
            }
            if self.output.wants_output() {
                // A superseded pending connection must not leave speaker
                // playback muted while waiting for a link it cannot open.
                self.seat.step_back(now);
                self.routing.release_audio();
                self.resume_output_on_grant = false;
                self.output.release();
                self.replan(now);
            }
            return;
        }
        if !available
            || self.connection.reason() == "disconnect"
            || self.radio_quiet()
            || self.keyring_locked
            || !matches!(self.connection.stage(), Stage::Idle | Stage::Waiting | Stage::Interrupted)
            || self.routing.phase().busy()
            || self.connection.apple_reconnect()
            || self.outside_device_playing(now)
            || self.background_reopen_blocked(now.mono)
        {
            return;
        }
        let (Some(record), Some(local)) = (self.preferred_connector(), self.local_address) else {
            return;
        };
        if self.activity_preference.take_wake(record, local, now.mono) {
            self.connection.prepare();
            self.open_background_connection(now);
        }
    }

    fn activity(&mut self, now: Clock) -> Response {
        self.remember_activity(now);
        if self.config.accessory.is_none()
            || self.radio_quiet()
            || self.hold_pairing_connection(now.mono)
            || self.hold_stowed_connection()
            || self.routing.phase().busy()
            || self.connection.apple_reconnect()
            || self.connection.stage().is_opening()
            || self.connection.stage() == Stage::Retrying
        {
            return Response::line("ignored");
        }
        if self.connection.stage() == Stage::Ready {
            self.activity_at = Some(now.mono);
            return Response::line(if self.claim_idle_on_activity(now) {
                "accepted"
            } else {
                "ignored"
            });
        }
        // Connecting this laptop in the background takes one of the pods' two
        // connections. Input alone must not cost a playing phone or Mac its
        // connection (gram, 2026-10-19); a play here still takes over.
        if self.outside_device_playing(now) {
            eprintln!("fleetpodsd: activity ignored while a device outside the fleet plays on the AirPods");
            return Response::line("ignored");
        }
        // Input on an established link can claim idle audio immediately. Input
        // after a slot loss must not evict the device still taking that slot.
        if self.slot_hold_until.is_some_and(|until| now.mono.0 < until.0) {
            eprintln!("fleetpodsd: activity ignored while a yielded slot is held");
            return Response::line("ignored");
        }
        self.slot_hold_until = None;
        // Use a connected helper when present; otherwise open only the control link.
        let target = self.activity_target(now);
        // The reopen hold lets the pair's audio owner land first. A connected
        // fleet helper means it already has, so input asks that helper for a
        // seat instead of waiting out the hold.
        if matches!(target, ActivityTarget::One(_)) {
            self.reopen_wait = false;
            self.reopen_hold_until = None;
        } else if self.reopen_blocked(now.mono) {
            eprintln!("fleetpodsd: activity ignored while the audio owner reconnects first");
            return Response::line("ignored");
        }
        match target {
            ActivityTarget::Several => Response::line("ignored"),
            ActivityTarget::One(peer) => {
                self.connection.prepare();
                match self.routing.start(peer) {
                    crate::fleet::route::Start::Sent { .. }
                    | crate::fleet::route::Start::Duplicate => {
                        self.background_requested = true;
                        Response::line("accepted")
                    }
                    crate::fleet::route::Start::Busy => Response::line("ignored"),
                }
            }
            ActivityTarget::None => {
                self.connection.prepare();
                self.open_background_connection(now);
                Response::line("accepted")
            }
        }
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

    fn open_background_connection(&mut self, now: Clock) {
        if !self.preferred_connection_allowed() {
            return;
        }
        let out = |pod: &crate::types::Evidence<Pod>| {
            pod.as_fresh().is_some_and(|pod| pod.in_ear == Some(true) || pod.in_case == Some(false))
        };
        self.background_requested = self.config.accessory.is_some()
            && !self.radio_quiet()
            && !self.accessory.stowed()
            && (self.accessory.lid.latest.as_fresh() == Some(&LidState::Open)
                || out(&self.accessory.left)
                || out(&self.accessory.right));
        if self.background_requested {
            self.routing.clear_failure();
            self.displaced = false;
            self.open_connection(now.mono);
            eprintln!("fleetpodsd: activity opening the background control connection");
        }
    }

    /// Keep fresh input through the peer's pause debounce, never through a later session.
    pub(crate) fn claim_idle_on_activity(&mut self, now: Clock) -> bool {
        const ACTIVITY_GRACE_MS: u64 = 5_000;
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
            || self.accessory.both_docked()
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
            if let Some((false, _)) = self.remote_playback(peer, now) {
                if self.idle_claimed.is_none_or(|(previous, requested)| {
                    previous != peer || now.mono.saturating_since(requested) >= 2_000
                }) {
                    self.activity_at = None;
                    self.idle_claimed = Some((peer, now.mono));
                    self.effects
                        .control_out
                        .push((self.connection.generation(), crate::pods::setup::ownership(true)));
                    eprintln!("fleetpodsd: claiming idle AirPods ownership on desktop activity");
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

    fn connect(&mut self, now: Clock) -> Response {
        self.remember_activity(now);
        if self.hold_pairing_connection(now.mono) {
            return Response::line("error: AirPods are in pairing mode; finish pairing first");
        }
        if self.connection.apple_reconnect() {
            return Response::line("error: reconnect already in progress");
        }
        if self.routing.phase().busy() {
            if self.routing.phase() == Phase::WaitingCompletion
                && self.routing.queue_claim_until_route_lands()
            {
                return Response::line("accepted");
            }
            return Response::line("error: fleet routing in progress");
        }
        if self.config.accessory.is_none() {
            return Response::line("error: no paired AirPods address");
        }
        self.displaced = false;
        self.routing.claim_audio();
        self.open_connection(now.mono);
        Response::line("accepted")
    }

    fn claim_handoff(&mut self, now: Clock) -> Response {
        self.remember_activity(now);
        if self.routing.phase().busy() && self.routing.phase() != Phase::WaitingCompletion {
            return Response::line("fleet_routing_busy");
        }
        let linked = self.connection.socket_up() == Some(true);
        let packet = if linked {
            if self.priority_until.is_some() {
                return Response::line("request_pending");
            }
            let Some(local) = self.local_address else {
                return Response::line("control_unavailable");
            };
            let Some(packet) = crate::pods::setup::priority(local, None) else {
                return Response::line("invalid_priority_pair");
            };
            Some(packet)
        } else {
            None
        };
        self.claim_seat_user(now);
        if let Some(packet) = packet {
            self.queue_priority(packet, now);
            return Response::line("written_waiting_for_ack");
        }
        self.open_connection(now.mono);
        Response::line("queued")
    }

    /// Take the seat. Routing intent, the audio route, and the yield flag move together.
    fn claim_seat(&mut self, now: Clock) {
        self.seat.claim(now);
        self.apply_claim();
    }

    /// Take the seat for an explicit user switch.
    fn claim_seat_user(&mut self, now: Clock) {
        self.seat.claim_user(now);
        self.apply_claim();
    }

    fn apply_claim(&mut self) {
        self.routing.claim_audio();
        self.output.claim();
        self.transport_asked = false;
        self.audio.clear_yield();
    }

    /// Another laptop asked this one to leave so an Apple device can take the slot.
    pub(crate) fn free_slot(&mut self, peer: Address, now: Clock) {
        if self.local_address == Some(peer) || self.connection.apple_reconnect() {
            return;
        }
        let known = self.membership.contains(peer) || self.peers_seated.contains(&peer);
        if !known {
            return;
        }
        self.step_back(now);
        self.effects.drop_link = true;
        self.slot_hold_until = Some(now.mono.saturating_add(SLOT_HOLD_MS));
    }

    pub(crate) fn step_back(&mut self, now: Clock) {
        // Mute before the pause and before streams follow the default back
        // to the speakers: a player keeps playing for a moment after Pause.
        self.output.handoff(self.config.accessory, self.last_graph.as_ref(), now.mono);
        self.background_requested = false;
        self.deferred_playback_claim = None;
        self.resume_output_on_grant = false;
        self.seat.step_back(now);
        self.routing.release_audio();
        self.transport_asked = false;
        let players = self.audio.yield_playback();
        if !players.is_empty() {
            self.effects.pause_players = players;
        }
        self.replan(now);
    }

    pub(crate) fn seat_audio_failure(&mut self, reason: &'static str, now: Clock) {
        self.seat.note_failure(reason, now);
    }

    fn release(&mut self, now: Clock) -> Response {
        self.activity_at = None;
        self.idle_claimed = None;
        if self.routing.phase().busy() {
            return Response::line("fleet_routing_busy");
        }
        self.step_back(now);
        Response::line("ok")
    }

    fn reconnect_peer(&mut self, now: Clock) -> Response {
        if self.bluetooth_bonded == Some(false) {
            return Response::line("error: Bluetooth pairing was not saved. Pair the AirPods again before reconnecting.");
        }
        if self.routing.phase().busy() {
            return Response::line("error: fleet routing in progress");
        }
        match self.connection.begin_apple_reconnect() {
            Ok(()) => {
                self.routing.release_audio();
                self.background_requested = false;
                self.reconnect_retry_at = None;
                self.reconnect_retries = 0;
                self.seat.begin_reconnect(now);
                self.output.handoff(self.config.accessory, self.last_graph.as_ref(), now.mono);
                self.transport_asked = false;
                let players = self.audio.yield_playback();
                if !players.is_empty() {
                    self.effects.pause_players = players;
                }
                self.reconnect.begin(now.mono);
                Response::line("accepted")
            }
            Err(message) => Response::line(message.trim_end()),
        }
    }

    fn advance_reconnect(&mut self, now: Clock) {
        use crate::pods::reconnect::{Advance, Facts};
        let facts = Facts {
            quiet: !self.audio.local_playing(),
            peers_left: self.reconnect_peers().is_empty(),
            link_down: self.connection.socket_up() != Some(true)
                || self.connection.bluez() == Some(false),
            link_ready: self.connection.stage() == Stage::Ready
                && self.connection.socket_up() == Some(true)
                && self.connection.bluez() != Some(false),
            attempting: matches!(
                self.connection.stage(),
                Stage::Connecting
                    | Stage::Handshake
                    | Stage::Features
                    | Stage::Notifications
                    | Stage::Retrying
            ),
        };
        match self.reconnect.advance(now.mono, facts) {
            Advance::None => {}
            Advance::SendRequests => self.send_reconnect_requests(),
            Advance::DropLink => self.effects.drop_link = true,
            Advance::Rejoin => {
                self.background_requested = true;
                self.connection.rejoin_after_apple_reconnect(now.mono);
            }
            Advance::Done => self.seat.note_reconnected(now),
            Advance::Fail(reason) => self.fail_reconnect(reason, now),
        }
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

    fn fail_reconnect(&mut self, reason: &'static str, now: Clock) {
        self.connection.fail_apple_reconnect(reason);
        self.background_requested = false;
        self.reconnect.cancel();
        self.seat.note_failure(reason, now);
        // The attempt is over, but sitting disconnected until the next
        // activity helps nobody: retry the background connection on a
        // bounded schedule (see retry_failed_reconnect).
        self.reconnect_retries = 0;
        self.reconnect_retry_at = Some(now.mono.saturating_add(RECONNECT_RETRY_DELAY_MS));
    }

    /// Bounded automatic retry after a failed Apple reconnect: reopen the
    /// background control connection a few times, then go back to waiting
    /// for activity. Buds back in the case or a quiet radio end it sooner.
    fn retry_failed_reconnect(&mut self, now: Clock) {
        if self.connection.stage() == Stage::Ready {
            self.reconnect_retry_at = None;
            self.reconnect_retries = 0;
            return;
        }
        let Some(at) = self.reconnect_retry_at else {
            return;
        };
        if now.mono.0 < at.0 {
            return;
        }
        if self.reconnect_retries >= RECONNECT_RETRY_LIMIT
            || self.accessory.both_docked()
            || self.radio_quiet()
            || matches!(
                self.connection.stage(),
                Stage::Connecting | Stage::Handshake | Stage::Features | Stage::Notifications
            )
        {
            self.reconnect_retry_at = None;
            return;
        }
        self.reconnect_retries += 1;
        self.reconnect_retry_at = Some(now.mono.saturating_add(RECONNECT_RETRY_DELAY_MS));
        eprintln!(
            "fleetpodsd: automatically retrying the AirPods connection after a failed reconnect (attempt {})",
            self.reconnect_retries
        );
        self.open_background_connection(now);
        if !self.background_requested {
            self.reconnect_retry_at = None;
        }
    }

    fn can_rearm_output(&self) -> bool {
        self.connection.stage() == Stage::Ready
            && !self.suspending
            && !self.adapter_down
            && !self.routing.phase().busy()
            && !self.accessory.both_docked()
            && self.output.wants_output()
            && self.output.held_streams().is_empty()
            && !self.output.holding_takeover()
            && self.accessory.ownership.as_fresh() == Some(&true)
            && !(self.output_health.resets > 0 && self.output_health.last_ok.is_none())
    }

    fn reset_audio_output(&mut self, now: Clock) -> Response {
        if !self.can_rearm_output() {
            return Response::line("error: this machine must own the ready AirPods output, with no handoff or reset in progress");
        }
        let Some(output) =
            self.config.accessory.and_then(|a| self.output_health.output(a)).cloned()
        else {
            return Response::line("error: no AirPods audio output");
        };
        self.output_health.started(now.mono);
        self.rearm_output = Some(output);
        Response::line("accepted: reopening AirPods audio output")
    }

    /// The accessory's card and its active A2DP playback profile.
    pub(crate) fn playback_card(&self) -> Option<(String, String)> {
        let address = self.config.accessory?;
        let card =
            self.last_graph.as_ref()?.cards.iter().find(|card| card.address == Some(address))?;
        let profile = card.active.as_deref().filter(|profile| profile.starts_with("a2dp-sink"))?;
        Some((card.name.clone(), profile.to_string()))
    }

    pub(crate) fn pace_output_health(&mut self, now: Clock) -> Option<crate::step::Effect> {
        let eligible = self.can_rearm_output() && self.audio.local_playing();
        let output = self.rearm_output.take().or_else(|| {
            self.output_health.check(
                self.config.accessory,
                self.last_graph.as_ref(),
                eligible,
                now.mono,
            )
        })?;
        eprintln!(
            "fleetpodsd: reopening audio output sink={:?} state={} reset={}",
            output.name, output.state, self.output_health.resets
        );
        Some(crate::step::Effect::RearmOutput {
            id: self.output_health.resets,
            index: output.index,
            name: output.name,
        })
    }

    pub(crate) fn pace_audio(&mut self) -> Vec<crate::step::Effect> {
        use crate::step::Effect;
        let mut effects = Vec::new();
        let Some(address) = self.config.accessory else {
            return effects;
        };
        if !self.output.wants_output() || self.connection.stage() != Stage::Ready {
            return effects;
        }
        let card = self
            .last_graph
            .as_ref()
            .and_then(|graph| graph.cards.iter().find(|card| card.address == Some(address)));
        let transport_up = card
            .and_then(|card| card.active.as_deref())
            .is_some_and(|name| name.contains("a2dp-sink"))
            || self.last_graph.as_ref().is_some_and(|graph| {
                graph.sinks.iter().any(|sink| {
                    sink.address == Some(address)
                        && sink
                            .profile
                            .as_deref()
                            .is_some_and(|profile| profile.contains("a2dp-sink"))
                })
            });
        // Local playback must open A2DP even before an ownership report arrives.
        // A control-only link has no audio card until ConnectProfile succeeds;
        // waiting for a grant here can leave playback on the speakers forever.
        if !transport_up && !self.transport_asked {
            self.transport_asked = true;
            self.resume_output_on_grant = false;
            effects.push(Effect::ConnectTransport { accessory: address });
        }
        let (Some(profile), Some(card)) = (self.audio.selected(), card) else {
            return effects;
        };
        if card.active.as_deref() != Some(profile)
            && card.profiles.iter().any(|name| name == profile)
            && self.codec_asked.as_deref() != Some(profile)
        {
            self.codec_asked = Some(profile.to_string());
            effects.push(Effect::SelectProfile {
                card: card.name.clone(),
                profile: profile.to_string(),
            });
        }
        effects
    }

    pub(crate) fn note_profile(&mut self, ok: bool, now: Clock) {
        if ok {
            return;
        }
        self.codec_asked = None;
        self.seat.note_failure_if_empty("profile was not selected", now);
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

    /// Stream-only players can use ownership already confirmed by the pods.
    pub(crate) fn route_owned_playback(&mut self, graph: &crate::audio::route::Graph, now: Clock) {
        let Some(accessory) = self.config.accessory else {
            return;
        };
        if self.output.wants_output()
            || self.connection.stage() != Stage::Ready
            || self.audio.yielded()
            || self.accessory.both_docked()
        {
            return;
        }
        let resolved = self.resolved(now);
        if resolved.source != crate::audio::seat::Source::Direct
            || !matches!(resolved.holder, Holder::Local { .. })
        {
            return;
        }
        if crate::audio::route::playback_started(
            self.last_graph.as_ref(),
            graph,
            accessory,
            self.output.source_sink(),
        ) {
            self.remember_activity(now);
            self.claim_seat(now);
            eprintln!("fleetpodsd: routing new playback through already-owned AirPods");
        }
    }

    pub(crate) fn claim_playback(&mut self, now: Clock) {
        self.activity_at = None;
        if self.hold_pairing_connection(now.mono) {
            return;
        }
        // Pods stowed in a closed case cannot take audio, and no link will
        // be opened for them. Claiming anyway would soft-mute the speaker
        // streams for the whole takeover hold while nothing can arrive, so
        // local playback would sit silent for seconds before falling back.
        if self.accessory.stowed() || self.accessory.both_docked() {
            self.deferred_playback_claim = Some(now.mono);
            return;
        }
        if self.connection.stage() != Stage::Ready
            && self.connection.bluez() == Some(false)
            && !self.accessory.playback_available(now.mono)
        {
            // A historical sighting or an open case is not evidence that the
            // buds can play. Probe the link without muting speaker playback;
            // claim only when a bud is out or the control link becomes ready.
            self.deferred_playback_claim = Some(now.mono);
            self.open_connection(now.mono);
            return;
        }
        self.deferred_playback_claim = None;
        // Fleet admission only blocks opening a new link. A ready local link
        // can route a genuine play immediately, even before admission completes.
        if self.config.accessory.is_none() {
            return;
        }
        if self.routing.phase().busy() && self.connection.stage() != Stage::Ready {
            // Admission opens the link and the claim goes out once it is
            // ready. Hold speaker playback for it like any takeover, so the
            // play does not start on the speakers in the meantime.
            self.claim_seat(now);
            self.seat.note_claim(self.accessory.ownership.at());
            self.replan(now);
            return;
        }
        self.resume_output_on_grant = self.connection.stage() == Stage::Ready
            && (self.seat.yielded()
                || self.accessory.ownership.as_fresh() == Some(&false)
                || self.accessory.source.as_fresh().is_some_and(|source| !source.local));
        self.claim_seat(now);
        self.open_connection(now.mono);
        self.seat.note_claim(self.accessory.ownership.at());
        self.replan(now);
        // Reuse the live control link; a new link sends CLAIM once setup is ready.
        if self.connection.stage() == Stage::Ready {
            self.request_playback_ownership(now);
        }
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

    pub(crate) fn unpaired(&mut self, address: Address, now: Clock) {
        if self.config.accessory != Some(address) {
            return;
        }
        self.interrupt("disconnect", now);
        self.connection.wait_for_request();
        self.connection.note_bluez(false);
        self.select_accessory(None);
        self.reconnect.cancel();
        self.link_asked = None;
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
                both_docked: self.accessory.both_docked(),
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

    fn clock() -> Clock {
        Clock::new(1_000, 1_700_000_000_000)
    }

    fn addr(n: u8) -> Address {
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
        daemon.set_accessory_keys(keys.clone());
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
        assert_eq!(daemon.membership.accessory_keys(), Some(&keys));
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

        daemon.set_accessory_keys(AccessoryKeys { irk: [0x11; 16], encryption: [0x22; 16] });
        daemon.select_accessory(Some(addr(3)));
        daemon.step(Input::Proximity { address: rpa(), payload: captured_case(0x31) }, now);
        assert_eq!(daemon.status_value(now)["lid_state"], 1);
        assert_eq!(daemon.status_value(now)["last_advertisement_ms"], 1_700_000_000_000_u64);

        daemon.set_accessory_keys(AccessoryKeys {
            irk: (0u8..16).collect::<Vec<_>>().try_into().unwrap(),
            encryption: [0x22; 16],
        });
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
    fn disconnected_advertisements_replace_old_control_placement() {
        use crate::step::{Effect, Input};

        // Both laptops can lose their control slots; neither needs a seated
        // fleet peer to keep receiving placement and lid telemetry.
        for name in ["xps", "gram"] {
            for previously_docked in [[false, false], [true, false], [false, true], [true, true]] {
                let mut daemon = Daemon::new(name);
                let now = clock();
                ready_link(&mut daemon, now);
                daemon.set_accessory_keys(AccessoryKeys {
                    irk: (0u8..16).collect::<Vec<_>>().try_into().unwrap(),
                    encryption: [0x22; 16],
                });
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
        daemon.set_accessory_keys(AccessoryKeys {
            irk: (0u8..16).collect::<Vec<_>>().try_into().unwrap(),
            encryption: [0x22; 16],
        });
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
        daemon.set_accessory_keys(AccessoryKeys {
            irk: (0u8..16).collect::<Vec<_>>().try_into().unwrap(),
            encryption: [0x22; 16],
        });
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
        daemon.set_accessory_keys(AccessoryKeys {
            irk: (0u8..16).collect::<Vec<_>>().try_into().unwrap(),
            encryption: [0x22; 16],
        });
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
        daemon.set_accessory_keys(AccessoryKeys { irk: [1; 16], encryption: [2; 16] });
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
            daemon.set_accessory_keys(AccessoryKeys {
                irk: (0u8..16).collect::<Vec<_>>().try_into().unwrap(),
                encryption: [0x22; 16],
            });
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
        daemon.set_accessory_keys(AccessoryKeys {
            irk: (0u8..16).collect::<Vec<_>>().try_into().unwrap(),
            encryption: [0x22; 16],
        });
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
        assert_eq!(secret_writes(&first.effects), 2);
        // Every connection reports the keys; the keyring is not asked to replace the same items.
        let again =
            daemon.step(Input::Accessory { generation, packet: key_packet(0x11, 0x22) }, now);
        assert_eq!(secret_writes(&again.effects), 0);
        let changed =
            daemon.step(Input::Accessory { generation, packet: key_packet(0x33, 0x22) }, now);
        assert_eq!(secret_writes(&changed.effects), 2);
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
        assert_eq!(secret_writes(&opened.effects), 2, "keys learned while locked are saved");
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

        let mut daemon = Daemon::new("gram");
        daemon.set_identity("Pods", "A3064");
        daemon.set_local_address(Some(addr(1)));
        let now = clock();
        ready_link(&mut daemon, now);
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

    #[test]
    fn a_link_drop_stops_the_microphone_and_a_down_source_does_not_play() {
        use std::sync::atomic::Ordering;

        use crate::step::Input;

        let mut daemon = Daemon::new("gram");
        daemon.set_identity("Pods", "A3064");
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

    #[test]
    fn bluetooth_forget_stops_retries_and_restores_setup_with_saved_keys() {
        use crate::step::{Effect, Input};
        for established in [false, true] {
            let mut daemon = Daemon::new("xps");
            let now = Clock::new(1000, 1000);
            let pods = addr(3);
            let keys = AccessoryKeys { irk: [0x11; 16], encryption: [0x22; 16] };
            daemon.set_accessory_keys(keys.clone());
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
            assert_eq!(daemon.membership.accessory_keys(), Some(&keys));
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
    fn release_pauses_the_player_and_a_reconnect_writes_the_request() {
        use crate::audio::play::PlayerSeen;
        use crate::fleet::members::{GroupKey, Member, Proposal};
        use crate::step::{Effect, Input};
        use crate::types::FleetId;

        let mut daemon = Daemon::new("gram");
        let now = clock();
        let local = Address::parse("02:00:00:00:00:01").unwrap();
        let peer = Address::parse("02:00:00:00:00:02").unwrap();
        daemon.set_local_address(Some(local));
        ready_link(&mut daemon, now);
        daemon.set_local_address(Some(local));
        daemon.audio.startup_players(&[PlayerSeen {
            service: "org.mpris.MediaPlayer2.spotify".into(),
            playing: true,
        }]);
        daemon.output.claim();
        let released = daemon.step(Input::Command(Command::HandoffRelease), now);
        assert_eq!(released.reply.as_deref(), Some("ok\n"));
        assert!(released.effects.iter().any(|effect| matches!(
            effect,
            Effect::PausePlayers(names) if names == &["org.mpris.MediaPlayer2.spotify".to_string()]
        )));
        assert!(!daemon.output.wants_output());
        let still = daemon.step(
            Input::Player { service: "org.mpris.MediaPlayer2.spotify".into(), playing: Some(true) },
            now,
        );
        assert!(still.effects.iter().all(|effect| !matches!(effect, Effect::OpenLink { .. })));
        assert!(!daemon.output.wants_output());

        daemon.note_bluez(true);
        let away = Address::parse("02:00:00:00:00:09").unwrap();
        daemon
            .membership
            .apply(Proposal {
                id: FleetId::new([7; 16]),
                key: GroupKey::new([9; 32]),
                members: vec![
                    Member { address: local, label: "gram".into() },
                    Member { address: peer, label: "xps".into() },
                    Member { address: away, label: "away".into() },
                ],
            })
            .unwrap();
        // The direct AirPods roster is current; the legacy announcement is stale.
        daemon.peers_seated.push(away);
        daemon.note_pod_peers(vec![local, peer], now.unix);
        let intent = daemon.routing().intent();
        let started = daemon.step(Input::Command(Command::ReconnectPeer), now);
        assert_eq!(started.reply.as_deref(), Some("accepted\n"));
        assert_eq!(daemon.routing().intent(), intent);
        assert!(started.effects.iter().all(|effect| !matches!(effect, Effect::Disconnect { .. })));
        assert_eq!(daemon.status_value(now)["native_handoff"]["phase"], "reconnecting");
        let waiting = daemon.step(Input::Tick, Clock::new(now.mono.0 + 2_999, now.unix.0));
        assert!(waiting.effects.iter().all(|effect| !matches!(effect, Effect::Disconnect { .. })));
        daemon.step(
            Input::Player {
                service: "org.mpris.MediaPlayer2.spotify".into(),
                playing: Some(false),
            },
            Clock::new(now.mono.0 + 3_000, now.unix.0),
        );
        let asked = daemon.step(Input::Tick, Clock::new(now.mono.0 + 3_000, now.unix.0));
        let request = crate::pods::setup::reconnect_request(peer, local).unwrap();
        let other = crate::pods::setup::reconnect_request(away, local).unwrap();
        assert!(asked.effects.iter().any(|effect| matches!(
            effect,
            Effect::WriteControl { bytes, .. } if bytes == &request
        )));
        assert!(asked.effects.iter().all(|effect| !matches!(
            effect,
            Effect::WriteControl { bytes, .. } if bytes == &other
        )));
        assert!(asked.effects.iter().all(|effect| !matches!(effect, Effect::Disconnect { .. })));
        let still = daemon.step(Input::Tick, Clock::new(now.mono.0 + 4_000, now.unix.0));
        assert!(still.effects.iter().all(|effect| !matches!(effect, Effect::Disconnect { .. })));
        daemon.note_pod_peers(vec![local], now.unix);
        let dropped = daemon.step(Input::Tick, Clock::new(now.mono.0 + 4_100, now.unix.0));
        assert!(dropped.effects.iter().any(|effect| matches!(
            effect,
            Effect::Disconnect { accessory } if *accessory == addr(3)
        )));
        let generation = daemon.connection().generation();
        daemon.step(
            Input::ControlSocket { up: false, generation },
            Clock::new(now.mono.0 + 4_200, now.unix.0),
        );
        let disconnected_at = Clock::new(now.mono.0 + 4_300, now.unix.0);
        let disconnected = daemon.step(Input::Bluez(false), disconnected_at);
        assert!(disconnected
            .effects
            .iter()
            .all(|effect| !matches!(effect, Effect::RemoveDevice { .. })));
        assert_eq!(daemon.status_value(disconnected_at)["bluetooth_paired"], true);
        assert_eq!(daemon.status_value(disconnected_at)["native_handoff"]["phase"], "reconnecting");
        // The pods sit free for the rejoin delay first (giving a nearby
        // Apple device its chance to connect), then this laptop rejoins.
        let soon = daemon.step(Input::Tick, Clock::new(now.mono.0 + 9_200, now.unix.0));
        assert!(soon.effects.iter().all(|effect| !matches!(effect, Effect::FollowLink)));
        let back = daemon.step(Input::Tick, Clock::new(now.mono.0 + 14_300, now.unix.0));
        assert!(back.effects.iter().any(|effect| matches!(effect, Effect::OpenLink { .. })));
        assert!(daemon.background_requested);
        assert_eq!(daemon.routing().intent(), crate::fleet::route::Intent::Release);

        // A reset while rejoining is a failed setup, not the intentional drop.
        let generation = daemon.connection.generation();
        let failed_at = Clock::new(now.mono.0 + 14_400, now.unix.0);
        daemon.step(Input::ControlSocket { up: false, generation }, failed_at);
        daemon.step(Input::LinkFailed { generation, busy: false }, failed_at);
        assert_eq!(daemon.connection.stage(), Stage::Retrying);
        let retried = daemon.step(Input::Tick, Clock::new(now.mono.0 + 15_400, now.unix.0));
        assert!(retried.effects.iter().any(|effect| matches!(effect, Effect::OpenLink { .. })));
        assert_eq!(daemon.connection.retry().0, 2);
        assert!(daemon.background_requested);
        assert_eq!(daemon.routing().intent(), crate::fleet::route::Intent::Release);

        // An attempt still in flight is not failed by the reconnect's
        // return wait, however long the link layer takes with it; when it
        // lands, the reconnect completes instead of recording a failure.
        daemon.step(Input::Tick, Clock::new(now.mono.0 + 30_000, now.unix.0));
        assert_eq!(daemon.connection.stage(), Stage::Connecting);
        let generation = daemon.connection.generation();
        let landed = Clock::new(now.mono.0 + 30_100, now.unix.0);
        daemon.step(Input::ControlSocket { up: true, generation }, landed);
        daemon.step(
            Input::Accessory { generation, packet: vec![0x01, 0x00, 0x04, 0x00, 0x01] },
            landed,
        );
        daemon.step(
            Input::Accessory { generation, packet: vec![0x04, 0x00, 0x04, 0x00, 0x2b, 0x00, 0x01] },
            landed,
        );
        let mut battery = vec![0x04, 0x00, 0x04, 0x00, 0x04, 0x00, 0x03];
        battery.extend_from_slice(&[0x04, 0x01, 80, 0x02, 0x01]);
        battery.extend_from_slice(&[0x02, 0x01, 60, 0x01, 0x01]);
        battery.extend_from_slice(&[0x08, 0x01, 40, 0x02, 0x01]);
        daemon.step(Input::Accessory { generation, packet: battery }, landed);
        daemon.step(
            Input::Accessory {
                generation,
                packet: vec![0x04, 0x00, 0x04, 0x00, 0x06, 0x00, 0x00, 0x02],
            },
            landed,
        );
        assert_eq!(daemon.connection.stage(), Stage::Ready);
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
    fn a_failed_reconnect_retries_the_background_connection_on_its_own() {
        use crate::step::Input;

        let mut daemon = Daemon::new("xps");
        let now = clock();
        ready_link(&mut daemon, now);
        daemon.note_lid(true, now.mono);
        let generation = daemon.connection().generation();
        daemon.note_socket(false, generation, now.mono);
        daemon.fail_reconnect("link did not return", now);
        assert!(daemon.reconnect_retry_at.is_some());

        let early = Clock::new(now.mono.0 + 1_000, now.unix.0 + 1_000);
        daemon.step(Input::Tick, early);
        assert_eq!(daemon.reconnect_retries, 0, "no retry before the delay");

        let due = Clock::new(
            now.mono.0 + RECONNECT_RETRY_DELAY_MS + 100,
            now.unix.0 + RECONNECT_RETRY_DELAY_MS + 100,
        );
        daemon.step(Input::Tick, due);
        assert_eq!(daemon.reconnect_retries, 1, "the retry fires on its own");
        assert!(daemon.background_requested, "the retry opens the background connection");
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
    fn the_preferred_connector_never_falls_back_to_the_other_pairs_member() {
        use crate::fleet::members::Fleet;
        let setup = |owner_local: Option<bool>| {
            let mut daemon = Daemon::new("gram");
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
            daemon.select_accessory(Some(addr(3)));
            daemon.note_bluez(true);
            daemon.note_pod_peers(vec![local, peer], now.unix);
            if let Some(local_owner) = owner_local {
                daemon.note_ownership(local_owner, now.mono);
                daemon.activity_preference.note_local(
                    [3; 16],
                    addr(3),
                    if local_owner { local } else { peer },
                    now.unix,
                );
            }
            daemon.note_socket(false, daemon.connection().generation(), now.mono);
            daemon
        };
        let now = clock();
        // The preferred peer keeps its preference even after the old grace window.
        let mut daemon = setup(Some(false));
        let generation = daemon.connection().generation();
        daemon.attach_available(generation, now.mono.saturating_add(1_000));
        assert!(!daemon.background_requested, "the non-owner does not race the reopen");
        daemon.attach_available(generation, now.mono.saturating_add(REOPEN_HOLD_MS + 1_000));
        assert!(!daemon.background_requested, "there is no timeout fallback");
        // The owner itself reconnects at once.
        let mut daemon = setup(Some(true));
        let generation = daemon.connection().generation();
        daemon.attach_available(generation, now.mono.saturating_add(1_000));
        assert!(daemon.background_requested, "the owner reconnects first");
        // With no activity record, an enrolled fleet waits for actual input.
        let mut daemon = setup(None);
        let generation = daemon.connection().generation();
        daemon.attach_available(generation, now.mono.saturating_add(1_000));
        assert!(!daemon.background_requested, "unknown preference must not cause a race");
    }

    #[test]
    fn a_yielded_slot_stays_yielded_until_the_hold_passes() {
        let mut daemon = Daemon::new("gram");
        let now = clock();
        let local = Address::parse("02:00:00:00:00:01").unwrap();
        let peer = Address::parse("02:00:00:00:00:02").unwrap();
        daemon.set_local_address(Some(local));
        daemon.select_accessory(Some(addr(3)));
        daemon.note_bluez(true);
        daemon.peers_seated.push(peer);

        daemon.free_slot(peer, now);
        assert!(daemon.effects.drop_link);
        assert!(daemon.slot_hold_until.is_some());

        let generation = daemon.connection().generation();
        daemon.attach_available(generation, now.mono.saturating_add(1_000));
        assert!(!daemon.background_requested, "no auto-attach during the hold");

        daemon.attach_available(generation, now.mono.saturating_add(SLOT_HOLD_MS + 1));
        assert!(daemon.background_requested, "auto-attach resumes after the hold");
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
        }
        let settled = Clock::new(until.0, now.unix.0 + DISPLACEMENT_HOLD_MS);
        let outcome = daemon.step(Input::Command(Command::Activity), settled);
        assert!(outcome.effects.iter().any(|e| matches!(e, Effect::OpenLink { .. })));
    }

    #[test]
    fn a_yielded_slot_holds_activity_but_explicit_connect_and_play_can_reopen() {
        use crate::step::{Effect, Input};
        for explicit_play in [false, true] {
            let mut daemon = Daemon::new("xps");
            let now = clock();
            ready_link(&mut daemon, now);
            daemon.set_local_address(Some(addr(1)));
            daemon.peers_seated.push(addr(2));
            daemon.step(Input::Players(Vec::new()), now);
            daemon.free_slot(addr(2), now);
            let generation = daemon.connection.generation();
            daemon.step(Input::ControlSocket { up: false, generation }, now);
            assert_eq!(daemon.slot_hold_until, Some(now.mono.saturating_add(SLOT_HOLD_MS)));
            let activity = daemon.step(Input::Command(Command::Activity), now);
            assert!(!activity.effects.iter().any(|e| matches!(e, Effect::OpenLink { .. })));
            assert!(!daemon.background_requested);
            let input = if explicit_play {
                Input::Player { service: "browser".into(), playing: Some(true) }
            } else {
                Input::Command(Command::Connect)
            };
            let requested = daemon.step(input, now);
            assert!(requested.effects.iter().any(|e| matches!(e, Effect::OpenLink { .. })));
        }
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
        use crate::pods::link::OpenStep;
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
                },
                now,
            );
            daemon.peers_seated.push(gram);
            let Start::Sent { tx } = daemon.start_route(gram) else { panic!("route") };
            assert_eq!(daemon.routing.admit(tx, true, Some(retained)), Admit::Beside);
            daemon.open_connection(now.mono);
            let generation = daemon.connection.generation();
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
            assert!(daemon.connection.advance(OpenStep::Notifications, generation, now.mono));
            assert!(daemon.connection.mark_ready(generation));
            assert_eq!(
                daemon.status_value(now)["connected_peers"],
                serde_json::json!([gram.display(), local.display()])
            );
        }
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
            },
            now,
        );
        assert_eq!(daemon.roster_peers(now), vec![local, gram]);
        assert_eq!(
            daemon.status_value(now)["connected_peers"],
            serde_json::json!([local.display(), gram.display()])
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

    fn conflicting_roster_setup() -> Daemon {
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

    fn conflicting_roster(at: Clock, sequence: u64) -> crate::fleet::roster_air::Snapshot {
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
            Effect::SelectProfile { card, profile }
                if card == "bluez_card.reported" && profile == "a2dp-sink-sbc_xq"
        )));
        assert!(chosen.effects.iter().any(|effect| matches!(
            effect,
            Effect::ConnectTransport { accessory } if *accessory == pods
        )));
        let again = daemon.step(Input::Tick, now);
        assert!(again.effects.iter().all(|effect| !matches!(
            effect,
            Effect::SelectProfile { .. } | Effect::ConnectTransport { .. }
        )));
        let failed = daemon.step(Input::ProfileResult { ok: false }, now);
        assert_eq!(
            daemon.status_value(now)["native_handoff"]["failure"],
            "profile was not selected"
        );
        assert!(failed.effects.iter().any(|effect| matches!(effect, Effect::SelectProfile { .. })));
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

    #[test]
    fn output_recovery_is_local_bounded_and_preserves_pairing() {
        use crate::audio::health::Output;
        use crate::audio::route::{Graph, LiveStream, Sink};
        use crate::step::{Effect, Input};
        for scenario in ["fault", "healthy", "paused", "peer", "sleep"] {
            let mut daemon = Daemon::new("xps");
            let now = clock();
            ready_link(&mut daemon, now);
            daemon.step(Input::Bonded { address: addr(3), saved: true }, now);
            daemon
                .step(Input::Observation(crate::pods::observe::Observation::Ownership(true)), now);
            daemon.step(
                Input::Player {
                    service: "org.mpris.MediaPlayer2.brave".into(),
                    playing: Some(false),
                },
                now,
            );
            daemon.step(
                Input::Player {
                    service: "org.mpris.MediaPlayer2.brave".into(),
                    playing: Some(true),
                },
                now,
            );
            let graph = Graph {
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
                    volume: 1.0,
                    corked: false,
                }],
            };
            daemon.step(Input::Audio(graph), now);
            assert!(daemon.can_rearm_output());
            daemon.step(
                Input::Outputs(vec![Output {
                    name: "pods".into(),
                    index: 7,
                    address: Some(addr(3)),
                    profile: Some("a2dp-sink".into()),
                    state: if scenario == "healthy" { 0 } else { 2 },
                    muted: false,
                    audible_volume: true,
                }]),
                now,
            );
            match scenario {
                "paused" => {
                    daemon.audio.update_player("org.mpris.MediaPlayer2.brave", Some(false));
                }
                "peer" => {
                    daemon.step(
                        Input::Observation(crate::pods::observe::Observation::Ownership(false)),
                        now,
                    );
                }
                "sleep" => {
                    daemon.step(Input::Suspend, now);
                }
                _ => {}
            }
            daemon.step(Input::Tick, now);
            let later = Clock::new(now.mono.0 + 3_100, now.unix.0 + 3_100);
            let result = daemon.step(Input::Tick, later);
            assert_eq!(
                result.effects.iter().any(|e| matches!(e, Effect::RearmOutput { index: 7, .. })),
                scenario == "fault",
                "{scenario}"
            );
            assert!(
                !result.effects.iter().any(|e| matches!(
                    e,
                    Effect::RemoveDevice { .. }
                        | Effect::Disconnect { .. }
                        | Effect::WriteSecret { .. }
                )),
                "{scenario}"
            );
            assert_eq!(daemon.config.accessory, Some(addr(3)));
            assert_eq!(daemon.status_value(later)["bluetooth_bonded"], true);
            if scenario == "healthy" {
                let manual = daemon.step(Input::Command(Command::AudioReset), later);
                assert!(manual.reply.unwrap().starts_with("accepted:"));
                assert!(manual
                    .effects
                    .iter()
                    .any(|e| matches!(e, Effect::RearmOutput { index: 7, .. })));
                assert!(!manual
                    .effects
                    .iter()
                    .any(|e| matches!(e, Effect::Disconnect { .. } | Effect::RemoveDevice { .. })));
            }
            if scenario == "fault" {
                let duplicate = daemon.step(Input::Command(Command::AudioReset), later);
                assert!(duplicate.reply.unwrap().starts_with("error:"));
                assert!(!duplicate.effects.iter().any(|e| matches!(e, Effect::RearmOutput { .. })));
                daemon.step(Input::OutputRearmed { id: 99, ok: true }, later);
                assert_eq!(daemon.output_health.last_ok, None);
                daemon.step(Input::OutputRearmed { id: 1, ok: true }, later);
                assert_eq!(daemon.output_health.last_ok, Some(true));
            }
        }
    }

    fn ready_link(daemon: &mut Daemon, now: Clock) {
        use crate::step::Input;
        daemon.select_accessory(Some(addr(3)));
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
        daemon.step(
            Input::Accessory {
                generation,
                packet: vec![0x04, 0x00, 0x04, 0x00, 0x06, 0x00, 0x00, 0x02],
            },
            now,
        );
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
                    wire_session: Some([4; 16]),
                    last_use: UnixMs(0),
                });
            }
            daemon.step_back(now);
            daemon.displaced = true;
            daemon.accessory.left = crate::types::Evidence::fresh(
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
    fn activity_asks_the_connected_audio_owner_instead_of_waiting_out_the_reopen_hold() {
        use crate::fleet::members::{Fleet, Member};
        use crate::step::Input;
        for owner_connected in [true, false] {
            let now = clock();
            let local = addr(1);
            let xps = addr(2);
            let pods = addr(9);
            let mut daemon = Daemon::new("gram");
            daemon.set_local_address(Some(local));
            daemon.select_accessory(Some(pods));
            daemon.restore_fleet(Fleet {
                id: FleetId::new([3; 16]),
                key: GroupKey::new([0x11; 32]),
                members: vec![
                    Member { address: local, label: "gram".into() },
                    Member { address: xps, label: "xps".into() },
                ],
            });
            daemon.observe_nearby(host_id(Some(xps)), xps, "xps", now.unix, true, Some([4; 16]));
            daemon.note_roster(RosterReport {
                reporter: host_id(Some(xps)),
                reporter_address: xps,
                accessory: pods,
                session: crate::types::SessionId(0),
                seq: 1,
                at: now.mono,
                sent: now.unix,
                peers: if owner_connected { vec![xps] } else { Vec::new() },
                audio_source: owner_connected.then_some(xps),
                pending: false,
                audio_idle: false,
                wire_session: Some([4; 16]),
                last_use: UnixMs(0),
            });
            // This laptop's link dropped while the XPS owned the pair's audio.
            daemon.reopen_wait = true;
            daemon.accessory.left = crate::types::Evidence::fresh(
                Pod {
                    battery: crate::types::Battery::new(60, false).unwrap(),
                    in_ear: Some(true),
                    in_case: Some(false),
                },
                now.mono,
            );
            daemon.step(Input::Command(Command::Activity), now);
            if owner_connected {
                // The owner already landed: ask it for a seat alongside it.
                assert_eq!(daemon.routing.phase(), Phase::WaitingAdmission);
                assert!(!daemon.reopen_wait);
            } else {
                // The owner is not back yet, so it still gets to land first.
                assert!(!daemon.routing.phase().busy());
                assert!(daemon.reopen_wait);
                assert!(!daemon.background_requested);
            }
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
            daemon.set_accessory_keys(AccessoryKeys {
                irk: (0u8..16).collect::<Vec<_>>().try_into().unwrap(),
                encryption: [0x22; 16],
            });
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
    fn an_idle_laptop_does_not_connect_when_the_case_opens_and_a_peer_connects() {
        use crate::step::{Effect, Input};
        let mut daemon = conflicting_roster_setup();
        let now = clock();
        let generation = daemon.connection.generation();
        daemon.step(Input::ControlSocket { up: false, generation }, now);
        daemon.step(Input::Bluez(false), now);
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
        daemon.step(Input::Lid { open: false }, now);
        assert_eq!(daemon.connection.stage(), Stage::Waiting);
        // The other laptop and phone take the slots. Repeat open-lid reports
        // through both the case-wake and former reconnect-order grace periods.
        for elapsed in [1_000, 3_000, 10_000, 30_000, 60_000] {
            let at = Clock::new(now.mono.0 + elapsed, now.unix.0 + elapsed);
            daemon.hear_roster(conflicting_roster(at, elapsed), at);
            for input in [Input::Lid { open: true }, Input::Tick] {
                let result = daemon.step(input, at);
                assert!(!result
                    .effects
                    .iter()
                    .any(|e| matches!(e, Effect::OpenLink { .. } | Effect::WriteControl { .. })));
            }
            assert!(!daemon.background_requested);
            assert_eq!(daemon.connection.stage(), Stage::Waiting);
            assert_eq!(daemon.status_value(at)["lid_state"], 0);
        }
    }

    #[test]
    fn activity_after_lid_wake_and_existing_acl_share_bounded_setup_retries() {
        use crate::step::{Effect, Input};
        // Activity after opening the lid and attachment to an existing ACL
        // both need tracked intent for the retry loop.
        for (existing_acl, released) in [(false, false), (false, true), (true, false), (true, true)]
        {
            let mut daemon = Daemon::new("xps");
            daemon.select_accessory(Some(addr(9)));
            if released {
                // Reproduce the original reset after an earlier audio yield.
                daemon.routing.release_audio();
            }
            let mut now = clock();
            let no_open =
                |effects: &[Effect]| !effects.iter().any(|e| matches!(e, Effect::OpenLink { .. }));
            if existing_acl {
                daemon.step(Input::Bluez(true), now);
                let generation = daemon.connection.generation();
                let attach = daemon.step(Input::AttachAvailable { generation }, now);
                assert!(!no_open(&attach.effects));
            } else {
                daemon.connection.prepare();
                let opening = daemon.step(Input::Lid { open: true }, now);
                assert!(no_open(&opening.effects));
                daemon.step(Input::Command(Command::Activity), now);
                let start = now;
                // Repeated advertisements neither bypass nor restart the grace.
                for elapsed in [500, 1_999] {
                    now = Clock::new(start.mono.0 + elapsed, start.unix.0 + elapsed);
                    for input in [Input::Lid { open: true }, Input::Tick] {
                        assert!(no_open(&daemon.step(input, now).effects));
                    }
                }
                now = Clock::new(start.mono.0 + 2_000, start.unix.0 + 2_000);
                assert!(!no_open(&daemon.step(Input::Tick, now).effects));
            }
            assert!(daemon.background_requested);
            assert!(!daemon.output.wants_output(), "background setup must not claim audio");
            for attempt in 1..=crate::pods::link::RETRY_LIMIT {
                let generation = daemon.connection.generation();
                daemon.step(Input::ControlSocket { up: false, generation }, now);
                daemon.step(Input::Bluez(false), now);
                daemon.step(Input::LinkFailed { generation, busy: false }, now);
                if attempt == crate::pods::link::RETRY_LIMIT {
                    assert_eq!(daemon.connection.stage(), Stage::Failed);
                    assert!(!daemon.background_requested);
                    now = Clock::new(now.mono.0 + 120_000, now.unix.0 + 120_000);
                    daemon.step(Input::Bluez(true), now);
                    assert!(no_open(
                        &daemon.step(Input::AttachAvailable { generation }, now).effects
                    ));
                    assert!(no_open(&daemon.step(Input::Tick, now).effects));
                } else {
                    assert_eq!(daemon.connection.stage(), Stage::Retrying);
                    let delay = 1_000 << (attempt - 1);
                    let early = Clock::new(now.mono.0 + delay - 1, now.unix.0 + delay - 1);
                    assert!(no_open(&daemon.step(Input::Tick, early).effects));
                    now = Clock::new(now.mono.0 + delay, now.unix.0 + delay);
                    let retry = daemon.step(Input::Tick, now);
                    assert!(!no_open(&retry.effects), "retry needs no second activity event");
                    assert_eq!(daemon.connection.retry().0, attempt + 1);
                    let current = daemon.connection.generation();
                    assert_ne!(current, generation);
                    daemon.step(Input::LinkFailed { generation, busy: false }, now);
                    assert_eq!(daemon.connection.generation(), current);
                    assert_eq!(daemon.connection.stage(), Stage::Connecting);
                }
            }
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
    fn playback_with_no_sign_of_the_pods_defers_the_claim_until_they_appear() {
        let mut daemon = Daemon::new("omarchy-xps");
        let now = clock();
        daemon.select_accessory(Some(addr(9)));
        daemon.connection.note_bluez(false);
        assert!(!daemon.accessory.playback_available(now.mono));
        daemon.handle(play("deferred"), now);
        // Playing, but never seen this session: no claim, no muted speakers.
        assert!(!daemon.output.wants_output());
        assert!(daemon.deferred_playback_claim.is_some());
        // Still nothing seen: a tick does not claim either.
        daemon.step(crate::step::Input::Tick, now);
        assert!(!daemon.output.wants_output());
        // Opening the case alone must not silence the speakers.
        daemon.note_lid(true, now.mono);
        daemon.step(crate::step::Input::Tick, now);
        assert!(!daemon.output.wants_output());
        daemon.accessory.note_placement(false, Some(true), Some(false), now.mono);
        daemon.step(crate::step::Input::Tick, now);
        assert!(daemon.output.wants_output());
        assert!(daemon.deferred_playback_claim.is_none());
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
        daemon.note_peer_playback(peer, true, now.mono);
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
    fn a_player_that_pauses_itself_as_the_output_is_rebuilt_is_played_again() {
        use crate::audio::play::PlayerSeen;
        use crate::step::{Effect, Input};
        let brave = "org.mpris.MediaPlayer2.brave";
        let resumed = |effects: &[Effect]| {
            effects.iter().any(|e| matches!(e, Effect::ResumePlayers(names) if names == &[brave]))
        };
        // Within the window after a rebuild, outside it, and after the
        // daemon itself gave the seat away.
        for (after_ms, step_back, expect) in
            [(100, false, true), (600, false, false), (100, true, false)]
        {
            let mut daemon = Daemon::new("xps");
            let now = clock();
            ready_link(&mut daemon, now);
            daemon.audio.startup_players(&[PlayerSeen { service: brave.into(), playing: true }]);
            daemon.output.claim();
            daemon.step(Input::OutputRestarted { id: 0, ok: true }, now);
            daemon.rebuild_pause_until = Some(now.mono.saturating_add(500));
            if step_back {
                daemon.step_back(now);
            }
            let later = Clock::new(now.mono.0 + after_ms, now.unix.0 + after_ms);
            let paused =
                daemon.step(Input::Player { service: brave.into(), playing: Some(false) }, later);
            assert_eq!(
                resumed(&paused.effects),
                expect,
                "after {after_ms} ms, step_back={step_back}"
            );
            if expect {
                // Our own Play continues the claim; it does not start another.
                let serial = daemon.output_restart_serial;
                let resume_output = daemon.resume_output_on_grant;
                daemon.step(Input::Player { service: brave.into(), playing: Some(true) }, later);
                assert_eq!(daemon.output_restart_serial, serial);
                assert_eq!(daemon.resume_output_on_grant, resume_output);
                assert!(daemon.resumed_players.is_empty());
            }
        }
    }

    #[test]
    fn a_rebuilt_output_that_never_streams_is_rebuilt_again_a_bounded_number_of_times() {
        use crate::audio::route::{Graph, LiveStream, Sink};
        use crate::pods::observe::Observation;
        use crate::step::{Effect, Input};
        for streamed in [false, true] {
            let mut daemon = Daemon::new("gram");
            let now = clock();
            let at = |ms: u64| Clock::new(now.mono.0 + ms, now.unix.0 + ms);
            daemon.set_local_address(Some(addr(2)));
            ready_link(&mut daemon, now);
            daemon.step(Input::Players(Vec::new()), now);
            let playing = Graph {
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
                    volume: 1.0,
                    corked: false,
                }],
            };
            let mut parked = playing.clone();
            parked.streams[0].mute = true;
            parked.streams[0].sink = "speakers".into();
            daemon.step(Input::Audio(playing.clone()), now);
            daemon.step(Input::Observation(Observation::Ownership(false)), now);
            daemon.output.claim();
            daemon.audio.set_local_playing_for_test(true);
            let mut serial = daemon.output_restart_serial;
            let mut rebuilds = 0;
            for round in 0..4u64 {
                let base = round * 3_000;
                if round == 0 {
                    daemon.step(Input::Observation(Observation::Ownership(true)), at(base));
                } else {
                    daemon.step(Input::Tick, at(base));
                }
                if daemon.output_restart_serial == serial {
                    continue;
                }
                serial = daemon.output_restart_serial;
                rebuilds += 1;
                let reset = daemon.step(Input::Audio(parked.clone()), at(base + 10));
                assert!(reset.effects.iter().any(|e| matches!(e, Effect::RestartOutput { .. })));
                daemon.step(Input::OutputRestarted { id: serial, ok: true }, at(base + 20));
                daemon.step(Input::Audio(playing.clone()), at(base + 30));
                daemon.step(Input::Transport { active: streamed }, at(base + 40));
                // Before the check is due nothing happens.
                daemon.step(Input::Tick, at(base + 1_000));
                assert_eq!(daemon.output_restart_serial, serial);
            }
            // The grant's rebuild, then at most two more when audio never flows.
            assert_eq!(rebuilds, if streamed { 1 } else { 3 });
        }
    }

    #[test]
    fn a_grant_after_losing_the_pods_relinks_unless_audio_already_streams() {
        use crate::audio::route::{Graph, LiveStream, Sink};
        use crate::pods::observe::Observation;
        use crate::step::{Effect, Input};
        for (state, flagged) in [(2, false), (0, false), (2, true), (0, true)] {
            let mut daemon = Daemon::new("gram");
            let now = clock();
            daemon.set_local_address(Some(addr(2)));
            ready_link(&mut daemon, now);
            daemon.step(Input::Players(Vec::new()), now);
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
                    generation: 10,
                    sink: "pods".into(),
                    role: String::new(),
                    node: "Brave".into(),
                    mute: false,
                    volume: 1.0,
                    corked: false,
                }],
            };
            daemon.step(Input::Audio(graph.clone()), now);
            // The other laptop took the pods, which ended this one's A2DP stream.
            daemon.step(Input::Observation(Observation::Ownership(false)), now);
            if state == 0 {
                daemon.step(Input::Transport { active: true }, now);
            }
            // With or without the older grant flag, a running sink is kept.
            daemon.output.claim();
            daemon.resume_output_on_grant = flagged;
            let grant = daemon.step(Input::Observation(Observation::Ownership(true)), now);
            // Recovery parks the muted stream on the speakers before restarting.
            let mut parked = graph.clone();
            parked.streams[0].mute = true;
            parked.streams[0].sink = "speakers".into();
            let settled = daemon.step(Input::Audio(parked), now);
            let restarted = [&grant.effects, &settled.effects].iter().any(|effects| {
                effects.iter().any(|effect| matches!(effect, Effect::RestartOutput { .. }))
            });
            assert_eq!(restarted, state != 0, "sink state {state} flagged {flagged}");
            assert!(!daemon.rearm_after_loss);
            assert!(!daemon.resume_output_on_grant);
        }
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
            let mut muted = graph;
            muted.streams[0].mute = true;
            let reset = daemon.step(Input::Audio(muted), now);
            let card = reset.effects.iter().find_map(|effect| match effect {
                Effect::RestartOutput { card, .. } => Some(card.clone()),
                _ => None,
            });
            let expected =
                card_known.then(|| ("bluez_card.pods".to_string(), "a2dp-sink".to_string()));
            assert_eq!(card, Some(expected));
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
        assert!(grant.effects.iter().any(|effect| matches!(effect, Effect::MuteStreams(_))));
        assert!(!grant.effects.iter().any(|effect| matches!(effect, Effect::RestartOutput { .. })));
        assert!(!grant.effects.iter().any(|effect| matches!(effect, Effect::MoveStreams(_))));
        let mut muted = graph;
        muted.streams[0].mute = true;
        let reset = daemon.step(Input::Audio(muted), now);
        assert!(reset.effects.iter().any(|effect| matches!(effect, Effect::RestartOutput { .. })));
        assert!(!daemon.output.held_streams()[0].mute, "save the speaker stream before muting it");
    }

    #[test]
    fn recovery_defers_an_unmute_queued_before_the_ownership_grant() {
        use crate::audio::route::{Graph, LiveStream, Sink};
        use crate::step::{Effect, Input};
        let mut daemon = Daemon::new("xps");
        let now = clock();
        daemon.set_local_address(Some(addr(2)));
        ready_link(&mut daemon, now);
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
        daemon.output.hold_streams(&[StreamSnap {
            id: 1,
            generation: 10,
            mute: false,
            volume: 1.0,
        }]);
        daemon.resume_output_on_grant = true;
        daemon.note_ownership(true, now.mono);
        let action = daemon.step(
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
                    mute: true,
                    volume: 1.0,
                    corked: false,
                }],
            }),
            now,
        );
        assert!(action
            .effects
            .iter()
            .any(|e| matches!(e, Effect::MoveStreams(m) if m.sink == "speakers")));
        assert!(
            !action.effects.iter().any(|e| matches!(e, Effect::Restore(_))),
            "the pending unmute must wait until recovery moves the stream back to the pods"
        );
        assert_eq!(daemon.output.held_streams().len(), 1);
        assert!(!daemon.output.held_streams()[0].mute);
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
            assert_eq!(
                granted
                    .effects
                    .iter()
                    .filter(|effect| matches!(effect,
                        Effect::RestartOutput { name, .. } if name == "pods"
                    ))
                    .count(),
                usize::from(completion == "grant")
            );
            assert!(!granted.effects.iter().any(|effect| matches!(
                effect,
                Effect::OpenLink { .. } | Effect::Disconnect { .. } | Effect::SelectProfile { .. }
            )));
            assert_eq!(daemon.connection.generation(), generation);
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

            let duplicate = daemon.step(Input::Accessory { generation, packet }, remote_at);
            assert!(!duplicate
                .effects
                .iter()
                .any(|effect| matches!(effect, Effect::PausePlayers(_))));
            daemon.step(Input::Player { service: service.into(), playing: Some(true) }, remote_at);
            assert!(!daemon.output.wants_output(), "buffered playing feedback must not reclaim");
            daemon.step(Input::Player { service: service.into(), playing: Some(false) }, remote_at);
            daemon.step(Input::Player { service: service.into(), playing: Some(true) }, remote_at);
            assert!(daemon.output.wants_output(), "a later play action can take audio back");
            assert!(!daemon.audio.yielded());
        }
    }

    #[test]
    fn setup_idle_and_local_reports_do_not_pause_a_new_claim() {
        use crate::step::{Effect, Input};
        let mut daemon = Daemon::new("xps");
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
        muted.streams[0].mute = true;
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
                    volume: 1.0,
                    corked: false,
                }],
                cards: Vec::new(),
            }),
            now,
        );
        let elapsed = crate::pods::reports::REPORT_TTL_MS + 1;
        let later = Clock::new(now.mono.0 + elapsed, now.unix.0 + elapsed);
        daemon.tick(later);
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

    fn play(token: &str) -> Command {
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
        earlier.select_accessory(Some(addr(9)));
        earlier.note_ownership(false, now.mono);
        earlier.handle(play("earlier"), now);
        assert_eq!(earlier.accessory().ownership.as_fresh(), Some(&false));
        assert!(earlier.wants_local_confirm(now.mono));

        let mut later = Daemon::new("gram");
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
        use crate::pods::link::{Claim, OpenStep};
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
        assert!(daemon.connection.advance(OpenStep::Notifications, generation, now.mono));
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
    fn playback_on_a_control_only_connection_opens_audio_without_waiting_for_ownership() {
        use crate::step::{Effect, Input};
        let now = clock();
        let mut daemon = Daemon::new("xps");
        daemon.select_accessory(Some(addr(9)));
        ready(&mut daemon, now.mono);
        daemon.note_ownership(false, now.mono);
        let background = daemon.step(Input::Tick, now);
        assert!(!background.effects.iter().any(|e| matches!(e, Effect::ConnectTransport { .. })));
        daemon.claim_playback(now);
        let playing = daemon.step(Input::Tick, now);
        assert!(playing
            .effects
            .iter()
            .any(|e| matches!(e, Effect::ConnectTransport { accessory } if *accessory == addr(9))));
        assert!(!daemon.resume_output_on_grant, "a new transport needs no old-output recovery");
        let repeated = daemon.step(Input::Tick, now);
        assert!(!repeated.effects.iter().any(|e| matches!(e, Effect::ConnectTransport { .. })));
        let granted = daemon
            .step(Input::Observation(crate::pods::observe::Observation::Ownership(true)), now);
        assert!(!granted.effects.iter().any(|e| matches!(e, Effect::ConnectTransport { .. })));
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
        use crate::pods::link::{Claim, OpenStep};
        use crate::pods::observe::Observation;
        use crate::step::{Effect, Input};
        let now = clock();
        let at = |ms: u64| Clock::new(now.mono.0 + ms, now.unix.0 + ms);
        let mut daemon = Daemon::new("gram");
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
        assert!(daemon.connection.advance(OpenStep::Notifications, generation, now.mono));
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
    fn a_player_already_playing_claims_when_the_pods_are_identified() {
        use crate::audio::play::PlayerSeen;
        use crate::step::{Effect, Input};

        let mut daemon = Daemon::new("gram");
        let now = clock();
        daemon.step(
            Input::Players(vec![PlayerSeen {
                service: "org.mpris.MediaPlayer2.brave".into(),
                playing: true,
            }]),
            now,
        );
        let found = daemon.step(Input::Paired { address: addr(3), name: "Pods".into() }, now);
        assert!(found.effects.iter().any(|effect| matches!(effect, Effect::OpenLink { .. })));
        assert!(daemon.output.wants_output());
    }

    fn ready(daemon: &mut Daemon, now: MonoMs) {
        use crate::pods::link::{Claim, OpenStep};
        let Claim::Started(generation) = daemon.connection.claim(now) else {
            panic!("claim");
        };
        daemon.note_socket(true, generation, now);
        assert!(daemon.connection.advance(OpenStep::Notifications, generation, now));
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
        use crate::step::{Effect, Input};
        let mut daemon = Daemon::new("xps");
        let mut now = clock();
        let local = addr(2);
        let gram = addr(4);
        daemon.set_local_address(Some(local));
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
        daemon.note_peer_playback(phone, true, now.mono);
        assert_eq!(daemon.status_value(now)["audio_owner"]["playing"], true);
        // The pods report playback when it changes. A song that keeps playing sends nothing more.
        let elapsed = crate::pods::reports::REPORT_TTL_MS + 60_000;
        now = Clock::new(now.mono.0 + elapsed, now.unix.0 + elapsed);
        daemon.tick(now);
        let status = daemon.status_value(now);
        assert_eq!(status["audio_owner"]["holder"], "remote");
        assert_eq!(status["audio_owner"]["playing"], true);
        assert_eq!(status["remote_playing"], true);
        // A pause is reported, and it stands too.
        daemon.note_peer_playback(phone, false, now.mono);
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
        daemon.tick(later);
        // Nothing changed for six minutes on a live link: the pods had nothing new to say.
        assert!(daemon.accessory.source.as_fresh().is_some());
        assert_eq!(daemon.accessory.remote_address.as_fresh(), Some(&phone));
        assert!(daemon.accessory.left.as_fresh().is_some());
        assert!(daemon.accessory.noise.as_fresh().is_some() || daemon.accessory.noise.is_unknown());
        // Without the link, the battery reading ages out as before.
        daemon.note_socket(false, daemon.connection.generation(), now.mono);
        daemon.tick(later_by(crate::pods::reports::REPORT_TTL_MS * 3 + 1));
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
        daemon.note_peer_playback(gram, false, now.mono);
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
        daemon.note_peer_playback(addr(6), false, now.mono);
        assert!(!daemon.peer_playback.contains_key(&addr(6)));
        assert_eq!(daemon.status_value(now)["audio_owner"]["playing"], true);
        daemon.note_peer_playback(gram, false, now.mono);
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
        gram.note_peer_playback(local, false, now.mono);
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
        use crate::step::{Effect, Input};
        for delay in [3_500, 5_001] {
            let mut daemon = Daemon::new("xps");
            let mut now = clock();
            let local = addr(2);
            let gram = addr(4);
            daemon.set_local_address(Some(local));
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
            daemon.note_peer_playback(gram, true, now.mono);
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
        use crate::step::{Effect, Input};
        let local = addr(2);
        let peer = addr(4);
        let mut daemon = Daemon::new("xps");
        let mut now = clock();
        daemon.set_local_address(Some(local));
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
        daemon.note_peer_playback(peer, false, now.mono);
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
            for right in [false, true] {
                daemon.accessory.note_placement(right, Some(true), Some(false), now.mono);
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
                    volume: 0.7,
                    corked: false,
                }],
            };
            let held = a.step(Input::Audio(graph.clone()), now);
            assert!(held.effects.iter().any(|e| matches!(e, Effect::MuteStreams(_))));
            graph.streams[0].mute = true;
            a.step(Input::Audio(graph), now);
            let mut b = peer(2, now);
            b.step(Input::Command(Command::Activity), now);
            let record = b.preferred_connector().unwrap();
            advance_clock(&mut now, crate::fleet::activity::REFRESH_MS);
            let frame =
                b.activity_preference.offer(record, addr(2), now, &keys_for_test()).unwrap();
            let heard = a.step(Input::Advertisement(frame), now);
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
}
