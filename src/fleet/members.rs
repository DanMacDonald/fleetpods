//! Durable fleet identity. Enrollment proposes a change. This module accepts
//! or rejects it. Discovery cannot turn a sighting into a member.

use std::fmt;

use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::types::{valid_name, Address, FleetId};

pub const MEMBER_LIMIT: usize = 32;

/// 32-byte fleet group key. Debug and status never print it.
#[derive(Clone, PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
pub struct GroupKey([u8; 32]);

impl GroupKey {
    pub fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for GroupKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GroupKey([redacted])")
    }
}

/// Both accessory keys, or neither. A missing half stays missing.
#[derive(Clone, PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
pub struct AccessoryKeys {
    pub irk: [u8; 16],
    pub encryption: [u8; 16],
}

impl fmt::Debug for AccessoryKeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AccessoryKeys([redacted])")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    pub address: Address,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proposal {
    pub id: FleetId,
    pub key: GroupKey,
    pub members: Vec<Member>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Store {
    /// Not loaded yet. Not the same as an empty fleet.
    Unloaded,
    /// Keyring locked or unreadable. Must not be treated as empty.
    Unavailable,
    /// Confirmed absence, including a legacy fleet whose member list was empty.
    None,
    Open(Fleet),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fleet {
    pub id: FleetId,
    pub key: GroupKey,
    pub members: Vec<Member>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reject {
    Unavailable,
    ForeignFleet,
    KeyMismatch,
    Empty,
    Oversized,
    InvalidMember,
}

pub struct Membership {
    store: Store,
    accessory_keys: Option<AccessoryKeys>,
}

impl Membership {
    pub fn new() -> Self {
        Self { store: Store::Unloaded, accessory_keys: None }
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn fleet(&self) -> Option<&Fleet> {
        match &self.store {
            Store::Open(fleet) => Some(fleet),
            _ => None,
        }
    }

    pub fn contains(&self, address: Address) -> bool {
        self.fleet()
            .is_some_and(|fleet| fleet.members.iter().any(|member| member.address == address))
    }

    /// Update memory only after the caller has saved the reduced member list.
    pub(crate) fn remove(&mut self, address: Address) {
        if let Store::Open(fleet) = &mut self.store {
            fleet.members.retain(|member| member.address != address);
        }
    }

    pub fn accessory_keys_ready(&self) -> bool {
        self.accessory_keys.is_some()
    }

    pub fn accessory_keys(&self) -> Option<&AccessoryKeys> {
        self.accessory_keys.as_ref()
    }

    pub fn set_accessory_keys(&mut self, keys: AccessoryKeys) {
        self.accessory_keys = Some(keys);
    }

    pub fn mark_unavailable(&mut self) {
        self.store = Store::Unavailable;
    }

    /// The keyring opened after a locked start. Its record can be read again.
    pub fn mark_unlocked(&mut self) {
        if matches!(self.store, Store::Unavailable) {
            self.store = Store::Unloaded;
        }
    }

    /// A confirmed-empty legacy record may be dropped. A read failure may not.
    pub fn mark_confirmed_empty(&mut self) -> bool {
        if matches!(self.store, Store::Unavailable) {
            return false;
        }
        self.store = Store::None;
        true
    }

    /// Validate and merge before touching durable storage.
    pub fn prepare(&self, proposal: Proposal) -> Result<Fleet, Reject> {
        accept(&self.store, proposal)
    }

    pub fn apply(&mut self, proposal: Proposal) -> Result<&Fleet, Reject> {
        let fleet = self.prepare(proposal)?;
        self.store = Store::Open(fleet);
        match &self.store {
            Store::Open(fleet) => Ok(fleet),
            _ => unreachable!("just stored"),
        }
    }
}

impl Default for Membership {
    fn default() -> Self {
        Self::new()
    }
}

fn accept(store: &Store, proposal: Proposal) -> Result<Fleet, Reject> {
    if proposal.members.is_empty() {
        return Err(Reject::Empty);
    }
    if proposal.members.len() > MEMBER_LIMIT {
        return Err(Reject::Oversized);
    }
    if proposal.members.iter().any(|member| !valid_name(&member.label)) {
        return Err(Reject::InvalidMember);
    }
    match store {
        Store::Unavailable => Err(Reject::Unavailable),
        Store::Unloaded | Store::None => Ok(dedup(proposal)),
        Store::Open(current) if current.id != proposal.id => Err(Reject::ForeignFleet),
        Store::Open(current) if current.key != proposal.key => Err(Reject::KeyMismatch),
        Store::Open(current) => {
            let mut merged = current.clone();
            for member in proposal.members {
                upsert(&mut merged.members, member)?;
            }
            Ok(merged)
        }
    }
}

fn dedup(proposal: Proposal) -> Fleet {
    let mut members = Vec::new();
    for member in proposal.members {
        let _ = upsert(&mut members, member);
    }
    Fleet { id: proposal.id, key: proposal.key, members }
}

fn upsert(members: &mut Vec<Member>, member: Member) -> Result<(), Reject> {
    if let Some(existing) = members.iter_mut().find(|item| item.address == member.address) {
        existing.label = member.label;
        return Ok(());
    }
    if members.len() >= MEMBER_LIMIT {
        return Err(Reject::Oversized);
    }
    members.push(member);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(n: u8) -> Address {
        Address::new([n, 1, 2, 3, 4, 5]).unwrap()
    }

    fn proposal(id: u8, key: u8, members: Vec<Member>) -> Proposal {
        Proposal { id: FleetId::new([id; 16]), key: GroupKey::new([key; 32]), members }
    }

    #[test]
    fn unavailable_is_not_empty_and_a_foreign_fleet_does_not_overwrite() {
        let mut membership = Membership::new();
        membership.mark_unavailable();
        let first = proposal(1, 9, vec![Member { address: addr(1), label: "gram".into() }]);
        assert_eq!(membership.apply(first).err(), Some(Reject::Unavailable));
        assert!(matches!(membership.store(), Store::Unavailable));

        assert!(!membership.mark_confirmed_empty());
        assert!(matches!(membership.store(), Store::Unavailable));

        let mut open = Membership::new();
        open.apply(proposal(
            1,
            9,
            vec![
                Member { address: addr(1), label: "gram".into() },
                Member { address: addr(2), label: "xps".into() },
            ],
        ))
        .unwrap();
        let err =
            open.apply(proposal(2, 9, vec![Member { address: addr(3), label: "other".into() }]));
        assert_eq!(err.err(), Some(Reject::ForeignFleet));
        match open.store() {
            Store::Open(fleet) => assert_eq!(fleet.members.len(), 2),
            other => panic!("fleet replaced: {other:?}"),
        }
    }

    #[test]
    fn the_same_fleet_merges_once_and_a_different_key_does_not() {
        let mut membership = Membership::new();
        let members = vec![
            Member { address: addr(1), label: "gram".into() },
            Member { address: addr(2), label: "xps".into() },
        ];
        membership.apply(proposal(1, 9, members.clone())).unwrap();
        membership.apply(proposal(1, 9, members)).unwrap();
        match membership.store() {
            Store::Open(fleet) => assert_eq!(fleet.members.len(), 2),
            other => panic!("{other:?}"),
        }
        let err = membership.apply(proposal(
            1,
            8,
            vec![Member { address: addr(2), label: "xps".into() }],
        ));
        assert_eq!(err.err(), Some(Reject::KeyMismatch));
    }

    #[test]
    fn the_member_cap_is_enforced() {
        let mut members = Vec::new();
        for n in 1..=33 {
            members.push(Member { address: addr(n), label: format!("m{n}") });
        }
        let mut membership = Membership::new();
        assert_eq!(membership.apply(proposal(1, 1, members)).err(), Some(Reject::Oversized));
    }

    #[test]
    fn debug_does_not_include_key_bytes() {
        let key = GroupKey::new([0xab; 32]);
        assert!(!format!("{key:?}").contains("ab"));
    }
}
