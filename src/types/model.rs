//! AirPods family identity. `model_int` matches the values already stored and published.
//! Capability flags describe the family. They are not a claim that this daemon has
//! verified every SKU on hardware.

/// Discriminants are the published `model_int`. Append new models; do not insert.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum Model {
    Unknown = 0,
    AirPods1 = 1,
    AirPods2 = 2,
    AirPods3 = 3,
    AirPodsPro = 4,
    AirPodsPro2Lightning = 5,
    AirPodsPro2Usbc = 6,
    AirPodsMaxLightning = 7,
    AirPodsMaxUsbc = 8,
    AirPods4 = 9,
    AirPods4Anc = 10,
    AirPodsPro3 = 11,
    AirPodsMax2 = 12,
}

impl Model {
    /// Product id in a proximity-pairing advertisement. An unknown id stays unknown.
    pub fn from_product(id: u16) -> Self {
        match id {
            0x0220 => Self::AirPods1,
            0x0f20 => Self::AirPods2,
            0x1320 => Self::AirPods3,
            0x1920 => Self::AirPods4,
            0x1b20 => Self::AirPods4Anc,
            0x0a20 => Self::AirPodsMaxLightning,
            0x1f20 => Self::AirPodsMaxUsbc,
            0x0e20 => Self::AirPodsPro,
            0x1420 => Self::AirPodsPro2Lightning,
            0x2420 => Self::AirPodsPro2Usbc,
            0x2720 => Self::AirPodsPro3,
            _ => Self::Unknown,
        }
    }

    pub fn from_sku(sku: &str) -> Self {
        match sku {
            "A1523" | "A1722" => Self::AirPods1,
            "A2032" | "A2031" => Self::AirPods2,
            "A2084" | "A2083" => Self::AirPodsPro,
            "A2096" => Self::AirPodsMaxLightning,
            "A3184" => Self::AirPodsMaxUsbc,
            "A3454" => Self::AirPodsMax2,
            "A2565" | "A2564" => Self::AirPods3,
            "A3047" | "A3048" | "A3049" => Self::AirPodsPro2Usbc,
            "A2931" | "A2699" | "A2698" => Self::AirPodsPro2Lightning,
            "A3053" | "A3050" | "A3054" => Self::AirPods4,
            "A3056" | "A3055" | "A3057" => Self::AirPods4Anc,
            "A3063" | "A3064" | "A3065" => Self::AirPodsPro3,
            _ => Self::Unknown,
        }
    }

    pub fn wire(self) -> i32 {
        self as i32
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::AirPods1 => "AirPods",
            Self::AirPods2 => "AirPods (2nd generation)",
            Self::AirPods3 => "AirPods (3rd generation)",
            Self::AirPods4 | Self::AirPods4Anc => "AirPods 4",
            Self::AirPodsPro => "AirPods Pro",
            Self::AirPodsPro2Lightning => "AirPods Pro 2",
            Self::AirPodsPro2Usbc => "AirPods Pro 2 (USB-C)",
            Self::AirPodsMaxLightning => "AirPods Max",
            Self::AirPodsMaxUsbc => "AirPods Max (USB-C)",
            Self::AirPodsMax2 => "AirPods Max 2",
            Self::AirPodsPro3 => "AirPods Pro 3",
            Self::Unknown => "",
        }
    }

    pub fn is_headset(self) -> bool {
        matches!(self, Self::AirPodsMaxLightning | Self::AirPodsMaxUsbc | Self::AirPodsMax2)
    }

    pub fn is_pro_series(self) -> bool {
        matches!(
            self,
            Self::AirPodsPro
                | Self::AirPodsPro2Lightning
                | Self::AirPodsPro2Usbc
                | Self::AirPodsPro3
        )
    }

    /// Pro 3 accepts an Off command and ignores it. The panel must not offer the mode.
    pub fn supports_noise_off(self) -> bool {
        self != Self::AirPodsPro3
    }

    /// Unknown fails open so an unmapped model keeps the controls it had.
    pub fn supports_noise_control(self) -> bool {
        !matches!(self, Self::AirPods1 | Self::AirPods2 | Self::AirPods3 | Self::AirPods4)
    }

    fn h2_listening(self) -> bool {
        matches!(
            self,
            Self::AirPods4Anc
                | Self::AirPodsPro2Lightning
                | Self::AirPodsPro2Usbc
                | Self::AirPodsPro3
                | Self::AirPodsMax2
        )
    }

    pub fn supports_adaptive(self) -> bool {
        self.h2_listening()
    }

    pub fn supports_conversation_awareness(self) -> bool {
        self.h2_listening()
    }

    pub fn supports_one_bud_anc(self) -> bool {
        matches!(
            self,
            Self::AirPods4Anc
                | Self::AirPodsPro
                | Self::AirPodsPro2Lightning
                | Self::AirPodsPro2Usbc
                | Self::AirPodsPro3
        )
    }

    pub fn allows(self, mode: super::NoiseMode) -> bool {
        if !self.supports_noise_control() {
            return false;
        }
        match mode {
            super::NoiseMode::Off => self.supports_noise_off(),
            super::NoiseMode::Adaptive => self.supports_adaptive(),
            super::NoiseMode::Anc | super::NoiseMode::Transparency => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pro_3_keeps_its_published_int_and_drops_off() {
        assert_eq!(Model::from_sku("A3064"), Model::AirPodsPro3);
        assert_eq!(Model::AirPodsPro3.wire(), 11);
        assert!(!Model::AirPodsPro3.supports_noise_off());
        assert!(Model::AirPodsPro3.supports_adaptive());
        assert!(Model::AirPodsPro3.supports_one_bud_anc());
        assert!(!Model::AirPodsMaxLightning.supports_one_bud_anc());
        assert!(Model::AirPodsMaxLightning.is_headset());
        assert!(!Model::AirPods1.supports_noise_control());
        assert!(Model::Unknown.supports_noise_control());
        assert_eq!(Model::from_sku("A3066"), Model::Unknown);
        assert_eq!(Model::from_product(0x2720), Model::AirPodsPro3);
        assert_eq!(Model::from_product(0x2027), Model::Unknown);
    }
}
