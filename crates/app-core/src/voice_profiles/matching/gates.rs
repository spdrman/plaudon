//! Minimum-speech gates (VP-065, VP-066, VP-023).
//!
//! Short-utterance error is the quiet failure mode of speaker recognition: the
//! ERes2NetV2 paper measures roughly 1.4–2.3× nominal EER at 3 s and 2.0–3.9×
//! at 2 s. The product answer is to refuse rather than to score badly — VP-023
//! requires "an explicit 'not enough clean speech' outcome, never a
//! low-quality embedding".
//!
//! Every duration here is **clean single-speaker** speech. Overlapping speech
//! never contributes (VP-022), so a cluster that is mostly overlap arrives with
//! a small clean total and is refused by exactly the same rule — that is VP-066
//! with no extra machinery.
//!
//! Architectural minima are not reliability minima: a ResNet34 will happily
//! consume 25 ms. These gates are the reliability ones.

use serde::{Deserialize, Serialize};

/// Clean speech required before an embedding is computed at all (VP-065).
pub const MIN_EMBED_SPEECH_MS: u32 = 1_000;

/// Clean speech a cluster needs before it may be identified (VP-065).
pub const MIN_IDENTIFY_SPEECH_MS: u32 = 3_000;

/// Clean speech a profile needs across its exemplars to be enrolled (VP-065).
pub const MIN_ENROL_SPEECH_MS: u32 = 10_000;

/// Which minimum applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpeechGate {
    /// Computing one embedding.
    Embed,
    /// Identifying a cluster against enrolled profiles.
    Identify,
    /// Enrolling a person (total across exemplars).
    Enrol,
}

impl SpeechGate {
    /// The minimum, in milliseconds of clean single-speaker speech.
    #[must_use]
    pub const fn minimum_ms(self) -> u32 {
        match self {
            SpeechGate::Embed => MIN_EMBED_SPEECH_MS,
            SpeechGate::Identify => MIN_IDENTIFY_SPEECH_MS,
            SpeechGate::Enrol => MIN_ENROL_SPEECH_MS,
        }
    }

    /// Short label used in the refusal message.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            SpeechGate::Embed => "compute an embedding",
            SpeechGate::Identify => "identify a cluster",
            SpeechGate::Enrol => "enrol a person",
        }
    }

    /// Check a clean-speech total against this gate.
    pub fn check(self, clean_speech_ms: u32) -> Result<(), InsufficientSpeech> {
        if clean_speech_ms >= self.minimum_ms() {
            return Ok(());
        }
        Err(InsufficientSpeech {
            gate: self,
            required_ms: self.minimum_ms(),
            available_ms: clean_speech_ms,
        })
    }
}

/// Not enough clean single-speaker speech (VP-023, VP-066).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "not enough clean speech to {}: {available_ms} ms of {required_ms} ms",
    gate.label()
)]
pub struct InsufficientSpeech {
    /// Which minimum was not met.
    pub gate: SpeechGate,
    /// Milliseconds the gate requires.
    pub required_ms: u32,
    /// Milliseconds of clean speech that were available.
    pub available_ms: u32,
}

#[cfg(test)]
mod tests {
    use super::{
        InsufficientSpeech, SpeechGate, MIN_EMBED_SPEECH_MS, MIN_ENROL_SPEECH_MS,
        MIN_IDENTIFY_SPEECH_MS,
    };

    #[test]
    fn the_three_minima_are_the_specified_durations() {
        assert_eq!(SpeechGate::Embed.minimum_ms(), MIN_EMBED_SPEECH_MS);
        assert_eq!(SpeechGate::Identify.minimum_ms(), MIN_IDENTIFY_SPEECH_MS);
        assert_eq!(SpeechGate::Enrol.minimum_ms(), MIN_ENROL_SPEECH_MS);
        assert_eq!(MIN_EMBED_SPEECH_MS, 1_000);
        assert_eq!(MIN_IDENTIFY_SPEECH_MS, 3_000);
        assert_eq!(MIN_ENROL_SPEECH_MS, 10_000);
    }

    #[test]
    fn a_gate_passes_at_its_minimum_and_refuses_one_millisecond_below() {
        for gate in [SpeechGate::Embed, SpeechGate::Identify, SpeechGate::Enrol] {
            assert_eq!(gate.check(gate.minimum_ms()), Ok(()));
            assert_eq!(
                gate.check(gate.minimum_ms() - 1),
                Err(InsufficientSpeech {
                    gate,
                    required_ms: gate.minimum_ms(),
                    available_ms: gate.minimum_ms() - 1,
                })
            );
            assert_eq!(
                gate.check(0),
                Err(InsufficientSpeech {
                    gate,
                    required_ms: gate.minimum_ms(),
                    available_ms: 0,
                })
            );
        }
    }

    #[test]
    fn the_refusal_says_not_enough_clean_speech() {
        let error = SpeechGate::Identify.check(1_500).expect_err("refusal");
        assert_eq!(
            error.to_string(),
            "not enough clean speech to identify a cluster: 1500 ms of 3000 ms"
        );
    }
}
