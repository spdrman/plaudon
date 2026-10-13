//! The bundle-tier model artifacts, as facts (ADR-0081 §1, VP-040, VP-051).
//!
//! An artifact is identified by *exact file name and digest*, never by a
//! release tag and a glob. That is not pedantry: the sherpa-onnx release tags
//! that carry the permissively-licensed WeSpeaker export also carry Rev's
//! non-commercial models, and a download loop that takes "the ONNX file in
//! this tag" would eventually take one of those. The names below are the ones
//! published upstream, including the upstream tag's spelling mistake.
//!
//! Every digest here was computed from a downloaded file on 2026-10-12 and is
//! recorded in `docs/licensing/third-party-register.md`'s model-weights table,
//! in `NOTICE` and in `scripts/fetch-speaker-models.sh`. The integration test
//! `licensing_register::the_speaker_model_artifacts_match_the_register` fails
//! the build when those four stop agreeing, so there is no "the document says
//! one thing, the code another" state to discover later.
//!
//! Nothing here downloads anything, and nothing here is enabled in a default
//! build: these are the records a model pack is verified against.

use std::fmt;

/// One redistributable model artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelArtifact {
    /// Stable model identifier used by the matching core's [`ModelBinding`].
    ///
    /// [`ModelBinding`]: plaudon_app_core::voice_profiles::matching::ModelBinding
    pub model_id: &'static str,
    /// Checkpoint version string, part of the on-disk model path.
    pub version: &'static str,
    /// Name shown to the user before the download starts (§12).
    pub display_name: &'static str,
    /// File name as published upstream.
    pub source_file_name: &'static str,
    /// File name the model pack stores it under.
    pub file_name: &'static str,
    /// Exact size in bytes of the published file.
    pub byte_length: u64,
    /// Lowercase hexadecimal SHA-256 of the published file.
    pub sha256_hex: &'static str,
    /// SPDX identifier of the licence the *weights* ship under.
    pub licence_spdx: &'static str,
    /// Copyright holder named in the licence text.
    pub licence_holder: &'static str,
    /// Attribution line, verbatim, as it appears in `NOTICE`.
    pub attribution: &'static str,
    /// Exact URL the bytes are fetched from.
    pub source_url: &'static str,
    /// URL of the licence text that travels with the weights.
    pub licence_url: &'static str,
    /// What this artifact is for, in one line.
    pub role: &'static str,
}

impl ModelArtifact {
    /// Path inside a model pack, relative to the pack root.
    #[must_use]
    pub fn pack_relative_path(&self) -> String {
        format!(
            "models/{}/{}/{}",
            self.model_id, self.version, self.file_name
        )
    }

    /// Whether `digest` is this artifact's published digest.
    #[must_use]
    pub fn matches_digest(&self, digest: &str) -> bool {
        digest.eq_ignore_ascii_case(self.sha256_hex)
    }
}

impl fmt::Display for ModelArtifact {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.display_name, self.licence_spdx)
    }
}

/// The speaker-embedding checkpoint: WeSpeaker ResNet34-LM, 256-dimensional.
///
/// The file published in the sherpa-onnx release is the upstream WeSpeaker
/// export with 241 bytes of ONNX `metadata_props` appended — `framework`,
/// `language`, the upstream `url`, `sample_rate=16000`, `output_dim=256` and
/// `normalize_samples=0`. Verified on 2026-10-12 by byte comparison: the first
/// 26 530 309 bytes are identical to
/// `https://wespeaker-1256283475.cos.ap-shanghai.myqcloud.com/models/voxceleb/voxceleb_resnet34_LM.onnx`
/// (sha256 `7bb2f06e9df17cdf1ef14ee8a15ab08ed28e8d0ef5054ee135741560df2ec068`),
/// so the graph and the weights are upstream's and only the metadata is the
/// mirror's. That mirror is the acquisition target because the metadata is
/// what states the front end's contract.
pub const WESPEAKER_RESNET34_LM: ModelArtifact = ModelArtifact {
    model_id: "wespeaker-resnet34-lm",
    version: "voxceleb-2022-10",
    display_name: "WeSpeaker ResNet34-LM (English, VoxCeleb)",
    source_file_name: "wespeaker_en_voxceleb_resnet34_LM.onnx",
    file_name: "wespeaker_en_voxceleb_resnet34_LM.onnx",
    byte_length: 26_530_550,
    sha256_hex: "e9848563da86f263117134dfd7ad63c92355b37de492b55e325400c9d9c39012",
    licence_spdx: "CC-BY-4.0",
    licence_holder: "WeSpeaker authors (WeNet Open Source Community)",
    attribution: "WeSpeaker ResNet34-LM speaker-embedding weights, \
                  (c) WeSpeaker authors, CC-BY-4.0; trained on VoxCeleb \
                  (Nagrani, Chung, Zisserman; Visual Geometry Group, \
                  University of Oxford), CC-BY-4.0",
    source_url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/\
                 speaker-recongition-models/wespeaker_en_voxceleb_resnet34_LM.onnx",
    licence_url: "https://github.com/wenet-e2e/wespeaker/blob/master/docs/pretrained.md",
    role: "256-dimensional speaker embedding over a window of single-speaker speech",
};

/// The segmentation checkpoint: pyannote segmentation-3.0, exported to ONNX.
///
/// Published un-gated with its MIT licence text, which is what makes it
/// usable at all: the Hugging Face original is behind an account-and-terms
/// gate that ADR-0400 condition 3 forbids on a default path.
pub const PYANNOTE_SEGMENTATION_3_0: ModelArtifact = ModelArtifact {
    model_id: "pyannote-segmentation",
    version: "3.0",
    display_name: "pyannote segmentation-3.0",
    source_file_name: "model.onnx",
    file_name: "pyannote-segmentation-3.0.onnx",
    byte_length: 5_992_913,
    sha256_hex: "220ad67ca923bef2fa91f2390c786097bf305bceb5e261d4af67b38e938e1079",
    licence_spdx: "MIT",
    licence_holder: "CNRS",
    attribution: "pyannote segmentation-3.0, (c) 2022 CNRS, MIT licence; \
                  ONNX export published by the sherpa-onnx project",
    source_url: "https://huggingface.co/csukuangfj/\
                 sherpa-onnx-pyannote-segmentation-3-0/resolve/main/model.onnx",
    licence_url: "https://huggingface.co/csukuangfj/\
                  sherpa-onnx-pyannote-segmentation-3-0/resolve/main/LICENSE",
    role: "frame-level speaker-activity segmentation, including overlap",
};

/// Every artifact the bundle tier would install, in display order.
pub const BUNDLE_TIER: &[ModelArtifact] = &[WESPEAKER_RESNET34_LM, PYANNOTE_SEGMENTATION_3_0];

/// Dimensionality of a [`WESPEAKER_RESNET34_LM`] embedding.
pub const WESPEAKER_EMBEDDING_DIM: usize = 256;

/// Sample rate every bundle-tier artifact expects, in Hz.
pub const MODEL_SAMPLE_RATE_HZ: u32 = 16_000;

/// Why an installed artifact was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ArtifactError {
    /// The file is not the size the record states.
    #[error("{artifact}: expected {expected} byte(s), found {found}")]
    Size {
        /// Artifact that was checked.
        artifact: &'static str,
        /// Size the record states.
        expected: u64,
        /// Size on disk.
        found: u64,
    },
    /// The file's digest is not the record's digest.
    #[error("{artifact}: digest {found} is not the published {expected}")]
    Digest {
        /// Artifact that was checked.
        artifact: &'static str,
        /// Digest the record states.
        expected: &'static str,
        /// Digest observed on disk.
        found: String,
    },
    /// The file could not be read at all.
    #[error("{artifact}: {reason}")]
    Unreadable {
        /// Artifact that was checked.
        artifact: &'static str,
        /// What went wrong.
        reason: String,
    },
}

/// Verify a file against an artifact record.
///
/// Size first, then digest: a truncated download is the common case and
/// hashing 26 MB to learn it is wasted work.
///
/// # Errors
///
/// [`ArtifactError`] when the file is missing, the wrong size or the wrong
/// bytes. There is no "warn and continue" path: a biometric model whose
/// provenance is not the reviewed one is not a model we have reviewed.
pub fn verify_artifact(
    artifact: &'static ModelArtifact,
    path: &std::path::Path,
) -> Result<(), ArtifactError> {
    use sha2::{Digest, Sha256};
    use std::io::Read;

    let mut file = std::fs::File::open(path).map_err(|source| ArtifactError::Unreadable {
        artifact: artifact.model_id,
        reason: source.to_string(),
    })?;
    let length = file
        .metadata()
        .map_err(|source| ArtifactError::Unreadable {
            artifact: artifact.model_id,
            reason: source.to_string(),
        })?
        .len();
    if length != artifact.byte_length {
        return Err(ArtifactError::Size {
            artifact: artifact.model_id,
            expected: artifact.byte_length,
            found: length,
        });
    }

    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|source| ArtifactError::Unreadable {
                artifact: artifact.model_id,
                reason: source.to_string(),
            })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let found = hex::encode(hasher.finalize());
    if !artifact.matches_digest(&found) {
        return Err(ArtifactError::Digest {
            artifact: artifact.model_id,
            expected: artifact.sha256_hex,
            found,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn every_artifact_records_a_full_digest_and_a_real_licence() {
        for artifact in BUNDLE_TIER {
            assert_eq!(
                artifact.sha256_hex.len(),
                64,
                "{} has a truncated digest",
                artifact.model_id
            );
            assert!(
                artifact
                    .sha256_hex
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                "{} has a malformed digest",
                artifact.model_id
            );
            assert!(artifact.byte_length > 0);
            assert!(
                matches!(artifact.licence_spdx, "MIT" | "CC-BY-4.0"),
                "{} carries an unreviewed licence {}",
                artifact.model_id,
                artifact.licence_spdx
            );
            assert!(
                artifact.attribution.len() > 16,
                "{} has no usable attribution line",
                artifact.model_id
            );
            assert!(artifact.source_url.starts_with("https://"));
            assert!(artifact.licence_url.starts_with("https://"));
        }
    }

    #[test]
    fn an_artifact_is_acquired_by_exact_file_name_never_by_a_tag() {
        for artifact in BUNDLE_TIER {
            assert!(
                artifact.source_url.ends_with(artifact.source_file_name),
                "{} is not fetched by its exact file name",
                artifact.model_id
            );
            assert!(
                !artifact.source_url.contains('*'),
                "{} is fetched by a glob",
                artifact.model_id
            );
        }
    }

    #[test]
    fn the_attribution_names_cnrs_wespeaker_and_voxceleb() {
        let all: String = BUNDLE_TIER
            .iter()
            .map(|artifact| artifact.attribution)
            .collect::<Vec<_>>()
            .join("\n");
        for required in ["CNRS", "WeSpeaker", "VoxCeleb"] {
            assert!(all.contains(required), "no attribution names {required}");
        }
    }

    #[test]
    fn a_file_of_the_wrong_size_is_refused_before_it_is_hashed() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("short.onnx");
        std::fs::File::create(&path)
            .expect("create")
            .write_all(b"not a model")
            .expect("write");
        let error = verify_artifact(&WESPEAKER_RESNET34_LM, &path).expect_err("refused");
        assert!(matches!(error, ArtifactError::Size { .. }));
    }

    #[test]
    fn a_file_of_the_right_size_but_the_wrong_bytes_is_refused() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("impostor.onnx");
        let filler = vec![0u8; PYANNOTE_SEGMENTATION_3_0.byte_length as usize];
        std::fs::write(&path, &filler).expect("write");
        let error = verify_artifact(&PYANNOTE_SEGMENTATION_3_0, &path).expect_err("refused");
        match error {
            ArtifactError::Digest { expected, .. } => {
                assert_eq!(expected, PYANNOTE_SEGMENTATION_3_0.sha256_hex);
            }
            other => panic!("expected a digest refusal, got {other}"),
        }
    }

    #[test]
    fn a_missing_file_is_an_error_not_an_empty_success() {
        let error = verify_artifact(
            &WESPEAKER_RESNET34_LM,
            std::path::Path::new("/nonexistent/model.onnx"),
        )
        .expect_err("refused");
        assert!(matches!(error, ArtifactError::Unreadable { .. }));
    }

    #[test]
    fn pack_paths_are_versioned_per_checkpoint() {
        assert_eq!(
            WESPEAKER_RESNET34_LM.pack_relative_path(),
            "models/wespeaker-resnet34-lm/voxceleb-2022-10/wespeaker_en_voxceleb_resnet34_LM.onnx"
        );
    }
}
