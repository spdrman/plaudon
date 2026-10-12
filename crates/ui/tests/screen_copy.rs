//! The copy gate, ported from the React shell's `devices/copy.test.ts`.
//!
//! App. B.9 and §4.2A.6: what a direct BLE source yields is the compressed
//! on-device derivative. It is never described as an original recording, as
//! high-quality audio, or as a protected connection, and the label for what a
//! source yields comes from the register-derived DTO rather than from copy in
//! these modules. The React suite enforced that by scanning its own sources;
//! this test scans the Rust ones.

use std::fs;
use std::path::{Path, PathBuf};

/// Every source file of the two destinations, excluding this test itself.
fn sources() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/screens");
    let mut files = Vec::new();
    collect(&root, &mut files);
    assert!(
        files.len() >= 9,
        "the scan found too few files to be scanning the screens: {:?}",
        files.iter().map(|(name, _)| name).collect::<Vec<_>>()
    );
    files
}

fn collect(dir: &Path, out: &mut Vec<(String, String)>) {
    let entries = fs::read_dir(dir).expect("the screens directory is readable");
    for entry in entries {
        let path: PathBuf = entry.expect("a directory entry").path();
        if path.is_dir() {
            collect(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let name = path
                .strip_prefix(Path::new(env!("CARGO_MANIFEST_DIR")))
                .unwrap_or(&path)
                .display()
                .to_string();
            out.push((name, fs::read_to_string(&path).expect("a readable file")));
        }
    }
}

/// Lowercased text with the `#[cfg(test)]` module stripped off, so an
/// assertion in a unit test is not mistaken for user-facing copy.
fn copy_of(text: &str) -> String {
    let body = text
        .split_once("\n#[cfg(test)]\n")
        .map_or(text, |(before, _)| before);
    body.to_lowercase()
}

#[test]
fn the_scan_covers_both_destinations() {
    let names: Vec<String> = sources().into_iter().map(|(name, _)| name).collect();
    for expected in [
        "src/screens/devices/mod.rs",
        "src/screens/devices/detail.rs",
        "src/screens/devices/transfers.rs",
        "src/screens/devices/wizard.rs",
        "src/screens/devices/presentation.rs",
        "src/screens/sources/mod.rs",
        "src/screens/sources/connector.rs",
        "src/screens/sources/recordings.rs",
        "src/screens/sources/folders.rs",
    ] {
        assert!(
            names.iter().any(|name| name.contains(expected)),
            "{expected} is not in the scan: {names:?}"
        );
    }
}

#[test]
fn no_screen_claims_an_original_a_high_quality_file_or_a_secure_connection() {
    // The same four patterns the React gate forbade.
    let banned = [
        "original wav",
        "high-quality",
        "high quality",
        "full-resolution",
        "full resolution",
        "secure connection",
    ];
    for (name, text) in sources() {
        let copy = copy_of(&text);
        for pattern in banned {
            assert!(
                !copy.contains(pattern),
                "{name} contains the forbidden claim {pattern:?}"
            );
        }
    }
}

#[test]
fn the_media_label_is_taken_from_the_dto_and_never_hand_written() {
    for (name, text) in sources() {
        assert!(
            !copy_of(&text).contains("compressed asr"),
            "{name} writes the media label itself instead of rendering source_label"
        );
    }
}

#[test]
fn the_cloud_sentence_exists_in_exactly_one_place_and_is_derived_from_the_dto() {
    let offenders: Vec<String> = sources()
        .into_iter()
        .filter(|(name, text)| {
            text.contains("Cloud: not used") && !name.ends_with("devices/presentation.rs")
        })
        .map(|(name, _)| name)
        .collect();
    assert!(
        offenders.is_empty(),
        "only cloud_label() may contain the sentence; these print it themselves: {offenders:?}"
    );

    let (_, presentation) = sources()
        .into_iter()
        .find(|(name, _)| name.ends_with("devices/presentation.rs"))
        .expect("the presentation module");
    let function = presentation
        .split_once("pub const fn cloud_label")
        .expect("cloud_label exists")
        .1;
    let body = &function[..function.find("\n}").expect("a function body")];
    assert!(
        body.contains("CloudRoute::NotUsed => \"Cloud: not used\"")
            && body.contains("CloudRoute::Used => \"Cloud: used\""),
        "the sentence must follow the DTO's own cloud_route: {body}"
    );
}

#[test]
fn no_screen_promises_a_speed_a_completeness_or_an_eta_it_cannot_know() {
    let banned = [
        "will take",
        "guaranteed",
        "complete archive",
        "all your recordings",
    ];
    for (name, text) in sources() {
        let copy = copy_of(&text);
        for pattern in banned {
            assert!(
                !copy.contains(pattern),
                "{name} contains the unsupportable claim {pattern:?}"
            );
        }
    }
}
