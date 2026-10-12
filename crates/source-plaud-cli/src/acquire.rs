//! Streaming acquisition into staging (spec 7.3, 13.3A).
//!
//! The order of operations is the contract:
//!
//! 1. ask the CLI for a fresh signed URL **at the moment the import starts**,
//!    never earlier and never from a cache;
//! 2. authorize it, and every redirect hop, against [`DownloadPolicy`];
//! 3. stream into `<target>.part` inside the staging directory, counting bytes
//!    and hashing as we go, failing the moment the cap is exceeded;
//! 4. `fsync` the file, sniff the container from the *bytes* rather than from
//!    a filename, and verify the declared length when one was given;
//! 5. rename `.part` onto the target and `fsync` the directory, so a crash
//!    leaves either nothing or a complete file — never a half file under the
//!    final name.
//!
//! The signed URL lives in a [`Zeroizing<String>`] for the duration of one
//! attempt and is never returned, stored, logged or placed in an error.
//! Resume is not implemented: spec 7.3 forbids it until range semantics and a
//! strong validator have been verified against the host, and
//! [`UrlContract::may_resume`] is `false` for every contract this build ships.

use std::fmt;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use plaudon_audio::sniff::SNIFF_WINDOW_BYTES;
use plaudon_audio::{sniff_container, SniffedContainer};
use plaudon_domain::media::{MediaIdentity, QualityClass, Sha256Digest};
use plaudon_domain::source::ExternalRecordingId;
use plaudon_recording_sources::source::{AcquiredAsset, SourceError, SourceItem};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::download::{DownloadPolicy, DownloadRejection, UrlContract};
use crate::transport::HttpFetcher;

/// Suffix of an in-progress transfer.
pub const PART_SUFFIX: &str = "part";

/// Supplies a transient download URL, just in time.
///
/// Implemented by the connector over the CLI runner. It is a trait so the
/// acquisition pipeline can be tested without a subprocess, and so the URL
/// never has to be threaded through a long call chain where it could be
/// captured in a log span.
#[async_trait]
pub trait SignedUrlProvider: Send + Sync + fmt::Debug {
    /// Ask the vendor tool for a fresh URL for one recording.
    async fn fresh_url(
        &self,
        external_id: &ExternalRecordingId,
    ) -> Result<Zeroizing<String>, SourceError>;
}

/// Caps applied to one acquisition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcquireLimits {
    /// Largest file we will write.
    pub max_bytes: u64,
    /// Free space we require beyond the declared length before starting.
    pub headroom_bytes: u64,
}

impl Default for AcquireLimits {
    fn default() -> Self {
        Self {
            max_bytes: 8 * 1024 * 1024 * 1024,
            headroom_bytes: 64 * 1024 * 1024,
        }
    }
}

/// What one acquisition produced, beyond the asset itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcquisitionReport {
    /// The verified asset.
    pub asset: AcquiredAsset,
    /// Container sniffed from the first bytes.
    pub container: SniffedContainer,
    /// Whether a fresh URL had to be requested because the first expired.
    pub retried_after_expiry: bool,
    /// Redirect hops followed, for the observed-contract record.
    pub redirect_hops: u8,
}

/// Streams a signed URL into staging and verifies it.
#[derive(Debug, Clone)]
pub struct AcquisitionPipeline {
    policy: Arc<DownloadPolicy>,
    fetcher: Arc<dyn HttpFetcher>,
    limits: AcquireLimits,
    contract: UrlContract,
}

impl AcquisitionPipeline {
    /// Build a pipeline.
    #[must_use]
    pub fn new(
        policy: Arc<DownloadPolicy>,
        fetcher: Arc<dyn HttpFetcher>,
        contract: UrlContract,
    ) -> Self {
        Self {
            policy,
            fetcher,
            limits: AcquireLimits::default(),
            contract,
        }
    }

    /// Replace the default caps.
    #[must_use]
    pub const fn with_limits(mut self, limits: AcquireLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Whether resume is permitted against the configured host.
    #[must_use]
    pub const fn may_resume(&self) -> bool {
        self.contract.may_resume()
    }

    /// Acquire one item into `target`.
    ///
    /// `target` must be a path inside a staging directory that the caller
    /// owns; the pipeline writes `<target>.part` next to it and renames within
    /// the same directory, so there is no cross-volume rename to assume.
    pub async fn acquire(
        &self,
        provider: &dyn SignedUrlProvider,
        item: &SourceItem,
        target: &Path,
    ) -> Result<AcquisitionReport, SourceError> {
        let external_id = item
            .identity
            .external_id
            .as_ref()
            .ok_or(SourceError::AssetNotFound)?;

        match self.attempt(provider, external_id, item, target).await {
            Err(SourceError::LinkExpired) => {
                // Spec 7.3: exactly one fresh-URL retry. A second expiry is a
                // broken contract, not a race, and retrying again would just
                // burn the user's time.
                tracing::info!(
                    target: "plaudon::plaud_cli",
                    "the download link had expired; requesting a fresh one"
                );
                let mut report = self.attempt(provider, external_id, item, target).await?;
                report.retried_after_expiry = true;
                Ok(report)
            }
            other => other,
        }
    }

    async fn attempt(
        &self,
        provider: &dyn SignedUrlProvider,
        external_id: &ExternalRecordingId,
        item: &SourceItem,
        target: &Path,
    ) -> Result<AcquisitionReport, SourceError> {
        if let Some(declared) = item.declared_byte_length {
            self.policy
                .authorize_size(declared)
                .map_err(SourceError::from)?;
            if declared > self.limits.max_bytes {
                return Err(SourceError::from(DownloadRejection::BodyTooLarge {
                    limit_bytes: self.limits.max_bytes,
                }));
            }
        }

        let part = part_path(target);
        // A stale `.part` from an interrupted run is removed rather than
        // appended to: without verified range semantics, resuming would mean
        // trusting that the server returns the same bytes from the same offset.
        if part.exists() {
            std::fs::remove_file(&part).map_err(|_| SourceError::DownloadInterrupted)?;
        }

        let url = provider.fresh_url(external_id).await?;
        let fetched = self.fetcher.get(&url).await.map_err(SourceError::from)?;
        drop(url);

        if let Some(declared) = fetched.head.content_length {
            self.policy
                .authorize_size(declared)
                .map_err(SourceError::from)?;
            if declared > self.limits.max_bytes {
                return Err(SourceError::from(DownloadRejection::BodyTooLarge {
                    limit_bytes: self.limits.max_bytes,
                }));
            }
        }
        if let Some(declared) = &fetched.head.content_type {
            // A vendor host answering `text/html` is almost always an error
            // page behind a 200, which is an integrity failure, not audio.
            self.policy
                .authorize_content_type(declared)
                .map_err(SourceError::from)?;
        }

        let mut stream = fetched.stream;
        let outcome = {
            let mut writer = StagingWriter::create(&part, self.limits.max_bytes)?;
            loop {
                match stream.next_chunk().await {
                    Ok(Some(chunk)) => {
                        if let Err(error) = writer.write(&chunk) {
                            drop(writer);
                            let _ = std::fs::remove_file(&part);
                            return Err(error);
                        }
                    }
                    Ok(None) => break,
                    Err(error) => {
                        drop(writer);
                        let _ = std::fs::remove_file(&part);
                        return Err(SourceError::from(error));
                    }
                }
            }
            writer.finish()
        };
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                let _ = std::fs::remove_file(&part);
                return Err(error);
            }
        };

        let verified = self.verify(&part, &outcome, fetched.head.content_length);
        let container = match verified {
            Ok(container) => container,
            Err(error) => {
                // Nothing committed, nothing left behind: a failed verification
                // must not leave a plausible-looking file in staging.
                let _ = std::fs::remove_file(&part);
                return Err(error);
            }
        };

        commit_part(&part, target)?;

        Ok(AcquisitionReport {
            asset: AcquiredAsset {
                identity: MediaIdentity::new(
                    outcome.digest,
                    outcome.byte_length,
                    container.as_str(),
                ),
                quality_class: QualityClass::VendorCloudExport,
                staged_path: target.to_path_buf(),
            },
            container,
            retried_after_expiry: false,
            redirect_hops: fetched.head.redirect_hops,
        })
    }

    /// Verify the staged bytes without trusting the filename.
    fn verify(
        &self,
        part: &Path,
        outcome: &WriteOutcome,
        declared_length: Option<u64>,
    ) -> Result<SniffedContainer, SourceError> {
        if outcome.byte_length == 0 {
            return Err(SourceError::IntegrityFailed);
        }
        if let Some(declared) = declared_length {
            if declared != outcome.byte_length {
                // Fewer bytes than declared is an interrupted transfer; more is
                // a server that cannot be trusted about anything else either.
                return Err(if outcome.byte_length < declared {
                    SourceError::DownloadInterrupted
                } else {
                    SourceError::IntegrityFailed
                });
            }
        }
        let prefix = read_prefix(part, SNIFF_WINDOW_BYTES)?;
        let container = sniff_container(&prefix);
        if container == SniffedContainer::Unknown {
            return Err(SourceError::IntegrityFailed);
        }
        Ok(container)
    }
}

/// Running state of one staged write.
#[derive(Debug)]
struct StagingWriter {
    file: File,
    hasher: Sha256,
    written: u64,
    max_bytes: u64,
}

/// What a completed staged write produced.
#[derive(Debug, Clone, PartialEq, Eq)]
struct WriteOutcome {
    digest: Sha256Digest,
    byte_length: u64,
}

impl StagingWriter {
    fn create(path: &Path, max_bytes: u64) -> Result<Self, SourceError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|_| SourceError::DownloadInterrupted)?;
        }
        let file = File::create(path).map_err(|_| SourceError::DownloadInterrupted)?;
        Ok(Self {
            file,
            hasher: Sha256::new(),
            written: 0,
            max_bytes,
        })
    }

    fn write(&mut self, chunk: &[u8]) -> Result<(), SourceError> {
        let next = self.written.saturating_add(chunk.len() as u64);
        if next > self.max_bytes {
            return Err(SourceError::from(DownloadRejection::BodyTooLarge {
                limit_bytes: self.max_bytes,
            }));
        }
        self.file
            .write_all(chunk)
            .map_err(|_| SourceError::DownloadInterrupted)?;
        self.hasher.update(chunk);
        self.written = next;
        Ok(())
    }

    fn finish(mut self) -> Result<WriteOutcome, SourceError> {
        self.file
            .flush()
            .map_err(|_| SourceError::DownloadInterrupted)?;
        // Durability before the rename, so the rename can never publish a name
        // whose contents are still in the page cache (spec 7.3, 13.3A).
        self.file
            .sync_all()
            .map_err(|_| SourceError::DownloadInterrupted)?;
        let digest = Sha256Digest::from_bytes(self.hasher.finalize().into());
        Ok(WriteOutcome {
            digest,
            byte_length: self.written,
        })
    }
}

/// `<target>.part`, in the same directory as the target.
#[must_use]
pub fn part_path(target: &Path) -> PathBuf {
    let mut name = target.file_name().unwrap_or_default().to_os_string();
    name.push(".");
    name.push(PART_SUFFIX);
    target.with_file_name(name)
}

/// Rename the verified part file onto its target and persist the directory.
///
/// Both paths are in the same directory by construction, so this is a true
/// atomic rename rather than a copy that might straddle volumes. The directory
/// `fsync` is what makes the new name survive a power loss on Unix; on Windows
/// `ReplaceFile`/`MoveFileEx` semantics make the rename itself durable and
/// opening a directory handle is not supported, so the step is skipped there.
fn commit_part(part: &Path, target: &Path) -> Result<(), SourceError> {
    std::fs::rename(part, target).map_err(|_| SourceError::DownloadInterrupted)?;
    #[cfg(unix)]
    {
        if let Some(parent) = target.parent() {
            if let Ok(dir) = File::open(parent) {
                let _ = dir.sync_all();
            }
        }
    }
    Ok(())
}

/// Read at most `limit` bytes from the start of a file.
fn read_prefix(path: &Path, limit: usize) -> Result<Vec<u8>, SourceError> {
    use std::io::Read;
    let mut file = File::open(path).map_err(|_| SourceError::IntegrityFailed)?;
    let mut buffer = vec![0u8; limit];
    let mut filled = 0usize;
    while filled < limit {
        match file.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(_) => return Err(SourceError::IntegrityFailed),
        }
    }
    buffer.truncate(filled);
    Ok(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{ByteStream, FetchedBody, ResponseHead, TransportError};
    use plaudon_domain::source::{SourceAccountScope, SourceIdentityKey, SourceKind};
    use std::sync::Mutex;

    /// A minimal RIFF/WAVE header plus a little PCM, so the sniffer recognizes
    /// the container without the test needing a fixture file.
    fn wav_bytes(payload: usize) -> Vec<u8> {
        let data_len = payload as u32;
        let riff_len = 36 + data_len;
        let mut bytes = Vec::with_capacity(44 + payload);
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&riff_len.to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&16_000u32.to_le_bytes());
        bytes.extend_from_slice(&32_000u32.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());
        bytes.resize(44 + payload, 0);
        bytes
    }

    #[derive(Debug)]
    struct VecStream {
        chunks: Vec<Result<Vec<u8>, TransportError>>,
        index: usize,
    }

    #[async_trait]
    impl ByteStream for VecStream {
        async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, TransportError> {
            if self.index >= self.chunks.len() {
                return Ok(None);
            }
            let chunk = self.chunks[self.index].clone();
            self.index += 1;
            chunk.map(Some)
        }
    }

    #[derive(Debug)]
    struct ScriptedFetcher {
        responses: Mutex<Vec<Result<(ResponseHead, Vec<Result<Vec<u8>, TransportError>>), TransportError>>>,
        urls_seen: Mutex<Vec<String>>,
    }

    impl ScriptedFetcher {
        fn ok(head: ResponseHead, body: Vec<u8>) -> Self {
            Self {
                responses: Mutex::new(vec![Ok((head, vec![Ok(body)]))]),
                urls_seen: Mutex::new(Vec::new()),
            }
        }

        fn scripted(
            responses: Vec<Result<(ResponseHead, Vec<Result<Vec<u8>, TransportError>>), TransportError>>,
        ) -> Self {
            Self {
                responses: Mutex::new(responses),
                urls_seen: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl HttpFetcher for ScriptedFetcher {
        async fn get(&self, url: &str) -> Result<FetchedBody, TransportError> {
            self.urls_seen.lock().expect("lock").push(url.to_owned());
            let mut responses = self.responses.lock().expect("lock");
            if responses.is_empty() {
                return Err(TransportError::Connection);
            }
            let response = responses.remove(0);
            response.map(|(head, chunks)| FetchedBody {
                head,
                stream: Box::new(VecStream { chunks, index: 0 }),
            })
        }
    }

    #[derive(Debug)]
    struct CountingProvider {
        url: String,
        calls: Mutex<usize>,
    }

    #[async_trait]
    impl SignedUrlProvider for CountingProvider {
        async fn fresh_url(
            &self,
            _external_id: &ExternalRecordingId,
        ) -> Result<Zeroizing<String>, SourceError> {
            *self.calls.lock().expect("lock") += 1;
            Ok(Zeroizing::new(self.url.clone()))
        }
    }

    fn provider() -> CountingProvider {
        CountingProvider {
            url: "https://cdn.example-vendor.com/a?sig=x".to_owned(),
            calls: Mutex::new(0),
        }
    }

    fn item(declared: Option<u64>) -> SourceItem {
        SourceItem {
            identity: SourceIdentityKey::new(
                SourceKind::PlaudCloudCli,
                SourceAccountScope::new("account-a"),
                Some(ExternalRecordingId::new("rec_1")),
            ),
            display_name: "Board sync".to_owned(),
            captured_at: None,
            duration_ms: None,
            declared_byte_length: declared,
            remote_version: None,
            quality_class: QualityClass::VendorCloudExport,
        }
    }

    fn head(length: Option<u64>, content_type: Option<&str>) -> ResponseHead {
        ResponseHead {
            content_type: content_type.map(str::to_owned),
            content_length: length,
            ..ResponseHead::default()
        }
    }

    fn pipeline(fetcher: Arc<dyn HttpFetcher>) -> AcquisitionPipeline {
        AcquisitionPipeline::new(
            Arc::new(DownloadPolicy::with_hosts(&["cdn.example-vendor.com"])),
            fetcher,
            UrlContract::undocumented("cdn.example-vendor.com"),
        )
    }

    #[tokio::test]
    async fn a_verified_download_is_committed_atomically() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("staged.bin");
        let body = wav_bytes(128);
        let expected = Sha256Digest::of(&body);
        let fetcher = Arc::new(ScriptedFetcher::ok(
            head(Some(body.len() as u64), Some("audio/wav")),
            body.clone(),
        ));
        let report = pipeline(fetcher)
            .acquire(&provider(), &item(None), &target)
            .await
            .expect("acquired");

        assert_eq!(report.asset.identity.sha256, expected);
        assert_eq!(report.asset.identity.byte_length, body.len() as u64);
        assert_eq!(report.container, SniffedContainer::Wav);
        assert_eq!(report.asset.quality_class, QualityClass::VendorCloudExport);
        assert!(!report.retried_after_expiry);
        assert_eq!(std::fs::read(&target).expect("read"), body);
        assert!(!part_path(&target).exists(), "no part file may survive");
    }

    #[tokio::test]
    async fn an_expired_link_triggers_exactly_one_fresh_url() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("staged.bin");
        let body = wav_bytes(64);
        let fetcher = Arc::new(ScriptedFetcher::scripted(vec![
            Err(TransportError::Status { status: 403 }),
            Ok((head(None, Some("audio/wav")), vec![Ok(body.clone())])),
        ]));
        let provider = provider();
        let report = pipeline(fetcher)
            .acquire(&provider, &item(None), &target)
            .await
            .expect("acquired");
        assert!(report.retried_after_expiry);
        assert_eq!(*provider.calls.lock().expect("lock"), 2);
    }

    #[tokio::test]
    async fn a_second_expiry_is_reported_rather_than_retried_forever() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("staged.bin");
        let fetcher = Arc::new(ScriptedFetcher::scripted(vec![
            Err(TransportError::Status { status: 403 }),
            Err(TransportError::Status { status: 403 }),
            Ok((head(None, Some("audio/wav")), vec![Ok(wav_bytes(16))])),
        ]));
        let provider = provider();
        let error = pipeline(fetcher)
            .acquire(&provider, &item(None), &target)
            .await
            .unwrap_err();
        assert_eq!(error, SourceError::LinkExpired);
        assert_eq!(*provider.calls.lock().expect("lock"), 2);
    }

    #[tokio::test]
    async fn a_misleading_extension_does_not_make_bytes_audio() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("totally-real.wav");
        let fetcher = Arc::new(ScriptedFetcher::ok(
            head(None, Some("audio/wav")),
            b"<html><body>expired</body></html>".to_vec(),
        ));
        let error = pipeline(fetcher)
            .acquire(&provider(), &item(None), &target)
            .await
            .unwrap_err();
        assert_eq!(error, SourceError::IntegrityFailed);
        assert!(!target.exists(), "nothing may be committed");
        assert!(!part_path(&target).exists());
    }

    #[tokio::test]
    async fn a_non_audio_content_type_is_refused_before_the_body_is_read() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("staged.bin");
        let fetcher = Arc::new(ScriptedFetcher::ok(
            head(None, Some("text/html")),
            wav_bytes(16),
        ));
        let error = pipeline(fetcher)
            .acquire(&provider(), &item(None), &target)
            .await
            .unwrap_err();
        assert_eq!(error, SourceError::IntegrityFailed);
        assert!(!part_path(&target).exists());
    }

    #[tokio::test]
    async fn an_oversized_body_fails_without_filling_the_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("staged.bin");
        let fetcher = Arc::new(ScriptedFetcher::ok(head(None, None), wav_bytes(4096)));
        let error = pipeline(fetcher)
            .with_limits(AcquireLimits {
                max_bytes: 1024,
                headroom_bytes: 0,
            })
            .acquire(&provider(), &item(None), &target)
            .await
            .unwrap_err();
        assert_eq!(error, SourceError::DownloadInterrupted);
        assert!(!target.exists());
        assert!(!part_path(&target).exists());
    }

    #[tokio::test]
    async fn a_declared_length_over_the_cap_is_refused_before_any_request() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("staged.bin");
        let fetcher = Arc::new(ScriptedFetcher::ok(head(None, None), wav_bytes(16)));
        let provider = provider();
        let error = pipeline(fetcher)
            .with_limits(AcquireLimits {
                max_bytes: 1024,
                headroom_bytes: 0,
            })
            .acquire(&provider, &item(Some(1_000_000)), &target)
            .await
            .unwrap_err();
        assert_eq!(error, SourceError::DownloadInterrupted);
        assert_eq!(
            *provider.calls.lock().expect("lock"),
            0,
            "no URL may be requested for a file we will refuse"
        );
    }

    #[tokio::test]
    async fn a_short_body_against_a_declared_length_is_an_interrupted_transfer() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("staged.bin");
        let body = wav_bytes(64);
        let fetcher = Arc::new(ScriptedFetcher::ok(
            head(Some(body.len() as u64 + 10), Some("audio/wav")),
            body,
        ));
        let error = pipeline(fetcher)
            .acquire(&provider(), &item(None), &target)
            .await
            .unwrap_err();
        assert_eq!(error, SourceError::DownloadInterrupted);
        assert!(!target.exists());
    }

    #[tokio::test]
    async fn an_interrupted_stream_leaves_no_part_file_behind() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("staged.bin");
        let fetcher = Arc::new(ScriptedFetcher::scripted(vec![Ok((
            head(None, Some("audio/wav")),
            vec![Ok(wav_bytes(32)), Err(TransportError::Connection)],
        ))]));
        let error = pipeline(fetcher)
            .acquire(&provider(), &item(None), &target)
            .await
            .unwrap_err();
        assert_eq!(error, SourceError::DownloadInterrupted);
        assert!(!target.exists());
        assert!(!part_path(&target).exists());
    }

    #[tokio::test]
    async fn an_empty_body_is_an_integrity_failure() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("staged.bin");
        let fetcher = Arc::new(ScriptedFetcher::ok(head(None, Some("audio/wav")), Vec::new()));
        let error = pipeline(fetcher)
            .acquire(&provider(), &item(None), &target)
            .await
            .unwrap_err();
        assert_eq!(error, SourceError::IntegrityFailed);
    }

    #[tokio::test]
    async fn a_stale_part_file_is_discarded_rather_than_resumed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("staged.bin");
        std::fs::write(part_path(&target), b"stale bytes from a previous run").expect("write");
        let body = wav_bytes(32);
        let fetcher = Arc::new(ScriptedFetcher::ok(
            head(None, Some("audio/wav")),
            body.clone(),
        ));
        let report = pipeline(fetcher)
            .acquire(&provider(), &item(None), &target)
            .await
            .expect("acquired");
        assert_eq!(report.asset.identity.byte_length, body.len() as u64);
        assert_eq!(std::fs::read(&target).expect("read"), body);
    }

    #[tokio::test]
    async fn resume_is_never_enabled_by_an_unobserved_contract() {
        let fetcher = Arc::new(ScriptedFetcher::ok(head(None, None), wav_bytes(16)));
        assert!(!pipeline(fetcher).may_resume());
    }

    #[tokio::test]
    async fn an_item_without_an_external_id_cannot_be_acquired() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("staged.bin");
        let fetcher = Arc::new(ScriptedFetcher::ok(head(None, None), wav_bytes(16)));
        let mut item = item(None);
        item.identity.external_id = None;
        let error = pipeline(fetcher)
            .acquire(&provider(), &item, &target)
            .await
            .unwrap_err();
        assert_eq!(error, SourceError::AssetNotFound);
    }

    #[test]
    fn the_part_path_stays_in_the_target_directory() {
        let target = Path::new("/staging/abc.bin");
        assert_eq!(part_path(target), Path::new("/staging/abc.bin.part"));
        assert_eq!(part_path(target).parent(), target.parent());
    }
}
