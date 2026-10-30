//! The process loop. Commands and adapter reports enter through `Daemon::step`.
//! The BlueZ thread sends the same [`Input`] values the acceptance world does.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::panel::ipc::Command;
use crate::step::{Effect, Input, TransportReply};
use crate::types::{Address, Clock, Generation};
use crate::Daemon;

use super::bluez::{self, Bus, CallResult, Device, Notice};
use super::control::{Attach, ControlLink};
use super::keyring::{self, Load};
use super::pipewire::{self, Op};
use super::players;
use super::wake::Wake;

enum Job {
    Open {
        generation: Generation,
    },
    Write {
        address: Address,
        bytes: Vec<u8>,
    },
    ConnectTransport {
        address: Address,
    },
    RestartOutput {
        id: u64,
        address: Address,
    },
    Disconnect {
        address: Address,
    },
    /// Drop the kernel pairing for this accessory.
    Remove {
        address: Address,
    },
    /// One frame the daemon already chose.
    Advertise(Vec<u8>),
    /// Drop the advertisement. Discovery stays up.
    ReleaseAir,
    /// Keep discovery running.
    Listen,
    /// Stop discovery.
    StopListen,
    /// The daemon is not holding the link. Attach if the accessory is already connected.
    Follow,
    Stop,
}

pub struct Session {
    daemon: Daemon,
    radio: Option<Radio>,
    reports: Receiver<Input>,
    /// Poked for every report, so the process loop handles it at once
    /// instead of at its next timer.
    reports_wake: Option<Wake>,
    audio: Option<pipewire::Handle>,
    players: Option<players::Handle>,
    secrets: Option<Sender<SecretJob>>,
    secret_thread: Option<JoinHandle<()>>,
    warned: bool,
    watermark: Option<PathBuf>,
    mic_path: std::sync::Arc<crate::audio::queue::MicPath>,
    mic_source: Option<super::micsource::Running>,
    /// Last streaming and mute report taken from the hidden source.
    mic_picture: Option<(bool, bool)>,
    /// When an unavailable keyring was last asked for the fleet again.
    keyring_retry_at: Option<crate::types::MonoMs>,
}

struct Radio {
    jobs: Sender<Job>,
    wake: Wake,
    thread: Option<JoinHandle<()>>,
}

impl Radio {
    fn send(&self, job: Job) {
        let _ = self.jobs.send(job);
        self.wake.poke();
    }
}

impl Session {
    pub fn start(name: impl Into<String>) -> Self {
        let (reports_tx, reports_rx, reports_wake) = waking_reports();
        let configured = configured_accessory();
        let radio = spawn_radio(reports_tx.clone(), configured);
        let audio = pipewire::spawn(reports_tx.clone());
        let players = players::spawn(reports_tx.clone());
        let (secrets, secret_thread) = spawn_secrets(reports_tx);
        let mut daemon = Daemon::new(name);
        daemon.load_apple_names();
        let mic_path = std::sync::Arc::clone(&daemon.mic_path);
        remember_fleet(&mut daemon);
        Self {
            daemon,
            radio,
            reports: reports_rx,
            reports_wake,
            audio,
            players,
            secrets,
            secret_thread,
            warned: false,
            watermark: None,
            mic_path,
            mic_source: None,
            mic_picture: None,
            keyring_retry_at: None,
        }
    }

    /// Load the durable roster watermark and sibling activity preference, and
    /// remember their state directory for subsequent writes.
    pub fn use_roster_file(&mut self, path: &std::path::Path) {
        self.watermark = Some(path.to_path_buf());
        if let Ok(bytes) = std::fs::read(path) {
            self.daemon.import_roster_watermark(&bytes);
        }
        if let Ok(bytes) = std::fs::read(path.with_file_name("activity.preference")) {
            self.daemon.activity_preference.restore(&bytes);
        }
    }

    pub fn command(&mut self, command: Command, now: Clock) -> Option<String> {
        if command == Command::FleetReset {
            return Some(self.reset_fleet(now));
        }
        if let Command::Fleet(Ok(crate::panel::ipc::FleetCommand::Remove { address })) = command {
            return Some(self.remove_fleet_member(address, now));
        }
        if let Command::Fleet(Ok(crate::panel::ipc::FleetCommand::LeaveInvitation { id })) = command
        {
            // The user explicitly chose to leave, but the incoming invitation
            // and its offer must survive the durable local fleet reset. The
            // daemon decides which invitation may be held aside; this adapter
            // only runs the deletion between taking and restoring it.
            let save = match self.daemon.take_invitation(id) {
                Ok(save) => save,
                Err(error) => return Some(error.into()),
            };
            let result = self.reset_fleet(now);
            self.daemon.restore_invitation(save);
            return Some(if result.starts_with("error:") { result } else { "ok\n".into() });
        }
        if let Command::Fleet(Ok(crate::panel::ipc::FleetCommand::Join {
            address,
            ref name,
            leave: true,
        })) = command
        {
            if let Err(error) = self.daemon.check_rejoin(address) {
                return Some(error.into());
            }
            let result = self.reset_fleet(now);
            if result.starts_with("error:") {
                return Some(result);
            }
            let outcome = self.daemon.step(
                Input::Command(Command::Fleet(Ok(crate::panel::ipc::FleetCommand::Join {
                    address,
                    name: name.clone(),
                    leave: false,
                }))),
                now,
            );
            self.perform(&outcome.effects, now);
            return outcome.reply;
        }
        let outcome = self.daemon.step(Input::Command(command), now);
        self.perform(&outcome.effects, now);
        self.observe_microphone(now);
        outcome.reply
    }

    fn reset_fleet(&mut self, now: Clock) -> String {
        let (reply, done) = mpsc::channel();
        let Some(worker) = &self.secrets else {
            return "error: keyring worker unavailable; fleet was not reset\n".into();
        };
        if worker.send(SecretJob::ResetFleet(reply)).is_err() {
            return "error: keyring worker unavailable; fleet was not reset\n".into();
        }
        // The session loop is paused here. The FIFO worker finishes every prior
        // save/reload before deleting, and no new enrollment write can be queued.
        // Do not time out and resume using the old key while deletion is in flight.
        match done.recv() {
            Ok(Ok(())) => {}
            Ok(Err(err)) => return format!("error: fleet reset failed: {err}\n"),
            Err(_) => return "error: keyring worker stopped during fleet reset\n".into(),
        }
        self.daemon.reset_fleet();
        self.keyring_retry_at = None;
        self.radio(Job::ReleaseAir);
        self.drain_prior_keyring_reports(now);
        let outcome = self.daemon.step(Input::Tick, now);
        self.perform(&outcome.effects, now);
        "Fleet reset on this machine. AirPods pairing preserved.\n".into()
    }

    fn remove_fleet_member(&mut self, address: Address, now: Clock) -> String {
        let fleet = match self.daemon.fleet_without(address) {
            Ok(Some(fleet)) => fleet,
            Ok(None) => return "Device is not a member of this machine's fleet.\n".into(),
            Err(error) => return format!("error: {error}\n"),
        };
        let (reply, done) = mpsc::channel();
        let Some(worker) = &self.secrets else {
            return "error: keyring worker unavailable; member was not removed\n".into();
        };
        if worker.send(SecretJob::ReplaceFleet { fleet, reply }).is_err() {
            return "error: keyring worker unavailable; member was not removed\n".into();
        }
        // Serialize behind earlier saves. A failed save leaves memory unchanged.
        match done.recv() {
            Ok(Ok(())) => {}
            Ok(Err(err)) => return format!("error: fleet member removal failed: {err}\n"),
            Err(_) => return "error: keyring worker stopped during member removal\n".into(),
        }
        self.daemon.remove_fleet_member(address);
        self.drain_prior_keyring_reports(now);
        let outcome = self.daemon.step(Input::Tick, now);
        self.perform(&outcome.effects, now);
        format!(
            "Removed {} from this machine's fleet. AirPods pairing preserved.\n",
            address.display()
        )
    }

    fn drain_prior_keyring_reports(&mut self, now: Clock) {
        // Replies to earlier saves/reloads must not resurrect old membership.
        // Non-keyring observations remain valid and are processed normally.
        let pending: Vec<_> = self.reports.try_iter().collect();
        for input in pending {
            match input {
                Input::KeyringStored | Input::KeyringFailed => continue,
                Input::KeyringUnlocked { keys, .. } => {
                    if !self.daemon.membership().accessory_keys_ready() {
                        if let Some(keys) = keys {
                            self.daemon.set_accessory_keys(keys);
                        }
                    }
                    continue;
                }
                _ => {}
            }
            let outcome = self.daemon.step(input, now);
            self.perform(&outcome.effects, now);
        }
    }

    /// Readable when a report waits for [`Session::poll`].
    pub fn wake_fd(&self) -> Option<std::os::fd::RawFd> {
        self.reports_wake.as_ref().map(Wake::raw)
    }

    pub fn poll(&mut self, now: Clock) {
        if let Some(wake) = &self.reports_wake {
            wake.drain();
        }
        while let Ok(input) = self.reports.try_recv() {
            let outcome = self.daemon.step(input, now);
            self.perform(&outcome.effects, now);
        }
        let outcome = self.daemon.step(Input::Tick, now);
        self.perform(&outcome.effects, now);
        self.observe_microphone(now);
        self.retry_locked_keyring(now);
    }

    /// Retry locked or failed startup reads until the keyring can be read.
    fn retry_locked_keyring(&mut self, now: Clock) {
        const RETRY_MS: u64 = 5_000;
        if !self.daemon.keyring_locked() {
            self.keyring_retry_at = None;
            return;
        }
        if self.keyring_retry_at.is_some_and(|at| now.mono.saturating_since(at) < RETRY_MS) {
            return;
        }
        self.keyring_retry_at = Some(now.mono);
        self.secret(SecretJob::Reload);
    }

    fn observe_microphone(&mut self, now: Clock) {
        use std::sync::atomic::Ordering;
        let picture = (
            self.mic_path.streaming.load(Ordering::Relaxed),
            self.mic_path.muted.load(Ordering::Relaxed),
        );
        if self.mic_picture == Some(picture) {
            return;
        }
        self.mic_picture = Some(picture);
        let outcome =
            self.daemon.step(Input::Microphone { streaming: picture.0, muted: picture.1 }, now);
        self.perform(&outcome.effects, now);
    }

    pub fn status(&self, now: Clock) -> String {
        self.daemon.status_line(now)
    }

    pub fn shutdown(&mut self) {
        if let Some(radio) = &self.radio {
            radio.send(Job::Stop);
        }
        if let Some(audio) = &mut self.audio {
            audio.stop();
        }
        if let Some(players) = &mut self.players {
            players.stop();
        }
        if let Some(secrets) = &self.secrets {
            let _ = secrets.send(SecretJob::Stop);
        }
        if let Some(radio) = &mut self.radio {
            if let Some(thread) = radio.thread.take() {
                let _ = thread.join();
            }
        }
        if let Some(thread) = self.secret_thread.take() {
            let _ = thread.join();
        }
    }

    fn perform(&mut self, effects: &[Effect], _now: Clock) {
        let mut ops = Vec::new();
        for effect in effects {
            match effect {
                Effect::MuteStreams(streams) => {
                    ops.extend(streams.iter().copied().map(Op::Mute));
                }
                Effect::SetDefaultSink { name } => ops.push(Op::Default(name.clone())),
                Effect::StartOutput { name } => ops.push(Op::StartOutput(name.clone())),
                Effect::RearmOutput { id, index, name } => {
                    ops.push(Op::RearmOutput { id: *id, index: *index, name: name.clone() })
                }
                Effect::MoveStreams(move_streams) => {
                    for stream in &move_streams.streams {
                        ops.push(Op::Move { sink: move_streams.sink.clone(), stream: *stream });
                    }
                }
                Effect::Restore(streams) => {
                    ops.extend(streams.iter().cloned().map(Op::Restore));
                }
                other => {
                    self.flush_audio(&mut ops);
                    self.perform_one(other);
                }
            }
        }
        self.flush_audio(&mut ops);
    }

    fn perform_one(&mut self, effect: &Effect) {
        match effect {
            Effect::OpenLink { generation } => self.radio(Job::Open { generation: *generation }),
            Effect::FollowLink => self.radio(Job::Follow),
            Effect::ShowMicrophone { address, rate, description } => {
                self.park_microphone();
                self.mic_source = super::micsource::Running::start(
                    std::sync::Arc::clone(&self.mic_path),
                    *address,
                    *rate,
                    description.clone(),
                );
                if let Some(audio) = &self.audio {
                    audio.send(vec![pipewire::Op::DefaultSource(
                        crate::audio::mic::SOURCE_NAME.into(),
                    )]);
                }
            }
            Effect::HideMicrophone => self.park_microphone(),
            Effect::WriteControl { accessory, bytes, .. } => {
                self.radio(Job::Write { address: *accessory, bytes: bytes.clone() })
            }
            Effect::SelectProfile { card, profile } => {
                if let Some(audio) = &self.audio {
                    audio.send(vec![pipewire::Op::Profile {
                        card: card.clone(),
                        name: profile.clone(),
                    }]);
                }
            }
            Effect::ConnectTransport { accessory } => {
                self.radio(Job::ConnectTransport { address: *accessory })
            }
            Effect::RestartOutput { id, accessory, card, .. } => match (card, &self.audio) {
                // Rebuilding the card's nodes is local and quick; a Bluetooth
                // profile reconnect is slower and the pods can refuse it.
                (Some((card, profile)), Some(audio)) => audio.send(vec![pipewire::Op::Refresh {
                    card: card.clone(),
                    name: profile.clone(),
                    restart: *id,
                }]),
                _ => self.radio(Job::RestartOutput { id: *id, address: *accessory }),
            },

            Effect::PausePlayers(names) => {
                if let Some(players) = &self.players {
                    players.pause(names.clone());
                }
            }
            Effect::SuspendOutput { name } => {
                if let Some(audio) = &self.audio {
                    audio.send(vec![Op::Suspend(name.clone())]);
                }
            }
            Effect::ResumePlayers(names) => {
                if let Some(players) = &self.players {
                    players.play(names.clone());
                }
            }
            Effect::Disconnect { accessory } => self.radio(Job::Disconnect { address: *accessory }),
            Effect::RemoveDevice { accessory } => self.radio(Job::Remove { address: *accessory }),
            Effect::WriteSecret { name, bytes } => {
                self.secret(SecretJob::Item { name: name.clone(), bytes: bytes.clone() });
            }
            Effect::WriteKeyring(fleet) => self.secret(SecretJob::Fleet(fleet.clone())),
            // Send is the in-process postcard. Announce is presence, which is not
            // a version-1 fragment. This thread only writes bytes the daemon chose.
            Effect::Send { .. } | Effect::Announce(_) => {}
            Effect::Advertise(bytes) => self.radio(Job::Advertise(bytes.clone())),
            Effect::ReleaseAir => self.radio(Job::ReleaseAir),
            Effect::Listen => self.radio(Job::Listen),
            Effect::StopListen => self.radio(Job::StopListen),
            Effect::StoreWatermark(bytes) => self.store_watermark(bytes),
            Effect::StoreActivity(bytes) => {
                if let Some(path) = &self.watermark {
                    Self::store_state(&path.with_file_name("activity.preference"), bytes);
                }
            }
            Effect::StoreDevices(labels) => {
                if let Err(err) = crate::devices::store(labels) {
                    eprintln!("fleetpodsd: could not save device names: {err}");
                }
            }
            Effect::MuteStreams(_)
            | Effect::RearmOutput { .. }
            | Effect::StartOutput { .. }
            | Effect::SetDefaultSink { .. }
            | Effect::MoveStreams(_)
            | Effect::Restore(_) => {}
        }
    }

    fn park_microphone(&mut self) {
        use std::sync::atomic::Ordering;
        if let Some(running) = self.mic_source.take() {
            running.stop();
        }
        self.mic_path.streaming.store(false, Ordering::Relaxed);
        self.mic_path.muted.store(false, Ordering::Relaxed);
        self.mic_path.format_ok.store(false, Ordering::Relaxed);
        self.mic_picture = Some((false, false));
    }

    fn flush_audio(&mut self, ops: &mut Vec<Op>) {
        if ops.is_empty() {
            return;
        }
        match self.audio.as_ref() {
            Some(audio) => audio.send(std::mem::take(ops)),
            None => {
                ops.clear();
                if !self.warned {
                    eprintln!("fleetpodsd: PipeWire is not attached");
                    self.warned = true;
                }
            }
        }
    }

    fn radio(&mut self, job: Job) {
        match self.radio.as_ref() {
            Some(radio) => radio.send(job),
            None => eprintln!("fleetpodsd: bluetooth worker is not running"),
        }
    }

    fn store_watermark(&self, bytes: &[u8]) {
        let Some(path) = &self.watermark else {
            return;
        };
        Self::store_state(path, bytes);
    }

    fn store_state(path: &std::path::Path, bytes: &[u8]) {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = path.with_extension("tmp");
        let Ok(mut file) = std::fs::File::create(&tmp) else {
            return;
        };
        if file.write_all(bytes).is_err() {
            return;
        }
        let _ = file.sync_all();
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
        let _ = std::fs::rename(&tmp, path);
    }

    fn secret(&mut self, job: SecretJob) {
        if self.secrets.as_ref().is_none_or(|secrets| secrets.send(job).is_err()) {
            eprintln!("fleetpodsd: keyring thread is not running");
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn remember_fleet(daemon: &mut Daemon) {
    restore_saved_fleet(daemon, keyring::load());
    if let Some(keys) = load_accessory_keys() {
        daemon.set_accessory_keys(keys);
    }
}

fn restore_saved_fleet(daemon: &mut Daemon, loaded: Result<Load, String>) {
    match loaded {
        Ok(Load::Open(fleet)) => daemon.restore_fleet(fleet),
        Ok(Load::Missing) => {}
        Ok(Load::Locked) => {
            eprintln!("fleetpodsd: the keyring is locked; the fleet loads once it is unlocked");
            daemon.set_keyring_locked(true);
            daemon.mark_secrets_unavailable();
        }
        Err(err) => {
            eprintln!("fleetpodsd: keyring is not available; retrying every five seconds: {err}");
            daemon.set_keyring_locked(true);
            daemon.mark_secrets_unavailable();
        }
    }
}

fn load_accessory_keys() -> Option<crate::fleet::members::AccessoryKeys> {
    let irk = keyring::load_secret(keyring::IRK_ITEM).ok().flatten()?;
    let encryption = keyring::load_secret(keyring::ENC_ITEM).ok().flatten()?;
    Some(crate::fleet::members::AccessoryKeys {
        irk: irk.as_slice().try_into().ok()?,
        encryption: encryption.as_slice().try_into().ok()?,
    })
}

enum SecretJob {
    ResetFleet(Sender<Result<(), String>>),
    ReplaceFleet {
        fleet: crate::fleet::members::Fleet,
        reply: Sender<Result<(), String>>,
    },
    Fleet(crate::fleet::members::Fleet),
    Item {
        name: String,
        bytes: Vec<u8>,
    },
    /// Retry reading the keyring. Nothing is reported while locked or unavailable.
    Reload,
    Stop,
}

enum CallKind {
    StartDiscovery,
    StopDiscovery,
    Disconnect {
        address: Address,
        generation: Generation,
    },
    Profile,
    Advertise,
    Unadvertise,
    Remove,
    RestartDisconnect {
        id: u64,
        address: Address,
    },
    RestartConnect {
        id: u64,
    },
    /// Our own write of Adapter1 `Powered`. The value is the one we asked for.
    Power(bool),
}

struct PendingCall {
    serial: u32,
    kind: CallKind,
    sent: Instant,
}

/// Worker threads report through the returned sender. A relay forwards each
/// report and pokes the wake, so a blocked process loop sees it immediately.
fn waking_reports() -> (Sender<Input>, Receiver<Input>, Option<Wake>) {
    let (raw_tx, raw_rx) = mpsc::channel();
    let Ok((wake, relay_wake)) = Wake::pair() else {
        return (raw_tx, raw_rx, None);
    };
    let (tx, rx) = mpsc::channel();
    let relay = thread::Builder::new().name("fleetpods-reports".into()).spawn(move || {
        while let Ok(input) = raw_rx.recv() {
            if tx.send(input).is_err() {
                break;
            }
            relay_wake.poke();
        }
    });
    match relay {
        Ok(_) => (raw_tx, rx, Some(wake)),
        Err(_) => {
            let (raw_tx, raw_rx) = mpsc::channel();
            (raw_tx, raw_rx, None)
        }
    }
}

fn spawn_radio(reports: Sender<Input>, configured: Option<Address>) -> Option<Radio> {
    let (jobs_tx, jobs_rx) = mpsc::channel();
    let (wake, thread_wake) = Wake::pair().ok()?;
    let thread = thread::Builder::new()
        .name("fleetpods-bluez".into())
        .spawn(move || worker(jobs_rx, reports, configured, thread_wake))
        .ok()?;
    Some(Radio { jobs: jobs_tx, wake, thread: Some(thread) })
}

fn spawn_secrets(reports: Sender<Input>) -> (Option<Sender<SecretJob>>, Option<JoinHandle<()>>) {
    let (tx, rx) = mpsc::channel();
    let thread = thread::Builder::new()
        .name("fleetpods-keyring".into())
        .spawn(move || secret_thread(rx, reports))
        .ok();
    if thread.is_some() {
        (Some(tx), thread)
    } else {
        (None, None)
    }
}

fn secret_thread(jobs: Receiver<SecretJob>, reports: Sender<Input>) {
    while let Ok(job) = jobs.recv() {
        match job {
            SecretJob::Stop => break,
            SecretJob::ResetFleet(reply) => {
                let _ = reply.send(keyring::clear_fleet());
            }
            SecretJob::ReplaceFleet { fleet, reply } => {
                let _ = reply.send(keyring::store(&fleet));
            }
            SecretJob::Fleet(fleet) => {
                let input = match keyring::store(&fleet) {
                    Ok(()) => Input::KeyringStored,
                    Err(err) => {
                        eprintln!("fleetpodsd: could not store the fleet: {err}");
                        Input::KeyringFailed
                    }
                };
                let _ = reports.send(input);
            }
            SecretJob::Item { name, bytes } => {
                if let Err(err) = keyring::store_secret(&name, &bytes) {
                    eprintln!("fleetpodsd: could not store {name}: {err}");
                }
            }
            SecretJob::Reload => {
                let fleet = match keyring::load() {
                    Ok(Load::Open(fleet)) => Some(fleet),
                    Ok(Load::Missing) => None,
                    Ok(Load::Locked) | Err(_) => continue,
                };
                let keys = load_accessory_keys();
                let _ = reports.send(Input::KeyringUnlocked { fleet, keys });
            }
        }
    }
}

fn worker(jobs: Receiver<Job>, reports: Sender<Input>, configured: Option<Address>, wake: Wake) {
    let bus = match Bus::system() {
        Ok(bus) => bus,
        Err(err) => {
            eprintln!("fleetpodsd: bluetooth is unavailable: {err}");
            while !matches!(jobs.recv(), Ok(Job::Stop) | Err(_)) {}
            return;
        }
    };
    if let Err(err) = bus.listen() {
        eprintln!("fleetpodsd: bluetooth signals are unavailable: {err}");
    }
    let mut live = Live::default();
    // Unique bus names for the services whose signals we act on. A unicast
    // signal addressed to us bypasses the sender filters in our broadcast
    // match rules, so senders are checked against these owners instead.
    let mut bluez_owner = bus.name_owner("org.bluez");
    let mut login1_owner = bus.name_owner("org.freedesktop.login1");
    if let Ok(inventory) = bus.inventory() {
        remember_inventory(&reports, configured, &mut live, inventory);
        sync(&reports, configured, &mut live);
    }
    loop {
        let timeout = poll_timeout(&live);
        let polled_socket = live.link.identity();
        let (dbus_ready, socket_ready) = wait(&wake, &bus, &live.link, timeout);
        expire_calls(&bus, &reports, &mut live);
        let mut stop = false;
        while let Ok(job) = jobs.try_recv() {
            if !handle_job(&bus, &reports, configured, &mut live, job) {
                stop = true;
                break;
            }
        }
        if stop {
            break;
        }
        if dbus_ready {
            if bus.pull().is_err() {
                eprintln!("fleetpodsd: bluetooth bus closed");
                release_air(&bus, &mut live);
                break;
            }
            while let Some(message) = bus.pop() {
                let interface =
                    message.interface().map(|value| value.to_string()).unwrap_or_default();
                let member = message.member().map(|value| value.to_string()).unwrap_or_default();
                // A reply is trusted only if it answers a call that is
                // actually outstanding and it comes from the BlueZ owner:
                // merely bearing a reply-serial header proves nothing, as
                // any sender can stamp one onto a forged message.
                // Everything else must come from the service that owns the
                // interface: signals (a forged unicast PrepareForSleep or
                // PropertiesChanged must not suspend us or rewrite device
                // state) and method calls to our advertisement object.
                let trusted = match message.get_reply_serial() {
                    Some(serial) => {
                        bluez_owner.is_some()
                            && message.sender().map(|sender| sender.to_string()).as_deref()
                                == bluez_owner.as_deref()
                            && live.calls.iter().any(|call| call.serial == serial)
                    }
                    None => match message.msg_type() {
                        dbus::message::MessageType::Signal => bluez::sender_allowed(
                            message.sender().map(|sender| sender.to_string()).as_deref(),
                            &interface,
                            bluez_owner.as_deref(),
                            login1_owner.as_deref(),
                        ),
                        dbus::message::MessageType::MethodCall => {
                            bluez_owner.is_some()
                                && message.sender().map(|sender| sender.to_string()).as_deref()
                                    == bluez_owner.as_deref()
                        }
                        _ => false,
                    },
                };
                if !trusted {
                    eprintln!(
                        "fleetpodsd: ignored bus message from untrusted sender (interface={interface} member={member})"
                    );
                    continue;
                }
                // A trusted NameOwnerChanged may mean BlueZ or logind
                // restarted under a new unique name; refresh both owners.
                if interface == "org.freedesktop.DBus" && member == "NameOwnerChanged" {
                    bluez_owner = bus.name_owner("org.bluez");
                    login1_owner = bus.name_owner("org.freedesktop.login1");
                }
                if serve_advertisement(&bus, &mut live, &message) {
                    continue;
                }
                if let Some(notice) = bluez::interpret(message) {
                    on_notice(&bus, &reports, configured, &mut live, notice);
                }
            }
        }
        // Jobs and BlueZ notices may replace the socket after poll returns.
        if socket_ready && polled_socket == live.link.identity() {
            on_socket(&reports, &mut live);
        } else if socket_ready {
            eprintln!("fleetpodsd: discarded readiness for a replaced control socket");
        }
        service_controller_reset(&bus, &reports, &mut live);
        if live.air.want_scan {
            verify_scan(&bus, &mut live);
            ensure_scan(&bus, &mut live);
        }
    }
    live.link.close();
}

struct Live {
    link: ControlLink,
    devices: Vec<Device>,
    adapters: HashMap<String, Address>,
    paths: HashMap<Address, String>,
    adapter: Option<Address>,
    accessory: Option<Address>,
    bonded: Option<(Address, bool)>,
    name: String,
    connected: Option<bool>,
    held: Option<Address>,
    generation: Generation,
    socket_for: Option<Generation>,
    calls: Vec<PendingCall>,
    adapter_path: Option<String>,
    /// A service interruption is in progress. Do not open the control socket until the daemon asks.
    defer_attach: bool,
    /// bluetoothd lost its inventory. Its bus name can return before the
    /// selected device, so absence is not unpairing until that device has
    /// been reported again (or explicitly removed).
    restoring_bluez: bool,
    /// `BluezBack` waits until the post-restart power cycle finishes, so the
    /// daemon does not reconnect into a controller that is about to go down.
    bluez_back_pending: bool,
    // Link state is split on purpose across the thread seam: this
    // watcher owns the D-Bus connection and the socket fds, while the
    // daemon owns link stage and policy. The generation originates in
    // the daemon's Connection; the watcher only echoes it as a tag on
    // socket events so stale readiness is discarded, and `defer_attach`
    // above is the watcher's rate-limiter so `AttachAvailable` is
    // emitted once per ACL rather than on every notice.
    air: Air,
}

impl Default for Live {
    fn default() -> Self {
        Self {
            link: ControlLink::new(),
            devices: Vec::new(),
            adapters: HashMap::new(),
            paths: HashMap::new(),
            adapter: None,
            accessory: None,
            bonded: None,
            name: String::new(),
            connected: None,
            held: None,
            generation: Generation(0),
            socket_for: None,
            calls: Vec::new(),
            adapter_path: None,
            defer_attach: false,
            restoring_bluez: false,
            bluez_back_pending: false,
            air: Air::default(),
        }
    }
}

/// How often a scan believed to be running is confirmed with BlueZ.
const SCAN_CHECK: Duration = Duration::from_secs(30);
/// Pause between attempts to power the controller off or back on.
const RESET_RETRY: Duration = Duration::from_secs(2);
/// Give up turning the adapter off after this many failed writes. Turning it
/// back on is not capped: a failed "on" would otherwise leave the radio down.
const RESET_OFF_LIMIT: u8 = 3;

/// One power cycle after bluetoothd returns. `Discovering` can stay true
/// across that restart while the controller delivers no AirPods advertisements.
/// Cycling `Powered` is what makes those advertisements arrive again.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ControllerReset {
    #[default]
    Idle,
    /// Ask BlueZ to set `Powered` false, once `at` has passed.
    Off { at: Instant },
    /// Ask BlueZ to set `Powered` true. Do not turn it off again.
    On { at: Instant },
}

#[derive(Default)]
struct Air {
    current: Vec<u8>,
    /// True only after RegisterAdvertisement has succeeded.
    registered: bool,
    wanted: bool,
    /// The daemon asked discovery to stay up.
    want_scan: bool,
    scanned: bool,
    /// When BlueZ last confirmed, or was last asked, that discovery is running.
    scan_checked_at: Option<Instant>,
    scan_retry_at: Option<Instant>,
    /// Yield our scan while the user pairs through the Bluetooth panel.
    pairing_until: Option<Instant>,
    pairing_prepare_at: Option<Instant>,
    scan_transport: Option<&'static str>,
    reset: ControllerReset,
    reset_off_failures: u8,
}

impl Air {
    fn pairing_paused(&self, now: Instant) -> bool {
        self.pairing_until.is_some_and(|until| now < until)
    }

    fn observe_pairing(&mut self, paired: bool, payload: Option<&[u8]>, now: Instant) {
        // A paired host needs ongoing proximity reports to suppress its own
        // reconnects while another computer is pairing. Only the host doing
        // first-run setup yields discovery to its Bluetooth panel.
        if !paired && payload.and_then(crate::pods::air::pairing_model).is_some() {
            if !self.pairing_paused(now) {
                self.pairing_prepare_at = None;
            }
            self.pairing_until = Some(now + Duration::from_secs(120));
        }
    }

    fn pairing_completed(&mut self, now: Instant) {
        if self.pairing_paused(now) {
            // Let BlueZ finish initial service discovery and profile setup.
            self.pairing_until = Some(now + Duration::from_secs(5));
        }
    }

    fn scan_result(&mut self, accepted: bool, now: Instant) {
        self.scanned = accepted;
        self.scan_checked_at = accepted.then_some(now);
        self.scan_retry_at = (!accepted).then_some(now + Duration::from_secs(1));
    }

    fn scan_check_due(&self, now: Instant) -> bool {
        self.want_scan
            && self.scanned
            && self.scan_checked_at.is_none_or(|at| now.duration_since(at) >= SCAN_CHECK)
    }

    fn scan_due(&self, now: Instant) -> bool {
        !self.scanned && !self.pairing_paused(now) && self.scan_retry_at.is_none_or(|at| now >= at)
    }

    /// bluetoothd has a new owner. A cycle already in flight is the one for
    /// this return; a second signal must not start another.
    fn arm_controller_reset(&mut self, now: Instant) {
        if self.reset_blocks_scan() {
            return;
        }
        self.reset = ControllerReset::Off { at: now };
        self.reset_off_failures = 0;
    }

    fn cancel_controller_reset(&mut self) {
        self.reset = ControllerReset::Idle;
        self.reset_off_failures = 0;
    }

    fn reset_blocks_scan(&self) -> bool {
        !matches!(self.reset, ControllerReset::Idle)
    }

    fn reset_deadline(&self) -> Option<Instant> {
        match self.reset {
            ControllerReset::Off { at } | ControllerReset::On { at } => Some(at),
            ControllerReset::Idle => None,
        }
    }

    fn delay_reset(&mut self, at: Instant) {
        self.reset = match self.reset {
            ControllerReset::Off { .. } => ControllerReset::Off { at },
            ControllerReset::On { .. } => ControllerReset::On { at },
            ControllerReset::Idle => ControllerReset::Idle,
        };
    }

    /// The next `Powered` value to write, when its wait has elapsed.
    fn power_request(&self, now: Instant) -> Option<bool> {
        match self.reset {
            ControllerReset::Off { at } if now >= at => Some(false),
            ControllerReset::On { at } if now >= at => Some(true),
            _ => None,
        }
    }

    /// `up` is the value just requested. The adapter's own Powered signal does
    /// not come through here: that signal is how the cycle looks from outside,
    /// and treating it as a new return would cycle forever.
    fn note_power_reply(&mut self, up: bool, ok: bool, now: Instant) {
        if ok && !up {
            self.reset_off_failures = 0;
            self.reset = ControllerReset::On { at: now };
            return;
        }
        if ok && up {
            self.reset = ControllerReset::Idle;
            self.reset_off_failures = 0;
            // The power-down signal scheduled a scan retry. Discovery should
            // start now, on the controller that just came back.
            if self.want_scan {
                self.scanned = false;
                self.scan_retry_at = None;
            }
            return;
        }
        if up {
            self.reset = ControllerReset::On { at: now + RESET_RETRY };
            return;
        }
        self.reset_off_failures = self.reset_off_failures.saturating_add(1);
        if self.reset_off_failures >= RESET_OFF_LIMIT {
            self.reset = ControllerReset::Idle;
            self.reset_off_failures = 0;
            return;
        }
        self.reset = ControllerReset::Off { at: now + RESET_RETRY };
    }
}

fn poll_timeout(live: &Live) -> i32 {
    let now = Instant::now();
    let mut timeout = (!live.calls.is_empty()).then_some(1_000);
    if live.air.want_scan && !live.air.reset_blocks_scan() {
        // A running scan is rechecked; see verify_scan. A scan that needs
        // a new session is retried on the one-second poll.
        let scan = if live.air.scanned { SCAN_CHECK.as_millis() as i32 } else { 1_000 };
        timeout = Some(timeout.map_or(scan, |current| current.min(scan)));
    }
    let power_in_flight = live.calls.iter().any(|call| matches!(call.kind, CallKind::Power(_)));
    if !power_in_flight {
        if let Some(at) = live.air.reset_deadline() {
            let wait = at.saturating_duration_since(now).as_millis().min(60_000) as i32;
            timeout = Some(timeout.map_or(wait, |current| current.min(wait)));
        }
    }
    timeout.unwrap_or(-1)
}

fn wait(wake: &Wake, bus: &Bus, link: &ControlLink, timeout_ms: i32) -> (bool, bool) {
    let mut fds = vec![
        libc::pollfd { fd: wake.raw(), events: libc::POLLIN, revents: 0 },
        libc::pollfd { fd: bus.fd(), events: libc::POLLIN, revents: 0 },
    ];
    let socket_at = link.fd().map(|fd| {
        let events = if link.connecting() { libc::POLLOUT } else { libc::POLLIN };
        fds.push(libc::pollfd { fd, events, revents: 0 });
        fds.len() - 1
    });
    unsafe {
        libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout_ms);
    }
    wake.drain();
    let dbus_ready = fds[1].revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP) != 0;
    let socket_ready = socket_at.is_some_and(|index| fds[index].revents != 0);
    (dbus_ready, socket_ready)
}

fn handle_job(
    bus: &Bus,
    reports: &Sender<Input>,
    configured: Option<Address>,
    live: &mut Live,
    job: Job,
) -> bool {
    match job {
        Job::Stop => {
            release_air(bus, live);
            false
        }
        Job::Open { generation } => {
            if live.generation != generation {
                // A setup retry needs a new AACP session. Repeating the hello
                // on the old socket receives an already-connected refusal.
                close_control(live, reports);
            }
            live.defer_attach = false;
            live.generation = generation;
            live.socket_for = None;
            begin_open(bus, reports, configured, live);
            true
        }
        Job::Follow => {
            live.defer_attach = false;
            sync(reports, configured, live);
            true
        }
        Job::ConnectTransport { address } => {
            if live.calls.iter().any(|call| {
                matches!(
                    call.kind,
                    CallKind::Profile
                        | CallKind::RestartDisconnect { .. }
                        | CallKind::RestartConnect { .. }
                )
            }) {
                return true;
            }
            match live.paths.get(&address).cloned() {
                Some(path) => {
                    if let Err(err) = enqueue(
                        bus,
                        live,
                        &path,
                        "ConnectProfile",
                        Some(bluez::A2DP_SINK),
                        CallKind::Profile,
                    ) {
                        eprintln!("fleetpodsd: A2DP connect on {address} failed: {err}");
                    }
                }
                None => eprintln!("fleetpodsd: no BlueZ device for {address}"),
            }
            true
        }
        Job::RestartOutput { id, address } => {
            let result = match live.paths.get(&address).cloned() {
                Some(path) => enqueue(
                    bus,
                    live,
                    &path,
                    "DisconnectProfile",
                    Some(bluez::A2DP_SINK),
                    CallKind::RestartDisconnect { id, address },
                ),
                None => Err(format!("no BlueZ device for {address}")),
            };
            if let Err(err) = result {
                eprintln!("fleetpodsd: A2DP restart failed: {err}");
                let _ = reports.send(Input::OutputRestarted { id, ok: false });
            }
            true
        }
        Job::Disconnect { address } => {
            eprintln!(
                "fleetpodsd: local disconnect requested address={address} generation={}",
                live.generation.0
            );
            let result = live
                .paths
                .get(&address)
                .cloned()
                .ok_or_else(|| "device path unavailable".to_owned())
                .and_then(|path| {
                    enqueue(
                        bus,
                        live,
                        &path,
                        "Disconnect",
                        None,
                        CallKind::Disconnect { address, generation: live.generation },
                    )
                });
            if let Err(err) = result {
                eprintln!("fleetpodsd: BlueZ disconnect of {address} failed: {err}");
                let _ = reports.send(Input::LinkReleased {
                    accessory: address,
                    generation: live.generation,
                    ok: false,
                });
            }
            if live.held == Some(address) {
                drop_socket(live, reports);
            }
            true
        }
        Job::Remove { address } => {
            close_control(live, reports);
            match (live.paths.get(&address).cloned(), live.adapter_path.clone()) {
                (Some(device), Some(adapter)) => {
                    if let Err(err) = enqueue_remove(bus, live, &adapter, &device) {
                        eprintln!("fleetpodsd: forget {address} failed: {err}");
                    }
                }
                _ => eprintln!("fleetpodsd: no BlueZ device to forget {address}"),
            }
            true
        }
        Job::Advertise(bytes) => {
            live.air.wanted = true;
            live.air.current = bytes;
            let _ = emit_service_data(bus, &live.air.current);
            ensure_scan(bus, live);
            if !live.air.registered
                && !live
                    .calls
                    .iter()
                    .any(|call| matches!(call.kind, CallKind::Advertise | CallKind::Unadvertise))
            {
                register_advertisement(bus, live);
            }
            true
        }
        Job::Listen => {
            live.air.want_scan = true;
            ensure_scan(bus, live);
            true
        }
        Job::StopListen => {
            live.air.want_scan = false;
            if live.air.scanned {
                stop_discovery(bus, live);
            }
            true
        }
        Job::ReleaseAir => {
            release_advertisement(bus, live);
            true
        }
        Job::Write { address, bytes } => {
            if live.held != Some(address) || !live.link.is_open() {
                eprintln!("fleetpodsd: control socket for {address} is not open");
                return true;
            }
            if let Err(err) = live.link.send(&bytes) {
                eprintln!("fleetpodsd: control write failed: {err}");
                drop_socket(live, reports);
            } else {
                let _ = read_socket(live, reports);
            }
            true
        }
    }
}

fn begin_open(bus: &Bus, reports: &Sender<Input>, configured: Option<Address>, live: &mut Live) {
    let Some(device) = bluez::select(&live.devices, configured).cloned() else {
        let _ = reports.send(Input::LinkFailed { generation: live.generation, busy: false });
        return;
    };
    if let Some(adapter) = device.adapter {
        note_device(live, reports, adapter, &device);
    } else {
        live.paths.insert(device.address, device.path.clone());
    }
    // A background seat needs AACP only; opening A2DP can fail while the other host plays.
    // Start the nonblocking socket first. A synchronous Connected property
    // query can wait on BlueZ, but is not a prerequisite for L2CAP attach.
    if live.link.is_open() {
        announce_socket(live, reports);
    } else if !live.link.connecting() {
        if let Err(err) = attach_control(live, device.address, reports) {
            eprintln!("fleetpodsd: {err}");
            let _ = reports.send(Input::LinkFailed { generation: live.generation, busy: false });
        }
    }
    let _ = radio_is_up(bus, live, device.address, &device.path, reports);
}

/// `Device1.Connected` at this moment. The cached flag lags a connection BlueZ already finished.
fn radio_is_up(
    bus: &Bus,
    live: &mut Live,
    address: Address,
    path: &str,
    reports: &Sender<Input>,
) -> bool {
    let up = match bus.connected(path) {
        Ok(up) => up,
        Err(err) => {
            eprintln!("fleetpodsd: could not read Connected for {address}: {err}");
            return false;
        }
    };
    if let Some(device) = live.devices.iter_mut().find(|device| device.address == address) {
        device.connected = up;
    }
    if up {
        note_connected(live, reports, true);
    }
    up
}

fn enqueue_remove(bus: &Bus, live: &mut Live, adapter: &str, device: &str) -> Result<(), String> {
    let serial = bus.remove_device(adapter, device)?;
    live.calls.push(PendingCall { serial, kind: CallKind::Remove, sent: Instant::now() });
    Ok(())
}

fn enqueue(
    bus: &Bus,
    live: &mut Live,
    path: &str,
    method: &str,
    arg: Option<&str>,
    kind: CallKind,
) -> Result<(), String> {
    let serial = bus.call(path, method, arg)?;
    live.calls.push(PendingCall { serial, kind, sent: Instant::now() });
    Ok(())
}

fn expire_calls(bus: &Bus, reports: &Sender<Input>, live: &mut Live) {
    let pending = std::mem::take(&mut live.calls);
    let mut keep = Vec::new();
    let mut advertisement_timed_out = false;
    for call in pending {
        if call.sent.elapsed() < Duration::from_secs(25) {
            keep.push(call);
            continue;
        }
        if matches!(call.kind, CallKind::Advertise | CallKind::Unadvertise) {
            advertisement_timed_out = true;
        }
        if matches!(call.kind, CallKind::StopDiscovery) {
            eprintln!("fleetpodsd: Bluetooth discovery stop timed out; retrying");
        }
        if matches!(call.kind, CallKind::StartDiscovery) {
            eprintln!("fleetpodsd: Bluetooth discovery start timed out; retrying");
            live.air.scan_result(false, Instant::now());
        }
        if let CallKind::Power(up) = call.kind {
            eprintln!("fleetpodsd: adapter power set timed out");
            settle_controller_reset(reports, live, up, false);
        }
        if matches!(call.kind, CallKind::Profile) {
            eprintln!("fleetpodsd: AirPods audio profile request went unanswered");
            let _ = reports.send(Input::TransportReply(TransportReply::TimedOut));
        }
        if let CallKind::RestartDisconnect { id, .. } | CallKind::RestartConnect { id } = call.kind
        {
            let _ = reports.send(Input::OutputRestarted { id, ok: false });
        }
    }
    live.calls = keep;
    if advertisement_timed_out {
        live.air.registered = false;
        unregister_advertisement(bus, live);
    }
}

fn on_notice(
    bus: &Bus,
    reports: &Sender<Input>,
    configured: Option<Address>,
    live: &mut Live,
    notice: Notice,
) {
    match notice {
        Notice::Reply { serial, result } => on_reply(bus, reports, live, serial, result),
        Notice::Removed(path) => {
            device_removed(reports, configured, live, &path);
        }
        Notice::Upsert { mut device, fleet, apple } => {
            observe_pairing(bus, live, apple.as_deref());
            report_advertisement(reports, Some(device.address), fleet, apple);
            report_nearby_metadata(reports, &device, device.rssi);
            if device.adapter.is_none() {
                device.adapter = live.adapters.get(&device.adapter_path).copied();
            }
            if let Some(slot) = live.devices.iter_mut().find(|known| known.path == device.path) {
                *slot = device;
            } else {
                live.devices.push(device);
            }
            sync(reports, configured, live);
        }
        Notice::Adapter { path, address } => {
            live.adapters.insert(path.clone(), address);
            if live.adapter.is_none() {
                live.adapter = Some(address);
                live.adapter_path = Some(path.clone());
                let _ = reports.send(Input::Controller(address));
            }
            for device in &mut live.devices {
                if device.adapter_path == path {
                    device.adapter = Some(address);
                }
            }
            sync(reports, configured, live);
        }
        Notice::Name { present: false } => note_bluez_left(reports, live),
        Notice::Name { present: true } => {
            // A direct owner replacement does not deliver a separate "gone"
            // signal. One cycle covers that return; a cycle already in
            // flight is that same return.
            if !live.air.reset_blocks_scan() {
                begin_controller_cycle(live);
            }
            bluez_returned(reports, configured, live, bus.inventory().ok());
        }
        Notice::Sleep(sleeping) => report_sleep(reports, live, sleeping),
        Notice::AdapterPower { path, up } => report_adapter_power(reports, live, &path, up),
        Notice::DiscoveryStopped(path) => {
            if live.adapter_path.as_deref() == Some(path.as_str()) {
                if live.air.want_scan {
                    eprintln!("fleetpodsd: Bluetooth discovery stopped; restarting");
                }
                live.air.scan_result(false, Instant::now());
            }
        }
        Notice::Transport { device, active } => {
            if device.is_some() && (device == live.accessory || device == configured) {
                let _ = reports.send(Input::Transport { active });
            }
        }
        Notice::Patch(patch) => {
            observe_pairing(bus, live, patch.apple.as_deref());
            report_advertisement(
                reports,
                bluez::address_of_path(&patch.path),
                patch.fleet,
                patch.apple,
            );
            let Some(device) = live.devices.iter_mut().find(|device| device.path == patch.path)
            else {
                return;
            };
            if let Some(connected) = patch.connected {
                device.connected = connected;
            }
            if let Some(paired) = patch.paired {
                device.paired = paired;
            }
            if let Some(bonded) = patch.bonded {
                device.bonded = bonded;
            }
            if let Some(rssi) = patch.rssi {
                device.rssi = Some(rssi);
            }
            if let Some(name) = patch.name {
                device.name = name;
            }
            if let Some(uuids) = patch.uuids {
                device.control =
                    uuids.iter().any(|uuid| uuid.eq_ignore_ascii_case(bluez::CONTROL_SERVICE));
            }
            report_nearby_metadata(reports, device, patch.rssi);
            sync(reports, configured, live);
        }
    }
}

fn report_nearby_metadata(reports: &Sender<Input>, device: &Device, rssi: Option<i16>) {
    let _ = reports.send(Input::NearbyMetadata {
        address: device.address,
        name: device.name.clone(),
        rssi,
    });
}

fn report_advertisement(
    reports: &Sender<Input>,
    address: Option<Address>,
    fleet: Option<Vec<u8>>,
    apple: Option<Vec<u8>>,
) {
    if let Some(bytes) = fleet.filter(|bytes| !bytes.is_empty()) {
        let _ = reports.send(Input::Advertisement(bytes));
    }
    if let (Some(address), Some(payload)) = (address, apple.filter(|bytes| !bytes.is_empty())) {
        let _ = reports.send(Input::Proximity { address, payload });
    }
}

fn on_reply(bus: &Bus, reports: &Sender<Input>, live: &mut Live, serial: u32, result: CallResult) {
    let Some(index) = live.calls.iter().position(|call| call.serial == serial) else {
        return;
    };
    let call = live.calls.remove(index);
    let failed = match &result {
        CallResult::Done => None,
        CallResult::Failed { name, .. }
            if name.ends_with("InProgress")
                || name.ends_with("AlreadyConnected")
                || (matches!(call.kind, CallKind::Disconnect { .. })
                    && name.ends_with("NotConnected")) =>
        {
            None
        }
        CallResult::Failed { name, message } => Some(format!("{name}: {message}")),
    };
    match call.kind {
        CallKind::StartDiscovery => {
            // A submitted request is not proof that BlueZ actually started scanning.
            let accepted = matches!(&result, CallResult::Done)
                || matches!(&result, CallResult::Failed { name, .. } if name.ends_with("InProgress"));
            live.air.scan_result(accepted, Instant::now());
            if accepted {
                if matches!(&result, CallResult::Done) {
                    eprintln!("fleetpodsd: Bluetooth discovery started");
                } else {
                    eprintln!("fleetpodsd: Bluetooth discovery already running");
                }
                if (!live.air.want_scan && !live.air.wanted)
                    || live.air.pairing_paused(Instant::now())
                {
                    stop_discovery(bus, live);
                }
            } else if let CallResult::Failed { name, message } = &result {
                eprintln!("fleetpodsd: Bluetooth discovery failed: {name}: {message}; retrying");
            }
        }
        CallKind::StopDiscovery => {
            let stopped = matches!(&result, CallResult::Done)
                || matches!(&result, CallResult::Failed { name, message }
                    if name.ends_with("NotReady")
                        || (name.ends_with("Failed") && message == "No discovery started"));
            if stopped {
                live.air.scan_result(false, Instant::now());
                live.air.scan_transport = None;
                set_scan_filter(bus, live, None);
                eprintln!("fleetpodsd: Bluetooth discovery released");
            } else if let Some(error) = failed {
                eprintln!("fleetpodsd: Bluetooth discovery stop failed: {error}");
            }
        }
        CallKind::RestartDisconnect { id, address } => {
            // Reconnect even if the disconnect failed; never leave a transport
            // deliberately disconnected after an interrupted reset.
            if let Some(err) = failed {
                eprintln!("fleetpodsd: A2DP reset disconnect: {err}");
            }
            let result = match live.paths.get(&address).cloned() {
                Some(path) => enqueue(
                    bus,
                    live,
                    &path,
                    "ConnectProfile",
                    Some(bluez::A2DP_SINK),
                    CallKind::RestartConnect { id },
                ),
                None => Err(format!("no BlueZ device for {address}")),
            };
            if let Err(err) = result {
                eprintln!("fleetpodsd: A2DP reset reconnect: {err}");
                let _ = reports.send(Input::OutputRestarted { id, ok: false });
            }
        }
        CallKind::RestartConnect { id } => {
            let ok = failed.is_none();
            if let Some(err) = failed {
                eprintln!("fleetpodsd: A2DP reset reconnect: {err}");
            }
            let _ = reports.send(Input::OutputRestarted { id, ok });
        }
        CallKind::Advertise => {
            let accepted = match &result {
                CallResult::Done => true,
                CallResult::Failed { name, .. } if name.ends_with("AlreadyExists") => true,
                CallResult::Failed { name, message } => {
                    eprintln!("fleetpodsd: fleet advertisement failed: {name}: {message}");
                    false
                }
            };
            live.air.registered = accepted;
            if accepted && !live.air.wanted {
                unregister_advertisement(bus, live);
            }
        }
        CallKind::Unadvertise => {
            // A new fragment waits until removal of the old registration finishes.
            live.air.registered = false;
            let removed = matches!(&result, CallResult::Done)
                || matches!(&result, CallResult::Failed { name, .. } if name.ends_with("DoesNotExist"));
            if removed
                && live.air.wanted
                && !live.calls.iter().any(|call| matches!(call.kind, CallKind::Advertise))
            {
                register_advertisement(bus, live);
            }
        }
        CallKind::Disconnect { address, generation } => {
            let ok = matches!(&result, CallResult::Done)
                || matches!(&result, CallResult::Failed { name, .. } if name.ends_with("NotConnected"));
            eprintln!(
                "fleetpodsd: Bluetooth release completed address={address} generation={} ok={ok}",
                generation.0
            );
            let _ = reports.send(Input::LinkReleased { accessory: address, generation, ok });
            if let Some(err) = failed {
                eprintln!("fleetpodsd: bluetooth disconnect failed: {err}");
            }
        }
        CallKind::Profile => {
            eprintln!(
                "fleetpodsd: AirPods audio profile reply after {} ms: {}",
                call.sent.elapsed().as_millis(),
                match &result {
                    CallResult::Done => "connected".to_string(),
                    CallResult::Failed { name, message } => format!("{name}: {message}"),
                }
            );
            if let Some(err) = &failed {
                eprintln!("fleetpodsd: bluetooth call failed: {err}");
            }
            let reply = match &result {
                CallResult::Failed { name, .. } if name.ends_with("InProgress") => {
                    TransportReply::Busy
                }
                _ if failed.is_some() => TransportReply::Failed,
                _ => TransportReply::Connected,
            };
            let _ = reports.send(Input::TransportReply(reply));
        }
        CallKind::Remove => {
            if let Some(err) = failed {
                eprintln!("fleetpodsd: bluetooth call failed: {err}");
            }
        }
        CallKind::Power(up) => {
            let ok = matches!(result, CallResult::Done);
            if let CallResult::Failed { name, message } = &result {
                eprintln!("fleetpodsd: adapter power set failed: {name}: {message}");
            }
            settle_controller_reset(reports, live, up, ok);
        }
    }
}

fn on_socket(reports: &Sender<Input>, live: &mut Live) {
    if live.link.connecting() {
        match live.link.finish_connect() {
            Ok(false) => {}
            Ok(true) => {
                announce_socket(live, reports);
                let _ = read_socket(live, reports);
            }
            Err(err) => {
                eprintln!("fleetpodsd: control socket failed: {err}");
                drop_socket(live, reports);
                let _ =
                    reports.send(Input::LinkFailed { generation: live.generation, busy: false });
            }
        }
        return;
    }
    let _ = read_socket(live, reports);
}

fn note_bluez_left(reports: &Sender<Input>, live: &mut Live) {
    // These registrations belong to the old bluetoothd instance.
    live.restoring_bluez = true;
    live.bluez_back_pending = false;
    live.air.cancel_controller_reset();
    live.air.scan_result(false, Instant::now());
    live.air.registered = false;
    live.air.scan_transport = None;
    drop_restarted_calls(live);
    live.defer_attach = true;
    live.devices.clear();
    let _ = reports.send(Input::BluezLost);
    note_connected(live, reports, false);
}

fn begin_controller_cycle(live: &mut Live) {
    drop_restarted_calls(live);
    live.air.registered = false;
    live.air.scan_transport = None;
    live.air.scan_result(false, Instant::now());
    live.air.arm_controller_reset(Instant::now());
    live.bluez_back_pending = true;
    eprintln!("fleetpodsd: bluetoothd returned; power-cycling the adapter");
}

/// A Powered change on our controller. While a cycle is in progress the
/// signal is that cycle, so the daemon stays in the bluetoothd-lost
/// interrupt until [`settle_controller_reset`] sends one `BluezBack`.
fn report_adapter_power(reports: &Sender<Input>, live: &mut Live, path: &str, up: bool) {
    if live.adapter_path.as_deref() != Some(path) {
        return;
    }
    if !up {
        live.air.scan_result(false, Instant::now());
        live.defer_attach = true;
        close_control(live, reports);
    }
    if live.air.reset_blocks_scan() {
        return;
    }
    let _ = reports.send(Input::AdapterPower(up));
}

fn bluez_returned(
    reports: &Sender<Input>,
    configured: Option<Address>,
    live: &mut Live,
    inventory: Option<bluez::Inventory>,
) {
    // Do not end restoration merely because GetManagedObjects succeeds.
    // BlueZ may still be loading the saved adapter and device objects.
    if let Some(inventory) = inventory {
        remember_inventory(reports, configured, live, inventory);
        sync(reports, configured, live);
    }
    if live.bluez_back_pending {
        return;
    }
    let _ = reports.send(Input::BluezBack);
}

fn drop_restarted_calls(live: &mut Live) {
    live.calls.retain(|call| {
        !matches!(
            call.kind,
            CallKind::StartDiscovery
                | CallKind::StopDiscovery
                | CallKind::Advertise
                | CallKind::Unadvertise
                | CallKind::Power(_)
        )
    });
}

fn settle_controller_reset(reports: &Sender<Input>, live: &mut Live, up: bool, ok: bool) {
    live.air.note_power_reply(up, ok, Instant::now());
    if ok && up {
        eprintln!("fleetpodsd: adapter power cycle finished");
    } else if !ok && !live.air.reset_blocks_scan() {
        eprintln!("fleetpodsd: adapter power cycle failed; leaving the controller as it is");
    } else if !ok {
        eprintln!("fleetpodsd: adapter power change failed; retrying");
    }
    if live.bluez_back_pending && !live.air.reset_blocks_scan() {
        live.bluez_back_pending = false;
        let _ = reports.send(Input::BluezBack);
    }
}

fn service_controller_reset(bus: &Bus, reports: &Sender<Input>, live: &mut Live) {
    let now = Instant::now();
    let Some(up) = live.air.power_request(now) else {
        return;
    };
    if live.calls.iter().any(|call| matches!(call.kind, CallKind::Power(_))) {
        return;
    }
    // bluetoothd can publish its bus name before the adapter object exists.
    // Waiting must not use up the failed-write budget.
    if live.adapter_path.is_none() {
        live.air.delay_reset(now + RESET_RETRY);
        return;
    }
    if !set_powered(bus, live, up) {
        settle_controller_reset(reports, live, up, false);
    }
}

fn set_powered(bus: &Bus, live: &mut Live, up: bool) -> bool {
    let Some(path) = live.adapter_path.clone() else {
        return false;
    };
    if live.calls.iter().any(|call| matches!(call.kind, CallKind::Power(_))) {
        return false;
    }
    let Ok(message) = dbus::Message::new_method_call(
        "org.bluez",
        &path,
        "org.freedesktop.DBus.Properties",
        "Set",
    ) else {
        return false;
    };
    let message = message.append3("org.bluez.Adapter1", "Powered", dbus::arg::Variant(up));
    match bus.submit(message) {
        Ok(serial) => {
            live.calls.push(PendingCall {
                serial,
                kind: CallKind::Power(up),
                sent: Instant::now(),
            });
            true
        }
        Err(err) => {
            eprintln!("fleetpodsd: adapter power request failed: {err}");
            false
        }
    }
}

fn device_removed(
    reports: &Sender<Input>,
    configured: Option<Address>,
    live: &mut Live,
    path: &str,
) {
    if live.accessory.and_then(|address| live.paths.get(&address)).map(String::as_str) == Some(path)
    {
        live.restoring_bluez = false;
    }
    live.devices.retain(|device| device.path != path);
    sync(reports, configured, live);
}

fn remember_inventory(
    reports: &Sender<Input>,
    configured: Option<Address>,
    live: &mut Live,
    inventory: bluez::Inventory,
) {
    live.adapters = inventory.adapters;
    live.devices = inventory.devices;
    // First-run discovery must not depend on an already paired accessory.
    // Prefer its adapter when paired; otherwise choose a stable adapter path.
    let selected = bluez::select(&live.devices, configured)
        .and_then(|device| device.adapter.map(|address| (device.adapter_path.clone(), address)))
        .or_else(|| {
            live.adapters
                .iter()
                .min_by_key(|(path, _)| *path)
                .map(|(path, address)| (path.clone(), *address))
        });
    if let Some((path, address)) = selected {
        live.adapter_path = Some(path);
        if live.adapter != Some(address) {
            live.adapter = Some(address);
            let _ = reports.send(Input::Controller(address));
        }
    }
    if let Some(device) = bluez::select(&live.devices, configured) {
        let active = inventory.transports.get(&device.address).copied().unwrap_or(false);
        let _ = reports.send(Input::Transport { active });
    }
}

fn sync(reports: &Sender<Input>, configured: Option<Address>, live: &mut Live) {
    if live.accessory.is_some_and(|address| live.devices.iter().any(|d| d.address == address)) {
        // The device is known again. Its Paired/Bonded values, including an
        // explicit loss of both, are authoritative even during restoration.
        live.restoring_bluez = false;
    }
    let Some(device) = bluez::select(&live.devices, configured).cloned() else {
        // Preserve pairing through the outage and the partial inventories
        // emitted during startup. Unrelated adapter/device notices cannot
        // decide whether this accessory is still paired.
        if live.restoring_bluez {
            note_connected(live, reports, false);
            return;
        }
        if let Some(address) = live.accessory.take() {
            close_control(live, reports);
            live.name.clear();
            live.bonded = None;
            live.paths.remove(&address);
            live.defer_attach = false;
            let _ = reports.send(Input::Unpaired { address });
        }
        note_connected(live, reports, false);
        return;
    };
    live.restoring_bluez = false;
    if let Some(adapter) = device.adapter {
        note_device(live, reports, adapter, &device);
    } else {
        live.paths.insert(device.address, device.path.clone());
    }
    if !device.connected {
        note_connected(live, reports, false);
        return;
    }
    note_connected(live, reports, true);
    if live.defer_attach {
        return;
    }
    if !live.link.is_open() && !live.link.connecting() {
        // Register the attempt before opening a socket: a reset during attach
        // must reach the same retry loop as an explicitly requested connection.
        live.defer_attach = true;
        let _ = reports.send(Input::AttachAvailable { generation: live.generation });
    } else if live.link.is_open() {
        announce_socket(live, reports);
    }
}

fn note_device(live: &mut Live, reports: &Sender<Input>, adapter: Address, device: &Device) {
    live.paths.insert(device.address, device.path.clone());
    if live.adapter != Some(adapter) {
        live.adapter = Some(adapter);
        live.adapter_path = Some(device.adapter_path.clone());
        let _ = reports.send(Input::Controller(adapter));
    }
    if live.accessory != Some(device.address) || live.name != device.name {
        live.accessory = Some(device.address);
        live.name = device.name.clone();
        let _ = reports.send(Input::Paired { address: device.address, name: device.name.clone() });
    }
    if live.bonded != Some((device.address, device.bonded)) {
        live.bonded = Some((device.address, device.bonded));
        if device.bonded {
            live.air.pairing_completed(Instant::now());
        }
        let _ = reports.send(Input::Bonded { address: device.address, saved: device.bonded });
        eprintln!(
            "fleetpodsd: Bluetooth pairing address={} paired={} bonded={}",
            device.address, device.paired, device.bonded
        );
    }
}

fn observe_pairing(bus: &Bus, live: &mut Live, payload: Option<&[u8]>) {
    let now = Instant::now();
    let bonded = live.bonded.is_some_and(|(_, saved)| saved);
    live.air.observe_pairing(bonded, payload, now);
    if bonded
        || !live.air.pairing_paused(now)
        || live.air.pairing_prepare_at.is_some_and(|at| now < at)
    {
        return;
    }
    let Some(adapter) = live.adapter_path.as_deref() else { return };
    match bus.enable_pairing(adapter) {
        Ok(()) => live.air.pairing_prepare_at = live.air.pairing_until,
        Err(err) => {
            eprintln!("fleetpodsd: could not enable Bluetooth pairing: {err}");
            live.air.pairing_prepare_at = Some(now + Duration::from_secs(5));
        }
    }
}

fn note_connected(live: &mut Live, reports: &Sender<Input>, up: bool) {
    if live.connected == Some(up) {
        return;
    }
    eprintln!(
        "fleetpodsd: Bluetooth connection changed up={up} generation={} control_open={}",
        live.generation.0,
        live.link.is_open()
    );
    live.connected = Some(up);
    let _ = reports.send(Input::Bluez(up));
    if !up && live.link.fd().is_some() {
        live.defer_attach = true;
        live.link.close();
        live.held = None;
        live.socket_for = None;
        let _ = reports.send(Input::ControlSocket { up: false, generation: live.generation });
    }
}

fn attach_control(
    live: &mut Live,
    address: Address,
    reports: &Sender<Input>,
) -> Result<(), String> {
    let same = live.held == Some(address) && (live.link.is_open() || live.link.connecting());
    if !same {
        live.link.close();
        live.held = None;
        live.socket_for = None;
        eprintln!("fleetpodsd: starting control socket generation={}", live.generation.0);
    }
    match live.link.attach(address) {
        Ok(Attach::Up) => {
            live.held = Some(address);
            announce_socket(live, reports);
            Ok(())
        }
        Ok(Attach::Connecting) => {
            live.held = Some(address);
            Ok(())
        }
        Err(err) => Err(format!("control socket to {address} failed: {err}")),
    }
}

fn announce_socket(live: &mut Live, reports: &Sender<Input>) {
    if live.socket_for != Some(live.generation) {
        eprintln!("fleetpodsd: control socket ready generation={}", live.generation.0);
        let _ = reports.send(Input::ControlSocket { up: true, generation: live.generation });
        live.socket_for = Some(live.generation);
    }
}

fn report_sleep(reports: &Sender<Input>, live: &mut Live, sleeping: bool) {
    // Retire the daemon generation before the locally closed socket reports
    // down. Otherwise the close looks like lost ownership and clears wake intent.
    let _ = reports.send(if sleeping { Input::Suspend } else { Input::Resume });
    if sleeping {
        live.defer_attach = true;
        close_control(live, reports);
    }
}

fn close_control(live: &mut Live, reports: &Sender<Input>) {
    let open = live.link.fd().is_some();
    if open {
        eprintln!("fleetpodsd: locally closing control generation={}", live.generation.0);
    }
    live.link.close();
    live.held = None;
    if open {
        live.socket_for = None;
        let _ = reports.send(Input::ControlSocket { up: false, generation: live.generation });
    }
}

fn drop_socket(live: &mut Live, reports: &Sender<Input>) {
    // A lost seat waits for an explicit Open/Follow job. BlueZ property updates
    // must not silently attach again and displace another host.
    live.defer_attach = true;
    let open = live.link.fd().is_some();
    live.link.close();
    live.held = None;
    if open {
        live.socket_for = None;
        let _ = reports.send(Input::ControlSocket { up: false, generation: live.generation });
    }
    note_connected(live, reports, false);
}

fn read_socket(live: &mut Live, reports: &Sender<Input>) -> Result<(), String> {
    let packets = live.link.read_packets().map_err(|err| {
        eprintln!(
            "fleetpodsd: control read ended generation={} address={:?} error={err}",
            live.generation.0, live.held
        );
        live.defer_attach = true;
        live.held = None;
        live.socket_for = None;
        let _ = reports.send(Input::ControlSocket { up: false, generation: live.generation });
        err.to_string()
    })?;
    for packet in packets {
        let _ = reports.send(Input::Accessory { generation: live.generation, packet });
    }
    Ok(())
}

const ADVERTISEMENT: &str = "/org/fleetpods/advertisement";
/// BlueZ 5.87 reads these as milliseconds. The previous daemon uses this
/// 1M interval, and that is the channel the other laptop scans.
const ADV_MIN_MS: u32 = 100;
const ADV_MAX_MS: u32 = 150;

fn release_advertisement(bus: &Bus, live: &mut Live) {
    live.air.wanted = false;
    // BlueZ reserves a slot before registration completes. Cancel that slot too.
    if live.air.registered || live.calls.iter().any(|call| matches!(call.kind, CallKind::Advertise))
    {
        unregister_advertisement(bus, live);
    }
    // Keep valid properties available until BlueZ finishes removing the object.
}

fn release_air(bus: &Bus, live: &mut Live) {
    release_advertisement(bus, live);
    live.air.want_scan = false;
    if live.air.scanned {
        stop_discovery(bus, live);
    }
    live.air.scanned = false;
}

fn unregister_advertisement(bus: &Bus, live: &mut Live) {
    if live.calls.iter().any(|call| matches!(call.kind, CallKind::Unadvertise)) {
        return;
    }
    let Some(path) = live.adapter_path.clone() else {
        return;
    };
    let Ok(object) = dbus::Path::new(ADVERTISEMENT) else {
        return;
    };
    let Ok(message) = dbus::Message::new_method_call(
        "org.bluez",
        &path,
        "org.bluez.LEAdvertisingManager1",
        "UnregisterAdvertisement",
    ) else {
        return;
    };
    if let Ok(serial) = bus.submit(message.append1(object)) {
        live.calls.push(PendingCall { serial, kind: CallKind::Unadvertise, sent: Instant::now() });
    }
}

fn stop_discovery(bus: &Bus, live: &mut Live) {
    if live.calls.iter().any(|call| matches!(call.kind, CallKind::StopDiscovery)) {
        return;
    }
    let Some(path) = live.adapter_path.as_deref() else {
        return;
    };
    let Ok(message) =
        dbus::Message::new_method_call("org.bluez", path, "org.bluez.Adapter1", "StopDiscovery")
    else {
        return;
    };
    // Stop only this D-Bus client's discovery session. Other scanners keep theirs.
    if let Ok(serial) = bus.submit(message) {
        live.calls.push(PendingCall {
            serial,
            kind: CallKind::StopDiscovery,
            sent: Instant::now(),
        });
    }
}

fn set_scan_filter(bus: &Bus, live: &Live, transport: Option<&str>) {
    let Some(path) = live.adapter_path.as_deref() else { return };
    let mut filter: HashMap<String, dbus::arg::Variant<Box<dyn dbus::arg::RefArg>>> =
        HashMap::new();
    if let Some(transport) = transport {
        filter.insert("Transport".into(), dbus::arg::Variant(Box::new(transport.to_string())));
        filter.insert("DuplicateData".into(), dbus::arg::Variant(Box::new(true)));
    }
    // An empty dictionary removes our filter rather than changing anyone else's.
    if let Ok(message) = dbus::Message::new_method_call(
        "org.bluez",
        path,
        "org.bluez.Adapter1",
        "SetDiscoveryFilter",
    ) {
        let _ = bus.submit(message.append1(filter));
    }
}

/// BlueZ can stop scanning while still holding this client's discovery
/// session, without this client seeing a `Discovering` change. Another
/// StartDiscovery then only answers InProgress, so trusting `scanned` left
/// the daemon deaf to case and ear reports. Release the session and start
/// a new one instead.
fn verify_scan(bus: &Bus, live: &mut Live) {
    if live.air.reset_blocks_scan() {
        return;
    }
    let now = Instant::now();
    if !live.air.scan_check_due(now)
        || live.air.pairing_paused(now)
        || live
            .calls
            .iter()
            .any(|call| matches!(call.kind, CallKind::StartDiscovery | CallKind::StopDiscovery))
    {
        return;
    }
    let Some(path) = live.adapter_path.as_deref() else { return };
    match bus.discovering(path) {
        Ok(true) => live.air.scan_checked_at = Some(now),
        Ok(false) => {
            eprintln!("fleetpodsd: Bluetooth discovery stopped without notice; restarting");
            live.air.scan_checked_at = Some(now);
            // The stop reply clears `scanned`, and ensure_scan starts again.
            stop_discovery(bus, live);
        }
        // bluetoothd may be restarting; its name change resets the scan.
        Err(_) => live.air.scan_checked_at = Some(now),
    }
}

fn ensure_scan(bus: &Bus, live: &mut Live) {
    if live.air.reset_blocks_scan() {
        return;
    }
    let now = Instant::now();
    if live.air.pairing_paused(now) {
        if live.air.scanned {
            stop_discovery(bus, live);
        }
        return;
    }
    if live
        .calls
        .iter()
        .any(|call| matches!(call.kind, CallKind::StartDiscovery | CallKind::StopDiscovery))
    {
        return;
    }
    let Some(path) = live.adapter_path.clone() else { return };
    // During setup the panel may adopt existing discovery instead of starting
    // its own session. Include classic Bluetooth so the audio device is found.
    let transport = if live.accessory.is_some() { "le" } else { "auto" };
    if live.air.scan_transport != Some(transport) {
        set_scan_filter(bus, live, Some(transport));
        live.air.scan_transport = Some(transport);
    }
    if !live.air.scan_due(now) {
        return;
    }
    if let Ok(message) =
        dbus::Message::new_method_call("org.bluez", &path, "org.bluez.Adapter1", "StartDiscovery")
    {
        match bus.submit(message) {
            Ok(serial) => live.calls.push(PendingCall {
                serial,
                kind: CallKind::StartDiscovery,
                sent: Instant::now(),
            }),
            Err(err) => {
                eprintln!("fleetpodsd: Bluetooth discovery request failed: {err}; retrying");
                live.air.scan_result(false, Instant::now());
            }
        }
    }
}

fn register_advertisement(bus: &Bus, live: &mut Live) {
    let Some(path) = live.adapter_path.clone() else {
        return;
    };
    let Ok(object) = dbus::Path::new(ADVERTISEMENT) else {
        return;
    };
    let Ok(message) = dbus::Message::new_method_call(
        "org.bluez",
        &path,
        "org.bluez.LEAdvertisingManager1",
        "RegisterAdvertisement",
    ) else {
        return;
    };
    let options: HashMap<String, dbus::arg::Variant<Box<dyn dbus::arg::RefArg>>> = HashMap::new();
    let Ok(serial) = bus.submit(message.append2(object, options)) else {
        return;
    };
    live.calls.push(PendingCall { serial, kind: CallKind::Advertise, sent: Instant::now() });
}

fn emit_service_data(bus: &Bus, payload: &[u8]) -> Result<(), String> {
    let mut services = HashMap::new();
    services.insert(
        crate::fleet::wire::FLEET_SERVICE_UUID.to_string(),
        dbus::arg::Variant(payload.to_vec()),
    );
    let mut changed: HashMap<String, dbus::arg::Variant<Box<dyn dbus::arg::RefArg>>> =
        HashMap::new();
    changed.insert("ServiceData".into(), dbus::arg::Variant(Box::new(services)));
    let message = dbus::Message::new_signal(
        ADVERTISEMENT,
        "org.freedesktop.DBus.Properties",
        "PropertiesChanged",
    )?;
    bus.submit(message.append3("org.bluez.LEAdvertisement1", changed, Vec::<String>::new()))?;
    Ok(())
}

fn serve_advertisement(bus: &Bus, live: &mut Live, message: &dbus::Message) -> bool {
    if message.path().map(|path| path.to_string()).as_deref() != Some(ADVERTISEMENT) {
        return false;
    }
    if message.msg_type() != dbus::message::MessageType::MethodCall {
        return false;
    }
    let member = message.member().map(|member| member.to_string()).unwrap_or_default();
    if member == "Release" {
        live.air.registered = false;
    }
    if let Some(reply) = advertisement_reply(&live.air.current, message) {
        let _ = bus.submit(reply);
    }
    true
}

fn advertisement_reply(payload: &[u8], message: &dbus::Message) -> Option<dbus::Message> {
    let interface = message.interface().map(|name| name.to_string()).unwrap_or_default();
    let member = message.member().map(|name| name.to_string()).unwrap_or_default();
    let error = |name: &str, text: &str| {
        Some(message.error(
            &dbus::strings::ErrorName::new(name).unwrap(),
            &std::ffi::CString::new(text).unwrap(),
        ))
    };
    match (interface.as_str(), member.as_str()) {
        ("org.freedesktop.DBus.ObjectManager", "GetManagedObjects") => {
            let interfaces = HashMap::from([(
                "org.bluez.LEAdvertisement1".to_string(),
                advertisement_properties(payload),
            )]);
            let objects = HashMap::from([(dbus::Path::new(ADVERTISEMENT).unwrap(), interfaces)]);
            Some(message.method_return().append1(objects))
        }
        ("org.freedesktop.DBus.Properties", "GetAll") => {
            let Ok(name) = message.read1::<String>() else {
                return error("org.freedesktop.DBus.Error.InvalidArgs", "Expected interface name");
            };
            let properties = if name == "org.bluez.LEAdvertisement1" {
                advertisement_properties(payload)
            } else {
                HashMap::new()
            };
            Some(message.method_return().append1(properties))
        }
        ("org.freedesktop.DBus.Properties", "Get") => {
            let Ok((name, property)) = message.read2::<String, String>() else {
                return error(
                    "org.freedesktop.DBus.Error.InvalidArgs",
                    "Expected interface and property name",
                );
            };
            if name == "org.bluez.LEAdvertisement1" {
                if let Some(value) = advertisement_property(&property, payload) {
                    return Some(message.method_return().append1(value));
                }
            }
            error("org.freedesktop.DBus.Error.UnknownProperty", "Unknown advertisement property")
        }
        ("org.freedesktop.DBus.Introspectable", "Introspect") => {
            Some(message.method_return().append1(ADVERTISEMENT_XML))
        }
        ("org.bluez.LEAdvertisement1", "Release") => Some(message.method_return()),
        _ => error("org.freedesktop.DBus.Error.UnknownMethod", "Unknown advertisement method"),
    }
}

fn advertisement_property(
    name: &str,
    payload: &[u8],
) -> Option<dbus::arg::Variant<Box<dyn dbus::arg::RefArg>>> {
    let uuid = crate::fleet::wire::FLEET_SERVICE_UUID.to_string();
    match name {
        "Type" => Some(dbus::arg::Variant(Box::new("broadcast".to_string()))),
        "SecondaryChannel" => Some(dbus::arg::Variant(Box::new("1M".to_string()))),
        "MinInterval" => Some(dbus::arg::Variant(Box::new(ADV_MIN_MS))),
        "MaxInterval" => Some(dbus::arg::Variant(Box::new(ADV_MAX_MS))),
        "ServiceUUIDs" => Some(dbus::arg::Variant(Box::new(vec![uuid]))),
        "ServiceData" => {
            let mut services = HashMap::new();
            services.insert(uuid, dbus::arg::Variant(payload.to_vec()));
            Some(dbus::arg::Variant(Box::new(services)))
        }
        _ => None,
    }
}

fn advertisement_properties(
    payload: &[u8],
) -> HashMap<String, dbus::arg::Variant<Box<dyn dbus::arg::RefArg>>> {
    let uuid = crate::fleet::wire::FLEET_SERVICE_UUID.to_string();
    let mut services = HashMap::new();
    services.insert(uuid.clone(), dbus::arg::Variant(payload.to_vec()));
    let mut properties: HashMap<String, dbus::arg::Variant<Box<dyn dbus::arg::RefArg>>> =
        HashMap::new();
    properties.insert(
        "Type".into(),
        dbus::arg::Variant(Box::new("broadcast".to_string()) as Box<dyn dbus::arg::RefArg>),
    );
    properties.insert(
        "SecondaryChannel".into(),
        dbus::arg::Variant(Box::new("1M".to_string()) as Box<dyn dbus::arg::RefArg>),
    );
    properties.insert(
        "MinInterval".into(),
        dbus::arg::Variant(Box::new(ADV_MIN_MS) as Box<dyn dbus::arg::RefArg>),
    );
    properties.insert(
        "MaxInterval".into(),
        dbus::arg::Variant(Box::new(ADV_MAX_MS) as Box<dyn dbus::arg::RefArg>),
    );
    properties.insert(
        "ServiceUUIDs".into(),
        dbus::arg::Variant(Box::new(vec![uuid]) as Box<dyn dbus::arg::RefArg>),
    );
    properties.insert(
        "ServiceData".into(),
        dbus::arg::Variant(Box::new(services) as Box<dyn dbus::arg::RefArg>),
    );
    properties
}

const ADVERTISEMENT_XML: &str = concat!(
    "<node>",
    "<interface name=\"org.freedesktop.DBus.ObjectManager\">",
    "<method name=\"GetManagedObjects\"><arg type=\"a{oa{sa{sv}}}\" direction=\"out\"/></method>",
    "</interface>",
    "<interface name=\"org.bluez.LEAdvertisement1\">",
    "<method name=\"Release\"/>",
    "<property name=\"Type\" type=\"s\" access=\"read\"/>",
    "<property name=\"SecondaryChannel\" type=\"s\" access=\"read\"/>",
    "<property name=\"MinInterval\" type=\"u\" access=\"read\"/>",
    "<property name=\"MaxInterval\" type=\"u\" access=\"read\"/>",
    "<property name=\"ServiceUUIDs\" type=\"as\" access=\"read\"/>",
    "<property name=\"ServiceData\" type=\"a{sv}\" access=\"read\"/>",
    "</interface>",
    "<interface name=\"org.freedesktop.DBus.Properties\">",
    "<method name=\"Get\">",
    "<arg name=\"interface\" type=\"s\" direction=\"in\"/>",
    "<arg name=\"name\" type=\"s\" direction=\"in\"/>",
    "<arg name=\"value\" type=\"v\" direction=\"out\"/>",
    "</method>",
    "<method name=\"GetAll\">",
    "<arg name=\"interface\" type=\"s\" direction=\"in\"/>",
    "<arg name=\"properties\" type=\"a{sv}\" direction=\"out\"/>",
    "</method>",
    "</interface>",
    "</node>"
);

fn configured_accessory() -> Option<Address> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".config"))
        })?;
    let text = fs::read_to_string(base.join("fleetpods/accessory")).ok()?;
    Address::parse(text.lines().next()?.trim())
}

#[cfg(test)]
mod fleet_reset_tests {
    use super::*;
    use crate::fleet::members::{AccessoryKeys, Fleet, GroupKey, Member};
    use crate::types::{FleetId, MonoMs};

    fn fleet() -> Fleet {
        Fleet {
            id: FleetId::new([1; 16]),
            key: GroupKey::new([2; 32]),
            members: vec![Member {
                address: Address::parse("02:00:00:00:00:01").unwrap(),
                label: "test-peer".into(),
            }],
        }
    }

    // No live radio, audio, socket or Secret Service is opened by these tests.
    fn session(reports: Receiver<Input>, jobs: Sender<SecretJob>) -> Session {
        let mut daemon = Daemon::new("test-host");
        daemon.restore_fleet(fleet());
        let mic_path = std::sync::Arc::clone(&daemon.mic_path);
        Session {
            daemon,
            radio: None,
            reports,
            reports_wake: None,
            audio: None,
            players: None,
            secrets: Some(jobs),
            secret_thread: None,
            warned: false,
            watermark: None,
            mic_path,
            mic_source: None,
            mic_picture: None,
            keyring_retry_at: Some(MonoMs(1)),
        }
    }

    #[test]
    fn locked_and_failed_startup_reads_retry_until_membership_is_restored() {
        for loaded in [Ok(Load::Locked), Err("Secret Service is not ready".into())] {
            let (reports, rx) = mpsc::channel();
            let (jobs, worker) = mpsc::channel();
            let mut session = session(rx, jobs);
            session.daemon = Daemon::new("test-host");
            session.keyring_retry_at = None;
            restore_saved_fleet(&mut session.daemon, loaded);

            let status = session.daemon.status_value(Clock::new(1000, 1000));
            assert_eq!(status["fleet"]["waiting_for_keyring"], true);
            assert_eq!(status["fleet"]["enrolled"], false);
            assert!(session.daemon.keyring_locked());
            assert!(session.daemon.saved_fleet().is_none());

            session.retry_locked_keyring(Clock::new(1000, 1000));
            assert!(matches!(worker.try_recv(), Ok(SecretJob::Reload)));
            session.retry_locked_keyring(Clock::new(5999, 5999));
            assert!(worker.try_recv().is_err(), "no busy-loop while waiting");
            session.retry_locked_keyring(Clock::new(6000, 6000));
            assert!(matches!(worker.try_recv(), Ok(SecretJob::Reload)));
            session.retry_locked_keyring(Clock::new(11000, 11000));
            assert!(matches!(worker.try_recv(), Ok(SecretJob::Reload)));

            reports.send(Input::KeyringUnlocked { fleet: Some(fleet()), keys: None }).unwrap();
            session.poll(Clock::new(16000, 16000));
            assert_eq!(session.daemon.saved_fleet(), Some(fleet()));
            let status = session.daemon.status_value(Clock::new(16000, 16000));
            assert_eq!(status["fleet"]["waiting_for_keyring"], false);
            assert_eq!(status["fleet"]["enrolled"], true);
            assert!(!session.daemon.keyring_locked());
            assert!(session.keyring_retry_at.is_none());
            assert!(worker.try_recv().is_err(), "successful recovery stops retries");
        }
    }

    #[test]
    fn successful_startup_reads_do_not_wait_for_the_keyring() {
        for loaded in [Ok(Load::Missing), Ok(Load::Open(fleet()))] {
            let (_reports, rx) = mpsc::channel();
            let (jobs, worker) = mpsc::channel();
            let mut session = session(rx, jobs);
            session.daemon = Daemon::new("test-host");
            restore_saved_fleet(&mut session.daemon, loaded);
            session.retry_locked_keyring(Clock::new(6000, 6000));
            assert!(worker.try_recv().is_err());
            assert!(!session.daemon.keyring_locked());
            assert_eq!(
                session.daemon.status_value(Clock::new(6000, 6000))["fleet"]["waiting_for_keyring"],
                false
            );
        }
    }

    #[test]
    fn leaving_for_an_incoming_invitation_preserves_its_offer_and_errors_are_non_destructive() {
        for saved in [false, true] {
            let (_tx, rx) = mpsc::channel();
            let (jobs, worker) = mpsc::channel();
            let mut session = session(rx, jobs);
            let local = Address::parse("02:00:00:00:00:02").unwrap();
            let peer = Address::parse("02:00:00:00:00:01").unwrap();
            let id = crate::types::InvitationId::new([6; 16]);
            let now = Clock::new(1000, 1000);
            session.daemon.set_local_address(Some(local));
            assert!(session.daemon.receive_invite(peer, "gram", id, now));
            assert!(session.daemon.enrollment.offer(id, 0));
            session.daemon.offer = Some(vec![1, 2, 3]);
            let old = session.daemon.saved_fleet();
            let thread = std::thread::spawn(move || {
                let Ok(SecretJob::ResetFleet(reply)) = worker.recv() else {
                    panic!("expected reset")
                };
                reply.send(if saved { Ok(()) } else { Err("keyring locked".into()) }).unwrap();
            });
            let reply = session
                .command(
                    Command::Fleet(Ok(crate::panel::ipc::FleetCommand::LeaveInvitation { id })),
                    now,
                )
                .unwrap();
            thread.join().unwrap();
            assert_eq!(session.daemon.enrollment.id(), Some(id));
            assert_eq!(session.daemon.enrollment.phase(), crate::fleet::enroll::Phase::Incoming);
            assert_eq!(session.daemon.offer, Some(vec![1, 2, 3]));
            if saved {
                assert_eq!(reply, "ok\n");
                assert!(session.daemon.saved_fleet().is_none());
            } else {
                assert!(reply.starts_with("error:"));
                assert_eq!(session.daemon.saved_fleet(), old);
            }
        }
    }

    #[test]
    fn leave_and_join_waits_for_confirmed_deletion_and_preserves_pairing_keys() {
        for saved in [false, true] {
            let (tx, rx) = mpsc::channel();
            let (jobs, worker) = mpsc::channel();
            let mut session = session(rx, jobs);
            let local = Address::parse("02:00:00:00:00:02").unwrap();
            session.daemon.set_local_address(Some(local));
            let keys = AccessoryKeys { irk: [3; 16], encryption: [4; 16] };
            session.daemon.set_accessory_keys(keys.clone());
            let old = session.daemon.saved_fleet();
            let thread = std::thread::spawn(move || {
                let Ok(SecretJob::ResetFleet(reply)) = worker.recv() else {
                    panic!("expected reset")
                };
                // Earlier asynchronous snapshots must not restore the old fleet.
                tx.send(Input::KeyringStored).unwrap();
                reply.send(if saved { Ok(()) } else { Err("keyring locked".into()) }).unwrap();
            });
            let reply = session
                .command(
                    Command::Fleet(Ok(crate::panel::ipc::FleetCommand::Join {
                        address: Address::parse("02:00:00:00:00:01").unwrap(),
                        name: "gram".into(),
                        leave: true,
                    })),
                    Clock::new(1000, 1000),
                )
                .unwrap();
            thread.join().unwrap();
            assert_eq!(session.daemon.membership.accessory_keys(), Some(&keys));
            if saved {
                assert_eq!(reply, "ok\n");
                assert!(session.daemon.saved_fleet().is_none());
                assert_eq!(
                    session.daemon.enrollment.phase(),
                    crate::fleet::enroll::Phase::WaitingOffer
                );
            } else {
                assert!(reply.starts_with("error:"));
                assert_eq!(session.daemon.saved_fleet(), old);
                assert_eq!(session.daemon.enrollment.phase(), crate::fleet::enroll::Phase::Idle);
            }
        }
    }

    #[test]
    fn reset_waits_behind_writes_and_discards_their_fleet_replies_but_preserves_keys() {
        let (reports, inbox) = mpsc::channel();
        let (jobs, worker) = mpsc::channel();
        let mut session = session(inbox, jobs.clone());
        let keys = AccessoryKeys { irk: [3; 16], encryption: [4; 16] };
        let expected_keys = keys.clone();
        jobs.send(SecretJob::Fleet(fleet())).unwrap();
        jobs.send(SecretJob::Reload).unwrap();
        let thread = thread::spawn(move || {
            assert!(matches!(worker.recv(), Ok(SecretJob::Fleet(_))));
            reports.send(Input::KeyringStored).unwrap();
            assert!(matches!(worker.recv(), Ok(SecretJob::Reload)));
            reports
                .send(Input::KeyringUnlocked { fleet: Some(fleet()), keys: Some(keys) })
                .unwrap();
            let Ok(SecretJob::ResetFleet(reply)) = worker.recv() else {
                panic!("expected reset after writes")
            };
            // The memory reset must not be reported as success until this reply.
            reply.send(Ok(())).unwrap();
            let Ok(SecretJob::ResetFleet(reply)) = worker.recv() else {
                panic!("expected repeated reset")
            };
            reply.send(Ok(())).unwrap();
        });
        let now = Clock::new(100, 1_700_000_000_000);
        for _ in 0..2 {
            let reply = session.command(Command::FleetReset, now).unwrap();
            assert!(reply.starts_with("Fleet reset on this machine."));
            assert!(session.daemon.saved_fleet().is_none());
            assert_eq!(session.daemon.membership().accessory_keys(), Some(&expected_keys));
            assert!(session.keyring_retry_at.is_none());
        }
        thread.join().unwrap();
    }

    #[test]
    fn reset_failure_keeps_membership_and_does_not_consume_pending_reports() {
        let (reports, inbox) = mpsc::channel();
        let (jobs, worker) = mpsc::channel();
        let mut session = session(inbox, jobs);
        let thread = thread::spawn(move || {
            let Ok(SecretJob::ResetFleet(reply)) = worker.recv() else { panic!("expected reset") };
            reports.send(Input::KeyringStored).unwrap();
            reply.send(Err("keyring is locked".into())).unwrap();
        });
        let reply = session.command(Command::FleetReset, Clock::new(100, 100)).unwrap();
        assert_eq!(reply, "error: fleet reset failed: keyring is locked\n");
        assert_eq!(session.daemon.saved_fleet(), Some(fleet()));
        assert!(matches!(session.reports.try_recv(), Ok(Input::KeyringStored)));
        thread.join().unwrap();
    }

    fn removable_session(reports: Receiver<Input>, jobs: Sender<SecretJob>) -> Session {
        let mut session = session(reports, jobs);
        let local = Address::parse("02:00:00:00:00:02").unwrap();
        session.daemon.set_local_address(Some(local));
        let mut saved = fleet();
        saved.members.push(Member { address: local, label: "test-host".into() });
        saved.members.push(Member {
            address: Address::parse("02:00:00:00:00:03").unwrap(),
            label: "other-peer".into(),
        });
        session.daemon.restore_fleet(saved);
        session
    }

    fn remove_peer() -> Command {
        Command::Fleet(Ok(crate::panel::ipc::FleetCommand::Remove {
            address: fleet().members[0].address,
        }))
    }

    #[test]
    fn remove_saves_only_the_reduced_membership_and_ignores_stale_keyring_reports() {
        let (reports, inbox) = mpsc::channel();
        let (jobs, worker) = mpsc::channel();
        let mut session = removable_session(inbox, jobs.clone());
        let keys = AccessoryKeys { irk: [3; 16], encryption: [4; 16] };
        session.daemon.set_accessory_keys(keys.clone());
        let previous = session.daemon.saved_fleet().unwrap();
        let mut expected = previous.clone();
        expected.members.remove(0);
        let saved = expected.clone();
        jobs.send(SecretJob::Fleet(previous.clone())).unwrap();
        let thread = thread::spawn(move || {
            assert!(matches!(worker.recv(), Ok(SecretJob::Fleet(_))));
            reports.send(Input::KeyringUnlocked { fleet: Some(previous), keys: None }).unwrap();
            let Ok(SecretJob::ReplaceFleet { fleet, reply }) = worker.recv() else {
                panic!("expected reduced fleet after pending write")
            };
            assert_eq!(fleet, saved);
            reply.send(Ok(())).unwrap();
        });
        let now = Clock::new(100, 100);
        assert!(session.command(remove_peer(), now).unwrap().starts_with("Removed "));
        assert_eq!(session.daemon.saved_fleet(), Some(expected));
        assert_eq!(session.daemon.membership().accessory_keys(), Some(&keys));
        assert!(session.command(remove_peer(), now).unwrap().starts_with("Device is not a member"));
        thread.join().unwrap();
    }

    #[test]
    fn failed_member_removal_preserves_the_saved_membership_in_memory() {
        let (_reports, inbox) = mpsc::channel();
        let (jobs, worker) = mpsc::channel();
        let mut session = removable_session(inbox, jobs);
        let previous = session.daemon.saved_fleet();
        let thread = thread::spawn(move || {
            let Ok(SecretJob::ReplaceFleet { reply, .. }) = worker.recv() else {
                panic!("expected reduced fleet")
            };
            reply.send(Err("keyring is locked".into())).unwrap();
        });
        assert_eq!(
            session.command(remove_peer(), Clock::new(100, 100)).unwrap(),
            "error: fleet member removal failed: keyring is locked\n"
        );
        assert_eq!(session.daemon.saved_fleet(), previous);
        thread.join().unwrap();
    }

    #[test]
    fn removing_self_or_removing_during_enrollment_does_not_write_the_keyring() {
        let (_reports, inbox) = mpsc::channel();
        let (jobs, worker) = mpsc::channel();
        let mut session = removable_session(inbox, jobs);
        let now = Clock::new(100, 100);
        let local = session.daemon.local_address.unwrap();
        let self_remove =
            Command::Fleet(Ok(crate::panel::ipc::FleetCommand::Remove { address: local }));
        assert!(session.command(self_remove, now).unwrap().contains("use fleet reset"));
        assert!(session.daemon.invite(
            fleet().members[0].address,
            "test-peer",
            crate::types::Code::parse("123456").unwrap(),
            crate::types::InvitationId::new([1; 16]),
            now
        ));
        assert!(session.command(remove_peer(), now).unwrap().contains("finish or cancel"));
        assert!(worker.try_recv().is_err());
    }
}

#[cfg(test)]
mod advertisement_tests {
    use super::*;

    #[test]
    fn fresh_install_discovers_controller_without_pairing_or_connecting() {
        let address = Address::parse("02:00:00:00:00:01").unwrap();
        let (tx, rx) = mpsc::channel();
        let mut live = Live::default();
        remember_inventory(
            &tx,
            None,
            &mut live,
            bluez::Inventory {
                adapters: HashMap::from([("/org/bluez/hci0".into(), address)]),
                devices: Vec::new(),
                transports: HashMap::new(),
            },
        );
        assert_eq!(live.adapter_path.as_deref(), Some("/org/bluez/hci0"));
        let input = rx.try_recv().unwrap();
        assert!(matches!(input, Input::Controller(found) if found == address));
        let mut daemon = Daemon::new("test-host");
        let effects = daemon.step(input, crate::types::Clock::new(1, 1)).effects;
        assert!(effects.iter().any(|effect| matches!(effect, Effect::Listen)));
        assert!(rx.try_recv().is_err());
        assert!(live.accessory.is_none());
        assert!(live.link.fd().is_none());
    }

    #[test]
    fn failed_or_lost_discovery_retries_without_becoming_a_connection_request() {
        let now = Instant::now();
        let mut air = Air { want_scan: true, ..Air::default() };
        assert!(air.scan_due(now));
        // StartDiscovery can fail while bluetoothd is restarting. Do not
        // mistake submitting that request for a running discovery session.
        air.scan_result(false, now);
        assert!(!air.scanned);
        assert!(!air.scan_due(now + Duration::from_millis(999)));
        assert!(air.scan_due(now + Duration::from_secs(1)));
        air.scan_result(true, now + Duration::from_secs(1));
        assert!(air.scanned);
        assert!(!air.scan_due(now + Duration::from_secs(10)));
        // DiscoveryStopped / bluetoothd loss invalidates a successful start.
        air.scan_result(false, now + Duration::from_secs(10));
        assert!(air.want_scan);
        assert!(air.scan_due(now + Duration::from_secs(11)));
    }

    #[test]
    fn bluetoothd_returning_power_cycles_the_adapter_once() {
        let now = Instant::now();
        let mut air = Air { want_scan: true, ..Air::default() };
        air.scan_result(true, now);
        air.arm_controller_reset(now);
        assert_eq!(air.power_request(now), Some(false));
        assert!(air.reset_blocks_scan());
        // The return signal and the Powered signals the cycle causes must not
        // start a second cycle. Powered is not a reply, so it does not advance
        // this one either: only the Set call's reply does.
        air.arm_controller_reset(now + Duration::from_millis(1));
        assert_eq!(air.power_request(now + Duration::from_millis(1)), Some(false));
        air.note_power_reply(false, true, now);
        assert_eq!(air.power_request(now), Some(true));
        air.arm_controller_reset(now + Duration::from_millis(2));
        assert_eq!(air.power_request(now + Duration::from_millis(2)), Some(true));
        air.note_power_reply(true, true, now);
        assert_eq!(air.power_request(now), None);
        assert!(!air.reset_blocks_scan());
        assert!(air.scan_due(now));
        // The next time bluetoothd returns, one new cycle starts.
        air.arm_controller_reset(now + Duration::from_secs(1));
        assert_eq!(air.power_request(now + Duration::from_secs(1)), Some(false));
    }

    #[test]
    fn a_failed_power_off_retries_then_leaves_the_controller_alone() {
        let now = Instant::now();
        let mut air = Air::default();
        air.arm_controller_reset(now);
        air.note_power_reply(false, false, now);
        assert_eq!(air.power_request(now), None);
        assert_eq!(air.power_request(now + RESET_RETRY), Some(false));
        air.note_power_reply(false, false, now + RESET_RETRY);
        air.note_power_reply(false, false, now + RESET_RETRY * 2);
        assert!(!air.reset_blocks_scan());
        assert_eq!(air.power_request(now + RESET_RETRY * 3), None);
    }

    #[test]
    fn power_on_keeps_retrying_without_turning_the_adapter_off_again() {
        let now = Instant::now();
        let mut air = Air::default();
        air.arm_controller_reset(now);
        air.note_power_reply(false, true, now);
        air.note_power_reply(true, false, now);
        assert_eq!(air.power_request(now), None);
        assert_eq!(air.power_request(now + RESET_RETRY), Some(true));
        air.note_power_reply(true, false, now + RESET_RETRY);
        assert_eq!(air.power_request(now + RESET_RETRY * 2), Some(true));
        air.note_power_reply(true, true, now + RESET_RETRY * 2);
        assert!(!air.reset_blocks_scan());
    }

    #[test]
    fn bluetoothd_leaving_cancels_the_power_cycle() {
        let now = Instant::now();
        let mut air = Air::default();
        air.arm_controller_reset(now);
        air.note_power_reply(false, true, now);
        air.cancel_controller_reset();
        assert!(!air.reset_blocks_scan());
        assert_eq!(air.power_request(now), None);
        air.arm_controller_reset(now);
        assert_eq!(air.power_request(now), Some(false));
    }

    #[test]
    fn bluez_back_waits_until_the_power_cycle_finishes() {
        let (tx, rx) = mpsc::channel();
        let mut live = Live { bluez_back_pending: true, ..Default::default() };
        live.air.arm_controller_reset(Instant::now());
        settle_controller_reset(&tx, &mut live, false, true);
        assert!(rx.try_recv().is_err());
        assert!(live.bluez_back_pending);
        settle_controller_reset(&tx, &mut live, true, true);
        assert!(matches!(rx.try_recv(), Ok(Input::BluezBack)));
        assert!(!live.bluez_back_pending);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn a_power_cycle_keeps_its_powered_signals_until_one_return() {
        let (tx, rx) = mpsc::channel();
        let mut live = Live { adapter_path: Some("/org/bluez/hci0".into()), ..Default::default() };
        begin_controller_cycle(&mut live);
        report_adapter_power(&tx, &mut live, "/org/bluez/hci0", false);
        report_adapter_power(&tx, &mut live, "/org/bluez/hci0", true);
        assert!(rx.try_recv().is_err(), "this cycle's Powered stays inside the thread");
        settle_controller_reset(&tx, &mut live, false, true);
        assert!(rx.try_recv().is_err());
        settle_controller_reset(&tx, &mut live, true, true);
        assert!(matches!(rx.try_recv(), Ok(Input::BluezBack)));
        assert!(rx.try_recv().is_err());
        // The cycle is idle. A later toggle is the radio, and the daemon hears it.
        report_adapter_power(&tx, &mut live, "/org/bluez/hci0", false);
        assert!(matches!(rx.try_recv(), Ok(Input::AdapterPower(false))));
    }

    #[test]
    fn bluetoothd_leaving_during_a_cycle_does_not_report_the_return() {
        let (tx, rx) = mpsc::channel();
        let mut live = Live { adapter_path: Some("/org/bluez/hci0".into()), ..Default::default() };
        begin_controller_cycle(&mut live);
        note_bluez_left(&tx, &mut live);
        let mut saw_lost = false;
        while let Ok(input) = rx.try_recv() {
            match input {
                Input::BluezLost => saw_lost = true,
                Input::BluezBack | Input::AdapterPower(_) => {
                    panic!("leaving cancels the cycle without a return")
                }
                _ => {}
            }
        }
        assert!(saw_lost);
        report_adapter_power(&tx, &mut live, "/org/bluez/hci0", true);
        assert!(matches!(rx.try_recv(), Ok(Input::AdapterPower(true))));
    }

    #[test]
    fn a_running_scan_is_rechecked_with_bluez() {
        let now = Instant::now();
        let mut air = Air { want_scan: true, ..Air::default() };
        assert!(!air.scan_check_due(now));
        air.scan_result(true, now);
        assert!(!air.scan_check_due(now + SCAN_CHECK - Duration::from_millis(1)));
        assert!(air.scan_check_due(now + SCAN_CHECK));
        // A confirmation pushes the next check out.
        air.scan_checked_at = Some(now + SCAN_CHECK);
        assert!(!air.scan_check_due(now + SCAN_CHECK + Duration::from_secs(1)));
        // A scan the daemon no longer wants is not checked.
        air.want_scan = false;
        assert!(!air.scan_check_due(now + SCAN_CHECK * 3));
    }

    fn pairing_frame() -> Vec<u8> {
        let mut frame = vec![0; 27];
        frame[..5].copy_from_slice(&[0x07, 0x19, 0x07, 0x27, 0x20]);
        frame
    }

    #[test]
    fn setup_yields_scanning_until_pairing_finishes_or_the_attempt_expires() {
        let now = Instant::now();
        let mut air = Air { want_scan: true, ..Air::default() };
        air.observe_pairing(false, Some(&pairing_frame()), now);
        assert!(air.pairing_paused(now));
        assert!(!air.scan_due(now));
        // A late discovery reply, adapter recovery, or scan retry must not
        // reclaim discovery from the Bluetooth panel during pairing.
        air.scan_result(true, now);
        assert!(air.pairing_paused(now));
        air.scan_result(false, now + Duration::from_secs(1));
        assert!(!air.scan_due(now + Duration::from_secs(119)));
        assert!(air.scan_due(now + Duration::from_secs(120)));

        air.observe_pairing(false, Some(&pairing_frame()), now);
        air.pairing_completed(now + Duration::from_secs(10));
        assert!(!air.scan_due(now + Duration::from_secs(14)));
        assert!(air.scan_due(now + Duration::from_secs(15)));
        assert!(air.want_scan);
    }

    #[test]
    fn pairing_yield_ignores_normal_packets_and_keeps_paired_hosts_observing() {
        let now = Instant::now();
        let mut air = Air { want_scan: true, ..Air::default() };
        air.observe_pairing(true, Some(&pairing_frame()), now);
        assert!(air.scan_due(now));
        let mut ordinary = pairing_frame();
        ordinary[2] = 1;
        air.observe_pairing(false, Some(&ordinary), now);
        air.observe_pairing(false, Some(&pairing_frame()[..26]), now);
        assert!(air.scan_due(now));
        air.observe_pairing(false, Some(&pairing_frame()), now);
        // Interleaved ordinary packets must not end the pairing window.
        air.observe_pairing(false, Some(&ordinary), now + Duration::from_secs(1));
        assert!(!air.scan_due(now + Duration::from_secs(2)));
        // Reports from another client's scan keep an ongoing attempt protected.
        air.observe_pairing(false, Some(&pairing_frame()), now + Duration::from_secs(119));
        assert!(!air.scan_due(now + Duration::from_secs(120)));
        assert!(air.scan_due(now + Duration::from_secs(239)));
    }

    #[test]
    fn fleet_advertisements_do_not_require_a_tracked_bluetooth_device() {
        let (tx, rx) = std::sync::mpsc::channel();
        let bytes = vec![0x05, 0x01, 0x02];
        report_advertisement(&tx, None, Some(bytes.clone()), None);
        assert!(matches!(rx.try_recv(), Ok(Input::Advertisement(heard)) if heard == bytes));
        report_advertisement(&tx, None, Some(Vec::new()), Some(Vec::new()));
        assert!(rx.try_recv().is_err());
    }

    fn request(interface: &str, member: &str) -> dbus::Message {
        let mut message =
            dbus::Message::new_method_call("org.fleetpods.Test", ADVERTISEMENT, interface, member)
                .unwrap();
        message.set_serial(1);
        message
    }

    #[test]
    fn removing_bluetooth_pairing_is_distinct_from_an_ordinary_disconnect() {
        for removed in [false, true] {
            let address = Address::parse("CC:22:FE:54:DF:C7").unwrap();
            let mut live = Live::default();
            let started = Instant::now();
            live.air.observe_pairing(false, Some(&pairing_frame()), started);
            live.devices.push(Device {
                path: "/org/bluez/hci0/dev_CC_22_FE_54_DF_C7".into(),
                adapter_path: "/org/bluez/hci0".into(),
                adapter: Some(Address::parse("A8:72:4D:0C:7C:EE").unwrap()),
                address,
                name: "AirPods".into(),
                rssi: None,
                paired: true,
                bonded: false,
                connected: true,
                control: true,
            });
            let (tx, rx) = mpsc::channel();
            sync(&tx, Some(address), &mut live);
            let initial: Vec<_> = rx.try_iter().collect();
            assert!(initial.iter().any(|input| matches!(input, Input::Paired { .. })));
            assert!(initial
                .iter()
                .any(|input| matches!(input, Input::Bonded { saved: false, .. })));
            assert!(
                !live.air.scan_due(started + Duration::from_secs(30)),
                "a temporary pairing must not end the setup scan pause"
            );
            live.devices[0].connected = false;
            sync(&tx, Some(address), &mut live);
            assert!(!rx.try_iter().any(|input| matches!(input, Input::Unpaired { .. })));
            // A durable bond keeps the accessory selected even if Paired drops
            // during disconnection. The daemon must retain its reconnect timer.
            live.devices[0].bonded = true;
            live.devices[0].paired = false;
            sync(&tx, Some(address), &mut live);
            let bonded: Vec<_> = rx.try_iter().collect();
            assert!(!bonded.iter().any(|input| matches!(input, Input::Unpaired { .. })));
            assert!(bonded.iter().any(|input| matches!(input, Input::Bonded { saved: true, .. })));
            assert!(
                live.air.scan_due(started + Duration::from_secs(30)),
                "a saved bond ends setup after the service discovery grace"
            );
            assert_eq!(live.accessory, Some(address));
            assert!(live.paths.contains_key(&address));
            if removed {
                live.devices.clear();
            } else {
                live.devices[0].bonded = false;
            }
            sync(&tx, Some(address), &mut live);
            sync(&tx, Some(address), &mut live);
            assert_eq!(
                rx.try_iter()
                    .filter(
                        |input| matches!(input, Input::Unpaired { address: a } if *a == address)
                    )
                    .count(),
                1
            );
            assert!(live.accessory.is_none());
            assert!(!live.paths.contains_key(&address));
        }
    }

    // bluetoothd crashed at 07:04:12 on the XPS and systemd restarted it.
    // Its whole inventory vanished while it was down; the daemon read the
    // absent accessory as a removed pairing and the panel showed its
    // pairing screen even though BlueZ still held the bond.
    #[test]
    fn a_bluetoothd_outage_does_not_unpair_the_accessory() {
        let address = Address::parse("CC:22:FE:54:DF:C7").unwrap();
        let mut live = Live::default();
        live.devices.push(Device {
            path: "/org/bluez/hci0/dev_CC_22_FE_54_DF_C7".into(),
            adapter_path: "/org/bluez/hci0".into(),
            adapter: Some(Address::parse("A8:72:4D:0C:7C:EE").unwrap()),
            address,
            name: "AirPods".into(),
            rssi: None,
            paired: true,
            bonded: true,
            connected: true,
            control: true,
        });
        let (tx, rx) = mpsc::channel();
        sync(&tx, Some(address), &mut live);
        assert_eq!(live.accessory, Some(address));
        let _ = rx.try_iter().count();
        // bluetoothd goes away and takes its inventory with it.
        live.restoring_bluez = true;
        live.devices.clear();
        sync(&tx, Some(address), &mut live);
        sync(&tx, Some(address), &mut live);
        assert!(
            !rx.try_iter().any(|input| matches!(input, Input::Unpaired { .. })),
            "an outage must not read as a removed pairing"
        );
        assert_eq!(live.accessory, Some(address), "the selection survives the outage");
        // Owning the bus name again is not the end of startup: BlueZ may
        // publish an empty inventory, then the adapter, then its devices.
        bluez_returned(
            &tx,
            Some(address),
            &mut live,
            Some(bluez::Inventory {
                adapters: HashMap::new(),
                devices: Vec::new(),
                transports: HashMap::new(),
            }),
        );
        sync(&tx, Some(address), &mut live);
        assert!(
            !rx.try_iter().any(|input| matches!(input, Input::Unpaired { .. })),
            "an incomplete startup inventory is not an unpair"
        );
        assert_eq!(live.accessory, Some(address));
        // An explicit removal still wins while startup is incomplete.
        device_removed(&tx, Some(address), &mut live, "/org/bluez/hci0/dev_CC_22_FE_54_DF_C7");
        assert!(rx
            .try_iter()
            .any(|input| matches!(input, Input::Unpaired { address: a } if a == address)));
        assert!(live.accessory.is_none());
    }

    // 2026-10-28 07:36:16: BlueZ crashed five seconds into Brave playback
    // on the XPS speakers. Its empty startup inventory caused an Unpaired
    // event, and the daemon paused Brave even though the pods were in the case.
    #[test]
    fn bluetooth_restart_preserves_speaker_playback_while_the_pods_are_unreachable() {
        use crate::audio::play::PlayerSeen;
        use crate::audio::route::{Graph, LiveStream, Sink};

        for closed_case_reported in [false, true] {
            let address = Address::parse("CC:22:FE:54:DF:C7").unwrap();
            let adapter = Address::parse("A8:72:4D:0C:7C:EE").unwrap();
            let device = Device {
                path: "/org/bluez/hci0/dev_CC_22_FE_54_DF_C7".into(),
                adapter_path: "/org/bluez/hci0".into(),
                adapter: Some(adapter),
                address,
                name: "AirPods".into(),
                rssi: None,
                paired: true,
                bonded: true,
                connected: false,
                control: true,
            };
            let mut live = Live::default();
            live.devices.push(device.clone());
            let (tx, rx) = mpsc::channel();
            let mut daemon = Daemon::new("xps");
            let mut now = Clock::new(1_000, 1_000);
            let mut deliver = |input| {
                now.mono.0 += 1;
                now.unix.0 += 1;
                let outcome = daemon.step(input, now);
                assert!(
                    !outcome.effects.iter().any(|effect| matches!(
                        effect,
                        Effect::PausePlayers(_)
                            | Effect::ResumePlayers(_)
                            | Effect::MuteStreams(_)
                            | Effect::MoveStreams(_)
                            | Effect::SetDefaultSink { .. }
                    )),
                    "Bluetooth recovery must leave speaker playback alone: {:?}",
                    outcome.effects
                );
            };
            sync(&tx, Some(address), &mut live);
            for input in rx.try_iter() {
                deliver(input);
            }
            if closed_case_reported {
                deliver(Input::Lid { open: false });
            }
            deliver(Input::Players(vec![PlayerSeen {
                service: "org.mpris.MediaPlayer2.brave".into(),
                playing: false,
            }]));
            deliver(Input::Audio(Graph {
                default_sink: "speakers".into(),
                sinks: vec![Sink { name: "speakers".into(), address: None, profile: None }],
                cards: Vec::new(),
                streams: vec![LiveStream {
                    id: 1,
                    generation: 1,
                    sink: "speakers".into(),
                    role: String::new(),
                    node: "Brave".into(),
                    mute: false,
                    volume: 1.0,
                    corked: false,
                }],
            }));
            deliver(Input::Player {
                service: "org.mpris.MediaPlayer2.brave".into(),
                playing: Some(true),
            });
            live.restoring_bluez = true;
            live.devices.clear();
            deliver(Input::BluezLost);
            sync(&tx, Some(address), &mut live);
            for input in rx.try_iter() {
                deliver(input);
            }
            // A failed inventory read, an empty snapshot, and an adapter-only
            // snapshot can all precede the saved device's InterfacesAdded.
            for inventory in [
                None,
                Some(bluez::Inventory {
                    adapters: HashMap::new(),
                    devices: Vec::new(),
                    transports: HashMap::new(),
                }),
                Some(bluez::Inventory {
                    adapters: HashMap::from([("/org/bluez/hci0".into(), adapter)]),
                    devices: Vec::new(),
                    transports: HashMap::new(),
                }),
            ] {
                bluez_returned(&tx, Some(address), &mut live, inventory);
                sync(&tx, Some(address), &mut live);
                for input in rx.try_iter() {
                    deliver(input);
                }
                deliver(Input::Tick);
                assert_eq!(live.accessory, Some(address));
            }
            device_removed(&tx, Some(address), &mut live, "/org/bluez/hci0/dev_00_00_00_00_00_01");
            for input in rx.try_iter() {
                deliver(input);
            }
            assert!(live.restoring_bluez, "an unrelated removal cannot end restoration");
            // Restoration ends on a real device report; no second Play is needed.
            live.devices.push(device);
            sync(&tx, Some(address), &mut live);
            for input in rx.try_iter() {
                deliver(input);
            }
            deliver(Input::Tick);
            assert!(!live.restoring_bluez);
            assert_eq!(daemon.status_value(now)["bluetooth_paired"], true);
            assert_eq!(daemon.status_value(now)["native_handoff"]["playing"], true);

            // Losing the bond after restoration is still a real unpair.
            live.devices[0].paired = false;
            live.devices[0].bonded = false;
            sync(&tx, Some(address), &mut live);
            assert!(rx
                .try_iter()
                .any(|input| matches!(input, Input::Unpaired { address: a } if a == address)));
        }
    }

    #[test]
    fn an_existing_acl_requests_tracked_setup_once_without_opening_a_socket() {
        let address = Address::parse("CC:22:FE:54:DF:C7").unwrap();
        let mut live = Live::default();
        live.devices.push(Device {
            path: "/org/bluez/hci0/dev_CC_22_FE_54_DF_C7".into(),
            adapter_path: "/org/bluez/hci0".into(),
            adapter: None,
            address,
            name: "AirPods".into(),
            rssi: None,
            paired: true,
            bonded: false,
            connected: true,
            control: true,
        });
        let (tx, rx) = std::sync::mpsc::channel();
        for _ in 0..3 {
            sync(&tx, Some(address), &mut live);
        }
        let requests = rx
            .try_iter()
            .filter(|input| matches!(input, Input::AttachAvailable { generation: Generation(0) }))
            .count();
        assert_eq!(requests, 1);
        assert!(live.link.fd().is_none(), "only an explicit Open job may create the socket");
        assert!(live.defer_attach);
    }

    #[test]
    fn bluez_can_enumerate_the_advertisement_without_waiting_for_a_timeout() {
        let request = request("org.freedesktop.DBus.ObjectManager", "GetManagedObjects");
        let response = advertisement_reply(&[1, 2, 3], &request)
            .expect("BlueZ needs a reply before it refreshes advertisement properties");
        type Objects = HashMap<dbus::Path<'static>, HashMap<String, dbus::arg::PropMap>>;
        let objects: Objects = response.read1().unwrap();
        let props =
            &objects[&dbus::Path::new(ADVERTISEMENT).unwrap()]["org.bluez.LEAdvertisement1"];
        assert_eq!(props["Type"].0.as_str(), Some("broadcast"));
        let services = props["ServiceData"].0.as_iter().unwrap().collect::<Vec<_>>();
        assert!(!services.is_empty());
    }

    #[test]
    fn unsupported_advertisement_calls_get_an_error_instead_of_hanging() {
        let request = request("org.fleetpods.Unknown", "Unknown");
        let response = advertisement_reply(&[1], &request).expect("every call gets a reply");
        assert_eq!(response.msg_type(), dbus::message::MessageType::Error);
    }
}
#[test]
fn sleep_is_reported_before_the_locally_closed_control_socket() {
    let (socket, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
    let mut live = Live {
        link: ControlLink::from_test_fd(socket.into()),
        generation: Generation(7),
        socket_for: Some(Generation(7)),
        ..Live::default()
    };
    let (tx, rx) = mpsc::channel();
    report_sleep(&tx, &mut live, true);
    assert!(matches!(rx.try_recv().unwrap(), Input::Suspend));
    assert!(matches!(
        rx.try_recv().unwrap(),
        Input::ControlSocket { up: false, generation: Generation(7) }
    ));
    assert!(rx.try_recv().is_err());
    assert!(live.link.fd().is_none());
    assert!(live.defer_attach);
    report_sleep(&tx, &mut live, false);
    assert!(matches!(rx.try_recv().unwrap(), Input::Resume));
    assert!(rx.try_recv().is_err());
}
