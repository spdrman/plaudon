//! The committed test keypair generator for `tests/fixtures/updater/`
//! (SPEC v1.3 §18.5).
//!
//! The generator *is* this test. Running it normally asserts that every
//! committed fixture is byte-for-byte what the committed test keys produce,
//! so a hand-edited fixture or a changed builder is caught in review. Running
//! it with `UPDATER_REGENERATE_FIXTURES=1` rewrites the tree:
//!
//! ```sh
//! UPDATER_REGENERATE_FIXTURES=1 cargo test -p plaudon-updater --test fixture_generator
//! ```
//!
//! The keys are the clearly-marked test-only pair in
//! [`plaudon_updater::testkeys`]. They are never pinned for release; a unit
//! test in that module enforces it.

use std::path::{Path, PathBuf};

use plaudon_updater::testkeys::{fixture_set, Artifact};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repository root")
        .join("tests/fixtures/updater")
}

fn paths(dir: &Path, name: &str) -> (PathBuf, PathBuf) {
    (
        dir.join(format!("{name}.manifest.json")),
        dir.join(format!("{name}.package.bin")),
    )
}

fn regenerating() -> bool {
    std::env::var("UPDATER_REGENERATE_FIXTURES").is_ok_and(|v| v == "1")
}

#[test]
fn the_committed_fixtures_match_the_committed_generator() {
    let dir = fixtures_dir();
    if regenerating() {
        std::fs::create_dir_all(&dir).expect("create fixtures dir");
    }

    for (name, Artifact { envelope, package }) in fixture_set() {
        let (manifest_path, package_path) = paths(&dir, name);
        if regenerating() {
            std::fs::write(&manifest_path, envelope.as_bytes()).expect("write manifest fixture");
            std::fs::write(&package_path, &package).expect("write package fixture");
            continue;
        }

        let committed_manifest = std::fs::read_to_string(&manifest_path).unwrap_or_else(|e| {
            panic!(
                "missing fixture {}: {e}; regenerate with UPDATER_REGENERATE_FIXTURES=1",
                manifest_path.display()
            )
        });
        let committed_package = std::fs::read(&package_path).unwrap_or_else(|e| {
            panic!("missing fixture {}: {e}", package_path.display())
        });

        assert_eq!(
            committed_manifest, envelope,
            "{name}.manifest.json drifted from the generator"
        );
        assert_eq!(
            committed_package, package,
            "{name}.package.bin drifted from the generator"
        );
    }
}

#[test]
fn the_fixtures_directory_holds_nothing_unexplained() {
    if regenerating() {
        return;
    }
    let dir = fixtures_dir();
    let expected: Vec<String> = fixture_set()
        .iter()
        .flat_map(|(name, _)| {
            [
                format!("{name}.manifest.json"),
                format!("{name}.package.bin"),
            ]
        })
        .collect();

    let mut found: Vec<String> = std::fs::read_dir(&dir)
        .expect("fixtures dir exists")
        .map(|entry| {
            entry
                .expect("dir entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    found.sort_unstable();

    let mut want = expected;
    want.sort_unstable();
    assert_eq!(
        found, want,
        "an unexplained file in tests/fixtures/updater is a review hazard"
    );
}
