//! Authenticated fleet reports. A report stays tied to its reporter, accessory,
//! and session. It does not overwrite a direct accessory observation.

use std::collections::{BTreeMap, HashMap};

use crate::types::{Address, Clock, HostId, MonoMs, SessionId, UnixMs};

pub const DEFAULT_TTL_MS: u64 = 30_000;

/// Complete a pod list that names only this laptop. When two laptops
/// connect at the same moment, the pods can tell each of them it is alone
/// and never send a corrected list; the other laptop's fresh roster,
/// naming only itself, is the missing half. A report naming anyone else
/// (or no reporter) does not complete the list.
pub fn complete_solo_list(
    listed: &[Address],
    local: Address,
    reporter: Option<Address>,
    report: &[Address],
) -> Vec<Address> {
    if listed.len() != 1 || listed[0] != local {
        return listed.to_vec();
    }
    let Some(reporter) = reporter.filter(|reporter| *reporter != local) else {
        return listed.to_vec();
    };
    if report.contains(&reporter) && report.iter().all(|peer| *peer == local || *peer == reporter) {
        // Join order: the reporter was on the pods first unless it already
        // lists this laptop, in which case both keep its order.
        return if report.contains(&local) { report.to_vec() } else { vec![reporter, local] };
    }
    listed.to_vec()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RosterReport {
    pub reporter: HostId,
    pub reporter_address: Address,
    pub accessory: Address,
    pub session: SessionId,
    pub seq: u64,
    pub at: MonoMs,
    pub sent: UnixMs,
    pub peers: Vec<Address>,
    pub audio_source: Option<Address>,
    pub pending: bool,
    pub audio_idle: bool,
    /// Air session, when this report came from a roster advertisement.
    pub wire_session: Option<[u8; 16]>,
    pub last_use: UnixMs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ingest {
    Stored,
    Replay,
    Mismatched,
}

#[derive(Clone, Copy)]
struct AirMark {
    session: [u8; 16],
    seq: u64,
    sent: UnixMs,
}

pub struct Peers {
    reports: Vec<RosterReport>,
    high_water: BTreeMap<(HostId, SessionId), u64>,
    air_seen: HashMap<Address, AirMark>,
    ttl_ms: u64,
}

impl Peers {
    pub fn new() -> Self {
        Self {
            reports: Vec::new(),
            high_water: BTreeMap::new(),
            air_seen: HashMap::new(),
            ttl_ms: DEFAULT_TTL_MS,
        }
    }

    pub fn ingest(&mut self, report: RosterReport, configured: Option<Address>) -> Ingest {
        if report.seq == 0 {
            return Ingest::Replay;
        }
        let key = (report.reporter, report.session);
        if self.high_water.get(&key).is_some_and(|prev| report.seq <= *prev) {
            return Ingest::Replay;
        }
        let mismatched = configured.is_some_and(|accessory| accessory != report.accessory);
        self.high_water.insert(key, report.seq);
        if mismatched {
            return Ingest::Mismatched;
        }
        let reporter = report.reporter_address;
        self.reports.push(report);
        self.cap_reports(reporter);
        Ingest::Stored
    }

    /// Only the freshest few reports per reporter are ever consulted
    /// (`fresh` scans from the newest), so older ones are dead weight.
    /// Without a cap, a fleet insider flooding authenticated rosters at
    /// radio cadence grows this Vec for the life of the process.
    fn cap_reports(&mut self, reporter: Address) {
        const KEEP_PER_REPORTER: usize = 4;
        let total = self.reports.iter().filter(|r| r.reporter_address == reporter).count();
        if total <= KEEP_PER_REPORTER {
            return;
        }
        // Reports arrive oldest-first per reporter, so drop from the front.
        let mut drop = total - KEEP_PER_REPORTER;
        self.reports.retain(|report| {
            if drop > 0 && report.reporter_address == reporter {
                drop -= 1;
                return false;
            }
            true
        });
    }

    /// Newest fresh report for this accessory. An air roster expires 15 s after
    /// its own timestamp. An in-process report uses the monotonic window.
    pub fn fresh(&self, now: Clock, accessory: Address) -> Option<&RosterReport> {
        self.reports.iter().rev().find(|report| {
            if report.accessory != accessory {
                return false;
            }
            if report.wire_session.is_some() {
                super::roster_air::fresh(report.sent, now.unix)
            } else {
                // A report dated after the receiving clock is not fresh;
                // otherwise a forged future timestamp never expires.
                report.at <= now.mono && now.mono.saturating_since(report.at) <= self.ttl_ms
            }
        })
    }

    /// A helper's own current report determines whether it can release a slot.
    pub fn fresh_from(
        &self,
        now: Clock,
        accessory: Address,
        reporter: Address,
    ) -> Option<&RosterReport> {
        self.reports.iter().rev().find(|report| {
            report.accessory == accessory
                && report.reporter_address == reporter
                && if report.wire_session.is_some() {
                    super::roster_air::fresh(report.sent, now.unix)
                } else {
                    report.at <= now.mono && now.mono.saturating_since(report.at) <= self.ttl_ms
                }
        })
    }

    /// Drop in-memory replay state. A restarted daemon still needs a persisted
    /// watermark before an old packet with a fresh timestamp can be rejected.
    pub fn invalidate(&mut self) {
        self.reports.clear();
        self.high_water.clear();
        self.air_seen.clear();
    }

    /// Retain replay watermarks, but stop trusting this host's cached reports.
    pub(crate) fn forget(&mut self, address: Address) {
        self.reports.retain(|report| report.reporter_address != address);
    }

    /// An advertisement from a member. `sent` is compared across sessions.
    /// Sequence is compared only inside one session.
    pub fn ingest_air(&mut self, report: RosterReport, configured: Option<Address>) -> Ingest {
        let Some(session) = report.wire_session else {
            return Ingest::Mismatched;
        };
        if report.seq == 0 || report.sent.0 == 0 {
            return Ingest::Replay;
        }
        if let Some(old) = self.air_seen.get(&report.reporter_address) {
            if report.sent <= old.sent || (session == old.session && report.seq <= old.seq) {
                return Ingest::Replay;
            }
        }
        self.air_seen.insert(
            report.reporter_address,
            AirMark { session, seq: report.seq, sent: report.sent },
        );
        let mismatched = configured.is_some_and(|accessory| accessory != report.accessory);
        if mismatched {
            return Ingest::Mismatched;
        }
        let reporter = report.reporter_address;
        self.reports.push(report);
        self.cap_reports(reporter);
        Ingest::Stored
    }

    pub fn export_air(&self) -> Vec<u8> {
        let mut lines = vec!["v1".to_string()];
        let mut marks: Vec<_> = self.air_seen.iter().collect();
        marks.sort_by_key(|(address, _)| address.display());
        for (address, mark) in marks {
            lines.push(format!(
                "{} {} {} {}",
                address.display(),
                hex16(&mark.session),
                mark.seq,
                mark.sent.0
            ));
        }
        let mut bytes = lines.join("\n").into_bytes();
        bytes.push(b'\n');
        bytes
    }

    pub fn has_air(&self) -> bool {
        !self.air_seen.is_empty()
    }

    pub fn import_air(&mut self, bytes: &[u8]) {
        let Ok(text) = std::str::from_utf8(bytes) else {
            return;
        };
        let mut lines = text.lines();
        if lines.next() != Some("v1") {
            return;
        }
        self.air_seen.clear();
        for line in lines {
            let mut parts = line.split_whitespace();
            let Some(address) = parts.next().and_then(Address::parse) else {
                continue;
            };
            let Some(session) = parts.next().and_then(unhex16) else {
                continue;
            };
            let Some(seq) = parts.next().and_then(|part| part.parse().ok()) else {
                continue;
            };
            let Some(sent) = parts.next().and_then(|part| part.parse().ok()) else {
                continue;
            };
            if parts.next().is_some() {
                continue;
            }
            self.air_seen.insert(address, AirMark { session, seq, sent: UnixMs(sent) });
        }
    }
}

fn hex16(bytes: &[u8; 16]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn unhex16(text: &str) -> Option<[u8; 16]> {
    if text.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    for (slot, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[slot * 2..slot * 2 + 2], 16).ok()?;
    }
    Some(out)
}

impl Default for Peers {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(seq: u64, accessory: Address) -> RosterReport {
        RosterReport {
            reporter: HostId::new([2; 16]),
            reporter_address: Address::parse("22:22:22:22:22:22").unwrap(),
            accessory,
            session: SessionId(7),
            seq,
            at: MonoMs(0),
            sent: UnixMs(0),
            peers: vec![Address::parse("22:22:22:22:22:22").unwrap()],
            audio_source: None,
            pending: false,
            audio_idle: true,
            wire_session: None,
            last_use: UnixMs(0),
        }
    }

    #[test]
    fn replays_and_the_wrong_accessory_do_not_become_a_roster() {
        let pods = Address::parse("AA:AA:AA:AA:AA:AA").unwrap();
        let other = Address::parse("BB:BB:BB:BB:BB:BB").unwrap();
        let mut peers = Peers::new();
        assert_eq!(peers.ingest(report(1, pods), Some(pods)), Ingest::Stored);
        assert_eq!(peers.ingest(report(1, pods), Some(pods)), Ingest::Replay);
        assert_eq!(peers.ingest(report(2, other), Some(pods)), Ingest::Mismatched);
        let now = Clock::new(0, 0);
        assert!(peers.fresh(now, pods).is_some());
        assert!(peers.fresh(now, other).is_none());
        peers.invalidate();
        assert!(peers.fresh(now, pods).is_none());
    }

    #[test]
    fn old_reports_from_one_reporter_are_capped() {
        let pods = Address::parse("AA:AA:AA:AA:AA:AA").unwrap();
        let mut peers = Peers::new();
        for seq in 1..=12 {
            assert_eq!(peers.ingest(report(seq, pods), Some(pods)), Ingest::Stored);
        }
        assert_eq!(peers.reports.len(), 4);
        assert_eq!(peers.reports.first().unwrap().seq, 9);
        // The newest is still the fresh one.
        assert_eq!(peers.fresh(Clock::new(0, 0), pods).unwrap().seq, 12);
    }

    #[test]
    fn a_report_dated_in_the_future_is_never_fresh() {
        let pods = Address::parse("AA:AA:AA:AA:AA:AA").unwrap();
        let mut peers = Peers::new();
        let mut future = report(1, pods);
        future.at = MonoMs(60_000);
        let reporter = future.reporter_address;
        assert_eq!(peers.ingest(future, Some(pods)), Ingest::Stored);
        let now = Clock::new(0, 0);
        assert!(peers.fresh(now, pods).is_none());
        assert!(peers.fresh_from(now, pods, reporter).is_none());
    }

    #[test]
    fn an_air_roster_expires_fifteen_seconds_after_its_timestamp() {
        let pods = Address::parse("AA:AA:AA:AA:AA:AA").unwrap();
        let mut peers = Peers::new();
        let mut aired = report(1, pods);
        aired.sent = UnixMs(5_000);
        aired.wire_session = Some([1; 16]);
        assert_eq!(peers.ingest_air(aired, Some(pods)), Ingest::Stored);
        assert!(peers.fresh(Clock::new(0, 5_000 + 14_999), pods).is_some());
        assert!(peers.fresh(Clock::new(90_000, 5_000 + 15_000), pods).is_none());
    }
}
