//! Secret-exclusion enforcement for archives (spec 13.6, 22(9), J2.4).
//!
//! Exclusion has two halves and this module is the second one:
//!
//! 1. **Allowlist.** [`crate::archive::documents`] names every field that may
//!    be serialized. A column added to SQLite later is absent from an archive
//!    until somebody adds it on purpose, and `tests/integration` fails until
//!    that decision is recorded.
//! 2. **Detection.** Before an archive is committed, every document it
//!    contains is scanned with the **same** pattern table the log redactor
//!    uses (`plaudon_diagnostics::patterns`). There is no second definition of
//!    "what a secret looks like" in the workspace, so a shape the log sink
//!    scrubs is a shape an archive cannot carry.
//!
//! Scanning is **field-scoped**: documents are JSON, so each string value is
//! scanned on its own rather than as one flat blob. That matters because the
//! shared `device_token_hex` pattern matches any 16/32/64 hex run, and this
//! format is built out of SHA-256 digests and UUIDs. Scanning per value lets
//! us state a short, documented exception list instead of either drowning in
//! false positives or weakening the pattern for everybody else.
//!
//! A detection is fatal: the archive is abandoned and no file is committed.
//! We do not redact and continue, because a credential appearing where the
//! allowlist says none can appear means the allowlist is wrong.

use std::collections::BTreeSet;

use plaudon_diagnostics::redact::Redactor;
use serde_json::Value;

use crate::error::{ExportError, ExportResult};

/// Bytes scanned per window. Values larger than this are scanned in
/// overlapping windows so a credential spanning a boundary is still found.
pub const WINDOW_BYTES: usize = 256 * 1024;

/// Overlap between windows, comfortably longer than any credential shape.
pub const WINDOW_OVERLAP_BYTES: usize = 4 * 1024;

/// Keys whose value is a content digest this format is built on.
pub const DIGEST_KEYS: &[&str] = &["sha256", "input_sha256", "integrity_digest"];

/// One documented reason a pattern hit is expected in a given position.
///
/// The list is deliberately tiny and lives next to the scanner, mirroring the
/// documented-allowlist rule the log scanner follows: an undocumented
/// exception is how a scanner quietly stops working.
pub const DOCUMENTED_EXCEPTIONS: &[(&str, &str)] = &[
    (
        "sha256 | input_sha256 | integrity_digest",
        "A 64-character lowercase hex value in one of these fields is a SHA-256 content digest. The archive is content-addressed: if this value were not the real digest of the bytes, verification would reject the archive.",
    ),
    (
        "path (media/<sha256>.<ext>)",
        "A media entry path contains the content digest by construction, and the reader rejects a media path whose stem is not its entry digest.",
    ),
    (
        "any canonical UUID value",
        "Recording, revision, asset and output identities are UUIDs. Credentials are never stored in the database at all, so a UUID-shaped value is an identity, not a secret.",
    ),
];

/// One credential-shaped hit. Never carries the matched bytes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SecretFinding {
    /// Archive-relative path of the entry that contained it.
    pub entry_path: String,
    /// JSON pointer to the value, or `/` for a non-JSON document.
    pub field: String,
    /// Stable pattern name from the shared table.
    pub pattern: String,
}

/// Scans archive entries for credential shapes.
#[derive(Debug, Clone, Copy, Default)]
pub struct ArchiveSecretScanner;

impl ArchiveSecretScanner {
    /// Scan one metadata document.
    ///
    /// Binary media payloads are not scanned: they are user audio, not
    /// serialized database fields, and a false positive there would abort a
    /// backup for nothing. The spec requires the scan to cover the manifest,
    /// record JSON and annotation free text, which is exactly what passes
    /// through here.
    #[must_use]
    pub fn findings(entry_path: &str, document: &[u8]) -> Vec<SecretFinding> {
        let Ok(text) = std::str::from_utf8(document) else {
            return Vec::new();
        };
        let mut found: BTreeSet<SecretFinding> = BTreeSet::new();
        match serde_json::from_str::<Value>(text) {
            Ok(value) => walk(entry_path, "", None, &value, &mut found),
            Err(_) => scan_value(entry_path, "/", text, &mut found),
        }
        found.into_iter().collect()
    }

    /// Fail closed when an entry carries a credential shape.
    pub fn enforce(entry_path: &str, document: &[u8]) -> ExportResult<()> {
        let findings = Self::findings(entry_path, document);
        if findings.is_empty() {
            return Ok(());
        }
        let detail: Vec<String> = findings
            .iter()
            .map(|finding| format!("{} at {}", finding.pattern, finding.field))
            .collect();
        Err(ExportError::SecretExclusionFailed(format!(
            "entry {entry_path} matches credential pattern(s) {}; nothing was written",
            detail.join(", ")
        )))
    }
}

fn walk(
    entry_path: &str,
    pointer: &str,
    key: Option<&str>,
    value: &Value,
    found: &mut BTreeSet<SecretFinding>,
) {
    match value {
        Value::String(text) => {
            if is_expected_identity(key, text) {
                return;
            }
            let field = if pointer.is_empty() { "/" } else { pointer };
            scan_value(entry_path, field, text, found);
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                walk(entry_path, &format!("{pointer}/{index}"), key, item, found);
            }
        }
        Value::Object(fields) => {
            for (name, item) in fields {
                walk(
                    entry_path,
                    &format!("{pointer}/{name}"),
                    Some(name),
                    item,
                    found,
                );
            }
        }
        _ => {}
    }
}

fn scan_value(entry_path: &str, field: &str, text: &str, found: &mut BTreeSet<SecretFinding>) {
    let redactor = Redactor::shared();
    for window in windows(text) {
        for finding in redactor.matches(window) {
            found.insert(SecretFinding {
                entry_path: entry_path.to_owned(),
                field: field.to_owned(),
                pattern: finding.pattern.to_owned(),
            });
        }
    }
}

/// Whether this value is one of the documented identity shapes.
fn is_expected_identity(key: Option<&str>, value: &str) -> bool {
    if is_uuid(value) {
        return true;
    }
    match key {
        Some(key) if DIGEST_KEYS.contains(&key) => is_hex64(value),
        Some("path") => is_content_addressed_media_path(value),
        _ => false,
    }
}

fn is_hex64(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

fn is_uuid(value: &str) -> bool {
    let groups = [8usize, 4, 4, 4, 12];
    let mut parts = value.split('-');
    for expected in groups {
        let Some(part) = parts.next() else {
            return false;
        };
        if part.len() != expected || !part.bytes().all(|b| b.is_ascii_hexdigit()) {
            return false;
        }
    }
    parts.next().is_none()
}

fn is_content_addressed_media_path(value: &str) -> bool {
    let Some(name) = value.strip_prefix("media/") else {
        return false;
    };
    let Some((stem, extension)) = name.split_once('.') else {
        return false;
    };
    is_hex64(stem) && !extension.is_empty() && extension.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Split `text` into overlapping windows at character boundaries.
fn windows(text: &str) -> Vec<&str> {
    if text.len() <= WINDOW_BYTES {
        return vec![text];
    }
    let mut out = Vec::new();
    let mut start = 0usize;
    while start < text.len() {
        let mut end = (start + WINDOW_BYTES).min(text.len());
        while end < text.len() && !text.is_char_boundary(end) {
            end += 1;
        }
        out.push(&text[start..end]);
        if end == text.len() {
            break;
        }
        let mut next = end.saturating_sub(WINDOW_OVERLAP_BYTES);
        while next < text.len() && !text.is_char_boundary(next) {
            next += 1;
        }
        if next <= start {
            break;
        }
        start = next;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clean_document_passes() {
        let document = br#"{"title":"Weekly sync","annotation":"Discussed the roadmap."}"#;
        assert_eq!(
            ArchiveSecretScanner::findings("records/a.json", document),
            Vec::new()
        );
        ArchiveSecretScanner::enforce("records/a.json", document).unwrap();
    }

    #[test]
    fn content_digests_and_identities_are_not_mistaken_for_tokens() {
        let digest = "a".repeat(64);
        let document = format!(
            r#"{{"sha256":"{digest}","input_sha256":"{digest}","integrity_digest":"{digest}",
                 "id":"01932b3a-4c5d-7e8f-9a0b-1c2d3e4f5061",
                 "path":"media/{digest}.wav"}}"#
        );
        assert_eq!(
            ArchiveSecretScanner::findings("manifest.json", document.as_bytes()),
            Vec::new()
        );
    }

    #[test]
    fn the_same_hex_run_in_free_text_is_still_a_finding() {
        let digest = "a".repeat(64);
        let document = format!(r#"{{"annotation":"device token {digest}"}}"#);
        let findings = ArchiveSecretScanner::findings("records/a.json", document.as_bytes());
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].pattern, "device_token_hex");
        assert_eq!(findings[0].field, "/annotation");
    }

    #[test]
    fn an_api_key_in_an_annotation_aborts_the_archive() {
        let document =
            br#"{"annotation":"my key is sk-or-v1-0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"}"#;
        let error = ArchiveSecretScanner::enforce("records/a.json", document).unwrap_err();
        match error {
            ExportError::SecretExclusionFailed(message) => {
                assert!(message.contains("records/a.json"));
                assert!(message.contains("/annotation"));
                assert!(message.contains("nothing was written"));
                assert!(
                    !message.contains("0123456789abcdef"),
                    "the message must not repeat the secret"
                );
            }
            other => panic!("expected a secret-exclusion failure, got {other:?}"),
        }
    }

    #[test]
    fn a_signed_url_and_a_bearer_token_are_both_caught() {
        let url = br#"{"note":"https://files.example.com/a.wav?X-Amz-Signature=deadbeefdeadbeefdeadbeefdeadbeef&X-Amz-Expires=86400"}"#;
        assert_ne!(
            ArchiveSecretScanner::findings("records/a.json", url),
            Vec::new()
        );

        let bearer = br#"{"note":"Authorization: Bearer abcdefghijklmnopqrstuvwxyz012345"}"#;
        assert_ne!(
            ArchiveSecretScanner::findings("records/b.json", bearer),
            Vec::new()
        );
    }

    #[test]
    fn a_nested_value_reports_its_json_pointer() {
        let document =
            br#"{"outputs":[{"body":{"note":"Bearer abcdefghijklmnopqrstuvwxyz012345"}}]}"#;
        let findings = ArchiveSecretScanner::findings("outputs/a.json", document);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].field, "/outputs/0/body/note");
    }

    #[test]
    fn a_secret_spanning_a_window_boundary_is_still_found() {
        // The pattern keys on a word boundary, so the secret is preceded by a
        // separator exactly as it would be inside a JSON document.
        let mut document = "x".repeat(WINDOW_BYTES - 9);
        document.push(' ');
        document
            .push_str("sk-or-v1-0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef");
        document.push_str(&"y".repeat(1024));
        let findings = ArchiveSecretScanner::findings("records/a.json", document.as_bytes());
        assert_ne!(findings, Vec::new(), "boundary-spanning secret missed");
    }

    #[test]
    fn binary_payloads_are_not_scanned_as_text() {
        let audio = [0xffu8, 0xfe, 0x00, 0x01, 0x80];
        assert_eq!(
            ArchiveSecretScanner::findings("media/aa.wav", &audio),
            Vec::new()
        );
    }

    #[test]
    fn every_exception_is_documented() {
        assert_eq!(DOCUMENTED_EXCEPTIONS.len(), 3);
        for (position, reason) in DOCUMENTED_EXCEPTIONS {
            assert_ne!(*position, "");
            assert!(reason.len() > 40, "an exception needs a real reason");
        }
    }

    #[test]
    fn windows_cover_the_whole_document() {
        let text = "é".repeat(WINDOW_BYTES);
        let parts = windows(&text);
        assert!(parts.len() > 1);
        assert_eq!(parts[0].as_ptr(), text.as_ptr());
        let last = parts.last().unwrap();
        let end = (last.as_ptr() as usize - text.as_ptr() as usize) + last.len();
        assert_eq!(end, text.len(), "the final window must reach the end");
    }
}
