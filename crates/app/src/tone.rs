//! The status tone a presentation model carries.
//!
//! Spec §5.3 forbids communicating status by colour alone, so every tone in
//! this crate travels with a text glyph chosen by the presentation model that
//! produced it. The tone itself names an intent; which palette entry a theme
//! maps it to is a view concern.

use serde::{Deserialize, Serialize};

/// Intent of a status indicator, independent of any palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    /// No judgement: a fact, or a state with no consequence.
    Neutral,
    /// Work in progress.
    Running,
    /// Blocked on something outside the application.
    Waiting,
    /// Something the user should read before acting.
    Warning,
    /// A completed, durable result.
    Success,
    /// A failure.
    Danger,
}

impl Tone {
    /// Stable wire string, used in tests and in the activity log.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Neutral => "neutral",
            Self::Running => "running",
            Self::Waiting => "waiting",
            Self::Warning => "warning",
            Self::Success => "success",
            Self::Danger => "danger",
        }
    }
}
