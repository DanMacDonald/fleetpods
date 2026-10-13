//! Unpaired setup candidates. Names label records, but only the advertiser's
//! address identifies one. Equal names/models and stronger RSSI never merge them.
//! A rotating private address cannot be resolved before we have pairing keys.

use super::{air, reports::Accessory};
use crate::types::{Address, LidState, Model, MonoMs};
use serde_json::{json, Value};

const MAX_CANDIDATES: usize = 32;
const SESSION_MS: u64 = 120_000;
const PAIRING_MS: u64 = 3000;

struct Candidate {
    address: Address,
    report: Accessory,
    seen: MonoMs,
    rssi: Option<i16>,
    signal: Option<(f64, MonoMs)>,
}

impl Candidate {
    fn open_and_docked(&self, now: MonoMs) -> bool {
        let mut report = self.report.clone();
        report.expire(now, false);
        report.lid.latest.as_fresh() == Some(&LidState::Open) && report.both_docked()
    }

    fn pairing(&self, now: MonoMs) -> bool {
        self.report.pairing_at.is_some_and(|at| now.saturating_since(at) < PAIRING_MS)
    }
}

#[derive(Default)]
pub struct Nearby {
    candidates: Vec<Candidate>,
    selected: Option<Address>,
    /// Keep setup focused only while pairing advertisements are still fresh.
    focused_at: Option<MonoMs>,
}

impl Nearby {
    pub fn hear(&mut self, address: Address, payload: &[u8], now: MonoMs) -> bool {
        let pairing = air::pairing_model(payload);
        let frame = air::parse(payload);
        let model = pairing.or_else(|| frame.as_ref().map(|frame| frame.model));
        let Some(model) = model.filter(|model| *model != Model::Unknown) else {
            return false;
        };
        self.candidates.retain(|candidate| {
            Some(candidate.address) == self.selected
                || now.saturating_since(candidate.seen) < SESSION_MS
        });
        if !self.candidates.iter().any(|candidate| candidate.address == address) {
            if self.candidates.len() >= MAX_CANDIDATES {
                let oldest = self
                    .candidates
                    .iter()
                    .enumerate()
                    .filter(|(_, candidate)| Some(candidate.address) != self.selected)
                    .max_by_key(|(_, candidate)| now.saturating_since(candidate.seen))
                    .map(|(index, _)| index)
                    .unwrap();
                self.candidates.remove(oldest);
            }
            self.candidates.push(Candidate {
                address,
                report: Accessory::new(),
                seen: now,
                rssi: None,
                signal: None,
            });
        }
        let candidate =
            self.candidates.iter_mut().find(|candidate| candidate.address == address).unwrap();
        candidate.seen = now;
        candidate.report.note_model(model);
        if pairing.is_some() {
            candidate.report.note_advertisement(now, 255);
            candidate.report.pairing_at = Some(now);
            // The alternate payload does not contain normal lid/placement fields.
        } else if let Some(frame) = frame {
            candidate.report.note_advertisement(now, frame.connection);
            if !model.is_headset() {
                for right in [false, true] {
                    let ear = if frame.both_in_case {
                        false
                    } else if right {
                        frame.right_in_ear
                    } else {
                        frame.left_in_ear
                    };
                    let docked = if frame.both_in_case {
                        Some(true)
                    } else if ear {
                        Some(false)
                    } else {
                        None
                    };
                    candidate.report.note_placement(right, Some(ear), docked, now);
                }
            }
            if !model.is_headset() {
                if let Some(battery) = frame
                    .case_level
                    .and_then(|level| crate::types::Battery::new(level, frame.case_charging))
                {
                    candidate.report.note_case(battery, now);
                }
            }
            if let Some(lid) = frame.lid {
                candidate.report.note_lid(lid, now);
                if lid == LidState::Closed {
                    candidate.report.pairing_at = None;
                }
            }
        }
        let focus_live = self.focused_at.is_some_and(|at| now.saturating_since(at) < PAIRING_MS);
        if pairing.is_some() && (!focus_live || self.selected == Some(address)) {
            self.selected = Some(address);
            self.focused_at = Some(now);
        } else if self.selected.is_none() {
            self.selected = Some(address);
        }
        self.prefer_signal(now);
        true
    }

    pub fn metadata(&mut self, address: Address, name: &str, rssi: Option<i16>, now: MonoMs) {
        // Metadata alone cannot create a candidate or refresh its advertisement age.
        if let Some(candidate) =
            self.candidates.iter_mut().find(|candidate| candidate.address == address)
        {
            if !name.trim().is_empty() {
                candidate.report.name = name.chars().take(128).collect();
            }
            if let Some(rssi) = rssi.filter(|rssi| (-127..=20).contains(rssi)) {
                candidate.rssi = Some(rssi);
                let smoothed = match candidate.signal {
                    Some((previous, at)) if now.saturating_since(at) < SESSION_MS => {
                        previous * 0.75 + f64::from(rssi) * 0.25
                    }
                    _ => f64::from(rssi),
                };
                candidate.signal = Some((smoothed, now));
            }
        }
        self.prefer_signal(now);
    }

    /// Expiry must advance selection even if no new advertisement arrives.
    pub fn refresh(&mut self, now: MonoMs) -> bool {
        let previous = self.selected;
        self.prefer_signal(now);
        previous != self.selected
    }

    fn prefer_signal(&mut self, now: MonoMs) {
        if self.focused_at.is_some_and(|at| now.saturating_since(at) < PAIRING_MS) {
            return;
        }
        let signal = |candidate: &Candidate| {
            candidate
                .signal
                .filter(|(_, at)| {
                    now.saturating_since(*at) < SESSION_MS
                        && now.saturating_since(candidate.seen) < SESSION_MS
                })
                .map(|(strength, _)| strength)
        };
        let have_open_case = self.candidates.iter().any(|candidate| candidate.open_and_docked(now));
        let eligible = |candidate: &&Candidate| {
            // Pairing envelopes have no placement fields. Once they stop, their
            // old RSSI must not beat a normal report with actual lid evidence.
            (candidate.report.ear_reported || candidate.report.model.is_headset())
                && (!have_open_case || candidate.open_and_docked(now))
        };
        let best = self
            .candidates
            .iter()
            .filter(eligible)
            .map(|candidate| (candidate.address, signal(candidate).unwrap_or(f64::NEG_INFINITY)))
            .max_by(|a, b| a.1.total_cmp(&b.1));
        let current = self
            .candidates
            .iter()
            .filter(eligible)
            .find(|candidate| Some(candidate.address) == self.selected)
            .map(|candidate| signal(candidate).unwrap_or(f64::NEG_INFINITY));
        if let Some((address, strength)) = best {
            // RSSI chooses guidance only. A clear margin prevents instruction flicker.
            if current.is_none_or(|current| strength > current && strength >= current + 8.0) {
                self.selected = Some(address);
            }
        }
    }

    /// Bluetooth selection is authoritative, including when it differs from focus.
    pub fn report_for_pairing(&self, address: Address, now: MonoMs) -> Option<Accessory> {
        if self.candidates.is_empty() {
            return None;
        }
        let mut report = self
            .candidates
            .iter()
            .find(|candidate| candidate.address == address)
            .map(|candidate| candidate.report.clone())
            .unwrap_or_default();
        report.pairing_at = None;
        report.expire(now, false);
        Some(report)
    }

    pub fn selected_report(&self, now: MonoMs) -> Option<Accessory> {
        let mut report = self
            .candidates
            .iter()
            .find(|candidate| Some(candidate.address) == self.selected)?
            .report
            .clone();
        report.expire(now, false);
        Some(report)
    }

    pub fn pairing_count(&self, now: MonoMs) -> usize {
        self.candidates.iter().filter(|candidate| candidate.pairing(now)).count()
    }

    pub fn status(&self, now: MonoMs) -> Value {
        json!(self.candidates.iter().filter(|candidate| Some(candidate.address) == self.selected
            || now.saturating_since(candidate.seen) < SESSION_MS).map(|candidate| {
            let report = &candidate.report;
            json!({
                "address": candidate.address.display(), "name": report.name,
                "model_name": report.model.display_name(), "model_int": report.model.wire(),
                "selected": self.selected == Some(candidate.address), "rssi_dbm": candidate.rssi,
                "smoothed_rssi_dbm": candidate.signal.map(|(strength, _)| strength),
                "last_seen_age_ms": now.saturating_since(candidate.seen), "pairing_mode": candidate.pairing(now),
                "last_known_lid_state": report.lid.last_known.map(LidState::wire),
                "left_in_case": report.left_placement.known().and_then(|pod| pod.in_case),
                "right_in_case": report.right_placement.known().and_then(|pod| pod.in_case),
            })
        }).collect::<Vec<_>>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn address(n: u8) -> Address {
        Address::new([2, 0, 0, 0, 0, n]).unwrap()
    }
    fn frame(model: u8, mode: u8, lid: u8) -> Vec<u8> {
        let mut bytes = vec![0x07, 0x19, mode, model, 0x20, 0x55, 0x87, 0xb1, lid, 0, 4];
        bytes.extend_from_slice(&[0; 16]);
        bytes
    }
    #[test]
    fn signal_guidance_is_stable_and_pairing_gesture_overrides_stronger_neighbors() {
        let mut nearby = Nearby::default();
        let now = MonoMs(1000);
        nearby.hear(address(1), &frame(0x0e, 1, 0x31), now);
        nearby.metadata(address(1), "Same name", Some(-85), now);
        nearby.hear(address(2), &frame(0x27, 1, 0x31), now);
        nearby.metadata(address(2), "Same name", Some(-45), now);
        assert_eq!(nearby.selected, Some(address(2)));
        assert_eq!(nearby.selected_report(now).unwrap().lid.last_known, Some(LidState::Open));
        // Similar readings, even repeatedly, do not change the instruction's model.
        for i in 1..30 {
            nearby.metadata(address(1), "Same name", Some(-42), MonoMs(1000 + i));
        }
        assert_eq!(nearby.selected, Some(address(2)));
        // A sustained clear improvement can change guidance before any pairing gesture.
        for i in 30..60 {
            nearby.metadata(address(1), "Same name", Some(-30), MonoMs(1000 + i));
        }
        assert_eq!(nearby.selected, Some(address(1)));
        nearby.hear(address(2), &frame(0x27, 7, 0), MonoMs(1100));
        assert_eq!(nearby.selected, Some(address(2)));
        for i in 1..30 {
            nearby.metadata(address(1), "Same name", Some(-10), MonoMs(1100 + i));
        }
        nearby.hear(address(1), &frame(0x27, 7, 0), MonoMs(1200));
        assert_eq!(nearby.selected, Some(address(2)));
        assert_eq!(nearby.pairing_count(MonoMs(1200)), 2);
        assert_eq!(nearby.candidates.len(), 2);
        // The first pair's own normal closed report stops its light, not the other's.
        nearby.hear(address(2), &frame(0x27, 1, 0x39), MonoMs(1300));
        assert_eq!(nearby.pairing_count(MonoMs(1300)), 1);
        assert_eq!(nearby.selected, Some(address(2)));
    }

    #[test]
    fn pairing_address_releases_focus_to_normal_case_reports_on_expiry() {
        let mut nearby = Nearby::default();
        nearby.hear(address(1), &frame(0x27, 7, 0), MonoMs(1000));
        nearby.metadata(address(1), "Pairing name", Some(-20), MonoMs(1000));
        nearby.hear(address(2), &frame(0x27, 1, 0x31), MonoMs(2000));
        nearby.metadata(address(2), "", Some(-50), MonoMs(2000));
        assert_eq!(nearby.selected, Some(address(1)));
        assert!(!nearby.refresh(MonoMs(3999)));
        assert!(nearby.refresh(MonoMs(4000)));
        assert_eq!(nearby.selected, Some(address(2)));
        assert_eq!(
            nearby.selected_report(MonoMs(4000)).unwrap().lid.last_known,
            Some(LidState::Open)
        );
        // Closing the case must not reselect the stronger, silent pairing address.
        nearby.hear(address(2), &frame(0x27, 1, 0x39), MonoMs(4100));
        assert_eq!(nearby.selected, Some(address(2)));
        assert_eq!(
            nearby.selected_report(MonoMs(4100)).unwrap().lid.last_known,
            Some(LidState::Closed)
        );
        assert_eq!(nearby.selected_report(MonoMs(4100)).unwrap().name, ""); // identities stay separate
    }

    #[test]
    fn an_open_docked_case_wins_over_stronger_closed_or_worn_pairs() {
        let mut nearby = Nearby::default();
        let now = MonoMs(1000);
        nearby.hear(address(1), &frame(0x0e, 1, 0x39), now);
        nearby.metadata(address(1), "Closed", Some(-20), now);
        nearby.hear(address(2), &frame(0x27, 1, 0x31), now);
        // Even without RSSI the only open, docked case wins.
        assert_eq!(nearby.selected, Some(address(2)));
        nearby.metadata(address(2), "Open", Some(-80), now);
        assert_eq!(nearby.selected, Some(address(2)));
        nearby.hear(address(1), &frame(0x0e, 1, 0x31), now);
        assert_eq!(nearby.selected, Some(address(1))); // both open: stronger wins
        let mut worn = frame(0x0e, 1, 0x31);
        worn[5] = 0x2a;
        nearby.hear(address(1), &worn, now);
        assert_eq!(nearby.selected, Some(address(2))); // open lid alone is insufficient
        nearby.hear(address(2), &frame(0x27, 1, 0x39), now);
        assert!(!nearby.candidates.iter().any(|candidate| candidate.open_and_docked(now)));
        // Old open-case evidence must not qualify after expiry.
        nearby.hear(address(2), &frame(0x27, 1, 0x31), now);
        assert!(!nearby
            .candidates
            .iter()
            .any(|candidate| candidate.open_and_docked(MonoMs(121001))));
    }

    #[test]
    fn candidates_are_bounded_and_metadata_cannot_create_or_refresh_presence() {
        let mut nearby = Nearby::default();
        nearby.metadata(address(1), "Not an AirPods advertisement", Some(-10), MonoMs(0));
        assert!(nearby.candidates.is_empty());
        for n in 1..64 {
            nearby.hear(address(n), &frame(0x27, 1, 0x31), MonoMs(n as u64));
        }
        assert_eq!(nearby.candidates.len(), MAX_CANDIDATES);
        nearby.metadata(address(1), "name update", Some(-40), MonoMs(120_002));
        nearby.hear(address(100), &frame(0x27, 1, 0x39), MonoMs(120_064));
        assert!(nearby.candidates.len() <= 2);
        nearby.metadata(address(100), "Fresh", Some(-80), MonoMs(120_064));
        assert_eq!(nearby.selected, Some(address(100))); // stale stronger signal loses
    }
}
