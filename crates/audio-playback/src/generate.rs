//! Building a peak pyramid from a media file.
//!
//! The decode is `plaudon_audio`'s: the same validation, the same streaming
//! decoder, the same deterministic resampler the transcription pipeline uses,
//! so the waveform and the transcript are looking at identical samples. The
//! only new part is the sink, which folds each block into the pyramid instead
//! of writing it anywhere — peak building never materialises the recording,
//! on disk or in memory.
//!
//! Opus used to be the one gap: Symphonia has no Opus decoder for any
//! feature set, and device `.ASR` derivatives are remuxed to Ogg Opus by
//! `plaudon_plaud_protocol::remux_to_ogg_opus`, so such a file was refused
//! here. O1.6 (#214) closed it inside `plaudon_audio` — a pure-Rust decoder
//! registered on `plaudon_audio::codecs::registry()`, which this decode path
//! already goes through — so an Opus recording now builds a pyramid like any
//! other, and the test below holds that.

use std::path::{Path, PathBuf};

use plaudon_ai_contracts::job::{JobContext, Progress};
use plaudon_audio::decode::{
    decode_to_canonical_pcm, CanonicalPcmProfile, ChannelPolicy, PcmSampleFormat, PcmSink,
};
use plaudon_audio::error::AudioError;
use plaudon_audio::validate::{validate_media, ValidatedMedia, ValidationRequest};

use crate::peaks::{PeakBuilder, PeakParams, PeakPyramid};

/// Why a pyramid could not be built.
#[derive(Debug, thiserror::Error)]
pub enum PeakBuildError {
    /// The file failed `plaudon_audio` validation (unreadable, unrecognised
    /// container, path policy refusal).
    #[error("media at {path} could not be validated: {source}")]
    Validate {
        /// File that was rejected.
        path: PathBuf,
        /// Underlying failure.
        #[source]
        source: AudioError,
    },
    /// The file could not be decoded: a codec no bundled decoder handles, or
    /// a stream that fails part-way through.
    #[error("media at {path} could not be decoded: {source}")]
    Decode {
        /// File that was rejected.
        path: PathBuf,
        /// Underlying failure.
        #[source]
        source: AudioError,
    },
    /// The caller cancelled the build.
    #[error("peak build cancelled")]
    Cancelled,
}

/// A pyramid and the content hash it is keyed on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltPeaks {
    /// The pyramid.
    pub pyramid: PeakPyramid,
    /// SHA-256 of the source media bytes, computed during validation.
    pub content_hash: [u8; 32],
}

/// Validate `path` and return it with its content hash.
///
/// Separate from [`fold_media`] because the hash is the cache key: a worker
/// that already knows the hash can skip straight to the cache, and one that
/// does not can still check the cache after validation instead of decoding
/// the whole recording first.
pub fn validate_for_peaks(path: &Path) -> Result<(ValidatedMedia, [u8; 32]), PeakBuildError> {
    let media = validate_media(path, &ValidationRequest::manual_import()).map_err(|source| {
        PeakBuildError::Validate {
            path: path.to_path_buf(),
            source,
        }
    })?;
    let hash = *media.sha256().as_bytes();
    Ok((media, hash))
}

/// Decode validated media and fold it into a pyramid.
pub fn fold_media(
    media: &ValidatedMedia,
    params: PeakParams,
    ctx: &JobContext,
) -> Result<PeakPyramid, PeakBuildError> {
    let params = if params.is_valid() {
        params
    } else {
        PeakParams::default()
    };
    if ctx.is_cancelled() {
        return Err(PeakBuildError::Cancelled);
    }
    let profile = CanonicalPcmProfile {
        sample_rate_hz: params.analysis_rate_hz,
        channel_policy: ChannelPolicy::DownmixToMono,
        sample_format: PcmSampleFormat::F32Le,
    };
    let mut sink = PeakSink {
        builder: PeakBuilder::new(params),
    };
    match decode_to_canonical_pcm(media, &profile, &mut sink, ctx) {
        Ok(_) => {}
        Err(AudioError::Cancelled) => return Err(PeakBuildError::Cancelled),
        Err(source) => {
            return Err(PeakBuildError::Decode {
                path: media.path().to_path_buf(),
                source,
            })
        }
    }
    ctx.report(Progress::fraction(1.0));
    Ok(sink.builder.finish())
}

/// Fold `path` into a peak pyramid, honouring `ctx`'s cancellation and
/// reporting its progress.
pub fn build_peaks(
    path: &Path,
    params: PeakParams,
    ctx: &JobContext,
) -> Result<BuiltPeaks, PeakBuildError> {
    let (media, content_hash) = validate_for_peaks(path)?;
    let pyramid = fold_media(&media, params, ctx)?;
    Ok(BuiltPeaks {
        pyramid,
        content_hash,
    })
}

/// A [`PcmSink`] that keeps nothing: every block is folded straight into the
/// pyramid and dropped.
#[derive(Debug)]
struct PeakSink {
    builder: PeakBuilder,
}

impl PcmSink for PeakSink {
    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), AudioError> {
        self.builder.push_f32le(bytes);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use plaudon_domain::ids::JobId;

    use super::*;

    /// A committed media fixture, canonicalised for the path guard.
    fn fixture_path(name: &str) -> std::path::PathBuf {
        std::fs::canonicalize(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/audio/valid")
                .join(name),
        )
        .expect("committed fixture")
    }

    /// A 16-bit PCM WAV of `seconds` of a 220 Hz tone at 16 kHz, mono.
    fn write_wav(path: &Path, seconds: f64) {
        let rate = 16_000u32;
        let frames = (f64::from(rate) * seconds) as u32;
        let data_bytes = frames * 2;
        let mut file = std::fs::File::create(path).expect("create wav");
        file.write_all(b"RIFF").expect("write");
        file.write_all(&(36 + data_bytes).to_le_bytes())
            .expect("write");
        file.write_all(b"WAVEfmt ").expect("write");
        file.write_all(&16u32.to_le_bytes()).expect("write");
        file.write_all(&1u16.to_le_bytes()).expect("write");
        file.write_all(&1u16.to_le_bytes()).expect("write");
        file.write_all(&rate.to_le_bytes()).expect("write");
        file.write_all(&(rate * 2).to_le_bytes()).expect("write");
        file.write_all(&2u16.to_le_bytes()).expect("write");
        file.write_all(&16u16.to_le_bytes()).expect("write");
        file.write_all(b"data").expect("write");
        file.write_all(&data_bytes.to_le_bytes()).expect("write");
        for frame in 0..frames {
            let phase = f64::from(frame) * 220.0 * std::f64::consts::TAU / f64::from(rate);
            let sample = (phase.sin() * 0.5 * f64::from(i16::MAX)) as i16;
            file.write_all(&sample.to_le_bytes()).expect("write");
        }
        file.flush().expect("flush");
    }

    #[test]
    fn a_real_wav_decodes_into_a_pyramid_with_the_expected_duration() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("tone.wav");
        write_wav(&path, 2.0);
        let ctx = JobContext::new(JobId::new());
        let built = build_peaks(&path, PeakParams::default(), &ctx).expect("build");
        assert_eq!(built.pyramid.total_frames(), 32_000);
        assert_eq!(built.pyramid.duration().as_millis(), 2_000);
        assert!(built.pyramid.levels().len() > 2);
        let loudest = built.pyramid.levels()[0]
            .buckets()
            .iter()
            .map(|bucket| bucket.amplitude())
            .fold(0.0f32, f32::max);
        assert!((loudest - 0.5).abs() < 0.02, "amplitude {loudest}");
        assert_eq!(built.content_hash.len(), 32);
    }

    #[test]
    fn cancellation_before_the_decode_abandons_the_build() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("tone.wav");
        write_wav(&path, 1.0);
        let ctx = JobContext::new(JobId::new());
        ctx.cancel.cancel();
        assert!(matches!(
            build_peaks(&path, PeakParams::default(), &ctx),
            Err(PeakBuildError::Cancelled)
        ));
    }

    #[test]
    fn a_file_that_is_not_media_is_reported_not_silently_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, b"this is not audio").expect("write");
        let ctx = JobContext::new(JobId::new());
        assert!(matches!(
            build_peaks(&path, PeakParams::default(), &ctx),
            Err(PeakBuildError::Validate { .. })
        ));
    }

    /// O1.5 pinned the Opus gap here and said this test must be rewritten the
    /// day a decoder is added. O1.6 (#214) added one, so the fixture that was
    /// refused now produces a waveform.
    #[test]
    fn an_ogg_opus_recording_builds_a_pyramid() {
        // Canonicalised: the path guard refuses a `..` component outright,
        // which would make this test pass for the wrong reason.
        let fixture = fixture_path("tone-mono-16k.opus");
        let ctx = JobContext::new(JobId::new());
        let built = build_peaks(&fixture, PeakParams::default(), &ctx).expect("ogg opus");
        // One second of 440 Hz tone, analysed at the default rate.
        let duration = built.pyramid.duration().as_millis();
        assert!((950..=1_050).contains(&duration), "duration {duration} ms");
        let loudest = built.pyramid.levels()[0]
            .buckets()
            .iter()
            .map(|bucket| bucket.amplitude())
            .fold(0.0f32, f32::max);
        assert!(loudest > 0.05, "the waveform is flat: {loudest}");
        assert_eq!(built.content_hash.len(), 32);

        // Ogg Vorbis is unaffected by the new codec.
        let vorbis = fixture_path("tone-mono-16k.ogg");
        let built = build_peaks(&vorbis, PeakParams::default(), &ctx).expect("ogg vorbis");
        assert!(built.pyramid.total_frames() > 0);
    }

    #[test]
    fn invalid_parameters_fall_back_to_the_defaults() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("tone.wav");
        write_wav(&path, 0.5);
        let ctx = JobContext::new(JobId::new());
        let broken = PeakParams {
            analysis_rate_hz: 0,
            base_bucket_frames: 0,
            mip_factor: 1,
            max_levels: 0,
        };
        let built = build_peaks(&path, broken, &ctx).expect("build");
        assert_eq!(built.pyramid.params(), PeakParams::default());
    }
}
