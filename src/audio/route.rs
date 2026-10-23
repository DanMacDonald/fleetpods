//! Which output to select once this machine has claimed playback.
//! The audio server reports the graph. This module decides. It does not
//! open PipeWire.

use std::collections::{BTreeMap, BTreeSet};

use crate::audio::play::StreamSnap;
use crate::types::{Address, MonoMs};

/// A playback sink the audio server currently publishes.
/// `profile` is the server's own label, such as `a2dp-sink`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sink {
    pub name: String,
    pub address: Option<Address>,
    pub profile: Option<String>,
}

/// One playback stream. `generation` is the server's object serial, so a
/// recycled `id` is not the same stream.
#[derive(Clone, Debug, PartialEq)]
pub struct LiveStream {
    pub id: u32,
    pub generation: u64,
    pub sink: String,
    pub role: String,
    /// PipeWire node or application name. The speaker-tuning output is not a playback stream.
    pub node: String,
    pub mute: bool,
    pub volume: f64,
    pub corked: bool,
}

/// A BlueZ card the audio server reported, with the profile names it offered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Card {
    pub name: String,
    pub address: Option<Address>,
    pub profiles: Vec<String>,
    pub active: Option<String>,
}

/// The audio server's picture of outputs and the streams on them.
#[derive(Clone, Debug, PartialEq)]
pub struct Graph {
    pub default_sink: String,
    pub sinks: Vec<Sink>,
    pub streams: Vec<LiveStream>,
    pub cards: Vec<Card>,
}

/// A stream index plus the serial that makes the index meaningful.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamTarget {
    pub id: u32,
    pub generation: u64,
}

/// Remembered across graphs. Retries use the daemon clock.
#[derive(Clone, Debug, PartialEq)]
struct RouteState {
    want: bool,
    from: Option<String>,
    /// Original mute and volume of streams this claim muted.
    held: Vec<StreamSnap>,
    quieted: BTreeSet<(u32, u64)>,
    quiet_at: Option<MonoMs>,
    asked_default: Option<MonoMs>,
    asked_move: Option<MonoMs>,
    asked_restore: Option<MonoMs>,
    restore_sent: BTreeSet<(u32, u64)>,
    takeover_started: Option<MonoMs>,
    missing_since: BTreeMap<(u32, u64), MonoMs>,
    recovery: Option<Recovery>,
    start_after_relink: Option<Restart>,
    relinked_at: Option<MonoMs>,
    /// Released with the pods still connected: move streams off them too.
    returning: bool,
    asked_return: Option<MonoMs>,
}

#[derive(Clone, Debug, PartialEq)]
struct Recovery {
    id: u64,
    sink: String,
    via: String,
    started: MonoMs,
    resetting: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Restart {
    pub id: u64,
    pub name: String,
}

impl RouteState {
    fn retire_missing(&mut self, graph: &Graph, now: MonoMs) {
        self.held.retain(|saved| {
            let key = (saved.id, saved.generation);
            if find_held(graph, saved).is_some() {
                self.missing_since.remove(&key);
                return true;
            }
            let since = *self.missing_since.entry(key).or_insert(now);
            now.saturating_since(since) < RETRY_MS
        });
        self.missing_since.retain(|key, _| self.held.iter().any(|s| (s.id, s.generation) == *key));
        self.quieted.retain(|key| self.held.iter().any(|s| (s.id, s.generation) == *key));
        self.restore_sent.retain(|key| self.held.iter().any(|s| (s.id, s.generation) == *key));
    }

    /// Streams are parked while the accessory output is rebuilt.
    pub fn recovering(&self) -> bool {
        self.recovery.is_some()
    }

    pub fn holding_takeover(&self) -> bool {
        self.takeover_started.is_some()
    }

    /// The audio server restarted. Stream serials from it are no longer the same streams.
    pub fn forget_server(&mut self) {
        self.recovery = None;
        self.start_after_relink = None;
        self.relinked_at = None;
        self.held.clear();
        self.missing_since.clear();
        self.quieted.clear();
        self.quiet_at = None;
        self.asked_default = None;
        self.asked_move = None;
        self.asked_restore = None;
        self.restore_sent.clear();
        self.takeover_started = None;
        self.from = None;
    }

    pub fn new() -> Self {
        Self {
            want: false,
            from: None,
            held: Vec::new(),
            quieted: BTreeSet::new(),
            quiet_at: None,
            asked_default: None,
            asked_move: None,
            asked_restore: None,
            restore_sent: BTreeSet::new(),
            takeover_started: None,
            missing_since: BTreeMap::new(),
            recovery: None,
            start_after_relink: None,
            relinked_at: None,
            returning: false,
            asked_return: None,
        }
    }

    /// Detach held streams before resetting the sink, then let normal routing
    /// relink them and restore their original volume and mute state.
    pub fn begin_recovery(
        &mut self,
        id: u64,
        accessory: Address,
        graph: &Graph,
        now: MonoMs,
    ) -> bool {
        if self.recovery.is_some() || !self.want {
            return false;
        }
        let Some(sink) = sink_for(graph, accessory) else {
            return false;
        };
        let via = self
            .from
            .as_ref()
            .and_then(|from| graph.sinks.iter().find(|s| &s.name == from && s.name != sink.name))
            .or_else(|| graph.sinks.iter().find(|s| s.name == "omarchy_speaker_tuning"))
            .or_else(|| {
                graph.sinks.iter().find(|s| s.address.is_none() && follows_seat(&s.name, None))
            })
            // Built-in outputs are not always named "speaker" (an analog-stereo
            // sink, for instance). Any local output other than a display will do
            // to park muted streams briefly.
            .or_else(|| {
                graph.sinks.iter().find(|s| {
                    let name = s.name.to_ascii_lowercase();
                    s.address.is_none()
                        && s.name != sink.name
                        && !name.contains("hdmi")
                        && !name.contains("displayport")
                })
            });
        let Some(via) = via else {
            eprintln!("fleetpodsd: no local output to park streams on; cannot re-arm AirPods");
            return false;
        };
        self.from = Some(via.name.clone());
        for stream in graph.streams.iter().filter(|stream| {
            stream.sink == sink.name || (stream.sink == via.name && managed(stream))
        }) {
            if !self.held.iter().any(|s| s.id == stream.id && s.generation == stream.generation) {
                self.held.push(snap(stream));
            }
            self.quieted.insert((stream.id, stream.generation));
        }
        self.quiet_at = None;
        self.asked_move = None;
        self.asked_restore = None;
        self.restore_sent.clear();
        self.start_after_relink = None;
        self.relinked_at = None;
        self.recovery = Some(Recovery {
            id,
            sink: sink.name.clone(),
            via: via.name.clone(),
            started: now,
            resetting: false,
        });
        true
    }

    /// Take over streams muted elsewhere as already quieted, with their
    /// original state, so routing restores them instead of saving them muted.
    pub fn adopt_quieted(&mut self, streams: Vec<StreamSnap>) {
        for saved in streams {
            if self.quieted.insert((saved.id, saved.generation)) {
                self.held.push(saved);
            }
        }
    }

    pub fn finish_recovery(&mut self, id: u64, ok: bool) -> bool {
        if !self.recovery.as_ref().is_some_and(|r| r.id == id && r.resetting) {
            return false;
        }
        let recovery = self.recovery.take().unwrap();
        self.start_after_relink = ok.then_some(Restart { id, name: recovery.sink });
        self.relinked_at = None;
        self.asked_move = None;
        if !ok {
            self.want = false;
        }
        true
    }
}

impl Default for RouteState {
    fn default() -> Self {
        Self::new()
    }
}

/// Move these streams onto `sink`. Each entry carries its serial.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Move {
    pub sink: String,
    pub streams: Vec<StreamTarget>,
}

#[derive(Clone, Debug, PartialEq)]
struct RouteAction {
    pub routed: bool,
    pub quiet: Vec<StreamTarget>,
    pub set_default: Option<String>,
    pub move_streams: Option<Move>,
    pub restore: Vec<StreamSnap>,
    pub state: RouteState,
    pub restart_output: Option<Restart>,
    pub start_output: Option<Restart>,
    pub failure: Option<&'static str>,
}

/// One ordered piece of work from planning a graph. The Router emits
/// these in execution order: quiet, eligible restores, restart/start,
/// default sink, then moves. The daemon places quiet ahead of player pauses.
#[derive(Clone, Debug, PartialEq)]
pub enum Work {
    Quiet(Vec<StreamTarget>),
    Restart(Restart),
    Start(Restart),
    SetDefault(String),
    Move(Move),
    Restore(Vec<StreamSnap>),
}

/// What planning a graph decided, apart from the retained work.
#[derive(Clone, Debug, PartialEq)]
pub struct Outcome {
    pub routed: bool,
    pub failure: Option<&'static str>,
}

/// The routing transaction. It owns the route state and the work that
/// planning produced but the platform has not run yet. A graph goes in;
/// ordered work comes out through [`Router::take_work`].
#[derive(Clone, Debug, PartialEq)]
pub struct Router {
    state: RouteState,
    pending: Vec<Work>,
    restores: Vec<StreamSnap>,
    handoff: Option<Handoff>,
    handoff_quiet: Vec<StreamTarget>,
}

#[derive(Clone, Debug, PartialEq)]
struct Handoff {
    streams: Vec<StreamSnap>,
    muted_at: MonoMs,
    quiet_since: Option<MonoMs>,
}

impl Router {
    pub fn new() -> Self {
        Self {
            state: RouteState::new(),
            pending: Vec::new(),
            restores: Vec::new(),
            handoff: None,
            handoff_quiet: Vec::new(),
        }
    }

    pub fn claim(&mut self) {
        self.state.want = true;
        self.state.returning = false;
        // Transfer originals before the next graph can snapshot a muted stream.
        if let Some(handoff) = self.handoff.take() {
            self.state.adopt_quieted(handoff.streams);
        }
    }
    pub fn release(&mut self) {
        self.state.want = false;
    }
    /// Give playback back to the speakers while the pods stay connected, as
    /// when both are docked in an open case. A plain release only restores the
    /// default; streams this daemon moved keep the pods as their target.
    pub fn return_to_speakers(&mut self) {
        self.state.want = false;
        self.state.returning = true;
    }
    pub fn wants_output(&self) -> bool {
        self.state.want
    }
    pub fn source_sink(&self) -> Option<&str> {
        self.state.from.as_deref()
    }
    pub fn held_streams(&self) -> &[StreamSnap] {
        &self.state.held
    }
    pub fn recovering(&self) -> bool {
        self.state.recovering()
    }
    pub fn holding_takeover(&self) -> bool {
        self.state.holding_takeover()
    }
    pub fn needs_graph(&self) -> bool {
        self.state.want
            || self.state.returning
            || self.state.from.is_some()
            || !self.state.held.is_empty()
    }

    /// Saved states from an acquisition that muted streams before routing.
    pub fn hold_streams(&mut self, streams: &[StreamSnap]) {
        self.state.adopt_quieted(streams.to_vec());
    }

    pub fn fail_acquisition(&mut self) {
        self.restores.extend(std::mem::take(&mut self.state.held));
        self.state.quieted.clear();
    }

    /// Forget every stream serial and queued effect from the dead server.
    /// The playback claim survives; old restores must not touch its replacement.
    pub fn forget_server(&mut self) {
        self.state.forget_server();
        self.pending.clear();
        self.restores.clear();
        self.handoff = None;
        self.handoff_quiet.clear();
    }

    pub fn begin_recovery(
        &mut self,
        id: u64,
        accessory: Address,
        graph: &Graph,
        now: MonoMs,
    ) -> bool {
        if !self.state.begin_recovery(id, accessory, graph, now) {
            return false;
        }
        // A graph earlier in this step may already have queued an unmute.
        // Recovery owns those originals until the streams are safely relinked.
        self.restores.retain(|saved| {
            !self
                .state
                .held
                .iter()
                .any(|held| held.id == saved.id && held.generation == saved.generation)
        });
        true
    }

    pub fn finish_recovery(&mut self, id: u64, ok: bool) -> bool {
        self.state.finish_recovery(id, ok)
    }

    /// Yield audio without leaking a player's queued tail onto the speakers.
    /// Capture originals before pause/default-switch effects can execute.
    pub fn handoff(&mut self, accessory: Option<Address>, graph: Option<&Graph>, now: MonoMs) {
        // Already-held originals take precedence over a graph that observes
        // our mute. Saving that graph first would restore the stream muted.
        self.restores.extend(std::mem::take(&mut self.state.held));
        self.defer_restores(now);
        self.state.quieted.clear();
        self.state.restore_sent.clear();
        if let (Some(accessory), Some(graph)) = (accessory, graph) {
            for saved in audible_on_accessory(graph, accessory) {
                let handoff = self.handoff.get_or_insert_with(|| Handoff {
                    streams: Vec::new(),
                    muted_at: now,
                    quiet_since: None,
                });
                if !handoff
                    .streams
                    .iter()
                    .any(|s| s.id == saved.id && s.generation == saved.generation)
                {
                    handoff.streams.push(saved);
                }
                let target = StreamTarget { id: saved.id, generation: saved.generation };
                if !self.handoff_quiet.contains(&target) {
                    self.handoff_quiet.push(target);
                }
            }
        }
        if let Some(handoff) = &mut self.handoff {
            handoff.quiet_since = None;
        }
        self.pending.clear();
        self.release();
    }

    fn defer_restores(&mut self, now: MonoMs) {
        for saved in std::mem::take(&mut self.restores) {
            let handoff = self.handoff.get_or_insert_with(|| Handoff {
                streams: Vec::new(),
                muted_at: now,
                quiet_since: None,
            });
            if !handoff.streams.iter().any(|s| s.id == saved.id && s.generation == saved.generation)
            {
                handoff.streams.push(saved);
            }
        }
    }

    fn pace_handoff(&mut self, now: MonoMs, playing: bool) {
        let Some(handoff) = &mut self.handoff else {
            return;
        };
        if !self.state.want {
            let quiet = if playing {
                handoff.quiet_since = None;
                false
            } else {
                let since = *handoff.quiet_since.get_or_insert(now);
                now.saturating_since(since) >= HANDOFF_DRAIN_MS
            };
            if !quiet && now.saturating_since(handoff.muted_at) < HANDOFF_MUTE_MS {
                return;
            }
        }
        let handoff = self.handoff.take().unwrap();
        if self.state.want {
            self.state.adopt_quieted(handoff.streams);
        } else {
            self.restores.extend(handoff.streams);
        }
    }

    pub fn on_graph(
        &mut self,
        accessory: Option<Address>,
        graph: &Graph,
        waiting_for_grant: bool,
        now: MonoMs,
    ) -> Outcome {
        let state = std::mem::take(&mut self.state);
        let action = if waiting_for_grant {
            hold_for_takeover(state, accessory, graph, now)
        } else {
            plan(state, accessory, graph, now)
        };
        let mut work = Vec::new();
        if !action.quiet.is_empty() {
            work.push(Work::Quiet(action.quiet));
        }
        if let Some(restart) = action.restart_output {
            work.push(Work::Restart(restart));
        }
        if let Some(start) = action.start_output {
            work.push(Work::Start(start));
        }
        if let Some(name) = action.set_default {
            work.push(Work::SetDefault(name));
        }
        if let Some(moves) = action.move_streams {
            work.push(Work::Move(moves));
        }
        self.state = action.state;
        self.pending = work;
        self.restores.extend(action.restore);
        if self.handoff.is_some() {
            self.defer_restores(now);
        }
        Outcome { routed: action.routed, failure: action.failure }
    }

    /// Drain one ordered batch after all decisions for this input. Quiet work
    /// precedes the daemon's pause/control effects; restores precede route moves.
    pub fn take_work(&mut self, now: MonoMs, playing: bool) -> Vec<Work> {
        self.pace_handoff(now, playing);
        let mut work = Vec::new();
        if !self.handoff_quiet.is_empty() {
            work.push(Work::Quiet(std::mem::take(&mut self.handoff_quiet)));
        }
        if !self.restores.is_empty() {
            work.push(Work::Restore(std::mem::take(&mut self.restores)));
        }
        work.append(&mut self.pending);
        work.sort_by_key(|item| if matches!(item, Work::Quiet(_)) { 0 } else { 1 });
        work
    }
}

impl Default for Router {
    fn default() -> Self {
        Self::new()
    }
}

/// How long a handoff-muted stream stays muted after every player reports
/// Paused. Brave keeps playing audio it had already queued for a while after
/// it reports Paused, and the XPS speaker filter (EQ and a lookahead limiter)
/// holds more. Unmuting 150 ms after Paused still let that tail reach the
/// speakers as a short blip (2026-10-19); 500 ms did not, across rapid
/// switching.
const HANDOFF_DRAIN_MS: u64 = 500;
/// Streams muted at a handoff are unmuted by then even if a player never
/// reports Paused. A stream with no MPRIS player for the daemon to pause
/// keeps playing, and is silenced for at most this long. It must exceed
/// HANDOFF_DRAIN_MS plus the few tens of milliseconds a player takes to
/// report Paused.
const HANDOFF_MUTE_MS: u64 = 1_000;

const RETRY_MS: u64 = 1_000;
/// How long speaker playback stays muted waiting for the pods. A play that
/// first needs fleet admission and a new link took 6-8 s on 2026-10-19.
const TAKEOVER_HOLD_MS: u64 = 12_000;

/// Hold speaker playback while ownership or the AirPods output is still pending.
fn hold_for_takeover(
    mut state: RouteState,
    accessory: Option<Address>,
    graph: &Graph,
    now: MonoMs,
) -> RouteAction {
    state.retire_missing(graph, now);
    let pods = accessory.and_then(|address| sink_for(graph, address));
    if !state.want {
        return release(state, pods, graph, now);
    }
    if accessory.is_none() {
        return waiting(state);
    }
    if state.takeover_started.is_some_and(|at| now.saturating_since(at) >= TAKEOVER_HOLD_MS) {
        let mut action = release(state, pods, graph, now);
        action.failure = Some("audio takeover timed out");
        return action;
    }
    if state.from.is_none()
        && !graph.default_sink.is_empty()
        && pods.is_none_or(|sink| graph.default_sink != sink.name)
    {
        state.from = Some(graph.default_sink.clone());
    }
    let mut quiet = Vec::new();
    for stream in graph.streams.iter().filter(|stream| {
        managed(stream)
            && !is_speaker_chain(stream)
            && follows_seat(&stream.sink, state.from.as_deref())
            && pods.is_none_or(|sink| stream.sink != sink.name)
    }) {
        if state.quieted.insert((stream.id, stream.generation)) {
            state.held.push(snap(stream));
            quiet.push(target(stream));
        } else if due(&state.quiet_at, now) {
            quiet.push(target(stream));
        }
    }
    if !state.held.is_empty() {
        state.takeover_started.get_or_insert(now);
    }
    if !quiet.is_empty() {
        state.quiet_at = Some(now);
    }
    waiting_with(state, quiet, Vec::new())
}

/// Mute managed streams on the current default, then switch, then move them.
/// A stream already on some other sink is left there. The same graph planned
/// again does not repeat a request inside the retry interval.
fn plan(
    mut state: RouteState,
    accessory: Option<Address>,
    graph: &Graph,
    now: MonoMs,
) -> RouteAction {
    state.retire_missing(graph, now);
    let accessory_sink = accessory.and_then(|address| sink_for(graph, address));
    if !state.want {
        return release(state, accessory_sink, graph, now);
    }
    if let Some(recovery) = state.recovery.clone() {
        if now.saturating_since(recovery.started) >= 1_500 {
            let mut action = release(state, accessory_sink, graph, now);
            action.failure = Some("audio recovery timed out");
            return action;
        }
        if recovery.resetting {
            return waiting(state);
        }
        let on_pods: Vec<_> = state
            .held
            .iter()
            .filter_map(|s| find_held(graph, s))
            .filter(|s| s.sink == recovery.sink)
            .collect();
        let hot: Vec<_> = state
            .held
            .iter()
            .filter_map(|saved| find_held(graph, saved))
            .filter(|s| (s.sink == recovery.sink || s.sink == recovery.via) && !s.mute)
            .map(target)
            .collect();
        if !hot.is_empty() {
            if due(&state.quiet_at, now) {
                state.quiet_at = Some(now);
                return waiting_with(state, hot, Vec::new());
            }
            return waiting(state);
        }
        let moving: Vec<_> = on_pods.iter().map(|s| target(s)).collect();
        if !moving.is_empty() {
            let ask = due(&state.asked_move, now);
            if ask {
                state.asked_move = Some(now);
            }
            let mut action = waiting(state);
            if ask {
                action.move_streams = Some(Move { sink: recovery.via, streams: moving });
            }
            return action;
        }
        state.recovery.as_mut().unwrap().resetting = true;
        let mut action = waiting(state);
        action.restart_output = Some(Restart { id: recovery.id, name: recovery.sink });
        return action;
    }
    let Some(sink) = accessory_sink else {
        return hold_for_takeover(state, accessory, graph, now);
    };
    state.takeover_started = None;
    if state.from.is_none() && !graph.default_sink.is_empty() && graph.default_sink != sink.name {
        state.from = Some(graph.default_sink.clone());
    }
    let on_from = on_sink(graph, state.from.as_deref());
    let mut quiet = Vec::new();
    for stream in &on_from {
        let key = (stream.id, stream.generation);
        if state.quieted.insert(key) {
            state.held.push(snap(stream));
            quiet.push(target(stream));
        }
    }
    if !quiet.is_empty() {
        state.quiet_at = Some(now);
        return waiting_with(state, quiet, Vec::new());
    }
    let still_hot: Vec<StreamTarget> = on_from
        .iter()
        .filter(|stream| !stream.mute && state.quieted.contains(&(stream.id, stream.generation)))
        .map(|stream| target(stream))
        .collect();
    if !still_hot.is_empty() {
        if due(&state.quiet_at, now) {
            state.quiet_at = Some(now);
            return waiting_with(state, still_hot, Vec::new());
        }
        return waiting(state);
    }
    let restore = take_arrived(&mut state, graph, &sink.name, now);
    if graph.default_sink != sink.name {
        let set_default = if due(&state.asked_default, now) {
            state.asked_default = Some(now);
            Some(sink.name.clone())
        } else {
            None
        };
        return RouteAction {
            routed: false,
            quiet: Vec::new(),
            set_default,
            move_streams: None,
            restore,
            state,
            restart_output: None,
            start_output: None,
            failure: None,
        };
    }
    state.asked_default = None;
    let moving = streams_to_move(&state, graph, &sink.name);
    let move_streams = if moving.is_empty() {
        None
    } else if due(&state.asked_move, now) {
        state.asked_move = Some(now);
        Some(Move { sink: sink.name.clone(), streams: moving })
    } else {
        None
    };
    let mut start_output = None;
    if state.start_after_relink.is_some()
        && state.held.is_empty()
        && moving_is_complete(&state, graph, &sink.name)
    {
        let relinked = *state.relinked_at.get_or_insert(now);
        if now.saturating_since(relinked) >= 250 {
            start_output = state.start_after_relink.take();
            state.relinked_at = None;
        }
    } else {
        state.relinked_at = None;
    }
    RouteAction {
        routed: true,
        quiet: Vec::new(),
        set_default: None,
        move_streams,
        restore,
        state,
        restart_output: None,
        start_output,
        failure: None,
    }
}

fn moving_is_complete(state: &RouteState, graph: &Graph, sink: &str) -> bool {
    streams_to_move(state, graph, sink).is_empty()
        && graph.streams.iter().any(|stream| stream.sink == sink && managed(stream))
}

fn release(
    mut state: RouteState,
    accessory_sink: Option<&Sink>,
    graph: &Graph,
    now: MonoMs,
) -> RouteAction {
    state.recovery = None;
    state.start_after_relink = None;
    state.relinked_at = None;
    let restore = std::mem::take(&mut state.held);
    state.quieted.clear();
    state.restore_sent.clear();
    state.takeover_started = None;
    state.quiet_at = None;
    state.asked_move = None;
    state.want = false;
    let on_accessory = accessory_sink.is_some_and(|sink| sink.name == graph.default_sink);
    let speakers = if state.returning {
        state.from.clone().or_else(|| speaker_sink(graph, accessory_sink))
    } else {
        state.from.clone()
    };
    let returning: Vec<StreamTarget> = match accessory_sink {
        Some(sink) if state.returning => graph
            .streams
            .iter()
            .filter(|stream| stream.sink == sink.name && returnable(stream))
            .map(target)
            .collect(),
        _ => Vec::new(),
    };
    if !on_accessory && restore.is_empty() && returning.is_empty() {
        return RouteAction {
            routed: false,
            quiet: Vec::new(),
            set_default: None,
            move_streams: None,
            restore,
            state: RouteState::new(),
            restart_output: None,
            start_output: None,
            failure: None,
        };
    }
    let move_streams = match speakers.clone() {
        Some(sink) if !returning.is_empty() && due(&state.asked_return, now) => {
            state.asked_return = Some(now);
            Some(Move { sink, streams: returning })
        }
        _ => None,
    };
    let set_default = if on_accessory {
        speakers.and_then(|from| {
            if due(&state.asked_default, now) {
                state.asked_default = Some(now);
                Some(from)
            } else {
                None
            }
        })
    } else {
        state.from = None;
        state.asked_default = None;
        None
    };
    RouteAction {
        routed: false,
        quiet: Vec::new(),
        set_default,
        move_streams,
        restore,
        state,
        restart_output: None,
        start_output: None,
        failure: None,
    }
}

fn waiting(state: RouteState) -> RouteAction {
    waiting_with(state, Vec::new(), Vec::new())
}

fn waiting_with(
    state: RouteState,
    quiet: Vec<StreamTarget>,
    restore: Vec<StreamSnap>,
) -> RouteAction {
    RouteAction {
        routed: false,
        quiet,
        set_default: None,
        move_streams: None,
        restore,
        state,
        restart_output: None,
        start_output: None,
        failure: None,
    }
}

fn due(asked: &Option<MonoMs>, now: MonoMs) -> bool {
    asked.is_none_or(|at| now.saturating_since(at) >= RETRY_MS)
}

fn on_sink<'a>(graph: &'a Graph, sink: Option<&str>) -> Vec<&'a LiveStream> {
    let Some(sink) = sink else {
        return Vec::new();
    };
    graph
        .streams
        .iter()
        .filter(|stream| stream.sink == sink && managed(stream) && !is_speaker_chain(stream))
        .collect()
}

fn take_arrived(
    state: &mut RouteState,
    graph: &Graph,
    accessory: &str,
    now: MonoMs,
) -> Vec<StreamSnap> {
    let from = state.from.clone();
    let mut restore = Vec::new();
    let mut keep = Vec::new();
    let mut ask = false;
    for saved in state.held.drain(..) {
        let Some(live) = find_held(graph, &saved) else {
            // A move can temporarily remove the stream from a graph snapshot.
            // Keep its original state through the bounded graph gap.
            // The serial check still excludes a replacement using the same ID.
            keep.push(saved);
            continue;
        };
        let saved = StreamSnap {
            id: live.id,
            generation: live.generation,
            mute: saved.mute,
            volume: saved.volume,
        };
        if from.as_deref().is_some_and(|from| live.sink == from) && live.sink != accessory {
            keep.push(saved);
            continue;
        }
        // An unmuted snapshot can precede our queued mute, especially when
        // WirePlumber auto-moves a stream onto a newly created output. Always
        // send the matching restore before considering that stream settled.
        if settled(live, &saved)
            && (saved.mute || state.restore_sent.contains(&(saved.id, saved.generation)))
        {
            continue;
        }
        ask = true;
        restore.push(saved);
        keep.push(saved);
    }
    if ask && due(&state.asked_restore, now) {
        state.asked_restore = Some(now);
    } else if ask {
        restore.clear();
    }
    state.restore_sent.extend(restore.iter().map(|saved| (saved.id, saved.generation)));
    state.held = keep;
    // Once a stream is restored, a later route must save it afresh before
    // muting it again. Remembering only "already quieted" loses that save.
    state.quieted.retain(|&(id, generation)| {
        state.held.iter().any(|saved| saved.id == id && saved.generation == generation)
    });
    state.restore_sent.retain(|&(id, generation)| {
        state.held.iter().any(|saved| saved.id == id && saved.generation == generation)
    });
    if state.held.is_empty() {
        state.asked_restore = None;
    }
    restore
}

fn find_held<'a>(graph: &'a Graph, saved: &StreamSnap) -> Option<&'a LiveStream> {
    graph
        .streams
        .iter()
        .find(|stream| stream.id == saved.id && stream.generation == saved.generation)
        .or_else(|| {
            graph
                .streams
                .iter()
                .find(|stream| saved.generation != 0 && stream.generation == saved.generation)
        })
}

fn settled(live: &LiveStream, saved: &StreamSnap) -> bool {
    live.mute == saved.mute && (live.volume - saved.volume).abs() < 0.05
}

fn streams_to_move(state: &RouteState, graph: &Graph, accessory: &str) -> Vec<StreamTarget> {
    let mut moving = still_on(state, graph);
    for stream in &graph.streams {
        if !managed(stream) || stream.sink == accessory || is_speaker_chain(stream) {
            continue;
        }
        if !follows_seat(&stream.sink, state.from.as_deref()) {
            continue;
        }
        let next = target(stream);
        if moving.iter().any(|item| item.id == next.id && item.generation == next.generation) {
            continue;
        }
        moving.push(next);
    }
    moving
}

/// The laptop speakers, including a stream that started after the pods were already the default.
fn follows_seat(sink: &str, from: Option<&str>) -> bool {
    if from.is_some_and(|from| sink == from) {
        return true;
    }
    let name = sink.to_ascii_lowercase();
    name.contains("speaker") && !name.contains("hdmi") && !name.contains("displayport")
}

/// A new or resumed playback stream, not the initial graph or a routing echo.
pub fn playback_started(
    previous: Option<&Graph>,
    graph: &Graph,
    accessory: Address,
    from: Option<&str>,
) -> bool {
    let Some(previous) = previous else {
        return false;
    };
    let pods = sink_for(graph, accessory);
    graph.streams.iter().any(|stream| {
        managed(stream)
            && !is_speaker_chain(stream)
            && (pods.is_some_and(|sink| stream.sink == sink.name)
                || follows_seat(&stream.sink, from))
            && !previous.streams.iter().any(|old| {
                old.generation == stream.generation
                    && (stream.generation != 0 || old.id == stream.id)
                    && managed(old)
            })
    })
}

/// Audible playback on the laptop speakers, which putting the pods in moves.
pub fn playing_on_speakers(graph: &Graph, from: Option<&str>) -> bool {
    graph.streams.iter().any(|stream| {
        managed(stream) && !is_speaker_chain(stream) && follows_seat(&stream.sink, from)
    })
}

/// Anything on the pods but event sounds, playing or not: a play is
/// often reported before its stream uncorks.
fn returnable(stream: &LiveStream) -> bool {
    let role = stream.role.to_ascii_lowercase();
    role != "event" && role != "notification" && !is_speaker_chain(stream)
}

/// The laptop speakers when no claim recorded where playback came from.
/// A filter on the speakers (the Omarchy tuning) is what the user hears.
fn speaker_sink(graph: &Graph, accessory: Option<&Sink>) -> Option<String> {
    let mut found: Option<&Sink> = None;
    for sink in &graph.sinks {
        if accessory.is_some_and(|pods| pods.name == sink.name) || !follows_seat(&sink.name, None) {
            continue;
        }
        if !sink.name.starts_with("alsa_output") {
            return Some(sink.name.clone());
        }
        found.get_or_insert(sink);
    }
    found.map(|sink| sink.name.clone())
}

fn is_speaker_chain(stream: &LiveStream) -> bool {
    stream.node.contains("omarchy_speaker_tuning")
}

fn still_on(state: &RouteState, graph: &Graph) -> Vec<StreamTarget> {
    let Some(from) = state.from.as_deref() else {
        return Vec::new();
    };
    state
        .held
        .iter()
        .filter(|saved| {
            graph.streams.iter().any(|stream| {
                stream.id == saved.id
                    && stream.generation == saved.generation
                    && stream.sink == from
            })
        })
        .map(|saved| StreamTarget { id: saved.id, generation: saved.generation })
        .collect()
}

fn sink_for(graph: &Graph, address: Address) -> Option<&Sink> {
    let mut found: Option<&Sink> = None;
    for sink in &graph.sinks {
        if sink.address != Some(address) {
            continue;
        }
        if sink.profile.as_deref().is_some_and(|profile| profile.contains("a2dp")) {
            return Some(sink);
        }
        if found.is_none()
            && sink
                .profile
                .as_deref()
                .is_none_or(|profile| !profile.contains("headset") && !profile.contains("sco"))
        {
            found = Some(sink);
        }
    }
    found.or_else(|| graph.sinks.iter().find(|sink| sink.address == Some(address)))
}

/// A stream the user is hearing. Event sounds, notifications, muted streams,
/// and corked streams are not moved.
pub fn managed(stream: &LiveStream) -> bool {
    if stream.corked || stream.mute {
        return false;
    }
    let role = stream.role.to_ascii_lowercase();
    role != "event" && role != "notification"
}

/// Audible playback streams on the accessory's output.
pub fn audible_on_accessory(graph: &Graph, accessory: Address) -> Vec<StreamSnap> {
    let Some(sink) = sink_for(graph, accessory) else {
        return Vec::new();
    };
    graph
        .streams
        .iter()
        .filter(|stream| stream.sink == sink.name && managed(stream))
        .map(snap)
        .collect()
}

fn snap(stream: &LiveStream) -> StreamSnap {
    StreamSnap {
        id: stream.id,
        generation: stream.generation,
        mute: stream.mute,
        volume: stream.volume,
    }
}

fn target(stream: &LiveStream) -> StreamTarget {
    StreamTarget { id: stream.id, generation: stream.generation }
}

#[cfg(test)]
fn released(from: &str, held: Vec<StreamSnap>) -> RouteState {
    RouteState { want: false, from: Some(from.into()), held, ..RouteState::new() }
}

#[cfg(test)]
fn claiming() -> RouteState {
    RouteState { want: true, ..RouteState::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address() -> Address {
        Address::new([0xA0, 0, 0, 0, 0, 3]).unwrap()
    }

    fn sink(name: &str, address: Option<Address>, profile: Option<&str>) -> Sink {
        Sink { name: name.into(), address, profile: profile.map(str::to_string) }
    }

    fn stream(id: u32, generation: u64, sink: &str, role: &str, mute: bool) -> LiveStream {
        LiveStream {
            id,
            generation,
            sink: sink.into(),
            role: role.into(),
            node: String::new(),
            mute,
            volume: 1.0,
            corked: false,
        }
    }

    fn graph(default_sink: &str, sinks: Vec<Sink>, streams: Vec<LiveStream>) -> Graph {
        Graph { default_sink: default_sink.into(), sinks, streams, cards: Vec::new() }
    }

    #[test]
    fn a_destroyed_browser_stream_does_not_accumulate_across_handoffs() {
        let pods = address();
        let mut picture = graph(
            "pods",
            vec![sink("pods", Some(pods), None), sink("speakers", None, None)],
            vec![stream(1, 8, "pods", "", false)],
        );
        let mut state = claiming();
        state.from = Some("speakers".into());
        state.held.push(snap(&picture.streams[0]));
        picture.streams.clear();
        let gap = plan(state, Some(pods), &picture, MonoMs(0));
        assert_eq!(gap.state.held.len(), 1, "retain a brief graph gap during a move");
        let gone = plan(gap.state, Some(pods), &picture, MonoMs(1_100));
        assert!(
            gone.state.held.is_empty(),
            "a closed browser node cannot block the next output restart"
        );
    }

    #[test]
    fn takeover_mutes_speakers_before_the_airpods_output_exists() {
        let pods = address();
        let mut picture = graph(
            "speakers",
            vec![sink("speakers", None, None)],
            vec![
                stream(1, 8, "speakers", "", false),
                stream(2, 9, "speakers", "", true),
                stream(3, 10, "speakers", "notification", false),
                stream(4, 11, "speakers", "", false),
            ],
        );
        picture.streams[0].volume = 1.4;
        picture.streams[3].node = "omarchy_speaker_tuning_output".into();
        let quiet = plan(claiming(), Some(pods), &picture, MonoMs(0));
        assert_eq!(quiet.quiet, vec![StreamTarget { id: 1, generation: 8 }]);
        assert!(quiet.move_streams.is_none());
        assert!(quiet.set_default.is_none());
        picture.streams[0].mute = true;
        let waiting = plan(quiet.state, Some(pods), &picture, MonoMs(200));
        assert!(waiting.restore.is_empty());
        assert!(!waiting.state.held[0].mute);
        picture.sinks.push(sink("pods", Some(pods), Some("a2dp-sink")));
        let output = plan(waiting.state, Some(pods), &picture, MonoMs(400));
        assert_eq!(output.set_default.as_deref(), Some("pods"));
        picture.default_sink = "pods".into();
        picture.streams[0].sink = "pods".into();
        let arrived = plan(output.state, Some(pods), &picture, MonoMs(600));
        assert_eq!(
            arrived.restore,
            vec![StreamSnap { id: 1, generation: 8, mute: false, volume: 1.4 }]
        );
    }

    #[test]
    fn a_cancelled_or_timed_out_takeover_restores_the_speaker_stream() {
        let pods = address();
        let mut picture = graph(
            "speakers",
            vec![sink("speakers", None, None)],
            vec![stream(1, 8, "speakers", "", false)],
        );
        let held = plan(claiming(), Some(pods), &picture, MonoMs(0)).state;
        assert_eq!(held.held.len(), 1);
        picture.streams[0].mute = true;
        for cancel in [false, true] {
            let mut state = held.clone();
            if cancel {
                state.want = false;
            }
            let released = plan(state, Some(pods), &picture, MonoMs(TAKEOVER_HOLD_MS));
            assert_eq!(released.restore.len(), 1);
            assert!(!released.restore[0].mute);
            assert!(!released.state.want);
            assert!(released.state.held.is_empty());
            assert_eq!(released.failure.is_some(), !cancel);
            let again = plan(released.state, Some(pods), &picture, MonoMs(TAKEOVER_HOLD_MS + 100));
            assert!(again.quiet.is_empty());
        }
    }

    #[test]
    fn an_unmuted_arrival_still_restores_a_pending_mute() {
        let pods = address();
        let mut picture = graph(
            "speakers",
            vec![sink("speakers", None, None), sink("pods", Some(pods), Some("a2dp-sink"))],
            vec![stream(1, 8, "speakers", "", false)],
        );
        let quiet = plan(claiming(), Some(pods), &picture, MonoMs(0));
        assert_eq!(quiet.quiet.len(), 1);
        // PipeWire may auto-route the stream before the asynchronous mute is
        // reflected in its snapshot. Arrival alone does not cancel that mute.
        picture.default_sink = "pods".into();
        picture.streams[0].sink = "pods".into();
        let arrived = plan(quiet.state, Some(pods), &picture, MonoMs(100));
        assert_eq!(arrived.restore.len(), 1);
        assert!(!arrived.restore[0].mute);
        picture.streams[0].mute = true;
        let retry = plan(arrived.state, Some(pods), &picture, MonoMs(1_200));
        assert_eq!(retry.restore.len(), 1);
        assert!(!retry.restore[0].mute);
        picture.streams[0].mute = false;
        let done = plan(retry.state, Some(pods), &picture, MonoMs(1_300));
        assert!(done.state.held.is_empty());
        assert!(done.restore.is_empty());
    }

    #[test]
    fn a_claim_mutes_before_it_switches_and_moves_with_the_serial() {
        let pods = address();
        let pods_sink = "bluez_output.A0_00_00_00_00_03.1";
        let speakers = "alsa_output.pci.analog-stereo";
        let picture = graph(
            speakers,
            vec![
                sink(speakers, None, None),
                sink(pods_sink, Some(pods), Some("a2dp-sink")),
                sink("bluez_output.A0_00_00_00_00_03.0", Some(pods), Some("headset-head-unit")),
            ],
            vec![
                stream(1, 8, speakers, "", false),
                stream(2, 9, speakers, "notification", false),
                stream(3, 10, "alsa_output.hdmi", "", false),
            ],
        );
        let first = plan(claiming(), Some(pods), &picture, MonoMs(0));
        assert_eq!(first.quiet, vec![StreamTarget { id: 1, generation: 8 }]);
        assert!(first.set_default.is_none());
        assert!(first.move_streams.is_none());
        assert_eq!(first.state.held[0].volume, 1.0);

        let waiting = plan(first.state.clone(), Some(pods), &picture, MonoMs(200));
        assert!(waiting.quiet.is_empty());
        assert!(waiting.set_default.is_none());

        let mut muted = picture.clone();
        muted.streams[0].mute = true;
        let switching = plan(first.state, Some(pods), &muted, MonoMs(300));
        assert_eq!(switching.set_default.as_deref(), Some(pods_sink));

        let mut landed = muted;
        landed.default_sink = pods_sink.into();
        let moving = plan(switching.state, Some(pods), &landed, MonoMs(400));
        let moves = moving.move_streams.expect("the held stream is still on the speakers");
        assert_eq!(moves.sink, pods_sink);
        assert_eq!(moves.streams, vec![StreamTarget { id: 1, generation: 8 }]);

        landed.streams[0].sink = pods_sink.into();
        let arrived = plan(moving.state, Some(pods), &landed, MonoMs(500));
        assert!(arrived.routed);
        assert!(arrived.move_streams.is_none());
        assert_eq!(arrived.restore[0].id, 1);
        assert!(!arrived.restore[0].mute);
        assert_eq!(arrived.state.held.len(), 1);

        let again = plan(arrived.state, Some(pods), &landed, MonoMs(600));
        assert!(again.restore.is_empty(), "the unmute is not repeated inside the retry interval");
        assert_eq!(again.state.held.len(), 1);
        let retry = plan(again.state, Some(pods), &landed, MonoMs(1_700));
        assert!(!retry.restore[0].mute);
        landed.streams[0].mute = false;
        let done = plan(retry.state, Some(pods), &landed, MonoMs(1_800));
        assert!(done.restore.is_empty());
        assert!(done.state.held.is_empty());
    }

    #[test]
    fn releasing_puts_the_default_back_and_a_manual_choice_stays() {
        let pods = address();
        let pods_sink = "bluez_output.A0_00_00_00_00_03.1";
        let speakers = "alsa_output.pci.analog-stereo";
        let picture = graph(
            pods_sink,
            vec![sink(pods_sink, Some(pods), Some("a2dp-sink")), sink(speakers, None, None)],
            vec![],
        );
        let state =
            released(speakers, vec![StreamSnap { id: 1, generation: 4, mute: false, volume: 1.4 }]);
        let back = plan(state, Some(pods), &picture, MonoMs(0));
        assert_eq!(back.set_default.as_deref(), Some(speakers));
        assert_eq!(back.restore[0].volume, 1.4);

        let hdmi =
            graph("alsa_output.hdmi", vec![sink(pods_sink, Some(pods), Some("a2dp-sink"))], vec![]);
        let left = plan(released(speakers, Vec::new()), Some(pods), &hdmi, MonoMs(0));
        assert!(left.set_default.is_none());
        assert!(left.state.from.is_none());
    }

    #[test]
    fn a_new_playback_on_the_speakers_follows_the_pods() {
        let pods = address();
        let pods_sink = "bluez_output.A0_00_00_00_00_03.1";
        let speakers = "alsa_output.pci.HiFi__Speaker__sink";
        let mut picture = graph(
            pods_sink,
            vec![
                sink(speakers, None, None),
                sink(pods_sink, Some(pods), Some("a2dp-sink")),
                sink("alsa_output.hdmi", None, None),
                sink("omarchy_speaker_tuning", None, None),
            ],
            vec![
                stream(4, 12, speakers, "", false),
                stream(5, 13, "alsa_output.hdmi", "", false),
                stream(6, 14, "omarchy_speaker_tuning", "", false),
            ],
        );
        picture.streams[2].node = "omarchy_speaker_tuning_output".into();
        let action = plan(claiming(), Some(pods), &picture, MonoMs(0));
        let moves = action.move_streams.expect("the speaker playback follows");
        assert_eq!(moves.sink, pods_sink);
        assert_eq!(moves.streams, vec![StreamTarget { id: 4, generation: 12 }]);
    }

    #[test]
    fn recovery_parks_on_a_built_in_output_not_named_speaker() {
        let pods = Address::parse("AA:BB:CC:DD:EE:FF").unwrap();
        let graph = Graph {
            default_sink: "bluez_output.pods".into(),
            sinks: vec![
                Sink {
                    name: "alsa_output.pci-0000_00_1f.3.hdmi-stereo".into(),
                    address: None,
                    profile: None,
                },
                Sink {
                    name: "alsa_output.pci-0000_00_1f.3.analog-stereo".into(),
                    address: None,
                    profile: None,
                },
                Sink {
                    name: "bluez_output.pods".into(),
                    address: Some(pods),
                    profile: Some("a2dp-sink".into()),
                },
            ],
            cards: Vec::new(),
            streams: Vec::new(),
        };
        let mut state = RouteState { want: true, ..RouteState::default() };
        assert!(state.begin_recovery(1, pods, &graph, MonoMs(0)));
        assert_eq!(
            state.recovery.as_ref().unwrap().via,
            "alsa_output.pci-0000_00_1f.3.analog-stereo"
        );
    }

    #[test]
    fn recovery_detaches_only_after_muting_and_relinks_only_after_resume() {
        let pods = address();
        let mut picture = graph(
            "pods",
            vec![sink("pods", Some(pods), Some("a2dp-sink")), sink("speakers", None, None)],
            vec![stream(1, 10, "pods", "", false), stream(2, 20, "pods", "", true)],
        );
        picture.streams[0].volume = 1.4;
        let mut state = claiming();
        assert!(state.begin_recovery(7, pods, &picture, MonoMs(0)));
        let muted = plan(state, Some(pods), &picture, MonoMs(0));
        assert_eq!(muted.quiet, vec![StreamTarget { id: 1, generation: 10 }]);
        assert!(muted.move_streams.is_none());
        assert!(muted.restart_output.is_none());
        picture.streams[0].mute = true;
        let detached = plan(muted.state, Some(pods), &picture, MonoMs(10));
        assert_eq!(detached.move_streams.as_ref().unwrap().sink, "speakers");
        assert_eq!(detached.move_streams.as_ref().unwrap().streams.len(), 2);
        assert!(detached.restart_output.is_none());
        for stream in &mut picture.streams {
            stream.sink = "speakers".into();
        }
        let reset = plan(detached.state, Some(pods), &picture, MonoMs(20));
        assert_eq!(reset.restart_output, Some(Restart { id: 7, name: "pods".into() }));
        let waiting = plan(reset.state, Some(pods), &picture, MonoMs(30));
        assert!(waiting.move_streams.is_none());
        assert!(waiting.restore.is_empty());
        assert!(waiting.restart_output.is_none());
        let mut state = waiting.state;
        assert!(!state.finish_recovery(6, true));
        assert!(state.finish_recovery(7, true));
        let relink = plan(state, Some(pods), &picture, MonoMs(40));
        assert_eq!(relink.move_streams.as_ref().unwrap().sink, "pods");
        assert!(relink.restore.is_empty(), "no unmute on speakers");
        for stream in &mut picture.streams {
            stream.sink = "pods".into();
        }
        let restored = plan(relink.state, Some(pods), &picture, MonoMs(50));
        assert_eq!(
            restored.restore,
            vec![StreamSnap { id: 1, generation: 10, mute: false, volume: 1.4 }]
        );
        assert!(restored.restart_output.is_none());
        assert!(picture.streams[1].mute, "an originally muted stream stays muted");
        assert!(restored.start_output.is_none(), "wait for the unmute to be observed");
        let waiting = plan(restored.state, Some(pods), &picture, MonoMs(300));
        assert!(waiting.start_output.is_none());
        picture.streams[0].mute = false;
        let linked = plan(waiting.state, Some(pods), &picture, MonoMs(310));
        assert!(linked.start_output.is_none());
        let mut cancelled = linked.state.clone();
        cancelled.want = false;
        assert!(plan(cancelled, Some(pods), &picture, MonoMs(700)).start_output.is_none());
        let early = plan(linked.state, Some(pods), &picture, MonoMs(559));
        assert!(early.start_output.is_none());
        let started = plan(early.state, Some(pods), &picture, MonoMs(560));
        assert_eq!(started.start_output, Some(Restart { id: 7, name: "pods".into() }));
        let repeat = plan(started.state, Some(pods), &picture, MonoMs(900));
        assert!(repeat.start_output.is_none(), "one Start per recovery");
    }

    #[test]
    fn recovery_timeout_and_cancellation_restore_and_retire_callbacks() {
        let pods = address();
        let mut picture = graph(
            "pods",
            vec![sink("pods", Some(pods), Some("a2dp-sink")), sink("speakers", None, None)],
            vec![stream(1, 10, "pods", "", false)],
        );
        for timeout in [false, true] {
            let mut state = claiming();
            assert!(state.begin_recovery(7, pods, &picture, MonoMs(0)));
            if !timeout {
                state.want = false;
            }
            let mut released = plan(state, Some(pods), &picture, MonoMs(1_500));
            assert!(!released.state.want);
            assert_eq!(released.restore.len(), 1);
            assert_eq!(released.failure.is_some(), timeout);
            assert!(released.restart_output.is_none());
            assert!(!released.state.finish_recovery(7, true));
        }
        let mut state = claiming();
        assert!(state.begin_recovery(8, pods, &picture, MonoMs(0)));
        picture.streams[0].generation = 11;
        let mut reset = plan(state, Some(pods), &picture, MonoMs(10));
        assert!(reset.quiet.is_empty(), "do not mute a recycled stream ID");
        assert!(reset.move_streams.is_none());
        assert!(reset.state.finish_recovery(8, false));
        let released = plan(reset.state, Some(pods), &picture, MonoMs(20));
        assert!(!released.state.want);
        assert!(
            released.restore.iter().all(|s| s.generation == 10),
            "the adapter checks this serial before restoring"
        );
    }

    #[test]
    fn routing_the_same_stream_again_saves_its_state_before_muting_again() {
        let pods = address();
        let mut picture = graph(
            "pods",
            vec![sink("pods", Some(pods), Some("a2dp-sink")), sink("speakers", None, None)],
            vec![stream(1, 10, "pods", "", false)],
        );
        let mut state = claiming();
        state.from = Some("speakers".into());
        state.held.push(snap(&picture.streams[0]));
        state.quieted.insert((1, 10));
        state.restore_sent.insert((1, 10));
        // The previous restoration was observed on the pods.
        state = plan(state, Some(pods), &picture, MonoMs(0)).state;
        assert!(state.held.is_empty());
        // PipeWire can publish the same stream back on the speakers while
        // relinking. A previous mute is not permission to forget its original.
        picture.streams[0].sink = "speakers".into();
        let quiet = plan(state, Some(pods), &picture, MonoMs(10));
        assert_eq!(quiet.quiet.len(), 1);
        assert_eq!(quiet.state.held.len(), 1, "every mute needs a saved state");
        picture.streams[0].mute = true;
        let moved = plan(quiet.state, Some(pods), &picture, MonoMs(20));
        picture.streams[0].sink = "pods".into();
        let restored = plan(moved.state, Some(pods), &picture, MonoMs(30));
        assert_eq!(restored.restore.len(), 1);
        assert!(!restored.restore[0].mute);
    }

    #[test]
    fn a_stream_missing_during_relink_keeps_its_original_mute_state() {
        let pods = address();
        let mut picture = graph(
            "pods",
            vec![sink("pods", Some(pods), Some("a2dp-sink")), sink("speakers", None, None)],
            vec![stream(1, 10, "pods", "", false)],
        );
        let mut state = claiming();
        // Repeated handoffs reuse this stream. A graph during the move can omit
        // it while its sink is temporarily unavailable.
        for id in 1..=3 {
            assert!(state.begin_recovery(id, pods, &picture, MonoMs(id * 100)));
            let muted = plan(state, Some(pods), &picture, MonoMs(id * 100));
            picture.streams[0].mute = true;
            let detached = plan(muted.state, Some(pods), &picture, MonoMs(id * 100 + 10));
            picture.streams[0].sink = "speakers".into();
            let reset = plan(detached.state, Some(pods), &picture, MonoMs(id * 100 + 20));
            state = reset.state;
            assert!(state.finish_recovery(id, true));
            let relink = plan(state, Some(pods), &picture, MonoMs(id * 100 + 30));
            let mut missing = picture.clone();
            missing.streams.clear();
            let gap = plan(relink.state, Some(pods), &missing, MonoMs(id * 100 + 40));
            picture.streams[0].sink = "pods".into();
            let arrived = plan(gap.state, Some(pods), &picture, MonoMs(id * 100 + 50));
            assert_eq!(arrived.restore.len(), 1, "switch {id} must restore after the graph gap");
            assert!(!arrived.restore[0].mute);
            picture.streams[0].mute = false;
            state = plan(arrived.state, Some(pods), &picture, MonoMs(id * 100 + 60)).state;
            assert!(state.held.is_empty());
        }
    }

    #[test]
    fn preparation_does_not_move_the_default() {
        let pods = address();
        let picture = graph(
            "speakers",
            vec![sink("bluez_output.A0_00_00_00_00_03.1", Some(pods), Some("a2dp-sink"))],
            vec![stream(1, 1, "speakers", "", false)],
        );
        let action = plan(RouteState::new(), Some(pods), &picture, MonoMs(0));
        assert!(action.quiet.is_empty());
        assert!(action.set_default.is_none());
        assert!(action.move_streams.is_none());
        assert!(!action.routed);
    }
    #[test]
    fn stream_play_edges_exclude_snapshots_moves_and_unrelated_outputs() {
        let pods = Address::new([2, 2, 3, 4, 5, 6]).unwrap();
        let mut current = Graph {
            default_sink: "pods".into(),
            sinks: vec![Sink {
                name: "pods".into(),
                address: Some(pods),
                profile: Some("a2dp-sink".into()),
            }],
            streams: vec![],
            cards: vec![],
        };
        let empty = current.clone();
        let mut browser = stream(4, 20, "speakers", "video", false);
        current.streams.push(browser.clone());
        assert!(!playback_started(None, &current, pods, None));
        assert!(playback_started(Some(&empty), &current, pods, None));
        assert!(!playback_started(Some(&current), &current, pods, None));
        browser.sink = "pods".into();
        let mut moved = current.clone();
        moved.streams = vec![browser];
        assert!(!playback_started(Some(&current), &moved, pods, None));
        for sink in ["hdmi", "usb-headset"] {
            current.streams[0].sink = sink.into();
            assert!(!playback_started(Some(&empty), &current, pods, None));
        }
        current.streams[0].sink = "speakers".into();
        for role in ["event", "notification"] {
            current.streams[0].role = role.into();
            assert!(!playback_started(Some(&empty), &current, pods, None));
        }
        current.streams[0].role = "video".into();
        current.streams[0].corked = true;
        assert!(!playback_started(Some(&empty), &current, pods, None));
        let paused = current.clone();
        current.streams[0].corked = false;
        assert!(playback_started(Some(&paused), &current, pods, None));
        current.streams[0].node = "omarchy_speaker_tuning_output".into();
        assert!(!playback_started(Some(&empty), &current, pods, None));
    }

    #[test]
    fn router_retains_state_and_orders_quiet_before_default() {
        let pods = address();
        let mut router = Router::new();
        router.claim();
        let mut picture = graph(
            "speakers",
            vec![sink("speakers", None, None)],
            vec![stream(1, 8, "speakers", "", false)],
        );
        router.on_graph(Some(pods), &picture, false, MonoMs(0));
        assert_eq!(
            router.take_work(MonoMs(400), false),
            vec![Work::Quiet(vec![StreamTarget { id: 1, generation: 8 }])]
        );
        assert!(router.take_work(MonoMs(400), false).is_empty(), "work drains exactly once");
        // State survived inside the Router: the muted stream is held, and a
        // later graph with the pods sink moves to SetDefault, not Quiet again.
        assert_eq!(router.held_streams().len(), 1);
        picture.streams[0].mute = true;
        picture.sinks.push(sink("pods", Some(pods), Some("a2dp-sink")));
        router.on_graph(Some(pods), &picture, false, MonoMs(400));
        assert_eq!(router.take_work(MonoMs(400), false), vec![Work::SetDefault("pods".into())]);
    }

    #[test]
    fn router_drains_a_handoff_and_bounds_an_unresponsive_player() {
        for keeps_playing in [false, true] {
            let mut router = Router::new();
            let picture = graph(
                "pods",
                vec![sink("pods", Some(address()), None)],
                vec![stream(1, 8, "pods", "", false)],
            );
            router.claim();
            router.handoff(Some(address()), Some(&picture), MonoMs(0));
            router.on_graph(Some(address()), &picture, false, MonoMs(0));
            assert_eq!(
                router.take_work(MonoMs(0), keeps_playing),
                vec![Work::Quiet(vec![StreamTarget { id: 1, generation: 8 }])]
            );
            let deadline = if keeps_playing { HANDOFF_MUTE_MS } else { HANDOFF_DRAIN_MS };
            assert!(router.take_work(MonoMs(deadline - 1), keeps_playing).is_empty());
            assert_eq!(
                router.take_work(MonoMs(deadline), keeps_playing),
                vec![Work::Restore(vec![snap(&picture.streams[0])])]
            );
            assert!(router.take_work(MonoMs(deadline + 1), keeps_playing).is_empty());
        }
    }

    #[test]
    fn router_reclaim_preserves_originals_before_reading_the_muted_graph() {
        let mut router = Router::new();
        let mut picture = graph(
            "speakers",
            vec![sink("speakers", None, None), sink("pods", Some(address()), None)],
            vec![stream(1, 8, "speakers", "", false)],
        );
        picture.streams[0].volume = 1.4;
        let original = snap(&picture.streams[0]);
        router.claim();
        router.on_graph(Some(address()), &picture, false, MonoMs(0));
        router.take_work(MonoMs(0), true);
        picture.streams[0].mute = true;
        picture.streams[0].sink = "pods".into();
        picture.default_sink = "pods".into();
        // The last graph already reflects our mute; handoff must keep the original.
        router.handoff(Some(address()), Some(&picture), MonoMs(10));
        router.on_graph(Some(address()), &picture, false, MonoMs(10));
        router.take_work(MonoMs(10), false);
        router.claim();
        router.on_graph(Some(address()), &picture, false, MonoMs(20));
        let work = router.take_work(MonoMs(20), true);
        assert!(work.contains(&Work::Restore(vec![original])));
        assert!(!work.iter().any(|w| matches!(w, Work::Restore(s) if s.iter().any(|s| s.mute))));
    }

    #[test]
    fn router_forgets_queued_handoff_work_when_the_server_dies() {
        let mut router = Router::new();
        let picture = graph(
            "pods",
            vec![sink("pods", Some(address()), None)],
            vec![stream(1, 8, "pods", "", false)],
        );
        router.handoff(Some(address()), Some(&picture), MonoMs(0));
        router.forget_server();
        assert!(router.take_work(MonoMs(2_000), false).is_empty());
        router.claim();
        let replacement = graph(
            "speakers",
            vec![sink("speakers", None, None)],
            vec![stream(1, 9, "speakers", "", false)],
        );
        router.on_graph(Some(address()), &replacement, false, MonoMs(2_001));
        router.take_work(MonoMs(2_001), false);
        router.fail_acquisition();
        assert_eq!(
            router.take_work(MonoMs(2_002), false),
            vec![Work::Restore(vec![snap(&replacement.streams[0])])]
        );
    }
}
