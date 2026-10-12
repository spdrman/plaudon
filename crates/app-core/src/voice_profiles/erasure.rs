//! "Forget everyone": wholesale erasure of the biometric tier
//! (`VOICE_PROFILES_SPEC` VP-095, VP-132; ADR-0080; threat-model row TM-046).
//!
//! The action has three parts, and only all three together are the erasure the
//! button promises:
//!
//! 1. **The key goes first.** Destroying
//!    [`SecretScope::VoiceProfileKey`](plaudon_security::secrets::SecretScope::VoiceProfileKey)
//!    is the only step that reaches ciphertexts this process does not control —
//!    the ones inside an encrypted portable backup the user wrote last month.
//!    It happens before the rows precisely because it is the irreversible part:
//!    if the machine dies between steps, the remaining rows are already
//!    undecryptable noise rather than a live profile store.
//! 2. **The rows go next**, in one transaction, with one journal event carrying
//!    the counts the user was shown.
//! 3. **The feature switches off.** An erasure that left matching armed would
//!    start rebuilding the thing the user just destroyed at the next job.
//!
//! The roster survives all three. Names are not voiceprints (spec §12.2A), and
//! deleting people is a separate, ordinary action.
//!
//! # What this cannot do, stated where the code is
//!
//! Names already written into transcripts, exports and shared files are plain
//! text and are not recalled. Cryptographic erasure makes ciphertexts
//! permanently undecryptable; it does not scrub SSD sectors, snapshots or
//! someone else's backup medium, and we make no forensic-erasure claim (base
//! spec §4.6). [`FORGET_ALL_LIMITS`] is that disclosure, carried in the type so
//! the UI cannot show counts without it.

use plaudon_domain::time::Timestamp;
use plaudon_persistence::voice_profiles::{
    ForgetAllOutcome, VoiceEnrollmentRepository, VoiceProfileGateRepository,
};
use plaudon_persistence::Database;

use super::store::{EmbeddingCipher, VoiceProfileResult};

/// What "forget everyone" cannot reach, in the ADR-0072 disclosure style.
pub const FORGET_ALL_LIMITS: &[&str] = &[
    "Every voiceprint in this library is destroyed, and the key that could read one is destroyed with it. Copies inside encrypted backups you already wrote stay on disk as unreadable bytes.",
    "Names are not voiceprints. The people list, the speaker labels on your recordings and every transcript stay exactly as they are; deleting a person is a separate action.",
    "Names you already exported — in transcripts, summaries or files you shared — are ordinary text. We cannot recall them.",
    "We delete rows and ask the operating system to free the space. On an SSD, in a snapshotted filesystem or on a backed-up volume we cannot guarantee the bytes are unrecoverable.",
];

/// What the user must see before confirming (VP-095).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgetAllDisclosure {
    /// Enrolments that will be destroyed.
    pub enrollments: u32,
    /// Exemplar ciphertexts that will be destroyed.
    pub exemplars: u32,
    /// Suggestions that will be destroyed.
    pub suggestions: u32,
    /// Roster entries that will survive.
    pub people_kept: u32,
    /// Whether a data key currently exists to destroy.
    pub key_present: bool,
    /// What the erasure cannot reach.
    pub limits: Vec<String>,
}

/// What the erasure did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgetAllReceipt {
    /// Enrolments destroyed.
    pub enrollments_destroyed: u32,
    /// Exemplar ciphertexts destroyed.
    pub exemplars_destroyed: u32,
    /// Suggestions destroyed.
    pub suggestions_destroyed: u32,
    /// Roster entries left alone.
    pub people_kept: u32,
    /// Whether the data key is gone from the OS credential store.
    pub key_destroyed: bool,
    /// Whether biometric matching is now off.
    pub feature_disabled: bool,
}

/// Destroys the biometric tier wholesale.
#[derive(Debug, Clone)]
pub struct VoiceProfileEraser {
    enrollments: VoiceEnrollmentRepository,
    gate: VoiceProfileGateRepository,
    cipher: EmbeddingCipher,
}

impl VoiceProfileEraser {
    /// Wrap a library and its key store.
    #[must_use]
    pub fn new(db: Database, cipher: EmbeddingCipher) -> Self {
        Self {
            enrollments: VoiceEnrollmentRepository::new(db.clone()),
            gate: VoiceProfileGateRepository::new(db),
            cipher,
        }
    }

    /// Count what the erasure would destroy. Reads only.
    ///
    /// The counts come from the same tables the erasure empties, so the number
    /// the user confirms is the number that happens — the disclosure is not a
    /// separately maintained estimate.
    ///
    /// # Errors
    ///
    /// [`super::VoiceProfileError::Persistence`] when the library fails. A locked
    /// credential store is *not* an error here: the disclosure reports
    /// `key_present: false` and the erasure refuses later, rather than
    /// pretending there is nothing to destroy.
    pub async fn disclose(&self) -> VoiceProfileResult<ForgetAllDisclosure> {
        let counts = self.enrollments.tier_totals().await?;
        Ok(ForgetAllDisclosure {
            enrollments: counts.enrollments,
            exemplars: counts.exemplars,
            suggestions: counts.suggestions,
            people_kept: counts.people_kept,
            key_present: self.cipher.check_available().unwrap_or(false),
            limits: FORGET_ALL_LIMITS
                .iter()
                .map(|line| (*line).to_owned())
                .collect(),
        })
    }

    /// Destroy every enrolment, exemplar and suggestion, destroy the data key
    /// and switch biometric matching off.
    ///
    /// Running it twice is not an error: the second run destroys nothing,
    /// destroys a key that is already gone, and leaves the feature off.
    ///
    /// # Errors
    ///
    /// [`super::VoiceProfileError::StoreLocked`] when the credential store cannot be
    /// reached — the key would survive the rows, which is exactly the outcome
    /// VP-095 promises not to leave behind, so nothing is deleted and the user
    /// is told to unlock. [`super::VoiceProfileError::Persistence`] when the library
    /// fails.
    pub async fn forget_all(&self, now: Timestamp) -> VoiceProfileResult<ForgetAllReceipt> {
        self.cipher.destroy_key()?;
        let outcome: ForgetAllOutcome = self.enrollments.forget_all(now).await?;
        self.gate.disable().await?;
        Ok(ForgetAllReceipt {
            enrollments_destroyed: outcome.enrollments,
            exemplars_destroyed: outcome.exemplars,
            suggestions_destroyed: outcome.suggestions,
            people_kept: outcome.people_kept,
            key_destroyed: true,
            feature_disabled: true,
        })
    }
}
