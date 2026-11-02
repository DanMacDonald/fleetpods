//! The selected accessory and the user's saved preferences.
//! A preference is not an accessory-confirmed setting.

use crate::types::{Address, EarBehavior, NoiseMode};

/// Desired settings for one pair. Remembered per accessory so switching
/// pairs restores that pair's own noise/ear/conversation choices.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairPrefs {
    pub ear: EarBehavior,
    pub noise: Option<NoiseMode>,
    pub adaptive: Option<u8>,
    pub conversation: Option<bool>,
    pub one_bud: Option<bool>,
}

impl PairPrefs {
    pub fn new() -> Self {
        Self {
            ear: EarBehavior::PauseOne,
            noise: None,
            adaptive: None,
            conversation: None,
            one_bud: None,
        }
    }
}

impl Default for PairPrefs {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub accessory: Option<Address>,
    pub ear: EarBehavior,
    pub noise: Option<NoiseMode>,
    pub adaptive: Option<u8>,
    pub conversation: Option<bool>,
    pub one_bud: Option<bool>,
    /// Saved prefs for non-selected pairs, keyed by accessory address.
    /// The fields above are always the selected pair's live prefs.
    pub saved_prefs: std::collections::BTreeMap<Address, PairPrefs>,
}

impl Config {
    pub fn new() -> Self {
        Self {
            accessory: None,
            ear: EarBehavior::PauseOne,
            noise: None,
            adaptive: None,
            conversation: None,
            one_bud: None,
            saved_prefs: std::collections::BTreeMap::new(),
        }
    }

    /// The live prefs fields as one bundle.
    pub fn live_prefs(&self) -> PairPrefs {
        PairPrefs {
            ear: self.ear,
            noise: self.noise,
            adaptive: self.adaptive,
            conversation: self.conversation,
            one_bud: self.one_bud,
        }
    }

    pub fn apply_prefs(&mut self, prefs: PairPrefs) {
        self.ear = prefs.ear;
        self.noise = prefs.noise;
        self.adaptive = prefs.adaptive;
        self.conversation = prefs.conversation;
        self.one_bud = prefs.one_bud;
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::new()
    }
}
