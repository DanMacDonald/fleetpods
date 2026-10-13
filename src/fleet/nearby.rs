//! Nearby hosts seen on public discovery. Membership is a different fact:
//! a sighting never becomes a fleet member here.
//!
//! A record keeps the unix time from the announcement. That clock ages the
//! host, including across suspend. Sixty-four peers is the cap.

use crate::types::{Address, HostId, UnixMs};

/// How long an announcement stays nearby. The same bound rejects a stale frame.
pub const FRESH_MS: u64 = 30_000;
const MAX_HOSTS: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Nearby {
    pub host: HostId,
    pub address: Address,
    pub name: String,
    /// Timestamp on the latest accepted announcement. Replay order uses this.
    pub sent: UnixMs,
    /// Expiry clock. A sealed frame may carry an older timestamp than the
    /// public one it replaces, and that does not shorten the host's life.
    pub fresh: UnixMs,
    /// Sealed with the fleet key. A later public frame does not replace it.
    pub authenticated: bool,
    pub session: Option<[u8; 16]>,
    /// An unauthenticated UI hint, never authority to replace a key.
    pub enrolled: Option<bool>,
}

pub struct Discovery {
    hosts: Vec<Nearby>,
}

impl Discovery {
    pub fn new() -> Self {
        Self { hosts: Vec::new() }
    }

    /// A rotating address updates the existing host. An older timestamp of the
    /// same kind does not. A public frame does not replace a sealed one.
    /// Returns whether the list changed.
    pub fn observe(
        &mut self,
        host: HostId,
        address: Address,
        name: impl Into<String>,
        sent: UnixMs,
        authenticated: bool,
        session: Option<[u8; 16]>,
    ) -> bool {
        let name = name.into();
        if let Some(existing) = self.hosts.iter_mut().find(|item| item.host == host) {
            if existing.authenticated && !authenticated {
                return false;
            }
            if existing.authenticated == authenticated && sent <= existing.sent {
                return false;
            }
            existing.address = address;
            existing.name = name;
            existing.fresh = existing.fresh.max(sent);
            existing.sent = sent;
            existing.authenticated = authenticated;
            existing.session = session;
            return true;
        }
        if self.hosts.len() >= MAX_HOSTS && !self.make_room(sent, authenticated) {
            return false;
        }
        self.hosts.push(Nearby {
            host,
            address,
            name,
            sent,
            fresh: sent,
            authenticated,
            session,
            enrolled: authenticated.then_some(true),
        });
        true
    }

    pub fn observe_presence(&mut self, sight: super::presence::Sight, host: HostId) {
        if self.observe(
            host,
            sight.address,
            sight.name,
            sight.sent,
            sight.authenticated,
            sight.session,
        ) {
            if let Some(peer) = self.hosts.iter_mut().find(|peer| peer.host == host) {
                peer.enrolled = sight.enrolled;
            }
        }
    }

    /// A public arrival takes an unauthenticated slot. It does not remove a
    /// sealed host. A sealed arrival may, once no public slot is left.
    fn make_room(&mut self, sent: UnixMs, authenticated: bool) -> bool {
        let unauthenticated = self
            .hosts
            .iter()
            .enumerate()
            .filter(|(_, host)| !host.authenticated)
            .min_by_key(|(_, host)| host.sent)
            .map(|(index, host)| (index, host.sent));
        if let Some((index, oldest)) = unauthenticated {
            if !authenticated && sent <= oldest {
                return false;
            }
            self.hosts.remove(index);
            return true;
        }
        if !authenticated {
            return false;
        }
        let Some((index, oldest)) = self
            .hosts
            .iter()
            .enumerate()
            .min_by_key(|(_, host)| host.sent)
            .map(|(index, host)| (index, host.sent))
        else {
            return false;
        };
        if sent <= oldest {
            return false;
        }
        self.hosts.remove(index);
        true
    }

    pub fn expire(&mut self, now: UnixMs) {
        self.hosts.retain(|host| host.fresh.0.saturating_add(FRESH_MS) >= now.0);
    }

    /// Authenticated traffic from an already discovered session is also proof
    /// of presence. Use its signed timestamp, never receipt time: replaying a
    /// packet cannot extend the sender's lifetime beyond the original window.
    /// This cannot introduce a host, promote public discovery, or change sessions.
    pub fn refresh_session(&mut self, address: Address, session: [u8; 16], sent: UnixMs) -> bool {
        let Some(host) = self.hosts.iter_mut().find(|host| {
            host.address == address && host.authenticated && host.session == Some(session)
        }) else {
            return false;
        };
        host.fresh = host.fresh.max(sent);
        true
    }

    pub fn hosts(&self) -> &[Nearby] {
        &self.hosts
    }

    pub(crate) fn forget(&mut self, address: Address) {
        self.hosts.retain(|host| host.address != address);
    }
}

impl Default for Discovery {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(byte: u8) -> Address {
        Address::new([byte, 0, 0, 0, 0, 1]).unwrap()
    }

    #[test]
    fn a_rotating_address_keeps_one_host_and_expiry_removes_it() {
        let mut discovery = Discovery::new();
        let host = HostId::new([1; 16]);
        let first = Address::parse("10:00:00:00:00:01").unwrap();
        let second = Address::parse("10:00:00:00:00:02").unwrap();
        assert!(discovery.observe(host, first, "gram", UnixMs(1_000), false, None));
        assert!(discovery.observe(host, second, "gram", UnixMs(2_000), false, None));
        assert!(!discovery.observe(host, first, "old", UnixMs(1_500), false, None));
        assert_eq!(discovery.hosts().len(), 1);
        assert_eq!(discovery.hosts()[0].address, second);
        assert_eq!(discovery.hosts()[0].name, "gram");
        discovery.expire(UnixMs(2_000 + FRESH_MS));
        assert_eq!(discovery.hosts().len(), 1);
        discovery.expire(UnixMs(2_000 + FRESH_MS + 1));
        assert!(discovery.hosts().is_empty());
    }

    #[test]
    fn authenticated_traffic_keeps_only_its_known_session_fresh() {
        let mut discovery = Discovery::new();
        let host = HostId::new([1; 16]);
        let session = [4; 16];
        assert!(!discovery.refresh_session(addr(1), session, UnixMs(20_000)));
        discovery.observe(host, addr(1), "public", UnixMs(1_000), false, None);
        assert!(!discovery.refresh_session(addr(1), session, UnixMs(20_000)));
        discovery.observe(host, addr(1), "gram", UnixMs(2_000), true, Some(session));
        assert!(!discovery.refresh_session(addr(1), [5; 16], UnixMs(20_000)));
        assert!(!discovery.refresh_session(addr(2), session, UnixMs(20_000)));
        assert!(discovery.refresh_session(addr(1), session, UnixMs(20_000)));
        discovery.expire(UnixMs(32_001));
        assert_eq!(discovery.hosts().len(), 1, "live traffic outlasts the old presence frame");
        // Receiving the same signed packet again cannot move the expiry.
        assert!(discovery.refresh_session(addr(1), session, UnixMs(20_000)));
        assert!(discovery.refresh_session(addr(1), session, UnixMs(10_000)));
        let known = &discovery.hosts()[0];
        assert_eq!(known.name, "gram");
        assert_eq!(known.sent, UnixMs(2_000));
        assert_eq!(known.fresh, UnixMs(20_000));
        discovery.expire(UnixMs(50_001));
        assert!(discovery.hosts().is_empty());
        assert!(!discovery.refresh_session(addr(1), session, UnixMs(50_001)));
    }

    #[test]
    fn a_public_frame_does_not_replace_a_sealed_host() {
        let mut discovery = Discovery::new();
        let host = HostId::new([1; 16]);
        assert!(discovery.observe(host, addr(1), "sealed", UnixMs(1_000), true, Some([4; 16])));
        assert!(!discovery.observe(host, addr(1), "public", UnixMs(9_000), false, None));
        assert_eq!(discovery.hosts()[0].name, "sealed");
        assert_eq!(discovery.hosts()[0].session, Some([4; 16]));
    }

    #[test]
    fn a_sealed_frame_keeps_the_later_expiry_and_a_public_one_does_not_evict_it() {
        let mut discovery = Discovery::new();
        let host = HostId::new([1; 16]);
        assert!(discovery.observe(host, addr(1), "public", UnixMs(5_000), false, None));
        assert!(discovery.observe(host, addr(1), "sealed", UnixMs(1_000), true, None));
        assert_eq!(discovery.hosts()[0].name, "sealed");
        assert!(discovery.observe(host, addr(1), "later", UnixMs(2_000), true, None));
        assert_eq!(discovery.hosts()[0].name, "later");
        discovery.expire(UnixMs(5_000 + FRESH_MS));
        assert_eq!(discovery.hosts().len(), 1);
        discovery.expire(UnixMs(5_000 + FRESH_MS + 1));
        assert!(discovery.hosts().is_empty());

        let mut full = Discovery::new();
        for byte in 1..=MAX_HOSTS as u8 {
            assert!(full.observe(
                HostId::new([byte; 16]),
                addr(byte),
                "peer",
                UnixMs(u64::from(byte)),
                byte != 1,
                None,
            ));
        }
        assert!(full.observe(HostId::new([90; 16]), addr(90), "new", UnixMs(1_000), false, None));
        assert!(full.hosts().iter().all(|host| host.address != addr(1)));
        assert!(full.hosts().iter().any(|host| host.address == addr(2) && host.authenticated));

        let mut sealed = Discovery::new();
        for byte in 1..=MAX_HOSTS as u8 {
            assert!(sealed.observe(
                HostId::new([byte; 16]),
                addr(byte),
                "peer",
                UnixMs(u64::from(byte)),
                true,
                None,
            ));
        }
        assert!(!sealed.observe(
            HostId::new([90; 16]),
            addr(90),
            "new",
            UnixMs(10_000),
            false,
            None,
        ));
        assert_eq!(sealed.hosts().len(), MAX_HOSTS);
    }

    #[test]
    fn the_oldest_host_is_dropped_when_the_list_is_full() {
        let mut discovery = Discovery::new();
        for byte in 1..=MAX_HOSTS as u8 {
            assert!(discovery.observe(
                HostId::new([byte; 16]),
                addr(byte),
                "peer",
                UnixMs(u64::from(byte)),
                false,
                None,
            ));
        }
        assert_eq!(discovery.hosts().len(), MAX_HOSTS);
        assert!(discovery.observe(
            HostId::new([90; 16]),
            addr(90),
            "new",
            UnixMs(1_000),
            false,
            None
        ));
        assert_eq!(discovery.hosts().len(), MAX_HOSTS);
        assert!(discovery.hosts().iter().all(|host| host.address != addr(1)));
        assert!(discovery.hosts().iter().any(|host| host.address == addr(90)));
    }
}
