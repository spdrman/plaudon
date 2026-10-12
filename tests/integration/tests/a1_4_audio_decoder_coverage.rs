//! A1.4: the decoder coverage matrix and the hostile-input contract.
//!
//! Every fixture under `tests/fixtures/audio/` is a real file produced by
//! `scripts/make-audio-fixtures.sh`, not a hand-written byte string, so the
//! matrix this suite asserts is a measurement rather than an assumption.

use std::path::{Path, PathBuf};

use plaudon_integration_tests::spikes::audio_decoder::{
    probe_and_decode, sniff_container, support_row, SniffedContainer, SpikeError,
};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repository root")
        .join("tests/fixtures/audio")
}

fn valid(name: &str) -> PathBuf {
    fixtures().join("valid").join(name)
}

fn hostile(name: &str) -> PathBuf {
    fixtures().join("hostile").join(name)
}

#[test]
fn the_mandatory_v1_formats_decode() {
    // Spec 8.1 accepts at minimum WAV, MP3 and M4A/AAC. FLAC comes free with
    // the same decoder set and is recorded because it costs nothing.
    for (name, container) in [
        ("tone-mono-16k.wav", SniffedContainer::Wav),
        ("tone-stereo-44k.wav", SniffedContainer::Wav),
        ("tone-mono-16k.mp3", SniffedContainer::Mp3),
        ("tone-mono-16k.m4a", SniffedContainer::Mp4),
        ("tone-mono-16k.flac", SniffedContainer::Flac),
        ("tone-mono-16k.ogg", SniffedContainer::OggVorbis),
    ] {
        let (probe, pcm) = probe_and_decode(&valid(name))
            .unwrap_or_else(|error| panic!("{name} failed to decode: {error:?}"));
        assert_eq!(probe.container, container, "{name}");
        assert!(probe.decoded_frames > 0, "{name} decoded no frames");
        assert!(!pcm.samples.is_empty(), "{name} produced no samples");
        let duration = probe.duration_ms.unwrap_or_default();
        assert!(
            (900..=1_200).contains(&duration),
            "{name} duration was {duration}ms"
        );
    }
}

#[test]
fn opus_is_the_coverage_gap_and_it_is_reported_not_silently_skipped() {
    // The A1.4 finding: the Symphonia feature set we enable demuxes Ogg but
    // ships no Opus decoder, and PLAUD device sidecars are Opus. The failure
    // must be a typed UnsupportedCodec, never an empty transcript.
    let result = probe_and_decode(&valid("tone-mono-16k.opus"));
    assert!(
        matches!(result, Err(SpikeError::UnsupportedCodec { .. })),
        "expected an unsupported-codec error, got {result:?}"
    );
    assert_eq!(
        sniff_container(&std::fs::read(valid("tone-mono-16k.opus")).expect("read")),
        SniffedContainer::OggOpus,
        "the container is recognized even though the codec is not decodable"
    );
}

#[test]
fn a_device_sidecar_shaped_file_fails_closed() {
    // A raw `.ASR` Opus sidecar has no container at all, so it cannot even be
    // sniffed. It must never be treated as decodable audio.
    let result = probe_and_decode(&hostile("device-sidecar.asr"));
    assert!(
        matches!(
            result,
            Err(SpikeError::UnrecognizedContainer | SpikeError::UnsupportedCodec { .. })
        ),
        "got {result:?}"
    );
}

#[test]
fn a_misleading_extension_is_a_typed_mismatch_not_a_decode() {
    let result = probe_and_decode(&hostile("mp3-bytes-named.wav"));
    assert_eq!(
        result,
        Err(SpikeError::ExtensionContentMismatch {
            declared: "wav".to_owned(),
            sniffed: SniffedContainer::Mp3,
        })
    );
}

#[test]
fn every_hostile_fixture_fails_closed_without_panicking() {
    let mut seen = 0;
    for entry in std::fs::read_dir(fixtures().join("hostile")).expect("hostile fixtures") {
        let path = entry.expect("entry").path();
        if !path.is_file() {
            continue;
        }
        seen += 1;
        let result = probe_and_decode(&path);
        assert!(
            result.is_err(),
            "{} was accepted; a hostile fixture must fail closed",
            path.display()
        );
    }
    assert!(
        seen >= 10,
        "the hostile corpus should cover every documented case, saw {seen}"
    );
}

#[test]
fn specific_hostile_shapes_map_to_the_expected_errors() {
    assert_eq!(
        probe_and_decode(&hostile("zero-length.wav")),
        Err(SpikeError::ZeroLength)
    );
    assert_eq!(
        probe_and_decode(&hostile("text-named.mp3")),
        Err(SpikeError::UnrecognizedContainer)
    );
    // A header with no samples declares 32 KiB of data it does not have.
    assert_eq!(
        probe_and_decode(&hostile("header-only.wav")),
        Err(SpikeError::DeclaredSizeMismatch {
            declared: 32_078,
            actual: 44
        })
    );
    // A stream cut in half is caught by the same declared-length check, which
    // is ours: Symphonia decodes a truncated RIFF stream happily and simply
    // returns fewer samples.
    assert_eq!(
        probe_and_decode(&hostile("truncated.wav")),
        Err(SpikeError::DeclaredSizeMismatch {
            declared: 32_078,
            actual: 16_039
        })
    );
    // Padding before a real header defeats the magic-byte check, as it should.
    assert_eq!(
        probe_and_decode(&hostile("leading-nulls.wav")),
        Err(SpikeError::UnrecognizedContainer)
    );
}

#[test]
fn a_forged_declared_length_is_rejected_before_any_allocation() {
    // Both files declare enormous sizes in their headers while carrying only
    // 32 KiB of real data, and both are refused before a decoder is built.
    for name in ["forged-riff-size.wav", "forged-data-length.wav"] {
        let path = hostile(name);
        let on_disk = std::fs::metadata(&path).expect("metadata").len();
        assert!(on_disk < 64 * 1024, "{name} should be small on disk");
        let error = probe_and_decode(&path).expect_err("must be refused");
        let SpikeError::DeclaredSizeMismatch { declared, actual } = error else {
            panic!("{name} produced {error:?}");
        };
        assert_eq!(actual, on_disk);
        assert!(
            declared > actual,
            "{name} declared {declared} with {actual} present"
        );
    }
}

#[test]
fn a_forged_sample_rate_is_refused_rather_than_reported() {
    // Symphonia reports the header value verbatim, so the plausibility check
    // has to be ours. A 4 GHz sample rate must never reach a caller.
    assert_eq!(
        probe_and_decode(&hostile("forged-sample-rate.wav")),
        Err(SpikeError::ImplausibleSampleRate { declared: u32::MAX })
    );
}

#[test]
fn the_canonical_derivative_is_byte_deterministic() {
    for name in [
        "tone-mono-16k.wav",
        "tone-stereo-44k.wav",
        "tone-mono-16k.mp3",
    ] {
        let (_, first) = probe_and_decode(&valid(name)).expect("decode");
        let (_, second) = probe_and_decode(&valid(name)).expect("decode");
        assert_eq!(
            first.digest(),
            second.digest(),
            "{name} is not deterministic"
        );
        assert_eq!(
            first.samples, second.samples,
            "{name} sample data differs between runs"
        );
    }
}

#[test]
fn stereo_channels_are_preserved_rather_than_collapsed() {
    let (probe, pcm) = probe_and_decode(&valid("tone-stereo-44k.wav")).expect("decode");
    assert_eq!(probe.channels, Some(2));
    assert_eq!(pcm.channels, 2);
    assert_eq!(pcm.sample_rate_hz, 44_100);
    assert_eq!(
        pcm.samples.len() % 2,
        0,
        "an interleaved stereo buffer must hold whole frames"
    );

    let (mono_probe, mono_pcm) = probe_and_decode(&valid("tone-mono-16k.wav")).expect("decode");
    assert_eq!(mono_probe.channels, Some(1));
    assert_eq!(mono_pcm.channels, 1);
    assert_ne!(
        pcm.digest(),
        mono_pcm.digest(),
        "a stereo derivative must not equal the mono one"
    );
}

#[test]
fn decoding_never_modifies_the_source_file() {
    let path = valid("tone-mono-16k.wav");
    let before = std::fs::read(&path).expect("read");
    let modified_before = std::fs::metadata(&path).expect("metadata").modified().ok();
    probe_and_decode(&path).expect("decode");
    assert_eq!(std::fs::read(&path).expect("read"), before);
    assert_eq!(
        std::fs::metadata(&path).expect("metadata").modified().ok(),
        modified_before
    );
}

#[test]
fn the_support_matrix_matches_the_committed_finding() {
    // The matrix in docs/spikes/A1.4-audio-decoder-coverage.md must describe
    // what the decoder actually does, so the document cannot go stale.
    let document = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("root")
            .join("docs/spikes/A1.4-audio-decoder-coverage.md"),
    )
    .expect("the A1.4 finding must be committed");

    for name in [
        "tone-mono-16k.wav",
        "tone-mono-16k.mp3",
        "tone-mono-16k.m4a",
        "tone-mono-16k.flac",
        "tone-mono-16k.ogg",
        "tone-mono-16k.opus",
    ] {
        let row = support_row(&valid(name));
        assert!(
            document.contains(name),
            "the finding does not mention {name}"
        );
        let row_line = document
            .lines()
            .find(|line| line.contains(name))
            .unwrap_or_else(|| panic!("the finding has no row for {name}"));
        let claimed_decoded = row_line.contains("| decoded |");
        assert_eq!(
            row.decoded, claimed_decoded,
            "the finding claims a different outcome for {name} than the decoder produces"
        );
    }
}
