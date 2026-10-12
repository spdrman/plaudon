//! Voice profiles: the policy layer over the people roster and its optional
//! biometric tier (`VOICE_PROFILES_SPEC`, ADR-0080, ADR-0082).
//!
//! # The two tiers, and why the split is in the type system
//!
//! The roster — `plaudon_persistence::people::PeopleRepository` — is a contact
//! list: typed names, aliases, optional email and organisation. It is ordinary
//! personal data (spec §12.2A), it needs no consent gate, no retention clock
//! and no key, and it keeps working when the OS credential store is locked or
//! absent. Nothing in this module is required to use it.
//!
//! The biometric tier — enrolments, exemplars, suggestions — only exists after
//! the consent gate, and an embedding only ever exists at rest as
//! XChaCha20-Poly1305 ciphertext under the library's
//! [`SecretScope::VoiceProfileKey`](plaudon_security::secrets::SecretScope::VoiceProfileKey).
//! That is what [`store`] implements. The key never leaves the OS vault except
//! into a zeroizing buffer, there is no file-backed fallback, and an
//! unavailable vault produces [`VoiceProfileError::StoreLocked`] — ADR-0014's
//! rule, applied to this feature.
//!
//! [`matching`] is the other half: the pure scoring layer of spec §6. It has no
//! I/O, no model and no state, and three rules hold throughout it.
//!
//! * **Off by default (VP-003).** Nothing there runs unless a caller hands it
//!   embeddings, and embeddings only exist after the consent gate.
//! * **A match is a suggestion, never an identity (ADR-0082, VP-060).** The
//!   output is a score and a band. There is no auto-assignment tier at any
//!   score, so no code path there can change a transcript label.
//! * **An embedding never leaves the process (VP-002, VP-110).** Vectors are
//!   carried by [`matching::Embedding`], which deliberately implements neither
//!   `serde::Serialize` nor `serde::Deserialize` and whose `Debug` prints the
//!   checkpoint and the dimension only. Everything that *is* serializable —
//!   scores, bands, person identifiers — is score metadata, which
//!   `plaudon_security::response::assert_no_secret_in_response` accepts and
//!   whose `VoiceEmbedding` shape would reject a smuggled vector.
//!
//! That last rule is a compile-time one. A match report is serializable:
//!
//! ```
//! use plaudon_app_core::voice_profiles::matching::MatchReport;
//! fn requires_serialize<T: serde::Serialize>() {}
//! requires_serialize::<MatchReport>();
//! ```
//!
//! and the same assertion over [`matching::Embedding`] does not compile — see
//! the two `compile_fail` examples on that type.
//!
//! # Honest limits
//!
//! Encryption here protects data **at rest**, not data in use: while a match
//! job runs, plaintext vectors exist in process memory, and a privileged
//! attacker on an unlocked machine can read them. The control that matters
//! against that attacker is the platform's own (FileVault/BitLocker, ADR-0402),
//! plus the fact that the feature is off by default and computes nothing until
//! a user turns it on.
//!
//! # Lifecycle
//!
//! Two modules implement what §12.4 promises about the end of a profile's
//! life: [`retention`] expires what nobody uses (VP-130) and [`erasure`]
//! destroys everything at once, key included (VP-095). Both leave the roster
//! standing.

pub mod consent;
pub mod embedder;
pub mod enroll;
pub mod erasure;
pub mod matching;
pub mod pipeline;
pub mod retention;
pub mod store;

pub use consent::{
    authorise_biometric_work, gate_state, ActivationDisclosure, ConsentError, ConsentGateState,
    ConsentProof, DisclosureReason, EnrolmentAttestation, ACTIVATION_DISCLOSURE,
    DEFAULT_RETENTION_MONTHS, MAX_CONSENT_NOTE_CHARS, RESPONSIBILITY_SENTENCE, TWO_TIER_SENTENCE,
};
pub use embedder::{
    EmbedError, EmbedWindow, EmbedderCheckpoint, ScriptedEmbedder, SpeakerEmbedder,
    SCRIPTED_EMBEDDER_ID,
};
pub use enroll::{
    reseal_exemplars, EnrolError, EnrolmentSource, ExemplarOutcome, SplitOutcome,
    VoiceEnrolmentService, ENROL_SPEECH_MS, EXEMPLAR_SPEECH_MS,
};
pub use erasure::{ForgetAllDisclosure, ForgetAllReceipt, VoiceProfileEraser, FORGET_ALL_LIMITS};
pub use pipeline::{
    cluster_speech, profile_set_digest, reconciliation_from_assignments, ClusterSpeech,
    CorruptReconciliation, GateRefusal, MatchGate, SkippedCluster, SpeakerMatchError,
    SpeakerMatchOutcome, SpeakerMatchPipeline, SpeakerMatchPlan, SpeakerMatchRequest,
    SpeakerMatchStage, CONSENT_DISCLOSURE_VERSION, EMBED_WINDOW_MS,
};
pub use retention::{RetentionReport, VoiceRetentionService, TOMBSTONE_DAYS};
pub use store::{
    ClusterBinding, EmbeddingCipher, ExemplarBinding, SealedEmbedding, VoiceProfileError,
    VoiceProfileResult, EMBEDDING_KEY_BYTES, EMBEDDING_NONCE_BYTES, MAX_EMBEDDING_DIM,
};
