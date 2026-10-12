//! Crash reports and OS notifications are the two places a secret or a
//! transcript escapes the process without going through a log sink
//! (spec App. B.7, §18.6).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::fs;
use std::panic;

use plaudon_diagnostics::logging::{build_subscriber, LOG_FILE_NAME};
use plaudon_diagnostics::{install_crash_hook, redact_notification, LoggingConfig};

const OPENROUTER_KEY: &str =
    "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";

#[test]
fn crash_hook_writes_a_redacted_panic_line() {
    let dir = tempfile::tempdir().unwrap();
    let (_subscriber, handle) =
        build_subscriber(LoggingConfig::new(dir.path()).with_filter("info")).unwrap();
    handle.register_secret("QuietRiverLantern42").unwrap();

    install_crash_hook(&handle);
    let result = panic::catch_unwind(|| {
        panic!("decoder died holding {OPENROUTER_KEY} and QuietRiverLantern42");
    });
    // Restore the default hook so a later failing test still prints normally.
    let _ = panic::take_hook();

    assert!(result.is_err(), "the panic should still propagate");
    handle.flush().unwrap();
    let log = fs::read_to_string(dir.path().join(LOG_FILE_NAME)).unwrap();

    assert!(
        log.contains("PANIC location="),
        "no crash line written: {log}"
    );
    assert!(
        !log.contains("sk-or-v1-9f3c"),
        "crash report leaked the API key: {log}"
    );
    assert!(
        !log.contains("QuietRiverLantern42"),
        "crash report leaked a registered secret: {log}"
    );
    assert!(
        log.contains("<redacted:openrouter_key>"),
        "marker missing: {log}"
    );
    assert!(
        log.contains("<redacted:registered>"),
        "registered marker missing: {log}"
    );
    assert!(
        log.contains("crash_and_notifications.rs"),
        "panic location should survive redaction: {log}"
    );
}

#[test]
fn notification_from_a_transcript_excerpt_loses_the_excerpt() {
    let excerpt = "Dana said we should drop the Henderson contract before the audit";
    let (title, body) = redact_notification("New transcript", excerpt);

    assert!(
        !body.contains("Henderson"),
        "transcript content leaked into the body: {body}"
    );
    assert!(
        !body.contains("Dana"),
        "speaker name leaked into the body: {body}"
    );
    assert!(
        !title.contains("Henderson"),
        "transcript content leaked into the title: {title}"
    );
    assert_eq!(title, "Plaudon: Transcript");
    assert_eq!(body, "Open Plaudon for details.");
}

#[test]
fn notification_keeps_counts_and_recording_identifiers() {
    let (title, body) = redact_notification(
        "Transcription complete",
        "2 recordings finished for rec_7hd92kfa; preview: \"we agreed to ship on Friday\"",
    );
    assert_eq!(title, "Plaudon: Transcription complete");
    assert!(body.contains("2 recordings"), "count missing: {body}");
    assert!(
        body.contains("rec_7hd92kfa"),
        "recording id missing: {body}"
    );
    assert!(!body.contains("ship on Friday"), "content leaked: {body}");
}

#[test]
fn notification_drops_secrets_before_summarising() {
    let (title, body) = redact_notification(
        &format!("Sync failed with {OPENROUTER_KEY}"),
        &format!("device token 0123456789abcdef0123456789abcdef and key {OPENROUTER_KEY}"),
    );
    assert!(
        !title.contains("sk-or-v1-9f3c"),
        "key leaked into the title: {title}"
    );
    assert!(
        !body.contains("sk-or-v1-9f3c"),
        "key leaked into the body: {body}"
    );
    assert!(
        !body.contains("0123456789abcdef"),
        "device token leaked into the body: {body}"
    );
    assert_eq!(title, "Plaudon: Sync failed");
}
