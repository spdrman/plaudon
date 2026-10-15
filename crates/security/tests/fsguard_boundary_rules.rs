//! The exact boundaries of the name rules: the byte at which a long name is
//! cut, the combining runs NFC has to reorder rather than compose, a policy
//! root that cannot be resolved, and the two shapes a destination root can have
//! that the joined-length rule cannot measure (§13.4, §13.5, §13.6, §16.5).
//!
//! `fsguard_hostile_paths.rs` runs the shared corpus and
//! `fsguard_rejection_vocabulary.rs` the refusal table; this suite covers the
//! off-by-one cases on either side of those, where the rule is "accept, but
//! exactly this much" rather than "reject".

use std::path::{Path, PathBuf};

use plaudon_security::fsguard::{
    canonicalize_and_validate, is_nfc, sanitize_filename, to_nfc, validate_archive_entry,
    ArchiveEntryMeta, ArchivePolicy, ArchiveRejection, PathPolicy, PathRejection,
    MAX_ON_DISK_BYTES, WINDOWS_SHORT_PATH_LIMIT,
};

/// A name over the on-disk budget is cut *before* the character that straddles
/// the limit, never through it: the byte at the budget is in the middle of a
/// two-byte sequence, and the result must still be the longest valid prefix.
#[test]
fn a_name_over_the_budget_is_cut_before_the_character_that_straddles_the_limit() {
    // 199 ASCII bytes, then a two-byte `é` occupying bytes 199 and 200: the
    // 200-byte budget lands inside it.
    let stem = "a".repeat(MAX_ON_DISK_BYTES - 1);
    let raw = format!("{stem}\u{e9}{}", "z".repeat(50));
    assert!(raw.len() > MAX_ON_DISK_BYTES);

    let sanitized = sanitize_filename(&raw);
    assert_eq!(
        sanitized.on_disk(),
        stem,
        "the straddling character is dropped whole"
    );
    assert_eq!(sanitized.on_disk().len(), MAX_ON_DISK_BYTES - 1);
    assert!(sanitized.changed());
    assert_eq!(sanitized.extension(), None);
}

/// A combining run that starts a name — no base character at all — is still
/// canonically ordered, which means the insertion walks all the way to the
/// front of the buffer.
#[test]
fn a_leading_combining_run_is_reordered_even_with_no_base_character() {
    // U+0301 (combining acute, class 230) written before U+0323 (combining dot
    // below, class 220). Canonical order is the lower class first.
    let input = "\u{301}\u{323}";
    assert!(!is_nfc(input), "the input order is not canonical");
    assert_eq!(to_nfc(input), "\u{323}\u{301}");
    assert!(is_nfc(&to_nfc(input)), "normalising is idempotent");
}

/// A Hangul syllable that already carries a trailing jamo cannot absorb a
/// second one: the extra jamo survives as its own character instead of being
/// folded into the syllable.
#[test]
fn a_second_trailing_jamo_is_not_folded_into_a_hangul_syllable() {
    // U+AC01 각 is LVT (base 가 plus trailing ᆨ); U+11A8 is another trailing ᆨ.
    let input = "\u{ac01}\u{11a8}";
    assert_eq!(to_nfc(input), input, "the second jamo stays separate");
    assert!(is_nfc(input));
    assert_eq!(input.chars().count(), 2);

    // The composable case, for contrast: L + V + T do fold into the syllable.
    assert_eq!(to_nfc("\u{1100}\u{1161}\u{11a8}"), "\u{ac01}");
}

/// An ASCII name is NFC by definition and is answered without normalising it.
#[test]
fn an_ascii_name_is_nfc_without_being_normalised() {
    assert!(is_nfc("Recording 2026-02-11.wav"));
    assert!(is_nfc(""));
    assert_eq!(
        to_nfc("Recording 2026-02-11.wav"),
        "Recording 2026-02-11.wav"
    );
    assert!(!is_nfc("e\u{301}"), "a decomposed name is not NFC");
}

/// A relative policy root that does not exist is not quietly resolved against
/// the process working directory: the candidate is refused because there is no
/// existing ancestor to anchor it to.
#[test]
fn a_relative_policy_root_is_never_resolved_against_the_working_directory() {
    let real = tempfile::tempdir().expect("temp dir");
    let relative = PathBuf::from("no-such-staging-root-cov276");
    assert!(!relative.exists(), "the fixture name must not exist");

    // The second root exists, so the policy has a canonical root and the
    // candidate is not refused for want of one; the first root is what a
    // relative candidate is anchored to.
    let policy = PathPolicy::for_roots([relative.clone(), real.path().to_path_buf()]);
    assert_eq!(policy.base(), Some(relative.as_path()));

    let error = canonicalize_and_validate(&policy, Path::new("notes.md"))
        .expect_err("an unanchored relative root cannot be resolved");
    assert_eq!(error.reason(), PathRejection::NoExistingAncestor);
}

/// The joined length is what has to fit, not the entry name: a short name under
/// a deep destination root is refused when the archive must stay restorable on
/// legacy Windows.
#[test]
fn a_short_entry_name_under_a_deep_destination_root_is_refused_by_length() {
    let root = PathBuf::from(format!("/srv/{}", "r".repeat(300)));
    let mut policy = ArchivePolicy::new(&root).with_windows_short_path_compatible(true);

    let entry = ArchiveEntryMeta::file("notes.md", 16, 16);
    assert!(entry.path.len() < WINDOWS_SHORT_PATH_LIMIT, "the name fits");
    assert!(
        root.join(&entry.path).as_os_str().len() > WINDOWS_SHORT_PATH_LIMIT,
        "the joined path does not"
    );

    let error = validate_archive_entry(&entry, &mut policy).expect_err("joined path too long");
    assert_eq!(error.reason(), ArchiveRejection::UnsupportedName);
    assert_eq!(error.name_rule(), Some(PathRejection::PathTooLong));

    // Without the legacy-Windows requirement the same entry is accepted: the
    // default ceiling is the extended-length one.
    let mut permissive = ArchivePolicy::new(&root);
    let accepted = validate_archive_entry(&entry, &mut permissive).expect("accepted");
    assert_eq!(accepted.relative_path(), Path::new("notes.md"));
}

/// A destination root that is not valid UTF-8 cannot be measured in bytes of a
/// `str`, and that is not a reason to refuse the entry: the entry's own name
/// rules still decide, and a valid name is still accepted.
#[cfg(unix)]
#[test]
fn a_non_utf8_destination_root_does_not_refuse_a_valid_entry() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let mut raw = b"/srv/backup-".to_vec();
    raw.push(0xff);
    let root = PathBuf::from(OsString::from_vec(raw));
    assert!(root.to_str().is_none(), "the root is not UTF-8");

    let mut policy = ArchivePolicy::new(&root).with_windows_short_path_compatible(true);
    let accepted = validate_archive_entry(&ArchiveEntryMeta::file("notes.md", 16, 16), &mut policy)
        .expect("a valid entry is accepted under an unmeasurable root");
    assert_eq!(accepted.relative_path(), Path::new("notes.md"));
    assert_eq!(accepted.duplicate_key(), "notes.md");

    // The name rules still apply under the same root.
    let error =
        validate_archive_entry(&ArchiveEntryMeta::file("../escape.md", 16, 16), &mut policy)
            .expect_err("traversal is still refused");
    assert_eq!(error.reason(), ArchiveRejection::Traversal);
}
