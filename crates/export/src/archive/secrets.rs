//! Secret-exclusion enforcement for archives (spec 13.6, 22(9), J2.4).
//!
//! Exclusion has two halves and this module is the second one:
//!
//! 1. **Allowlist.** [`crate::bundle`] names every field that may be
//!    serialized. A column added to SQLite later is absent from an archive
//!    until somebody adds it on purpose, and `tests/integration` fails until
//!    that decision is recorded.
//! 2. **Detection.** Before an archive is committed, every metadata document
//!    it contains is scanned with the **same** pattern table the log redactor
//!    uses (`plaudon_diagnostics::patterns`). There is no second definition of
//!    "what a secret looks like" in the workspace, so a shape the log sink
//!    scrubs is a shape an archive cannot carry.
//!
//! A detection is fatal: the archive is abandoned and no file is committed.
//! We do not redact and continue, because a credential appearing where the
//! allowlist says none can appear means the allowlist is wrong.

use std::collections::BTreeSet;

use plaudon_diagnostics::redact::Redactor;

use crate::error::{ExportError, ExportResult};

/// Bytes scanned per window. Documents larger than this are scanned in
/// overlapping windows so a credential spanning a boundary is still found.
pub const WINDOW_BYTES: usize = 256 * 1024;

/// Overlap between windows, comfortably longer than any credential shape.
pub const WINDOW_OVERLAP_BYTES: usize = 4 * 1024;

/// One credential-shaped hit. Never carries the matched bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretFinding {
    /// Archive-relative path of the entry that contained it.
    pub entry_path: String,
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
        let redactor = Redactor::shared();
        let mut names: BTreeSet<&'static str> = BTreeSet::new();
        for window in windows(text) {
            for finding in redactor.matches(window) {
                names.insert(finding.pattern);
            }
        }
        names
            .into_iter()
            .map(|pattern| SecretFinding {
                entry_path: entry_path.to_owned(),
                pattern: pattern.to_owned(),
            })
            .collect()
    }

    /// Fail closed when an entry carries a credential shape.
    pub fn enforce(entry_path: &str, document: &[u8]) -> ExportResult<()> {
        let findings = Self::findings(entry_path, document);
        if findings.is_empty() {
            return Ok(());
        }
        let patterns: Vec<&str> = findings.iter().map(|f| f.pattern.as_str()).collect();
        Err(ExportError::SecretExclusionFailed(format!(
            "entry {entry_path} matches credential pattern(s) {}; nothing was written",
            patterns.join(", ")
        )))
    }
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
    fn an_api_key_in_an_annotation_aborts_the_archive() {
        let document =
            br#"{"annotation":"my key is sk-or-v1-0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"}"#;
        let error = ArchiveSecretScanner::enforce("records/a.json", document).unwrap_err();
        match error {
            ExportError::SecretExclusionFailed(message) => {
                assert!(message.contains("records/a.json"));
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
    fn a_secret_spanning_a_window_boundary_is_still_found() {
        // The pattern keys on a word boundary, so the secret is preceded by a
        // separator exactly as it would be inside a JSON document.
        let mut document = "x".repeat(WINDOW_BYTES - 9);
        document.push(' ');
        document.push_str(
            "sk-or-v1-0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        );
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
