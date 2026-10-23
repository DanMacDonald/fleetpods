//! Local Bluetooth and control-socket lifecycle.
//! This is the only retry loop for the local link. Routing must not start another.

use crate::types::{Generation, MonoMs, OperationId};

/// Observed service discovery can take 15–18s. This budget is a starting bound
/// around that, not a claim that the timer layout matches the C++ daemon.
pub const SETUP_BUDGET_MS: u64 = 25_000;
/// A handshake or feature reply. Notifications can sit longer.
pub const HANDSHAKE_WAIT_MS: u64 = 2_000;
pub const NOTIFY_WAIT_MS: u64 = 22_000;
pub const RETRY_LIMIT: u8 = 5;
/// BlueZ and the controller need a moment after wake or a service return.
pub const RESUME_GRACE_MS: u64 = 2_000;
/// Let the pods finish reconnecting their previous host after the case opens.
pub const CASE_WAKE_GRACE_MS: u64 = 2_000;
const BACKOFF_MS: [u64; 5] = [1_000, 2_000, 4_000, 8_000, 16_000];
/// How long to leave a profile connect alone when BlueZ says one is already running.
const BUSY_WAIT_MS: u64 = 5_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenStep {
    Connecting,
    Handshake,
    Features,
    Notifications,
}

impl OpenStep {
    pub fn label(self) -> &'static str {
        Stage::from(self).wire()
    }
}

/// Where the local link is. Decisions match this. The status file uses [`Stage::wire`].
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

impl From<OpenStep> for Stage {
    fn from(step: OpenStep) -> Self {
        match step {
            OpenStep::Connecting => Self::Connecting,
            OpenStep::Handshake => Self::Handshake,
            OpenStep::Features => Self::Features,
            OpenStep::Notifications => Self::Notifications,
        }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tick {
    Quiet,
    Open { generation: Generation, attempt: u8 },
}

enum Live {
    Idle,
    Waiting {
        op: OperationId,
    },
    Opening {
        op: OperationId,
        step: OpenStep,
        attempt: u8,
        deadline: MonoMs,
    },
    Ready {
        op: OperationId,
    },
    Retrying {
        op: OperationId,
        attempt: u8,
        deadline: MonoMs,
        reason: FailReason,
        failed: OpenStep,
    },
    Failed {
        op: OperationId,
        reason: FailReason,
        failed: OpenStep,
    },
    Interrupted {
        op: OperationId,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AppleReconnect {
    Idle,
    Releasing,
    Rejoining,
}

pub struct Connection {
    generation: Generation,
    next_op: u64,
    open_at: MonoMs,
    /// `None` until BlueZ has reported. That is not the same as "down".
    bluez: Option<bool>,
    /// `None` until this daemon has observed the control socket.
    socket: Option<bool>,
    live: Live,
    profile_pending: bool,
    apple_reconnect: AppleReconnect,
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
            next_op: 0,
            open_at: MonoMs(0),
            bluez: None,
            socket: None,
            live: Live::Idle,
            profile_pending: false,
            apple_reconnect: AppleReconnect::Idle,
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
        self.apple_reconnect != AppleReconnect::Idle
    }

    /// Only the deliberate release suppresses link failures. Rejoining uses
    /// the normal bounded setup retries while the transaction remains active.
    pub fn apple_reconnect_releasing(&self) -> bool {
        self.apple_reconnect == AppleReconnect::Releasing
    }

    pub fn peer_reconnect_error(&self) -> &'static str {
        self.peer_reconnect_error
    }

    pub fn stage(&self) -> Stage {
        match self.live {
            Live::Idle => Stage::Idle,
            Live::Waiting { .. } => Stage::Waiting,
            Live::Opening { step, .. } => step.into(),
            Live::Ready { .. } => Stage::Ready,
            Live::Retrying { .. } => Stage::Retrying,
            Live::Failed { .. } => Stage::Failed,
            Live::Interrupted { .. } => Stage::Interrupted,
        }
    }

    pub fn failed_stage(&self) -> &'static str {
        match self.live {
            Live::Retrying { failed, .. } | Live::Failed { failed, .. } => failed.label(),
            _ => "idle",
        }
    }

    pub fn reason(&self) -> &'static str {
        match self.live {
            Live::Retrying { reason, .. } | Live::Failed { reason, .. } => reason.label(),
            Live::Interrupted { .. } => self.interrupt_reason,
            _ => "",
        }
    }

    /// Attempt count and limit. Zero means no attempt has started.
    pub fn retry(&self) -> (u8, u8) {
        let attempt = match self.live {
            Live::Opening { attempt, .. } | Live::Retrying { attempt, .. } => attempt,
            _ => 0,
        };
        (attempt, RETRY_LIMIT)
    }

    pub fn recovering(&self) -> bool {
        matches!(self.live, Live::Retrying { .. } | Live::Interrupted { .. })
            || matches!(self.live, Live::Opening { attempt, .. } if attempt > 1)
    }

    pub fn deadline(&self) -> Option<MonoMs> {
        match self.live {
            Live::Opening { deadline, .. } | Live::Retrying { deadline, .. } => Some(deadline),
            _ => None,
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
        if matches!(
            self.live,
            Live::Opening { .. }
                | Live::Retrying { .. }
                | Live::Ready { .. }
                | Live::Waiting { .. }
        ) {
            return;
        }
        self.live = Live::Waiting { op: self.mint_op() };
    }

    pub fn may_open(&self, now: MonoMs) -> bool {
        self.stage().is_opening() && now.0 >= self.open_at.0
    }

    pub fn claim(&mut self, now: MonoMs) -> Claim {
        self.claim_after(now, now)
    }

    pub fn claim_after(&mut self, now: MonoMs, not_before: MonoMs) -> Claim {
        if self.apple_reconnect()
            || matches!(
                self.live,
                Live::Opening { .. } | Live::Retrying { .. } | Live::Ready { .. }
            )
        {
            return Claim::AlreadyUnderway;
        }
        self.reopen_at = None;
        self.open_at = MonoMs(now.0.max(not_before.0));
        self.generation = Generation(self.generation.0 + 1);
        let op = match self.live {
            Live::Waiting { op } => op,
            _ => self.mint_op(),
        };
        self.live = Live::Opening {
            op,
            step: OpenStep::Connecting,
            attempt: 1,
            deadline: self.open_at.saturating_add(SETUP_BUDGET_MS),
        };
        Claim::Started(self.generation)
    }

    /// Park until an eligible request, retiring callbacks and retry deadlines.
    /// An established link may finish its normal shutdown on its own.
    pub fn wait_for_request(&mut self) -> bool {
        if matches!(self.live, Live::Waiting { .. } | Live::Ready { .. }) {
            return false;
        }
        let cancel = matches!(self.live, Live::Opening { .. } | Live::Retrying { .. });
        let op = self.current_op().unwrap_or_else(|| self.mint_op());
        self.generation = Generation(self.generation.0 + 1);
        self.socket = Some(false);
        self.apple_reconnect = AppleReconnect::Idle;
        self.resumable = false;
        self.resume_at = None;
        self.reopen_at = None;
        self.live = Live::Waiting { op };
        cancel
    }

    /// The control socket is already up. Enter the handshake if this link is not already opening.
    pub fn ensure_handshake(&mut self, generation: Generation, now: MonoMs) -> bool {
        if generation != self.generation || self.socket != Some(true) {
            return false;
        }
        if matches!(self.live, Live::Opening { .. } | Live::Ready { .. }) {
            return true;
        }
        let op = self.current_op().unwrap_or_else(|| self.mint_op());
        self.live = Live::Opening {
            op,
            step: OpenStep::Handshake,
            attempt: 1,
            deadline: now.saturating_add(HANDSHAKE_WAIT_MS),
        };
        true
    }

    pub fn advance(&mut self, step: OpenStep, generation: Generation, now: MonoMs) -> bool {
        if generation != self.generation {
            return false;
        }
        let wait = match step {
            OpenStep::Connecting | OpenStep::Features => SETUP_BUDGET_MS,
            OpenStep::Handshake => HANDSHAKE_WAIT_MS,
            OpenStep::Notifications => NOTIFY_WAIT_MS,
        };
        match &mut self.live {
            Live::Opening { step: current, deadline, .. } => {
                *current = step;
                *deadline = now.saturating_add(wait);
                true
            }
            _ => false,
        }
    }

    pub fn mark_ready(&mut self, generation: Generation) -> bool {
        if generation != self.generation || self.socket != Some(true) {
            return false;
        }
        let Live::Opening { op, .. } = self.live else {
            return false;
        };
        self.live = Live::Ready { op };
        self.apple_reconnect = AppleReconnect::Idle;
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
            if let Live::Ready { op } | Live::Opening { op, .. } = self.live {
                self.live = Live::Opening {
                    op,
                    step: OpenStep::Connecting,
                    attempt: 1,
                    deadline: MonoMs(u64::MAX),
                };
            }
            return false;
        }
        if matches!(self.live, Live::Ready { .. }) {
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
        let Live::Opening { op, attempt, step, .. } = self.live else {
            return;
        };
        if reason == FailReason::Busy {
            self.live = Live::Retrying {
                op,
                attempt,
                deadline: now.saturating_add(BUSY_WAIT_MS),
                reason,
                failed: step,
            };
            return;
        }
        if intent_valid && attempt < RETRY_LIMIT {
            let delay = BACKOFF_MS[usize::from(attempt - 1)];
            self.live = Live::Retrying {
                op,
                attempt,
                deadline: now.saturating_add(delay),
                reason,
                failed: step,
            };
        } else {
            self.live = Live::Failed { op, reason, failed: step };
            self.reopen_at = Some(now.saturating_add(BACKOFF_MS[4]));
        }
    }

    /// Arm the grace. Returns whether a reconnect is now waiting.
    pub fn arm_resume(&mut self, at: MonoMs) -> bool {
        if self.resumable && matches!(self.live, Live::Interrupted { .. }) {
            self.resume_at = Some(at);
        }
        self.resume_at.is_some()
    }

    pub fn on_tick(&mut self, now: MonoMs, intent_valid: bool) -> Tick {
        if let Some(at) = self.resume_at {
            if now.0 >= at.0 {
                self.resume_at = None;
                if intent_valid && self.resumable {
                    if let Live::Interrupted { op } = self.live {
                        self.generation = Generation(self.generation.0 + 1);
                        self.resumable = false;
                        self.live = Live::Opening {
                            op,
                            step: OpenStep::Connecting,
                            attempt: 1,
                            deadline: now.saturating_add(SETUP_BUDGET_MS),
                        };
                        return Tick::Open { generation: self.generation, attempt: 1 };
                    }
                }
            }
        }
        if let Live::Failed { .. } = self.live {
            if intent_valid && self.reopen_at.is_none_or(|at| now.0 >= at.0) {
                self.reopen_at = None;
                if let Claim::Started(generation) = self.claim(now) {
                    return Tick::Open { generation, attempt: 1 };
                }
            }
        }
        if let Live::Opening { deadline, .. } = self.live {
            if now.0 >= deadline.0 {
                self.setup_failed(self.generation, FailReason::Setup, now, intent_valid);
            }
        }
        let Live::Retrying { op, attempt, deadline, reason, .. } = self.live else {
            return Tick::Quiet;
        };
        if now.0 < deadline.0 || (reason != FailReason::Busy && attempt >= RETRY_LIMIT) {
            return Tick::Quiet;
        }
        let attempt = if reason == FailReason::Busy { attempt } else { attempt + 1 };
        self.generation = Generation(self.generation.0 + 1);
        self.live = Live::Opening {
            op,
            step: OpenStep::Connecting,
            attempt,
            deadline: now.saturating_add(SETUP_BUDGET_MS),
        };
        Tick::Open { generation: self.generation, attempt }
    }

    pub fn interrupt(&mut self, reason: &'static str) {
        if matches!(self.live, Live::Interrupted { .. }) {
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
            matches!(self.live, Live::Opening { .. } | Live::Ready { .. } | Live::Retrying { .. });
        self.resume_at = None;
        self.interrupt_reason = reason;
        if !active && reason != "disconnect" {
            self.resumable = false;
            return;
        }
        let op = self.current_op().unwrap_or_else(|| self.mint_op());
        self.generation = Generation(self.generation.0 + 1);
        self.socket = Some(false);
        self.apple_reconnect = AppleReconnect::Idle;
        self.resumable = active && !matches!(reason, "disconnect" | "link_lost");
        self.live = Live::Interrupted { op };
    }

    pub fn begin_apple_reconnect(&mut self) -> Result<(), &'static str> {
        if self.apple_reconnect() {
            return Err("error: reconnect already in progress\n");
        }
        if !matches!(self.live, Live::Ready { .. })
            || self.socket != Some(true)
            || self.bluez != Some(true)
        {
            return Err("error: wait for the AirPods connection\n");
        }
        self.apple_reconnect = AppleReconnect::Releasing;
        self.peer_reconnect_error = "";
        Ok(())
    }

    pub fn fail_apple_reconnect(&mut self, message: &'static str) {
        self.apple_reconnect = AppleReconnect::Idle;
        if self.socket != Some(true) {
            self.interrupt("link_lost");
        }
        self.peer_reconnect_error = message;
    }

    pub fn rejoin_after_apple_reconnect(&mut self, now: MonoMs) {
        if !self.apple_reconnect_releasing() || self.socket == Some(true) {
            return;
        }
        self.apple_reconnect = AppleReconnect::Rejoining;
        let op = self.current_op().unwrap_or_else(|| self.mint_op());
        self.generation = Generation(self.generation.0 + 1);
        self.open_at = now;
        self.live = Live::Opening {
            op,
            step: OpenStep::Connecting,
            attempt: 1,
            deadline: now.saturating_add(SETUP_BUDGET_MS),
        };
    }

    fn mint_op(&mut self) -> OperationId {
        self.next_op += 1;
        OperationId(self.next_op)
    }

    fn current_op(&self) -> Option<OperationId> {
        Some(match self.live {
            Live::Idle => return None,
            Live::Waiting { op }
            | Live::Opening { op, .. }
            | Live::Ready { op }
            | Live::Retrying { op, .. }
            | Live::Failed { op, .. }
            | Live::Interrupted { op } => op,
        })
    }
}

impl Default for Connection {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(!link.advance(OpenStep::Handshake, first, MonoMs(0)));
        let Tick::Open { generation, attempt } = link.on_tick(MonoMs(2_000), true) else {
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
            let Tick::Open { generation, .. } = link.on_tick(MonoMs(20_000), true) else {
                panic!("retry should open");
            };
            gen = generation;
        }
        assert_eq!(link.stage(), Stage::Failed);
        assert_eq!(link.on_tick(MonoMs(1_000), true), Tick::Quiet);
    }

    #[test]
    fn an_open_socket_is_not_a_reason_to_connect_again() {
        let mut link = Connection::new();
        let gen = match link.claim(MonoMs(0)) {
            Claim::Started(gen) => gen,
            Claim::AlreadyUnderway => panic!("claim should start"),
        };
        assert!(link.advance(OpenStep::Handshake, gen, MonoMs(0)));
        link.note_socket(true, gen);
        assert_eq!(link.on_tick(MonoMs(1_000), true), Tick::Quiet);
        assert_eq!(link.stage(), Stage::Handshake);
        assert_eq!(link.on_tick(MonoMs(HANDSHAKE_WAIT_MS), true), Tick::Quiet);
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
        assert!(link.begin_apple_reconnect().is_ok());
        link.note_socket(false, gen);
        assert!(link.apple_reconnect());
        assert_ne!(link.stage(), Stage::Failed);
        assert_eq!(link.reason(), "");
        link.rejoin_after_apple_reconnect(MonoMs(2_000));
        assert!(link.generation() != gen);
        assert!(link.may_open(MonoMs(2_000)));
        assert_eq!(link.deadline(), Some(MonoMs(2_000 + SETUP_BUDGET_MS)));
        link.note_socket(false, link.generation());
        assert_eq!(link.deadline(), Some(MonoMs(2_000 + SETUP_BUDGET_MS)));
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
        link.begin_apple_reconnect().unwrap();
        link.note_socket(false, link.generation());
        link.rejoin_after_apple_reconnect(MonoMs(2_000));
        let first = link.generation();
        link.note_socket(false, first);
        link.setup_failed(first, FailReason::Transport, MonoMs(3_000), true);
        assert_eq!(link.on_tick(MonoMs(3_999), true), Tick::Quiet);
        let Tick::Open { generation, attempt } = link.on_tick(MonoMs(4_000), true) else {
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
        assert_eq!(link.on_tick(MonoMs(60_000), false), Tick::Quiet);
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
        assert_eq!(link.on_tick(MonoMs(RESUME_GRACE_MS), true), Tick::Quiet);
        assert_eq!(link.stage(), Stage::Idle);

        let gen = match link.claim(MonoMs(0)) {
            Claim::Started(gen) => gen,
            Claim::AlreadyUnderway => panic!("claim should start"),
        };
        assert!(link.mark_ready(gen) || link.advance(OpenStep::Handshake, gen, MonoMs(0)));
        link.note_bluez(true);
        link.note_socket(true, gen);
        assert!(link.mark_ready(gen));
        link.interrupt("suspend");
        link.arm_resume(MonoMs(RESUME_GRACE_MS));
        assert_eq!(link.on_tick(MonoMs(RESUME_GRACE_MS - 1), true), Tick::Quiet);
        let Tick::Open { .. } = link.on_tick(MonoMs(RESUME_GRACE_MS), true) else {
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
        assert_eq!(dropped.on_tick(MonoMs(RESUME_GRACE_MS), true), Tick::Quiet);
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
        let Tick::Open { .. } = again.on_tick(MonoMs(RESUME_GRACE_MS), true) else {
            panic!("a later service loss still reconnects");
        };
    }

    #[test]
    fn a_socket_that_is_already_up_can_finish_the_handshake() {
        let mut link = Connection::new();
        let generation = link.generation();
        link.note_socket(true, generation);
        assert!(link.ensure_handshake(generation, MonoMs(0)));
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
            let Tick::Open { generation: next, .. } = link.on_tick(MonoMs(100_000), true) else {
                panic!("retry should open");
            };
            generation = next;
        }
        assert_eq!(link.stage(), Stage::Failed);
        assert_eq!(link.on_tick(MonoMs(100_000), false), Tick::Quiet);
        assert_eq!(link.stage(), Stage::Failed);
        assert_eq!(link.on_tick(MonoMs(10), true), Tick::Quiet);
        let Tick::Open { .. } = link.on_tick(MonoMs(100_000), true) else {
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
        assert_eq!(link.on_tick(MonoMs(1_000), true), Tick::Quiet);
        let Tick::Open { attempt, .. } = link.on_tick(MonoMs(BUSY_WAIT_MS), true) else {
            panic!("the same attempt should open again");
        };
        assert_eq!(attempt, 1);
    }
}
