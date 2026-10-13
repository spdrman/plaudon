//! Typed, non-interchangeable identifiers.
//!
//! Spec 6.3B separates media identity, recording identity, source association,
//! source-media revision, transcript revision and derived output revision. Spec
//! 6.5 additionally requires typed source/provenance identifiers. Every
//! identifier below is a distinct Rust type, so passing one where another is
//! expected is a compile error:
//!
//! ```compile_fail
//! use plaudon_domain::ids::{RecordingId, SourceId};
//! fn takes_recording(_: RecordingId) {}
//! takes_recording(SourceId::new());
//! ```

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Failure parsing a typed identifier from its canonical string form.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid {type_name}: not a canonical UUID")]
pub struct IdParseError {
    /// Name of the identifier type that failed to parse.
    pub type_name: &'static str,
}

macro_rules! typed_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            /// Mint a fresh, time-ordered identifier.
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }

            /// Wrap an existing UUID that was produced for this identifier kind.
            #[must_use]
            pub const fn from_uuid(value: Uuid) -> Self {
                Self(value)
            }

            /// Borrow the underlying UUID.
            #[must_use]
            pub const fn as_uuid(&self) -> &Uuid {
                &self.0
            }

            /// Name of this identifier type, used in diagnostics and errors.
            pub const TYPE_NAME: &'static str = stringify!($name);
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }

        impl FromStr for $name {
            type Err = IdParseError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(s)
                    .map(Self)
                    .map_err(|_| IdParseError { type_name: Self::TYPE_NAME })
            }
        }
    };
}

typed_id!(
    /// Identity of a user-owned logical meeting (spec 6.3B "recording identity").
    RecordingId
);
typed_id!(
    /// Identity of a stored immutable media blob row.
    MediaAssetId
);
typed_id!(
    /// Identity of a configured recording source.
    SourceId
);
typed_id!(
    /// Identity of one source-side item association.
    SourceItemId
);
typed_id!(
    /// Identity of an immutable transcript revision.
    TranscriptRevisionId
);
typed_id!(
    /// Identity of a derived AI output revision.
    OutputId
);
typed_id!(
    /// Identity of a physical device record.
    DeviceId
);
typed_id!(
    /// Identity of a persisted job.
    JobId
);
typed_id!(
    /// Identity of one on-device recording as reported by a device profile.
    DeviceRecordingId
);
typed_id!(
    /// Identity of a single device transfer attempt.
    TransferId
);
typed_id!(
    /// Identity of an import journal intent.
    ImportIntentId
);
typed_id!(
    /// Identity of a redacted diagnostics entry referenced by an error envelope.
    DiagnosticId
);
typed_id!(
    /// Identity of a speaker record scoped to a recording.
    SpeakerId
);
typed_id!(
    /// Identity of one generated claim inside a derived output (spec 9.6A).
    ClaimId
);
typed_id!(
    /// Identity of one executed summarization-pipeline stage (spec 9.1).
    StageRunId
);
typed_id!(
    /// Identity of one transcript-tool run (spec 13.3 `tool_runs`).
    ToolRunId
);
typed_id!(
    /// Identity of a person on the master speaker list, the library-wide
    /// roster that recordings assign speakers from (VOICE_PROFILES_SPEC
    /// VP-010). A roster entry is a name, not a voiceprint.
    PersonId
);
typed_id!(
    /// Identity of one stored voice exemplar, i.e. one encrypted embedding
    /// with its provenance (VOICE_PROFILES_SPEC VP-070).
    VoiceExemplarId
);
typed_id!(
    /// Identity of one speaker-match suggestion. A suggestion is a score
    /// awaiting a human decision, never an identity (ADR-0082).
    SpeakerSuggestionId
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip_through_their_canonical_string_form() {
        let id = RecordingId::new();
        let parsed: RecordingId = id.to_string().parse().unwrap();
        assert_eq!(id, parsed);
    }

    #[test]
    fn ids_serialize_as_a_bare_string() {
        let id = SourceId::from_uuid(Uuid::nil());
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"00000000-0000-0000-0000-000000000000\"");
        let back: SourceId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn parsing_a_non_uuid_reports_the_expected_type() {
        let err = "not-a-uuid".parse::<JobId>().unwrap_err();
        assert_eq!(err.type_name, "JobId");
    }

    #[test]
    fn freshly_minted_ids_are_distinct_and_time_ordered() {
        let a = RecordingId::new();
        let b = RecordingId::new();
        assert_ne!(a, b);
        assert_eq!(a.as_uuid().get_version_num(), 7);
    }
}
