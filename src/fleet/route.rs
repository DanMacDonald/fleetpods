//! Handoff intent and one fleet admission transaction.
//! A routing retry never opens a second local Bluetooth attempt.

use crate::types::{Address, MonoMs, SessionId, UnixMs};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Idle,
    WaitingAdmission,
    Admitting,
    Connecting,
    WaitingConnection,
    WaitingCompletion,
    Success,
    Failed,
}

impl Phase {
    pub fn wire(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::WaitingAdmission => "waiting_admission",
            Self::Admitting => "admitting",
            Self::Connecting => "connecting",
            Self::WaitingConnection => "waiting_connection",
            Self::WaitingCompletion => "waiting_completion",
            Self::Success => "success",
            Self::Failed => "failed",
        }
    }

    pub fn busy(self) -> bool {
        matches!(
            self,
            Self::WaitingAdmission
                | Self::Admitting
                | Self::Connecting
                | Self::WaitingConnection
                | Self::WaitingCompletion
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Intent {
    None,
    Claim,
    Release,
}

impl Intent {
    pub fn wire(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Claim => "claim",
            Self::Release => "release",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Start {
    Sent { tx: u64 },
    Duplicate,
    Busy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admit {
    /// A free slot. The helper stays connected.
    Beside,
    /// The helper has to move something before the requester can attach.
    NeedsHandoff,
    Stale,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub at: UnixMs,
    pub result: &'static str,
    pub peer: Address,
    pub retain: Option<Address>,
}

pub struct Routing {
    phase: Phase,
    tx: u64,
    next_tx: u64,
    peer: Option<Address>,
    retain: Option<Address>,
    error: &'static str,
    admission: &'static str,
    intent: Intent,
    want_seat: bool,
    handoff_result: &'static str,
    outcome: Option<Outcome>,
    retry_deadline: Option<MonoMs>,
    attempt: u8,
    session: SessionId,
    claim_queued: bool,
    helper: bool,
}

impl Routing {
    pub fn new() -> Self {
        Self {
            phase: Phase::Idle,
            tx: 0,
            next_tx: 0,
            peer: None,
            retain: None,
            error: "",
            admission: "none",
            intent: Intent::None,
            want_seat: false,
            handoff_result: "none",
            outcome: None,
            retry_deadline: None,
            attempt: 0,
            session: SessionId(1),
            claim_queued: false,
            helper: false,
        }
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn tx(&self) -> u64 {
        self.tx
    }

    pub fn peer(&self) -> Option<Address> {
        self.peer
    }

    pub fn retain(&self) -> Option<Address> {
        self.retain
    }

    pub fn error(&self) -> &'static str {
        self.error
    }

    pub fn admission(&self) -> &'static str {
        self.admission
    }

    pub fn intent(&self) -> Intent {
        self.intent
    }

    pub fn want_seat(&self) -> bool {
        self.want_seat
    }

    pub fn handoff_result(&self) -> &'static str {
        self.handoff_result
    }

    pub fn outcome(&self) -> Option<&Outcome> {
        self.outcome.as_ref()
    }

    pub fn retry_deadline(&self) -> Option<MonoMs> {
        self.retry_deadline
    }

    pub fn session(&self) -> SessionId {
        self.session
    }

    pub fn claim_queued(&self) -> bool {
        self.claim_queued
    }

    pub fn restart(&mut self) {
        self.session = SessionId(self.session.0 + 1);
        self.tx = self.tx.wrapping_add(1);
        if self.phase.busy() {
            self.phase = Phase::Failed;
            self.error = "interrupted";
        }
        self.retry_deadline = None;
        self.claim_queued = false;
    }

    pub fn clear_failure(&mut self) {
        if self.phase != Phase::Failed {
            return;
        }
        self.phase = Phase::Idle;
        self.error = "";
        self.peer = None;
        self.retain = None;
        self.admission = "none";
    }

    pub fn start(&mut self, peer: Address) -> Start {
        if self.phase.busy() {
            return if self.peer == Some(peer) { Start::Duplicate } else { Start::Busy };
        }
        self.next_tx += 1;
        self.tx = self.next_tx;
        self.peer = Some(peer);
        self.retain = None;
        self.phase = Phase::WaitingAdmission;
        self.error = "";
        self.admission = "none";
        self.attempt = 1;
        self.retry_deadline = None;
        self.helper = false;
        Start::Sent { tx: self.tx }
    }

    /// A request arrived while this host was idle. The transaction id is the one on the air.
    pub fn accept_request(&mut self, peer: Address, tx: u64) -> bool {
        if self.phase.busy() {
            return false;
        }
        self.tx = tx;
        self.next_tx = self.next_tx.max(tx);
        self.peer = Some(peer);
        self.retain = None;
        self.phase = Phase::Admitting;
        self.error = "";
        self.admission = "none";
        self.retry_deadline = None;
        self.attempt = 1;
        self.helper = true;
        true
    }

    /// The helper named who stays and is waiting for the requester to connect.
    pub fn grant(&mut self, tx: u64, retain: Option<Address>) -> bool {
        if tx != self.tx || self.phase != Phase::Admitting {
            return false;
        }
        self.retain = retain;
        self.phase = Phase::WaitingConnection;
        self.admission = "ok";
        true
    }

    pub fn peer_busy(&mut self, tx: u64, now: MonoMs) -> bool {
        if tx != self.tx || self.phase != Phase::WaitingAdmission {
            return false;
        }
        self.attempt = self.attempt.saturating_add(1);
        self.retry_deadline = Some(now.saturating_add(1_500));
        self.error = "peer_busy";
        true
    }

    pub fn admit(&mut self, tx: u64, free_slot: bool, retain: Option<Address>) -> Admit {
        if tx != self.tx || self.phase != Phase::WaitingAdmission {
            return Admit::Stale;
        }
        self.retain = retain;
        if free_slot {
            self.phase = Phase::Connecting;
            self.admission = "ok";
            Admit::Beside
        } else {
            self.phase = Phase::Admitting;
            self.admission = "ok";
            Admit::NeedsHandoff
        }
    }

    pub fn note_connecting(&mut self, tx: u64) -> bool {
        if tx != self.tx || !self.phase.busy() {
            return false;
        }
        self.phase = Phase::WaitingCompletion;
        true
    }

    pub fn complete(&mut self, tx: u64, alongside: bool, at: UnixMs) -> bool {
        if tx != self.tx || !self.phase.busy() {
            return false;
        }
        let Some(peer) = self.peer else {
            return false;
        };
        self.phase = Phase::Success;
        self.error = "";
        self.retry_deadline = None;
        let result = if self.helper {
            if alongside {
                "admitted_alongside"
            } else {
                "admitted"
            }
        } else if alongside {
            "joined_alongside"
        } else {
            "joined"
        };
        self.outcome = Some(Outcome { at, result, peer, retain: self.retain });
        true
    }

    pub fn fail(&mut self, tx: u64, reason: &'static str, at: UnixMs) -> bool {
        if tx != self.tx || !self.phase.busy() {
            return false;
        }
        // An admitted requester is terminal on the admission question: a
        // delayed or replayed failure for the same transaction must not
        // rewind a handoff the peer already granted. The helper side keeps
        // its fail: its own admission prerequisite can still collapse.
        if !self.helper && self.admission == "ok" {
            return false;
        }
        let Some(peer) = self.peer else {
            return false;
        };
        self.phase = Phase::Failed;
        self.error = reason;
        self.retry_deadline = None;
        self.outcome = Some(Outcome { at, result: reason, peer, retain: self.retain });
        true
    }

    /// Audio ownership ask. Independent of whether a fleet transaction is open.
    pub fn claim_audio(&mut self) {
        self.want_seat = true;
        self.intent = Intent::Claim;
        self.handoff_result = "none";
    }

    pub fn release_audio(&mut self) {
        self.want_seat = false;
        self.intent = Intent::Release;
        self.handoff_result = "none";
    }

    pub fn note_alongside(&mut self, peer: Address, at: UnixMs) {
        self.outcome = Some(Outcome { at, result: "joined_alongside", peer, retain: Some(peer) });
        self.phase = Phase::Success;
        self.error = "";
        self.retry_deadline = None;
    }

    pub fn queue_claim_until_route_lands(&mut self) -> bool {
        if self.phase != Phase::WaitingCompletion {
            return false;
        }
        self.claim_queued = true;
        self.want_seat = true;
        true
    }
}

impl Default for Routing {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer() -> Address {
        Address::parse("AA:BB:CC:DD:EE:01").unwrap()
    }

    #[test]
    fn a_duplicate_request_keeps_the_same_transaction() {
        let mut routing = Routing::new();
        let Start::Sent { tx } = routing.start(peer()) else { panic!("start") };
        assert_eq!(routing.start(peer()), Start::Duplicate);
        assert_eq!(routing.tx(), tx);
        assert!(routing.peer_busy(tx, MonoMs(0)));
        assert_eq!(routing.phase(), Phase::WaitingAdmission);
        assert!(routing.phase().busy());
        assert!(!routing.complete(tx + 1, false, UnixMs(1)));
        assert!(routing.complete(tx, true, UnixMs(2)));
        assert_eq!(routing.outcome().unwrap().result, "joined_alongside");
        assert!(!routing.phase().busy());
    }

    #[test]
    fn repeated_admission_does_not_rewind_an_active_connection() {
        let mut routing = Routing::new();
        let Start::Sent { tx } = routing.start(peer()) else { panic!("start") };
        assert_eq!(routing.admit(tx, true, Some(peer())), Admit::Beside);
        assert_eq!(routing.admit(tx, true, Some(peer())), Admit::Stale);
        assert!(routing.note_connecting(tx));
        assert_eq!(routing.admit(tx, true, Some(peer())), Admit::Stale);
        assert_eq!(routing.phase(), Phase::WaitingCompletion);
    }

    #[test]
    fn a_failure_after_admission_does_not_rewind_the_requester() {
        let mut routing = Routing::new();
        let Start::Sent { tx } = routing.start(peer()) else { panic!("start") };
        // Before admission, a failure is a failure.
        assert!(routing.fail(tx, "unavailable", UnixMs(1)));
        assert_eq!(routing.phase(), Phase::Failed);

        let mut routing = Routing::new();
        let Start::Sent { tx } = routing.start(peer()) else { panic!("start") };
        assert_eq!(routing.admit(tx, true, None), Admit::Beside);
        assert!(!routing.fail(tx, "unavailable", UnixMs(2)));
        assert_eq!(routing.phase(), Phase::Connecting);
        assert!(routing.note_connecting(tx));
        assert!(!routing.fail(tx, "unavailable", UnixMs(3)));
        assert_eq!(routing.phase(), Phase::WaitingCompletion);

        // The helper side can still fail its own admission prerequisite.
        let mut helper = Routing::new();
        assert!(helper.accept_request(peer(), 9));
        assert!(helper.grant(9, None));
        assert!(helper.fail(9, "unavailable", UnixMs(4)));
        assert_eq!(helper.phase(), Phase::Failed);
    }

    #[test]
    fn a_free_slot_does_not_ask_the_helper_to_disconnect() {
        let mut routing = Routing::new();
        let Start::Sent { tx } = routing.start(peer()) else { panic!("start") };
        assert_eq!(routing.admit(tx, true, None), Admit::Beside);
        assert_eq!(routing.admit(tx - 1, false, None), Admit::Stale);
    }

    #[test]
    fn restart_retires_the_transaction_a_late_success_would_finish() {
        let mut routing = Routing::new();
        let Start::Sent { tx } = routing.start(peer()) else { panic!("start") };
        let session = routing.session();
        routing.restart();
        assert_ne!(routing.session(), session);
        assert!(!routing.complete(tx, false, UnixMs(5)));
        assert_eq!(routing.phase(), Phase::Failed);
    }

    #[test]
    fn an_idle_host_accepts_one_request_and_a_busy_host_does_not() {
        let mut routing = Routing::new();
        assert!(routing.accept_request(peer(), 4));
        assert!(routing.grant(4, Some(peer())));
        assert_eq!(routing.phase(), Phase::WaitingConnection);
        assert!(!routing.accept_request(peer(), 5));
    }
}
