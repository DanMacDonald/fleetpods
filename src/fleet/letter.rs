//! Enrollment messages exchanged over the fleet radio.
//! The six-digit code is not one of them.

use crate::types::{Address, InvitationId};

use super::members::{Member, Proposal};

#[derive(Debug)]
pub struct Outbound {
    pub to: Address,
    pub body: Letter,
}

#[derive(Debug)]
pub enum Letter {
    JoinRequest { id: InvitationId, name: String },
    Hello { id: InvitationId, name: String },
    Ack { id: InvitationId },
    Offer { id: InvitationId, attempt: u8, public: Vec<u8> },
    Response { id: InvitationId, attempt: u8, public: Vec<u8>, confirm: [u8; 32] },
    Grant { id: InvitationId, confirm: [u8; 32], proposal: Proposal },
    Saved { id: InvitationId, members: Vec<Member> },
    Commit { id: InvitationId },
    Done { id: InvitationId },
    Cancel { id: InvitationId },
    Rejected { id: InvitationId, attempt: u8 },
}
