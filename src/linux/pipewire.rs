//! PipeWire, through the PulseAudio protocol it speaks.
//! This thread reports the sink graph and performs sink changes the daemon
//! already chose. It does not decide who owns playback.
//!
//! The Pulse mainloop is polled together with a wake fd. Jobs do not wait on
//! a sleep, and a snapshot is published only when the picture changes.

use std::collections::HashMap;
use std::ffi::CStr;
use std::os::raw::{c_ulong, c_void};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use libpulse_binding::callbacks::ListResult;
use libpulse_binding::context::introspect::{CardInfo, ServerInfo, SinkInputInfo};
use libpulse_binding::context::subscribe::InterestMaskSet;
use libpulse_binding::context::{Context, FlagSet as ContextFlagSet, State};
use libpulse_binding::mainloop::standard::Mainloop;
use libpulse_binding::operation::Operation;
use libpulse_binding::volume::{Volume, VolumeLinear};

use crate::audio::health::Output;
use crate::audio::play::StreamSnap;
use crate::audio::route::{Card, Graph, LiveStream, Sink, StreamTarget};
use crate::linux::wake::Wake;
use crate::retry::{Delay, Limit, Policy, Requests, Task};
use crate::step::Input;
use crate::types::{Address, Generation, MonoMs};

pub enum Op {
    StartOutput {
        id: u64,
        name: String,
    },
    Mute(StreamTarget),
    Default(String),
    Move {
        sink: String,
        stream: StreamTarget,
    },
    Restore(StreamSnap),
    /// Suspend this sink and let it resume, so an idle output stops now.
    Suspend(String),
    /// Make this capture node the default source.
    DefaultSource(String),
    /// Select this codec profile on the card the audio server named.
    Profile {
        id: crate::pods::lifecycle::RequestId,
        card: String,
        name: String,
    },
    /// Rebuild the card's nodes: profile off, then this profile again, and
    /// report completion as output restart `restart`.
    Refresh {
        card: String,
        name: String,
        restart: u64,
    },
}

enum Job {
    Ops(Vec<Op>),
    Stop,
}

pub struct Handle {
    jobs: Sender<Job>,
    wake: Wake,
    thread: Option<JoinHandle<()>>,
}

impl Handle {
    pub fn send(&self, ops: Vec<Op>) {
        if ops.is_empty() {
            return;
        }
        let _ = self.jobs.send(Job::Ops(ops));
        self.wake.poke();
    }

    pub fn stop(&mut self) {
        let _ = self.jobs.send(Job::Stop);
        self.wake.poke();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct StreamIdentity {
    generation: u64,
    object_id: Option<u32>,
}

struct TemporaryMute {
    object_id: u32,
    confirmed: bool,
    release: Option<Task>,
}

/// The audio worker owns every possibly-applied soft mute until it is released
/// or a complete stream inventory proves that its node no longer exists.
struct TemporaryMutes {
    held: HashMap<u64, TemporaryMute>,
    requests: Requests,
    origin: std::time::Instant,
}

impl Default for TemporaryMutes {
    fn default() -> Self {
        Self {
            held: HashMap::new(),
            requests: Requests::default(),
            origin: std::time::Instant::now(),
        }
    }
}

impl TemporaryMutes {
    fn now(&self) -> MonoMs {
        MonoMs(self.origin.elapsed().as_millis() as u64)
    }
    fn hold(
        &mut self,
        stream: StreamTarget,
        object_id: u32,
        mut set: impl FnMut(u32, bool) -> bool,
    ) {
        let held = self.held.entry(stream.generation).or_insert(TemporaryMute {
            object_id,
            confirmed: false,
            release: None,
        });
        // A new hold cancels pending cleanup. Record ownership before either
        // pw-cli can apply the command: failed confirmation is not failed mute.
        held.release = None;
        held.confirmed |= set(object_id, true);
    }

    fn release(&mut self, stream: StreamTarget) {
        if let Some(held) = self.held.get_mut(&stream.generation) {
            held.release.get_or_insert_with(|| {
                Task::new(Policy {
                    delay: Delay::Staged(&[100, 250, 1_000, 5_000]),
                    limit: Limit::UntilCancelled,
                    request_timeout_ms: None, // The command itself has a bounded timeout.
                    confirmation_timeout_ms: None,
                })
            });
        }
    }

    fn release_all(&mut self) {
        let targets: Vec<_> =
            self.held.keys().map(|&generation| StreamTarget { id: 0, generation }).collect();
        for target in targets {
            self.release(target);
        }
    }

    fn confirmed(&self, stream: &LiveStream) -> bool {
        self.held.get(&stream.generation).is_some_and(|held| held.confirmed)
    }

    fn observe(&mut self, cached: &HashMap<u32, StreamIdentity>) {
        self.held.retain(|&generation, held| {
            cached.values().any(|stream| {
                stream.generation == generation && stream.object_id == Some(held.object_id)
            })
        });
    }

    fn clean_up(
        &mut self,
        cached: &HashMap<u32, StreamIdentity>,
        now: MonoMs,
        mut set: impl FnMut(u32, bool) -> bool,
    ) {
        self.held.retain(|&generation, held| {
            let Some(release) = &mut held.release else { return true };
            // Never address a recycled node, or act while the Pulse connection
            // has no current inventory. A later observation resumes cleanup.
            if !cached.values().any(|stream| {
                stream.generation == generation && stream.object_id == Some(held.object_id)
            }) {
                return true;
            }
            let Some(id) = release.launch(&mut self.requests, Generation(generation), now) else {
                return true;
            };
            let ok = set(held.object_id, false);
            release.reply(id, ok, now);
            !ok
        });
    }
}

struct Sample {
    graph: Graph,
    outputs: Vec<Output>,
    cache: HashMap<u32, StreamIdentity>,
    levels: HashMap<String, SinkLevel>,
}

#[derive(Clone, Debug, PartialEq)]
struct SinkLevel {
    mute: bool,
    // Pulse's slider scale, per channel (100% = PA_VOLUME_NORM), not linear gain.
    volume_percent: Vec<f64>,
}

/// Observe server state independently of the policy graph and its soft-mute
/// overlay. A volume notification tells us what changed, not which gesture or
/// application caused it. Journal timestamps provide ordering with commands.
#[derive(Default)]
struct MediaTrace {
    levels: HashMap<String, SinkLevel>,
    streams: HashMap<(u32, u64), LiveStream>,
}

impl MediaTrace {
    fn observe(&mut self, sample: &Sample) {
        for (name, level) in &sample.levels {
            let previous = self.levels.get(name);
            if previous != Some(level) {
                eprintln!("fleetpodsd: media output source=pulse sink={name:?} previous={previous:?} observed={level:?}");
            }
        }
        for name in self.levels.keys().filter(|name| !sample.levels.contains_key(*name)) {
            eprintln!("fleetpodsd: media output source=pulse sink={name:?} removed");
        }
        self.levels.clone_from(&sample.levels);
        let mut streams = HashMap::new();
        for stream in &sample.graph.streams {
            let key = (stream.id, stream.generation);
            let previous = self.streams.get(&key);
            if previous != Some(stream) {
                // LiveStream contains routing/control fields, no media titles or URLs.
                eprintln!("fleetpodsd: media stream source=pulse previous={previous:?} observed={stream:?}");
            }
            streams.insert(key, stream.clone());
        }
        for (id, generation) in self.streams.keys().filter(|key| !streams.contains_key(*key)) {
            eprintln!(
                "fleetpodsd: media stream source=pulse id={id} generation={generation} removed"
            );
        }
        self.streams = streams;
    }
}

struct RawStream {
    id: u32,
    object_id: Option<u32>,
    generation: u64,
    sink_index: u32,
    role: String,
    node: String,
    mute: bool,
    volume: f64,
    corked: bool,
}

struct RawSink {
    index: u32,
    name: String,
    address: Option<Address>,
    profile: Option<String>,
    state: i32,
    level: SinkLevel,
}

struct RawCard {
    name: String,
    address: Option<Address>,
    profiles: Vec<String>,
    active: Option<String>,
}

struct Picture {
    default_sink: String,
    sinks: Vec<RawSink>,
    streams: Vec<RawStream>,
    cards: Vec<RawCard>,
    sinks_done: bool,
    streams_done: bool,
    streams_valid: bool,
    server_done: bool,
    cards_done: bool,
}

impl Picture {
    fn fresh() -> Self {
        Self {
            default_sink: String::new(),
            sinks: Vec::new(),
            streams: Vec::new(),
            cards: Vec::new(),
            sinks_done: false,
            streams_done: false,
            streams_valid: true,
            server_done: false,
            cards_done: false,
        }
    }

    fn ready(&self) -> bool {
        self.sinks_done && self.streams_done && self.server_done && self.cards_done
    }
}

type StreamQuery = Operation<dyn FnMut(ListResult<&SinkInputInfo>)>;
type ServerQuery = Operation<dyn FnMut(&ServerInfo)>;
type CardQuery = Operation<dyn FnMut(ListResult<&CardInfo>)>;
type ProfileOp = Operation<dyn FnMut(bool)>;

struct Query {
    picture: Arc<Mutex<Picture>>,
    _sinks: Option<SinkList>,
    _streams: StreamQuery,
    _server: ServerQuery,
    _cards: CardQuery,
}

/// The sink-list operation and the callback state it owns.
///
/// `libpulse-binding` builds every `SinkInfo` by transmuting `pa_sink_state_t` into
/// `SinkState`. That enum only has Invalid, Running, Idle, and Suspended. PipeWire's
/// pulse module sends `PA_SINK_UNLINKED` (-3) and `PA_SINK_INIT` (-2) while a node is
/// in error or being created. Rust treats that transmute as a nounwind panic, so
/// `catch_unwind` around the binding's callback cannot stop the process aborting.
/// This list reads fields through the C API, decoding the state bytes as a raw
/// integer so values outside the binding's Rust enum remain safe.
struct SinkList {
    op: *mut libpulse_sys::pa_operation,
    userdata: *mut SinkWatch,
    done: Arc<AtomicBool>,
}

struct SinkWatch {
    picture: Arc<Mutex<Picture>>,
    done: Arc<AtomicBool>,
}

impl Drop for SinkList {
    fn drop(&mut self) {
        unsafe {
            if !self.op.is_null() {
                libpulse_sys::pa_operation_cancel(self.op);
                libpulse_sys::pa_operation_unref(self.op);
            }
        }
        if !self.done.swap(true, Ordering::AcqRel) && !self.userdata.is_null() {
            drop(unsafe { Box::from_raw(self.userdata) });
        }
    }
}

impl Query {
    fn start(context: &Context, raw: *mut libpulse_sys::pa_context) -> Self {
        let picture = Arc::new(Mutex::new(Picture::fresh()));
        let sinks = Arc::clone(&picture);
        let streams = Arc::clone(&picture);
        let server = Arc::clone(&picture);
        let cards = Arc::clone(&picture);
        let sink_op = SinkList::start(raw, &sinks);
        let stream_op = context.introspect().get_sink_input_info_list(move |item| {
            let Ok(mut picture) = streams.lock() else {
                return;
            };
            match item {
                ListResult::Item(info) => picture.streams.push(raw_stream(info)),
                ListResult::End => picture.streams_done = true,
                ListResult::Error => {
                    picture.streams_done = true;
                    picture.streams_valid = false;
                }
            }
        });
        let card_op = context.introspect().get_card_info_list(move |item| {
            let Ok(mut picture) = cards.lock() else {
                return;
            };
            match item {
                ListResult::Item(info) => {
                    if let Some(name) = info.name.as_deref() {
                        picture.cards.push(RawCard {
                            name: name.to_string(),
                            address: info
                                .proplist
                                .get_str("api.bluez5.address")
                                .as_deref()
                                .and_then(Address::parse),
                            profiles: info
                                .profiles
                                .iter()
                                .filter(|profile| profile.available)
                                .filter_map(|profile| profile.name.as_deref().map(str::to_string))
                                .collect(),
                            active: info
                                .active_profile
                                .as_ref()
                                .and_then(|profile| profile.name.as_deref().map(str::to_string)),
                        });
                    }
                }
                ListResult::End | ListResult::Error => picture.cards_done = true,
            }
        });
        let server_op = context.introspect().get_server_info(move |info| {
            if let Ok(mut picture) = server.lock() {
                if let Some(name) = info.default_sink_name.as_deref() {
                    picture.default_sink = name.to_string();
                }
                picture.server_done = true;
            }
        });
        Self { picture, _sinks: sink_op, _streams: stream_op, _server: server_op, _cards: card_op }
    }

    fn ready(&self) -> bool {
        self.picture.lock().ok().is_some_and(|picture| picture.ready())
    }

    fn finish(self) -> Option<Sample> {
        let picture = self.picture.lock().ok()?;
        if !picture.ready() || !picture.streams_valid {
            return None;
        }
        let names: HashMap<u32, String> =
            picture.sinks.iter().map(|sink| (sink.index, sink.name.clone())).collect();
        let mut cache = HashMap::new();
        let streams = picture
            .streams
            .iter()
            .filter_map(|stream| {
                cache.insert(
                    stream.id,
                    StreamIdentity { generation: stream.generation, object_id: stream.object_id },
                );
                // An output rebuild can hide the sink while the muted stream
                // still exists. Keep its identity available for cleanup.
                let sink = names.get(&stream.sink_index)?.clone();
                Some(LiveStream {
                    id: stream.id,
                    generation: stream.generation,
                    sink,
                    role: stream.role.clone(),
                    node: stream.node.clone(),
                    mute: stream.mute,
                    temporarily_muted: false,
                    volume: stream.volume,
                    corked: stream.corked,
                })
            })
            .collect();
        let sinks = picture
            .sinks
            .iter()
            .map(|sink| Sink {
                name: sink.name.clone(),
                address: sink.address,
                profile: sink.profile.clone(),
            })
            .collect();
        let cards = picture
            .cards
            .iter()
            .map(|card| Card {
                name: card.name.clone(),
                address: card.address,
                profiles: card.profiles.clone(),
                active: card.active.clone(),
            })
            .collect();
        Some(Sample {
            graph: Graph { default_sink: picture.default_sink.clone(), sinks, streams, cards },
            outputs: picture
                .sinks
                .iter()
                .map(|sink| Output {
                    name: sink.name.clone(),
                    index: sink.index,
                    address: sink.address,
                    profile: sink.profile.clone(),
                    state: sink.state,
                    muted: sink.level.mute,
                    audible_volume: sink.level.volume_percent.iter().any(|v| *v > 0.0),
                })
                .collect(),
            cache,
            levels: picture
                .sinks
                .iter()
                .map(|sink| (sink.name.clone(), sink.level.clone()))
                .collect(),
        })
    }
}

pub fn spawn(reports: Sender<Input>) -> Option<Handle> {
    let (jobs_tx, jobs_rx) = mpsc::channel();
    let (wake, thread_wake) = Wake::pair().ok()?;
    let worker = std::thread::Builder::new()
        .name("fleetpods-audio".into())
        .spawn(move || run(jobs_rx, reports, thread_wake))
        .ok()?;
    Some(Handle { jobs: jobs_tx, wake, thread: Some(worker) })
}

fn run(jobs: Receiver<Job>, reports: Sender<Input>, wake: Wake) {
    let Some(mut mainloop) = Mainloop::new() else {
        eprintln!("fleetpodsd: PipeWire mainloop could not start");
        return;
    };
    let wake_fd = wake.raw() as usize;
    mainloop.set_poll_func((poll_with_wake, wake_fd as *mut c_void));
    let Some(mut pulse) = open(&mainloop) else {
        eprintln!("fleetpodsd: PipeWire is not available");
        return;
    };
    open_pw_cli();
    let dirty = Arc::new(AtomicBool::new(true));
    arm_subscribe(&mut pulse.context, &dirty);
    let mut subscribed = false;
    let mut server_up = true;
    let mut cached: HashMap<u32, StreamIdentity> = HashMap::new();
    let mut soft_mutes = TemporaryMutes::default();
    let mut last: Option<Graph> = None;
    let mut last_outputs: Vec<Output> = Vec::new();
    let mut refreshed = std::time::Instant::now();
    let mut media_trace = MediaTrace::default();
    let mut query: Option<Query> = None;
    let mut profile_op: Option<ProfileOp> = None;

    loop {
        wake.drain();
        if !drain(
            &jobs,
            &mut pulse.context,
            &cached,
            &reports,
            &mut profile_op,
            &mut soft_mutes,
            &dirty,
        ) {
            break;
        }
        match (pulse.context.get_state(), pulse.sinks.state()) {
            (State::Ready, libpulse_sys::pa_context_state_t::Ready) => {
                server_up = true;
                if !subscribed {
                    let _ = pulse.context.subscribe(
                        InterestMaskSet::SINK
                            | InterestMaskSet::SINK_INPUT
                            | InterestMaskSet::SERVER
                            | InterestMaskSet::CARD,
                        |_| {},
                    );
                    subscribed = true;
                    dirty.store(true, Ordering::Relaxed);
                }
            }
            (State::Failed | State::Terminated, _)
            | (
                _,
                libpulse_sys::pa_context_state_t::Failed
                | libpulse_sys::pa_context_state_t::Terminated,
            ) => {
                if server_up {
                    server_up = false;
                    let _ = reports.send(Input::AudioLost);
                }
                subscribed = false;
                soft_mutes.release_all();
                cached.clear();
                last_outputs.clear();
                last = None;
                media_trace = MediaTrace::default();
                query = None;
                profile_op = None;
                if let Some(next) = open(&mainloop) {
                    pulse = next;
                    arm_subscribe(&mut pulse.context, &dirty);
                    dirty.store(true, Ordering::Relaxed);
                } else {
                    let mut waiting =
                        [libc::pollfd { fd: wake.raw(), events: libc::POLLIN, revents: 0 }];
                    unsafe { libc::poll(waiting.as_mut_ptr(), 1, 1_000) };
                    continue;
                }
            }
            _ => {}
        }
        if query.as_ref().is_some_and(|query| query.ready()) {
            if let Some(mut sample) = query.take().and_then(Query::finish) {
                media_trace.observe(&sample);
                if last_outputs != sample.outputs {
                    for output in &sample.outputs {
                        if output.address.is_some() && !last_outputs.contains(output) {
                            eprintln!(
                                "fleetpodsd: audio output state sink={:?} raw_state={}",
                                output.name, output.state
                            );
                        }
                    }
                    last_outputs.clone_from(&sample.outputs);
                    let _ = reports.send(Input::Outputs(sample.outputs));
                }
                cached = sample.cache;
                soft_mutes.observe(&cached);
                soft_mutes.clean_up(&cached, soft_mutes.now(), set_soft_mute);
                for stream in &mut sample.graph.streams {
                    stream.temporarily_muted = soft_mutes.confirmed(stream);
                }
                if last.as_ref() != Some(&sample.graph) {
                    last = Some(sample.graph.clone());
                    let _ = reports.send(Input::Audio(sample.graph));
                }
            }
        }
        if refreshed.elapsed() >= std::time::Duration::from_secs(1) {
            refreshed = std::time::Instant::now();
            dirty.store(true, Ordering::Relaxed);
        }
        if query.is_none()
            && pulse.context.get_state() == State::Ready
            && pulse.sinks.state() == libpulse_sys::pa_context_state_t::Ready
            && dirty.swap(false, Ordering::Relaxed)
        {
            query = Some(Query::start(&pulse.context, pulse.sinks.raw));
        }
        if matches!(
            mainloop.iterate(true),
            libpulse_binding::mainloop::standard::IterateResult::Quit(_)
                | libpulse_binding::mainloop::standard::IterateResult::Err(_)
        ) {
            break;
        }
    }
    soft_mutes.release_all();
    // Normal shutdown gives failed releases a bounded chance to recover. Stop
    // the fast writer first so no buffered mute can arrive after cleanup.
    close_pw_cli();
    let until = std::time::Instant::now() + std::time::Duration::from_secs(7);
    while !soft_mutes.held.is_empty() && std::time::Instant::now() < until {
        soft_mutes.clean_up(&cached, soft_mutes.now(), soft_mute_once);
        if !soft_mutes.held.is_empty() {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
    if !soft_mutes.held.is_empty() {
        eprintln!(
            "fleetpodsd: {} temporary stream mutes could not be released before shutdown",
            soft_mutes.held.len()
        );
    }
}

extern "C" fn poll_with_wake(
    ufds: *mut libc::pollfd,
    nfds: c_ulong,
    timeout: i32,
    data: *mut c_void,
) -> i32 {
    let wake = data as usize as i32;
    let count = nfds as usize;
    let mut fds = Vec::with_capacity(count + 1);
    if !ufds.is_null() && count > 0 {
        fds.extend_from_slice(unsafe { std::slice::from_raw_parts(ufds, count) });
    }
    fds.push(libc::pollfd { fd: wake, events: libc::POLLIN, revents: 0 });
    // Refresh health once a second even if subscriptions miss a state change.
    let timeout = if timeout < 0 { 1_000 } else { timeout.min(1_000) };
    let result = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };
    if !ufds.is_null() && count > 0 {
        unsafe { std::ptr::copy_nonoverlapping(fds.as_ptr(), ufds, count) };
    }
    result
}

fn drain(
    jobs: &Receiver<Job>,
    context: &mut Context,
    cached: &HashMap<u32, StreamIdentity>,
    reports: &Sender<Input>,
    profile_op: &mut Option<ProfileOp>,
    soft_mutes: &mut TemporaryMutes,
    dirty: &AtomicBool,
) -> bool {
    loop {
        match jobs.try_recv() {
            Ok(Job::Stop) | Err(mpsc::TryRecvError::Disconnected) => return false,
            Ok(Job::Ops(ops)) => {
                apply(context, cached, reports, profile_op, soft_mutes, &ops);
                dirty.store(true, Ordering::Relaxed);
            }
            Err(mpsc::TryRecvError::Empty) => return true,
        }
    }
}

fn apply(
    context: &mut Context,
    cached: &HashMap<u32, StreamIdentity>,
    reports: &Sender<Input>,
    profile_op: &mut Option<ProfileOp>,
    soft_mutes: &mut TemporaryMutes,
    ops: &[Op],
) {
    // Cleanup intent survives a lost Pulse connection and a one-shot release
    // from a cancelled route. It never writes the user's persistent mute/gain.
    if context.get_state() != State::Ready {
        for op in ops {
            if let Op::Restore(snap) = op {
                soft_mutes.release(StreamTarget { id: snap.id, generation: snap.generation });
            }
        }
        return;
    }
    for op in ops {
        if let Op::Default(name) = op {
            let _ = context.set_default_sink(name, |_| {});
        }
    }
    let mut intro = context.introspect();
    for op in ops {
        match op {
            Op::StartOutput { id, name } => start_output_node(*id, name.clone(), reports.clone()),
            Op::Default(_) => {}
            Op::Mute(stream) => {
                if stream.generation != 0 && generation_ok(cached, stream) {
                    if let Some(object_id) = cached.get(&stream.id).and_then(|s| s.object_id) {
                        soft_mutes.hold(*stream, object_id, set_soft_mute);
                    }
                }
            }
            Op::Move { sink, stream } => {
                if generation_ok(cached, stream) {
                    let _ = intro.move_sink_input_by_name(stream.id, sink, None);
                }
            }
            Op::Suspend(name) => {
                // Applied in order: the second call only allows a later
                // stream to start the output again.
                let _ = intro.suspend_sink_by_name(name, true, None);
                let _ = intro.suspend_sink_by_name(name, false, None);
            }
            Op::DefaultSource(name) => {
                let shown = name.clone();
                let _ = context.set_default_source(name, move |ok| {
                    if !ok {
                        eprintln!("fleetpodsd: default source {shown} was not selected");
                    }
                });
            }
            Op::Profile { id, card, name } => {
                let id = *id;
                let reports = reports.clone();
                let card_name = card.clone();
                *profile_op = Some(intro.set_card_profile_by_name(
                    card,
                    name,
                    Some(Box::new(move |ok| {
                        let _ = reports.send(Input::ProfileResult { id, ok });
                        if !ok {
                            eprintln!("fleetpodsd: card profile on {card_name} was not selected");
                        }
                    })),
                ));
            }
            Op::Refresh { card, name, restart } => {
                // The server applies these in order, so the second call
                // brings the card back with fresh nodes.
                let _ = intro.set_card_profile_by_name(card, "off", None);
                let reports = reports.clone();
                let card_name = card.clone();
                let restart = *restart;
                let _ = intro.set_card_profile_by_name(
                    card,
                    name,
                    Some(Box::new(move |ok| {
                        if !ok {
                            eprintln!("fleetpodsd: audio output refresh on {card_name} failed");
                        }
                        let _ = reports.send(Input::OutputRestarted { id: restart, ok });
                    })),
                );
            }
            Op::Restore(snap) => {
                soft_mutes.release(StreamTarget { id: snap.id, generation: snap.generation });
                soft_mutes.clean_up(cached, soft_mutes.now(), set_soft_mute);
            }
        }
    }
}

/// A Pulse mute is saved as the application's future mute setting. PipeWire's
/// softMute only silences this node. Pulse doesn't report it, so the worker
/// overlays confirmed holds on its graph. Possible mutes remain owned even
/// when confirmation fails; release retries belong to this same worker.
///
/// A takeover mutes a stream that may already be playing on the speakers.
/// The command first goes to a pw-cli kept open, which applies it within a
/// few milliseconds; starting pw-cli takes 10-15 ms. That pw-cli reports
/// nothing back, so a one-off pw-cli then applies it again and confirms it.
fn set_soft_mute(object_id: u32, muted: bool) -> bool {
    let line = format!("set-param {object_id} Props {{ softMute: {muted} }}\n");
    send_pw_cli(&line);
    soft_mute_once(object_id, muted)
}

fn soft_mute_once(object_id: u32, muted: bool) -> bool {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    let value = if muted { "{ softMute: true }" } else { "{ softMute: false }" };
    eprintln!("fleetpodsd: media command source=fleetpods node={object_id} soft_mute={muted} sent");
    let Ok(mut child) = Command::new("pw-cli")
        .args(["set-param", &object_id.to_string(), "Props", value])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        eprintln!("fleetpodsd: media command node={object_id} soft_mute={muted} spawn_failed");
        return false;
    };
    let deadline = Instant::now() + Duration::from_millis(500);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                eprintln!(
                    "fleetpodsd: media command node={object_id} soft_mute={muted} success={}",
                    status.success()
                );
                return status.success();
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                eprintln!("fleetpodsd: temporary stream mute failed for node {object_id}");
                return false;
            }
        }
    }
}

struct PwCli {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
}

static PW_CLI: Mutex<Option<PwCli>> = Mutex::new(None);

fn spawn_pw_cli() -> Option<PwCli> {
    use std::process::{Command, Stdio};
    let mut child = Command::new("pw-cli")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdin = child.stdin.take()?;
    Some(PwCli { child, stdin })
}

/// Write one command to the open pw-cli. One that has exited is replaced for
/// the next command: a pw-cli just started drops commands until it has read
/// the registry.
fn send_pw_cli(line: &str) {
    use std::io::Write;
    let Ok(mut open) = PW_CLI.lock() else {
        return;
    };
    if let Some(cli) = open.as_mut() {
        if matches!(cli.child.try_wait(), Ok(None))
            && cli.stdin.write_all(line.as_bytes()).and_then(|()| cli.stdin.flush()).is_ok()
        {
            return;
        }
    }
    if let Some(mut stale) = open.take() {
        let _ = stale.child.kill();
        let _ = stale.child.wait();
    }
    *open = spawn_pw_cli();
}

/// Start pw-cli ahead of the first takeover so it is ready by then.
fn open_pw_cli() {
    if let Ok(mut open) = PW_CLI.lock() {
        if open.is_none() {
            *open = spawn_pw_cli();
        }
    }
}

/// End the open pw-cli. It does not exit at the end of its input.
fn close_pw_cli() {
    let Some(PwCli { mut child, stdin }) = PW_CLI.lock().ok().and_then(|mut open| open.take())
    else {
        return;
    };
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
}

fn generation_ok(cached: &HashMap<u32, StreamIdentity>, stream: &StreamTarget) -> bool {
    cached.get(&stream.id).is_some_and(|saved| saved.generation == stream.generation)
}

/// Pulse can resume a sink while its PipeWire node still waits for an error
/// retry. Send the node command directly, as OmniPods does after relinking.
/// Keep the bounded subprocess off both event loops; no shell interprets names.
fn start_output_node(id: u64, name: String, reports: Sender<Input>) {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    let failed = reports.clone();
    let result =
        std::thread::Builder::new().name("fleetpods-output-start".into()).spawn(move || {
            let child = Command::new("pw-cli")
                .args(["send-command", &name, "Start", "{}"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
            let Ok(mut child) = child else {
                eprintln!("fleetpodsd: could not start PipeWire output command for {name}");
                let _ = reports.send(Input::OutputStarted { id, ok: false });
                return;
            };
            let deadline = Instant::now() + Duration::from_secs(1);
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        eprintln!(
                            "fleetpodsd: PipeWire output Start completed (ok={})",
                            status.success()
                        );
                        let _ = reports.send(Input::OutputStarted { id, ok: status.success() });
                        return;
                    }
                    Ok(None) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    _ => {
                        let _ = child.kill();
                        let _ = child.wait();
                        eprintln!("fleetpodsd: PipeWire output Start failed or timed out");
                        let _ = reports.send(Input::OutputStarted { id, ok: false });
                        return;
                    }
                }
            }
        });
    if result.is_err() {
        eprintln!("fleetpodsd: could not launch PipeWire output worker");
        let _ = failed.send(Input::OutputStarted { id, ok: false });
    }
}

struct Pulse {
    context: Context,
    sinks: SinkContext,
}

// Sink inspection uses the C API to tolerate PipeWire's transient sink states.
// Own that connection directly; the Rust binding's private layout is not an API.
// Both contexts use the worker's mainloop and are dropped before that mainloop.
struct SinkContext {
    raw: *mut libpulse_sys::pa_context,
}

impl SinkContext {
    fn new(mainloop: &Mainloop) -> Option<Self> {
        let raw = unsafe {
            libpulse_sys::pa_context_new(mainloop.get_api().as_ref(), c"FleetPods sinks".as_ptr())
        };
        if raw.is_null() {
            return None;
        }
        Some(Self { raw })
    }

    fn connect(&mut self) -> bool {
        unsafe {
            libpulse_sys::pa_context_connect(
                self.raw,
                std::ptr::null(),
                libpulse_sys::PA_CONTEXT_NOFLAGS,
                std::ptr::null(),
            ) >= 0
        }
    }

    fn state(&self) -> libpulse_sys::pa_context_state_t {
        unsafe { libpulse_sys::pa_context_get_state(self.raw) }
    }
}

impl Drop for SinkContext {
    fn drop(&mut self) {
        unsafe {
            libpulse_sys::pa_context_disconnect(self.raw);
            libpulse_sys::pa_context_unref(self.raw);
        }
    }
}

fn open(mainloop: &Mainloop) -> Option<Pulse> {
    let mut context = Context::new(mainloop, "FleetPods")?;
    let mut sinks = SinkContext::new(mainloop)?;
    if !sinks.connect() {
        return None;
    }
    context.connect(None, ContextFlagSet::NOFLAGS, None).ok()?;
    Some(Pulse { context, sinks })
}

fn arm_subscribe(context: &mut Context, dirty: &Arc<AtomicBool>) {
    let flag = Arc::clone(dirty);
    context.set_subscribe_callback(Some(Box::new(move |_, _, _| {
        flag.store(true, Ordering::Relaxed);
    })));
}

impl SinkList {
    fn start(raw: *mut libpulse_sys::pa_context, picture: &Arc<Mutex<Picture>>) -> Option<Self> {
        let done = Arc::new(AtomicBool::new(false));
        let watch = Box::into_raw(Box::new(SinkWatch {
            picture: Arc::clone(picture),
            done: Arc::clone(&done),
        }));
        let op = unsafe {
            libpulse_sys::pa_context_get_sink_info_list(raw, Some(on_sink), watch.cast::<c_void>())
        };
        if op.is_null() {
            drop(unsafe { Box::from_raw(watch) });
            if let Ok(mut picture) = picture.lock() {
                picture.sinks_done = true;
            }
            return None;
        }
        Some(Self { op, userdata: watch, done })
    }
}

extern "C" fn on_sink(
    _: *mut libpulse_sys::pa_context,
    info: *const libpulse_sys::pa_sink_info,
    eol: i32,
    userdata: *mut c_void,
) {
    let watch = unsafe { &*userdata.cast::<SinkWatch>() };
    if eol != 0 {
        if let Ok(mut picture) = watch.picture.lock() {
            picture.sinks_done = true;
        }
        watch.done.store(true, Ordering::Release);
        drop(unsafe { Box::from_raw(userdata.cast::<SinkWatch>()) });
        return;
    }
    if let Some(sink) = read_sink(info) {
        if let Ok(mut picture) = watch.picture.lock() {
            picture.sinks.push(sink);
        }
    }
}

fn read_sink(info: *const libpulse_sys::pa_sink_info) -> Option<RawSink> {
    if info.is_null() {
        return None;
    }
    // Read the ABI bytes as an integer, never construct a Rust SinkState enum:
    // PipeWire's -2 and -3 values are not valid enum variants.
    let state = unsafe {
        info.cast::<u8>()
            .add(std::mem::offset_of!(libpulse_sys::pa_sink_info, state))
            .cast::<i32>()
            .read_unaligned()
    };
    let name = unsafe { (*info).name };
    if name.is_null() {
        return None;
    }
    let name = unsafe { CStr::from_ptr(name) }.to_string_lossy().into_owned();
    let index = unsafe { (*info).index };
    let props = unsafe { (*info).proplist };
    let address = prop(props, c"api.bluez5.address")
        .or_else(|| prop(props, c"device.string"))
        .as_deref()
        .and_then(Address::parse);
    let profile = prop(props, c"api.bluez5.profile");
    let volume = unsafe { (*info).volume };
    let level = SinkLevel {
        mute: unsafe { (*info).mute } != 0,
        volume_percent: volume.values[..usize::from(volume.channels).min(volume.values.len())]
            .iter()
            .map(|value| f64::from(*value) * 100.0 / f64::from(Volume::NORMAL.0))
            .collect(),
    };
    Some(RawSink { index, name, address, profile, state, level })
}

fn prop(list: *mut libpulse_sys::pa_proplist, key: &CStr) -> Option<String> {
    if list.is_null() {
        return None;
    }
    let value = unsafe { libpulse_sys::pa_proplist_gets(list, key.as_ptr()) };
    if value.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(value) }.to_str().ok().map(str::to_string)
}

fn raw_stream(info: &SinkInputInfo) -> RawStream {
    let role = info.proplist.get_str("media.role").unwrap_or_default();
    let node = info
        .proplist
        .get_str("application.name")
        .or_else(|| info.proplist.get_str("node.name"))
        .unwrap_or_default();
    let generation =
        info.proplist.get_str("object.serial").and_then(|text| text.parse().ok()).unwrap_or(0);
    RawStream {
        id: info.index,
        object_id: info.proplist.get_str("object.id").and_then(|text| text.parse().ok()),
        generation,
        sink_index: info.sink,
        role,
        node,
        mute: info.mute,
        volume: VolumeLinear::from(info.volume.avg()).0,
        corked: info.corked,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mute_target() -> StreamTarget {
        StreamTarget { id: 7, generation: 70 }
    }

    fn mute_inventory() -> HashMap<u32, StreamIdentity> {
        HashMap::from([(7, StreamIdentity { generation: 70, object_id: Some(17) })])
    }

    #[test]
    fn unconfirmed_mute_is_owned_and_a_one_shot_release_retries_until_success() {
        let mut mutes = TemporaryMutes::default();
        let cached = mute_inventory();
        let mut node_muted = false;
        mutes.hold(mute_target(), 17, |_, muted| {
            node_muted = muted; // The fast writer succeeded, but confirmation failed.
            false
        });
        assert!(node_muted);
        assert_eq!(mutes.held.len(), 1);
        assert!(!mutes.held[&70].confirmed);
        mutes.release(mute_target()); // The cancelled route never sends another request.
        let mut calls = 0;
        for now in [0, 100, 350, 1_350, 6_350, 11_350, 16_350] {
            mutes.clean_up(&cached, MonoMs(now), |node, muted| {
                assert_eq!(node, 17);
                assert!(!muted);
                calls += 1;
                false
            });
            assert_eq!(mutes.held.len(), 1, "cleanup failure cannot abandon our mute");
        }
        assert_eq!(calls, 7, "required cleanup does not exhaust after five failures");
        mutes.clean_up(&cached, MonoMs(16_351), |_, _| panic!("backoff must hold"));
        mutes.clean_up(&cached, MonoMs(21_350), |_, muted| {
            node_muted = muted;
            true
        });
        assert!(!node_muted);
        assert!(mutes.held.is_empty());
    }

    #[test]
    fn renewed_hold_cancels_release_without_repeated_release_resetting_backoff() {
        let mut mutes = TemporaryMutes::default();
        let cached = mute_inventory();
        mutes.hold(mute_target(), 17, |_, _| true);
        mutes.release(mute_target());
        mutes.clean_up(&cached, MonoMs(0), |_, _| false);
        mutes.release(mute_target());
        mutes.clean_up(&cached, MonoMs(99), |_, _| panic!("duplicate release reset backoff"));
        mutes.hold(mute_target(), 17, |_, muted| muted);
        mutes.clean_up(&cached, MonoMs(1_000), |_, _| panic!("new hold cancelled old cleanup"));
        mutes.release(mute_target());
        mutes.clean_up(&cached, MonoMs(1_000), |_, muted| !muted);
        assert!(mutes.held.is_empty());
    }

    #[test]
    fn cleanup_waits_for_inventory_and_never_unmutes_a_recycled_node() {
        for survives in [false, true] {
            let mut mutes = TemporaryMutes::default();
            mutes.hold(mute_target(), 17, |_, _| true);
            mutes.release_all();
            mutes.clean_up(&HashMap::new(), MonoMs(0), |_, _| panic!("no live inventory"));
            assert_eq!(mutes.held.len(), 1);
            let mut current = mute_inventory();
            if !survives {
                current.get_mut(&7).unwrap().generation = 71;
            }
            mutes.observe(&current);
            let mut releases = 0;
            mutes.clean_up(&current, MonoMs(1_000), |_, muted| {
                assert!(survives, "the replacement shares an ID, not mute ownership");
                assert!(!muted);
                releases += 1;
                true
            });
            assert_eq!(releases, usize::from(survives));
            assert!(mutes.held.is_empty());
        }
    }

    #[test]
    fn release_received_while_pulse_is_down_is_not_lost() {
        let mainloop = Mainloop::new().unwrap();
        let mut context = Context::new(&mainloop, "mute-test").unwrap();
        let (tx, _rx) = mpsc::channel();
        let mut mutes = TemporaryMutes::default();
        mutes.hold(mute_target(), 17, |_, _| true);
        apply(
            &mut context,
            &HashMap::new(),
            &tx,
            &mut None,
            &mut mutes,
            &[Op::Restore(StreamSnap { id: 7, generation: 70, mute: false, volume: 1.0 })],
        );
        let current = mute_inventory();
        mutes.observe(&current);
        mutes.clean_up(&current, MonoMs(0), |_, muted| !muted);
        assert!(mutes.held.is_empty());
    }

    #[test]
    fn a_changed_pulse_index_keeps_the_same_native_mute_owner() {
        let mut mutes = TemporaryMutes::default();
        mutes.hold(mute_target(), 17, |_, _| true);
        let current = HashMap::from([(8, StreamIdentity { generation: 70, object_id: Some(17) })]);
        mutes.observe(&current);
        mutes.release(StreamTarget { id: 8, generation: 70 });
        mutes.clean_up(&current, MonoMs(0), |node, muted| {
            assert_eq!(node, 17);
            !muted
        });
        assert!(mutes.held.is_empty());
    }

    #[test]
    fn unknown_sink_states_are_read_as_integers_without_constructing_an_enum() {
        let name = c"bluez_output.CC_22_FE_54_DF_C7.1";
        let mut info = std::mem::MaybeUninit::<libpulse_sys::pa_sink_info>::zeroed();
        let ptr = info.as_mut_ptr();
        unsafe {
            (*ptr).name = name.as_ptr();
            (*ptr).index = 7;
            (*ptr).mute = 1;
            (*ptr).volume.channels = 2;
            (*ptr).volume.values[0] = Volume::NORMAL.0 / 2;
            (*ptr).volume.values[1] = Volume::NORMAL.0;
            let state = (ptr as *mut u8)
                .add(std::mem::offset_of!(libpulse_sys::pa_sink_info, state))
                .cast::<i32>();
            assert_eq!(
                std::mem::size_of::<libpulse_sys::pa_sink_state_t>(),
                std::mem::size_of::<i32>()
            );
            state.write(-3);
            let sink = read_sink(ptr).expect("unlinked sink");
            assert_eq!(sink.state, -3);
            assert_eq!(sink.index, 7);
            assert_eq!(sink.name, "bluez_output.CC_22_FE_54_DF_C7.1");
            assert!(sink.address.is_none());
            assert_eq!(sink.level, SinkLevel { mute: true, volume_percent: vec![50.0, 100.0] });
            state.write(-2);
            let init = read_sink(ptr).expect("init sink");
            assert_eq!(init.name, sink.name);
            assert_eq!(init.state, -2);
            state.write(99);
            assert_eq!(read_sink(ptr).unwrap().state, 99);
        }
    }

    #[test]
    fn sink_contexts_can_be_created_and_dropped_without_a_server() {
        let mainloop = Mainloop::new().expect("mainloop");
        for _ in 0..100 {
            let sinks = SinkContext::new(&mainloop).expect("sink context");
            assert_eq!(sinks.state(), libpulse_sys::pa_context_state_t::Unconnected);
        }
    }

    #[test]
    fn a_gain_above_unity_survives_the_pulse_volume_curve() {
        let volume = Volume::from(VolumeLinear(1.4));
        let back = VolumeLinear::from(volume).0;
        assert!((back - 1.4).abs() < 0.02);
    }
}
