//! The enrollment commit transaction.
//!
//! One proposal, one pending letter, one keyring write. The proposal is not
//! membership until the write returns; this module owns that interval so the
//! staging, the stored reply, and the failure reply cannot drift apart.

use super::members::{Fleet, Proposal};
use crate::types::{Address, InvitationId};

/// A fleet the keyring has not accepted yet. Membership stays unchanged.
pub enum Pending {
    Grant { to: Address, id: InvitationId, confirm: [u8; 32], proposal: Proposal },
    Saved { to: Address, id: InvitationId, proposal: Proposal },
}

impl Pending {
    pub fn id(&self) -> InvitationId {
        match self {
            Self::Grant { id, .. } | Self::Saved { id, .. } => *id,
        }
    }

    pub fn fleet(&self) -> Fleet {
        let proposal = match self {
            Self::Grant { proposal, .. } | Self::Saved { proposal, .. } => proposal,
        };
        Fleet { id: proposal.id, key: proposal.key.clone(), members: proposal.members.clone() }
    }
}

/// At most one commit is ever staged: a second staging replaces nothing
/// silently because callers clear or take the first one before staging again.
#[derive(Default)]
pub struct Commit {
    pending: Option<Pending>,
}

impl Commit {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn stage_grant(
        &mut self,
        to: Address,
        id: InvitationId,
        confirm: [u8; 32],
        proposal: Proposal,
    ) {
        self.pending = Some(Pending::Grant { to, id, confirm, proposal });
    }

    pub fn stage_saved(&mut self, to: Address, id: InvitationId, proposal: Proposal) {
        self.pending = Some(Pending::Saved { to, id, proposal });
    }

    pub fn is_staged(&self) -> bool {
        self.pending.is_some()
    }

    /// The fleet a staged commit would write, for the keyring effect.
    pub fn fleet(&self) -> Option<Fleet> {
        self.pending.as_ref().map(Pending::fleet)
    }

    pub fn clear(&mut self) {
        self.pending = None;
    }

    pub fn take(&mut self) -> Option<Pending> {
        self.pending.take()
    }
}
