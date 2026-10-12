//! Fixture builders for a mounted legacy PLAUD Note USB volume
//! (spec 18.6A USB media cluster).
//!
//! Builders return bytes or write a directory tree; none of them touch a
//! real device. Callers own the directory (normally a `tempfile` dir).

use std::path::Path;

/// Serial string the fixture WAVs embed in vendor metadata chunks.
pub const FIXTURE_SERIAL: &str = "PLAUD-SN-A1B2C3D4E5";

/// Sample bytes used as the `data` chunk of [`wav_with_serial`] callers.
pub const FIXTURE_PCM: &[u8] = &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];

fn chunk(id: [u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = id.to_vec();
    out.extend_from_slice(
        &u32::try_from(body.len())
            .expect("small fixture")
            .to_le_bytes(),
    );
    out.extend_from_slice(body);
    if body.len() % 2 == 1 {
        out.push(0);
    }
    out
}

/// A RIFF/WAVE whose `LIST`/`INFO` and vendor `PLDN` chunks carry
/// [`FIXTURE_SERIAL`].
#[must_use]
pub fn wav_with_serial(pcm: &[u8]) -> Vec<u8> {
    let mut fmt = Vec::new();
    fmt.extend_from_slice(&1u16.to_le_bytes());
    fmt.extend_from_slice(&1u16.to_le_bytes());
    fmt.extend_from_slice(&16_000u32.to_le_bytes());
    fmt.extend_from_slice(&32_000u32.to_le_bytes());
    fmt.extend_from_slice(&2u16.to_le_bytes());
    fmt.extend_from_slice(&16u16.to_le_bytes());
    let mut info = b"INFO".to_vec();
    info.extend_from_slice(&chunk(*b"ISRC", FIXTURE_SERIAL.as_bytes()));
    let mut body = b"WAVE".to_vec();
    body.extend_from_slice(&chunk(*b"LIST", &info));
    body.extend_from_slice(&chunk(*b"fmt ", &fmt));
    body.extend_from_slice(&chunk(*b"PLDN", FIXTURE_SERIAL.as_bytes()));
    body.extend_from_slice(&chunk(*b"data", pcm));
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(
        &u32::try_from(body.len())
            .expect("small fixture")
            .to_le_bytes(),
    );
    out.extend_from_slice(&body);
    out
}

/// A valid length-prefixed Opus `.ASR` sidecar of `packets` 20 ms packets.
#[must_use]
pub fn asr_sidecar(packets: u8) -> Vec<u8> {
    let mut out = Vec::new();
    for i in 0..packets {
        let packet = [0x08, i, 0xAA, 0xBB];
        out.extend_from_slice(&4u16.to_be_bytes());
        out.extend_from_slice(&packet);
    }
    out
}

/// Create `NOTES/` and `CALLS/` under `root` and write each
/// `(relative path, bytes)` file.
///
/// # Panics
///
/// Panics if the fixture tree cannot be written.
pub fn build_volume(root: &Path, files: &[(&str, &[u8])]) {
    std::fs::create_dir_all(root.join("NOTES")).expect("NOTES");
    std::fs::create_dir_all(root.join("CALLS")).expect("CALLS");
    for (name, bytes) in files {
        std::fs::write(root.join(name), bytes).expect("fixture file");
    }
}

/// Whether `needle` occurs in `haystack`.
#[must_use]
pub fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}
