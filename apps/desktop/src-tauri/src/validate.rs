//! Pure argument validation for the command boundary (spec §15.1).
//!
//! Every function here is total: it returns `Result<_, AppError>` and never
//! panics, never touches global state and never runs a shell. Hostile input —
//! shell metacharacters, `;rm -rf /`, unicode direction overrides, 300-character
//! names, Windows reserved device names — is treated as **data**: a file name is
//! only ever stored as metadata, and the archived name is derived from the
//! content hash, so no validated value is ever interpolated into a command line.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use plaudon_domain::error::{AppError, AppErrorCode};
use plaudon_domain::ids::{IdParseError, RecordingId, TranscriptRevisionId};
use plaudon_domain::time::Timestamp;

use crate::dto::{
    ExportFormat, ImportStatus, JobKind, JobState, RecordingSort, SourceKind, TranscriptionStatus,
};
use crate::error::invalid_argument;

/// Smallest accepted page size.
pub const MIN_LIMIT: u32 = 1;
/// Largest accepted page size.
pub const MAX_LIMIT: u32 = 1000;
/// Largest accepted number of paths in one import request.
pub const MAX_IMPORT_PATHS: usize = 512;
/// Largest accepted number of segment edits in one save.
pub const MAX_EDITS: usize = 100_000;

/// Media container extensions the importer accepts (spec §7.4, §13.4).
pub const ALLOWED_MEDIA_EXTENSIONS: &[&str] = &["wav", "mp3", "m4a", "flac", "ogg", "opus"];

/* ------------------------------------------------------------------- ids */

/// Parse a typed identifier out of a frontend-supplied string.
pub fn parse_id<T>(field: &'static str, value: &str) -> Result<T, AppError>
where
    T: FromStr<Err = IdParseError>,
{
    value.parse::<T>().map_err(|_| invalid_argument(format!("{field} is not a canonical UUID")))
}

/* ----------------------------------------------------------------- paging */

/// Validate a requested page size.
pub fn limit(value: u32) -> Result<u32, AppError> {
    if (MIN_LIMIT..=MAX_LIMIT).contains(&value) {
        Ok(value)
    } else {
        Err(invalid_argument(format!("limit {value} is outside {MIN_LIMIT}..={MAX_LIMIT}")))
    }
}

/// An opaque pagination cursor.
///
/// The encoding is deliberately not a number the frontend can synthesize
/// arithmetic on: it is the hex of a tagged offset, and anything else is
/// rejected. Pagination is an offset snapshot over the query's ordering, which
/// is why `total` is documented as a snapshot too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageCursor {
    offset: u64,
}

impl PageCursor {
    /// The first page.
    pub const START: Self = Self { offset: 0 };

    /// Cursor addressing `offset` rows into the result set.
    #[must_use]
    pub const fn at(offset: u64) -> Self {
        Self { offset }
    }

    /// Row offset addressed by this cursor.
    #[must_use]
    pub const fn offset(self) -> u64 {
        self.offset
    }

    /// Encode to the opaque wire form.
    #[must_use]
    pub fn encode(self) -> String {
        hex::encode(format!("off:{}", self.offset))
    }

    /// Decode an opaque cursor, rejecting anything this build did not produce.
    pub fn decode(raw: &str) -> Result<Self, AppError> {
        let bytes = hex::decode(raw).map_err(|_| invalid_argument("cursor is not valid hex"))?;
        let text =
            String::from_utf8(bytes).map_err(|_| invalid_argument("cursor is not valid utf-8"))?;
        let digits = text
            .strip_prefix("off:")
            .ok_or_else(|| invalid_argument("cursor has an unknown prefix"))?;
        let offset =
            digits.parse::<u64>().map_err(|_| invalid_argument("cursor offset is not a number"))?;
        Ok(Self { offset })
    }
}

/// Decode an optional cursor, defaulting to the first page.
pub fn cursor(raw: Option<&str>) -> Result<PageCursor, AppError> {
    match raw {
        None => Ok(PageCursor::START),
        Some(value) => PageCursor::decode(value),
    }
}

/* ------------------------------------------------------------------ dates */

/// Parse an inclusive RFC 3339 bound.
pub fn parse_rfc3339(field: &'static str, value: &str) -> Result<Timestamp, AppError> {
    let parsed =
        time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
            .map_err(|_| invalid_argument(format!("{field} is not an RFC 3339 timestamp")))?;
    let millis = parsed.unix_timestamp_nanos() / 1_000_000;
    let millis = i64::try_from(millis)
        .map_err(|_| invalid_argument(format!("{field} is outside the representable range")))?;
    Ok(Timestamp::from_millis(millis))
}

/// Validate an inclusive date range.
pub fn date_bounds(
    from: Option<&str>,
    to: Option<&str>,
) -> Result<(Option<Timestamp>, Option<Timestamp>), AppError> {
    let from = from.map(|value| parse_rfc3339("date_from", value)).transpose()?;
    let to = to.map(|value| parse_rfc3339("date_to", value)).transpose()?;
    if let (Some(start), Some(end)) = (from, to) {
        if start > end {
            return Err(invalid_argument("date_from is after date_to"));
        }
    }
    Ok((from, to))
}

/* ------------------------------------------------------------- filter sets */

/// Parse the `sources` filter, which arrives as free strings.
pub fn source_kinds(values: &[String]) -> Result<Vec<SourceKind>, AppError> {
    values
        .iter()
        .map(|value| match value.as_str() {
            "manual_import" => Ok(SourceKind::ManualImport),
            "plaud_cloud" => Ok(SourceKind::PlaudCloud),
            "plaud_device" => Ok(SourceKind::PlaudDevice),
            "watched_folder" => Ok(SourceKind::WatchedFolder),
            other => Err(invalid_argument(format!("unknown source filter {other:?}"))),
        })
        .collect()
}

/// Reject duplicate entries in an enum filter set so a query cannot be made
/// quadratic from the frontend.
pub fn unique_states(states: &[JobState]) -> Result<Vec<JobState>, AppError> {
    let mut seen: Vec<JobState> = Vec::with_capacity(states.len());
    for state in states {
        if seen.contains(state) {
            return Err(invalid_argument(format!("duplicate job state {}", state.as_str())));
        }
        seen.push(*state);
    }
    Ok(seen)
}

/// Reject duplicate job kinds for the same reason.
pub fn unique_kinds(kinds: &[JobKind]) -> Result<Vec<JobKind>, AppError> {
    let mut seen: Vec<JobKind> = Vec::with_capacity(kinds.len());
    for kind in kinds {
        if seen.contains(kind) {
            return Err(invalid_argument(format!("duplicate job kind {}", kind.as_str())));
        }
        seen.push(*kind);
    }
    Ok(seen)
}

/* ------------------------------------------------------------ import paths */

/// A path that passed every import rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedImportPath {
    /// The path exactly as the frontend supplied it, kept for the response.
    pub requested: String,
    /// The fully resolved path the importer will read.
    pub resolved: PathBuf,
    /// Original file name, stored as metadata only; never used to build a path.
    pub original_file_name: String,
    /// Lowercase extension from the allowlist.
    pub extension: String,
    /// Length of the file as observed at validation time.
    pub byte_length: u64,
}

/// Validate one user-approved import path (spec §7.4, §13.4).
///
/// The path is canonicalized first, which resolves symlinks and Windows
/// junctions, and the **resolved** target must stay inside one of the approved
/// roots. Only regular files are accepted: directories, FIFOs, sockets and
/// device nodes are rejected before anything is opened.
pub fn validate_import_path(
    requested: &str,
    approved_roots: &[PathBuf],
) -> Result<ValidatedImportPath, AppError> {
    if requested.is_empty() {
        return Err(invalid_argument("import path is empty"));
    }
    if requested.contains('\0') {
        return Err(invalid_argument("import path contains a NUL byte"));
    }
    let candidate = Path::new(requested);
    let resolved = std::fs::canonicalize(candidate).map_err(|err| {
        AppError::new(
            AppErrorCode::ArchivePathRejected,
            format!("cannot resolve import path ({}): {err}", candidate.display()),
        )
    })?;

    if !is_inside_approved_root(&resolved, approved_roots)? {
        return Err(AppError::new(
            AppErrorCode::ArchivePathRejected,
            format!("resolved target {} is outside every approved root", resolved.display()),
        ));
    }

    let metadata = std::fs::metadata(&resolved).map_err(|err| {
        AppError::new(
            AppErrorCode::ArchivePathRejected,
            format!("cannot stat import path ({}): {err}", resolved.display()),
        )
    })?;
    if !metadata.is_file() || !is_regular_file(&metadata) {
        return Err(AppError::new(
            AppErrorCode::ArchivePathRejected,
            format!("{} is not a regular file", resolved.display()),
        ));
    }

    let extension = resolved
        .extension()
        .and_then(OsStr::to_str)
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if !ALLOWED_MEDIA_EXTENSIONS.contains(&extension.as_str()) {
        return Err(AppError::new(
            AppErrorCode::UnsupportedMedia,
            format!("extension {extension:?} is not in the import allowlist"),
        ));
    }

    // The original name is metadata. It is never parsed, never joined onto the
    // archive path and never passed to a process, so hostile names are data.
    let original_file_name = candidate
        .file_name()
        .or_else(|| resolved.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();

    Ok(ValidatedImportPath {
        requested: requested.to_owned(),
        resolved,
        original_file_name,
        extension,
        byte_length: metadata.len(),
    })
}

fn is_inside_approved_root(resolved: &Path, approved_roots: &[PathBuf]) -> Result<bool, AppError> {
    if approved_roots.is_empty() {
        return Err(AppError::new(
            AppErrorCode::ArchivePathRejected,
            "no approved import root is configured",
        ));
    }
    for root in approved_roots {
        let canonical_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.clone());
        if resolved.starts_with(&canonical_root) {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(unix)]
fn is_regular_file(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::FileTypeExt;
    let file_type = metadata.file_type();
    !file_type.is_fifo()
        && !file_type.is_socket()
        && !file_type.is_block_device()
        && !file_type.is_char_device()
        && file_type.is_file()
}

#[cfg(not(unix))]
fn is_regular_file(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_file()
}

/* --------------------------------------------------------- export destination */

/// A destination that passed every export rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedDestination {
    /// Absolute path the export job will write.
    pub path: PathBuf,
}

/// Validate a destination chosen through the OS save dialog.
///
/// The parent directory must already exist (we never create a tree the user did
/// not choose) and the destination itself must not be an existing directory or
/// special file. We only ever write the exact path the caller passed.
pub fn validate_destination(destination: &str) -> Result<ValidatedDestination, AppError> {
    if destination.is_empty() {
        return Err(invalid_argument("destination is empty"));
    }
    if destination.contains('\0') {
        return Err(invalid_argument("destination contains a NUL byte"));
    }
    let path = Path::new(destination);
    if !path.is_absolute() {
        return Err(AppError::new(
            AppErrorCode::ArchivePathRejected,
            "destination must be an absolute path chosen by the user",
        ));
    }
    let file_name = path
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| {
            AppError::new(AppErrorCode::ArchivePathRejected, "destination has no file name")
        })?
        .to_owned();
    if file_name == "." || file_name == ".." {
        return Err(AppError::new(
            AppErrorCode::ArchivePathRejected,
            "destination file name is a directory reference",
        ));
    }
    let parent = path.parent().ok_or_else(|| {
        AppError::new(AppErrorCode::ArchivePathRejected, "destination has no parent directory")
    })?;
    let parent = std::fs::canonicalize(parent).map_err(|err| {
        AppError::new(
            AppErrorCode::ArchivePathRejected,
            format!("destination directory does not exist: {err}"),
        )
    })?;
    if !parent.is_dir() {
        return Err(AppError::new(
            AppErrorCode::ArchivePathRejected,
            "destination parent is not a directory",
        ));
    }
    let resolved = parent.join(file_name);
    if let Ok(metadata) = std::fs::metadata(&resolved) {
        if !is_regular_file(&metadata) {
            return Err(AppError::new(
                AppErrorCode::ArchivePathRejected,
                "destination exists and is not a regular file",
            ));
        }
    }
    Ok(ValidatedDestination { path: resolved })
}

/* ------------------------------------------------------- validated requests */

/// Validated `list_recordings` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedListRecordings {
    /// Row offset of the requested page.
    pub cursor: PageCursor,
    /// Page size.
    pub limit: u32,
    /// Filter.
    pub filter: ValidatedRecordingFilter,
    /// Sort.
    pub sort: RecordingSort,
}

/// Validated library filter.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ValidatedRecordingFilter {
    /// Free-text query over the title.
    pub query: Option<String>,
    /// Import statuses to include; empty means all.
    pub import_status: Vec<ImportStatus>,
    /// Transcription statuses to include; empty means all.
    pub transcription_status: Vec<TranscriptionStatus>,
    /// Source kinds to include; empty means all.
    pub sources: Vec<SourceKind>,
    /// Tags that must all be present; empty means all.
    pub tags: Vec<String>,
    /// Inclusive lower bound.
    pub date_from: Option<Timestamp>,
    /// Inclusive upper bound.
    pub date_to: Option<Timestamp>,
}

/// Validated `get_transcript` arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValidatedGetTranscript {
    /// Revision to read.
    pub revision_id: TranscriptRevisionId,
    /// First segment index of the page.
    pub cursor: u32,
    /// Page size.
    pub limit: u32,
}

/// Validated `save_transcript_revision` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedSaveRevision {
    /// Recording the revision belongs to.
    pub recording_id: RecordingId,
    /// Revision the edits were made against.
    pub base_revision_id: TranscriptRevisionId,
    /// Edits, keyed by segment index.
    pub edits: Vec<(u32, String)>,
}

/// Validated `list_jobs` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedListJobs {
    /// Row offset of the requested page.
    pub cursor: PageCursor,
    /// Page size.
    pub limit: u32,
    /// States to include; empty means all.
    pub states: Vec<JobState>,
    /// Kinds to include; empty means all.
    pub kinds: Vec<JobKind>,
}

/// Validated `list_activity` arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValidatedListActivity {
    /// Row offset of the requested page.
    pub cursor: PageCursor,
    /// Page size.
    pub limit: u32,
}

/// Validated `export_recording` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedExport {
    /// Recording to export.
    pub recording_id: RecordingId,
    /// Revision to render.
    pub revision_id: TranscriptRevisionId,
    /// Output format.
    pub format: ExportFormat,
    /// Destination file.
    pub destination: ValidatedDestination,
}

/// Validate the edit list of a save request.
pub fn segment_edits(edits: &[crate::dto::SegmentEdit]) -> Result<Vec<(u32, String)>, AppError> {
    if edits.is_empty() {
        return Err(invalid_argument("edits must contain at least one segment"));
    }
    if edits.len() > MAX_EDITS {
        return Err(invalid_argument("edits exceed the maximum accepted in one save"));
    }
    let mut seen: Vec<u32> = Vec::with_capacity(edits.len());
    let mut out = Vec::with_capacity(edits.len());
    for edit in edits {
        if seen.contains(&edit.index) {
            return Err(invalid_argument(format!("duplicate edit for segment {}", edit.index)));
        }
        if edit.text.contains('\0') {
            return Err(invalid_argument("segment text contains a NUL byte"));
        }
        seen.push(edit.index);
        out.push((edit.index, edit.text.clone()));
    }
    Ok(out)
}

/// Validate the path list of an import request.
pub fn import_paths(paths: &[String]) -> Result<Vec<String>, AppError> {
    if paths.is_empty() {
        return Err(invalid_argument("import request contains no paths"));
    }
    if paths.len() > MAX_IMPORT_PATHS {
        return Err(invalid_argument(format!(
            "import request exceeds {MAX_IMPORT_PATHS} paths"
        )));
    }
    Ok(paths.to_vec())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn roots(dir: &Path) -> Vec<PathBuf> {
        vec![dir.to_path_buf()]
    }

    #[test]
    fn limits_are_bounded() {
        assert!(limit(0).is_err());
        assert_eq!(limit(1).unwrap(), 1);
        assert_eq!(limit(1000).unwrap(), 1000);
        assert!(limit(1001).is_err());
        assert_eq!(
            limit(0).unwrap_err().code(),
            plaudon_domain::error::AppErrorCode::InvalidArgument
        );
    }

    #[test]
    fn cursors_round_trip_and_reject_foreign_values() {
        let encoded = PageCursor::at(42).encode();
        assert_ne!(encoded, "42");
        assert_eq!(PageCursor::decode(&encoded).unwrap().offset(), 42);
        assert!(PageCursor::decode("42").is_err());
        assert!(PageCursor::decode(&hex::encode("offset:1")).is_err());
        assert!(PageCursor::decode(&hex::encode("off:-1")).is_err());
        assert_eq!(cursor(None).unwrap(), PageCursor::START);
    }

    #[test]
    fn id_parsing_reports_invalid_argument() {
        let err = parse_id::<RecordingId>("recording_id", "nope").unwrap_err();
        assert_eq!(err.code(), plaudon_domain::error::AppErrorCode::InvalidArgument);
        let id = RecordingId::new();
        assert_eq!(parse_id::<RecordingId>("recording_id", &id.to_string()).unwrap(), id);
    }

    #[test]
    fn date_bounds_reject_an_inverted_range_and_bad_syntax() {
        assert!(date_bounds(Some("2026-01-02T00:00:00Z"), Some("2026-01-01T00:00:00Z")).is_err());
        assert!(date_bounds(Some("yesterday"), None).is_err());
        let (from, to) = date_bounds(Some("1970-01-01T00:00:01Z"), None).unwrap();
        assert_eq!(from, Some(Timestamp::from_millis(1000)));
        assert_eq!(to, None);
    }

    #[test]
    fn source_filters_reject_unknown_values() {
        assert_eq!(
            source_kinds(&["manual_import".to_owned()]).unwrap(),
            vec![SourceKind::ManualImport]
        );
        assert!(source_kinds(&["ftp".to_owned()]).is_err());
    }

    #[test]
    fn hostile_file_names_are_accepted_as_data() {
        let dir = tempfile::tempdir().unwrap();
        let three_hundred = format!("{}.wav", "a".repeat(296));
        let hostile = [
            "; rm -rf / .wav",
            "$(whoami).wav",
            "`id`.wav",
            "a|b&c;d.wav",
            "weird\u{202e}gnp.wav",
            "CON.wav",
            "NUL.wav",
            "--help.wav",
            "..wav.wav",
            "quote'and\"double.wav",
            "new\tline.wav",
            three_hundred.as_str(),
        ];
        for name in hostile {
            let path = dir.path().join(name);
            fs::write(&path, b"RIFF0000WAVE").unwrap();
            let validated =
                validate_import_path(path.to_str().unwrap(), &roots(dir.path())).unwrap_or_else(
                    |err| panic!("hostile name {name:?} must be accepted as data: {err:?}"),
                );
            assert_eq!(validated.original_file_name, name);
            assert_eq!(validated.extension, "wav");
            assert_eq!(validated.byte_length, 12);
        }
    }

    #[test]
    fn rejected_paths_are_table_driven() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();

        let directory = dir.path().join("a_directory.wav");
        fs::create_dir(&directory).unwrap();

        let wrong_ext = dir.path().join("notes.txt");
        fs::write(&wrong_ext, b"text").unwrap();

        let no_ext = dir.path().join("plainfile");
        fs::write(&no_ext, b"text").unwrap();

        let escaping_target = outside.path().join("secret.wav");
        fs::write(&escaping_target, b"RIFF").unwrap();

        let missing = dir.path().join("does-not-exist.wav");

        let cases: Vec<(&str, String, AppErrorCode)> = vec![
            (
                "directory",
                directory.to_string_lossy().into_owned(),
                AppErrorCode::ArchivePathRejected,
            ),
            (
                "extension not allowlisted",
                wrong_ext.to_string_lossy().into_owned(),
                AppErrorCode::UnsupportedMedia,
            ),
            (
                "no extension",
                no_ext.to_string_lossy().into_owned(),
                AppErrorCode::UnsupportedMedia,
            ),
            (
                "missing file",
                missing.to_string_lossy().into_owned(),
                AppErrorCode::ArchivePathRejected,
            ),
            (
                "outside the approved root",
                escaping_target.to_string_lossy().into_owned(),
                AppErrorCode::ArchivePathRejected,
            ),
            ("empty", String::new(), AppErrorCode::InvalidArgument),
            ("nul byte", "/tmp/a\0b.wav".to_owned(), AppErrorCode::InvalidArgument),
            (
                "traversal out of the root",
                dir.path().join("../../etc/passwd").to_string_lossy().into_owned(),
                AppErrorCode::ArchivePathRejected,
            ),
        ];

        for (label, path, expected) in cases {
            let err = validate_import_path(&path, &roots(dir.path()))
                .expect_err(&format!("{label} must be rejected"));
            assert_eq!(err.code(), expected, "{label}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_that_escapes_the_root_is_rejected_and_one_that_stays_is_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();

        let inside_target = dir.path().join("real.wav");
        fs::write(&inside_target, b"RIFF").unwrap();
        let inside_link = dir.path().join("inside-link.wav");
        std::os::unix::fs::symlink(&inside_target, &inside_link).unwrap();

        let outside_target = outside.path().join("escaped.wav");
        fs::write(&outside_target, b"RIFF").unwrap();
        let escaping_link = dir.path().join("escape-link.wav");
        std::os::unix::fs::symlink(&outside_target, &escaping_link).unwrap();

        let accepted =
            validate_import_path(inside_link.to_str().unwrap(), &roots(dir.path())).unwrap();
        assert_eq!(accepted.resolved, fs::canonicalize(&inside_target).unwrap());
        assert_eq!(accepted.original_file_name, "inside-link.wav");

        let err = validate_import_path(escaping_link.to_str().unwrap(), &roots(dir.path()))
            .unwrap_err();
        assert_eq!(err.code(), AppErrorCode::ArchivePathRejected);
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_is_rejected_even_with_an_allowlisted_extension() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("pipe.wav");
        let status = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("mkfifo is available on unix hosts");
        assert!(status.success());
        let err = validate_import_path(fifo.to_str().unwrap(), &roots(dir.path())).unwrap_err();
        assert_eq!(err.code(), AppErrorCode::ArchivePathRejected);
    }

    #[test]
    fn an_empty_approved_root_set_rejects_everything() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.wav");
        fs::write(&path, b"RIFF").unwrap();
        let err = validate_import_path(path.to_str().unwrap(), &[]).unwrap_err();
        assert_eq!(err.code(), AppErrorCode::ArchivePathRejected);
    }

    #[test]
    fn destinations_require_an_existing_parent_and_a_real_file_name() {
        let dir = tempfile::tempdir().unwrap();
        let ok = dir.path().join("export.txt");
        assert_eq!(validate_destination(ok.to_str().unwrap()).unwrap().path, ok);

        assert!(validate_destination("relative.txt").is_err());
        assert!(validate_destination("").is_err());
        assert!(validate_destination(
            dir.path().join("missing-dir").join("x.txt").to_str().unwrap()
        )
        .is_err());
        assert!(validate_destination(dir.path().to_str().unwrap()).is_err());
    }

    #[test]
    fn edits_reject_duplicates_and_empty_lists() {
        use crate::dto::SegmentEdit;
        assert!(segment_edits(&[]).is_err());
        let dup = vec![
            SegmentEdit { index: 1, text: "a".into() },
            SegmentEdit { index: 1, text: "b".into() },
        ];
        assert!(segment_edits(&dup).is_err());
        let ok = vec![SegmentEdit { index: 0, text: "a".into() }];
        assert_eq!(segment_edits(&ok).unwrap(), vec![(0, "a".to_owned())]);
    }

    #[test]
    fn import_path_lists_are_bounded() {
        assert!(import_paths(&[]).is_err());
        let many: Vec<String> = (0..=MAX_IMPORT_PATHS).map(|i| format!("/tmp/{i}.wav")).collect();
        assert!(import_paths(&many).is_err());
    }

    #[test]
    fn filter_sets_reject_duplicates() {
        assert!(unique_states(&[JobState::Queued, JobState::Queued]).is_err());
        assert!(unique_kinds(&[JobKind::Import, JobKind::Import]).is_err());
        assert_eq!(unique_states(&[JobState::Queued]).unwrap(), vec![JobState::Queued]);
    }
}
