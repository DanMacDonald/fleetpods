//! Enrollment conversation. `daemon` decides to invite or cancel. This module
//! turns the letters into one SPAKE2 attempt and one membership commit.

use crate::fleet::crypto::Pake;
use crate::fleet::letter::Letter;
use crate::fleet::members::{Member, Proposal, Reject};
use crate::fleet::wire::enrollment_context;
use crate::linux::os;
use crate::types::{Address, Clock, FleetId, InvitationId};

use crate::daemon::Daemon;

impl Daemon {
    pub(crate) fn deliver(&mut self, from: Address, letter: Letter, now: Clock) {
        match letter {
            Letter::JoinRequest { id, name } => {
                if self.enrollment.id() == Some(id)
                    && self.enrollment.peer().map(|(peer, _)| peer) == Some(from)
                {
                    return;
                }
                if self.keyring_locked
                    || self.membership.fleet().is_none()
                    || self.enrollment.phase().active()
                {
                    self.send(from, Letter::Cancel { id });
                    return;
                }
                if let Ok(code) = crate::types::code::from_os() {
                    if self.invite(from, &name, code, id, now) {
                        self.pake = None;
                        self.offer = None;
                        self.send(from, Letter::Hello { id, name: self.local_name.clone() });
                    }
                }
            }

            Letter::Hello { id, name } => {
                let requested = !self.enrollment.initiator()
                    && self.enrollment.id() == Some(id)
                    && self.enrollment.peer().map(|(peer, _)| peer) == Some(from)
                    && self.enrollment.phase() == crate::fleet::enroll::Phase::WaitingOffer;
                if requested
                    || self.enrollment.receive_invite(
                        self.local_address.unwrap_or(from),
                        from,
                        &name,
                        id,
                        now.mono,
                    )
                {
                    self.send(from, Letter::Ack { id });
                }
            }
            Letter::Ack { id } => {
                if self.enrollment.mark_delivered(id) {
                    self.send_offer();
                }
            }
            Letter::Offer { id, attempt, public } => {
                if self.enrollment.offer(id, attempt) {
                    self.offer = Some(public);
                }
            }
            Letter::Response { id, attempt, public, confirm } => {
                self.on_response(id, attempt, &public, confirm);
            }
            Letter::Grant { id, confirm, proposal } => self.on_grant(from, id, confirm, proposal),
            Letter::Saved { id, .. } => {
                // The joiner has stored the key and is waiting for commit.
                if self.enrollment.initiator()
                    && self.enrollment.id() == Some(id)
                    && self.enrollment.phase() == crate::fleet::enroll::Phase::WaitingDone
                {
                    self.send(from, Letter::Commit { id });
                }
            }
            Letter::Commit { id } => {
                if !self.enrollment.initiator()
                    && self.enrollment.id() == Some(id)
                    && self.enrollment.phase() == crate::fleet::enroll::Phase::WaitingCommit
                {
                    self.send(from, Letter::Done { id });
                    self.enrollment.note_confirmed(id);
                }
            }
            Letter::Done { id } => {
                self.enrollment.note_confirmed(id);
            }
            Letter::Cancel { id } => {
                if self.enrollment.id() != Some(id)
                    || self.enrollment.peer().map(|(peer, _)| peer) != Some(from)
                    || !self.enrollment.phase().active()
                {
                    return;
                }
                if self.enrollment.phase().active()
                    && matches!(
                        self.enrollment.phase(),
                        crate::fleet::enroll::Phase::Exchanging
                            | crate::fleet::enroll::Phase::WaitingAck
                            | crate::fleet::enroll::Phase::WaitingCommit
                            | crate::fleet::enroll::Phase::WaitingDone
                    )
                {
                    self.enrollment.fail(id, "The other device could not finish fleet setup. Check its message before trying again.");
                } else {
                    self.enrollment.cancel(id);
                }
                self.commit.clear();
                self.keyring_dirty = false;
                self.pake = None;
                self.offer = None;
            }
            Letter::Rejected { id, attempt } => {
                self.enrollment.code_rejected(id, attempt);
                self.pake = None;
                self.offer = None;
            }
        }
    }

    pub(crate) fn answer_submit(&mut self) {
        let (Some(offer), Some(code), Some(id), Some((peer, _))) = (
            self.offer.clone(),
            self.enrollment.password(),
            self.enrollment.id(),
            self.enrollment.peer(),
        ) else {
            return;
        };
        let context = self.pake_context();
        let (mut pake, public) = Pake::start(false, code.as_bytes(), &context);
        if !pake.finish(&offer) {
            return;
        }
        let Some(confirm) = pake.confirm() else {
            return;
        };
        let attempt = self.enrollment.code_attempt();
        self.pake = Some(pake);
        self.send(peer, Letter::Response { id, attempt, public, confirm });
    }

    fn on_response(&mut self, id: InvitationId, attempt: u8, public: &[u8], confirm: [u8; 32]) {
        let Some(pake) = self.pake.as_mut() else {
            return;
        };
        if !pake.finish(public) || !pake.verify(&confirm) {
            let rejected = self.enrollment.auth_rejected(id, attempt);
            self.pake = None;
            if !rejected {
                return;
            }
            if self.enrollment.phase() == crate::fleet::enroll::Phase::Error {
                if let Some((peer, _)) = self.enrollment.peer() {
                    self.send(peer, Letter::Rejected { id, attempt });
                }
            } else {
                self.send_offer();
            }
            return;
        }
        let Some(proposal) = self.proposal_for_peer() else {
            return;
        };
        let Some(proposal) = self.prepare_enrollment(id, proposal) else { return };
        let Some(tag) = self.pake.as_ref().and_then(Pake::confirm) else {
            return;
        };
        if !self.enrollment.auth_accepted(id, proposal.clone()) {
            return;
        }
        let Some((peer, _)) = self.enrollment.peer() else {
            return;
        };
        self.commit.stage_grant(peer, id, tag, proposal);
        self.keyring_dirty = true;
    }

    fn on_grant(&mut self, from: Address, id: InvitationId, confirm: [u8; 32], proposal: Proposal) {
        let verified = self.pake.as_ref().is_some_and(|pake| pake.verify(&confirm));
        if !verified || !self.enrollment.grant_accepted(id, proposal.clone()) {
            return;
        }
        let Some(proposal) = self.prepare_enrollment(id, proposal) else { return };
        self.commit.stage_saved(from, id, proposal);
        self.keyring_dirty = true;
    }

    fn prepare_enrollment(&mut self, id: InvitationId, proposal: Proposal) -> Option<Proposal> {
        match self.membership.prepare(proposal) {
            Ok(fleet) => Some(Proposal { id: fleet.id, key: fleet.key, members: fleet.members }),
            Err(reject) => {
                self.reject_enrollment(id, reject);
                None
            }
        }
    }

    pub(crate) fn reject_enrollment(&mut self, id: InvitationId, reject: Reject) {
        let message = match reject {
            Reject::ForeignFleet | Reject::KeyMismatch => "This device already belongs to a different fleet. Reset its fleet before joining this one. Its saved fleet was not replaced.",
            Reject::Unavailable => "The keyring is unavailable. Unlock it before joining a fleet.",
            _ => "The proposed fleet membership is invalid. Its saved fleet was not replaced.",
        };
        if self.enrollment.fail(id, message) {
            self.commit.clear();
            self.keyring_dirty = false;
            if let Some((peer, _)) = self.enrollment.peer() {
                self.send(peer, Letter::Cancel { id });
            }
            eprintln!("fleetpodsd: fleet enrollment rejected: {reject:?}");
        }
    }

    fn send_offer(&mut self) {
        let (Some(code), Some(id), Some((peer, _))) =
            (self.enrollment.password(), self.enrollment.id(), self.enrollment.peer())
        else {
            return;
        };
        let context = self.pake_context();
        let (pake, public) = Pake::start(true, code.as_bytes(), &context);
        let attempt = self.enrollment.code_attempt();
        self.pake = Some(pake);
        self.send(peer, Letter::Offer { id, attempt, public });
    }

    fn proposal_for_peer(&self) -> Option<Proposal> {
        let (peer, peer_name) = self.enrollment.peer()?;
        let local = self.local_address?;
        if let Some(fleet) = self.membership.fleet() {
            let mut members = fleet.members.clone();
            upsert_member(&mut members, Member { address: local, label: self.local_name.clone() });
            upsert_member(&mut members, Member { address: peer, label: peer_name.to_string() });
            return Some(Proposal { id: fleet.id, key: fleet.key.clone(), members });
        }
        let mut id = [0u8; 16];
        let mut key = [0u8; 32];
        os::random_bytes(&mut id).ok()?;
        os::random_bytes(&mut key).ok()?;
        Some(Proposal {
            id: FleetId::new(id),
            key: crate::fleet::members::GroupKey::new(key),
            members: vec![
                Member { address: local, label: self.local_name.clone() },
                Member { address: peer, label: peer_name.to_string() },
            ],
        })
    }

    fn pake_context(&self) -> Vec<u8> {
        let Some((peer, peer_name)) = self.enrollment.peer() else {
            return Vec::new();
        };
        let Some(local) = self.local_address else {
            return Vec::new();
        };
        let Some(id) = self.enrollment.id() else {
            return Vec::new();
        };
        enrollment_context(
            id,
            self.enrollment.initiator(),
            local,
            peer,
            &self.local_name,
            peer_name,
            self.enrollment.code_attempt(),
        )
    }

    pub(crate) fn send(&mut self, to: Address, body: Letter) {
        self.outbox.push(crate::fleet::letter::Outbound { to, body });
    }
}

fn upsert_member(members: &mut Vec<Member>, member: Member) {
    if let Some(existing) = members.iter_mut().find(|item| item.address == member.address) {
        existing.label = member.label;
    } else {
        members.push(member);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::enroll::Phase;
    use crate::fleet::members::{Fleet, GroupKey};
    use crate::step::{Effect, Input};
    use crate::types::Code;

    fn address(n: u8) -> Address {
        Address::new([n, 2, 3, 4, 5, 6]).unwrap()
    }
    fn fleet(id: u8, key: u8) -> Fleet {
        Fleet {
            id: FleetId::new([id; 16]),
            key: GroupKey::new([key; 32]),
            members: vec![Member { address: address(2), label: "gram".into() }],
        }
    }
    fn transfer(from: &mut Daemon, to: &mut Daemon, now: Clock) {
        for letter in from.take_letters() {
            to.deliver(from.local_address.unwrap(), letter.body, now);
        }
    }
    fn authenticated_pair(joiner_fleet: Option<Fleet>) -> (Daemon, Daemon, Clock) {
        let now = Clock::new(1000, 1000);
        let mut inviter = Daemon::new("xps");
        let mut joiner = Daemon::new("gram");
        inviter.set_local_address(Some(address(1)));
        joiner.set_local_address(Some(address(2)));
        inviter.restore_fleet(fleet(1, 1));
        if let Some(fleet) = joiner_fleet {
            joiner.restore_fleet(fleet);
        }
        let id = InvitationId::new([5; 16]);
        let code = Code::parse("123456").unwrap();
        assert!(inviter.invite(address(2), "gram", code.clone(), id, now));
        assert!(joiner.receive_invite(address(1), "xps", id, now));
        assert!(inviter.deliver_invite(id));
        inviter.send_offer();
        transfer(&mut inviter, &mut joiner, now);
        assert!(joiner.enrollment.submit_code(id, code));
        joiner.answer_submit();
        transfer(&mut joiner, &mut inviter, now);
        assert!(inviter.commit.is_staged());
        // The inviter's write is complete before its grant is sent.
        inviter.persist_enrollment(id).unwrap();
        let crate::fleet::commit::Pending::Grant { to, id, confirm, proposal } =
            inviter.commit.take().unwrap()
        else {
            panic!("expected grant")
        };
        inviter.keyring_dirty = false;
        inviter.send(to, Letter::Grant { id, confirm, proposal });
        transfer(&mut inviter, &mut joiner, now);
        (inviter, joiner, now)
    }

    #[test]
    fn join_request_uses_the_existing_peers_fleet_and_requires_its_code() {
        let now = Clock::new(1000, 1000);
        let mut xps = Daemon::new("xps");
        let mut gram = Daemon::new("gram");
        xps.set_local_address(Some(address(1)));
        gram.set_local_address(Some(address(2)));
        let original = fleet(9, 8);
        gram.restore_fleet(original.clone());
        assert!(xps.join_os(address(2), "gram", now));
        // Exercise the new on-air request's encoding as well as the roles.
        let request = xps.take_letters().pop().unwrap();
        let bytes =
            crate::fleet::cppmsg::encode(address(1), address(2), "xps", &request.body, None)
                .unwrap();
        gram.deliver(address(1), crate::fleet::cppmsg::decode(&bytes, None).unwrap(), now);
        transfer(&mut gram, &mut xps, now);
        transfer(&mut xps, &mut gram, now);
        transfer(&mut gram, &mut xps, now);
        assert!(gram.enrollment.initiator());
        assert!(!xps.enrollment.initiator());
        assert_eq!(xps.enrollment.phase(), Phase::Incoming);
        assert!(xps.saved_fleet().is_none());
        assert_eq!(gram.saved_fleet(), Some(original.clone()));
        let code = Code::parse(&gram.enrollment.displayed_code().unwrap()).unwrap();
        assert!(xps.enrollment.submit_code(xps.enrollment.id().unwrap(), code));
        xps.answer_submit();
        transfer(&mut xps, &mut gram, now);
        let proposed = gram.commit.fleet().unwrap();
        assert_eq!(proposed.id, original.id);
        assert_eq!(proposed.key, original.key);
        assert!(proposed.members.iter().any(|member| member.address == address(1)));
    }

    #[test]
    fn joining_without_leaving_never_changes_an_existing_fleet() {
        let mut xps = Daemon::new("xps");
        xps.set_local_address(Some(address(1)));
        let original = fleet(9, 8);
        xps.restore_fleet(original.clone());
        assert!(!xps.join_os(address(2), "gram", Clock::new(1, 1)));
        assert_eq!(xps.saved_fleet(), Some(original));
        assert!(xps.take_letters().is_empty());
    }

    #[test]
    fn conflicting_fleet_fails_before_any_keyring_write_and_notifies_inviter() {
        for original in [fleet(2, 2), fleet(1, 2)] {
            let (mut inviter, mut joiner, now) = authenticated_pair(Some(original.clone()));
            assert_eq!(joiner.enrollment.phase(), Phase::Error);
            assert!(joiner.enrollment.error().contains("different fleet"));
            assert_eq!(joiner.saved_fleet(), Some(original));
            assert!(!joiner.commit.is_staged());
            assert!(!joiner.keyring_dirty);
            transfer(&mut joiner, &mut inviter, now);
            assert_eq!(inviter.enrollment.phase(), Phase::Error);
            assert!(inviter.enrollment.error().contains("other device"));
            let result = joiner.step(Input::Tick, now);
            assert!(!result.effects.iter().any(|e| matches!(e, Effect::WriteKeyring(_))));
        }
    }

    #[test]
    fn empty_and_same_fleet_joiners_still_finish_after_persistence_and_confirmation() {
        for previous in [None, Some(fleet(1, 1))] {
            let (mut inviter, mut joiner, now) = authenticated_pair(previous);
            assert_eq!(joiner.enrollment.phase(), Phase::Exchanging);
            let effects = joiner.step(Input::Tick, now).effects;
            assert!(effects.iter().any(|e| matches!(e, Effect::WriteKeyring(_))));
            let saved = joiner.step(Input::KeyringStored, now);
            assert_eq!(joiner.enrollment.phase(), Phase::WaitingCommit);
            // Decode the real encrypted completion message, not an invented ack.
            for effect in saved.effects {
                if let Effect::Send { bytes, .. } = effect {
                    let letter =
                        crate::fleet::cppmsg::decode(&bytes, inviter.pake.as_ref()).unwrap();
                    inviter.deliver(address(2), letter, now);
                }
            }
            transfer(&mut inviter, &mut joiner, now);
            assert_eq!(joiner.enrollment.phase(), Phase::Success);
            transfer(&mut joiner, &mut inviter, now);
            assert_eq!(inviter.enrollment.phase(), Phase::Success);
            assert_eq!(joiner.saved_fleet(), inviter.saved_fleet());
        }
    }
}
