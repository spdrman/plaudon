//! Tri-state capability primitives (spec §8.3).
//!
//! > Values may be `unknown`; unknown is not equivalent to supported.
//!
//! Every capability flag in [`crate::capabilities::SttCapabilities`] is a [`Tri`]
//! and every capability *limit* is a [`Known`]. Neither type exposes a
//! "treat unknown as permissive" accessor: the only way to read a flag for a
//! gating decision is [`Tri::satisfies`], and the only way to read a limit is
//! [`Known::known`], which forces the caller to handle the unknown arm.

use serde::{Deserialize, Serialize};

/// A capability flag that may be affirmatively supported, affirmatively
/// unsupported, or undetermined.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tri {
    Yes,
    No,
    Unknown,
}

impl Tri {
    /// The *only* way a [`Tri`] may gate behaviour.
    ///
    /// `Unknown` never satisfies a requirement (§8.3).
    #[inline]
    #[must_use]
    pub const fn satisfies(self) -> bool {
        matches!(self, Tri::Yes)
    }

    /// True when the capability is affirmatively known to be absent.
    ///
    /// Distinct from `!satisfies()`: an `Unknown` flag is neither satisfied nor
    /// refuted, which matters when we explain *why* an engine was rejected.
    #[inline]
    #[must_use]
    pub const fn is_refuted(self) -> bool {
        matches!(self, Tri::No)
    }

    #[inline]
    #[must_use]
    pub const fn is_unknown(self) -> bool {
        matches!(self, Tri::Unknown)
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Tri::Yes => "yes",
            Tri::No => "no",
            Tri::Unknown => "unknown",
        }
    }
}

impl From<bool> for Tri {
    #[inline]
    fn from(value: bool) -> Self {
        if value {
            Tri::Yes
        } else {
            Tri::No
        }
    }
}

impl Default for Tri {
    /// Defaults to `Unknown`: a capability we never measured is never claimed.
    #[inline]
    fn default() -> Self {
        Tri::Unknown
    }
}

/// A capability *value* (a limit, a list, a size) that may be undetermined.
///
/// Serialised as the inner value or `null`, so a manifest that omits a limit
/// round-trips to `Unknown` instead of silently becoming a default.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Known<T> {
    Known(T),
    #[serde(serialize_with = "serialize_unknown")]
    Unknown,
}

fn serialize_unknown<S: serde::Serializer>(serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_none()
}

impl<T> Known<T> {
    #[inline]
    pub const fn known(&self) -> Option<&T> {
        match self {
            Known::Known(value) => Some(value),
            Known::Unknown => None,
        }
    }

    #[inline]
    pub const fn is_unknown(&self) -> bool {
        matches!(self, Known::Unknown)
    }

    pub fn map<U, F: FnOnce(T) -> U>(self, f: F) -> Known<U> {
        match self {
            Known::Known(value) => Known::Known(f(value)),
            Known::Unknown => Known::Unknown,
        }
    }
}

impl<T> Default for Known<T> {
    #[inline]
    fn default() -> Self {
        Known::Unknown
    }
}

impl<T> From<Option<T>> for Known<T> {
    #[inline]
    fn from(value: Option<T>) -> Self {
        match value {
            Some(v) => Known::Known(v),
            None => Known::Unknown,
        }
    }
}

impl<T: PartialOrd> Known<T> {
    /// Checks a *requirement* against a declared ceiling.
    ///
    /// Returns [`LimitCheck::Unknown`] when the ceiling is undetermined; callers
    /// must treat that as "not permitted" for gating (§8.3) while still being
    /// able to report it as a distinct reason from an outright breach.
    pub fn within_ceiling(&self, requested: &T) -> LimitCheck {
        match self {
            Known::Unknown => LimitCheck::Unknown,
            Known::Known(ceiling) => {
                if requested <= ceiling {
                    LimitCheck::Within
                } else {
                    LimitCheck::Exceeds
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitCheck {
    Within,
    Exceeds,
    Unknown,
}

impl LimitCheck {
    #[inline]
    #[must_use]
    pub const fn satisfies(self) -> bool {
        matches!(self, LimitCheck::Within)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_never_satisfies() {
        assert!(!Tri::Unknown.satisfies());
        assert!(!Tri::No.satisfies());
        assert!(Tri::Yes.satisfies());
    }

    #[test]
    fn unknown_is_not_refuted() {
        // "unsupported" and "we never checked" must stay distinguishable so the
        // selection explanation can say which one happened.
        assert!(!Tri::Unknown.is_refuted());
        assert!(Tri::No.is_refuted());
    }

    #[test]
    fn default_tri_is_unknown() {
        assert_eq!(Tri::default(), Tri::Unknown);
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
    }

    #[test]
    fn known_round_trips_as_nullable_json() {
        let known: Known<u64> = Known::Known(7);
        assert_eq!(serde_json::to_string(&known).unwrap(), "7");
        let unknown: Known<u64> = Known::Unknown;
        assert_eq!(serde_json::to_string(&unknown).unwrap(), "null");
        let parsed: Known<u64> = serde_json::from_str("null").unwrap();
        assert_eq!(parsed, Known::Unknown);
        let parsed: Known<u64> = serde_json::from_str("7").unwrap();
        assert_eq!(parsed, Known::Known(7));
    }
}
