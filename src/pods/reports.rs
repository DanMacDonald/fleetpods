//! Decoded accessory telemetry, confirmed settings, and the direct roster.
//! Desired settings live in [`super::prefs`] and are not written here.

use crate::types::{
    Address, Battery, EarBehavior, Evidence, Lid, LidState, Model, MonoMs, NoiseMode,
};

/// How long a direct report stays fresh for the resolution policy.
pub const REPORT_TTL_MS: u64 = 120_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceReport {
    pub local: bool,
    /// `0` is not a playing source. `1` and `2` are.
    pub kind: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pod {
    pub battery: Battery,
    pub in_ear: Option<bool>,
    pub in_case: Option<bool>,
}

/// Placement can be learned from public advertisements without battery keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    pub in_ear: Option<bool>,
    pub in_case: Option<bool>,
}

#[derive(Clone, Debug)]
pub struct Accessory {
    pub name: String,
    pub model: Model,
    pub sku: String,
    /// `Some(true)` means the accessory named this laptop.
    pub ownership: Evidence<bool>,
    pub source: Evidence<SourceReport>,
    pub remote_address: Evidence<Address>,
    pub left: Evidence<Pod>,
    pub right: Evidence<Pod>,
    pub left_placement: Evidence<Placement>,
    pub right_placement: Evidence<Placement>,
    pub case_battery: Evidence<Battery>,
    pub headset: Evidence<Battery>,
    pub lid: Lid,
    pub noise: Evidence<NoiseMode>,
    pub adaptive: Evidence<u8>,
    pub conversation: Evidence<bool>,
    pub one_bud: Evidence<bool>,
    pub ear: Evidence<EarBehavior>,
    pub ear_reported: bool,
    /// When a proximity advertisement for this accessory was last accepted.
    pub advertised_at: Option<MonoMs>,
    /// Last verified pairing-mode advertisement; expires quickly if broadcasts stop.
    pub pairing_at: Option<MonoMs>,
    /// Connection byte from that advertisement. Absent until one is accepted.
    pub ble_connection: Option<u8>,
}

impl Accessory {
    pub fn new() -> Self {
        Self {
            name: String::new(),
            model: Model::Unknown,
            sku: String::new(),
            ownership: Evidence::Unknown,
            source: Evidence::Unknown,
            remote_address: Evidence::Unknown,
            left: Evidence::Unknown,
            right: Evidence::Unknown,
            left_placement: Evidence::Unknown,
            right_placement: Evidence::Unknown,
            case_battery: Evidence::Unknown,
            headset: Evidence::Unknown,
            lid: Lid::default(),
            noise: Evidence::Unknown,
            adaptive: Evidence::Unknown,
            conversation: Evidence::Unknown,
            one_bud: Evidence::Unknown,
            ear: Evidence::Unknown,
            ear_reported: false,
            advertised_at: None,
            pairing_at: None,
            ble_connection: None,
        }
    }

    pub fn set_identity(&mut self, name: impl Into<String>, sku: &str) {
        self.name = name.into();
        self.sku = sku.to_string();
        self.model = Model::from_sku(sku);
    }

    /// The link that reported this seat is gone. Battery and lid stay.
    pub fn drop_owner(&mut self) {
        self.ownership = Evidence::Unknown;
        self.source = Evidence::Unknown;
        self.remote_address = Evidence::Unknown;
    }

    pub fn note_ownership(&mut self, local: bool, at: MonoMs) {
        self.ownership = Evidence::fresh(local, at);
    }

    pub fn note_source(&mut self, local: bool, kind: u8, at: MonoMs) {
        self.source = Evidence::fresh(SourceReport { local, kind }, at);
    }

    pub fn note_remote_address(&mut self, address: Address, at: MonoMs) {
        self.remote_address = Evidence::fresh(address, at);
    }

    /// A battery report says nothing about the ears, so it keeps the last ear reading. Without that,
    /// a new link's first battery report drew the buds leaving the ears. A case reading is not kept:
    /// an old docked report must not make a fresh link look stowed.
    pub fn note_pod(&mut self, right: bool, mut pod: Pod, at: MonoMs) {
        let placement = if right { &self.right_placement } else { &self.left_placement };
        pod.in_ear = pod.in_ear.or(placement.known().and_then(|placement| placement.in_ear));
        let slot = if right { &mut self.right } else { &mut self.left };
        if let Some(previous) = slot.known() {
            pod.in_ear = pod.in_ear.or(previous.in_ear);
        }
        *slot = Evidence::fresh(pod, at);
        self.note_placement(right, pod.in_ear, pod.in_case, at);
    }

    pub fn note_placement(
        &mut self,
        right: bool,
        in_ear: Option<bool>,
        in_case: Option<bool>,
        at: MonoMs,
    ) {
        let slot = if right { &mut self.right_placement } else { &mut self.left_placement };
        *slot = Evidence::fresh(Placement { in_ear, in_case }, at);
        if in_ear.is_some() {
            self.ear_reported = true;
        }
    }

    pub fn note_case(&mut self, battery: Battery, at: MonoMs) {
        self.case_battery = Evidence::fresh(battery, at);
    }

    pub fn note_headset(&mut self, battery: Battery, at: MonoMs) {
        self.headset = Evidence::fresh(battery, at);
    }

    pub fn note_lid(&mut self, state: LidState, at: MonoMs) {
        self.lid.observe(state, at);
    }

    /// Evidence that disconnected pods can play now. Case advertisements and
    /// old placement reports do not justify silencing the laptop's speakers.
    pub fn playback_available(&self, now: MonoMs) -> bool {
        let recent = |at| now.saturating_since(at) <= 10_000;
        if self.model.is_headset() {
            return self.advertised_at.is_some_and(recent);
        }
        [&self.left_placement, &self.right_placement].into_iter().any(|pod| {
            pod.at().is_some_and(recent)
                && pod
                    .as_fresh()
                    .is_some_and(|pod| pod.in_ear == Some(true) || pod.in_case == Some(false))
        })
    }

    /// A closed case stops advertising. Keep the connection hold until a newer
    /// lid or placement report contradicts it, without calling old telemetry fresh.
    pub fn stowed(&self) -> bool {
        let docked = |pod: &Evidence<Placement>| {
            pod.known().is_some_and(|pod| pod.in_case == Some(true) && pod.in_ear != Some(true))
        };
        !self.model.is_headset()
            && self.lid.last_known == Some(LidState::Closed)
            && docked(&self.left_placement)
            && docked(&self.right_placement)
    }

    /// Fresh placement reports rule out playback through docked buds even
    /// while the case is open and the last audio-source report remains active.
    pub fn both_docked(&self) -> bool {
        let docked = |pod: &Evidence<Placement>| {
            pod.as_fresh().is_some_and(|pod| pod.in_case == Some(true) && pod.in_ear != Some(true))
        };
        !self.model.is_headset() && docked(&self.left_placement) && docked(&self.right_placement)
    }

    pub fn note_model(&mut self, model: Model) {
        if model != Model::Unknown {
            self.model = model;
        }
    }

    pub fn note_advertisement(&mut self, at: MonoMs, connection: u8) {
        self.advertised_at = Some(at);
        self.ble_connection = Some(connection);
    }

    pub fn note_noise(&mut self, mode: NoiseMode, at: MonoMs) {
        self.noise = Evidence::fresh(mode, at);
    }

    pub fn note_adaptive(&mut self, level: u8, at: MonoMs) {
        if level <= 100 {
            self.adaptive = Evidence::fresh(level, at);
        }
    }

    pub fn note_conversation(&mut self, enabled: bool, at: MonoMs) {
        self.conversation = Evidence::fresh(enabled, at);
    }

    pub fn note_one_bud(&mut self, enabled: bool, at: MonoMs) {
        self.one_bud = Evidence::fresh(enabled, at);
    }

    pub fn note_ear(&mut self, behavior: EarBehavior, at: MonoMs) {
        self.ear = Evidence::fresh(behavior, at);
    }

    /// Advertisements repeat while the pods are nearby, so their readings age out. The control
    /// link reports a fact only when it changes and its owner reports are dropped with the link,
    /// so while `linked` its readings stand until the next report. The lid is advertised only.
    pub fn expire(&mut self, now: MonoMs, linked: bool) {
        self.lid.latest = std::mem::take(&mut self.lid.latest).expire(now, REPORT_TTL_MS);
        if linked {
            return;
        }
        self.ownership = std::mem::take(&mut self.ownership).expire(now, REPORT_TTL_MS);
        self.source = std::mem::take(&mut self.source).expire(now, REPORT_TTL_MS);
        self.remote_address = std::mem::take(&mut self.remote_address).expire(now, REPORT_TTL_MS);
        self.left = std::mem::take(&mut self.left).expire(now, REPORT_TTL_MS);
        self.right = std::mem::take(&mut self.right).expire(now, REPORT_TTL_MS);
        self.left_placement = std::mem::take(&mut self.left_placement).expire(now, REPORT_TTL_MS);
        self.right_placement = std::mem::take(&mut self.right_placement).expire(now, REPORT_TTL_MS);
        self.case_battery = std::mem::take(&mut self.case_battery).expire(now, REPORT_TTL_MS);
        self.headset = std::mem::take(&mut self.headset).expire(now, REPORT_TTL_MS);
        self.noise = std::mem::take(&mut self.noise).expire(now, REPORT_TTL_MS);
        self.adaptive = std::mem::take(&mut self.adaptive).expire(now, REPORT_TTL_MS);
        self.conversation = std::mem::take(&mut self.conversation).expire(now, REPORT_TTL_MS);
        self.one_bud = std::mem::take(&mut self.one_bud).expire(now, REPORT_TTL_MS);
        self.ear = std::mem::take(&mut self.ear).expire(now, REPORT_TTL_MS);
    }
}

impl Default for Accessory {
    fn default() -> Self {
        Self::new()
    }
}
