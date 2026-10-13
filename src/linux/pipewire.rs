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
use libpulse_binding::volume::{ChannelVolumes, Volume, VolumeLinear};

use crate::audio::play::StreamSnap;
use crate::audio::route::{Card, Graph, LiveStream, Sink, StreamTarget};
use crate::linux::wake::Wake;
use crate::step::Input;
use crate::types::Address;

pub enum Op {
    StartOutput(String),
    Mute(StreamTarget),
    Default(String),
    Move {
        sink: String,
        stream: StreamTarget,
    },
    Restore(StreamSnap),
    /// Make this capture node the default source.
    DefaultSource(String),
    /// Select this codec profile on the card the audio server named.
    Profile {
        card: String,
        name: String,
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

struct StreamChannels {
    generation: u64,
    object_id: Option<u32>,
    channels: u8,
}

struct Sample {
    graph: Graph,
    cache: HashMap<u32, StreamChannels>,
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
    channels: u8,
}

struct RawSink {
    index: u32,
    name: String,
    address: Option<Address>,
    profile: Option<String>,
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
/// This list reads the name, index, and proplist through the C API and does not load
/// the state field.
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
                ListResult::End | ListResult::Error => picture.streams_done = true,
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
        if !picture.ready() {
            return None;
        }
        let names: HashMap<u32, String> =
            picture.sinks.iter().map(|sink| (sink.index, sink.name.clone())).collect();
        let mut cache = HashMap::new();
        let streams = picture
            .streams
            .iter()
            .filter_map(|stream| {
                let sink = names.get(&stream.sink_index)?.clone();
                cache.insert(
                    stream.id,
                    StreamChannels {
                        generation: stream.generation,
                        channels: stream.channels,
                        object_id: stream.object_id,
                    },
                );
                Some(LiveStream {
                    id: stream.id,
                    generation: stream.generation,
                    sink,
                    role: stream.role.clone(),
                    node: stream.node.clone(),
                    mute: stream.mute,
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
    let dirty = Arc::new(AtomicBool::new(true));
    arm_subscribe(&mut pulse.context, &dirty);
    let mut subscribed = false;
    let mut server_up = true;
    let mut cached: HashMap<u32, StreamChannels> = HashMap::new();
    let mut soft_mutes = HashMap::new();
    let mut last: Option<Graph> = None;
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
                soft_mutes.clear();
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
                cached = sample.cache;
                for stream in &mut sample.graph.streams {
                    stream.mute |= soft_mutes.contains_key(&(stream.id, stream.generation));
                }
                if last.as_ref() != Some(&sample.graph) {
                    last = Some(sample.graph.clone());
                    let _ = reports.send(Input::Audio(sample.graph));
                }
            }
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
    for ((id, generation), object_id) in soft_mutes {
        if generation_ok(&cached, &StreamTarget { id, generation }) {
            set_soft_mute(object_id, false);
        }
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
    let result = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };
    if !ufds.is_null() && count > 0 {
        unsafe { std::ptr::copy_nonoverlapping(fds.as_ptr(), ufds, count) };
    }
    result
}

fn drain(
    jobs: &Receiver<Job>,
    context: &mut Context,
    cached: &HashMap<u32, StreamChannels>,
    reports: &Sender<Input>,
    profile_op: &mut Option<ProfileOp>,
    soft_mutes: &mut HashMap<(u32, u64), u32>,
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
    cached: &HashMap<u32, StreamChannels>,
    reports: &Sender<Input>,
    profile_op: &mut Option<ProfileOp>,
    soft_mutes: &mut HashMap<(u32, u64), u32>,
    ops: &[Op],
) {
    if context.get_state() != State::Ready {
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
            Op::StartOutput(name) => start_output_node(name.clone()),
            Op::Default(_) => {}
            Op::Mute(stream) => {
                if generation_ok(cached, stream) {
                    if let Some(object_id) = cached.get(&stream.id).and_then(|s| s.object_id) {
                        if set_soft_mute(object_id, true) {
                            soft_mutes.insert((stream.id, stream.generation), object_id);
                        }
                    }
                }
            }
            Op::Move { sink, stream } => {
                if generation_ok(cached, stream) {
                    let _ = intro.move_sink_input_by_name(stream.id, sink, None);
                }
            }
            Op::DefaultSource(name) => {
                let shown = name.clone();
                let _ = context.set_default_source(name, move |ok| {
                    if !ok {
                        eprintln!("fleetpodsd: default source {shown} was not selected");
                    }
                });
            }
            Op::Profile { card, name } => {
                let reports = reports.clone();
                let card_name = card.clone();
                *profile_op = Some(intro.set_card_profile_by_name(
                    card,
                    name,
                    Some(Box::new(move |ok| {
                        let _ = reports.send(Input::ProfileResult { ok });
                        if !ok {
                            eprintln!("fleetpodsd: card profile on {card_name} was not selected");
                        }
                    })),
                ));
            }
            Op::Restore(snap) => {
                let Some(saved) = cached.get(&snap.id) else {
                    continue;
                };
                if saved.generation != snap.generation {
                    continue;
                }
                let key = (snap.id, snap.generation);
                if let Some(object_id) = soft_mutes.get(&key).copied() {
                    if !set_soft_mute(object_id, false) {
                        continue;
                    }
                    soft_mutes.remove(&key);
                }
                let channels = if saved.channels == 0 { 2 } else { saved.channels };
                eprintln!("fleetpodsd: media command source=fleetpods restore id={} generation={} mute={} linear_gain={} channels={channels} sent", snap.id, snap.generation, snap.mute, snap.volume);
                let mut volumes = ChannelVolumes::default();
                volumes.set(channels, Volume::from(VolumeLinear(snap.volume)));
                let _ = intro.set_sink_input_mute(snap.id, snap.mute, None);
                let _ = intro.set_sink_input_volume(snap.id, &volumes, None);
            }
        }
    }
}

/// A Pulse mute is saved as the application's future mute setting. PipeWire's
/// softMute only silences this node. Pulse doesn't report it, so the worker
/// overlays confirmed holds on its graph and clears them before shutting down.
fn set_soft_mute(object_id: u32, muted: bool) -> bool {
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

fn generation_ok(cached: &HashMap<u32, StreamChannels>, stream: &StreamTarget) -> bool {
    cached.get(&stream.id).is_some_and(|saved| saved.generation == stream.generation)
}

/// Pulse can resume a sink while its PipeWire node still waits for an error
/// retry. Send the node command directly, as OmniPods does after relinking.
/// Keep the bounded subprocess off both event loops; no shell interprets names.
fn start_output_node(name: String) {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
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
                        return;
                    }
                    Ok(None) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    _ => {
                        let _ = child.kill();
                        let _ = child.wait();
                        eprintln!("fleetpodsd: PipeWire output Start failed or timed out");
                        return;
                    }
                }
            }
        });
    if result.is_err() {
        eprintln!("fleetpodsd: could not launch PipeWire output worker");
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
    // Field reads only. `state` is not loaded: -2 and -3 are not `SinkState` variants.
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
    Some(RawSink { index, name, address, profile, level })
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
        channels: info.volume.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unlinked_sink_is_recorded_without_reading_its_state() {
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
            assert_eq!(sink.index, 7);
            assert_eq!(sink.name, "bluez_output.CC_22_FE_54_DF_C7.1");
            assert!(sink.address.is_none());
            assert_eq!(sink.level, SinkLevel { mute: true, volume_percent: vec![50.0, 100.0] });
            state.write(-2);
            assert_eq!(read_sink(ptr).expect("init sink").name, sink.name);
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
