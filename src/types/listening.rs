//! Listening controls and the telemetry values those controls are not.
//! A saved preference and an accessory-confirmed report use the same enums
//! and live in different modules: `pods::prefs` and `pods::reports`.

use super::time::MonoMs;

/// Wire values match the panel: 0 off, 1 ANC, 2 transparency, 3 adaptive.
/// Unknown is the absence of a mode, projected as `-1`, not a variant here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoiseMode {
    Off = 0,
    Anc = 1,
    Transparency = 2,
    Adaptive = 3,
}

impl NoiseMode {
    pub fn from_wire(value: i64) -> Option<Self> {
        Some(match value {
            0 => Self::Off,
            1 => Self::Anc,
            2 => Self::Transparency,
            3 => Self::Adaptive,
            _ => return None,
        })
    }

    pub fn wire(self) -> i64 {
        self as i64
    }

    pub fn verb(self) -> &'static str {
        match self {
            Self::Off => "noise:off",
            Self::Anc => "noise:anc",
            Self::Transparency => "noise:transparency",
            Self::Adaptive => "noise:adaptive",
        }
    }
}

/// Wire values match the panel: 0 one bud, 1 both, 2 disabled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EarBehavior {
    PauseOne = 0,
    PauseBoth = 1,
    Disabled = 2,
}

impl EarBehavior {
    pub fn from_wire(value: i64) -> Option<Self> {
        Some(match value {
            0 => Self::PauseOne,
            1 => Self::PauseBoth,
            2 => Self::Disabled,
            _ => return None,
        })
    }

    pub fn wire(self) -> i64 {
        self as i64
    }
}

/// A reported battery. Level is never used to mean "missing".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Battery {
    level: u8,
    pub charging: bool,
}

impl Battery {
    pub fn new(level: u8, charging: bool) -> Option<Self> {
        if level > 100 {
            None
        } else {
            Some(Self { level, charging })
        }
    }

    pub fn level(self) -> u8 {
        self.level
    }
}

/// Latest lid observation and the last value that was actually reported.
/// A missing advertisement does not change either.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lid {
    pub latest: super::Evidence<LidState>,
    pub last_known: Option<LidState>,
}

impl Default for Lid {
    fn default() -> Self {
        Self { latest: super::Evidence::Unknown, last_known: None }
    }
}

impl Lid {
    pub fn observe(&mut self, state: LidState, at: MonoMs) {
        self.latest = super::Evidence::fresh(state, at);
        self.last_known = Some(state);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LidState {
    Open = 0,
    Closed = 1,
}

impl LidState {
    pub fn wire(self) -> i64 {
        self as i64
    }
}

/// Adaptive level is an integer percent, not a boolean.
pub fn adaptive_level(value: i64) -> Option<u8> {
    u8::try_from(value).ok().filter(|n| *n <= 100)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn battery_rejects_out_of_range_and_zero_is_a_real_level() {
        assert!(Battery::new(101, false).is_none());
        assert_eq!(Battery::new(0, true).unwrap().level(), 0);
    }
}
