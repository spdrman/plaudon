//! The `voiceprofiles/` archive namespace: biometric data in a backup, or not
//! at all (`VOICE_PROFILES_SPEC` VP-077, VP-078; ADR-0080; threat-model rows
//! TM-046/TM-048).
//!
//! # Three rules, each enforced by a different mechanism
//!
//! 1. **Off by default.** Nothing in this module runs unless the caller asked
//!    for voice profiles explicitly. `export_recording` cannot reach it at all:
//!    a recording export carries the resolved display-name *text* per speaker
//!    and nothing else (VP-077).
//! 2. **Encrypted or unrepresentable.** [`VoiceProfilePermit`] is the only way
//!    to stage an entry in this namespace, and the only way to obtain one is
//!    [`VoiceProfilePermit::for_protection`] with
//!    [`Protection::Encrypted`](crate::archive::Protection::Encrypted). There
//!    is no constructor that takes a plaintext archive, so "plaintext backup
//!    containing voiceprints" is not a state this crate can represent — which
//!    is the mitigation TM-048 leans on, because two *plaintext*-era exports
//!    compared side by side is exactly the cross-library correlation attack.
//! 3. **Nowhere else.** [`embedding_shape`] gives the archive secret scanner a
//!    second rule: an embedding-shaped value anywhere outside this namespace
//!    aborts the backup. A column copied into the wrong document by a future
//!    change fails loudly instead of travelling in the clear.
//!
//! # What travels, and what deliberately does not
//!
//! An entry carries the enrolment (consent version, retention term, status) and
//! its sealed exemplars: ciphertext, nonce, model binding and the provenance
//! that is still true. It does **not** carry
//! `speaker_suggestions`: a suggestion is a pending decision about a
//! diarization run, runs are not archived at all, and a restored library has no
//! run to attach one to. Re-running matching after a restore recomputes them
//! from scratch, which is also the honest outcome — a score produced on another
//! machine, against another threshold set, is not evidence here.
//!
//! The ciphertext travels sealed, and the key never does: the key lives in the
//! OS credential store under `SecretScope::VoiceProfileKey`, which no archive
//! can reach. Restoring voice profiles into a library whose key is gone (a
//! different machine, or one where "forget everyone" ran) therefore yields rows
//! that will not decrypt — by design, and the restore says so rather than
//! pretending otherwise.

use serde::{Deserialize, Serialize};

use super::writer::Protection;
use crate::error::{ExportError, ExportResult};

/// Namespace prefix of every entry this module produces.
pub const NAMESPACE: &str = "voiceprofiles/";

/// Longest retention term the schema accepts (VP-130).
pub const MAX_RETENTION_MONTHS: u8 = plaudon_persistence::voice_profiles::MAX_RETENTION_MONTHS;

/// Largest embedding this build will carry, mirroring the sealing store.
pub const MAX_EMBEDDING_DIM: usize = 8192;

/// Shortest exemplar the schema stores, mirroring the `CHECK` in migration
/// 0900 (VP-064).
pub const MIN_EXEMPLAR_SPEECH_MS: u64 = 1000;

/// Schema version of `voiceprofiles/<person id>.json`.
pub const VOICEPROFILE_SCHEMA: &str = "archive-voiceprofile/1";

/// Shortest run of continuous base64/hex that is treated as a binary blob.
///
/// A sealed 192-dimensional embedding is 784 bytes of ciphertext, i.e. over a
/// thousand base64 characters, so this bound sits far below the thing it is
/// looking for and far above anything the other documents carry: the longest
/// legitimate unbroken token in an archive document is a 64-character digest.
/// Annotation and transcript text is prose — it contains spaces, punctuation
/// and non-base64 characters, which end a run.
pub const BLOB_RUN_CHARS: usize = 512;

/// Field names that mean "this value is derived from somebody's voice".
///
/// A mistake in this area looks like a column being copied into a document with
/// its name intact, so the name is the cheapest and most reliable signal.
pub const EMBEDDING_FIELD_FRAGMENTS: &[&str] = &[
    "embedding",
    "voiceprint",
    "exemplar_cipher",
    "speaker_vector",
];

/// Permission to put an entry in the `voiceprofiles/` namespace.
///
/// Deliberately unconstructable except through [`Self::for_protection`]: the
/// type *is* the proof that the archive being written is encrypted, and it is
/// taken by value-reference at every call site that stages such an entry.
#[derive(Debug, Clone, Copy)]
pub struct VoiceProfilePermit {
    /// Private so no other module can forge one.
    _encrypted: (),
}

impl VoiceProfilePermit {
    /// A permit for an encrypted archive; a refusal for a plaintext one.
    ///
    /// # Errors
    ///
    /// [`ExportError::VoiceProfilesRequireEncryption`] for
    /// [`Protection::Plaintext`]. The refusal is the whole feature: a user who
    /// asks for voice profiles in an unencrypted backup is told no, not warned
    /// and obeyed.
    pub const fn for_protection(protection: &Protection) -> ExportResult<Self> {
        match protection {
            Protection::Encrypted { .. } => Ok(Self { _encrypted: () }),
            Protection::Plaintext => Err(ExportError::VoiceProfilesRequireEncryption),
        }
    }
}

/// Whether an entry path is inside the voice namespace.
#[must_use]
pub fn in_namespace(entry_path: &str) -> bool {
    entry_path.starts_with(NAMESPACE)
}

/// Why a value looks like biometric material.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbeddingShape {
    /// The field is named for an embedding, a voiceprint or its ciphertext.
    NamedField,
    /// The value is a long unbroken base64/hex run, i.e. a binary blob.
    BinaryBlob,
}

impl EmbeddingShape {
    /// Stable name, reported in the refusal.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NamedField => "voice_embedding_field",
            Self::BinaryBlob => "voice_embedding_blob",
        }
    }
}

/// Classify one JSON value as biometric-shaped, or not.
///
/// `key` is the field name the value sits under, when it has one. Both rules
/// are shape rules: this function never decrypts, never consults the database
/// and never sees a vector — it exists so that *if* one ever reached a document
/// it could not be written.
#[must_use]
pub fn embedding_shape(key: Option<&str>, value: &str) -> Option<EmbeddingShape> {
    if let Some(key) = key {
        let lowered = key.to_ascii_lowercase();
        if EMBEDDING_FIELD_FRAGMENTS
            .iter()
            .any(|fragment| lowered.contains(fragment))
        {
            return Some(EmbeddingShape::NamedField);
        }
    }
    if longest_blob_run(value) >= BLOB_RUN_CHARS {
        return Some(EmbeddingShape::BinaryBlob);
    }
    None
}

/// Longest run of base64/hex alphabet characters in `value`.
fn longest_blob_run(value: &str) -> usize {
    let mut longest = 0usize;
    let mut current = 0usize;
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || byte == b'+' || byte == b'/' || byte == b'=' {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    longest
}

/// One sealed exemplar, as it travels.
///
/// `embedding_cipher` and `embedding_nonce` are base64 (standard alphabet, with
/// padding) because an archive document is JSON and JSON has no bytes. Nothing
/// here is readable without the library's data key, which does not travel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExemplarRow {
    /// Row identity.
    pub id: String,
    /// Recording the exemplar was cut from, when that recording is still
    /// present. Provenance the target library cannot resolve is dropped on
    /// restore rather than fabricated.
    pub recording_id: Option<String>,
    /// Cluster within the recording's diarization run, when known.
    pub cluster_key: Option<String>,
    /// Start of the source span, in milliseconds.
    pub source_start_ms: Option<u64>,
    /// End of the source span, in milliseconds.
    pub source_end_ms: Option<u64>,
    /// Net speech the exemplar was built from, in milliseconds.
    pub speech_ms: u64,
    /// Embedding model identity.
    pub model_id: String,
    /// Embedding model version.
    pub model_version: String,
    /// Embedding model digest, 64 hex characters.
    pub model_sha256: String,
    /// Dimensionality of the sealed vector.
    pub embedding_dim: u32,
    /// Base64 XChaCha20-Poly1305 ciphertext.
    pub embedding_cipher_b64: String,
    /// Base64 nonce.
    pub embedding_nonce_b64: String,
    /// When the exemplar was stored, in epoch milliseconds.
    pub created_at: i64,
}

/// The enrolment itself: consent state and retention clock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnrollmentRow {
    /// Disclosure version the user accepted at the gate (VP-120).
    pub consent_version: String,
    /// Free-text basis the user recorded, when they did.
    pub consent_note: Option<String>,
    /// Retention term in months, 1..=12 (VP-130).
    pub retention_months: u8,
    /// Last time a match used this enrolment.
    pub last_matched_at: Option<i64>,
    /// `active` or `expired`.
    pub status: String,
    /// When retention elapsed, for an expired enrolment.
    pub expired_at: Option<i64>,
    /// When the enrolment was created.
    pub created_at: i64,
}

/// `voiceprofiles/<person id>.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VoiceProfileDocument {
    /// Document schema version.
    pub schema_version: String,
    /// Person the enrolment belongs to. The person's *name* lives in the
    /// ordinary `people/` namespace; this document carries no name at all, so
    /// an attacker holding only this entry holds unlabelled ciphertext.
    pub person_id: String,
    /// The enrolment.
    pub enrollment: EnrollmentRow,
    /// Its sealed exemplars.
    pub exemplars: Vec<ExemplarRow>,
}

impl VoiceProfileDocument {
    /// Archive path for this document.
    #[must_use]
    pub fn entry_path(person_id: &str) -> String {
        format!("{NAMESPACE}{person_id}.json")
    }

    /// Check a parsed document against what the schema promises.
    ///
    /// `deny_unknown_fields` catches a document from a build that added a
    /// field; this catches one whose fields are the right *shape* and the wrong
    /// *values* — a retention term beyond the twelve-month ceiling, a
    /// dimensionality the store would refuse, a span that ends before it
    /// starts. The restorer calls it before the first insert, so a hostile or
    /// corrupt archive is refused rather than partially applied and left for a
    /// `CHECK` constraint to catch halfway through.
    ///
    /// # Errors
    ///
    /// A sentence naming the field that is wrong. It never contains a value:
    /// the fields this validates sit next to biometric material.
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != VOICEPROFILE_SCHEMA {
            return Err(format!(
                "a voice-profile document must declare {VOICEPROFILE_SCHEMA}"
            ));
        }
        if self.person_id.is_empty() || self.person_id.len() > 64 {
            return Err("a voice-profile document names exactly one person".to_owned());
        }
        let enrollment = &self.enrollment;
        if !(1..=MAX_RETENTION_MONTHS).contains(&enrollment.retention_months) {
            return Err(format!(
                "retention is 1..={MAX_RETENTION_MONTHS} months (VP-130)"
            ));
        }
        if enrollment.consent_version.is_empty() || enrollment.consent_version.len() > 64 {
            return Err("an enrolment records the disclosure version it was made under".to_owned());
        }
        if enrollment.status != "active" && enrollment.status != "expired" {
            return Err("an enrolment is active or expired".to_owned());
        }
        for exemplar in &self.exemplars {
            if exemplar.id.is_empty() || exemplar.id.len() > 64 {
                return Err("an exemplar carries an identity".to_owned());
            }
            if exemplar.embedding_dim == 0 || exemplar.embedding_dim as usize > MAX_EMBEDDING_DIM {
                return Err(format!(
                    "an embedding has 1..={MAX_EMBEDDING_DIM} dimensions"
                ));
            }
            if exemplar.speech_ms < MIN_EXEMPLAR_SPEECH_MS {
                return Err("an exemplar below the enrolment minimum is not storable".to_owned());
            }
            if exemplar.model_sha256.len() != 64
                || !exemplar.model_sha256.bytes().all(|b| b.is_ascii_hexdigit())
            {
                return Err("an exemplar is bound to a model digest".to_owned());
            }
            if let (Some(start), Some(end)) = (exemplar.source_start_ms, exemplar.source_end_ms) {
                if end < start {
                    return Err("a source span cannot end before it starts".to_owned());
                }
            }
            if exemplar.embedding_cipher_b64.is_empty() || exemplar.embedding_nonce_b64.is_empty() {
                return Err("a sealed exemplar carries ciphertext and a nonce".to_owned());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::crypto::KdfProfile;

    fn encrypted() -> Protection {
        Protection::Encrypted {
            password: "correct horse battery staple".to_owned(),
            profile: KdfProfile::Backup,
        }
    }

    #[test]
    fn a_permit_exists_only_for_an_encrypted_archive() {
        assert!(VoiceProfilePermit::for_protection(&encrypted()).is_ok());
        assert!(matches!(
            VoiceProfilePermit::for_protection(&Protection::Plaintext),
            Err(ExportError::VoiceProfilesRequireEncryption)
        ));
    }

    #[test]
    fn an_embedding_named_field_is_recognised_whatever_its_value() {
        assert_eq!(
            embedding_shape(Some("embedding_cipher"), "short"),
            Some(EmbeddingShape::NamedField)
        );
        assert_eq!(
            embedding_shape(Some("speaker_VOICEPRINT"), ""),
            Some(EmbeddingShape::NamedField)
        );
        assert_eq!(embedding_shape(Some("title"), "Weekly sync"), None);
    }

    #[test]
    fn a_long_unbroken_base64_run_is_a_binary_blob() {
        let blob = "A".repeat(BLOB_RUN_CHARS);
        assert_eq!(
            embedding_shape(Some("note"), &blob),
            Some(EmbeddingShape::BinaryBlob)
        );
        // One character short, and prose of any length, are not blobs.
        assert_eq!(
            embedding_shape(Some("note"), &"A".repeat(BLOB_RUN_CHARS - 1)),
            None
        );
        let prose = "the quick brown fox jumps over the lazy dog ".repeat(100);
        assert_eq!(embedding_shape(Some("annotation"), &prose), None);
    }

    #[test]
    fn a_digest_and_a_uuid_are_not_blobs() {
        assert_eq!(embedding_shape(Some("sha256"), &"a".repeat(64)), None);
        assert_eq!(
            embedding_shape(Some("id"), "018f3b3a-7b4f-7c21-9a3e-2b1f0c5d6e7a"),
            None
        );
    }

    #[test]
    fn the_namespace_test_is_prefix_exact() {
        assert!(in_namespace("voiceprofiles/p1.json"));
        assert!(!in_namespace("records/r1.json"));
        assert!(!in_namespace("notvoiceprofiles/p1.json"));
    }

    #[test]
    fn a_document_refuses_an_unknown_field() {
        let json = r#"{"schema_version":"archive-voiceprofile/1","person_id":"p1",
            "enrollment":{"consent_version":"v1","consent_note":null,"retention_months":12,
            "last_matched_at":null,"status":"active","expired_at":null,"created_at":1},
            "exemplars":[],"embedding":"AAAA"}"#;
        assert!(serde_json::from_str::<VoiceProfileDocument>(json).is_err());
    }
}
