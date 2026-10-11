//! UTC timestamps with millisecond resolution.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A UTC instant stored as milliseconds since the Unix epoch.
///
/// The domain never stores local time: spec 13.2 records `captured_at`,
/// `created_at` and friends as absolute instants so that archives remain
/// interpretable after a restore on a different host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(i64);

impl Timestamp {
    /// The Unix epoch.
    pub const EPOCH: Self = Self(0);

    /// Construct from milliseconds since the Unix epoch.
    #[must_use]
    pub const fn from_millis(millis: i64) -> Self {
        Self(millis)
    }

    /// Milliseconds since the Unix epoch.
    #[must_use]
    pub const fn as_millis(self) -> i64 {
        self.0
    }

    /// Current wall-clock instant.
    #[must_use]
    pub fn now() -> Self {
        let now = time::OffsetDateTime::now_utc();
        Self(i64::try_from(now.unix_timestamp_nanos() / 1_000_000).unwrap_or(i64::MAX))
    }

    /// Render as an RFC 3339 string, or `None` if the value is out of range.
    #[must_use]
    pub fn to_rfc3339(self) -> Option<String> {
        let nanos = i128::from(self.0) * 1_000_000;
        time::OffsetDateTime::from_unix_timestamp_nanos(nanos)
            .ok()?
            .format(&time::format_description::well_known::Rfc3339)
            .ok()
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.to_rfc3339() {
            Some(text) => f.write_str(&text),
            None => write!(f, "{}ms", self.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_formats_as_rfc3339() {
        assert_eq!(Timestamp::EPOCH.to_rfc3339().unwrap(), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn now_is_after_the_epoch() {
        assert!(Timestamp::now() > Timestamp::EPOCH);
    }
}
