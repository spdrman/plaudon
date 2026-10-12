//! Documentation lint for the public documentation set (spec 1.6, 22(10), 23, 24).
//!
//! Three failure modes are cheap to introduce and expensive to retract:
//! inventing evidence vocabulary, making an unmeasured performance or privacy
//! claim, and implying vendor endorsement. This lint blocks all three, plus
//! broken internal links.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::compat::EVIDENCE_VOCABULARY;

/// A phrase documentation may never contain, with the reason it is forbidden.
pub const FORBIDDEN_CLAIMS: &[(&str, &str)] = &[
    (
        "guaranteed erasure",
        "spec 23: we cannot guarantee a third party erased data",
    ),
    (
        "endorsed by plaud",
        "spec 22(10): the product is independent; no endorsement may be implied",
    ),
    (
        "official plaud app",
        "spec 22(10): we are not the vendor application",
    ),
    (
        "in partnership with plaud",
        "spec 22(10): no partnership exists",
    ),
    ("military-grade", "spec 23: unmeasurable marketing claim"),
    (
        "100% accurate",
        "spec 17.2: accuracy is measured and reported, never asserted",
    ),
    (
        "perfect transcription",
        "spec 17.2: accuracy is measured and reported, never asserted",
    ),
    (
        "fastest",
        "spec 17.4: no comparative speed claim without a published measurement",
    ),
    (
        "unlimited free",
        "spec 23: free local inference means no per-call fee only",
    ),
    (
        "your data never leaves",
        "spec 16.2A: the egress guarantee covers app-initiated traffic only",
    ),
    (
        "bank-level security",
        "spec 23: unmeasurable marketing claim",
    ),
];

/// A claim that must be present verbatim-ish in the public documentation set,
/// because omitting it would mislead (spec 22(10), 23).
pub const REQUIRED_DISCLOSURES: &[(&str, &str)] = &[
    ("docs/public/privacy-disclosure.md", "app-initiated"),
    ("docs/public/privacy-disclosure.md", "Local-only Mode"),
    ("docs/public/vendor-matrix.md", "OpenRouter"),
    ("docs/public/vendor-matrix.md", "official PLAUD Cloud CLI"),
    ("docs/public/vendor-matrix.md", "unofficial"),
    ("docs/public/troubleshooting.md", "backup"),
];

/// One lint violation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// File the violation was found in, relative to the repository root.
    pub file: String,
    /// 1-based line number, or 0 for a whole-file finding.
    pub line: usize,
    /// What is wrong.
    pub message: String,
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}: {}", self.file, self.line, self.message)
    }
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Lints every Markdown file under `docs/public`.
///
/// `root` is the repository root. Returns every violation found rather than
/// the first, so one CI run reports the whole backlog.
pub fn lint_public_docs(root: &Path) -> Vec<Violation> {
    let mut violations = Vec::new();
    let docs = root.join("docs/public");
    let mut files: Vec<PathBuf> = Vec::new();
    for entry in walkdir::WalkDir::new(&docs).sort_by_file_name() {
        let Ok(entry) = entry else { continue };
        if entry.file_type().is_file() && entry.path().extension().is_some_and(|e| e == "md") {
            files.push(entry.into_path());
        }
    }

    if files.is_empty() {
        violations.push(Violation {
            file: "docs/public".to_owned(),
            line: 0,
            message: "the public documentation set is missing entirely".to_owned(),
        });
        return violations;
    }

    for file in &files {
        let name = relative(root, file);
        let Ok(text) = std::fs::read_to_string(file) else {
            violations.push(Violation {
                file: name,
                line: 0,
                message: "unreadable".to_owned(),
            });
            continue;
        };
        violations.extend(lint_text(&name, &text));
        violations.extend(lint_links(root, file, &name, &text));
    }

    for (file, phrase) in REQUIRED_DISCLOSURES {
        let path = root.join(file);
        let present = std::fs::read_to_string(&path).is_ok_and(|text| text.contains(phrase));
        if !present {
            violations.push(Violation {
                file: (*file).to_owned(),
                line: 0,
                message: format!("required disclosure missing: `{phrase}`"),
            });
        }
    }

    violations
}

/// Lints the body of one document.
pub fn lint_text(file: &str, text: &str) -> Vec<Violation> {
    let mut violations = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let lower = line.to_lowercase();
        for (phrase, reason) in FORBIDDEN_CLAIMS {
            if lower.contains(phrase) {
                violations.push(Violation {
                    file: file.to_owned(),
                    line: index + 1,
                    message: format!("forbidden claim `{phrase}`: {reason}"),
                });
            }
        }
        violations.extend(lint_status_cell(file, index + 1, line));
    }
    violations
}

/// A table row of the form `| ... | **Term** | ... |` must use the spec 1.6
/// vocabulary for its claim column.
fn lint_status_cell(file: &str, line_number: usize, line: &str) -> Vec<Violation> {
    let mut violations = Vec::new();
    if !line.trim_start().starts_with('|') {
        return violations;
    }
    for cell in line.split('|') {
        let trimmed = cell.trim();
        let Some(term) = trimmed
            .strip_prefix("**")
            .and_then(|rest| rest.strip_suffix("**"))
        else {
            continue;
        };
        if !EVIDENCE_VOCABULARY.contains(&term) {
            violations.push(Violation {
                file: file.to_owned(),
                line: line_number,
                message: format!(
                    "`{term}` is not spec 1.6 evidence vocabulary; allowed: {}",
                    EVIDENCE_VOCABULARY.join(", ")
                ),
            });
        }
    }
    violations
}

/// Checks that every relative Markdown link resolves to a committed file.
fn lint_links(root: &Path, file: &Path, name: &str, text: &str) -> Vec<Violation> {
    let mut violations = Vec::new();
    let parent = file.parent().unwrap_or(root);
    for (index, line) in text.lines().enumerate() {
        for target in extract_link_targets(line) {
            if target.starts_with("http://")
                || target.starts_with("https://")
                || target.starts_with('#')
                || target.starts_with("mailto:")
            {
                continue;
            }
            let path_part = target.split('#').next().unwrap_or(&target);
            if path_part.is_empty() {
                continue;
            }
            let resolved = if let Some(absolute) = path_part.strip_prefix('/') {
                root.join(absolute)
            } else {
                parent.join(path_part)
            };
            if !resolved.exists() {
                violations.push(Violation {
                    file: name.to_owned(),
                    line: index + 1,
                    message: format!("broken relative link `{target}`"),
                });
            }
        }
    }
    violations
}

fn extract_link_targets(line: &str) -> BTreeSet<String> {
    let mut targets = BTreeSet::new();
    let bytes: Vec<char> = line.chars().collect();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == ']' && index + 1 < bytes.len() && bytes[index + 1] == '(' {
            let mut end = index + 2;
            while end < bytes.len() && bytes[end] != ')' {
                end += 1;
            }
            if end < bytes.len() {
                let target: String = bytes[index + 2..end].iter().collect();
                let target = target.split_whitespace().next().unwrap_or("").to_owned();
                if !target.is_empty() {
                    targets.insert(target);
                }
            }
            index = end;
        }
        index += 1;
    }
    targets
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_forbidden_claim_is_reported_with_its_line() {
        let violations = lint_text("docs/public/x.md", "ok\nWe offer bank-level security.\n");
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].line, 2);
        assert!(violations[0].message.contains("bank-level security"));
    }

    #[test]
    fn an_invented_status_term_fails_the_vocabulary_check() {
        let violations = lint_text("docs/public/x.md", "| macos | **Mostly works** | id |");
        assert_eq!(violations.len(), 1);
        assert!(violations[0].message.contains("not spec 1.6"));
    }

    #[test]
    fn every_allowed_term_passes_the_vocabulary_check() {
        for term in EVIDENCE_VOCABULARY {
            let line = format!("| macos | **{term}** | id |");
            assert!(
                lint_text("docs/public/x.md", &line).is_empty(),
                "{term} must be allowed"
            );
        }
    }

    #[test]
    fn link_targets_are_extracted_from_markdown() {
        let targets =
            extract_link_targets("see [a](./a.md) and [b](../b.md#frag) and [c](https://x)");
        assert!(targets.contains("./a.md"));
        assert!(targets.contains("../b.md#frag"));
        assert!(targets.contains("https://x"));
    }

    #[test]
    fn a_broken_relative_link_is_reported() {
        let dir = tempfile::tempdir().expect("tempdir");
        let docs = dir.path().join("docs/public");
        std::fs::create_dir_all(&docs).expect("mkdir");
        let file = docs.join("a.md");
        std::fs::write(&file, "[missing](./nope.md)\n").expect("write");
        let violations = lint_links(
            dir.path(),
            &file,
            "docs/public/a.md",
            "[missing](./nope.md)\n",
        );
        assert_eq!(violations.len(), 1);
        assert!(violations[0].message.contains("broken relative link"));
    }

    #[test]
    fn an_absent_documentation_set_is_itself_a_violation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let violations = lint_public_docs(dir.path());
        assert!(violations.iter().any(|v| v.file == "docs/public"));
    }
}
