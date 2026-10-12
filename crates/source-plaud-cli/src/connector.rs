//! The PLAUD Cloud CLI connector (spec 7.0-7.3, 4.2).
//!
//! This is the assembly point: discovery, the compatibility table, the
//! hardened runner, the version-locked parser, the error classifier and the
//! acquisition pipeline, driven by the state machine of spec 7.1.
//!
//! Two invariants are worth stating because they are easy to lose in wiring:
//!
//! * **Nothing happens without a user action.** Every method that can launch
//!   the vendor tool takes a [`UserInitiated`], and Local-only Mode denies the
//!   launch underneath that regardless.
//! * **Fail closed on version.** Until [`CliCompatibilityTable::verdict`]
//!   returns a validated row there is no parser, so `list` and `acquire`
//!   return `CliUnsupported` and manual import remains the working path.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use plaudon_domain::capability::{CapabilityStatus, Tri};
use plaudon_domain::media::QualityClass;
use plaudon_domain::source::{ExternalRecordingId, SourceAccountScope, SourceKind};
use plaudon_recording_sources::source::{
    AcquiredAsset, RecordingSource, SourceCapabilities, SourceError, SourceItem, SourcePage,
};
use zeroize::Zeroizing;

use crate::acquire::{AcquisitionPipeline, AcquisitionReport, SignedUrlProvider};
use crate::compatibility::{CliCompatibilityTable, VersionVerdict};
use crate::diagnostics::{ConnectorDiagnostics, DisconnectCopy};
use crate::discovery::{CliDiscovery, DiscoveredCli, DiscoveryError};
use crate::errors::classify;
use crate::invocation::CliCommand;
use crate::parser::{CliVersion, ListingCompleteness, ParsedListing, VersionedParser};
use crate::runner::{CliOutput, CliRunner, RunLimits, UserInitiated};
use crate::state::{ConnectorEvent, ConnectorStatus};

/// Mutable connector state, kept behind one lock so a status read can never
/// observe a half-applied transition.
#[derive(Debug)]
struct Session {
    status: ConnectorStatus,
    program: Option<DiscoveredCli>,
    parser: Option<VersionedParser>,
    logout_supported: bool,
}

/// The official PLAUD Cloud CLI as a [`RecordingSource`].
#[derive(Debug)]
pub struct PlaudCloudCliConnector {
    discovery: CliDiscovery,
    runner: CliRunner,
    table: Arc<CliCompatibilityTable>,
    account_scope: SourceAccountScope,
    pipeline: AcquisitionPipeline,
    session: Mutex<Session>,
}

/// A page of listing results with the disclosure that belongs to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListingResult {
    /// The page itself.
    pub page: SourcePage,
    /// Whether the page may be partial (spec 7.2).
    pub completeness: ListingCompleteness,
    /// Advisory to show when the account looks empty (spec 7.5).
    pub empty_account_advisory: Option<&'static str>,
}

impl PlaudCloudCliConnector {
    /// Build a connector.
    #[must_use]
    pub fn new(
        discovery: CliDiscovery,
        runner: CliRunner,
        pipeline: AcquisitionPipeline,
        account_scope: SourceAccountScope,
        now_ms: i64,
    ) -> Self {
        Self {
            discovery,
            runner,
            table: Arc::new(CliCompatibilityTable::committed().clone()),
            account_scope,
            pipeline,
            session: Mutex::new(Session {
                status: ConnectorStatus::unconfigured(now_ms),
                program: None,
                parser: None,
                logout_supported: false,
            }),
        }
    }

    /// Use a compatibility table other than the committed one.
    ///
    /// The golden-fixture suite uses this to drive the connector against a
    /// recorded release before that release is promoted into the committed
    /// table. The application always uses [`CliCompatibilityTable::committed`].
    #[must_use]
    pub fn with_table(mut self, table: Arc<CliCompatibilityTable>) -> Self {
        self.table = table;
        self
    }

    /// Current persisted status.
    #[must_use]
    pub fn status(&self) -> ConnectorStatus {
        self.lock().status.clone()
    }

    /// Current diagnostics panel.
    #[must_use]
    pub fn diagnostics(&self) -> ConnectorDiagnostics {
        ConnectorDiagnostics::render(&self.status(), &self.table)
    }

    /// The truthful disconnect copy for the detected release.
    #[must_use]
    pub fn disconnect_copy(&self) -> DisconnectCopy {
        DisconnectCopy::for_logout_support(self.lock().logout_supported)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Session> {
        self.session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn apply(&self, event: ConnectorEvent, at_ms: i64) -> ConnectorStatus {
        let mut session = self.lock();
        session.status = session.status.clone().apply(event, at_ms);
        session.status.clone()
    }

    fn record_error(&self, error: SourceError, at_ms: i64) -> SourceError {
        let mut session = self.lock();
        session.status = session.status.clone().with_error(error.clone(), at_ms);
        error
    }

    fn program(&self) -> Result<PathBuf, SourceError> {
        self.lock()
            .program
            .as_ref()
            .map(|found| found.program.clone())
            .ok_or(SourceError::CliMissing)
    }

    /// Step 1 of spec 4.2: find the tool and establish its version.
    ///
    /// `selected` carries a user-approved non-standard location, when the user
    /// picked one; otherwise the fixed candidate list is searched.
    pub async fn connect(
        &self,
        selected: Option<&DiscoveredCli>,
        intent: &UserInitiated,
    ) -> Result<ConnectorStatus, SourceError> {
        let found = match selected {
            Some(found) => found.clone(),
            None => match self.discovery.discover() {
                Ok(found) => found,
                Err(error) => {
                    let mapped = SourceError::from(error.clone());
                    let event = match error {
                        DiscoveryError::NotFound => ConnectorEvent::CliMissing,
                        _ => ConnectorEvent::CliVersionUnvalidated,
                    };
                    self.apply(event, intent.at_ms);
                    return Err(self.record_error(mapped, intent.at_ms));
                }
            },
        };

        let origin = found.origin.label().to_owned();
        let program = found.program.clone();
        self.lock().program = Some(found);

        let output = self
            .runner
            .run(&program, &CliCommand::Version, RunLimits::PROBE, intent)
            .await
            .map_err(SourceError::from)?;
        if let Some(error) = classify(&output, &CliCommand::Version) {
            self.apply(ConnectorEvent::EnumerationFailed, intent.at_ms);
            return Err(self.record_error(error, intent.at_ms));
        }

        let reported = CliVersion::parse_version_output(&output.stdout);
        let verdict = self.table.verdict(reported.as_ref());
        let mut session = self.lock();
        session.status.detected_version = reported.map(|v| v.as_str().to_owned());
        session.status.program_origin = Some(origin);
        match &verdict {
            VersionVerdict::Validated(row) => {
                session.status.version_validated = true;
                session.status.scraping_warning = row.needs_scraping_warning();
                session.status.listing_possibly_incomplete = row.listing_may_be_incomplete();
                session.logout_supported = matches!(row.logout_command, Tri::Yes);
                session.parser = Some(VersionedParser::for_row(row, self.account_scope.clone()));
                session.status = session.status.clone().apply(ConnectorEvent::CliFound, intent.at_ms);
                Ok(session.status.clone())
            }
            VersionVerdict::NotValidated { .. } | VersionVerdict::Unreadable => {
                session.status.version_validated = false;
                session.parser = None;
                session.logout_supported = false;
                session.status = session
                    .status
                    .clone()
                    .apply(ConnectorEvent::CliVersionUnvalidated, intent.at_ms);
                drop(session);
                Err(self.record_error(SourceError::CliUnsupported, intent.at_ms))
            }
        }
    }

    /// Step 3 of spec 4.2: hand authentication to the official tool.
    pub async fn sign_in(&self, intent: &UserInitiated) -> Result<ConnectorStatus, SourceError> {
        let program = self.program()?;
        self.require_validated_version(intent.at_ms)?;
        let output = self
            .runner
            .run(&program, &CliCommand::Login, RunLimits::INTERACTIVE, intent)
            .await
            .map_err(SourceError::from)?;
        if let Some(error) = classify(&output, &CliCommand::Login) {
            let event = if error == SourceError::AuthCanceled {
                ConnectorEvent::LoginCanceled
            } else {
                ConnectorEvent::LoginRequired
            };
            self.apply(event, intent.at_ms);
            return Err(self.record_error(error, intent.at_ms));
        }
        Ok(self.apply(ConnectorEvent::LoginSucceeded, intent.at_ms))
    }

    /// Spec 4.2.7: drop our association, and sign out only if we can.
    pub async fn disconnect(&self, intent: &UserInitiated) -> Result<DisconnectCopy, SourceError> {
        let copy = self.disconnect_copy();
        if copy == DisconnectCopy::WithVendorLogout {
            let program = self.program()?;
            let _ = self
                .runner
                .run(&program, &CliCommand::Logout, RunLimits::INTERACTIVE, intent)
                .await;
        }
        {
            let mut session = self.lock();
            session.program = None;
            session.parser = None;
            session.logout_supported = false;
        }
        self.apply(ConnectorEvent::Disconnected, intent.at_ms);
        Ok(copy)
    }

    /// Enumerate one page, as a user action.
    pub async fn enumerate(
        &self,
        cursor: Option<&str>,
        intent: &UserInitiated,
    ) -> Result<ListingResult, SourceError> {
        let program = self.program()?;
        let parser = self.require_validated_version(intent.at_ms)?;
        let command = match (cursor, self.cursor_flag()) {
            (Some(cursor), Some(flag)) => CliCommand::files_page(flag, cursor),
            // A caller that supplies a cursor for a release with no observed
            // pagination is asking us to guess; spec 7.2 says we do not.
            (Some(_), None) => return Err(self.record_error(SourceError::MalformedOutput, intent.at_ms)),
            (None, _) => CliCommand::files(),
        };

        self.apply(ConnectorEvent::EnumerationStarted, intent.at_ms);
        let output = match self
            .runner
            .run(&program, &command, RunLimits::ENUMERATE, intent)
            .await
        {
            Ok(output) => output,
            Err(error) => {
                self.apply(ConnectorEvent::EnumerationFailed, intent.at_ms);
                return Err(self.record_error(SourceError::from(error), intent.at_ms));
            }
        };
        if let Some(error) = classify(&output, &command) {
            let event = match error {
                SourceError::AuthenticationRequired => ConnectorEvent::SessionExpired,
                _ => ConnectorEvent::EnumerationFailed,
            };
            self.apply(event, intent.at_ms);
            return Err(self.record_error(error, intent.at_ms));
        }

        let page = match parser.parse_listing(&output.stdout) {
            Ok(page) => page,
            Err(error) => {
                self.apply(ConnectorEvent::EnumerationFailed, intent.at_ms);
                return Err(self.record_error(SourceError::from(error), intent.at_ms));
            }
        };
        let completeness = page.completeness;
        let empty = page.items.is_empty();
        let items: Vec<SourceItem> = page.items.iter().map(to_source_item).collect();

        {
            let mut session = self.lock();
            session.status.listing_possibly_incomplete =
                completeness == ListingCompleteness::PossiblyIncomplete;
        }
        self.apply(ConnectorEvent::EnumerationSucceeded, intent.at_ms);

        Ok(ListingResult {
            page: SourcePage {
                items,
                next_cursor: page.next_cursor,
            },
            completeness,
            empty_account_advisory: empty.then(|| crate::diagnostics::empty_listing_advisory().1),
        })
    }

    /// Acquire one item into a staging path, as a user action.
    pub async fn acquire_item(
        &self,
        item: &SourceItem,
        target: &std::path::Path,
        intent: &UserInitiated,
    ) -> Result<AcquisitionReport, SourceError> {
        let program = self.program()?;
        self.require_validated_version(intent.at_ms)?;
        let provider = CliUrlProvider {
            runner: self.runner.clone(),
            program,
            intent: intent.clone(),
        };
        self.pipeline
            .acquire(&provider, item, target)
            .await
            .map_err(|error| self.record_error(error, intent.at_ms))
    }

    /// The parser for the detected release, or `CliUnsupported`.
    fn require_validated_version(&self, at_ms: i64) -> Result<VersionedParser, SourceError> {
        let parser = self.lock().parser.clone();
        parser.ok_or_else(|| self.record_error(SourceError::CliUnsupported, at_ms))
    }

    fn cursor_flag(&self) -> Option<String> {
        None.or_else(|| {
            let version = self.lock().status.detected_version.clone()?;
            self.table
                .versions
                .iter()
                .find(|row| row.version == version)
                .and_then(|row| match &row.pagination {
                    crate::compatibility::PaginationEvidence::Observed { cursor_flag, .. } => {
                        Some(cursor_flag.clone())
                    }
                    crate::compatibility::PaginationEvidence::NotObserved => None,
                })
        })
    }
}

/// Convert a parsed record into the generic source item.
fn to_source_item(listing: &ParsedListing) -> SourceItem {
    SourceItem {
        identity: listing.identity.clone(),
        display_name: listing.display_name.clone(),
        captured_at: listing
            .captured_at_ms
            .map(plaudon_domain::time::Timestamp::from_millis),
        duration_ms: listing.duration_ms,
        declared_byte_length: listing.declared_byte_length,
        remote_version: listing.remote_version.clone(),
        // Spec 7.0: a cloud export is a vendor cloud export, never an
        // "original device recording", whatever the vendor calls it.
        quality_class: QualityClass::VendorCloudExport,
    }
}

/// Asks the vendor tool for a transient URL at the moment it is needed.
#[derive(Debug, Clone)]
struct CliUrlProvider {
    runner: CliRunner,
    program: PathBuf,
    intent: UserInitiated,
}

#[async_trait]
impl SignedUrlProvider for CliUrlProvider {
    async fn fresh_url(
        &self,
        external_id: &ExternalRecordingId,
    ) -> Result<Zeroizing<String>, SourceError> {
        let command = CliCommand::Audio {
            external_id: external_id.as_str().to_owned(),
        };
        let output = self
            .runner
            .run(&self.program, &command, RunLimits::PROBE, &self.intent)
            .await
            .map_err(SourceError::from)?;
        if let Some(error) = classify(&output, &command) {
            return Err(error);
        }
        extract_url(&output).ok_or(SourceError::MalformedOutput)
    }
}

/// Pull the first URL out of `plaud audio` output.
///
/// The value is moved straight into a [`Zeroizing`] buffer and the owning
/// [`CliOutput`] is dropped by the caller; it is never logged, never returned
/// to the UI and never written to the database (spec 7.3).
fn extract_url(output: &CliOutput) -> Option<Zeroizing<String>> {
    const TRIM: &[char] = &['"', '\'', ',', '.', '<', '>', '(', ')', '[', ']', ';'];
    output
        .stdout
        .split_whitespace()
        .map(|token| token.trim_matches(TRIM))
        .find(|token| token.starts_with("https://") || token.starts_with("http://"))
        .map(|token| Zeroizing::new(token.to_owned()))
}

#[async_trait]
impl RecordingSource for PlaudCloudCliConnector {
    async fn capabilities(&self) -> SourceCapabilities {
        let session = self.lock();
        let version = session.status.detected_version.clone();
        drop(session);
        version
            .and_then(|version| {
                self.table
                    .versions
                    .iter()
                    .find(|row| row.version == version)
            })
            .map_or_else(
                || {
                    let mut unknown = SourceCapabilities::unknown(SourceKind::PlaudCloudCli);
                    // Spec 2.1: nothing about this connector is assumed. An
                    // unvalidated release claims nothing and is not selectable.
                    unknown.status = CapabilityStatus::Unsupported;
                    unknown
                },
                crate::compatibility::CliCompatibilityRow::capabilities,
            )
    }

    async fn list(&self, cursor: Option<&str>) -> Result<SourcePage, SourceError> {
        // The trait has no place for a user-intent token, so the typed entry
        // point is `enumerate`. Reaching this one means a generic caller
        // iterated sources; it is still a user-initiated refresh, recorded as
        // such.
        let intent = UserInitiated::new("refresh_sources", now_ms());
        self.enumerate(cursor, &intent).await.map(|result| result.page)
    }

    async fn acquire(
        &self,
        item: &SourceItem,
        target: &std::path::Path,
    ) -> Result<AcquiredAsset, SourceError> {
        let intent = UserInitiated::new("import_recording", now_ms());
        self.acquire_item(item, target, &intent)
            .await
            .map(|report| report.asset)
    }
}

/// Wall-clock milliseconds, for intent records written by trait methods.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn output(stdout: &str) -> CliOutput {
        CliOutput {
            stdout: stdout.to_owned(),
            stderr_excerpt: String::new(),
            exit_code: Some(0),
            elapsed: Duration::from_millis(1),
        }
    }

    #[test]
    fn a_signed_url_is_extracted_without_being_logged() {
        let extracted = extract_url(&output(
            "Download URL (valid 24h):\nhttps://cdn.example-vendor.com/a.wav?sig=abc\n",
        ))
        .expect("url");
        assert_eq!(&*extracted, "https://cdn.example-vendor.com/a.wav?sig=abc");
    }

    #[test]
    fn trailing_punctuation_is_trimmed_from_an_extracted_url() {
        let extracted = extract_url(&output("url: \"https://cdn.example.com/a.wav\"")).expect("url");
        assert_eq!(&*extracted, "https://cdn.example.com/a.wav");
    }

    #[test]
    fn output_without_a_url_yields_nothing_to_guess_from() {
        assert!(extract_url(&output("no link here")).is_none());
        assert!(extract_url(&output("")).is_none());
    }

    #[test]
    fn a_parsed_record_becomes_a_vendor_cloud_export_item() {
        let listing = ParsedListing {
            identity: plaudon_domain::source::SourceIdentityKey::new(
                SourceKind::PlaudCloudCli,
                SourceAccountScope::new("account-a"),
                Some(ExternalRecordingId::new("rec_1")),
            ),
            display_name: "Board sync".to_owned(),
            duration_ms: Some(1000),
            declared_byte_length: Some(2048),
            captured_at_ms: Some(1_700_000_000_000),
            remote_version: Some("7".to_owned()),
            vendor_transcript_available: true,
        };
        let item = to_source_item(&listing);
        assert_eq!(item.quality_class, QualityClass::VendorCloudExport);
        assert_eq!(item.declared_byte_length, Some(2048));
        assert_eq!(item.remote_version.as_deref(), Some("7"));
    }
}
