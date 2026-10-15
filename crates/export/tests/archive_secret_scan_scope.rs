//! Spec 13.6/22(9) and VP-078/VP-112: what the archive secret scanner refuses,
//! and the two places where it deliberately does not.
//!
//! The scanner runs the shared `plaudon_diagnostics` pattern table over every
//! string value of every document. Two exceptions make the format usable at
//! all — content-addressed identities, and sealed biometric payloads inside
//! `voiceprofiles/` — and both are narrow enough that a real credential in the
//! same position is still fatal. Every test here asserts which pattern fired
//! on which JSON pointer, because "the archive was refused" is not the same
//! claim as "the archive was refused for the right reason".

use plaudon_diagnostics::patterns::{API_KEY, BEARER, BINARY_BLOB, DEVICE_TOKEN, URL_USERINFO};
use plaudon_export::archive::secrets::{ArchiveSecretScanner, SecretFinding};
use plaudon_export::archive::voiceprofiles::NAMESPACE;
use plaudon_export::error::ExportError;

/// An API key the shared table matches, kept in one place so no test invents a
/// shape the redactor does not actually recognise.
const API_KEY_VALUE: &str = "sk-abcdefghijklmnopqrstuvwx";

fn patterns(findings: &[SecretFinding]) -> Vec<&str> {
    findings
        .iter()
        .map(|finding| finding.pattern.as_str())
        .collect()
}

fn fields(findings: &[SecretFinding]) -> Vec<&str> {
    findings
        .iter()
        .map(|finding| finding.field.as_str())
        .collect()
}

fn refusal_message(entry_path: &str, document: &[u8]) -> String {
    match ArchiveSecretScanner::enforce(entry_path, document).unwrap_err() {
        ExportError::SecretExclusionFailed(message) => message,
        other => panic!("expected a secret-exclusion failure, got {other:?}"),
    }
}

/// A sealed embedding: a long opaque run, which is exactly the
/// `binary_blob` redaction-only shape.
fn sealed_blob() -> String {
    "QUJDRGVmZ2hpSktMbW5vcFFSU3R1dnd4WXowMTIzNDU2Nzg5".repeat(5)
}

// ---------------------------------------------------------------------------
// Document shapes
// ---------------------------------------------------------------------------

#[test]
fn a_document_that_is_one_bare_string_reports_the_document_root_as_its_field() {
    let document = format!("\"{API_KEY_VALUE}\"");
    let findings = ArchiveSecretScanner::findings("records/a.json", document.as_bytes());

    assert_eq!(patterns(&findings), vec![API_KEY]);
    assert_eq!(
        fields(&findings),
        vec!["/"],
        "a value with no JSON pointer is reported at the document root"
    );
    assert!(refusal_message("records/a.json", document.as_bytes()).contains(" at /"));
}

// ---------------------------------------------------------------------------
// The voiceprofiles/ exception (VP-078, VP-112)
// ---------------------------------------------------------------------------

#[test]
fn a_sealed_embedding_is_carried_only_inside_the_voiceprofiles_namespace() {
    let document = format!("{{\"sealed_embedding\":\"{}\"}}", sealed_blob());

    // Inside the encrypted-only namespace the sealed payload is the point of
    // the entry: the redaction-only shape patterns are skipped and the backup
    // proceeds. Before this exception existed, every encrypted backup carrying
    // a voice profile failed here.
    let inside = format!("{NAMESPACE}01932b3a-4c5d-7e8f-9a0b-1c2d3e4f5061.json");
    assert_eq!(
        ArchiveSecretScanner::findings(&inside, document.as_bytes()),
        Vec::new()
    );
    ArchiveSecretScanner::enforce(&inside, document.as_bytes()).unwrap();

    // The same bytes anywhere else are biometric material in a namespace that
    // may not hold it, and the archive is abandoned.
    let findings = ArchiveSecretScanner::findings("records/a.json", document.as_bytes());
    assert!(
        patterns(&findings).contains(&BINARY_BLOB),
        "unexpected {findings:?}"
    );
    assert!(ArchiveSecretScanner::enforce("records/a.json", document.as_bytes()).is_err());
}

#[test]
fn a_real_credential_inside_voiceprofiles_is_still_fatal() {
    let entry = format!("{NAMESPACE}01932b3a-4c5d-7e8f-9a0b-1c2d3e4f5061.json");

    // An API key beside the sealed payload: only the shape patterns are
    // skipped in this namespace, so the credential still aborts the backup and
    // the sealed blob still does not appear in the reason.
    let with_key = format!(
        "{{\"sealed_embedding\":\"{}\",\"note\":\"{API_KEY_VALUE}\"}}",
        sealed_blob()
    );
    let findings = ArchiveSecretScanner::findings(&entry, with_key.as_bytes());
    assert_eq!(patterns(&findings), vec![API_KEY]);
    assert_eq!(fields(&findings), vec!["/note"]);
    let message = refusal_message(&entry, with_key.as_bytes());
    assert!(message.contains(API_KEY), "{message}");
    assert!(message.contains("/note"), "{message}");
    assert!(!message.contains(&sealed_blob()), "{message}");

    // A bearer token is the same refusal.
    let with_bearer =
        br#"{"label":"desk mic","hint":"Bearer abcdefghijklmnopqrstuvwxyz012345"}"#.to_vec();
    assert_eq!(
        patterns(&ArchiveSecretScanner::findings(&entry, &with_bearer)),
        vec![BEARER]
    );
    assert!(ArchiveSecretScanner::enforce(&entry, &with_bearer).is_err());

    // So is userinfo in a URL.
    let with_url = br#"{"source":"https://operator:hunter2@voices.example.com/enrol"}"#.to_vec();
    assert_eq!(
        patterns(&ArchiveSecretScanner::findings(&entry, &with_url)),
        vec![URL_USERINFO]
    );
    assert!(ArchiveSecretScanner::enforce(&entry, &with_url).is_err());
}

// ---------------------------------------------------------------------------
// The identity exceptions
// ---------------------------------------------------------------------------

#[test]
fn only_a_complete_content_addressed_media_path_is_an_identity() {
    let digest = "a".repeat(64);

    // The documented shape: `media/<sha256>.<ext>`.
    let addressed = format!("{{\"path\":\"media/{digest}.wav\"}}");
    assert_eq!(
        ArchiveSecretScanner::findings("manifest.json", addressed.as_bytes()),
        Vec::new()
    );

    // No extension at all, an empty extension and a non-alphanumeric one are
    // not that shape, so the hex run is scanned like any other value and the
    // archive is refused.
    for value in [
        format!("media/{digest}"),
        format!("media/{digest}."),
        format!("media/{digest}.wav!"),
        format!("library/{digest}.wav"),
    ] {
        let document = format!("{{\"path\":\"{value}\"}}");
        let findings = ArchiveSecretScanner::findings("manifest.json", document.as_bytes());
        assert_eq!(patterns(&findings), vec![DEVICE_TOKEN], "for {value}");
        assert_eq!(fields(&findings), vec!["/path"], "for {value}");
    }
}

#[test]
fn only_a_complete_uuid_is_an_identity() {
    let digest = "a".repeat(64);
    let document = format!(
        "{{\"id\":\"01932b3a-4c5d-7e8f-9a0b-1c2d3e4f5061\",\
           \"partial\":\"deadbeef-dead-beef-dead\",\
           \"sha256\":\"{digest}\",\
           \"fingerprint\":\"{digest}\"}}"
    );

    let findings = ArchiveSecretScanner::findings("records/a.json", document.as_bytes());
    // The UUID and the documented digest key are exceptions; a value that only
    // begins like a UUID is not one, and a 64-hex run under an undocumented
    // key is a device token.
    assert_eq!(patterns(&findings), vec![DEVICE_TOKEN]);
    assert_eq!(fields(&findings), vec!["/fingerprint"]);
}

// ---------------------------------------------------------------------------
// Window splitting
// ---------------------------------------------------------------------------

#[test]
fn a_window_boundary_inside_a_character_is_moved_to_the_next_boundary() {
    // `WINDOW_BYTES` is 256 KiB and the overlap is 4 KiB. This value puts a
    // two-byte character across both offsets, so a scanner that sliced at the
    // raw byte offset would panic instead of scanning the tail.
    // Prose, not an opaque run: the filler is spaced so it is not itself one
    // of the long-blob shapes, which would drown the assertion below.
    let mut text = "abcdefghijklmno ".repeat(16_128); // 258_048 bytes
    text.push('é'); // straddles the start of the second window
    text.push_str(&"abcdefghijklmno ".repeat(255));
    text.push_str("abcdefghijkl "); // 4_093 bytes of filler in total
    text.push('é'); // straddles the end of the first window
    assert_eq!(text.len(), 262_145);
    text.push(' ');
    text.push_str(API_KEY_VALUE);

    let document = serde_json::to_vec(&serde_json::json!({ "annotation": text })).unwrap();
    let findings = ArchiveSecretScanner::findings("records/a.json", &document);

    assert_eq!(patterns(&findings), vec![API_KEY]);
    assert_eq!(fields(&findings), vec!["/annotation"]);
}
