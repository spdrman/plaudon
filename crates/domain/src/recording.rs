//! Logical recordings and their media candidates (spec 6.3B).
//!
//! A recording is the user-owned logical meeting. It can acquire several media
//! candidates of different fidelity — for example a compressed BLE sidecar and
//! a later original WAV — and acquiring one never overwrites another.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::ids::{MediaAssetId, RecordingId};
use crate::media::QualityClass;
use crate::time::Timestamp;

/// One acquired media candidate attached to a recording.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordingMedia {
    /// Stored asset backing this candidate.
    pub media_asset_id: MediaAssetId,
    /// Fidelity class of the candidate.
    pub quality_class: QualityClass,
    /// When the candidate was acquired.
    pub obtained_at: Timestamp,
}

/// Reason a media candidate could not be attached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RecordingMediaError {
    /// The candidate is already attached to this recording.
    #[error("media candidate is already attached to this recording")]
    AlreadyAttached,
    /// The requested preferred candidate is not attached to this recording.
    #[error("preferred media candidate is not attached to this recording")]
    NotAttached,
}

/// A user-owned logical meeting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recording {
    /// Durable recording identity.
    pub id: RecordingId,
    /// User-editable title.
    pub title: String,
    /// When the audio was captured, when known.
    pub captured_at: Option<Timestamp>,
    /// Duration in milliseconds, when known.
    pub duration_ms: Option<u64>,
    /// User-editable tags.
    pub tags: BTreeSet<String>,
    /// User-editable free-form annotation.
    pub annotation: Option<String>,
    /// Row creation time.
    pub created_at: Timestamp,
    /// Local tombstone time, if the user deleted this recording.
    pub deleted_at: Option<Timestamp>,
    media_candidates: Vec<RecordingMedia>,
    preferred_media_asset_id: Option<MediaAssetId>,
}

impl Recording {
    /// Create an empty recording with no media attached yet.
    #[must_use]
    pub fn new(id: RecordingId, title: impl Into<String>, created_at: Timestamp) -> Self {
        Self {
            id,
            title: title.into(),
            captured_at: None,
            duration_ms: None,
            tags: BTreeSet::new(),
            annotation: None,
            created_at,
            deleted_at: None,
            media_candidates: Vec::new(),
            preferred_media_asset_id: None,
        }
    }

    /// All attached media candidates, in acquisition order.
    #[must_use]
    pub fn media_candidates(&self) -> &[RecordingMedia] {
        &self.media_candidates
    }

    /// The explicitly preferred candidate, if one has been chosen.
    #[must_use]
    pub const fn preferred_media_asset_id(&self) -> Option<MediaAssetId> {
        self.preferred_media_asset_id
    }

    /// Attach a media candidate without disturbing existing candidates.
    ///
    /// The first candidate becomes the preferred one. Subsequent candidates do
    /// not change the preference: promoting a higher-fidelity candidate is an
    /// explicit action (spec 6.3B).
    pub fn attach_media(&mut self, candidate: RecordingMedia) -> Result<(), RecordingMediaError> {
        if self.media_candidates.iter().any(|c| c.media_asset_id == candidate.media_asset_id) {
            return Err(RecordingMediaError::AlreadyAttached);
        }
        if self.preferred_media_asset_id.is_none() {
            self.preferred_media_asset_id = Some(candidate.media_asset_id);
        }
        self.media_candidates.push(candidate);
        Ok(())
    }

    /// Explicitly promote an attached candidate to preferred.
    pub fn set_preferred_media(
        &mut self,
        media_asset_id: MediaAssetId,
    ) -> Result<(), RecordingMediaError> {
        if !self.media_candidates.iter().any(|c| c.media_asset_id == media_asset_id) {
            return Err(RecordingMediaError::NotAttached);
        }
        self.preferred_media_asset_id = Some(media_asset_id);
        Ok(())
    }

    /// Whether the user has deleted this recording locally.
    #[must_use]
    pub const fn is_tombstoned(&self) -> bool {
        self.deleted_at.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(quality: QualityClass) -> RecordingMedia {
        RecordingMedia {
            media_asset_id: MediaAssetId::new(),
            quality_class: quality,
            obtained_at: Timestamp::from_millis(1),
        }
    }

    #[test]
    fn a_recording_holds_several_media_candidates_without_overwriting() {
        let mut recording = Recording::new(RecordingId::new(), "Standup", Timestamp::EPOCH);
        let ble = candidate(QualityClass::CompressedDeviceTransfer);
        let usb = candidate(QualityClass::OriginalDeviceRecording);
        recording.attach_media(ble.clone()).unwrap();
        recording.attach_media(usb.clone()).unwrap();

        assert_eq!(recording.media_candidates(), [ble.clone(), usb.clone()]);
        assert_eq!(
            recording.preferred_media_asset_id(),
            Some(ble.media_asset_id),
            "attaching a better candidate must not silently change the preference"
        );

        recording.set_preferred_media(usb.media_asset_id).unwrap();
        assert_eq!(recording.preferred_media_asset_id(), Some(usb.media_asset_id));
        assert_eq!(recording.media_candidates().len(), 2, "promotion must not remove a candidate");
    }

    #[test]
    fn attaching_the_same_candidate_twice_is_rejected() {
        let mut recording = Recording::new(RecordingId::new(), "Standup", Timestamp::EPOCH);
        let only = candidate(QualityClass::UserSuppliedFile);
        recording.attach_media(only.clone()).unwrap();
        assert_eq!(recording.attach_media(only), Err(RecordingMediaError::AlreadyAttached));
    }

    #[test]
    fn an_unattached_candidate_cannot_be_preferred() {
        let mut recording = Recording::new(RecordingId::new(), "Standup", Timestamp::EPOCH);
        assert_eq!(
            recording.set_preferred_media(MediaAssetId::new()),
            Err(RecordingMediaError::NotAttached)
        );
    }
}
