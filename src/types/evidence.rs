//! Unknown, absent, stale, and fresh are different. None of them is `false` or zero.

use super::time::MonoMs;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Evidence<T> {
    /// This source has not reported the fact.
    #[default]
    Unknown,
    /// The source reported that the fact does not apply.
    Absent,
    /// The last report is past its freshness window. The value is retained.
    Stale { value: T, at: MonoMs },
    /// The report is inside its freshness window.
    Fresh { value: T, at: MonoMs },
}

impl<T> Evidence<T> {
    pub fn fresh(value: T, at: MonoMs) -> Self {
        Self::Fresh { value, at }
    }

    pub fn fresh_ref(&self) -> Option<&T> {
        match self {
            Self::Fresh { value, .. } => Some(value),
            _ => None,
        }
    }

    pub fn as_fresh(&self) -> Option<&T> {
        self.fresh_ref()
    }

    /// Fresh or stale. Unknown and absent have no value.
    pub fn known(&self) -> Option<&T> {
        match self {
            Self::Fresh { value, .. } | Self::Stale { value, .. } => Some(value),
            _ => None,
        }
    }

    pub fn at(&self) -> Option<MonoMs> {
        match self {
            Self::Fresh { at, .. } | Self::Stale { at, .. } => Some(*at),
            _ => None,
        }
    }

    pub fn is_unknown(&self) -> bool {
        matches!(self, Self::Unknown)
    }

    /// A fresh report older than `ttl` becomes stale. Other variants are unchanged.
    pub fn expire(self, now: MonoMs, ttl_ms: u64) -> Self {
        match self {
            Self::Fresh { value, at } if now.saturating_since(at) > ttl_ms => {
                Self::Stale { value, at }
            }
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expiry_keeps_the_value_and_drops_freshness() {
        let seen = Evidence::fresh(7u8, MonoMs(100));
        let stale = seen.expire(MonoMs(250), 100);
        assert_eq!(stale, Evidence::Stale { value: 7, at: MonoMs(100) });
        assert!(stale.as_fresh().is_none());
        assert_eq!(stale.known(), Some(&7));
    }

    #[test]
    fn unknown_is_not_a_zero() {
        let missing: Evidence<u8> = Evidence::Unknown;
        assert!(missing.known().is_none());
        assert!(missing.is_unknown());
    }
}
