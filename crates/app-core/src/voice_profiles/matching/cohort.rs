//! The AS-Norm impostor cohort and adaptive score normalisation (VP-052).
//!
//! Adaptive symmetric score normalisation (Matějka et al., Interspeech 2017)
//! replaces a raw cosine with
//!
//! ```text
//! s_asnorm(e, t) = ½ · [ (s − μ_E)/σ_E + (s − μ_T)/σ_T ]
//! ```
//!
//! where `μ_E, σ_E` are the mean and standard deviation of the similarities
//! between the *enrolment* vector and its top-K impostor cohort, and `μ_T, σ_T`
//! the same for the *test* vector. The point for this product is not the EER
//! headline but comparability: a score computed from a laptop microphone and a
//! score computed from a conference speakerphone land in the same space, which
//! is the cross-channel variance problem of VP-067.
//!
//! The cohort itself is licence-clean (VP-181): a `[N, D]` f32 matrix derived
//! from LibriSpeech/synthetic fixtures, shipped inside the model pack beside
//! the checkpoint it belongs to, and bound to that checkpoint exactly like an
//! exemplar is (VP-075).

use std::cmp::Ordering;

use super::embedding::{dot, CosineSimilarity, Embedding, ModelBinding, NormalisedScore};

/// Magic bytes of the cohort file carried by the model pack.
pub const COHORT_MAGIC: [u8; 6] = *b"PLVCOH";

/// Version of the cohort file layout.
pub const COHORT_FORMAT_VERSION: u16 = 1;

/// Size of the cohort file header: magic, version, row count, dimension.
pub const COHORT_HEADER_BYTES: usize = 16;

/// Smallest usable cohort.
///
/// AS-Norm needs a standard deviation, and a standard deviation needs at least
/// two samples; [`TopK`] enforces the same floor on the selection size.
pub const MIN_COHORT_ROWS: usize = 2;

/// Largest cohort this module will decode.
///
/// The spec budgets "a few hundred KB" for the shipped matrix; the ceiling is
/// far above that and exists so a corrupt length field cannot become a
/// multi-gigabyte allocation.
pub const MAX_COHORT_ROWS: usize = 100_000;

/// Smallest standard deviation that still normalises a score.
///
/// A cohort whose top-K similarities are all equal carries no information about
/// the score's scale; dividing by it would manufacture an arbitrarily large
/// score. That is a broken cohort, and a refusal is the honest outcome.
const MIN_STD_DEV: f64 = 1e-6;

/// How many cohort scores AS-Norm averages over (the "adaptive" part).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TopK(usize);

impl TopK {
    /// Wrap a selection size, refusing anything below [`MIN_COHORT_ROWS`].
    pub fn new(k: usize) -> Result<Self, CohortError> {
        if k < MIN_COHORT_ROWS {
            return Err(CohortError::TopKTooSmall { k });
        }
        Ok(Self(k))
    }

    /// The selection size.
    #[must_use]
    pub fn get(self) -> usize {
        self.0
    }
}

/// Why a cohort could not be built, decoded or used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CohortError {
    /// Fewer than [`MIN_COHORT_ROWS`] rows, or more than [`MAX_COHORT_ROWS`].
    #[error("cohort has {rows} rows, outside {MIN_COHORT_ROWS}..={MAX_COHORT_ROWS}")]
    RowCount {
        /// Row count that was offered.
        rows: usize,
    },

    /// A row was not a usable vector.
    #[error("cohort row {row} is not a usable vector")]
    Row {
        /// Index of the offending row.
        row: usize,
    },

    /// Rows disagreed about the dimension.
    #[error("cohort row {row} has dimension {found}, expected {expected}")]
    RaggedRow {
        /// Index of the offending row.
        row: usize,
        /// Dimension that row carries.
        found: usize,
        /// Dimension the matrix is built on.
        expected: usize,
    },

    /// The cohort dimension does not match the vector being scored.
    #[error("cohort is {cohort}-dimensional, the embedding is {embedding}-dimensional")]
    Dimension {
        /// Dimension of the cohort.
        cohort: usize,
        /// Dimension of the embedding.
        embedding: usize,
    },

    /// `K` is below the minimum.
    #[error("AS-Norm needs at least {MIN_COHORT_ROWS} cohort scores, asked for {k}")]
    TopKTooSmall {
        /// Requested selection size.
        k: usize,
    },

    /// `K` exceeds the cohort.
    #[error("AS-Norm asked for the top {k} of only {rows} cohort rows")]
    TopKTooLarge {
        /// Requested selection size.
        k: usize,
        /// Rows available.
        rows: usize,
    },

    /// The top-K cohort scores have no spread, so they cannot normalise.
    #[error("cohort statistics are degenerate: the top-K similarities have no spread")]
    DegenerateStatistics,

    /// The encoded cohort is not a cohort file.
    #[error("cohort file is malformed: {reason}")]
    Malformed {
        /// What failed, with no attacker-controlled content.
        reason: &'static str,
    },
}

/// An impostor cohort: `[N, D]` unit vectors bound to one checkpoint.
#[derive(Debug, Clone, PartialEq)]
pub struct CohortMatrix {
    binding: ModelBinding,
    dim: usize,
    /// Row-major `rows × dim`, each row L2-normalised.
    values: Vec<f32>,
}

impl CohortMatrix {
    /// Build a cohort from raw vectors, normalising each row.
    pub fn from_vectors(binding: ModelBinding, vectors: &[Vec<f32>]) -> Result<Self, CohortError> {
        if !(MIN_COHORT_ROWS..=MAX_COHORT_ROWS).contains(&vectors.len()) {
            return Err(CohortError::RowCount {
                rows: vectors.len(),
            });
        }
        let dim = vectors[0].len();
        let mut values = Vec::with_capacity(vectors.len() * dim);
        for (row, vector) in vectors.iter().enumerate() {
            if vector.len() != dim {
                return Err(CohortError::RaggedRow {
                    row,
                    found: vector.len(),
                    expected: dim,
                });
            }
            let normalised = Embedding::l2_normalise(binding.clone(), vector)
                .map_err(|_| CohortError::Row { row })?;
            values.extend_from_slice(normalised.unit());
        }
        Ok(Self {
            binding,
            dim,
            values,
        })
    }

    /// Decode the cohort file shipped inside the model pack.
    ///
    /// Layout, little-endian throughout:
    ///
    /// ```text
    /// offset  size  field
    ///      0     6  magic "PLVCOH"
    ///      6     2  format version (u16)
    ///      8     4  row count (u32)
    ///     12     4  dimension (u32)
    ///     16   4·N·D  f32 components, row-major
    /// ```
    ///
    /// Every length is checked against the actual byte count before a single
    /// component is read, and trailing bytes are a refusal rather than padding.
    pub fn decode(binding: ModelBinding, bytes: &[u8]) -> Result<Self, CohortError> {
        if bytes.len() < COHORT_HEADER_BYTES {
            return Err(CohortError::Malformed {
                reason: "shorter than the header",
            });
        }
        if bytes[..6] != COHORT_MAGIC {
            return Err(CohortError::Malformed {
                reason: "wrong magic",
            });
        }
        let version = u16::from_le_bytes([bytes[6], bytes[7]]);
        if version != COHORT_FORMAT_VERSION {
            return Err(CohortError::Malformed {
                reason: "unsupported format version",
            });
        }
        let rows = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize;
        let dim = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]) as usize;
        if !(MIN_COHORT_ROWS..=MAX_COHORT_ROWS).contains(&rows) {
            return Err(CohortError::RowCount { rows });
        }
        let components = rows
            .checked_mul(dim)
            .ok_or(CohortError::Malformed {
                reason: "declared size overflows",
            })?
            .checked_mul(4)
            .ok_or(CohortError::Malformed {
                reason: "declared size overflows",
            })?;
        if bytes.len() != COHORT_HEADER_BYTES + components {
            return Err(CohortError::Malformed {
                reason: "byte count does not match the declared shape",
            });
        }

        let mut vectors = Vec::with_capacity(rows);
        let mut offset = COHORT_HEADER_BYTES;
        for _ in 0..rows {
            let mut vector = Vec::with_capacity(dim);
            for _ in 0..dim {
                let component = f32::from_le_bytes([
                    bytes[offset],
                    bytes[offset + 1],
                    bytes[offset + 2],
                    bytes[offset + 3],
                ]);
                vector.push(component);
                offset += 4;
            }
            vectors.push(vector);
        }
        Self::from_vectors(binding, &vectors)
    }

    /// Encode in the [`CohortMatrix::decode`] layout.
    ///
    /// Used by the §17.2 harness and the pack builder; the round trip is what
    /// keeps the decoder honest.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(COHORT_HEADER_BYTES + self.values.len() * 4);
        out.extend_from_slice(&COHORT_MAGIC);
        out.extend_from_slice(&COHORT_FORMAT_VERSION.to_le_bytes());
        out.extend_from_slice(&(self.len() as u32).to_le_bytes());
        out.extend_from_slice(&(self.dim as u32).to_le_bytes());
        for value in &self.values {
            out.extend_from_slice(&value.to_le_bytes());
        }
        out
    }

    /// Checkpoint this cohort belongs to.
    #[must_use]
    pub fn binding(&self) -> &ModelBinding {
        &self.binding
    }

    /// Number of impostor vectors.
    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len() / self.dim
    }

    /// Always `false`: a cohort below [`MIN_COHORT_ROWS`] cannot be built.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Dimension of the cohort vectors.
    #[must_use]
    pub fn dim(&self) -> usize {
        self.dim
    }

    /// Mean and standard deviation of the top-`k` similarities between
    /// `embedding` and this cohort.
    pub fn statistics(&self, embedding: &Embedding, k: TopK) -> Result<CohortStats, CohortError> {
        if embedding.dim() != self.dim {
            return Err(CohortError::Dimension {
                cohort: self.dim,
                embedding: embedding.dim(),
            });
        }
        let rows = self.len();
        if k.get() > rows {
            return Err(CohortError::TopKTooLarge { k: k.get(), rows });
        }

        let mut similarities: Vec<f64> = (0..rows)
            .map(|row| {
                let start = row * self.dim;
                dot(embedding.unit(), &self.values[start..start + self.dim]).clamp(-1.0, 1.0)
            })
            .collect();

        // Partial selection: only the top K matter, so do not pay for a sort.
        let k = k.get();
        if k < rows {
            similarities.select_nth_unstable_by(k - 1, |a, b| descending(*a, *b));
        }
        let top = &similarities[..k];

        let count = k as f64;
        let mean = top.iter().sum::<f64>() / count;
        let variance = top.iter().map(|s| (s - mean) * (s - mean)).sum::<f64>() / count;
        let std_dev = variance.sqrt();
        if !std_dev.is_finite() || std_dev < MIN_STD_DEV {
            return Err(CohortError::DegenerateStatistics);
        }
        Ok(CohortStats { mean, std_dev })
    }
}

fn descending(a: f64, b: f64) -> Ordering {
    b.total_cmp(&a)
}

/// Top-K cohort statistics for one vector (VP-052).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CohortStats {
    mean: f64,
    std_dev: f64,
}

impl CohortStats {
    /// Mean of the top-K cohort similarities.
    #[must_use]
    pub fn mean(self) -> f64 {
        self.mean
    }

    /// Standard deviation of the top-K cohort similarities (population form).
    #[must_use]
    pub fn std_dev(self) -> f64 {
        self.std_dev
    }
}

/// Adaptive S-norm: normalise `raw` against both sides' cohort statistics.
///
/// Both standard deviations are strictly positive — [`CohortMatrix::statistics`]
/// refuses to produce a degenerate one — so the result is finite.
#[must_use]
pub fn as_norm(
    raw: CosineSimilarity,
    enrolment: CohortStats,
    test: CohortStats,
) -> NormalisedScore {
    let raw = f64::from(raw.value());
    let enrolment_z = (raw - enrolment.mean) / enrolment.std_dev;
    let test_z = (raw - test.mean) / test.std_dev;
    NormalisedScore::finite(f64::midpoint(enrolment_z, test_z))
}

#[cfg(test)]
mod tests {
    use super::{
        as_norm, CohortError, CohortMatrix, TopK, COHORT_FORMAT_VERSION, COHORT_HEADER_BYTES,
        COHORT_MAGIC, MAX_COHORT_ROWS,
    };
    use crate::voice_profiles::matching::embedding::CosineSimilarity;
    use crate::voice_profiles::matching::test_support::{binding, cohort_vectors, unit};

    fn cohort() -> CohortMatrix {
        CohortMatrix::from_vectors(binding(), &cohort_vectors(16, 8, 11)).expect("cohort")
    }

    #[test]
    fn statistics_are_taken_over_exactly_the_top_k_similarities() {
        // A cohort built from the axes: the similarities against `probe` are
        // known exactly, so the expected mean and deviation are hand-computed.
        let vectors = vec![
            vec![1.0, 0.0, 0.0, 0.0],
            vec![0.0, 1.0, 0.0, 0.0],
            vec![0.0, 0.0, 1.0, 0.0],
            vec![0.0, 0.0, 0.0, 1.0],
        ];
        let cohort = CohortMatrix::from_vectors(binding(), &vectors).expect("cohort");
        let probe = unit(&[0.8, 0.6, 0.0, 0.0]);
        let stats = cohort
            .statistics(&probe, TopK::new(2).expect("k"))
            .expect("stats");
        assert!((stats.mean() - 0.7).abs() < 1e-6, "{:?}", stats.mean());
        assert!(
            (stats.std_dev() - 0.1).abs() < 1e-6,
            "{:?}",
            stats.std_dev()
        );
    }

    #[test]
    fn as_norm_is_the_mean_of_the_two_z_scores() {
        let vectors = vec![
            vec![1.0, 0.0, 0.0, 0.0],
            vec![0.0, 1.0, 0.0, 0.0],
            vec![0.0, 0.0, 1.0, 0.0],
            vec![0.0, 0.0, 0.0, 1.0],
        ];
        let cohort = CohortMatrix::from_vectors(binding(), &vectors).expect("cohort");
        let k = TopK::new(2).expect("k");
        let enrolment = cohort
            .statistics(&unit(&[0.8, 0.6, 0.0, 0.0]), k)
            .expect("enrolment stats");
        let test = cohort
            .statistics(&unit(&[0.6, 0.8, 0.0, 0.0]), k)
            .expect("test stats");
        let raw = CosineSimilarity::new(0.96).expect("raw");
        // Both sides have mean 0.7 and deviation 0.1, so the z-scores coincide.
        let expected = (0.96 - 0.7) / 0.1;
        let scored = as_norm(raw, enrolment, test);
        assert!(
            (f64::from(scored.value()) - expected).abs() < 1e-5,
            "{scored}"
        );
    }

    #[test]
    fn a_cohort_with_no_spread_refuses_to_normalise() {
        let identical = vec![
            vec![1.0, 0.0, 0.0],
            vec![2.0, 0.0, 0.0],
            vec![3.0, 0.0, 0.0],
        ];
        let cohort = CohortMatrix::from_vectors(binding(), &identical).expect("cohort");
        assert_eq!(
            cohort.statistics(&unit(&[0.0, 1.0, 0.0]), TopK::new(3).expect("k")),
            Err(CohortError::DegenerateStatistics)
        );
    }

    #[test]
    fn top_k_must_be_at_least_two_and_must_fit_the_cohort() {
        assert_eq!(TopK::new(1), Err(CohortError::TopKTooSmall { k: 1 }));
        let cohort = cohort();
        let probe = unit(&[1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
        assert_eq!(
            cohort.statistics(&probe, TopK::new(17).expect("k")),
            Err(CohortError::TopKTooLarge { k: 17, rows: 16 })
        );
    }

    #[test]
    fn a_cohort_of_a_different_dimension_refuses_to_score() {
        let cohort = cohort();
        let probe = unit(&[1.0, 0.0, 0.0]);
        assert_eq!(
            cohort.statistics(&probe, TopK::new(4).expect("k")),
            Err(CohortError::Dimension {
                cohort: 8,
                embedding: 3
            })
        );
    }

    #[test]
    fn the_pack_file_round_trips() {
        let cohort = cohort();
        let bytes = cohort.encode();
        assert_eq!(bytes.len(), COHORT_HEADER_BYTES + 16 * 8 * 4);
        let decoded = CohortMatrix::decode(binding(), &bytes).expect("decode");
        assert_eq!(decoded, cohort);
    }

    #[test]
    fn a_malformed_cohort_file_is_refused_rather_than_trusted() {
        let bytes = cohort().encode();

        assert!(matches!(
            CohortMatrix::decode(binding(), &bytes[..8]),
            Err(CohortError::Malformed { .. })
        ));

        let mut wrong_magic = bytes.clone();
        wrong_magic[0] = b'X';
        assert!(matches!(
            CohortMatrix::decode(binding(), &wrong_magic),
            Err(CohortError::Malformed {
                reason: "wrong magic"
            })
        ));

        let mut wrong_version = bytes.clone();
        wrong_version[6] = COHORT_FORMAT_VERSION as u8 + 1;
        assert!(matches!(
            CohortMatrix::decode(binding(), &wrong_version),
            Err(CohortError::Malformed {
                reason: "unsupported format version"
            })
        ));

        let truncated = &bytes[..bytes.len() - 4];
        assert!(matches!(
            CohortMatrix::decode(binding(), truncated),
            Err(CohortError::Malformed { .. })
        ));

        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(matches!(
            CohortMatrix::decode(binding(), &trailing),
            Err(CohortError::Malformed { .. })
        ));

        // A hostile shape that would multiply into an enormous allocation.
        let mut hostile = Vec::new();
        hostile.extend_from_slice(&COHORT_MAGIC);
        hostile.extend_from_slice(&COHORT_FORMAT_VERSION.to_le_bytes());
        hostile.extend_from_slice(&u32::MAX.to_le_bytes());
        hostile.extend_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(
            CohortMatrix::decode(binding(), &hostile),
            Err(CohortError::RowCount {
                rows: u32::MAX as usize
            })
        );
        assert!(MAX_COHORT_ROWS < u32::MAX as usize);
    }

    #[test]
    fn a_cohort_file_carrying_a_non_finite_component_is_refused() {
        let mut bytes = cohort().encode();
        let at = COHORT_HEADER_BYTES;
        bytes[at..at + 4].copy_from_slice(&f32::NAN.to_le_bytes());
        assert_eq!(
            CohortMatrix::decode(binding(), &bytes),
            Err(CohortError::Row { row: 0 })
        );
    }

    #[test]
    fn a_ragged_or_too_small_cohort_is_refused() {
        assert_eq!(
            CohortMatrix::from_vectors(binding(), &[vec![1.0, 0.0]]),
            Err(CohortError::RowCount { rows: 1 })
        );
        assert_eq!(
            CohortMatrix::from_vectors(binding(), &[vec![1.0, 0.0], vec![1.0, 0.0, 0.0]]),
            Err(CohortError::RaggedRow {
                row: 1,
                found: 3,
                expected: 2
            })
        );
        assert_eq!(
            CohortMatrix::from_vectors(binding(), &[vec![1.0, 0.0], vec![0.0, 0.0]]),
            Err(CohortError::Row { row: 1 })
        );
    }
}
