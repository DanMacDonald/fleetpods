//! Local playback operations. This is the composition seam between setup,
//! seat intent, player state and the stream router. Event handlers call these
//! operations; they do not coordinate recovery flags or deferred resumes.
use super::*;
use crate::pods::setup;
use crate::retry::{Delay, Limit, Policy, Requests, State as TaskState, Task};
use crate::step::Effect;

/// A local play waiting to become, or having become, a pods claim.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum PlaybackClaim {
    #[default]
    Idle,
    /// Pressed without evidence the pods can play; waits for pods or a
    /// link so the speakers are not held for a takeover that may never
    /// happen.
    Deferred(MonoMs),
    /// Applied to the router and the seat; the grant is still due.
    Active,
}

/// Why an ownership claim is being written. Callers keep their own gates;
/// `emit_ownership_claim` is the only writer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClaimReason {
    Playback,
    IdleActivity,
    OwnershipReassert,
    StemContest,
}

#[derive(Default)]
pub(super) struct Session {
    /// The pods reset this laptop's link mid-playback with no fleet laptop
    /// taking over: an Apple device reconnecting as the buds go back in.
    /// The players stay paused until this laptop is back alongside it.
    rejoin: Option<Rejoin>,
    rejoined_at: Option<MonoMs>,
    background_requested: bool,
    /// The one representation of a pending playback claim: deferred
    /// claims wait for pods or a link without holding speaker playback;
    /// an active claim has been applied to the router and the seat.
    claim: PlaybackClaim,
    /// When this laptop last sent a claim on the control link.
    claim_sent_at: Option<MonoMs>,
    /// A remote report arrived just after a claim or during setup settle;
    /// it ends the claim at the deadline unless the pods grant it first.
    yield_pending: bool,
    /// When a claim was sent as a new link became ready.
    setup_claim_at: Option<MonoMs>,
    /// When this laptop last yielded to a remote report. A player that
    /// comes back within the rebound window is re-paused, not treated as
    /// a new takeover claim, unless desktop input marks it as a press.
    yield_pause_at: Option<MonoMs>,
    /// When the shell last reported physical desktop input. A play that
    /// follows fresh input is a press, not a rebound.
    last_input_at: Option<MonoMs>,
    /// Deadline for a speculative step-back on a fleet intent report.
    /// Players paused for it resume if no claim becomes a grant in time.
    pub(super) intent_until: Option<MonoMs>,
    /// Players paused by that speculative step-back.
    intent_paused: Vec<String>,
    /// Players the last yield paused. Only these can be stem-toggled
    /// back by the pods; a player that starts fresh is a local press.
    yield_paused: Vec<String>,
    /// When the control link last became ready.
    link_ready_at: Option<MonoMs>,

    resume_output_on_grant: bool,
    rearm_after_loss: bool,
    output_recovery: OutputRecovery,
    rebuild_pause_until: Option<MonoMs>,
    resume_players: Vec<String>,
    resumed_players: Vec<String>,
    release_output_until: Option<MonoMs>,
    /// When actively playing local audio last lost ownership.
    ownership_loss_at: Option<MonoMs>,
    /// A stem-toggled outside session can insist for a few seconds.
    /// Its started reports get bounded ownership contests.
    stem_loss_at: Option<MonoMs>,
    stem_peer: Option<Address>,
    stem_claims_sent: u8,
    /// Player that paused while this laptop owned the pods, and when.
    /// A fleet peer's stem blip can ask for it back.
    stem_paused: Option<(String, MonoMs)>,
    /// Deadline at which a peer blip becomes a resume of `stem_paused`.
    stem_resume_at: Option<MonoMs>,
}

/// One admission, identity and cancellation owner for takeover, watchdog and
/// manual repairs. The router executes the same park/reset/relink/Start sequence.
struct OutputRecovery {
    task: Task,
    requests: Requests,
    repair_requested: bool,
    resets: u64,
    last_ok: Option<bool>,
}

impl Default for OutputRecovery {
    fn default() -> Self {
        Self {
            task: Task::new(Policy {
                delay: Delay::Fixed(0),
                limit: Limit::Attempts(3),
                // The router bounds park/reset/relink/Start itself.
                request_timeout_ms: None,
                confirmation_timeout_ms: Some(2_500),
            }),
            requests: Requests::default(),
            repair_requested: false,
            resets: 0,
            last_ok: None,
        }
    }
}

impl OutputRecovery {
    fn cancel(&mut self) {
        if self.resets > 0 && self.last_ok.is_none() {
            self.last_ok = Some(false);
        }
        self.task.reset();
        self.repair_requested = false;
    }

    fn request_repair(&mut self) {
        self.cancel();
        self.repair_requested = true;
    }

    // Startup and its bounded retries own recovery through completion. The
    // steady-playback watchdog must not launch a second reset alongside them,
    // or replenish their budget once exhausted.
    fn watchdog_allowed(&self) -> bool {
        matches!(self.task.state(), TaskState::Idle | TaskState::Succeeded { .. })
    }

    /// Start completion begins verification, not proof of audible playback.
    fn started(&mut self, serial: u64, now: MonoMs) {
        if let TaskState::Running { id, .. } = self.task.state() {
            if id.serial == serial {
                self.task.reply(id, true, now);
                self.last_ok = Some(true);
            }
        }
    }

    /// True when verification calls for another rebuild. An early transport
    /// blip is insufficient; sink and transport must both run at the deadline.
    fn retry_due(&mut self, granted: bool, streaming: bool, playing: bool, now: MonoMs) -> bool {
        let checking =
            matches!(self.task.state(), TaskState::Confirming { until, .. } if now >= until);
        let retry = matches!(self.task.state(), TaskState::Backoff { .. });
        if !granted || (checking && !streaming && !playing) {
            // A paused player cannot prove playback. Discard the check rather
            // than leave a retry waiting for their next Play.
            self.cancel();
        } else if (checking || retry) && streaming {
            self.task.observed_success();
        } else {
            self.task.tick(now);
        }
        matches!(self.task.state(), TaskState::Backoff { .. }) && playing
    }

    /// Admit one output attempt, sharing identity and budget across all triggers.
    /// A direct attempt
    /// starts its confirmation deadline at once (the router only starts
    /// the existing node); a rebuild attempt confirms only after its
    /// start lands. Silence at the deadline retries as a rebuild.
    fn begin(
        &mut self,
        requested: bool,
        generation: Generation,
        now: MonoMs,
        direct: bool,
        admit: impl FnOnce(u64) -> bool,
    ) -> bool {
        if direct {
            if matches!(self.task.state(), TaskState::Running { .. } | TaskState::Confirming { .. })
            {
                return true;
            }
            if matches!(self.task.state(), TaskState::Succeeded { .. } | TaskState::Exhausted) {
                self.cancel();
            }
            let Some(id) = self.task.try_launch(&mut self.requests, generation, now, |id| {
                admit(id.serial);
                true
            }) else {
                return false;
            };
            self.task.reply(id, true, now);
            return true;
        }
        if requested
            && matches!(self.task.state(), TaskState::Succeeded { .. } | TaskState::Exhausted)
        {
            self.cancel();
        }
        let began = self
            .task
            .try_launch(&mut self.requests, generation, now, |id| admit(id.serial))
            .is_some();
        if began {
            self.repair_requested = false;
            self.resets += 1;
            self.last_ok = None;
        }
        began
    }
}
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
const SETUP_CLAIM_SETTLE_MS: u64 = 1_500;
/// The pods can send one more report naming the previous owner just before
/// they grant a claim (2026-10-19: 1 ms apart). A remote report this soon
/// after a claim waits this long for the grant before it ends the claim.
const CLAIM_ANSWER_MS: u64 = 300;
/// A browser whose output device just changed can resume by itself within
/// seconds of being paused for a yield. A replay inside this window is a
/// rebound; pressing play again is intent.
const REBOUND_WINDOW_MS: u64 = 10_000;
/// The shell reports desktop input as the user reaches for a key, so a
/// play this soon after input is a press (the space bar needs no mouse),
/// however fresh the yield it follows. Brave's own rebound has no input
/// anywhere near it.
const INPUT_INTENT_MS: u64 = 15_000;
/// How long a speculative step-back on fleet intent waits for the claim to
/// become a grant before local playback resumes. A slow cold connection
/// (link plus profile) took about 8 s on 2026-11-02.
const INTENT_HOLD_MS: u64 = 12_000;
/// How long a deferred claim keeps airing intent while it waits for the
/// pods or the link. An actively connecting claim is already covered by
/// the router's want; this bounds the waiting case so a peer that never
/// becomes available does not hold the owner's playback hostage.
const INTENT_AIR_MS: u64 = 15_000;
/// The outside host's toggled session can insist for a few seconds;
/// answer its started reports with bounded ownership contests.
const STEM_CONTEST_WINDOW_MS: u64 = 5_000;
const STEM_CONTEST_MAX_CLAIMS: u8 = 3;
/// Wait after a fleet peer's started report before treating it as a stem
/// blip: a genuine takeover claims ownership well inside this beat, while
/// a stem toggle delivered to the peer is re-paused almost immediately.
const STEM_RESUME_DELAY_MS: u64 = 600;

/// Why an opening attempt must be retired.
enum ConnectionHold {
    Pairing,
    Stowed,
    Superseded,
}

/// Release behavior is explicit: only handoff/reconnect pause players.
enum Release {
    Handoff,
    Speakers,
    Preference,
    AwaitAvailability,
    AppleReconnect,
}

struct Rejoin {
    /// When to open the link; once opened, when to stop waiting for it.
    at: MonoMs,
    players: Vec<String>,
    opened: bool,
}

impl Session {
    fn retire_player_work(&mut self) {
        self.resume_players.clear();
        self.resumed_players.clear();
        self.rebuild_pause_until = None;
        self.output_recovery.cancel();
    }
}

impl Daemon {
    /// The single advancement order after an input has been applied. No caller
    /// schedules the session's timers, rejoin, ownership or routing separately.
    pub(crate) fn advance_session(&mut self, now: Clock, tick: bool) -> Vec<Effect> {
        if tick {
            self.tick_session(now);
        }
        // A terminal background attempt does not create another connection
        // request. This applies equally to a callback failure and a timeout.
        if self.connection.stage() == Stage::Failed {
            self.session.background_requested = false;
        }
        self.advance_rejoin(now);
        self.reconnect_preferred(now);
        self.settle_playback_reports(now);
        self.introduce_peers();
        self.claim_idle_on_activity(now);
        self.tick_ears(now.mono);
        self.follow_insertion(now);
        // Fleet admission expiry can change connection intent. Collect it
        // before the final route/setup decision, then publish the final state.
        let mut effects = self.collect_background_effects(now);
        self.expire_intent_hold(now, &mut effects);
        self.fire_stem_resume(now, &mut effects);
        self.finish_session(now, &mut effects);
        self.announce_session(now, &mut effects);
        effects
    }

    fn tick_session(&mut self, now: Clock) {
        self.expire_roster_probe(now);
        if self.config.accessory.is_none() && self.nearby_pods.refresh(now.mono) {
            if let Some(report) = self.nearby_pods.selected_report(now.mono) {
                self.accessory = report;
            }
        }
        self.accessory.expire(now.mono, self.connection.stage() == Stage::Ready);
        for report in self.tracked.values_mut() {
            report.expire(now.mono, false);
        }
        self.discovery.expire(now.unix);
        self.enrollment.on_tick(now.mono);
        self.advance_deferred_claim(now);
        if let Some(pending) = &self.pending_admission {
            let failure = if pending.release.is_none() && self.connection.stage() != Stage::Ready {
                Some(crate::fleet::route_air::Answer::Unavailable)
            } else if now.mono.0 >= pending.until.0 {
                Some(crate::fleet::route_air::Answer::TimedOut)
            } else {
                None
            };
            if let Some(reason) = failure {
                self.finish_admission(reason, now);
            }
        }
        self.tick_connection(now);
        self.audio.touch_players(now.unix);
        if self.priority_until.is_some_and(|at| now.mono.0 >= at.0) {
            self.priority_until = None;
        }
        self.consider_capture_stall(now.mono);
        self.advance_reconnect(now);
    }

    pub(super) fn background_admission_failed(&mut self, unavailable: bool, now: Clock) {
        if unavailable && self.session.background_requested {
            self.open_background_connection(now);
        } else {
            self.session.background_requested = false;
        }
    }

    pub fn note_socket(&mut self, up: bool, generation: Generation, now: MonoMs) {
        if generation != self.connection.generation() {
            return;
        }
        if !up {
            if self.connection.stage() == Stage::Ready
                && !self.routing.phase().busy()
                && !self.connection.apple_reconnect()
            {
                let until = now.saturating_add(DISPLACEMENT_HOLD_MS);
                self.slot_hold_until = Some(self.slot_hold_until.map_or(until, |held| {
                    if held.0 > until.0 {
                        held
                    } else {
                        until
                    }
                }));
            }
            self.consider_apple_displacement();
            self.consider_reopen_order();
        }
        if self.connection.note_socket(up, generation) {
            self.session.background_requested = false;
            self.drop_reports();
        }
    }

    pub fn mark_control_ready(&mut self, generation: Generation) -> bool {
        let ready = self.connection.mark_ready(generation);
        if ready {
            self.displaced = false;
            self.slot_hold_until = None;
            self.reopen_wait = false;
            self.reopen_hold_until = None;
            self.session.background_requested = false;
            self.microphone.set_control_ready(true);
        }
        ready
    }

    pub fn note_setup_failed(&mut self, generation: Generation, reason: FailReason, now: MonoMs) {
        let intent_valid = self.connection_intended();
        let stage = self.connection.stage();
        self.connection.setup_failed(generation, reason, now, intent_valid);
        eprintln!(
            "fleetpodsd: setup failure generation={} stage={} reason={} intent={} next={} attempt={}",
            generation.0, stage.wire(), reason.label(), intent_valid,
            self.connection.stage().wire(), self.connection.retry().0
        );
    }

    /// The ready link is about to drop. Read before the drop clears the pods'
    /// list: a fleet laptop in it is a handoff, and a playing phone or Mac
    /// keeps what it took.
    fn evicted_while_playing(&self, now: Clock) -> bool {
        let fleet_laptop = self
            .pod_peers
            .iter()
            .any(|peer| Some(*peer) != self.local_address && self.membership.contains(*peer));
        self.output.wants_output()
            && self.audio.local_playing()
            && !fleet_laptop
            && !self.routing.phase().busy()
            && !self.connection.apple_reconnect()
            && !self.radio_quiet()
            && self.connection.reason() != "disconnect"
            && self.preferred_connection_allowed()
            && !self.outside_device_playing(now)
            && self
                .session
                .rejoined_at
                .is_none_or(|at| now.mono.saturating_since(at) >= REJOIN_WINDOW_MS)
    }

    /// Keep the players this drop paused and rejoin once the newcomer settles.
    fn plan_rejoin(&mut self, players: Vec<String>, now: MonoMs) {
        eprintln!("fleetpodsd: the pods dropped this laptop mid-playback; rejoining in {REJOIN_DELAY_MS} ms");
        self.session.rejoined_at = Some(now);
        self.session.rejoin =
            Some(Rejoin { at: now.saturating_add(REJOIN_DELAY_MS), players, opened: false });
    }

    /// Open the link alongside the device that took this laptop's place.
    /// The claim and the resume wait for control readiness.
    fn advance_rejoin(&mut self, now: Clock) {
        let Some(rejoin) = &self.session.rejoin else {
            return;
        };
        if now.mono.0 < rejoin.at.0 {
            return;
        }
        if rejoin.opened {
            eprintln!("fleetpodsd: rejoin gave up; the AirPods link did not come back");
            self.session.rejoin = None;
            return;
        }
        let cancel = if self.output.wants_output() {
            Some("a newer play took over")
        } else if self.routing.phase().busy() || self.connection.apple_reconnect() {
            Some("routing in progress")
        } else if self.radio_quiet() || self.connection.reason() == "disconnect" {
            Some("link unavailable")
        } else if !self.preferred_connection_allowed() {
            Some("another laptop is preferred")
        } else if self.outside_device_playing(now) {
            Some("a device outside the fleet is playing")
        } else if self.accessory.stowed() || self.accessory.both_docked() {
            Some("the buds are docked")
        } else {
            None
        };
        if let Some(reason) = cancel {
            eprintln!("fleetpodsd: rejoin cancelled: {reason}");
            self.session.rejoin = None;
            return;
        }
        if let Some(rejoin) = &mut self.session.rejoin {
            rejoin.opened = true;
            rejoin.at = now.mono.saturating_add(REJOIN_WINDOW_MS);
        }
        if self.connection.stage() == Stage::Ready {
            self.claim_rejoin(now);
        } else if !self.connection.stage().is_opening() {
            eprintln!("fleetpodsd: rejoining the AirPods alongside the device that took this link");
            self.displaced = false;
            self.slot_hold_until = None;
            self.routing.claim_audio();
            self.open_connection(now.mono);
        }
    }

    /// The remembered host alone reacts to availability. A sleeping owner is
    /// never timed out in favor of a less recently used laptop.
    fn reconnect_preferred(&mut self, now: Clock) {
        let available = self.pods_available(now.mono);
        self.activity_preference.observe_availability(available, now.mono);
        if self.connection.stage() != Stage::Ready
            && !self.preferred_connection_allowed()
            && !self.connection.apple_reconnect()
        {
            self.cancel_connection_attempt(ConnectionHold::Superseded);
            self.session.claim = PlaybackClaim::Idle;
            if self.output.wants_output() {
                // A superseded pending connection must not leave speaker
                // playback muted while waiting for a link it cannot open.
                // Release restores the route and does not pause players.
                self.release_preference(now);
            }
            return;
        }
        if !available
            || self.connection.reason() == "disconnect"
            || self.radio_quiet()
            || self.keyring_locked
            || !matches!(self.connection.stage(), Stage::Idle | Stage::Waiting | Stage::Interrupted)
            || self.routing.phase().busy()
            || self.connection.apple_reconnect()
            || self.outside_device_playing(now)
            || self.background_reopen_blocked(now.mono)
        {
            return;
        }
        let (Some(record), Some(local)) = (self.preferred_connector(), self.local_address) else {
            return;
        };
        // Commit the one wake only when this same evidence will open.
        // A fresh Pod placement can make the pods available while the
        // battery slots are still empty; that open has to proceed.
        if self.wake_can_open(now) && self.activity_preference.take_wake(record, local, now.mono) {
            self.connection.prepare();
            self.session.background_requested = true;
            self.begin_background_open(now);
        }
    }

    fn wake_can_open(&self, now: Clock) -> bool {
        self.preferred_connection_allowed()
            && self.config.accessory.is_some()
            && !self.radio_quiet()
            && self.connection_hold(now.mono).is_none()
            && self.pods_available(now.mono)
    }

    pub(super) fn activity(&mut self, now: Clock) -> Response {
        // Recorded before every gate: even an ignored activity report is
        // proof someone is at this keyboard, which is what separates a
        // press from a player's self-resume after a yield.
        self.session.last_input_at = Some(now.mono);
        self.remember_activity(now);
        if self.config.accessory.is_none()
            || self.radio_quiet()
            || !self.enforce_connection_admission(now.mono)
            || self.routing.phase().busy()
            || self.connection.apple_reconnect()
            || self.connection.stage().is_opening()
            || self.connection.stage() == Stage::Retrying
        {
            return Response::line("ignored");
        }
        if self.connection.stage() == Stage::Ready {
            self.activity_at = Some(now.mono);
            return Response::line(if self.claim_idle_on_activity(now) {
                "accepted"
            } else {
                "ignored"
            });
        }
        // Connecting this laptop in the background takes one of the pods' two
        // connections. Input alone must not cost a playing phone or Mac its
        // connection (gram, 2026-10-19); a play here still takes over.
        if self.outside_device_playing(now) {
            eprintln!("fleetpodsd: activity ignored while a device outside the fleet plays on the AirPods");
            return Response::line("ignored");
        }
        // Input on an established link can claim idle audio immediately. Input
        // after a slot loss must not evict the device still taking that slot.
        if self.slot_hold_until.is_some_and(|until| now.mono.0 < until.0) {
            eprintln!("fleetpodsd: activity ignored while a yielded slot is held");
            return Response::line("ignored");
        }
        self.slot_hold_until = None;
        // Use a connected helper when present; otherwise open only the control link.
        let target = self.activity_target(now);
        // The reopen hold lets the pair's audio owner land first. A connected
        // fleet helper means it already has, so input asks that helper for a
        // seat instead of waiting out the hold.
        if matches!(target, ActivityTarget::One(_)) {
            self.reopen_wait = false;
            self.reopen_hold_until = None;
        } else if self.reopen_blocked(now.mono) {
            eprintln!("fleetpodsd: activity ignored while the audio owner reconnects first");
            return Response::line("ignored");
        }
        match target {
            ActivityTarget::Several => Response::line("ignored"),
            ActivityTarget::One(peer) => {
                self.connection.prepare();
                match self.routing.start(peer) {
                    crate::fleet::route::Start::Sent { .. }
                    | crate::fleet::route::Start::Duplicate => {
                        self.session.background_requested = true;
                        Response::line("accepted")
                    }
                    crate::fleet::route::Start::Busy => Response::line("ignored"),
                }
            }
            ActivityTarget::None => {
                self.connection.prepare();
                self.open_background_connection(now);
                Response::line("accepted")
            }
        }
    }

    fn background_connection_allowed(&self) -> bool {
        let out = |pod: &crate::types::Evidence<Pod>| {
            pod.as_fresh().is_some_and(|pod| pod.in_ear == Some(true) || pod.in_case == Some(false))
        };
        self.preferred_connection_allowed()
            && self.config.accessory.is_some()
            && !self.radio_quiet()
            && !self.accessory.stowed()
            && self.connection.reason() != "disconnect"
            && (self.accessory.lid.latest.as_fresh() == Some(&LidState::Open)
                || out(&self.accessory.left)
                || out(&self.accessory.right))
    }

    fn open_background_connection(&mut self, now: Clock) {
        self.session.background_requested = self.background_connection_allowed();
        if self.session.background_requested {
            self.begin_background_open(now);
        }
    }

    fn begin_background_open(&mut self, now: Clock) {
        self.routing.clear_failure();
        self.displaced = false;
        self.open_connection(now.mono);
        eprintln!("fleetpodsd: activity opening the background control connection");
    }

    /// Admission is a read-only decision. Applying a hold retires the attempt
    /// in one place, preserving a live control connection and explicit disconnect.
    fn connection_hold(&self, now: MonoMs) -> Option<ConnectionHold> {
        if self.pairing_elsewhere(now) {
            Some(ConnectionHold::Pairing)
        } else if self.accessory.stowed() {
            Some(ConnectionHold::Stowed)
        } else {
            None
        }
    }

    fn cancel_connection_attempt(&mut self, hold: ConnectionHold) {
        self.session.background_requested = false;
        let requested = self.lifecycle.link_requested(self.connection.generation());
        if self.connection.reason() != "disconnect" && self.connection.wait_for_request() {
            self.effects.drop_link |= requested || matches!(hold, ConnectionHold::Stowed);
            if !matches!(hold, ConnectionHold::Superseded) {
                self.drop_reports();
            }
        }
        let cancel_routing = match hold {
            ConnectionHold::Superseded => matches!(
                self.routing.phase(),
                Phase::WaitingAdmission | Phase::Connecting | Phase::WaitingCompletion
            ),
            _ => self.routing.phase().busy(),
        };
        if cancel_routing {
            self.routing.restart();
        }
    }

    /// Called after physical observations and before requests/ticks advance.
    pub(super) fn enforce_connection_admission(&mut self, now: MonoMs) -> bool {
        let Some(hold) = self.connection_hold(now) else { return true };
        self.cancel_connection_attempt(hold);
        false
    }

    fn advance_deferred_claim(&mut self, now: Clock) {
        if let PlaybackClaim::Deferred(at) = self.session.claim {
            if now.mono.saturating_since(at) > 30_000 || !self.audio.local_playing() {
                self.session.claim = PlaybackClaim::Idle;
            } else if !self.accessory.stowed()
                && !self.accessory.both_docked()
                && self.preferred_connection_allowed()
                // Setup can complete with battery and roster reports alone.
                // That says nothing about whether a docked bud has come out.
                && self.accessory.playback_available(now.mono)
            {
                self.session.claim = PlaybackClaim::Idle;
                self.claim_playback(now);
                // This play predates the link, so its opening reports do too.
                if self.output.wants_output()
                    && self.session.link_ready_at.is_some_and(|ready| {
                        now.mono.saturating_since(ready) < SETUP_CLAIM_SETTLE_MS
                    })
                {
                    self.session.setup_claim_at = Some(now.mono);
                }
            }
        }
    }

    fn connection_intended(&self) -> bool {
        self.session.background_requested
            || self.routing.intent() == crate::fleet::route::Intent::Claim
            || matches!(
                self.routing.phase(),
                crate::fleet::route::Phase::Connecting
                    | crate::fleet::route::Phase::WaitingCompletion
            )
    }

    fn tick_connection(&mut self, now: Clock) {
        if self.enforce_connection_admission(now.mono) && !self.radio_quiet() {
            // A previously ready, idle connection may have rejoined at the
            // Bluetooth layer during wake. Restore its control session after
            // the grace without requiring a playback claim or peer admission.
            let resume_existing = self.preferred_connection_allowed()
                && self.connection.bluez() == Some(true)
                && self.connection.stage() == Stage::Interrupted
                && matches!(self.connection.reason(), "suspend" | "bluez" | "adapter");
            self.connection.on_tick(now.mono, self.connection_intended() || resume_existing);
        }
    }

    fn can_open_link(&self, now: MonoMs) -> bool {
        (self.preferred_connection_allowed() || self.connection.apple_reconnect())
            && !self.pairing_elsewhere(now)
            && self.connection.may_open(now)
    }

    fn fail_reconnect(&mut self, reason: &'static str, now: Clock) {
        self.session.background_requested = false;
        self.seat.note_failure(reason, now);
        if self.background_connection_allowed()
            && !self.accessory.both_docked()
            && self.enforce_connection_admission(now.mono)
        {
            // One delayed operation owns all attempts. The session supplies
            // permission to connect, never another retry deadline or counter.
            self.session.background_requested = matches!(
                self.connection.reopen_background(now.mono),
                crate::pods::link::Claim::Started(_)
            );
        }
    }

    fn advance_reconnect(&mut self, now: Clock) {
        use crate::pods::reconnect::Advance;
        let peers_left = self.reconnect_peers().is_empty();
        match self.connection.advance_reconnect(now.mono, !self.audio.local_playing(), peers_left) {
            Advance::None => {}
            Advance::SendRequests => self.send_reconnect_requests(),
            Advance::DropLink => self.effects.drop_link = true,
            Advance::Rejoin => {
                self.session.background_requested = true;
            }
            Advance::Done => self.seat.note_reconnected(now),
            Advance::Fail(reason) => self.fail_reconnect(reason, now),
        }
    }

    /// Stream-only players can use ownership already confirmed by the pods.
    fn route_owned_playback(&mut self, graph: &crate::audio::route::Graph, now: Clock) {
        let Some(accessory) = self.config.accessory else {
            return;
        };
        if self.accessory.both_docked()
            && crate::audio::route::playback_started(
                self.last_graph.as_ref(),
                graph,
                accessory,
                self.output.source_sink(),
            )
        {
            self.return_to_speakers(now);
            return;
        }
        if self.output.wants_output()
            || self.connection.stage() != Stage::Ready
            || self.audio.yielded()
            || self.accessory.both_docked()
        {
            return;
        }
        let resolved = self.resolved(now);
        if resolved.source != crate::audio::seat::Source::Direct
            || !matches!(resolved.holder, Holder::Local { .. })
        {
            return;
        }
        if crate::audio::route::playback_started(
            self.last_graph.as_ref(),
            graph,
            accessory,
            self.output.source_sink(),
        ) {
            self.remember_activity(now);
            self.claim_seat(now);
            eprintln!("fleetpodsd: routing new playback through already-owned AirPods");
        }
    }

    /// An existing Bluetooth ACL still needs a tracked, retryable control setup.
    /// Property updates must never reopen an established seat that was lost.
    pub(crate) fn attach_available(&mut self, generation: Generation, now: MonoMs) {
        if !self.preferred_connection_allowed() {
            if generation == self.connection.generation()
                && self.connection.bluez() == Some(true)
                && self.connection.stage() != Stage::Ready
            {
                self.effects.drop_link = true;
            }
            return;
        }
        if self.background_reopen_blocked(now) {
            return;
        }
        if generation != self.connection.generation()
            || !matches!(self.connection.stage(), Stage::Idle | Stage::Waiting)
            || self.connection.bluez() != Some(true)
            || self.routing.phase().busy()
            || self.radio_quiet()
            || !self.enforce_connection_admission(now)
        {
            return;
        }
        self.session.background_requested = true;
        self.open_connection(now);
    }

    pub(super) fn open_connection(&mut self, now: MonoMs) {
        // A nearby advertisement is not a local Bluetooth pairing.
        if self.config.accessory.is_none() || !self.enforce_connection_admission(now) {
            return;
        }
        let not_before = self
            .case_opened_at
            .map(|at| at.saturating_add(crate::pods::link::CASE_WAKE_GRACE_MS))
            .unwrap_or(now);
        self.connection.claim_after(now, not_before);
    }

    /// Another laptop asked this one to leave so an Apple device can take the slot.
    pub(crate) fn free_slot(&mut self, peer: Address, now: Clock) {
        if self.local_address == Some(peer) || self.connection.apple_reconnect() {
            return;
        }
        let known = self.membership.contains(peer) || self.peers_seated.contains(&peer);
        if !known {
            return;
        }
        self.step_back(now);
        self.effects.drop_link = true;
        self.slot_hold_until = Some(now.mono.saturating_add(SLOT_HOLD_MS));
    }

    pub(super) fn claim_handoff(&mut self, now: Clock) -> Response {
        self.remember_activity(now);
        if self.routing.phase().busy() && self.routing.phase() != Phase::WaitingCompletion {
            return Response::line("fleet_routing_busy");
        }
        let linked = self.connection.socket_up() == Some(true);
        let packet = if linked {
            if self.priority_until.is_some() {
                return Response::line("request_pending");
            }
            let Some(local) = self.local_address else {
                return Response::line("control_unavailable");
            };
            let Some(packet) = crate::pods::setup::priority(local, None) else {
                return Response::line("invalid_priority_pair");
            };
            Some(packet)
        } else {
            None
        };
        self.claim_seat_user(now);
        if let Some(packet) = packet {
            self.queue_priority(packet, now);
            return Response::line("written_waiting_for_ack");
        }
        self.open_connection(now.mono);
        Response::line("queued")
    }

    pub(super) fn connect(&mut self, now: Clock) -> Response {
        self.remember_activity(now);
        if self.pairing_elsewhere(now.mono) {
            self.cancel_connection_attempt(ConnectionHold::Pairing);
            return Response::line("error: AirPods are in pairing mode; finish pairing first");
        }
        if self.connection.apple_reconnect() {
            return Response::line("error: reconnect already in progress");
        }
        if self.routing.phase().busy() {
            if self.routing.phase() == Phase::WaitingCompletion
                && self.routing.queue_claim_until_route_lands()
            {
                return Response::line("accepted");
            }
            return Response::line("error: fleet routing in progress");
        }
        if self.config.accessory.is_none() {
            return Response::line("error: no paired AirPods address");
        }
        self.displaced = false;
        self.routing.claim_audio();
        self.open_connection(now.mono);
        Response::line("accepted")
    }

    pub(crate) fn link_failed(&mut self, generation: Generation, busy: bool, now: Clock) {
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

    pub(crate) fn control_socket_changed(&mut self, up: bool, generation: Generation, now: Clock) {
        if generation != self.connection.generation() {
            return;
        }
        let was_ready = self.connection.stage() == Stage::Ready;
        // A control link can outlive our return to the speakers.
        // Losing that link must not pause or cancel the speaker play.
        let released_audio = !self.output.wants_output() && self.audio.yielded();
        let evicted = !up && was_ready && self.evicted_while_playing(now);
        self.note_socket(up, generation, now.mono);
        if up {
            self.begin_setup(generation, now);
        } else {
            if was_ready
                && !released_audio
                && !self.routing.phase().busy()
                && !self.connection.apple_reconnect()
            {
                self.step_back(now);
                if evicted {
                    let players = self.effects.pause_players.clone();
                    self.plan_rejoin(players, now.mono);
                }
            }
        }
    }

    /// Take the seat. Routing intent, the audio route, and the yield flag move together.
    pub(super) fn claim_seat(&mut self, now: Clock) {
        self.seat.claim(now);
        self.apply_claim();
        self.output.extend_takeover(now.mono);
    }

    /// Take the seat for an explicit user switch.
    pub(super) fn claim_seat_user(&mut self, now: Clock) {
        self.seat.claim_user(now);
        self.apply_claim();
        self.output.extend_takeover(now.mono);
    }

    fn apply_claim(&mut self) {
        self.session.claim = PlaybackClaim::Active;
        self.session.intent_until = None;
        self.session.intent_paused.clear();
        self.session.retire_player_work();
        self.lifecycle.reset_audio();
        self.routing.claim_audio();
        self.output.claim();
        self.audio.clear_yield();
    }

    /// Whether this laptop should air fleet intent: it wants the
    /// accessory's audio (or a claim is deferred waiting for it) and the
    /// pods have not granted it.
    pub(crate) fn roster_intent(&self, now: Clock) -> bool {
        let pending = match self.session.claim {
            PlaybackClaim::Active => true,
            PlaybackClaim::Deferred(at) => now.mono.saturating_since(at) <= INTENT_AIR_MS,
            PlaybackClaim::Idle => false,
        };
        pending && self.accessory.ownership.as_fresh() != Some(&true) && !self.accessory.stowed()
    }

    /// A fleet peer announced it intends to claim this accessory. If this
    /// laptop owns the audio, step back ahead of the claim so the pods are
    /// free when the peer's connection lands. The step-back is speculative:
    /// players paused for it resume when the hold expires without a grant.
    /// Only a fresh, stored roster report reaches this; replays are dropped
    /// by the roster watermark before they can act twice.
    pub(crate) fn note_fleet_intent(&mut self, reporter: Address, reason: &str, now: Clock) {
        if self.session.intent_until.is_some()
            || self.accessory.ownership.as_fresh() != Some(&true)
            || self.connection.stage() != Stage::Ready
            || self.seat.user_intent_fresh(now.mono)
        {
            return;
        }
        eprintln!("fleetpodsd: {reason} from {reporter}; stepping back ahead of its claim");
        self.session.intent_paused = self.audio.playing_players();
        self.session.intent_until = Some(now.mono.saturating_add(INTENT_HOLD_MS));
        self.session.release_output_until = Some(now.mono.saturating_add(RELEASE_OUTPUT_MS));
        self.step_back(now);
    }

    /// Give intent-paused playback back when no grant landed in the hold,
    /// and forget the hold once the pods granted the peer instead.
    fn expire_intent_hold(&mut self, now: Clock, effects: &mut Vec<Effect>) {
        let Some(until) = self.session.intent_until else {
            return;
        };
        if self.accessory.ownership.as_fresh() == Some(&false) {
            self.session.intent_until = None;
            self.session.intent_paused.clear();
            return;
        }
        if now.mono.0 < until.0 {
            return;
        }
        self.session.intent_until = None;
        let players = std::mem::take(&mut self.session.intent_paused);
        if !players.is_empty() {
            eprintln!("fleetpodsd: fleet intent expired without a claim; resuming local players");
            self.session.resumed_players.clone_from(&players);
            effects.push(Effect::ResumePlayers(players));
        }
    }

    pub(crate) fn step_back(&mut self, now: Clock) {
        self.session.yield_pause_at = Some(now.mono);
        self.session.rejoin = None;
        self.session.background_requested = false;
        self.session.claim = PlaybackClaim::Idle;
        self.release_playback(Release::Handoff, now);
    }

    /// End setup, deferred player work and routing intent together. Saved
    /// stream restoration stays with Router; callers never reset these pieces.
    fn release_playback(&mut self, how: Release, now: Clock) {
        self.lifecycle.reset_audio();
        self.session.retire_player_work();
        self.session.resume_output_on_grant = false;
        self.routing.release_audio();
        let pause = matches!(how, Release::Handoff | Release::AppleReconnect);
        match how {
            Release::Handoff | Release::AppleReconnect => {
                // The router queues quieting ahead of Pause and control writes.
                self.output.handoff(self.config.accessory, self.last_graph.as_ref(), now.mono);
            }
            Release::Speakers | Release::AwaitAvailability => self.output.return_to_speakers(),
            Release::Preference => self.output.release(),
        }
        match how {
            Release::AwaitAvailability => return, // Keep local intent for the deferred claim.
            Release::AppleReconnect => self.seat.begin_reconnect(now),
            _ => self.seat.step_back(now),
        };
        let players = self.audio.yield_playback();
        if pause && !players.is_empty() {
            self.session.yield_paused.clone_from(&players);
            self.effects.pause_players = players;
        }
    }

    fn release_preference(&mut self, now: Clock) {
        self.release_playback(Release::Preference, now);
    }

    pub(super) fn queue_resume(&mut self, players: Vec<String>) {
        for player in players {
            if !self.session.resume_players.contains(&player) {
                self.session.resume_players.push(player);
            }
        }
    }

    pub(super) fn disconnect_session(&mut self, now: Clock) {
        self.session.rejoin = None;
        self.effects.drop_link = true;
        self.interrupt("disconnect", now);
    }

    pub(super) fn release(&mut self, now: Clock) -> Response {
        self.activity_at = None;
        self.idle_claimed = None;
        if self.routing.phase().busy() {
            return Response::line("fleet_routing_busy");
        }
        self.step_back(now);
        Response::line("ok")
    }

    pub(super) fn reconnect_peer(&mut self, now: Clock) -> Response {
        if self.bluetooth_bonded == Some(false) {
            return Response::line("error: Bluetooth pairing was not saved. Pair the AirPods again before reconnecting.");
        }
        if self.routing.phase().busy() {
            return Response::line("error: fleet routing in progress");
        }
        match self.connection.begin_apple_reconnect(now.mono) {
            Ok(()) => {
                self.session.background_requested = false;
                self.release_playback(Release::AppleReconnect, now);
                Response::line("accepted")
            }
            Err(message) => Response::line(message.trim_end()),
        }
    }

    /// Docked pods in an open case stay connected for a while (XPS,
    /// 2026-10-23: 23 s), and a play went to them in the case. Docking alone
    /// only pauses; a play while docked belongs on the speakers. The deferred
    /// claim still takes the pods back once a bud comes out.
    fn return_to_speakers(&mut self, now: Clock) {
        eprintln!("fleetpodsd: both pods are in the case; playing on the speakers");
        self.release_playback(Release::Speakers, now);
    }

    pub(crate) fn claim_playback(&mut self, now: Clock) {
        self.activity_at = None;
        self.session.stem_loss_at = None;
        self.session.stem_peer = None;
        self.session.stem_claims_sent = 0;
        // Play intent belongs to the pair in the user's ears. If a
        // tracked pair is worn and the selected pair is not, switch to
        // it first (its wear transition may have been missed, e.g. the
        // buds went in before the daemon started).
        let candidate = self
            .tracked
            .iter()
            .find(|(address, report)| {
                self.membership.accessory_keys_ready(**address) && super::worn(report)
            })
            .map(|(address, _)| *address);
        if let Some(candidate) = candidate {
            self.maybe_promote(candidate, now);
        }
        if self.pairing_elsewhere(now.mono) {
            self.cancel_connection_attempt(ConnectionHold::Pairing);
            return;
        }
        // Pods stowed in a closed case cannot take audio, and no link will
        // be opened for them. Claiming anyway would soft-mute the speaker
        // streams for the whole takeover hold while nothing can arrive, so
        // local playback would sit silent for seconds before falling back.
        if self.accessory.stowed() || self.accessory.both_docked() {
            self.session.claim = PlaybackClaim::Deferred(now.mono);
            self.return_to_speakers(now);

            return;
        }
        if self.connection.stage() != Stage::Ready
            && self.connection.bluez() == Some(false)
            && !self.accessory.playback_available(now.mono)
        {
            // A historical sighting or an open case is not evidence that the
            // buds can play. Stay on the speakers without probing the link:
            // absent pods only fail Host is down and cycle the adapter.
            // Advertisements alone make a bud available, and tick then
            // claims independently of control readiness. No seat yield:
            // playback stays local so the deferred claim can still fire.
            self.session.claim = PlaybackClaim::Deferred(now.mono);
            self.release_playback(Release::AwaitAvailability, now);

            return;
        }
        self.session.claim = PlaybackClaim::Idle;
        // Fleet admission only blocks opening a new link. A ready local link
        // can route a genuine play immediately, even before admission completes.
        if self.config.accessory.is_none() {
            return;
        }
        if self.routing.phase().busy() && self.connection.stage() != Stage::Ready {
            // Admission opens the link and the claim goes out once it is
            // ready. Hold speaker playback for it like any takeover, so the
            // play does not start on the speakers in the meantime. The hold
            // lasts until the ownership grant: a pods sink that is already
            // present (dual-connected) is not ownership.
            self.session.resume_output_on_grant = true;
            self.claim_seat(now);
            self.seat.note_claim(self.accessory.ownership.at());

            return;
        }
        self.session.resume_output_on_grant = self.connection.stage() == Stage::Ready
            && (self.seat.yielded()
                || self.accessory.ownership.as_fresh() == Some(&false)
                || self.accessory.source.as_fresh().is_some_and(|source| !source.local));
        self.claim_seat(now);
        self.open_connection(now.mono);
        self.seat.note_claim(self.accessory.ownership.at());

        // Reuse the live control link; a new link sends CLAIM once setup is ready.
        if self.connection.stage() == Stage::Ready {
            self.request_playback_ownership(now);
        }
    }

    /// The link report is stale. The playback claim stays until the user releases it.
    pub(super) fn drop_reports(&mut self) {
        self.roster_probe = None;
        self.roster_probe_after = None;
        self.session.resume_output_on_grant = false;
        self.session.ownership_loss_at = None;
        self.session.stem_loss_at = None;
        self.session.stem_peer = None;
        self.session.stem_claims_sent = 0;
        self.end_capture();
        self.microphone.set_control_ready(false);
        self.accessory.drop_owner();
        self.peer_playback.clear();
        self.idle_claimed = None;
        self.activity_at = None;
        self.pod_peers.clear();
        self.pod_peers_known = false;
        self.pod_peers_at = None;
    }

    pub fn interrupt(&mut self, reason: &'static str, now: Clock) {
        self.session.background_requested = false;
        self.connection.interrupt(reason);
        self.drop_reports();
        if reason == "disconnect" {
            self.step_back(now);
        }
        if matches!(reason, "suspend" | "bluez" | "adapter" | "disconnect") {
            self.routing.restart();
            if self.peers.has_air() {
                self.roster_dirty = true;
            }
            self.peers.invalidate();
        }
    }

    /// The rejoined link is ready: the players this laptop paused are its claim.
    fn claim_rejoin(&mut self, now: Clock) {
        if !self.session.rejoin.as_ref().is_some_and(|rejoin| rejoin.opened) {
            return;
        }
        let Some(Rejoin { players, .. }) = self.session.rejoin.take() else {
            return;
        };
        if self.outside_device_playing(now) {
            eprintln!("fleetpodsd: rejoin cancelled: a device outside the fleet is playing");
            return;
        }
        eprintln!("fleetpodsd: rejoined the AirPods; resuming {players:?}");
        self.claim_seat(now);
        self.queue_resume(players);
    }

    fn output_running(&self) -> bool {
        self.config.accessory.is_some_and(|address| self.output_health.running(address))
    }

    fn playback_running(&self) -> bool {
        self.transport_active && self.output_running()
    }

    fn lifecycle_facts(&self, now: MonoMs) -> crate::pods::lifecycle::Facts<'_> {
        crate::pods::lifecycle::Facts {
            accessory: self.config.accessory,
            generation: self.connection.generation(),
            control: self.connection.stage(),
            bluetooth: self.connection.bluez(),
            may_open: self.can_open_link(now),
            // The router owns output intent: no claim means this attempt
            // may only join in the background, never pursue audio.
            intent: if self.output.wants_output() {
                crate::pods::lifecycle::Intent::TakeAudio
            } else {
                crate::pods::lifecycle::Intent::JoinInBackground
            },
            early_audio_allowed: self.preferred_connection_allowed(),
            graph: self.last_graph.as_ref(),
            selected_codec: self.audio.selected(),
            routed: self.audio.routed(),
            recovering: self.output.recovering(),
            output_running: self.output_running(),
            transport_active: self.transport_active,
        }
    }

    pub(super) fn advance_connection_tasks(&mut self, now: MonoMs) -> Vec<crate::step::Effect> {
        use crate::pods::lifecycle::Action;
        use crate::step::Effect;
        // Borrow observations from the existing owners; advance the lifecycle gates.
        let mut lifecycle = std::mem::take(&mut self.lifecycle);
        let previous = lifecycle.progress().state;
        let actions = lifecycle.advance(&self.lifecycle_facts(now), now);
        let progress = lifecycle.progress();
        if previous != progress.state {
            eprintln!(
                "fleetpodsd: connection lifecycle {} -> {} waiting_for={}",
                previous.wire(),
                progress.state.wire(),
                progress.waiting_for
            );
        }
        self.lifecycle = lifecycle;
        let Some(accessory) = self.config.accessory else { return Vec::new() };
        actions
            .into_iter()
            .map(|action| match action {
                Action::OpenLink(generation) => {
                    eprintln!(
                        "fleetpodsd: opening control generation={} attempt={} background={}",
                        generation.0,
                        self.connection.retry().0,
                        self.session.background_requested
                    );
                    if !self.session.background_requested {
                        if let Some(peer) = self.peers_seated.first().copied() {
                            self.request_route(peer);
                        }
                    }
                    Effect::OpenLink { generation }
                }
                Action::ConnectAudio(id) => {
                    eprintln!("fleetpodsd: requesting audio profile {id:?}");
                    Effect::ConnectTransport { accessory, id }
                }
                Action::CompleteProfiles(id) => {
                    eprintln!("fleetpodsd: completing hands-free setup {id:?}");
                    Effect::ConnectHandsfree { accessory, id }
                }
                Action::SelectCodec { id, card, profile } => {
                    Effect::SelectProfile { id, card, profile }
                }
            })
            .collect()
    }

    pub(crate) fn note_transport_reply(
        &mut self,
        id: crate::pods::lifecycle::RequestId,
        reply: crate::step::TransportReply,
        now: MonoMs,
    ) {
        if self.lifecycle.note_audio(id, reply, now)
            && reply == crate::step::TransportReply::Connected
        {
            self.output.extend_takeover(now);
        }
    }

    pub(crate) fn note_profile(
        &mut self,
        id: crate::pods::lifecycle::RequestId,
        ok: bool,
        now: Clock,
    ) {
        if self.lifecycle.note_codec(id, ok, now.mono) && !ok {
            self.seat.note_failure_if_empty("profile was not selected", now);
        }
    }

    /// Request local playback when this machine is playing or has a queued
    /// resume, and no newer remote report has arrived. A remote report stored
    /// before the claim is older intent, even within the same millisecond.
    /// A remote report after the claim wins unless the pods grant the claim
    /// within [`CLAIM_ANSWER_MS`].
    pub(crate) fn wants_local_confirm(&self, now: MonoMs) -> bool {
        self.seat.wants_local_confirm(
            self.output.wants_output(),
            // A player paused by FleetPods still supplies playback intent.
            // Rejoining must claim before our Play reply arrives, otherwise
            // an opening owner report immediately cancels the resumed media.
            self.audio.local_playing() || !self.session.resume_players.is_empty(),
            self.audio.yielded(),
            &self.accessory.ownership,
            now,
        )
    }

    /// The one place an ownership claim is written to the control link.
    /// Callers own their gates; this owns the write and its log line.
    /// Sending CLAIM is not confirmation of playback.
    pub(crate) fn emit_ownership_claim(&mut self, reason: ClaimReason) {
        match reason {
            ClaimReason::Playback => {
                eprintln!("fleetpodsd: claiming AirPods audio on the existing control link")
            }
            ClaimReason::IdleActivity => {
                eprintln!("fleetpodsd: claiming idle AirPods ownership on desktop activity")
            }
            ClaimReason::OwnershipReassert => eprintln!(
                "fleetpodsd: local playback paused while owning the AirPods; re-asserting ownership"
            ),
            ClaimReason::StemContest => {}
        }
        self.effects.control_out.push((self.connection.generation(), setup::ownership(true)));
    }

    /// Ask on the existing control link. Ownership and source remain observations
    /// from the accessory; sending CLAIM is not confirmation of playback.
    fn request_playback_ownership(&mut self, now: Clock) -> bool {
        if !self.wants_local_confirm(now.mono) {
            return false;
        }
        self.emit_ownership_claim(ClaimReason::Playback);
        self.session.claim_sent_at = Some(now.mono);
        true
    }

    pub(crate) fn control_ready(&mut self, generation: Generation, now: Clock) {
        if !self.mark_control_ready(generation) {
            return;
        }

        if let Some(peer) = self.peers_seated.first().copied() {
            self.note_alongside(peer, now.unix);
        }
        self.session.link_ready_at = Some(now.mono);
        self.output.extend_takeover(now.mono);
        self.claim_rejoin(now);
        // A play waiting for this link asked before anything the pods
        // reported during setup, so those reports cannot override it.
        if self.output.wants_output() {
            self.seat.note_claim(self.accessory.ownership.at());
        }
        if self.request_playback_ownership(now) {
            self.session.setup_claim_at = Some(now.mono);
        }
        self.flush_settings();
        self.effects.announce_dirty = true;
    }

    pub(crate) fn observe_ownership(&mut self, local: bool, now: Clock) {
        eprintln!(
            "fleetpodsd: media event source=airpods ownership_local={local} mono_ms={}",
            now.mono.0
        );
        let was_local = self.accessory.ownership.as_fresh() == Some(&true);
        if !local && was_local && self.audio.local_playing() && self.output.wants_output() {
            self.session.ownership_loss_at = Some(now.mono);
        }
        self.note_ownership(local, now.mono);
        if local {
            self.session.ownership_loss_at = None;
            self.session.setup_claim_at = None;
            self.session.yield_pending = false;
        }
        if !local {
            // The pods end this laptop's A2DP stream, which can leave its
            // audio node failed. The next grant relinks unless the sink runs.
            self.session.rearm_after_loss = true;
            self.session.stem_paused = None;
            self.session.stem_resume_at = None;
            self.yield_to_remote_report(now);
        }
        self.effects.announce_dirty = true;
    }

    fn outside_owner(&self, now: Clock) -> Option<Address> {
        let peer = match self.resolved(now).holder {
            crate::audio::seat::Holder::Remote { address: Some(peer), .. } => Some(peer),
            crate::audio::seat::Holder::Remote { address: None, .. } => {
                self.accessory.remote_address.as_fresh().copied()
            }
            _ => None,
        }?;
        (self.local_address != Some(peer) && !self.membership.contains(peer)).then_some(peer)
    }

    /// Remember an outside takeover from active local playback. Its
    /// started session is contested below without reclaiming the
    /// output, which would restart the pods' routing competition.
    fn note_stem_context(&mut self, peer: Address, now: Clock) {
        self.session.stem_loss_at = Some(now.mono);
        self.session.stem_peer = Some(peer);
        self.session.stem_claims_sent = 0;
    }

    /// A stem click aimed at this laptop's active playback is delivered
    /// to the outside host. Once that session actually starts, answer
    /// it with ownership only; bounded repeats cover its insistence.
    pub(crate) fn contest_started_outside_playback(
        &mut self,
        peer: Address,
        playing: bool,
        now: Clock,
    ) {
        if !playing
            || self.session.stem_peer != Some(peer)
            || self.session.stem_claims_sent >= STEM_CONTEST_MAX_CLAIMS
        {
            return;
        }
        let Some(loss_at) = self.session.stem_loss_at else {
            return;
        };
        if now.mono.saturating_since(loss_at) > STEM_CONTEST_WINDOW_MS {
            self.session.stem_loss_at = None;
            self.session.stem_peer = None;
            self.session.stem_claims_sent = 0;
            return;
        }
        if self.connection.stage() != Stage::Ready || self.audio.local_playing() {
            return;
        }
        self.session.stem_claims_sent += 1;
        eprintln!(
            "fleetpodsd: outside peer {} started {} ms after ownership moved; contesting ownership ({}/{})",
            peer.display(),
            now.mono.saturating_since(loss_at),
            self.session.stem_claims_sent,
            STEM_CONTEST_MAX_CLAIMS
        );
        self.emit_ownership_claim(ClaimReason::StemContest);
    }

    pub(crate) fn observe_source(
        &mut self,
        local: bool,
        kind: u8,
        remote: Option<Address>,
        now: Clock,
    ) {
        eprintln!("fleetpodsd: media event source=airpods audio_local={local} kind={kind} remote={remote:?} mono_ms={}", now.mono.0);
        self.note_source(local, kind, now.mono);
        if local && kind > 0 {
            // Audio already reaches the pods from here; nothing to relink.
            self.session.resume_output_on_grant = false;
            self.session.rearm_after_loss = false;
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
            .session
            .setup_claim_at
            .is_some_and(|at| now.mono.saturating_since(at) < SETUP_CLAIM_SETTLE_MS)
        {
            if !self.session.yield_pending {
                eprintln!(
                    "fleetpodsd: deferring a remote report until this laptop's claim settles"
                );
            }
            self.session.yield_pending = true;
            return;
        }
        if self.output.wants_output()
            && self
                .session
                .claim_sent_at
                .is_some_and(|at| now.mono.saturating_since(at) < CLAIM_ANSWER_MS)
        {
            if !self.session.yield_pending {
                eprintln!("fleetpodsd: waiting for the pods to answer this laptop's claim");
            }
            self.session.yield_pending = true;
            return;
        }
        if self.connection.stage() == Stage::Ready && self.output.wants_output() {
            let stem_peer =
                if self.audio.local_playing() && self.session.ownership_loss_at.is_some() {
                    self.outside_owner(now)
                } else {
                    None
                };
            self.session.ownership_loss_at = None;
            eprintln!("fleetpodsd: AirPods audio moved remotely; pausing local players");
            self.session.yield_pause_at = Some(now.mono);
            self.step_back(now);
            self.session.release_output_until = Some(now.mono.saturating_add(RELEASE_OUTPUT_MS));
            if let Some(peer) = stem_peer {
                self.note_stem_context(peer, now);
            }
        }
    }
    pub(crate) fn observe_audio_graph(&mut self, graph: &crate::audio::route::Graph, now: Clock) {
        self.route_owned_playback(graph, now);
        self.last_graph = Some(graph.clone());
    }

    fn follow_graph(&mut self, graph: &crate::audio::route::Graph, now: Clock) {
        self.last_graph = Some(graph.clone());
        // The takeover hold applies while admission is still opening the
        // link, not only once it is ready: routing onto a present sink
        // before the grant lets the pods drop the stream back to the
        // speakers when the other laptop keeps ownership.
        let waiting_for_grant = self.output.wants_output()
            && (self.session.resume_output_on_grant
                || (self.connection.stage() != Stage::Ready && self.routing.phase().busy()));
        let outcome =
            self.output.on_graph(self.config.accessory, graph, waiting_for_grant, now.mono);
        if let Some(failure) = outcome.failure {
            self.output_recovery_failed(failure, now);
        }
        self.audio.set_routed(outcome.routed);
    }

    pub(crate) fn output_restarted(&mut self, id: u64, ok: bool, now: Clock) {
        if self.output.finish_recovery(id, ok) {
            eprintln!("fleetpodsd: AirPods sink restart {id} completed (ok={ok})");
            if !ok {
                self.output_recovery_failed("audio sink restart failed", now);
            } else {
                // The profile reply precedes stream relinking and Start.
                self.session.rebuild_pause_until = Some(now.mono.saturating_add(REBUILD_PAUSE_MS));
            }
        }
    }

    pub(crate) fn output_started(&mut self, id: u64, ok: bool, now: Clock) {
        if self.output.finish_start(id, ok) {
            if ok {
                self.session.output_recovery.started(id, now.mono);
            } else if matches!(
                self.session.output_recovery.task.state(),
                TaskState::Confirming { .. }
            ) {
                // A direct route's start failed: the confirmation deadline
                // turns the silent transport into a rebuild on its own.
                eprintln!("fleetpodsd: direct output start failed; awaiting the rebuild check");
            } else {
                self.output_recovery_failed("audio output start failed", now);
            }
        }
    }

    fn output_recovery_failed(&mut self, failure: &'static str, now: Clock) {
        self.session.retire_player_work();
        self.session.resume_output_on_grant = false;
        if failure == "audio takeover timed out"
            && self.audio.local_playing()
            && matches!(
                self.connection.stage(),
                Stage::Waiting
                    | Stage::Connecting
                    | Stage::Handshake
                    | Stage::Features
                    | Stage::Notifications
                    | Stage::Retrying
            )
        {
            // The link this claim is waiting for is still on its way: the
            // speakers keep the stream for now, and the deferred claim
            // lands the play when the link arrives.
            self.session.claim = PlaybackClaim::Deferred(now.mono);
            self.seat_audio_failure(failure, now);
        } else {
            self.session.claim = PlaybackClaim::Idle;
        }
    }
    pub(crate) fn audio_server_lost(&mut self) {
        self.lifecycle.audio_lost();
        self.last_graph = None;
        self.output.forget_server();
        self.session.retire_player_work();
        self.output_health.forget_server();
        self.audio.set_routed(false);
    }

    pub(crate) fn player_changed(&mut self, service: String, playing: Option<bool>, now: Clock) {
        if playing == Some(false)
            && self.output.wants_output()
            && self.audio.player_playing(&service)
            && (self.output.recovering()
                || self.session.rebuild_pause_until.is_some_and(|until| now.mono.0 < until.0))
        {
            eprintln!("fleetpodsd: player {service:?} paused during the AirPods rebuild; deferring resume until output start");
            self.queue_resume(vec![service.clone()]);
        }
        let was_playing = self.audio.player_playing(&service);
        let after_yield = self.audio.yielded();
        // Our own Play continues the claim already in progress.
        let resumed = playing == Some(true)
            && self
                .session
                .resumed_players
                .iter()
                .position(|s| s == &service)
                .map(|at| self.session.resumed_players.remove(at))
                .is_some();
        if self.audio.update_player(&service, playing) && !resumed {
            if self.replay_after_yield(&service, now) {
                self.effects.announce_dirty = true;
                return;
            }
            eprintln!("fleetpodsd: player {service:?} reported new Playing state (after_yield={after_yield})");
            self.remember_activity(now);
            self.claim_playback(now);
            self.effects.announce_dirty = true;
        }
        if playing == Some(false) && was_playing && !self.audio.local_playing() {
            self.reassert_ownership_on_pause(&service, now);
        }
        if playing == Some(true)
            && self.session.stem_paused.as_ref().is_some_and(|(name, _)| name == &service)
        {
            self.session.stem_paused = None;
            self.session.stem_resume_at = None;
        }
    }

    /// Pausing while this laptop owns the pods (a stem click lands here)
    /// leaves the roster owner unchanged, yet the pods retarget the next
    /// stem toggle to another connected host. Refresh ownership with one
    /// ownership-only claim so the resume toggle stays on this host. A
    /// later active play elsewhere claims after this and still takes over.
    /// Also remember the paused player: if the resume toggle lands on a
    /// fleet peer instead, its blip can ask for this player back.
    fn reassert_ownership_on_pause(&mut self, service: &str, now: Clock) {
        if self.connection.stage() != Stage::Ready
            || !self.output.wants_output()
            || self.accessory.ownership.as_fresh() != Some(&true)
        {
            return;
        }
        self.session.stem_paused = Some((service.to_string(), now.mono));
        self.emit_ownership_claim(ClaimReason::OwnershipReassert);
    }

    /// A fleet peer started playing while this laptop owns the pods and
    /// has a player paused on them. The pods deliver stem toggles to the
    /// peer regardless of ownership, so its brief started report is most
    /// likely a resume click meant for this host. Give a genuine takeover
    /// a short beat to claim ownership, then resume the paused player.
    pub(crate) fn note_peer_stem_relay(&mut self, peer: Address, playing: bool, now: Clock) {
        if !playing
            || !self.membership.contains(peer)
            || self.accessory.ownership.as_fresh() != Some(&true)
            || self.audio.local_playing()
            || self.session.stem_paused.is_none()
        {
            return;
        }
        self.session.stem_resume_at = Some(now.mono.saturating_add(STEM_RESUME_DELAY_MS));
        eprintln!(
            "fleetpodsd: fleet peer {} started while this laptop owns the AirPods; treating it as a possible stem blip",
            peer.display()
        );
    }

    /// Fire a scheduled stem relay if the wait confirmed it: ownership
    /// held, no fleet intent claim is in flight, the peer stopped again,
    /// and the paused player is still paused.
    fn fire_stem_resume(&mut self, now: Clock, effects: &mut Vec<Effect>) {
        let Some(at) = self.session.stem_resume_at else {
            return;
        };
        if now.mono.0 < at.0 {
            return;
        }
        self.session.stem_resume_at = None;
        let Some((player, _)) = self.session.stem_paused.take() else {
            return;
        };
        if self.accessory.ownership.as_fresh() != Some(&true)
            || self.audio.local_playing()
            || self.audio.player_playing(&player)
            || self.session.intent_until.is_some_and(|until| now.mono.0 < until.0)
            || self.peer_playback.keys().any(|peer| {
                self.membership.contains(*peer)
                    && self.remote_playback(*peer, now).is_some_and(|(playing, _)| playing)
            })
        {
            return;
        }
        eprintln!("fleetpodsd: peer stem blip confirmed; resuming paused player {player:?}");
        self.session.resumed_players = vec![player.clone()];
        effects.push(Effect::ResumePlayers(vec![player]));
    }
    /// Classify a player reporting Playing after this laptop yielded.
    /// Two replays are not takeover presses and are re-paused here:
    ///
    /// - a stem toggle: the pods toggle every connected laptop's
    ///   players, so a player this yield paused comes back whenever the
    ///   active device is paused or resumed from the pods;
    /// - a self-rebound: within the rebound window, an output-device
    ///   change looks like an ended interruption and restarts playback
    ///   while the remote owner is playing.
    ///
    /// Desktop input after the yield (a click, or space over the media)
    /// makes either one a real press that takes over normally.
    fn replay_after_yield(&mut self, service: &str, now: Clock) -> bool {
        let Some(at) = self.session.yield_pause_at else {
            return false;
        };
        let input = self.session.last_input_at;
        if self.session.yield_paused.iter().any(|name| name == service)
            && self.accessory.ownership.as_fresh() != Some(&true)
            && !input.is_some_and(|at_input| at_input > at)
        {
            eprintln!(
                "fleetpodsd: player {service:?} toggled by the pods while yielded; keeping it paused"
            );
            self.effects.pause_players.push(service.to_string());
            return true;
        }
        if now.mono.saturating_since(at) > REBOUND_WINDOW_MS {
            self.session.yield_pause_at = None;
            return false;
        }
        // An ownership-only report names no peer address, so the resolved
        // holder cannot carry the peer's playing state; the relayed peer
        // media report can. Consult it when the holder names nobody.
        let remote_playing = match self.resolved(now).holder {
            crate::audio::seat::Holder::Remote { address: None, playing } => {
                playing
                    || self.peer_playback.keys().any(|peer| {
                        self.remote_playback(*peer, now).is_some_and(|(observed, _)| observed)
                    })
            }
            crate::audio::seat::Holder::Remote { playing, .. } => playing,
            _ => false,
        };
        if !remote_playing {
            return false;
        }
        // Fresh desktop input after the yield makes this a press, not a
        // rebound: someone came to this keyboard after the other laptop
        // took over (the space bar counts; no mouse is needed). Input
        // from before the yield is just the session that got paused.
        // The yield is over, so the guard lifts with it.
        if input
            .is_some_and(|input| input >= at && now.mono.saturating_since(input) <= INPUT_INTENT_MS)
        {
            self.session.yield_pause_at = None;
            self.session.last_input_at = None;
            eprintln!(
                "fleetpodsd: player {service:?} played after desktop input; treating it as a press"
            );
            return false;
        }
        // The stamp is not spent: a player whose output device keeps
        // flipping can rebound several times inside the window, and every
        // replay would restart the claim war. It expires with the window.
        eprintln!(
            "fleetpodsd: player {service:?} replayed right after a yield; treating it as a rebound"
        );
        self.effects.pause_players.push(service.to_string());
        true
    }
    fn settle_playback_reports(&mut self, now: Clock) {
        // A deferred remote report that was never answered by a grant ends
        // the claim once both the claim-answer and setup-settle windows close.
        if !self.session.yield_pending {
            return;
        }
        let claim_answered = self
            .session
            .claim_sent_at
            .is_none_or(|at| now.mono.saturating_since(at) >= CLAIM_ANSWER_MS);
        let setup_settled = self
            .session
            .setup_claim_at
            .is_none_or(|at| now.mono.saturating_since(at) >= SETUP_CLAIM_SETTLE_MS);
        if claim_answered && setup_settled {
            self.session.yield_pending = false;
            if self.accessory.ownership.as_fresh() != Some(&true) {
                self.yield_to_remote_report(now);
            }
        }
    }
    fn announce_session(&mut self, now: Clock, effects: &mut Vec<Effect>) {
        if self.effects.announce_dirty {
            self.effects.announce_dirty = false;
            if let Some(address) = self.local_address {
                let seated = self.connection.stage() == Stage::Ready;
                let roster = self.audio_roster(now);
                effects.push(Effect::Announce(crate::fleet::postcard::encode_announce(
                    address,
                    &self.local_name,
                    seated,
                    roster.as_ref(),
                )));
            }
        }
    }

    pub(super) fn can_repair_output(&self) -> bool {
        self.connection.stage() == Stage::Ready
            && !self.suspending
            && !self.adapter_down
            && !self.routing.phase().busy()
            && !self.accessory.both_docked()
            && self.output.wants_output()
            && self.output.held_streams().is_empty()
            && !self.output.holding_takeover()
            && !self.output.recovering()
            && self.accessory.ownership.as_fresh() == Some(&true)
            && !self.session.output_recovery.repair_requested
    }

    pub(super) fn reset_audio_output(&mut self) -> Response {
        if !self.can_repair_output() {
            return Response::line("error: this machine must own the ready AirPods output, with no handoff or reset in progress");
        }
        if self.config.accessory.and_then(|a| self.output_health.output(a)).is_none() {
            return Response::line("error: no AirPods audio output");
        }
        self.session.output_recovery.request_repair();
        Response::line("accepted: rebuilding AirPods audio output")
    }

    pub(super) fn output_health_status(&self) -> serde_json::Value {
        serde_json::json!({
            "state": self.config.accessory.and_then(|a| self.output_health.output(a)).map(|o| o.state),
            "resets": self.session.output_recovery.resets,
            "last_reset_ok": self.session.output_recovery.last_ok,
        })
    }

    /// Final setup/recovery/player work, after admission and intent decisions.
    fn finish_session(&mut self, now: Clock, effects: &mut Vec<Effect>) {
        let accessory = self.config.accessory;
        let players = std::mem::take(&mut self.effects.pause_players);
        if !players.is_empty() {
            // Pausing is the daemon's own decision; do not undo it.
            self.session.resume_players.clear();
            effects.push(Effect::PausePlayers(players));
        }
        if let Some(until) = self.session.release_output_until {
            if self.output.wants_output() || !self.transport_active || now.mono.0 >= until.0 {
                self.session.release_output_until = None;
            } else if let Some(idle) = self.output_health.outputs.iter().find(|output| {
                output.address.is_some() && output.address == accessory && output.state != 0
            }) {
                // Paused players have left the output. PipeWire would keep
                // streaming silence to the pods for another second or so.
                eprintln!("fleetpodsd: releasing the idle AirPods output for the new owner");
                effects.push(Effect::SuspendOutput { name: idle.name.clone() });
                self.session.release_output_until = None;
            }
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
        // Ownership loss can damage an existing audio node, not one that has
        // yet to be created. Opening reports can still name the previous owner.
        // Do not carry their recovery flags forward onto a fresh output.
        let has_output = self.config.accessory.is_some_and(|address| {
            self.last_graph
                .as_ref()
                .is_some_and(|graph| graph.sinks.iter().any(|sink| sink.address == Some(address)))
        });
        if !has_output {
            self.session.rearm_after_loss = false;
            self.session.resume_output_on_grant = false;
        }
        let granted = self.output.wants_output()
            && self.connection.stage() == Stage::Ready
            && self.accessory.ownership.as_fresh() == Some(&true);
        let retry = self.session.output_recovery.retry_due(
            granted,
            self.playback_running(),
            self.audio.local_playing(),
            now.mono,
        );
        if granted && self.playback_running() {
            // Neither observation alone proves readiness. The same joint
            // predicate drives verification and the lifecycle's final gates.
            self.session.rearm_after_loss = false;
            self.session.resume_output_on_grant = false;
        }
        let watchdog = self
            .output_health
            .check(
                self.config.accessory,
                self.last_graph.as_ref(),
                self.can_repair_output()
                    && self.audio.local_playing()
                    && self.session.output_recovery.watchdog_allowed(),
                now.mono,
            )
            .is_some();
        let repair = self.session.output_recovery.repair_requested || watchdog;
        let requested =
            self.session.resume_output_on_grant || self.session.rearm_after_loss || repair;
        if granted && (requested || retry) {
            // A known failed node needs a rebuild now, even if it subsequently
            // reported suspended. Other outputs get the direct route and a
            // bounded check of both sink and transport. The router owns either
            // path; only the rebuild pauses players below.
            let failed_output = self.config.accessory.is_some_and(|a| self.output_health.failed(a));
            let direct = requested && !retry && !failed_output && !repair;
            let began = match (self.config.accessory, self.last_graph.as_ref()) {
                (Some(address), Some(graph)) => self.session.output_recovery.begin(
                    requested,
                    self.connection.generation(),
                    now.mono,
                    direct,
                    |id| self.output.begin_output(id, direct, address, graph, now.mono),
                ),
                _ if direct => self.session.output_recovery.begin(
                    requested,
                    self.connection.generation(),
                    now.mono,
                    true,
                    |_| true,
                ),
                _ => false,
            };
            if began {
                if direct {
                    self.session.resume_output_on_grant = false;
                    self.session.rearm_after_loss = false;
                    eprintln!(
                        "fleetpodsd: routing directly after ownership grant; verifying running output and active transport"
                    );
                } else {
                    self.output_health.started(now.mono);
                    // Stop the media clock while its output is unavailable.
                    // This pause retains the playback claim, unlike yielding.
                    let players = self.audio.playing_players();
                    self.queue_resume(players.clone());
                    if !players.is_empty() {
                        effects.push(Effect::PausePlayers(players));
                    }
                    self.session.resume_output_on_grant = false;
                    self.session.rearm_after_loss = false;
                    eprintln!("fleetpodsd: rebuilding AirPods playback (failed_node={failed_output}, verification_retry={retry}, repair={repair})");
                }
            }
        }
        // Decide routing once, after every intent/recovery change for this input.
        // Handlers only update intent and observations; none can replace queued work.
        if !self.radio_quiet() {
            if let Some(graph) = self.last_graph.clone() {
                self.follow_graph(&graph, now);
            }
        }
        let setup = self.advance_connection_tasks(now.mono);
        // A new socket must be installed before writes already queued by its
        // setup callback. The dispatcher does not need this ordering rule.
        let setup_at = effects
            .iter()
            .position(|effect| {
                matches!(
                    effect,
                    Effect::OpenLink { .. } | Effect::FollowLink | Effect::WriteControl { .. }
                )
            })
            .unwrap_or(effects.len());
        effects.splice(setup_at..setup_at, setup);
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
                    effects.push(Effect::StartOutput { id: start.id, name: start.name });
                }
                crate::audio::route::Work::SetDefault(name) => {
                    effects.push(Effect::SetDefaultSink { name });
                }
                crate::audio::route::Work::Move(moves) => {
                    effects.push(Effect::MoveStreams(moves));
                }
            }
        }
        if !self.output.wants_output() {
            self.session.retire_player_work();
        } else if !self.session.resume_players.is_empty()
            && !self.session.yield_pending
            && !self.output.recovering()
        {
            // A deferred remote report decides before a queued resume can play.
            let players = std::mem::take(&mut self.session.resume_players);
            self.session.resumed_players.clone_from(&players);
            effects.push(Effect::ResumePlayers(players));
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::tests::*;
    use crate::fleet::members::{GroupKey, Member};
    use crate::types::Battery;

    fn recovery_id(daemon: &Daemon) -> Option<u64> {
        match daemon.session.output_recovery.task.state() {
            TaskState::Running { id, .. }
            | TaskState::Confirming { id, .. }
            | TaskState::Succeeded { id: Some(id) } => Some(id.serial),
            _ => None,
        }
    }

    fn output(state: i32) -> crate::audio::health::Output {
        crate::audio::health::Output {
            name: "pods".into(),
            index: 7,
            address: Some(addr(3)),
            profile: Some("a2dp-sink".into()),
            state,
            muted: false,
            audible_volume: true,
        }
    }

    #[test]
    fn preference_loss_yields_the_seat_and_the_audio_together() {
        use crate::fleet::members::Fleet;
        let mut daemon = Daemon::new("gram");
        let now = clock();
        let local = addr(1);
        let peer = addr(2);
        daemon.set_local_address(Some(local));
        daemon.restore_fleet(Fleet {
            id: FleetId::new([3; 16]),
            key: GroupKey::new([0x11; 32]),
            members: vec![
                Member { address: local, label: "lg-gram".into() },
                Member { address: peer, label: "omarchy-xps".into() },
            ],
        });
        daemon.select_accessory(Some(addr(3)));
        daemon.note_bluez(true);
        daemon.activity_preference.note_local([3; 16], addr(3), peer, now.unix);
        // A playing player, without the Play input that would make this machine preferred.
        daemon.audio.update_player("org.mpris.MediaPlayer2.brave", Some(true));
        daemon.output.claim();
        daemon.reconnect_preferred(now);
        assert!(daemon.seat.yielded());
        assert!(daemon.audio.yielded());
        assert!(daemon.effects.pause_players.is_empty(), "a preference release does not pause");
        assert!(!daemon.output.wants_output());

        let mut speakers = Daemon::new("gram");
        speakers.output.claim();
        speakers.return_to_speakers(now);
        assert!(speakers.seat.yielded());
        assert!(speakers.audio.yielded());
        assert!(speakers.effects.pause_players.is_empty());
        assert!(!speakers.output.wants_output());
    }

    #[test]
    fn disconnect_cancels_the_entire_apple_reconnect_sequence() {
        use crate::step::{Effect, Input};
        let mut daemon = Daemon::new("xps");
        let now = clock();
        ready_link(&mut daemon, now);
        daemon.note_bluez(true);
        let started = daemon.step(Input::Command(Command::ReconnectPeer), now);
        assert_eq!(started.reply.as_deref(), Some("accepted\n"));
        daemon.step(Input::Command(Command::Disconnect), now);
        let generation = daemon.connection.generation();
        for elapsed in [1, 2, 3, 6_000, 20_000, 60_000] {
            let tick =
                daemon.step(Input::Tick, Clock::new(now.mono.0 + elapsed, now.unix.0 + elapsed));
            assert!(
                !tick.effects.iter().any(|e| matches!(
                    e,
                    Effect::OpenLink { .. }
                        | Effect::Disconnect { .. }
                        | Effect::WriteControl { .. }
                )),
                "a cancelled reconnect must not resume its cadence: {:?}",
                tick.effects
            );
            assert!(!daemon.connection.apple_reconnect());
            assert_eq!(daemon.connection.generation(), generation);
        }
    }

    #[test]
    fn disconnecting_docked_pods_does_not_pause_playback_resumed_on_speakers() {
        use crate::step::{Effect, Input};
        let mut daemon = Daemon::new("xps");
        let now = clock();
        let at = |ms| Clock::new(now.mono.0 + ms, now.unix.0 + ms);
        let brave = "org.mpris.MediaPlayer2.brave";
        ready_link(&mut daemon, now);
        daemon.audio.startup_players(&[]);
        for right in [false, true] {
            daemon.note_pod(
                right,
                Pod {
                    battery: Battery::new(80, true).unwrap(),
                    in_ear: Some(false),
                    in_case: Some(true),
                },
                now.mono,
            );
        }
        daemon.step(Input::Player { service: brave.into(), playing: Some(true) }, at(1_000));
        assert!(daemon.audio.local_playing());
        assert!(!daemon.output.wants_output());
        assert!(matches!(daemon.session.claim, PlaybackClaim::Deferred(_)));
        let generation = daemon.connection.generation();
        let mut effects =
            daemon.step(Input::ControlSocket { up: false, generation }, at(7_000)).effects;
        effects.extend(daemon.step(Input::Bluez(false), at(7_000)).effects);
        effects.extend(daemon.step(Input::Lid { open: false }, at(7_100)).effects);
        effects.extend(daemon.step(Input::Tick, at(8_000)).effects);
        assert!(
            effects.iter().all(|e| !matches!(e, Effect::PausePlayers(_) | Effect::MuteStreams(_))),
            "{effects:?}"
        );
        assert!(daemon.audio.local_playing());
        assert!(
            matches!(daemon.session.claim, PlaybackClaim::Deferred(_)),
            "insertion can still claim this play"
        );
    }

    #[test]
    fn playback_with_no_sign_of_the_pods_does_not_open_a_link() {
        use crate::step::{Effect, Input};

        let mut daemon = Daemon::new("omarchy-xps");
        let now = clock();
        daemon.select_accessory(Some(addr(9)));
        daemon.connection.note_bluez(false);
        assert!(!daemon.accessory.playback_available(now.mono));
        use crate::audio::route::{Graph, LiveStream, Sink};
        daemon.step(
            Input::Audio(Graph {
                default_sink: "speakers".into(),
                sinks: vec![Sink { name: "speakers".into(), address: None, profile: None }],
                cards: vec![],
                streams: vec![LiveStream {
                    id: 1,
                    generation: 10,
                    sink: "speakers".into(),
                    role: String::new(),
                    node: "Brave".into(),
                    mute: false,
                    volume: 1.0,
                    corked: false,
                }],
            }),
            now,
        );
        let played = daemon.step(Input::Command(play("unseen")), now);
        // No sign of the pods: speakers keep playing and no link is probed.
        // Probing absent pods only fails Host is down and cycles the adapter.
        assert!(!played.effects.iter().any(|e| matches!(e, Effect::OpenLink { .. })));
        assert!(!daemon.output.wants_output());
        assert!(matches!(daemon.session.claim, PlaybackClaim::Deferred(_)));
        // A bud appearing while playback continues still claims the seat.
        daemon.accessory.note_placement(false, Some(true), Some(false), now.mono);
        let appeared = daemon.step(Input::Tick, now);
        assert!(appeared.effects.iter().any(|e| matches!(e, Effect::OpenLink { .. })));
        assert!(daemon.output.wants_output());
        assert!(!matches!(daemon.session.claim, PlaybackClaim::Deferred(_)));
        let mutes: Vec<_> = appeared
            .effects
            .iter()
            .filter_map(|e| match e {
                Effect::MuteStreams(streams) => Some(streams),
                _ => None,
            })
            .collect();
        assert_eq!(mutes.len(), 1, "the deferred claim and tick must not overwrite its mute");
        assert_eq!(mutes[0][0].id, 1);
        assert!(matches!(appeared.effects.first(), Some(Effect::MuteStreams(_))));
    }

    #[test]
    fn playback_with_no_sign_of_the_pods_defers_the_claim_until_they_appear() {
        let mut daemon = Daemon::new("omarchy-xps");
        let now = clock();
        daemon.select_accessory(Some(addr(9)));
        daemon.connection.note_bluez(false);
        assert!(!daemon.accessory.playback_available(now.mono));
        daemon.handle(play("deferred"), now);
        // Playing, but never seen this session: no claim, no muted speakers.
        assert!(!daemon.output.wants_output());
        assert!(matches!(daemon.session.claim, PlaybackClaim::Deferred(_)));
        // Still nothing seen: a tick does not claim either.
        daemon.step(crate::step::Input::Tick, now);
        assert!(!daemon.output.wants_output());
        // Opening the case alone must not silence the speakers.
        daemon.note_lid(true, now.mono);
        daemon.step(crate::step::Input::Tick, now);
        assert!(!daemon.output.wants_output());
        daemon.accessory.note_placement(false, Some(true), Some(false), now.mono);
        daemon.step(crate::step::Input::Tick, now);
        assert!(daemon.output.wants_output());
        assert!(!matches!(daemon.session.claim, PlaybackClaim::Deferred(_)));
    }

    #[test]
    fn activity_after_lid_wake_and_existing_acl_share_bounded_setup_retries() {
        use crate::step::{Effect, Input};
        // Activity after opening the lid and attachment to an existing ACL
        // both need tracked intent for the retry loop.
        for (existing_acl, released) in [(false, false), (false, true), (true, false), (true, true)]
        {
            let mut daemon = Daemon::new("xps");
            daemon.select_accessory(Some(addr(9)));
            if released {
                // Reproduce the original reset after an earlier audio yield.
                daemon.routing.release_audio();
            }
            let mut now = clock();
            let no_open =
                |effects: &[Effect]| !effects.iter().any(|e| matches!(e, Effect::OpenLink { .. }));
            if existing_acl {
                daemon.step(Input::Bluez(true), now);
                let generation = daemon.connection.generation();
                let attach = daemon.step(Input::AttachAvailable { generation }, now);
                assert!(!no_open(&attach.effects));
            } else {
                daemon.connection.prepare();
                let opening = daemon.step(Input::Lid { open: true }, now);
                assert!(no_open(&opening.effects));
                daemon.step(Input::Command(Command::Activity), now);
                let start = now;
                // Repeated advertisements neither bypass nor restart the grace.
                for elapsed in [500, 1_999] {
                    now = Clock::new(start.mono.0 + elapsed, start.unix.0 + elapsed);
                    for input in [Input::Lid { open: true }, Input::Tick] {
                        assert!(no_open(&daemon.step(input, now).effects));
                    }
                }
                now = Clock::new(start.mono.0 + 2_000, start.unix.0 + 2_000);
                assert!(!no_open(&daemon.step(Input::Tick, now).effects));
            }
            assert!(daemon.session.background_requested);
            assert!(!daemon.output.wants_output(), "background setup must not claim audio");
            for attempt in 1..=crate::pods::link::RETRY_LIMIT {
                let generation = daemon.connection.generation();
                daemon.step(Input::ControlSocket { up: false, generation }, now);
                daemon.step(Input::Bluez(false), now);
                daemon.step(Input::LinkFailed { generation, busy: false }, now);
                if attempt == crate::pods::link::RETRY_LIMIT {
                    assert_eq!(daemon.connection.stage(), Stage::Failed);
                    assert!(!daemon.session.background_requested);
                    now = Clock::new(now.mono.0 + 120_000, now.unix.0 + 120_000);
                    daemon.step(Input::Bluez(true), now);
                    assert!(no_open(
                        &daemon.step(Input::AttachAvailable { generation }, now).effects
                    ));
                    assert!(no_open(&daemon.step(Input::Tick, now).effects));
                } else {
                    assert_eq!(daemon.connection.stage(), Stage::Retrying);
                    let delay = 1_000 << (attempt - 1);
                    let early = Clock::new(now.mono.0 + delay - 1, now.unix.0 + delay - 1);
                    assert!(no_open(&daemon.step(Input::Tick, early).effects));
                    now = Clock::new(now.mono.0 + delay, now.unix.0 + delay);
                    let retry = daemon.step(Input::Tick, now);
                    assert!(!no_open(&retry.effects), "retry needs no second activity event");
                    assert_eq!(daemon.connection.retry().0, attempt + 1);
                    let current = daemon.connection.generation();
                    assert_ne!(current, generation);
                    daemon.step(Input::LinkFailed { generation, busy: false }, now);
                    assert_eq!(daemon.connection.generation(), current);
                    assert_eq!(daemon.connection.stage(), Stage::Connecting);
                }
            }
        }
    }

    #[test]
    fn an_idle_laptop_does_not_connect_when_the_case_opens_and_a_peer_connects() {
        use crate::step::{Effect, Input};
        let mut daemon = conflicting_roster_setup();
        let now = clock();
        let generation = daemon.connection.generation();
        daemon.step(Input::ControlSocket { up: false, generation }, now);
        daemon.step(Input::Bluez(false), now);
        for right in [false, true] {
            daemon.note_pod(
                right,
                Pod {
                    battery: Battery::new(80, true).unwrap(),
                    in_ear: Some(false),
                    in_case: Some(true),
                },
                now.mono,
            );
        }
        daemon.step(Input::Lid { open: false }, now);
        assert_eq!(daemon.connection.stage(), Stage::Waiting);
        // The other laptop and phone take the slots. Repeat open-lid reports
        // through both the case-wake and former reconnect-order grace periods.
        for elapsed in [1_000, 3_000, 10_000, 30_000, 60_000] {
            let at = Clock::new(now.mono.0 + elapsed, now.unix.0 + elapsed);
            daemon.hear_roster(conflicting_roster(at, elapsed), at);
            for input in [Input::Lid { open: true }, Input::Tick] {
                let result = daemon.step(input, at);
                assert!(!result
                    .effects
                    .iter()
                    .any(|e| matches!(e, Effect::OpenLink { .. } | Effect::WriteControl { .. })));
            }
            assert!(!daemon.session.background_requested);
            assert_eq!(daemon.connection.stage(), Stage::Waiting);
            assert_eq!(daemon.status_value(at)["lid_state"], 0);
        }
    }

    #[test]
    fn activity_asks_the_connected_audio_owner_instead_of_waiting_out_the_reopen_hold() {
        use crate::fleet::members::{Fleet, Member};
        use crate::step::Input;
        for owner_connected in [true, false] {
            let now = clock();
            let local = addr(1);
            let xps = addr(2);
            let pods = addr(9);
            let mut daemon = Daemon::new("gram");
            daemon.set_local_address(Some(local));
            daemon.select_accessory(Some(pods));
            daemon.restore_fleet(Fleet {
                id: FleetId::new([3; 16]),
                key: GroupKey::new([0x11; 32]),
                members: vec![
                    Member { address: local, label: "gram".into() },
                    Member { address: xps, label: "xps".into() },
                ],
            });
            daemon.observe_nearby(host_id(Some(xps)), xps, "xps", now.unix, true, Some([4; 16]));
            daemon.note_roster(RosterReport {
                reporter: host_id(Some(xps)),
                reporter_address: xps,
                accessory: pods,
                session: crate::types::SessionId(0),
                seq: 1,
                at: now.mono,
                sent: now.unix,
                peers: if owner_connected { vec![xps] } else { Vec::new() },
                audio_source: owner_connected.then_some(xps),
                pending: false,
                audio_idle: false,
                intent: false,
                wire_session: Some([4; 16]),
                last_use: UnixMs(0),
            });
            // This laptop's link dropped while the XPS owned the pair's audio.
            daemon.reopen_wait = true;
            daemon.accessory.left = crate::types::Evidence::fresh(
                Pod {
                    battery: crate::types::Battery::new(60, false).unwrap(),
                    in_ear: Some(true),
                    in_case: Some(false),
                },
                now.mono,
            );
            daemon.step(Input::Command(Command::Activity), now);
            if owner_connected {
                // The owner already landed: ask it for a seat alongside it.
                assert_eq!(daemon.routing.phase(), Phase::WaitingAdmission);
                assert!(!daemon.reopen_wait);
            } else {
                // The owner is not back yet, so it still gets to land first.
                assert!(!daemon.routing.phase().busy());
                assert!(daemon.reopen_wait);
                assert!(!daemon.session.background_requested);
            }
        }
    }

    #[test]
    fn pausing_while_owning_the_pods_reasserts_ownership() {
        use crate::pods::observe::Observation;
        use crate::step::{Effect, Input};
        let mut daemon = Daemon::new("gram");
        let now = clock();
        let at = |ms: u64| Clock::new(now.mono.0 + ms, now.unix.0 + ms);
        let service = "org.mpris.MediaPlayer2.brave";
        let claims_in = |effects: &[Effect]| {
            effects
                .iter()
                .filter(|effect| {
                    matches!(effect,
                        Effect::WriteControl { bytes, .. }
                            if *bytes == crate::pods::setup::ownership(true))
                })
                .count()
        };
        ready_link(&mut daemon, now);
        daemon.step(Input::Players(Vec::new()), now);
        daemon.step(Input::Player { service: service.into(), playing: Some(true) }, now);
        daemon.step(Input::Observation(Observation::Ownership(true)), at(10));

        // The active host pauses (a stem click lands here): one
        // ownership-only re-assert, and nothing resumes or re-pauses.
        let paused =
            daemon.step(Input::Player { service: service.into(), playing: Some(false) }, at(1_000));
        assert_eq!(claims_in(&paused.effects), 1, "pause while owner re-asserts ownership");
        assert!(
            !paused
                .effects
                .iter()
                .any(|effect| matches!(effect, Effect::ResumePlayers(_) | Effect::PausePlayers(_))),
            "re-asserting ownership must not touch the players"
        );

        // Pausing while a peer owns the pods asserts nothing.
        daemon.step(Input::Player { service: service.into(), playing: Some(true) }, at(2_000));
        daemon.step(Input::Observation(Observation::Ownership(false)), at(2_100));
        let paused_remote =
            daemon.step(Input::Player { service: service.into(), playing: Some(false) }, at(2_200));
        assert_eq!(
            claims_in(&paused_remote.effects),
            0,
            "pause while a peer owns the pods must not claim"
        );
    }

    #[test]
    fn a_fleet_peer_stem_blip_resumes_the_paused_owner_player() {
        use crate::fleet::members::{Fleet, Member};
        use crate::pods::observe::Observation;
        use crate::step::{Effect, Input};
        // (scenario, peer stops again, ownership held) -> expect resume
        for (scenario, blip, held, expect_resume) in [
            ("stem blip while owner", true, true, true),
            ("peer keeps playing", false, true, false),
            ("ownership taken during the wait", true, false, false),
        ] {
            let local = addr(1);
            let peer = addr(2);
            let mut daemon = Daemon::new("gram");
            let now = clock();
            let at = |ms: u64| Clock::new(now.mono.0 + ms, now.unix.0 + ms);
            let service = "org.mpris.MediaPlayer2.brave";
            daemon.set_local_address(Some(local));
            daemon.restore_fleet(Fleet {
                id: FleetId::new([3; 16]),
                key: GroupKey::new([0x11; 32]),
                members: vec![
                    Member { address: local, label: "lg-gram".into() },
                    Member { address: peer, label: "omarchy-xps".into() },
                ],
            });
            ready_link(&mut daemon, now);
            daemon.step(Input::Players(Vec::new()), now);
            daemon.step(
                Input::Observation(Observation::PodPeers {
                    peers: vec![local, peer],
                    owner: Some(local),
                }),
                now,
            );
            daemon.step(Input::Player { service: service.into(), playing: Some(true) }, now);
            daemon.step(Input::Observation(Observation::Ownership(true)), at(10));
            // Stem pause on the owning laptop.
            daemon.step(Input::Player { service: service.into(), playing: Some(false) }, at(1_000));
            // The pods deliver the resume click to the fleet peer instead.
            daemon.step(
                Input::Observation(Observation::PeerPlaying { address: peer, playing: true }),
                at(1_100),
            );
            if blip {
                daemon.step(
                    Input::Observation(Observation::PeerPlaying { address: peer, playing: false }),
                    at(1_200),
                );
            }
            if !held {
                daemon.step(Input::Observation(Observation::Ownership(false)), at(1_300));
            }
            let fired = daemon.step(Input::Tick, at(2_000));
            let resumed = fired.effects.iter().any(|effect| {
                matches!(effect, Effect::ResumePlayers(names) if names == &[service.to_string()])
            });
            assert_eq!(resumed, expect_resume, "{scenario}");
        }
    }

    #[test]
    fn a_yielded_slot_holds_activity_but_explicit_connect_and_play_can_reopen() {
        use crate::step::{Effect, Input};
        for explicit_play in [false, true] {
            let mut daemon = Daemon::new("xps");
            let now = clock();
            ready_link(&mut daemon, now);
            daemon.set_local_address(Some(addr(1)));
            daemon.peers_seated.push(addr(2));
            daemon.step(Input::Players(Vec::new()), now);
            daemon.free_slot(addr(2), now);
            let generation = daemon.connection.generation();
            daemon.step(Input::ControlSocket { up: false, generation }, now);
            assert_eq!(daemon.slot_hold_until, Some(now.mono.saturating_add(SLOT_HOLD_MS)));
            let activity = daemon.step(Input::Command(Command::Activity), now);
            assert!(!activity.effects.iter().any(|e| matches!(e, Effect::OpenLink { .. })));
            assert!(!daemon.session.background_requested);
            let input = if explicit_play {
                Input::Player { service: "browser".into(), playing: Some(true) }
            } else {
                Input::Command(Command::Connect)
            };
            let requested = daemon.step(input, now);
            assert!(requested.effects.iter().any(|e| matches!(e, Effect::OpenLink { .. })));
        }
    }

    #[test]
    fn a_yielded_slot_stays_yielded_until_the_hold_passes() {
        let mut daemon = Daemon::new("gram");
        let now = clock();
        let local = Address::parse("02:00:00:00:00:01").unwrap();
        let peer = Address::parse("02:00:00:00:00:02").unwrap();
        daemon.set_local_address(Some(local));
        daemon.select_accessory(Some(addr(3)));
        daemon.note_bluez(true);
        daemon.peers_seated.push(peer);

        daemon.free_slot(peer, now);
        assert!(daemon.effects.drop_link);
        assert!(daemon.slot_hold_until.is_some());

        let generation = daemon.connection().generation();
        daemon.attach_available(generation, now.mono.saturating_add(1_000));
        assert!(!daemon.session.background_requested, "no auto-attach during the hold");

        daemon.attach_available(generation, now.mono.saturating_add(SLOT_HOLD_MS + 1));
        assert!(daemon.session.background_requested, "auto-attach resumes after the hold");
    }

    #[test]
    fn a_fresh_placement_opens_without_spending_the_wake_on_a_refusal() {
        use crate::fleet::members::Fleet;
        use crate::step::Effect;
        let mut daemon = Daemon::new("gram");
        let now = clock();
        let local = addr(1);
        let peer = addr(2);
        daemon.set_local_address(Some(local));
        daemon.restore_fleet(Fleet {
            id: FleetId::new([3; 16]),
            key: GroupKey::new([0x11; 32]),
            members: vec![
                Member { address: local, label: "lg-gram".into() },
                Member { address: peer, label: "omarchy-xps".into() },
            ],
        });
        daemon.select_accessory(Some(addr(3)));
        daemon.note_bluez(true);
        daemon.activity_preference.note_local([3; 16], addr(3), local, now.unix);
        // Public placement says a bud is out. The battery slots are still empty.
        daemon.accessory.note_placement(false, Some(true), Some(false), now.mono);
        assert!(daemon.accessory.left.is_unknown());
        let seen = daemon.step(crate::step::Input::Tick, now);
        assert!(seen.effects.iter().all(|effect| !matches!(effect, Effect::OpenLink { .. })));
        let settled = Clock::new(now.mono.0 + 2_000, now.unix.0 + 2_000);
        let opened = daemon.step(crate::step::Input::Tick, settled);
        assert!(
            opened.effects.iter().any(|effect| matches!(effect, Effect::OpenLink { .. })),
            "availability evidence opens, and the wake commits because the open proceeds"
        );
        assert!(daemon.session.background_requested);
    }

    #[test]
    fn the_preferred_connector_never_falls_back_to_the_other_pairs_member() {
        use crate::fleet::members::Fleet;
        let setup = |owner_local: Option<bool>| {
            let mut daemon = Daemon::new("gram");
            let now = clock();
            let local = addr(1);
            let peer = addr(2);
            daemon.set_local_address(Some(local));
            daemon.restore_fleet(Fleet {
                id: FleetId::new([3; 16]),
                key: GroupKey::new([0x11; 32]),
                members: vec![
                    Member { address: local, label: "lg-gram".into() },
                    Member { address: peer, label: "omarchy-xps".into() },
                ],
            });
            daemon.select_accessory(Some(addr(3)));
            daemon.note_bluez(true);
            daemon.note_pod_peers(vec![local, peer], now.unix);
            if let Some(local_owner) = owner_local {
                daemon.note_ownership(local_owner, now.mono);
                daemon.activity_preference.note_local(
                    [3; 16],
                    addr(3),
                    if local_owner { local } else { peer },
                    now.unix,
                );
            }
            daemon.note_socket(false, daemon.connection().generation(), now.mono);
            daemon
        };
        let now = clock();
        // The preferred peer keeps its preference even after the old grace window.
        let mut daemon = setup(Some(false));
        let generation = daemon.connection().generation();
        daemon.attach_available(generation, now.mono.saturating_add(1_000));
        assert!(!daemon.session.background_requested, "the non-owner does not race the reopen");
        daemon.attach_available(generation, now.mono.saturating_add(REOPEN_HOLD_MS + 1_000));
        assert!(!daemon.session.background_requested, "there is no timeout fallback");
        // The owner itself reconnects at once.
        let mut daemon = setup(Some(true));
        let generation = daemon.connection().generation();
        daemon.attach_available(generation, now.mono.saturating_add(1_000));
        assert!(daemon.session.background_requested, "the owner reconnects first");
        // With no activity record, an enrolled fleet waits for actual input.
        let mut daemon = setup(None);
        let generation = daemon.connection().generation();
        daemon.attach_available(generation, now.mono.saturating_add(1_000));
        assert!(!daemon.session.background_requested, "unknown preference must not cause a race");
    }

    #[test]
    fn failed_apple_reconnect_has_three_actual_background_attempts_and_then_stops() {
        use crate::step::Input;

        for timeout in [false, true] {
            let mut daemon = Daemon::new("xps");
            let now = clock();
            ready_link(&mut daemon, now);
            daemon.release_playback(Release::AppleReconnect, now);
            daemon.note_lid(true, now.mono);
            let generation = daemon.connection().generation();
            daemon.note_socket(false, generation, now.mono);
            daemon.fail_reconnect("link did not return", now);
            assert_eq!(daemon.connection.retry(), (1, 3));
            assert!(!daemon.connection.may_open(now.mono));

            let early = Clock::new(now.mono.0 + 1_000, now.unix.0 + 1_000);
            daemon.step(Input::Tick, early);
            assert!(!daemon.connection.may_open(early.mono), "no opening before the delay");

            let due = Clock::new(
                now.mono.0 + crate::pods::link::BACKGROUND_REOPEN_DELAY_MS + 100,
                now.unix.0 + crate::pods::link::BACKGROUND_REOPEN_DELAY_MS + 100,
            );
            let opened = daemon.step(Input::Tick, due);
            assert!(
                opened.effects.iter().any(|effect| matches!(effect, Effect::OpenLink { .. })),
                "the delayed operation opens on its own"
            );
            assert_eq!(daemon.connection.retry(), (1, 3));
            assert!(
                daemon.session.background_requested,
                "the retry opens the background connection"
            );
            let mut at = due;
            for attempt in 1..=3 {
                assert_eq!(daemon.connection.retry(), (attempt, 3));
                let generation = daemon.connection.generation();
                let failure = if timeout {
                    at = Clock::new(daemon.connection.deadline().unwrap().0, at.unix.0);
                    Input::Tick
                } else {
                    Input::LinkFailed { generation, busy: false }
                };
                daemon.step(failure, at);
                if attempt < 3 {
                    let deadline = daemon.connection.deadline().unwrap();
                    let early = Clock::new(deadline.0 - 1, at.unix.0);
                    assert!(!daemon
                        .step(Input::Tick, early)
                        .effects
                        .iter()
                        .any(|e| matches!(e, Effect::OpenLink { .. })));
                    at = Clock::new(deadline.0, at.unix.0);
                    let retry = daemon.step(Input::Tick, at);
                    assert_eq!(
                        retry
                            .effects
                            .iter()
                            .filter(|e| matches!(e, Effect::OpenLink { .. }))
                            .count(),
                        1
                    );
                }
            }
            assert_eq!(daemon.connection.stage(), Stage::Failed);
            assert!(!daemon.session.background_requested);
            let later = Clock::new(at.mono.0 + 60_000, at.unix.0 + 60_000);
            assert!(!daemon
                .step(Input::Tick, later)
                .effects
                .iter()
                .any(|e| matches!(e, Effect::OpenLink { .. })));
        }
    }

    #[test]
    fn background_reopen_obeys_session_admission_during_delay_and_busy_wait() {
        use crate::fleet::members::Fleet;
        use crate::step::Input;
        for during_retry in [false, true] {
            for stop in ["disconnect", "case_closed", "radio_off", "other_host"] {
                let mut daemon = Daemon::new("xps");
                let now = clock();
                let at = |ms| Clock::new(now.mono.0 + ms, now.unix.0 + ms);
                ready_link(&mut daemon, now);
                daemon.release_playback(Release::AppleReconnect, now);
                daemon.note_lid(true, now.mono);
                daemon.note_socket(false, daemon.connection.generation(), now.mono);
                daemon.fail_reconnect("link did not return", now);
                let generation = daemon.connection.generation();
                if during_retry {
                    let opened = daemon.step(Input::Tick, at(3_000));
                    assert!(opened.effects.iter().any(|e| matches!(e, Effect::OpenLink { .. })));
                    daemon.step(Input::LinkFailed { generation, busy: true }, at(3_100));
                    assert_eq!(daemon.connection.retry(), (1, 3));
                }
                let stopped_at = at(if during_retry { 3_200 } else { 1_000 });
                let input = match stop {
                    "disconnect" => Input::Command(Command::Disconnect),
                    "case_closed" => {
                        for right in [false, true] {
                            daemon.accessory.note_placement(
                                right,
                                Some(false),
                                Some(true),
                                stopped_at.mono,
                            );
                        }
                        Input::Lid { open: false }
                    }
                    "radio_off" => Input::AdapterPower(false),
                    _ => {
                        let local = addr(1);
                        let peer = addr(2);
                        daemon.set_local_address(Some(local));
                        daemon.restore_fleet(Fleet {
                            id: FleetId::new([3; 16]),
                            key: GroupKey::new([0x11; 32]),
                            members: vec![
                                Member { address: local, label: "xps".into() },
                                Member { address: peer, label: "gram".into() },
                            ],
                        });
                        daemon.activity_preference.note_local(
                            [3; 16],
                            addr(3),
                            peer,
                            stopped_at.unix,
                        );
                        Input::Tick
                    }
                };
                let stopped = daemon.step(input, stopped_at);
                let late = daemon.step(Input::LinkFailed { generation, busy: false }, at(10_000));
                let later = daemon.step(Input::Tick, at(60_000));
                for effects in [&stopped.effects, &late.effects, &later.effects] {
                    assert!(
                        !effects.iter().any(|e| matches!(e, Effect::OpenLink { .. })),
                        "{stop}, during_retry={during_retry}"
                    );
                }
                assert!(
                    !daemon.session.background_requested,
                    "{stop}, during_retry={during_retry}"
                );
            }
        }
    }

    #[test]
    fn release_pauses_the_player_and_a_reconnect_writes_the_request() {
        use crate::audio::play::PlayerSeen;
        use crate::fleet::members::{GroupKey, Member, Proposal};
        use crate::step::{Effect, Input};
        use crate::types::FleetId;

        let mut daemon = Daemon::new("gram");
        let now = clock();
        let local = Address::parse("02:00:00:00:00:01").unwrap();
        let peer = Address::parse("02:00:00:00:00:02").unwrap();
        daemon.set_local_address(Some(local));
        ready_link(&mut daemon, now);
        daemon.set_local_address(Some(local));
        daemon.audio.startup_players(&[PlayerSeen {
            service: "org.mpris.MediaPlayer2.spotify".into(),
            playing: true,
        }]);
        daemon.output.claim();
        let released = daemon.step(Input::Command(Command::HandoffRelease), now);
        assert_eq!(released.reply.as_deref(), Some("ok\n"));
        assert!(released.effects.iter().any(|effect| matches!(
            effect,
            Effect::PausePlayers(names) if names == &["org.mpris.MediaPlayer2.spotify".to_string()]
        )));
        assert!(!daemon.output.wants_output());
        let still = daemon.step(
            Input::Player { service: "org.mpris.MediaPlayer2.spotify".into(), playing: Some(true) },
            now,
        );
        assert!(still.effects.iter().all(|effect| !matches!(effect, Effect::OpenLink { .. })));
        assert!(!daemon.output.wants_output());

        daemon.note_bluez(true);
        let away = Address::parse("02:00:00:00:00:09").unwrap();
        daemon
            .membership
            .apply(Proposal {
                id: FleetId::new([7; 16]),
                key: GroupKey::new([9; 32]),
                members: vec![
                    Member { address: local, label: "gram".into() },
                    Member { address: peer, label: "xps".into() },
                    Member { address: away, label: "away".into() },
                ],
            })
            .unwrap();
        // The direct AirPods roster is current; the legacy announcement is stale.
        daemon.peers_seated.push(away);
        daemon.note_pod_peers(vec![local, peer], now.unix);
        let intent = daemon.routing().intent();
        let started = daemon.step(Input::Command(Command::ReconnectPeer), now);
        assert_eq!(started.reply.as_deref(), Some("accepted\n"));
        assert_eq!(daemon.routing().intent(), intent);
        assert!(started.effects.iter().all(|effect| !matches!(effect, Effect::Disconnect { .. })));
        assert_eq!(daemon.status_value(now)["native_handoff"]["phase"], "reconnecting");
        let waiting = daemon.step(Input::Tick, Clock::new(now.mono.0 + 2_999, now.unix.0));
        assert!(waiting.effects.iter().all(|effect| !matches!(effect, Effect::Disconnect { .. })));
        daemon.step(
            Input::Player {
                service: "org.mpris.MediaPlayer2.spotify".into(),
                playing: Some(false),
            },
            Clock::new(now.mono.0 + 3_000, now.unix.0),
        );
        let asked = daemon.step(Input::Tick, Clock::new(now.mono.0 + 3_000, now.unix.0));
        let request = crate::pods::setup::reconnect_request(peer, local).unwrap();
        let other = crate::pods::setup::reconnect_request(away, local).unwrap();
        assert!(asked.effects.iter().any(|effect| matches!(
            effect,
            Effect::WriteControl { bytes, .. } if bytes == &request
        )));
        assert!(asked.effects.iter().all(|effect| !matches!(
            effect,
            Effect::WriteControl { bytes, .. } if bytes == &other
        )));
        assert!(asked.effects.iter().all(|effect| !matches!(effect, Effect::Disconnect { .. })));
        let still = daemon.step(Input::Tick, Clock::new(now.mono.0 + 4_000, now.unix.0));
        assert!(still.effects.iter().all(|effect| !matches!(effect, Effect::Disconnect { .. })));
        daemon.note_pod_peers(vec![local], now.unix);
        let dropped = daemon.step(Input::Tick, Clock::new(now.mono.0 + 4_100, now.unix.0));
        assert!(dropped.effects.iter().any(|effect| matches!(
            effect,
            Effect::Disconnect { accessory } if *accessory == addr(3)
        )));
        let generation = daemon.connection().generation();
        daemon.step(
            Input::ControlSocket { up: false, generation },
            Clock::new(now.mono.0 + 4_200, now.unix.0),
        );
        let disconnected_at = Clock::new(now.mono.0 + 4_300, now.unix.0);
        let disconnected = daemon.step(Input::Bluez(false), disconnected_at);
        assert!(disconnected
            .effects
            .iter()
            .all(|effect| !matches!(effect, Effect::RemoveDevice { .. })));
        assert_eq!(daemon.status_value(disconnected_at)["bluetooth_paired"], true);
        assert_eq!(daemon.status_value(disconnected_at)["native_handoff"]["phase"], "reconnecting");
        // The pods sit free for the rejoin delay first (giving a nearby
        // Apple device its chance to connect), then this laptop rejoins.
        let soon = daemon.step(Input::Tick, Clock::new(now.mono.0 + 9_200, now.unix.0));
        assert!(soon.effects.iter().all(|effect| !matches!(effect, Effect::FollowLink)));
        let back = daemon.step(Input::Tick, Clock::new(now.mono.0 + 14_300, now.unix.0));
        assert!(back.effects.iter().any(|effect| matches!(effect, Effect::OpenLink { .. })));
        assert!(daemon.session.background_requested);
        assert_eq!(daemon.routing().intent(), crate::fleet::route::Intent::Release);

        // A reset while rejoining is a failed setup, not the intentional drop.
        let generation = daemon.connection.generation();
        let failed_at = Clock::new(now.mono.0 + 14_400, now.unix.0);
        daemon.step(Input::ControlSocket { up: false, generation }, failed_at);
        daemon.step(Input::LinkFailed { generation, busy: false }, failed_at);
        assert_eq!(daemon.connection.stage(), Stage::Retrying);
        let retried = daemon.step(Input::Tick, Clock::new(now.mono.0 + 15_400, now.unix.0));
        assert!(retried.effects.iter().any(|effect| matches!(effect, Effect::OpenLink { .. })));
        assert_eq!(daemon.connection.retry().0, 2);
        assert!(daemon.session.background_requested);
        assert_eq!(daemon.routing().intent(), crate::fleet::route::Intent::Release);

        // An attempt still in flight is not failed by the reconnect's
        // return wait, however long the link layer takes with it; when it
        // lands, the reconnect completes instead of recording a failure.
        daemon.step(Input::Tick, Clock::new(now.mono.0 + 30_000, now.unix.0));
        assert_eq!(daemon.connection.stage(), Stage::Connecting);
        let generation = daemon.connection.generation();
        let landed = Clock::new(now.mono.0 + 30_100, now.unix.0);
        daemon.step(Input::ControlSocket { up: true, generation }, landed);
        daemon.step(
            Input::Accessory { generation, packet: vec![0x01, 0x00, 0x04, 0x00, 0x01] },
            landed,
        );
        daemon.step(
            Input::Accessory { generation, packet: vec![0x04, 0x00, 0x04, 0x00, 0x2b, 0x00, 0x01] },
            landed,
        );
        let mut battery = vec![0x04, 0x00, 0x04, 0x00, 0x04, 0x00, 0x03];
        battery.extend_from_slice(&[0x04, 0x01, 80, 0x02, 0x01]);
        battery.extend_from_slice(&[0x02, 0x01, 60, 0x01, 0x01]);
        battery.extend_from_slice(&[0x08, 0x01, 40, 0x02, 0x01]);
        daemon.step(Input::Accessory { generation, packet: battery }, landed);
        daemon.step(
            Input::Accessory {
                generation,
                packet: vec![0x04, 0x00, 0x04, 0x00, 0x06, 0x00, 0x00, 0x02],
            },
            landed,
        );
        assert_eq!(daemon.connection.stage(), Stage::Ready);
    }

    #[test]
    fn releasing_a_claim_retires_setup_and_queued_resumes_for_every_release_path() {
        use crate::audio::play::{PlayerSeen, Profile};
        use crate::audio::route::{Card, Graph};
        use crate::step::{Effect, Input, TransportReply};
        let player = "org.mpris.MediaPlayer2.brave";
        for ending in ["handoff", "speakers", "preference", "disconnect", "apple"] {
            let mut daemon = Daemon::new("xps");
            let now = clock();
            ready_link(&mut daemon, now);
            daemon.note_bluez(true);
            daemon.audio.startup_players(&[PlayerSeen { service: player.into(), playing: true }]);
            daemon.set_profiles(vec![Profile {
                name: "a2dp-sink-aac".into(),
                description: "AAC".into(),
                priority: 10,
                available: true,
                sinks: 1,
                sources: 0,
            }]);
            daemon.activate_profile();
            daemon.claim_playback(now);
            let setup = daemon.step(
                Input::Audio(Graph {
                    default_sink: "speakers".into(),
                    sinks: vec![],
                    streams: vec![],
                    cards: vec![Card {
                        address: Some(addr(3)),
                        name: "card".into(),
                        active: Some("off".into()),
                        profiles: vec!["a2dp-sink-aac".into()],
                    }],
                }),
                now,
            );
            let audio = setup
                .effects
                .iter()
                .find_map(|e| match e {
                    Effect::ConnectTransport { id, .. } => Some(*id),
                    _ => None,
                })
                .unwrap();
            let codec = setup
                .effects
                .iter()
                .find_map(|e| match e {
                    Effect::SelectProfile { id, .. } => Some(*id),
                    _ => None,
                })
                .unwrap();
            daemon.queue_resume(vec![player.into()]);
            let ended = match ending {
                "speakers" => {
                    daemon.return_to_speakers(now);
                    daemon.step(Input::Tick, now)
                }
                "preference" => {
                    daemon.release_preference(now);
                    daemon.step(Input::Tick, now)
                }
                _ => daemon.step(
                    Input::Command(match ending {
                        "handoff" => Command::HandoffRelease,
                        "disconnect" => Command::Disconnect,
                        _ => Command::ReconnectPeer,
                    }),
                    now,
                ),
            };
            let should_pause = matches!(ending, "handoff" | "disconnect" | "apple");
            if ending == "apple" {
                assert_eq!(ended.reply.as_deref(), Some("accepted\n"));
            }
            assert_eq!(
                ended.effects.iter().any(|e| matches!(e, Effect::PausePlayers(_))),
                should_pause,
                "{ending}"
            );
            assert!(!daemon.output.wants_output(), "{ending}");
            let late_audio = daemon
                .step(Input::TransportReply { id: audio, reply: TransportReply::Connected }, now);
            let late_codec = daemon.step(Input::ProfileResult { id: codec, ok: false }, now);
            for effects in [&ended.effects, &late_audio.effects, &late_codec.effects] {
                assert!(
                    !effects.iter().any(|e| matches!(
                        e,
                        Effect::ResumePlayers(_)
                            | Effect::ConnectTransport { .. }
                            | Effect::ConnectHandsfree { .. }
                            | Effect::SelectProfile { .. }
                    )),
                    "{ending}"
                );
            }
            assert_ne!(
                daemon.status_value(now)["native_handoff"]["failure"],
                "profile was not selected",
                "{ending}"
            );
        }
    }

    #[test]
    fn taking_audio_back_keeps_the_player_paused_until_the_relinked_output_starts() {
        use crate::audio::play::PlayerSeen;
        use crate::audio::route::{Graph, LiveStream, Sink};
        use crate::pods::observe::Observation;
        use crate::step::{Effect, Input};
        let brave = "org.mpris.MediaPlayer2.brave";
        let resumed =
            |effects: &[Effect]| effects.iter().any(|e| matches!(e, Effect::ResumePlayers(_)));
        for ending in ["success", "closed_stream", "failed", "canceled", "timeout", "audio_lost"] {
            let mut daemon = Daemon::new("xps");
            let now = clock();
            let at = |ms| Clock::new(now.mono.0 + ms, now.unix.0 + ms);
            ready_link(&mut daemon, now);
            let mut graph = Graph {
                default_sink: "pods".into(),
                sinks: vec![
                    Sink {
                        name: "pods".into(),
                        address: Some(addr(3)),
                        profile: Some("a2dp-sink".into()),
                    },
                    Sink { name: "speakers".into(), address: None, profile: None },
                ],
                cards: vec![],
                streams: vec![LiveStream {
                    id: 1,
                    generation: 10,
                    sink: "pods".into(),
                    role: String::new(),
                    node: "Brave".into(),
                    mute: false,
                    volume: 1.0,
                    corked: false,
                }],
            };
            daemon.step(Input::Audio(graph.clone()), now);
            daemon.step(Input::Observation(Observation::Ownership(false)), now);
            daemon.step(
                Input::Players(vec![PlayerSeen { service: brave.into(), playing: true }]),
                now,
            );
            daemon.output.claim();
            // The grant routes directly now: the player keeps playing and
            // nothing is paused or restarted unless the transport stays
            // silent. That silence is what this test drives next: the
            // muted stream on the speakers past the confirmation deadline
            // starts the rebuild, and from there the relink owns the pause.
            let grant = daemon.step(Input::Observation(Observation::Ownership(true)), at(10));
            assert!(!grant
                .effects
                .iter()
                .any(|e| matches!(e, Effect::PausePlayers(_) | Effect::RestartOutput { .. })));
            assert!(!resumed(&grant.effects));
            graph.streams[0].mute = true;
            graph.streams[0].corked = true;
            graph.streams[0].sink = "speakers".into();
            let reset = daemon.step(Input::Audio(graph.clone()), at(2_600));
            assert!(reset.effects.iter().any(|e| matches!(e, Effect::RestartOutput { .. })));
            let id = recovery_id(&daemon).unwrap();
            let paused = daemon
                .step(Input::Player { service: brave.into(), playing: Some(false) }, at(2_620));
            assert!(!resumed(&paused.effects));
            // The direct route was given live audio and still stayed
            // silent, so the rebuild owns the output from here.
            daemon.step(Input::Transport { active: true }, at(2_630));
            daemon.step(Input::Transport { active: false }, at(2_640));
            let reset_done = daemon.step(Input::OutputRestarted { id, ok: true }, at(2_660));
            assert!(!resumed(&reset_done.effects), "profile completion is too early");
            // A pause notification caused by the reset also cannot release the hold.
            assert!(!resumed(
                &daemon
                    .step(Input::Player { service: brave.into(), playing: Some(false) }, at(2_670))
                    .effects
            ));
            if ending == "closed_stream" {
                graph.streams.clear();
                daemon.step(Input::Audio(graph.clone()), at(2_680));
            } else {
                graph.streams[0].sink = "pods".into();
                let linked = daemon.step(Input::Audio(graph.clone()), at(2_680));
                assert!(!resumed(&linked.effects));
                graph.streams[0].mute = false;
                // The relink entered Settling on the linked snapshot and
                // this un-muted one lands more than 250 ms later, so the
                // router starts the output here.
                let start = daemon.step(Input::Audio(graph.clone()), at(3_700));
                assert!(!resumed(&start.effects));
                daemon.step(Input::Transport { active: true }, at(3_960));

                assert!(start.effects.iter().any(
                    |e| matches!(e, Effect::StartOutput { id: started, .. } if *started == id)
                ));
                assert!(!resumed(&start.effects), "sending Start is not its completion");
                assert!(!resumed(
                    &daemon.step(Input::OutputStarted { id: id + 1, ok: true }, at(3_960)).effects
                ));
                let before = match ending {
                    "canceled" => Some(Input::Command(Command::HandoffRelease)),
                    "audio_lost" => Some(Input::AudioLost),
                    "timeout" => Some(Input::Tick),
                    _ => None,
                };
                // The timeout ending drives past the relink deadline,
                // which runs from the rebuild request (2_600) for 5 s.
                let completed_at = if before.is_some() { 7_800 } else { 4_000 };
                if let Some(input) = before {
                    let stopped = daemon.step(input, at(7_700));
                    assert!(!resumed(&stopped.effects));
                }
                let finished = daemon
                    .step(Input::OutputStarted { id, ok: ending != "failed" }, at(completed_at));
                let success = matches!(ending, "success" | "closed_stream");
                assert_eq!(resumed(&finished.effects), success, "ending={ending}");
                if !success {
                    assert!(daemon.session.resume_players.is_empty());
                    assert!(daemon.session.resumed_players.is_empty());
                    assert_eq!(daemon.session.rebuild_pause_until, None);
                    assert_eq!(daemon.session.output_recovery.task.state(), TaskState::Idle);
                }
                assert!(!resumed(
                    &daemon
                        .step(Input::OutputStarted { id, ok: true }, at(completed_at + 10))
                        .effects
                ));
                if success {
                    daemon.step(
                        Input::Player { service: brave.into(), playing: Some(true) },
                        at(completed_at + 20),
                    );
                    assert_eq!(
                        recovery_id(&daemon),
                        Some(id),
                        "resume must not start another recovery"
                    );
                    assert!(daemon.session.resumed_players.is_empty());
                }
            }
        }
    }
    #[test]
    fn a_player_that_pauses_itself_as_the_output_is_rebuilt_is_played_again() {
        use crate::audio::play::PlayerSeen;
        use crate::step::{Effect, Input};
        let brave = "org.mpris.MediaPlayer2.brave";
        let resumed = |effects: &[Effect]| {
            effects.iter().any(|e| matches!(e, Effect::ResumePlayers(names) if names == &[brave]))
        };
        // Within the window after a rebuild, outside it, and after the
        // daemon itself gave the seat away.
        for (after_ms, step_back, expect) in
            [(100, false, true), (600, false, false), (100, true, false)]
        {
            let mut daemon = Daemon::new("xps");
            let now = clock();
            ready_link(&mut daemon, now);
            daemon.audio.startup_players(&[PlayerSeen { service: brave.into(), playing: true }]);
            daemon.output.claim();
            daemon.step(Input::OutputRestarted { id: 0, ok: true }, now);
            daemon.session.rebuild_pause_until = Some(now.mono.saturating_add(500));
            if step_back {
                daemon.step_back(now);
            }
            let later = Clock::new(now.mono.0 + after_ms, now.unix.0 + after_ms);
            let paused =
                daemon.step(Input::Player { service: brave.into(), playing: Some(false) }, later);
            assert_eq!(
                resumed(&paused.effects),
                expect,
                "after {after_ms} ms, step_back={step_back}"
            );
            if expect {
                // Our own Play continues the claim; it does not start another.
                let serial = recovery_id(&daemon);
                let resume_output = daemon.session.resume_output_on_grant;
                daemon.step(Input::Player { service: brave.into(), playing: Some(true) }, later);
                assert_eq!(recovery_id(&daemon), serial);
                assert_eq!(daemon.session.resume_output_on_grant, resume_output);
                assert!(daemon.session.resumed_players.is_empty());
            }
        }
    }

    #[test]
    fn output_verification_retries_only_missing_playback_within_the_live_claim() {
        use crate::audio::route::{Graph, LiveStream, Sink};
        use crate::pods::observe::Observation;
        use crate::step::{Effect, Input};
        for outcome in [
            "silent",
            "streaming",
            "transient",
            "paused",
            "released",
            "server_lost",
            "reset_failed",
            "start_failed",
            "no_parking",
        ] {
            let streamed = outcome == "streaming";
            let mut daemon = Daemon::new("gram");
            let now = clock();
            let at = |ms: u64| Clock::new(now.mono.0 + ms, now.unix.0 + ms);
            daemon.set_local_address(Some(addr(2)));
            ready_link(&mut daemon, now);
            daemon.step(Input::Players(Vec::new()), now);
            let playing = Graph {
                default_sink: "pods".into(),
                sinks: vec![
                    Sink {
                        name: "pods".into(),
                        address: Some(addr(3)),
                        profile: Some("a2dp-sink".into()),
                    },
                    Sink { name: "speakers".into(), address: None, profile: None },
                ],
                cards: Vec::new(),
                streams: vec![LiveStream {
                    id: 1,
                    generation: 10,
                    sink: "pods".into(),
                    role: String::new(),
                    node: "Brave".into(),
                    mute: false,
                    volume: 1.0,
                    corked: false,
                }],
            };
            let mut parked = playing.clone();
            parked.streams[0].mute = true;
            parked.streams[0].sink = "speakers".into();
            daemon.step(Input::Audio(playing.clone()), now);
            daemon.step(Input::Observation(Observation::Ownership(false)), now);
            daemon.output.claim();
            daemon.audio.set_local_playing_for_test(true);
            let mut serial = 0;
            let mut rebuilds = 0;
            for round in 0..4u64 {
                let base = round * 4_000;
                if round == 2 && outcome == "paused" {
                    daemon.audio.set_local_playing_for_test(true);
                }
                if round == 0 {
                    daemon.step(Input::Observation(Observation::Ownership(true)), at(base));
                    // The direct watch comes first; drive it past its
                    // deadline so the retry launches the rebuild below.
                    daemon.audio.set_local_playing_for_test(true);
                    daemon.step(Input::Tick, at(2_600));
                    daemon.step(Input::Tick, at(2_700));
                } else {
                    daemon.step(Input::Tick, at(base));
                }
                if recovery_id(&daemon).is_none_or(|id| id == serial) {
                    continue;
                }
                serial = recovery_id(&daemon).unwrap();
                rebuilds += 1;
                let reset = daemon.step(Input::Audio(parked.clone()), at(base + 10));
                assert!(reset.effects.iter().any(|e| matches!(e, Effect::RestartOutput { .. })));
                daemon.step(
                    Input::OutputRestarted { id: serial, ok: outcome != "reset_failed" },
                    at(base + 20),
                );
                if outcome == "reset_failed" {
                    assert_eq!(daemon.session.output_recovery.task.state(), TaskState::Idle);
                    continue;
                }
                daemon.step(Input::Audio(playing.clone()), at(base + 30));
                daemon.step(Input::Transport { active: streamed }, at(base + 40));
                daemon.step(Input::Outputs(vec![output(0)]), at(base + 40));
                let early = daemon.step(Input::Tick, at(base + 280));
                assert!(
                    !early.effects.iter().any(|e| matches!(e, Effect::StartOutput { .. })),
                    "the relink must settle for 250 ms before Start"
                );
                let mut starts = daemon.step(Input::Tick, at(base + 300)).effects;
                starts.extend(daemon.step(Input::Tick, at(base + 600)).effects);
                assert_eq!(starts.iter().filter(|e| matches!(e, Effect::StartOutput { .. })).count(), 1,
                "Start is issued once after settling, regardless of which input advances routing");
                // Transport activity before Start must not retire router work.
                assert!(matches!(
                    daemon.session.output_recovery.task.state(),
                    TaskState::Running { .. }
                ));
                daemon.step(
                    Input::OutputStarted { id: serial, ok: outcome != "start_failed" },
                    at(base + 610),
                );
                if outcome == "start_failed" {
                    assert_eq!(daemon.session.output_recovery.task.state(), TaskState::Idle);
                    continue;
                }
                assert!(matches!(
                    daemon.session.output_recovery.task.state(),
                    TaskState::Confirming { .. }
                ));
                if outcome == "transient" {
                    daemon.step(Input::Transport { active: true }, at(base + 700));
                    daemon.step(Input::Transport { active: false }, at(base + 900));
                }
                if outcome == "silent" {
                    // Silence must still be silence at the check: the
                    // transport report from the relink does not count.
                    daemon.step(Input::Transport { active: false }, at(base + 950));
                }
                // Before the check is due nothing happens.
                daemon.step(Input::Tick, at(base + 1_000));
                assert_eq!(recovery_id(&daemon), Some(serial));
                if round == 0 {
                    match outcome {
                        "paused" => daemon.audio.set_local_playing_for_test(false),
                        "released" | "server_lost" => {
                            let input = if outcome == "released" {
                                Input::Command(Command::HandoffRelease)
                            } else {
                                Input::AudioLost
                            };
                            daemon.step(input, at(1_100));
                            daemon.step(Input::OutputStarted { id: serial, ok: true }, at(1_200));
                            assert_eq!(
                                daemon.session.output_recovery.task.state(),
                                TaskState::Idle
                            );
                        }
                        "no_parking" => {
                            let mut unavailable = playing.clone();
                            unavailable.sinks.retain(|sink| sink.address.is_some());
                            daemon.step(Input::Audio(unavailable), at(3_000));
                            for ms in [3_110, 3_200, 3_300, 3_400] {
                                let waiting = daemon.step(Input::Tick, at(ms));
                                assert!(!daemon.output.recovering());
                                assert!(!waiting
                                    .effects
                                    .iter()
                                    .any(|e| matches!(e, Effect::RestartOutput { .. })));
                            }
                            daemon.step(Input::Audio(playing.clone()), at(3_500));
                        }
                        _ => {}
                    }
                }
                // Drive past this round's confirmation deadline so the
                // next round can launch its rebuild.
                daemon.step(Input::Tick, at(base + 3_200));
            }
            // The grant's direct watch is not a rebuild; rebuilds follow it,
            // at most two more when audio never flows.
            assert_eq!(
                rebuilds,
                if matches!(outcome, "silent" | "no_parking" | "transient") { 2 } else { 1 },
                "{outcome}"
            );
        }
    }
    #[test]
    fn a_grant_after_losing_the_pods_relinks_unless_audio_already_streams() {
        use crate::audio::route::{Graph, LiveStream, Sink};
        use crate::pods::observe::Observation;
        use crate::step::{Effect, Input};
        for (state, flagged) in [(2, false), (0, false), (2, true), (0, true)] {
            let mut daemon = Daemon::new("gram");
            let now = clock();
            daemon.set_local_address(Some(addr(2)));
            ready_link(&mut daemon, now);
            daemon.step(Input::Players(Vec::new()), now);
            let graph = Graph {
                default_sink: "pods".into(),
                sinks: vec![
                    Sink {
                        name: "pods".into(),
                        address: Some(addr(3)),
                        profile: Some("a2dp-sink".into()),
                    },
                    Sink { name: "speakers".into(), address: None, profile: None },
                ],
                cards: Vec::new(),
                streams: vec![LiveStream {
                    id: 1,
                    generation: 10,
                    sink: "pods".into(),
                    role: String::new(),
                    node: "Brave".into(),
                    mute: false,
                    volume: 1.0,
                    corked: false,
                }],
            };
            daemon.step(Input::Audio(graph.clone()), now);
            // The other laptop took the pods, which ended this one's A2DP stream.
            daemon.step(Input::Observation(Observation::Ownership(false)), now);
            if state == 0 {
                daemon.step(Input::Transport { active: true }, now);
            }
            // With or without the older grant flag, a running sink is kept.
            daemon.output.claim();
            daemon.session.resume_output_on_grant = flagged;
            let grant = daemon.step(Input::Observation(Observation::Ownership(true)), now);
            // Recovery parks the muted stream on the speakers before restarting.
            let mut parked = graph.clone();
            parked.streams[0].mute = true;
            parked.streams[0].sink = "speakers".into();
            let settled = daemon.step(Input::Audio(parked), now);
            let restarted = [&grant.effects, &settled.effects].iter().any(|effects| {
                effects.iter().any(|effect| matches!(effect, Effect::RestartOutput { .. }))
            });
            assert!(!restarted, "sink state {state} flagged {flagged}");
            assert!(!daemon.session.rearm_after_loss);
            assert!(!daemon.session.resume_output_on_grant);
        }
    }

    #[test]
    fn a_grant_routes_directly_and_rebuilds_only_if_playback_stays_silent() {
        use crate::audio::route::{Graph, LiveStream, Sink};
        use crate::pods::observe::Observation;
        use crate::step::{Effect, Input};
        for silent in [false, true] {
            let mut daemon = Daemon::new("gram");
            let now = clock();
            let at = |ms: u64| Clock::new(now.mono.0 + ms, now.unix.0 + ms);
            daemon.set_local_address(Some(addr(2)));
            ready_link(&mut daemon, now);
            daemon.step(Input::Players(Vec::new()), now);
            let playing = Graph {
                default_sink: "pods".into(),
                sinks: vec![
                    Sink {
                        name: "pods".into(),
                        address: Some(addr(3)),
                        profile: Some("a2dp-sink".into()),
                    },
                    Sink { name: "speakers".into(), address: None, profile: None },
                ],
                cards: Vec::new(),
                streams: vec![LiveStream {
                    id: 1,
                    generation: 10,
                    sink: "pods".into(),
                    role: String::new(),
                    node: "Brave".into(),
                    mute: false,
                    volume: 1.0,
                    corked: false,
                }],
            };
            let mut parked = playing.clone();
            parked.streams[0].mute = true;
            parked.streams[0].sink = "speakers".into();
            daemon.step(Input::Audio(playing.clone()), now);
            daemon.step(Input::Observation(Observation::Ownership(false)), now);
            daemon.output.claim();
            daemon.audio.set_local_playing_for_test(true);
            // The grant routes directly: no sink restart, no player pause,
            // only a verification watch on the transport.
            let grant = daemon.step(Input::Observation(Observation::Ownership(true)), at(0));
            assert!(
                !grant
                    .effects
                    .iter()
                    .any(|e| matches!(e, Effect::RestartOutput { .. } | Effect::PausePlayers(_))),
                "a grant must not restart the sink or pause players first (silent={silent})"
            );
            assert!(matches!(
                daemon.session.output_recovery.task.state(),
                TaskState::Confirming { .. }
            ));
            assert!(!daemon.session.rearm_after_loss);
            if silent {
                // The failed node the restart protected against shows up as
                // silence: past the confirmation deadline the rebuild begins.
                let parked_step = daemon.step(Input::Audio(parked.clone()), at(2_600));
                let tick = daemon.step(Input::Tick, at(2_700));
                assert!(
                    [&parked_step.effects, &tick.effects].iter().any(|effects| effects
                        .iter()
                        .any(|e| matches!(e, Effect::RestartOutput { .. }))),
                    "a silent transport must fall back to the rebuild"
                );
            } else {
                // Routing directly still starts the output node: a sink
                // suspended by the previous owner's step-back will not
                // acquire its transport just because a stream landed on it.
                let started = daemon.step(Input::Audio(playing.clone()), at(300));
                assert!(
                    started.effects.iter().any(|e| matches!(e, Effect::StartOutput { .. })),
                    "a direct route must start the output, without restarting it"
                );
                assert!(!started
                    .effects
                    .iter()
                    .any(|e| matches!(e, Effect::RestartOutput { .. } | Effect::PausePlayers(_))));
                daemon.step(Input::Transport { active: true }, at(500));
                daemon.step(Input::Outputs(vec![output(0)]), at(500));
                daemon.step(Input::Tick, at(2_600));
                assert!(matches!(
                    daemon.session.output_recovery.task.state(),
                    TaskState::Succeeded { .. }
                ));
            }
        }
    }
    #[test]
    fn a_failed_output_is_rebuilt_on_the_grant_without_waiting_for_verification() {
        use crate::pods::observe::Observation;
        use crate::step::{Effect, Input};
        // Both captured takeovers: node error, then suspension while Gram owns
        // playback. Also check a healthy/new node retains the direct path.
        for prior in ["failed", "replaced", "running", "suspended"] {
            let (mut daemon, playing, now) = takeover_fixture();
            let at = |ms| Clock::new(now.mono.0 + ms, now.unix.0 + ms);
            if prior != "suspended" {
                daemon.step(Input::Outputs(vec![output(-3)]), at(10));
            }
            let mut current = output(if prior == "running" { 0 } else { 2 });
            if prior == "replaced" {
                current.index += 1;
            }
            daemon.step(Input::Outputs(vec![current]), at(20));
            daemon.output.claim();
            daemon.audio.update_player("org.mpris.MediaPlayer2.brave", Some(true));
            let grant = daemon.step(Input::Observation(Observation::Ownership(true)), at(30));
            assert_eq!(
                grant.effects.iter().any(|e| matches!(e, Effect::PausePlayers(_))),
                prior == "failed",
                "{prior}"
            );
            if prior == "failed" {
                let id = recovery_id(&daemon).unwrap();
                assert!(matches!(
                    daemon.session.output_recovery.task.state(),
                    TaskState::Running { .. }
                ));
                let mut parked = playing;
                parked.streams[0].mute = true;
                parked.streams[0].sink = "speakers".into();
                let next = daemon.step(Input::Audio(parked), at(40));
                assert!(next
                    .effects
                    .iter()
                    .any(|e| matches!(e, Effect::RestartOutput { id: got, .. } if *got == id)));
                assert!(next.effects.iter().all(|e| !matches!(e, Effect::StartOutput { .. })));
            } else {
                assert!(matches!(
                    daemon.session.output_recovery.task.state(),
                    TaskState::Confirming { .. }
                ));
            }
        }
    }

    fn takeover_fixture() -> (Daemon, crate::audio::route::Graph, Clock) {
        use crate::audio::route::{Graph, LiveStream, Sink};
        use crate::pods::observe::Observation;
        use crate::step::Input;
        let now = clock();
        let mut daemon = Daemon::new("xps");
        ready_link(&mut daemon, now);
        let graph = Graph {
            default_sink: "pods".into(),
            sinks: vec![
                Sink {
                    name: "pods".into(),
                    address: Some(addr(3)),
                    profile: Some("a2dp-sink".into()),
                },
                Sink { name: "speakers".into(), address: None, profile: None },
            ],
            cards: vec![],
            streams: vec![LiveStream {
                id: 1,
                generation: 10,
                sink: "pods".into(),
                role: String::new(),
                node: "Brave".into(),
                mute: false,
                volume: 1.0,
                corked: false,
            }],
        };
        daemon.step(Input::Audio(graph.clone()), now);
        daemon.step(Input::Observation(Observation::Ownership(false)), now);
        (daemon, graph, now)
    }

    fn streaming_fixture() -> (Daemon, crate::audio::route::Graph, Clock) {
        use crate::pods::observe::Observation;
        use crate::step::Input;
        let (mut daemon, graph, now) = takeover_fixture();
        let at = |ms| Clock::new(now.mono.0 + ms, now.unix.0 + ms);
        daemon.step(Input::Bonded { address: addr(3), saved: true }, now);
        daemon.output.claim();
        daemon.audio.update_player("org.mpris.MediaPlayer2.brave", Some(true));
        daemon.step(Input::Outputs(vec![output(0)]), now);
        daemon.step(Input::Observation(Observation::Ownership(true)), now);
        let id = recovery_id(&daemon).unwrap();
        daemon.step(Input::Tick, at(300));
        daemon.step(Input::OutputStarted { id, ok: true }, at(400));
        daemon.step(Input::Transport { active: true }, at(500));
        daemon.step(Input::Tick, at(2_600));
        assert!(matches!(daemon.session.output_recovery.task.state(), TaskState::Succeeded { .. }));
        (daemon, graph, at(2_600))
    }

    #[test]
    fn watchdog_and_manual_repairs_share_takeover_cancellation_and_ids() {
        use crate::pods::observe::Observation;
        use crate::step::{Effect, Input};
        for trigger in ["watchdog", "manual"] {
            for end in ["takeover", "release", "server_lost", "suspend", "success"] {
                let (mut daemon, playing, now) = streaming_fixture();
                let at = |ms| Clock::new(now.mono.0 + ms, now.unix.0 + ms);
                daemon.step(Input::Outputs(vec![output(-3)]), at(10));
                let start = if trigger == "manual" {
                    let result = daemon.step(Input::Command(Command::AudioReset), at(3_010));
                    assert!(result.reply.as_ref().unwrap().starts_with("accepted:"));
                    result
                } else {
                    daemon.step(Input::Tick, at(3_010))
                };
                assert!(start.effects.iter().any(|e| matches!(e, Effect::PausePlayers(_))));
                let old = recovery_id(&daemon).unwrap();
                let mut parked = playing.clone();
                parked.streams[0].mute = true;
                parked.streams[0].sink = "speakers".into();
                let restart = daemon.step(Input::Audio(parked.clone()), at(3_020));
                assert!(restart
                    .effects
                    .iter()
                    .any(|e| matches!(e, Effect::RestartOutput { id, .. } if *id == old)));
                let duplicate = daemon.step(Input::Command(Command::AudioReset), at(3_030));
                assert!(duplicate.reply.unwrap().starts_with("error:"));
                assert_eq!(daemon.session.output_recovery.resets, 1);
                assert_eq!(recovery_id(&daemon), Some(old));
                assert!(!duplicate
                    .effects
                    .iter()
                    .any(|e| matches!(e, Effect::RestartOutput { .. })));

                match end {
                    "success" => {
                        daemon.step(Input::OutputRestarted { id: old, ok: true }, at(3_040));
                        daemon.step(Input::Audio(playing), at(3_050));
                        daemon.step(Input::Outputs(vec![output(0)]), at(3_060));
                        let mut starts = daemon.step(Input::Tick, at(3_310)).effects;
                        starts.extend(daemon.step(Input::Tick, at(3_610)).effects);
                        assert_eq!(
                            starts
                                .iter()
                                .filter(|e| matches!(e,
                            Effect::StartOutput { id, .. } if *id == old))
                                .count(),
                            1
                        );
                        let started =
                            daemon.step(Input::OutputStarted { id: old, ok: true }, at(3_620));
                        assert!(started
                            .effects
                            .iter()
                            .any(|e| matches!(e, Effect::ResumePlayers(_))));
                        assert_eq!(daemon.output_health_status()["last_reset_ok"], true);
                        daemon.step(Input::Tick, at(6_120));
                        assert!(matches!(
                            daemon.session.output_recovery.task.state(),
                            TaskState::Succeeded { .. }
                        ));
                        continue;
                    }
                    "takeover" => {
                        daemon.step(Input::Observation(Observation::Ownership(false)), at(3_040));
                        daemon.step(Input::Tick, at(3_400));
                        daemon.claim_playback(at(3_410));
                        daemon.audio.update_player("org.mpris.MediaPlayer2.brave", Some(true));
                        daemon.step(Input::Transport { active: false }, at(3_420));
                        daemon.step(Input::Observation(Observation::Ownership(true)), at(3_430));
                        daemon.step(Input::Audio(parked), at(3_440));
                        let new = recovery_id(&daemon).unwrap();
                        assert_ne!(new, old, "a new claim cannot reuse the retired repair's ID");
                    }
                    "release" => {
                        daemon.step(Input::Command(Command::HandoffRelease), at(3_040));
                    }
                    "server_lost" => {
                        daemon.step(Input::AudioLost, at(3_040));
                    }
                    _ => {
                        daemon.step(Input::Suspend, at(3_040));
                    }
                }
                let state = daemon.session.output_recovery.task.state();
                let stats = daemon.output_health_status();
                // The old reset is still outstanding when the new claim starts.
                // Neither success nor failure from it may advance the new work.
                for input in [
                    Input::OutputRestarted { id: old, ok: true },
                    Input::OutputStarted { id: old, ok: true },
                    Input::OutputRestarted { id: old, ok: false },
                ] {
                    let stale = daemon.step(input, at(3_450));
                    assert_eq!(
                        daemon.session.output_recovery.task.state(),
                        state,
                        "{trigger}/{end}"
                    );
                    assert_eq!(daemon.output_health_status(), stats);
                    assert!(!stale.effects.iter().any(|e| matches!(
                        e,
                        Effect::ResumePlayers(_)
                            | Effect::StartOutput { .. }
                            | Effect::RestartOutput { .. }
                    )));
                }
                assert_eq!(daemon.config.accessory, Some(addr(3)));
                assert_eq!(daemon.status_value(at(3_450))["bluetooth_bonded"], true);
                assert!(!start.effects.iter().chain(restart.effects.iter()).any(|e| matches!(
                    e,
                    Effect::RemoveDevice { .. }
                        | Effect::Disconnect { .. }
                        | Effect::WriteSecret { .. }
                )));
            }
        }
    }

    #[test]
    fn watchdog_requires_local_playback_and_repair_admission() {
        use crate::pods::observe::Observation;
        use crate::step::{Effect, Input};
        for condition in ["healthy", "paused", "peer", "sleep", "no_parking"] {
            let (mut daemon, mut graph, now) = streaming_fixture();
            let at = |ms| Clock::new(now.mono.0 + ms, now.unix.0 + ms);
            daemon.step(
                Input::Outputs(vec![output(if condition == "healthy" { 0 } else { 2 })]),
                now,
            );
            match condition {
                "paused" => {
                    daemon.audio.update_player("org.mpris.MediaPlayer2.brave", Some(false));
                }
                "peer" => {
                    daemon.step(Input::Observation(Observation::Ownership(false)), now);
                }
                "sleep" => {
                    daemon.step(Input::Suspend, now);
                }
                "no_parking" => {
                    graph.sinks.retain(|sink| sink.address.is_some());
                    daemon.step(Input::Audio(graph.clone()), now);
                }
                _ => {}
            }
            for ms in [3_100, 3_200, 3_300] {
                let result = daemon.step(Input::Tick, at(ms));
                assert!(
                    !result.effects.iter().any(|e| matches!(e, Effect::RestartOutput { .. })),
                    "{condition}"
                );
                assert_eq!(daemon.session.output_recovery.resets, 0, "{condition}");
            }
            if condition == "no_parking" {
                // Refused admission consumes neither an attempt nor cooldown.
                graph.sinks.push(crate::audio::route::Sink {
                    name: "speakers".into(),
                    address: None,
                    profile: None,
                });
                daemon.step(Input::Audio(graph), at(3_400));
                assert_eq!(daemon.session.output_recovery.resets, 1);
                assert!(daemon.output.recovering());
            } else if condition == "healthy" {
                let manual = daemon.step(Input::Command(Command::AudioReset), at(3_400));
                assert!(manual.reply.unwrap().starts_with("accepted:"));
                assert_eq!(daemon.session.output_recovery.resets, 1);
                assert!(daemon.output.recovering());
            }
        }
    }

    #[test]
    fn an_active_transport_on_a_suspended_output_has_one_recovery_owner() {
        use crate::pods::observe::Observation;
        use crate::step::{Effect, Input};
        let (mut daemon, playing, now) = takeover_fixture();
        let at = |ms| Clock::new(now.mono.0 + ms, now.unix.0 + ms);
        daemon.step(Input::Outputs(vec![output(2)]), now);
        daemon.output.claim();
        daemon.audio.update_player("org.mpris.MediaPlayer2.brave", Some(true));
        daemon.step(Input::Observation(Observation::Ownership(true)), now);
        let direct = recovery_id(&daemon).unwrap();
        daemon.step(Input::Tick, at(300));
        daemon.step(Input::OutputStarted { id: direct, ok: true }, at(400));
        daemon.step(Input::Transport { active: true }, at(800));
        assert_eq!(daemon.lifecycle.progress().state, crate::pods::lifecycle::State::Routing);
        assert_eq!(daemon.lifecycle.progress().waiting_for, "output_running");
        let mut effects = daemon.step(Input::Tick, at(2_499)).effects;
        assert_eq!(recovery_id(&daemon), Some(direct));
        effects.extend(daemon.step(Input::Tick, at(2_500)).effects);
        let rebuild = recovery_id(&daemon).unwrap();
        assert_ne!(rebuild, direct, "transport activity alone cannot satisfy verification");
        assert!(effects.iter().any(|e| matches!(e, Effect::PausePlayers(_))));
        effects.extend(daemon.step(Input::Tick, at(3_300)).effects);
        assert!(
            effects.iter().all(|e| !matches!(e, Effect::RestartOutput { .. })),
            "the watchdog cannot launch a competing reset"
        );
        assert_eq!(daemon.session.output_recovery.resets, 1);
        assert_eq!(recovery_id(&daemon), Some(rebuild));

        let mut parked = playing.clone();
        parked.streams[0].mute = true;
        parked.streams[0].sink = "speakers".into();
        let reset = daemon.step(Input::Audio(parked), at(3_310));
        assert!(reset
            .effects
            .iter()
            .any(|e| matches!(e, Effect::RestartOutput { id, .. } if *id == rebuild)));
        daemon.step(Input::OutputRestarted { id: rebuild, ok: true }, at(3_320));
        daemon.step(Input::Audio(playing), at(3_330));
        let mut current = output(0);
        current.index += 1;
        daemon.step(Input::Outputs(vec![current]), at(3_340));
        daemon.step(Input::Tick, at(3_600));
        daemon.step(Input::Tick, at(3_900));
        daemon.step(Input::OutputStarted { id: rebuild, ok: true }, at(3_910));
        assert_eq!(daemon.lifecycle.progress().state, crate::pods::lifecycle::State::Streaming);
        let verified = daemon.step(Input::Tick, at(6_500));
        assert!(matches!(daemon.session.output_recovery.task.state(), TaskState::Succeeded { .. }));
        assert!(verified.effects.iter().all(|e| !matches!(e, Effect::RestartOutput { .. })));
    }

    #[test]
    fn a_new_audio_output_is_routed_without_restarting_it_for_an_old_ownership_loss() {
        use crate::audio::route::{Graph, LiveStream, Sink};
        use crate::pods::observe::Observation;
        use crate::step::{Effect, Input};

        for old_output in [true, false] {
            let mut daemon = Daemon::new("xps");
            let now = clock();
            ready_link(&mut daemon, now);
            daemon.step(Input::Players(Vec::new()), now);
            let pods = Sink {
                name: "pods".into(),
                address: Some(addr(3)),
                profile: Some("a2dp-sink".into()),
            };
            let mut graph = Graph {
                default_sink: "speakers".into(),
                sinks: vec![Sink { name: "speakers".into(), address: None, profile: None }],
                cards: vec![],
                streams: vec![LiveStream {
                    id: 1,
                    generation: 10,
                    sink: "speakers".into(),
                    role: String::new(),
                    node: "Brave".into(),
                    mute: false,
                    volume: 1.0,
                    corked: false,
                }],
            };
            if old_output {
                graph.sinks.push(pods.clone());
            }
            daemon.step(Input::Audio(graph.clone()), now);
            daemon.step(Input::Observation(Observation::Ownership(false)), now);
            graph.sinks.retain(|s| s.address.is_none());
            daemon.step(Input::Audio(graph.clone()), now);
            daemon.step(
                Input::Player {
                    service: "org.mpris.MediaPlayer2.brave".into(),
                    playing: Some(true),
                },
                now,
            );
            // Opening reports may name the old owner before granting this play.
            daemon.step(Input::Observation(Observation::Ownership(false)), now);
            daemon.step(Input::Observation(Observation::Ownership(true)), now);
            graph.streams[0].mute = true;
            daemon.step(Input::Audio(graph.clone()), now);
            graph.sinks.push(pods);
            let arrived = daemon.step(Input::Audio(graph.clone()), now);
            assert!(
                !arrived.effects.iter().any(|e| matches!(e, Effect::RestartOutput { .. })),
                "old_output={old_output}: {:?}",
                arrived.effects
            );
            assert_eq!(recovery_id(&daemon), None);
            assert!(
                arrived.effects.iter().any(|e| matches!(e,
                    Effect::SetDefaultSink { name } if name == "pods"
                )),
                "route as soon as the sink arrives: {:?}",
                arrived.effects
            );
            graph.default_sink = "pods".into();
            let selected = daemon.step(Input::Audio(graph), now);
            assert!(
                selected.effects.iter().any(|e| matches!(e,
                    Effect::MoveStreams(moved) if moved.sink == "pods"
                )),
                "move immediately after selection: {:?}",
                selected.effects
            );
        }
    }
    #[test]
    fn recovery_defers_an_unmute_queued_before_the_ownership_grant() {
        use crate::audio::route::{Graph, LiveStream, Sink};
        use crate::step::{Effect, Input};
        let mut daemon = Daemon::new("xps");
        let now = clock();
        daemon.set_local_address(Some(addr(2)));
        ready_link(&mut daemon, now);
        daemon.output.claim();
        daemon.output.on_graph(
            Some(addr(3)),
            &Graph {
                default_sink: "speakers".into(),
                sinks: vec![],
                streams: vec![],
                cards: vec![],
            },
            true,
            now.mono,
        );
        daemon.output.hold_streams(&[StreamSnap {
            id: 1,
            generation: 10,
            mute: false,
            volume: 1.0,
        }]);
        // Answer the pending audio ask, as BlueZ does, so the lifecycle
        // reaches Routing before the grant's recovery is decided.
        if let Some(id) = daemon.lifecycle.request_id() {
            daemon.step(
                Input::TransportReply { id, reply: crate::step::TransportReply::Connected },
                Clock::new(now.mono.0 + 50, now.unix.0 + 50),
            );
            daemon.note_ownership(true, now.mono);
            daemon.audio.set_local_playing_for_test(true);
            // Give the session a graph with the output in it, as the live
            // daemon has, so the grant flag survives to the watch.
            daemon.step(
                Input::Audio(Graph {
                    default_sink: "speakers".into(),
                    sinks: vec![
                        Sink {
                            name: "pods".into(),
                            address: Some(addr(3)),
                            profile: Some("a2dp-sink".into()),
                        },
                        Sink { name: "speakers".into(), address: None, profile: None },
                    ],
                    cards: Vec::new(),
                    streams: vec![LiveStream {
                        id: 1,
                        generation: 10,
                        sink: "speakers".into(),
                        role: String::new(),
                        node: "Brave".into(),
                        mute: true,
                        volume: 1.0,
                        corked: false,
                    }],
                }),
                Clock::new(now.mono.0 + 60, now.unix.0 + 60),
            );
            daemon.step(
                Input::Audio(Graph {
                    default_sink: "pods".into(),
                    sinks: vec![
                        Sink {
                            name: "pods".into(),
                            address: Some(addr(3)),
                            profile: Some("a2dp-sink".into()),
                        },
                        Sink { name: "speakers".into(), address: None, profile: None },
                    ],
                    cards: Vec::new(),
                    streams: vec![LiveStream {
                        id: 1,
                        generation: 10,
                        sink: "pods".into(),
                        role: String::new(),
                        node: "Brave".into(),
                        mute: true,
                        volume: 1.0,
                        corked: false,
                    }],
                }),
                Clock::new(now.mono.0 + 2_700, now.unix.0 + 2_700),
            );
            daemon.session.resume_output_on_grant = true;
            // Start the watch while the output still exists in the last graph:
            // a step with no output on record would drop the grant flag first.
            daemon.step(Input::Tick, Clock::new(now.mono.0 + 100, now.unix.0 + 100));
            daemon.step(Input::Tick, Clock::new(now.mono.0 + 2_600, now.unix.0 + 2_600));
            let action = daemon.step(
                Input::Audio(Graph {
                    default_sink: "pods".into(),
                    sinks: vec![
                        Sink {
                            name: "pods".into(),
                            address: Some(addr(3)),
                            profile: Some("a2dp-sink".into()),
                        },
                        Sink { name: "speakers".into(), address: None, profile: None },
                    ],
                    cards: Vec::new(),
                    streams: vec![LiveStream {
                        id: 1,
                        generation: 10,
                        sink: "pods".into(),
                        role: String::new(),
                        node: "Brave".into(),
                        mute: true,
                        volume: 1.0,
                        corked: false,
                    }],
                }),
                now,
            );
            assert!(action
                .effects
                .iter()
                .any(|e| matches!(e, Effect::MoveStreams(m) if m.sink == "speakers")));
            assert!(
                !action.effects.iter().any(|e| matches!(e, Effect::Restore(_))),
                "the pending unmute must wait until recovery moves the stream back to the pods"
            );
            assert_eq!(daemon.output.held_streams().len(), 1);
            assert!(!daemon.output.held_streams()[0].mute);
        }
    }
    #[test]
    fn playback_on_a_control_only_connection_opens_audio_without_waiting_for_ownership() {
        use crate::step::{Effect, Input};
        let now = clock();
        let mut daemon = Daemon::new("xps");
        daemon.select_accessory(Some(addr(9)));
        ready(&mut daemon, now.mono);
        daemon.note_ownership(false, now.mono);
        let background = daemon.step(Input::Tick, now);
        assert!(!background.effects.iter().any(|e| matches!(e, Effect::ConnectTransport { .. })));
        daemon.claim_playback(now);
        let playing = daemon.step(Input::Tick, now);
        assert!(playing.effects.iter().any(
            |e| matches!(e, Effect::ConnectTransport { accessory, .. } if *accessory == addr(9))
        ));
        assert!(
            !daemon.session.resume_output_on_grant,
            "a new transport needs no old-output recovery"
        );
        let repeated = daemon.step(Input::Tick, now);
        assert!(!repeated.effects.iter().any(|e| matches!(e, Effect::ConnectTransport { .. })));
        let granted = daemon
            .step(Input::Observation(crate::pods::observe::Observation::Ownership(true)), now);
        assert!(!granted.effects.iter().any(|e| matches!(e, Effect::ConnectTransport { .. })));
    }
}
