//! A deterministic in-memory [`RecordingSource`].

use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use plaudon_domain::media::MediaIdentity;
use plaudon_recording_sources::source::{
    AcquiredAsset, RecordingSource, SourceCapabilities, SourceError, SourceItem, SourcePage,
};

/// One page of canned enumeration results plus the bytes each item yields.
#[derive(Debug, Clone)]
pub struct FakeSourceItem {
    /// The item as enumerated.
    pub item: SourceItem,
    /// Bytes `acquire` writes for this item.
    pub bytes: Vec<u8>,
}

/// A recording source that serves canned pages from memory.
#[derive(Debug)]
pub struct FakeSource {
    capabilities: SourceCapabilities,
    pages: Vec<Vec<FakeSourceItem>>,
    next_error: Mutex<Option<SourceError>>,
    acquisitions: Arc<Mutex<Vec<String>>>,
}

impl FakeSource {
    /// Create a source serving `pages` in order.
    #[must_use]
    pub fn new(capabilities: SourceCapabilities, pages: Vec<Vec<FakeSourceItem>>) -> Self {
        Self {
            capabilities,
            pages,
            next_error: Mutex::new(None),
            acquisitions: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Make the next `list` or `acquire` call fail with `error`.
    pub fn fail_next(&self, error: SourceError) {
        *self.next_error.lock().expect("fake source lock") = Some(error);
    }

    /// Display names of every acquisition performed so far.
    #[must_use]
    pub fn acquisitions(&self) -> Vec<String> {
        self.acquisitions.lock().expect("fake source lock").clone()
    }

    fn take_error(&self) -> Option<SourceError> {
        self.next_error.lock().expect("fake source lock").take()
    }
}

#[async_trait]
impl RecordingSource for FakeSource {
    async fn capabilities(&self) -> SourceCapabilities {
        self.capabilities.clone()
    }

    async fn list(&self, cursor: Option<&str>) -> Result<SourcePage, SourceError> {
        if let Some(error) = self.take_error() {
            return Err(error);
        }
        let index: usize = match cursor {
            None => 0,
            Some(raw) => raw.parse().map_err(|_| SourceError::MalformedOutput)?,
        };
        let Some(page) = self.pages.get(index) else {
            return Ok(SourcePage {
                items: Vec::new(),
                next_cursor: None,
            });
        };
        let next_cursor = if index + 1 < self.pages.len() {
            Some((index + 1).to_string())
        } else {
            None
        };
        Ok(SourcePage {
            items: page.iter().map(|entry| entry.item.clone()).collect(),
            next_cursor,
        })
    }

    async fn acquire(
        &self,
        item: &SourceItem,
        target: &Path,
    ) -> Result<AcquiredAsset, SourceError> {
        if let Some(error) = self.take_error() {
            return Err(error);
        }
        let entry = self
            .pages
            .iter()
            .flatten()
            .find(|candidate| candidate.item.identity == item.identity)
            .ok_or(SourceError::AssetNotFound)?;
        tokio::fs::write(target, &entry.bytes)
            .await
            .map_err(|_| SourceError::DownloadInterrupted)?;
        self.acquisitions
            .lock()
            .expect("fake source lock")
            .push(item.display_name.clone());
        Ok(AcquiredAsset {
            identity: MediaIdentity::of_bytes(&entry.bytes, "pcm_s16le"),
            quality_class: item.quality_class,
            staged_path: target.to_path_buf(),
        })
    }
}
