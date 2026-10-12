//! The tested-version compatibility table of spec 7.1.
//!
//! Spec 4.2 step 1 requires us to establish, before anything else, whether the
//! user-installed `plaud` command meets a version range we have actually
//! tested. Spec 7.1 requires that range to be a maintained table rather than a
//! comparison against a floor, because the thing that breaks between CLI
//! releases is the *output contract*, not a semantic-version ordering: a newer
//! release is no more likely to be parseable than an older one.
//!
//! The table therefore matches exact release strings and fails closed. A
//! version outside it produces [`VersionVerdict::NotValidated`], which the
//! connector turns into `INCOMPATIBLE_VERSION`, which in turn blocks
//! enumeration and download while leaving manual import untouched.

use std::sync::LazyLock;

use plaudon_domain::capability::{CapabilityStatus, Known, Tri};
use plaudon_domain::source::SourceKind;
use plaudon_recording_sources::source::{SourceCapabilities, SourceError};
use serde::{Deserialize, Serialize};

use crate::parser::{CliVersion, ListingFormat};

/// The committed table, compiled into the binary.
const TABLE_DOCUMENT: &str = include_str!("../cli-compatibility.yaml");

/// How well a row is evidenced (mirrors the device register of spec 2.5B).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CliEvidenceLevel {
    /// The exact release was installed and its output captured.
    ObservedOnIdentifiedRelease,
    /// Implemented against vendor documentation, never run.
    ImplementedNotVerified,
    /// Described in documentation only.
    Proposed,
    /// Known not to work.
    KnownUnsupported,
}

impl CliEvidenceLevel {
    /// Whether a row at this evidence level may drive real enumeration.
    #[must_use]
    pub const fn permits_use(self) -> bool {
        matches!(self, Self::ObservedOnIdentifiedRelease)
    }
}

/// What we observed about pagination for one release (spec 7.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PaginationEvidence {
    /// A cursor flag and page size were observed in a real session.
    Observed {
        /// Flag that carries the cursor, verbatim.
        cursor_flag: String,
        /// Items returned per page.
        page_size: u32,
    },
    /// Pagination was not observed. Enumeration is single-page and labelled
    /// possibly incomplete; we never guess a cursor flag.
    NotObserved,
}

impl PaginationEvidence {
    /// Whether the connector may request a second page.
    #[must_use]
    pub const fn is_observed(&self) -> bool {
        matches!(self, Self::Observed { .. })
    }
}

/// One tested release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliCompatibilityRow {
    /// Exact release string as the CLI prints it.
    pub version: String,
    /// Output format `plaud files` produces on this release.
    pub listing_format: ListingFormat,
    /// Observed pagination behaviour.
    pub pagination: PaginationEvidence,
    /// Whether this release exposes vendor transcripts or summaries (spec 7.5).
    #[serde(default)]
    pub vendor_artifacts: Tri,
    /// Whether this release has a supported logout command (spec 4.2.7).
    #[serde(default)]
    pub logout_command: Tri,
    /// Whether `plaud files` output is a documented machine-readable format
    /// rather than a scraped human-readable table (spec 7.2).
    #[serde(default)]
    pub machine_readable: Tri,
    /// Evidence backing this row.
    pub evidence_level: CliEvidenceLevel,
    /// Golden fixture directory name under
    /// `tests/integration/fixtures/plaud-cli/`.
    pub fixture_id: String,
    /// SHA-256 of the fixture's captured `files.stdout`.
    pub fixture_sha256: String,
    /// Date the release was exercised, `YYYY-MM-DD`.
    pub verified_on: Option<String>,
    /// Host operating systems the release was exercised on.
    #[serde(default)]
    pub host_os: Vec<String>,
}

/// Why a table or row was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TableError {
    /// The document did not parse.
    #[error("the compatibility table could not be parsed: {0}")]
    Malformed(String),
    /// The document declares a schema this build does not understand.
    #[error("the compatibility table declares unsupported schema version {found}")]
    UnsupportedSchema {
        /// Schema version found in the document.
        found: u32,
    },
    /// A row is internally inconsistent.
    #[error("compatibility row {version}: {reason}")]
    InvalidRow {
        /// Version the row describes.
        version: String,
        /// Why it was rejected.
        reason: &'static str,
    },
    /// Two rows describe the same release.
    #[error("compatibility row {version} is declared twice")]
    DuplicateVersion {
        /// The duplicated version.
        version: String,
    },
}

impl CliCompatibilityRow {
    /// Check the promotion rules documented in `cli-compatibility.yaml`.
    pub fn validate(&self) -> Result<(), TableError> {
        let reject = |reason: &'static str| {
            Err(TableError::InvalidRow {
                version: self.version.clone(),
                reason,
            })
        };
        if CliVersion::parse_version_output(&self.version).is_none() {
            return reject("version is not an exact dotted release string");
        }
        if self.fixture_id.is_empty()
            || !self
                .fixture_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        {
            return reject("fixture_id must be a plain directory name");
        }
        if self.fixture_sha256.len() != 64
            || !self.fixture_sha256.chars().all(|c| c.is_ascii_hexdigit())
        {
            return reject("fixture_sha256 must be a 64-character hex digest");
        }
        if let PaginationEvidence::Observed {
            cursor_flag,
            page_size,
        } = &self.pagination
        {
            if cursor_flag.is_empty() || *page_size == 0 {
                return reject("observed pagination needs a cursor flag and a page size");
            }
        }
        if self.evidence_level == CliEvidenceLevel::ObservedOnIdentifiedRelease
            && (self.verified_on.is_none() || self.host_os.is_empty())
        {
            return reject("an observed release needs verified_on and at least one host_os");
        }
        Ok(())
    }

    /// Capabilities this release grants, expressed in the spec 6.6 contract.
    #[must_use]
    pub fn capabilities(&self) -> SourceCapabilities {
        SourceCapabilities {
            payload_version: SourceCapabilities::SUPPORTED_PAYLOAD_VERSION,
            status: if self.evidence_level.permits_use() {
                CapabilityStatus::Supported
            } else {
                CapabilityStatus::Experimental
            },
            source_kind: SourceKind::PlaudCloudCli,
            supports_listing: Tri::Yes,
            supports_pagination: if self.pagination.is_observed() {
                Tri::Yes
            } else {
                Tri::No
            },
            supports_stable_external_ids: Tri::Yes,
            supports_remote_versions: Tri::Unknown,
            // Spec 7.3: resume stays off until range semantics and validators
            // are verified against the download host, which is a property of
            // the host rather than of the CLI release.
            supports_resume: Tri::No,
            supports_vendor_artifacts: self.vendor_artifacts,
            page_size: match &self.pagination {
                PaginationEvidence::Observed { page_size, .. } => Known::Known(*page_size),
                PaginationEvidence::NotObserved => Known::Unknown,
            },
        }
    }

    /// Whether enumeration results from this release must be labelled as
    /// possibly incomplete (spec 7.2: no guessed pagination).
    #[must_use]
    pub const fn listing_may_be_incomplete(&self) -> bool {
        !self.pagination.is_observed()
    }

    /// Whether the parser for this release scrapes a human-readable layout and
    /// therefore needs the spec 7.2 compatibility warning.
    #[must_use]
    pub fn needs_scraping_warning(&self) -> bool {
        !matches!(self.machine_readable, Tri::Yes)
    }
}

/// The committed set of tested releases.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliCompatibilityTable {
    /// Schema version of the document.
    pub schema_version: u32,
    /// Date the document was last revised.
    pub updated_at: String,
    /// Tested releases, in no particular order.
    #[serde(default)]
    pub versions: Vec<CliCompatibilityRow>,
}

/// The verdict on a version the CLI reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionVerdict {
    /// The release is in the table and may be driven.
    Validated(Box<CliCompatibilityRow>),
    /// The release parsed but is not in the table.
    NotValidated {
        /// Version string as reported.
        reported: String,
    },
    /// `plaud --version` produced output we could not read as a version.
    Unreadable,
}

impl VersionVerdict {
    /// The validated row, when there is one.
    #[must_use]
    pub fn row(&self) -> Option<&CliCompatibilityRow> {
        match self {
            Self::Validated(row) => Some(row),
            Self::NotValidated { .. } | Self::Unreadable => None,
        }
    }

    /// Whether enumeration and download may proceed.
    #[must_use]
    pub const fn permits_use(&self) -> bool {
        matches!(self, Self::Validated(_))
    }

    /// The stable source error this verdict reports, if any (spec 7.6).
    #[must_use]
    pub const fn as_source_error(&self) -> Option<SourceError> {
        match self {
            Self::Validated(_) => None,
            Self::NotValidated { .. } | Self::Unreadable => Some(SourceError::CliUnsupported),
        }
    }
}

impl CliCompatibilityTable {
    /// Schema version this build understands.
    pub const SUPPORTED_SCHEMA_VERSION: u32 = 1;

    /// Parse and validate a table document.
    pub fn from_yaml(document: &str) -> Result<Self, TableError> {
        let table: Self =
            serde_norway::from_str(document).map_err(|e| TableError::Malformed(e.to_string()))?;
        table.validate()?;
        Ok(table)
    }

    /// The table compiled into this build.
    ///
    /// Parsed once; a malformed committed table is a build-time defect caught
    /// by [`crate::compatibility::tests`], so this cannot fail silently at
    /// runtime.
    #[must_use]
    pub fn committed() -> &'static Self {
        static TABLE: LazyLock<CliCompatibilityTable> = LazyLock::new(|| {
            CliCompatibilityTable::from_yaml(TABLE_DOCUMENT)
                .expect("the committed compatibility table must parse")
        });
        &TABLE
    }

    /// Check schema version, row validity and uniqueness.
    pub fn validate(&self) -> Result<(), TableError> {
        if self.schema_version != Self::SUPPORTED_SCHEMA_VERSION {
            return Err(TableError::UnsupportedSchema {
                found: self.schema_version,
            });
        }
        let mut seen: Vec<&str> = Vec::with_capacity(self.versions.len());
        for row in &self.versions {
            row.validate()?;
            if seen.contains(&row.version.as_str()) {
                return Err(TableError::DuplicateVersion {
                    version: row.version.clone(),
                });
            }
            seen.push(&row.version);
        }
        Ok(())
    }

    /// Decide what to do with a version the CLI reported.
    ///
    /// Matching is exact. A release one patch newer than a tested one is *not*
    /// validated, because spec 7.2 forbids falling back to a neighbouring
    /// version's parser.
    #[must_use]
    pub fn verdict(&self, reported: Option<&CliVersion>) -> VersionVerdict {
        let Some(version) = reported else {
            return VersionVerdict::Unreadable;
        };
        self.versions
            .iter()
            .find(|row| row.version == version.as_str() && row.evidence_level.permits_use())
            .map_or_else(
                || VersionVerdict::NotValidated {
                    reported: version.as_str().to_owned(),
                },
                |row| VersionVerdict::Validated(Box::new(row.clone())),
            )
    }

    /// Versions a user could install to get a working connector, for the
    /// diagnostics panel. Empty while nothing has been validated.
    #[must_use]
    pub fn usable_versions(&self) -> Vec<&str> {
        let mut out: Vec<&str> = self
            .versions
            .iter()
            .filter(|row| row.evidence_level.permits_use())
            .map(|row| row.version.as_str())
            .collect();
        out.sort_unstable();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observed_row(version: &str) -> CliCompatibilityRow {
        CliCompatibilityRow {
            version: version.to_owned(),
            listing_format: ListingFormat::JsonLines,
            pagination: PaginationEvidence::NotObserved,
            vendor_artifacts: Tri::No,
            logout_command: Tri::No,
            machine_readable: Tri::Yes,
            evidence_level: CliEvidenceLevel::ObservedOnIdentifiedRelease,
            fixture_id: format!("v{version}"),
            fixture_sha256: "a".repeat(64),
            verified_on: Some("2026-10-11".to_owned()),
            host_os: vec!["macos".to_owned()],
        }
    }

    fn table(rows: Vec<CliCompatibilityRow>) -> CliCompatibilityTable {
        CliCompatibilityTable {
            schema_version: 1,
            updated_at: "2026-10-11".to_owned(),
            versions: rows,
        }
    }

    #[test]
    fn the_committed_table_parses_and_validates() {
        let committed = CliCompatibilityTable::committed();
        assert_eq!(
            committed.schema_version,
            CliCompatibilityTable::SUPPORTED_SCHEMA_VERSION
        );
        committed.validate().expect("committed table is valid");
    }

    #[test]
    fn the_committed_table_declares_no_usable_release_yet() {
        // Spec 2.1: we have not been permitted to assume an output contract,
        // and no release has been exercised. This assertion is what makes the
        // connector fail closed rather than guess; a future validated row is
        // expected to change it deliberately, together with a golden fixture.
        assert!(CliCompatibilityTable::committed().usable_versions().is_empty());
    }

    #[test]
    fn an_unknown_version_is_not_validated() {
        let t = table(vec![observed_row("1.4.2")]);
        let verdict = t.verdict(Some(&CliVersion::new("9.9.9")));
        assert_eq!(
            verdict,
            VersionVerdict::NotValidated {
                reported: "9.9.9".to_owned()
            }
        );
        assert_eq!(verdict.as_source_error(), Some(SourceError::CliUnsupported));
    }

    #[test]
    fn a_neighbouring_version_never_borrows_a_validated_parser() {
        let t = table(vec![observed_row("1.4.2")]);
        for neighbour in ["1.4.1", "1.4.3", "1.4", "1.4.2.1", "1.4.2-beta"] {
            assert!(
                !t.verdict(Some(&CliVersion::new(neighbour))).permits_use(),
                "{neighbour} must not reuse the 1.4.2 parser"
            );
        }
    }

    #[test]
    fn an_exact_validated_version_is_usable() {
        let t = table(vec![observed_row("1.4.2")]);
        let verdict = t.verdict(Some(&CliVersion::new("1.4.2")));
        assert!(verdict.permits_use());
        assert_eq!(verdict.row().map(|r| r.listing_format), Some(ListingFormat::JsonLines));
        assert_eq!(verdict.as_source_error(), None);
    }

    #[test]
    fn an_unreadable_version_fails_closed() {
        let t = table(vec![observed_row("1.4.2")]);
        let verdict = t.verdict(None);
        assert_eq!(verdict, VersionVerdict::Unreadable);
        assert_eq!(verdict.as_source_error(), Some(SourceError::CliUnsupported));
    }

    #[test]
    fn a_row_that_was_never_observed_cannot_be_driven() {
        let mut row = observed_row("1.4.2");
        row.evidence_level = CliEvidenceLevel::ImplementedNotVerified;
        row.verified_on = None;
        row.host_os.clear();
        let t = table(vec![row]);
        assert!(!t.verdict(Some(&CliVersion::new("1.4.2"))).permits_use());
        assert!(t.usable_versions().is_empty());
    }

    #[test]
    fn duplicate_rows_are_rejected() {
        let t = table(vec![observed_row("1.4.2"), observed_row("1.4.2")]);
        assert_eq!(
            t.validate().unwrap_err(),
            TableError::DuplicateVersion {
                version: "1.4.2".to_owned()
            }
        );
    }

    #[test]
    fn a_future_schema_version_is_rejected_rather_than_guessed() {
        let mut t = table(vec![]);
        t.schema_version = 2;
        assert_eq!(
            t.validate().unwrap_err(),
            TableError::UnsupportedSchema { found: 2 }
        );
    }

    #[test]
    fn promotion_rules_are_enforced_per_row() {
        let cases: Vec<(&'static str, Box<dyn Fn(&mut CliCompatibilityRow)>)> = vec![
            ("version", Box::new(|r: &mut CliCompatibilityRow| r.version = "latest".to_owned())),
            ("fixture", Box::new(|r: &mut CliCompatibilityRow| r.fixture_id = "../escape".to_owned())),
            ("digest", Box::new(|r: &mut CliCompatibilityRow| r.fixture_sha256 = "short".to_owned())),
            (
                "pagination",
                Box::new(|r: &mut CliCompatibilityRow| {
                    r.pagination = PaginationEvidence::Observed {
                        cursor_flag: String::new(),
                        page_size: 0,
                    };
                }),
            ),
            ("evidence", Box::new(|r: &mut CliCompatibilityRow| r.verified_on = None)),
        ];
        for (name, mutate) in cases {
            let mut row = observed_row("1.4.2");
            mutate(&mut row);
            assert!(row.validate().is_err(), "{name} should have been rejected");
        }
    }

    #[test]
    fn unobserved_pagination_labels_the_listing_incomplete() {
        let row = observed_row("1.4.2");
        assert!(row.listing_may_be_incomplete());
        assert_eq!(row.capabilities().supports_pagination, Tri::No);
        assert_eq!(row.capabilities().page_size, Known::Unknown);
    }

    #[test]
    fn observed_pagination_is_reported_as_a_capability() {
        let mut row = observed_row("1.4.2");
        row.pagination = PaginationEvidence::Observed {
            cursor_flag: "--cursor".to_owned(),
            page_size: 50,
        };
        assert!(!row.listing_may_be_incomplete());
        assert_eq!(row.capabilities().supports_pagination, Tri::Yes);
        assert_eq!(row.capabilities().page_size, Known::Known(50));
    }

    #[test]
    fn a_scraped_layout_requires_the_compatibility_warning() {
        let mut row = observed_row("1.4.2");
        row.machine_readable = Tri::No;
        row.listing_format = ListingFormat::DelimitedTable;
        assert!(row.needs_scraping_warning());
        row.machine_readable = Tri::Yes;
        assert!(!row.needs_scraping_warning());
    }

    #[test]
    fn resume_is_never_claimed_by_a_cli_release() {
        assert_eq!(observed_row("1.4.2").capabilities().supports_resume, Tri::No);
    }
}
