//! One simulated desk: hosts, one accessory, and a clock.
//!
//! The world keeps the time, the mailbox, and the effects each daemon returns.
//! It does not open BlueZ, the control socket, PipeWire, or the Secret Service.
//! A passing scenario means the daemon decided correctly when those reports
//! were handed to it. Profile choice, routing, and playback confirmation stay
//! in the daemon.

use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

use fleetpods::audio::play::{Profile, StreamSnap};
use fleetpods::fleet::members::Fleet;
use fleetpods::panel::ipc::{self, Incoming};
use fleetpods::pods::setup::{self, Outgoing};

use super::accessory;
use fleetpods::step::{Effect, Input, PodSlot};
use fleetpods::types::{Address, Clock, Generation, MonoMs};
use fleetpods::Daemon;

const UNIX_ORIGIN: u64 = 1_700_000_000_000;

#[derive(Clone, Copy)]
pub enum Slot {
    Left,
    Right,
    Case,
}

enum Event {
    Lid { open: bool },
    ConnectFault { host: String },
    Profiles { host: String, offers: Vec<Profile> },
    Activate { host: String },
    Battery { host: String, slot: Slot, level: u8, charging: bool },
    Ears { host: String, left: Option<bool>, right: Option<bool>, at: MonoMs },
    RemotePlay { who: String },
    Advertisement(Vec<u8>),
    KeyringLock { host: String },
    Restart { host: String },
    Mic { host: String, clients: u32, muted: bool },
    Streams { host: String, streams: Vec<StreamSnap> },
    Tone { host: String, millis: u64 },
    Suspend { host: String },
}

struct Host {
    name: String,
    daemon: Daemon,
    address: Address,
    connect_attempts: u32,
    opened: Option<Generation>,
    restorations: Vec<StreamSnap>,
    held: Vec<StreamSnap>,
    samples: u64,
    effects: Vec<Effect>,
}

pub struct World {
    mono: u64,
    unix: u64,
    hosts: BTreeMap<String, Host>,
    pending: Vec<Event>,
    pods: Address,
    iphone: Address,
    case_open: bool,
    faults: BTreeSet<String>,
    saved: BTreeMap<String, Fleet>,
    locked: BTreeSet<String>,
    plays: u64,
    dropped_letters: BTreeSet<(String, String)>,
    blocked_frames: BTreeSet<(String, [u8; 8])>,
    letters: Vec<(String, Address, Vec<u8>)>,
    dropped_fragments: usize,
    dropped_routes: BTreeMap<String, usize>,
    advertisements: Vec<(String, Vec<u8>)>,
}

impl World {
    pub fn pair() -> Self {
        let mut world = Self::empty();
        world.add_host("gram", 0x10);
        world.add_host("xps", 0x20);
        world
    }

    pub fn joined_pair() -> Self {
        let mut world = Self::pair();
        world.invite("gram", "xps");
        world.run_for(12_000);
        let code = world.displayed_code("gram").expect("invitation code");
        world.submit("xps", "gram", &code);
        world.run_for(12_000);
        assert!(world.enrolled("gram") && world.enrolled("xps"));
        world
    }

    /// One host observes the lid. The other remains idle until a user request.
    pub fn open_case_on(&mut self, host: &str) {
        self.case_open = true;
        self.step_host(host, Input::Lid { open: true });
    }

    pub fn single() -> Self {
        let mut world = Self::empty();
        world.add_host("gram", 0x10);
        world
    }

    fn empty() -> Self {
        Self {
            mono: 0,
            unix: UNIX_ORIGIN,
            hosts: BTreeMap::new(),
            pending: Vec::new(),
            pods: Address::new([0xA0, 0, 0, 0, 0, 0x03]).expect("pods address"),
            iphone: Address::new([0x30, 0, 0, 0, 0, 0x04]).expect("phone address"),
            case_open: false,
            faults: BTreeSet::new(),
            saved: BTreeMap::new(),
            locked: BTreeSet::new(),
            plays: 0,
            dropped_letters: BTreeSet::new(),
            blocked_frames: BTreeSet::new(),
            letters: Vec::new(),
            dropped_fragments: 0,
            dropped_routes: BTreeMap::new(),
            advertisements: Vec::new(),
        }
    }

    fn add_host(&mut self, name: &str, mark: u8) {
        let address = Address::new([mark, 0, 0, 0, 0, 0x01]).expect("host address");
        let mut daemon = Daemon::new(name);
        daemon.set_local_address(Some(address));
        daemon.select_accessory(Some(self.pods));
        daemon.set_identity("AirPods Pro", "A3064");
        self.hosts.insert(
            name.to_string(),
            Host {
                name: name.to_string(),
                daemon,
                address,
                connect_attempts: 0,
                opened: None,
                restorations: Vec::new(),
                held: Vec::new(),
                samples: 0,
                effects: Vec::new(),
            },
        );
    }

    pub fn advance(&mut self, ms: u64) {
        self.mono += ms;
        self.unix += ms;
    }

    /// Deliver queued platform events, then let each daemon move its own clock.
    /// An `OpenLink` already queued is finished before that tick, so a long
    /// jump cannot expire a link the platform is about to complete.
    pub fn settle(&mut self) {
        let pending = std::mem::take(&mut self.pending);
        let (mut first, mut mic) = (Vec::new(), Vec::new());
        for event in pending {
            match event {
                Event::Mic { .. } | Event::Tone { .. } => mic.push(event),
                other => first.push(other),
            }
        }
        for event in first {
            self.apply_event(event);
        }
        self.drain();
        for name in self.hosts.keys().cloned().collect::<Vec<_>>() {
            self.step_host(&name, Input::Tick);
        }
        self.drain();
        for event in mic {
            self.apply_event(event);
        }
        self.drain();
        self.faults.clear();
    }

    /// Let the production radio scheduler transmit while simulated time advances.
    pub fn run_for(&mut self, ms: u64) {
        self.settle();
        let end = self.mono + ms;
        while self.mono < end {
            self.advance((end - self.mono).min(100));
            self.settle();
        }
    }

    pub fn command(&mut self, host: &str, line: &str) -> String {
        let mut bytes = line.as_bytes().to_vec();
        let framed = line.starts_with("fleet:") || line.starts_with("playback:");
        if framed && !bytes.ends_with(b"\n") {
            bytes.push(b'\n');
        }
        let command = match ipc::push(&bytes) {
            Incoming::Ready(command) => command,
            other => panic!("scenario command {line:?} parsed as {other:?}"),
        };
        self.step_host(host, Input::Command(command)).unwrap_or_default()
    }

    pub fn invite(&mut self, from: &str, to: &str) -> String {
        let address = self.hosts[to].address.display();
        self.command(
            from,
            &format!(r#"fleet:{{"action":"invite","address":"{address}","name":"{to}"}}"#),
        )
    }

    pub fn submit(&mut self, host: &str, inviter: &str, code: &str) -> String {
        let id =
            self.invitation_id(host).or_else(|| self.invitation_id(inviter)).unwrap_or_default();
        self.command(host, &format!(r#"fleet:{{"action":"submit","id":"{id}","code":"{code}"}}"#))
    }

    pub fn cancel(&mut self, host: &str) -> String {
        let id = self.invitation_id(host).unwrap_or_default();
        self.command(host, &format!(r#"fleet:{{"action":"cancel","id":"{id}"}}"#))
    }

    pub fn play(&mut self, host: &str) -> String {
        let now = self.unix;
        self.plays += 1;
        let token = format!("{host}-{now}-{}", self.plays);
        self.command(
            host,
            &format!(
                r#"playback:{{"kind":"play","application":"Browser","time":{now},"player":"tab","token":"{token}"}}"#
            ),
        )
    }

    pub fn close_case(&mut self) {
        self.pending.push(Event::Lid { open: false });
    }

    pub fn open_case(&mut self) {
        self.pending.push(Event::Lid { open: true });
    }

    pub fn fail_connect(&mut self, host: &str) {
        self.pending.push(Event::ConnectFault { host: host.to_string() });
    }

    pub fn offer_profiles(&mut self, host: &str, offers: Vec<Profile>) {
        self.pending.push(Event::Profiles { host: host.to_string(), offers });
    }

    pub fn activate_audio(&mut self, host: &str) {
        self.pending.push(Event::Activate { host: host.to_string() });
    }

    pub fn battery(&mut self, host: &str, slot: Slot, level: u8, charging: bool) {
        self.pending.push(Event::Battery { host: host.to_string(), slot, level, charging });
    }

    pub fn ears(&mut self, host: &str, left: Option<bool>, right: Option<bool>) {
        let at = MonoMs(self.mono);
        self.pending.push(Event::Ears { host: host.to_string(), left, right, at });
    }

    pub fn remote_playing(&mut self, who: &str) {
        self.pending.push(Event::RemotePlay { who: who.to_string() });
    }

    pub fn advertisements(&self, host: &str) -> Vec<Vec<u8>> {
        self.advertisements
            .iter()
            .filter(|(name, _)| name == host)
            .map(|(_, bytes)| bytes.clone())
            .collect()
    }

    pub fn inject(&mut self, bytes: &[u8]) {
        self.pending.push(Event::Advertisement(bytes.to_vec()));
    }

    pub fn drop_route_frames(&mut self, host: &str, count: usize) {
        self.dropped_routes.insert(host.to_owned(), count);
    }

    pub fn drop_letter(&mut self, host: &str, kind: &str) {
        self.dropped_letters.insert((host.to_owned(), kind.to_owned()));
    }

    pub fn dropped_fragments(&self) -> usize {
        self.dropped_fragments
    }

    pub fn saved_fleet(&self, host: &str) -> Option<Fleet> {
        self.saved.get(host).cloned()
    }

    pub fn replay_letter(&mut self, host: &str, kind: &str) {
        let (_, to, bytes) = self
            .letters
            .iter()
            .find(|(from, _, bytes)| {
                from == host
                    && serde_json::from_slice::<serde_json::Value>(bytes)
                        .ok()
                        .is_some_and(|value| value["type"] == kind)
            })
            .expect("captured enrollment letter");
        // A fresh fragment id bypasses transport deduplication: the enrollment
        // state machine itself must reject the old invitation/proof.
        for frame in
            fleetpods::fleet::radio::fragments(bytes, self.hosts[host].address, *to, [0xfe; 8])
        {
            self.pending.push(Event::Advertisement(frame));
        }
    }

    pub fn lock_keyring(&mut self, host: &str) {
        self.pending.push(Event::KeyringLock { host: host.to_string() });
    }

    pub fn restart(&mut self, host: &str) {
        self.pending.push(Event::Restart { host: host.to_string() });
    }

    pub fn mic(&mut self, host: &str, clients: u32, muted: bool) {
        self.pending.push(Event::Mic { host: host.to_string(), clients, muted });
    }

    pub fn hold_streams(&mut self, host: &str, streams: Vec<StreamSnap>) {
        self.pending.push(Event::Streams { host: host.to_string(), streams });
    }

    pub fn tone(&mut self, host: &str, millis: u64) {
        self.pending.push(Event::Tone { host: host.to_string(), millis });
    }

    pub fn suspend(&mut self, host: &str) {
        self.pending.push(Event::Suspend { host: host.to_string() });
    }

    pub fn status(&self, host: &str) -> serde_json::Value {
        self.hosts[host].daemon.status_value(self.clock())
    }

    pub fn displayed_code(&self, host: &str) -> Option<String> {
        match self.status(host)["fleet"]["code"].as_str() {
            Some(code) if !code.is_empty() => Some(code.to_string()),
            _ => None,
        }
    }

    pub fn invitation_id(&self, host: &str) -> Option<String> {
        match self.status(host)["fleet"]["id"].as_str() {
            Some(id) if !id.is_empty() => Some(id.to_string()),
            _ => None,
        }
    }

    pub fn phase(&self, host: &str) -> String {
        text(&self.status(host), &["fleet", "phase"])
    }

    pub fn stage(&self, host: &str) -> String {
        text(&self.status(host), &["control_connection", "stage"])
    }

    pub fn enrolled(&self, host: &str) -> bool {
        self.status(host)["fleet"]["enrolled"] == true
    }

    pub fn seat_up(&self, host: &str) -> bool {
        let status = self.status(host);
        status["connected"] == true || status["bluez_connected"] == true
    }

    pub fn discovered(&self, host: &str, label: &str) -> bool {
        devices(&self.status(host))
            .any(|device| device["label"] == label && device["discovered"] == true)
    }

    pub fn lists_member(&self, host: &str, label: &str) -> bool {
        devices(&self.status(host))
            .any(|device| device["label"] == label && device["discovered"] != true)
    }

    pub fn holder(&self, host: &str) -> String {
        text(&self.status(host), &["audio_owner", "holder"])
    }

    pub fn selected_profile(&self, host: &str) -> Option<String> {
        self.hosts[host].daemon.audio().selected().map(str::to_string)
    }

    pub fn connect_attempts(&self, host: &str) -> u32 {
        self.hosts[host].connect_attempts
    }

    pub fn restorations(&self, host: &str) -> &[StreamSnap] {
        &self.hosts[host].restorations
    }

    pub fn mic_samples(&self, host: &str) -> u64 {
        self.hosts[host].samples
    }

    fn step_host(&mut self, name: &str, input: Input) -> Option<String> {
        let clock = self.clock();
        let host = self.hosts.get_mut(name).expect("host");
        let outcome = host.daemon.step(input, clock);
        host.effects.extend(outcome.effects);
        outcome.reply
    }

    fn apply_event(&mut self, event: Event) {
        match event {
            Event::Lid { open } => {
                self.case_open = open;
                let names = self.hosts.keys().cloned().collect::<Vec<_>>();
                for name in names {
                    self.step_host(&name, Input::Lid { open });
                }
            }
            Event::ConnectFault { host } => {
                self.faults.insert(host);
            }
            Event::Profiles { host, offers } => {
                self.step_host(&host, Input::Profiles(offers));
            }
            Event::Activate { host } => {
                self.step_host(&host, Input::ActivateAudio);
            }
            Event::Battery { host, slot, level, charging } => {
                self.step_host(&host, Input::Battery { slot: slot.into(), level, charging });
            }
            Event::Ears { host, left, right, at } => {
                self.step_host(&host, Input::Ears { left, right, at });
            }
            Event::RemotePlay { who } => {
                let remote = Some(self.player_address(&who));
                let names = self.hosts.keys().cloned().collect::<Vec<_>>();
                for name in names {
                    self.step_host(&name, Input::PodsConfirmed { local: false, remote });
                }
            }
            Event::Advertisement(bytes) => {
                for name in self.hosts.keys().cloned().collect::<Vec<_>>() {
                    self.step_host(&name, Input::Advertisement(bytes.clone()));
                }
            }
            Event::KeyringLock { host } => {
                self.locked.insert(host.clone());
                self.step_host(&host, Input::KeyringLocked(true));
            }
            Event::Restart { host } => self.restart_host(&host),
            Event::Suspend { host } => {
                self.step_host(&host, Input::Suspend);
            }
            Event::Streams { host, streams } => {
                if let Some(entry) = self.hosts.get_mut(&host) {
                    entry.held = streams.clone();
                }
                self.step_host(&host, Input::Streams(streams));
            }
            Event::Mic { host, clients, muted } => {
                self.step_host(&host, Input::Microphone { streaming: clients > 0, muted });
                let capturing = self.hosts[&host].daemon.microphone().capturing();
                if !capturing {
                    self.hosts.get_mut(&host).unwrap().samples = 0;
                }
            }
            Event::Tone { host, millis } => {
                let entry = self.hosts.get_mut(&host).expect("host");
                if entry.daemon.microphone().capturing() {
                    entry.samples = millis.saturating_mul(64);
                } else {
                    entry.samples = 0;
                }
            }
        }
    }

    fn drain(&mut self) {
        for _ in 0..32 {
            if !self.dispatch() {
                return;
            }
        }
        panic!("effects did not settle");
    }

    fn dispatch(&mut self) -> bool {
        let mut batch = Vec::new();
        for name in self.hosts.keys().cloned().collect::<Vec<_>>() {
            let effects = std::mem::take(&mut self.hosts.get_mut(&name).unwrap().effects);
            for effect in effects {
                batch.push((name.clone(), effect));
            }
        }
        if batch.is_empty() {
            return false;
        }
        for (name, effect) in batch {
            self.run_effect(&name, effect);
        }
        true
    }

    fn run_effect(&mut self, name: &str, effect: Effect) {
        match effect {
            Effect::OpenLink { generation } => self.run_open(name, generation),
            // Send/Announce are observation effects. Production transmits the
            // paced Advertise frames, including the sender and fragment identity.
            Effect::Send { to, bytes } => {
                if let Some(kind) = serde_json::from_slice::<serde_json::Value>(&bytes)
                    .ok()
                    .and_then(|value| value["type"].as_str().map(str::to_owned))
                {
                    if self.dropped_letters.contains(&(name.to_owned(), kind)) {
                        self.blocked_frames.insert((
                            name.to_owned(),
                            Sha256::digest(&bytes)[..8].try_into().unwrap(),
                        ));
                    }
                }
                self.letters.push((name.to_owned(), to, bytes));
            }
            Effect::Advertise(bytes) => {
                self.advertisements.push((name.to_owned(), bytes.clone()));
                if bytes.first() == Some(&fleetpods::fleet::route_air::VERSION) {
                    if let Some(left) = self.dropped_routes.get_mut(name) {
                        if *left > 0 {
                            *left -= 1;
                            self.dropped_fragments += 1;
                            return;
                        }
                    }
                }
                if bytes.first() == Some(&1)
                    && bytes.len() > fleetpods::fleet::radio::HEADER
                    && self
                        .blocked_frames
                        .contains(&(name.to_owned(), bytes[23..31].try_into().unwrap()))
                {
                    self.dropped_fragments += 1;
                    return;
                }
                for other in self
                    .hosts
                    .keys()
                    .filter(|other| other.as_str() != name)
                    .cloned()
                    .collect::<Vec<_>>()
                {
                    self.step_host(&other, Input::Advertisement(bytes.clone()));
                }
            }
            Effect::Announce(_) => {}
            Effect::Restore(streams) => {
                self.hosts.get_mut(name).expect("host").restorations.extend(streams);
            }
            Effect::WriteKeyring(fleet) => {
                if self.locked.contains(name) {
                    self.step_host(name, Input::KeyringFailed);
                } else {
                    self.saved.insert(name.to_string(), fleet);
                    self.step_host(name, Input::KeyringStored);
                }
            }
            Effect::WriteControl { generation, bytes, .. } => {
                self.answer_accessory(name, generation, &bytes);
            }
            Effect::Disconnect { accessory } => {
                let generation = self.hosts[name].daemon.connection().generation();
                self.step_host(name, Input::ControlSocket { up: false, generation });
                self.step_host(name, Input::Bluez(false));
                self.step_host(name, Input::LinkReleased { accessory, generation, ok: true });
            }
            Effect::SelectProfile { .. }
            | Effect::RearmOutput { .. }
            | Effect::RestartOutput { .. }
            | Effect::StartOutput { .. }
            | Effect::ConnectTransport { .. }
            | Effect::RemoveDevice { .. }
            | Effect::ShowMicrophone { .. }
            | Effect::HideMicrophone
            | Effect::WriteSecret { .. }
            | Effect::SetDefaultSink { .. }
            | Effect::MuteStreams(_)
            | Effect::MoveStreams(_)
            | Effect::PausePlayers(_)
            | Effect::ReleaseAir
            | Effect::Listen
            | Effect::StopListen
            | Effect::FollowLink
            | Effect::StoreDevices(_)
            | Effect::StoreWatermark(_) => {}
        }
    }

    fn answer_accessory(&mut self, name: &str, generation: Generation, bytes: &[u8]) {
        if bytes == [4, 0, 4, 0, 9, 0, 6, 1, 0, 0, 0] {
            // CLAIM is a request, not a daemon-invented ownership report.
            // The simulated accessory grants it and reports the source to both hosts.
            let mut source = vec![4, 0, 4, 0, 0x0e, 0];
            source.extend(self.hosts[name].address.bytes().into_iter().rev());
            source.push(2);
            let connected: Vec<_> = self
                .hosts
                .iter()
                .filter(|(_, host)| {
                    host.daemon.connection().stage() == fleetpods::pods::link::Stage::Ready
                })
                .map(|(name, host)| (name.clone(), host.daemon.connection().generation()))
                .collect();
            for (target, generation) in connected {
                let ownership = vec![4, 0, 4, 0, 9, 0, 6, u8::from(target == name), 0, 0, 0];
                self.step_host(&target, Input::Accessory { generation, packet: ownership });
                self.step_host(&target, Input::Accessory { generation, packet: source.clone() });
            }
            return;
        }
        let Some(kind) = setup::classify_outgoing(bytes) else {
            return;
        };
        let packets = match kind {
            Outgoing::Handshake => vec![accessory::handshake_ack()],
            Outgoing::Features => vec![accessory::features_ack()],
            Outgoing::Notifications => vec![accessory::sample_battery(), accessory::sample_ears()],
        };
        for packet in packets {
            self.step_host(name, Input::Accessory { generation, packet });
        }
    }

    fn run_open(&mut self, name: &str, generation: Generation) {
        let Some((stage, current, reconnect)) = self.hosts.get(name).map(|host| {
            (
                host.daemon.connection().stage(),
                host.daemon.connection().generation(),
                host.daemon.connection().apple_reconnect(),
            )
        }) else {
            return;
        };
        if !stage.is_opening() || current != generation {
            return;
        }
        let host = self.hosts.get_mut(name).unwrap();
        if host.opened != Some(generation) {
            host.connect_attempts += 1;
            host.opened = Some(generation);
        }
        if self.faults.remove(name) {
            self.step_host(name, Input::LinkFailed { generation, busy: false });
            return;
        }
        if !self.case_open || reconnect {
            return;
        }
        if self.hosts[name].daemon.audio().selected().is_none() {
            self.step_host(name, Input::Profiles(default_profiles()));
            self.step_host(name, Input::ActivateAudio);
        }
        self.step_host(name, Input::Bluez(true));
        self.step_host(name, Input::ControlSocket { up: true, generation });
    }

    fn restart_host(&mut self, name: &str) {
        let Some(previous) = self.hosts.remove(name) else {
            return;
        };
        let saved = self.saved.get(name).cloned();
        let locked = self.locked.contains(name);
        let mut daemon = Daemon::new(previous.name.clone());
        daemon.set_local_address(Some(previous.address));
        daemon.select_accessory(Some(self.pods));
        daemon.set_identity("AirPods Pro", "A3064");
        if let Some(fleet) = saved {
            daemon.restore_fleet(fleet);
        }
        if locked {
            daemon.set_keyring_locked(true);
        }
        self.hosts.insert(
            name.to_string(),
            Host {
                name: previous.name,
                daemon,
                address: previous.address,
                connect_attempts: 0,
                opened: None,
                restorations: previous.restorations,
                held: Vec::new(),
                samples: 0,
                effects: Vec::new(),
            },
        );
    }

    fn player_address(&self, who: &str) -> Address {
        self.hosts.get(who).map(|host| host.address).unwrap_or(self.iphone)
    }

    fn clock(&self) -> Clock {
        Clock::new(self.mono, self.unix)
    }
}

impl From<Slot> for PodSlot {
    fn from(slot: Slot) -> Self {
        match slot {
            Slot::Left => Self::Left,
            Slot::Right => Self::Right,
            Slot::Case => Self::Case,
        }
    }
}

fn default_profiles() -> Vec<Profile> {
    vec![
        profile("a2dp-sink-aac", "High Fidelity Playback (A2DP Sink, codec AAC)", 80, 0),
        profile("a2dp-sink-sbc_xq", "High Fidelity Playback (A2DP Sink, codec SBC-XQ)", 10, 0),
        profile("headset-head-unit-msbc", "Headset Head Unit (HSP/HFP, codec mSBC)", 100, 1),
    ]
}

fn devices(status: &serde_json::Value) -> impl Iterator<Item = &serde_json::Value> {
    status["devices"].as_array().into_iter().flatten()
}

fn text(value: &serde_json::Value, path: &[&str]) -> String {
    let mut cursor = value;
    for key in path {
        cursor = &cursor[*key];
    }
    cursor.as_str().unwrap_or("").to_string()
}

pub fn profile(name: &str, description: &str, priority: i32, sources: u32) -> Profile {
    Profile {
        name: name.to_string(),
        description: description.to_string(),
        priority,
        available: true,
        sinks: 1,
        sources,
    }
}
