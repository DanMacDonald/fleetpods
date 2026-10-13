//! Monotonic instants for decisions. Unix milliseconds for the status file only.

/// Milliseconds from an arbitrary monotonic origin. Not comparable across boots.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MonoMs(pub u64);

impl MonoMs {
    pub fn saturating_add(self, ms: u64) -> Self {
        Self(self.0.saturating_add(ms))
    }

    pub fn saturating_since(self, earlier: Self) -> u64 {
        self.0.saturating_sub(earlier.0)
    }
}

/// Unix epoch milliseconds, the unit the panel uses for deadlines.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UnixMs(pub u64);

/// One reading of both clocks, taken together by the runtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Clock {
    pub mono: MonoMs,
    pub unix: UnixMs,
}

impl Clock {
    pub fn new(mono: u64, unix: u64) -> Self {
        Self { mono: MonoMs(mono), unix: UnixMs(unix) }
    }

    /// Convert a monotonic deadline into the unix time the panel counts down.
    pub fn unix_at(self, at: MonoMs) -> UnixMs {
        if at.0 >= self.mono.0 {
            UnixMs(self.unix.0.saturating_add(at.0 - self.mono.0))
        } else {
            UnixMs(self.unix.0.saturating_sub(self.mono.0 - at.0))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadlines_use_the_paired_clocks() {
        let clock = Clock::new(1_000, 50_000);
        assert_eq!(clock.unix_at(MonoMs(1_250)).0, 50_250);
        assert_eq!(clock.unix_at(MonoMs(900)).0, 49_900);
    }
}
