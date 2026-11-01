//! Local Bluetooth and control-socket subtask of `pods::lifecycle`.
//! This is the only retry loop for the local link. Routing must not start another.

use crate::retry::{Delay, Failure, Limit, Policy, Requests, State as TaskState, Task};
use crate::types::{Generation, MonoMs};

pub use super::setup::{HANDSHAKE_WAIT_MS, NOTIFY_WAIT_MS, SETUP_BUDGET_MS};
use super::{
    observe::Observation,
    setup::{FollowUp, Setup},
};
use crate::types::Address;
pub const RETRY_LIMIT: u8 = 5;
/// BlueZ and the controller need a moment after wake or a service return.
pub const RESUME_GRACE_MS: u64 = 2_000;
/// Let the pods finish reconnecting their previous host after the case opens.
pub const CASE_WAKE_GRACE_MS: u64 = 2_000;
const BACKOFF_MS: [u64; 5] = [1_000, 2_000, 4_000, 8_000, 16_000];
/// How long to leave a profile connect alone when BlueZ says one is already running.
const BUSY_WAIT_MS: u64 = 5_000;
pub(crate) const BACKGROUND_REOPEN_DELAY_MS: u64 = 3_000;
const BACKGROUND_REOPEN_LIMIT: u8 = 3;

fn opening_task(limit: u8) -> Task {
    Task::new(Policy {
        delay: Delay::Staged(&BACKOFF_MS),
        limit: Limit::Attempts(u32::from(limit)),
        // Each explicit protocol stage owns its deadline.
        request_timeout_ms: None,
        confirmation_timeout_ms: None,
    })
}

/// Control setup/backoff progress, not connection-to-playback readiness.
/// The overall lifecycle lives in `pods::lifecycle::State`; legacy status uses [`Stage::wire`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Idle,
    Waiting,
    Connecting,
    Handshake,
    Features,
    Notifications,
    Ready,
    Retrying,
    Failed,
    Interrupted,
}

impl Stage {
    pub fn wire(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Waiting => "waiting",
            Self::Connecting => "connecting",
            Self::Handshake => "handshake",
            Self::Features => "features",
            Self::Notifications => "notifications",
            Self::Ready => "ready",
            Self::Retrying => "retrying",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
        }
    }

    /// BlueZ connect is in flight. A retry that has not started again is not opening.
    pub fn is_opening(self) -> bool {
        matches!(self, Self::Connecting | Self::Handshake | Self::Features | Self::Notifications)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailReason {
    /// BlueZ `InProgress` while a profile operation is still running.
    InProgress,
    /// BlueZ was already connecting a profile. This wait does not spend an attempt.
    Busy,
    Transport,
    Setup,
}

impl FailReason {
    pub fn label(self) -> &'static str {
        match self {
            Self::InProgress => "in_progress",
            Self::Busy => "busy",
            Self::Transport => "transport",
            Self::Setup => "setup",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Claim {
    Started(Generation),
    AlreadyUnderway,
}

enum Live {
    Idle,
    Waiting,
    Setup {
        failure: Option<FailReason>,
    },
    Ready,
    /// Deliberate socket release is timed by the Apple reconnect cadence.
    Releasing,
    Interrupted,
}

pub struct Connection {
    generation: Generation,
    open_at: MonoMs,
    /// `None` until BlueZ has reported. That is not the same as "down".
    bluez: Option<bool>,
    /// `None` until this daemon has observed the control socket.
    socket: Option<bool>,
    live: Live,
    setup: Setup,
    task: Task,
    requests: Requests,
    profile_pending: bool,
    reconnect: super::reconnect::Reconnect,
    peer_reconnect_error: &'static str,
    interrupt_reason: &'static str,
    /// Set when an active link was interrupted by something other than the user.
    resumable: bool,
    resume_at: Option<MonoMs>,
    /// Earliest time a claimed seat may open again after a terminal failure.
    reopen_at: Option<MonoMs>,
}

impl Connection {
    pub fn new() -> Self {
        Self {
            generation: Generation(0),
            open_at: MonoMs(0),
            bluez: None,
            socket: None,
            live: Live::Idle,
            setup: Setup::new(),
            task: opening_task(RETRY_LIMIT),
            requests: Requests::default(),
            profile_pending: false,
            reconnect: super::reconnect::Reconnect::new(),
            peer_reconnect_error: "",
            interrupt_reason: "",
            resumable: false,
            resume_at: None,
            reopen_at: None,
        }
    }

    pub fn generation(&self) -> Generation {
        self.generation
    }

    pub fn bluez(&self) -> Option<bool> {
        self.bluez
    }

    pub fn socket_up(&self) -> Option<bool> {
        self.socket
    }

    pub fn apple_reconnect(&self) -> bool {
        self.reconnect.active()
    }

    /// Only the deliberate release suppresses link failures. Rejoining uses
    /// the normal bounded setup retries while the transaction remains active.
    pub fn apple_reconnect_releasing(&self) -> bool {
        self.reconnect.releasing()
    }

    pub fn peer_reconnect_error(&self) -> &'static str {
        self.peer_reconnect_error
    }

    pub fn stage(&self) -> Stage {
        match self.live {
            Live::Idle => Stage::Idle,
            Live::Waiting => Stage::Waiting,
            Live::Setup { .. } => match self.task.state() {
                TaskState::Running { .. } => self.setup.stage(),
                TaskState::Backoff { .. } | TaskState::Deferred { .. } => Stage::Retrying,
                TaskState::Exhausted => Stage::Failed,
                _ => unreachable!("setup must have a live or exhausted attempt"),
            },
            Live::Ready => Stage::Ready,
            Live::Releasing => Stage::Connecting,
            Live::Interrupted => Stage::Interrupted,
        }
    }

    pub fn failed_stage(&self) -> &'static str {
        match self.live {
            Live::Setup { failure: Some(_) } => self.setup.stage().wire(),
            _ => "idle",
        }
    }

    pub fn reason(&self) -> &'static str {
        match self.live {
            Live::Setup { failure: Some(reason), .. } => reason.label(),
            Live::Interrupted => self.interrupt_reason,
            _ => "",
        }
    }

    /// Attempt count and the active operation's limit. Idle/terminal counts stay zero.
    pub fn retry(&self) -> (u8, u8) {
        let (attempts, Limit::Attempts(limit)) = self.task.budget() else {
            unreachable!("link attempts always have a count budget");
        };
        let count = if self.stage().is_opening() || self.stage() == Stage::Retrying {
            attempts as u8
        } else {
            0
        };
        (count, limit as u8)
    }

    pub fn recovering(&self) -> bool {
        matches!(self.stage(), Stage::Retrying | Stage::Interrupted)
            || (self.stage().is_opening() && self.retry().0 > 1)
    }

    pub fn deadline(&self) -> Option<MonoMs> {
        if let Live::Setup { .. } = self.live {
            match self.task.state() {
                TaskState::Running { .. } => self.setup.deadline(),
                TaskState::Backoff { until } | TaskState::Deferred { until } => Some(until),
                _ => None,
            }
        } else {
            None
        }
    }

    pub fn note_bluez(&mut self, up: bool) {
        self.bluez = Some(up);
    }

    pub fn note_profile_pending(&mut self, pending: bool) {
        self.profile_pending = pending;
    }

    /// Activity prepares a link and does not open one that is already in motion.
    pub fn prepare(&mut self) {
        if self.stage().is_opening()
            || matches!(self.stage(), Stage::Retrying | Stage::Ready | Stage::Waiting)
        {
            return;
        }
        self.task.reset();
        self.live = Live::Waiting;
    }

    pub fn may_open(&self, now: MonoMs) -> bool {
        matches!(self.live, Live::Setup { .. })
            && matches!(self.task.state(), TaskState::Running { .. })
            && now >= self.open_at
    }

    pub fn claim(&mut self, now: MonoMs) -> Claim {
        self.claim_after(now, now)
    }

    pub fn claim_after(&mut self, now: MonoMs, not_before: MonoMs) -> Claim {
        self.claim_with_budget(now, not_before, RETRY_LIMIT)
    }

    /// One bounded background operation, not an outer loop around link retries.
    /// The session admits it using availability, preference and user intent.
    pub(crate) fn reopen_background(&mut self, now: MonoMs) -> Claim {
        if self.reason() == "disconnect" {
            return Claim::AlreadyUnderway;
        }
        self.claim_with_budget(
            now,
            now.saturating_add(BACKGROUND_REOPEN_DELAY_MS),
            BACKGROUND_REOPEN_LIMIT,
        )
    }

    fn claim_with_budget(&mut self, now: MonoMs, not_before: MonoMs, limit: u8) -> Claim {
        if self.apple_reconnect()
            || self.stage().is_opening()
            || matches!(self.stage(), Stage::Retrying | Stage::Ready)
        {
            return Claim::AlreadyUnderway;
        }
        self.task = opening_task(limit);
        self.reopen_at = None;
        self.launch(Generation(self.generation.0 + 1), now, not_before);
        Claim::Started(self.generation)
    }

    /// All fresh, resumed and retried socket attempts enter setup here.
    fn launch(&mut self, generation: Generation, now: MonoMs, not_before: MonoMs) {
        if self.task.launch(&mut self.requests, generation, now).is_none() {
            return;
        }
        self.generation = generation;
        self.open_at = now.max(not_before);
        self.setup.connect(self.open_at);
        self.live = Live::Setup { failure: None };
    }

    /// Park until an eligible request, retiring callbacks and retry deadlines.
    /// An established link may finish its normal shutdown on its own.
    pub fn wait_for_request(&mut self) -> bool {
        if matches!(self.live, Live::Waiting | Live::Ready) {
            return false;
        }
        let cancel = self.stage().is_opening() || self.stage() == Stage::Retrying;
        self.generation = Generation(self.generation.0 + 1);
        self.socket = Some(false);
        self.reconnect.cancel();
        self.resumable = false;
        self.resume_at = None;
        self.reopen_at = None;
        self.task.reset();
        self.live = Live::Waiting;
        cancel
    }

    /// Start the protocol once for this socket. Duplicate socket notifications
    /// cannot rewind a handshake or extend its deadline.
    pub(crate) fn begin_setup(
        &mut self,
        generation: Generation,
        now: MonoMs,
    ) -> Option<&'static [u8]> {
        if generation != self.generation
            || self.socket != Some(true)
            || matches!(self.live, Live::Ready | Live::Releasing)
            || self.reason() == "disconnect"
        {
            return None;
        }
        if !matches!(self.live, Live::Setup { .. }) {
            self.task = opening_task(RETRY_LIMIT);
            self.launch(generation, now, now);
        }
        if !matches!(self.task.state(), TaskState::Running { .. })
            || self.setup.stage() != Stage::Connecting
        {
            return None;
        }
        Some(self.setup.start(now))
    }

    pub(crate) fn observe(
        &mut self,
        packet: &[u8],
        generation: Generation,
        local: Option<Address>,
        now: MonoMs,
    ) -> (Option<FollowUp>, Vec<Observation>) {
        if generation != self.generation {
            return (None, Vec::new());
        }
        // Reports remain useful outside an opening attempt (for example keys
        // received while the keyring is locked). Only a live attempt may
        // advance setup or emit its follow-up writes.
        let setup_time = (matches!(self.live, Live::Setup { .. })
            && matches!(self.task.state(), TaskState::Running { .. }))
        .then_some(now);
        super::observe::translate(&mut self.setup, packet, local, setup_time)
    }

    pub fn mark_ready(&mut self, generation: Generation) -> bool {
        if generation != self.generation || self.socket != Some(true) {
            return false;
        }
        if !matches!(self.live, Live::Setup { .. }) {
            return false;
        }
        if !matches!(self.task.state(), TaskState::Running { .. }) {
            return false;
        }
        self.task.observed_success();
        self.live = Live::Ready;
        self.reconnect.connected();
        self.peer_reconnect_error = "";
        true
    }

    /// `true` when a ready link was dropped and the caller should clear the seat.
    pub fn note_socket(&mut self, up: bool, generation: Generation) -> bool {
        if generation != self.generation {
            return false;
        }
        self.socket = Some(up);
        if up {
            return false;
        }
        if self.apple_reconnect_releasing() {
            if let Live::Ready | Live::Setup { .. } = self.live {
                self.task.reset();
                self.live = Live::Releasing;
            }
            return false;
        }
        if matches!(self.live, Live::Ready) {
            self.interrupt("link_lost");
            return true;
        }
        false
    }

    /// `intent_valid` is false after the user released the seat. A scheduled
    /// retry is not a terminal failure, and a terminal failure is not also a retry.
    pub fn setup_failed(
        &mut self,
        generation: Generation,
        reason: FailReason,
        now: MonoMs,
        intent_valid: bool,
    ) {
        if generation != self.generation {
            return;
        }
        if reason == FailReason::InProgress && self.profile_pending {
            return;
        }
        let TaskState::Running { id, .. } = self.task.state() else { return };
        let Live::Setup { failure, .. } = &mut self.live else { return };
        *failure = Some(reason);
        let action = if reason == FailReason::Busy {
            Failure::Defer(BUSY_WAIT_MS)
        } else if !intent_valid {
            Failure::Stop
        } else {
            Failure::Retry
        };
        self.task.fail(id, action, now);
        if self.task.state() == TaskState::Exhausted {
            self.reopen_at = Some(now.saturating_add(BACKOFF_MS[4]));
        }
    }

    /// Arm the grace. Returns whether a reconnect is now waiting.
    pub fn arm_resume(&mut self, at: MonoMs) -> bool {
        if self.resumable && matches!(self.live, Live::Interrupted) {
            self.resume_at = Some(at);
        }
        self.resume_at.is_some()
    }

    pub fn on_tick(&mut self, now: MonoMs, intent_valid: bool) {
        if let Some(at) = self.resume_at {
            if now >= at {
                self.resume_at = None;
                if intent_valid && self.resumable {
                    if let Live::Interrupted = self.live {
                        self.resumable = false;
                        self.task = opening_task(RETRY_LIMIT);
                        self.launch(Generation(self.generation.0 + 1), now, now);
                        return;
                    }
                }
            }
        }
        if self.stage() == Stage::Failed
            && intent_valid
            && self.reopen_at.is_none_or(|at| now >= at)
        {
            if let Claim::Started(_) = self.claim(now) {
                return;
            }
        }
        if self.stage().is_opening() && self.deadline().is_some_and(|at| now >= at) {
            self.setup_failed(self.generation, FailReason::Setup, now, intent_valid);
        }
        if self.stage() != Stage::Retrying {
            return;
        }
        self.launch(Generation(self.generation.0 + 1), now, now);
    }

    pub fn interrupt(&mut self, reason: &'static str) {
        if matches!(self.live, Live::Interrupted) {
            if reason == "disconnect" {
                self.resumable = false;
                self.resume_at = None;
                self.interrupt_reason = reason;
            } else if self.resumable {
                self.interrupt_reason = reason;
            }
            return;
        }
        let active =
            self.stage().is_opening() || matches!(self.stage(), Stage::Ready | Stage::Retrying);
        self.resume_at = None;
        self.interrupt_reason = reason;
        if !active && reason != "disconnect" {
            self.resumable = false;
            return;
        }
        self.generation = Generation(self.generation.0 + 1);
        self.socket = Some(false);
        self.reconnect.cancel();
        self.resumable = active && !matches!(reason, "disconnect" | "link_lost");
        self.task.reset();
        self.live = Live::Interrupted;
    }

    pub fn begin_apple_reconnect(&mut self, now: MonoMs) -> Result<(), &'static str> {
        if self.apple_reconnect() {
            return Err("error: reconnect already in progress\n");
        }
        if !matches!(self.live, Live::Ready)
            || self.socket != Some(true)
            || self.bluez != Some(true)
        {
            return Err("error: wait for the AirPods connection\n");
        }
        self.reconnect.begin(now);
        self.peer_reconnect_error = "";
        Ok(())
    }

    fn fail_apple_reconnect(&mut self, message: &'static str) {
        self.reconnect.cancel();
        if self.socket != Some(true) {
            self.interrupt("link_lost");
        }
        self.peer_reconnect_error = message;
    }

    /// The reconnect cadence and the link generation change have one owner.
    /// Playback supplies only peer/player facts and consumes external actions.
    pub fn advance_reconnect(
        &mut self,
        now: MonoMs,
        quiet: bool,
        peers_left: bool,
    ) -> super::reconnect::Advance {
        use super::reconnect::{Advance, Facts};
        let action = self.reconnect.advance(
            now,
            Facts {
                quiet,
                peers_left,
                link_down: self.socket != Some(true) || self.bluez == Some(false),
                link_ready: self.stage() == Stage::Ready
                    && self.socket == Some(true)
                    && self.bluez != Some(false),
                attempting: self.stage().is_opening() || self.stage() == Stage::Retrying,
            },
        );
        match action {
            Advance::Rejoin => self.reopen_after_reconnect(now),
            Advance::Fail(reason) => self.fail_apple_reconnect(reason),
            _ => {}
        }
        action
    }

    fn reopen_after_reconnect(&mut self, now: MonoMs) {
        self.task = opening_task(RETRY_LIMIT);
        self.launch(Generation(self.generation.0 + 1), now, now);
    }
}

impl Default for Connection {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
pub(crate) fn reach_stage(
    link: &mut Connection,
    stage: Stage,
    generation: Generation,
    now: MonoMs,
) -> bool {
    if generation != link.generation() {
        return false;
    }
    if link.stage() == Stage::Connecting {
        link.note_socket(true, generation);
        if link.begin_setup(generation, now).is_none() {
            return false;
        }
    }
    if stage != Stage::Handshake && link.stage() == Stage::Handshake {
        link.observe(&[1, 0, 4, 0, 0, 0], generation, None, now);
    }
    if stage == Stage::Notifications && link.stage() == Stage::Features {
        link.observe(&[4, 0, 4, 0, 0x2b, 0], generation, None, now);
    }
    link.stage() == stage
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tick(link: &mut Connection, now: MonoMs, intended: bool) -> Option<(Generation, u8)> {
        let previous = link.generation();
        link.on_tick(now, intended);
        (link.generation() != previous && link.stage().is_opening())
            .then(|| (link.generation(), link.retry().0))
    }

    #[test]
    fn packets_advance_setup_once_and_readiness_commits_after_reports() {
        let mut link = Connection::new();
        link.claim(MonoMs(0));
        let gen = link.generation();
        link.note_socket(true, gen);
        assert!(link.begin_setup(gen, MonoMs(100)).is_some());
        assert_eq!(link.deadline(), Some(MonoMs(100 + HANDSHAKE_WAIT_MS)));
        let handshake = [1, 0, 4, 0, 0, 0];
        assert_eq!(link.observe(&handshake, gen, None, MonoMs(200)).0, Some(FollowUp::Features));
        assert_eq!(link.stage(), Stage::Features);
        assert_eq!(link.deadline(), Some(MonoMs(200 + SETUP_BUDGET_MS)));
        // A repeated socket-up or acknowledgement cannot rewind or extend setup.
        link.note_socket(true, gen);
        assert!(link.begin_setup(gen, MonoMs(300)).is_none());
        assert_eq!(link.observe(&handshake, gen, None, MonoMs(300)), (None, vec![]));
        assert_eq!(link.deadline(), Some(MonoMs(200 + SETUP_BUDGET_MS)));
        assert_eq!(
            link.observe(&[4, 0, 4, 0, 0x2b, 0], gen, None, MonoMs(400)).0,
            Some(FollowUp::Notifications)
        );
        let battery = [4, 0, 4, 0, 4, 0, 1, 4, 1, 80, 2, 1];
        link.observe(&battery, gen, None, MonoMs(500));
        let roster = [4, 0, 4, 0, 0x2e, 0, 1, 2, 0];
        let (_, reports) = link.observe(&roster, gen, None, MonoMs(600));
        assert!(reports.contains(&Observation::Ready));
        assert_eq!(link.stage(), Stage::Notifications);
        assert_eq!(link.deadline(), Some(MonoMs(400 + NOTIFY_WAIT_MS)));
        assert!(link.mark_ready(gen));
        assert_eq!(link.stage(), Stage::Ready);
        assert_eq!(link.deadline(), None);
        assert!(!link.observe(&roster, gen, None, MonoMs(700)).1.contains(&Observation::Ready));
    }

    #[test]
    fn retired_attempts_report_facts_without_advancing_the_handshake() {
        let mut link = Connection::new();
        link.claim(MonoMs(0));
        let gen = link.generation();
        link.note_socket(true, gen);
        link.begin_setup(gen, MonoMs(0));
        link.setup_failed(gen, FailReason::Transport, MonoMs(10), true);
        let deadline = link.deadline();
        assert_eq!(link.observe(&[1, 0, 4, 0, 0, 0], gen, None, MonoMs(20)), (None, vec![]));
        assert_eq!(link.failed_stage(), "handshake");
        assert_eq!(link.deadline(), deadline);
        let ownership = [4, 0, 4, 0, 9, 0, 6, 1, 0, 0, 0];
        assert_eq!(
            link.observe(&ownership, gen, None, MonoMs(20)).1,
            vec![Observation::Ownership(true)]
        );
        link.on_tick(deadline.unwrap(), true);
        assert_ne!(link.generation(), gen);
        assert_eq!(link.observe(&ownership, gen, None, MonoMs(2_000)), (None, vec![]));
        assert_eq!(link.stage(), Stage::Connecting);
    }

    #[test]
    fn protocol_stages_share_one_budget_and_keep_their_own_deadlines() {
        let mut link = Connection::new();
        link.claim_after(MonoMs(0), MonoMs(2_000));
        assert!(!link.may_open(MonoMs(1_999)));
        assert_eq!(link.deadline(), Some(MonoMs(2_000 + SETUP_BUDGET_MS)));
        let mut now = MonoMs(2_000);
        for attempt in 1..=RETRY_LIMIT {
            let generation = link.generation();
            assert_eq!(link.retry(), (attempt, RETRY_LIMIT));
            for (stage, wait) in [
                (Stage::Handshake, HANDSHAKE_WAIT_MS),
                (Stage::Features, SETUP_BUDGET_MS),
                (Stage::Notifications, NOTIFY_WAIT_MS),
            ] {
                assert!(reach_stage(&mut link, stage, generation, now));
                assert_eq!(link.deadline(), Some(now.saturating_add(wait)));
                assert_eq!(link.retry(), (attempt, RETRY_LIMIT));
                now = now.saturating_add(wait - 1);
                assert_eq!(tick(&mut link, now, true), None);
                assert_eq!(link.stage(), stage);
            }
            now = now.saturating_add(1);
            assert_eq!(tick(&mut link, now, true), None);
            assert_eq!(link.failed_stage(), "notifications");
            assert_eq!(link.reason(), "setup");
            // Neither completion nor attachment can revive a retired attempt.
            link.note_socket(true, generation);
            assert!(link.begin_setup(generation, now).is_none());
            assert!(!link.mark_ready(generation));
            if attempt < RETRY_LIMIT {
                now = now.saturating_add(BACKOFF_MS[usize::from(attempt - 1)]);
                assert_eq!(link.deadline(), Some(now));
                assert_eq!(tick(&mut link, MonoMs(now.0 - 1), true), None);
                assert!(tick(&mut link, now, true).is_some());
                assert!(!reach_stage(&mut link, Stage::Handshake, generation, now));
            } else {
                assert_eq!(link.stage(), Stage::Failed);
                assert_eq!(link.deadline(), None);
            }
        }
    }

    #[test]
    fn delayed_background_reopen_has_one_budget_and_explicit_requests_can_start_fresh() {
        let mut link = Connection::new();
        assert!(matches!(link.reopen_background(MonoMs(0)), Claim::Started(_)));
        assert!(!link.may_open(MonoMs(BACKGROUND_REOPEN_DELAY_MS - 1)));
        assert!(link.may_open(MonoMs(BACKGROUND_REOPEN_DELAY_MS)));
        let mut now = MonoMs(BACKGROUND_REOPEN_DELAY_MS);
        for attempt in 1..=BACKGROUND_REOPEN_LIMIT {
            let generation = link.generation();
            assert_eq!(link.retry(), (attempt, BACKGROUND_REOPEN_LIMIT));
            link.setup_failed(generation, FailReason::Transport, now, true);
            if attempt < BACKGROUND_REOPEN_LIMIT {
                assert_eq!(link.reopen_background(now), Claim::AlreadyUnderway);
                assert_eq!(link.claim(now), Claim::AlreadyUnderway);
                assert_eq!(link.retry(), (attempt, BACKGROUND_REOPEN_LIMIT));
                now = link.deadline().unwrap();
                assert!(tick(&mut link, now, true).is_some());
            }
        }
        assert_eq!(link.stage(), Stage::Failed);
        assert_eq!(tick(&mut link, now.saturating_add(60_000), false), None);
        assert!(matches!(link.claim(now), Claim::Started(_)));
        assert_eq!(link.retry(), (1, RETRY_LIMIT));
    }

    #[test]
    fn cancellation_retires_busy_waits_and_scheduled_background_opens() {
        for busy in [false, true] {
            let mut link = Connection::new();
            link.reopen_background(MonoMs(0));
            let generation = link.generation();
            if busy {
                link.setup_failed(generation, FailReason::Busy, MonoMs(3_000), true);
            }
            link.interrupt("disconnect");
            assert_eq!(link.reopen_background(MonoMs(4_000)), Claim::AlreadyUnderway);
            link.note_socket(true, generation);
            assert!(link.begin_setup(generation, MonoMs(4_000)).is_none());
            assert_eq!(tick(&mut link, MonoMs(60_000), true), None);
            assert_eq!(link.reason(), "disconnect");
            link.claim(MonoMs(60_000));
            assert_eq!(link.retry(), (1, RETRY_LIMIT));
            link.wait_for_request();
            assert_eq!(tick(&mut link, MonoMs(120_000), true), None);
            assert_eq!(link.stage(), Stage::Waiting);
        }
    }

    #[test]
    fn in_progress_during_a_profile_change_is_not_a_control_failure() {
        let mut link = Connection::new();
        let gen = match link.claim(MonoMs(0)) {
            Claim::Started(gen) => gen,
            Claim::AlreadyUnderway => panic!("claim should start"),
        };
        link.note_profile_pending(true);
        link.setup_failed(gen, FailReason::InProgress, MonoMs(10), true);
        assert_eq!(link.stage(), Stage::Connecting);
        link.note_profile_pending(false);
        link.note_socket(true, gen);
        assert!(link.mark_ready(gen));
        assert_eq!(link.stage(), Stage::Ready);
        assert_eq!(link.reason(), "");
    }

    #[test]
    fn a_transient_failure_retries_and_a_stale_completion_does_not_land() {
        let mut link = Connection::new();
        let first = match link.claim(MonoMs(0)) {
            Claim::Started(gen) => gen,
            Claim::AlreadyUnderway => panic!("claim should start"),
        };
        link.setup_failed(first, FailReason::Transport, MonoMs(10), true);
        assert_eq!(link.stage(), Stage::Retrying);
        assert!(link.recovering());
        assert!(!reach_stage(&mut link, Stage::Handshake, first, MonoMs(0)));
        let Some((generation, attempt)) = tick(&mut link, MonoMs(2_000), true) else {
            panic!("retry should open");
        };
        assert_eq!(attempt, 2);
        assert!(link.recovering());
        assert!(generation != first);
        assert!(!link.mark_ready(first));
        link.note_socket(true, generation);
        assert!(link.mark_ready(generation));
        assert_eq!(link.stage(), Stage::Ready);
    }

    #[test]
    fn the_budget_ends_in_a_terminal_failure_that_is_not_also_a_retry() {
        let mut link = Connection::new();
        let mut gen = match link.claim(MonoMs(0)) {
            Claim::Started(gen) => gen,
            Claim::AlreadyUnderway => panic!("claim should start"),
        };
        for _ in 0..RETRY_LIMIT {
            link.setup_failed(gen, FailReason::Setup, MonoMs(0), true);
            if link.stage() == Stage::Failed {
                break;
            }
            let Some((generation, _)) = tick(&mut link, MonoMs(20_000), true) else {
                panic!("retry should open");
            };
            gen = generation;
        }
        assert_eq!(link.stage(), Stage::Failed);
        assert_eq!(tick(&mut link, MonoMs(1_000), true), None);
    }

    #[test]
    fn an_open_socket_is_not_a_reason_to_connect_again() {
        let mut link = Connection::new();
        let gen = match link.claim(MonoMs(0)) {
            Claim::Started(gen) => gen,
            Claim::AlreadyUnderway => panic!("claim should start"),
        };
        assert!(reach_stage(&mut link, Stage::Handshake, gen, MonoMs(0)));
        link.note_socket(true, gen);
        assert_eq!(tick(&mut link, MonoMs(1_000), true), None);
        assert_eq!(link.stage(), Stage::Handshake);
        assert_eq!(tick(&mut link, MonoMs(HANDSHAKE_WAIT_MS), true), None);
        assert_eq!(link.stage(), Stage::Retrying);
    }

    #[test]
    fn an_intentional_apple_drop_is_not_an_unexpected_failure() {
        let mut link = Connection::new();
        let gen = match link.claim(MonoMs(0)) {
            Claim::Started(gen) => gen,
            Claim::AlreadyUnderway => panic!("claim should start"),
        };
        link.note_bluez(true);
        link.note_socket(true, gen);
        assert!(link.mark_ready(gen));
        assert!(link.begin_apple_reconnect(MonoMs(0)).is_ok());
        link.note_socket(false, gen);
        assert!(link.apple_reconnect());
        assert_ne!(link.stage(), Stage::Failed);
        assert_eq!(link.reason(), "");
        use super::super::reconnect::Advance;
        assert_eq!(link.advance_reconnect(MonoMs(0), true, true), Advance::SendRequests);
        assert_eq!(link.advance_reconnect(MonoMs(0), true, true), Advance::DropLink);
        assert_eq!(link.advance_reconnect(MonoMs(0), true, true), Advance::None);
        assert_eq!(link.advance_reconnect(MonoMs(5_000), true, true), Advance::Rejoin);
        assert!(link.generation() != gen);
        assert!(link.may_open(MonoMs(5_000)));
        assert_eq!(link.deadline(), Some(MonoMs(5_000 + SETUP_BUDGET_MS)));
        link.note_socket(false, link.generation());
        assert_eq!(link.deadline(), Some(MonoMs(5_000 + SETUP_BUDGET_MS)));
        link.fail_apple_reconnect("link did not return");
        assert_eq!(link.stage(), Stage::Interrupted);
        assert_eq!(link.deadline(), None);
        assert!(matches!(link.claim(MonoMs(30_000)), Claim::Started(_)));
    }

    #[test]
    fn an_apple_rejoin_retry_can_finish_without_claiming_again() {
        let mut link = Connection::new();
        link.claim(MonoMs(0));
        link.note_bluez(true);
        link.note_socket(true, link.generation());
        assert!(link.mark_ready(link.generation()));
        link.begin_apple_reconnect(MonoMs(0)).unwrap();
        link.note_socket(false, link.generation());
        use super::super::reconnect::Advance;
        assert_eq!(link.advance_reconnect(MonoMs(0), true, true), Advance::SendRequests);
        assert_eq!(link.advance_reconnect(MonoMs(0), true, true), Advance::DropLink);
        assert_eq!(link.advance_reconnect(MonoMs(0), true, true), Advance::None);
        assert_eq!(link.advance_reconnect(MonoMs(5_000), true, true), Advance::Rejoin);
        let first = link.generation();
        link.note_socket(false, first);
        link.setup_failed(first, FailReason::Transport, MonoMs(6_000), true);
        assert_eq!(tick(&mut link, MonoMs(6_999), true), None);
        let Some((generation, attempt)) = tick(&mut link, MonoMs(7_000), true) else {
            panic!("rejoin should retry without another claim");
        };
        assert_eq!(attempt, 2);
        assert!(link.apple_reconnect());
        link.note_socket(false, first);
        assert_eq!(link.retry().0, 2);
        link.note_socket(true, generation);
        assert!(link.mark_ready(generation));
        assert!(!link.apple_reconnect());
        assert_eq!(link.stage(), Stage::Ready);
        // A later displacement is an ordinary loss, not another reconnect.
        link.note_socket(false, generation);
        assert_eq!(link.stage(), Stage::Interrupted);
        assert_eq!(tick(&mut link, MonoMs(60_000), false), None);
    }

    #[test]
    fn unknown_bluez_is_not_stored_as_down() {
        let link = Connection::new();
        assert_eq!(link.bluez(), None);
        assert_eq!(link.socket_up(), None);
    }

    #[test]
    fn interrupting_the_link_invalidates_the_old_generation() {
        let mut link = Connection::new();
        let gen = match link.claim(MonoMs(0)) {
            Claim::Started(gen) => gen,
            Claim::AlreadyUnderway => panic!("claim should start"),
        };
        link.interrupt("bluez_restart");
        assert_eq!(link.stage(), Stage::Interrupted);
        link.note_socket(true, gen);
        assert_eq!(link.socket_up(), Some(false));
        assert!(!link.mark_ready(gen));
    }

    #[test]
    fn wake_waits_and_a_user_disconnect_does_not_reconnect() {
        let mut link = Connection::new();
        link.interrupt("suspend");
        link.arm_resume(MonoMs(RESUME_GRACE_MS));
        assert_eq!(tick(&mut link, MonoMs(RESUME_GRACE_MS), true), None);
        assert_eq!(link.stage(), Stage::Idle);

        let gen = match link.claim(MonoMs(0)) {
            Claim::Started(gen) => gen,
            Claim::AlreadyUnderway => panic!("claim should start"),
        };
        assert!(link.mark_ready(gen) || reach_stage(&mut link, Stage::Handshake, gen, MonoMs(0)));
        link.note_bluez(true);
        link.note_socket(true, gen);
        assert!(link.mark_ready(gen));
        link.interrupt("suspend");
        link.arm_resume(MonoMs(RESUME_GRACE_MS));
        assert_eq!(tick(&mut link, MonoMs(RESUME_GRACE_MS - 1), true), None);
        let Some(_) = tick(&mut link, MonoMs(RESUME_GRACE_MS), true) else {
            panic!("grace elapsed");
        };
        assert!(link.stage().is_opening());

        let mut dropped = Connection::new();
        let gen = match dropped.claim(MonoMs(0)) {
            Claim::Started(gen) => gen,
            Claim::AlreadyUnderway => panic!("claim should start"),
        };
        dropped.note_bluez(true);
        dropped.note_socket(true, gen);
        assert!(dropped.mark_ready(gen));
        dropped.interrupt("disconnect");
        dropped.arm_resume(MonoMs(RESUME_GRACE_MS));
        assert_eq!(tick(&mut dropped, MonoMs(RESUME_GRACE_MS), true), None);
        assert_eq!(dropped.stage(), Stage::Interrupted);

        let mut again = Connection::new();
        let gen = match again.claim(MonoMs(0)) {
            Claim::Started(gen) => gen,
            Claim::AlreadyUnderway => panic!("claim should start"),
        };
        again.note_bluez(true);
        again.note_socket(true, gen);
        assert!(again.mark_ready(gen));
        again.interrupt("suspend");
        assert!(again.arm_resume(MonoMs(RESUME_GRACE_MS)));
        again.interrupt("bluez");
        assert_eq!(again.reason(), "bluez");
        let Some(_) = tick(&mut again, MonoMs(RESUME_GRACE_MS), true) else {
            panic!("a later service loss still reconnects");
        };
    }

    #[test]
    fn a_socket_that_is_already_up_can_finish_the_handshake() {
        let mut link = Connection::new();
        let generation = link.generation();
        link.note_socket(true, generation);
        assert!(link.begin_setup(generation, MonoMs(0)).is_some());
        assert_eq!(link.stage(), Stage::Handshake);
        assert!(link.mark_ready(generation));
        assert_eq!(link.stage(), Stage::Ready);
    }

    #[test]
    fn a_claimed_seat_opens_again_after_the_failure_wait() {
        let mut link = Connection::new();
        let mut generation = match link.claim(MonoMs(0)) {
            Claim::Started(gen) => gen,
            Claim::AlreadyUnderway => panic!("claim should start"),
        };
        for _ in 0..RETRY_LIMIT {
            link.setup_failed(generation, FailReason::Transport, MonoMs(10), true);
            if link.stage() == Stage::Failed {
                break;
            }
            let Some((next, _)) = tick(&mut link, MonoMs(100_000), true) else {
                panic!("retry should open");
            };
            generation = next;
        }
        assert_eq!(link.stage(), Stage::Failed);
        assert_eq!(tick(&mut link, MonoMs(100_000), false), None);
        assert_eq!(link.stage(), Stage::Failed);
        assert_eq!(tick(&mut link, MonoMs(10), true), None);
        let Some(_) = tick(&mut link, MonoMs(100_000), true) else {
            panic!("a claimed seat should try again");
        };
        assert!(link.stage().is_opening());
    }

    #[test]
    fn a_busy_profile_connect_waits_without_spending_an_attempt() {
        let mut link = Connection::new();
        let generation = match link.claim(MonoMs(0)) {
            Claim::Started(generation) => generation,
            Claim::AlreadyUnderway => panic!("claim should start"),
        };
        link.setup_failed(generation, FailReason::Busy, MonoMs(0), true);
        assert_eq!(link.stage(), Stage::Retrying);
        assert_eq!(tick(&mut link, MonoMs(1_000), true), None);
        let Some((_, attempt)) = tick(&mut link, MonoMs(BUSY_WAIT_MS), true) else {
            panic!("the same attempt should open again");
        };
        assert_eq!(attempt, 1);
    }
}
