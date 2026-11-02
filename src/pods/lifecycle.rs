//! One connection sequence. Phase arms work; resource-owned tasks retain pending
//! attempts across subsequent phases. Link/setup and stream routing retain their retries
//! and transactions; this owner issues OpenLink and joins their observations.

use crate::audio::route::{Card, Graph};
use crate::pods::link::Stage;
pub use crate::retry::RequestId;
use crate::retry::{Delay, Limit, Policy, Requests, State as TaskState, Task};
use crate::step::TransportReply;
use crate::types::{Address, Generation, MonoMs};

pub const RETRY_MS: u64 = 2_000;
pub const RETRY_WINDOW_MS: u64 = 30_000;
const AUDIO_POLICY: Policy = Policy {
    delay: Delay::Fixed(RETRY_MS),
    limit: Limit::FailureWindow(RETRY_WINDOW_MS),
    request_timeout_ms: None, // BlueZ reports its existing bounded call timeout.
    confirmation_timeout_ms: None,
};
const CODEC_POLICY: Policy = Policy {
    delay: Delay::Fixed(1_000),
    limit: Limit::Attempts(3),
    request_timeout_ms: Some(5_000),
    confirmation_timeout_ms: Some(5_000),
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum State {
    #[default]
    Availability,
    Linking,
    PreparingPaths,
    CompletingProfiles,
    Routing,
    Streaming,
    ControlReady,
    Retrying,
    Failed,
    Interrupted,
}
impl State {
    pub fn wire(self) -> &'static str {
        match self {
            Self::Availability => "availability",
            Self::Linking => "linking",
            Self::PreparingPaths => "preparing_paths",
            Self::CompletingProfiles => "completing_profiles",
            Self::Routing => "routing",
            Self::Streaming => "streaming",
            Self::ControlReady => "control_ready",
            Self::Retrying => "retrying",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
        }
    }
}

struct AudioProfile {
    task: Task,
    early: bool,
}
impl Default for AudioProfile {
    fn default() -> Self {
        Self { task: Task::new(AUDIO_POLICY), early: false }
    }
}
#[derive(Clone, Copy, Debug, Default)]
pub enum Handsfree {
    #[default]
    Idle,
    Waiting {
        audio: RequestId,
    },
    Connecting {
        id: RequestId,
    },
    Finished {
        ok: bool,
    },
    Unsupported,
}
#[derive(PartialEq, Eq)]
struct CodecTarget {
    card: String,
    profile: String,
}
struct Codec {
    target: Option<CodecTarget>,
    task: Task,
}
impl Default for Codec {
    fn default() -> Self {
        Self { target: None, task: Task::new(CODEC_POLICY) }
    }
}

/// Current device/worker observations, not a second connection policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Profiles {
    pub accessory: Address,
    pub connected: bool,
    pub services_resolved: bool,
    pub handsfree: bool,
    pub busy: bool,
}
pub struct Facts<'a> {
    pub accessory: Option<Address>,
    pub generation: Generation,
    pub control: Stage,
    pub bluetooth: Option<bool>,
    pub may_open: bool,
    pub early_audio_allowed: bool,
    pub wants_audio: bool,
    pub graph: Option<&'a Graph>,
    pub selected_codec: Option<&'a str>,
    pub routed: bool,
    pub recovering: bool,
    pub transport_active: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    OpenLink(Generation),
    ConnectAudio(RequestId),
    CompleteProfiles(RequestId),
    SelectCodec { id: RequestId, card: String, profile: String },
}

pub struct Progress<'a> {
    pub state: State,
    pub waiting_for: &'static str,
    pub audio: &'a Task,
    pub handsfree: &'a Handsfree,
    pub codec: &'a Task,
}
#[derive(Default)]
pub struct Lifecycle {
    scope: Option<(Address, Generation)>,
    requests: Requests,
    link_requested: Option<Generation>,
    state: State,
    waiting_for: &'static str,
    audio: AudioProfile,
    handsfree: Handsfree,
    codec: Codec,
    profiles: Option<Profiles>,
}
impl Lifecycle {
    #[cfg(test)]
    pub fn request_id(&self) -> Option<RequestId> {
        match self.audio.task.state() {
            TaskState::Running { id, .. } | TaskState::Succeeded { id: Some(id) } => Some(id),
            _ => None,
        }
    }

    pub fn progress(&self) -> Progress<'_> {
        Progress {
            state: self.state,
            waiting_for: self.waiting_for,
            audio: &self.audio.task,
            handsfree: &self.handsfree,
            codec: &self.codec.task,
        }
    }
    pub fn link_requested(&self, generation: Generation) -> bool {
        self.link_requested == Some(generation)
    }
    /// End this playback claim, including codec work on the same connection.
    /// In-flight platform work may finish; retired IDs cannot change this owner.
    pub fn reset_audio(&mut self) {
        self.audio = AudioProfile::default();
        self.handsfree = Handsfree::Idle;
        self.codec = Codec::default();
    }
    pub fn audio_lost(&mut self) {
        self.codec = Codec::default();
    }
    pub fn note_profiles(&mut self, profiles: Profiles) {
        self.profiles = Some(profiles);
    }
    pub fn note_audio(&mut self, id: RequestId, reply: TransportReply, now: MonoMs) -> bool {
        self.audio.task.reply(id, reply == TransportReply::Connected, now)
    }
    pub fn note_handsfree(&mut self, id: RequestId, ok: bool) {
        if matches!(self.handsfree, Handsfree::Connecting { id: current } if current == id) {
            self.handsfree = Handsfree::Finished { ok };
        }
    }
    pub fn note_codec(&mut self, id: RequestId, ok: bool, now: MonoMs) -> bool {
        self.codec.task.reply(id, ok, now)
    }

    /// Walk the launch/transition gates in order. The first unmet gate is the
    /// current phase; tasks retain in-flight work as later gates are crossed.
    /// Rechecking resources here also handles loss, without a separate repair path.
    pub fn advance(&mut self, f: &Facts<'_>, now: MonoMs) -> Vec<Action> {
        let scope = f.accessory.map(|address| (address, f.generation));
        if self.scope != scope {
            self.reset_audio();
            self.link_requested = None;
            self.state = State::Availability;
            self.scope = scope;
        }
        let mut actions = Vec::new();
        self.waiting_for = "";
        if f.bluetooth == Some(false) {
            self.profiles = None;
        }
        if !f.wants_audio {
            self.reset_audio();
        }
        // Cancellation and retry admission are owned by the link subtask.
        match f.control {
            Stage::Idle | Stage::Waiting | Stage::Retrying | Stage::Failed | Stage::Interrupted => {
                self.reset_audio();
                self.state = match f.control {
                    Stage::Retrying => State::Retrying,
                    Stage::Failed => State::Failed,
                    Stage::Interrupted => State::Interrupted,
                    _ => State::Availability,
                };
                self.waiting_for = "link_admission";
                return actions;
            }
            _ => {}
        }
        if f.accessory.is_none() {
            self.state = State::Availability;
            return actions;
        }
        let ready = f.control == Stage::Ready;
        let output = f.graph.is_some_and(|g| g.sinks.iter().any(|s| s.address == f.accessory));
        let audio_present = audio_present(f);

        self.state = State::Linking;
        if f.may_open && !self.link_requested(f.generation) {
            self.link_requested = Some(f.generation);
            actions.push(Action::OpenLink(f.generation));
        }
        if !ready && !self.link_requested(f.generation) {
            self.waiting_for = "link_admission";
            return actions;
        }
        if f.bluetooth != Some(true) && !ready {
            self.waiting_for = "bluetooth";
            return actions;
        }

        self.state = State::PreparingPaths;
        if !f.wants_audio {
            if ready {
                self.state = State::ControlReady;
            } else {
                self.waiting_for = "control_ready";
            }
            return actions;
        }
        if !ready && !f.early_audio_allowed {
            self.state = State::Linking;
            self.waiting_for = "link_admission";
            return actions;
        }
        // An existing sink may still be hands-free: start A2DP when needed.
        // Once acquisition has started, an output satisfies its join even if
        // the method reply failed or has not arrived yet.
        // A failed ask is still retried on its backoff while the transport
        // stays inactive, even when a dual-connected sink already exists:
        // that output satisfies the join, but only a successful profile ask
        // starts the transport.
        let retry_failed =
            matches!(self.audio.task.state(), TaskState::Backoff { .. }) && !f.transport_active;
        if !output || self.audio.task.state() == TaskState::Idle || retry_failed {
            self.open_audio(f, now, audio_present, &mut actions);
        }
        // Codec work starts here and continues once per step in later phases.
        self.select_codec(f, now, &mut actions);
        if !output
            && !audio_present
            && !matches!(self.audio.task.state(), TaskState::Succeeded { .. })
        {
            self.waiting_for = "audio_profile";
            return actions;
        }

        self.state = State::CompletingProfiles;
        if matches!(self.handsfree, Handsfree::Idle) {
            if let TaskState::Running { id, .. } | TaskState::Succeeded { id: Some(id) } =
                self.audio.task.state()
            {
                self.handsfree = Handsfree::Waiting { audio: id };
            }
        }
        // HFP joins discovery and A2DP success, but never holds up routing.
        self.finish_profiles(f, &mut actions);
        if !output {
            self.waiting_for = "output";
            return actions;
        }

        self.state = State::Routing;
        self.waiting_for = if !f.routed || f.recovering {
            "stream_route"
        } else if !f.transport_active {
            "transport_active"
        } else {
            self.state = State::Streaming;
            ""
        };
        actions
    }

    fn open_audio(&mut self, f: &Facts<'_>, now: MonoMs, present: bool, actions: &mut Vec<Action>) {
        // A present A2DP profile needs no ask, unless a previous ask failed
        // and the transport never started (a busy dual-connected peer): the
        // backoff retry is then the only way to start it.
        if present
            && (f.transport_active || !matches!(self.audio.task.state(), TaskState::Backoff { .. }))
        {
            return;
        }
        if self.audio.task.state() == TaskState::Exhausted
            && self.audio.early
            && f.control == Stage::Ready
        {
            self.audio.task.reset();
        }
        if let Some(id) = self.audio.task.launch(&mut self.requests, f.generation, now) {
            self.audio.early = f.control != Stage::Ready;
            self.handsfree = Handsfree::Idle;
            actions.push(Action::ConnectAudio(id));
        }
    }
    fn finish_profiles(&mut self, f: &Facts<'_>, actions: &mut Vec<Action>) {
        let Handsfree::Waiting { audio } = self.handsfree else { return };
        if !matches!(self.audio.task.state(), TaskState::Succeeded { id: Some(id) } if id == audio)
        {
            return;
        }
        let Some(p) = self.profiles.filter(|p| {
            Some(p.accessory) == f.accessory && p.connected && p.services_resolved && !p.busy
        }) else {
            return;
        };
        if p.handsfree {
            self.handsfree = Handsfree::Connecting { id: audio };
            actions.push(Action::CompleteProfiles(audio));
        } else {
            self.handsfree = Handsfree::Unsupported;
        }
    }
    fn select_codec(&mut self, f: &Facts<'_>, now: MonoMs, actions: &mut Vec<Action>) {
        // A deadline retires the attempt even while recovery or a missing card
        // prevents launching another one. Neither consumes additional attempts.
        self.codec.task.tick(now);
        let Some(profile) = f.selected_codec else {
            self.codec = Codec::default();
            return;
        };
        if self.codec.target.as_ref().is_some_and(|target| target.profile != profile) {
            self.codec = Codec::default();
        }
        if f.control != Stage::Ready || !f.wants_audio || f.recovering {
            return;
        }
        let Some(card) = card(f) else { return };
        let target = CodecTarget { card: card.name.clone(), profile: profile.into() };
        if self.codec.target.as_ref() != Some(&target) {
            self.codec.target = Some(target);
            self.codec.task.reset();
        }
        if card.active.as_deref() == Some(profile) {
            self.codec.task.observed_success();
            return;
        }
        if !card.profiles.iter().any(|p| p == profile) {
            return;
        }
        // A completed selection changed externally: this is a new operation.
        // Temporary graph gaps alone never replenish a failed target's budget.
        if matches!(self.codec.task.state(), TaskState::Succeeded { .. }) {
            self.codec.task.reset();
        }
        if let Some(id) = self.codec.task.launch(&mut self.requests, f.generation, now) {
            actions.push(Action::SelectCodec {
                id,
                card: card.name.clone(),
                profile: profile.into(),
            });
        }
    }
}
fn card<'a>(f: &Facts<'a>) -> Option<&'a Card> {
    f.graph?.cards.iter().find(|c| c.address.is_some() && c.address == f.accessory)
}
fn audio_present(f: &Facts<'_>) -> bool {
    card(f).and_then(|c| c.active.as_deref()).is_some_and(|p| p.contains("a2dp-sink"))
        || f.graph.is_some_and(|g| {
            g.sinks.iter().any(|s| {
                s.address == f.accessory
                    && s.profile.as_deref().is_some_and(|p| p.contains("a2dp-sink"))
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::route::Sink;
    fn facts() -> Facts<'static> {
        Facts {
            accessory: Some(Address::parse("CC:22:FE:54:DF:C7").unwrap()),
            generation: Generation(1),
            control: Stage::Connecting,
            bluetooth: Some(false),
            may_open: true,
            early_audio_allowed: true,
            wants_audio: true,
            graph: None,
            selected_codec: None,
            routed: false,
            recovering: false,
            transport_active: false,
        }
    }
    fn profiles(f: &Facts<'_>) -> Profiles {
        Profiles {
            accessory: f.accessory.unwrap(),
            connected: true,
            services_resolved: true,
            handsfree: true,
            busy: false,
        }
    }
    fn graph(f: &Facts<'_>) -> Graph {
        Graph {
            default_sink: "pods".into(),
            streams: vec![],
            cards: vec![Card {
                address: f.accessory,
                name: "card".into(),
                active: Some("a2dp-sink".into()),
                profiles: vec!["aac".into()],
            }],
            sinks: vec![Sink {
                name: "pods".into(),
                address: f.accessory,
                profile: Some("a2dp-sink".into()),
            }],
        }
    }
    fn start(l: &mut Lifecycle, f: &mut Facts<'_>) -> RequestId {
        assert_eq!(l.advance(f, MonoMs(0)), vec![Action::OpenLink(f.generation)]);
        assert_eq!(l.progress().state, State::Linking);
        f.bluetooth = Some(true);
        let actions = l.advance(f, MonoMs(1));
        let [Action::ConnectAudio(id)] = actions.as_slice() else { panic!("{actions:?}") };
        *id
    }
    fn codec_request(actions: Vec<Action>) -> Option<RequestId> {
        actions.into_iter().find_map(|a| match a {
            Action::SelectCodec { id, .. } => Some(id),
            _ => None,
        })
    }

    #[test]
    fn an_existing_output_starts_a2dp_if_needed_but_does_not_repeat_acquisition() {
        for profile in [None, Some("headset-head-unit")] {
            let mut l = Lifecycle::default();
            let mut f = facts();
            let mut g = graph(&f);
            g.cards.clear();
            g.sinks[0].profile = profile.map(String::from);
            f.graph = Some(&g);
            f.control = Stage::Ready;
            f.may_open = false;
            f.routed = true;
            f.transport_active = true;
            let actions = l.advance(&f, MonoMs(0));
            let [Action::ConnectAudio(id)] = actions.as_slice() else { panic!("{actions:?}") };
            assert_eq!(l.progress().state, State::Streaming);
            l.note_audio(*id, TransportReply::Failed, MonoMs(1));
            for at in [2, 2_001, 5_000] {
                assert!(l.advance(&f, MonoMs(at)).is_empty());
                assert_eq!(l.progress().state, State::Streaming);
            }
        }
    }

    #[test]
    fn a_busy_audio_profile_is_retried_while_the_transport_stays_inactive() {
        let mut l = Lifecycle::default();
        let mut f = facts();
        let first = start(&mut l, &mut f);
        assert!(l.note_audio(first, TransportReply::Busy, MonoMs(2)));
        // The pods are dual-connected: a sink already exists on this host,
        // but its transport never started while the peer holds the profile.
        let g = graph(&f);
        f.graph = Some(&g);
        f.control = Stage::Ready;
        f.routed = true;
        assert!(l.advance(&f, MonoMs(3)).is_empty());
        assert_eq!(l.progress().state, State::Routing);
        assert_eq!(l.progress().waiting_for, "transport_active");
        let retry = l.advance(&f, MonoMs(2_002));
        let [Action::ConnectAudio(second)] = retry.as_slice() else { panic!("{retry:?}") };
        assert_ne!(*second, first);
        assert!(l.note_audio(*second, TransportReply::Connected, MonoMs(2_003)));
        f.transport_active = true;
        assert!(l.advance(&f, MonoMs(2_004)).is_empty());
        assert_eq!(l.progress().state, State::Streaming);
    }

    #[test]
    fn losing_output_reenters_acquisition_without_retiring_parallel_codec_work() {
        let mut l = Lifecycle::default();
        let mut f = facts();
        let first = start(&mut l, &mut f);
        let g = graph(&f);
        f.graph = Some(&g);
        f.control = Stage::Ready;
        f.selected_codec = Some("aac");
        f.routed = true;
        f.transport_active = true;
        let codec = codec_request(l.advance(&f, MonoMs(2))).unwrap();
        assert_eq!(l.progress().state, State::Streaming);
        l.note_audio(first, TransportReply::Failed, MonoMs(3));
        assert!(l.advance(&f, MonoMs(4)).is_empty());
        assert_eq!(l.progress().state, State::Streaming);
        f.graph = None;
        assert!(l.advance(&f, MonoMs(5)).is_empty());
        assert_eq!(l.progress().state, State::PreparingPaths);
        assert_eq!(l.progress().waiting_for, "audio_profile");
        let retry = l.advance(&f, MonoMs(2_003));
        let [Action::ConnectAudio(second)] = retry.as_slice() else { panic!("{retry:?}") };
        assert_ne!(*second, first);
        assert!(!l.note_audio(first, TransportReply::Connected, MonoMs(2_004)));
        assert!(l.note_codec(codec, true, MonoMs(2_004)), "the parallel task survived");
        l.note_audio(*second, TransportReply::Connected, MonoMs(2_004));
        l.advance(&f, MonoMs(2_004));
        assert_eq!(l.progress().state, State::CompletingProfiles);
        assert_eq!(l.progress().waiting_for, "output");
        f.graph = Some(&g);
        assert!(l.advance(&f, MonoMs(2_005)).is_empty());
        assert_eq!(l.progress().state, State::Streaming);
    }

    #[test]
    fn early_audio_exhaustion_gets_one_fresh_window_at_control_readiness() {
        let mut l = Lifecycle::default();
        let mut f = facts();
        let first = start(&mut l, &mut f);
        assert!(l.note_audio(first, TransportReply::Busy, MonoMs(2)));
        assert!(l.advance(&f, MonoMs(2_001)).is_empty());
        let next = l.advance(&f, MonoMs(2_002));
        let [Action::ConnectAudio(second)] = next.as_slice() else { panic!("{next:?}") };
        assert!(l.note_audio(*second, TransportReply::TimedOut, MonoMs(30_002)));
        assert_eq!(l.progress().audio.state(), TaskState::Exhausted);
        assert!(l.advance(&f, MonoMs(32_002)).is_empty());
        f.control = Stage::Ready;
        let ready = l.advance(&f, MonoMs(32_003));
        let [Action::ConnectAudio(third)] = ready.as_slice() else { panic!("{ready:?}") };
        assert!(!l.note_audio(*second, TransportReply::Connected, MonoMs(32_004)));
        assert!(l.note_audio(*third, TransportReply::Busy, MonoMs(32_004)));
        let retry = l.advance(&f, MonoMs(34_004));
        let [Action::ConnectAudio(last)] = retry.as_slice() else { panic!("{retry:?}") };
        assert!(l.note_audio(*last, TransportReply::Failed, MonoMs(62_004)));
        assert_eq!(l.progress().audio.state(), TaskState::Exhausted);
        assert!(l.advance(&f, MonoMs(90_000)).is_empty(), "readiness must not reset again");
    }

    #[test]
    fn codec_failures_back_off_and_exhaust_without_blocking_streaming() {
        let mut l = Lifecycle::default();
        let mut f = facts();
        let mut g = graph(&f);
        g.cards[0].profiles.push("sbc".into());
        f.control = Stage::Ready;
        f.may_open = false;
        f.graph = Some(&g);
        f.selected_codec = Some("aac");
        f.routed = true;
        f.transport_active = true;
        let mut last = None;
        for attempt in 0..3 {
            let at = attempt * 1_001;
            let id = codec_request(l.advance(&f, MonoMs(at))).unwrap();
            if let Some(old) = last {
                assert!(!l.note_codec(old, true, MonoMs(at)));
            }
            assert!(l.note_codec(id, false, MonoMs(at + 1)));
            assert!(codec_request(l.advance(&f, MonoMs(at + 1))).is_none());
            assert!(codec_request(l.advance(&f, MonoMs(at + 1_000))).is_none());
            assert_eq!(l.progress().state, State::Streaming);
            last = Some(id);
        }
        assert_eq!(l.progress().codec.state(), TaskState::Exhausted);
        assert_eq!(crate::panel::status::lifecycle(&l.progress())["codec"], "exhausted");
        f.graph = None;
        l.advance(&f, MonoMs(30_000));
        f.graph = Some(&g);
        assert!(
            codec_request(l.advance(&f, MonoMs(30_001))).is_none(),
            "a transient card gap does not replenish the target's budget"
        );
        // A different requested target gets its own budget and retires old replies.
        f.selected_codec = Some("sbc");
        let replacement = codec_request(l.advance(&f, MonoMs(30_002))).unwrap();
        assert!(!l.note_codec(last.unwrap(), true, MonoMs(30_003)));
        assert!(l.note_codec(replacement, true, MonoMs(30_003)));
    }

    #[test]
    fn codec_missing_callbacks_and_missing_graph_confirmation_are_bounded() {
        for acknowledge in [false, true] {
            let mut l = Lifecycle::default();
            let mut f = facts();
            let g = graph(&f);
            f.control = Stage::Ready;
            f.may_open = false;
            f.graph = Some(&g);
            f.selected_codec = Some("aac");
            let mut at = 0;
            let mut old = None;
            for attempt in 0..3 {
                let id = codec_request(l.advance(&f, MonoMs(at))).unwrap();
                if let Some(old) = old {
                    assert!(!l.note_codec(old, true, MonoMs(at)));
                }
                if acknowledge {
                    assert!(l.note_codec(id, true, MonoMs(at + 1)));
                }
                let deadline = at + 5_000 + u64::from(acknowledge);
                assert!(codec_request(l.advance(&f, MonoMs(deadline - 1))).is_none());
                // Recovery prevents launches but does not keep a timed-out request alive.
                f.recovering = true;
                assert!(codec_request(l.advance(&f, MonoMs(deadline))).is_none());
                assert!(!l.note_codec(id, true, MonoMs(deadline)));
                f.recovering = false;
                assert!(codec_request(l.advance(&f, MonoMs(deadline + 999))).is_none());
                assert_eq!(l.progress().codec.state() == TaskState::Exhausted, attempt == 2);
                at = deadline + 1_000;
                old = Some(id);
            }
            assert!(codec_request(l.advance(&f, MonoMs(at + 60_000))).is_none());
            // A delayed graph showing the requested profile is still authoritative.
            let mut applied = g.clone();
            applied.cards[0].active = Some("aac".into());
            f.graph = Some(&applied);
            assert!(codec_request(l.advance(&f, MonoMs(at + 60_001))).is_none());
            assert!(matches!(l.progress().codec.state(), TaskState::Succeeded { .. }));
        }
    }

    #[test]
    fn codec_backoff_is_cancelled_by_release_and_replaced_by_a_new_card() {
        let mut l = Lifecycle::default();
        let mut f = facts();
        let g = graph(&f);
        f.control = Stage::Ready;
        f.may_open = false;
        f.graph = Some(&g);
        f.selected_codec = Some("aac");
        let first = codec_request(l.advance(&f, MonoMs(0))).unwrap();
        l.note_codec(first, false, MonoMs(1));
        f.wants_audio = false;
        assert!(codec_request(l.advance(&f, MonoMs(1_001))).is_none());
        assert!(codec_request(l.advance(&f, MonoMs(60_000))).is_none());
        f.wants_audio = true;
        let second = codec_request(l.advance(&f, MonoMs(60_001))).unwrap();
        assert!(!l.note_codec(first, true, MonoMs(60_002)));
        let mut replacement = g.clone();
        replacement.cards[0].name = "replacement-card".into();
        f.graph = Some(&replacement);
        let third = codec_request(l.advance(&f, MonoMs(60_003))).unwrap();
        assert!(!l.note_codec(second, false, MonoMs(60_004)));
        assert!(l.note_codec(third, true, MonoMs(60_004)));
    }

    #[test]
    fn phases_launch_once_and_background_work_outlives_routing() {
        let mut l = Lifecycle::default();
        let mut f = facts();
        let id = start(&mut l, &mut f);
        assert_eq!(l.progress().state, State::PreparingPaths);
        l.note_profiles(profiles(&f));
        assert!(l.advance(&f, MonoMs(2)).is_empty());
        // Output can appear before the method reply, while control is opening.
        let g = graph(&f);
        f.graph = Some(&g);
        assert!(l.advance(&f, MonoMs(3)).is_empty());
        assert_eq!(l.progress().state, State::Routing);
        assert!(matches!(l.progress().handsfree, Handsfree::Waiting { .. }));
        f.routed = true;
        f.transport_active = true;
        assert!(l.advance(&f, MonoMs(4)).is_empty());
        assert_eq!(l.progress().state, State::Streaming);
        assert!(l.note_audio(id, TransportReply::Connected, MonoMs(5)));
        assert_eq!(l.advance(&f, MonoMs(5)), vec![Action::CompleteProfiles(id)]);
        assert_eq!(l.progress().state, State::Streaming);
        l.note_handsfree(id, false);
        assert!(l.advance(&f, MonoMs(6)).is_empty());
        assert_eq!(l.progress().state, State::Streaming);
        f.recovering = true;
        l.advance(&f, MonoMs(7));
        assert_eq!(l.progress().state, State::Routing);
        assert_eq!(l.progress().waiting_for, "stream_route");
    }
    #[test]
    fn hfp_joins_success_discovery_and_idle_worker_in_either_order() {
        for discovery_first in [false, true] {
            let mut l = Lifecycle::default();
            let mut f = facts();
            let id = start(&mut l, &mut f);
            if discovery_first {
                l.note_profiles(profiles(&f));
            } else {
                l.note_audio(id, TransportReply::Connected, MonoMs(2));
            }
            assert!(l.advance(&f, MonoMs(2)).is_empty());
            if discovery_first {
                l.note_audio(id, TransportReply::Connected, MonoMs(3));
            }
            let mut p = profiles(&f);
            p.busy = true;
            l.note_profiles(p);
            assert!(l.advance(&f, MonoMs(3)).is_empty());
            p.busy = false;
            l.note_profiles(p);
            assert_eq!(l.advance(&f, MonoMs(4)), vec![Action::CompleteProfiles(id)]);
            assert_eq!(l.progress().state, State::CompletingProfiles);
            assert_eq!(l.progress().waiting_for, "output");
            assert!(l.advance(&f, MonoMs(5)).is_empty());
        }
    }
    #[test]
    fn unsupported_handsfree_does_not_hold_output_and_release_retires_pending_work() {
        let mut l = Lifecycle::default();
        let mut f = facts();
        let id = start(&mut l, &mut f);
        l.note_audio(id, TransportReply::Connected, MonoMs(2));
        let mut p = profiles(&f);
        p.handsfree = false;
        l.note_profiles(p);
        assert!(l.advance(&f, MonoMs(3)).is_empty());
        assert!(matches!(l.progress().handsfree, Handsfree::Unsupported));
        let g = graph(&f);
        f.graph = Some(&g);
        f.routed = true;
        f.transport_active = true;
        assert!(l.advance(&f, MonoMs(4)).is_empty());
        assert_eq!(l.progress().state, State::Streaming);
        f.wants_audio = false;
        l.advance(&f, MonoMs(5));
        assert!(!l.note_audio(id, TransportReply::Connected, MonoMs(6)));
        assert_ne!(l.progress().state, State::Streaming);
    }

    #[test]
    fn claims_and_generations_retire_pending_profiles() {
        for replacement in ["claim", "generation", "accessory"] {
            let mut l = Lifecycle::default();
            let mut f = facts();
            let old = start(&mut l, &mut f);
            match replacement {
                "generation" => f.generation.0 += 1,
                "accessory" => f.accessory = Some(Address::parse("11:22:33:44:55:66").unwrap()),
                _ => l.reset_audio(),
            }
            let actions = l.advance(&f, MonoMs(2));
            let new = actions
                .iter()
                .find_map(|a| match a {
                    Action::ConnectAudio(id) => Some(*id),
                    _ => None,
                })
                .unwrap();
            assert_ne!(old, new);
            assert!(!l.note_audio(old, TransportReply::Connected, MonoMs(3)));
            assert!(l.note_audio(new, TransportReply::Connected, MonoMs(3)));
            l.note_profiles(profiles(&f));
            assert_eq!(l.advance(&f, MonoMs(4)), vec![Action::CompleteProfiles(new)]);
            l.note_handsfree(old, true);
            assert!(matches!(l.progress().handsfree, Handsfree::Connecting { id } if *id == new));
        }
    }
    #[test]
    fn background_links_and_cancellation_do_not_start_audio() {
        let mut l = Lifecycle::default();
        let mut f = facts();
        f.wants_audio = false;
        assert_eq!(l.advance(&f, MonoMs(0)), vec![Action::OpenLink(f.generation)]);
        f.control = Stage::Ready;
        f.bluetooth = Some(true);
        assert!(l.advance(&f, MonoMs(1)).is_empty());
        assert_eq!(l.progress().state, State::ControlReady);
        f.wants_audio = true;
        let actions = l.advance(&f, MonoMs(2));
        let [Action::ConnectAudio(id)] = actions.as_slice() else { panic!("{actions:?}") };
        f.control = Stage::Interrupted;
        assert!(l.advance(&f, MonoMs(3)).is_empty());
        assert!(!l.note_audio(*id, TransportReply::Connected, MonoMs(4)));
        assert_eq!(l.progress().state, State::Interrupted);
    }
    #[test]
    fn codec_lifetime_covers_replacement_cancellation_and_audio_server_loss() {
        let mut l = Lifecycle::default();
        let mut f = facts();
        start(&mut l, &mut f);
        let mut g = graph(&f);
        g.cards[0].profiles.push("sbc".into());
        f.graph = Some(&g);
        f.control = Stage::Ready;
        f.selected_codec = Some("aac");
        let select = |actions: Vec<Action>| {
            actions
                .into_iter()
                .find_map(|a| match a {
                    Action::SelectCodec { id, .. } => Some(id),
                    _ => None,
                })
                .unwrap()
        };
        let old = select(l.advance(&f, MonoMs(2)));
        assert!(l.advance(&f, MonoMs(3)).is_empty());
        f.selected_codec = Some("sbc");
        let new = select(l.advance(&f, MonoMs(4)));
        assert!(!l.note_codec(old, false, MonoMs(4)));
        assert!(l.note_codec(new, true, MonoMs(4)));
        assert!(matches!(l.progress().codec.state(), TaskState::Confirming { .. }));
        assert!(l.advance(&f, MonoMs(5)).is_empty());
        let mut applied = g.clone();
        applied.cards[0].active = Some("sbc".into());
        f.graph = Some(&applied);
        l.advance(&f, MonoMs(6));
        assert!(matches!(l.progress().codec.state(), TaskState::Succeeded { .. }));
        // The router owns card changes during recovery, even if the profile
        // temporarily differs from the chosen codec.
        f.graph = Some(&g);
        f.recovering = true;
        assert!(l.advance(&f, MonoMs(7)).is_empty());
        f.recovering = false;
        // An externally changed profile then requires a fresh selection.
        let before_loss = select(l.advance(&f, MonoMs(7)));
        l.audio_lost();
        f.graph = None;
        l.advance(&f, MonoMs(8));
        assert!(!l.note_codec(before_loss, true, MonoMs(8)));
        f.graph = Some(&g);
        let after_loss = select(l.advance(&f, MonoMs(9)));
        l.reset_audio();
        assert!(!l.note_codec(after_loss, false, MonoMs(9)));
    }
}
