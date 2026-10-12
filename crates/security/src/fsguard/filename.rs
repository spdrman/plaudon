//! Hostile filename normalisation (spec §13.4 "sanitize extensions, preserve
//! original names as metadata only", §18.5 "test hostile filenames", §16.6 the
//! UI must not be able to misrepresent what a file is).
//!
//! [`sanitize_filename`] produces two strings from one untrusted name:
//!
//! * [`SanitizedName::on_disk`] — a single path component that satisfies every
//!   rule in [`super::check_component`] on every platform;
//! * [`SanitizedName::display`] — what the user sees. It keeps the characters
//!   the on-disk form had to drop (spaces, punctuation, scripts) but is itself
//!   stripped of control, bidi-override and zero-width characters, so a
//!   `README\u{202E}gnp.exe` cannot render as `READMEexe.png` in the library
//!   list.
//!
//! Neither form is a security boundary on its own: the archive is addressed by
//! `sha256(bytes)` (§6.3B) and the original name is metadata.

use sha2::{Digest, Sha256};

use super::canonical::{is_invisible_or_bidi, is_windows_device_name, WINDOWS_RESERVED_CHARS};
use super::nfc::{combining_class, to_nfc};

/// Byte budget for the on-disk name. Below the 255-byte component limit by
/// enough room for the `.part` staging suffix and a disambiguating counter.
pub const MAX_ON_DISK_BYTES: usize = 200;

/// Character budget for the display name, so one pathological title cannot
/// dominate a list row or a notification body (§5.5).
pub const MAX_DISPLAY_CHARS: usize = 120;

/// Longest extension we keep. Anything longer is treated as part of the stem.
pub const MAX_EXTENSION_BYTES: usize = 16;

/// Replacement for a character that cannot appear in a filename.
const REPLACEMENT: char = '_';

/// A filename that is safe to put on disk, plus a display form that is safe to
/// put in the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SanitizedName {
    on_disk: String,
    display: String,
    extension: Option<String>,
    changed: bool,
    mixed_scripts: bool,
}

impl SanitizedName {
    /// The name to use on disk: one component, no separators, no reserved
    /// characters or names, no trailing dot or space, at most
    /// [`MAX_ON_DISK_BYTES`] bytes.
    pub fn on_disk(&self) -> &str {
        &self.on_disk
    }

    /// The name to show the user. Free of control, bidi and zero-width
    /// characters, length-bounded, never empty.
    pub fn display(&self) -> &str {
        &self.display
    }

    /// Lowercased, alphanumeric extension of the on-disk name, if it has one.
    /// This is the `<ext>` of `media/sha256/<ab>/<hash>.<ext>` (§13.4).
    pub fn extension(&self) -> Option<&str> {
        self.extension.as_deref()
    }

    /// Whether the on-disk form differs from the input.
    pub fn changed(&self) -> bool {
        self.changed
    }

    /// Whether the display name mixes Latin with Cyrillic or Greek letters.
    ///
    /// That is the homograph pattern (`pаypal.pdf` with a Cyrillic `а`). We do
    /// **not** rewrite those characters — plenty of legitimate names are not
    /// Latin — we flag them so the UI can mark the name as unverified rather
    /// than quietly presenting a lookalike.
    pub fn mixed_scripts(&self) -> bool {
        self.mixed_scripts
    }
}

/// Normalise one untrusted filename.
///
/// Deterministic: the same input always produces the same output, including the
/// `file-<hash>` fallback, so a repeated import does not produce a new name.
pub fn sanitize_filename(raw: &str) -> SanitizedName {
    let normalised = to_nfc(raw);

    let mut on_disk = String::with_capacity(normalised.len());
    for ch in normalised.chars() {
        if is_invisible_or_bidi(ch) {
            // Dropped, not replaced: these characters carry no content, and a
            // visible `_` for each one would make the on-disk name noisier than
            // the original it is meant to stand in for.
            continue;
        }
        if ch == '\0' || ch.is_control() || ch.is_whitespace() {
            // Control characters become a space rather than vanishing, so a
            // newline cannot silently weld two words together, and so the
            // on-disk and display forms agree on where the word breaks are.
            on_disk.push(' ');
        } else if ch == '/' || ch == '\\' || WINDOWS_RESERVED_CHARS.contains(&ch) {
            on_disk.push(REPLACEMENT);
        } else {
            on_disk.push(ch);
        }
    }

    let on_disk = collapse_spaces(&on_disk);
    // Leading dots are kept (a name may legitimately start with one); trailing
    // dots and spaces are not, because Windows strips them silently and
    // `evil.exe. ` and `evil.exe` would become the same file after a restore.
    let on_disk = on_disk
        .trim_start_matches(' ')
        .trim_end_matches([' ', '.'])
        .to_owned();
    let on_disk = truncate_component(&on_disk);

    let on_disk = if on_disk.is_empty() || is_windows_device_name(&on_disk) {
        fallback_name(raw, extension_of(&on_disk))
    } else {
        on_disk
    };

    let extension = extension_of(&on_disk).map(str::to_owned);
    let display = build_display(&normalised, &on_disk);
    let mixed_scripts = mixes_scripts(&normalised);

    SanitizedName {
        changed: on_disk != raw,
        on_disk,
        display,
        extension,
        mixed_scripts,
    }
}

/// Deterministic replacement for a name that sanitised to nothing usable.
fn fallback_name(raw: &str, extension: Option<&str>) -> String {
    let digest = Sha256::digest(raw.as_bytes());
    let short = hex::encode(&digest[..6]);
    match extension {
        Some(ext) => format!("file-{short}.{ext}"),
        None => format!("file-{short}"),
    }
}

/// Lowercased alphanumeric extension, or `None` when the tail is not one.
fn extension_of(name: &str) -> Option<&str> {
    let (stem, ext) = name.rsplit_once('.')?;
    if stem.is_empty() || ext.is_empty() || ext.len() > MAX_EXTENSION_BYTES {
        return None;
    }
    if ext.bytes().all(|b| b.is_ascii_alphanumeric()) {
        Some(ext)
    } else {
        None
    }
}

fn collapse_spaces(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut previous_space = false;
    for ch in input.chars() {
        if ch == ' ' {
            if !previous_space {
                out.push(' ');
            }
            previous_space = true;
        } else {
            previous_space = false;
            out.push(ch);
        }
    }
    out
}

/// Truncate to [`MAX_ON_DISK_BYTES`], keeping the extension and never cutting a
/// UTF-8 sequence or a combining cluster.
fn truncate_component(name: &str) -> String {
    if name.len() <= MAX_ON_DISK_BYTES {
        return name.to_owned();
    }
    let suffix = match extension_of(name) {
        Some(ext) => format!(".{ext}"),
        None => String::new(),
    };
    let stem_end = name.len() - suffix.len();
    let stem = &name[..stem_end];
    let budget = MAX_ON_DISK_BYTES.saturating_sub(suffix.len());
    let mut truncated = truncate_at_cluster(stem, budget).to_owned();
    truncated = truncated.trim_end_matches([' ', '.']).to_owned();
    if truncated.is_empty() {
        return String::new();
    }
    truncated.push_str(&suffix);
    truncated
}

/// Largest prefix of `input` that is at most `max_bytes` long, ends on a
/// character boundary, and does not leave a combining cluster half-copied.
///
/// "Cluster" here is the conservative subset we can decide without a full
/// grapheme-break table: canonical combining marks, variation selectors, emoji
/// skin-tone modifiers and the combining enclosing keycap. A cut in the middle
/// of, say, a regional-indicator flag pair is therefore still possible; the
/// result is always valid UTF-8 and never a dangling mark without its base.
fn truncate_at_cluster(input: &str, max_bytes: usize) -> &str {
    if input.len() <= max_bytes {
        return input;
    }
    let mut end = max_bytes;
    while end > 0 && !input.is_char_boundary(end) {
        end -= 1;
    }
    loop {
        if end == 0 {
            return "";
        }
        let next_extends = input[end..].chars().next().is_some_and(is_cluster_extender);
        if !next_extends {
            return &input[..end];
        }
        match input[..end].chars().next_back() {
            Some(previous) => end -= previous.len_utf8(),
            None => return "",
        }
    }
}

fn is_cluster_extender(ch: char) -> bool {
    combining_class(ch) != 0
        || matches!(ch,
            '\u{200D}'
            | '\u{20E3}'
            | '\u{FE00}'..='\u{FE0F}'
            | '\u{1F3FB}'..='\u{1F3FF}'
            | '\u{E0100}'..='\u{E01EF}'
        )
}

/// Build the display form: the user's own name, minus anything that can lie
/// about it, bounded in length, never empty.
fn build_display(normalised: &str, on_disk: &str) -> String {
    let mut out = String::with_capacity(normalised.len());
    for ch in normalised.chars() {
        if ch == '\0' || is_invisible_or_bidi(ch) {
            continue;
        }
        if ch.is_control() || ch.is_whitespace() {
            out.push(' ');
        } else {
            out.push(ch);
        }
    }
    let out = collapse_spaces(&out);
    let out = out.trim();
    let out = truncate_at_cluster(out, out.len().min(display_byte_budget(out)));
    let out = out.trim_end();
    if out.is_empty() {
        on_disk.to_owned()
    } else {
        out.to_owned()
    }
}

/// Byte offset of the [`MAX_DISPLAY_CHARS`]-th character.
fn display_byte_budget(input: &str) -> usize {
    input
        .char_indices()
        .nth(MAX_DISPLAY_CHARS)
        .map_or(input.len(), |(offset, _)| offset)
}

/// Script buckets we can distinguish cheaply, chosen because they are the ones
/// that share lookalike letters with ASCII.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Script {
    Latin,
    Cyrillic,
    Greek,
}

fn script_of(ch: char) -> Option<Script> {
    match ch {
        'A'..='Z' | 'a'..='z' | '\u{00C0}'..='\u{024F}' | '\u{1E00}'..='\u{1EFF}' => {
            Some(Script::Latin)
        }
        '\u{0370}'..='\u{03FF}' | '\u{1F00}'..='\u{1FFF}' => Some(Script::Greek),
        '\u{0400}'..='\u{052F}' => Some(Script::Cyrillic),
        _ => None,
    }
}

/// Whether any single *word* mixes two of these scripts.
///
/// The check is per run of letters, not per name: `Привет мир.mp3` is a
/// perfectly ordinary Cyrillic name whose extension happens to be Latin, and
/// flagging it would train the user to ignore the flag. `pаypal.pdf`, where one
/// word is Latin except for a Cyrillic `а`, is the pattern worth reporting.
fn mixes_scripts(input: &str) -> bool {
    let mut run: Option<Script> = None;
    for ch in input.chars() {
        match script_of(ch) {
            Some(script) => match run {
                None => run = Some(script),
                Some(previous) if previous != script => return true,
                Some(_) => {}
            },
            // A digit, separator, punctuation mark or character from any other
            // script ends the run.
            None => run = None,
        }
    }
    false
}
