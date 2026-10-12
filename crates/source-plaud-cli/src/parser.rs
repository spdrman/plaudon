//! Version-locked parsing of CLI output (spec 7.2, 18.3).
//!
//! Spec 2.1 forbids assuming that a machine-readable mode exists at all, and
//! spec 7.2 gives exactly two acceptable designs: ship a **version-locked
//! parser with golden snapshots and a compatibility warning**, or postpone
//! automated enumeration while preserving manual import. This module implements
//! the first, and the empty compatibility table implements the second until a
//! release is actually exercised.
//!
//! Three rules hold regardless of format:
//!
//! * **No neighbouring-version fallback.** A parser exists only for an exact
//!   release string; an unknown version yields [`ParserError::UnvalidatedVersion`].
//! * **No partial listing shown as complete.** Any malformed, truncated or
//!   duplicated record fails the whole page. A page that parsed cleanly but may
//!   have more behind it is labelled [`ListingCompleteness::PossiblyIncomplete`].
//! * **No guessed pagination.** A cursor is returned only when the validated
//!   row recorded an observed cursor flag *and* the output actually carried a
//!   cursor field.

use std::collections::BTreeSet;

use plaudon_domain::source::{
    ExternalRecordingId, SourceAccountScope, SourceIdentityKey, SourceKind,
};
use plaudon_recording_sources::source::SourceError;
use serde::{Deserialize, Serialize};

/// Largest number of records we will materialize from one page.
///
/// Spec 18.3 requires the parser to never allocate unboundedly on hostile
/// input. The runner already caps the byte count; this caps the record count,
/// which is what a pathological one-byte-per-record format would exploit.
pub const MAX_RECORDS_PER_PAGE: usize = 50_000;

/// Longest line the table parser will consider.
pub const MAX_TABLE_LINE_BYTES: usize = 8192;

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
    /// `plaud/1.2.3` and `1.2.3`, and nothing else. A pre-release suffix is
    /// *not* stripped: `1.2.3-beta.1` is a different artifact from `1.2.3` and
    /// must not inherit its parser.
    #[must_use]
    pub fn parse_version_output(stdout: &str) -> Option<Self> {
        let line = stdout.lines().find(|line| !line.trim().is_empty())?.trim();
        let candidate = line.rsplit(['/', ' ']).next()?.trim();
        let core = candidate.strip_prefix('v').unwrap_or(candidate);
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
    /// A human-readable column layout, scraped under a version lock.
    ///
    /// Spec 7.2 permits this only with golden snapshots and a compatibility
    /// warning; [`crate::compatibility::CliCompatibilityRow::needs_scraping_warning`]
    /// carries the warning.
    DelimitedTable,
}

impl ListingFormat {
    /// Whether this format is a documented machine-readable one.
    #[must_use]
    pub const fn is_machine_readable(self) -> bool {
        matches!(self, Self::JsonArray | Self::JsonLines)
    }
}

/// Whether a page can be claimed to be the whole listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ListingCompleteness {
    /// Pagination was observed for this release and the listing is exhausted.
    Complete,
    /// Pagination behaviour was never observed, so a single page is all we
    /// will ask for and we say so (spec 7.2).
    PossiblyIncomplete,
}

impl ListingCompleteness {
    /// Copy for the library header when a listing may be partial.
    #[must_use]
    pub const fn disclosure(self) -> Option<&'static str> {
        match self {
            Self::Complete => None,
            Self::PossiblyIncomplete => Some(
                "This list shows one page of results. This version of the PLAUD command line \
                 tool has not been tested for paging, so there may be more recordings in your \
                 account.",
            ),
        }
    }
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
    /// Two records claimed the same external identifier.
    #[error("the command line tool listed the same recording identifier twice")]
    DuplicateExternalId,
    /// The output was cut short.
    #[error("the command line tool output was truncated")]
    Truncated,
    /// The output carried more records than we will materialize.
    #[error("the command line tool listed more recordings than we will read at once")]
    TooManyRecords {
        /// The cap that was exceeded.
        limit: usize,
    },
}

impl From<ParserError> for SourceError {
    fn from(error: ParserError) -> Self {
        match error {
            ParserError::UnvalidatedVersion { .. } => Self::CliUnsupported,
            ParserError::MalformedOutput
            | ParserError::MissingField { .. }
            | ParserError::DuplicateExternalId
            | ParserError::TooManyRecords { .. }
            | ParserError::Truncated => Self::MalformedOutput,
        }
    }
}

/// One enumerated recording, as the parser understands it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedListing {
    /// Source-scoped identity of the item.
    pub identity: SourceIdentityKey,
    /// Display name reported by the vendor. Advisory metadata only.
    pub display_name: String,
    /// Duration in milliseconds, when reported.
    pub duration_ms: Option<u64>,
    /// Byte length the vendor claims. Untrusted until the bytes are verified.
    pub declared_byte_length: Option<u64>,
    /// Capture time in milliseconds since the Unix epoch, when reported.
    pub captured_at_ms: Option<i64>,
    /// Remote version or modification marker, when reported.
    pub remote_version: Option<String>,
    /// Whether the vendor reports a generated transcript for this item.
    pub vendor_transcript_available: bool,
}

/// One parsed page of a listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedListingPage {
    /// Items on this page, in the order the CLI reported them.
    pub items: Vec<ParsedListing>,
    /// Cursor for the next page, present only when pagination was observed.
    pub next_cursor: Option<String>,
    /// Whether the page may be a partial view of the account.
    pub completeness: ListingCompleteness,
}

/// Raw listing record, as it appears in machine-readable CLI output.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRecord {
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    duration_ms: Option<u64>,
    #[serde(default)]
    size_bytes: Option<u64>,
    #[serde(default)]
    created_at_ms: Option<i64>,
    #[serde(default)]
    updated_at: Option<String>,
    #[serde(default)]
    has_transcript: Option<bool>,
}

/// A JSON array page, which may carry a cursor alongside its records.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum RawPage {
    /// A bare array of records.
    Bare(Vec<RawRecord>),
    /// An envelope with records and an optional cursor.
    Enveloped {
        /// The records.
        files: Vec<RawRecord>,
        /// Cursor for the next page.
        #[serde(default)]
        next_cursor: Option<String>,
    },
}

/// A parser pinned to one validated CLI version.
#[derive(Debug, Clone)]
pub struct VersionedParser {
    version: CliVersion,
    format: ListingFormat,
    account_scope: SourceAccountScope,
    pagination_observed: bool,
}

impl VersionedParser {
    /// Build a parser for a validated compatibility row.
    ///
    /// This is the only constructor the connector uses. Building a parser for
    /// an unvalidated version is impossible: there is no row to pass.
    #[must_use]
    pub fn for_row(
        row: &crate::compatibility::CliCompatibilityRow,
        account_scope: SourceAccountScope,
    ) -> Self {
        Self {
            version: CliVersion::new(row.version.clone()),
            format: row.listing_format,
            account_scope,
            pagination_observed: row.pagination.is_observed(),
        }
    }

    /// Build a parser for a candidate version and format.
    ///
    /// Used by the golden-fixture harness to exercise a format against captured
    /// output *before* the version is promoted into the committed table. It
    /// cannot be reached from the connector, which only ever calls
    /// [`Self::for_row`].
    #[must_use]
    pub fn candidate(
        version: CliVersion,
        format: ListingFormat,
        account_scope: SourceAccountScope,
        pagination_observed: bool,
    ) -> Self {
        Self {
            version,
            format,
            account_scope,
            pagination_observed,
        }
    }

    /// The version this parser is pinned to.
    #[must_use]
    pub fn version(&self) -> &CliVersion {
        &self.version
    }

    /// The format this parser expects.
    #[must_use]
    pub const fn format(&self) -> ListingFormat {
        self.format
    }

    /// Parse one page of `plaud files` output.
    pub fn parse_listing(&self, stdout: &str) -> Result<ParsedListingPage, ParserError> {
        let (records, cursor) = match self.format {
            ListingFormat::JsonArray => parse_json_array(stdout)?,
            ListingFormat::JsonLines => (parse_json_lines(stdout)?, None),
            ListingFormat::DelimitedTable => (parse_table(stdout)?, None),
        };
        if records.len() > MAX_RECORDS_PER_PAGE {
            return Err(ParserError::TooManyRecords {
                limit: MAX_RECORDS_PER_PAGE,
            });
        }

        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut items = Vec::with_capacity(records.len());
        for record in records {
            let id = record.id.ok_or(ParserError::MissingField { field: "id" })?;
            let id = id.trim().to_owned();
            if id.is_empty() {
                return Err(ParserError::MissingField { field: "id" });
            }
            if !seen.insert(id.clone()) {
                // Spec 7.2 persists the external id as identity. Two records
                // sharing one id means the identity is not stable, so the page
                // is refused rather than silently deduplicated.
                return Err(ParserError::DuplicateExternalId);
            }
            items.push(ParsedListing {
                identity: SourceIdentityKey::new(
                    SourceKind::PlaudCloudCli,
                    self.account_scope.clone(),
                    Some(ExternalRecordingId::new(id)),
                ),
                display_name: display_name(record.title, record.name),
                duration_ms: record.duration_ms,
                declared_byte_length: record.size_bytes,
                captured_at_ms: record.created_at_ms,
                remote_version: record.updated_at.filter(|v| !v.trim().is_empty()),
                vendor_transcript_available: record.has_transcript.unwrap_or(false),
            });
        }

        // A cursor is only meaningful when the release was observed to page.
        let next_cursor = if self.pagination_observed {
            cursor.filter(|c| !c.trim().is_empty())
        } else {
            None
        };
        let completeness = if self.pagination_observed {
            ListingCompleteness::Complete
        } else {
            ListingCompleteness::PossiblyIncomplete
        };

        Ok(ParsedListingPage {
            items,
            next_cursor,
            completeness,
        })
    }
}

/// Choose a display name without ever letting it become identity.
fn display_name(title: Option<String>, name: Option<String>) -> String {
    title
        .or(name)
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "Untitled recording".to_owned())
}

fn json_error(error: &serde_json::Error) -> ParserError {
    if error.is_eof() {
        ParserError::Truncated
    } else {
        ParserError::MalformedOutput
    }
}

fn parse_json_array(stdout: &str) -> Result<(Vec<RawRecord>, Option<String>), ParserError> {
    let page: RawPage = serde_json::from_str(stdout.trim()).map_err(|e| json_error(&e))?;
    Ok(match page {
        RawPage::Bare(records) => (records, None),
        RawPage::Enveloped { files, next_cursor } => (files, next_cursor),
    })
}

fn parse_json_lines(stdout: &str) -> Result<Vec<RawRecord>, ParserError> {
    let mut out = Vec::new();
    for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
        if out.len() >= MAX_RECORDS_PER_PAGE {
            return Err(ParserError::TooManyRecords {
                limit: MAX_RECORDS_PER_PAGE,
            });
        }
        out.push(serde_json::from_str(line).map_err(|e| json_error(&e))?);
    }
    Ok(out)
}

/// Column headers the table parser understands, mapped onto record fields.
///
/// Matching is on the *header text* rather than on column order, because a
/// release that reorders its columns must not silently shift every value by
/// one. A header we do not recognize is ignored; a missing `id` header fails.
const TABLE_HEADERS: &[(&str, TableColumn)] = &[
    ("id", TableColumn::Id),
    ("file id", TableColumn::Id),
    ("name", TableColumn::Name),
    ("title", TableColumn::Name),
    ("duration", TableColumn::Duration),
    ("size", TableColumn::Size),
    ("created", TableColumn::Created),
    ("created at", TableColumn::Created),
    ("updated", TableColumn::Updated),
    ("updated at", TableColumn::Updated),
    ("transcript", TableColumn::Transcript),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TableColumn {
    Id,
    Name,
    Duration,
    Size,
    Created,
    Updated,
    Transcript,
}

/// Scrape a human-readable column layout under a version lock.
///
/// Columns are separated by runs of two or more spaces, which is how every
/// aligned CLI table distinguishes padding from a space inside a value. A
/// single-space separator is not assumed, because recording titles contain
/// spaces.
fn parse_table(stdout: &str) -> Result<Vec<RawRecord>, ParserError> {
    let mut lines = stdout
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty() && !is_rule_line(line));
    let header = lines.next().ok_or(ParserError::MalformedOutput)?;
    if header.len() > MAX_TABLE_LINE_BYTES {
        return Err(ParserError::MalformedOutput);
    }
    let columns: Vec<Option<TableColumn>> = split_columns(header)
        .iter()
        .map(|cell| {
            let key = cell.trim().to_ascii_lowercase();
            TABLE_HEADERS
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, column)| *column)
        })
        .collect();
    if !columns.iter().any(|c| *c == Some(TableColumn::Id)) {
        return Err(ParserError::MissingField { field: "id" });
    }

    let mut records = Vec::new();
    for line in lines {
        if line.len() > MAX_TABLE_LINE_BYTES {
            return Err(ParserError::MalformedOutput);
        }
        if records.len() >= MAX_RECORDS_PER_PAGE {
            return Err(ParserError::TooManyRecords {
                limit: MAX_RECORDS_PER_PAGE,
            });
        }
        let cells = split_columns(line);
        if cells.len() != columns.len() {
            // A row that does not match the header shape is exactly the
            // "unexpectedly-shaped output" of spec 7.2's acceptance list.
            return Err(ParserError::MalformedOutput);
        }
        let mut record = RawRecord::default();
        for (cell, column) in cells.iter().zip(&columns) {
            let value = cell.trim();
            match column {
                Some(TableColumn::Id) => record.id = Some(value.to_owned()),
                Some(TableColumn::Name) => record.name = Some(value.to_owned()),
                Some(TableColumn::Duration) => record.duration_ms = parse_duration_ms(value),
                Some(TableColumn::Size) => record.size_bytes = value.parse().ok(),
                Some(TableColumn::Created) => record.created_at_ms = value.parse().ok(),
                Some(TableColumn::Updated) => {
                    record.updated_at = (!value.is_empty()).then(|| value.to_owned());
                }
                Some(TableColumn::Transcript) => {
                    record.has_transcript = Some(matches!(
                        value.to_ascii_lowercase().as_str(),
                        "yes" | "true" | "y" | "1" | "✓"
                    ));
                }
                None => {}
            }
        }
        records.push(record);
    }
    Ok(records)
}

/// Whether a line is a `---+---` style separator rule.
fn is_rule_line(line: &str) -> bool {
    let trimmed = line.trim();
    !trimmed.is_empty()
        && trimmed
            .chars()
            .all(|c| matches!(c, '-' | '=' | '+' | '|' | ' '))
}

/// Split a row on runs of two or more spaces.
fn split_columns(line: &str) -> Vec<&str> {
    let mut cells = Vec::new();
    let bytes = line.as_bytes();
    let mut start = 0usize;
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b' ' {
            let run_start = index;
            while index < bytes.len() && bytes[index] == b' ' {
                index += 1;
            }
            if index - run_start >= 2 {
                cells.push(&line[start..run_start]);
                start = index;
            }
            continue;
        }
        index += 1;
    }
    if start <= line.len() {
        cells.push(&line[start..]);
    }
    cells.retain(|cell| !cell.is_empty() || cells.len() == 1);
    cells
}

/// Parse `HH:MM:SS`, `MM:SS` or a bare millisecond count.
fn parse_duration_ms(value: &str) -> Option<u64> {
    if value.is_empty() {
        return None;
    }
    if !value.contains(':') {
        return value.parse().ok();
    }
    let mut total: u64 = 0;
    for part in value.split(':') {
        let unit: u64 = part.trim().parse().ok()?;
        total = total.checked_mul(60)?.checked_add(unit)?;
    }
    total.checked_mul(1000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn parser(format: ListingFormat, pagination: bool) -> VersionedParser {
        VersionedParser::candidate(
            CliVersion::new("1.4.2"),
            format,
            SourceAccountScope::new("account-a"),
            pagination,
        )
    }

    fn external_ids(page: &ParsedListingPage) -> Vec<String> {
        page.items
            .iter()
            .map(|item| {
                item.identity
                    .external_id
                    .as_ref()
                    .expect("external id")
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn version_output_shapes_from_vendor_documentation_parse() {
        for (input, expected) in [
            ("plaud/1.2.3\n", "1.2.3"),
            ("1.2.3", "1.2.3"),
            ("v1.2.3\n", "1.2.3"),
            ("\n\n  plaud 0.9.1  \n", "0.9.1"),
            ("1.2.3.4", "1.2.3.4"),
        ] {
            assert_eq!(
                CliVersion::parse_version_output(input).map(|v| v.as_str().to_owned()),
                Some(expected.to_owned()),
                "{input:?}"
            );
        }
    }

    #[test]
    fn unreadable_version_output_is_not_guessed() {
        for input in [
            "",
            "\n\n",
            "unknown",
            "1",
            "1.2.3.4.5",
            "1.2.beta",
            "plaud/",
            "1..2",
            "nightly-2026-10-11",
            "1.2.3-beta.1",
        ] {
            assert!(
                CliVersion::parse_version_output(input).is_none(),
                "{input:?} must not be read as a version"
            );
        }
    }

    #[test]
    fn a_json_lines_listing_yields_the_identity_triple() {
        let stdout = concat!(
            r#"{"id":"rec_1","title":"Board sync","duration_ms":1800000,"updated_at":"7","has_transcript":true}"#,
            "\n",
            r#"{"id":"rec_2","name":"1:1","size_bytes":4096}"#,
            "\n"
        );
        let page = parser(ListingFormat::JsonLines, false)
            .parse_listing(stdout)
            .expect("parsed");
        assert_eq!(external_ids(&page), ["rec_1", "rec_2"]);
        assert_eq!(page.items[0].identity.source_kind, SourceKind::PlaudCloudCli);
        assert_eq!(
            page.items[0].identity.account_scope,
            SourceAccountScope::new("account-a")
        );
        assert!(page.items[0].identity.is_stable());
        assert_eq!(page.items[0].display_name, "Board sync");
        assert_eq!(page.items[0].duration_ms, Some(1_800_000));
        assert_eq!(page.items[0].remote_version.as_deref(), Some("7"));
        assert!(page.items[0].vendor_transcript_available);
        assert_eq!(page.items[1].display_name, "1:1");
        assert_eq!(page.items[1].declared_byte_length, Some(4096));
        assert!(!page.items[1].vendor_transcript_available);
    }

    #[test]
    fn a_json_array_listing_parses() {
        let stdout = r#"[{"id":"rec_1"},{"id":"rec_2"}]"#;
        let page = parser(ListingFormat::JsonArray, false)
            .parse_listing(stdout)
            .expect("parsed");
        assert_eq!(external_ids(&page), ["rec_1", "rec_2"]);
    }

    #[test]
    fn an_empty_listing_is_a_success_not_an_error() {
        for (format, stdout) in [
            (ListingFormat::JsonArray, "[]"),
            (ListingFormat::JsonLines, "\n\n"),
        ] {
            let page = parser(format, false).parse_listing(stdout).expect("parsed");
            assert!(page.items.is_empty());
        }
    }

    #[test]
    fn duplicate_external_ids_fail_the_page() {
        let stdout = "{\"id\":\"rec_1\"}\n{\"id\":\"rec_1\"}\n";
        assert_eq!(
            parser(ListingFormat::JsonLines, false)
                .parse_listing(stdout)
                .unwrap_err(),
            ParserError::DuplicateExternalId
        );
    }

    #[test]
    fn a_missing_or_blank_id_fails_the_page() {
        for stdout in ["{\"title\":\"x\"}\n", "{\"id\":\"\"}\n", "{\"id\":\"   \"}\n"] {
            assert_eq!(
                parser(ListingFormat::JsonLines, false)
                    .parse_listing(stdout)
                    .unwrap_err(),
                ParserError::MissingField { field: "id" }
            );
        }
    }

    #[test]
    fn truncated_output_is_distinguished_from_malformed_output() {
        assert_eq!(
            parser(ListingFormat::JsonArray, false)
                .parse_listing("[{\"id\":\"rec_1\"")
                .unwrap_err(),
            ParserError::Truncated
        );
        assert_eq!(
            parser(ListingFormat::JsonArray, false)
                .parse_listing("not json at all")
                .unwrap_err(),
            ParserError::MalformedOutput
        );
    }

    #[test]
    fn an_unexpected_field_is_refused_rather_than_ignored() {
        // A release that adds a field has changed its contract; spec 7.2 says
        // we re-validate rather than hope the new field is cosmetic.
        assert_eq!(
            parser(ListingFormat::JsonLines, false)
                .parse_listing("{\"id\":\"rec_1\",\"surprise\":1}\n")
                .unwrap_err(),
            ParserError::MalformedOutput
        );
    }

    #[test]
    fn every_parser_error_maps_to_a_stable_source_error() {
        assert_eq!(
            SourceError::from(ParserError::UnvalidatedVersion {
                reported: "9.9.9".to_owned()
            }),
            SourceError::CliUnsupported
        );
        for error in [
            ParserError::MalformedOutput,
            ParserError::MissingField { field: "id" },
            ParserError::DuplicateExternalId,
            ParserError::Truncated,
            ParserError::TooManyRecords { limit: 1 },
        ] {
            assert_eq!(SourceError::from(error), SourceError::MalformedOutput);
        }
    }

    #[test]
    fn a_cursor_is_ignored_when_pagination_was_never_observed() {
        let stdout = r#"{"files":[{"id":"rec_1"}],"next_cursor":"abc"}"#;
        let page = parser(ListingFormat::JsonArray, false)
            .parse_listing(stdout)
            .expect("parsed");
        assert_eq!(page.next_cursor, None);
        assert_eq!(page.completeness, ListingCompleteness::PossiblyIncomplete);
        assert!(page.completeness.disclosure().is_some());
    }

    #[test]
    fn a_cursor_is_honoured_when_pagination_was_observed() {
        let stdout = r#"{"files":[{"id":"rec_1"}],"next_cursor":"abc"}"#;
        let page = parser(ListingFormat::JsonArray, true)
            .parse_listing(stdout)
            .expect("parsed");
        assert_eq!(page.next_cursor.as_deref(), Some("abc"));
        assert_eq!(page.completeness, ListingCompleteness::Complete);
        assert!(page.completeness.disclosure().is_none());
    }

    #[test]
    fn an_empty_cursor_does_not_request_another_page() {
        let stdout = r#"{"files":[{"id":"rec_1"}],"next_cursor":"  "}"#;
        let page = parser(ListingFormat::JsonArray, true)
            .parse_listing(stdout)
            .expect("parsed");
        assert_eq!(page.next_cursor, None);
    }

    #[test]
    fn a_regional_account_display_difference_is_metadata_not_identity() {
        // Spec 7.5/18.6: regional accounts render names differently. The
        // identity triple must be unaffected by display text.
        let eu = parser(ListingFormat::JsonLines, false)
            .parse_listing("{\"id\":\"rec_1\",\"title\":\"Réunion — 11/10/2026\"}\n")
            .expect("parsed");
        let us = VersionedParser::candidate(
            CliVersion::new("1.4.2"),
            ListingFormat::JsonLines,
            SourceAccountScope::new("account-a"),
            false,
        )
        .parse_listing("{\"id\":\"rec_1\",\"title\":\"Meeting 10/11/2026\"}\n")
        .expect("parsed");
        assert_eq!(eu.items[0].identity, us.items[0].identity);
        assert_ne!(eu.items[0].display_name, us.items[0].display_name);
    }

    #[test]
    fn two_accounts_reporting_the_same_id_are_two_identities() {
        let a = parser(ListingFormat::JsonLines, false)
            .parse_listing("{\"id\":\"rec_1\"}\n")
            .expect("parsed");
        let b = VersionedParser::candidate(
            CliVersion::new("1.4.2"),
            ListingFormat::JsonLines,
            SourceAccountScope::new("account-b"),
            false,
        )
        .parse_listing("{\"id\":\"rec_1\"}\n")
        .expect("parsed");
        assert_ne!(a.items[0].identity, b.items[0].identity);
    }

    #[test]
    fn a_missing_title_never_becomes_identity() {
        let page = parser(ListingFormat::JsonLines, false)
            .parse_listing("{\"id\":\"rec_1\"}\n{\"id\":\"rec_2\"}\n")
            .expect("parsed");
        assert_eq!(page.items[0].display_name, "Untitled recording");
        assert_eq!(page.items[1].display_name, "Untitled recording");
        assert_ne!(page.items[0].identity, page.items[1].identity);
    }

    #[test]
    fn a_table_layout_parses_by_header_name() {
        let stdout = "\
ID        Name                 Duration   Transcript
--------  -------------------  ---------  ----------
rec_1     Board sync           00:30:00   yes
rec_2     Réunion d'équipe     01:02:03   no
";
        let page = parser(ListingFormat::DelimitedTable, false)
            .parse_listing(stdout)
            .expect("parsed");
        assert_eq!(external_ids(&page), ["rec_1", "rec_2"]);
        assert_eq!(page.items[0].display_name, "Board sync");
        assert_eq!(page.items[0].duration_ms, Some(1_800_000));
        assert!(page.items[0].vendor_transcript_available);
        assert_eq!(page.items[1].display_name, "Réunion d'équipe");
        assert_eq!(page.items[1].duration_ms, Some(3_723_000));
        assert!(!page.items[1].vendor_transcript_available);
    }

    #[test]
    fn a_reordered_table_is_read_by_header_not_by_position() {
        let stdout = "\
Name              ID
----------------  -------
Board sync        rec_1
";
        let page = parser(ListingFormat::DelimitedTable, false)
            .parse_listing(stdout)
            .expect("parsed");
        assert_eq!(external_ids(&page), ["rec_1"]);
        assert_eq!(page.items[0].display_name, "Board sync");
    }

    #[test]
    fn a_table_without_an_id_column_is_refused() {
        let stdout = "Name       Duration\nBoard sync  00:30:00\n";
        assert_eq!(
            parser(ListingFormat::DelimitedTable, false)
                .parse_listing(stdout)
                .unwrap_err(),
            ParserError::MissingField { field: "id" }
        );
    }

    #[test]
    fn a_table_row_that_does_not_match_the_header_shape_is_refused() {
        let stdout = "ID       Name\nrec_1    Board sync    extra\n";
        assert_eq!(
            parser(ListingFormat::DelimitedTable, false)
                .parse_listing(stdout)
                .unwrap_err(),
            ParserError::MalformedOutput
        );
    }

    #[test]
    fn an_empty_table_output_is_refused_rather_than_read_as_zero_rows() {
        // No header means we cannot know whether the listing was empty or the
        // command failed, so it is malformed rather than "no recordings".
        assert_eq!(
            parser(ListingFormat::DelimitedTable, false)
                .parse_listing("")
                .unwrap_err(),
            ParserError::MalformedOutput
        );
    }

    #[test]
    fn machine_readable_formats_are_identified() {
        assert!(ListingFormat::JsonArray.is_machine_readable());
        assert!(ListingFormat::JsonLines.is_machine_readable());
        assert!(!ListingFormat::DelimitedTable.is_machine_readable());
    }

    #[test]
    fn duration_parsing_is_total_and_never_panics() {
        assert_eq!(parse_duration_ms("00:30:00"), Some(1_800_000));
        assert_eq!(parse_duration_ms("90"), Some(90));
        assert_eq!(parse_duration_ms(""), None);
        assert_eq!(parse_duration_ms(":::"), None);
        assert_eq!(parse_duration_ms("99999999999999999999:00"), None);
        assert_eq!(parse_duration_ms("18446744073709551615:00"), None);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn the_table_parser_never_panics_on_bounded_random_text(
            text in proptest::collection::vec(
                proptest::string::string_regex("[ -~\\t\\n]{0,80}").expect("regex"),
                0..24,
            )
        ) {
            let stdout = text.join("\n");
            let result = parser(ListingFormat::DelimitedTable, false).parse_listing(&stdout);
            if let Ok(page) = result {
                prop_assert!(page.items.len() <= MAX_RECORDS_PER_PAGE);
            }
        }

        #[test]
        fn the_json_parsers_never_panic_on_bounded_random_text(
            text in proptest::string::string_regex("[ -~\\n\"{}\\[\\]:,]{0,400}").expect("regex")
        ) {
            for format in [ListingFormat::JsonArray, ListingFormat::JsonLines] {
                let _ = parser(format, false).parse_listing(&text);
            }
        }

        #[test]
        fn a_parsed_identity_is_always_stable_and_scoped(
            ids in proptest::collection::hash_set("[a-z0-9_]{1,24}", 1..16)
        ) {
            let stdout = ids
                .iter()
                .map(|id| format!("{{\"id\":\"{id}\"}}"))
                .collect::<Vec<_>>()
                .join("\n");
            let page = parser(ListingFormat::JsonLines, false)
                .parse_listing(&stdout)
                .expect("parsed");
            prop_assert_eq!(page.items.len(), ids.len());
            for item in &page.items {
                prop_assert!(item.identity.is_stable());
                prop_assert_eq!(item.identity.source_kind, SourceKind::PlaudCloudCli);
            }
        }
    }
}
