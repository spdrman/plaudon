//! Opt-in verification against the **real** official PLAUD CLI (spec 2.1, 7.1).
//!
//! Every other test in this repository is hermetic: they drive a mock
//! executable with captured output. This one drives the vendor's own program
//! against the operator's own PLAUD account, which is the only way to find out
//! whether the committed compatibility table describes reality.
//!
//! It is therefore opt-in twice over: `#[ignore]`, so a default `cargo test`
//! never lists it as run, and gated on `PLAUDON_PLAUD_CLI_TESTS=1`, so even
//! `cargo test -- --ignored` prints `SKIPPED` instead of launching a vendor
//! binary nobody asked for.
//!
//! ```text
//! PLAUDON_PLAUD_CLI_TESTS=1 cargo test -p plaudon-app --test plaud_real_cli \
//!   -- --ignored --nocapture
//! ```
//!
//! What it does to the account: it reads. `version`, `files` and `audio` are
//! the only subcommands the adapter can express; there is no delete, no
//! settings write, and the run never calls `login` or `logout`, so an operator
//! who was signed in stays signed in.
//!
//! What it does to the machine: it builds a library in a fresh temporary
//! directory and deletes it at the end. It never touches the real data
//! directory, so a failed run cannot leave rows in the operator's library.
//!
//! The audio it downloads is the operator's private meeting recording. It is
//! never printed, never copied out of the temporary directory and never
//! uploaded anywhere; the assertions use its length and the first bytes of its
//! digest.

use std::collections::BTreeSet;
use std::sync::Arc;

use plaudon_app::command::{AppCommand, AppResponse};
use plaudon_app::dto::{
    ListRecordingsRequest, PlaudConnectRequest, PlaudImportOutcome, PlaudImportRequest,
    PlaudListRequest, PlaudListResponse, RecordingFilter, RecordingSort, RecordingSortField,
    SortDirection,
};
use plaudon_app::notify::UnsupportedNotifier;
use plaudon_app::runtime::{AppEngine, EngineConfig};
use plaudon_app::secrets::InMemorySecretStore;
use plaudon_source_plaud_cli::compatibility::CliCompatibilityTable;
use plaudon_source_plaud_cli::discovery::CliDiscovery;
use plaudon_source_plaud_cli::parser::CliVersion;

/// Set to exactly `1` to opt in.
const GATE_ENV: &str = "PLAUDON_PLAUD_CLI_TESTS";

/// Whether the operator asked for a real vendor run.
fn opted_in() -> bool {
    std::env::var(GATE_ENV).is_ok_and(|value| value == "1")
}

/// The `plaud` the adapter itself would launch, or `None` when it is absent.
fn discovered_program() -> Option<std::path::PathBuf> {
    CliDiscovery::new().discover().ok().map(|cli| cli.program)
}

/// Run one read-only vendor subcommand directly, as the cross-check oracle.
///
/// The adapter's own runner is deliberately not used here: the point of the
/// cross-check is to compare what the adapter produces against what the tool
/// says when nothing of ours is in the way.
fn vendor_stdout(program: &std::path::Path, args: &[&str]) -> String {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("could not run the vendor CLI: {error}"));
    assert!(
        output.status.success(),
        "`plaud {}` failed with {:?}",
        args.join(" "),
        output.status.code()
    );
    String::from_utf8(output.stdout).expect("the vendor CLI printed valid UTF-8")
}

/// Identifiers `plaud today` lists, which is the set this run may import.
///
/// `today` prints the same row geometry as `files` without the header, so the
/// identifier is the first whitespace-separated token of an indented row.
fn todays_external_ids(program: &std::path::Path) -> BTreeSet<String> {
    vendor_stdout(program, &["today"])
        .lines()
        .filter(|line| line.starts_with("  ") && !line.trim().is_empty())
        .filter_map(|line| line.split_whitespace().next())
        .filter(|token| token.starts_with("of_"))
        .map(str::to_owned)
        .collect()
}

#[test]
#[ignore = "launches the real vendor CLI against a real PLAUD account; set PLAUDON_PLAUD_CLI_TESTS=1"]
fn the_committed_table_describes_the_installed_release() {
    if !opted_in() {
        eprintln!(
            "SKIPPED the_committed_table_describes_the_installed_release: {GATE_ENV} is not 1"
        );
        return;
    }
    let Some(program) = discovered_program() else {
        panic!("no `plaud` executable was discovered; install the official CLI first");
    };
    eprintln!("program: {}", program.display());

    let banner = vendor_stdout(&program, &["version"]);
    let version = CliVersion::parse_version_output(&banner)
        .unwrap_or_else(|| panic!("could not read a version from {banner:?}"));
    eprintln!("version: {}", version.as_str());

    let verdict = CliCompatibilityTable::committed().verdict(Some(&version));
    let row = verdict.row().unwrap_or_else(|| {
        panic!(
            "the installed release {} is not in the committed compatibility table; \
             the table lists {:?}",
            version.as_str(),
            CliCompatibilityTable::committed().usable_versions()
        )
    });
    assert_eq!(row.version, version.as_str());
    assert!(
        row.host_os.iter().any(|os| os == std::env::consts::OS),
        "the committed row records {:?}, not {}",
        row.host_os,
        std::env::consts::OS
    );
    eprintln!(
        "verdict: validated (format {:?}, pagination {:?})",
        row.listing_format, row.pagination
    );
}

#[test]
#[ignore = "launches the real vendor CLI against a real PLAUD account; set PLAUDON_PLAUD_CLI_TESTS=1"]
fn todays_recordings_are_listed_imported_and_shown_in_the_library() {
    if !opted_in() {
        eprintln!(
            "SKIPPED todays_recordings_are_listed_imported_and_shown_in_the_library: \
             {GATE_ENV} is not 1"
        );
        return;
    }
    let Some(program) = discovered_program() else {
        panic!("no `plaud` executable was discovered; install the official CLI first");
    };
    let expected_today = todays_external_ids(&program);
    eprintln!("`plaud today` lists {} recording(s)", expected_today.len());
    assert!(
        !expected_today.is_empty(),
        "the account has no recording from today, so there is nothing to verify; \
         make a short recording and sync it first"
    );

    // A throwaway library. `EngineConfig::new` takes the same directory the
    // PLAUDON_DATA_DIR override feeds, so the engine runs exactly as it would
    // in production while the operator's real library stays untouched.
    let data_dir = tempfile::tempdir().expect("a temporary data directory");
    let engine = AppEngine::start(
        EngineConfig::new(data_dir.path().to_path_buf())
            .with_secrets(Arc::new(InMemorySecretStore::new()))
            .with_notifier(Arc::new(UnsupportedNotifier))
            .with_worker_threads(2),
    )
    .expect("the engine starts");
    let call = |command: AppCommand| {
        engine
            .dispatcher()
            .call_blocking(command)
            .unwrap_or_else(|envelope| panic!("command failed: {envelope:?}"))
    };

    let AppResponse::PlaudConnect(snapshot) = call(AppCommand::PlaudConnect(PlaudConnectRequest {
        user_intent: "connect_plaud_account".to_owned(),
    })) else {
        panic!("unexpected response to plaud_connect");
    };
    assert!(
        snapshot.status.version_validated,
        "the installed release was not validated: {:?}",
        snapshot.status
    );
    eprintln!(
        "connected: state {:?}, version {:?}",
        snapshot.status.state, snapshot.status.detected_version
    );

    // Page through the account, importing the recordings `plaud today` named
    // and nothing else. Importing a page at a time is what the application
    // itself does: `plaud_import` only accepts identifiers from the listing it
    // most recently returned.
    let mut seen_today: BTreeSet<String> = BTreeSet::new();
    let mut imported: Vec<(String, plaudon_domain::ids::RecordingId)> = Vec::new();
    let mut cursor: Option<String> = None;
    let mut pages = 0usize;
    loop {
        let AppResponse::PlaudList(listing) =
            call(AppCommand::PlaudList(PlaudListRequest { cursor }))
        else {
            panic!("unexpected response to plaud_list");
        };
        pages += 1;
        let listing: PlaudListResponse = listing;
        eprintln!(
            "page {pages}: {} item(s), completeness {:?}",
            listing.items.len(),
            listing.completeness
        );
        let on_this_page: Vec<String> = listing
            .items
            .iter()
            .map(|item| item.external_id.clone())
            .filter(|id| expected_today.contains(id))
            .collect();
        for item in listing
            .items
            .iter()
            .filter(|item| expected_today.contains(&item.external_id))
        {
            eprintln!(
                "  today: {} duration {:?} ms, captured_at {:?}",
                item.external_id, item.duration_ms, item.captured_at
            );
        }
        seen_today.extend(on_this_page.iter().cloned());

        if !on_this_page.is_empty() {
            let AppResponse::PlaudImport(response) =
                call(AppCommand::PlaudImport(PlaudImportRequest {
                    external_ids: on_this_page.clone(),
                }))
            else {
                panic!("unexpected response to plaud_import");
            };
            for outcome in response.results {
                match outcome {
                    PlaudImportOutcome::Imported {
                        external_id,
                        recording_id,
                        ..
                    }
                    | PlaudImportOutcome::AlreadyImported {
                        external_id,
                        recording_id,
                    } => imported.push((external_id, recording_id)),
                    PlaudImportOutcome::Failed {
                        external_id, error, ..
                    } => panic!("importing {external_id} failed: {error:?}"),
                }
            }
        }

        if seen_today.len() == expected_today.len() {
            break;
        }
        match listing.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
        assert!(pages < 200, "refusing to page through the account forever");
    }

    assert_eq!(
        seen_today, expected_today,
        "the connector's listing disagrees with `plaud today`"
    );
    assert_eq!(imported.len(), expected_today.len());

    // The library is the proof: the bytes were downloaded, hashed and
    // committed under an identity the application can find again.
    let AppResponse::ListRecordings(page) =
        call(AppCommand::ListRecordings(ListRecordingsRequest {
            cursor: None,
            limit: 50,
            filter: RecordingFilter {
                query: None,
                import_status: Vec::new(),
                transcription_status: Vec::new(),
                sources: Vec::new(),
                tags: Vec::new(),
                date_from: None,
                date_to: None,
            },
            sort: RecordingSort {
                field: RecordingSortField::ImportedAt,
                direction: SortDirection::Desc,
            },
        }))
    else {
        panic!("unexpected response to list_recordings");
    };
    assert_eq!(
        page.total as usize,
        imported.len(),
        "the temporary library holds exactly the recordings this run imported"
    );
    for (external_id, recording_id) in &imported {
        let summary = page
            .items
            .iter()
            .find(|row| row.recording_id == *recording_id)
            .unwrap_or_else(|| panic!("{external_id} is not in the library listing"));
        assert!(summary.byte_length > 0);
        // Only the digest prefix is ever printed: the rest is a fingerprint of
        // the operator's private audio.
        eprintln!(
            "library: {} bytes, sha256 {}…, duration {:?} ms",
            summary.byte_length,
            &summary.sha256.to_string()[..12],
            summary.duration_ms
        );
    }

    drop(engine);
}
