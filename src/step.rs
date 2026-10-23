//! One input, one clock, a list of effects. Adapters run the effects.
//! They do not decide when a link opens, who owns playback, or what a letter means.

use crate::audio::play::{PlayerSeen, Profile, StreamSnap};
use crate::audio::route::{Graph, Move, StreamTarget};
use crate::fleet::members::Fleet;
use crate::fleet::postcard::{self, Postcard};
use crate::panel::ipc::Command;
use crate::pods::link::{FailReason, Stage};
use crate::pods::reports::Pod;
use crate::pods::setup::{self, Place};
use crate::types::{Address, Battery, Clock, Generation, MonoMs};

use crate::daemon::{host_id, Daemon};

/// How long after an output rebuild audio must be streaming to the pods.
const REARM_VERIFY_MS: u64 = 2_500;
/// Rebuilds after a failed start, per grant.
const REARM_RETRY_LIMIT: u8 = 2;
/// A player that pauses this soon after a rebuilt output came back paused
/// because its stream was moved, not because the user asked.
const REBUILD_PAUSE_MS: u64 = 500;
/// How long after losing the pods this laptop may still release its idle
/// AirPods output early, so the pods start the new owner's audio sooner.
const RELEASE_OUTPUT_MS: u64 = 3_000;
/// After a claim sent as a new link becomes ready (or just after, by a play
/// that waited for it), the rest of the pods' opening reports still
/// describe who played before it. A remote owner in
/// them is not a takeover. The pods answer a claim well within this.
pub(crate) const SETUP_CLAIM_SETTLE_MS: u64 = 1_500;
/// The pods can send one more report naming the previous owner just before
/// they grant a claim (2026-10-19: 1 ms apart). A remote report this soon
/// after a claim waits this long for the grant before it ends the claim.
const CLAIM_ANSWER_MS: u64 = 300;

/// Something the platform observed, or one command from the panel.
pub enum Input {
    /// Pulse's raw output states, independent of ownership or PCM content.
    Outputs(Vec<crate::audio::health::Output>),
    OutputRearmed {
        id: u64,
        ok: bool,
    },
    OutputRestarted {
        id: u64,
        ok: bool,
    },
    /// The accessory's A2DP transport started (`active`) or stopped streaming.
    Transport {
        active: bool,
    },
    Command(Command),
    Tick,
    Lid {
        open: bool,
    },
    LinkFailed {
        generation: Generation,
        /// BlueZ was already connecting. This does not spend a retry.
        busy: bool,
    },
    /// BlueZ completed an explicit Disconnect, rather than only closing AACP.
    LinkReleased {
        accessory: Address,
        generation: Generation,
        ok: bool,
    },
    /// One packet read from the control socket.
    Accessory {
        generation: Generation,
        packet: Vec<u8>,
    },
    /// The keyring write returned. Membership is unchanged until this arrives.
    KeyringStored,
    KeyringFailed,
    /// The keyring was locked at start and has opened. `fleet` is `None` when it holds no fleet.
    KeyringUnlocked {
        fleet: Option<Fleet>,
        keys: Option<crate::fleet::members::AccessoryKeys>,
    },
    /// This machine's Bluetooth adapter, from BlueZ rather than a fixed name.
    Controller(Address),
    /// The paired accessory BlueZ reported.
    Paired {
        address: Address,
        name: String,
    },
    /// Whether BlueZ saved Bluetooth pairing keys, independently of an ACL.
    Bonded {
        address: Address,
        saved: bool,
    },
    /// BlueZ removed the selected device or explicitly cleared its pairing.
    Unpaired {
        address: Address,
    },
    /// BlueZ reported the ACL. This is not the control socket and not readiness.
    Bluez(bool),
    /// BlueZ has an ACL that needs a control socket. The daemon owns setup and retries.
    AttachAvailable {
        generation: Generation,
    },
    /// The L2CAP control socket opened or closed for this generation.
    ControlSocket {
        up: bool,
        generation: Generation,
    },
    /// BlueZ `InProgress` while a profile operation is still running.
    ProfilePending(bool),
    /// One typed accessory observation, translated from a control
    /// packet by `pods::observe` — the same vocabulary production
    /// packets arrive in.
    Observation(crate::pods::observe::Observation),
    Profiles(Vec<Profile>),
    ActivateAudio,
    Radio(Vec<u8>),
    /// One fleet service-data advertisement. A finished frame is heard as `Radio`.
    Advertisement(Vec<u8>),
    /// Apple proximity-pairing manufacturer data, with the advertiser's address.
    Proximity {
        address: Address,
        payload: Vec<u8>,
    },
    /// Display metadata for an observed advertiser; never an identity proof.
    NearbyMetadata {
        address: Address,
        name: String,
        rssi: Option<i16>,
    },
    KeyringLocked(bool),
    /// The hidden microphone source. `streaming` is a linked recorder.
    /// `muted` is that source's own mute.
    Microphone {
        streaming: bool,
        muted: bool,
    },
    Suspend,
    /// Sleep ended.
    Resume,
    /// bluetoothd left the system bus.
    BluezLost,
    /// bluetoothd owns `org.bluez` again.
    BluezBack,
    /// The controller's Powered property.
    AdapterPower(bool),
    /// The audio server context died. Held stream serials are no longer meaningful.
    AudioLost,
    /// The card profile the daemon selected was or was not applied.
    ProfileResult {
        ok: bool,
    },
    Streams(Vec<StreamSnap>),
    /// The startup picture of MPRIS players. This does not claim.
    Players(Vec<PlayerSeen>),
    /// One MPRIS player appeared, changed, or left the bus (`playing` is absent).
    Player {
        service: String,
        playing: Option<bool>,
    },
    /// Sinks and playback streams from PipeWire.
    Audio(Graph),
}

/// Which pod a battery report belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PodSlot {
    Left,
    Right,
    Case,
}

/// Work the platform must do. The daemon has already decided.
#[derive(Clone, Debug)]
pub enum Effect {
    OpenLink {
        generation: Generation,
    },
    Send {
        to: Address,
        bytes: Vec<u8>,
    },
    /// One frame the daemon already chose. The radio only writes it.
    Advertise(Vec<u8>),
    /// Stop the advertisement. Discovery stays up.
    ReleaseAir,
    /// Start discovery. It stays up while this host has a controller address.
    Listen,
    /// Stop discovery. This host no longer has a controller address.
    StopListen,
    /// Persist the roster replay watermark. The bytes are not a secret.
    StoreWatermark(Vec<u8>),
    /// Persist the last-active fleet device independently of roster expiry.
    StoreActivity(Vec<u8>),
    /// Remember learned Apple peer names across daemon restarts.
    StoreDevices(std::collections::HashMap<Address, String>),
    Announce(Vec<u8>),
    Restore(Vec<StreamSnap>),
    WriteKeyring(Fleet),
    /// Bytes for the open control socket. The daemon chose them.
    WriteControl {
        accessory: Address,
        generation: Generation,
        bytes: Vec<u8>,
    },
    /// Select this codec profile on the card the audio server reported.
    SelectProfile {
        card: String,
        profile: String,
    },
    /// Connect the A2DP transport. Sent only when that transport is not already up.
    ConnectTransport {
        accessory: Address,
    },
    /// Rebuild A2DP after detaching muted streams, preserving the control link.
    /// With the card known, its profile is refreshed (off, then this profile),
    /// which rebuilds the audio nodes without a Bluetooth reconnect.
    RestartOutput {
        id: u64,
        name: String,
        accessory: Address,
        card: Option<(String, String)>,
    },
    /// Start the relinked PipeWire node once, after recovery has restored streams.
    StartOutput {
        name: String,
    },
    /// Briefly suspend/resume only this Pulse sink. No Bluetooth unpairing.
    RearmOutput {
        id: u64,
        index: u32,
        name: String,
    },
    /// Pause these MPRIS players. The daemon already decided the seat was given away.
    PausePlayers(Vec<String>),
    /// Play these MPRIS players again. They paused themselves while the
    /// AirPods output was rebuilt under them.
    ResumePlayers(Vec<String>),
    /// Suspend this idle output so its Bluetooth audio stops now.
    SuspendOutput {
        name: String,
    },
    /// The daemon is not holding the link. Attach the control socket if the accessory is already up.
    FollowLink,
    /// Publish the microphone source for this accessory. `description` is the name the panel shows.
    ShowMicrophone {
        address: Address,
        rate: u32,
        description: String,
    },
    /// Remove that source.
    HideMicrophone,
    /// Ask BlueZ to forget this accessory. The pairing goes with it.
    RemoveDevice {
        accessory: Address,
    },
    /// Drop the BlueZ link for this accessory. The control socket goes with it.
    Disconnect {
        accessory: Address,
    },
    /// One accessory secret. The name is the keyring item, not a log line.
    WriteSecret {
        name: String,
        bytes: Vec<u8>,
    },
    /// Mute these streams before the default output changes.
    MuteStreams(Vec<StreamTarget>),
    /// Make this sink the default output. The name came from the audio server.
    SetDefaultSink {
        name: String,
    },
    /// Move these streams onto a sink: the accessory, or the speakers when
    /// docked pods hand playback back. The serial must still match.
    MoveStreams(Move),
}

pub struct StepOutcome {
    pub reply: Option<String>,
    pub effects: Vec<Effect>,
}

impl Daemon {
    pub fn step(&mut self, input: Input, now: Clock) -> StepOutcome {
        let reply = self.apply(input, now);
        StepOutcome { reply, effects: self.harvest(now) }
    }

    /// Request local playback when this machine is playing and no newer remote
    /// report has arrived. A remote report stored before the claim is older
    /// intent, even when both timestamps are the same millisecond. A remote
    /// report that shows up after the claim wins unless the pods grant the
    /// claim within [`CLAIM_ANSWER_MS`].
    pub(crate) fn wants_local_confirm(&self, now: MonoMs) -> bool {
        self.seat.wants_local_confirm(
            self.output.wants_output(),
            self.audio.local_playing(),
            self.audio.yielded(),
            &self.accessory.ownership,
            now,
        )
    }

    /// Ask on the existing control link. Ownership and source remain observations
    /// from the accessory; sending CLAIM is not confirmation of playback.
    pub(crate) fn request_playback_ownership(&mut self, now: Clock) -> bool {
        if !self.wants_local_confirm(now.mono) {
            return false;
        }
        self.effects.control_out.push((self.connection.generation(), setup::ownership(true)));
        eprintln!("fleetpodsd: claiming AirPods audio on the existing control link");
        self.claim_sent_at = Some(now.mono);
        true
    }

    fn apply(&mut self, input: Input, now: Clock) -> Option<String> {
        match input {
            Input::Outputs(outputs) => {
                self.output_health.outputs = outputs;
                None
            }
            Input::OutputRearmed { id, ok } => {
                if id == self.output_health.resets {
                    self.output_health.last_ok = Some(ok);
                    eprintln!("fleetpodsd: audio output reset {id} completed (ok={ok})");
                }
                None
            }
            Input::Transport { active } => {
                if self.transport_active != active {
                    eprintln!("fleetpodsd: AirPods audio transport active={active}");
                }
                self.transport_active = active;
                None
            }
            Input::OutputRestarted { id, ok } => {
                if self.output.finish_recovery(id, ok) {
                    eprintln!("fleetpodsd: AirPods sink restart {id} completed (ok={ok})");
                    if !ok {
                        self.seat_audio_failure("audio sink restart failed", now);
                    } else {
                        // A rebuilt node can fail to start if the pods were not
                        // ready yet; check that audio actually flows.
                        self.rearm_verify_at = Some(now.mono.saturating_add(REARM_VERIFY_MS));
                        self.rebuild_pause_until = Some(now.mono.saturating_add(REBUILD_PAUSE_MS));
                    }
                    self.replan(now);
                }
                None
            }
            Input::Command(command) => self.handle(command, now).body,
            Input::Tick => {
                self.tick(now);
                if !self.radio_quiet() {
                    self.replan(now);
                }
                self.effects.announce_dirty = true;
                None
            }
            Input::Lid { open } => {
                self.note_lid(open, now.mono);
                None
            }
            Input::LinkFailed { generation, busy } => {
                self.link_failed(generation, busy, now);
                None
            }
            Input::Controller(address) => {
                self.set_local_address(Some(address));
                None
            }
            Input::Paired { address, name } => {
                let known = self.config.accessory == Some(address);
                if !known {
                    if let Some(report) = self.nearby_pods.report_for_pairing(address, now.mono) {
                        self.accessory = report;
                    }
                    self.accessory.pairing_at = None;
                }
                self.select_accessory(Some(address));
                let sku = self.accessory.sku.clone();
                if !name.is_empty() {
                    if sku.is_empty() {
                        self.accessory.name = name;
                    } else {
                        self.set_identity(name, &sku);
                    }
                }
                if !known && self.audio.local_playing() && !self.audio.yielded() {
                    self.claim_playback(now);
                }
                None
            }
            Input::Unpaired { address } => {
                self.unpaired(address, now);
                None
            }
            Input::Bonded { address, saved } => {
                if self.config.accessory == Some(address) {
                    self.bluetooth_bonded = Some(saved);
                }
                None
            }
            Input::LinkReleased { accessory, generation, ok } => {
                self.note_link_released(accessory, generation, ok, now);
                None
            }
            Input::Bluez(up) => {
                self.note_bluez(up);
                None
            }
            Input::AttachAvailable { generation } => {
                self.attach_available(generation, now.mono);
                None
            }
            Input::ControlSocket { up, generation } => {
                if generation != self.connection.generation() {
                    return None;
                }
                let was_ready = self.connection.stage() == Stage::Ready;
                self.note_socket(up, generation, now.mono);
                if up {
                    self.begin_setup(generation, now);
                } else {
                    self.accessory_setup = setup::Setup::new();
                    if was_ready
                        && !self.routing.phase().busy()
                        && !self.connection.apple_reconnect()
                    {
                        self.step_back(now);
                    }
                }
                None
            }
            Input::Accessory { generation, packet } => {
                if generation == self.connection.generation() {
                    self.feed_accessory(&packet, generation, now);
                }
                None
            }
            Input::KeyringStored => {
                self.commit_keyring();
                None
            }
            Input::KeyringUnlocked { fleet, keys } => {
                self.keyring_unlocked(fleet, keys);
                None
            }
            Input::KeyringFailed => {
                if let Some(pending) = self.commit.take() {
                    self.enrollment.note_store_failed(pending.id());
                }
                None
            }
            Input::ProfilePending(pending) => {
                self.note_profile_pending(pending);
                None
            }
            Input::Observation(observation) => {
                let generation = self.connection.generation();
                self.apply_observation(observation, generation, now);
                None
            }
            Input::Profiles(profiles) => {
                self.set_profiles(profiles);
                None
            }
            Input::ActivateAudio => {
                self.activate_profile();
                None
            }
            Input::Radio(bytes) => {
                self.hear_radio(&bytes, None, now);
                None
            }
            Input::Proximity { address, payload } => {
                self.hear_proximity(address, &payload, now);
                None
            }
            Input::NearbyMetadata { address, name, rssi } => {
                self.nearby_pods.metadata(address, &name, rssi, now.mono);
                if self.config.accessory.is_none() {
                    if let Some(report) = self.nearby_pods.selected_report(now.mono) {
                        self.accessory = report;
                    }
                }
                None
            }
            Input::Advertisement(bytes) => {
                // A version-1 fragment header carries its sender; keep a
                // known letter peer nearby while it keeps transmitting.
                if bytes.first() == Some(&1) && bytes.len() >= 13 {
                    if let Ok(raw) = <[u8; 6]>::try_from(&bytes[1..7]) {
                        if let Some(source) = Address::new(raw) {
                            self.refresh_letter_peer(source, now);
                        }
                    }
                }
                if let Some((source, frame)) =
                    self.radio_inbox.accept(&bytes, self.local_address, now.mono)
                {
                    self.hear_radio(&frame, Some(source), now);
                } else if bytes.first() == Some(&crate::fleet::pod_state::VERSION) {
                    self.hear_pod_state(&bytes, now);
                } else if bytes.first() == Some(&crate::fleet::activity::VERSION) {
                    self.hear_activity(&bytes, now);
                } else if let Some(sight) = self.read_presence(&bytes, now.unix) {
                    self.hear_presence(sight);
                } else if let Some(snapshot) = self.read_roster(&bytes) {
                    self.hear_roster(snapshot, now);
                } else if let Some(message) = self.read_route(&bytes) {
                    self.hear_route(message, now);
                }
                None
            }
            Input::KeyringLocked(locked) => {
                self.set_keyring_locked(locked);
                None
            }
            Input::Microphone { streaming, muted } => {
                self.set_microphone(streaming, muted, now);
                None
            }
            Input::Suspend => {
                self.suspending = true;
                self.interrupt("suspend", now);
                None
            }
            Input::Resume => {
                self.suspending = false;
                self.note_link_returned(now.mono);
                None
            }
            Input::BluezLost => {
                self.interrupt("bluez", now);
                None
            }
            Input::BluezBack => {
                self.note_link_returned(now.mono);
                None
            }
            Input::AdapterPower(up) => {
                self.adapter_down = !up;
                if up {
                    self.note_link_returned(now.mono);
                } else {
                    self.interrupt("adapter", now);
                }
                None
            }
            Input::AudioLost => {
                self.last_graph = None;
                self.output.forget_server();
                self.output_health.forget_server();
                self.audio.set_routed(false);
                None
            }
            Input::ProfileResult { ok } => {
                self.note_profile(ok, now);
                None
            }
            Input::Streams(streams) => {
                self.output.hold_streams(&streams);
                None
            }
            Input::Players(players) => {
                self.audio.startup_players(&players);
                self.effects.announce_dirty = true;
                None
            }
            Input::Player { service, playing } => {
                if playing == Some(false)
                    && self.output.wants_output()
                    && self.audio.player_playing(&service)
                    && (self.output.recovering()
                        || self.rebuild_pause_until.is_some_and(|until| now.mono.0 < until.0))
                    && !self.resume_players.contains(&service)
                {
                    eprintln!("fleetpodsd: player {service:?} paused during the AirPods rebuild; resuming it");
                    self.resume_players.push(service.clone());
                }
                let after_yield = self.audio.yielded();
                // Our own Play continues the claim already in progress.
                let resumed = playing == Some(true)
                    && self
                        .resumed_players
                        .iter()
                        .position(|s| s == &service)
                        .map(|at| self.resumed_players.remove(at))
                        .is_some();
                if self.audio.update_player(&service, playing) && !resumed {
                    eprintln!("fleetpodsd: player {service:?} reported new Playing state (after_yield={after_yield})");
                    self.remember_activity(now);
                    self.claim_playback(now);
                }
                self.effects.announce_dirty = true;
                None
            }
            Input::Audio(graph) => {
                self.route_owned_playback(&graph, now);
                self.follow_graph(&graph, now);
                None
            }
        }
    }

    fn note_bud(
        &mut self,
        bud: crate::pods::setup::Bud,
        level: u8,
        charging: bool,
        in_ear: Option<bool>,
        in_case: Option<bool>,
        now: MonoMs,
    ) {
        let Some(battery) = Battery::new(level, charging) else {
            return;
        };
        match bud {
            crate::pods::setup::Bud::Headset => {
                self.accessory.note_headset(battery, now);
            }
            crate::pods::setup::Bud::Left => {
                self.note_pod(false, Pod { battery, in_ear, in_case }, now);
            }
            crate::pods::setup::Bud::Right => {
                self.note_pod(true, Pod { battery, in_ear, in_case }, now);
            }
        }
    }

    fn control_ready(&mut self, generation: Generation, now: Clock) {
        if !self.mark_control_ready(generation) {
            return;
        }

        if let Some(peer) = self.peers_seated.first().copied() {
            self.note_alongside(peer, now.unix);
        }
        self.link_ready_at = Some(now.mono);
        // A play waiting for this link asked before anything the pods
        // reported during setup, so those reports cannot override it.
        if self.output.wants_output() {
            self.seat.note_claim(self.accessory.ownership.at());
        }
        if self.request_playback_ownership(now) {
            self.setup_claim_at = Some(now.mono);
        }
        self.flush_settings();
        self.effects.announce_dirty = true;
    }

    fn begin_setup(&mut self, generation: Generation, now: Clock) {
        if !self.connection.ensure_handshake(generation, now.mono) {
            return;
        }
        let hello = self.accessory_setup.start().to_vec();
        self.connection.advance(crate::pods::link::OpenStep::Handshake, generation, now.mono);
        self.effects.control_out.push((generation, hello));
    }

    fn feed_accessory(&mut self, packet: &[u8], generation: Generation, now: Clock) {
        if self.hear_microphone(packet, now.mono) {
            self.note_control_response(generation);
            return;
        }
        // One packet, interpreted once: `pods::observe` reads the peer
        // relay, the priority acknowledgement, the peer-reconnect, and
        // the setup report, and states each as a typed observation.
        let (follow, observations) =
            crate::pods::observe::translate(&mut self.accessory_setup, packet, self.local_address);
        if let Some(follow) = follow {
            self.connection.advance(follow.step(), generation, now.mono);
            self.effects.control_out.push((generation, follow.bytes().to_vec()));
            if follow == setup::FollowUp::Notifications {
                self.effects.control_out.push((generation, setup::request_keys().to_vec()));
            }
        }
        for observation in observations {
            self.apply_observation(observation, generation, now);
        }
    }

    fn apply_observation(
        &mut self,
        observation: crate::pods::observe::Observation,
        generation: Generation,
        now: Clock,
    ) {
        use crate::pods::observe::Observation;
        self.note_control_response(generation);
        match observation {
            Observation::Bud { bud, level, charging, place } => {
                let (in_ear, in_case) = place_of(place);
                self.note_bud(bud, level, charging, in_ear, in_case, now.mono);
            }
            Observation::CaseBattery { level, charging } => {
                self.note_battery(PodSlot::Case, level, charging, now.mono);
            }
            Observation::Ownership(local) => {
                self.observe_ownership(local, now);
            }
            Observation::Source { local, kind, remote } => {
                self.observe_source(local, kind, remote, now);
            }
            Observation::Noise(mode) => self.note_noise_report(mode, now.mono),
            Observation::Identity { name, model } => {
                let sku = if model.is_empty() { self.accessory.sku.clone() } else { model };
                if !name.is_empty() || !sku.is_empty() {
                    self.set_identity(name, &sku);
                }
            }
            Observation::Keys { irk, encryption } => {
                // Every link reports the keys. Rewriting unchanged items on each connection
                // only makes the keyring replace them, so write only what changed.
                let keys = crate::fleet::members::AccessoryKeys { irk, encryption };
                if self.membership.accessory_keys() != Some(&keys) {
                    self.set_accessory_keys(keys);
                    self.queue_accessory_keys();
                }
            }
            Observation::PodPeers { peers, owner } => {
                self.note_pod_peers(peers, now.unix);
                if let (Some(owner), Some(local)) = (owner, self.local_address) {
                    if owner != local {
                        self.note_remote(owner, now.mono);
                    }
                    eprintln!("fleetpodsd: media event source=airpods-roster owner={owner}");
                    self.observe_ownership(owner == local, now);
                }
            }
            Observation::PeerName { address, label } => {
                self.learn_apple_name(address, &label);
            }
            Observation::PeerPlaying { address, playing } => {
                self.note_peer_playback(address, playing, now.mono);
            }
            Observation::PriorityAcknowledged => {
                self.note_priority_ack();
                self.note_pair_reserved(now);
            }
            Observation::PeerReconnect(peer) => {
                self.free_slot(peer, now);
            }
            Observation::Ready => {
                self.control_ready(generation, now);
            }
        }
    }

    fn observe_ownership(&mut self, local: bool, now: Clock) {
        eprintln!(
            "fleetpodsd: media event source=airpods ownership_local={local} mono_ms={}",
            now.mono.0
        );
        self.note_ownership(local, now.mono);
        if local {
            self.setup_claim_at = None;
            self.yield_pending = false;
        }
        if !local {
            // The pods end this laptop's A2DP stream, which can leave its
            // audio node failed. The next grant relinks unless the sink runs.
            self.rearm_after_loss = true;
            self.yield_to_remote_report(now);
        }
        self.effects.announce_dirty = true;
    }

    fn observe_source(&mut self, local: bool, kind: u8, remote: Option<Address>, now: Clock) {
        eprintln!("fleetpodsd: media event source=airpods audio_local={local} kind={kind} remote={remote:?} mono_ms={}", now.mono.0);
        self.note_source(local, kind, now.mono);
        if local && kind > 0 {
            // Audio already reaches the pods from here; nothing to relink.
            self.resume_output_on_grant = false;
            self.rearm_after_loss = false;
        }
        if let Some(address) = remote {
            self.note_remote(address, now.mono);
        }
        if !local && kind > 0 {
            self.yield_to_remote_report(now);
        }
        self.effects.announce_dirty = true;
    }

    fn yield_to_remote_report(&mut self, now: Clock) {
        // Setup reports describe the seat before acquisition. Once the link is
        // ready, losing ownership or seeing a remote source ends our claim.
        // An explicit user switch inside its window is not reversed.
        if self.seat.user_intent_fresh(now.mono) {
            eprintln!("fleetpodsd: holding the seat for an explicit user switch");
            return;
        }
        if self
            .setup_claim_at
            .is_some_and(|at| now.mono.saturating_since(at) < SETUP_CLAIM_SETTLE_MS)
        {
            eprintln!("fleetpodsd: ignoring a remote report that predates this laptop's claim");
            return;
        }
        if self.output.wants_output()
            && self.claim_sent_at.is_some_and(|at| now.mono.saturating_since(at) < CLAIM_ANSWER_MS)
        {
            if !self.yield_pending {
                eprintln!("fleetpodsd: waiting for the pods to answer this laptop's claim");
            }
            self.yield_pending = true;
            return;
        }
        if self.connection.stage() == Stage::Ready && self.output.wants_output() {
            eprintln!("fleetpodsd: AirPods audio moved remotely; pausing local players");
            self.step_back(now);
            self.release_output_until = Some(now.mono.saturating_add(RELEASE_OUTPUT_MS));
        }
    }

    fn link_failed(&mut self, generation: Generation, busy: bool, now: Clock) {
        if generation != self.connection.generation() {
            return;
        }
        if self.connection.apple_reconnect_releasing() {
            self.connection.note_socket(false, generation);
            return;
        }
        if !busy && !self.output.holding_takeover() {
            self.output.fail_acquisition();
        }
        let reason = if busy { FailReason::Busy } else { FailReason::Transport };
        self.note_setup_failed(generation, reason, now.mono);
    }

    fn note_battery(&mut self, slot: PodSlot, level: u8, charging: bool, now: MonoMs) {
        let Some(battery) = Battery::new(level, charging) else {
            return;
        };
        match slot {
            PodSlot::Case => self.note_case_battery(level, charging, now),
            PodSlot::Left => {
                self.note_pod(false, Pod { battery, in_ear: None, in_case: None }, now)
            }
            PodSlot::Right => {
                self.note_pod(true, Pod { battery, in_ear: None, in_case: None }, now)
            }
        }
    }

    fn hear_radio(&mut self, bytes: &[u8], source: Option<Address>, now: Clock) {
        match postcard::decode(bytes) {
            Some(Postcard::Announce { from, name, .. }) => {
                self.hear_announce(from, name, now);
            }
            None => {
                if let (Some(from), Some(letter)) =
                    (source, crate::fleet::cppmsg::decode(bytes, self.pake.as_ref()))
                {
                    eprintln!("fleetpodsd: fleet letter from {from}");
                    self.note_letter_peer(from, crate::fleet::cppmsg::sender_name(bytes), now);
                    self.deliver(from, letter, now);
                } else {
                    self.observe_radio(bytes);
                }
            }
        }
    }

    fn hear_announce(&mut self, from: Address, name: String, now: Clock) {
        if self.local_address == Some(from) {
            return;
        }
        // A plaintext announcement is only a nearby hint. It carries no
        // fleet-key proof, so neither its embedded roster nor its seated
        // flag can alter connection state: rosters and seated peers are
        // accepted only from authenticated roster and pod-list paths.
        self.observe_nearby(host_id(Some(from)), from, name, now.unix, false, None);
    }

    fn harvest(&mut self, now: Clock) -> Vec<Effect> {
        self.reconnect_preferred(now);
        // A remote report that arrived just after our claim and was never
        // answered by a grant ends the claim now.
        if self.yield_pending
            && self.claim_sent_at.is_none_or(|at| now.mono.saturating_since(at) >= CLAIM_ANSWER_MS)
        {
            self.yield_pending = false;
            if self.accessory.ownership.as_fresh() != Some(&true) {
                self.yield_to_remote_report(now);
            }
        }
        let mut effects = Vec::new();
        self.introduce_peers();
        self.claim_idle_on_activity(now);
        self.tick_ears(now.mono);
        self.follow_insertion(now);
        if let Some(labels) = self.take_apple_names() {
            effects.push(Effect::StoreDevices(labels));
        }
        if let Some(from) = self.local_address {
            for outbound in self.take_letters() {
                let Some(bytes) = crate::fleet::cppmsg::encode(
                    from,
                    outbound.to,
                    &self.local_name,
                    &outbound.body,
                    self.pake.as_ref(),
                ) else {
                    eprintln!("fleetpodsd: fleet letter to {} was not sealed", outbound.to);
                    continue;
                };
                eprintln!("fleetpodsd: fleet letter to {}", outbound.to);
                self.outbound.push(outbound.to, bytes.clone());
                effects.push(Effect::Send { to: outbound.to, bytes });
            }
        } else {
            self.outbox.clear();
        }
        if let Some(effect) = self.pace_listen() {
            effects.push(effect);
        }
        self.note_radio_source();
        if self.radio_quiet() {
            if !self.radio_silenced {
                self.radio_silenced = true;
                effects.push(Effect::ReleaseAir);
            }
        } else {
            self.radio_silenced = false;
            self.offer_presence(now);
            self.offer_roster(now);
            self.offer_activity(now);
            self.offer_pod_state(now);
            self.queue_outgoing_route(now);
            match self.outbound.due(now.mono) {
                crate::fleet::radio::Duty::Publish(crate::fleet::radio::Item::PodState(bytes)) => {
                    effects.push(Effect::Advertise(bytes));
                }
                crate::fleet::radio::Duty::Publish(crate::fleet::radio::Item::Letter(bytes)) => {
                    effects.push(Effect::Advertise(bytes));
                }
                crate::fleet::radio::Duty::Publish(crate::fleet::radio::Item::Presence(bytes)) => {
                    self.note_presence_aired(now.mono);
                    effects.push(Effect::Advertise(bytes));
                }
                crate::fleet::radio::Duty::Publish(crate::fleet::radio::Item::Roster(bytes)) => {
                    self.note_roster_aired(now.mono);
                    effects.push(Effect::Advertise(bytes));
                }
                crate::fleet::radio::Duty::Publish(crate::fleet::radio::Item::Activity(bytes)) => {
                    effects.push(Effect::Advertise(bytes));
                }
                crate::fleet::radio::Duty::Publish(crate::fleet::radio::Item::Route(bytes)) => {
                    effects.push(Effect::Advertise(bytes));
                }
                crate::fleet::radio::Duty::Release => effects.push(Effect::ReleaseAir),
                crate::fleet::radio::Duty::Hold => {}
            }
        }
        if let Some(bytes) = self.activity_preference.take_save() {
            effects.push(Effect::StoreActivity(bytes));
        }
        if let Some(bytes) = self.take_roster_watermark() {
            effects.push(Effect::StoreWatermark(bytes));
        }
        if let Some(generation) = self.take_open_link(now.mono) {
            effects.push(Effect::OpenLink { generation });
        }
        if self.effects.follow_link {
            self.effects.follow_link = false;
            effects.push(Effect::FollowLink);
        }
        if self.effects.keyring_dirty {
            self.effects.keyring_dirty = false;
            if let Some(fleet) = self.commit.fleet() {
                effects.push(Effect::WriteKeyring(fleet));
            }
        }
        let accessory = self.config.accessory;
        for (generation, bytes) in std::mem::take(&mut self.effects.control_out) {
            if let Some(accessory) = accessory {
                effects.push(Effect::WriteControl { accessory, generation, bytes });
            }
        }
        for (name, bytes) in std::mem::take(&mut self.effects.secret_out) {
            effects.push(Effect::WriteSecret { name, bytes });
        }
        let players = std::mem::take(&mut self.effects.pause_players);
        if !players.is_empty() {
            // Pausing is the daemon's own decision; do not undo it.
            self.resume_players.clear();
            effects.push(Effect::PausePlayers(players));
        }
        if let Some(until) = self.release_output_until {
            if self.output.wants_output() || !self.transport_active || now.mono.0 >= until.0 {
                self.release_output_until = None;
            } else if let Some(idle) = self.output_health.outputs.iter().find(|output| {
                output.address.is_some() && output.address == accessory && output.state != 0
            }) {
                // Paused players have left the output. PipeWire would keep
                // streaming silence to the pods for another second or so.
                eprintln!("fleetpodsd: releasing the idle AirPods output for the new owner");
                effects.push(Effect::SuspendOutput { name: idle.name.clone() });
                self.release_output_until = None;
            }
        }
        if !self.output.wants_output() {
            self.resume_players.clear();
            self.resumed_players.clear();
        } else if !self.resume_players.is_empty() && !self.output.recovering() {
            let players = std::mem::take(&mut self.resume_players);
            self.resumed_players.clone_from(&players);
            effects.push(Effect::ResumePlayers(players));
        }
        if self.effects.drop_link {
            self.effects.drop_link = false;
            if let Some(accessory) = accessory {
                effects.push(Effect::Disconnect { accessory });
            }
        }
        if let Some(accessory) = self.effects.remove_device.take() {
            effects.push(Effect::RemoveDevice { accessory });
        }
        if let Some(effect) = self.pace_microphone() {
            effects.push(effect);
        }
        effects.extend(self.pace_audio());
        let granted = self.output.wants_output()
            && self.connection.stage() == Stage::Ready
            && self.accessory.ownership.as_fresh() == Some(&true);
        if !granted {
            self.force_rearm = false;
        }
        if self.rearm_verify_at.is_some_and(|at| now.mono.0 >= at.0) {
            self.rearm_verify_at = None;
            if granted
                && !self.transport_active
                && self.audio.local_playing()
                && self.rearm_retries < REARM_RETRY_LIMIT
            {
                // Pulse can report the sink running while its node failed.
                self.rearm_retries += 1;
                self.force_rearm = true;
                eprintln!("fleetpodsd: AirPods output did not start; re-arming again");
            }
        }
        if granted
            && !self.force_rearm
            && (self.rearm_after_loss || self.resume_output_on_grant)
            && self.transport_active
        {
            // Audio already streams to the pods; there is nothing to relink.
            // Rebuilding now would cut it. Pulse's sink state is not used:
            // it reads running even while the node has failed.
            self.rearm_after_loss = false;
            self.resume_output_on_grant = false;
            self.replan(now);
        }
        if granted && (self.resume_output_on_grant || self.rearm_after_loss || self.force_rearm) {
            if let (Some(address), Some(graph)) = (self.config.accessory, self.last_graph.as_ref())
            {
                let id = self.output_restart_serial + 1;
                if self.output.begin_recovery(id, address, graph, now.mono) {
                    self.output_restart_serial = id;
                    self.resume_output_on_grant = false;
                    self.rearm_after_loss = false;
                    if !std::mem::take(&mut self.force_rearm) {
                        self.rearm_retries = 0;
                    }
                    self.replan(now);
                    eprintln!("fleetpodsd: re-arming AirPods playback after ownership grant");
                }
            }
        }
        if let Some(effect) = self.pace_output_health(now) {
            effects.push(effect);
        }
        if self.effects.announce_dirty {
            self.effects.announce_dirty = false;
            if let Some(address) = self.local_address {
                let seated = self.connection.stage() == Stage::Ready;
                let roster = self.audio_roster(now);
                effects.push(Effect::Announce(postcard::encode_announce(
                    address,
                    &self.local_name,
                    seated,
                    roster.as_ref(),
                )));
            }
        }
        // The Router ordered this work; translate it in one place. A quiet
        // still leads the whole step, before any control write.
        for work in self.output.take_work(now.mono, self.audio.local_playing()) {
            match work {
                crate::audio::route::Work::Restore(streams) => {
                    effects.push(Effect::Restore(streams));
                }
                crate::audio::route::Work::Quiet(streams) => {
                    effects.insert(0, Effect::MuteStreams(streams));
                }
                crate::audio::route::Work::Restart(restart) => {
                    if let Some(accessory) = accessory {
                        effects.push(Effect::RestartOutput {
                            id: restart.id,
                            name: restart.name,
                            accessory,
                            card: self.playback_card(),
                        });
                    }
                }
                crate::audio::route::Work::Start(start) => {
                    effects.push(Effect::StartOutput { name: start.name });
                }
                crate::audio::route::Work::SetDefault(name) => {
                    effects.push(Effect::SetDefaultSink { name });
                }
                crate::audio::route::Work::Move(moves) => {
                    effects.push(Effect::MoveStreams(moves));
                }
            }
        }
        effects
    }

    fn pace_listen(&mut self) -> Option<Effect> {
        let want = self.local_address.is_some() && !self.radio_quiet();
        if want == self.listening {
            return None;
        }
        self.listening = want;
        Some(if want { Effect::Listen } else { Effect::StopListen })
    }

    pub(crate) fn replan(&mut self, now: Clock) {
        let Some(graph) = self.last_graph.clone() else {
            return;
        };
        if self.output.needs_graph() {
            self.follow_graph(&graph, now);
        }
    }

    fn follow_graph(&mut self, graph: &crate::audio::route::Graph, now: Clock) {
        self.last_graph = Some(graph.clone());
        let waiting_for_grant = self.resume_output_on_grant
            && self.output.wants_output()
            && self.connection.stage() == Stage::Ready;
        let outcome =
            self.output.on_graph(self.config.accessory, graph, waiting_for_grant, now.mono);
        if let Some(failure) = outcome.failure {
            self.resume_output_on_grant = false;
            self.seat_audio_failure(failure, now);
        }
        self.audio.set_routed(outcome.routed);
    }

    fn take_open_link(&mut self, now: MonoMs) -> Option<Generation> {
        if (!self.preferred_connection_allowed() && !self.connection.apple_reconnect())
            || self.pairing_elsewhere(now)
            || !self.connection.may_open(now)
        {
            return None;
        }
        let generation = self.connection.generation();
        if self.link_asked == Some(generation) {
            return None;
        }
        if !self.background_requested {
            if let Some(peer) = self.peers_seated.first().copied() {
                self.request_route(peer);
            }
        }
        self.link_asked = Some(generation);
        eprintln!(
            "fleetpodsd: opening control generation={} attempt={} background={}",
            generation.0,
            self.connection.retry().0,
            self.background_requested
        );
        Some(generation)
    }

    fn commit_keyring(&mut self) {
        let Some(pending) = self.commit.take() else {
            return;
        };
        let id = pending.id();
        if let Err(reject) = self.persist_enrollment(id) {
            self.reject_enrollment(id, reject);
            return;
        }
        match pending {
            crate::fleet::commit::Pending::Grant { to, id, confirm, proposal } => {
                self.send(to, crate::fleet::letter::Letter::Grant { id, confirm, proposal });
            }
            crate::fleet::commit::Pending::Saved { to, id, proposal } => {
                self.send(
                    to,
                    crate::fleet::letter::Letter::Saved { id, members: proposal.members.clone() },
                );
            }
        }
    }
}

fn place_of(place: Option<Place>) -> (Option<bool>, Option<bool>) {
    match place {
        Some(Place::InEar) => (Some(true), Some(false)),
        Some(Place::Out) => (Some(false), Some(false)),
        Some(Place::InCase) => (Some(false), Some(true)),
        None => (None, None),
    }
}
