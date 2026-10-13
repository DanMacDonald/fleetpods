//! Who holds audio: the seat. Claim, yield, failure, and the interpretation
//! of playback all live here. Direct reports, fleet reports, and local
//! playback stay where they were recorded; this module reads them through
//! [`Inputs`] and does not write them back. Callers apply the returned
//! [`Intent`] to routing, the audio route, and the player yield exactly once.

use crate::fleet::roster::RosterReport;
use crate::pods::reports::SourceReport;
use crate::types::{Address, Clock, Evidence, MonoMs, UnixMs};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Holder {
    Unknown,
    Local {
        playing: bool,
    },
    /// `address` is absent when something remote is playing and nobody named it.
    Remote {
        address: Option<Address>,
        playing: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    None,
    Direct,
    Fleet,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub holder: Holder,
    pub source: Source,
}

/// What the caller must do to routing, the audio route, and the player
/// yield because the seat changed. The seat decides; the caller applies
/// this once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Intent {
    Claim,
    Release,
}

/// One reading of every fact `resolve` interprets. Callers build this;
/// they do not patch the result afterwards.
pub struct Inputs<'a> {
    pub ownership: &'a Evidence<bool>,
    pub source_report: &'a Evidence<SourceReport>,
    pub remote: &'a Evidence<Address>,
    pub fleet: Option<&'a RosterReport>,
    pub local_address: Option<Address>,
    pub local_playing: bool,
    pub routed_here: bool,
    /// Playback observed on this machine, when a player reported it.
    pub observed_local: Option<bool>,
    /// Both buds docked silences any playing reading.
    pub both_docked: bool,
}

/// How long an explicit user switch outranks a remote resumption.
pub const USER_WINDOW_MS: u64 = 30_000;

/// The seat record. Claim and step-back are its writers, together with
/// failure and reconnect notes. Status reads it through the accessors.
#[derive(Clone, Debug)]
pub struct Seat {
    held: bool,
    yielded: bool,
    phase: &'static str,
    failure: &'static str,
    updated: Option<UnixMs>,
    ownership_at_claim: Option<MonoMs>,
    user_claimed_at: Option<MonoMs>,
}

impl Seat {
    pub fn new() -> Self {
        Self {
            held: false,
            yielded: false,
            phase: "idle",
            failure: "",
            updated: None,
            ownership_at_claim: None,
            user_claimed_at: None,
        }
    }

    pub fn held(&self) -> bool {
        self.held
    }

    pub fn yielded(&self) -> bool {
        self.yielded
    }

    pub fn phase(&self) -> &'static str {
        self.phase
    }

    pub fn failure(&self) -> &'static str {
        self.failure
    }

    pub fn updated(&self) -> Option<UnixMs> {
        self.updated
    }

    pub fn claim(&mut self, now: Clock) -> Intent {
        self.held = true;
        self.yielded = false;
        self.phase = "idle";
        self.failure = "";
        self.updated = Some(now.unix);
        Intent::Claim
    }

    /// An explicit user switch. Inside [`USER_WINDOW_MS`] a remote
    /// resumption does not reverse this claim.
    pub fn claim_user(&mut self, now: Clock) -> Intent {
        self.user_claimed_at = Some(now.mono);
        self.claim(now)
    }

    pub fn user_intent_fresh(&self, now: MonoMs) -> bool {
        self.user_claimed_at.is_some_and(|at| now.saturating_since(at) < USER_WINDOW_MS)
    }

    /// Record a play that asked for the link. A remote report already
    /// stored at this point is older intent, even at the same millisecond.
    pub fn note_claim(&mut self, ownership_at: Option<MonoMs>) {
        self.ownership_at_claim = ownership_at;
    }

    pub fn step_back(&mut self, now: Clock) -> Intent {
        self.held = false;
        self.yielded = true;
        self.phase = "idle";
        self.failure = "";
        self.updated = Some(now.unix);
        self.user_claimed_at = None;
        Intent::Release
    }

    pub fn note_failure(&mut self, reason: &'static str, now: Clock) {
        self.phase = "failed";
        self.failure = reason;
        self.updated = Some(now.unix);
    }

    pub fn note_failure_if_empty(&mut self, reason: &'static str, now: Clock) {
        if self.failure.is_empty() {
            self.failure = reason;
            self.updated = Some(now.unix);
        }
    }

    /// The seat is left so an Apple device can take the slot. The
    /// reconnect state machine itself stays with the caller.
    pub fn begin_reconnect(&mut self, now: Clock) -> Intent {
        self.held = false;
        self.yielded = true;
        self.phase = "reconnecting";
        self.failure = "";
        self.updated = Some(now.unix);
        self.user_claimed_at = None;
        Intent::Release
    }

    pub fn note_reconnected(&mut self, now: Clock) {
        self.phase = "idle";
        self.updated = Some(now.unix);
    }

    /// A local ownership report arrived; this machine is no longer yielded.
    pub fn mark_local_owned(&mut self) {
        self.yielded = false;
    }

    /// Request local confirmation when this machine is playing and no
    /// newer remote report has arrived. A remote report that shows up
    /// after the claim wins.
    pub fn wants_local_confirm(
        &self,
        want: bool,
        local_playing: bool,
        audio_yielded: bool,
        ownership: &Evidence<bool>,
        now: MonoMs,
    ) -> bool {
        if !want || audio_yielded {
            return false;
        }
        // An explicit switch confirms even when local playback was paused
        // by an earlier yield; an automatic claim still needs a player.
        if !local_playing && !self.user_intent_fresh(now) {
            return false;
        }
        match ownership {
            Evidence::Fresh { value: false, at } => {
                matches!(self.ownership_at_claim, Some(seen) if *at <= seen)
            }
            _ => true,
        }
    }

    /// Interpret who holds audio, including observed playback and the
    /// docked silence. `observed_remote` is looked up only when the
    /// resolved holder names a peer.
    pub fn resolve(
        &self,
        inputs: Inputs<'_>,
        observed_remote: &dyn Fn(Address) -> Option<bool>,
    ) -> Resolved {
        let mut resolved = resolve(
            inputs.ownership,
            inputs.source_report,
            inputs.remote,
            inputs.fleet,
            inputs.local_address,
            inputs.local_playing,
            inputs.routed_here,
        );
        match &mut resolved.holder {
            Holder::Local { playing } => {
                if let Some(observed) = inputs.observed_local {
                    *playing = observed;
                }
            }
            Holder::Remote { address: Some(peer), playing } => {
                if let Some(observed) = observed_remote(*peer) {
                    *playing = observed;
                }
            }
            _ => {}
        }
        if inputs.both_docked {
            match &mut resolved.holder {
                Holder::Local { playing } | Holder::Remote { playing, .. } => *playing = false,
                Holder::Unknown => {}
            }
        }
        resolved
    }
}

impl Default for Seat {
    fn default() -> Self {
        Self::new()
    }
}

pub fn resolve(
    ownership: &Evidence<bool>,
    source_report: &Evidence<SourceReport>,
    remote: &Evidence<Address>,
    fleet: Option<&RosterReport>,
    local_address: Option<Address>,
    local_playing: bool,
    routed_here: bool,
) -> Resolved {
    if let Some(mut holder) = from_direct(ownership, source_report, remote) {
        if let Holder::Local { playing } = &mut holder {
            // Kind above zero is the report the roster sends as not idle. The other
            // laptops draw that as playing. A desktop player can still light an
            // idle owner once the sink is routed here, and it does not erase the
            // pod report when no player is visible.
            *playing = *playing || (local_playing && routed_here);
        }
        return Resolved { holder, source: Source::Direct };
    }
    if let Some(report) = fleet.filter(|report| !report.pending) {
        let holder = match report.audio_source {
            Some(address) if Some(address) == local_address => {
                Holder::Local { playing: !report.audio_idle || (local_playing && routed_here) }
            }
            Some(address) => Holder::Remote { address: Some(address), playing: !report.audio_idle },
            None => Holder::Unknown,
        };
        if !matches!(holder, Holder::Unknown) {
            return Resolved { holder, source: Source::Fleet };
        }
    }
    // Source notifications describe changes, not a heartbeat. Keep the last
    // reported holder until the control session ends (drop_owner clears these
    // reports), but do not turn its old source category into live playback.
    if let Some(mut holder) = from_last_report(ownership, source_report, remote) {
        if let Holder::Local { playing } = &mut holder {
            *playing = local_playing && routed_here;
        }
        return Resolved { holder, source: Source::Direct };
    }
    Resolved { holder: Holder::Unknown, source: Source::None }
}

fn from_last_report(
    ownership: &Evidence<bool>,
    source_report: &Evidence<SourceReport>,
    remote: &Evidence<Address>,
) -> Option<Holder> {
    let local = source_report
        .known()
        .filter(|source| source.kind > 0)
        .filter(|_| match (source_report.at(), ownership.at()) {
            (Some(source), Some(owner)) => source > owner,
            (Some(_), None) => true,
            _ => false,
        })
        .map(|source| source.local)
        .or_else(|| ownership.known().copied())?;
    Some(if local {
        Holder::Local { playing: false }
    } else {
        Holder::Remote { address: remote.known().copied(), playing: false }
    })
}

fn from_direct(
    ownership: &Evidence<bool>,
    source_report: &Evidence<SourceReport>,
    remote: &Evidence<Address>,
) -> Option<Holder> {
    if let Evidence::Fresh { value, at } = source_report {
        if value.kind > 0 && source_takes_precedence(*at, value.local, ownership) {
            return Some(holder(value.local, true, remote));
        }
    }
    match ownership {
        Evidence::Fresh { value: true, .. } => Some(Holder::Local { playing: false }),
        Evidence::Fresh { value: false, .. } => {
            Some(Holder::Remote { address: remote.as_fresh().copied(), playing: false })
        }
        _ => None,
    }
}

fn source_takes_precedence(source_at: MonoMs, local: bool, ownership: &Evidence<bool>) -> bool {
    match ownership {
        // Source and ownership notifications can share a clock tick. A
        // contradictory source must not override the confirmed owner then.
        Evidence::Fresh { at, value } => source_at > *at || (source_at == *at && local == *value),
        _ => true,
    }
}

fn holder(local: bool, playing: bool, remote: &Evidence<Address>) -> Holder {
    if local {
        Holder::Local { playing }
    } else {
        Holder::Remote { address: remote.as_fresh().copied(), playing }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::roster::RosterReport;
    use crate::pods::reports::SourceReport;
    use crate::types::{HostId, SessionId, UnixMs};

    fn fleet(source: Option<Address>, idle: bool) -> RosterReport {
        RosterReport {
            reporter: HostId::new([1; 16]),
            reporter_address: Address::parse("11:11:11:11:11:11").unwrap(),
            accessory: Address::parse("AA:AA:AA:AA:AA:AA").unwrap(),
            session: SessionId(1),
            seq: 1,
            at: MonoMs(0),
            sent: UnixMs(10),
            peers: vec![Address::parse("11:11:11:11:11:11").unwrap()],
            audio_source: source,
            pending: false,
            audio_idle: idle,
            wire_session: None,
            last_use: UnixMs(0),
        }
    }

    #[test]
    fn direct_remote_evidence_survives_local_playback() {
        let remote = Address::parse("22:22:22:22:22:22").unwrap();
        let resolved = resolve(
            &Evidence::fresh(false, MonoMs(10)),
            &Evidence::fresh(SourceReport { local: false, kind: 2 }, MonoMs(20)),
            &Evidence::fresh(remote, MonoMs(20)),
            Some(&fleet(Some(Address::parse("11:11:11:11:11:11").unwrap()), false)),
            None,
            true,
            true,
        );
        assert_eq!(resolved.source, Source::Direct);
        assert_eq!(resolved.holder, Holder::Remote { address: Some(remote), playing: true });
    }

    #[test]
    fn a_stale_direct_report_yields_to_a_fresh_fleet_roster() {
        let peer = Address::parse("22:22:22:22:22:22").unwrap();
        let resolved = resolve(
            &Evidence::Stale { value: true, at: MonoMs(1) },
            &Evidence::Unknown,
            &Evidence::Unknown,
            Some(&fleet(Some(peer), false)),
            None,
            true,
            false,
        );
        assert_eq!(resolved.source, Source::Fleet);
        assert_eq!(resolved.holder, Holder::Remote { address: Some(peer), playing: true });
    }

    #[test]
    fn ownership_wins_a_conflicting_source_at_the_same_clock_tick() {
        let peer = Address::parse("22:22:22:22:22:22").unwrap();
        for local in [true, false] {
            for stale in [false, true] {
                let owner = if stale {
                    Evidence::Stale { value: local, at: MonoMs(10) }
                } else {
                    Evidence::fresh(local, MonoMs(10))
                };
                let report = SourceReport { local: !local, kind: 2 };
                let source = if stale {
                    Evidence::Stale { value: report, at: MonoMs(10) }
                } else {
                    Evidence::fresh(report, MonoMs(10))
                };
                let resolved = resolve(
                    &owner,
                    &source,
                    &Evidence::fresh(peer, MonoMs(10)),
                    None,
                    None,
                    false,
                    false,
                );
                assert_eq!(
                    resolved.holder,
                    if local {
                        Holder::Local { playing: false }
                    } else {
                        Holder::Remote { address: Some(peer), playing: false }
                    }
                );
            }
        }
    }

    #[test]
    fn a_local_source_stays_playing_without_a_desktop_player() {
        let resolved = resolve(
            &Evidence::fresh(true, MonoMs(10)),
            &Evidence::fresh(SourceReport { local: true, kind: 2 }, MonoMs(20)),
            &Evidence::Unknown,
            None,
            None,
            false,
            true,
        );
        assert_eq!(resolved.holder, Holder::Local { playing: true });
    }

    #[test]
    fn an_idle_owner_stays_dark_until_a_player_is_routed_here() {
        let idle = resolve(
            &Evidence::fresh(true, MonoMs(10)),
            &Evidence::Unknown,
            &Evidence::Unknown,
            None,
            None,
            false,
            true,
        );
        assert_eq!(idle.holder, Holder::Local { playing: false });
        let playing = resolve(
            &Evidence::fresh(true, MonoMs(10)),
            &Evidence::Unknown,
            &Evidence::Unknown,
            None,
            None,
            true,
            true,
        );
        assert_eq!(playing.holder, Holder::Local { playing: true });
    }

    #[test]
    fn remembered_remote_ownership_does_not_invent_current_playback() {
        let peer = Address::parse("22:22:22:22:22:22").unwrap();
        let source =
            Evidence::Stale { value: SourceReport { local: false, kind: 2 }, at: MonoMs(20) };
        let resolved = resolve(
            &Evidence::Unknown,
            &source,
            &Evidence::Stale { value: peer, at: MonoMs(20) },
            None,
            None,
            true,
            true,
        );
        assert_eq!(resolved.holder, Holder::Remote { address: Some(peer), playing: false });
        let newer_owner = resolve(
            &Evidence::Stale { value: true, at: MonoMs(30) },
            &source,
            &Evidence::Unknown,
            None,
            None,
            true,
            true,
        );
        assert_eq!(newer_owner.holder, Holder::Local { playing: true });
    }

    #[test]
    fn a_playing_remote_may_have_no_address() {
        let resolved = resolve(
            &Evidence::Unknown,
            &Evidence::fresh(SourceReport { local: false, kind: 1 }, MonoMs(5)),
            &Evidence::Unknown,
            None,
            None,
            false,
            false,
        );
        assert_eq!(resolved.holder, Holder::Remote { address: None, playing: true });
    }

    #[test]
    fn a_remote_report_at_the_claim_millisecond_still_loses() {
        let mut seat = Seat::new();
        seat.note_claim(Some(MonoMs(50)));
        // Remote report stored before the claim, stamped at the claim time.
        let older = Evidence::fresh(false, MonoMs(50));
        assert!(seat.wants_local_confirm(true, true, false, &older, MonoMs(100)));
        // A report stamped after the stored ownership loses the seat.
        let newer = Evidence::fresh(false, MonoMs(80));
        assert!(!seat.wants_local_confirm(true, true, false, &newer, MonoMs(100)));
    }

    #[test]
    fn user_intent_expires_at_the_window_edge() {
        let mut seat = Seat::new();
        seat.claim_user(crate::types::Clock::new(1_000, 1_000));
        assert!(seat.user_intent_fresh(MonoMs(1_000 + USER_WINDOW_MS - 1)));
        assert!(!seat.user_intent_fresh(MonoMs(1_000 + USER_WINDOW_MS)));
        // Stepping back consumes the intent at once.
        seat.step_back(crate::types::Clock::new(2_000, 2_000));
        assert!(!seat.user_intent_fresh(MonoMs(2_000)));
    }

    #[test]
    fn a_user_switch_confirms_without_a_local_player() {
        let mut seat = Seat::new();
        seat.claim_user(crate::types::Clock::new(1_000, 1_000));
        let ownership = Evidence::Unknown;
        assert!(seat.wants_local_confirm(true, false, false, &ownership, MonoMs(2_000)));
        // An automatic claim without a player still does not confirm.
        let auto = Seat::new();
        assert!(!auto.wants_local_confirm(true, false, false, &ownership, MonoMs(2_000)));
    }

    #[test]
    fn docked_buds_silence_the_resolved_holder() {
        let seat = Seat::new();
        let resolved = seat.resolve(
            Inputs {
                ownership: &Evidence::fresh(true, MonoMs(10)),
                source_report: &Evidence::Unknown,
                remote: &Evidence::Unknown,
                fleet: None,
                local_address: None,
                local_playing: true,
                routed_here: true,
                observed_local: None,
                both_docked: true,
            },
            &|_| None,
        );
        assert_eq!(resolved.holder, Holder::Local { playing: false });
    }
}
