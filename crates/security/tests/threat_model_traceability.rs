//! Drift guard for the threat-model documentation set (SPEC §16, §6.3A, §22.8).
//!
//! `docs/threat-model/traceability.json` is the normative threat/control/
//! verification matrix. This test is the reason it cannot rot: it checks the
//! matrix against the repository itself — every cited test function must
//! exist, every owner path must exist, every SPEC §16.2 threat must be covered
//! exactly once, every unresolved row must be restated as an open finding, and
//! every `NOT_GUARANTEED` statement must be reproduced in the documentation.
//!
//! `scripts/check-threat-model.py` performs the same checks for the docs-only
//! CI job. The two are kept in step by
//! [`python_checker_and_rust_test_share_the_same_rules`].

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use plaudon_security::outbound::NOT_GUARANTEED;
use serde::Deserialize;

const REQUIRED_ROW_KEYS: [&str; 9] = [
    "id",
    "spec",
    "threat",
    "control",
    "owner",
    "epic",
    "verification",
    "status",
    "notes",
];
const STATUSES: [&str; 3] = ["implemented", "open", "planned"];
const KNOWN_EPICS: [&str; 14] = [
    "A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K", "L", "M", "N",
];
const TABLE_BEGIN: &str = "<!-- BEGIN GENERATED MATRIX -->";
const TABLE_END: &str = "<!-- END GENERATED MATRIX -->";

/// Assurance language SPEC §16.2A / §16.6 / §23 forbid us from using about
/// this system. Keep in sync with `PROHIBITED_PHRASES` in
/// `scripts/check-threat-model.py`.
const PROHIBITED_PHRASES: [&str; 12] = [
    "fully isolated",
    "completely isolated",
    "cryptographically verified identity",
    "military-grade",
    "unhackable",
    "guarantees privacy",
    "guarantees confidentiality",
    "gdpr compliant",
    "hipaa compliant",
    "data residency guarantee",
    "securely erased",
    "forensically erased",
];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Matrix {
    version: u32,
    #[serde(rename = "_schema")]
    #[allow(dead_code)]
    schema: serde_json::Value,
    spec_threats: Vec<String>,
    rows: Vec<Row>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Row {
    id: String,
    spec: String,
    threat: String,
    control: String,
    owner: String,
    epic: String,
    verification: Vec<String>,
    status: String,
    notes: String,
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("workspace root is reachable from crates/security")
}

fn doc_dir() -> PathBuf {
    repo_root().join("docs").join("threat-model")
}

fn adr_files() -> Vec<PathBuf> {
    let adr = repo_root().join("docs").join("adr");
    vec![
        adr.join("ADR-0013-single-egress-gate.md"),
        adr.join("ADR-0014-no-plaintext-credential-fallback.md"),
        adr.join("ADR-0015-out-of-process-untrusted-input.md"),
    ]
}

fn checked_documents() -> Vec<PathBuf> {
    let mut docs: Vec<PathBuf> = fs::read_dir(doc_dir())
        .expect("docs/threat-model exists")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "md"))
        .collect();
    docs.sort();
    docs.extend(adr_files());
    docs
}

fn read(path: &Path) -> String {
    fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
}

fn load_matrix() -> Matrix {
    let path = doc_dir().join("traceability.json");
    let text = read(&path);
    let matrix: Matrix = serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("{} is not a valid matrix: {error}", path.display()));
    assert_eq!(matrix.version, 1, "unexpected matrix version");
    matrix
}

/// Every `.rs` file that may define a test, mirroring the Python checker.
fn rust_sources() -> Vec<PathBuf> {
    let root = repo_root();
    let mut files = Vec::new();
    for subtree in ["crates", "tests", "apps"] {
        collect_rust_files(&root.join(subtree), &mut files);
    }
    files.sort();
    files
}

fn collect_rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name == "target") {
                continue;
            }
            collect_rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// Name of the function defined on `line`, if the line defines one.
fn function_name(line: &str) -> Option<&str> {
    let index = line.find("fn ")?;
    let before_ok =
        index == 0 || !line[..index].ends_with(|c: char| c.is_alphanumeric() || c == '_');
    if !before_ok {
        return None;
    }
    let rest = &line[index + 3..];
    let end = rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))?;
    let (name, tail) = rest.split_at(end);
    if name.is_empty() || !tail.starts_with(['(', '<']) {
        return None;
    }
    Some(name)
}

/// Names of every function annotated as a test anywhere in the repository.
fn test_functions() -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for path in rust_sources() {
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let mut attributes: Vec<String> = Vec::new();
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("#[") {
                attributes.push(trimmed.to_owned());
                continue;
            }
            if let Some(name) = function_name(trimmed) {
                if attributes
                    .iter()
                    .any(|attribute| attribute.contains("test"))
                {
                    names.insert(name.to_owned());
                }
                attributes.clear();
                continue;
            }
            if !trimmed.is_empty() && !trimmed.starts_with("//") {
                attributes.clear();
            }
        }
    }
    names
}

fn render_table(rows: &[Row]) -> String {
    let mut lines = vec![
        "| ID | Spec | Threat | Control | Owner | Epic | Status | Verification |".to_owned(),
        "|---|---|---|---|---|---|---|---|".to_owned(),
    ];
    for row in rows {
        let verification = if row.verification.is_empty() {
            "\u{2014}".to_owned()
        } else {
            row.verification
                .iter()
                .map(|name| format!("`{name}`"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        lines.push(format!(
            "| {} | §{} | {} | {} | `{}` | {} | {} | {} |",
            row.id,
            row.spec,
            row.threat,
            row.control,
            row.owner,
            row.epic,
            row.status,
            verification
        ));
    }
    lines.join("\n")
}

fn generated_table(readme: &str) -> &str {
    let begin = readme
        .find(TABLE_BEGIN)
        .expect("README.md has a generated-matrix begin marker");
    let end = readme
        .find(TABLE_END)
        .expect("README.md has a generated-matrix end marker");
    assert!(begin < end, "generated-matrix markers are out of order");
    readme[begin + TABLE_BEGIN.len()..end].trim()
}

/// Relative link targets of a Markdown document, in document order.
fn relative_links(text: &str) -> Vec<String> {
    let bytes: Vec<char> = text.chars().collect();
    let mut links = Vec::new();
    let mut index = 0;
    while index + 1 < bytes.len() {
        if bytes[index] == ']' && bytes[index + 1] == '(' {
            let mut cursor = index + 2;
            let mut target = String::new();
            while cursor < bytes.len() && bytes[cursor] != ')' {
                target.push(bytes[cursor]);
                cursor += 1;
            }
            let trimmed = target.trim();
            let is_external = trimmed.starts_with("http://")
                || trimmed.starts_with("https://")
                || trimmed.starts_with('#')
                || trimmed.starts_with("mailto:");
            if !trimmed.is_empty() && !is_external {
                links.push(trimmed.split('#').next().unwrap_or_default().to_owned());
            }
            index = cursor;
        }
        index += 1;
    }
    links
}

#[test]
fn traceability_matrix_parses_and_is_well_formed() {
    let matrix = load_matrix();
    assert!(!matrix.rows.is_empty(), "matrix has no rows");

    let mut seen = BTreeSet::new();
    for (index, row) in matrix.rows.iter().enumerate() {
        let expected = format!("TM-{:03}", index + 1);
        assert_eq!(row.id, expected, "ids must be unique and sequential");
        assert!(seen.insert(row.id.clone()), "duplicate id {}", row.id);
        assert!(
            STATUSES.contains(&row.status.as_str()),
            "{}: bad status {}",
            row.id,
            row.status
        );
        assert!(
            KNOWN_EPICS.contains(&row.epic.as_str()),
            "{}: unknown epic {}",
            row.id,
            row.epic
        );
        for (label, value) in [
            ("spec", &row.spec),
            ("threat", &row.threat),
            ("control", &row.control),
            ("owner", &row.owner),
            ("notes", &row.notes),
        ] {
            assert!(!value.trim().is_empty(), "{}: `{label}` is empty", row.id);
            assert!(
                !value.contains('|'),
                "{}: `{label}` contains a pipe",
                row.id
            );
        }
    }
}

#[test]
fn every_spec_16_2_threat_has_exactly_one_row() {
    let matrix = load_matrix();
    assert_eq!(
        matrix.spec_threats.len(),
        matrix.spec_threats.iter().collect::<BTreeSet<_>>().len(),
        "spec_threats contains duplicates"
    );

    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for row in &matrix.rows {
        *counts.entry(row.threat.as_str()).or_default() += 1;
    }
    for (threat, count) in &counts {
        assert_eq!(*count, 1, "threat {threat:?} appears {count} times");
    }
    for threat in &matrix.spec_threats {
        assert_eq!(
            counts.get(threat.as_str()).copied().unwrap_or_default(),
            1,
            "SPEC §16.2 threat {threat:?} is not covered exactly once"
        );
    }
    for row in matrix.rows.iter().filter(|row| row.spec == "16.2") {
        assert!(
            matrix.spec_threats.contains(&row.threat),
            "{} claims §16.2 but its threat is not in spec_threats",
            row.id
        );
    }
}

#[test]
fn implemented_rows_cite_tests_that_exist() {
    let matrix = load_matrix();
    let known = test_functions();
    assert!(
        known.contains("local_only_denies_every_request_in_the_cross_product"),
        "test discovery is broken: it cannot see the egress gate's own tests"
    );

    for row in &matrix.rows {
        match row.status.as_str() {
            "implemented" => assert!(
                !row.verification.is_empty(),
                "{} is implemented but cites no verification test",
                row.id
            ),
            "planned" => assert!(
                row.verification.is_empty(),
                "{} is planned and must not cite verification tests",
                row.id
            ),
            _ => {}
        }
        for name in &row.verification {
            assert!(
                known.contains(name),
                "{}: verification test `{name}` does not exist",
                row.id
            );
        }
    }
}

#[test]
fn owner_paths_exist_for_rows_that_are_not_planned() {
    let matrix = load_matrix();
    let root = repo_root();
    for row in matrix.rows.iter().filter(|row| row.status != "planned") {
        assert!(
            root.join(&row.owner).exists(),
            "{}: owner path `{}` is missing",
            row.id,
            row.owner
        );
    }
}

#[test]
fn readme_states_every_not_guaranteed_string() {
    let readme = read(&doc_dir().join("README.md"));
    for statement in NOT_GUARANTEED {
        assert!(
            readme.contains(statement),
            "docs/threat-model/README.md does not reproduce: {statement}"
        );
    }
}

#[test]
fn readme_matrix_table_matches_the_json_matrix() {
    let matrix = load_matrix();
    let readme = read(&doc_dir().join("README.md"));
    assert_eq!(
        generated_table(&readme),
        render_table(&matrix.rows),
        "README.md matrix table is stale; run `python3 scripts/check-threat-model.py --update`"
    );
}

#[test]
fn threat_model_docs_make_no_prohibited_assurance_claim() {
    for path in checked_documents() {
        assert!(path.exists(), "missing document {}", path.display());
        let lowered = read(&path).to_lowercase();
        for phrase in PROHIBITED_PHRASES {
            assert!(
                !lowered.contains(phrase),
                "{} uses prohibited assurance language: {phrase:?}",
                path.display()
            );
        }
    }
}

#[test]
fn threat_model_relative_links_resolve() {
    for path in checked_documents() {
        let parent = path
            .parent()
            .expect("document has a parent directory")
            .to_path_buf();
        for target in relative_links(&read(&path)) {
            assert!(
                parent.join(&target).exists(),
                "{}: broken relative link `{target}`",
                path.display()
            );
        }
    }
}

#[test]
fn open_findings_restates_every_unresolved_row() {
    let matrix = load_matrix();
    let findings = read(&doc_dir().join("open-findings.md"));
    for row in &matrix.rows {
        let mentioned = findings.contains(&row.id);
        match row.status.as_str() {
            "implemented" => {
                assert!(
                    !mentioned,
                    "{} is implemented but appears in open-findings.md",
                    row.id
                );
            }
            _ => assert!(
                mentioned,
                "{} is {} but is not restated in open-findings.md",
                row.id, row.status
            ),
        }
    }
}

#[test]
fn python_checker_and_rust_test_share_the_same_rules() {
    let script = read(&repo_root().join("scripts").join("check-threat-model.py"));

    let phrases = python_list(&script, "PROHIBITED_PHRASES");
    assert_eq!(
        phrases,
        PROHIBITED_PHRASES.to_vec(),
        "prohibited-phrase lists have drifted apart"
    );

    let keys = python_list(&script, "REQUIRED_KEYS");
    assert_eq!(
        keys,
        REQUIRED_ROW_KEYS.to_vec(),
        "required-key lists have drifted apart"
    );

    let statuses = python_list(&script, "STATUSES");
    assert_eq!(
        statuses,
        STATUSES.to_vec(),
        "status vocabularies have drifted apart"
    );
}

/// Extract the string literals of a simple `NAME = [...]` Python list.
fn python_list(script: &str, name: &str) -> Vec<String> {
    let start = script
        .find(&format!("{name} = "))
        .unwrap_or_else(|| panic!("{name} not found in check-threat-model.py"));
    let open = script[start..].find('[').expect("list literal") + start;
    let close = script[open..].find(']').expect("list literal end") + open;
    let body = &script[open + 1..close];

    let mut values = Vec::new();
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '"' {
            continue;
        }
        let mut value = String::new();
        for c in chars.by_ref() {
            if c == '"' {
                break;
            }
            value.push(c);
        }
        values.push(value);
    }
    values
}
