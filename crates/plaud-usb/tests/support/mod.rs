//! Fixture volumes shared by the import tests.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use plaudon_plaud_usb::{scan::scan, MountedVolume, ScannedRecording};

pub const SERIAL: &str = "PLAUD-SN-A1B2C3D4E5";
pub const PCM: &[u8] = &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];

fn chunk(id: [u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = id.to_vec();
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(body);
    if body.len() % 2 == 1 {
        out.push(0);
    }
    out
}

/// A WAV whose LIST/INFO and vendor chunks carry the device serial.
pub fn wav_with_serial(pcm: &[u8]) -> Vec<u8> {
    let mut fmt = Vec::new();
    fmt.extend_from_slice(&1u16.to_le_bytes());
    fmt.extend_from_slice(&1u16.to_le_bytes());
    fmt.extend_from_slice(&16_000u32.to_le_bytes());
    fmt.extend_from_slice(&32_000u32.to_le_bytes());
    fmt.extend_from_slice(&2u16.to_le_bytes());
    fmt.extend_from_slice(&16u16.to_le_bytes());

    let mut info = b"INFO".to_vec();
    info.extend_from_slice(&chunk(*b"ISRC", SERIAL.as_bytes()));
    let mut body = b"WAVE".to_vec();
    body.extend_from_slice(&chunk(*b"LIST", &info));
    body.extend_from_slice(&chunk(*b"fmt ", &fmt));
    body.extend_from_slice(&chunk(*b"PLDN", SERIAL.as_bytes()));
    body.extend_from_slice(&chunk(*b"data", pcm));
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

/// A valid length-prefixed Opus sidecar of `packets` 20 ms packets.
pub fn asr_sidecar(packets: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for i in 0..packets {
        let packet = [0x08, i as u8, 0xAA, 0xBB];
        out.extend_from_slice(&(packet.len() as u16).to_be_bytes());
        out.extend_from_slice(&packet);
    }
    out
}

pub struct Fixture {
    pub dir: tempfile::TempDir,
    pub root: PathBuf,
}

/// A `NOTES/` + `CALLS/` volume holding the given `(relative path, bytes)`.
pub fn volume(files: &[(&str, &[u8])]) -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("NOTES")).unwrap();
    std::fs::create_dir_all(root.join("CALLS")).unwrap();
    for (name, bytes) in files {
        std::fs::write(root.join(name), bytes).unwrap();
    }
    Fixture { dir, root }
}

pub fn mounted(root: &Path, label: &str, read_only: bool) -> MountedVolume {
    MountedVolume::inspect(root, label, read_only).expect("volume accepted")
}

pub fn find(volume: &MountedVolume, name: &str) -> ScannedRecording {
    scan(volume)
        .expect("scan")
        .recordings
        .into_iter()
        .find(|r| r.file_name == name)
        .unwrap_or_else(|| panic!("{name} not scanned"))
}

/// Sorted `(path, bytes)` of every regular file under the volume.
pub fn snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            let meta = std::fs::symlink_metadata(&path).unwrap();
            if meta.is_dir() {
                stack.push(path);
            } else if meta.is_file() {
                out.push((path.clone(), std::fs::read(&path).unwrap()));
            }
        }
    }
    out.sort();
    out
}

pub fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}
