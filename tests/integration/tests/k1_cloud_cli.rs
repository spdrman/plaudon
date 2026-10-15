//! EPIC K phase 1: the official PLAUD Cloud CLI connector, end to end.
//!
//! Hermetic by construction (spec 18.3, 18.8): the vendor tool is a mock
//! executable copied into a temporary directory under the pinned leaf name,
//! and the signed download URL points at an in-process loopback listener. No
//! network, no vendor account, no real CLI.
//!
//! The golden-snapshot half of K1.3 lives here too: every release described in
//! the fixture compatibility table must parse to exactly its recorded
//! `expected.json`, and the captured stdout must still hash to the digest the
//! table claims.

#![allow(clippy::too_many_lines)]

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use plaudon_app_core::import::{ImportOutcome, ImportRequest, ImportService};
use plaudon_domain::media::QualityClass;
use plaudon_domain::source::{ExternalRecordingId, SourceAccountScope, SourceKind};
use plaudon_domain::time::Timestamp;
use plaudon_persistence::archive::MediaArchive;
use plaudon_persistence::migrations::MigrationRunner;
use plaudon_persistence::Database;
use plaudon_recording_sources::source::{SourceError, SourceItem};
use plaudon_security::outbound::subprocess::GatedProcessLauncher;
use plaudon_security::outbound::{ApprovalSource, NetworkGrant, OutboundPolicy};
use plaudon_source_plaud_cli::acquire::AcquisitionPipeline;
use plaudon_source_plaud_cli::compatibility::CliCompatibilityTable;
use plaudon_source_plaud_cli::connector::PlaudCloudCliConnector;
use plaudon_source_plaud_cli::diagnostics::{DisconnectCopy, NextAction};
use plaudon_source_plaud_cli::discovery::{CliDiscovery, DiscoveredCli, DiscoveryOrigin};
use plaudon_source_plaud_cli::download::{DownloadPolicy, UrlContract};
use plaudon_source_plaud_cli::parser::{
    CliVersion, ListingCompleteness, ListingFormat, VersionedParser,
};
use plaudon_source_plaud_cli::runner::{CliRunner, UserInitiated};
use plaudon_source_plaud_cli::state::ConnectorState;
use plaudon_source_plaud_cli::transport::ReqwestFetcher;
use sha2::{Digest, Sha256};

// ---------------------------------------------------------------- fixtures

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/plaud-cli")
}

fn fixture_table() -> Arc<CliCompatibilityTable> {
    let document = std::fs::read_to_string(fixture_root().join("cli-compatibility.fixture.yaml"))
        .expect("read fixture table");
    Arc::new(CliCompatibilityTable::from_yaml(&document).expect("fixture table parses"))
}

#[derive(Debug, serde::Deserialize)]
struct FixtureMeta {
    version: String,
    listing_format: ListingFormat,
    pagination_observed: bool,
    account_scope: String,
}

#[derive(Debug, serde::Deserialize)]
struct ExpectedItem {
    external_id: String,
    display_name: String,
    duration_ms: Option<u64>,
    declared_byte_length: Option<u64>,
    captured_at_ms: Option<i64>,
    remote_version: Option<String>,
    vendor_transcript_available: bool,
}

#[derive(Debug, serde::Deserialize)]
struct ExpectedPage {
    completeness: String,
    next_cursor: Option<String>,
    items: Vec<ExpectedItem>,
}

fn load_fixture(id: &str) -> (FixtureMeta, String, ExpectedPage) {
    let dir = fixture_root().join(id);
    let meta: FixtureMeta =
        serde_json::from_str(&std::fs::read_to_string(dir.join("fixture.json")).expect("meta"))
            .expect("meta parses");
    let stdout = std::fs::read_to_string(dir.join("files.stdout")).expect("stdout");
    let expected: ExpectedPage = serde_json::from_str(
        &std::fs::read_to_string(dir.join("expected.json")).expect("expected"),
    )
    .expect("expected parses");
    (meta, stdout, expected)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

// ------------------------------------------------------------- mock vendor

/// A mock vendor CLI installed under the pinned leaf name.
struct MockCli {
    dir: tempfile::TempDir,
    program: PathBuf,
}

impl MockCli {
    fn install(script: &serde_json::Value) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let leaf = if cfg!(windows) { "plaud.exe" } else { "plaud" };
        let program = dir.path().join(leaf);
        std::fs::copy(env!("CARGO_BIN_EXE_mock_plaud"), &program).expect("copy mock");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
                .expect("chmod");
        }
        std::fs::write(
            dir.path().join("plaud.mock.json"),
            serde_json::to_vec_pretty(&script).expect("script"),
        )
        .expect("write script");
        Self { dir, program }
    }

    fn discovered(&self) -> DiscoveredCli {
        DiscoveredCli {
            program: self.program.clone(),
            origin: DiscoveryOrigin::WellKnownLocation,
            approval: None,
        }
    }

    fn invocations(&self) -> Vec<Vec<String>> {
        std::fs::read_to_string(self.dir.path().join("plaud.invocations.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).expect("invocation"))
            .collect()
    }
}

// ------------------------------------------------------------ loopback host

/// Bytes a loopback listener serves, and how many times it has been asked.
struct LoopbackHost {
    port: u16,
    requests: Arc<std::sync::atomic::AtomicUsize>,
}

/// Serve a fixed sequence of raw HTTP responses, one per connection.
///
/// Responses are raw bytes so a test can produce an expired-link status, a
/// wrong content type or a short body without a web framework.
fn serve(responses: Vec<Vec<u8>>) -> LoopbackHost {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = Arc::clone(&requests);
    std::thread::spawn(move || {
        for (index, mut stream) in listener.incoming().flatten().enumerate() {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            // Read the request head so the client does not see a reset.
            let mut buffer = [0u8; 2048];
            let _ = stream.read(&mut buffer);
            let response = responses
                .get(index)
                .or_else(|| responses.last())
                .cloned()
                .unwrap_or_default();
            let _ = stream.write_all(&response);
            let _ = stream.flush();
        }
    });
    LoopbackHost { port, requests }
}

fn http_response(status: &str, content_type: &str, body: &[u8]) -> Vec<u8> {
    let mut out = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    out.extend_from_slice(body);
    out
}

fn wav_bytes(payload: usize) -> Vec<u8> {
    let data_len = u32::try_from(payload).expect("payload fits");
    let riff_len = 36 + data_len;
    let mut bytes = Vec::with_capacity(44 + payload);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&riff_len.to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&16_000u32.to_le_bytes());
    bytes.extend_from_slice(&32_000u32.to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    bytes.resize(44 + payload, 0x11);
    bytes
}

// ----------------------------------------------------------------- wiring

fn permitted_runner() -> CliRunner {
    let policy = OutboundPolicy::network_permitted(NetworkGrant::new(
        ApprovalSource::TestHarness,
        0,
        "privacy-v1",
    ));
    CliRunner::new(
        GatedProcessLauncher::new(Arc::new(policy)),
        Arc::new(plaudon_diagnostics::redact::Redactor::new()),
    )
}

fn local_only_runner() -> CliRunner {
    CliRunner::new(
        GatedProcessLauncher::new(Arc::new(OutboundPolicy::local_only())),
        Arc::new(plaudon_diagnostics::redact::Redactor::new()),
    )
}

fn connector(runner: CliRunner) -> PlaudCloudCliConnector {
    let policy = Arc::new(DownloadPolicy::loopback_harness(&["127.0.0.1"]));
    let fetcher = Arc::new(ReqwestFetcher::new(Arc::clone(&policy)).expect("fetcher"));
    let pipeline =
        AcquisitionPipeline::new(policy, fetcher, UrlContract::undocumented("127.0.0.1"));
    PlaudCloudCliConnector::new(
        CliDiscovery::new(),
        runner,
        pipeline,
        SourceAccountScope::new("plaud:eu:account-a"),
        0,
    )
    .with_table(fixture_table())
}

/// A connector driven by the table the application actually ships, so the
/// committed row for `@plaud-ai/cli` 0.3.15 is exercised end to end rather
/// than only through its golden snapshot.
fn shipped_connector(runner: CliRunner) -> PlaudCloudCliConnector {
    let policy = Arc::new(DownloadPolicy::loopback_harness(&["127.0.0.1"]));
    let fetcher = Arc::new(ReqwestFetcher::new(Arc::clone(&policy)).expect("fetcher"));
    let pipeline =
        AcquisitionPipeline::new(policy, fetcher, UrlContract::undocumented("127.0.0.1"));
    PlaudCloudCliConnector::new(
        CliDiscovery::new(),
        runner,
        pipeline,
        SourceAccountScope::new("plaud:eu:account-a"),
        0,
    )
}

fn intent() -> UserInitiated {
    UserInitiated::new("connect_plaud_account", 1_760_000_000_000)
}

fn jsonl_script(port: Option<u16>) -> serde_json::Value {
    let (_, stdout, _) = load_fixture("v1.4.2-jsonl");
    let mut script = serde_json::json!({
        "version": { "stdout": "plaud/1.4.2\n" },
        "login": { "stdout": "Signed in as user@example.com\n" },
        "files": { "stdout": stdout },
        "record_invocations": true,
        "unknown_exit_code": 64,
    });
    if let Some(port) = port {
        script["audio_url_template"] = serde_json::json!(format!(
            "http://127.0.0.1:{port}/{{id}}.bin?sig=deadbeefcafe"
        ));
    }
    script
}

// ------------------------------------------------------- K1.3 golden tests

#[test]
fn golden_snapshots_match_the_parser_for_every_tested_release() {
    let table = fixture_table();
    let mut covered = 0usize;
    for row in &table.versions {
        if !matches!(
            row.evidence_level,
            plaudon_source_plaud_cli::compatibility::CliEvidenceLevel::ObservedOnIdentifiedRelease
        ) {
            continue;
        }
        let (meta, stdout, expected) = load_fixture(&row.fixture_id);
        assert_eq!(meta.version, row.version, "fixture/table version drift");
        assert_eq!(
            meta.listing_format, row.listing_format,
            "fixture/table format drift for {}",
            row.version
        );
        assert_eq!(
            sha256_hex(stdout.as_bytes()),
            row.fixture_sha256,
            "captured stdout for {} no longer matches the recorded digest",
            row.version
        );

        let parser = VersionedParser::for_row(row, SourceAccountScope::new(&meta.account_scope));
        let page = parser.parse_listing(&stdout).expect("golden parses");

        assert_eq!(
            match page.completeness {
                ListingCompleteness::Complete => "complete",
                ListingCompleteness::PossiblyIncomplete => "possibly_incomplete",
            },
            expected.completeness,
            "{}",
            row.version
        );
        assert_eq!(page.next_cursor, expected.next_cursor, "{}", row.version);
        assert_eq!(page.items.len(), expected.items.len(), "{}", row.version);
        for (actual, want) in page.items.iter().zip(&expected.items) {
            assert_eq!(
                actual
                    .identity
                    .external_id
                    .as_ref()
                    .map(ToString::to_string),
                Some(want.external_id.clone())
            );
            assert_eq!(actual.identity.source_kind, SourceKind::PlaudCloudCli);
            assert_eq!(
                actual.identity.account_scope,
                SourceAccountScope::new(&meta.account_scope)
            );
            assert_eq!(actual.display_name, want.display_name);
            assert_eq!(actual.duration_ms, want.duration_ms);
            assert_eq!(actual.declared_byte_length, want.declared_byte_length);
            assert_eq!(actual.captured_at_ms, want.captured_at_ms);
            assert_eq!(actual.remote_version, want.remote_version);
            assert_eq!(
                actual.vendor_transcript_available,
                want.vendor_transcript_available
            );
        }
        assert_eq!(parser.format(), meta.listing_format);
        assert_eq!(
            row.listing_may_be_incomplete(),
            !meta.pagination_observed,
            "{}",
            row.version
        );
        covered += 1;
    }
    assert_eq!(
        covered, 3,
        "every observed fixture release must be exercised"
    );
}

#[test]
fn a_regressed_parser_fails_the_golden_snapshot() {
    // Mutation sanity check: reading a release with a neighbouring release's
    // format must not quietly produce a plausible listing.
    let (meta, stdout, _) = load_fixture("v1.4.2-jsonl");
    let wrong = VersionedParser::candidate(
        CliVersion::new(&meta.version),
        ListingFormat::DelimitedTable,
        SourceAccountScope::new(&meta.account_scope),
        false,
    );
    assert!(
        wrong.parse_listing(&stdout).is_err(),
        "a mismatched format must fail rather than scrape JSON as a table"
    );

    let (table_meta, table_stdout, table_expected) = load_fixture("v1.2.0-table");
    let wrong = VersionedParser::candidate(
        CliVersion::new(&table_meta.version),
        ListingFormat::JsonLines,
        SourceAccountScope::new(&table_meta.account_scope),
        false,
    );
    assert!(wrong.parse_listing(&table_stdout).is_err());
    assert_eq!(table_expected.items.len(), 2);
}

#[test]
fn negative_fixtures_never_produce_a_partial_listing() {
    let scope = SourceAccountScope::new("plaud:eu:account-a");
    let jsonl = VersionedParser::candidate(
        CliVersion::new("1.4.2"),
        ListingFormat::JsonLines,
        scope.clone(),
        false,
    );
    let array = VersionedParser::candidate(
        CliVersion::new("1.5.0"),
        ListingFormat::JsonArray,
        scope.clone(),
        true,
    );
    let table = VersionedParser::candidate(
        CliVersion::new("1.2.0"),
        ListingFormat::DelimitedTable,
        scope.clone(),
        false,
    );
    let plaud = VersionedParser::candidate(
        CliVersion::new("0.3.15"),
        ListingFormat::PlaudTable03,
        scope,
        true,
    );
    let negative = fixture_root().join("negative");

    for (file, parser) in [
        ("duplicate-ids.stdout", &jsonl),
        ("unknown-field.stdout", &jsonl),
        ("blank-id.stdout", &jsonl),
        ("truncated.stdout", &array),
        ("empty.stdout", &table),
        ("table-row-shape.stdout", &table),
        // The 0.3.x layout: a declared count the rows do not meet, a duration
        // shape the release never prints, a missing `Page N` footer, the same
        // recording twice, and an identifier we could not pass back as an
        // argument. None of them may produce a listing.
        ("plaud-table-count-mismatch.stdout", &plaud),
        ("plaud-table-bad-duration.stdout", &plaud),
        ("plaud-table-missing-footer.stdout", &plaud),
        ("plaud-table-duplicate-ids.stdout", &plaud),
        ("plaud-table-hostile-id.stdout", &plaud),
    ] {
        let stdout = std::fs::read_to_string(negative.join(file)).expect("fixture");
        let error = parser
            .parse_listing(&stdout)
            .expect_err(&format!("{file} must not parse"));
        assert_eq!(
            SourceError::from(error),
            SourceError::MalformedOutput,
            "{file}"
        );
    }

    // Non-UTF-8 output never reaches the parser: the runner rejects it first.
    let raw = std::fs::read(negative.join("non-utf8.stdout")).expect("fixture");
    assert!(String::from_utf8(raw).is_err());
}

#[test]
fn the_shipped_table_carries_only_the_release_that_was_exercised() {
    // The fixtures under `v1.x` describe synthetic releases. If they ever
    // leaked into the committed table, the application would claim to support
    // a CLI nobody has run (spec 2.1, 2.5B). The one shipped row is
    // `@plaud-ai/cli` 0.3.15, exercised on macOS arm64 on 2026-10-12.
    let committed = CliCompatibilityTable::committed();
    assert_eq!(committed.usable_versions(), ["0.3.15"]);
    for row in &fixture_table().versions {
        assert!(
            !committed
                .versions
                .iter()
                .any(|shipped| shipped.version == row.version),
            "fixture release {} leaked into the shipped table",
            row.version
        );
    }
}

#[test]
fn the_shipped_table_matches_its_committed_golden_fixtures() {
    // Same contract as the fixture-table golden test, applied to the table
    // the application actually ships: the recorded digest, the recorded
    // format and the parser must all still agree with the captured layout.
    let committed = CliCompatibilityTable::committed();
    let mut covered = 0usize;
    for row in &committed.versions {
        let (meta, stdout, expected) = load_fixture(&row.fixture_id);
        assert_eq!(meta.version, row.version, "fixture/table version drift");
        assert_eq!(meta.listing_format, row.listing_format);
        assert_eq!(
            sha256_hex(stdout.as_bytes()),
            row.fixture_sha256,
            "captured stdout for {} no longer matches the recorded digest",
            row.version
        );
        let parser = VersionedParser::for_row(row, SourceAccountScope::new(&meta.account_scope));
        let page = parser.parse_listing(&stdout).expect("golden parses");
        assert_eq!(page.items.len(), expected.items.len());
        for (item, want) in page.items.iter().zip(&expected.items) {
            assert_eq!(
                item.identity
                    .external_id
                    .as_ref()
                    .map(ExternalRecordingId::as_str),
                Some(want.external_id.as_str())
            );
            assert_eq!(item.display_name, want.display_name);
            assert_eq!(item.duration_ms, want.duration_ms);
            assert_eq!(item.captured_at_ms, want.captured_at_ms);
            assert_eq!(item.declared_byte_length, want.declared_byte_length);
            assert_eq!(
                item.vendor_transcript_available,
                want.vendor_transcript_available
            );
        }
        assert_eq!(page.next_cursor, expected.next_cursor);
        covered += 1;
    }
    assert_eq!(covered, 1, "the shipped table must keep its golden fixture");
}

// ------------------------------------------------- K1.1/K1.2/K1.3 end to end

#[tokio::test]
async fn an_untested_cli_version_blocks_enumeration_and_keeps_manual_import() {
    let mock = MockCli::install(&serde_json::json!({
        "version": { "stdout": "plaud/9.9.9\n" },
        "files": { "stdout": "{\"id\":\"rec_1\"}\n" },
        "record_invocations": true,
    }));
    let connector = connector(permitted_runner());
    let error = connector
        .connect(Some(&mock.discovered()), &intent())
        .await
        .unwrap_err();
    assert_eq!(error, SourceError::CliUnsupported);

    let status = connector.status();
    assert_eq!(status.state, ConnectorState::IncompatibleVersion);
    assert_eq!(status.detected_version.as_deref(), Some("9.9.9"));
    assert!(!status.version_validated);

    let panel = connector.diagnostics();
    assert!(panel.detail.contains("9.9.9"));
    assert!(panel.detail.contains("by hand"));
    assert!(matches!(
        panel.next_action,
        NextAction::InstallTestedVersion { .. }
    ));

    // Enumeration is refused without ever asking the tool for a listing.
    assert_eq!(
        connector.enumerate(None, &intent()).await.unwrap_err(),
        SourceError::CliUnsupported
    );
    let invocations = mock.invocations();
    assert_eq!(invocations, vec![vec!["version".to_owned()]]);
}

#[tokio::test]
async fn a_release_that_was_never_run_is_refused_even_though_it_is_in_the_table() {
    let mock = MockCli::install(&serde_json::json!({
        "version": { "stdout": "plaud/2.0.0\n" },
        "record_invocations": true,
    }));
    let connector = connector(permitted_runner());
    assert_eq!(
        connector
            .connect(Some(&mock.discovered()), &intent())
            .await
            .unwrap_err(),
        SourceError::CliUnsupported
    );
    assert_eq!(
        connector.status().state,
        ConnectorState::IncompatibleVersion
    );
}

#[tokio::test]
async fn enumerating_plaud_cloud_recordings_yields_identity_scoped_items() {
    let mock = MockCli::install(&jsonl_script(None));
    let connector = connector(permitted_runner());
    let status = connector
        .connect(Some(&mock.discovered()), &intent())
        .await
        .expect("connected");
    assert_eq!(status.state, ConnectorState::CliDetected);
    assert!(status.version_validated);
    assert_eq!(status.detected_version.as_deref(), Some("1.4.2"));

    connector.sign_in(&intent()).await.expect("signed in");
    let listing = connector.enumerate(None, &intent()).await.expect("listed");

    assert_eq!(listing.page.items.len(), 3);
    assert_eq!(
        listing.completeness,
        ListingCompleteness::PossiblyIncomplete
    );
    assert!(listing.empty_account_advisory.is_none());
    assert_eq!(listing.page.next_cursor, None);
    for item in &listing.page.items {
        assert_eq!(item.identity.source_kind, SourceKind::PlaudCloudCli);
        assert_eq!(
            item.identity.account_scope,
            SourceAccountScope::new("plaud:eu:account-a")
        );
        assert!(item.identity.is_stable());
        assert_eq!(item.quality_class, QualityClass::VendorCloudExport);
    }
    assert_eq!(connector.status().state, ConnectorState::Ready);
    assert!(connector.status().listing_possibly_incomplete);

    // Spec 7.2: no cursor was observed for this release, so no paging flag is
    // ever passed.
    let files_calls: Vec<Vec<String>> = mock
        .invocations()
        .into_iter()
        .filter(|argv| argv.first().is_some_and(|a| a == "files"))
        .collect();
    assert_eq!(files_calls, vec![vec!["files".to_owned()]]);

    // And the connector refuses to invent one when asked to page.
    assert_eq!(
        connector
            .enumerate(Some("cursor-from-nowhere"), &intent())
            .await
            .unwrap_err(),
        SourceError::MalformedOutput
    );
}

#[tokio::test]
async fn pagination_is_followed_only_for_a_release_that_observed_it() {
    let (_, page_one, _) = load_fixture("v1.5.0-jsonarray");
    let mock = MockCli::install(&serde_json::json!({
        "version": { "stdout": "plaud/1.5.0\n" },
        "files": { "stdout": page_one },
        "files_page_2": { "stdout": "{\"files\":[{\"id\":\"rec_01HFD000\"}],\"next_cursor\":null}" },
        "logout": { "stdout": "signed out\n" },
        "record_invocations": true,
    }));
    let connector = connector(permitted_runner());
    connector
        .connect(Some(&mock.discovered()), &intent())
        .await
        .expect("connected");

    let first = connector.enumerate(None, &intent()).await.expect("page 1");
    assert_eq!(first.completeness, ListingCompleteness::Complete);
    let cursor = first.page.next_cursor.clone().expect("cursor");
    assert_eq!(cursor, "eyJvZmZzZXQiOjJ9");

    let second = connector
        .enumerate(Some(&cursor), &intent())
        .await
        .expect("page 2");
    assert_eq!(second.page.items.len(), 1);
    assert_eq!(second.page.next_cursor, None);

    let files_calls: Vec<Vec<String>> = mock
        .invocations()
        .into_iter()
        .filter(|argv| argv.first().is_some_and(|a| a == "files"))
        .collect();
    assert_eq!(
        files_calls,
        vec![
            vec!["files".to_owned()],
            vec![
                "files".to_owned(),
                "--cursor".to_owned(),
                "eyJvZmZzZXQiOjJ9".to_owned()
            ]
        ]
    );

    // Spec 4.2.7: this release records a supported logout command, so the
    // disconnect copy may claim a sign-out.
    assert_eq!(
        connector.disconnect_copy(),
        DisconnectCopy::WithVendorLogout
    );
    let copy = connector.disconnect(&intent()).await.expect("disconnected");
    assert_eq!(copy, DisconnectCopy::WithVendorLogout);
    assert_eq!(connector.status().state, ConnectorState::Unconfigured);
    assert!(mock
        .invocations()
        .iter()
        .any(|argv| argv.first().is_some_and(|a| a == "logout")));
}

/// An empty page in the 0.3.x layout, which is how that release says "no more
/// recordings": a declared count of zero, the header, the rule and the footer.
fn plaud_empty_page(page: u32) -> String {
    let header = format!("  {:<36}{:<38}{:<14}DURATION", "ID", "NAME", "DATE");
    let rule = format!("  {}", "\u{2500}".repeat(98));
    format!("\nFiles on this page: 0\n\n{header}\n{rule}\n\nPage {page}\n")
}

#[tokio::test]
async fn the_shipped_release_is_enumerated_by_numbered_pages() {
    // The committed row for `@plaud-ai/cli` 0.3.15, driven through the whole
    // connector against the captured layout: `version` rather than
    // `--version`, numbered paging with an explicit page size, and an empty
    // page ending enumeration.
    let (_, page_one, _) = load_fixture("v0.3.15-plaud-table");
    let mock = MockCli::install(&serde_json::json!({
        "version": { "stdout": "plaud 0.3.15\ncommit  0fa20ec\nbuilt   2026-10-08T07:14:57.467Z\n" },
        "files": { "stdout": page_one },
        "files_page_2": { "stdout": plaud_empty_page(2) },
        "record_invocations": true,
        "unknown_exit_code": 64,
    }));
    let connector = shipped_connector(permitted_runner());
    let status = connector
        .connect(Some(&mock.discovered()), &intent())
        .await
        .expect("connected");
    assert!(status.version_validated);
    assert_eq!(status.detected_version.as_deref(), Some("0.3.15"));
    // The listing is scraped, so the compatibility warning stays on; paging
    // was observed, so the listing is not labelled partial.
    assert!(status.scraping_warning);
    assert!(!status.listing_possibly_incomplete);

    let first = connector.enumerate(None, &intent()).await.expect("listed");
    assert_eq!(first.page.items.len(), 3);
    assert_eq!(first.completeness, ListingCompleteness::Complete);
    assert_eq!(first.page.next_cursor.as_deref(), Some("2"));
    assert_eq!(
        first.page.items[0]
            .identity
            .external_id
            .as_ref()
            .map(ExternalRecordingId::as_str),
        Some("of_1a2b3c4d5e6f708192a3b4c5d6e7f801")
    );
    assert_eq!(first.page.items[0].duration_ms, Some(2_467_000));
    // Day-precision dates are not turned into capture instants.
    assert!(first
        .page
        .items
        .iter()
        .all(|item| item.captured_at.is_none()));

    let second = connector
        .enumerate(Some("2"), &intent())
        .await
        .expect("listed");
    assert!(
        second.page.items.is_empty(),
        "a cursor past the last page lists nothing, got {} item(s)",
        second.page.items.len()
    );
    assert_eq!(second.page.next_cursor, None);

    let files_calls: Vec<Vec<String>> = mock
        .invocations()
        .into_iter()
        .filter(|argv| argv.first().is_some_and(|a| a == "files"))
        .collect();
    assert_eq!(
        files_calls,
        vec![
            vec![
                "files".to_owned(),
                "-p".to_owned(),
                "1".to_owned(),
                "-s".to_owned(),
                "20".to_owned()
            ],
            vec![
                "files".to_owned(),
                "-p".to_owned(),
                "2".to_owned(),
                "-s".to_owned(),
                "20".to_owned()
            ]
        ]
    );

    // Nothing was promised about signing the user out of the vendor session.
    assert_eq!(connector.disconnect_copy(), DisconnectCopy::AssociationOnly);
}

#[tokio::test]
async fn a_release_without_a_logout_command_never_claims_to_sign_out() {
    let mock = MockCli::install(&jsonl_script(None));
    let connector = connector(permitted_runner());
    connector
        .connect(Some(&mock.discovered()), &intent())
        .await
        .expect("connected");
    assert_eq!(connector.disconnect_copy(), DisconnectCopy::AssociationOnly);
    connector.disconnect(&intent()).await.expect("disconnected");
    assert!(
        !mock
            .invocations()
            .iter()
            .any(|argv| argv.first().is_some_and(|a| a == "logout")),
        "no logout command may be invented"
    );
}

#[tokio::test]
async fn local_only_mode_never_launches_the_vendor_tool() {
    let mock = MockCli::install(&jsonl_script(None));
    let connector = connector(local_only_runner());
    let error = connector
        .connect(Some(&mock.discovered()), &intent())
        .await
        .unwrap_err();
    // #304: the refusal keeps its own name instead of reading as an outage.
    assert_eq!(error, SourceError::LocalOnlyMode);
    assert!(
        mock.invocations().is_empty(),
        "spec 22(15): Local-only Mode must never launch the PLAUD Cloud CLI"
    );
    let panel = connector.diagnostics();
    assert!(matches!(panel.state, ConnectorState::LocalOnlyBlocked));
    assert!(panel.program_origin.is_some(), "the tool was found on disk");
}

#[tokio::test]
async fn an_empty_account_gets_region_guidance_rather_than_an_error() {
    let mock = MockCli::install(&serde_json::json!({
        "version": { "stdout": "plaud/1.4.2\n" },
        "files": { "stdout": "\n" },
    }));
    let connector = connector(permitted_runner());
    connector
        .connect(Some(&mock.discovered()), &intent())
        .await
        .expect("connected");
    let listing = connector.enumerate(None, &intent()).await.expect("listed");
    assert_eq!(listing.page.items, Vec::new());
    let advisory = listing.empty_account_advisory.expect("advisory");
    assert!(advisory.contains("regional"));
    let lower = advisory.to_ascii_lowercase();
    assert!(lower.contains("never ask you to paste"));
    for forbidden in ["enter your token", "copy the cookie", "your password below"] {
        assert!(!lower.contains(forbidden), "{forbidden}");
    }
}

#[tokio::test]
async fn a_hostile_listing_is_refused_rather_than_shown_as_complete() {
    let mock = MockCli::install(&serde_json::json!({
        "version": { "stdout": "plaud/1.4.2\n" },
        "files": { "stdout": "{\"id\":\"rec_1\"}\n{\"id\":\"rec_1\"}\n" },
    }));
    let connector = connector(permitted_runner());
    connector
        .connect(Some(&mock.discovered()), &intent())
        .await
        .expect("connected");
    assert_eq!(
        connector.enumerate(None, &intent()).await.unwrap_err(),
        SourceError::MalformedOutput
    );
    assert_eq!(connector.status().state, ConnectorState::Error);
}

#[tokio::test]
async fn an_expired_session_moves_the_connector_to_expired_auth() {
    let mock = MockCli::install(&serde_json::json!({
        "version": { "stdout": "plaud/1.4.2\n" },
        "files": { "stdout": "", "stderr": "Error: session expired, please re-authenticate\n", "exit_code": 1 },
    }));
    let connector = connector(permitted_runner());
    connector
        .connect(Some(&mock.discovered()), &intent())
        .await
        .expect("connected");
    assert_eq!(
        connector.enumerate(None, &intent()).await.unwrap_err(),
        SourceError::AuthenticationRequired
    );
    assert_eq!(connector.status().state, ConnectorState::ExpiredAuth);
    let panel = connector.diagnostics();
    assert_eq!(panel.next_action, NextAction::SignIn);
    assert!(!panel.detail.contains("Error:"));
}

#[tokio::test]
async fn an_oversized_listing_is_killed_at_the_output_cap() {
    let mock = MockCli::install(&serde_json::json!({
        "version": { "stdout": "plaud/1.4.2\n" },
        "files": { "stdout_filler_kib": 16384 },
    }));
    let connector = connector(permitted_runner());
    connector
        .connect(Some(&mock.discovered()), &intent())
        .await
        .expect("connected");
    assert_eq!(
        connector.enumerate(None, &intent()).await.unwrap_err(),
        SourceError::MalformedOutput
    );
}

// --------------------------------------------------------- K1.4 end to end

#[tokio::test]
async fn a_cloud_recording_is_downloaded_verified_and_committed_to_the_archive() {
    let audio = wav_bytes(4096);
    let expected_digest = sha256_hex(&audio);
    let host = serve(vec![http_response("200 OK", "audio/wav", &audio)]);
    let mock = MockCli::install(&jsonl_script(Some(host.port)));
    let connector = connector(permitted_runner());
    connector
        .connect(Some(&mock.discovered()), &intent())
        .await
        .expect("connected");
    let listing = connector.enumerate(None, &intent()).await.expect("listed");
    let item: &SourceItem = &listing.page.items[0];

    let library = tempfile::tempdir().expect("tempdir");
    let staging = library.path().join("acquire");
    std::fs::create_dir_all(&staging).expect("staging");
    let target = staging.join("incoming.bin");

    let report = connector
        .acquire_item(item, &target, &intent())
        .await
        .expect("acquired");

    assert_eq!(report.asset.identity.sha256.to_hex(), expected_digest);
    assert_eq!(report.asset.identity.byte_length, audio.len() as u64);
    assert_eq!(report.asset.quality_class, QualityClass::VendorCloudExport);
    assert!(!report.retried_after_expiry);
    assert_eq!(std::fs::read(&target).expect("staged"), audio);
    assert_eq!(host.requests.load(std::sync::atomic::Ordering::SeqCst), 1);

    // Commit it into the real library through the durable import journal.
    let db_path = library.path().join("app.sqlite");
    let database = Database::open(&db_path).await.expect("open db");
    MigrationRunner::new()
        .run(&database)
        .await
        .expect("migrate");
    let archive = MediaArchive::open(library.path().join("media")).expect("archive");
    let service = ImportService::new(database.clone(), archive);

    let request = ImportRequest {
        identity: item.identity.clone(),
        display_name: item.display_name.clone(),
        quality_class: report.asset.quality_class,
        extension: "wav".to_owned(),
        observed_codec: report.container.as_str().to_owned(),
        remote_version: item.remote_version.clone(),
        attach_to_recording: None,
    };
    let mut staged = std::fs::File::open(&target).expect("open staged");
    let outcome = service
        .import(
            &request,
            &mut staged,
            Timestamp::from_millis(1_760_000_000_000),
        )
        .await
        .expect("import");
    let recording_id = match outcome {
        ImportOutcome::Imported { recording_id, .. } => recording_id,
        other => panic!("expected a fresh import, got {other:?}"),
    };

    let stored: String = sqlx::query_scalar(
        "SELECT ma.sha256 FROM media_assets ma
         JOIN recording_media rm ON rm.media_asset_id = ma.id
         WHERE rm.recording_id = ?1",
    )
    .bind(recording_id.to_string())
    .fetch_one(database.pool())
    .await
    .expect("stored digest");
    assert_eq!(
        stored, expected_digest,
        "committed bytes must match the download"
    );

    let external: String =
        sqlx::query_scalar("SELECT si.external_id FROM source_items si WHERE si.recording_id = ?1")
            .bind(recording_id.to_string())
            .fetch_one(database.pool())
            .await
            .expect("source identity");
    assert_eq!(external, "rec_01HF8ZT9");

    // Spec 7.3: the signed URL is never persisted. Scan the whole database
    // file and the archive for the signature token.
    database.close().await;
    let db_bytes = std::fs::read(&db_path).expect("db bytes");
    assert!(
        !contains(&db_bytes, b"deadbeefcafe"),
        "a signed URL token reached the database"
    );
    for entry in walkdir::WalkDir::new(library.path())
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
    {
        let bytes = std::fs::read(entry.path()).unwrap_or_default();
        assert!(
            !contains(&bytes, b"sig=deadbeefcafe"),
            "a signed URL leaked into {}",
            entry.path().display()
        );
    }
}

#[tokio::test]
async fn an_expired_download_link_is_retried_once_with_a_fresh_url() {
    let audio = wav_bytes(1024);
    let host = serve(vec![
        http_response("403 Forbidden", "application/xml", b"<Error/>"),
        http_response("200 OK", "audio/wav", &audio),
    ]);
    let mock = MockCli::install(&jsonl_script(Some(host.port)));
    let connector = connector(permitted_runner());
    connector
        .connect(Some(&mock.discovered()), &intent())
        .await
        .expect("connected");
    let listing = connector.enumerate(None, &intent()).await.expect("listed");

    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("incoming.bin");
    let report = connector
        .acquire_item(&listing.page.items[0], &target, &intent())
        .await
        .expect("acquired after retry");

    assert!(report.retried_after_expiry);
    assert_eq!(report.asset.identity.byte_length, audio.len() as u64);
    let audio_calls = mock
        .invocations()
        .into_iter()
        .filter(|argv| argv.first().is_some_and(|a| a == "audio"))
        .count();
    assert_eq!(audio_calls, 2, "exactly one fresh-URL retry");
}

#[tokio::test]
async fn an_error_page_served_as_audio_fails_integrity_and_commits_nothing() {
    let host = serve(vec![http_response(
        "200 OK",
        "audio/wav",
        b"<html><body>link expired</body></html>",
    )]);
    let mock = MockCli::install(&jsonl_script(Some(host.port)));
    let connector = connector(permitted_runner());
    connector
        .connect(Some(&mock.discovered()), &intent())
        .await
        .expect("connected");
    let listing = connector.enumerate(None, &intent()).await.expect("listed");

    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("incoming.wav");
    let error = connector
        .acquire_item(&listing.page.items[0], &target, &intent())
        .await
        .unwrap_err();
    assert_eq!(error, SourceError::IntegrityFailed);
    assert!(!target.exists());
    assert!(!target.with_extension("wav.part").exists());
    assert!(
        std::fs::read_dir(dir.path())
            .expect("read dir")
            .next()
            .is_none(),
        "staging must be left clean"
    );
}

#[tokio::test]
async fn a_short_body_is_an_interrupted_transfer_and_leaves_no_staging_file() {
    let audio = wav_bytes(2048);
    // Declare more bytes than we send.
    let mut response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: audio/wav\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        audio.len() + 512
    )
    .into_bytes();
    response.extend_from_slice(&audio);
    let host = serve(vec![response]);
    let mock = MockCli::install(&jsonl_script(Some(host.port)));
    let connector = connector(permitted_runner());
    connector
        .connect(Some(&mock.discovered()), &intent())
        .await
        .expect("connected");
    let listing = connector.enumerate(None, &intent()).await.expect("listed");

    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("incoming.bin");
    let error = connector
        .acquire_item(&listing.page.items[0], &target, &intent())
        .await
        .unwrap_err();
    assert_eq!(error, SourceError::DownloadInterrupted);
    assert!(!target.exists());
}

#[tokio::test]
async fn a_download_url_outside_the_allowlist_is_refused_before_any_byte_is_read() {
    let host = serve(vec![http_response("200 OK", "audio/wav", &wav_bytes(64))]);
    let mut script = jsonl_script(Some(host.port));
    // A redirect-free but off-allowlist destination.
    script["audio_url_template"] = serde_json::json!("https://attacker.example.com/a.wav");
    let mock = MockCli::install(&script);
    let connector = connector(permitted_runner());
    connector
        .connect(Some(&mock.discovered()), &intent())
        .await
        .expect("connected");
    let listing = connector.enumerate(None, &intent()).await.expect("listed");

    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("incoming.bin");
    let error = connector
        .acquire_item(&listing.page.items[0], &target, &intent())
        .await
        .unwrap_err();
    assert_eq!(error, SourceError::SourceUnavailable);
    assert_eq!(host.requests.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_tool_that_returns_no_url_is_malformed_output() {
    let mut script = jsonl_script(None);
    script["audio"] = serde_json::json!({ "stdout": "Your download is ready.\n" });
    let mock = MockCli::install(&script);
    let connector = connector(permitted_runner());
    connector
        .connect(Some(&mock.discovered()), &intent())
        .await
        .expect("connected");
    let listing = connector.enumerate(None, &intent()).await.expect("listed");

    let dir = tempfile::tempdir().expect("tempdir");
    let error = connector
        .acquire_item(&listing.page.items[0], &dir.path().join("x.bin"), &intent())
        .await
        .unwrap_err();
    assert_eq!(error, SourceError::MalformedOutput);
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}
