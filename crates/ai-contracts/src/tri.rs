//! Limit checking over [`plaudon_domain::capability::Known`] ceilings.
//!
//! `Tri` and `Known` themselves live in [`plaudon_domain::capability`] so the
//! workspace has exactly one definition of "unknown is not supported". What
//! this module adds is the vocabulary engine selection and chunk planning need
//! when a *ceiling* is involved: an unknown ceiling must fail closed for
//! gating, while staying distinguishable from an outright breach so the
//! rejection explanation is honest (§8.3).

use plaudon_domain::capability::Known;

/// Result of checking a requested value against a declared ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitCheck {
    /// Ceiling is known and the request fits.
    Within,
    /// Ceiling is known and the request breaches it.
    Exceeds,
    /// Ceiling was never observed.
    Unknown,
}

impl LimitCheck {
    /// Whether this check permits the operation. Only `Within` does.
    #[inline]
    #[must_use]
    pub const fn satisfies(self) -> bool {
        matches!(self, Self::Within)
    }

    /// Stable label for diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Within => "within",
            Self::Exceeds => "exceeds",
            Self::Unknown => "unknown",
        }
    }
}

/// Checking a request against a [`Known`] ceiling.
pub trait KnownLimit<T> {
    /// Inclusive ceiling check.
    fn within_ceiling(&self, requested: &T) -> LimitCheck;
}

impl<T: PartialOrd> KnownLimit<T> for Known<T> {
    fn within_ceiling(&self, requested: &T) -> LimitCheck {
        match self.get() {
            None => LimitCheck::Unknown,
            Some(ceiling) => {
                if requested <= ceiling {
                    LimitCheck::Within
                } else {
                    LimitCheck::Exceeds
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plaudon_domain::capability::Tri;

    #[test]
    fn unknown_never_satisfies() {
        assert!(!Tri::Unknown.satisfies());
        assert!(!Tri::No.satisfies());
        assert!(Tri::Yes.satisfies());
    }

    #[test]
    fn unknown_ceiling_never_satisfies() {
        let unknown: Known<u64> = Known::Unknown;
        assert_eq!(unknown.within_ceiling(&1), LimitCheck::Unknown);
        assert!(!unknown.within_ceiling(&1).satisfies());
    }

    #[test]
    fn ceiling_is_inclusive() {
        let ceiling = Known::Known(100u64);
        assert_eq!(ceiling.within_ceiling(&100), LimitCheck::Within);
        assert_eq!(ceiling.within_ceiling(&101), LimitCheck::Exceeds);
        assert_eq!(ceiling.within_ceiling(&0), LimitCheck::Within);
    }

    #[test]
    fn unknown_and_exceeds_stay_distinguishable() {
        let unknown: Known<u64> = Known::Unknown;
        assert_ne!(
            unknown.within_ceiling(&5),
            Known::Known(1u64).within_ceiling(&5)
        );
        assert_eq!(LimitCheck::Unknown.as_str(), "unknown");
        assert_eq!(LimitCheck::Exceeds.as_str(), "exceeds");
    }
}
