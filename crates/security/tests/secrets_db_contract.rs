//! D1.2 persistence contract: the database never holds credential material.
//!
//! Spec §13.1 ("We will never store OpenRouter credentials, PLAUD sessions,
//! signed URLs, or encryption key material in plaintext database columns"),
//! App. B.7 ("DB and portable backups contain only opaque references /
//! fingerprints") and §16.2 (verification: "Search logs, crash reports and DB
//! fixture").
//!
//! This is the "DB fixture" half of that verification. It builds a real SQLite
//! database shaped like the application's settings and device tables, writes
//! the records the credential flow would really write, and then walks **every
//! column of every row of every table** looking for secret-shaped values.
//!
//! The scan is deliberately broader than the known test secret: it also hunts
//! for an OpenRouter-shaped key and a 32-hex device-token shape anywhere in the
//! database, so a future column that happened to persist a *different* real
//! credential would also fail.

use plaudon_security::secrets::{SecretRef, SecretScope, SecretString};
use rusqlite::types::ValueRef;
use rusqlite::Connection;

/// The exact credentials the fixture stores in the (simulated) OS vault.
const OPENROUTER_KEY: &str =
    "sk-or-v1-0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcd";
const DEVICE_TOKEN: &str = "9f2c4ab7e51d3860af7b21cc90de45f1";
const PLAUD_SESSION_TOKEN: &str = "plaud-session-6c1f8ba74d2e05399ab4cf71e8d0263a";

const CREATED_AT: i64 = 1_762_000_000;

/// Schema fragment covering every table the credential flow writes to.
const SCHEMA: &str = "
CREATE TABLE settings (
    key              TEXT PRIMARY KEY,
    value_json       TEXT NOT NULL,
    updated_at       INTEGER NOT NULL
) STRICT;

CREATE TABLE devices (
    id               TEXT PRIMARY KEY,
    display_name     TEXT NOT NULL,
    model            TEXT,
    firmware         TEXT,
    transport        TEXT NOT NULL,
    auth_state       TEXT NOT NULL,
    secret_ref_json  TEXT,
    last_seen_at     INTEGER
) STRICT;

CREATE TABLE source_accounts (
    id               TEXT PRIMARY KEY,
    provider         TEXT NOT NULL,
    auth_state       TEXT NOT NULL,
    secret_ref_json  TEXT
) STRICT;

CREATE TABLE cloud_request_ledger (
    id               TEXT PRIMARY KEY,
    provider         TEXT NOT NULL,
    model_id         TEXT NOT NULL,
    credential_ref   TEXT NOT NULL,
    dispatched_at    INTEGER NOT NULL,
    outcome          TEXT NOT NULL
) STRICT;
";

/// Build the database exactly as the credential flow would.
fn build_fixture() -> Connection {
    let connection = Connection::open_in_memory().expect("in-memory sqlite");
    connection.execute_batch(SCHEMA).expect("schema");

    // 1. The OpenRouter key: stored in the OS vault, only its reference in SQLite.
    let openrouter_scope = SecretScope::OpenRouterApiKey;
    let openrouter_ref = SecretRef::new(
        &openrouter_scope,
        &SecretString::new(OPENROUTER_KEY),
        CREATED_AT,
    );
    connection
        .execute(
            "INSERT INTO settings (key, value_json, updated_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![
                "openrouter.credential_ref",
                serde_json::to_string(&openrouter_ref).expect("serialize ref"),
                CREATED_AT
            ],
        )
        .expect("insert settings");

    // A neighbouring, unrelated setting, so the scan has realistic noise.
    connection
        .execute(
            "INSERT INTO settings (key, value_json, updated_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![
                "privacy.local_only_mode",
                r#"{"enabled":true,"approved_by":"user_settings_confirmation"}"#,
                CREATED_AT
            ],
        )
        .expect("insert settings");

    // 2. Two devices with distinct per-device authorization material (§7.7).
    for (device_id, token) in [
        ("plaud-note-AAAA", DEVICE_TOKEN),
        ("plaud-note-BBBB", "0b4e8c1d77aa39f25e6318cc40b9d7e2"),
    ] {
        let scope = SecretScope::device(device_id).expect("valid device id");
        let reference = SecretRef::new(&scope, &SecretString::new(token), CREATED_AT);
        connection
            .execute(
                "INSERT INTO devices
                 (id, display_name, model, firmware, transport, auth_state, secret_ref_json, last_seen_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    device_id,
                    "PLAUD Note",
                    "V0095",
                    "1.4.2",
                    "ble",
                    "profile_authenticated",
                    serde_json::to_string(&reference).expect("serialize ref"),
                    CREATED_AT
                ],
            )
            .expect("insert device");
    }

    // 3. A cloud account whose session token also lives only in the vault.
    let session_scope = SecretScope::device("plaud-cloud-session").expect("valid id");
    let session_ref = SecretRef::new(
        &session_scope,
        &SecretString::new(PLAUD_SESSION_TOKEN),
        CREATED_AT,
    );
    connection
        .execute(
            "INSERT INTO source_accounts (id, provider, auth_state, secret_ref_json)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                "acct_0001",
                "plaud_cloud",
                "connected",
                serde_json::to_string(&session_ref).expect("serialize ref")
            ],
        )
        .expect("insert account");

    // 4. A dispatched cloud request that records *which* credential was used,
    //    by fingerprint, never by value.
    connection
        .execute(
            "INSERT INTO cloud_request_ledger
             (id, provider, model_id, credential_ref, dispatched_at, outcome)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                "req_0001",
                "openrouter",
                "anthropic/claude-3.5-sonnet",
                openrouter_ref.fingerprint.clone(),
                CREATED_AT,
                "completed"
            ],
        )
        .expect("insert ledger row");

    connection
}

/// Every table name in the fixture, read from SQLite itself rather than from a
/// hardcoded list, so a table added later is scanned automatically.
fn table_names(connection: &Connection) -> Vec<String> {
    let mut statement = connection
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'")
        .expect("prepare table query");
    let names = statement
        .query_map([], |row| row.get::<_, String>(0))
        .expect("query tables")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect tables");
    assert!(!names.is_empty(), "fixture must contain tables");
    names
}

/// Render any column value as text for scanning, including BLOBs (a credential
/// smuggled in as bytes must not slip past a text-only scan).
fn cell_as_text(value: ValueRef<'_>) -> String {
    match value {
        ValueRef::Null => String::new(),
        ValueRef::Integer(i) => i.to_string(),
        ValueRef::Real(f) => f.to_string(),
        ValueRef::Text(bytes) | ValueRef::Blob(bytes) => {
            String::from_utf8_lossy(bytes).into_owned()
        }
    }
}

/// Collect `(table, column, text)` for every cell in the database.
fn all_cells(connection: &Connection) -> Vec<(String, String, String)> {
    let mut cells = Vec::new();
    for table in table_names(connection) {
        let mut statement = connection
            .prepare(&format!("SELECT * FROM \"{table}\""))
            .expect("prepare select");
        let column_names: Vec<String> = statement
            .column_names()
            .into_iter()
            .map(str::to_owned)
            .collect();
        let mut rows = statement.query([]).expect("query rows");
        while let Some(row) = rows.next().expect("step row") {
            for (index, column) in column_names.iter().enumerate() {
                let value = row.get_ref(index).expect("column value");
                cells.push((table.clone(), column.clone(), cell_as_text(value)));
            }
        }
    }
    assert!(
        cells.len() >= 30,
        "fixture should produce a substantial scan surface"
    );
    cells
}

/// Secret-shaped patterns. Independent of the known test values so the scan
/// also catches a *different* real credential landing in a column later.
fn looks_like_a_credential(text: &str) -> Option<&'static str> {
    if text.contains("sk-or-v1-") {
        return Some("OpenRouter-shaped API key");
    }
    if text.contains("plaud-session-") {
        return Some("PLAUD session token");
    }
    // A bare 32+ character hex run that is not one of our 32-hex fingerprints.
    let mut run = 0usize;
    for byte in text.bytes() {
        if byte.is_ascii_hexdigit() {
            run += 1;
            if run >= 32 {
                return Some("32-hex token-shaped run");
            }
        } else {
            run = 0;
        }
    }
    None
}

#[test]
fn database_stores_only_opaque_references_no_credential_material() {
    let connection = build_fixture();
    let cells = all_cells(&connection);

    let known_secrets = [OPENROUTER_KEY, DEVICE_TOKEN, PLAUD_SESSION_TOKEN];
    let mut hits = Vec::new();

    for (table, column, text) in &cells {
        for secret in known_secrets {
            if text.contains(secret) {
                hits.push(format!("{table}.{column} contains a known test credential"));
            }
        }
    }

    assert!(
        hits.is_empty(),
        "credential material found in the database: {hits:#?}"
    );
}

#[test]
fn database_contains_no_credential_shaped_value_in_any_column() {
    let connection = build_fixture();
    let fingerprints = known_fingerprints();

    let mut hits = Vec::new();
    for (table, column, text) in all_cells(&connection) {
        // A fingerprint is a 32-hex string by construction, so exempt exactly
        // the values we know to be fingerprints before applying the shape scan.
        let scanned = fingerprints
            .iter()
            .fold(text.clone(), |acc, fp| acc.replace(fp, "<ref>"));
        if let Some(shape) = looks_like_a_credential(&scanned) {
            hits.push(format!("{table}.{column} looks like a credential: {shape}"));
        }
    }

    assert!(
        hits.is_empty(),
        "credential-shaped values found in the database: {hits:#?}"
    );
}

/// Every fingerprint the fixture legitimately persists.
fn known_fingerprints() -> Vec<String> {
    let mut fingerprints = vec![SecretRef::fingerprint_of(
        &SecretScope::OpenRouterApiKey,
        &SecretString::new(OPENROUTER_KEY),
    )];
    for (device_id, token) in [
        ("plaud-note-AAAA", DEVICE_TOKEN),
        ("plaud-note-BBBB", "0b4e8c1d77aa39f25e6318cc40b9d7e2"),
    ] {
        let scope = SecretScope::device(device_id).expect("valid device id");
        fingerprints.push(SecretRef::fingerprint_of(&scope, &SecretString::new(token)));
    }
    let session_scope = SecretScope::device("plaud-cloud-session").expect("valid id");
    fingerprints.push(SecretRef::fingerprint_of(
        &session_scope,
        &SecretString::new(PLAUD_SESSION_TOKEN),
    ));
    fingerprints
}

#[test]
fn persisted_references_still_identify_their_credential_and_detect_rotation() {
    let connection = build_fixture();

    let stored: String = connection
        .query_row(
            "SELECT value_json FROM settings WHERE key = 'openrouter.credential_ref'",
            [],
            |row| row.get(0),
        )
        .expect("read stored reference");
    let reference: SecretRef = serde_json::from_str(&stored).expect("deserialize reference");

    assert_eq!(reference.service, "ai.plaudon.plaudon");
    assert_eq!(reference.account, "openrouter.api_key");
    assert_eq!(reference.created_at_unix, CREATED_AT);
    assert_eq!(reference.fingerprint.len(), 32);

    // The reference is useful: it recognises the same key and rejects another.
    let scope = SecretScope::OpenRouterApiKey;
    assert!(reference.matches(&scope, &SecretString::new(OPENROUTER_KEY)));
    assert!(!reference.matches(
        &scope,
        &SecretString::new(
            "sk-or-v1-ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"
        )
    ));
}

#[test]
fn device_rows_carry_distinct_references_so_no_token_is_shared() {
    let connection = build_fixture();

    let mut statement = connection
        .prepare("SELECT id, secret_ref_json FROM devices ORDER BY id")
        .expect("prepare devices query");
    let rows: Vec<(String, SecretRef)> = statement
        .query_map([], |row| {
            let id: String = row.get(0)?;
            let json: String = row.get(1)?;
            Ok((
                id,
                serde_json::from_str::<SecretRef>(&json).expect("deserialize ref"),
            ))
        })
        .expect("query devices")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect devices");

    assert_eq!(rows.len(), 2);
    assert_ne!(
        rows[0].1.fingerprint, rows[1].1.fingerprint,
        "two devices must not share one authorization secret (§16.4)"
    );
    for (id, reference) in &rows {
        assert_eq!(reference.account, format!("device.{id}.auth"));
    }
}

/// A backup is produced from the database rows, so proving the rows are clean
/// also proves the portable archive is (§13.6: backups exclude API keys and
/// PLAUD tokens).
#[test]
fn backup_projection_of_the_database_carries_no_credential_material() {
    let connection = build_fixture();
    let mut export = serde_json::Map::new();

    for table in table_names(&connection) {
        let mut statement = connection
            .prepare(&format!("SELECT * FROM \"{table}\""))
            .expect("prepare select");
        let column_names: Vec<String> = statement
            .column_names()
            .into_iter()
            .map(str::to_owned)
            .collect();
        let mut rows = statement.query([]).expect("query rows");
        let mut table_rows = Vec::new();
        while let Some(row) = rows.next().expect("step row") {
            let mut object = serde_json::Map::new();
            for (index, column) in column_names.iter().enumerate() {
                let value = row.get_ref(index).expect("column value");
                object.insert(
                    column.clone(),
                    serde_json::Value::String(cell_as_text(value)),
                );
            }
            table_rows.push(serde_json::Value::Object(object));
        }
        export.insert(table, serde_json::Value::Array(table_rows));
    }

    let archive = serde_json::to_string(&serde_json::Value::Object(export)).expect("serialize");
    for secret in [OPENROUTER_KEY, DEVICE_TOKEN, PLAUD_SESSION_TOKEN] {
        assert!(
            !archive.contains(secret),
            "backup projection leaked a credential"
        );
    }
    assert!(!archive.contains("sk-or-v1-"));
    assert!(
        archive.contains("ai.plaudon.plaudon"),
        "the archive should carry opaque references"
    );
}
