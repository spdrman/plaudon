//! Regenerate the malicious `.recording-archive` corpus under
//! `tests/fixtures/archives/` (see `scripts/make-archive-fixtures.sh`).

use std::path::{Path, PathBuf};

fn main() {
    let directory: PathBuf = std::env::args().nth(1).map_or_else(
        || {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .nth(2)
                .expect("workspace root")
                .join("tests/fixtures/archives")
        },
        PathBuf::from,
    );
    plaudon_integration_tests::fakes::hostile_archive::write_corpus(&directory);
    println!("wrote the hostile archive corpus to {}", directory.display());
}
