//! One enrollment attempt. Transport delivers bytes; this module owns the phase,
//! the attempt count, and the tentative keys. Membership is unchanged until a
//! later, explicit commit.

use crate::types::{valid_name, Address, Code, InvitationId, MonoMs};

use super::members::Proposal;

pub const ATTEMPT_LIMIT: u8 = 5;
pub const TTL_MS: u64 = 120_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Connecting,
    Inviting,
    WaitingOffer,
    Incoming,
    Exchanging,
    WaitingAck,
    WaitingCommit,
    WaitingDone,
    Success,
    Error,
    Expired,
    Cancelled,
}

impl Phase {
    pub fn wire(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Connecting => "connecting",
            Self::Inviting => "inviting",
            Self::WaitingOffer => "waiting_offer",
            Self::Incoming => "incoming",
            Self::Exchanging => "exchanging",
            Self::WaitingAck => "waiting_ack",
            Self::WaitingCommit => "waiting_commit",
            Self::WaitingDone => "waiting_done",
            Self::Success => "success",
            Self::Error => "error",
            Self::Expired => "expired",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn active(self) -> bool {
        matches!(
            self,
            Self::Connecting
                | Self::Inviting
                | Self::WaitingOffer
                | Self::Incoming
                | Self::Exchanging
                | Self::WaitingAck
                | Self::WaitingCommit
                | Self::WaitingDone
        )
    }
}

struct Exchange {
    id: InvitationId,
    initiator: bool,
    peer: Address,
    peer_name: String,
    code: Option<Code>,
    expiry: MonoMs,
    code_attempt: u8,
    phase: Phase,
    error: String,
    proposal: Option<Proposal>,
}

pub struct Enrollment {
    current: Option<Exchange>,
}

impl Enrollment {
    pub fn new() -> Self {
        Self { current: None }
    }

    pub fn phase(&self) -> Phase {
        self.current.as_ref().map(|exchange| exchange.phase).unwrap_or(Phase::Idle)
    }

    pub fn id(&self) -> Option<InvitationId> {
        self.current.as_ref().map(|exchange| exchange.id)
    }

    pub fn initiator(&self) -> bool {
        self.current.as_ref().is_some_and(|exchange| exchange.initiator)
    }

    pub fn peer(&self) -> Option<(Address, &str)> {
        self.current.as_ref().map(|exchange| (exchange.peer, exchange.peer_name.as_str()))
    }

    pub fn code_attempt(&self) -> u8 {
        self.current.as_ref().map(|exchange| exchange.code_attempt).unwrap_or(0)
    }

    pub fn error(&self) -> &str {
        self.current.as_ref().map(|exchange| exchange.error.as_str()).unwrap_or("")
    }

    pub fn expiry(&self) -> Option<MonoMs> {
        self.current
            .as_ref()
            .filter(|exchange| exchange.phase.active())
            .map(|exchange| exchange.expiry)
    }

    /// Shown to the inviter while the code is on screen. Absent after that.
    pub fn displayed_code(&self) -> Option<String> {
        let exchange = self.current.as_ref()?;
        if exchange.initiator && matches!(exchange.phase, Phase::Connecting | Phase::Inviting) {
            exchange.code.as_ref().map(Code::display)
        } else {
            None
        }
    }

    pub fn proposal(&self) -> Option<&Proposal> {
        self.current.as_ref().and_then(|exchange| exchange.proposal.as_ref())
    }

    pub fn password(&self) -> Option<Code> {
        self.current.as_ref().and_then(|exchange| exchange.code.clone())
    }

    fn early(&self) -> bool {
        matches!(
            self.phase(),
            Phase::Connecting
                | Phase::Inviting
                | Phase::WaitingOffer
                | Phase::Incoming
                | Phase::Exchanging
        )
    }

    pub fn begin_invite(
        &mut self,
        local: Address,
        peer: Address,
        name: &str,
        code: Code,
        id: InvitationId,
        now: MonoMs,
    ) -> bool {
        if peer == local || !valid_name(name) {
            return false;
        }
        // A new explicit invitation replaces an attempt that has not saved keys.
        if self.phase().active() && !self.early() {
            return false;
        }
        self.current = Some(Exchange {
            id,
            initiator: true,
            peer,
            peer_name: name.to_string(),
            code: Some(code),
            expiry: now.saturating_add(TTL_MS),
            code_attempt: 0,
            phase: Phase::Connecting,
            error: String::new(),
            proposal: None,
        });
        true
    }

    /// The invite advertisement was handed to the controller. Code entry is next,
    /// not the post-authentication "finishing" phases.
    pub fn mark_delivered(&mut self, id: InvitationId) -> bool {
        let Some(exchange) = self.current.as_mut() else {
            return false;
        };
        if exchange.id != id || exchange.phase != Phase::Connecting || !exchange.initiator {
            return false;
        }
        exchange.phase = Phase::Inviting;
        true
    }

    pub fn receive_invite(
        &mut self,
        local: Address,
        peer: Address,
        name: &str,
        id: InvitationId,
        now: MonoMs,
    ) -> bool {
        // Two invites crossed. The lower address stays the inviter so the
        // attempt has one role on each side.
        if self.initiator() && peer.bytes() < local.bytes() && self.early() {
            self.current = None;
        }
        if self.phase().active() || peer == local || !valid_name(name) {
            return false;
        }
        self.current = Some(Exchange {
            id,
            initiator: false,
            peer,
            peer_name: name.to_string(),
            code: None,
            expiry: now.saturating_add(TTL_MS),
            code_attempt: 0,
            phase: Phase::WaitingOffer,
            error: String::new(),
            proposal: None,
        });
        true
    }

    /// A new offer before the code, or a later offer after a wrong code.
    /// An offer does not authenticate the peer.
    pub fn offer(&mut self, id: InvitationId, attempt: u8) -> bool {
        let Some(exchange) = self.current.as_mut() else {
            return false;
        };
        if exchange.initiator || exchange.id != id {
            return false;
        }
        if exchange.phase == Phase::WaitingOffer && attempt == 0 {
            exchange.phase = Phase::Incoming;
            return true;
        }
        if exchange.phase == Phase::Exchanging && attempt == exchange.code_attempt + 1 {
            exchange.code_attempt = attempt;
            exchange.error = "Incorrect code. Try again.".to_string();
            exchange.phase = Phase::Incoming;
            exchange.proposal = None;
            return true;
        }
        false
    }

    /// Starts a fresh authentication exchange. The code itself is not checked here.
    pub fn submit_code(&mut self, id: InvitationId, code: Code) -> bool {
        let Some(exchange) = self.current.as_mut() else {
            return false;
        };
        if exchange.id != id || exchange.phase != Phase::Incoming || exchange.initiator {
            return false;
        }
        exchange.code = Some(code);
        exchange.error.clear();
        exchange.phase = Phase::Exchanging;
        true
    }

    /// Inviter-side rejection of the attempt currently in progress.
    /// A repeated delivery of that same attempt does not consume another one.
    pub fn auth_rejected(&mut self, id: InvitationId, attempt: u8) -> bool {
        let Some(exchange) = self.current.as_mut() else {
            return false;
        };
        if !exchange.initiator || exchange.id != id || exchange.phase != Phase::Inviting {
            return false;
        }
        if attempt != exchange.code_attempt {
            return false;
        }
        if u16::from(exchange.code_attempt) + 1 >= u16::from(ATTEMPT_LIMIT) {
            exchange.fail("Too many incorrect codes. Start a new invitation.");
            return true;
        }
        exchange.code_attempt += 1;
        true
    }

    /// Joiner-side notice that the attempt budget is exhausted.
    pub fn code_rejected(&mut self, id: InvitationId, attempt: u8) -> bool {
        let Some(exchange) = self.current.as_mut() else {
            return false;
        };
        if exchange.initiator || exchange.id != id || exchange.phase != Phase::Exchanging {
            return false;
        }
        if attempt != exchange.code_attempt {
            return false;
        }
        exchange.fail("Too many incorrect codes. Start a new invitation.");
        true
    }

    pub fn auth_accepted(&mut self, id: InvitationId, proposal: Proposal) -> bool {
        let Some(exchange) = self.current.as_mut() else {
            return false;
        };
        if exchange.id != id || !exchange.initiator || exchange.phase != Phase::Inviting {
            return false;
        }
        if proposal.members.is_empty() {
            return false;
        }
        exchange.code = None;
        exchange.proposal = Some(proposal);
        exchange.phase = Phase::WaitingAck;
        true
    }

    /// Joiner verified the grant and is about to persist. Not success yet.
    pub fn grant_accepted(&mut self, id: InvitationId, proposal: Proposal) -> bool {
        let Some(exchange) = self.current.as_mut() else {
            return false;
        };
        if exchange.id != id || exchange.initiator || exchange.phase != Phase::Exchanging {
            return false;
        }
        if exchange.proposal.is_some() {
            return false;
        }
        exchange.code = None;
        exchange.proposal = Some(proposal);
        true
    }

    pub fn fail(&mut self, id: InvitationId, message: &str) -> bool {
        let Some(exchange) = self.current.as_mut() else { return false };
        if exchange.id != id || !exchange.phase.active() {
            return false;
        }
        exchange.fail(message);
        true
    }

    pub fn note_store_failed(&mut self, id: InvitationId) {
        let Some(exchange) = self.current.as_mut() else {
            return;
        };
        if exchange.id != id {
            return;
        }
        exchange.fail("keyring write failed");
    }

    pub fn note_persisted(&mut self, id: InvitationId) -> bool {
        let Some(exchange) = self.current.as_mut() else {
            return false;
        };
        if exchange.id != id || exchange.proposal.is_none() {
            return false;
        }
        match (exchange.initiator, exchange.phase) {
            (true, Phase::WaitingAck) => exchange.phase = Phase::WaitingDone,
            (false, Phase::Exchanging) => exchange.phase = Phase::WaitingCommit,
            _ => return false,
        }
        exchange.proposal = None;
        true
    }

    pub fn note_confirmed(&mut self, id: InvitationId) -> bool {
        let Some(exchange) = self.current.as_mut() else {
            return false;
        };
        if exchange.id != id {
            return false;
        }
        let ready = matches!(
            (exchange.initiator, exchange.phase),
            (true, Phase::WaitingDone) | (false, Phase::WaitingCommit)
        );
        if !ready {
            return false;
        }
        exchange.proposal = None;
        exchange.code = None;
        exchange.phase = Phase::Success;
        true
    }

    pub fn cancel(&mut self, id: InvitationId) -> bool {
        let Some(exchange) = self.current.as_mut() else {
            return false;
        };
        if exchange.id != id || !exchange.phase.active() {
            return false;
        }
        exchange.code = None;
        exchange.proposal = None;
        exchange.phase = Phase::Cancelled;
        true
    }

    pub fn on_tick(&mut self, now: MonoMs) {
        let Some(exchange) = self.current.as_mut() else {
            return;
        };
        if exchange.phase.active() && now.0 >= exchange.expiry.0 {
            exchange.code = None;
            exchange.proposal = None;
            exchange.phase = Phase::Expired;
        }
    }
}

impl Default for Enrollment {
    fn default() -> Self {
        Self::new()
    }
}

impl Exchange {
    fn fail(&mut self, message: &str) {
        self.error = message.to_string();
        self.code = None;
        self.proposal = None;
        self.phase = Phase::Error;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::members::{GroupKey, Member};
    use crate::types::FleetId;

    fn addr(n: u8) -> Address {
        Address::new([n, 9, 8, 7, 6, 5]).unwrap()
    }

    fn proposal() -> Proposal {
        Proposal {
            id: FleetId::new([4; 16]),
            key: GroupKey::new([5; 32]),
            members: vec![
                Member { address: addr(1), label: "gram".into() },
                Member { address: addr(2), label: "xps".into() },
            ],
        }
    }

    #[test]
    fn five_wrong_codes_stop_and_a_replay_does_not_burn_two_attempts() {
        let mut enrollment = Enrollment::new();
        let id = InvitationId::new([9; 16]);
        let code = Code::parse("012345").unwrap();
        assert!(enrollment.begin_invite(addr(1), addr(2), "xps", code, id, MonoMs(0)));
        assert!(enrollment.mark_delivered(id));
        assert_eq!(enrollment.displayed_code().as_deref(), Some("012345"));
        assert!(enrollment.auth_rejected(id, 0));
        assert_eq!(enrollment.code_attempt(), 1);
        assert!(!enrollment.auth_rejected(id, 0));
        assert_eq!(enrollment.code_attempt(), 1);
        assert!(enrollment.auth_rejected(id, 1));
        assert!(enrollment.auth_rejected(id, 2));
        assert!(enrollment.auth_rejected(id, 3));
        assert_eq!(enrollment.phase(), Phase::Inviting);
        assert!(enrollment.auth_rejected(id, 4));
        assert_eq!(enrollment.phase(), Phase::Error);
        assert!(enrollment.proposal().is_none());
        assert!(enrollment.displayed_code().is_none());
    }

    #[test]
    fn a_wrong_code_then_the_right_one_stays_on_the_same_invitation() {
        let mut enrollment = Enrollment::new();
        let id = InvitationId::new([2; 16]);
        assert!(enrollment.receive_invite(addr(2), addr(1), "gram", id, MonoMs(0)));
        assert!(enrollment.offer(id, 0));
        assert_eq!(enrollment.phase(), Phase::Incoming);
        assert!(enrollment.submit_code(id, Code::parse("111111").unwrap()));
        assert_eq!(enrollment.phase(), Phase::Exchanging);
        assert!(enrollment.offer(id, 1));
        assert_eq!(enrollment.phase(), Phase::Incoming);
        assert_eq!(enrollment.code_attempt(), 1);
        assert_eq!(enrollment.error(), "Incorrect code. Try again.");
        assert!(enrollment.submit_code(id, Code::parse("012345").unwrap()));
        assert!(enrollment.grant_accepted(id, proposal()));
        assert_eq!(enrollment.phase(), Phase::Exchanging);
        assert!(!enrollment.note_confirmed(id));
        assert!(enrollment.note_persisted(id));
        assert_eq!(enrollment.phase(), Phase::WaitingCommit);
        assert!(enrollment.note_confirmed(id));
        assert_eq!(enrollment.phase(), Phase::Success);
    }

    #[test]
    fn cancel_drops_tentative_material_and_a_reverse_invite_can_start() {
        let mut enrollment = Enrollment::new();
        let id = InvitationId::new([1; 16]);
        assert!(enrollment.begin_invite(
            addr(1),
            addr(2),
            "xps",
            Code::parse("000001").unwrap(),
            id,
            MonoMs(0)
        ));
        assert!(!enrollment.receive_invite(
            addr(1),
            addr(2),
            "xps",
            InvitationId::new([3; 16]),
            MonoMs(10)
        ));
        assert!(enrollment.cancel(id));
        assert_eq!(enrollment.phase(), Phase::Cancelled);
        assert!(enrollment.proposal().is_none());
        let reverse = InvitationId::new([8; 16]);
        assert!(enrollment.receive_invite(addr(1), addr(2), "xps", reverse, MonoMs(20)));
        assert_eq!(enrollment.phase(), Phase::WaitingOffer);
    }

    #[test]
    fn success_requires_the_confirmation_and_expiry_discards_the_attempt() {
        let mut enrollment = Enrollment::new();
        let id = InvitationId::new([6; 16]);
        assert!(enrollment.begin_invite(
            addr(1),
            addr(2),
            "xps",
            Code::parse("000002").unwrap(),
            id,
            MonoMs(0)
        ));
        assert!(enrollment.mark_delivered(id));
        assert!(enrollment.auth_accepted(id, proposal()));
        assert_eq!(enrollment.phase(), Phase::WaitingAck);
        assert!(!enrollment.note_confirmed(id));
        assert!(enrollment.note_persisted(id));
        assert_eq!(enrollment.phase(), Phase::WaitingDone);
        assert_ne!(enrollment.phase(), Phase::Success);
        enrollment.on_tick(MonoMs(TTL_MS));
        assert_eq!(enrollment.phase(), Phase::Expired);
        assert!(enrollment.proposal().is_none());
    }
}
