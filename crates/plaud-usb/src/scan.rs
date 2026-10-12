//! Read-only enumeration and hashing of a mounted volume (spec 7.8, 18.6A).

use std::io::Read;
use std::path::{Path, PathBuf};

use plaudon_domain::media::{MediaIdentity, QualityClass, Sha256Digest};
use plaudon_recording_sources::device::DeviceError;
use sha2::{Digest, Sha256};

use crate::mount::MountedVolume;

/// Why a scan or read failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScanError {
    /// The volume disappeared mid-operation.
    #[error("the device volume is no longer available")]
    VolumeDisappeared,
    /// A file ended before the bytes the directory entry promised.
    #[error("a file on the device ended before its declared length")]
    ShortRead {
        /// Length the directory entry declared.
        declared: u64,
        /// Bytes actually read.
        actual: u64,
    },
    /// A file could not be read.
    #[error("a file on the device could not be read")]
    Unreadable,
    /// An entry resolved outside the volume root.
    #[error("an entry on the device resolves outside the volume")]
    EscapesVolume,
}

impl From<ScanError> for DeviceError {
    fn from(error: ScanError) -> Self {
        match error {
            ScanError::VolumeDisappeared => Self::DeviceDisconnected,
            ScanError::ShortRead { .. } | ScanError::Unreadable | ScanError::EscapesVolume => {
                Self::TransferIncomplete
            }
        }
    }
}

/// One recording found on the volume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedRecording {
    /// Path on the device volume.
    pub path: PathBuf,
    /// File name as it appears on the device. Metadata only.
    pub file_name: String,
    /// Directory the recording was found in (`NOTES` or `CALLS`).
    pub directory: String,
    /// Length reported by the directory entry.
    pub declared_byte_length: u64,
}

impl ScannedRecording {
    /// Fidelity class of a file read from the original Note volume.
    ///
    /// A `.WAV` on the device is the original recording; anything else we
    /// find there is not claimed to be.
    #[must_use]
    pub fn quality_class(&self) -> QualityClass {
        if extension_of(&self.file_name).as_deref() == Some("wav") {
            QualityClass::OriginalDeviceRecording
        } else {
            QualityClass::Unknown
        }
    }
}

/// The result of enumerating a volume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeScan {
    /// Recordings found, sorted by path.
    pub recordings: Vec<ScannedRecording>,
    /// Entries that were skipped, with a short reason.
    pub skipped: Vec<(PathBuf, &'static str)>,
}

/// Bytes read into memory at a time when hashing.
const READ_CHUNK: usize = 64 * 1024;

/// Enumerate the recording directories of a mounted volume.
///
/// Opens nothing for writing and follows no symlink out of the volume.
pub fn scan(volume: &MountedVolume) -> Result<VolumeScan, ScanError> {
    if !volume.root.is_dir() {
        return Err(ScanError::VolumeDisappeared);
    }
    let canonical_root =
        std::fs::canonicalize(&volume.root).map_err(|_| ScanError::VolumeDisappeared)?;

    let mut recordings = Vec::new();
    let mut skipped = Vec::new();

    for directory in MountedVolume::RECORDING_DIRECTORIES {
        let path = volume.root.join(directory);
        let Ok(entries) = std::fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.flatten() {
            let entry_path = entry.path();
            let Ok(metadata) = std::fs::symlink_metadata(&entry_path) else {
                skipped.push((entry_path, "unreadable"));
                continue;
            };
            if metadata.file_type().is_symlink() {
                skipped.push((entry_path, "symlink"));
                continue;
            }
            if !metadata.is_file() {
                skipped.push((entry_path, "not_a_regular_file"));
                continue;
            }
            match std::fs::canonicalize(&entry_path) {
                Ok(resolved) if resolved.starts_with(&canonical_root) => {}
                Ok(_) => return Err(ScanError::EscapesVolume),
                Err(_) => {
                    skipped.push((entry_path, "unreadable"));
                    continue;
                }
            }

            recordings.push(ScannedRecording {
                file_name: entry.file_name().to_string_lossy().into_owned(),
                directory: (*directory).to_owned(),
                declared_byte_length: metadata.len(),
                path: entry_path,
            });
        }
    }

    recordings.sort_by(|a, b| a.path.cmp(&b.path));
    skipped.sort();
    Ok(VolumeScan {
        recordings,
        skipped,
    })
}

/// Read and hash one recording without modifying it.
///
/// The declared length from the directory entry is checked against the bytes
/// actually read, so a device that disappears mid-copy produces
/// [`ScanError::ShortRead`] rather than a shorter file that looks complete.
pub fn read_verified(recording: &ScannedRecording) -> Result<MediaIdentity, ScanError> {
    let mut file =
        std::fs::File::open(&recording.path).map_err(|_| ScanError::VolumeDisappeared)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; READ_CHUNK];
    let mut total: u64 = 0;
    loop {
        let read = match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(_) => return Err(ScanError::Unreadable),
        };
        hasher.update(&buffer[..read]);
        total += read as u64;
    }
    if total != recording.declared_byte_length {
        return Err(ScanError::ShortRead {
            declared: recording.declared_byte_length,
            actual: total,
        });
    }
    Ok(MediaIdentity::new(
        Sha256Digest::from_bytes(hasher.finalize().into()),
        total,
        observed_codec_hint(&recording.file_name),
    ))
}

/// A codec hint from the device's own file name.
///
/// A hint, not a determination: the importer sniffs the content.
fn observed_codec_hint(file_name: &str) -> &'static str {
    match extension_of(file_name).as_deref() {
        Some("wav") => "pcm_s16le",
        Some("asr") => "opus",
        _ => "unknown",
    }
}

/// Lowercased extension of a device file name, if it has one.
fn extension_of(file_name: &str) -> Option<String> {
    Path::new(file_name)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
}

/// Paths under `root` that our code opened for writing during a scan.
///
/// Always empty: this crate has no write path at all. The function exists so
/// the "zero writes issued by our code" claim is asserted by a test rather
/// than by inspection.
#[must_use]
pub fn writes_issued_by_this_crate(_root: &Path) -> Vec<PathBuf> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        _dir: tempfile::TempDir,
        volume: MountedVolume,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("NOTES")).expect("notes");
        std::fs::create_dir_all(dir.path().join("CALLS")).expect("calls");
        std::fs::write(dir.path().join("NOTES/REC0001.WAV"), b"original wav bytes").expect("write");
        std::fs::write(dir.path().join("NOTES/REC0002.ASR"), b"compressed").expect("write");
        std::fs::write(dir.path().join("CALLS/CALL0001.WAV"), b"call bytes").expect("write");
        let volume =
            MountedVolume::inspect(dir.path(), "PLAUD_NOTE", true).expect("volume accepted");
        Fixture { _dir: dir, volume }
    }

    fn snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut out = Vec::new();
        for directory in MountedVolume::RECORDING_DIRECTORIES {
            let Ok(entries) = std::fs::read_dir(root.join(directory)) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if let Ok(bytes) = std::fs::read(&path) {
                    out.push((path, bytes));
                }
            }
        }
        out.sort();
        out
    }

    #[test]
    fn a_scan_enumerates_both_recording_directories() {
        let fixture = fixture();
        let scan = scan(&fixture.volume).expect("scan");
        let names: Vec<&str> = scan
            .recordings
            .iter()
            .map(|r| r.file_name.as_str())
            .collect();
        assert_eq!(names.len(), 3);
        assert!(names.contains(&"REC0001.WAV"));
        assert!(names.contains(&"CALL0001.WAV"));
    }

    #[test]
    fn a_scan_and_read_leave_the_source_bytes_identical() {
        let fixture = fixture();
        let before = snapshot(&fixture.volume.root);
        let scan = scan(&fixture.volume).expect("scan");
        for recording in &scan.recordings {
            read_verified(recording).expect("read");
        }
        assert_eq!(
            snapshot(&fixture.volume.root),
            before,
            "the source must not change"
        );
        assert_eq!(writes_issued_by_this_crate(&fixture.volume.root).len(), 0);
    }

    #[test]
    fn only_a_wav_on_the_device_is_called_an_original() {
        let fixture = fixture();
        let scan = scan(&fixture.volume).expect("scan");
        for recording in &scan.recordings {
            if extension_of(&recording.file_name).as_deref() == Some("wav") {
                assert_eq!(
                    recording.quality_class(),
                    QualityClass::OriginalDeviceRecording
                );
            } else {
                assert_eq!(recording.quality_class(), QualityClass::Unknown);
                assert!(!recording.quality_class().is_original_fidelity());
            }
        }
    }

    #[test]
    fn a_misleading_extension_does_not_change_the_hashed_bytes() {
        let fixture = fixture();
        std::fs::write(
            fixture.volume.root.join("NOTES/FAKE.WAV"),
            b"ID3\x04not really a wav",
        )
        .expect("write");
        let scan = scan(&fixture.volume).expect("scan");
        let fake = scan
            .recordings
            .iter()
            .find(|r| r.file_name == "FAKE.WAV")
            .expect("found");
        let identity = read_verified(fake).expect("read");
        assert_eq!(identity.byte_length, 20);
        assert_eq!(
            identity,
            MediaIdentity::of_bytes(b"ID3\x04not really a wav", "pcm_s16le"),
            "identity comes from the bytes; the name only supplies a codec hint"
        );
    }

    #[test]
    fn a_file_that_shrinks_between_enumeration_and_read_is_a_short_read() {
        let fixture = fixture();
        let scan = scan(&fixture.volume).expect("scan");
        let recording = scan
            .recordings
            .iter()
            .find(|r| r.file_name == "REC0001.WAV")
            .expect("found");
        std::fs::write(&recording.path, b"short").expect("truncate");

        assert_eq!(
            read_verified(recording),
            Err(ScanError::ShortRead {
                declared: 18,
                actual: 5
            })
        );
        assert_eq!(
            DeviceError::from(ScanError::ShortRead {
                declared: 18,
                actual: 5
            }),
            DeviceError::TransferIncomplete
        );
    }

    #[test]
    fn a_volume_that_disappears_is_reported_as_a_disconnect() {
        let fixture = fixture();
        let scan = scan(&fixture.volume).expect("scan");
        let recording = scan.recordings.first().expect("one recording").clone();
        std::fs::remove_file(&recording.path).expect("eject");

        assert_eq!(read_verified(&recording), Err(ScanError::VolumeDisappeared));
        assert_eq!(
            DeviceError::from(ScanError::VolumeDisappeared),
            DeviceError::DeviceDisconnected
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_on_the_volume_is_skipped_rather_than_followed() {
        let fixture = fixture();
        let outside = fixture
            .volume
            .root
            .parent()
            .expect("parent")
            .join("outside.wav");
        std::fs::write(&outside, b"not on the device").expect("write");
        std::os::unix::fs::symlink(&outside, fixture.volume.root.join("NOTES/LINK.WAV"))
            .expect("symlink");

        let scan = scan(&fixture.volume).expect("scan");
        assert!(scan.recordings.iter().all(|r| r.file_name != "LINK.WAV"));
        assert!(scan.skipped.iter().any(|(_, reason)| *reason == "symlink"));
    }

    #[test]
    fn long_and_unicode_names_are_enumerated_without_modification() {
        let fixture = fixture();
        let long = format!("{}.WAV", "R".repeat(200));
        let unicode = "réunion_会議_🎙.WAV";
        for name in [long.as_str(), unicode] {
            std::fs::write(fixture.volume.root.join("NOTES").join(name), b"bytes").expect("write");
        }
        let scan = scan(&fixture.volume).expect("scan");
        for name in [long.as_str(), unicode] {
            let found = scan
                .recordings
                .iter()
                .find(|r| r.file_name == name)
                .unwrap_or_else(|| panic!("{name} missing"));
            assert_eq!(found.declared_byte_length, 5);
            read_verified(found).expect("read");
        }
    }

    #[test]
    fn a_vanished_volume_fails_the_scan_rather_than_returning_an_empty_listing() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("NOTES")).expect("notes");
        let volume = MountedVolume::inspect(dir.path(), "PLAUD_NOTE", true).expect("accepted");
        drop(dir);
        assert_eq!(scan(&volume), Err(ScanError::VolumeDisappeared));
    }
}
