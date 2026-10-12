//! Reusable secret scanner (spec §18.5 "scan all logs for keys and signed
//! URLs", §1.5 "Secrets in application logs: 0 in automated scanning").
//!
//! The scanner walks files and SQLite databases and applies **the same**
//! [`crate::patterns`] set the log sink uses, through the same
//! [`Redactor`]. There is deliberately no second copy of the pattern table: a
//! shape the sink scrubs is a shape the scanner catches, always.
//!
//! Findings never contain the matched bytes — only `path:line:pattern`.

use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::redact::Redactor;

/// File extensions opened as SQLite databases rather than text.
pub const SQLITE_EXTENSIONS: &[&str] = &["sqlite", "sqlite3", "db", "db3"];

/// Largest file the scanner reads. Larger files are reported as skipped rather
/// than silently ignored, so a 2 GiB log cannot hide a key.
pub const DEFAULT_MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// One documented false positive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AllowlistEntry {
    /// Glob matched against the reported path (`*` does not cross `/`, `**`
    /// does).
    pub path: String,
    /// Pattern name, e.g. `device_token_hex`. Must be a known pattern.
    pub pattern: String,
    /// Why this is not a secret. Required: an undocumented allowlist entry is
    /// how a scanner quietly stops working.
    pub reason: String,
}

/// On-disk allowlist file format.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Allowlist {
    #[serde(default)]
    pub entries: Vec<AllowlistEntry>,
}

impl Allowlist {
    /// Load and validate an allowlist file.
    pub fn load(path: &Path) -> Result<Self, ScanError> {
        let text = fs::read_to_string(path).map_err(|source| ScanError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let list: Self = serde_json::from_str(&text).map_err(|source| ScanError::Allowlist {
            path: path.to_path_buf(),
            source,
        })?;
        let known = crate::patterns::pattern_names();
        for entry in &list.entries {
            if !known.contains(&entry.pattern.as_str()) {
                return Err(ScanError::UnknownPattern(entry.pattern.clone()));
            }
            if entry.reason.trim().is_empty() {
                return Err(ScanError::UndocumentedAllowlistEntry(entry.path.clone()));
            }
        }
        Ok(list)
    }
}

/// Where a finding was located.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Location {
    /// Line in a text file (1-based).
    Line { line: u64 },
    /// Row of a SQLite column (1-based row ordinal of the table scan).
    SqliteCell {
        table: String,
        column: String,
        row: u64,
    },
}

impl Location {
    fn line_field(&self) -> String {
        match self {
            Self::Line { line } => line.to_string(),
            Self::SqliteCell { table, column, row } => format!("{table}.{column}:{row}"),
        }
    }
}

/// One hit. Never contains the secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanFinding {
    pub path: String,
    pub pattern: String,
    pub description: String,
    pub location: Location,
}

impl fmt::Display for ScanFinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}:{}",
            self.path,
            self.location.line_field(),
            self.pattern
        )
    }
}

/// Result of a scan.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanReport {
    pub findings: Vec<ScanFinding>,
    /// Allowlist entries that matched nothing. Reported so the list cannot rot.
    pub unused_allowlist: Vec<AllowlistEntry>,
    /// Files that exceeded the size cap and were not read.
    pub skipped_oversize: Vec<String>,
    pub files_scanned: usize,
}

impl ScanReport {
    /// A scan "fails" on any finding *or* any stale allowlist entry.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.findings.is_empty() && self.unused_allowlist.is_empty()
    }
}

/// Scanner errors.
#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error("{path} could not be read: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("allowlist {path} is not valid JSON: {source}")]
    Allowlist {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("allowlist references unknown pattern `{0}`")]
    UnknownPattern(String),
    #[error("allowlist entry for `{0}` has no documented reason")]
    UndocumentedAllowlistEntry(String),
    #[error("sqlite database {path} could not be inspected: {source}")]
    Sqlite {
        path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },
}

/// Scanner configuration.
#[derive(Debug, Clone)]
pub struct ScanOptions {
    pub allowlist: Allowlist,
    pub max_file_bytes: u64,
    /// Path globs skipped entirely. Unlike an allowlist entry this is not
    /// "a documented false positive" but "not a scan target at all": the
    /// adversarial fuzz corpora under `tests/fixtures/fuzz-corpus` are
    /// *inputs* that contain synthetic secrets on purpose.
    pub exclude: Vec<String>,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            allowlist: Allowlist::default(),
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
            exclude: Vec::new(),
        }
    }
}

/// Walks paths and reports secret-shaped content.
#[derive(Debug)]
pub struct Scanner {
    redactor: Redactor,
    options: ScanOptions,
}

impl Scanner {
    #[must_use]
    pub fn new(options: ScanOptions) -> Self {
        Self {
            redactor: Redactor::new(),
            options,
        }
    }

    /// Scan every path (files are scanned directly, directories recursively).
    pub fn scan(&self, roots: &[PathBuf]) -> Result<ScanReport, ScanError> {
        let mut report = ScanReport::default();
        let mut used: BTreeSet<usize> = BTreeSet::new();
        for root in roots {
            for entry in WalkDir::new(root)
                .follow_links(false)
                .into_iter()
                .filter_map(Result::ok)
            {
                if !entry.file_type().is_file() || self.is_excluded(entry.path()) {
                    continue;
                }
                self.scan_file(entry.path(), &mut report, &mut used)?;
            }
        }
        report.unused_allowlist = self
            .options
            .allowlist
            .entries
            .iter()
            .enumerate()
            .filter(|(index, _)| !used.contains(index))
            .map(|(_, entry)| entry.clone())
            .collect();
        Ok(report)
    }

    fn is_excluded(&self, path: &Path) -> bool {
        let display = display_path(path);
        self.options
            .exclude
            .iter()
            .any(|glob| glob_match(glob, &display))
    }

    fn scan_file(
        &self,
        path: &Path,
        report: &mut ScanReport,
        used: &mut BTreeSet<usize>,
    ) -> Result<(), ScanError> {
        let metadata = fs::metadata(path).map_err(|source| ScanError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        if metadata.len() > self.options.max_file_bytes {
            report.skipped_oversize.push(display_path(path));
            return Ok(());
        }
        report.files_scanned += 1;
        let is_sqlite = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| SQLITE_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()));
        if is_sqlite {
            self.scan_sqlite(path, report, used)
        } else {
            self.scan_text(path, report, used)
        }
    }

    fn scan_text(
        &self,
        path: &Path,
        report: &mut ScanReport,
        used: &mut BTreeSet<usize>,
    ) -> Result<(), ScanError> {
        let bytes = fs::read(path).map_err(|source| ScanError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        // Crash dumps and snapshots may be binary; lossy decoding still exposes
        // ASCII credentials, which is what every shape in the table is.
        let text = String::from_utf8_lossy(&bytes);
        for (index, line) in text.lines().enumerate() {
            let line_no = index as u64 + 1;
            self.record(path, line, &Location::Line { line: line_no }, report, used);
        }
        Ok(())
    }

    fn scan_sqlite(
        &self,
        path: &Path,
        report: &mut ScanReport,
        used: &mut BTreeSet<usize>,
    ) -> Result<(), ScanError> {
        let sqlite = |source: rusqlite::Error| ScanError::Sqlite {
            path: path.to_path_buf(),
            source,
        };
        let conn = rusqlite::Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
        )
        .map_err(sqlite)?;
        let tables: Vec<String> = {
            let mut stmt = conn
                .prepare("SELECT name FROM sqlite_master WHERE type IN ('table','view')")
                .map_err(sqlite)?;
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(sqlite)?;
            rows.filter_map(Result::ok)
                .filter(|name| !name.starts_with("sqlite_"))
                .collect()
        };
        for table in tables {
            // A view can reference a missing table; that is not a secret
            // problem, and failing the scan on it would be noise.
            let query = format!("SELECT * FROM \"{}\"", table.replace('"', "\"\""));
            let Ok(mut stmt) = conn.prepare(&query) else {
                continue;
            };
            let columns: Vec<String> = stmt
                .column_names()
                .into_iter()
                .map(ToOwned::to_owned)
                .collect();
            let mut rows = stmt.query([]).map_err(sqlite)?;
            let mut row_no = 0u64;
            while let Some(row) = rows.next().map_err(sqlite)? {
                row_no += 1;
                for (index, column) in columns.iter().enumerate() {
                    let Ok(value) = row.get_ref(index) else {
                        continue;
                    };
                    let rendered = match value {
                        rusqlite::types::ValueRef::Null => continue,
                        rusqlite::types::ValueRef::Integer(i) => i.to_string(),
                        rusqlite::types::ValueRef::Real(f) => f.to_string(),
                        rusqlite::types::ValueRef::Text(t) => {
                            String::from_utf8_lossy(t).into_owned()
                        }
                        rusqlite::types::ValueRef::Blob(b) => {
                            String::from_utf8_lossy(b).into_owned()
                        }
                    };
                    self.record(
                        path,
                        &rendered,
                        &Location::SqliteCell {
                            table: table.clone(),
                            column: column.clone(),
                            row: row_no,
                        },
                        report,
                        used,
                    );
                }
            }
        }
        Ok(())
    }

    fn record(
        &self,
        path: &Path,
        text: &str,
        location: &Location,
        report: &mut ScanReport,
        used: &mut BTreeSet<usize>,
    ) {
        let display = display_path(path);
        let mut seen: BTreeSet<&'static str> = BTreeSet::new();
        for finding in self.redactor.matches(text) {
            if !seen.insert(finding.pattern) {
                continue;
            }
            if let Some(index) = self.allowlisted(&display, finding.pattern) {
                used.insert(index);
                continue;
            }
            let description = crate::patterns::pattern_set()
                .iter()
                .find(|p| p.names().any(|n| n == finding.pattern))
                .map_or(
                    "registered live secret",
                    crate::patterns::Pattern::description,
                );
            report.findings.push(ScanFinding {
                path: display.clone(),
                pattern: finding.pattern.to_owned(),
                description: description.to_owned(),
                location: location.clone(),
            });
        }
    }

    fn allowlisted(&self, path: &str, pattern: &str) -> Option<usize> {
        self.options.allowlist.entries.iter().position(|entry| {
            entry.pattern == pattern
                && (glob_match(&entry.path, path) || glob_match(&entry.path, &normalize(path)))
        })
    }
}

fn display_path(path: &Path) -> String {
    normalize(&path.display().to_string())
}

fn normalize(path: &str) -> String {
    let trimmed = path.strip_prefix("./").unwrap_or(path);
    trimmed.replace('\\', "/")
}

/// Minimal glob matcher: `?` matches one non-separator character, `*` matches
/// any run of non-separator characters, `**` matches across separators.
#[must_use]
pub fn glob_match(pattern: &str, text: &str) -> bool {
    glob_inner(pattern.as_bytes(), text.as_bytes())
}

fn glob_inner(pattern: &[u8], text: &[u8]) -> bool {
    if pattern.is_empty() {
        return text.is_empty();
    }
    match pattern[0] {
        b'*' => {
            if pattern.len() > 1 && pattern[1] == b'*' {
                let rest = skip_double_star(&pattern[2..]);
                for split in 0..=text.len() {
                    if glob_inner(rest, &text[split..]) {
                        return true;
                    }
                }
                false
            } else {
                let rest = &pattern[1..];
                let mut split = 0;
                loop {
                    if glob_inner(rest, &text[split..]) {
                        return true;
                    }
                    if split >= text.len() || text[split] == b'/' {
                        return false;
                    }
                    split += 1;
                }
            }
        }
        b'?' => !text.is_empty() && text[0] != b'/' && glob_inner(&pattern[1..], &text[1..]),
        other => !text.is_empty() && text[0] == other && glob_inner(&pattern[1..], &text[1..]),
    }
}

/// `a/**/b` should also match `a/b`, so a `/` directly after `**` is optional.
fn skip_double_star(rest: &[u8]) -> &[u8] {
    if rest.first() == Some(&b'/') {
        &rest[1..]
    } else {
        rest
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_star_does_not_cross_separators() {
        assert!(glob_match("tests/*.log", "tests/a.log"));
        assert!(!glob_match("tests/*.log", "tests/sub/a.log"));
        assert!(glob_match("tests/**/*.log", "tests/sub/deep/a.log"));
        assert!(glob_match("tests/**", "tests/sub/a.log"));
        assert!(glob_match("**/fixtures/*.json", "crates/x/fixtures/a.json"));
        assert!(glob_match("a/**/b", "a/b"));
        assert!(!glob_match("a/b", "a/c"));
        assert!(glob_match("a?c", "abc"));
        assert!(!glob_match("a?c", "a/c"));
    }

    #[test]
    fn location_renders_as_line_field() {
        assert_eq!(Location::Line { line: 7 }.line_field(), "7");
        assert_eq!(
            Location::SqliteCell {
                table: "t".into(),
                column: "c".into(),
                row: 3
            }
            .line_field(),
            "t.c:3"
        );
    }
}
