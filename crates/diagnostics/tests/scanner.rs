//! End-to-end scanner acceptance: a clean corpus must be silent, and a planted
//! corpus must fail — the negative control that proves the scanner can fail
//! (spec §18.5, §1.5).

#![cfg(feature = "scanner")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_plaudon-secret-scan");

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/secret-scan")
}

/// Planted secrets, one per line of `planted.log`, with the pattern each line
/// must be reported under.
const PLANTED: &[(&str, &str)] = &[
    (
        "openrouter_key",
        r#"{"level":"INFO","message":"dispatch","api_key":"sk-or-v1-9f3c2b1a0d4e5f60718293a4b5c6d7e8"}"#,
    ),
    ("api_key", r#"{"level":"INFO","key":"sk-proj-AbCdEf0123456789XyZwVuTs42"}"#),
    ("authorization_header", "GET /v1/models Authorization: Bearer abcDEF1234567890token"),
    ("bearer_token", "retrying with Bearer abcDEF1234567890token"),
    ("credential_param", "cli invoked with token=Zm9vYmFyYmF6cXV4"),
    (
        "signed_url",
        "download https://plaud-media.s3.amazonaws.com/u/42.wav?X-Amz-Signature=4f1c9d2e ok",
    ),
    ("url_userinfo", "proxy https://operator:hunter2@proxy.internal:8443/path"),
    ("device_token_hex", "device static secret 0123456789abcdef0123456789abcdef"),
    (
        "jwt",
        "session eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dBjftJeZ4CVPmB92K27uhbUJU1p1r_wW1gFWFOEjXk",
    ),
    (
        "private_key_pem",
        "-----BEGIN EC PRIVATE KEY-----MHcCAQEEIBmL7Q2nnrskQ8dYwF2hA==-----END EC PRIVATE KEY-----",
    ),
];

/// Copy the committed clean fixtures into a scratch directory and add a SQLite
/// database holding only opaque references.
fn clean_corpus() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for entry in fs::read_dir(fixture_root().join("clean")).unwrap() {
        let entry = entry.unwrap();
        fs::copy(entry.path(), dir.path().join(entry.file_name())).unwrap();
    }
    build_database(&dir.path().join("library.sqlite"), None);
    dir
}

/// Build the SQLite fixture. `planted` injects one secret into a row value.
fn build_database(path: &Path, planted: Option<&str>) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE recordings (
             id TEXT PRIMARY KEY,
             media_asset_id TEXT NOT NULL,
             duration_ms INTEGER NOT NULL
         );
         CREATE TABLE credentials (
             scope TEXT PRIMARY KEY,
             stored_in TEXT NOT NULL,
             reference TEXT NOT NULL,
             value TEXT
         );
         CREATE TABLE jobs (
             id TEXT PRIMARY KEY,
             job_type TEXT NOT NULL,
             state TEXT NOT NULL,
             last_error_code TEXT
         );
         INSERT INTO recordings VALUES ('rec_7hd92kfa','med_21c7b004',95439);
         INSERT INTO recordings VALUES ('rec_a013mq8x','med_7731be0f',184102);
         INSERT INTO credentials VALUES ('openrouter','os_keychain','plaudon/openrouter',NULL);
         INSERT INTO credentials VALUES
             ('device/dev_0a7c31','os_keychain','plaudon/device/dev_0a7c31',NULL);
         INSERT INTO jobs VALUES ('job_4f10aa9c','transcribe','completed',NULL);
         INSERT INTO jobs VALUES ('job_91ce0022','summarize','failed','EGRESS_DENIED');",
    )
    .unwrap();
    if let Some(secret) = planted {
        conn.execute(
            "INSERT INTO credentials VALUES ('leaked','sqlite_plaintext','plaudon/leaked',?1)",
            [secret],
        )
        .unwrap();
    }
    conn.close().unwrap();
}

fn run(args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .output()
        .expect("scanner binary runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn clean_corpus_has_zero_findings_and_exits_zero() {
    let dir = clean_corpus();
    let output = run(&[dir.path().to_str().unwrap()]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "clean corpus failed: {}{}",
        stdout(&output),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout(&output).trim().is_empty(),
        "unexpected findings: {}",
        stdout(&output)
    );
}

#[test]
fn planted_secrets_are_all_found_and_exit_is_non_zero() {
    let dir = clean_corpus();
    let planted_log = dir.path().join("planted.log");
    let body: String = PLANTED
        .iter()
        .map(|(_, line)| format!("{line}\n"))
        .collect::<Vec<_>>()
        .concat();
    fs::write(&planted_log, body).unwrap();
    fs::remove_file(dir.path().join("library.sqlite")).unwrap();
    build_database(
        &dir.path().join("library.sqlite"),
        Some("sk-or-v1-9f3c2b1a0d4e5f60718293a4b5c6d7e8"),
    );

    let output = run(&[dir.path().to_str().unwrap()]);
    assert_eq!(
        output.status.code(),
        Some(1),
        "planted corpus should fail the scan"
    );
    let findings = stdout(&output);

    // Every planted line is reported under its pattern, at its own line number.
    for (index, (pattern, _)) in PLANTED.iter().enumerate() {
        let expected = format!("planted.log:{}:{pattern}", index + 1);
        assert!(
            findings.contains(&expected),
            "missing finding `{expected}` in:\n{findings}"
        );
    }

    // The SQLite row is reported with table, column and row ordinal.
    assert!(
        findings.contains("library.sqlite:credentials.value:3:openrouter_key"),
        "sqlite planted secret not reported:\n{findings}"
    );

    // Nothing from the clean corpus is reported.
    for clean in ["app.log", "export-snapshot.json", "crash-report.txt"] {
        assert!(
            !findings.contains(clean),
            "clean file `{clean}` reported:\n{findings}"
        );
    }

    // The report never echoes a secret.
    for secret in [
        "sk-or-v1-9f3c2b1a",
        "sk-proj-AbCdEf",
        "abcDEF1234567890token",
        "Zm9vYmFyYmF6cXV4",
        "X-Amz-Signature",
        "hunter2",
        "0123456789abcdef",
        "eyJhbGciOiJIUzI1NiJ9",
        "MHcCAQEEIBmL7Q2nnrs",
    ] {
        assert!(
            !findings.contains(secret),
            "scanner echoed secret `{secret}`:\n{findings}"
        );
    }
}

#[test]
fn json_format_is_machine_readable() {
    let dir = clean_corpus();
    fs::write(
        dir.path().join("planted.log"),
        "key sk-or-v1-9f3c2b1a0d4e5f60718293a4b5c6d7e8\n",
    )
    .unwrap();
    let output = run(&["--format", "json", dir.path().to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
    let findings = report["findings"].as_array().unwrap();
    assert_ne!(findings.len(), 0, "expected at least one finding");
    assert!(findings.iter().any(|f| f["pattern"] == "openrouter_key"));
    assert!(findings.iter().all(|f| f["location"]["kind"] == "line"));
    assert!(report["files_scanned"].as_u64().unwrap() >= 4);
    assert!(
        !stdout(&output).contains("sk-or-v1-9f3c"),
        "JSON report echoed the secret"
    );
}

#[test]
fn allowlist_suppresses_a_documented_false_positive() {
    let documented = fixture_root().join("documented");
    let scratch = tempfile::tempdir().unwrap();
    let allowlist = scratch.path().join("allowlist.json");
    fs::write(
        &allowlist,
        r#"{"entries":[{"path":"**/documented/content-hashes.log","pattern":"device_token_hex","reason":"SHA-256 media digests, not credentials"}]}"#,
    )
    .unwrap();

    // Without the allowlist the SHA-256 digests look exactly like device tokens.
    let bare = run(&[documented.to_str().unwrap()]);
    assert_eq!(
        bare.status.code(),
        Some(1),
        "digests should look like device tokens"
    );
    assert!(stdout(&bare).contains("device_token_hex"));

    // With it, the scan is clean and the entry is used (so not reported stale).
    let allowed = run(&[
        "--allowlist",
        allowlist.to_str().unwrap(),
        documented.to_str().unwrap(),
    ]);
    assert_eq!(
        allowed.status.code(),
        Some(0),
        "allowlisted scan should pass: {}",
        stdout(&allowed)
    );
    assert_eq!(stdout(&allowed).trim(), "");
}

/// The gate `scripts/secret-scan.sh` runs in CI: every committed fixture,
/// the repo allowlist, and the adversarial corpora excluded.
#[test]
fn committed_fixtures_pass_the_repository_gate() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    let allowlist = fixture_root().join("allowlist.json");
    let output = run(&[
        "--allowlist",
        allowlist.to_str().unwrap(),
        "--exclude",
        "**/fuzz-corpus/**",
        // Build output is not a committed fixture. `scripts/ci-selftest.sh`
        // compiles the deliberately-broken crates under ci-selftest/, and a
        // target/ directory is full of dependency files and cache tags whose
        // hashes read as device tokens.
        "--exclude",
        "**/target/**",
        root.to_str().unwrap(),
    ]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "committed fixtures failed the secret scan:\n{}",
        stdout(&output)
    );
}

#[test]
fn an_unused_allowlist_entry_is_reported_so_the_list_cannot_rot() {
    let dir = clean_corpus();
    let allowlist = dir.path().join("allowlist.json");
    fs::write(
        &allowlist,
        r#"{"entries":[{"path":"**/nothing-here.log","pattern":"device_token_hex","reason":"stale"}]}"#,
    )
    .unwrap();
    let output = run(&[
        "--allowlist",
        allowlist.to_str().unwrap(),
        dir.path().to_str().unwrap(),
    ]);
    assert_eq!(
        output.status.code(),
        Some(1),
        "a stale allowlist entry must fail the scan"
    );
    assert!(
        stdout(&output).contains("allowlist:**/nothing-here.log:device_token_hex"),
        "stale entry not reported: {}",
        stdout(&output)
    );
}

#[test]
fn allowlist_with_an_unknown_pattern_is_a_usage_error() {
    let dir = clean_corpus();
    let allowlist = dir.path().join("bad-allowlist.json");
    fs::write(
        &allowlist,
        r#"{"entries":[{"path":"**","pattern":"not_a_pattern","reason":"typo"}]}"#,
    )
    .unwrap();
    let output = run(&[
        "--allowlist",
        allowlist.to_str().unwrap(),
        dir.path().to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown pattern"));
}

#[test]
fn exclude_globs_skip_adversarial_corpora() {
    let dir = clean_corpus();
    let corpus = dir.path().join("fuzz-corpus");
    fs::create_dir_all(&corpus).unwrap();
    fs::write(
        corpus.join("seed"),
        "sk-or-v1-9f3c2b1a0d4e5f60718293a4b5c6d7e8\n",
    )
    .unwrap();

    let included = run(&[dir.path().to_str().unwrap()]);
    assert_eq!(included.status.code(), Some(1));

    let excluded = run(&[
        "--exclude",
        "**/fuzz-corpus/**",
        dir.path().to_str().unwrap(),
    ]);
    assert_eq!(
        excluded.status.code(),
        Some(0),
        "exclude glob did not take effect: {}",
        stdout(&excluded)
    );
}

#[test]
fn missing_path_argument_is_a_usage_error() {
    let output = run(&[]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("at least one path is required"));
}
