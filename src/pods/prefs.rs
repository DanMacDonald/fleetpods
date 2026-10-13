//! The selected accessory and the user's saved preferences.
//! A preference is not an accessory-confirmed setting.

use crate::types::{Address, EarBehavior, NoiseMode};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub accessory: Option<Address>,
    pub ear: EarBehavior,
    pub noise: Option<NoiseMode>,
    pub adaptive: Option<u8>,
    pub conversation: Option<bool>,
    pub one_bud: Option<bool>,
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
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::new()
    }
}
