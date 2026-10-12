//! D2.2 unit tests. Hermetic: real temp directories, no network, no device.

use std::collections::BTreeSet;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use tempfile::TempDir;

use super::archive::{
    validate_archive_entry, ArchiveEntryKind, ArchiveEntryMeta, ArchivePolicy, ArchiveRejection,
};
use super::perms::require_same_volume;
use super::*;
use crate::error::{SecurityError, ARCHIVE_ENTRY_REJECTED, CHOOSE_DIFFERENT_FILE, PATH_REJECTED};

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn temp_root() -> TempDir {
    tempfile::tempdir().expect("temp dir")
}

fn croot(dir: &TempDir) -> PathBuf {
    fs::canonicalize(dir.path()).expect("canonical root")
}

fn policy_for(dir: &TempDir) -> PathPolicy {
    PathPolicy::for_roots([dir.path().to_path_buf()])
}

fn write_file(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("parent");
    }
    fs::write(path, contents).expect("write");
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn entry_count(dir: &Path) -> usize {
    let mut total = 0;
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    for entry in entries.flatten() {
        total += 1;
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            total += entry_count(&entry.path());
        }
    }
    total
}

// ---------------------------------------------------------------------------
// NFC conformance
// ---------------------------------------------------------------------------

/// Bases and marks mirrored exactly by the generator that produced
/// [`NFC_SEQUENCE_DIGEST`]; changing either list invalidates the digest.
const NFC_BASES: [u32; 24] = [
    0x0041, 0x0061, 0x0043, 0x0063, 0x00C5, 0x00E5, 0x01B7, 0x0272, 0x0391, 0x03B1, 0x0410, 0x0430,
    0x05D0, 0x0627, 0x0915, 0x0E01, 0x1E9B, 0x1F00, 0x3042, 0x4E00, 0xAC00, 0x1100, 0x1161, 0x11A8,
];
const NFC_MARKS: [u32; 16] = [
    0x0300, 0x0301, 0x0302, 0x0307, 0x0308, 0x031B, 0x0323, 0x0327, 0x0328, 0x0344, 0x0345, 0x05B0,
    0x0653, 0x093C, 0x0E38, 0x1161,
];

/// SHA-256 of `"\n".join(NFC(chr(cp)) for every non-surrogate scalar)`, taken
/// from Python's `unicodedata`, Unicode 16.0.0.
const NFC_SCALAR_DIGEST: &str = "900f12a0fd5b368b56e972d9dcc9cc0584951930f9d9724d079e44f36f332796";
/// SHA-256 of the same join over `base + mark1 + mark2` for every combination
/// of [`NFC_BASES`] and [`NFC_MARKS`], 6144 sequences.
const NFC_SEQUENCE_DIGEST: &str =
    "dc409203e6faa245b355d2cae27b34683125f403db43f6931ad8ec2c4e1a20d9";

fn digest_of_lines(lines: &[String]) -> String {
    sha256_hex(lines.join("\n").as_bytes())
}

#[test]
fn nfc_matches_unicode_for_every_scalar_value() {
    let lines: Vec<String> = (0..0x0011_0000u32)
        .filter_map(char::from_u32)
        .map(|ch| to_nfc(&ch.to_string()))
        .collect();
    assert_eq!(lines.len(), 0x0011_0000 - 0x0800);
    assert_eq!(digest_of_lines(&lines), NFC_SCALAR_DIGEST);
}

#[test]
fn nfc_matches_unicode_for_combining_sequences() {
    let mut lines = Vec::with_capacity(NFC_BASES.len() * NFC_MARKS.len() * NFC_MARKS.len());
    for base in NFC_BASES {
        for first in NFC_MARKS {
            for second in NFC_MARKS {
                let mut input = String::new();
                input.extend(char::from_u32(base));
                input.extend(char::from_u32(first));
                input.extend(char::from_u32(second));
                lines.push(to_nfc(&input));
            }
        }
    }
    assert_eq!(lines.len(), 6144);
    assert_eq!(digest_of_lines(&lines), NFC_SEQUENCE_DIGEST);
}

#[test]
fn nfc_known_vectors() {
    // Composition, singleton, reordering, Hangul, exclusions.
    let cases: [(&str, &str); 10] = [
        ("cafe\u{301}", "caf\u{E9}"),
        ("\u{212B}ngstrom", "\u{C5}ngstrom"),
        ("\u{1E9B}\u{323}", "\u{1E9B}\u{323}"),
        ("q\u{307}\u{323}", "q\u{323}\u{307}"),
        ("\u{1100}\u{1161}\u{11A8}", "\u{AC01}"),
        ("\u{AC00}\u{11A8}", "\u{AC01}"),
        ("A\u{328}\u{301}", "\u{104}\u{301}"),
        ("A\u{30A}", "\u{C5}"),
        ("\u{344}", "\u{308}\u{301}"),
        ("\u{1E0B}\u{323}", "\u{1E0D}\u{307}"),
    ];
    for (input, expected) in cases {
        assert_eq!(to_nfc(input), expected, "nfc({input:?})");
        assert_eq!(to_nfc(expected), expected, "idempotence for {expected:?}");
        assert!(is_nfc(expected));
    }
    assert!(!is_nfc("cafe\u{301}"));
}

// ---------------------------------------------------------------------------
// component rules
// ---------------------------------------------------------------------------

#[test]
fn component_rules_reject_each_hostile_shape() {
    let cases: [(&str, PathRejection); 18] = [
        ("", PathRejection::Empty),
        (".", PathRejection::CurrentDirSegment),
        ("..", PathRejection::ParentTraversal),
        ("nul\0byte.txt", PathRejection::NulByte),
        ("line\nbreak.txt", PathRejection::ControlCharacter),
        ("cr\rreturn.txt", PathRejection::ControlCharacter),
        ("del\u{7F}.txt", PathRejection::ControlCharacter),
        ("exploit\u{202E}gnp.exe", PathRejection::BidiControl),
        ("zero\u{200B}width.txt", PathRejection::BidiControl),
        ("bom\u{FEFF}file.txt", PathRejection::BidiControl),
        ("file.txt:hidden", PathRejection::AlternateDataStream),
        ("what?.txt", PathRejection::ReservedCharacter),
        ("a<b>c.txt", PathRejection::ReservedCharacter),
        ("back\\slash.txt", PathRejection::ReservedCharacter),
        ("CoN.TXT", PathRejection::WindowsDeviceName),
        ("com9.tar.gz", PathRejection::WindowsDeviceName),
        ("report.", PathRejection::TrailingDotOrSpace),
        ("report ", PathRejection::TrailingDotOrSpace),
    ];
    for (name, expected) in cases {
        let error = check_component(name, DEFAULT_MAX_COMPONENT_LEN)
            .expect_err(&format!("{name:?} must be rejected"));
        assert_eq!(error.reason(), expected, "{name:?}");
    }

    let long = "a".repeat(DEFAULT_MAX_COMPONENT_LEN + 1);
    assert_eq!(
        check_component(&long, DEFAULT_MAX_COMPONENT_LEN)
            .unwrap_err()
            .reason(),
        PathRejection::ComponentTooLong
    );

    for name in [
        "Meeting Notes.m4a",
        "r\u{E9}sum\u{E9}.pdf",
        "a\u{301}\u{302}.txt",
        "-rf.txt",
    ] {
        assert!(
            check_component(name, DEFAULT_MAX_COMPONENT_LEN).is_ok(),
            "{name:?}"
        );
    }
}

#[test]
fn every_windows_device_name_is_rejected_with_and_without_extension() {
    for device in WINDOWS_DEVICE_NAMES {
        for candidate in [
            device.to_owned(),
            format!("{device}.txt"),
            device.to_lowercase(),
        ] {
            assert!(is_windows_device_name(&candidate), "{candidate}");
            assert_eq!(
                check_component(&candidate, DEFAULT_MAX_COMPONENT_LEN)
                    .unwrap_err()
                    .reason(),
                PathRejection::WindowsDeviceName
            );
        }
    }
    // Near-misses must stay usable.
    for ok in [
        "CONTRACT.txt",
        "com.txt",
        "lpt.m4a",
        "null.json",
        "com10.txt",
    ] {
        assert!(!is_windows_device_name(ok), "{ok}");
    }
}

// ---------------------------------------------------------------------------
// canonicalisation and containment
// ---------------------------------------------------------------------------

#[test]
fn accepts_existing_file_inside_root() {
    let root = temp_root();
    let file = root.path().join("sub/recording.m4a");
    write_file(&file, "bytes");

    let validated = canonicalize_and_validate(&policy_for(&root), &file).expect("accepted");
    assert_eq!(validated.as_path(), croot(&root).join("sub/recording.m4a"));
    assert!(validated.as_path().starts_with(croot(&root)));
}

#[test]
fn accepts_staging_target_that_does_not_exist_yet() {
    let root = temp_root();
    fs::create_dir(root.path().join("media")).expect("mkdir");
    let target = root.path().join("media/not-created-yet.part");

    let validated = canonicalize_and_validate(&policy_for(&root), &target).expect("accepted");
    assert_eq!(
        validated.as_path(),
        croot(&root).join("media/not-created-yet.part")
    );
    assert!(!validated.as_path().exists());
}

#[test]
fn relative_candidate_resolves_against_the_first_root_not_the_process_cwd() {
    let root = temp_root();
    write_file(&root.path().join("inside.txt"), "x");

    let validated =
        canonicalize_and_validate(&policy_for(&root), Path::new("inside.txt")).expect("accepted");
    assert_eq!(validated.as_path(), croot(&root).join("inside.txt"));
}

#[test]
fn rejects_traversal_even_when_it_would_land_inside_the_root() {
    let root = temp_root();
    fs::create_dir(root.path().join("a")).expect("mkdir");
    write_file(&root.path().join("b.txt"), "x");

    // `<root>/a/../b.txt` resolves inside the root, and is still refused: the
    // rule is lexical and runs before anything touches the disk.
    let error = canonicalize_and_validate(&policy_for(&root), &root.path().join("a/../b.txt"))
        .expect_err("traversal");
    assert_eq!(error.reason(), PathRejection::ParentTraversal);
}

#[test]
fn rejects_paths_outside_every_root() {
    let root = temp_root();
    let outside = temp_root();
    write_file(&outside.path().join("secret.txt"), "x");

    let error = canonicalize_and_validate(&policy_for(&root), &outside.path().join("secret.txt"))
        .expect_err("outside");
    assert_eq!(error.reason(), PathRejection::OutsideRoot);
}

#[test]
fn rejects_empty_nul_device_namespace_and_unc() {
    let root = temp_root();
    let policy = policy_for(&root);
    let cases: [(&str, PathRejection); 5] = [
        ("", PathRejection::Empty),
        ("a\0b", PathRejection::NulByte),
        ("\\\\?\\C:\\x", PathRejection::DeviceNamespace),
        ("\\\\.\\COM1", PathRejection::DeviceNamespace),
        ("\\\\server\\share\\x", PathRejection::UncPath),
    ];
    for (raw, expected) in cases {
        let error =
            canonicalize_and_validate(&policy, Path::new(raw)).expect_err("must be rejected");
        assert_eq!(error.reason(), expected, "{raw:?}");
    }
}

#[test]
fn enforces_length_caps() {
    let root = temp_root();
    let policy = policy_for(&root);

    let long_component = "a".repeat(300);
    assert_eq!(
        canonicalize_and_validate(&policy, Path::new(&long_component))
            .unwrap_err()
            .reason(),
        PathRejection::ComponentTooLong
    );

    let long_path = vec!["b"; 2500].join("/");
    assert_eq!(long_path.len(), 4999);
    let tight = policy_for(&root).with_max_path_len(1024);
    assert_eq!(
        canonicalize_and_validate(&tight, Path::new(&long_path))
            .unwrap_err()
            .reason(),
        PathRejection::PathTooLong
    );

    let portable = policy_for(&root).with_windows_short_path_compatible(true);
    let medium = vec!["bb"; 100].join("/");
    assert_eq!(
        canonicalize_and_validate(&portable, Path::new(&medium))
            .unwrap_err()
            .reason(),
        PathRejection::NotWindowsShortPathCompatible
    );
}

#[test]
fn rejects_when_no_root_exists() {
    let missing = PathPolicy::for_roots([PathBuf::from("/plaudon-does-not-exist-9e3f")]);
    assert_eq!(
        canonicalize_and_validate(&missing, Path::new("x.txt"))
            .unwrap_err()
            .reason(),
        PathRejection::NoPermittedRoot
    );
}

#[cfg(unix)]
#[test]
fn symlink_escaping_the_root_is_rejected_and_its_target_is_not_opened() {
    let root = temp_root();
    let outside = temp_root();
    let secret = outside.path().join("secret.txt");
    write_file(&secret, "TOP-SECRET-CONTENT");
    // Unreadable: if validation ever opened the target instead of resolving it,
    // the failure would surface as a permission error rather than our rule.
    fs::set_permissions(
        &secret,
        <fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o000),
    )
    .expect("chmod");

    let link = root.path().join("innocent.m4a");
    std::os::unix::fs::symlink(&secret, &link).expect("symlink");

    let error = canonicalize_and_validate(&policy_for(&root), &link).expect_err("escape");
    assert_eq!(error.reason(), PathRejection::SymlinkEscapesRoot);

    let rendered = format!("{error}");
    assert!(!rendered.contains("TOP-SECRET-CONTENT"));
    assert!(!rendered.contains("secret.txt"));
    assert!(!rendered.contains(&outside.path().to_string_lossy().into_owned()));
}

#[cfg(unix)]
#[test]
fn symlink_pointing_inside_the_root_is_resolved_and_accepted() {
    let root = temp_root();
    let real = root.path().join("media/recording.m4a");
    write_file(&real, "bytes");
    let link = root.path().join("shortcut.m4a");
    std::os::unix::fs::symlink(&real, &link).expect("symlink");

    let validated = canonicalize_and_validate(&policy_for(&root), &link).expect("accepted");
    assert_eq!(
        validated.as_path(),
        croot(&root).join("media/recording.m4a")
    );
}

#[cfg(unix)]
#[test]
fn symlinked_directory_component_mid_path_is_resolved_and_contained() {
    let root = temp_root();
    let outside = temp_root();
    write_file(&root.path().join("real/inner/file.txt"), "x");
    write_file(&outside.path().join("inner/file.txt"), "x");

    std::os::unix::fs::symlink(root.path().join("real"), root.path().join("via")).expect("link");
    std::os::unix::fs::symlink(outside.path(), root.path().join("away")).expect("link");

    let inside =
        canonicalize_and_validate(&policy_for(&root), &root.path().join("via/inner/file.txt"))
            .expect("accepted");
    assert_eq!(inside.as_path(), croot(&root).join("real/inner/file.txt"));

    let error =
        canonicalize_and_validate(&policy_for(&root), &root.path().join("away/inner/file.txt"))
            .expect_err("escape");
    assert_eq!(error.reason(), PathRejection::SymlinkEscapesRoot);
}

#[cfg(unix)]
#[test]
fn dangling_symlink_is_not_treated_as_a_free_staging_target() {
    let root = temp_root();
    let link = root.path().join("staging.part");
    std::os::unix::fs::symlink(temp_root().path().join("gone.txt"), &link).expect("symlink");

    let error = canonicalize_and_validate(&policy_for(&root), &link).expect_err("dangling");
    assert_eq!(error.reason(), PathRejection::UnresolvableLink);
}

#[cfg(unix)]
#[test]
fn symlink_escape_is_allowed_only_with_the_explicit_opt_in() {
    let root = temp_root();
    let outside = temp_root();
    write_file(&outside.path().join("export.md"), "x");
    std::os::unix::fs::symlink(
        outside.path().join("export.md"),
        root.path().join("export.md"),
    )
    .expect("symlink");

    let opted_in = policy_for(&root).allowing_symlink_escape();
    let validated =
        canonicalize_and_validate(&opted_in, &root.path().join("export.md")).expect("accepted");
    assert_eq!(
        validated.as_path(),
        fs::canonicalize(outside.path().join("export.md")).unwrap()
    );
}

#[test]
fn every_accepted_path_resolves_inside_a_permitted_root() {
    const SEGMENTS: &[&str] = &[
        "sub",
        "deep",
        "ok.txt",
        "file.txt",
        "..",
        ".",
        "link_in",
        "link_out",
        "CON",
        "x:y",
        "trail.",
        "fi le",
        "e\u{301}",
        "%2e%2e%2f",
        "..\\..",
        "target.txt",
        "new.part",
    ];
    let root = temp_root();
    write_file(&root.path().join("sub/deep/file.txt"), "x");
    write_file(&root.path().join("ok.txt"), "x");
    let outside = temp_root();
    write_file(&outside.path().join("target.txt"), "x");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(root.path().join("sub"), root.path().join("link_in"))
            .expect("link");
        std::os::unix::fs::symlink(outside.path(), root.path().join("link_out")).expect("link");
    }

    let policy = policy_for(&root);
    let canonical_root = croot(&root);
    let mut accepted = 0usize;
    let mut rejected = 0usize;

    for a in SEGMENTS {
        for b in SEGMENTS {
            for c in SEGMENTS {
                for candidate in [format!("{a}/{b}"), format!("{a}/{b}/{c}")] {
                    match canonicalize_and_validate(&policy, Path::new(&candidate)) {
                        Ok(validated) => {
                            accepted += 1;
                            assert!(
                                validated.as_path().starts_with(&canonical_root),
                                "{candidate:?} resolved to {:?}",
                                validated.as_path()
                            );
                            assert!(
                                !validated
                                    .as_path()
                                    .components()
                                    .any(|part| part.as_os_str() == ".."),
                                "{candidate:?}"
                            );
                        }
                        Err(_) => rejected += 1,
                    }
                }
            }
        }
    }
    assert!(accepted > 20, "expected some acceptances, got {accepted}");
    assert!(
        rejected > accepted,
        "expected the hostile majority to be refused"
    );
}

#[test]
fn path_error_envelope_is_stable_and_never_echoes_the_path() {
    let root = temp_root();
    let hostile = root.path().join("exploit\u{202E}gnp.exe");
    let error = canonicalize_and_validate(&policy_for(&root), &hostile).expect_err("bidi");

    let parts = error.envelope_parts();
    assert_eq!(parts.code, PATH_REJECTED);
    assert_eq!(parts.action, Some(CHOOSE_DIFFERENT_FILE));
    assert!(!parts.retryable);
    assert!(parts.message.contains("bidi_or_invisible_character"));
    assert!(
        !parts.message.contains('\u{202E}'),
        "raw bidi override leaked into the message"
    );
    assert!(
        parts.message.contains("\\u{202E}"),
        "escaped form expected: {}",
        parts.message
    );
    assert!(
        !parts
            .message
            .contains(&root.path().to_string_lossy().into_owned()),
        "full path leaked: {}",
        parts.message
    );
}

#[test]
fn escape_for_message_truncates_and_escapes() {
    assert_eq!(escape_for_message("plain.txt", 40), "plain.txt");
    assert_eq!(escape_for_message("a\nb", 40), "a\\u{000A}b");
    assert_eq!(escape_for_message("c:\\x", 40), "c:\\\\x");
    assert_eq!(escape_for_message(&"z".repeat(50), 8), "zzzzzzzz…");
}

// ---------------------------------------------------------------------------
// filename sanitisation
// ---------------------------------------------------------------------------

#[test]
fn sanitize_filename_neutralises_hostile_names() {
    let cases: [(&str, &str); 12] = [
        (
            "Meeting Notes 2026-03-04.m4a",
            "Meeting Notes 2026-03-04.m4a",
        ),
        ("exploit\u{202E}gnp.exe", "exploitgnp.exe"),
        ("zero\u{200B}width.txt", "zerowidth.txt"),
        ("line\nbreak.txt", "line break.txt"),
        ("what?.txt", "what_.txt"),
        ("a<b>c.txt", "a_b_c.txt"),
        ("file.txt:hidden", "file.txt_hidden"),
        ("../../etc/passwd", ".._.._etc_passwd"),
        ("back\\slash.txt", "back_slash.txt"),
        ("report.   ", "report"),
        ("  spaced   out .txt", "spaced out .txt"),
        ("cafe\u{301}.txt", "caf\u{E9}.txt"),
    ];
    for (raw, expected) in cases {
        let sanitized = sanitize_filename(raw);
        assert_eq!(sanitized.on_disk(), expected, "{raw:?}");
        assert!(
            check_component(sanitized.on_disk(), DEFAULT_MAX_COMPONENT_LEN).is_ok(),
            "{raw:?} -> {:?} is not a valid component",
            sanitized.on_disk()
        );
    }
}

#[test]
fn sanitize_filename_falls_back_deterministically() {
    for raw in [
        "",
        "...",
        "..",
        "\u{202E}\u{200B}",
        "CON",
        "nul.mp3",
        "com1.tar.gz",
    ] {
        let first = sanitize_filename(raw);
        let second = sanitize_filename(raw);
        assert_eq!(
            first.on_disk(),
            second.on_disk(),
            "not deterministic for {raw:?}"
        );
        assert!(
            first.on_disk().starts_with("file-"),
            "{raw:?} -> {:?}",
            first.on_disk()
        );
        assert_ne!(first.on_disk().len(), 0);
        assert!(check_component(first.on_disk(), DEFAULT_MAX_COMPONENT_LEN).is_ok());
        assert!(!is_windows_device_name(first.on_disk()));
    }
    assert_eq!(sanitize_filename("nul.mp3").extension(), Some("mp3"));
    assert_ne!(
        sanitize_filename("CON").on_disk(),
        sanitize_filename("PRN").on_disk()
    );
}

#[test]
fn nfc_and_nfd_spellings_collapse_to_one_on_disk_name() {
    let composed = sanitize_filename("caf\u{E9}.txt");
    let decomposed = sanitize_filename("cafe\u{301}.txt");
    assert_eq!(composed.on_disk(), decomposed.on_disk());
    assert_eq!(composed.display(), decomposed.display());

    let angstrom_sign = sanitize_filename("\u{212B}ngstrom.txt");
    let angstrom_letter = sanitize_filename("\u{C5}ngstrom.txt");
    assert_eq!(angstrom_sign.on_disk(), angstrom_letter.on_disk());
}

#[test]
fn display_name_is_safe_but_keeps_the_users_characters() {
    let sanitized = sanitize_filename("Q3 review: budget? \u{202E}fdp.exe");
    assert!(!sanitized.display().contains('\u{202E}'));
    assert!(sanitized.display().contains("Q3 review: budget?"));
    // The display form may keep characters the on-disk form had to replace.
    assert!(sanitized.on_disk().contains('_'));

    for raw in ["line\nbreak", "tab\there", "\u{200B}\u{200D}", "\u{7F}"] {
        let sanitized = sanitize_filename(raw);
        assert!(!sanitized.display().is_empty(), "{raw:?}");
        assert!(
            !sanitized
                .display()
                .chars()
                .any(|ch| ch.is_control() || is_invisible_or_bidi(ch)),
            "{raw:?} -> {:?}",
            sanitized.display()
        );
    }
    assert!(
        sanitize_filename(&"x".repeat(5000))
            .display()
            .chars()
            .count()
            <= MAX_DISPLAY_CHARS
    );
}

#[test]
fn truncation_respects_the_byte_budget_and_utf8() {
    let long_ascii = format!("{}.m4a", "n".repeat(500));
    let sanitized = sanitize_filename(&long_ascii);
    assert!(sanitized.on_disk().len() <= MAX_ON_DISK_BYTES);
    assert!(
        sanitized.on_disk().len() > MAX_ON_DISK_BYTES - 8,
        "stem should fill the budget"
    );
    assert_eq!(sanitized.extension(), Some("m4a"));

    // Multi-byte characters must not be cut in half, and a combining mark must
    // not be separated from its base.
    let long_unicode = format!("{}.txt", "e\u{301}".repeat(400));
    let sanitized = sanitize_filename(&long_unicode);
    assert!(sanitized.on_disk().len() <= MAX_ON_DISK_BYTES);
    assert!(std::str::from_utf8(sanitized.on_disk().as_bytes()).is_ok());
    assert!(!sanitized.on_disk().starts_with('\u{301}'));
    assert_eq!(
        combining_class(sanitized.on_disk().chars().next().unwrap_or('a')),
        0
    );

    let emoji = format!("{}.m4a", "meeting \u{1F399}\u{FE0F} ".repeat(40));
    let sanitized = sanitize_filename(&emoji);
    assert!(sanitized.on_disk().len() <= MAX_ON_DISK_BYTES);
    assert!(check_component(sanitized.on_disk(), DEFAULT_MAX_COMPONENT_LEN).is_ok());
}

#[test]
fn mixed_script_names_are_flagged_not_rewritten() {
    let spoof = sanitize_filename("p\u{430}ypal-invoice.pdf");
    assert!(spoof.mixed_scripts());
    assert!(
        spoof.on_disk().contains('\u{430}'),
        "legitimate characters are preserved"
    );

    for honest in [
        "\u{41F}\u{440}\u{438}\u{432}\u{435}\u{442}.mp3",
        "r\u{E9}sum\u{E9}.pdf",
    ] {
        assert!(!sanitize_filename(honest).mixed_scripts(), "{honest:?}");
    }
}

// ---------------------------------------------------------------------------
// private temp directories
// ---------------------------------------------------------------------------

#[test]
fn private_temp_dir_is_owner_only_and_removed_on_drop() {
    let base = temp_root();
    let path = {
        let private = PrivateTempDir::create(Some(base.path()), "plaudon-test").expect("create");
        let path = private.path().to_path_buf();
        assert!(path.is_dir());
        assert!(path.starts_with(croot(&base)));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "staging directory must be owner-only");
        }
        assert_eq!(
            is_restrictive(&path),
            if cfg!(unix) { Some(true) } else { None }
        );
        path
    };
    assert!(!path.exists(), "staging directory must be removed on drop");
}

#[test]
fn two_staging_directories_never_collide() {
    let base = temp_root();
    let mut names = BTreeSet::new();
    let dirs: Vec<PrivateTempDir> = (0..8)
        .map(|_| PrivateTempDir::create(Some(base.path()), "plaudon-test").expect("create"))
        .collect();
    for dir in &dirs {
        names.insert(dir.path().to_path_buf());
    }
    assert_eq!(names.len(), dirs.len());
}

/// Build the mixed population `reconcile_abandoned` has to discriminate.
fn populate_staging_base(base: &Path, prefix: &str) -> StagingPopulation {
    let abandoned = base.join(format!("{prefix}-424242-{}", "ab12cd34".repeat(4)));
    fs::create_dir(&abandoned).expect("mkdir");
    write_file(&abandoned.join("leftover.part"), "x");
    let second = base.join(format!("{prefix}-424243-{}", "ef56ab78".repeat(4)));
    fs::create_dir(&second).expect("mkdir");

    let user_folder = base.join("My Recordings");
    fs::create_dir(&user_folder).expect("mkdir");
    let other_app = base.join(format!("someoneelse-1-{}", "ab12cd34".repeat(4)));
    fs::create_dir(&other_app).expect("mkdir");
    let wrong_shape = base.join(format!("{prefix}-notapid-{}", "ab12cd34".repeat(4)));
    fs::create_dir(&wrong_shape).expect("mkdir");
    let own = base.join(format!(
        "{prefix}-{}-{}",
        std::process::id(),
        "ab12cd34".repeat(4)
    ));
    fs::create_dir(&own).expect("mkdir");

    StagingPopulation {
        abandoned,
        second,
        user_folder,
        other_app,
        wrong_shape,
        own,
    }
}

struct StagingPopulation {
    abandoned: PathBuf,
    second: PathBuf,
    user_folder: PathBuf,
    other_app: PathBuf,
    wrong_shape: PathBuf,
    own: PathBuf,
}

impl StagingPopulation {
    fn assert_foreign_entries_survived(&self) {
        assert!(
            self.user_folder.exists(),
            "a user folder must never be touched"
        );
        assert!(
            self.other_app.exists(),
            "another app's directory must never be touched"
        );
        assert!(
            self.wrong_shape.exists(),
            "a non-matching name must never be touched"
        );
        assert!(
            self.own.exists(),
            "this process's own directory must never be removed"
        );
    }
}

#[test]
fn reconcile_abandoned_respects_the_grace_period() {
    let base = temp_root();
    let prefix = "plaudon-import";
    let population = populate_staging_base(base.path(), prefix);
    let now = now_unix();

    // Everything was created moments ago, so nothing is outside the grace
    // period yet.
    let report =
        PrivateTempDir::reconcile_abandoned(base.path(), prefix, 3600, now).expect("reconcile");
    assert_eq!(report.removed, 0);
    assert_eq!(report.skipped_recent, 2);
    assert_eq!(report.skipped_own, 1);
    assert_eq!(report.skipped_foreign, 3);
    assert!(population.abandoned.exists());
    population.assert_foreign_entries_survived();

    // A day later the two dead-process directories are reclaimable; nothing
    // else ever becomes reclaimable.
    let report = PrivateTempDir::reconcile_abandoned(base.path(), prefix, 3600, now + 86_400)
        .expect("reconcile");
    assert_eq!(report.removed, 2);
    assert_eq!(report.skipped_own, 1);
    assert_eq!(report.skipped_foreign, 3);
    assert!(
        !population.abandoned.exists(),
        "abandoned directory and its contents must be gone"
    );
    assert!(!population.second.exists());
    population.assert_foreign_entries_survived();
}

#[cfg(unix)]
#[test]
fn reconcile_abandoned_removes_only_old_matching_directories() {
    let base = temp_root();
    let prefix = "plaudon-import";
    let population = populate_staging_base(base.path(), prefix);
    let now = now_unix();

    // One matching directory is aged past the grace period; its sibling is not.
    set_mtime(&population.abandoned, now - 86_400);

    // Ours by name, but a symlink: must never be followed or deleted.
    let victim = temp_root();
    write_file(&victim.path().join("keepme.txt"), "x");
    let planted = base
        .path()
        .join(format!("{prefix}-424244-{}", "ab12cd34".repeat(4)));
    std::os::unix::fs::symlink(victim.path(), &planted).expect("symlink");

    let report =
        PrivateTempDir::reconcile_abandoned(base.path(), prefix, 3600, now).expect("reconcile");

    assert_eq!(report.removed, 1);
    assert!(
        !population.abandoned.exists(),
        "the aged staging directory must be removed"
    );
    assert!(
        population.second.exists(),
        "a directory inside the grace period must survive"
    );
    population.assert_foreign_entries_survived();
    assert!(
        victim.path().join("keepme.txt").exists(),
        "symlink target must survive"
    );
    assert!(
        fs::symlink_metadata(&planted).is_ok(),
        "planted symlink must survive"
    );
    assert_eq!(
        report.skipped_foreign, 4,
        "the symlink counts as foreign, not as ours"
    );
}

#[test]
fn reconcile_abandoned_is_a_noop_for_a_missing_base() {
    let report = PrivateTempDir::reconcile_abandoned(
        Path::new("/plaudon-no-such-base-7a1c"),
        "plaudon-import",
        60,
        1_800_000_000,
    )
    .expect("missing base is not an error");
    assert_eq!(report, ReconcileReport::default());
}

#[test]
fn reconcile_abandoned_refuses_a_hostile_prefix() {
    let base = temp_root();
    for prefix in ["", "../..", "a/b", "p*"] {
        assert!(
            PrivateTempDir::reconcile_abandoned(base.path(), prefix, 60, now_unix()).is_err(),
            "{prefix:?}"
        );
        assert!(
            PrivateTempDir::create(Some(base.path()), prefix).is_err(),
            "{prefix:?}"
        );
    }
}

fn now_unix() -> i64 {
    let delta = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after epoch");
    i64::try_from(delta.as_secs()).expect("representable")
}

/// Set a directory's mtime without `unsafe`: unix lets a directory be opened
/// read-only, and `File::set_times` is stable.
#[cfg(unix)]
fn set_mtime(path: &Path, seconds: i64) {
    let when = std::time::UNIX_EPOCH
        + std::time::Duration::from_secs(u64::try_from(seconds).expect("positive"));
    let handle = fs::File::options()
        .read(true)
        .open(path)
        .expect("open directory");
    handle
        .set_times(fs::FileTimes::new().set_modified(when))
        .expect("set mtime");
}

#[test]
fn stage_and_commit_by_rename() {
    let base = temp_root();
    let archive = temp_root();
    let private = PrivateTempDir::create(Some(base.path()), "plaudon-import").expect("create");

    let mut staged = private.stage_file("0f1e2d3c").expect("stage");
    assert!(staged
        .path()
        .as_path()
        .to_string_lossy()
        .ends_with(STAGED_SUFFIX));
    assert!(staged.path().as_path().starts_with(private.path()));
    staged.write_all(b"recording bytes").expect("write");
    let staged_path = staged.path().as_path().to_path_buf();

    fs::create_dir_all(archive.path().join("media/0f")).expect("mkdir");
    let target = canonicalize_and_validate(
        &PathPolicy::for_roots([archive.path().to_path_buf()]),
        &archive.path().join("media/0f/0f1e2d3c.m4a"),
    )
    .expect("target");

    let committed = staged.commit_by_rename(&target).expect("commit");
    assert_eq!(committed, target);
    assert_eq!(
        fs::read(committed.as_path()).expect("read"),
        b"recording bytes"
    );
    assert!(
        !staged_path.exists(),
        "the .part file must be gone after a rename commit"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(committed.as_path())
            .expect("stat")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}

#[test]
fn abandoned_staged_file_is_removed_on_drop() {
    let base = temp_root();
    let private = PrivateTempDir::create(Some(base.path()), "plaudon-import").expect("create");
    let path = {
        let mut staged = private.stage_file("abandoned").expect("stage");
        staged.write_all(b"partial").expect("write");
        staged.path().as_path().to_path_buf()
    };
    assert!(!path.exists(), "a dropped staged file must not linger");
}

#[test]
fn stage_file_refuses_a_hostile_name() {
    let base = temp_root();
    let private = PrivateTempDir::create(Some(base.path()), "plaudon-import").expect("create");
    for name in ["../escape", "CON", "a/b", "x\0y", "nul.mp3", ""] {
        assert!(
            private.stage_file(name).is_err(),
            "{name:?} must be refused"
        );
    }
    assert_eq!(
        entry_count(private.path()),
        0,
        "a refused name must not create a file"
    );
}

#[test]
fn commit_refuses_a_cross_volume_target() {
    // Deterministic half: the decision itself, with injected ids.
    assert!(require_same_volume(VolumeId::from_raw(7), VolumeId::from_raw(7)).is_ok());
    let error = require_same_volume(VolumeId::from_raw(7), VolumeId::from_raw(8))
        .expect_err("different volumes");
    assert_eq!(error.reason(), PathRejection::CrossVolumeRename);
    assert_eq!(error.envelope_parts().code, PATH_REJECTED);
    assert!(!same_volume(VolumeId::from_raw(7), VolumeId::from_raw(8)));

    // Opportunistic half: if this host exposes a second volume we can resolve a
    // path on, the real commit must refuse it.
    let base = temp_root();
    let private = PrivateTempDir::create(Some(base.path()), "plaudon-import").expect("create");
    let staging_volume = volume_id(private.path()).expect("volume");
    if let Some(other) = other_volume_target(staging_volume) {
        let mut staged = private.stage_file("cross").expect("stage");
        staged.write_all(b"x").expect("write");
        let error = staged.commit_by_rename(&other).expect_err("cross volume");
        assert_eq!(error.reason(), PathRejection::CrossVolumeRename);
    }
}

/// A validated path on a different volume, if this host has one we can reach.
fn other_volume_target(staging_volume: VolumeId) -> Option<ValidatedPath> {
    let candidates = [
        "/Volumes",
        "/System/Volumes",
        "/media",
        "/mnt",
        "/run/media",
    ];
    for base in candidates {
        let Ok(entries) = fs::read_dir(base) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(found) = volume_id(&path) else {
                continue;
            };
            if same_volume(found, staging_volume) {
                continue;
            }
            let policy = PathPolicy::for_roots([path.clone()]);
            if let Ok(validated) =
                canonicalize_and_validate(&policy, &path.join("plaudon-cross-volume-probe.m4a"))
            {
                return Some(validated);
            }
        }
    }
    None
}

#[test]
fn imported_source_file_is_untouched_by_staging() {
    // §22.11 / §18.6A "USB media … No damage to source".
    let source_dir = temp_root();
    let source = source_dir.path().join("original recording.m4a");
    write_file(&source, "original bytes that must not change");
    let before_meta = fs::metadata(&source).expect("stat");
    let before_len = before_meta.len();
    let before_mtime = before_meta.modified().expect("mtime");
    let before_hash = sha256_hex(&fs::read(&source).expect("read"));
    #[cfg(unix)]
    let before_mode = {
        use std::os::unix::fs::PermissionsExt;
        before_meta.permissions().mode()
    };

    let base = temp_root();
    let archive = temp_root();
    let private = PrivateTempDir::create(Some(base.path()), "plaudon-import").expect("create");

    let validated_source = canonicalize_and_validate(
        &PathPolicy::for_roots([source_dir.path().to_path_buf()]),
        &source,
    )
    .expect("source accepted");
    let bytes = fs::read(validated_source.as_path()).expect("read source");

    let mut staged = private.stage_file(&before_hash[..16]).expect("stage");
    staged.write_all(&bytes).expect("write");
    fs::create_dir_all(archive.path().join("media")).expect("mkdir");
    let target = canonicalize_and_validate(
        &PathPolicy::for_roots([archive.path().to_path_buf()]),
        &archive
            .path()
            .join(format!("media/{}.m4a", &before_hash[..16])),
    )
    .expect("target");
    staged.commit_by_rename(&target).expect("commit");

    let after_meta = fs::metadata(&source).expect("stat");
    assert_eq!(after_meta.len(), before_len);
    assert_eq!(after_meta.modified().expect("mtime"), before_mtime);
    assert_eq!(sha256_hex(&fs::read(&source).expect("read")), before_hash);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(after_meta.permissions().mode(), before_mode);
    }
    assert_eq!(fs::read(target.as_path()).expect("read"), bytes);
}

#[test]
fn set_restrictive_permissions_locks_down_managed_data() {
    let root = temp_root();
    let file = root.path().join("app.sqlite");
    write_file(&file, "x");
    let dir = root.path().join("media");
    fs::create_dir(&dir).expect("mkdir");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&file, fs::Permissions::from_mode(0o666)).expect("chmod");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o777)).expect("chmod");
    }

    let policy = policy_for(&root);
    for path in [&file, &dir] {
        let validated = canonicalize_and_validate(&policy, path).expect("validated");
        set_restrictive_permissions(&validated).expect("permissions");
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&file).expect("stat").permissions().mode() & 0o777,
            FILE_MODE
        );
        assert_eq!(
            fs::metadata(&dir).expect("stat").permissions().mode() & 0o777,
            DIRECTORY_MODE
        );
        assert_eq!(is_restrictive(&file), Some(true));
    }
    #[cfg(not(unix))]
    assert_eq!(is_restrictive(&file), None);
}

// ---------------------------------------------------------------------------
// archive entries
// ---------------------------------------------------------------------------

#[test]
fn archive_validation_rejects_every_hostile_entry_shape() {
    let destination = temp_root();
    let cases: [(ArchiveEntryMeta, ArchiveRejection); 11] = [
        (
            ArchiveEntryMeta::file("../../etc/passwd", 10, 10),
            ArchiveRejection::Traversal,
        ),
        (
            ArchiveEntryMeta::file("media/../../x", 10, 10),
            ArchiveRejection::Traversal,
        ),
        (
            ArchiveEntryMeta::file("/etc/passwd", 10, 10),
            ArchiveRejection::AbsolutePath,
        ),
        (
            ArchiveEntryMeta::file("C:\\Windows\\x", 10, 10),
            ArchiveRejection::AbsolutePath,
        ),
        (
            ArchiveEntryMeta::file("\\\\server\\share\\x", 10, 10),
            ArchiveRejection::AbsolutePath,
        ),
        (
            ArchiveEntryMeta::of_kind("media/link.m4a", ArchiveEntryKind::Symlink),
            ArchiveRejection::LinkEntry,
        ),
        (
            ArchiveEntryMeta::of_kind("media/hard.m4a", ArchiveEntryKind::HardLink),
            ArchiveRejection::LinkEntry,
        ),
        (
            ArchiveEntryMeta::of_kind("dev/null", ArchiveEntryKind::CharacterDevice),
            ArchiveRejection::SpecialEntry,
        ),
        (
            ArchiveEntryMeta::of_kind("pipe", ArchiveEntryKind::Fifo),
            ArchiveRejection::SpecialEntry,
        ),
        (
            ArchiveEntryMeta::file("media/CON", 10, 10),
            ArchiveRejection::UnsupportedName,
        ),
        (
            ArchiveEntryMeta::file("media/bidi\u{202E}x.m4a", 10, 10),
            ArchiveRejection::UnsupportedName,
        ),
    ];

    for (entry, expected) in cases {
        let mut policy = ArchivePolicy::new(destination.path());
        let error = validate_archive_entry(&entry, &mut policy)
            .expect_err(&format!("{:?} must be rejected", entry.path));
        assert_eq!(error.reason(), expected, "{:?}", entry.path);
        assert_eq!(error.envelope_parts().code, ARCHIVE_ENTRY_REJECTED);
        assert_eq!(policy.accepted_entries(), 0);
    }
}

#[test]
fn archive_validation_rejects_duplicates_including_case_and_nfc_folding() {
    let destination = temp_root();
    let mut policy = ArchivePolicy::new(destination.path());
    validate_archive_entry(
        &ArchiveEntryMeta::file("media/Recording.m4a", 10, 10),
        &mut policy,
    )
    .expect("first");

    for duplicate in [
        "media/Recording.m4a",
        "media/recording.m4a",
        "media\\RECORDING.M4A",
    ] {
        let error = validate_archive_entry(&ArchiveEntryMeta::file(duplicate, 10, 10), &mut policy)
            .expect_err("duplicate");
        assert_eq!(
            error.reason(),
            ArchiveRejection::DuplicatePath,
            "{duplicate:?}"
        );
    }

    let mut policy = ArchivePolicy::new(destination.path());
    validate_archive_entry(
        &ArchiveEntryMeta::file("media/caf\u{E9}.m4a", 10, 10),
        &mut policy,
    )
    .expect("composed");
    let error = validate_archive_entry(
        &ArchiveEntryMeta::file("media/cafe\u{301}.m4a", 10, 10),
        &mut policy,
    )
    .expect_err("decomposed duplicate");
    assert_eq!(error.reason(), ArchiveRejection::DuplicatePath);
}

#[test]
fn archive_validation_rejects_decompression_bombs() {
    let destination = temp_root();

    let mut policy = ArchivePolicy::new(destination.path()).with_max_entry_bytes(1024);
    assert_eq!(
        validate_archive_entry(&ArchiveEntryMeta::file("big.bin", 4096, 4096), &mut policy)
            .unwrap_err()
            .reason(),
        ArchiveRejection::EntryTooLarge
    );

    let mut policy = ArchivePolicy::new(destination.path()).with_max_total_bytes(1000);
    validate_archive_entry(&ArchiveEntryMeta::file("a.bin", 600, 600), &mut policy).expect("first");
    assert_eq!(
        validate_archive_entry(&ArchiveEntryMeta::file("b.bin", 600, 600), &mut policy)
            .unwrap_err()
            .reason(),
        ArchiveRejection::ArchiveTooLarge
    );
    assert_eq!(
        policy.accepted_bytes(),
        600,
        "a rejected entry must not count"
    );

    let mut policy = ArchivePolicy::new(destination.path())
        .with_ratio_check_min_bytes(1024)
        .with_max_compression_ratio(50);
    assert_eq!(
        validate_archive_entry(
            &ArchiveEntryMeta::file("bomb.bin", 10_000_000, 1000),
            &mut policy
        )
        .unwrap_err()
        .reason(),
        ArchiveRejection::CompressionRatioExceeded
    );
    // Zero declared compressed size is an infinite ratio, not a free pass.
    assert_eq!(
        validate_archive_entry(
            &ArchiveEntryMeta::file("bomb2.bin", 10_000_000, 0),
            &mut policy
        )
        .unwrap_err()
        .reason(),
        ArchiveRejection::CompressionRatioExceeded
    );
    // Incompressible media is unaffected.
    validate_archive_entry(
        &ArchiveEntryMeta::file("audio.m4a", 10_000_000, 9_900_000),
        &mut policy,
    )
    .expect("media entry");

    let mut policy = ArchivePolicy::new(destination.path()).with_max_entries(1);
    validate_archive_entry(&ArchiveEntryMeta::file("one.bin", 1, 1), &mut policy).expect("first");
    assert_eq!(
        validate_archive_entry(&ArchiveEntryMeta::file("two.bin", 1, 1), &mut policy)
            .unwrap_err()
            .reason(),
        ArchiveRejection::TooManyEntries
    );
}

#[test]
fn archive_validation_never_touches_the_filesystem() {
    let destination = temp_root();
    fs::create_dir(destination.path().join("existing")).expect("mkdir");
    let before = entry_count(destination.path());

    let mut policy = ArchivePolicy::new(destination.path());
    let hostile = [
        ArchiveEntryMeta::file("../../../../etc/passwd", 10, 10),
        ArchiveEntryMeta::of_kind("evil", ArchiveEntryKind::Symlink),
        ArchiveEntryMeta::file("media/nul.m4a", 10, 10),
        ArchiveEntryMeta::file("x".repeat(400), 10, 10),
    ];
    for entry in &hostile {
        assert!(validate_archive_entry(entry, &mut policy).is_err());
    }
    // Accepting an entry must not create it either: extraction is a separate,
    // explicit step (§13.5 inspect-before-write).
    validate_archive_entry(
        &ArchiveEntryMeta::file("media/new.m4a", 10, 10),
        &mut policy,
    )
    .expect("accepted");

    assert_eq!(
        entry_count(destination.path()),
        before,
        "validation mutated the destination"
    );
    assert!(!destination.path().join("media").exists());
}

#[test]
fn archive_validation_accepts_a_normal_manifest_and_resolves_targets() {
    let destination = temp_root();
    let mut policy = ArchivePolicy::new(destination.path());

    let manifest = [
        ArchiveEntryMeta::file("manifest.json", 2048, 512),
        ArchiveEntryMeta::directory("media"),
        ArchiveEntryMeta::file("media/0f1e2d3c.m4a", 5_000_000, 4_900_000),
        ArchiveEntryMeta::file("records/2026-03-04.json", 4096, 700),
    ];
    let mut validated = Vec::new();
    for entry in &manifest {
        validated.push(validate_archive_entry(entry, &mut policy).expect("accepted"));
    }
    assert_eq!(policy.accepted_entries(), 4);
    assert_eq!(policy.accepted_bytes(), 2048 + 5_000_000 + 4096);
    assert_eq!(
        validated[2].relative_path(),
        Path::new("media/0f1e2d3c.m4a")
    );
    assert_eq!(validated[2].kind(), ArchiveEntryKind::File);
    assert_eq!(validated[1].duplicate_key(), "media");

    // Resolution happens at write time, against the real destination.
    fs::create_dir(destination.path().join("media")).expect("mkdir");
    let resolved = validated[2].resolve_target(&policy).expect("resolved");
    assert_eq!(
        resolved.as_path(),
        fs::canonicalize(destination.path())
            .expect("root")
            .join("media/0f1e2d3c.m4a")
    );
}

#[cfg(unix)]
#[test]
fn archive_target_resolution_catches_a_symlink_planted_after_inspection() {
    let destination = temp_root();
    let outside = temp_root();
    let mut policy = ArchivePolicy::new(destination.path());
    let entry = validate_archive_entry(
        &ArchiveEntryMeta::file("media/recording.m4a", 10, 10),
        &mut policy,
    )
    .expect("accepted by pre-flight");

    // Between inspection and extraction, `media` becomes a link out of the tree.
    std::os::unix::fs::symlink(outside.path(), destination.path().join("media")).expect("link");

    let error = entry.resolve_target(&policy).expect_err("planted symlink");
    assert_eq!(error.reason(), PathRejection::SymlinkEscapesRoot);
}

#[test]
fn archive_policy_can_require_windows_short_paths() {
    let destination = temp_root();
    let mut policy = ArchivePolicy::new("/plaudon/dest").with_windows_short_path_compatible(true);
    let long = format!("media/{}/x.m4a", "d".repeat(250));
    let error =
        validate_archive_entry(&ArchiveEntryMeta::file(long, 10, 10), &mut policy).unwrap_err();
    assert_eq!(error.reason(), ArchiveRejection::UnsupportedName);
    assert_eq!(
        error.name_rule(),
        Some(PathRejection::NotWindowsShortPathCompatible)
    );

    let mut policy = ArchivePolicy::new(destination.path());
    validate_archive_entry(
        &ArchiveEntryMeta::file("media/short.m4a", 10, 10),
        &mut policy,
    )
    .expect("short path");
}

#[test]
fn archive_error_label_is_escaped_and_truncated() {
    let destination = temp_root();
    let mut policy = ArchivePolicy::new(destination.path());
    let entry = ArchiveEntryMeta::file(format!("media/{}\u{202E}.m4a", "z".repeat(200)), 10, 10);
    let error = validate_archive_entry(&entry, &mut policy).expect_err("rejected");
    assert!(!error.label().contains('\u{202E}'));
    assert!(error.label().chars().count() <= 49);
    let parts = error.envelope_parts();
    assert_eq!(parts.action, Some(CHOOSE_DIFFERENT_FILE));
    assert!(!parts.message.contains('\u{202E}'));
}
