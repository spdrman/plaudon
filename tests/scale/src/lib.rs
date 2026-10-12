//! Performance and scale harness for the spec 17.5 engineering budgets.
//!
//! Spec 17.5 asks for "archive/search performance tests with 10,000 recordings
//! and at least 100,000 transcript segments" and for "bounded memory growth
//! proportional to active chunk window rather than full decoded PCM". Spec
//! 18.6A adds the UI cluster row whose hostile case is a 100k-segment
//! transcript, whose required observable outcome is a correct accessible state
//! and no fabricated timeline — which, at this layer, means a windowed
//! transcript fetch that stays fast and returns exactly the requested slice.
//!
//! Three rules shape this crate:
//!
//! * **The dataset goes through the real schema.** Everything is written with
//!   [`plaudon_persistence`] migrations, repositories and the content-addressed
//!   [`plaudon_persistence::archive::MediaArchive`]. There is no mock table, so
//!   a measurement here is a measurement of the shipping storage layer.
//! * **The dataset is reproducible.** [`SyntheticLibrary`] derives every row
//!   from a seed with the small PRNG in this module, so two generations of the
//!   same seed are byte-identical ([`SyntheticLibrary::fingerprint`]) and a
//!   recorded result can be re-measured later.
//! * **Thresholds are data, not code.** Budgets live in `thresholds.toml`
//!   beside this crate (see [`thresholds`]); changing what counts as a
//!   regression means editing that file in a reviewable commit.
//!
//! Spec 18.9 is satisfied by assertion, not by coverage: every benchmark here
//! fails the test run when it exceeds its recorded budget.

pub mod load;
pub mod metrics;
pub mod pcm;
pub mod results;
pub mod rss;
pub mod thresholds;

use std::fmt;

use plaudon_domain::ids::{MediaAssetId, RecordingId, SpeakerId, TranscriptRevisionId};
use plaudon_domain::locale::LanguageTag;
use plaudon_domain::media::{MediaIdentity, QualityClass, Sha256Digest};
use plaudon_domain::source::SourceKind;
use plaudon_domain::time::Timestamp;
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Cargo package name of this crate, used by the workspace layout conformance test.
pub const PACKAGE_NAME: &str = "plaudon-scale-tests";

/// Seed used when `PLAUDON_SCALE_SEED` is unset.
pub const DEFAULT_SEED: u64 = 0x5CA1_ED05_EED0_1D04;

/// Environment variable selecting the dataset tier.
pub const TIER_ENV: &str = "PLAUDON_SCALE_TIER";

/// Environment variable overriding the dataset seed.
pub const SEED_ENV: &str = "PLAUDON_SCALE_SEED";

/// Rare terms injected into segment text so cross-recording search has needles
/// with a known, generator-computable match count.
pub const SEARCH_NEEDLES: &[&str] = &["amortization", "postmortem", "quarterly"];

/// One in this many segments receives a needle term.
const NEEDLE_PERIOD: u64 = 97;

/// Sample rate assumed by the synthetic durations and the PCM harness.
pub const SAMPLE_RATE_HZ: u32 = 16_000;

// ---------------------------------------------------------------------------
// PRNG
// ---------------------------------------------------------------------------

/// `splitmix64`, used only to expand a user seed into PRNG state.
const fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// A deterministic `xoshiro256**` generator.
///
/// This exists instead of a dependency because the only requirement is
/// reproducibility: no cryptographic property is claimed or needed, and a
/// dataset generator must never be able to pull entropy from the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rng {
    state: [u64; 4],
}

impl Rng {
    /// Expand a seed into generator state.
    #[must_use]
    pub fn seeded(seed: u64) -> Self {
        let mut expand = seed;
        let mut state = [0u64; 4];
        for slot in &mut state {
            *slot = splitmix64(&mut expand);
        }
        // All-zero state is the one degenerate case for xoshiro.
        if state == [0; 4] {
            state = [1, 2, 3, 4];
        }
        Self { state }
    }

    /// Next 64 random bits.
    pub fn next_u64(&mut self) -> u64 {
        let result = self.state[1]
            .wrapping_mul(5)
            .rotate_left(7)
            .wrapping_mul(9);
        let t = self.state[1] << 17;
        self.state[2] ^= self.state[0];
        self.state[3] ^= self.state[1];
        self.state[1] ^= self.state[2];
        self.state[0] ^= self.state[3];
        self.state[2] ^= t;
        self.state[3] = self.state[3].rotate_left(45);
        result
    }

    /// A value in `0..bound`, using Lemire's multiply-shift reduction.
    ///
    /// The modulo bias is below 2^-64 per draw, which is irrelevant for
    /// synthetic metadata and keeps the generator branch-free.
    pub fn below(&mut self, bound: u64) -> u64 {
        if bound == 0 {
            return 0;
        }
        let product = u128::from(self.next_u64()) * u128::from(bound);
        // The high 64 bits of a 128-bit product are by construction < `bound`.
        #[allow(clippy::cast_possible_truncation)]
        {
            (product >> 64) as u64
        }
    }

    /// A value in `low..=high`.
    pub fn range(&mut self, low: u64, high: u64) -> u64 {
        debug_assert!(low <= high, "range bounds are inverted");
        low + self.below(high - low + 1)
    }

    /// One element of a non-empty slice.
    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        assert!(!items.is_empty(), "cannot pick from an empty slice");
        let index = usize::try_from(self.below(items.len() as u64)).unwrap_or(0);
        &items[index]
    }

    /// Fill a buffer with generated bytes.
    pub fn fill_bytes(&mut self, buffer: &mut [u8]) {
        for chunk in buffer.chunks_mut(8) {
            let word = self.next_u64().to_le_bytes();
            chunk.copy_from_slice(&word[..chunk.len()]);
        }
    }

    /// A deterministic UUID with correct version/variant bits.
    pub fn uuid(&mut self) -> Uuid {
        let mut bytes = [0u8; 16];
        self.fill_bytes(&mut bytes);
        uuid::Builder::from_random_bytes(bytes).into_uuid()
    }
}

// ---------------------------------------------------------------------------
// Dataset shape
// ---------------------------------------------------------------------------

/// Which dataset size a run uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// Small enough that `cargo test -p plaudon-scale-tests` is a normal CI step.
    Ci,
    /// The spec 17.5 size: 10,000 recordings and more than 100,000 segments.
    Full,
}

impl Tier {
    /// Canonical name, used in `thresholds.toml` section keys and in results.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ci => "ci",
            Self::Full => "full",
        }
    }

    /// Parse a tier name, rejecting anything unknown rather than guessing.
    pub fn parse(raw: &str) -> Result<Self, UnknownTier> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "ci" => Ok(Self::Ci),
            "full" => Ok(Self::Full),
            _ => Err(UnknownTier(raw.to_owned())),
        }
    }
}

impl fmt::Display for Tier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A tier name this build does not know.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownTier(pub String);

impl fmt::Display for UnknownTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown scale tier `{}`: expected `ci` or `full`", self.0)
    }
}

impl std::error::Error for UnknownTier {}

/// The exact size and seed of one dataset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DatasetSpec {
    /// Which tier this spec came from.
    pub tier: Tier,
    /// Seed every row is derived from.
    pub seed: u64,
    /// Number of ordinary library recordings.
    pub recordings: u32,
    /// Segments in each ordinary recording's transcript revision.
    pub segments_per_recording: u32,
    /// Segments in the single marathon transcript (spec 18.6A UI row).
    pub marathon_segments: u32,
    /// Byte length of each synthetic media blob written into the archive.
    pub blob_bytes: usize,
}

impl DatasetSpec {
    /// The CI tier: large enough to exercise the same code paths and index
    /// choices, small enough to run on every push.
    #[must_use]
    pub const fn ci(seed: u64) -> Self {
        Self {
            tier: Tier::Ci,
            seed,
            recordings: 500,
            segments_per_recording: 6,
            marathon_segments: 5_000,
            blob_bytes: 512,
        }
    }

    /// The spec 17.5 tier: 10,000 recordings, 180,000 segments.
    #[must_use]
    pub const fn full(seed: u64) -> Self {
        Self {
            tier: Tier::Full,
            seed,
            recordings: 10_000,
            segments_per_recording: 8,
            marathon_segments: 100_000,
            blob_bytes: 512,
        }
    }

    /// Build a spec from `PLAUDON_SCALE_TIER` and `PLAUDON_SCALE_SEED`.
    ///
    /// An unparsable value is a hard error: silently running a smaller dataset
    /// than the operator asked for would make the recorded result a lie.
    pub fn from_env() -> Result<Self, UnknownTier> {
        let tier = match std::env::var(TIER_ENV) {
            Ok(raw) if !raw.trim().is_empty() => Tier::parse(&raw)?,
            _ => Tier::Ci,
        };
        let seed = match std::env::var(SEED_ENV) {
            Ok(raw) if !raw.trim().is_empty() => raw
                .trim()
                .parse::<u64>()
                .map_err(|_| UnknownTier(format!("{SEED_ENV}={raw}")))?,
            _ => DEFAULT_SEED,
        };
        Ok(match tier {
            Tier::Ci => Self::ci(seed),
            Tier::Full => Self::full(seed),
        })
    }

    /// Recordings including the marathon one.
    #[must_use]
    pub const fn total_recordings(&self) -> u64 {
        self.recordings as u64 + 1
    }

    /// Segments across the whole library.
    #[must_use]
    pub const fn total_segments(&self) -> u64 {
        self.recordings as u64 * self.segments_per_recording as u64 + self.marathon_segments as u64
    }

    /// Index of the marathon recording in the generator's index space.
    #[must_use]
    pub const fn marathon_index(&self) -> u32 {
        self.recordings
    }
}

// ---------------------------------------------------------------------------
// Generated rows
// ---------------------------------------------------------------------------

/// A generated media blob: identity plus the exact bytes it is derived from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntheticMedia {
    /// Row identity for `media_assets`.
    pub id: MediaAssetId,
    /// Content identity of [`SyntheticMedia::bytes`].
    pub identity: MediaIdentity,
    /// Conventional extension for the archive path.
    pub extension: &'static str,
    /// Fidelity class recorded for this candidate.
    pub quality_class: QualityClass,
    /// The bytes themselves, so the archive stores real content.
    pub bytes: Vec<u8>,
}

/// A generated transcript segment.
#[derive(Debug, Clone, PartialEq)]
pub struct SyntheticSegment {
    /// Row identity for `transcript_segments`, derived from the seed.
    pub id: Uuid,
    /// Zero-based position in the revision.
    pub seq_index: u32,
    /// Start offset in milliseconds.
    pub start_ms: u64,
    /// End offset in milliseconds.
    pub end_ms: u64,
    /// Segment text.
    pub text: String,
    /// Confidence in the `0.0..=1.0` engine-scoped range.
    pub confidence: f64,
    /// Index into the recording's speaker list, when the recording has speakers.
    pub speaker_slot: Option<u8>,
}

/// A generated recording with its metadata and derived identities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntheticRecording {
    /// Generator index.
    pub index: u32,
    /// Durable recording identity.
    pub id: RecordingId,
    /// Transcript revision identity.
    pub revision_id: TranscriptRevisionId,
    /// User-visible title.
    pub title: String,
    /// Capture instant.
    pub captured_at: Timestamp,
    /// Row creation instant, always at or after capture.
    pub created_at: Timestamp,
    /// Duration in milliseconds.
    pub duration_ms: u64,
    /// Transcript locale.
    pub locale: LanguageTag,
    /// Which source produced it.
    pub source_kind: SourceKind,
    /// Vendor identifier within that source's account scope.
    pub external_id: String,
    /// Account or device scope of the source.
    pub account_scope: String,
    /// Sorted, de-duplicated tags.
    pub tags: Vec<String>,
    /// Transcription engine name recorded on the revision.
    pub engine: &'static str,
    /// Model identifier recorded on the revision.
    pub model_id: &'static str,
    /// Speaker identities, empty when the recording was not diarized.
    pub speakers: Vec<(SpeakerId, String)>,
    /// Media blob attached to the recording.
    pub media: SyntheticMedia,
    /// Number of segments in the transcript revision.
    pub segment_count: u32,
}

const TITLE_SUBJECTS: &[&str] = &[
    "Platform sync",
    "Design review",
    "Customer call",
    "Field notes",
    "Standup",
    "Retrospective",
    "Interview",
    "Budget review",
    "Incident review",
    "Lecture",
    "Site walkthrough",
    "One-on-one",
];

const TITLE_QUALIFIERS: &[&str] = &[
    "storage",
    "mobile",
    "billing",
    "onboarding",
    "acoustics",
    "hardware",
    "localization",
    "procurement",
    "accessibility",
    "security",
];

const LOCALES: &[&str] = &[
    "en-US", "en-GB", "de-DE", "fr-FR", "es-ES", "pt-BR", "ja-JP", "nl-NL",
];

const SOURCE_KINDS: &[SourceKind] = &[
    SourceKind::PlaudDeviceBle,
    SourceKind::PlaudDeviceUsb,
    SourceKind::PlaudCloudCli,
    SourceKind::LocalFile,
    SourceKind::WatchedFolder,
];

const QUALITY_CLASSES: &[QualityClass] = &[
    QualityClass::OriginalDeviceRecording,
    QualityClass::CompressedDeviceTransfer,
    QualityClass::VendorCloudExport,
    QualityClass::UserSuppliedFile,
];

const EXTENSIONS: &[&str] = &["wav", "mp3", "m4a", "opus", "asr"];

const TAGS: &[&str] = &[
    "work",
    "personal",
    "research",
    "follow-up",
    "archive",
    "legal",
    "draft",
    "travel",
];

const SPEAKER_NAMES: &[&str] = &[
    "Speaker 1",
    "Speaker 2",
    "Speaker 3",
    "Speaker 4",
    "Speaker 5",
];

const WORDS: &[&str] = &[
    "the", "we", "should", "probably", "move", "that", "deadline", "because", "the", "vendor",
    "still", "has", "not", "confirmed", "delivery", "and", "I", "do", "not", "want", "another",
    "surprise", "before", "the", "review", "meeting", "next", "week", "about", "storage",
    "capacity", "latency", "budget", "transcript", "accuracy", "battery", "firmware", "consent",
    "export", "retention",
];

const FIRST_SEGMENT_OPENERS: &[&str] = &[
    "Let's get started.",
    "Thanks for joining.",
    "Recording now.",
    "Picking up where we left off.",
];

/// Epoch of the synthetic library: 2023-01-01T00:00:00Z.
const LIBRARY_EPOCH_MS: i64 = 1_672_531_200_000;

/// Span the capture instants are spread across: about three years.
const LIBRARY_SPAN_MS: u64 = 94_608_000_000;

/// The deterministic dataset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntheticLibrary {
    spec: DatasetSpec,
}

impl SyntheticLibrary {
    /// Build a generator for a spec.
    #[must_use]
    pub const fn new(spec: DatasetSpec) -> Self {
        Self { spec }
    }

    /// The spec this generator produces.
    #[must_use]
    pub const fn spec(&self) -> &DatasetSpec {
        &self.spec
    }

    /// Per-row generator stream.
    ///
    /// Each row gets its own stream keyed by `(seed, index, purpose)`, so a
    /// row can be regenerated in isolation and changing one row's size never
    /// shifts another row's values.
    fn stream(&self, index: u32, purpose: u64) -> Rng {
        let mut mixer = self
            .spec
            .seed
            .wrapping_add(u64::from(index).wrapping_mul(0x1000_0000_1B3))
            .wrapping_add(purpose.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        Rng::seeded(splitmix64(&mut mixer))
    }

    /// Generate the metadata of recording `index`.
    ///
    /// `index == spec.marathon_index()` is the marathon recording whose
    /// transcript carries [`DatasetSpec::marathon_segments`] segments.
    #[must_use]
    pub fn recording(&self, index: u32) -> SyntheticRecording {
        let marathon = index == self.spec.marathon_index();
        let mut rng = self.stream(index, 1);

        let subject = *rng.pick(TITLE_SUBJECTS);
        let qualifier = *rng.pick(TITLE_QUALIFIERS);
        let title = if marathon {
            format!("All-day workshop — {qualifier} (archive stress case)")
        } else {
            format!("{subject} — {qualifier} #{index:05}")
        };

        let captured_offset = rng.below(LIBRARY_SPAN_MS);
        let captured_at = Timestamp::from_millis(
            LIBRARY_EPOCH_MS + i64::try_from(captured_offset).unwrap_or(i64::MAX),
        );
        // Import always lands after capture, between a minute and four days.
        let import_lag = rng.range(60_000, 345_600_000);
        let created_at = Timestamp::from_millis(
            captured_at.as_millis() + i64::try_from(import_lag).unwrap_or(i64::MAX),
        );

        let segment_count = if marathon {
            self.spec.marathon_segments
        } else {
            self.spec.segments_per_recording
        };
        // Durations are consistent with the transcript: segments tile the
        // recording, so a viewport query over time ranges is meaningful.
        let segment_span_ms = rng.range(2_400, 9_600);
        let duration_ms = u64::from(segment_count) * segment_span_ms;

        let locale = LanguageTag::parse(rng.pick(LOCALES))
            .expect("the generator's locale table contains only valid BCP 47 tags");
        let source_kind = *rng.pick(SOURCE_KINDS);
        let account_scope = match source_kind {
            SourceKind::PlaudCloudCli => format!("account-{}", rng.range(1, 3)),
            SourceKind::PlaudDeviceBle | SourceKind::PlaudDeviceUsb => {
                format!("device-{}", rng.range(1, 4))
            }
            _ => "local".to_owned(),
        };
        let external_id = format!("ext-{index:06}-{:08x}", rng.next_u64() & 0xFFFF_FFFF);

        let tag_count = rng.range(0, 3);
        let mut tags: Vec<String> = (0..tag_count)
            .map(|_| (*rng.pick(TAGS)).to_owned())
            .collect();
        tags.sort();
        tags.dedup();

        let (engine, model_id) = if source_kind == SourceKind::PlaudCloudCli {
            ("openrouter", "whisper-large-v3")
        } else {
            ("whisper-cpp", "ggml-base.en-q5_1")
        };

        let speaker_count = if marathon {
            SPEAKER_NAMES.len()
        } else {
            usize::try_from(rng.below(3)).unwrap_or(0)
        };
        let speakers = (0..speaker_count)
            .map(|slot| {
                (
                    SpeakerId::from_uuid(rng.uuid()),
                    (*SPEAKER_NAMES
                        .get(slot)
                        .expect("speaker slot is bounded by the name table"))
                    .to_owned(),
                )
            })
            .collect::<Vec<_>>();

        let media = self.media(index, &mut rng);

        SyntheticRecording {
            index,
            id: RecordingId::from_uuid(rng.uuid()),
            revision_id: TranscriptRevisionId::from_uuid(rng.uuid()),
            title,
            captured_at,
            created_at,
            duration_ms,
            locale,
            source_kind,
            external_id,
            account_scope,
            tags,
            engine,
            model_id,
            speakers,
            media,
            segment_count,
        }
    }

    fn media(&self, index: u32, rng: &mut Rng) -> SyntheticMedia {
        let mut bytes = vec![0u8; self.spec.blob_bytes];
        // Prefix the index so two blobs can never collide, then fill the rest
        // from the stream; the archive stores and re-hashes these exact bytes.
        bytes[..4].copy_from_slice(&index.to_le_bytes());
        let mut body = Rng::seeded(self.spec.seed ^ (u64::from(index) << 17) ^ 0xA5A5);
        body.fill_bytes(&mut bytes[4..]);
        let extension = *rng.pick(EXTENSIONS);
        let identity = MediaIdentity::new(
            Sha256Digest::of(&bytes),
            bytes.len() as u64,
            codec_for(extension),
        );
        SyntheticMedia {
            id: MediaAssetId::from_uuid(rng.uuid()),
            identity,
            extension,
            quality_class: *rng.pick(QUALITY_CLASSES),
            bytes,
        }
    }

    /// Generate segments `[from, from + len)` of recording `index`.
    ///
    /// Segments are generated per absolute sequence index rather than as one
    /// vector, so a 100,000-segment transcript can be streamed into SQLite in
    /// batches without ever existing in memory as a whole.
    #[must_use]
    pub fn segments(&self, index: u32, from: u32, len: u32) -> Vec<SyntheticSegment> {
        let recording = self.recording(index);
        self.segments_of(&recording, from, len)
    }

    /// Same as [`SyntheticLibrary::segments`] for an already-generated recording.
    #[must_use]
    pub fn segments_of(
        &self,
        recording: &SyntheticRecording,
        from: u32,
        len: u32,
    ) -> Vec<SyntheticSegment> {
        let end = from.saturating_add(len).min(recording.segment_count);
        if from >= end {
            return Vec::new();
        }
        let span = recording.duration_ms / u64::from(recording.segment_count.max(1));
        let speakers = u8::try_from(recording.speakers.len()).unwrap_or(u8::MAX);
        (from..end)
            .map(|seq_index| {
                let mut rng = self.stream(recording.index, 0x5E6_0000 + u64::from(seq_index));
                let id = rng.uuid();
                let start_ms = u64::from(seq_index) * span;
                // A real engine leaves sub-second gaps; keep end inside the
                // next segment's start so the timeline never overlaps.
                let end_ms = start_ms + span.saturating_sub(rng.below(span / 4 + 1));
                let text = self.segment_text(&mut rng, recording.index, seq_index);
                SyntheticSegment {
                    id,
                    seq_index,
                    start_ms,
                    end_ms,
                    text,
                    // Engine-scoped confidence, two decimals, never exactly 1.0.
                    #[allow(clippy::cast_precision_loss)]
                    confidence: (rng.range(55, 99) as f64) / 100.0,
                    speaker_slot: if speakers == 0 {
                        None
                    } else {
                        Some(u8::try_from(rng.below(u64::from(speakers))).unwrap_or(0))
                    },
                }
            })
            .collect()
    }

    fn segment_text(&self, rng: &mut Rng, index: u32, seq_index: u32) -> String {
        let mut text = String::with_capacity(96);
        if seq_index == 0 {
            text.push_str(rng.pick(FIRST_SEGMENT_OPENERS));
            text.push(' ');
        }
        let word_count = rng.range(8, 22);
        for position in 0..word_count {
            if position > 0 {
                text.push(' ');
            }
            text.push_str(rng.pick(WORDS));
        }
        if let Some(needle) = self.needle_for(index, seq_index) {
            text.push(' ');
            text.push_str(needle);
        }
        text.push('.');
        text
    }

    /// The needle injected into a given segment, if any.
    ///
    /// Injection is a pure function of `(index, seq_index)` so the expected
    /// match count of a search is computable without scanning the database.
    #[must_use]
    pub fn needle_for(&self, index: u32, seq_index: u32) -> Option<&'static str> {
        let position = u64::from(index)
            .wrapping_mul(0x9E37_79B9)
            .wrapping_add(u64::from(seq_index));
        if position % NEEDLE_PERIOD != 0 {
            return None;
        }
        let slot = usize::try_from((position / NEEDLE_PERIOD) % SEARCH_NEEDLES.len() as u64)
            .unwrap_or(0);
        SEARCH_NEEDLES.get(slot).copied()
    }

    /// How many segments in the whole library contain `needle`.
    ///
    /// Counted from the injection rule, which is the only place needles are
    /// added — the word tables deliberately exclude them.
    #[must_use]
    pub fn expected_needle_matches(&self, needle: &str) -> u64 {
        let mut total = 0;
        for index in 0..=self.spec.marathon_index() {
            let count = if index == self.spec.marathon_index() {
                self.spec.marathon_segments
            } else {
                self.spec.segments_per_recording
            };
            for seq_index in 0..count {
                if self.needle_for(index, seq_index) == Some(needle) {
                    total += 1;
                }
            }
        }
        total
    }

    /// SHA-256 over every generated field in the dataset.
    ///
    /// Two generations from one seed must produce the same fingerprint; that
    /// is what makes a recorded benchmark result re-measurable.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(b"plaudon-scale-dataset-v1\n");
        hasher.update(self.spec.tier.as_str().as_bytes());
        hasher.update(self.spec.seed.to_le_bytes());
        hasher.update(self.spec.recordings.to_le_bytes());
        hasher.update(self.spec.segments_per_recording.to_le_bytes());
        hasher.update(self.spec.marathon_segments.to_le_bytes());
        hasher.update(self.spec.blob_bytes.to_le_bytes());

        for index in 0..=self.spec.marathon_index() {
            let recording = self.recording(index);
            hash_recording(&mut hasher, &recording);
            let mut from = 0;
            while from < recording.segment_count {
                let batch = self.segments_of(&recording, from, 4_096);
                for segment in &batch {
                    hasher.update(segment.id.as_bytes());
                    hasher.update(segment.seq_index.to_le_bytes());
                    hasher.update(segment.start_ms.to_le_bytes());
                    hasher.update(segment.end_ms.to_le_bytes());
                    hasher.update(segment.text.as_bytes());
                    hasher.update(segment.confidence.to_le_bytes());
                    hasher.update([segment.speaker_slot.unwrap_or(u8::MAX)]);
                }
                from += 4_096;
            }
        }
        hex::encode(hasher.finalize())
    }
}

fn hash_recording(hasher: &mut Sha256, recording: &SyntheticRecording) {
    hasher.update(recording.index.to_le_bytes());
    hasher.update(recording.id.to_string().as_bytes());
    hasher.update(recording.revision_id.to_string().as_bytes());
    hasher.update(recording.title.as_bytes());
    hasher.update(recording.captured_at.as_millis().to_le_bytes());
    hasher.update(recording.created_at.as_millis().to_le_bytes());
    hasher.update(recording.duration_ms.to_le_bytes());
    hasher.update(recording.locale.as_str().as_bytes());
    hasher.update(format!("{:?}", recording.source_kind).as_bytes());
    hasher.update(recording.external_id.as_bytes());
    hasher.update(recording.account_scope.as_bytes());
    for tag in &recording.tags {
        hasher.update(tag.as_bytes());
    }
    hasher.update(recording.engine.as_bytes());
    hasher.update(recording.model_id.as_bytes());
    for (id, label) in &recording.speakers {
        hasher.update(id.to_string().as_bytes());
        hasher.update(label.as_bytes());
    }
    hasher.update(recording.media.id.to_string().as_bytes());
    hasher.update(recording.media.identity.sha256.to_hex().as_bytes());
    hasher.update(recording.media.identity.byte_length.to_le_bytes());
    hasher.update(recording.media.extension.as_bytes());
    hasher.update(format!("{:?}", recording.media.quality_class).as_bytes());
    hasher.update(&recording.media.bytes);
    hasher.update(recording.segment_count.to_le_bytes());
}

/// Codec string recorded as provenance for a generated extension.
const fn codec_for(extension: &str) -> &'static str {
    match extension.as_bytes() {
        b"wav" => "pcm_s16le",
        b"mp3" => "mp3",
        b"m4a" => "aac",
        b"opus" => "opus",
        _ => "plaud-asr",
    }
}
