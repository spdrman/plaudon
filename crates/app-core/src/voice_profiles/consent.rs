//! The consent gate: the three-level activation and per-enrolment attestation
//! of §12.2 (`VOICE_PROFILES_SPEC` VP-003, VP-120..VP-123, VP-126..VP-128;
//! ADR-0080).
//!
//! # Three levels, each strictly stronger
//!
//! 1. **The toggle** ([`plaudon_persistence::voice_profiles::VoiceProfileGate::enabled`]),
//!    off in every library nobody turned it on in.
//! 2. **The activation disclosure** ([`ACTIVATION_DISCLOSURE`]), a *versioned*
//!    text. Accepting it records the version; publishing a newer one re-prompts
//!    rather than inheriting the old acceptance (VP-121), and a restore that
//!    carried somebody else's profiles re-prompts in the library that received
//!    them (VP-078).
//! 3. **The per-enrolment attestation** ([`EnrolmentAttestation`]), in which the
//!    user states that the named person has agreed, picks the retention term and
//!    may record the basis. It is journalled with the disclosure version and the
//!    timestamp (§7.2 `voice_profile_events`).
//!
//! # Why the proof is a value
//!
//! [`ConsentProof`] has no public constructor: the only way to obtain one is
//! [`authorise_biometric_work`], which is the gate. Every function that can
//! reach an embedder takes one, so "the gate structurally precedes embedding"
//! is a property of the type system rather than of a review — the manual
//! roster path cannot construct the token, and therefore cannot call the
//! embedding code at all (VP-011, VP-215, §17.1).
//!
//! # What the text has to say
//!
//! The disclosure carries the two-tier sentence (VP-128) and the
//! responsibility sentence (VP-120) verbatim, in this module, so the UI, the
//! exported consent-request template and the published documents quote one
//! source instead of three paraphrases.

use plaudon_persistence::voice_profiles::{VoiceProfileGate, MAX_RETENTION_MONTHS};
use serde::{Deserialize, Serialize};

use super::pipeline::CONSENT_DISCLOSURE_VERSION;

/// Default retention of a new enrolment, in months (VP-130).
///
/// Twelve is the Texas CUBI outer bound; the user may shorten it per profile
/// and may never lengthen it, which the schema's `CHECK` also enforces.
pub const DEFAULT_RETENTION_MONTHS: u8 = MAX_RETENTION_MONTHS;

/// Longest consent note the gate accepts.
///
/// The note is the user's own record of the basis they obtained ("signed
/// release, 2026-03-02"). It is free text with a bound, not a form.
pub const MAX_CONSENT_NOTE_CHARS: usize = 500;

/// The one-sentence boundary of §12.2A, quoted verbatim where users see it
/// (VP-128).
pub const TWO_TIER_SENTENCE: &str = "Keeping a list of names and assigning them to recordings \
     needs no special consent; computing a voiceprint to recognise someone automatically does.";

/// The sentence that puts the legal duty where it belongs (VP-120).
pub const RESPONSIBILITY_SENTENCE: &str = "People you enrol must agree first. In Illinois, Texas, \
     Washington, the EU and other places, their explicit — sometimes written — consent is legally \
     required, and that is your responsibility, not this software's.";

/// The versioned activation disclosure (VP-120, VP-121).
///
/// Carried as a value rather than assembled in the UI so that the text the
/// user accepted is the text this build records a version for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActivationDisclosure {
    /// Version string recorded with an acceptance.
    pub version: &'static str,
    /// Heading of the gate.
    pub title: &'static str,
    /// Body paragraphs, in reading order.
    pub body: &'static [&'static str],
    /// The §12.2A boundary sentence (VP-128).
    pub two_tier_sentence: &'static str,
    /// The user-responsibility sentence (VP-120).
    pub responsibility_sentence: &'static str,
    /// Retention term a new enrolment gets unless the user shortens it.
    pub retention_default_months: u8,
}

/// The disclosure this build shows and records acceptances against.
pub const ACTIVATION_DISCLOSURE: ActivationDisclosure = ActivationDisclosure {
    version: CONSENT_DISCLOSURE_VERSION,
    title: "Turn on voice matching?",
    body: &[
        "Voice matching computes a voiceprint — a numeric summary of how somebody's voice \
         sounds — for each person you choose to enrol, and compares it with the speakers in \
         recordings you import later.",
        "A voiceprint is biometric data. Everything stays on this machine: voiceprints are \
         computed here, encrypted here with a key held by your operating system's credential \
         store, and never sent anywhere. We cannot read them, and neither can anyone else \
         without your machine.",
        "A match is a suggestion, never proof of identity. Nothing is renamed until you \
         confirm it, at any score.",
        "Enrolments expire after 12 months without use unless you shorten that, and you can \
         delete one person's voiceprint — or every voiceprint and the key that reads them — at \
         any time. Deleting voiceprints keeps your people list and your transcripts.",
    ],
    two_tier_sentence: TWO_TIER_SENTENCE,
    responsibility_sentence: RESPONSIBILITY_SENTENCE,
    retention_default_months: DEFAULT_RETENTION_MONTHS,
};

/// Why the activation disclosure has to be shown again (VP-121).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum DisclosureReason {
    /// Nobody has ever accepted it in this library.
    NeverAccepted,
    /// A newer disclosure was published since the acceptance.
    VersionChanged {
        /// Version the user accepted.
        accepted: String,
        /// Version in force now.
        required: String,
    },
    /// A restore brought voice profiles into this library; the gate is re-run
    /// here before any of them is used (VP-078).
    RestoredLibrary,
}

/// Where the consent gate stands right now (§12.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ConsentGateState {
    /// The toggle is off, which is its default (VP-003). The roster still
    /// works, and nothing biometric is computed or stored.
    Off,
    /// The toggle is on but the disclosure has to be accepted first.
    DisclosureRequired {
        /// Why it has to be shown.
        #[serde(flatten)]
        reason: DisclosureReason,
    },
    /// Biometric work is authorised in this library.
    Active,
}

impl ConsentGateState {
    /// Stable short form, for a DTO's discriminator and a job's step label.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::DisclosureRequired { .. } => "disclosure_required",
            Self::Active => "active",
        }
    }

    /// Whether biometric work is authorised.
    #[must_use]
    pub const fn is_active(&self) -> bool {
        matches!(self, Self::Active)
    }
}

/// Read the gate's state out of what the library stored.
///
/// The order matters: an off toggle is reported as off even when the
/// disclosure is also stale, because turning the feature on is the first thing
/// the user would have to do and the gate never reports a condition the user
/// cannot act on yet.
#[must_use]
pub fn gate_state(gate: &VoiceProfileGate) -> ConsentGateState {
    if !gate.enabled {
        return ConsentGateState::Off;
    }
    if gate.restore_pending_consent {
        return ConsentGateState::DisclosureRequired {
            reason: DisclosureReason::RestoredLibrary,
        };
    }
    match gate.consent_version.as_deref() {
        None => ConsentGateState::DisclosureRequired {
            reason: DisclosureReason::NeverAccepted,
        },
        Some(accepted) if accepted != ACTIVATION_DISCLOSURE.version => {
            ConsentGateState::DisclosureRequired {
                reason: DisclosureReason::VersionChanged {
                    accepted: accepted.to_owned(),
                    required: ACTIVATION_DISCLOSURE.version.to_owned(),
                },
            }
        }
        Some(_) => ConsentGateState::Active,
    }
}

/// Why a biometric operation is refused before it starts.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConsentError {
    /// Biometric matching is off (VP-003). The roster is unaffected.
    #[error("voice matching is off, so nothing biometric may be computed")]
    MatchingOff,
    /// The activation disclosure has to be accepted (again).
    #[error("the voice-matching disclosure has to be accepted before anything is computed")]
    DisclosureRequired(DisclosureReason),
    /// The user did not attest that the person agreed (VP-120 level 3).
    #[error("an enrolment needs the user's attestation that this person has agreed")]
    NotAttested,
    /// The attestation quoted a disclosure version this build does not show.
    #[error("the attestation quotes disclosure {quoted}, and this build shows {required}")]
    AttestationVersionMismatch {
        /// Version the caller quoted.
        quoted: String,
        /// Version in force.
        required: String,
    },
    /// Retention outside the 1..=12 month range VP-130 allows.
    #[error("retention is between 1 and {MAX_RETENTION_MONTHS} months, got {0}")]
    RetentionOutOfRange(u8),
    /// The consent note is longer than the gate accepts.
    #[error("a consent note is at most {MAX_CONSENT_NOTE_CHARS} characters")]
    NoteTooLong,
}

/// Proof that the consent gate of §12.2 was passed.
///
/// There is no public constructor and no `Default`: the single way to make one
/// is [`authorise_biometric_work`]. Functions that can reach an embedder or a
/// stored embedding take one by reference, so a caller on the manual roster
/// path has nothing to hand them (VP-011, VP-215).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsentProof {
    disclosure_version: String,
}

impl ConsentProof {
    /// The disclosure version the authorisation was granted under, which is
    /// what an enrolment records (§7.2).
    #[must_use]
    pub fn disclosure_version(&self) -> &str {
        &self.disclosure_version
    }
}

/// The gate itself: authorise biometric work, or say why not.
///
/// # Errors
///
/// [`ConsentError::MatchingOff`] when the toggle is off and
/// [`ConsentError::DisclosureRequired`] when the disclosure is unaccepted,
/// stale or waiting after a restore.
pub fn authorise_biometric_work(gate: &VoiceProfileGate) -> Result<ConsentProof, ConsentError> {
    match gate_state(gate) {
        ConsentGateState::Off => Err(ConsentError::MatchingOff),
        ConsentGateState::DisclosureRequired { reason } => {
            Err(ConsentError::DisclosureRequired(reason))
        }
        ConsentGateState::Active => Ok(ConsentProof {
            disclosure_version: ACTIVATION_DISCLOSURE.version.to_owned(),
        }),
    }
}

/// What the user attested before one person's voice was enrolled (VP-120
/// level 3, VP-122, VP-123).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct EnrolmentAttestation {
    /// Disclosure version the user was shown.
    pub disclosure_version: String,
    /// Whether the user states the person has agreed. An enrolment with
    /// `false` here never happens: the gate refuses rather than recording a
    /// negative attestation and continuing.
    pub person_attested: bool,
    /// Retention term chosen, 1..=12 months.
    pub retention_months: u8,
    /// The user's own note of the consent basis, when they recorded one.
    pub consent_note: Option<String>,
}

impl EnrolmentAttestation {
    /// An attestation for this build's disclosure with the default retention.
    #[must_use]
    pub fn attested() -> Self {
        Self {
            disclosure_version: ACTIVATION_DISCLOSURE.version.to_owned(),
            person_attested: true,
            retention_months: DEFAULT_RETENTION_MONTHS,
            consent_note: None,
        }
    }

    /// Check the attestation against the gate the user passed.
    ///
    /// # Errors
    ///
    /// [`ConsentError::NotAttested`] when the user did not attest,
    /// [`ConsentError::AttestationVersionMismatch`] when it quotes another
    /// disclosure, [`ConsentError::RetentionOutOfRange`] and
    /// [`ConsentError::NoteTooLong`] for the two bounded fields.
    pub fn validate(&self, proof: &ConsentProof) -> Result<(), ConsentError> {
        if !self.person_attested {
            return Err(ConsentError::NotAttested);
        }
        if self.disclosure_version != proof.disclosure_version() {
            return Err(ConsentError::AttestationVersionMismatch {
                quoted: self.disclosure_version.clone(),
                required: proof.disclosure_version().to_owned(),
            });
        }
        if self.retention_months == 0 || self.retention_months > MAX_RETENTION_MONTHS {
            return Err(ConsentError::RetentionOutOfRange(self.retention_months));
        }
        if let Some(note) = &self.consent_note {
            if note.chars().count() > MAX_CONSENT_NOTE_CHARS {
                return Err(ConsentError::NoteTooLong);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        authorise_biometric_work, gate_state, ActivationDisclosure, ConsentError, ConsentGateState,
        DisclosureReason, EnrolmentAttestation, ACTIVATION_DISCLOSURE, MAX_CONSENT_NOTE_CHARS,
        RESPONSIBILITY_SENTENCE, TWO_TIER_SENTENCE,
    };
    use plaudon_persistence::voice_profiles::VoiceProfileGate;

    fn active() -> VoiceProfileGate {
        VoiceProfileGate {
            enabled: true,
            consent_version: Some(ACTIVATION_DISCLOSURE.version.to_owned()),
            consent_recorded_at: Some(0),
            restore_pending_consent: false,
        }
    }

    #[test]
    fn a_library_nobody_touched_is_off() {
        assert_eq!(
            gate_state(&VoiceProfileGate::default()),
            ConsentGateState::Off
        );
        assert_eq!(
            authorise_biometric_work(&VoiceProfileGate::default()),
            Err(ConsentError::MatchingOff)
        );
    }

    #[test]
    fn the_toggle_alone_does_not_pass_the_gate() {
        let gate = VoiceProfileGate {
            enabled: true,
            ..VoiceProfileGate::default()
        };
        assert_eq!(
            gate_state(&gate),
            ConsentGateState::DisclosureRequired {
                reason: DisclosureReason::NeverAccepted
            }
        );
        assert!(matches!(
            authorise_biometric_work(&gate),
            Err(ConsentError::DisclosureRequired(
                DisclosureReason::NeverAccepted
            ))
        ));
    }

    #[test]
    fn a_disclosure_version_bump_re_prompts() {
        let gate = VoiceProfileGate {
            enabled: true,
            consent_version: Some("voice-matching-v0".to_owned()),
            consent_recorded_at: Some(0),
            restore_pending_consent: false,
        };
        let state = gate_state(&gate);
        assert_eq!(
            state,
            ConsentGateState::DisclosureRequired {
                reason: DisclosureReason::VersionChanged {
                    accepted: "voice-matching-v0".to_owned(),
                    required: ACTIVATION_DISCLOSURE.version.to_owned(),
                }
            }
        );
        assert!(authorise_biometric_work(&gate).is_err());
    }

    #[test]
    fn a_restore_waiting_for_the_gate_is_not_active() {
        let gate = VoiceProfileGate {
            restore_pending_consent: true,
            ..active()
        };
        assert_eq!(
            gate_state(&gate),
            ConsentGateState::DisclosureRequired {
                reason: DisclosureReason::RestoredLibrary
            }
        );
    }

    #[test]
    fn an_accepted_current_disclosure_authorises_biometric_work() {
        assert_eq!(gate_state(&active()), ConsentGateState::Active);
        let proof = authorise_biometric_work(&active()).expect("the gate is open");
        assert_eq!(proof.disclosure_version(), ACTIVATION_DISCLOSURE.version);
    }

    #[test]
    fn an_unattested_enrolment_is_refused() {
        let proof = authorise_biometric_work(&active()).expect("gate");
        let attestation = EnrolmentAttestation {
            person_attested: false,
            ..EnrolmentAttestation::attested()
        };
        assert_eq!(attestation.validate(&proof), Err(ConsentError::NotAttested));
    }

    #[test]
    fn an_attestation_quoting_another_disclosure_is_refused() {
        let proof = authorise_biometric_work(&active()).expect("gate");
        let attestation = EnrolmentAttestation {
            disclosure_version: "voice-matching-v0".to_owned(),
            ..EnrolmentAttestation::attested()
        };
        assert!(matches!(
            attestation.validate(&proof),
            Err(ConsentError::AttestationVersionMismatch { .. })
        ));
    }

    #[test]
    fn retention_may_be_shortened_and_never_lengthened() {
        let proof = authorise_biometric_work(&active()).expect("gate");
        for months in 1..=12u8 {
            let attestation = EnrolmentAttestation {
                retention_months: months,
                ..EnrolmentAttestation::attested()
            };
            assert!(attestation.validate(&proof).is_ok(), "{months}");
        }
        for months in [0u8, 13, 240] {
            let attestation = EnrolmentAttestation {
                retention_months: months,
                ..EnrolmentAttestation::attested()
            };
            assert_eq!(
                attestation.validate(&proof),
                Err(ConsentError::RetentionOutOfRange(months))
            );
        }
    }

    #[test]
    fn an_over_long_consent_note_is_refused() {
        let proof = authorise_biometric_work(&active()).expect("gate");
        let attestation = EnrolmentAttestation {
            consent_note: Some("x".repeat(MAX_CONSENT_NOTE_CHARS + 1)),
            ..EnrolmentAttestation::attested()
        };
        assert_eq!(attestation.validate(&proof), Err(ConsentError::NoteTooLong));
    }

    #[test]
    fn the_disclosure_carries_the_two_tier_and_responsibility_sentences() {
        let ActivationDisclosure {
            body,
            two_tier_sentence,
            responsibility_sentence,
            retention_default_months,
            ..
        } = ACTIVATION_DISCLOSURE;
        assert_eq!(two_tier_sentence, TWO_TIER_SENTENCE);
        assert_eq!(responsibility_sentence, RESPONSIBILITY_SENTENCE);
        assert_eq!(retention_default_months, 12);
        assert!(body.len() >= 3);
        // ADR-0082 wording: a match is a suggestion, and the text never
        // claims the feature identifies anybody.
        let joined = body.join(" ").to_lowercase();
        for forbidden in ["identified", "verified", "recognised as", "proves"] {
            assert!(!joined.contains(forbidden), "{forbidden} in {joined}");
        }
        assert!(joined.contains("suggestion"));
    }
}
