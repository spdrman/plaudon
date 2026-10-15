//! Path and archive rejection vocabulary, and the rules a happy path never
//! reaches (§16.5, §7.4, §13.4, §13.5, §13.6).
//!
//! `src/fsguard/tests.rs` covers the filesystem-touching behaviour. This suite
//! covers the parts that are pure: every rejection's stable rule name and
//! user-choosability, the policy builders, the relative-path validator's
//! absolute forms, the message escaper, and the full archive refusal table
//! including the caps at their exact boundaries.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use plaudon_security::error::SecurityError;
use plaudon_security::fsguard::{
    canonicalize_and_validate, check_component, escape_for_message, is_invisible_or_bidi,
    is_windows_device_name, sanitize_filename, validate_archive_entry, validate_relative,
    ArchiveEntryKind, ArchiveEntryMeta, ArchivePolicy, ArchiveRejection, PathPolicy, PathRejection,
    DEFAULT_MAX_COMPONENT_LEN, WINDOWS_DEVICE_NAMES, WINDOWS_RESERVED_CHARS,
    WINDOWS_SHORT_PATH_LIMIT,
};

const ALL_PATH_REJECTIONS: [PathRejection; 28] = [
    PathRejection::Empty,
    PathRejection::NonUtf8,
    PathRejection::NulByte,
    PathRejection::ControlCharacter,
    PathRejection::BidiControl,
    PathRejection::ParentTraversal,
    PathRejection::CurrentDirSegment,
    PathRejection::UncPath,
    PathRejection::DeviceNamespace,
    PathRejection::WindowsDeviceName,
    PathRejection::AlternateDataStream,
    PathRejection::ReservedCharacter,
    PathRejection::TrailingDotOrSpace,
    PathRejection::LeadingSpace,
    PathRejection::ComponentTooLong,
    PathRejection::PathTooLong,
    PathRejection::NotWindowsShortPathCompatible,
    PathRejection::AbsolutePath,
    PathRejection::ForeignAbsolutePath,
    PathRejection::AmbiguousRootRelativePath,
    PathRejection::NoPermittedRoot,
    PathRejection::OutsideRoot,
    PathRejection::SymlinkEscapesRoot,
    PathRejection::UnresolvableLink,
    PathRejection::NoExistingAncestor,
    PathRejection::Io,
    PathRejection::CrossVolumeRename,
    PathRejection::PermissionsNotApplied,
];

/// Every rejection has a distinct `snake_case` rule name, renders as that name,
/// and only the four internal failures withhold the "choose another file"
/// affordance — a user cannot fix a cross-volume rename by picking a file.
#[test]
fn every_path_rejection_has_a_distinct_rule_and_an_honest_user_affordance() {
    let mut rules = BTreeSet::new();
    for rejection in ALL_PATH_REJECTIONS {
        let rule = rejection.rule();
        assert!(rules.insert(rule), "duplicate rule {rule}");
        assert!(
            rule.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
            "{rule} is not snake_case"
        );
        assert_eq!(format!("{rejection}"), rule);

        let internal = matches!(
            rejection,
            PathRejection::Io
                | PathRejection::NoPermittedRoot
                | PathRejection::PermissionsNotApplied
                | PathRejection::CrossVolumeRename
        );
        assert_eq!(
            rejection.is_user_choosable(),
            !internal,
            "{rule} reports the wrong affordance"
        );
    }
    assert_eq!(rules.len(), ALL_PATH_REJECTIONS.len());
}

/// The envelope carries the rule, offers the file-picker action only when the
/// user can act, and is never retryable.
#[test]
fn the_path_envelope_offers_an_action_only_when_the_user_can_act() {
    let user_fixable = validate_relative(&PathPolicy::default(), "../escape").unwrap_err();
    assert_eq!(user_fixable.reason(), PathRejection::ParentTraversal);
    let parts = user_fixable.envelope_parts();
    assert_eq!(parts.code.as_str(), "PATH_REJECTED");
    assert!(!parts.retryable);
    assert_eq!(
        parts.action.map(plaudon_security::ErrorAction::as_str),
        Some("CHOOSE_DIFFERENT_FILE")
    );
    assert!(parts.message.contains("parent_traversal"));

    // An empty policy permits nothing, and that is our failure, not the user's.
    let internal =
        canonicalize_and_validate(&PathPolicy::default(), Path::new("anything")).unwrap_err();
    assert_eq!(internal.reason(), PathRejection::NoPermittedRoot);
    assert_eq!(internal.detail(), None);
    assert_eq!(internal.to_string(), "path rejected: no_permitted_root");
    assert_eq!(internal.envelope_parts().action, None);
    assert!(PathPolicy::default().base().is_none());
}

/// Rules that carry an offending component render it escaped and in
/// parentheses; rules that carry a measurement render that instead.
#[test]
fn a_rejection_detail_is_either_an_escaped_component_or_a_measurement() {
    let component = check_component("bad\u{202E}name", DEFAULT_MAX_COMPONENT_LEN).unwrap_err();
    assert_eq!(component.reason(), PathRejection::BidiControl);
    let detail = component.detail().expect("the component is carried");
    assert!(detail.contains("\\u{202E}"), "{detail}");
    assert!(!detail.contains('\u{202E}'), "the raw control survived");
    assert_eq!(
        component.to_string(),
        format!("path rejected: bidi_or_invisible_character ({detail})")
    );

    let too_long = check_component(&"a".repeat(300), DEFAULT_MAX_COMPONENT_LEN).unwrap_err();
    assert_eq!(too_long.reason(), PathRejection::ComponentTooLong);
    assert_eq!(too_long.detail(), Some("300 bytes"));
}

/// Every name rule, each asserted by its own variant, at the boundary where it
/// starts to fire.
#[test]
fn the_component_checker_names_the_exact_rule_for_each_hostile_shape() {
    let cases: [(&str, PathRejection); 14] = [
        ("", PathRejection::Empty),
        (".", PathRejection::CurrentDirSegment),
        ("..", PathRejection::ParentTraversal),
        ("a\0b", PathRejection::NulByte),
        ("a\tb", PathRejection::ControlCharacter),
        ("a\u{200B}b", PathRejection::BidiControl),
        ("stream:$DATA", PathRejection::AlternateDataStream),
        ("a/b", PathRejection::ReservedCharacter),
        ("a\\b", PathRejection::ReservedCharacter),
        ("a*b", PathRejection::ReservedCharacter),
        ("NUL.txt", PathRejection::WindowsDeviceName),
        ("evil.exe.", PathRejection::TrailingDotOrSpace),
        ("evil.exe ", PathRejection::TrailingDotOrSpace),
        (" invoice.pdf", PathRejection::LeadingSpace),
    ];
    for (name, expected) in cases {
        assert_eq!(
            check_component(name, DEFAULT_MAX_COMPONENT_LEN)
                .unwrap_err()
                .reason(),
            expected,
            "wrong rule for {name:?}"
        );
    }

    for ch in WINDOWS_RESERVED_CHARS {
        let name = format!("a{ch}b");
        let reason = check_component(&name, DEFAULT_MAX_COMPONENT_LEN)
            .unwrap_err()
            .reason();
        let expected = if ch == ':' {
            PathRejection::AlternateDataStream
        } else {
            PathRejection::ReservedCharacter
        };
        assert_eq!(reason, expected, "wrong rule for {name}");
    }

    // The component cap is exact: at the limit it passes, one byte over it does not.
    assert!(check_component(&"a".repeat(8), 8).is_ok());
    assert_eq!(
        check_component(&"a".repeat(9), 8).unwrap_err().reason(),
        PathRejection::ComponentTooLong
    );
    assert!(check_component("ordinary name.wav", DEFAULT_MAX_COMPONENT_LEN).is_ok());
}

/// The device-name rule matches the stem case-insensitively with any extension
/// and with trailing blanks, and stops at names that are merely similar.
#[test]
fn the_windows_device_name_rule_matches_the_stem_and_nothing_wider() {
    for device in WINDOWS_DEVICE_NAMES {
        for candidate in [
            device.to_owned(),
            device.to_lowercase(),
            format!("{device}.txt"),
            format!("{device} "),
            format!("{device}\t"),
            format!("{}.tar.gz", device.to_lowercase()),
        ] {
            assert!(
                is_windows_device_name(&candidate),
                "{candidate:?} must be reserved"
            );
        }
    }

    for safe in [
        "CONSOLE",
        "CONS",
        "NULL",
        "COM10",
        "LPT",
        "",
        ".txt",
        "a.CON",
        "PRNX",
        "AUXILIARY",
    ] {
        assert!(
            !is_windows_device_name(safe),
            "{safe:?} must not be reserved"
        );
    }
}

/// The invisible/bidi set covers the spoofing characters and deliberately
/// excludes the emoji variation selector, so `🎙️` survives a filename.
#[test]
fn the_invisible_character_set_covers_the_spoofers_but_not_the_emoji_selector() {
    for ch in [
        '\u{00AD}', '\u{061C}', '\u{180E}', '\u{200B}', '\u{200D}', '\u{200F}', '\u{202A}',
        '\u{202E}', '\u{2060}', '\u{2064}', '\u{2066}', '\u{2069}', '\u{FEFF}', '\u{FFF9}',
        '\u{FFFB}',
    ] {
        assert!(
            is_invisible_or_bidi(ch),
            "U+{:04X} must be refused",
            ch as u32
        );
    }
    for ch in ['\u{FE0F}', 'a', '\u{00E9}', '\u{1F399}', ' ', '\u{3000}'] {
        assert!(
            !is_invisible_or_bidi(ch),
            "U+{:04X} must survive",
            ch as u32
        );
    }
    assert!(sanitize_filename("mic \u{1F399}\u{FE0F}.wav")
        .on_disk()
        .contains('\u{FE0F}'));
}

/// The escaper renders control and invisible characters as `\u{…}`, doubles a
/// backslash so the escape is unambiguous, and truncates on a character
/// boundary with an ellipsis.
#[test]
fn the_message_escaper_is_unambiguous_and_bounded() {
    assert_eq!(escape_for_message("plain", 40), "plain");
    assert_eq!(escape_for_message("a\\b", 40), "a\\\\b");
    assert_eq!(escape_for_message("a\nb", 40), "a\\u{000A}b");
    assert_eq!(escape_for_message("a\u{202E}b", 40), "a\\u{202E}b");
    assert_eq!(escape_for_message("", 40), "");
    assert_eq!(escape_for_message("abcdef", 3), "abc…");
    assert_eq!(escape_for_message("abc", 3), "abc", "the cap is inclusive");
    // Truncation counts characters, not bytes, and never splits one.
    assert_eq!(escape_for_message("ααααα", 2), "αα…");
}

/// Relative validation accepts a normalised relative path and refuses every
/// absolute form, including the ones that are only absolute on another host.
#[test]
fn the_relative_validator_refuses_every_absolute_form() {
    let policy = PathPolicy::default();

    assert_eq!(
        validate_relative(&policy, "media//sha256/./ab/x.wav")
            .unwrap_err()
            .reason(),
        PathRejection::CurrentDirSegment,
        "a `.` segment fails closed rather than being normalised away"
    );
    assert_eq!(
        validate_relative(&policy, "media/sha256/ab/x.wav").unwrap(),
        PathBuf::from("media/sha256/ab/x.wav")
    );
    assert_eq!(
        validate_relative(&policy, "media\\sha256\\ab\\x.wav").unwrap(),
        PathBuf::from("media/sha256/ab/x.wav"),
        "a Windows-produced entry means the same thing here"
    );
    assert_eq!(
        validate_relative(&policy, "media//sha256///x.wav").unwrap(),
        PathBuf::from("media/sha256/x.wav"),
        "doubled separators collapse"
    );

    let absolute: [(&str, PathRejection); 7] = [
        ("/etc/passwd", PathRejection::AbsolutePath),
        ("\\windows\\system32", PathRejection::AbsolutePath),
        ("C:/Windows", PathRejection::AbsolutePath),
        ("c:", PathRejection::AbsolutePath),
        ("\\\\server\\share\\x", PathRejection::UncPath),
        ("//server/share/x", PathRejection::UncPath),
        ("\\\\?\\C:\\x", PathRejection::DeviceNamespace),
    ];
    for (raw, expected) in absolute {
        assert_eq!(
            validate_relative(&policy, raw).unwrap_err().reason(),
            expected,
            "wrong rule for {raw}"
        );
    }
    assert_eq!(
        validate_relative(&policy, "\\\\.\\PhysicalDrive0")
            .unwrap_err()
            .reason(),
        PathRejection::DeviceNamespace
    );
    assert_eq!(
        validate_relative(&policy, "//?/C:/x").unwrap_err().reason(),
        PathRejection::DeviceNamespace
    );
    assert_eq!(
        validate_relative(&policy, "").unwrap_err().reason(),
        PathRejection::Empty
    );
    assert_eq!(
        validate_relative(&policy, "..").unwrap_err().reason(),
        PathRejection::ParentTraversal
    );
}

/// The two length caps are independent, and the legacy-Windows cap only applies
/// when the policy asks for it.
#[test]
fn the_path_length_caps_fire_at_their_own_boundaries() {
    let base = PathPolicy::default();
    let name = "a".repeat(100);

    let short = base.clone().with_max_path_len(10);
    assert_eq!(
        validate_relative(&short, &name).unwrap_err().reason(),
        PathRejection::PathTooLong
    );
    assert_eq!(
        validate_relative(&short, &name)
            .unwrap_err()
            .detail()
            .unwrap(),
        "100 bytes"
    );

    let narrow = base.clone().with_max_component_len(10);
    assert_eq!(
        validate_relative(&narrow, &name).unwrap_err().reason(),
        PathRejection::ComponentTooLong
    );
    assert!(validate_relative(&narrow, "short/also-short").is_ok());

    let portable = base.with_windows_short_path_compatible(true);
    let long = format!("{}/{}", "d".repeat(130), "f".repeat(130));
    assert!(long.len() > WINDOWS_SHORT_PATH_LIMIT);
    assert_eq!(
        validate_relative(&portable, &long).unwrap_err().reason(),
        PathRejection::NotWindowsShortPathCompatible
    );
    assert!(
        validate_relative(&portable, "media/x.wav").is_ok(),
        "a short path is still portable"
    );
}

/// A candidate resolved against real roots: outside, drive-qualified and
/// root-relative forms all report their own rule.
#[test]
fn a_candidate_outside_the_roots_reports_the_rule_that_refused_it() {
    let root = tempfile::tempdir().expect("temp dir");
    let canonical = std::fs::canonicalize(root.path()).expect("canonical root");
    let policy = PathPolicy::for_roots([canonical.clone()]);
    assert_eq!(policy.base(), Some(canonical.as_path()));

    assert_eq!(
        canonicalize_and_validate(&policy, Path::new("/etc/hosts"))
            .unwrap_err()
            .reason(),
        PathRejection::OutsideRoot
    );
    // `C:\…` cannot be resolved against a host that has no drives.
    assert_eq!(
        canonicalize_and_validate(&policy, Path::new("C:\\Users\\a\\x.wav"))
            .unwrap_err()
            .reason(),
        if cfg!(windows) {
            PathRejection::OutsideRoot
        } else {
            PathRejection::ForeignAbsolutePath
        }
    );
    assert_eq!(
        canonicalize_and_validate(&policy, Path::new("../../etc/hosts"))
            .unwrap_err()
            .reason(),
        PathRejection::ParentTraversal
    );

    // A relative candidate resolves against the first root, not the cwd.
    let accepted = canonicalize_and_validate(&policy, Path::new("staged/out.pcm")).unwrap();
    assert!(accepted.as_path().starts_with(&canonical));
    assert_eq!(
        accepted.file_name().and_then(std::ffi::OsStr::to_str),
        Some("out.pcm")
    );
    assert_eq!(accepted.parent(), Some(canonical.join("staged").as_path()));
    let borrowed: &Path = accepted.as_ref();
    assert_eq!(borrowed, accepted.as_path());
    let owned = accepted.clone().into_path_buf();
    assert_eq!(owned, accepted.as_path());
}

/// A non-UTF-8 candidate is refused before anything touches the disk, because
/// the managed archive stores UTF-8 names only.
#[cfg(unix)]
#[test]
fn a_non_utf8_candidate_is_refused_without_touching_the_filesystem() {
    use std::os::unix::ffi::OsStrExt;

    let root = tempfile::tempdir().expect("temp dir");
    let policy = PathPolicy::for_roots([root.path().to_path_buf()]);
    let raw = std::ffi::OsStr::from_bytes(b"inva\xFFlid.wav");
    assert_eq!(
        canonicalize_and_validate(&policy, Path::new(raw))
            .unwrap_err()
            .reason(),
        PathRejection::NonUtf8
    );
    assert_eq!(
        std::fs::read_dir(root.path()).unwrap().count(),
        0,
        "nothing was created"
    );
}

// ---------------------------------------------------------------------------
// Archive entries
// ---------------------------------------------------------------------------

/// Every entry kind reports a distinct label, and only files and directories
/// are ever extractable.
#[test]
fn every_archive_entry_kind_is_labelled_and_only_two_are_extractable() {
    let kinds = [
        ArchiveEntryKind::File,
        ArchiveEntryKind::Directory,
        ArchiveEntryKind::Symlink,
        ArchiveEntryKind::HardLink,
        ArchiveEntryKind::CharacterDevice,
        ArchiveEntryKind::BlockDevice,
        ArchiveEntryKind::Fifo,
        ArchiveEntryKind::Socket,
        ArchiveEntryKind::Unknown,
    ];
    let mut labels = BTreeSet::new();
    for kind in kinds {
        assert!(labels.insert(kind.label()));
        assert_eq!(format!("{kind}"), kind.label());
        assert_eq!(
            kind.is_extractable(),
            matches!(kind, ArchiveEntryKind::File | ArchiveEntryKind::Directory),
            "{kind}"
        );
    }
    assert_eq!(labels.len(), kinds.len());
}

/// Every archive rejection has a distinct rule name.
#[test]
fn every_archive_rejection_has_a_distinct_rule() {
    let all = [
        ArchiveRejection::LinkEntry,
        ArchiveRejection::SpecialEntry,
        ArchiveRejection::AbsolutePath,
        ArchiveRejection::Traversal,
        ArchiveRejection::EscapesDestination,
        ArchiveRejection::DuplicatePath,
        ArchiveRejection::UnsupportedName,
        ArchiveRejection::EntryTooLarge,
        ArchiveRejection::ArchiveTooLarge,
        ArchiveRejection::TooManyEntries,
        ArchiveRejection::CompressionRatioExceeded,
    ];
    let mut rules = BTreeSet::new();
    for rejection in all {
        assert!(rules.insert(rejection.rule()));
        assert_eq!(format!("{rejection}"), rejection.rule());
    }
    assert_eq!(rules.len(), all.len());
}

/// Every non-extractable kind is refused by the rule that names what it is: a
/// link entry is a link, everything else is a special entry.
#[test]
fn a_link_entry_and_a_special_entry_are_refused_by_their_own_rules() {
    let mut policy = ArchivePolicy::new("/managed/restore");
    assert_eq!(policy.destination_root(), Path::new("/managed/restore"));

    for kind in [ArchiveEntryKind::Symlink, ArchiveEntryKind::HardLink] {
        let entry = ArchiveEntryMeta::of_kind("notes/link", kind);
        let error = validate_archive_entry(&entry, &mut policy).unwrap_err();
        assert_eq!(error.reason(), ArchiveRejection::LinkEntry);
        assert_eq!(error.kind(), kind);
        assert_eq!(error.name_rule(), None);
        assert_eq!(error.label(), "notes/link");
    }

    for kind in [
        ArchiveEntryKind::CharacterDevice,
        ArchiveEntryKind::BlockDevice,
        ArchiveEntryKind::Fifo,
        ArchiveEntryKind::Socket,
        ArchiveEntryKind::Unknown,
    ] {
        let entry = ArchiveEntryMeta::of_kind("dev/node", kind);
        let error = validate_archive_entry(&entry, &mut policy).unwrap_err();
        assert_eq!(error.reason(), ArchiveRejection::SpecialEntry, "{kind}");
        assert_eq!(error.kind(), kind);
    }

    assert_eq!(policy.accepted_entries(), 0);
    assert_eq!(policy.accepted_bytes(), 0);
}

/// A name-rule failure is translated into the archive vocabulary, keeping the
/// underlying path rule for diagnostics, and the rendered sentence names both.
#[test]
fn a_rejected_entry_name_keeps_both_the_archive_rule_and_the_path_rule() {
    let cases: [(&str, ArchiveRejection, PathRejection); 8] = [
        (
            "../../etc/passwd",
            ArchiveRejection::Traversal,
            PathRejection::ParentTraversal,
        ),
        (
            "/etc/passwd",
            ArchiveRejection::AbsolutePath,
            PathRejection::AbsolutePath,
        ),
        (
            "\\\\server\\share\\x",
            ArchiveRejection::AbsolutePath,
            PathRejection::UncPath,
        ),
        (
            "\\\\?\\C:\\x",
            ArchiveRejection::AbsolutePath,
            PathRejection::DeviceNamespace,
        ),
        (
            "notes/NUL.txt",
            ArchiveRejection::UnsupportedName,
            PathRejection::WindowsDeviceName,
        ),
        (
            "notes/a\0b",
            ArchiveRejection::UnsupportedName,
            PathRejection::NulByte,
        ),
        (
            "notes/trailing. ",
            ArchiveRejection::UnsupportedName,
            PathRejection::TrailingDotOrSpace,
        ),
        (
            "notes/./x",
            ArchiveRejection::UnsupportedName,
            PathRejection::CurrentDirSegment,
        ),
    ];

    let mut policy = ArchivePolicy::new("/managed/restore");
    for (path, archive_rule, path_rule) in cases {
        let entry = ArchiveEntryMeta::file(path, 1, 1);
        let error = validate_archive_entry(&entry, &mut policy).unwrap_err();
        assert_eq!(
            error.reason(),
            archive_rule,
            "wrong archive rule for {path}"
        );
        assert_eq!(error.name_rule(), Some(path_rule), "for {path}");
        let rendered = error.to_string();
        assert!(rendered.contains(archive_rule.rule()), "{rendered}");
        assert!(rendered.contains(path_rule.rule()), "{rendered}");

        let parts = error.envelope_parts();
        assert_eq!(parts.code.as_str(), "ARCHIVE_ENTRY_REJECTED");
        assert!(!parts.retryable);
        assert_eq!(
            parts.action.map(plaudon_security::ErrorAction::as_str),
            Some("CHOOSE_DIFFERENT_FILE")
        );
    }

    // A refusal that is not about the name carries no path rule and still renders.
    let error = validate_archive_entry(
        &ArchiveEntryMeta::of_kind("x", ArchiveEntryKind::Fifo),
        &mut policy,
    )
    .unwrap_err();
    assert_eq!(error.name_rule(), None);
    assert_eq!(
        error.to_string(),
        "archive entry rejected: special_entry (fifo entry \"x\")"
    );
}

/// Each cap is checked at its exact boundary, and the running totals only
/// advance for entries that were accepted.
#[test]
fn every_archive_cap_fires_one_unit_past_its_boundary() {
    // Entry size.
    let mut policy = ArchivePolicy::new("/r").with_max_entry_bytes(100);
    assert!(validate_archive_entry(&ArchiveEntryMeta::file("a", 100, 100), &mut policy).is_ok());
    assert_eq!(policy.accepted_bytes(), 100);
    assert_eq!(
        validate_archive_entry(&ArchiveEntryMeta::file("b", 101, 101), &mut policy)
            .unwrap_err()
            .reason(),
        ArchiveRejection::EntryTooLarge
    );
    assert_eq!(policy.accepted_bytes(), 100, "a refusal adds nothing");
    assert_eq!(policy.accepted_entries(), 1);

    // Archive total.
    let mut policy = ArchivePolicy::new("/r").with_max_total_bytes(100);
    assert!(validate_archive_entry(&ArchiveEntryMeta::file("a", 60, 60), &mut policy).is_ok());
    assert!(validate_archive_entry(&ArchiveEntryMeta::file("b", 40, 40), &mut policy).is_ok());
    assert_eq!(policy.accepted_bytes(), 100);
    assert_eq!(
        validate_archive_entry(&ArchiveEntryMeta::file("c", 1, 1), &mut policy)
            .unwrap_err()
            .reason(),
        ArchiveRejection::ArchiveTooLarge
    );

    // Entry count. A directory counts too, because it still occupies a name.
    let mut policy = ArchivePolicy::new("/r").with_max_entries(2);
    assert!(validate_archive_entry(&ArchiveEntryMeta::directory("d"), &mut policy).is_ok());
    assert!(validate_archive_entry(&ArchiveEntryMeta::file("d/a", 1, 1), &mut policy).is_ok());
    assert_eq!(
        validate_archive_entry(&ArchiveEntryMeta::file("d/b", 1, 1), &mut policy)
            .unwrap_err()
            .reason(),
        ArchiveRejection::TooManyEntries
    );

    // A zero-entry archive admits nothing at all.
    let mut none = ArchivePolicy::new("/r").with_max_entries(0);
    assert_eq!(
        validate_archive_entry(&ArchiveEntryMeta::file("a", 1, 1), &mut none)
            .unwrap_err()
            .reason(),
        ArchiveRejection::TooManyEntries
    );
}

/// The compression-ratio check only applies above the minimum size, and fires
/// one unit past the ratio cap; a zero compressed size is treated as one so the
/// division cannot panic.
#[test]
fn the_compression_ratio_check_has_a_floor_and_an_exact_cap() {
    let mut policy = ArchivePolicy::new("/r")
        .with_max_compression_ratio(10)
        .with_ratio_check_min_bytes(1000);

    // Below the floor, a wild ratio is not even measured.
    assert!(validate_archive_entry(&ArchiveEntryMeta::file("small", 999, 1), &mut policy).is_ok());

    // At the floor, the ratio is measured. 10:1 is the cap and passes.
    assert!(validate_archive_entry(&ArchiveEntryMeta::file("at", 1000, 100), &mut policy).is_ok());
    // 11:1 does not.
    assert_eq!(
        validate_archive_entry(&ArchiveEntryMeta::file("over", 1100, 100), &mut policy)
            .unwrap_err()
            .reason(),
        ArchiveRejection::CompressionRatioExceeded
    );
    // A declared zero compressed size is the worst case and is refused, not divided by zero.
    assert_eq!(
        validate_archive_entry(&ArchiveEntryMeta::file("bomb", 1_000_000, 0), &mut policy)
            .unwrap_err()
            .reason(),
        ArchiveRejection::CompressionRatioExceeded
    );
    // A directory is never size- or ratio-checked.
    assert!(validate_archive_entry(&ArchiveEntryMeta::directory("dir"), &mut policy).is_ok());
}

/// Two entries that would collide on a case-insensitive or NFC-folding
/// filesystem are a duplicate, and the key is what says so.
#[test]
fn duplicate_detection_folds_case_and_unicode_composition() {
    let mut policy = ArchivePolicy::new("/r");
    let first = validate_archive_entry(&ArchiveEntryMeta::file("Notes/Café.md", 1, 1), &mut policy)
        .unwrap();
    assert_eq!(first.relative_path(), Path::new("Notes/Café.md"));
    assert_eq!(first.kind(), ArchiveEntryKind::File);
    assert_eq!(first.declared_uncompressed_size(), 1);
    assert_eq!(first.duplicate_key(), "notes/café.md");

    for colliding in ["notes/café.md", "NOTES/CAFÉ.MD", "Notes/Cafe\u{301}.md"] {
        assert_eq!(
            validate_archive_entry(&ArchiveEntryMeta::file(colliding, 1, 1), &mut policy)
                .unwrap_err()
                .reason(),
            ArchiveRejection::DuplicatePath,
            "{colliding} must collide"
        );
    }
    assert_eq!(policy.accepted_entries(), 1);

    // A genuinely different name is accepted.
    assert!(
        validate_archive_entry(&ArchiveEntryMeta::file("Notes/Other.md", 1, 1), &mut policy)
            .is_ok()
    );
    assert_eq!(policy.accepted_entries(), 2);
}

/// An entry whose *expanded destination* breaks a length budget is refused as
/// an unsupported name, carrying the path-too-long rule.
#[test]
fn an_entry_that_expands_past_the_destination_budget_is_refused() {
    let root = "/managed/restore";
    let mut policy = ArchivePolicy::new(root).with_max_path_len(root.len() + 8);
    assert!(validate_archive_entry(&ArchiveEntryMeta::file("short", 1, 1), &mut policy).is_ok());

    let error = validate_archive_entry(
        &ArchiveEntryMeta::file("much-longer-name", 1, 1),
        &mut policy,
    )
    .unwrap_err();
    assert_eq!(error.reason(), ArchiveRejection::UnsupportedName);
    assert_eq!(error.name_rule(), Some(PathRejection::PathTooLong));

    let mut portable = ArchivePolicy::new(root).with_windows_short_path_compatible(true);
    let long = format!("{}/{}", "d".repeat(130), "f".repeat(130));
    assert!(long.len() > WINDOWS_SHORT_PATH_LIMIT);
    let error =
        validate_archive_entry(&ArchiveEntryMeta::file(&long, 1, 1), &mut portable).unwrap_err();
    assert_eq!(error.reason(), ArchiveRejection::UnsupportedName);
    assert_eq!(
        error.name_rule(),
        Some(PathRejection::NotWindowsShortPathCompatible)
    );
}

/// The label an archive error carries is escaped and truncated, so a hostile
/// entry name cannot inject control characters into a log line.
#[test]
fn an_archive_error_label_is_escaped_and_bounded() {
    let mut policy = ArchivePolicy::new("/r");
    let hostile = format!("notes/{}\u{202E}gnp.exe", "a".repeat(80));
    let error = validate_archive_entry(
        &ArchiveEntryMeta::of_kind(hostile, ArchiveEntryKind::Symlink),
        &mut policy,
    )
    .unwrap_err();
    let label = error.label();
    assert!(!label.contains('\u{202E}'), "{label}");
    assert!(label.chars().count() <= 49, "{label}");
    assert!(label.ends_with('…'), "{label}");
    assert!(!error.to_string().contains('\u{202E}'));
}

/// An accepted entry resolves against the real destination immediately before
/// the write, which is the check that catches a symlink planted afterwards.
#[test]
fn an_accepted_entry_resolves_against_the_real_destination() {
    let root = tempfile::tempdir().expect("temp dir");
    let canonical = std::fs::canonicalize(root.path()).expect("canonical");
    let mut policy = ArchivePolicy::new(canonical.clone());

    let entry = validate_archive_entry(&ArchiveEntryMeta::file("notes.md", 10, 10), &mut policy)
        .expect("a plain name is accepted");
    let resolved = entry.resolve_target(&policy).expect("resolves");
    assert_eq!(resolved.as_path(), canonical.join("notes.md"));

    // A nested entry whose parent does not exist yet still resolves lexically.
    let nested = validate_archive_entry(&ArchiveEntryMeta::file("a/b/c.md", 1, 1), &mut policy)
        .expect("accepted");
    assert_eq!(
        nested.resolve_target(&policy).unwrap().as_path(),
        canonical.join("a/b/c.md")
    );
}

// ---------------------------------------------------------------------------
// Filename sanitisation: the shapes that have no usable extension
// ---------------------------------------------------------------------------

/// The extension is only kept when the tail is a short, purely alphanumeric
/// run after a non-empty stem; everything else is part of the stem.
#[test]
fn only_a_short_alphanumeric_tail_counts_as_an_extension() {
    let cases: [(&str, Option<&str>); 8] = [
        ("recording.wav", Some("wav")),
        ("archive.tar.gz", Some("gz")),
        ("UPPER.WAV", Some("WAV")),
        ("noextension", None),
        (".hidden", None),
        ("name.v1-2", None),
        ("name.a_b", None),
        ("name.averylongextensionindeed", None),
    ];
    for (raw, expected) in cases {
        assert_eq!(
            sanitize_filename(raw).extension(),
            expected,
            "wrong extension for {raw:?}"
        );
    }

    // A trailing dot is stripped before the extension is read, so `file.` has
    // no extension rather than an empty one.
    let trimmed = sanitize_filename("file.");
    assert_eq!(trimmed.on_disk(), "file");
    assert_eq!(trimmed.extension(), None);
    assert!(trimmed.changed(), "the trailing dot was removed");
}

/// `changed()` distinguishes a name we rewrote from one we passed through, so
/// the UI can say "stored as" only when it actually differs.
#[test]
fn a_name_reports_whether_it_was_rewritten() {
    let untouched = sanitize_filename("Quarterly review.m4a");
    assert_eq!(untouched.on_disk(), "Quarterly review.m4a");
    assert_eq!(untouched.display(), "Quarterly review.m4a");
    assert!(!untouched.changed());
    assert!(!untouched.mixed_scripts());

    for hostile in [
        "bad/name.wav",
        "col:on.wav",
        "trailing ",
        "\u{202E}gnp.exe",
        "two  spaces.wav",
    ] {
        assert!(
            sanitize_filename(hostile).changed(),
            "{hostile:?} should be reported as rewritten"
        );
    }
}

/// A name that sanitises to nothing usable gets the deterministic
/// content-addressed fallback, keeping any extension it had.
#[test]
fn a_name_that_sanitises_to_nothing_gets_a_deterministic_fallback() {
    for raw in ["   ", "\u{200B}\u{200B}", "\u{202E}", "..", "."] {
        let name = sanitize_filename(raw);
        assert!(
            name.on_disk().starts_with("file-"),
            "{raw:?} produced {:?}",
            name.on_disk()
        );
        assert_eq!(
            name.on_disk(),
            sanitize_filename(raw).on_disk(),
            "the fallback must be deterministic for {raw:?}"
        );
        assert!(!name.display().is_empty(), "{raw:?} has an empty display");
    }

    // Two different inputs that both sanitise to nothing do not collide.
    assert_ne!(
        sanitize_filename("   ").on_disk(),
        sanitize_filename("\u{200B}").on_disk()
    );

    // A reserved device name is replaced wholesale, extension kept.
    let device = sanitize_filename("CON.wav");
    assert!(device.on_disk().starts_with("file-"));
    assert_eq!(device.extension(), Some("wav"));
    assert!(!is_windows_device_name(device.on_disk()));
}

/// Truncation respects the on-disk byte budget while keeping the extension and
/// never splitting a UTF-8 sequence.
#[test]
fn an_over_long_name_is_truncated_without_losing_its_extension() {
    let long = format!("{}.wav", "x".repeat(400));
    let name = sanitize_filename(&long);
    assert!(name.on_disk().len() <= 200, "{}", name.on_disk().len());
    assert_eq!(
        Path::new(name.on_disk())
            .extension()
            .and_then(std::ffi::OsStr::to_str),
        Some("wav")
    );
    assert_eq!(name.extension(), Some("wav"));
    assert!(name.changed());

    // Multi-byte characters survive truncation intact.
    let wide = format!("{}.m4a", "é".repeat(400));
    let name = sanitize_filename(&wide);
    assert!(name.on_disk().len() <= 200);
    assert!(std::str::from_utf8(name.on_disk().as_bytes()).is_ok());
    assert_eq!(
        Path::new(name.on_disk())
            .extension()
            .and_then(std::ffi::OsStr::to_str),
        Some("m4a")
    );

    // The display form is bounded independently and is never empty.
    assert!(
        name.display().chars().count() <= 121,
        "{}",
        name.display().chars().count()
    );
    assert!(!name.display().is_empty(), "display must never be empty");
}
