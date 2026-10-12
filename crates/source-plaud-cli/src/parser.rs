//! Version-locked parsing of CLI output (spec 7.2).
//!
//! Spec 2.1 forbids assuming a machine-readable mode exists. The parser
//! therefore accepts a *declared* format per validated version, refuses to
//! guess for an unknown version, and never falls back to scraping an
//! unrecognized layout.

use plaudon_domain::source::{
    ExternalRecordingId, SourceAccountScope, SourceIdentityKey, SourceKind,
};
use plaudon_recording_sources::source::SourceError;
use serde::{Deserialize, Serialize};

/// A CLI version we have tested.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CliVersion(String);

impl CliVersion {
    /// Wrap a version string exactly as the CLI reported it.
    #[must_use]
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    /// The version string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Extract a version from `plaud --version` output.
    ///
    /// Accepts the two shapes observed in vendor documentation,
    /// `plaud/1.2.3` and `1.2.3`, and nothing else.
    #[must_use]
    pub fn parse_version_output(stdout: &str) -> Option<Self> {
        let line = stdout.lines().find(|line| !line.trim().is_empty())?.trim();
        let candidate = line.rsplit(['/', ' ']).next()?.trim();
        let core = candidate.trim_start_matches('v');
        let parts: Vec<&str> = core.split('.').collect();
        if parts.len() < 2 || parts.len() > 4 {
            return None;
        }
        if !parts
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
        {
            return None;
        }
        Some(Self::new(core.to_owned()))
    }
}

/// Output format a validated CLI version produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ListingFormat {
    /// A JSON array of objects, one per recording.
    JsonArray,
    /// JSON Lines, one object per line.
    JsonLines,
}

/// Why parsing failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParserError {
    /// The CLI version is not in the validated table.
    #[error("this command line tool version has not been validated")]
    UnvalidatedVersion {
        /// Version reported by the tool.
        reported: String,
    },
    /// The output did not parse as the format the version declares.
    #[error("the command line tool produced output we could not interpret")]
    MalformedOutput,
    /// A required field was missing from a record.
    #[error("the command line tool output is missing a required field")]
    MissingField {
        /// Name of the missing field.
        field: &'static str,
    },
    /// The output was cut short.
    #[error("the command line tool output was truncated")]
    Truncated,
}

impl From<ParserError> for SourceError {
    fn from(error: ParserError) -> Self {
        match error {
            ParserError::UnvalidatedVersion { .. } => Self::CliUnsupported,
            ParserError::MalformedOutput
            | ParserError::MissingField { .. }
            | ParserError::Truncated => Self::MalformedOutput,
        }
    }
}

/// One enumerated recording, as the parser understands it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedListing {
    /// Source-scoped identity of the item.
    pub identity: SourceIdentityKey,
    /// Display name reported by the vendor. Advisory metadata only.
    pub display_name: String,
    /// Duration in milliseconds, when reported.
    pub duration_ms: Option<u64>,
    /// Remote version or modification marker, when reported.
    pub remote_version: Option<String>,
    /// Whether the vendor reports a generated transcript for this item.
    pub vendor_transcript_available: bool,
}

/// Raw listing record, as it appears in CLI output.
#[derive(Debug, Clone, Deserialize)]
struct RawRecord {
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    duration_ms: Option<u64>,
    #[serde(default)]
    updated_at: Option<String>,
    #[serde(default)]
    has_transcript: Option<bool>,
}

/// A parser pinned to one validated CLI version.
#[derive(Debug, Clone)]
pub struct VersionedParser {
    version: CliVersion,
    format: ListingFormat,
    account_scope: SourceAccountScope,
}

impl VersionedParser {
    /// Versions whose `files` output format we have actually validated.
    ///
    /// The table is intentionally empty. No release of the official CLI has
    /// been exercised against this build, and spec 2.1 forbids assuming an
    /// output contract we have not observed. Adding a row requires a golden
    /// fixture in `tests/fixtures/plaud-cli/` captured from that exact
    /// version.
    pub const VALIDATED: &'static [(&'static str, ListingFormat)] = &[];

    /// Build a parser for a reported version, or refuse.
    pub fn for_version(
        version: &CliVersion,
        account_scope: SourceAccountScope,
    ) -> Result<Self, ParserError> {
        let format = Self::VALIDATED
            .iter()
            .find(|(validated, _)| *validated == version.as_str())
            .map(|(_, format)| *format)
            .ok_or_else(|| ParserError::UnvalidatedVersion {
                reported: version.as_str().to_owned(),
            })?;
        Ok(Self {
            version: version.clone(),
            format,
            account_scope,
        })
    }

    /// Build a parser for a candidate version and format.
    ///
    /// Used by the spike harness to exercise a format against a captured
    /// fixture before the version is added to [`Self::VALIDATED`].
    #[must_use]
    pub fn candidate(
        version: CliVersion,
        format: ListingFormat,
        account_scope: SourceAccountScope,
    ) -> Self {
        Self {
            version,
            format,
            account_scope,
        }
    }

    /// The version this parser is pinned to.
    #[must_use]
    pub fn version(&self) -> &CliVersion {
        &self.version
    }

    /// Parse `files` output.
    pub fn parse_listing(&self, stdout: &str) -> Result<Vec<ParsedListing>, ParserError> {
        let records: Vec<RawRecord> = match self.format {
            ListingFormat::JsonArray => serde_json::from_str(stdout).map_err(|error| {
                if error.is_eof() {
                    ParserError::Truncated
                } else {
                    ParserError::MalformedOutput
                }
            })?,
            ListingFormat::JsonLines => {
                let mut out = Vec::new();
                for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
                    out.push(serde_json::from_str(line).map_err(|error| {
                        if error.is_eof() {
                            ParserError::Truncated
                        } else {
                            ParserError::MalformedOutput
                        }
                    })?);
                }
                out
            }
        };

        records
            .into_iter()
            .map(|record| {
                let id = record.id.ok_or(ParserError::MissingField { field: "id" })?;
                if id.trim().is_empty() {
                    return Err(ParserError::MissingField { field: "id" });
                }
                Ok(ParsedListing {
                    identity: SourceIdentityKey::new(
                        SourceKind::PlaudCloudCli,
                        self.account_scope.clone(),
                        Some(ExternalRecordingId::new(id)),
                    ),
                    display_name: record
                        .title
                        .or(record.name)
                        .unwrap_or_else(|| "Untitled recording".to_owned()),
                    duration_ms: record.duration_ms,
                    remote_version: record.updated_at,
                    vendor_transcript_available: record.has_transcript.unwrap_or(false),
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> SourceAccountScope {
        SourceAccountScope::new("account-a")
    }

    fn candidate(format: ListingFormat) -> VersionedParser {
        VersionedParser::candidate(CliVersion::new("1.0.0"), format, scope())
    }

    #[test]
    fn no_cli_version_is_validated_yet_so_every_version_fails_closed() {
        assert!(
            VersionedParser::VALIDATED.is_empty(),
            "adding a version requires a captured golden fixture from that exact release"
        );
        let error = VersionedParser::for_version(&CliVersion::new("1.0.0"), scope()).unwrap_err();
        assert_eq!(
            error,
            ParserError::UnvalidatedVersion {
                reported: "1.0.0".to_owned()
            }
        );
        assert_eq!(SourceError::from(error), SourceError::CliUnsupported);
    }

    #[test]
    fn version_output_is_parsed_only_in_the_documented_shapes() {
        assert_eq!(
            CliVersion::parse_version_output("plaud/1.2.3\n"),
            Some(CliVersion::new("1.2.3"))
        );
        assert_eq!(
            CliVersion::parse_version_output("1.2\n"),
            Some(CliVersion::new("1.2"))
        );
        assert_eq!(
            CliVersion::parse_version_output("plaud version v2.0.1\n"),
            Some(CliVersion::new("2.0.1"))
        );
        for unparseable in [
            "",
            "\n\n",
            "unknown",
            "plaud/next",
            "1",
            "1.2.3.4.5",
            "1..3",
        ] {
            assert_eq!(
                CliVersion::parse_version_output(unparseable),
                None,
                "{unparseable:?}"
            );
        }
    }

    #[test]
    fn a_json_array_listing_parses_into_source_identities() {
        let stdout = r#"[
            {"id":"rec-1","title":"Monday standup","duration_ms":1800000,"updated_at":"2026-10-01T09:00:00Z","has_transcript":true},
            {"id":"rec-2","name":"Client call"}
        ]"#;
        let listings = candidate(ListingFormat::JsonArray)
            .parse_listing(stdout)
            .expect("parses");
        assert_eq!(listings.len(), 2);
        assert_eq!(listings[0].display_name, "Monday standup");
        assert_eq!(listings[0].duration_ms, Some(1_800_000));
        assert!(listings[0].vendor_transcript_available);
        assert_eq!(listings[1].display_name, "Client call");
        assert!(
            !listings[1].vendor_transcript_available,
            "absent means false, never unknown-true"
        );
        assert_eq!(
            listings[0]
                .identity
                .external_id
                .as_ref()
                .map(ExternalRecordingId::as_str),
            Some("rec-1")
        );
        assert_ne!(listings[0].identity, listings[1].identity);
    }

    #[test]
    fn json_lines_output_parses_line_by_line() {
        let stdout = "{\"id\":\"rec-1\"}\n\n{\"id\":\"rec-2\"}\n";
        let listings = candidate(ListingFormat::JsonLines)
            .parse_listing(stdout)
            .expect("parses");
        assert_eq!(listings.len(), 2);
    }

    #[test]
    fn malformed_truncated_and_incomplete_output_is_refused() {
        let parser = candidate(ListingFormat::JsonArray);
        assert_eq!(
            parser.parse_listing("NAME            DURATION\nMonday standup  30:00\n"),
            Err(ParserError::MalformedOutput),
            "a tabular layout must not be scraped"
        );
        assert_eq!(
            parser.parse_listing(r#"[{"id":"rec-1"}"#),
            Err(ParserError::Truncated)
        );
        assert_eq!(
            parser.parse_listing(r#"[{"title":"no id here"}]"#),
            Err(ParserError::MissingField { field: "id" })
        );
        assert_eq!(
            parser.parse_listing(r#"[{"id":"   "}]"#),
            Err(ParserError::MissingField { field: "id" })
        );
    }

    #[test]
    fn parser_failures_never_surface_vendor_text() {
        let error = candidate(ListingFormat::JsonArray)
            .parse_listing("Error: PLAUD session token a1b2c3 expired")
            .unwrap_err();
        let rendered = error.to_string();
        assert!(!rendered.contains("a1b2c3"));
        assert!(!rendered.contains("PLAUD session token"));
        assert_eq!(SourceError::from(error), SourceError::MalformedOutput);
    }
}
