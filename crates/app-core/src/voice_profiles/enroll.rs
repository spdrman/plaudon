//! Enrolment: turning a consented person plus one recording's cluster into
//! exemplars (`VOICE_PROFILES_SPEC` VP-007, VP-022, VP-023, VP-064, VP-065,
//! VP-094, VP-120..VP-123, VP-213c, VP-216; ADR-0080; threat-model row
//! TM-047).
//!
//! # The gate is a parameter, not a convention
//!
//! Every function here that can reach a [`SpeakerEmbedder`] takes a
//! [`ConsentProof`], and the only way to obtain one is
//! [`super::consent::authorise_biometric_work`]. A caller on the manual
//! roster path has nothing to hand it, so it cannot call this code at all —
//! VP-011's "nothing in the manual path may call into the embedding code",
//! expressed in the type system rather than in a comment. The runtime half of
//! the same claim is the scripted embedder that panics if it is ever called
//! (§17.1).
//!
//! # Only clean single-speaker speech, and only enough of it
//!
//! Spans come from the stored reconciliation, so a window is always inside a
//! stretch attributed to exactly one cluster (VP-022). The totals are checked
//! against [`SpeechGate`] before anything is embedded: an enrolment needs ten
//! seconds of clean speech, an added exemplar three. Below that the answer is
//! "not enough clean speech" (VP-023), never a low-quality voiceprint — the
//! poisoning control of TM-047.
//!
//! # Moving a voiceprint re-seals it
//!
//! A merge or a split repoints exemplars at another person. The ciphertext
//! authenticates its person as AEAD additional data, so moving a row means
//! opening and re-sealing the vector under the destination's binding
//! ([`reseal_exemplars`]); the plaintext exists for the duration of one
//! re-seal in a [`zeroize`](zeroize::Zeroizing)d buffer and never leaves the
//! process. Rewriting the column alone would leave the survivor holding
//! exemplars nothing can decrypt — a silent destruction of the data the merge
//! disclosure promised to keep.

use std::path::PathBuf;
use std::sync::Arc;

use plaudon_ai_contracts::job::JobContext;
use plaudon_domain::ids::{PersonId, RecordingId, VoiceExemplarId};
use plaudon_domain::time::Timestamp;
use plaudon_domain::transcript::TranscriptRevision;
use plaudon_persistence::error::PersistenceError;
use plaudon_persistence::speakers::SpeakerRepository;
use plaudon_persistence::voice_profiles::{
    EnrollmentDraft, ExemplarInsert, ResealedExemplar, StoredExemplar, VoiceEnrollmentRepository,
};
use uuid::Uuid;

use super::consent::{ConsentError, ConsentProof, EnrolmentAttestation};
use super::embedder::{EmbedError, EmbedWindow, SpeakerEmbedder};
use super::matching::{
    Embedding, InsufficientSpeech, SpeechGate, MIN_ENROL_SPEECH_MS, MIN_IDENTIFY_SPEECH_MS,
};
use super::pipeline::{cluster_speech, reconciliation_from_assignments, ClusterSpeech};
use super::store::{EmbeddingCipher, ExemplarBinding, SealedEmbedding, VoiceProfileError};

/// Where an exemplar's speech comes from: one cluster of one diarization run
/// of one recording (VP-094 "shows what audio span the exemplars come from").
#[derive(Debug, Clone)]
pub struct EnrolmentSource {
    /// Recording the speech is in.
    pub recording_id: RecordingId,
    /// Diarization run whose clusters are being named.
    pub diarization_run_id: Uuid,
    /// Revision the run was reconciled against.
    pub revision: TranscriptRevision,
    /// The cluster the user picked.
    pub cluster_key: String,
    /// Path of the media the embedder reads.
    pub media_path: PathBuf,
}

/// What one exemplar addition produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExemplarOutcome {
    /// The person the exemplar belongs to.
    pub person_id: PersonId,
    /// The stored exemplar.
    pub exemplar_id: VoiceExemplarId,
    /// How many exemplars the enrolment now has.
    pub exemplars: u32,
    /// Clean speech this exemplar was computed from.
    pub speech_ms: u32,
    /// Clean speech across every exemplar of the enrolment.
    pub total_speech_ms: u32,
    /// How many distinct recordings contributed, the device-diversity hint of
    /// VP-091/§6.5.
    pub source_recordings: u32,
    /// Embedding model identity the exemplar is bound to (VP-075).
    pub model_id: String,
    /// Embedding model version.
    pub model_version: String,
}

/// What a split moved (§3.5, VP-213c).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitOutcome {
    /// Enrolment the exemplars came from.
    pub source_person_id: PersonId,
    /// Enrolment they went to.
    pub target_person_id: PersonId,
    /// How many moved.
    pub exemplars_moved: u32,
    /// How many the source has left.
    pub exemplars_remaining: u32,
    /// Whether the source enrolment was deleted because nothing was left.
    pub source_enrollment_deleted: bool,
}

/// Why an enrolment, exemplar addition or split could not happen.
#[derive(Debug, thiserror::Error)]
pub enum EnrolError {
    /// The consent gate refused (§12.2). No audio was read.
    #[error(transparent)]
    Consent(#[from] ConsentError),
    /// Not enough clean single-speaker speech (VP-023, VP-065).
    #[error(transparent)]
    NotEnoughSpeech(#[from] InsufficientSpeech),
    /// The recording has no such cluster in that run.
    #[error("this recording's diarization run has no cluster {cluster_key}")]
    UnknownCluster {
        /// The cluster the caller named.
        cluster_key: String,
    },
    /// The person is not on the roster.
    #[error("no such person on the roster")]
    UnknownPerson,
    /// The person already has an enrolment; adding speech to it is
    /// `add_exemplar`.
    #[error("this person is already enrolled")]
    AlreadyEnrolled,
    /// The person has no enrolment to extend or split.
    #[error("this person has no voice enrolment")]
    NotEnrolled,
    /// A named exemplar does not exist or belongs to somebody else.
    #[error("no such exemplar in this enrolment")]
    UnknownExemplar,
    /// A split that would move nothing, or everything onto the same person.
    #[error("{0}")]
    InvalidSplit(String),
    /// The embedder failed or was cancelled.
    #[error(transparent)]
    Embed(#[from] EmbedError),
    /// The biometric store is locked or the ciphertext did not authenticate.
    #[error(transparent)]
    Store(#[from] VoiceProfileError),
    /// The library failed.
    #[error(transparent)]
    Persistence(#[from] PersistenceError),
    /// The stored reconciliation will not decode.
    #[error("the stored reconciliation is corrupt: {0}")]
    CorruptReconciliation(String),
}

/// Re-seal exemplars under another person's binding, for a merge or a split.
///
/// The plaintext vector exists only inside this function, in a zeroizing
/// buffer, for as long as one re-seal takes.
///
/// # Errors
///
/// [`VoiceProfileError::StoreLocked`] when the key store is unavailable and
/// [`VoiceProfileError::Tampered`] when a stored ciphertext does not
/// authenticate under the binding its own row claims.
pub fn reseal_exemplars(
    cipher: &EmbeddingCipher,
    exemplars: &[StoredExemplar],
    to: PersonId,
) -> Result<Vec<ResealedExemplar>, VoiceProfileError> {
    let mut resealed = Vec::with_capacity(exemplars.len());
    for row in exemplars {
        let sealed = SealedEmbedding {
            cipher: row.embedding_cipher.clone(),
            nonce: row.embedding_nonce.clone(),
            embedding_dim: row.embedding_dim,
        };
        let from_binding = ExemplarBinding {
            person_id: row.person_id,
            model_id: row.model_id.clone(),
            model_version: row.model_version.clone(),
            embedding_dim: row.embedding_dim,
        };
        let vector = cipher.open(&sealed, &from_binding)?;
        let to_binding = ExemplarBinding {
            person_id: to,
            ..from_binding
        };
        let sealed = cipher.seal(vector.as_slice(), &to_binding)?;
        resealed.push(ResealedExemplar {
            id: row.id,
            embedding_cipher: sealed.cipher,
            embedding_nonce: sealed.nonce,
        });
    }
    Ok(resealed)
}

/// The clean single-speaker speech one cluster of one diarization run offers.
///
/// Rows and arithmetic only: no audio is opened, no model is called and no
/// [`ConsentProof`] is taken, because the number a VP-094 meter shows has to
/// exist *before* the consent gate is answered — "ends at the consent gate
/// before anything is computed" is only meaningful if the user can see how
/// much speech there is while deciding. Enrolment itself reaches this through
/// [`VoiceEnrolmentService`], which then applies the [`SpeechGate`] the
/// operation requires.
///
/// # Errors
///
/// [`EnrolError::CorruptReconciliation`] when the stored reconciliation cannot
/// be rebuilt, [`EnrolError::UnknownCluster`] when the run has no such cluster
/// and [`EnrolError::Persistence`] when the rows cannot be read.
pub async fn cluster_clean_speech(
    speakers: &SpeakerRepository,
    revision: &TranscriptRevision,
    diarization_run_id: Uuid,
    cluster_key: &str,
) -> Result<ClusterSpeech, EnrolError> {
    let rows = speakers
        .assignments(revision.id, diarization_run_id)
        .await?;
    let reconciliation = reconciliation_from_assignments(revision.id, &rows)
        .map_err(|error| EnrolError::CorruptReconciliation(error.detail))?;
    cluster_speech(&reconciliation, revision)
        .into_iter()
        .find(|cluster| cluster.cluster_key == cluster_key)
        .ok_or_else(|| EnrolError::UnknownCluster {
            cluster_key: cluster_key.to_owned(),
        })
}

/// Enrolment, exemplar addition and splitting, over one library.
#[derive(Clone)]
pub struct VoiceEnrolmentService {
    voices: VoiceEnrollmentRepository,
    speakers: SpeakerRepository,
    cipher: EmbeddingCipher,
    embedder: Arc<dyn SpeakerEmbedder>,
}

impl std::fmt::Debug for VoiceEnrolmentService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VoiceEnrolmentService")
            .field("embedder", &self.embedder)
            .finish_non_exhaustive()
    }
}

impl VoiceEnrolmentService {
    /// Assemble the service over a library, its key store and an embedder.
    #[must_use]
    pub fn new(
        voices: VoiceEnrollmentRepository,
        speakers: SpeakerRepository,
        cipher: EmbeddingCipher,
        embedder: Arc<dyn SpeakerEmbedder>,
    ) -> Self {
        Self {
            voices,
            speakers,
            cipher,
            embedder,
        }
    }

    /// Attach an enrolment to a roster person from one recording's cluster
    /// (VP-094).
    ///
    /// The order is deliberate and is what the structural test checks: the
    /// consent proof and the attestation are validated, then the *stored*
    /// reconciliation is read to find the cluster's clean speech, then the
    /// VP-065 minimum is enforced, and only then is the embedder called.
    ///
    /// # Errors
    ///
    /// Everything in [`EnrolError`]; nothing is embedded unless the gate, the
    /// attestation, the cluster and the speech minimum all pass.
    pub async fn enroll(
        &self,
        proof: &ConsentProof,
        person_id: PersonId,
        attestation: &EnrolmentAttestation,
        source: &EnrolmentSource,
        ctx: &JobContext,
        now: Timestamp,
    ) -> Result<ExemplarOutcome, EnrolError> {
        attestation.validate(proof)?;
        if self.voices.enrollment(person_id).await?.is_some() {
            return Err(EnrolError::AlreadyEnrolled);
        }
        let speech = self.clean_speech(source, SpeechGate::Enrol).await?;

        self.voices
            .enroll(
                person_id,
                &EnrollmentDraft {
                    consent_version: proof.disclosure_version().to_owned(),
                    consent_note: attestation.consent_note.clone(),
                    retention_months: attestation.retention_months,
                },
                now,
            )
            .await
            .map_err(|error| match error {
                PersistenceError::RecordNotFound => EnrolError::UnknownPerson,
                other => EnrolError::Persistence(other),
            })?;

        self.embed_and_store(proof, person_id, source, &speech, ctx, now)
            .await
    }

    /// Extend an existing enrolment from another recording's cluster, which is
    /// what improves cross-device robustness (§6.5, VP-064).
    ///
    /// # Errors
    ///
    /// As [`VoiceEnrolmentService::enroll`], plus [`EnrolError::NotEnrolled`].
    pub async fn add_exemplar(
        &self,
        proof: &ConsentProof,
        person_id: PersonId,
        source: &EnrolmentSource,
        ctx: &JobContext,
        now: Timestamp,
    ) -> Result<ExemplarOutcome, EnrolError> {
        if self.voices.enrollment(person_id).await?.is_none() {
            return Err(EnrolError::NotEnrolled);
        }
        let speech = self.clean_speech(source, SpeechGate::Identify).await?;
        self.embed_and_store(proof, person_id, source, &speech, ctx, now)
            .await
    }

    /// Move some of one enrolment's exemplars onto another person (§3.5).
    ///
    /// The target person must already be on the roster; the caller creates the
    /// row, because a split is a roster edit plus a biometric move and the two
    /// have different consent stories. The target's enrolment is created here
    /// under the attestation the user gave for *them* — the exemplars turned
    /// out to be somebody else's voice, so somebody else has to have agreed.
    ///
    /// # Errors
    ///
    /// [`EnrolError::NotEnrolled`] when the source has no enrolment,
    /// [`EnrolError::UnknownExemplar`] when an id is not the source's,
    /// [`EnrolError::InvalidSplit`] when the move is empty or circular.
    pub async fn split(
        &self,
        proof: &ConsentProof,
        source_person_id: PersonId,
        target_person_id: PersonId,
        exemplar_ids: &[VoiceExemplarId],
        attestation: &EnrolmentAttestation,
        now: Timestamp,
    ) -> Result<SplitOutcome, EnrolError> {
        attestation.validate(proof)?;
        if source_person_id == target_person_id {
            return Err(EnrolError::InvalidSplit(
                "a split moves exemplars to another person".to_owned(),
            ));
        }
        if exemplar_ids.is_empty() {
            return Err(EnrolError::InvalidSplit(
                "a split moves at least one exemplar".to_owned(),
            ));
        }
        if self.voices.enrollment(source_person_id).await?.is_none() {
            return Err(EnrolError::NotEnrolled);
        }

        let owned = self.voices.exemplars(source_person_id).await?;
        let mut moving = Vec::with_capacity(exemplar_ids.len());
        for id in exemplar_ids {
            let row = owned
                .iter()
                .find(|row| row.id == *id)
                .ok_or(EnrolError::UnknownExemplar)?;
            moving.push(row.clone());
        }
        if moving.len() == owned.len() {
            return Err(EnrolError::InvalidSplit(
                "a split leaves speech behind; moving everything is a merge".to_owned(),
            ));
        }

        if self.voices.enrollment(target_person_id).await?.is_none() {
            self.voices
                .enroll(
                    target_person_id,
                    &EnrollmentDraft {
                        consent_version: proof.disclosure_version().to_owned(),
                        consent_note: attestation.consent_note.clone(),
                        retention_months: attestation.retention_months,
                    },
                    now,
                )
                .await
                .map_err(|error| match error {
                    PersistenceError::RecordNotFound => EnrolError::UnknownPerson,
                    other => EnrolError::Persistence(other),
                })?;
        }

        let resealed = reseal_exemplars(&self.cipher, &moving, target_person_id)?;
        let moved = self
            .voices
            .move_exemplars(source_person_id, target_person_id, &resealed, now)
            .await?;
        let remaining = u32::try_from(owned.len() - moving.len()).unwrap_or(u32::MAX);

        Ok(SplitOutcome {
            source_person_id,
            target_person_id,
            exemplars_moved: moved,
            exemplars_remaining: remaining,
            source_enrollment_deleted: false,
        })
    }

    /// The clean single-speaker speech of one cluster, checked against the
    /// gate that applies.
    ///
    /// Reads rows and arithmetic only — no audio, no model. Whatever happens
    /// here, no embedding exists yet.
    async fn clean_speech(
        &self,
        source: &EnrolmentSource,
        gate: SpeechGate,
    ) -> Result<ClusterSpeech, EnrolError> {
        let speech = cluster_clean_speech(
            &self.speakers,
            &source.revision,
            source.diarization_run_id,
            &source.cluster_key,
        )
        .await?;
        gate.check(speech.clean_speech_ms)?;
        Ok(speech)
    }

    /// Embed the cluster's windows, aggregate, seal and store one exemplar.
    ///
    /// Takes the consent proof by reference because this is the function that
    /// touches the embedder: the gate cannot be bypassed by calling a
    /// lower-level helper, because the lower-level helper also needs the proof
    /// (VP-011, VP-215, §17.1).
    async fn embed_and_store(
        &self,
        _proof: &ConsentProof,
        person_id: PersonId,
        source: &EnrolmentSource,
        speech: &ClusterSpeech,
        ctx: &JobContext,
        now: Timestamp,
    ) -> Result<ExemplarOutcome, EnrolError> {
        let checkpoint = self.embedder.checkpoint();
        let windows = speech.windows();
        let mut sum = vec![0.0f64; checkpoint.dim];
        let mut embedded = 0usize;
        for (start_ms, end_ms) in &windows {
            if ctx.is_cancelled() {
                return Err(EnrolError::Embed(EmbedError::Cancelled));
            }
            let window = EmbedWindow {
                media_path: source.media_path.clone(),
                cluster_key: source.cluster_key.clone(),
                start_ms: *start_ms,
                end_ms: *end_ms,
            };
            let embedding = self.embedder.embed_window(&window, ctx).await?;
            if embedding.dim() != checkpoint.dim {
                return Err(EnrolError::Embed(EmbedError::Malformed(format!(
                    "the checkpoint is {}-dimensional, the vector is {}-dimensional",
                    checkpoint.dim,
                    embedding.dim()
                ))));
            }
            for (slot, component) in sum.iter_mut().zip(embedding.unit()) {
                *slot += f64::from(*component);
            }
            embedded += 1;
        }
        if embedded == 0 {
            return Err(EnrolError::NotEnoughSpeech(InsufficientSpeech {
                gate: SpeechGate::Embed,
                required_ms: SpeechGate::Embed.minimum_ms(),
                available_ms: speech.clean_speech_ms,
            }));
        }

        let mean: Vec<f32> = sum.iter().map(|value| *value as f32).collect();
        let aggregate = Embedding::l2_normalise(checkpoint.binding.clone(), &mean)
            .map_err(|error| EnrolError::Embed(error.into()))?;
        let binding = ExemplarBinding {
            person_id,
            model_id: checkpoint.binding.model_id.clone(),
            model_version: checkpoint.binding.model_version.clone(),
            embedding_dim: u32::try_from(aggregate.dim()).unwrap_or(u32::MAX),
        };
        let sealed = self.cipher.seal(aggregate.unit(), &binding)?;

        let span = (
            windows.first().map(|(start, _)| *start),
            windows.last().map(|(_, end)| *end),
        );
        let exemplar_id = self
            .voices
            .add_exemplar(
                &ExemplarInsert {
                    person_id,
                    recording_id: Some(source.recording_id),
                    diarization_run_id: Some(source.diarization_run_id),
                    cluster_key: Some(source.cluster_key.clone()),
                    source_start_ms: span.0,
                    source_end_ms: span.1,
                    speech_ms: u64::from(speech.clean_speech_ms),
                    model_id: checkpoint.binding.model_id.clone(),
                    model_version: checkpoint.binding.model_version.clone(),
                    model_sha256: checkpoint.binding.model_sha256.to_hex(),
                    embedding_dim: sealed.embedding_dim,
                    embedding_cipher: sealed.cipher,
                    embedding_nonce: sealed.nonce,
                },
                now,
            )
            .await?;

        let stored = self.voices.exemplars(person_id).await?;
        let mut recordings: Vec<RecordingId> =
            stored.iter().filter_map(|row| row.recording_id).collect();
        recordings.sort_unstable();
        recordings.dedup();
        Ok(ExemplarOutcome {
            person_id,
            exemplar_id,
            exemplars: u32::try_from(stored.len()).unwrap_or(u32::MAX),
            speech_ms: speech.clean_speech_ms,
            total_speech_ms: stored
                .iter()
                .map(|row| u32::try_from(row.speech_ms).unwrap_or(u32::MAX))
                .fold(0u32, u32::saturating_add),
            source_recordings: u32::try_from(recordings.len()).unwrap_or(u32::MAX),
            model_id: checkpoint.binding.model_id.clone(),
            model_version: checkpoint.binding.model_version.clone(),
        })
    }
}

/// Clean speech an enrolment needs, re-exported so a UI meter and this module
/// quote one number (VP-094).
pub const ENROL_SPEECH_MS: u32 = MIN_ENROL_SPEECH_MS;

/// Clean speech an added exemplar needs.
pub const EXEMPLAR_SPEECH_MS: u32 = MIN_IDENTIFY_SPEECH_MS;
