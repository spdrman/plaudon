//! On-disk peak cache.
//!
//! Rebuilding a three-hour pyramid costs a full decode; doing it every time
//! the user reopens a recording is the difference between an instant waveform
//! and a progress bar. The cache is keyed on the *content* — the media digest
//! `plaudon_audio` already computes during validation — together with the
//! decoder version and the fold parameters, so a re-encoded file, a changed
//! decode path or a different bucket size can never be served stale data.
//! Nothing is trusted on read: the key is re-checked against the header and
//! the body against a trailing digest, and a file that fails either is deleted
//! rather than repaired.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::peaks::{
    PeakBucket, PeakLevel, PeakParams, PeakPyramid, DECODER_VERSION, PEAK_FORMAT_VERSION,
};

const MAGIC: &[u8; 8] = b"PLDNPEAK";
const HEADER_BYTES: usize = 8 + 4 + 4 + 13 + 32 + 8 + 4;
const DIGEST_BYTES: usize = 32;
const EXTENSION: &str = "peaks";

/// Why a cached pyramid could not be used.
#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    /// The file does not start with the peak-cache magic.
    #[error("not a peak cache file")]
    NotPeakData,
    /// The file was written by a different layout, decoder or parameter set.
    #[error("peak cache entry is stale: {0}")]
    Stale(&'static str),
    /// The file is truncated or its trailing digest does not match.
    #[error("peak cache entry is corrupt")]
    Corrupt,
    /// The filesystem refused the operation.
    #[error("peak cache io at {path}: {source}")]
    Io {
        /// Path involved.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: io::Error,
    },
}

impl CacheError {
    fn io(path: &Path, source: io::Error) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}

/// Everything a cached pyramid is keyed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PeakCacheKey {
    /// SHA-256 of the source media bytes.
    pub content_hash: [u8; 32],
    /// Version of the decode path that produced the samples.
    pub decoder_version: u32,
    /// Fold parameters.
    pub params: PeakParams,
}

impl PeakCacheKey {
    /// A key for `content_hash` at the current decoder version.
    #[must_use]
    pub fn new(content_hash: [u8; 32], params: PeakParams) -> Self {
        Self {
            content_hash,
            decoder_version: DECODER_VERSION,
            params,
        }
    }

    /// Stable file name: the hash of every keyed field, so a parameter change
    /// lands on a different file instead of racing the old one.
    #[must_use]
    pub fn file_name(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(MAGIC);
        hasher.update(PEAK_FORMAT_VERSION.to_le_bytes());
        hasher.update(self.content_hash);
        hasher.update(self.decoder_version.to_le_bytes());
        hasher.update(self.params.key_bytes());
        let digest: [u8; 32] = hasher.finalize().into();
        let mut name = String::with_capacity(64 + 1 + EXTENSION.len());
        for byte in digest {
            name.push(nibble(byte >> 4));
            name.push(nibble(byte & 0x0f));
        }
        name.push('.');
        name.push_str(EXTENSION);
        name
    }
}

fn nibble(value: u8) -> char {
    char::from_digit(u32::from(value), 16).unwrap_or('0')
}

/// Serialise a pyramid together with the key it must be re-checked against.
#[must_use]
pub fn encode(key: &PeakCacheKey, pyramid: &PeakPyramid) -> Vec<u8> {
    let total_buckets: usize = pyramid
        .levels()
        .iter()
        .map(|level| level.buckets().len())
        .sum();
    let mut out = Vec::with_capacity(
        HEADER_BYTES + pyramid.levels().len() * 16 + total_buckets * 4 + DIGEST_BYTES,
    );
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&PEAK_FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&key.decoder_version.to_le_bytes());
    out.extend_from_slice(&key.params.key_bytes());
    out.extend_from_slice(&key.content_hash);
    out.extend_from_slice(&pyramid.total_frames().to_le_bytes());
    out.extend_from_slice(&(pyramid.levels().len() as u32).to_le_bytes());
    for level in pyramid.levels() {
        out.extend_from_slice(&level.bucket_frames().to_le_bytes());
        out.extend_from_slice(&(level.buckets().len() as u64).to_le_bytes());
    }
    for level in pyramid.levels() {
        for bucket in level.buckets() {
            out.extend_from_slice(&bucket.min.to_le_bytes());
            out.extend_from_slice(&bucket.max.to_le_bytes());
        }
    }
    let digest: [u8; 32] = Sha256::digest(&out).into();
    out.extend_from_slice(&digest);
    out
}

/// Parse a cache entry, rejecting anything that does not match `key`.
pub fn decode(key: &PeakCacheKey, bytes: &[u8]) -> Result<PeakPyramid, CacheError> {
    if bytes.len() < 8 || &bytes[..8] != MAGIC {
        return Err(CacheError::NotPeakData);
    }
    if bytes.len() < HEADER_BYTES + DIGEST_BYTES {
        return Err(CacheError::Corrupt);
    }
    let body_len = bytes.len() - DIGEST_BYTES;
    let expected: [u8; 32] = Sha256::digest(&bytes[..body_len]).into();
    if expected != bytes[body_len..] {
        return Err(CacheError::Corrupt);
    }

    let mut cursor = Cursor::new(&bytes[..body_len]);
    cursor.skip(8)?;
    if cursor.u32()? != PEAK_FORMAT_VERSION {
        return Err(CacheError::Stale("format version"));
    }
    if cursor.u32()? != key.decoder_version {
        return Err(CacheError::Stale("decoder version"));
    }
    if cursor.take::<13>()? != key.params.key_bytes() {
        return Err(CacheError::Stale("peak parameters"));
    }
    if cursor.take::<32>()? != key.content_hash {
        return Err(CacheError::Stale("content hash"));
    }
    let total_frames = cursor.u64()?;
    let level_count = cursor.u32()? as usize;
    if level_count == 0 || level_count > u8::MAX as usize {
        return Err(CacheError::Corrupt);
    }
    let mut table = Vec::with_capacity(level_count);
    for _ in 0..level_count {
        let bucket_frames = cursor.u64()?;
        let count = cursor.u64()?;
        // A level cannot plausibly hold a billion buckets (4 GiB); refusing
        // here keeps the `count * 4` below from overflowing.
        if bucket_frames == 0 || count > 1_000_000_000 {
            return Err(CacheError::Corrupt);
        }
        table.push((bucket_frames, count as usize));
    }
    let mut levels = Vec::with_capacity(level_count);
    for (bucket_frames, count) in table {
        let raw = cursor.take_slice(count * 4)?;
        let mut buckets = Vec::with_capacity(count);
        for chunk in raw.as_chunks::<4>().0 {
            buckets.push(PeakBucket {
                min: i16::from_le_bytes([chunk[0], chunk[1]]),
                max: i16::from_le_bytes([chunk[2], chunk[3]]),
            });
        }
        levels.push(PeakLevel::from_parts(bucket_frames, buckets));
    }
    if !cursor.is_empty() {
        return Err(CacheError::Corrupt);
    }
    Ok(PeakPyramid::from_levels(key.params, total_frames, levels))
}

struct Cursor<'a> {
    bytes: &'a [u8],
}

impl<'a> Cursor<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }

    const fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    fn take_slice(&mut self, n: usize) -> Result<&'a [u8], CacheError> {
        if self.bytes.len() < n {
            return Err(CacheError::Corrupt);
        }
        let (head, tail) = self.bytes.split_at(n);
        self.bytes = tail;
        Ok(head)
    }

    fn skip(&mut self, n: usize) -> Result<(), CacheError> {
        self.take_slice(n).map(|_| ())
    }

    fn take<const N: usize>(&mut self) -> Result<[u8; N], CacheError> {
        let slice = self.take_slice(N)?;
        let mut out = [0u8; N];
        out.copy_from_slice(slice);
        Ok(out)
    }

    fn u32(&mut self) -> Result<u32, CacheError> {
        Ok(u32::from_le_bytes(self.take::<4>()?))
    }

    fn u64(&mut self) -> Result<u64, CacheError> {
        Ok(u64::from_le_bytes(self.take::<8>()?))
    }
}

/// A directory of cached pyramids.
#[derive(Debug, Clone)]
pub struct PeakCache {
    root: PathBuf,
}

impl PeakCache {
    /// Open (creating) a cache rooted at `root`.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, CacheError> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(|source| CacheError::io(&root, source))?;
        Ok(Self { root })
    }

    /// Directory holding the entries.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Full path of an entry.
    #[must_use]
    pub fn path_for(&self, key: &PeakCacheKey) -> PathBuf {
        self.root.join(key.file_name())
    }

    /// Load a pyramid, or `None` on a miss.
    ///
    /// A stale or corrupt entry is removed on the way out: leaving it costs
    /// disk forever and would be re-read on every open.
    #[must_use]
    pub fn load(&self, key: &PeakCacheKey) -> Option<PeakPyramid> {
        let path = self.path_for(key);
        let bytes = fs::read(&path).ok()?;
        match decode(key, &bytes) {
            Ok(pyramid) => Some(pyramid),
            Err(error) => {
                tracing::debug!(
                    path = %path.display(),
                    %error,
                    "discarding unusable peak cache entry"
                );
                let _ = fs::remove_file(&path);
                None
            }
        }
    }

    /// Write a pyramid. The entry appears atomically: readers see either the
    /// previous file or the complete new one, never a half-written pyramid.
    pub fn store(&self, key: &PeakCacheKey, pyramid: &PeakPyramid) -> Result<(), CacheError> {
        let bytes = encode(key, pyramid);
        let final_path = self.path_for(key);
        let temp_path = final_path.with_extension("peaks.part");
        fs::write(&temp_path, &bytes).map_err(|source| CacheError::io(&temp_path, source))?;
        match fs::rename(&temp_path, &final_path) {
            Ok(()) => Ok(()),
            Err(source) => {
                let _ = fs::remove_file(&temp_path);
                Err(CacheError::io(&final_path, source))
            }
        }
    }

    /// Remove one entry.
    pub fn invalidate(&self, key: &PeakCacheKey) {
        let _ = fs::remove_file(self.path_for(key));
    }

    /// Total bytes held by cache entries.
    #[must_use]
    pub fn total_bytes(&self) -> u64 {
        self.entries().iter().map(|entry| entry.1).sum()
    }

    /// Delete oldest-first until the cache fits `max_bytes`. Returns the bytes
    /// reclaimed.
    pub fn prune(&self, max_bytes: u64) -> u64 {
        let mut entries = self.entries();
        let mut total: u64 = entries.iter().map(|entry| entry.1).sum();
        if total <= max_bytes {
            return 0;
        }
        entries.sort_by_key(|entry| entry.2);
        let mut freed = 0u64;
        for (path, size, _) in entries {
            if total <= max_bytes {
                break;
            }
            if fs::remove_file(&path).is_ok() {
                total = total.saturating_sub(size);
                freed += size;
            }
        }
        freed
    }

    /// Remove every entry.
    pub fn clear(&self) {
        for (path, _, _) in self.entries() {
            let _ = fs::remove_file(path);
        }
    }

    fn entries(&self) -> Vec<(PathBuf, u64, std::time::SystemTime)> {
        let Ok(dir) = fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for entry in dir.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some(EXTENSION) {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            let modified = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
            out.push((path, meta.len(), modified));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peaks::PeakBuilder;

    fn pyramid() -> PeakPyramid {
        let mut builder = PeakBuilder::new(PeakParams::default());
        let samples: Vec<f32> = (0..40_000)
            .map(|i| ((i % 311) as f32 / 155.0) - 1.0)
            .collect();
        builder.push_samples(&samples);
        builder.finish()
    }

    fn key() -> PeakCacheKey {
        PeakCacheKey::new([7u8; 32], PeakParams::default())
    }

    #[test]
    fn a_pyramid_survives_a_round_trip() {
        let original = pyramid();
        let bytes = encode(&key(), &original);
        assert_eq!(decode(&key(), &bytes).expect("decode"), original);
    }

    #[test]
    fn a_different_content_hash_is_a_stale_entry() {
        let bytes = encode(&key(), &pyramid());
        let other = PeakCacheKey::new([8u8; 32], PeakParams::default());
        assert!(matches!(
            decode(&other, &bytes),
            Err(CacheError::Stale("content hash"))
        ));
    }

    #[test]
    fn a_different_decoder_version_is_a_stale_entry() {
        let bytes = encode(&key(), &pyramid());
        let mut other = key();
        other.decoder_version += 1;
        assert!(matches!(
            decode(&other, &bytes),
            Err(CacheError::Stale("decoder version"))
        ));
    }

    #[test]
    fn different_parameters_land_on_a_different_file_and_are_rejected() {
        let other_params = PeakParams {
            base_bucket_frames: 128,
            ..PeakParams::default()
        };
        let other = PeakCacheKey::new([7u8; 32], other_params);
        assert_ne!(key().file_name(), other.file_name());
        let bytes = encode(&key(), &pyramid());
        assert!(matches!(
            decode(&other, &bytes),
            Err(CacheError::Stale("peak parameters"))
        ));
    }

    #[test]
    fn truncation_and_bit_flips_are_corruption_not_data() {
        let bytes = encode(&key(), &pyramid());
        assert!(matches!(
            decode(&key(), &bytes[..bytes.len() - 1]),
            Err(CacheError::Corrupt)
        ));
        let mut flipped = bytes.clone();
        let index = flipped.len() / 2;
        flipped[index] ^= 0xff;
        assert!(matches!(decode(&key(), &flipped), Err(CacheError::Corrupt)));
        assert!(matches!(
            decode(
                &key(),
                b"not a cache file at all, really truly not one of ours...."
            ),
            Err(CacheError::NotPeakData)
        ));
    }

    #[test]
    fn store_then_load_hits_and_a_corrupt_entry_is_deleted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = PeakCache::open(dir.path()).expect("open");
        let original = pyramid();
        cache.store(&key(), &original).expect("store");
        assert_eq!(cache.load(&key()), Some(original));

        fs::write(cache.path_for(&key()), b"rubbish").expect("overwrite");
        assert_eq!(cache.load(&key()), None);
        assert!(!cache.path_for(&key()).exists());
        // No partial file is left behind by a successful store.
        assert!(!cache.path_for(&key()).with_extension("peaks.part").exists());
    }

    #[test]
    fn pruning_removes_oldest_entries_until_the_budget_is_met() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = PeakCache::open(dir.path()).expect("open");
        let built = pyramid();
        for seed in 0u8..5 {
            let key = PeakCacheKey::new([seed; 32], PeakParams::default());
            cache.store(&key, &built).expect("store");
            // Distinct mtimes, so the prune order is defined.
            std::thread::sleep(std::time::Duration::from_millis(12));
        }
        let total = cache.total_bytes();
        assert!(total > 0);
        let freed = cache.prune(total / 2);
        assert!(freed > 0);
        assert!(cache.total_bytes() <= total / 2);
        // The newest entry survives.
        let newest = PeakCacheKey::new([4u8; 32], PeakParams::default());
        assert!(cache.load(&newest).is_some());

        cache.clear();
        assert_eq!(cache.total_bytes(), 0);
    }
}
