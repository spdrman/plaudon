//! The sink is the choke point: no call site can write an unredacted record.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use plaudon_diagnostics::logging::{build_subscriber, LOG_FILE_NAME};
use plaudon_diagnostics::{LogFormat, LoggingConfig, LoggingHandle, WirePayloadLogging};

const OPENROUTER_KEY: &str =
    "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";

const SIGNED_URL: &str = "https://plaud-media.s3.amazonaws.com/u/42.wav?X-Amz-Signature=4f1c9d2e";

#[derive(Debug)]
#[allow(dead_code)]
struct ApiConfig {
    base: &'static str,
    api_key: String,
}

fn read_log(dir: &Path) -> String {
    fs::read_to_string(dir.join(LOG_FILE_NAME)).unwrap_or_default()
}

/// Run `body` with a scoped subscriber writing into `dir`, then return the log.
fn with_logging<F>(config: LoggingConfig, body: F) -> (String, LoggingHandle)
where
    F: FnOnce(&LoggingHandle),
{
    let directory = config.directory.clone();
    let (subscriber, handle) = build_subscriber(config).unwrap();
    tracing::subscriber::with_default(subscriber, || body(&handle));
    handle.flush().unwrap();
    (read_log(&directory), handle)
}

#[test]
fn json_sink_redacts_messages_fields_and_debug_values() {
    let dir = tempfile::tempdir().unwrap();
    let config = LoggingConfig::new(dir.path()).with_filter("debug");
    let (log, _handle) = with_logging(config, |_| {
        tracing::info!("dispatching with {OPENROUTER_KEY}");
        tracing::info!(key = %OPENROUTER_KEY, "field value");
        tracing::info!(url = %SIGNED_URL, "signed url field");
        let config = ApiConfig {
            base: "https://openrouter.ai",
            api_key: OPENROUTER_KEY.into(),
        };
        tracing::info!(?config, "debug struct");
    });

    assert!(
        !log.contains("sk-or-v1-9f3c"),
        "key leaked into the log file: {log}"
    );
    assert!(
        !log.contains("X-Amz-Signature"),
        "signed url query leaked: {log}"
    );
    assert!(
        log.contains("<redacted:openrouter_key>"),
        "marker missing: {log}"
    );
    assert!(
        log.contains("<redacted:signed_url host=plaud-media.s3.amazonaws.com>"),
        "signed-url marker missing: {log}"
    );
    assert_eq!(
        log.lines().count(),
        4,
        "expected one JSON object per event: {log}"
    );
    for line in log.lines() {
        serde_json::from_str::<serde_json::Value>(line)
            .unwrap_or_else(|err| panic!("line is not JSON ({err}): {line}"));
    }
}

#[test]
fn human_sink_redacts_too() {
    let dir = tempfile::tempdir().unwrap();
    let config = LoggingConfig::new(dir.path())
        .with_filter("info")
        .with_format(LogFormat::Human);
    let (log, _handle) = with_logging(config, |_| {
        tracing::info!("Authorization: Bearer abcDEF1234567890token");
    });
    assert!(
        !log.contains("abcDEF1234567890token"),
        "bearer leaked: {log}"
    );
    assert!(log.contains("<redacted:bearer>"), "marker missing: {log}");
}

#[test]
fn registered_secret_never_reaches_the_file_in_any_position() {
    let dir = tempfile::tempdir().unwrap();
    // A value with no recognisable shape: only registration can scrub it.
    let secret = "QuietRiverLantern42";
    let config = LoggingConfig::new(dir.path()).with_filter("debug");
    let (log, _handle) = with_logging(config, |handle| {
        handle.register_secret(secret).unwrap();
        tracing::info!("{secret} at the start");
        tracing::info!("in the {secret} middle");
        tracing::info!("at the end {secret}");
        tracing::info!(credential = %secret, "inside a field value");
        let config = ApiConfig {
            base: "https://openrouter.ai",
            api_key: secret.to_owned(),
        };
        tracing::info!(?config, "inside a Debug struct");
    });

    assert!(!log.contains(secret), "registered secret survived: {log}");
    assert_eq!(
        log.matches("<redacted:registered>").count(),
        5,
        "expected five markers: {log}"
    );
}

#[test]
fn wire_payload_logging_is_a_no_op_by_default() {
    static CALLED: AtomicBool = AtomicBool::new(false);
    let dir = tempfile::tempdir().unwrap();
    let config = LoggingConfig::new(dir.path()).with_filter("trace");
    assert_eq!(config.wire_payload_logging, WirePayloadLogging::Disabled);

    let (log, handle) = with_logging(config, |handle| {
        handle.wire_payload("plaud-ble", || {
            CALLED.store(true, Ordering::SeqCst);
            "0x1b 0x00 0xff raw gatt frame with audio bytes".to_owned()
        });
    });

    assert!(
        !CALLED.load(Ordering::SeqCst),
        "the payload closure must not even run"
    );
    assert!(log.is_empty(), "wire payload reached the log file: {log}");
    assert!(!handle.wire_payload_logging().is_enabled());
}

/// Only a debug build compiled with `wire-diagnostics` can construct
/// `WirePayloadLogging::Enabled`; everywhere else the default is the only
/// reachable value (spec App. B.7).
#[test]
fn wire_payload_enabled_is_unreachable_outside_debug_builds() {
    #[cfg(all(feature = "wire-diagnostics", debug_assertions))]
    {
        assert!(WirePayloadLogging::enabled_for_debug_build().is_enabled());
    }
    #[cfg(not(all(feature = "wire-diagnostics", debug_assertions)))]
    {
        // No constructor exists in this build configuration; the default is
        // the only value obtainable through the public API.
        assert_eq!(WirePayloadLogging::default(), WirePayloadLogging::Disabled);
    }
    #[cfg(not(debug_assertions))]
    {
        // Release build: even with the feature compiled in there is no
        // constructor, so wire logging cannot be turned on at all.
        assert!(!WirePayloadLogging::default().is_enabled());
    }
}

#[cfg(all(feature = "wire-diagnostics", debug_assertions))]
#[test]
fn wire_payload_logging_writes_redacted_frames_when_explicitly_enabled() {
    let dir = tempfile::tempdir().unwrap();
    let config = LoggingConfig::new(dir.path())
        .with_filter("debug")
        .with_wire_payload_logging(WirePayloadLogging::enabled_for_debug_build());
    let (log, _handle) = with_logging(config, |handle| {
        handle.wire_payload("plaud-ble", || format!("frame token={OPENROUTER_KEY}"));
    });
    assert!(log.contains("wire payload"), "frame was not logged: {log}");
    assert!(
        !log.contains("sk-or-v1-9f3c"),
        "frame logging bypassed redaction: {log}"
    );
}

#[test]
fn rotation_bounds_the_log_directory() {
    let dir = tempfile::tempdir().unwrap();
    let config = LoggingConfig::new(dir.path())
        .with_filter("info")
        .with_rotation(512, 3);
    let (_log, _handle) = with_logging(config, |_| {
        for index in 0..200 {
            tracing::info!(
                index,
                "a reasonably long log line to force rotation quickly"
            );
        }
    });

    let files: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(files.len() <= 3, "rotation kept too many files: {files:?}");
    assert!(
        files.contains(&LOG_FILE_NAME.to_owned()),
        "active file missing: {files:?}"
    );
    let total: u64 = fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|entry| entry.metadata().ok())
        .map(|meta| meta.len())
        .sum();
    assert!(
        total <= 512 * 3,
        "rotation did not bound total size: {total}"
    );
}

#[test]
fn init_logging_installs_the_global_subscriber_exactly_once() {
    let dir = tempfile::tempdir().unwrap();
    let first = plaudon_diagnostics::init_logging(
        LoggingConfig::new(dir.path().join("global")).with_filter("info"),
    );
    assert!(first.is_ok(), "first init failed: {:?}", first.err());

    let second = plaudon_diagnostics::init_logging(
        LoggingConfig::new(dir.path().join("global2")).with_filter("info"),
    );
    assert!(
        matches!(
            second,
            Err(plaudon_diagnostics::LoggingError::AlreadyInitialized)
        ),
        "second init should be refused"
    );

    let handle = first.unwrap();
    tracing::info!("global sink line with {OPENROUTER_KEY}");
    handle.flush().unwrap();
    let log = read_log(&dir.path().join("global"));
    assert!(!log.contains("sk-or-v1-9f3c"), "global sink leaked: {log}");
}
