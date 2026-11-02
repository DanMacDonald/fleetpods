//! One input, one clock, a list of effects. Adapters run the effects.
//! They do not decide when a link opens, who owns playback, or what a letter means.

use crate::audio::play::{PlayerSeen, Profile, StreamSnap};
use crate::audio::route::{Graph, Move, StreamTarget};
use crate::fleet::members::Fleet;
use crate::fleet::postcard::{self, Postcard};
use crate::panel::ipc::Command;
use crate::pods::link::Stage;
use crate::pods::reports::Pod;
use crate::pods::setup::{self, Place};
use crate::types::{Address, Battery, Clock, Generation, MonoMs};

use crate::daemon::{host_id, Daemon};

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
    OutputStarted {
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
        keys: Option<std::collections::BTreeMap<Address, crate::fleet::members::AccessoryKeys>>,
    },
    /// This machine's Bluetooth adapter, from BlueZ rather than a fixed name.
    Controller(Address),
    /// Another paired AirPods the platform knows about. Not selected;
    /// the daemon tracks it so the user can select it (and so its keys
    /// can be harvested on first connection).
    KnownAccessory {
        address: Address,
        name: String,
    },
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
        id: crate::pods::lifecycle::RequestId,
        ok: bool,
    },
    /// How BlueZ answered the A2DP `ConnectProfile` a `ConnectTransport` sent.
    TransportReply {
        id: crate::pods::lifecycle::RequestId,
        reply: TransportReply,
    },
    BluetoothProfiles(crate::pods::lifecycle::Profiles),
    HandsfreeReply {
        id: crate::pods::lifecycle::RequestId,
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
pub enum TransportReply {
    Connected,
    /// `InProgress`: BlueZ is still busy with another connection.
    Busy,
    Failed,
    /// No answer before the call expired.
    TimedOut,
}

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
    /// The selected accessory changed. The platform follows the new
    /// selection for its device choice and persists it for restart.
    SelectAccessory {
        accessory: Option<Address>,
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
        id: crate::pods::lifecycle::RequestId,
        card: String,
        profile: String,
    },
    /// Connect the A2DP transport. Sent only when that transport is not already up.
    ConnectTransport {
        accessory: Address,
        id: crate::pods::lifecycle::RequestId,
    },
    /// Optional signaling after A2DP and service discovery; never selects HFP audio.
    ConnectHandsfree {
        accessory: Address,
        id: crate::pods::lifecycle::RequestId,
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
        id: u64,
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
        let tick = matches!(input, Input::Tick);
        let reply = self.apply(input, now);
        StepOutcome { reply, effects: self.advance_session(now, tick) }
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
                self.output_restarted(id, ok, now);
                None
            }
            Input::OutputStarted { id, ok } => {
                self.output_started(id, ok, now);
                None
            }
            Input::Command(command) => self.handle(command, now).body,
            Input::Tick => {
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
            Input::KnownAccessory { address, name } => {
                self.note_known_accessory(address, &name);
                None
            }
            Input::Paired { address, name } => {
                let known = self.config.accessory == Some(address);
                if !known {
                    self.activate_accessory(address, now);
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
                if !up {
                    // No ACL, no A2DP transport: a disconnected accessory
                    // cannot still be streaming, whatever the last transport
                    // report said.
                    self.transport_active = false;
                }
                self.note_acl(up, now.unix);
                self.note_bluez(up);
                None
            }
            Input::AttachAvailable { generation } => {
                self.attach_available(generation, now.mono);
                None
            }
            Input::ControlSocket { up, generation } => {
                self.control_socket_changed(up, generation, now);
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
                self.transport_active = false;
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
                self.audio_server_lost();
                None
            }
            Input::ProfileResult { id, ok } => {
                self.note_profile(id, ok, now);
                None
            }
            Input::TransportReply { id, reply } => {
                self.note_transport_reply(id, reply, now.mono);
                None
            }
            Input::BluetoothProfiles(profiles) => {
                self.lifecycle.note_profiles(profiles);
                None
            }
            Input::HandsfreeReply { id, ok } => {
                self.lifecycle.note_handsfree(id, ok);
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
                self.player_changed(service, playing, now);
                None
            }
            Input::Audio(graph) => {
                self.observe_audio_graph(&graph, now);
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

    pub(crate) fn begin_setup(&mut self, generation: Generation, now: Clock) {
        if let Some(hello) = self.connection.begin_setup(generation, now.mono) {
            self.effects.control_out.push((generation, hello.to_vec()));
        }
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
            self.connection.observe(packet, generation, self.local_address, now.mono);
        if let Some(follow) = follow {
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
                // Every link reports its own accessory's keys. Rewriting
                // unchanged items on each connection only makes the keyring
                // replace them, so write only what changed.
                let keys = crate::fleet::members::AccessoryKeys { irk, encryption };
                if let Some(address) = self.config.accessory {
                    if self.membership.accessory_keys(address) != Some(&keys) {
                        self.set_accessory_keys(address, keys);
                        self.queue_accessory_keys();
                    }
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

    /// Fleet/radio and persistence work. Session advancement determines when
    /// this runs relative to local playback and final state publication.
    pub(crate) fn collect_background_effects(&mut self, now: Clock) -> Vec<Effect> {
        let mut effects = Vec::new();
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
            // Once the ACL is up, audio and control setup can run alongside
            // ordinary fleet traffic. Before that, leave paging room on the
            // controller without delaying admission, handoff or discovery.
            let connecting = self.connection.stage() == Stage::Connecting
                && self.connection.may_open(now.mono)
                && self.connection.bluez() != Some(true)
                && self.connection.socket_up() != Some(true);
            if self.outbound.defer_routine(connecting) {
                eprintln!(
                    "fleetpodsd: routine fleet advertisements {}",
                    if connecting { "deferred while Bluetooth connects" } else { "resumed" }
                );
            }
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
                    if connecting {
                        eprintln!("fleetpodsd: airing changed connector preference while Bluetooth connects");
                    }
                    effects.push(Effect::Advertise(bytes));
                }
                crate::fleet::radio::Duty::Publish(crate::fleet::radio::Item::Route(bytes)) => {
                    effects.push(Effect::Advertise(bytes));
                }
                crate::fleet::radio::Duty::Release => {
                    if connecting {
                        eprintln!(
                            "fleetpodsd: released fleet advertisement while Bluetooth connects"
                        );
                    }
                    effects.push(Effect::ReleaseAir);
                }
                crate::fleet::radio::Duty::Hold => {}
            }
        }
        if let Some(bytes) = self.activity_preference.take_save() {
            effects.push(Effect::StoreActivity(bytes));
        }
        if let Some(bytes) = self.take_roster_watermark() {
            effects.push(Effect::StoreWatermark(bytes));
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
        if let Some(accessory) = self.effects.disconnect_accessory.take() {
            effects.push(Effect::Disconnect { accessory });
        }
        if let Some(accessory) = self.effects.select_accessory.take() {
            effects.push(Effect::SelectAccessory { accessory });
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
