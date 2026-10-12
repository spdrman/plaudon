//! VP-112: a voice profile may not reach a log path.
//!
//! Two things are protected here, and they are protected differently.
//!
//! * **An embedding has a shape.** A named field, a long numeric vector or a
//!   long opaque run of bytes is recognisable without knowing anything about
//!   the library, so [`plaudon_diagnostics::patterns`] carries the shapes and
//!   every text path gets them for free.
//! * **A person's name has no shape at all.** `Dana Scully` looks exactly like
//!   any other two words, so the only way to scrub it is to be told about it:
//!   [`register_profile_name`] is that channel, and the registry is
//!   process-wide so the log sink, the crash hook and the notification reducer
//!   cannot disagree about which names are sensitive.
//!
//! The whole file runs in one test binary on purpose: the name registry is
//! process-wide, so these tests are the only ones allowed to populate it, and
//! they take [`REGISTRY`] so two of them never share it at once. That the
//! registry needs a guard here is the honest consequence of the design it is
//! testing — one roster for the whole process, not one per logging handle.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::fs;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use plaudon_diagnostics::logging::{build_subscriber, LOG_FILE_NAME};
use plaudon_diagnostics::patterns::MARKER_PREFIXES;
use plaudon_diagnostics::{
    forget_profile_names, redact_notification, register_profile_name, registered_profile_names,
    LoggingConfig, LoggingHandle, Redactor, RegisterProfileNameError,
};

/// Serialises the tests that populate the process-wide name registry.
static REGISTRY: Mutex<()> = Mutex::new(());

/// Take the registry for the duration of a test, starting from empty.
fn exclusive_registry() -> MutexGuard<'static, ()> {
    let guard = REGISTRY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    forget_profile_names();
    guard
}

/// A 192-component embedding, printed the way a careless `tracing::info!` would
/// print one.
fn float_vector(components: usize) -> String {
    use std::fmt::Write as _;

    let mut out = String::from("[");
    for i in 0..components {
        if i > 0 {
            out.push_str(", ");
        }
        // Deterministic, and shaped like a unit-norm component.
        #[allow(clippy::cast_precision_loss)]
        let value = (i as f64).mul_add(0.0037, -0.35);
        let _ = write!(out, "{value:.6}");
    }
    out.push(']');
    out
}

/// The sealed form: XChaCha20-Poly1305 ciphertext, base64-encoded.
fn sealed_blob(bytes: usize) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    (0..bytes)
        .map(|i| char::from(ALPHABET[(i * 7 + 13) % ALPHABET.len()]))
        .collect()
}

fn read_log(dir: &Path) -> String {
    fs::read_to_string(dir.join(LOG_FILE_NAME)).unwrap_or_default()
}

fn with_logging<F>(config: LoggingConfig, body: F) -> String
where
    F: FnOnce(&LoggingHandle),
{
    let directory = config.directory.clone();
    let (subscriber, handle) = build_subscriber(config).unwrap();
    tracing::subscriber::with_default(subscriber, || body(&handle));
    handle.flush().unwrap();
    read_log(&directory)
}

#[test]
fn an_embedding_cannot_be_logged_in_any_shape_it_takes() {
    let redactor = Redactor::new();
    let vector = float_vector(192);
    let blob = sealed_blob(1046);

    let lines = [
        format!(r#"{{"person_id":"018f3b3a","embedding":{vector}}}"#),
        format!("VoiceExemplar {{ voiceprint: {vector}, speech_ms: 4200 }}"),
        format!("speaker_embedding=\"{blob}\" dim=192"),
        format!("match window scores {vector} done"),
        format!("restoring exemplar payload {blob}"),
    ];

    for line in &lines {
        let output = redactor.redact(line);
        assert!(
            !output.contains("-0.350000"),
            "a vector component survived: {output}"
        );
        assert!(
            !output.contains(&blob[..64]),
            "a sealed blob survived: {output}"
        );
        assert!(
            output.len() < line.len(),
            "nothing was redacted from: {output}"
        );
        for finding in redactor.matches(line) {
            assert!(
                MARKER_PREFIXES
                    .iter()
                    .any(|prefix| finding.marker().contains(prefix)),
                "undocumented marker `{}`",
                finding.marker()
            );
        }
        let once = output.into_owned();
        assert_eq!(
            redactor.redact(&once).as_ref(),
            once,
            "redaction of a voice shape is not idempotent"
        );
    }
}

#[test]
fn the_dimension_of_an_embedding_stays_readable_so_a_bug_is_still_diagnosable() {
    let redactor = Redactor::new();
    // What `plaudon_app_core::voice_profiles::Embedding`'s Debug impl prints:
    // the checkpoint and the dimension, never the components.
    let line = "Embedding { model: ecapa-tdnn-v1, dim: 192, checkpoint: sha256-pinned }";
    assert_eq!(redactor.redact(line).as_ref(), line);
}

#[test]
fn an_embedding_shaped_sentence_in_prose_is_not_destroyed() {
    let redactor = Redactor::new();
    for benign in [
        "recipe: scripted embeddings: speaker direction plus seeded spread",
        "voiceprint storage is sealed; see ADR-0080",
        "speaker_vector = unavailable",
    ] {
        assert_eq!(
            redactor.redact(benign).as_ref(),
            benign,
            "a design note was redacted"
        );
    }
}

#[test]
fn a_registered_display_name_is_scrubbed_from_every_text_path() {
    let _registry = exclusive_registry();
    register_profile_name("Dana Scully").unwrap();

    // 1. An ordinary redactor, which is what the sink holds.
    let redactor = Redactor::new();
    let line = "suggestion for Dana Scully accepted";
    let output = redactor.redact(line);
    assert!(!output.contains("Dana"), "name survived: {output}");
    assert!(output.contains("<redacted:profile_name>"), "{output}");

    // 2. Case and whitespace are not an escape hatch.
    for variant in [
        "dana scully",
        "DANA SCULLY",
        "Dana  Scully",
        "Dana\nScully",
        "name=Dana Scully,",
    ] {
        let output = redactor.redact(variant);
        assert!(
            !output.to_lowercase().contains("scully"),
            "variant `{variant}` survived: {output}"
        );
    }

    // 3. The process-wide surfaces: the panic hook and the notification
    //    reducer both go through `Redactor::shared()`.
    let shared = Redactor::shared().redact("panic at speaker merge for Dana Scully");
    assert!(!shared.contains("Scully"), "crash path leaked: {shared}");
    let (title, body) = redact_notification("Plaudon", "Dana Scully was matched in 2 recordings.");
    assert!(!body.contains("Scully"), "notification leaked: {body}");
    assert!(
        !title.contains("Scully"),
        "notification title leaked: {title}"
    );

    forget_profile_names();
}

#[test]
fn a_registered_name_never_reaches_the_log_file() {
    let _registry = exclusive_registry();
    register_profile_name("Henderson").unwrap();

    let dir = tempfile::tempdir().unwrap();
    let config = LoggingConfig::new(dir.path()).with_filter("debug");
    let log = with_logging(config, |_handle| {
        tracing::info!("Henderson at the start");
        tracing::info!("matched the {} cluster", "Henderson");
        tracing::info!(person = "Henderson", "inside a field value");
    });

    assert!(!log.contains("Henderson"), "name reached the log: {log}");
    assert_eq!(
        log.matches("<redacted:profile_name>").count(),
        3,
        "expected one marker per record: {log}"
    );

    forget_profile_names();
}

#[test]
fn a_name_is_matched_as_a_word_and_not_inside_another_one() {
    let _registry = exclusive_registry();
    register_profile_name("Jo").unwrap();

    let redactor = Redactor::new();
    assert_eq!(
        redactor.redact("job 7 joined the queue, jovial").as_ref(),
        "job 7 joined the queue, jovial",
        "a name matched inside longer words"
    );
    let output = redactor.redact("Jo confirmed the suggestion");
    assert!(!output.starts_with("Jo "), "a standalone name survived");
    assert!(output.contains("<redacted:profile_name>"), "{output}");

    forget_profile_names();
}

#[test]
fn an_unregistered_name_is_left_alone_because_redaction_is_not_a_proof() {
    let _registry = exclusive_registry();
    let redactor = Redactor::new();
    let line = "suggestion for Mulder accepted";
    assert_eq!(
        redactor.redact(line).as_ref(),
        line,
        "the redactor invented a match it was never told about"
    );
}

#[test]
fn the_name_registry_is_bounded_deduplicated_and_clearable() {
    let _registry = exclusive_registry();
    assert_eq!(registered_profile_names(), 0);

    assert_eq!(
        register_profile_name("J"),
        Err(RegisterProfileNameError::TooShort)
    );
    assert_eq!(
        register_profile_name(&"n".repeat(200)),
        Err(RegisterProfileNameError::TooLong)
    );

    register_profile_name("Dana Scully").unwrap();
    register_profile_name("  Dana   Scully  ").unwrap();
    register_profile_name("dana scully").unwrap();
    assert_eq!(
        registered_profile_names(),
        1,
        "the same name was stored more than once"
    );

    for i in 0..600 {
        let name = format!("Person Number {i:04}");
        if let Err(error) = register_profile_name(&name) {
            assert_eq!(error, RegisterProfileNameError::RegistryFull);
            assert_eq!(registered_profile_names(), 512);
            forget_profile_names();
            assert_eq!(registered_profile_names(), 0);
            return;
        }
    }
    panic!("the registry accepted more than its documented maximum");
}
