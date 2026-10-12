//! Read-only import behaviour against fixture volumes (spec 4.2B, 7.8, 18.6A).

mod support;

use std::io::{self, Write};

use plaudon_domain::media::{QualityClass, Sha256Digest};
use plaudon_plaud_usb::{
    import_recording, ImportError, MemoryStagingSink, MountedVolume, ScannedRecording, SinkState,
    StagingSink, UsbRefusal, VendorModel, HOST_WRITE_NOTICE,
};
use support::*;

fn import_plain(
    volume: &MountedVolume,
    recording: &ScannedRecording,
) -> (
    Result<plaudon_plaud_usb::ImportedRecording, ImportError>,
    MemoryStagingSink,
) {
    let mut sink = MemoryStagingSink::new();
    let result = import_recording(volume, recording, &mut sink, None);
    (result, sink)
}

#[test]
fn a_wav_is_staged_byte_identical_and_classed_as_the_original() {
    let wav = wav_with_serial(PCM);
    let fixture = volume(&[("NOTES/REC0001.WAV", &wav)]);
    let volume = mounted(&fixture.root, "PLAUD_NOTE", true);
    let recording = find(&volume, "REC0001.WAV");
    let before = snapshot(&fixture.root);

    let (result, sink) = import_plain(&volume, &recording);
    let imported = result.expect("imported");

    assert_eq!(sink.state(), SinkState::Committed);
    assert_eq!(
        sink.committed_bytes(),
        wav.as_slice(),
        "serial bytes preserved"
    );
    assert!(contains(sink.committed_bytes(), SERIAL.as_bytes()));
    assert_eq!(imported.media.sha256, Sha256Digest::of(&wav));
    assert_eq!(imported.media.byte_length, wav.len() as u64);
    assert_eq!(
        imported.quality_class,
        QualityClass::OriginalDeviceRecording
    );
    assert_eq!(imported.detected_container, "wav");
    assert!(imported.derivative.is_none());
    assert_eq!(snapshot(&fixture.root), before, "source untouched");
}

#[test]
fn an_asr_sidecar_is_a_compressed_transfer_never_an_original() {
    let asr = asr_sidecar(5);
    let fixture = volume(&[("NOTES/REC0002.ASR", &asr)]);
    let volume = mounted(&fixture.root, "PLAUD_NOTE", true);
    let recording = find(&volume, "REC0002.ASR");

    let (result, sink) = import_plain(&volume, &recording);
    let imported = result.expect("imported");
    assert_eq!(
        imported.quality_class,
        QualityClass::CompressedDeviceTransfer
    );
    assert!(!imported.quality_class.is_original_fidelity());
    assert_eq!(imported.asr_summary.expect("measured").packet_count, 5);
    assert_eq!(sink.committed_bytes(), asr.as_slice());
}

#[test]
fn a_wav_extension_on_non_riff_content_is_reported_and_not_imported() {
    let fixture = volume(&[("NOTES/FAKE.WAV", b"ID3\x04 definitely not riff at all")]);
    let volume = mounted(&fixture.root, "PLAUD_NOTE", true);
    let (result, sink) = import_plain(&volume, &find(&volume, "FAKE.WAV"));
    assert!(matches!(
        result,
        Err(ImportError::SignatureMismatch { claimed: "wav", .. })
    ));
    assert_eq!(sink.state(), SinkState::Aborted);
    assert_eq!(sink.committed_bytes(), b"");
}

#[test]
fn an_asr_extension_on_riff_content_is_reported_as_a_wav() {
    let fixture = volume(&[("NOTES/SNEAKY.ASR", &wav_with_serial(PCM))]);
    let volume = mounted(&fixture.root, "PLAUD_NOTE", true);
    let (result, sink) = import_plain(&volume, &find(&volume, "SNEAKY.ASR"));
    assert_eq!(
        result,
        Err(ImportError::SignatureMismatch {
            claimed: "asr",
            detected: "wav"
        })
    );
    assert_eq!(sink.committed_bytes(), b"");
}

#[test]
fn an_asr_extension_on_garbage_fails_opus_framing_validation() {
    let fixture = volume(&[("NOTES/BAD.ASR", &[0xFF, 0xFF, 1, 2, 3, 4])]);
    let volume = mounted(&fixture.root, "PLAUD_NOTE", true);
    let (result, sink) = import_plain(&volume, &find(&volume, "BAD.ASR"));
    assert!(
        matches!(result, Err(ImportError::InvalidAsr(_))),
        "{result:?}"
    );
    assert_eq!(sink.state(), SinkState::Aborted);
}

#[test]
fn a_truncated_asr_sidecar_is_not_imported() {
    let mut asr = asr_sidecar(3);
    asr.truncate(asr.len() - 2);
    let fixture = volume(&[("NOTES/CUT.ASR", &asr)]);
    let volume = mounted(&fixture.root, "PLAUD_NOTE", true);
    let (result, sink) = import_plain(&volume, &find(&volume, "CUT.ASR"));
    assert!(
        matches!(result, Err(ImportError::InvalidAsr(_))),
        "{result:?}"
    );
    assert_eq!(sink.committed_bytes(), b"");
}

#[test]
fn an_unknown_extension_is_not_importable() {
    let fixture = volume(&[("NOTES/NOTES.TXT", b"hello")]);
    let volume = mounted(&fixture.root, "PLAUD_NOTE", true);
    let (result, _) = import_plain(&volume, &find(&volume, "NOTES.TXT"));
    assert_eq!(result, Err(ImportError::UnsupportedExtension));
}

#[test]
fn a_zero_byte_file_is_a_typed_empty_file_error() {
    let fixture = volume(&[("NOTES/EMPTY.WAV", b"")]);
    let volume = mounted(&fixture.root, "PLAUD_NOTE", true);
    let (result, sink) = import_plain(&volume, &find(&volume, "EMPTY.WAV"));
    assert_eq!(result, Err(ImportError::EmptyFile));
    assert_eq!(sink.state(), SinkState::Aborted);
}

#[test]
fn a_file_shorter_than_its_directory_entry_is_a_short_read() {
    let wav = wav_with_serial(PCM);
    let fixture = volume(&[("NOTES/SHORT.WAV", &wav)]);
    let volume = mounted(&fixture.root, "PLAUD_NOTE", true);
    let mut recording = find(&volume, "SHORT.WAV");
    let actual = recording.declared_byte_length;
    recording.declared_byte_length += 100;

    let (result, sink) = import_plain(&volume, &recording);
    assert_eq!(
        result,
        Err(ImportError::ShortRead {
            declared: actual + 100,
            actual
        })
    );
    assert_eq!(sink.state(), SinkState::Aborted);
    assert_eq!(sink.committed_bytes(), b"");
}

#[test]
fn a_file_longer_than_its_directory_entry_is_rejected() {
    let wav = wav_with_serial(PCM);
    let fixture = volume(&[("NOTES/LONG.WAV", &wav)]);
    let volume = mounted(&fixture.root, "PLAUD_NOTE", true);
    let mut recording = find(&volume, "LONG.WAV");
    recording.declared_byte_length -= 1;
    let (result, sink) = import_plain(&volume, &recording);
    assert!(matches!(result, Err(ImportError::GrewDuringCopy { .. })));
    assert_eq!(sink.committed_bytes(), b"");
}

#[cfg(unix)]
#[test]
fn a_symlink_escaping_the_volume_is_refused_and_never_read() {
    let fixture = volume(&[]);
    let outside = fixture.root.parent().unwrap().join("plaud_outside.wav");
    std::fs::write(&outside, wav_with_serial(PCM)).unwrap();
    let link = fixture.root.join("NOTES/LINK.WAV");
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    let volume = mounted(&fixture.root, "PLAUD_NOTE", true);
    let recording = ScannedRecording {
        path: link,
        file_name: "LINK.WAV".to_owned(),
        directory: "NOTES".to_owned(),
        declared_byte_length: 10,
    };
    let (result, sink) = import_plain(&volume, &recording);
    let _ = std::fs::remove_file(outside);
    assert_eq!(result, Err(ImportError::EscapesVolume));
    assert_eq!(sink.state(), SinkState::Aborted);
}

#[cfg(unix)]
#[test]
fn a_path_resolving_outside_the_volume_through_a_directory_link_is_refused() {
    let fixture = volume(&[]);
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("X.WAV"), wav_with_serial(PCM)).unwrap();
    std::os::unix::fs::symlink(outside.path(), fixture.root.join("NOTES/DIR")).unwrap();
    let volume = mounted(&fixture.root, "PLAUD_NOTE", true);
    let recording = ScannedRecording {
        path: fixture.root.join("NOTES/DIR/X.WAV"),
        file_name: "X.WAV".to_owned(),
        directory: "NOTES".to_owned(),
        declared_byte_length: 10,
    };
    let (result, _) = import_plain(&volume, &recording);
    assert_eq!(result, Err(ImportError::EscapesVolume));
}

#[cfg(unix)]
#[test]
fn lost_read_permission_is_a_distinct_typed_error() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = volume(&[("NOTES/LOCKED.WAV", &wav_with_serial(PCM))]);
    let volume = mounted(&fixture.root, "PLAUD_NOTE", true);
    let recording = find(&volume, "LOCKED.WAV");
    let path = fixture.root.join("NOTES/LOCKED.WAV");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::File::open(&path).is_ok() {
        return; // running as root: permissions cannot be withdrawn
    }
    let (result, sink) = import_plain(&volume, &recording);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(result, Err(ImportError::PermissionLost));
    assert_eq!(sink.state(), SinkState::Aborted);
}

#[test]
fn an_entry_that_is_not_a_regular_file_is_unreadable() {
    let fixture = volume(&[]);
    std::fs::create_dir(fixture.root.join("NOTES/DIR.WAV")).unwrap();
    let volume = mounted(&fixture.root, "PLAUD_NOTE", true);
    let recording = ScannedRecording {
        path: fixture.root.join("NOTES/DIR.WAV"),
        file_name: "DIR.WAV".to_owned(),
        directory: "NOTES".to_owned(),
        declared_byte_length: 4,
    };
    let (result, _) = import_plain(&volume, &recording);
    assert_eq!(result, Err(ImportError::Unreadable));
}

/// A sink that deletes the whole volume after receiving its first bytes.
struct EjectingSink {
    inner: MemoryStagingSink,
    eject: Option<tempfile::TempDir>,
}

impl Write for EjectingSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(buf)?;
        drop(self.eject.take());
        Ok(written)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl StagingSink for EjectingSink {
    fn commit(&mut self) -> io::Result<()> {
        self.inner.commit()
    }
    fn abort(&mut self) {
        self.inner.abort();
    }
}

#[test]
fn a_volume_deleted_mid_copy_commits_nothing_and_reports_the_disappearance() {
    let big = wav_with_serial(&vec![7u8; 400 * 1024]);
    let Fixture { dir, root } = volume(&[("NOTES/BIG.WAV", &big)]);
    let volume = mounted(&root, "PLAUD_NOTE", true);
    let recording = find(&volume, "BIG.WAV");
    let mut sink = EjectingSink {
        inner: MemoryStagingSink::new(),
        eject: Some(dir),
    };

    let result = import_recording(&volume, &recording, &mut sink, None);

    assert_eq!(result, Err(ImportError::VolumeDisappeared));
    assert_eq!(sink.inner.state(), SinkState::Aborted);
    assert_eq!(sink.inner.committed_bytes(), b"");
}

#[test]
fn a_staging_write_failure_is_reported_and_aborts() {
    struct Failing(bool);
    impl Write for Failing {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::StorageFull, "full"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl StagingSink for Failing {
        fn commit(&mut self) -> io::Result<()> {
            Ok(())
        }
        fn abort(&mut self) {
            self.0 = true;
        }
    }
    let fixture = volume(&[("NOTES/A.WAV", &wav_with_serial(PCM))]);
    let volume = mounted(&fixture.root, "PLAUD_NOTE", true);
    let mut sink = Failing(false);
    let result = import_recording(&volume, &find(&volume, "A.WAV"), &mut sink, None);
    assert_eq!(
        result,
        Err(ImportError::Staging(io::ErrorKind::StorageFull))
    );
    assert!(sink.0, "abort must be called");
}

#[test]
fn the_sanitized_derivative_drops_the_serial_and_keeps_audio_bit_exact() {
    let wav = wav_with_serial(PCM);
    let fixture = volume(&[("NOTES/REC.WAV", &wav)]);
    let volume = mounted(&fixture.root, "PLAUD_NOTE", true);
    let recording = find(&volume, "REC.WAV");
    let before = snapshot(&fixture.root);
    let mut staging = MemoryStagingSink::new();
    let mut derivative = MemoryStagingSink::new();

    let imported =
        import_recording(&volume, &recording, &mut staging, Some(&mut derivative)).expect("ok");

    let derived = imported.derivative.expect("derivative");
    let out = derivative.committed_bytes();
    assert_eq!(derived.quality_class, QualityClass::LocalDerivative);
    assert_eq!(
        imported.quality_class,
        QualityClass::OriginalDeviceRecording
    );
    assert_ne!(out, wav.as_slice());
    assert_ne!(derived.media.sha256, imported.media.sha256);
    assert_eq!(derived.derived_from, imported.media.sha256);
    assert_eq!(derived.media.sha256, Sha256Digest::of(out));
    assert_eq!(derived.media.byte_length, out.len() as u64);
    assert!(!contains(out, SERIAL.as_bytes()), "serial must be stripped");
    assert!(contains(staging.committed_bytes(), SERIAL.as_bytes()));
    assert_eq!(staging.committed_bytes(), wav.as_slice());
    assert_eq!(
        derived.dropped_chunks,
        vec!["LIST".to_owned(), "PLDN".to_owned()]
    );

    // RIFF structure: header, fmt, data (bit-exact), consistent size.
    assert_eq!(&out[..4], b"RIFF");
    assert_eq!(&out[8..12], b"WAVE");
    assert_eq!(
        u32::from_le_bytes(out[4..8].try_into().unwrap()) as usize,
        out.len() - 8
    );
    assert_eq!(&out[12..16], b"fmt ");
    assert_eq!(&out[36..40], b"data");
    assert_eq!(
        u32::from_le_bytes(out[40..44].try_into().unwrap()) as usize,
        PCM.len()
    );
    assert_eq!(&out[44..], PCM);
    assert_eq!(snapshot(&fixture.root), before);
}

#[test]
fn a_large_derivative_streams_across_read_chunks_with_exact_audio() {
    let pcm: Vec<u8> = (0..300_001u32).map(|i| (i % 251) as u8).collect();
    let wav = wav_with_serial(&pcm);
    let fixture = volume(&[("NOTES/BIG.WAV", &wav)]);
    let volume = mounted(&fixture.root, "PLAUD_NOTE", true);
    let mut staging = MemoryStagingSink::new();
    let mut derivative = MemoryStagingSink::new();
    import_recording(
        &volume,
        &find(&volume, "BIG.WAV"),
        &mut staging,
        Some(&mut derivative),
    )
    .expect("ok");
    let out = derivative.committed_bytes();
    assert_eq!(&out[44..44 + pcm.len()], pcm.as_slice());
    assert_eq!(out.len(), 44 + pcm.len() + 1, "odd data chunk is padded");
    assert!(!contains(out, SERIAL.as_bytes()));
}

#[test]
fn a_derivative_is_not_available_for_an_asr_sidecar() {
    let fixture = volume(&[("NOTES/A.ASR", &asr_sidecar(2))]);
    let volume = mounted(&fixture.root, "PLAUD_NOTE", true);
    let mut staging = MemoryStagingSink::new();
    let mut derivative = MemoryStagingSink::new();
    let result = import_recording(
        &volume,
        &find(&volume, "A.ASR"),
        &mut staging,
        Some(&mut derivative),
    );
    assert_eq!(result, Err(ImportError::DerivativeUnavailable));
    assert_eq!(staging.committed_bytes(), b"");
    assert_eq!(derivative.committed_bytes(), b"");
}

#[test]
fn a_wav_without_a_data_chunk_cannot_be_sanitized_and_commits_nothing() {
    let mut wav = b"RIFF".to_vec();
    wav.extend_from_slice(&4u32.to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    let fixture = volume(&[("NOTES/HOLLOW.WAV", &wav)]);
    let volume = mounted(&fixture.root, "PLAUD_NOTE", true);
    let mut staging = MemoryStagingSink::new();
    let mut derivative = MemoryStagingSink::new();
    let result = import_recording(
        &volume,
        &find(&volume, "HOLLOW.WAV"),
        &mut staging,
        Some(&mut derivative),
    );
    assert!(
        matches!(result, Err(ImportError::InvalidWav(_))),
        "{result:?}"
    );
    assert_eq!(staging.state(), SinkState::Aborted, "no partial archive");
    assert_eq!(derivative.state(), SinkState::Aborted);
}

#[test]
fn without_a_request_no_derivative_exists() {
    let fixture = volume(&[("NOTES/R.WAV", &wav_with_serial(PCM))]);
    let volume = mounted(&fixture.root, "PLAUD_NOTE", true);
    let (result, _) = import_plain(&volume, &find(&volume, "R.WAV"));
    assert!(result.unwrap().derivative.is_none());
}

#[test]
fn the_import_result_surfaces_host_write_behaviour_and_the_exact_notice() {
    let fixture = volume(&[("NOTES/R.WAV", &wav_with_serial(PCM))]);
    std::fs::create_dir(fixture.root.join(".Spotlight-V100")).unwrap();
    let volume = mounted(&fixture.root, "PLAUD_NOTE", false);
    let (result, _) = import_plain(&volume, &find(&volume, "R.WAV"));
    let imported = result.unwrap();
    assert!(!imported.host_write_behaviour.permits_zero_write_claim());
    assert_eq!(imported.host_write_notice(), HOST_WRITE_NOTICE);
    assert_eq!(
        HOST_WRITE_NOTICE,
        "Your computer, not PLAUDON, may write its own metadata (such as Spotlight or Trash folders) to the device when it mounts the device read-write. PLAUDON only reads from the device and never writes to it."
    );
}

#[test]
fn a_volume_value_for_an_unsupported_model_cannot_be_imported_from() {
    let wav = wav_with_serial(PCM);
    let fixture = volume(&[("NOTES/R.WAV", &wav)]);
    let note = mounted(&fixture.root, "PLAUD_NOTE", true);
    let recording = find(&note, "R.WAV");
    for model in [
        VendorModel::NotePro,
        VendorModel::NotePinS,
        VendorModel::Unknown,
    ] {
        let forged = MountedVolume {
            model,
            ..note.clone()
        };
        let (result, sink) = import_plain(&forged, &recording);
        let Err(ImportError::ModelRefused(refusal)) = result else {
            panic!("{model:?} must be refused, got {result:?}");
        };
        assert_eq!(
            refusal,
            UsbRefusal {
                model,
                message: model.usb_refusal().unwrap().message
            }
        );
        assert_eq!(sink.state(), SinkState::Aborted);
        assert!(model.mount_instruction().is_err());
    }
}

#[test]
fn unsupported_models_are_refused_at_inspection_with_no_mount_instruction() {
    let fixture = volume(&[]);
    for label in ["PLAUD_NOTE_PRO", "PLAUD_NOTEPIN_S", "SOMETHING_ELSE"] {
        assert!(MountedVolume::inspect(&fixture.root, label, true).is_err());
        assert!(VendorModel::from_volume_label(label)
            .mount_instruction()
            .is_err());
    }
}

#[test]
fn a_wrong_layout_volume_never_reaches_import() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("REC.WAV"), wav_with_serial(PCM)).unwrap();
    assert!(MountedVolume::inspect(dir.path(), "PLAUD_NOTE", true).is_err());
}
