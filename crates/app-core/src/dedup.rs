//! Content-hash deduplication that never merges logical meetings (spec 6.3B, 1.5).
//!
//! The rule has two halves that are easy to confuse:
//!
//! * **Blob level.** One stored file per verified `(sha256, byte_length)`.
//!   Reuse is only permitted after the existing bytes have been re-verified.
//! * **Record level.** Nothing is merged. Two source items with identical
//!   bytes stay two recordings with independent titles, tags and annotations.

use plaudon_domain::ids::MediaAssetId;
use plaudon_domain::media::{MediaAsset, MediaIdentity, VerificationState};

/// What the importer should do with the bytes it just staged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlobDecision {
    /// No blob exists for this identity; store the staged file.
    StoreNew,
    /// A verified blob already exists; reuse it and discard the staged copy.
    ReuseExisting(MediaAssetId),
    /// A blob exists but its bytes no longer verify. The import must fail
    /// rather than reuse it (spec: a corrupt existing blob fails the import
    /// with `IntegrityFailed`, never silent reuse).
    ExistingCorrupt(MediaAssetId),
}

/// Deduplication policy over `media_assets`.
#[derive(Debug, Clone, Copy, Default)]
pub struct DedupPolicy;

impl DedupPolicy {
    /// Decide what to do with newly staged bytes.
    ///
    /// `existing` is the row found for the identity, and `bytes_verify` is the
    /// result of re-reading and digesting that row's file. The verification is
    /// passed in rather than performed here so this stays a pure decision.
    #[must_use]
    pub fn decide(existing: Option<&MediaAsset>, bytes_verify: Option<bool>) -> BlobDecision {
        match (existing, bytes_verify) {
            (None, _) => BlobDecision::StoreNew,
            (Some(asset), Some(true)) => BlobDecision::ReuseExisting(asset.id),
            // A row that exists but could not be verified is never reused:
            // unknown is not "fine".
            (Some(asset), _) => BlobDecision::ExistingCorrupt(asset.id),
        }
    }

    /// Whether two media identities may share one stored blob.
    #[must_use]
    pub fn share_blob(a: &MediaIdentity, b: &MediaIdentity) -> bool {
        a == b
    }

    /// Whether two source items may be merged into one logical recording.
    ///
    /// Never. Identical bytes do not make two meetings the same meeting
    /// (spec 6.3B), so this is a function that documents the invariant and is
    /// asserted by tests rather than a policy with configuration.
    #[must_use]
    pub const fn may_merge_recordings() -> bool {
        false
    }

    /// Whether a stored asset is in a state that permits reuse.
    #[must_use]
    pub fn reusable(asset: &MediaAsset) -> bool {
        asset.verification_state == VerificationState::Verified
    }
}

#[cfg(test)]
mod tests {
    use plaudon_domain::media::{QualityClass, Sha256Digest};
    use plaudon_domain::time::Timestamp;

    use super::*;

    fn asset(bytes: &[u8]) -> MediaAsset {
        let identity = MediaIdentity::of_bytes(bytes, "wav");
        MediaAsset {
            id: MediaAssetId::new(),
            relative_path: format!("media/sha256/ab/{}.wav", identity.sha256.to_hex()),
            identity,
            quality_class: QualityClass::UserSuppliedFile,
            verification_state: VerificationState::Verified,
            verified_at: Some(Timestamp::EPOCH),
        }
    }

    #[test]
    fn absent_bytes_are_stored() {
        assert_eq!(DedupPolicy::decide(None, None), BlobDecision::StoreNew);
    }

    #[test]
    fn a_verified_existing_blob_is_reused() {
        let existing = asset(b"audio");
        assert_eq!(
            DedupPolicy::decide(Some(&existing), Some(true)),
            BlobDecision::ReuseExisting(existing.id)
        );
    }

    #[test]
    fn a_corrupt_or_unverifiable_existing_blob_is_never_reused() {
        let existing = asset(b"audio");
        assert_eq!(
            DedupPolicy::decide(Some(&existing), Some(false)),
            BlobDecision::ExistingCorrupt(existing.id)
        );
        assert_eq!(
            DedupPolicy::decide(Some(&existing), None),
            BlobDecision::ExistingCorrupt(existing.id),
            "an unverified blob must not be reused"
        );
    }

    #[test]
    fn identity_sharing_requires_digest_and_length() {
        let a = MediaIdentity::of_bytes(b"same", "wav");
        let b = MediaIdentity::of_bytes(b"same", "mp3");
        let c = MediaIdentity::new(Sha256Digest::of(b"same"), 99, "wav");
        assert!(DedupPolicy::share_blob(&a, &b));
        assert!(!DedupPolicy::share_blob(&a, &c));
    }

    #[test]
    fn recordings_are_never_merged() {
        assert!(!DedupPolicy::may_merge_recordings());
    }

    #[test]
    fn only_verified_assets_are_reusable() {
        let mut existing = asset(b"audio");
        assert!(DedupPolicy::reusable(&existing));
        existing.verification_state = VerificationState::Unverified;
        assert!(!DedupPolicy::reusable(&existing));
        existing.verification_state = VerificationState::Failed;
        assert!(!DedupPolicy::reusable(&existing));
    }
}
