//! Legacy USB mounted-Note read-only import, end to end over fixture volumes
//! (issue L2.3; spec 4.2B, 7.8, 18.6A USB media cluster).

use std::io::{self, Write};
use std::path::PathBuf;

use plaudon_domain::media::{QualityClass, Sha256Digest};
use plaudon_integration_tests::fakes::usb::{
    asr_sidecar, build_volume, contains, wav_with_serial, FIXTURE_PCM, FIXTURE_SERIAL,
};
use plaudon_plaud_usb::scan::scan;
use plaudon_plaud_usb::{
    import_recording, ImportError, MemoryStagingSink, MountedVolume, SinkState, StagingSink,
    VendorModel,
};

fn fixture(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../fixtures/usb")
        .join(name);
    std::fs::read(path).expect("committed usb fixture")
}

fn tree_digest(root: &std::path::Path) -> Vec<(PathBuf, Sha256Digest)> {
    let mut out = Vec::new();
    for dir in ["NOTES", "CALLS"] {
        for entry in std::fs::read_dir(root.join(dir)).unwrap().flatten() {
            out.push((
                entry.path(),
                Sha256Digest::of(&std::fs::read(entry.path()).unwrap()),
            ));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[test]
fn a_whole_volume_imports_byte_identically_with_the_right_quality_labels() {
    let wav = fixture("serial_note.wav");
    let asr = fixture("sidecar.asr");
    let dir = tempfile::tempdir().unwrap();
    build_volume(
        dir.path(),
        &[
            ("NOTES/A.WAV", &wav),
            ("NOTES/A.ASR", &asr),
            ("CALLS/C.WAV", &wav),
        ],
    );
    let volume = MountedVolume::inspect(dir.path(), "PLAUD_NOTE", true).unwrap();
    let before = tree_digest(dir.path());

    let scanned = scan(&volume).unwrap();
    assert_eq!(scanned.recordings.len(), 3);
    for recording in &scanned.recordings {
        let mut staging = MemoryStagingSink::new();
        let imported = import_recording(&volume, recording, &mut staging, None).unwrap();
        let source = std::fs::read(&recording.path).unwrap();
        assert_eq!(staging.committed_bytes(), source.as_slice());
        assert_eq!(imported.media.sha256, Sha256Digest::of(&source));
        let expected = if recording.file_name.ends_with("ASR") {
            QualityClass::CompressedDeviceTransfer
        } else {
            QualityClass::OriginalDeviceRecording
        };
        assert_eq!(imported.quality_class, expected, "{}", recording.file_name);
    }
    assert_eq!(
        tree_digest(dir.path()),
        before,
        "byte-identical source after import"
    );
}

#[test]
fn the_committed_serial_wav_yields_a_sanitized_derivative_distinct_from_the_source() {
    let wav = fixture("serial_note.wav");
    let dir = tempfile::tempdir().unwrap();
    build_volume(dir.path(), &[("NOTES/A.WAV", &wav)]);
    let volume = MountedVolume::inspect(dir.path(), "PLAUD_NOTE", true).unwrap();
    let recording = scan(&volume).unwrap().recordings.remove(0);
    let mut staging = MemoryStagingSink::new();
    let mut derivative = MemoryStagingSink::new();

    let imported =
        import_recording(&volume, &recording, &mut staging, Some(&mut derivative)).unwrap();

    let derived = imported.derivative.unwrap();
    assert_eq!(derived.quality_class, QualityClass::LocalDerivative);
    assert!(contains(
        staging.committed_bytes(),
        FIXTURE_SERIAL.as_bytes()
    ));
    assert!(!contains(
        derivative.committed_bytes(),
        FIXTURE_SERIAL.as_bytes()
    ));
    assert_ne!(derivative.committed_bytes(), wav.as_slice());
    // The data chunk is the final 3200 bytes of both files, bit-exact.
    let tail = |b: &[u8]| b[b.len() - 3200..].to_vec();
    assert_eq!(tail(derivative.committed_bytes()), tail(&wav));
}

#[test]
fn a_misleading_extension_is_reported_and_the_volume_stays_untouched() {
    let dir = tempfile::tempdir().unwrap();
    build_volume(
        dir.path(),
        &[
            (
                "NOTES/LIAR.WAV",
                b"not riff, just text pretending to be audio",
            ),
            ("NOTES/LIAR.ASR", &wav_with_serial(FIXTURE_PCM)),
        ],
    );
    let volume = MountedVolume::inspect(dir.path(), "PLAUD_NOTE", true).unwrap();
    let before = tree_digest(dir.path());
    for recording in scan(&volume).unwrap().recordings {
        let mut sink = MemoryStagingSink::new();
        let result = import_recording(&volume, &recording, &mut sink, None);
        assert!(
            matches!(result, Err(ImportError::SignatureMismatch { .. })),
            "{}: {result:?}",
            recording.file_name
        );
        assert_eq!(sink.state(), SinkState::Aborted);
    }
    assert_eq!(tree_digest(dir.path()), before);
}

struct EjectOnFirstWrite {
    inner: MemoryStagingSink,
    volume: Option<tempfile::TempDir>,
}

impl Write for EjectOnFirstWrite {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        drop(self.volume.take());
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl StagingSink for EjectOnFirstWrite {
    fn commit(&mut self) -> io::Result<()> {
        self.inner.commit()
    }
    fn abort(&mut self) {
        self.inner.abort();
    }
}

#[test]
fn ejecting_mid_copy_leaves_no_partially_committed_output() {
    let big = wav_with_serial(&vec![9u8; 300 * 1024]);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    build_volume(&root, &[("NOTES/BIG.WAV", &big)]);
    let volume = MountedVolume::inspect(&root, "PLAUD_NOTE", true).unwrap();
    let recording = scan(&volume).unwrap().recordings.remove(0);
    let mut sink = EjectOnFirstWrite {
        inner: MemoryStagingSink::new(),
        volume: Some(dir),
    };

    let result = import_recording(&volume, &recording, &mut sink, None);

    assert_eq!(result, Err(ImportError::VolumeDisappeared));
    assert_eq!(sink.inner.state(), SinkState::Aborted);
    assert_eq!(sink.inner.committed_bytes(), b"");
}

#[test]
fn unsupported_models_get_the_vendor_reason_and_never_mount_instructions() {
    for (label, needle) in [
        ("PLAUD_NOTE_PRO", "charging only"),
        ("PLAUD_NOTEPIN_S", "does not expose recordings over USB"),
    ] {
        let model = VendorModel::from_volume_label(label);
        let refusal = model.mount_instruction().expect_err("must refuse");
        assert!(refusal.message.contains(needle), "{}", refusal.message);
        let dir = tempfile::tempdir().unwrap();
        build_volume(dir.path(), &[]);
        assert!(MountedVolume::inspect(dir.path(), label, true).is_err());
    }
    assert!(VendorModel::Unknown.mount_instruction().is_err());
    assert!(VendorModel::Note.mount_instruction().is_ok());
}

#[test]
fn a_short_asr_fixture_is_measured_not_guessed() {
    let dir = tempfile::tempdir().unwrap();
    build_volume(dir.path(), &[("NOTES/S.ASR", &asr_sidecar(10))]);
    let volume = MountedVolume::inspect(dir.path(), "PLAUD_NOTE", true).unwrap();
    let recording = scan(&volume).unwrap().recordings.remove(0);
    let mut sink = MemoryStagingSink::new();
    let imported = import_recording(&volume, &recording, &mut sink, None).unwrap();
    let summary = imported.asr_summary.unwrap();
    assert_eq!(summary.packet_count, 10);
    assert_eq!(summary.duration_ms, 200);
}
