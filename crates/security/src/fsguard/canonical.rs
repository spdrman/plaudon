//! Path canonicalisation and containment — the single trust boundary for every
//! externally supplied path (spec §16.5, §7.4, §13.4).
//!
//! Externally supplied means: a file-dialog selection, a drag-and-drop payload,
//! a watched-folder event, a path read out of a portable backup, a path echoed
//! by the PLAUD Cloud CLI, or a path typed into the interface by the user.
//! None of those may reach `std::fs` directly; they go through
//! [`canonicalize_and_validate`] and come back as a [`ValidatedPath`], which has
//! a private field and therefore cannot be forged from an arbitrary `PathBuf`.

use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::error::{ErrorEnvelopeParts, SecurityError, CHOOSE_DIFFERENT_FILE, PATH_REJECTED};

/// Filesystem component length cap. 255 is the smallest of the limits we have
/// to satisfy (APFS/HFS+ 255 UTF-8 bytes, ext4 255 bytes, NTFS 255 UTF-16 code
/// units); we apply it in bytes, which is the stricter reading for non-ASCII.
pub const DEFAULT_MAX_COMPONENT_LEN: usize = 255;

/// Total path length cap. 32767 is the documented ceiling of the Windows
/// extended-length (`\\?\`) form; see [`PathPolicy::windows_short_path_compatible`]
/// for the 260-byte legacy limit.
pub const DEFAULT_MAX_PATH_LEN: usize = 32767;

/// Legacy Windows `MAX_PATH`. Paths longer than this work only when the process
/// and the filesystem both opt into long paths, which we cannot assume for a
/// user's own backup drive.
pub const WINDOWS_SHORT_PATH_LIMIT: usize = 260;

/// Windows device names. Reserved on Windows with *any* extension and
/// case-insensitively; enforced on every platform so an archive written on
/// macOS can be restored on Windows (§13.5, §18.5).
/// `COM0` and `LPT0` are included because Microsoft's "Naming Files, Paths,
/// and Namespaces" lists `COM0`-`COM9` and `LPT0`-`LPT9`; `CLOCK$` is not,
/// being DOS/9x folklore rather than a name current Windows reserves.
pub const WINDOWS_DEVICE_NAMES: [&str; 24] = [
    "CON", "PRN", "AUX", "NUL", "COM0", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
    "COM8", "COM9", "LPT0", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Characters Windows refuses in a filename. Enforced everywhere, same reason.
pub const WINDOWS_RESERVED_CHARS: [char; 7] = ['<', '>', ':', '"', '|', '?', '*'];

/// Policy for one trust boundary: which roots a path may resolve into and how
/// long its parts may be.
///
/// Construct with [`PathPolicy::for_roots`]. Roots are canonicalised at *check*
/// time rather than at construction, because a managed directory may be created
/// after the policy is built and because `/tmp` is itself a symlink on macOS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathPolicy {
    /// Permitted roots, in priority order. A relative candidate is resolved
    /// against `roots[0]`, never against the process working directory.
    pub roots: Vec<PathBuf>,
    /// Whether a symlink may legitimately leave the permitted roots.
    ///
    /// **Always `false` for externally supplied input.** When `false`, a path
    /// whose resolved target lands outside every root is rejected — the target
    /// is never opened or read. When `true`, a link that starts inside a root
    /// is allowed to resolve outside it; that is an explicit opt-in for
    /// app-owned locations (for example an export directory the user pointed at
    /// an external volume), never for untrusted input.
    pub follow_symlinks: bool,
    /// Per-component byte cap. See [`DEFAULT_MAX_COMPONENT_LEN`].
    pub max_component_len: usize,
    /// Whole-path byte cap. See [`DEFAULT_MAX_PATH_LEN`].
    pub max_path_len: usize,
    /// Reject anything longer than [`WINDOWS_SHORT_PATH_LIMIT`]. Set for paths
    /// that must survive a round trip through a portable backup (§13.6).
    pub windows_short_path_compatible: bool,
}

impl PathPolicy {
    /// Policy permitting exactly `roots`, with symlink escape refused and the
    /// default length caps.
    pub fn for_roots<I, P>(roots: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        Self {
            roots: roots.into_iter().map(Into::into).collect(),
            follow_symlinks: false,
            max_component_len: DEFAULT_MAX_COMPONENT_LEN,
            max_path_len: DEFAULT_MAX_PATH_LEN,
            windows_short_path_compatible: false,
        }
    }

    /// Require legacy-Windows-compatible path lengths (≤ 260 bytes).
    #[must_use]
    pub fn with_windows_short_path_compatible(mut self, yes: bool) -> Self {
        self.windows_short_path_compatible = yes;
        self
    }

    /// Override the per-component byte cap (never above the filesystem limit).
    #[must_use]
    pub fn with_max_component_len(mut self, len: usize) -> Self {
        self.max_component_len = len;
        self
    }

    /// Override the whole-path byte cap.
    #[must_use]
    pub fn with_max_path_len(mut self, len: usize) -> Self {
        self.max_path_len = len;
        self
    }

    /// Opt into following a symlink out of the permitted roots. Never call this
    /// for a path that came from outside the process.
    #[must_use]
    pub fn allowing_symlink_escape(mut self) -> Self {
        self.follow_symlinks = true;
        self
    }

    /// The root a relative candidate is resolved against.
    pub fn base(&self) -> Option<&Path> {
        self.roots.first().map(PathBuf::as_path)
    }

    /// Canonical forms of the roots that currently exist on disk.
    fn canonical_roots(&self) -> Vec<PathBuf> {
        self.roots
            .iter()
            .filter_map(|r| fs::canonicalize(r).ok())
            .collect()
    }
}

impl Default for PathPolicy {
    /// An empty policy permits nothing; every candidate is rejected with
    /// [`PathRejection::NoPermittedRoot`].
    fn default() -> Self {
        Self::for_roots(Vec::<PathBuf>::new())
    }
}

/// A path that passed [`canonicalize_and_validate`].
///
/// The inner `PathBuf` is private, so an unvalidated path cannot be substituted
/// where a `ValidatedPath` is required. That is the structural half of the
/// "no direct `std::fs` on unvalidated paths" rule; the lint half lives in
/// `crates/security/tests/fsguard_hostile_paths.rs`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ValidatedPath {
    path: PathBuf,
}

impl ValidatedPath {
    /// Only this module may assert validity; every other construction path goes
    /// through [`canonicalize_and_validate`].
    pub(crate) fn new_unchecked(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn as_path(&self) -> &Path {
        &self.path
    }

    pub fn into_path_buf(self) -> PathBuf {
        self.path
    }

    /// Final component, if any. Already validated, so it is safe to log.
    pub fn file_name(&self) -> Option<&std::ffi::OsStr> {
        self.path.file_name()
    }

    /// Parent directory. Still validated: a prefix of a contained path is
    /// contained as long as it is not shorter than the root, which the caller
    /// checks by re-validating if it intends to write there.
    pub fn parent(&self) -> Option<&Path> {
        self.path.parent()
    }
}

impl AsRef<Path> for ValidatedPath {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

/// Why a path was refused. One variant per rule so tests and telemetry can
/// assert the exact rule without parsing a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PathRejection {
    /// Empty string or empty `OsStr`.
    Empty,
    /// Not valid UTF-8. The managed archive stores UTF-8 names only (§13.4).
    NonUtf8,
    /// Embedded NUL: truncates the path in every C-level filesystem API.
    NulByte,
    /// C0/C1 control character, including TAB, LF, CR and DEL.
    ControlCharacter,
    /// Bidi override/isolate, zero-width or other invisible formatting
    /// character — the `exploit<U+202E>gnp.exe` class of display spoof.
    BidiControl,
    /// A `..` segment, before or after canonicalisation.
    ParentTraversal,
    /// A `.` segment. Our writers never emit one; failing closed is cheaper
    /// than reasoning about mixed `./` forms from a foreign archiver.
    CurrentDirSegment,
    /// `\\server\share` (or `//server/share`): a network location, not a path
    /// inside a managed root.
    UncPath,
    /// `\\?\` or `\\.\`: the Win32 device namespace, which bypasses the normal
    /// name parsing we rely on.
    DeviceNamespace,
    /// `CON`, `NUL`, `COM3.txt`, … — reserved on Windows with any extension.
    WindowsDeviceName,
    /// `:` in a non-drive component: an NTFS alternate data stream.
    AlternateDataStream,
    /// One of `< > " | ? *`.
    ReservedCharacter,
    /// Trailing `.` or space: silently stripped by Windows, so `evil.exe. `
    /// and `evil.exe` would collide after a restore.
    TrailingDotOrSpace,
    /// Leading space: invisible in most UI, so ` invoice.pdf` and
    /// `invoice.pdf` are two files the user cannot tell apart.
    LeadingSpace,
    /// Component longer than [`PathPolicy::max_component_len`].
    ComponentTooLong,
    /// Path longer than [`PathPolicy::max_path_len`].
    PathTooLong,
    /// Path longer than [`WINDOWS_SHORT_PATH_LIMIT`] under a policy that
    /// requires legacy-Windows portability.
    NotWindowsShortPathCompatible,
    /// An absolute path where a relative one is required (archive entries).
    AbsolutePath,
    /// A drive-qualified path (`C:\…`) on a host that has no drives; we cannot
    /// resolve it against this machine's roots, so we refuse it.
    ForeignAbsolutePath,
    /// Windows root-relative (`\x`, `/x` with no drive): the volume depends on
    /// the process working directory.
    AmbiguousRootRelativePath,
    /// The policy has no root that exists, so nothing can be contained.
    NoPermittedRoot,
    /// Resolved outside every permitted root.
    OutsideRoot,
    /// Resolved outside every permitted root *through a symlink or junction*.
    /// The target was never opened.
    SymlinkEscapesRoot,
    /// A component exists but cannot be resolved — a dangling or looping
    /// symlink. Writing through it could land anywhere, so it is refused.
    UnresolvableLink,
    /// Not even the root of the candidate exists.
    NoExistingAncestor,
    /// A filesystem call failed; the kind is carried in the detail, the path is
    /// never echoed.
    Io,
    /// A rename would cross a filesystem boundary, so it cannot be atomic
    /// (§6.3B: "verified same-volume rename/commit").
    CrossVolumeRename,
    /// Restrictive permissions could not be applied to managed data (§16.5).
    PermissionsNotApplied,
}

impl PathRejection {
    /// Stable `snake_case` rule name. Safe to log and to show in diagnostics.
    pub const fn rule(self) -> &'static str {
        match self {
            PathRejection::Empty => "empty_path",
            PathRejection::NonUtf8 => "non_utf8_path",
            PathRejection::NulByte => "nul_byte",
            PathRejection::ControlCharacter => "control_character",
            PathRejection::BidiControl => "bidi_or_invisible_character",
            PathRejection::ParentTraversal => "parent_traversal",
            PathRejection::CurrentDirSegment => "current_dir_segment",
            PathRejection::UncPath => "unc_path",
            PathRejection::DeviceNamespace => "device_namespace",
            PathRejection::WindowsDeviceName => "windows_device_name",
            PathRejection::AlternateDataStream => "alternate_data_stream",
            PathRejection::ReservedCharacter => "reserved_character",
            PathRejection::TrailingDotOrSpace => "trailing_dot_or_space",
            PathRejection::LeadingSpace => "leading_space",
            PathRejection::ComponentTooLong => "component_too_long",
            PathRejection::PathTooLong => "path_too_long",
            PathRejection::NotWindowsShortPathCompatible => "not_windows_short_path_compatible",
            PathRejection::AbsolutePath => "absolute_path",
            PathRejection::ForeignAbsolutePath => "foreign_absolute_path",
            PathRejection::AmbiguousRootRelativePath => "ambiguous_root_relative_path",
            PathRejection::NoPermittedRoot => "no_permitted_root",
            PathRejection::OutsideRoot => "outside_permitted_root",
            PathRejection::SymlinkEscapesRoot => "symlink_escapes_root",
            PathRejection::UnresolvableLink => "unresolvable_link",
            PathRejection::NoExistingAncestor => "no_existing_ancestor",
            PathRejection::Io => "filesystem_error",
            PathRejection::CrossVolumeRename => "cross_volume_rename",
            PathRejection::PermissionsNotApplied => "permissions_not_applied",
        }
    }

    /// Whether the user can fix this by picking another file. Internal
    /// filesystem failures are not the user's to fix, so they get no action.
    pub const fn is_user_choosable(self) -> bool {
        !matches!(
            self,
            PathRejection::Io
                | PathRejection::NoPermittedRoot
                | PathRejection::PermissionsNotApplied
                | PathRejection::CrossVolumeRename
        )
    }
}

impl fmt::Display for PathRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.rule())
    }
}

/// A refused path.
///
/// The offending path is **never** carried: §15.4 forbids leaking user data
/// into an error envelope, and a hostile path is itself a log-injection vector.
/// At most one offending component survives, truncated and with every control
/// character escaped (see [`escape_for_message`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathError {
    reason: PathRejection,
    detail: Option<String>,
}

impl PathError {
    pub(crate) fn new(reason: PathRejection) -> Self {
        Self {
            reason,
            detail: None,
        }
    }

    /// Attach the offending component, truncated and escaped.
    pub(crate) fn with_component(reason: PathRejection, component: &str) -> Self {
        Self {
            reason,
            detail: Some(escape_for_message(component, MAX_DETAIL_CHARS)),
        }
    }

    /// Attach a fixed, non-user-derived detail (a length, an `io::ErrorKind`).
    pub(crate) fn with_detail(reason: PathRejection, detail: impl Into<String>) -> Self {
        Self {
            reason,
            detail: Some(detail.into()),
        }
    }

    pub(crate) fn from_io(err: &io::Error) -> Self {
        Self::with_detail(PathRejection::Io, format!("{:?}", err.kind()))
    }

    pub const fn reason(&self) -> PathRejection {
        self.reason
    }

    /// The escaped offending component, when the rule had one.
    pub fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }
}

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "path rejected: {}", self.reason.rule())?;
        if let Some(detail) = &self.detail {
            write!(f, " ({detail})")?;
        }
        Ok(())
    }
}

impl std::error::Error for PathError {}

impl SecurityError for PathError {
    fn envelope_parts(&self) -> ErrorEnvelopeParts {
        let parts = ErrorEnvelopeParts::new(PATH_REJECTED, self.to_string()).retryable(false);
        if self.reason.is_user_choosable() {
            parts.action(CHOOSE_DIFFERENT_FILE)
        } else {
            parts
        }
    }
}

/// How much of an offending component may appear in a message.
const MAX_DETAIL_CHARS: usize = 40;

/// Render `raw` so it is safe to put in a log line or an error envelope:
/// control, bidi and invisible characters become `\u{…}`, and the result is
/// truncated on a character boundary.
pub fn escape_for_message(raw: &str, max_chars: usize) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    for (taken, ch) in raw.chars().enumerate() {
        if taken >= max_chars {
            out.push('…');
            break;
        }
        if ch == '\\' {
            out.push_str("\\\\");
        } else if ch.is_control() || is_invisible_or_bidi(ch) {
            // Writing into a String cannot fail.
            let _ = write!(out, "\\u{{{:04X}}}", ch as u32);
        } else {
            out.push(ch);
        }
    }
    out
}

/// Bidi controls, isolates, zero-width characters and other invisible
/// formatting marks. These are refused in a path and stripped by
/// [`crate::fsguard::sanitize_filename`].
///
/// U+200D ZERO WIDTH JOINER is included even though emoji sequences use it:
/// a managed archive name that depends on an invisible joiner is not worth the
/// spoofing surface, and the original name is preserved as display metadata.
/// U+FE0F VARIATION SELECTOR-16 is **not** included, so `🎙️` survives intact.
pub const fn is_invisible_or_bidi(ch: char) -> bool {
    matches!(ch,
        '\u{00AD}' | '\u{061C}' | '\u{180E}'
        | '\u{200B}'..='\u{200F}'
        | '\u{202A}'..='\u{202E}'
        | '\u{2060}'..='\u{2064}'
        | '\u{2066}'..='\u{2069}'
        | '\u{FEFF}'
        | '\u{FFF9}'..='\u{FFFB}'
    )
}

/// True when `name`'s pre-extension stem is a Windows device name.
pub fn is_windows_device_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name);
    let stem = stem.trim_end_matches([' ', '\t']);
    if stem.is_empty() || stem.len() > 4 {
        return false;
    }
    WINDOWS_DEVICE_NAMES
        .iter()
        .any(|device| device.eq_ignore_ascii_case(stem))
}

/// Validate one path component against every name rule.
///
/// Pure: it never touches the filesystem, so archive restore can run it over a
/// whole manifest before mutating anything (§13.5).
pub fn check_component(name: &str, max_component_len: usize) -> Result<(), PathError> {
    if name.is_empty() {
        return Err(PathError::new(PathRejection::Empty));
    }
    if name == "." {
        return Err(PathError::new(PathRejection::CurrentDirSegment));
    }
    if name == ".." {
        return Err(PathError::new(PathRejection::ParentTraversal));
    }
    for ch in name.chars() {
        let rejection = match ch {
            '\0' => Some(PathRejection::NulByte),
            c if c.is_control() => Some(PathRejection::ControlCharacter),
            c if is_invisible_or_bidi(c) => Some(PathRejection::BidiControl),
            ':' => Some(PathRejection::AlternateDataStream),
            '/' | '\\' => Some(PathRejection::ReservedCharacter),
            c if WINDOWS_RESERVED_CHARS.contains(&c) => Some(PathRejection::ReservedCharacter),
            _ => None,
        };
        if let Some(reason) = rejection {
            return Err(PathError::with_component(reason, name));
        }
    }
    if is_windows_device_name(name) {
        return Err(PathError::with_component(
            PathRejection::WindowsDeviceName,
            name,
        ));
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return Err(PathError::with_component(
            PathRejection::TrailingDotOrSpace,
            name,
        ));
    }
    if name.starts_with(' ') {
        return Err(PathError::with_component(PathRejection::LeadingSpace, name));
    }
    if name.len() > max_component_len {
        return Err(PathError::with_detail(
            PathRejection::ComponentTooLong,
            format!("{} bytes", name.len()),
        ));
    }
    Ok(())
}

/// How a candidate string is anchored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Anchor {
    /// `foo/bar` — resolved against the policy base.
    Relative,
    /// `/foo/bar` on unix.
    PosixAbsolute,
    /// `C:\foo` — absolute, but only on a host that has drive letters.
    DriveAbsolute,
    /// `\foo` or `/foo` on Windows: the volume comes from the process state.
    RootRelative,
}

/// The parsed, rule-checked form of a candidate string.
struct ParsedPath {
    anchor: Anchor,
    segments: Vec<String>,
}

/// Split on both separators and apply every name rule, without touching disk.
///
/// `\` is a separator here because a path produced on Windows must mean the
/// same thing when it is validated on macOS; `check_component` then refuses a
/// stray `\` that survives inside a component.
fn parse_and_check(raw: &str, policy: &PathPolicy) -> Result<ParsedPath, PathError> {
    if raw.is_empty() {
        return Err(PathError::new(PathRejection::Empty));
    }
    if raw.len() > policy.max_path_len {
        return Err(PathError::with_detail(
            PathRejection::PathTooLong,
            format!("{} bytes", raw.len()),
        ));
    }
    if policy.windows_short_path_compatible && raw.len() > WINDOWS_SHORT_PATH_LIMIT {
        return Err(PathError::with_detail(
            PathRejection::NotWindowsShortPathCompatible,
            format!("{} bytes", raw.len()),
        ));
    }
    if raw.starts_with("\\\\?\\") || raw.starts_with("\\\\.\\") || raw.starts_with("//?/") {
        return Err(PathError::new(PathRejection::DeviceNamespace));
    }
    if raw.starts_with("\\\\") || raw.starts_with("//") {
        return Err(PathError::new(PathRejection::UncPath));
    }

    let bytes = raw.as_bytes();
    let drive_prefixed = bytes.len() >= 2
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes.len() == 2 || bytes[2] == b'/' || bytes[2] == b'\\');
    let root_relative = bytes[0] == b'/' || bytes[0] == b'\\';

    let (anchor, body) = if drive_prefixed {
        (Anchor::DriveAbsolute, &raw[2..])
    } else if root_relative {
        (
            if cfg!(windows) {
                Anchor::RootRelative
            } else {
                Anchor::PosixAbsolute
            },
            raw,
        )
    } else {
        (Anchor::Relative, raw)
    };

    let mut segments = Vec::new();
    for segment in body.split(['/', '\\']) {
        if segment.is_empty() {
            // Leading separator of an absolute path, or a doubled separator.
            continue;
        }
        check_component(segment, policy.max_component_len)?;
        segments.push(segment.to_owned());
    }
    if segments.is_empty() && anchor == Anchor::Relative {
        return Err(PathError::new(PathRejection::Empty));
    }
    Ok(ParsedPath { anchor, segments })
}

/// Validate `candidate` and resolve it to a canonical path inside `policy`.
///
/// This is the only sanctioned way to turn an externally supplied path into
/// something the rest of the application may hand to `std::fs`.
///
/// Order of operations, all of which matter:
///
/// 1. name rules (NUL, control, bidi, reserved, device names, trailing dot,
///    lengths) and a `..`/`.` scan **before** anything touches the filesystem;
/// 2. resolution of the deepest existing ancestor with [`fs::canonicalize`],
///    which follows symlinks and junctions;
/// 3. a containment check on the **resolved** path, so a link pointing out of
///    the root is rejected rather than followed — the target is never opened;
/// 4. a lexical check of the not-yet-existing remainder, so a staging target
///    inside the root validates without being created first.
///
/// A relative candidate is resolved against `policy.roots[0]`, never against
/// the process working directory.
pub fn canonicalize_and_validate(
    policy: &PathPolicy,
    candidate: &Path,
) -> Result<ValidatedPath, PathError> {
    let raw = candidate
        .to_str()
        .ok_or_else(|| PathError::new(PathRejection::NonUtf8))?;
    let parsed = parse_and_check(raw, policy)?;

    let roots = policy.canonical_roots();
    if roots.is_empty() {
        return Err(PathError::new(PathRejection::NoPermittedRoot));
    }

    let lexical: PathBuf = match parsed.anchor {
        Anchor::Relative => {
            let base = policy
                .base()
                .ok_or_else(|| PathError::new(PathRejection::NoPermittedRoot))?;
            let mut p = base.to_path_buf();
            p.extend(parsed.segments.iter());
            p
        }
        Anchor::PosixAbsolute => {
            let mut p = PathBuf::from("/");
            p.extend(parsed.segments.iter());
            p
        }
        Anchor::DriveAbsolute if cfg!(windows) => {
            let mut p = PathBuf::from(&raw[..3.min(raw.len())]);
            p.extend(parsed.segments.iter());
            p
        }
        Anchor::DriveAbsolute => return Err(PathError::new(PathRejection::ForeignAbsolutePath)),
        Anchor::RootRelative => {
            return Err(PathError::new(PathRejection::AmbiguousRootRelativePath))
        }
    };

    let (resolved_base, remainder) = resolve_deepest_existing(&lexical)?;

    // Anything that exists under the resolved base but could not be resolved is
    // a dangling or looping link; writing through it could land anywhere.
    let mut probe = resolved_base.clone();
    for part in &remainder {
        probe.push(part);
        match fs::symlink_metadata(&probe) {
            Ok(_) => return Err(PathError::new(PathRejection::UnresolvableLink)),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(PathError::from_io(&err)),
        }
    }

    let mut resolved = resolved_base;
    for part in &remainder {
        resolved.push(part);
    }

    if roots.iter().any(|root| resolved.starts_with(root)) {
        return Ok(ValidatedPath::new_unchecked(resolved));
    }

    // Not contained. Distinguish "you picked a path outside the archive" from
    // "a link inside the archive pointed out of it", and honour the explicit
    // opt-in for app-owned locations.
    let started_inside = roots.iter().any(|root| lexical.starts_with(root))
        || policy.roots.iter().any(|root| lexical.starts_with(root));
    if started_inside {
        if policy.follow_symlinks {
            return Ok(ValidatedPath::new_unchecked(resolved));
        }
        return Err(PathError::new(PathRejection::SymlinkEscapesRoot));
    }
    Err(PathError::new(PathRejection::OutsideRoot))
}

/// Canonicalise the deepest existing ancestor of `lexical`, returning it plus
/// the components that do not exist yet (in order).
fn resolve_deepest_existing(lexical: &Path) -> Result<(PathBuf, Vec<OsString>), PathError> {
    let mut remainder: Vec<OsString> = Vec::new();
    let mut probe = lexical.to_path_buf();
    loop {
        match fs::canonicalize(&probe) {
            Ok(resolved) => {
                remainder.reverse();
                return Ok((resolved, remainder));
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                let Some(name) = probe.file_name().map(std::ffi::OsStr::to_owned) else {
                    return Err(PathError::new(PathRejection::NoExistingAncestor));
                };
                remainder.push(name);
                if !probe.pop() || probe.as_os_str().is_empty() {
                    return Err(PathError::new(PathRejection::NoExistingAncestor));
                }
            }
            Err(err) => return Err(PathError::from_io(&err)),
        }
    }
}

/// Validate a **relative** path without touching the filesystem.
///
/// Used by portable-backup restore, which must reject a whole manifest before
/// any user data is mutated (§13.5), and by anything that needs to check a name
/// that does not exist yet. Returns the normalised relative path (separators
/// unified, empty segments dropped).
pub fn validate_relative(policy: &PathPolicy, raw: &str) -> Result<PathBuf, PathError> {
    let parsed = parse_and_check(raw, policy)?;
    match parsed.anchor {
        Anchor::Relative => {}
        Anchor::DriveAbsolute | Anchor::PosixAbsolute | Anchor::RootRelative => {
            return Err(PathError::new(PathRejection::AbsolutePath))
        }
    }
    let mut out = PathBuf::new();
    out.extend(parsed.segments.iter());
    Ok(out)
}
