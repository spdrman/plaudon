//! The codec registry every decode path in this workspace uses (task O1.6).
//!
//! Symphonia's `symphonia::default::get_codecs()` is a global built from the
//! `symphonia` feature flags, and no feature flag adds Opus — the codec does
//! not exist in the crate (`docs/spikes/O1.2-audio-playback-and-opus.md` §2).
//! [`registry`] is that same set plus [`crate::opus::OpusAudioDecoder`], so
//! probing, canonical-PCM decode, waveform peaks and playback all see one
//! support matrix instead of each caller assembling its own.
//!
//! Callers must use this function rather than `symphonia::default::get_codecs`;
//! `tests/integration/tests/workspace_layout.rs` has no opinion on that, so
//! the rule is kept by review and by the tests that decode the committed Opus
//! fixtures through each entry point.

use std::sync::LazyLock;

use symphonia::core::codecs::CodecRegistry;

use crate::opus::OpusAudioDecoder;

/// Symphonia's enabled codecs plus Opus, built once on first use. The
/// registry is immutable afterwards, so it is safe to share across the decode
/// threads the job engine runs.
static REGISTRY: LazyLock<CodecRegistry> = LazyLock::new(|| {
    let mut registry = CodecRegistry::new();
    symphonia::default::register_enabled_codecs(&mut registry);
    registry.register_all::<OpusAudioDecoder>();
    registry
});

/// The process-wide codec registry: Symphonia's enabled codecs plus Opus.
#[must_use]
pub fn registry() -> &'static CodecRegistry {
    &REGISTRY
}

#[cfg(test)]
mod tests {
    use symphonia::core::codecs::{
        CODEC_TYPE_AAC, CODEC_TYPE_FLAC, CODEC_TYPE_MP3, CODEC_TYPE_OPUS, CODEC_TYPE_PCM_S16LE,
        CODEC_TYPE_VORBIS,
    };

    use super::registry;

    #[test]
    fn opus_is_registered_alongside_the_symphonia_codecs() {
        let registry = registry();
        for (codec, name) in [
            (CODEC_TYPE_OPUS, "opus"),
            (CODEC_TYPE_VORBIS, "vorbis"),
            (CODEC_TYPE_MP3, "mp3"),
            (CODEC_TYPE_AAC, "aac"),
            (CODEC_TYPE_FLAC, "flac"),
        ] {
            let descriptor = registry
                .get_codec(codec)
                .unwrap_or_else(|| panic!("{name} must be registered"));
            assert_eq!(descriptor.short_name, name);
        }
        assert!(registry.get_codec(CODEC_TYPE_PCM_S16LE).is_some());
    }

    #[test]
    fn the_opus_entry_can_instantiate_a_decoder() {
        // Registration is only useful if `make` succeeds; a descriptor with a
        // failing constructor would still appear in `get_codec`.
        let mut params = symphonia::core::codecs::CodecParameters::new();
        params
            .for_codec(CODEC_TYPE_OPUS)
            .with_sample_rate(48_000)
            .with_channels(symphonia::core::audio::Channels::FRONT_LEFT)
            .with_extra_data(Box::from(crate::opus::tests_support::OPUS_HEAD_MONO));
        assert!(registry()
            .make(&params, &symphonia::core::codecs::DecoderOptions::default())
            .is_ok());
    }
}
