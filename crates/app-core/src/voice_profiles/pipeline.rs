//! The resumable speaker-matching pipeline: stages 1–5 of §4
//! (`VOICE_PROFILES_SPEC` VP-020..VP-026, VP-100..VP-105; ADR-0080, ADR-0082).
//!
//! ```text
//!   diarize ──→ reconcile ──→ embed ──→ match ──→ suggest
//!      │            │           │         │          │
//!   run+turns   assignments   sealed   reports   speaker_suggestions
//!                             scratch
//! ```
//!
//! Each arrow is one `job_checkpoints` row: [`JobStore::prepare_checkpoint`]
//! before the work, [`JobStore::commit_checkpoint`] after its output is
//! durable. A process killed anywhere resumes at the first stage without a
//! commit mark, and a stage that already committed is never re-run — the store
//! refuses it, which is why the embedder's call count is the honest test of
//! "resumed at match" (VP-102).
//!
//! Four properties hold by construction.
//!
//! * **Nothing is auto-assigned.** The last stage writes `speaker_suggestions`
//!   rows and nothing else; no code path here can set a speaker's name
//!   (ADR-0082, VP-060).
//! * **Nothing is embedded unless the gate allows it.** [`SpeakerMatchPlan`]
//!   is computed from [`MatchGate`] *before* any audio is touched: with the
//!   feature off, the consent gate unpassed or nobody enrolled, the plan has no
//!   embed, match or suggest stage, and the embedder is never called — not even
//!   transiently (VP-003, VP-024).
//! * **Only clean single-speaker speech is embedded.** Windows come from spans
//!   the reconciliation attributed to exactly one cluster; overlap is excluded
//!   (VP-022), and a cluster under the identification minimum is reported as
//!   "not enough clean speech" instead of being embedded badly (VP-023,
//!   VP-065, VP-066).
//! * **A parked vector is ciphertext.** The embed stage's output has to survive
//!   a crash, so it is written to the 0901 scratch sealed under the library's
//!   voice-profile key and bound to the job, run and cluster it belongs to; the
//!   scratch is destroyed when the job settles (VP-071, VP-102).
//!
//! Cancellation is checked at every window boundary — the embedding loop is the
//! expensive one (VP-103) — and between stages; progress is reported as a
//! fraction for the coalescing sink to publish.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use plaudon_ai_contracts::job::{JobContext, Progress, ProgressSink};
use plaudon_ai_diarization::contract::{
    ClusterKey, DiarizationError, DiarizationInput, DiarizationProvider, DiarizationResult,
};
use plaudon_ai_diarization::provider::{run_guarded, RunPolicy};
use plaudon_domain::ids::{JobId, PersonId, RecordingId};
use plaudon_domain::time::Timestamp;
use plaudon_domain::transcript::TranscriptRevision;
use plaudon_jobs::error::JobError;
use plaudon_jobs::store::{Checkpoint, JobStore};
use plaudon_persistence::db::Database;
use plaudon_persistence::error::PersistenceError;
use plaudon_persistence::speaker_match::{
    ClusterEmbeddingInsert, MatchReportInsert, SpeakerMatchScratch,
};
use plaudon_persistence::speakers::{
    AssignmentInsert, DiarizationRunInsert, SpeakerRepository, SpeakerTurnInsert,
};
use plaudon_persistence::voice_profiles::{SuggestionInsert, VoiceEnrollmentRepository};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::embedder::{EmbedError, EmbedWindow, SpeakerEmbedder};
use super::matching::{
    ClusterEmbedding, Embedding, Exemplar, MatchError, MatchReport, Matcher, ProfileError,
    ProfileOffset, VoiceProfile, MIN_EMBED_SPEECH_MS, MIN_IDENTIFY_SPEECH_MS,
};
use super::store::{ClusterBinding, EmbeddingCipher, ExemplarBinding, SealedEmbedding};
use super::VoiceProfileError;
use crate::diarization::{reconcile, ReconcileError, Reconciliation, SegmentAssignment};

/// Version of the activation disclosure the consent gate requires (VP-120).
///
/// The disclosure *text* and the command that records acceptance land with the
/// roster and consent surface; this constant is the version the pipeline checks
/// an acceptance against, so a stale acceptance re-gates instead of silently
/// authorising a newer disclosure (VP-121).
pub const CONSENT_DISCLOSURE_VERSION: &str = "voice-matching-v1";

/// Length of one embedding window, in milliseconds.
///
/// Above the 1 s reliability floor of VP-065 with margin, and short enough that
/// a cancelled job stops promptly: the window boundary is the cancel point.
pub const EMBED_WINDOW_MS: u64 = 1_500;

/// Progress fraction reported once diarization is durable.
const PROGRESS_AFTER_DIARIZE: f32 = 0.25;
/// Progress fraction reported once reconciliation is durable.
const PROGRESS_AFTER_RECONCILE: f32 = 0.4;
/// Progress fraction reported once every cluster is embedded.
const PROGRESS_AFTER_EMBED: f32 = 0.8;
/// Progress fraction reported once scoring is durable.
const PROGRESS_AFTER_MATCH: f32 = 0.9;

/// One stage of the §4 pipeline, and one checkpoint (VP-020).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpeakerMatchStage {
    /// Stage 1: the diarization provider, through the ADR-0400 gate.
    Diarize,
    /// Stage 2: turns reconciled against the transcript revision.
    Reconcile,
    /// Stage 3: one embedding per cluster with enough clean speech.
    Embed,
    /// Stage 4: open-set scoring against the enrolled profiles.
    Match,
    /// Stage 5: suggestions persisted for the user to confirm or reject.
    Suggest,
}

impl SpeakerMatchStage {
    /// Every stage, in execution order.
    pub const ALL: &'static [Self] = &[
        Self::Diarize,
        Self::Reconcile,
        Self::Embed,
        Self::Match,
        Self::Suggest,
    ];

    /// The stages that run when the biometric gate is closed: diarization and
    /// reconciliation are ordinary per-recording work and carry no biometrics.
    pub const WITHOUT_MATCHING: &'static [Self] = &[Self::Diarize, Self::Reconcile];

    /// Stable checkpoint name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Diarize => "diarize",
            Self::Reconcile => "reconcile",
            Self::Embed => "embed",
            Self::Match => "match",
            Self::Suggest => "suggest",
        }
    }

    /// Position in the pipeline, which is the checkpoint's stage index.
    #[must_use]
    pub const fn index(self) -> i64 {
        match self {
            Self::Diarize => 0,
            Self::Reconcile => 1,
            Self::Embed => 2,
            Self::Match => 3,
            Self::Suggest => 4,
        }
    }

    /// Whether this stage touches biometric data.
    #[must_use]
    pub const fn is_biometric(self) -> bool {
        matches!(self, Self::Embed | Self::Match | Self::Suggest)
    }

    /// Parse a stored checkpoint name.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|s| s.as_str() == value)
    }
}

/// Why the biometric stages are not planned (VP-003, VP-024).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "refusal", rename_all = "snake_case")]
pub enum GateRefusal {
    /// The runtime toggle is off, which is its default.
    MatchingOff,
    /// The activation disclosure has never been accepted.
    DisclosureNotAccepted,
    /// A newer disclosure was published since the acceptance (VP-121).
    DisclosureOutdated {
        /// Version the user accepted.
        accepted: String,
        /// Version in force now.
        required: String,
    },
    /// Nobody is enrolled, so there is nothing to match against.
    NoEnrolledProfiles,
    /// The OS credential store is locked, so the biometric tier is locked
    /// (ADR-0014).
    KeyUnavailable,
}

impl GateRefusal {
    /// Stable short reason, for a job's stage label and its step rows.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::MatchingOff => "matching_off",
            Self::DisclosureNotAccepted => "disclosure_not_accepted",
            Self::DisclosureOutdated { .. } => "disclosure_outdated",
            Self::NoEnrolledProfiles => "no_enrolled_profiles",
            Self::KeyUnavailable => "key_unavailable",
        }
    }
}

/// The consent state the biometric stages are planned against (§12.2).
///
/// Every field is a fact the caller read from the library, never a default this
/// module invents: the gate is closed unless something says otherwise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchGate {
    /// The runtime toggle (VP-003), off by default.
    pub matching_enabled: bool,
    /// Disclosure version the user accepted, when they accepted one.
    pub accepted_disclosure_version: Option<String>,
    /// How many people have an active enrolment.
    pub enrolled_profiles: usize,
    /// Whether the voice-profile key store answered.
    pub key_available: bool,
}

impl MatchGate {
    /// The posture of a library where nothing has been turned on: off.
    #[must_use]
    pub const fn closed() -> Self {
        Self {
            matching_enabled: false,
            accepted_disclosure_version: None,
            enrolled_profiles: 0,
            key_available: false,
        }
    }

    /// Whether the biometric stages may run, and why not when they may not.
    ///
    /// # Errors
    ///
    /// The [`GateRefusal`] that closed the gate, strongest condition first:
    /// the toggle, then the disclosure, then enrolment, then the key.
    pub fn decide(&self) -> Result<(), GateRefusal> {
        if !self.matching_enabled {
            return Err(GateRefusal::MatchingOff);
        }
        match self.accepted_disclosure_version.as_deref() {
            None => return Err(GateRefusal::DisclosureNotAccepted),
            Some(accepted) if accepted != CONSENT_DISCLOSURE_VERSION => {
                return Err(GateRefusal::DisclosureOutdated {
                    accepted: accepted.to_owned(),
                    required: CONSENT_DISCLOSURE_VERSION.to_owned(),
                })
            }
            Some(_) => {}
        }
        if self.enrolled_profiles == 0 {
            return Err(GateRefusal::NoEnrolledProfiles);
        }
        if !self.key_available {
            return Err(GateRefusal::KeyUnavailable);
        }
        Ok(())
    }

    /// Whether the biometric stages may run.
    #[must_use]
    pub fn allows_matching(&self) -> bool {
        self.decide().is_ok()
    }
}

/// Which stages a job will run (VP-024).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeakerMatchPlan {
    stages: Vec<SpeakerMatchStage>,
    refusal: Option<GateRefusal>,
}

impl SpeakerMatchPlan {
    /// Plan a run against the consent state.
    #[must_use]
    pub fn for_gate(gate: &MatchGate) -> Self {
        match gate.decide() {
            Ok(()) => Self {
                stages: SpeakerMatchStage::ALL.to_vec(),
                refusal: None,
            },
            Err(refusal) => Self {
                stages: SpeakerMatchStage::WITHOUT_MATCHING.to_vec(),
                refusal: Some(refusal),
            },
        }
    }

    /// The stages, in execution order.
    #[must_use]
    pub fn stages(&self) -> &[SpeakerMatchStage] {
        &self.stages
    }

    /// Whether the plan contains the biometric stages.
    #[must_use]
    pub fn includes_matching(&self) -> bool {
        self.refusal.is_none()
    }

    /// Why the biometric stages are absent, when they are.
    #[must_use]
    pub fn refusal(&self) -> Option<&GateRefusal> {
        self.refusal.as_ref()
    }

    /// Whether a stage is planned.
    #[must_use]
    pub fn contains(&self, stage: SpeakerMatchStage) -> bool {
        self.stages.contains(&stage)
    }
}

/// Why a cluster produced no embedding (VP-023, VP-066).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkippedCluster {
    /// The cluster.
    pub cluster_key: String,
    /// Clean single-speaker speech it had, in milliseconds.
    pub clean_speech_ms: u32,
    /// Milliseconds it needed.
    pub required_ms: u32,
}

/// What one run of the pipeline did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeakerMatchOutcome {
    /// The diarization run the job produced or resumed.
    pub diarization_run_id: String,
    /// Clusters the provider reported.
    pub clusters: usize,
    /// Clusters that produced an embedding.
    pub embedded_clusters: usize,
    /// Clusters refused for lack of clean speech.
    pub skipped_clusters: Vec<SkippedCluster>,
    /// Suggestion rows written or refreshed.
    pub suggestions_written: u32,
    /// Enrolled profiles that were scored.
    pub profiles_scored: usize,
    /// Enrolled profiles left out because they were produced by another
    /// checkpoint (VP-026).
    pub profiles_skipped_wrong_checkpoint: usize,
    /// Stages that were already committed when the job started.
    pub resumed_stages: Vec<SpeakerMatchStage>,
    /// Why the biometric stages did not run, when they did not.
    pub refusal: Option<GateRefusal>,
}

impl SpeakerMatchOutcome {
    /// Whether this run reused a previous attempt's work.
    #[must_use]
    pub fn resumed(&self) -> bool {
        !self.resumed_stages.is_empty()
    }
}

/// Why the pipeline could not finish.
#[derive(Debug, thiserror::Error)]
pub enum SpeakerMatchError {
    /// The user cancelled the job.
    #[error("the speaker-matching job was cancelled")]
    Cancelled,
    /// Diarization refused or failed.
    #[error(transparent)]
    Diarization(#[from] DiarizationError),
    /// The revision carries no times, so turns cannot be aligned.
    #[error(transparent)]
    Reconcile(#[from] ReconcileError),
    /// The embedder refused or failed.
    #[error(transparent)]
    Embed(#[from] EmbedError),
    /// A profile could not be assembled from its exemplars.
    #[error(transparent)]
    Profile(#[from] ProfileError),
    /// Scoring refused.
    #[error(transparent)]
    Match(#[from] MatchError),
    /// The biometric tier is locked or a ciphertext did not authenticate.
    #[error(transparent)]
    Store(#[from] VoiceProfileError),
    /// The library could not be read or written.
    #[error(transparent)]
    Persistence(#[from] PersistenceError),
    /// The job store refused a checkpoint or could not be read.
    #[error(transparent)]
    Jobs(#[from] JobError),
    /// A committed checkpoint does not describe output this build understands.
    #[error("the checkpoint for stage `{stage}` is unreadable: {detail}")]
    CorruptCheckpoint {
        /// Stage whose checkpoint could not be used.
        stage: &'static str,
        /// What was wrong with it.
        detail: String,
    },
}

/// Everything one run of the pipeline needs.
#[derive(Debug, Clone)]
pub struct SpeakerMatchRequest {
    /// The job this run belongs to; its checkpoints are the resume points.
    pub job_id: JobId,
    /// Recording being matched.
    pub recording_id: RecordingId,
    /// Media asset the diarization run records.
    pub media_asset_id: String,
    /// Path of the media the provider and the embedder read.
    pub media_path: PathBuf,
    /// Digest of those bytes, which keys the job and the diarization run.
    pub audio_sha256: String,
    /// Media duration, when known.
    pub duration_ms: Option<u64>,
    /// Revision the turns are reconciled against.
    pub revision: TranscriptRevision,
    /// Consent state the plan is computed from.
    pub gate: MatchGate,
    /// Local-only mode and cloud consent, for the diarization gate.
    pub policy: RunPolicy,
    /// Speaker-count hint the user supplied, when they did.
    pub speaker_count_hint: Option<u8>,
}

/// One cluster's embeddable speech, derived from the reconciliation alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterSpeech {
    /// The cluster.
    pub cluster_key: String,
    /// Spans attributed to this cluster and nobody else (VP-022).
    pub spans: Vec<(u64, u64)>,
    /// Total clean single-speaker speech in those spans.
    pub clean_speech_ms: u32,
}

impl ClusterSpeech {
    /// Windows to embed, in order.
    ///
    /// Each span is cut into [`EMBED_WINDOW_MS`] windows; a trailing remainder
    /// is embedded only when it still carries the VP-065 minimum, because a
    /// 200 ms tail is an architectural possibility, not a reliable embedding.
    #[must_use]
    pub fn windows(&self) -> Vec<(u64, u64)> {
        let mut windows = Vec::new();
        for (start, end) in &self.spans {
            let mut cursor = *start;
            while cursor < *end {
                let window_end = (cursor + EMBED_WINDOW_MS).min(*end);
                if window_end - cursor >= u64::from(MIN_EMBED_SPEECH_MS) {
                    windows.push((cursor, window_end));
                }
                cursor = window_end;
            }
        }
        windows
    }
}

/// Clean single-speaker speech per cluster, in first-appearance order.
///
/// Pure: a function of the reconciliation and the revision's segment times,
/// which is why a resumed job can recompute the skip decisions without
/// re-reading audio or re-running the embedder.
#[must_use]
pub fn cluster_speech(
    reconciliation: &Reconciliation,
    revision: &TranscriptRevision,
) -> Vec<ClusterSpeech> {
    let times: BTreeMap<u32, (u64, u64)> = revision
        .segments()
        .iter()
        .filter_map(|segment| {
            let (start, end) = (segment.start?, segment.end?);
            Some((segment.seq_index, (start.get(), end.get())))
        })
        .collect();

    let mut spans: BTreeMap<String, Vec<(u64, u64)>> = BTreeMap::new();
    for segment in &reconciliation.segments {
        let SegmentAssignment::Single { cluster } = &segment.assignment else {
            // Overlapping speech never contributes to an embedding (VP-022),
            // and an unassigned segment has no cluster to contribute to.
            continue;
        };
        let Some((start, end)) = times.get(&segment.seq_index).copied() else {
            continue;
        };
        if end > start {
            spans
                .entry(cluster.to_string())
                .or_default()
                .push((start, end));
        }
    }

    reconciliation
        .cluster_order
        .iter()
        .map(|cluster| {
            let mut cluster_spans = spans.remove(cluster.as_str()).unwrap_or_default();
            cluster_spans.sort_unstable();
            let clean_speech_ms = cluster_spans
                .iter()
                .filter(|(start, end)| end - start >= u64::from(MIN_EMBED_SPEECH_MS))
                .map(|(start, end)| u32::try_from(end - start).unwrap_or(u32::MAX))
                .fold(0u32, u32::saturating_add);
            ClusterSpeech {
                cluster_key: cluster.to_string(),
                spans: cluster_spans,
                clean_speech_ms,
            }
        })
        .collect()
}

/// Digest of the enrolled profile set, a keyed dimension of the job (VP-101).
///
/// Enrolling a person, adding an exemplar to one or deleting a profile all
/// change the digest, so the same recording is matched again under a fresh job
/// key instead of deduplicating onto the previous answer.
#[must_use]
pub fn profile_set_digest(profiles: &[(String, Vec<String>)]) -> String {
    let mut ordered: Vec<(String, Vec<String>)> = profiles
        .iter()
        .map(|(person, exemplars)| {
            let mut exemplars = exemplars.clone();
            exemplars.sort();
            (person.clone(), exemplars)
        })
        .collect();
    ordered.sort();

    let mut hasher = Sha256::new();
    hasher.update(b"plaudon.voice-profile-set.v1");
    for (person, exemplars) in &ordered {
        hasher.update([0x1f]);
        hasher.update(person.as_bytes());
        for exemplar in exemplars {
            hasher.update([0x1e]);
            hasher.update(exemplar.as_bytes());
        }
    }
    hex::encode(hasher.finalize())
}

/// Stage 1–5 of §4, over one job's checkpoints.
pub struct SpeakerMatchPipeline {
    jobs: JobStore,
    speakers: SpeakerRepository,
    enrollments: VoiceEnrollmentRepository,
    scratch: SpeakerMatchScratch,
    cipher: EmbeddingCipher,
    diarizer: Arc<dyn DiarizationProvider>,
    embedder: Arc<dyn SpeakerEmbedder>,
}

impl std::fmt::Debug for SpeakerMatchPipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpeakerMatchPipeline")
            .field("diarizer", &self.diarizer)
            .field("embedder", &self.embedder)
            .finish_non_exhaustive()
    }
}

impl SpeakerMatchPipeline {
    /// Assemble the pipeline over a library and a job store.
    #[must_use]
    pub fn new(
        jobs: JobStore,
        db: Database,
        cipher: EmbeddingCipher,
        diarizer: Arc<dyn DiarizationProvider>,
        embedder: Arc<dyn SpeakerEmbedder>,
    ) -> Self {
        Self {
            jobs,
            speakers: SpeakerRepository::new(db.clone()),
            enrollments: VoiceEnrollmentRepository::new(db.clone()),
            scratch: SpeakerMatchScratch::new(db),
            cipher,
            diarizer,
            embedder,
        }
    }

    /// The embedder this pipeline runs, for the caller's job key (VP-101).
    #[must_use]
    pub fn embedder(&self) -> &Arc<dyn SpeakerEmbedder> {
        &self.embedder
    }

    /// Run the planned stages, resuming at the first one without a commit mark.
    ///
    /// # Errors
    ///
    /// [`SpeakerMatchError::Cancelled`] when the user cancelled, and otherwise
    /// the refusal or failure of whichever stage could not finish. A failure
    /// leaves every committed stage committed, so a retry resumes rather than
    /// restarting.
    pub async fn run(
        &self,
        request: &SpeakerMatchRequest,
        ctx: &JobContext,
    ) -> Result<SpeakerMatchOutcome, SpeakerMatchError> {
        let plan = SpeakerMatchPlan::for_gate(&request.gate);
        let committed = self.committed_stages(request.job_id).await?;
        let mut resumed_stages = Vec::new();

        let (run_id, result) = self
            .stage_diarize(request, ctx, &committed, &mut resumed_stages)
            .await?;
        Self::report(ctx, PROGRESS_AFTER_DIARIZE);

        let reconciliation = self
            .stage_reconcile(
                request,
                ctx,
                &committed,
                &mut resumed_stages,
                run_id,
                &result,
            )
            .await?;
        Self::report(ctx, PROGRESS_AFTER_RECONCILE);

        let speech = cluster_speech(&reconciliation, &request.revision);
        let mut outcome = SpeakerMatchOutcome {
            diarization_run_id: run_id.to_string(),
            clusters: reconciliation.cluster_order.len(),
            embedded_clusters: 0,
            skipped_clusters: Vec::new(),
            suggestions_written: 0,
            profiles_scored: 0,
            profiles_skipped_wrong_checkpoint: 0,
            resumed_stages,
            refusal: plan.refusal().cloned(),
        };

        if !plan.includes_matching() {
            // VP-003: with the gate closed the pipeline stops here, having
            // computed nothing biometric at all. The scratch of an earlier
            // attempt — from before the user turned the feature off — goes too.
            self.scratch.discard(request.job_id).await?;
            Self::report(ctx, 1.0);
            return Ok(outcome);
        }

        outcome.skipped_clusters = speech
            .iter()
            .filter(|cluster| cluster.clean_speech_ms < MIN_IDENTIFY_SPEECH_MS)
            .map(|cluster| SkippedCluster {
                cluster_key: cluster.cluster_key.clone(),
                clean_speech_ms: cluster.clean_speech_ms,
                required_ms: MIN_IDENTIFY_SPEECH_MS,
            })
            .collect();

        self.stage_embed(request, ctx, &committed, &mut outcome, run_id, &speech)
            .await?;
        Self::report(ctx, PROGRESS_AFTER_EMBED);

        let reports = self
            .stage_match(request, ctx, &committed, &mut outcome, run_id)
            .await?;
        Self::report(ctx, PROGRESS_AFTER_MATCH);

        self.stage_suggest(request, ctx, &committed, &mut outcome, run_id, &reports)
            .await?;

        // The job is done with the vectors it parked; nothing keeps biometric
        // scratch alive past the run that needed it (VP-071, §12.4).
        self.scratch.discard(request.job_id).await?;
        Self::report(ctx, 1.0);
        Ok(outcome)
    }

    /// Destroy a job's scratch, for a caller settling a cancelled or abandoned
    /// run.
    ///
    /// # Errors
    ///
    /// Propagates persistence failures.
    pub async fn discard_scratch(&self, job_id: JobId) -> Result<(), SpeakerMatchError> {
        self.scratch.discard(job_id).await?;
        Ok(())
    }

    /* ------------------------------------------------------------ stages */

    async fn stage_diarize(
        &self,
        request: &SpeakerMatchRequest,
        ctx: &JobContext,
        committed: &BTreeMap<String, Checkpoint>,
        resumed: &mut Vec<SpeakerMatchStage>,
    ) -> Result<(Uuid, DiarizationResult), SpeakerMatchError> {
        let stage = SpeakerMatchStage::Diarize;
        if let Some(checkpoint) = committed.get(stage.as_str()) {
            let run_id = parse_run_ref(checkpoint, stage)?;
            let stored = self.speakers.run(run_id).await?.ok_or_else(|| {
                SpeakerMatchError::CorruptCheckpoint {
                    stage: stage.as_str(),
                    detail: "the committed diarization run is gone".to_owned(),
                }
            })?;
            let turns = self.speakers.turns(run_id).await?;
            resumed.push(stage);
            return Ok((
                run_id,
                result_from_rows(&stored.provider_id, stored.provider_version.clone(), &turns),
            ));
        }

        Self::check_cancelled(ctx)?;
        let run_id = deterministic_uuid(&format!("{}:{}", request.job_id, stage.as_str()));
        self.jobs
            .prepare_checkpoint(
                request.job_id,
                stage.as_str(),
                stage.index(),
                request.audio_sha256.clone(),
            )
            .await?;

        let input = DiarizationInput {
            media_path: request.media_path.clone(),
            duration_ms: request.duration_ms,
            speaker_count_hint: request.speaker_count_hint,
        };
        // The provider reports its own 0..1; folded into this stage's share of
        // the job so a finished *stage* never looks like a finished *job*.
        let staged = JobContext {
            job_id: ctx.job_id,
            cancel: ctx.cancel.clone(),
            progress: Arc::new(ScaledProgress {
                inner: Arc::clone(&ctx.progress),
                from: 0.0,
                to: PROGRESS_AFTER_DIARIZE,
            }),
        };
        let result = run_guarded(self.diarizer.as_ref(), input, request.policy, staged)
            .await
            .map_err(|error| match error {
                DiarizationError::Cancelled => SpeakerMatchError::Cancelled,
                other => SpeakerMatchError::Diarization(other),
            })?;

        // An earlier attempt of this same job may have written rows before it
        // crashed; the run id is derived from the job, so this overwrites that
        // attempt instead of leaving an orphan run beside the real one.
        self.speakers.discard_run(run_id).await?;
        let now = Timestamp::now();
        let turns: Vec<SpeakerTurnInsert> = result
            .turns
            .iter()
            .enumerate()
            .map(|(index, turn)| SpeakerTurnInsert {
                seq_index: u32::try_from(index).unwrap_or(u32::MAX),
                cluster_key: turn.cluster.to_string(),
                start_ms: turn.start_ms,
                end_ms: turn.end_ms,
                confidence: turn.confidence,
                confidence_scale: turn.confidence.map(|_| "provider_native".to_owned()),
            })
            .collect();
        self.speakers
            .record_run(
                &DiarizationRunInsert {
                    id: run_id,
                    recording_id: request.recording_id,
                    media_asset_id: request.media_asset_id.clone(),
                    provider_id: result.provider_id.clone(),
                    provider_version: result.provider_version.clone(),
                    privacy_class: "local_only".to_owned(),
                    status: "succeeded".to_owned(),
                    input_sha256: request.audio_sha256.clone(),
                    detected_cluster_count: Some(
                        u32::try_from(result.clusters().len()).unwrap_or(u32::MAX),
                    ),
                    created_at: now,
                    finished_at: Some(now),
                },
                &turns,
            )
            .await?;
        self.jobs
            .commit_checkpoint(
                request.job_id,
                stage.as_str(),
                run_id.to_string(),
                Some(digest_of_turns(&turns)),
            )
            .await?;
        Ok((run_id, result))
    }

    async fn stage_reconcile(
        &self,
        request: &SpeakerMatchRequest,
        ctx: &JobContext,
        committed: &BTreeMap<String, Checkpoint>,
        resumed: &mut Vec<SpeakerMatchStage>,
        run_id: Uuid,
        result: &DiarizationResult,
    ) -> Result<Reconciliation, SpeakerMatchError> {
        let stage = SpeakerMatchStage::Reconcile;
        if committed.contains_key(stage.as_str()) {
            let stored = self
                .speakers
                .assignments(request.revision.id, run_id)
                .await?;
            resumed.push(stage);
            return reconciliation_from_rows(request, &stored);
        }

        Self::check_cancelled(ctx)?;
        self.jobs
            .prepare_checkpoint(
                request.job_id,
                stage.as_str(),
                stage.index(),
                run_id.to_string(),
            )
            .await?;
        let reconciliation = reconcile(&request.revision, result)?;
        let cluster_order: Vec<String> = reconciliation
            .cluster_order
            .iter()
            .map(ClusterKey::to_string)
            .collect();
        let assignments: Vec<AssignmentInsert> = reconciliation
            .segments
            .iter()
            .map(|segment| AssignmentInsert {
                seq_index: segment.seq_index,
                assignment: segment.assignment.as_str().to_owned(),
                cluster_key: segment
                    .assignment
                    .single()
                    .map(std::string::ToString::to_string),
                overlapping_keys_json: match &segment.assignment {
                    SegmentAssignment::Overlapping { clusters } => Some(
                        serde_json::to_string(
                            &clusters
                                .iter()
                                .map(ClusterKey::to_string)
                                .collect::<Vec<_>>(),
                        )
                        .unwrap_or_else(|_| "[]".to_owned()),
                    ),
                    _ => None,
                },
                covered_ms: segment.covered_ms,
            })
            .collect();
        self.speakers
            .apply_reconciliation(
                run_id,
                request.recording_id,
                request.revision.id,
                &cluster_order,
                &assignments,
            )
            .await?;
        self.jobs
            .commit_checkpoint(
                request.job_id,
                stage.as_str(),
                run_id.to_string(),
                Some(digest_of_assignments(&assignments)),
            )
            .await?;
        Ok(reconciliation)
    }

    async fn stage_embed(
        &self,
        request: &SpeakerMatchRequest,
        ctx: &JobContext,
        committed: &BTreeMap<String, Checkpoint>,
        outcome: &mut SpeakerMatchOutcome,
        run_id: Uuid,
        speech: &[ClusterSpeech],
    ) -> Result<(), SpeakerMatchError> {
        let stage = SpeakerMatchStage::Embed;
        if committed.contains_key(stage.as_str()) {
            outcome.embedded_clusters =
                self.scratch.cluster_embeddings(request.job_id).await?.len();
            outcome.resumed_stages.push(stage);
            return Ok(());
        }

        Self::check_cancelled(ctx)?;
        let checkpoint = self.embedder.checkpoint();
        self.jobs
            .prepare_checkpoint(
                request.job_id,
                stage.as_str(),
                stage.index(),
                run_id.to_string(),
            )
            .await?;

        let embeddable: Vec<&ClusterSpeech> = speech
            .iter()
            .filter(|cluster| cluster.clean_speech_ms >= MIN_IDENTIFY_SPEECH_MS)
            .collect();
        let total_windows: usize = embeddable.iter().map(|c| c.windows().len()).sum();
        let mut done_windows = 0usize;
        let mut sealed = Vec::with_capacity(embeddable.len());

        for cluster in embeddable {
            let mut sum = vec![0.0f64; checkpoint.dim];
            let mut embedded = 0usize;
            for (start_ms, end_ms) in cluster.windows() {
                // The cancel point VP-103 names: a user who pressed Cancel
                // waits for one window, not for a recording.
                Self::check_cancelled(ctx)?;
                let window = EmbedWindow {
                    media_path: request.media_path.clone(),
                    cluster_key: cluster.cluster_key.clone(),
                    start_ms,
                    end_ms,
                };
                let embedding = self.embedder.embed_window(&window, ctx).await.map_err(
                    |error| match error {
                        EmbedError::Cancelled => SpeakerMatchError::Cancelled,
                        other => SpeakerMatchError::Embed(other),
                    },
                )?;
                if embedding.dim() != checkpoint.dim {
                    return Err(SpeakerMatchError::Embed(EmbedError::Malformed(format!(
                        "the checkpoint is {}-dimensional, the vector is {}-dimensional",
                        checkpoint.dim,
                        embedding.dim()
                    ))));
                }
                for (slot, component) in sum.iter_mut().zip(embedding.unit()) {
                    *slot += f64::from(*component);
                }
                embedded += 1;
                done_windows += 1;
                if total_windows > 0 {
                    let fraction = done_windows as f32 / total_windows as f32;
                    Self::report(
                        ctx,
                        PROGRESS_AFTER_RECONCILE
                            + (PROGRESS_AFTER_EMBED - PROGRESS_AFTER_RECONCILE) * fraction,
                    );
                }
            }
            if embedded == 0 {
                continue;
            }
            let mean: Vec<f32> = sum.iter().map(|value| *value as f32).collect();
            let aggregate = Embedding::l2_normalise(checkpoint.binding.clone(), &mean)
                .map_err(|error| SpeakerMatchError::Embed(error.into()))?;
            let binding = ClusterBinding {
                job_id: request.job_id,
                diarization_run_id: run_id.to_string(),
                cluster_key: cluster.cluster_key.clone(),
                model_id: checkpoint.binding.model_id.clone(),
                model_version: checkpoint.binding.model_version.clone(),
                embedding_dim: u32::try_from(aggregate.dim()).unwrap_or(u32::MAX),
            };
            let sealed_embedding = self.cipher.seal_cluster(aggregate.unit(), &binding)?;
            sealed.push(ClusterEmbeddingInsert {
                job_id: request.job_id,
                diarization_run_id: run_id,
                cluster_key: cluster.cluster_key.clone(),
                speech_ms: u64::from(cluster.clean_speech_ms),
                model_id: binding.model_id.clone(),
                model_version: binding.model_version.clone(),
                embedding_dim: sealed_embedding.embedding_dim,
                embedding_cipher: sealed_embedding.cipher,
                embedding_nonce: sealed_embedding.nonce,
            });
        }

        outcome.embedded_clusters = sealed.len();
        self.scratch
            .put_cluster_embeddings(&sealed, Timestamp::now())
            .await?;
        self.jobs
            .commit_checkpoint(
                request.job_id,
                stage.as_str(),
                format!("clusters:{}", sealed.len()),
                None,
            )
            .await?;
        Ok(())
    }

    async fn stage_match(
        &self,
        request: &SpeakerMatchRequest,
        ctx: &JobContext,
        committed: &BTreeMap<String, Checkpoint>,
        outcome: &mut SpeakerMatchOutcome,
        run_id: Uuid,
    ) -> Result<Vec<MatchReport>, SpeakerMatchError> {
        let stage = SpeakerMatchStage::Match;
        if committed.contains_key(stage.as_str()) {
            let stored = self.scratch.reports(request.job_id).await?;
            outcome.resumed_stages.push(stage);
            return stored
                .iter()
                .map(|row| {
                    serde_json::from_str(&row.report_json).map_err(|error| {
                        SpeakerMatchError::CorruptCheckpoint {
                            stage: stage.as_str(),
                            detail: error.to_string(),
                        }
                    })
                })
                .collect();
        }

        Self::check_cancelled(ctx)?;
        self.jobs
            .prepare_checkpoint(
                request.job_id,
                stage.as_str(),
                stage.index(),
                run_id.to_string(),
            )
            .await?;

        let checkpoint = self.embedder.checkpoint();
        let (profiles, skipped) = self.load_profiles().await?;
        outcome.profiles_scored = profiles.len();
        outcome.profiles_skipped_wrong_checkpoint = skipped;

        let matcher = Matcher::new(
            checkpoint.binding.clone(),
            checkpoint.thresholds,
            checkpoint.cohort.clone(),
            checkpoint.top_k,
        )?;
        let prepared = matcher.prepare_all(&profiles)?;

        let mut reports = Vec::new();
        let mut rows = Vec::new();
        for parked in self.scratch.cluster_embeddings(request.job_id).await? {
            Self::check_cancelled(ctx)?;
            let binding = ClusterBinding {
                job_id: parked.job_id,
                diarization_run_id: parked.diarization_run_id.to_string(),
                cluster_key: parked.cluster_key.clone(),
                model_id: parked.model_id.clone(),
                model_version: parked.model_version.clone(),
                embedding_dim: parked.embedding_dim,
            };
            let vector = self.cipher.open_cluster(
                &SealedEmbedding {
                    cipher: parked.embedding_cipher.clone(),
                    nonce: parked.embedding_nonce.clone(),
                    embedding_dim: parked.embedding_dim,
                },
                &binding,
            )?;
            let embedding = Embedding::l2_normalise(checkpoint.binding.clone(), vector.as_slice())
                .map_err(|error| SpeakerMatchError::Embed(error.into()))?;
            let speech_ms = u32::try_from(parked.speech_ms).unwrap_or(u32::MAX);
            let cluster = ClusterEmbedding::new(parked.cluster_key.clone(), embedding, speech_ms)
                .map_err(MatchError::from)?;
            let report = matcher.match_cluster(&cluster, &prepared)?;
            rows.push(MatchReportInsert {
                job_id: request.job_id,
                diarization_run_id: run_id,
                cluster_key: parked.cluster_key.clone(),
                report_json: serde_json::to_string(&report).map_err(|error| {
                    SpeakerMatchError::CorruptCheckpoint {
                        stage: stage.as_str(),
                        detail: error.to_string(),
                    }
                })?,
            });
            reports.push(report);
        }

        self.scratch.put_reports(&rows, Timestamp::now()).await?;
        self.jobs
            .commit_checkpoint(
                request.job_id,
                stage.as_str(),
                format!("reports:{}", rows.len()),
                None,
            )
            .await?;
        Ok(reports)
    }

    async fn stage_suggest(
        &self,
        request: &SpeakerMatchRequest,
        ctx: &JobContext,
        committed: &BTreeMap<String, Checkpoint>,
        outcome: &mut SpeakerMatchOutcome,
        run_id: Uuid,
        reports: &[MatchReport],
    ) -> Result<(), SpeakerMatchError> {
        let stage = SpeakerMatchStage::Suggest;
        if let Some(checkpoint) = committed.get(stage.as_str()) {
            outcome.suggestions_written = checkpoint
                .output_ref
                .as_deref()
                .and_then(|reference| reference.strip_prefix("suggestions:"))
                .and_then(|count| count.parse().ok())
                .unwrap_or(0);
            outcome.resumed_stages.push(stage);
            return Ok(());
        }

        Self::check_cancelled(ctx)?;
        self.jobs
            .prepare_checkpoint(
                request.job_id,
                stage.as_str(),
                stage.index(),
                run_id.to_string(),
            )
            .await?;

        let binding = self.embedder.checkpoint();
        let mut suggestions = Vec::new();
        let mut scored_people: BTreeSet<String> = BTreeSet::new();
        for report in reports {
            for scored in &report.recorded {
                let Some(band) = scored.band.column_value() else {
                    continue;
                };
                let Ok(person_id) = scored.person_id.parse::<PersonId>() else {
                    continue;
                };
                scored_people.insert(scored.person_id.clone());
                suggestions.push(SuggestionInsert {
                    recording_id: request.recording_id,
                    diarization_run_id: run_id,
                    cluster_key: report.cluster_key.clone(),
                    person_id,
                    score: f64::from(scored.score.value()),
                    band: band.to_owned(),
                    model_id: binding.binding.model_id.clone(),
                    model_version: binding.binding.model_version.clone(),
                });
            }
        }

        let now = Timestamp::now();
        // Idempotent by the `(run, cluster, person)` unique key: a resumed or
        // re-run job refreshes a pending row and leaves a decided one alone, so
        // nothing resurrects a decision the user already made (VP-102).
        let written = self
            .enrollments
            .record_suggestions(&suggestions, now)
            .await?;
        for person in &scored_people {
            if let Ok(person_id) = person.parse::<PersonId>() {
                self.enrollments.mark_matched(person_id, now).await?;
            }
        }
        outcome.suggestions_written = written;
        self.jobs
            .commit_checkpoint(
                request.job_id,
                stage.as_str(),
                format!("suggestions:{written}"),
                None,
            )
            .await?;
        Ok(())
    }

    /* ------------------------------------------------------------ helpers */

    /// Every enrolled profile this checkpoint can score, plus how many were
    /// left out because another checkpoint produced them (VP-026).
    async fn load_profiles(&self) -> Result<(Vec<VoiceProfile>, usize), SpeakerMatchError> {
        let checkpoint = self.embedder.checkpoint();
        let mut profiles = Vec::new();
        let mut skipped = 0usize;
        for enrollment in self.enrollments.active_enrollments().await? {
            let stored = self.enrollments.exemplars(enrollment.person_id).await?;
            let mut exemplars = Vec::with_capacity(stored.len());
            let mut wrong_checkpoint = false;
            for row in &stored {
                if row.model_id != checkpoint.binding.model_id
                    || row.model_version != checkpoint.binding.model_version
                {
                    wrong_checkpoint = true;
                    break;
                }
                let vector = self.cipher.open(
                    &SealedEmbedding {
                        cipher: row.embedding_cipher.clone(),
                        nonce: row.embedding_nonce.clone(),
                        embedding_dim: row.embedding_dim,
                    },
                    &ExemplarBinding {
                        person_id: row.person_id,
                        model_id: row.model_id.clone(),
                        model_version: row.model_version.clone(),
                        embedding_dim: row.embedding_dim,
                    },
                )?;
                let embedding =
                    Embedding::l2_normalise(checkpoint.binding.clone(), vector.as_slice())
                        .map_err(|error| SpeakerMatchError::Embed(error.into()))?;
                let speech_ms = u32::try_from(row.speech_ms).unwrap_or(u32::MAX);
                exemplars.push(Exemplar::new(embedding, speech_ms).map_err(ProfileError::from)?);
            }
            if wrong_checkpoint || exemplars.is_empty() {
                // A profile enrolled under another model is not comparable and
                // is not silently compared (VP-026); it waits for the
                // re-embedding a model upgrade triggers (VP-075, §7.6).
                skipped += usize::from(wrong_checkpoint);
                continue;
            }
            match VoiceProfile::new(
                enrollment.person_id.to_string(),
                exemplars,
                ProfileOffset::ZERO,
            ) {
                Ok(profile) => profiles.push(profile),
                // A profile below the enrolment minimum stops matching rather
                // than matching on less evidence than the spec allows; it is
                // not a job failure (VP-065).
                Err(ProfileError::NotEnoughSpeech(_)) => {}
                Err(other) => return Err(other.into()),
            }
        }
        Ok((profiles, skipped))
    }

    async fn committed_stages(
        &self,
        job_id: JobId,
    ) -> Result<BTreeMap<String, Checkpoint>, SpeakerMatchError> {
        Ok(self
            .jobs
            .checkpoints(job_id)
            .await?
            .into_iter()
            .filter(Checkpoint::is_committed)
            .map(|checkpoint| (checkpoint.stage.clone(), checkpoint))
            .collect())
    }

    fn check_cancelled(ctx: &JobContext) -> Result<(), SpeakerMatchError> {
        if ctx.is_cancelled() {
            return Err(SpeakerMatchError::Cancelled);
        }
        Ok(())
    }

    fn report(ctx: &JobContext, fraction: f32) {
        ctx.report(Progress::fraction(fraction));
    }
}

/// Folds a sub-run's own `0..1` progress into the slice of the job it owns.
///
/// Without it a provider that reports "done" would publish a job-level 1.0 in
/// the middle of a five-stage pipeline, and the UI would show a finished job
/// that is still running.
#[derive(Debug)]
struct ScaledProgress {
    inner: Arc<dyn ProgressSink>,
    from: f32,
    to: f32,
}

impl ProgressSink for ScaledProgress {
    fn report(&self, progress: Progress) {
        let scaled = progress
            .fraction
            .map(|value| self.from + (self.to - self.from) * value.clamp(0.0, 1.0));
        self.inner.report(Progress {
            fraction: scaled,
            processed_ms: progress.processed_ms,
        });
    }
}

/// Rebuild a provider result from the rows a committed diarize stage wrote.
fn result_from_rows(
    provider_id: &str,
    provider_version: Option<String>,
    turns: &[SpeakerTurnInsert],
) -> DiarizationResult {
    DiarizationResult {
        payload_version: plaudon_ai_diarization::contract::DIARIZATION_CONTRACT_VERSION,
        provider_id: provider_id.to_owned(),
        provider_version,
        turns: turns
            .iter()
            .map(|turn| plaudon_ai_diarization::contract::SpeakerTurn {
                cluster: ClusterKey::new(turn.cluster_key.clone()),
                start_ms: turn.start_ms,
                end_ms: turn.end_ms,
                confidence: turn.confidence,
            })
            .collect(),
    }
}

/// Rebuild a reconciliation from the rows a committed reconcile stage wrote.
fn reconciliation_from_rows(
    request: &SpeakerMatchRequest,
    rows: &[AssignmentInsert],
) -> Result<Reconciliation, SpeakerMatchError> {
    let mut segments = Vec::with_capacity(rows.len());
    let mut cluster_order: Vec<ClusterKey> = Vec::new();
    for row in rows {
        let assignment = match row.assignment.as_str() {
            "single" => {
                let key = row.cluster_key.clone().ok_or_else(|| {
                    SpeakerMatchError::CorruptCheckpoint {
                        stage: SpeakerMatchStage::Reconcile.as_str(),
                        detail: "a single assignment without a cluster".to_owned(),
                    }
                })?;
                let cluster = ClusterKey::new(key);
                remember(&mut cluster_order, &cluster);
                SegmentAssignment::Single { cluster }
            }
            "overlapping" => {
                let json = row.overlapping_keys_json.as_deref().unwrap_or("[]");
                let keys: Vec<String> = serde_json::from_str(json).map_err(|error| {
                    SpeakerMatchError::CorruptCheckpoint {
                        stage: SpeakerMatchStage::Reconcile.as_str(),
                        detail: error.to_string(),
                    }
                })?;
                let clusters: Vec<ClusterKey> = keys.into_iter().map(ClusterKey::new).collect();
                for cluster in &clusters {
                    remember(&mut cluster_order, cluster);
                }
                SegmentAssignment::Overlapping { clusters }
            }
            _ => SegmentAssignment::Unassigned,
        };
        segments.push(crate::diarization::ReconciledSegment {
            seq_index: row.seq_index,
            assignment,
            covered_ms: row.covered_ms,
        });
    }
    Ok(Reconciliation {
        transcript_revision_id: request.revision.id,
        segments,
        cluster_order,
    })
}

fn remember(order: &mut Vec<ClusterKey>, cluster: &ClusterKey) {
    if !order.contains(cluster) {
        order.push(cluster.clone());
    }
}

fn parse_run_ref(
    checkpoint: &Checkpoint,
    stage: SpeakerMatchStage,
) -> Result<Uuid, SpeakerMatchError> {
    checkpoint
        .output_ref
        .as_deref()
        .and_then(|reference| reference.parse().ok())
        .ok_or_else(|| SpeakerMatchError::CorruptCheckpoint {
            stage: stage.as_str(),
            detail: "the commit mark does not name a diarization run".to_owned(),
        })
}

/// A UUID derived from a string, so a retried stage reuses its own row ids.
fn deterministic_uuid(seed: &str) -> Uuid {
    let digest = Sha256::digest(format!("plaudon.speaker-match.v1:{seed}").as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    // Version 8 (custom) with the RFC 4122 variant: an honest label for a
    // deterministically derived identifier.
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

/// Digest of the turns a committed diarize stage wrote.
///
/// Written with the commit mark so a resumed job can tell that the rows it is
/// about to reuse are the rows the stage produced.
fn digest_of_turns(turns: &[SpeakerTurnInsert]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"plaudon.speaker-match.turns.v1");
    for turn in turns {
        hasher.update([0x1e]);
        hasher.update(turn.cluster_key.as_bytes());
        hasher.update(turn.start_ms.to_be_bytes());
        hasher.update(turn.end_ms.to_be_bytes());
    }
    hex::encode(hasher.finalize())
}

/// Digest of the assignments a committed reconcile stage wrote.
fn digest_of_assignments(assignments: &[AssignmentInsert]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"plaudon.speaker-match.assignments.v1");
    for assignment in assignments {
        hasher.update([0x1e]);
        hasher.update(assignment.seq_index.to_be_bytes());
        hasher.update(assignment.assignment.as_bytes());
        hasher.update(assignment.cluster_key.as_deref().unwrap_or("").as_bytes());
        hasher.update(
            assignment
                .overlapping_keys_json
                .as_deref()
                .unwrap_or("")
                .as_bytes(),
        );
    }
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diarization::ReconciledSegment;
    use plaudon_domain::media::Sha256Digest;
    use plaudon_domain::model::{EngineId, ModelIdentity};
    use plaudon_domain::transcript::{
        AlignmentOrigin, RevisionOrigin, RevisionStatus, TranscriptRevisionDraft, TranscriptSegment,
    };

    /// Four two-second segments, so every span is above the one-second floor.
    fn revision() -> TranscriptRevision {
        let segments = (0..4u32)
            .map(|index| {
                let start = u64::from(index) * 2_000;
                TranscriptSegment::timed(index, start, start + 2_000, format!("s{index}"))
            })
            .collect();
        TranscriptRevision::try_new(TranscriptRevisionDraft {
            recording_id: RecordingId::new(),
            parent_id: None,
            origin: RevisionOrigin::Machine,
            input_sha256: Sha256Digest::of(b"fixture"),
            engine: EngineId::new("scripted"),
            model: ModelIdentity::new(EngineId::new("scripted"), "small"),
            locale: None,
            alignment_origin: AlignmentOrigin::Native,
            status: RevisionStatus::Complete,
            normalization_profile: "none".to_owned(),
            chunking_strategy: "single".to_owned(),
            duration_ms: Some(8_000),
            created_at: Timestamp::from_millis(0),
            segments,
        })
        .expect("fixture revision")
    }

    fn single(seq_index: u32, cluster: &str) -> ReconciledSegment {
        ReconciledSegment {
            seq_index,
            assignment: SegmentAssignment::Single {
                cluster: ClusterKey::new(cluster),
            },
            covered_ms: 2_000,
        }
    }

    #[test]
    fn overlapping_and_unassigned_segments_contribute_no_speech() {
        let revision = revision();
        let reconciliation = Reconciliation {
            transcript_revision_id: revision.id,
            segments: vec![
                single(0, "A"),
                single(1, "A"),
                ReconciledSegment {
                    seq_index: 2,
                    assignment: SegmentAssignment::Overlapping {
                        clusters: vec![ClusterKey::new("A"), ClusterKey::new("B")],
                    },
                    covered_ms: 2_000,
                },
                ReconciledSegment {
                    seq_index: 3,
                    assignment: SegmentAssignment::Unassigned,
                    covered_ms: 0,
                },
            ],
            cluster_order: vec![ClusterKey::new("A"), ClusterKey::new("B")],
        };

        let speech = cluster_speech(&reconciliation, &revision);
        assert_eq!(speech.len(), 2);
        assert_eq!(speech[0].cluster_key, "A");
        assert_eq!(
            speech[0].clean_speech_ms, 4_000,
            "only the two single-speaker segments counted"
        );
        assert_eq!(
            speech[1].clean_speech_ms, 0,
            "a cluster that only ever overlaps has no clean speech"
        );
        for (start, end) in &speech[0].spans {
            assert!(
                *end <= 4_000,
                "an overlapping span leaked into {start}..{end}"
            );
        }
    }

    #[test]
    fn a_window_shorter_than_the_embedding_minimum_is_not_embedded() {
        let cluster = ClusterSpeech {
            cluster_key: "A".to_owned(),
            // 2_000 ms yields one 1_500 ms window; the 500 ms tail is dropped.
            spans: vec![(0, 2_000), (10_000, 10_900)],
            clean_speech_ms: 2_000,
        };
        let windows = cluster.windows();
        assert_eq!(windows, vec![(0, 1_500)], "got {windows:?}");
    }

    #[test]
    fn a_long_span_is_cut_into_whole_windows() {
        let cluster = ClusterSpeech {
            cluster_key: "A".to_owned(),
            spans: vec![(0, 4_000)],
            clean_speech_ms: 4_000,
        };
        assert_eq!(
            cluster.windows(),
            vec![(0, 1_500), (1_500, 3_000), (3_000, 4_000)]
        );
    }

    #[test]
    fn the_gate_is_closed_by_default_and_names_the_strongest_reason() {
        assert_eq!(MatchGate::closed().decide(), Err(GateRefusal::MatchingOff));

        let open = MatchGate {
            matching_enabled: true,
            accepted_disclosure_version: Some(CONSENT_DISCLOSURE_VERSION.to_owned()),
            enrolled_profiles: 2,
            key_available: true,
        };
        assert_eq!(open.decide(), Ok(()));

        assert_eq!(
            MatchGate {
                accepted_disclosure_version: None,
                ..open.clone()
            }
            .decide(),
            Err(GateRefusal::DisclosureNotAccepted)
        );
        assert_eq!(
            MatchGate {
                accepted_disclosure_version: Some("older".to_owned()),
                ..open.clone()
            }
            .decide(),
            Err(GateRefusal::DisclosureOutdated {
                accepted: "older".to_owned(),
                required: CONSENT_DISCLOSURE_VERSION.to_owned(),
            })
        );
        assert_eq!(
            MatchGate {
                enrolled_profiles: 0,
                ..open.clone()
            }
            .decide(),
            Err(GateRefusal::NoEnrolledProfiles)
        );
        assert_eq!(
            MatchGate {
                key_available: false,
                ..open
            }
            .decide(),
            Err(GateRefusal::KeyUnavailable)
        );
    }

    #[test]
    fn a_closed_gate_plans_no_biometric_stage() {
        let plan = SpeakerMatchPlan::for_gate(&MatchGate::closed());
        assert_eq!(plan.stages(), SpeakerMatchStage::WITHOUT_MATCHING);
        assert!(!plan.includes_matching());
        for stage in SpeakerMatchStage::ALL {
            assert_eq!(
                plan.contains(*stage),
                !stage.is_biometric(),
                "{stage:?} should not be planned behind a closed gate"
            );
        }
    }

    #[test]
    fn every_stage_round_trips_through_its_checkpoint_name() {
        let mut indices: Vec<i64> = SpeakerMatchStage::ALL.iter().map(|s| s.index()).collect();
        indices.dedup();
        assert_eq!(indices.len(), SpeakerMatchStage::ALL.len());
        for stage in SpeakerMatchStage::ALL {
            assert_eq!(SpeakerMatchStage::parse(stage.as_str()), Some(*stage));
        }
    }

    #[test]
    fn the_profile_set_digest_changes_with_the_set_and_not_with_its_order() {
        let a = profile_set_digest(&[
            ("p1".to_owned(), vec!["e1".to_owned(), "e2".to_owned()]),
            ("p2".to_owned(), vec!["e3".to_owned()]),
        ]);
        let reordered = profile_set_digest(&[
            ("p2".to_owned(), vec!["e3".to_owned()]),
            ("p1".to_owned(), vec!["e2".to_owned(), "e1".to_owned()]),
        ]);
        assert_eq!(a, reordered, "the digest is order-free");

        let enrolled = profile_set_digest(&[
            ("p1".to_owned(), vec!["e1".to_owned(), "e2".to_owned()]),
            ("p2".to_owned(), vec!["e3".to_owned()]),
            ("p3".to_owned(), vec!["e4".to_owned()]),
        ]);
        assert_ne!(a, enrolled, "enrolling a person makes a fresh key");

        let exemplar_added = profile_set_digest(&[
            (
                "p1".to_owned(),
                vec!["e1".to_owned(), "e2".to_owned(), "e5".to_owned()],
            ),
            ("p2".to_owned(), vec!["e3".to_owned()]),
        ]);
        assert_ne!(a, exemplar_added, "adding an exemplar makes a fresh key");
        assert_eq!(a.len(), 64, "the digest is a sha256");
    }

    #[test]
    fn a_derived_identifier_is_stable_and_distinct_per_stage() {
        let job = JobId::new();
        let first = deterministic_uuid(&format!("{job}:diarize"));
        assert_eq!(first, deterministic_uuid(&format!("{job}:diarize")));
        assert_ne!(first, deterministic_uuid(&format!("{job}:embed")));
        assert_ne!(
            first,
            deterministic_uuid(&format!("{}:diarize", JobId::new()))
        );
    }
}
