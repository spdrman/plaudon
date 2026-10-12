//! The documents a `.recording-archive` carries (spec 13.6).
//!
//! These are the *restore* model: one document per archive entry, each
//! self-contained and each naming exactly the columns it carries. They are
//! separate from [`crate::bundle`], which is the human-readable export view,
//! because a restore needs identities and link rows that a Markdown export
//! has no business showing.
//!
//! Every struct is `deny_unknown_fields`: an archive produced by a newer
//! build that added a field is refused rather than silently restored with
//! that field dropped. The archive format version gate makes that explicit.

use serde::{Deserialize, Serialize};

/// Schema version of `records/<recording id>.json`.
pub const RECORD_SCHEMA: &str = "archive-record/1";
/// Schema version of `annotations/<recording id>.json`.
pub const ANNOTATION_SCHEMA: &str = "archive-annotations/1";
/// Schema version of `transcripts/<revision id>.json`.
pub const TRANSCRIPT_SCHEMA: &str = "archive-transcript/1";
/// Schema version of `outputs/<output id>.json`.
pub const OUTPUT_SCHEMA: &str = "archive-output/1";
/// Schema version of `sources/<source id>.json`.
pub const SOURCE_SCHEMA: &str = "archive-source/1";
/// Schema version of `tools/<tool id>.json`.
pub const TOOL_SCHEMA: &str = "archive-tool/1";

/// A stored media blob, as restored into `media_assets`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaAssetRow {
    /// Asset identity.
    pub id: String,
    /// Digest of the stored bytes.
    pub sha256: String,
    /// Length of the stored bytes.
    pub byte_length: u64,
    /// File extension used inside the archive and in the library path.
    pub extension: String,
    /// Declared MIME type, when known.
    pub mime: Option<String>,
    /// Observed codec, when probed.
    pub codec: Option<String>,
    /// Sample rate, when probed.
    pub sample_rate: Option<u32>,
    /// Channel count, when probed.
    pub channels: Option<u16>,
    /// Duration in milliseconds, when probed.
    pub duration_ms: Option<u64>,
    /// Fidelity class.
    pub quality_class: String,
    /// When the asset row was created.
    pub created_at: i64,
}

/// The link between a recording and one of its media candidates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordingMediaRow {
    /// Asset identity.
    pub media_asset_id: String,
    /// Fidelity class of the candidate.
    pub quality_class: String,
    /// When the candidate was acquired.
    pub obtained_at: i64,
}

/// A source association.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceItemRow {
    /// Row identity.
    pub id: String,
    /// Source the item belongs to.
    pub source_id: String,
    /// Stable external identifier, when the source exposes one.
    pub external_id: Option<String>,
    /// Import status.
    pub import_status: String,
    /// When the item was last seen.
    pub last_seen_at: Option<i64>,
    /// Remote version hint.
    pub remote_version: Option<String>,
    /// Remote lifecycle state.
    pub remote_state: String,
    /// Why the item is tombstoned, when it is.
    pub tombstone_reason: Option<String>,
    /// Row creation time.
    pub created_at: i64,
}

/// An immutable media revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaRevisionRow {
    /// Row identity.
    pub id: String,
    /// Asset the revision points at.
    pub media_asset_id: String,
    /// Source item the revision came from, when known.
    pub source_item_id: Option<String>,
    /// Source version hint.
    pub source_version: Option<String>,
    /// Previous revision, when this one superseded another.
    pub predecessor_id: Option<String>,
    /// When the bytes were obtained.
    pub obtained_at: i64,
}

/// Structural facts about one logical meeting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordingRow {
    /// Recording identity.
    pub id: String,
    /// Capture instant, when known.
    pub captured_at: Option<i64>,
    /// Duration in milliseconds, when known.
    pub duration_ms: Option<u64>,
    /// Preferred media candidate.
    pub preferred_media_asset_id: Option<String>,
    /// Row creation time.
    pub created_at: i64,
    /// Local deletion instant, when the recording is tombstoned.
    pub deleted_at: Option<i64>,
}

/// `records/<recording id>.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordDocument {
    /// Document schema version.
    pub schema_version: String,
    /// The recording row.
    pub recording: RecordingRow,
    /// Media assets this recording references.
    pub media_assets: Vec<MediaAssetRow>,
    /// Links from the recording to its candidates.
    pub recording_media: Vec<RecordingMediaRow>,
    /// Immutable media revisions.
    pub media_revisions: Vec<MediaRevisionRow>,
    /// Source associations.
    pub source_items: Vec<SourceItemRow>,
}

/// A speaker label, with its confirmation state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpeakerRow {
    /// Row identity.
    pub id: String,
    /// Stable cluster label.
    pub label: String,
    /// User-entered name, when confirmed.
    pub name: Option<String>,
    /// Provenance as recorded.
    pub provenance: String,
    /// Whether a human confirmed the identity.
    pub user_confirmed: bool,
}

/// `annotations/<recording id>.json`: everything the user typed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnnotationDocument {
    /// Document schema version.
    pub schema_version: String,
    /// Recording the annotations belong to.
    pub recording_id: String,
    /// User-editable title.
    pub title: String,
    /// User-editable free-form note.
    pub annotation: Option<String>,
    /// User-editable tags, sorted.
    pub tags: Vec<String>,
    /// Speaker labels and names.
    pub speakers: Vec<SpeakerRow>,
}

/// One transcript segment row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SegmentRow {
    /// Row identity, referenced by evidence anchors.
    pub id: String,
    /// Position in the revision.
    pub seq_index: u32,
    /// Start offset, when real times exist.
    pub start_ms: Option<u64>,
    /// End offset, when real times exist.
    pub end_ms: Option<u64>,
    /// Speaker attribution.
    pub speaker_id: Option<String>,
    /// Segment text.
    pub text: String,
    /// Engine-scoped confidence.
    pub confidence: Option<f64>,
    /// Scale the confidence is expressed on.
    pub confidence_scale: Option<String>,
    /// Declared alignment origin, denormalized as in the schema.
    pub alignment_origin: String,
    /// Word alignments.
    pub words: Vec<WordRow>,
}

/// One word alignment row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WordRow {
    /// Row identity.
    pub id: String,
    /// Position inside the segment.
    pub seq_index: u32,
    /// Start offset.
    pub start_ms: u64,
    /// End offset.
    pub end_ms: u64,
    /// The word.
    pub text: String,
    /// Engine-scoped confidence.
    pub confidence: Option<f64>,
    /// Scale the confidence is expressed on.
    pub confidence_scale: Option<String>,
}

/// A non-destructive quality flag row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptAnnotationRow {
    /// Row identity.
    pub id: String,
    /// Segment the flag applies to.
    pub segment_index: u32,
    /// Flag kind.
    pub kind: String,
    /// Why the flag was raised.
    pub reason: String,
    /// Start offset, when carried.
    pub start_ms: Option<u64>,
    /// End offset, when carried.
    pub end_ms: Option<u64>,
}

/// A transcript revision row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptRow {
    /// Revision identity.
    pub id: String,
    /// Parent revision.
    pub parent_id: Option<String>,
    /// How the revision came to exist.
    pub origin: String,
    /// Digest of the transcribed bytes.
    pub input_sha256: String,
    /// Producing engine.
    pub engine: String,
    /// Producing model.
    pub model_id: String,
    /// Model version.
    pub model_version: Option<String>,
    /// Locale.
    pub locale: Option<String>,
    /// Declared alignment origin.
    pub alignment_origin: String,
    /// Lifecycle status.
    pub status: String,
    /// Normalization profile.
    pub normalization_profile: String,
    /// Chunking strategy.
    pub chunking_strategy: String,
    /// Duration in milliseconds.
    pub duration_ms: Option<u64>,
    /// Creation instant.
    pub created_at: i64,
    /// Compute backend, when recorded.
    pub engine_backend: Option<String>,
    /// Canonical engine parameters, when recorded.
    pub engine_params_json: Option<String>,
}

/// `transcripts/<revision id>.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptDocument {
    /// Document schema version.
    pub schema_version: String,
    /// Recording the revision belongs to.
    pub recording_id: String,
    /// The revision row.
    pub revision: TranscriptRow,
    /// Ordered segments.
    pub segments: Vec<SegmentRow>,
    /// Quality flags.
    pub annotations: Vec<TranscriptAnnotationRow>,
}

/// One claim row, the parent of its anchors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimRow {
    /// Row identity.
    pub id: String,
    /// JSON Pointer into the output body.
    pub item_path: String,
    /// Claim kind.
    pub kind: String,
    /// Sensitivity category.
    pub category: String,
    /// The claim text.
    pub claim_text: String,
    /// Claim state.
    pub state: String,
    /// Entailment verdict.
    pub entailment: String,
    /// Review reasons, as stored JSON.
    pub review_reasons_json: String,
    /// Reviewer identity, when a human reviewed the claim.
    pub reviewer: Option<String>,
    /// Review instant, in epoch milliseconds.
    pub reviewed_at: Option<i64>,
    /// Whether the reviewer accepted the claim.
    pub review_accepted: Option<i64>,
    /// Reviewer note.
    pub review_note: Option<String>,
}

/// One evidence anchor row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceRow {
    /// Row identity.
    pub id: String,
    /// Claim the anchor belongs to.
    pub claim_id: String,
    /// Position of the anchor within the claim.
    pub position: i64,
    /// JSON Pointer into the output body.
    pub item_path: String,
    /// Revision the model cited, which need not exist.
    pub cited_revision_id: String,
    /// Segment position inside the cited revision.
    pub seq_index: i64,
    /// Inclusive start of the span, in `char` offsets.
    pub char_start: i64,
    /// Exclusive end of the span, in `char` offsets.
    pub char_end: i64,
    /// The quoted passage.
    pub quote: String,
    /// Verification status.
    pub verification_status: String,
    /// Why the anchor was rejected, when it was.
    pub rejection_code: Option<String>,
    /// The text actually occupying the span, when verified.
    pub matched_text: Option<String>,
    /// Alignment origin of the cited revision, when verified.
    pub alignment_origin: Option<String>,
    /// Playback position, only when the revision carries alignment.
    pub seek_ms: Option<i64>,
}

/// A generated output row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputRow {
    /// Output identity.
    pub id: String,
    /// Output type.
    pub output_type: String,
    /// Exact input revision.
    pub transcript_revision_id: String,
    /// Template version.
    pub template_version: String,
    /// Model identity.
    pub model_id: String,
    /// Prompt version.
    pub prompt_version: String,
    /// Output schema version.
    pub schema_version: String,
    /// The structured body, verbatim.
    pub body_json: String,
    /// Lifecycle status.
    pub status: String,
    /// Creation instant.
    pub created_at: i64,
}

/// `outputs/<output id>.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputDocument {
    /// Document schema version.
    pub schema_version: String,
    /// Recording the output belongs to.
    pub recording_id: String,
    /// The output row.
    pub output: OutputRow,
    /// Anchors, in stable order.
    pub evidence: Vec<EvidenceRow>,
    /// Claims, in stable order.
    pub claims: Vec<ClaimRow>,
}

/// `sources/<source id>.json`.
///
/// Carries the source's identity and policy, never its cursor, capability
/// snapshot or any credential handle (spec 13.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceDocument {
    /// Document schema version.
    pub schema_version: String,
    /// Source identity.
    pub id: String,
    /// Source kind.
    pub source_kind: String,
    /// Account or device scope.
    pub account_scope: String,
    /// Display name.
    pub display_name: String,
    /// Integration status.
    pub status: String,
    /// Synchronization policy.
    pub sync_policy: String,
    /// Row creation time.
    pub created_at: i64,
}

/// `tools/<tool id>.json`.
///
/// Tool *definitions* travel so a restored library renders tool outputs with
/// the template they were produced from. Tool runs are reproducible work, not
/// user data, and are not archived.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolDocument {
    /// Document schema version.
    pub schema_version: String,
    /// Tool identity.
    pub id: String,
    /// Tool version.
    pub version: String,
    /// Display name.
    pub display_name: String,
    /// Tool kind.
    pub kind: String,
    /// Prompt version.
    pub prompt_version: String,
    /// Output schema version.
    pub output_schema_version: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documents_refuse_unknown_fields() {
        let json = r#"{"schema_version":"archive-source/1","id":"s","source_kind":"local_file",
            "account_scope":"local","display_name":"Files","status":"ready","sync_policy":"manual",
            "created_at":1,"api_key":"sk-or-v1-abc"}"#;
        assert!(serde_json::from_str::<SourceDocument>(json).is_err());
    }

    #[test]
    fn a_record_document_round_trips() {
        let document = RecordDocument {
            schema_version: RECORD_SCHEMA.to_owned(),
            recording: RecordingRow {
                id: "r1".into(),
                captured_at: Some(5),
                duration_ms: Some(10),
                preferred_media_asset_id: Some("m1".into()),
                created_at: 1,
                deleted_at: None,
            },
            media_assets: vec![MediaAssetRow {
                id: "m1".into(),
                sha256: "a".repeat(64),
                byte_length: 10,
                extension: "wav".into(),
                mime: Some("audio/wav".into()),
                codec: Some("pcm".into()),
                sample_rate: Some(44_100),
                channels: Some(2),
                duration_ms: Some(10),
                quality_class: "user_supplied_file".into(),
                created_at: 1,
            }],
            recording_media: vec![RecordingMediaRow {
                media_asset_id: "m1".into(),
                quality_class: "user_supplied_file".into(),
                obtained_at: 1,
            }],
            media_revisions: Vec::new(),
            source_items: Vec::new(),
        };
        let json = serde_json::to_string(&document).unwrap();
        assert_eq!(
            serde_json::from_str::<RecordDocument>(&json).unwrap(),
            document
        );
    }
}
