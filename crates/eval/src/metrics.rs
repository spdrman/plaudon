//! The offline-computable part of the spec 17.2 metric set.
//!
//! > We will measure word error rate (WER), character error rate (CER),
//! > domain-term accuracy, number/date/amount accuracy, hallucinated content
//! > in silence, timestamp error where present, [...] time-to-first-result,
//! > real-time factor, peak RSS [...]
//!
//! Everything here is a pure function of a reference transcript and a
//! hypothesis transcript. Nothing calls an engine, reads audio, or touches the
//! network. Two deliberate refusals:
//!
//! * A metric with no denominator returns `None`, never `0.0`. An empty
//!   reference has no error rate; a hypothesis whose alignment origin is
//!   `unavailable` has no timestamp error.
//! * Runtime figures ([`RuntimeMeasurement`]) are *recorded inputs*. This
//!   module derives the real-time factor from them and invents nothing: a
//!   missing measurement stays missing all the way into the report.
//!
//! # Normalization pipeline
//!
//! [`normalize_tokens`] is applied identically to reference and hypothesis
//! before WER, CER, domain-term and numeric scoring:
//!
//! 1. Lowercase with `str::to_lowercase`.
//! 2. Split on Unicode whitespace.
//! 3. Trim punctuation from both ends of each token, except the characters in
//!    [`KEPT_EDGE_CHARACTERS`] (currency symbols and `%`), which carry meaning
//!    for amount scoring.
//! 4. Drop tokens that are empty after trimming.
//!
//! Token-interior characters are preserved, so `2026-03-14`, `1,250.00` and
//! `isn't` survive as single tokens. **Number spelling is left as-is**: the
//! pipeline never converts `twenty five` to `25` or the reverse, so a spelled
//! number against a digit reference counts as an error. That is a measurement
//! decision, documented here so a reported WER is interpretable, and it is why
//! [`NumericAccuracy`] is reported separately from WER.
//!
//! Unicode normalization (NFC/NFD) is deliberately not applied: it would need
//! a dependency and would silently change what a reference transcript says.
//! Corpus files are committed as NFC.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::hypothesis::HypothesisAlignment;

/// Characters kept at a token edge because they carry measurement meaning.
pub const KEPT_EDGE_CHARACTERS: &[char] = &['$', '€', '£', '¥', '%'];

/// Currency words and codes that mark an adjacent number as an amount.
const CURRENCY_WORDS: &[&str] = &[
    "usd", "eur", "gbp", "jpy", "chf", "cad", "aud", "dollar", "dollars", "euro", "euros", "pound",
    "pounds", "yen", "cent", "cents",
];

/// Month names and common abbreviations that mark an adjacent number as a date.
const MONTH_WORDS: &[&str] = &[
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
    "jan",
    "feb",
    "mar",
    "apr",
    "jun",
    "jul",
    "aug",
    "sep",
    "sept",
    "oct",
    "nov",
    "dec",
];

/// Divide two counts, refusing to invent a value when the denominator is zero.
#[allow(clippy::cast_precision_loss)]
fn ratio(numerator: usize, denominator: usize) -> Option<f64> {
    if denominator == 0 {
        return None;
    }
    Some(numerator as f64 / denominator as f64)
}

/// Apply the documented normalization pipeline and return the token stream.
#[must_use]
pub fn normalize_tokens(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split_whitespace()
        .map(|token| {
            token
                .trim_matches(|c: char| {
                    !c.is_alphanumeric() && !KEPT_EDGE_CHARACTERS.contains(&c)
                })
                .to_owned()
        })
        .filter(|token| !token.is_empty())
        .collect()
}

/// The normalized token stream rejoined with single spaces.
///
/// This is the exact character stream CER is measured over, so a reported CER
/// is reproducible from the published normalization rules alone.
#[must_use]
pub fn normalize_text(text: &str) -> String {
    normalize_tokens(text).join(" ")
}

/// Edit counts behind an error rate.
///
/// Counts are kept rather than only the ratio so that corpus-level aggregation
/// is a sum of edits over a sum of reference lengths, which is the only
/// correct way to pool WER across items of different lengths.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditCounts {
    /// Reference units replaced by a different hypothesis unit.
    pub substitutions: usize,
    /// Reference units absent from the hypothesis.
    pub deletions: usize,
    /// Hypothesis units absent from the reference.
    pub insertions: usize,
    /// Length of the normalized reference stream.
    pub reference_length: usize,
}

impl EditCounts {
    /// Total edit distance.
    #[must_use]
    pub const fn edits(&self) -> usize {
        self.substitutions + self.deletions + self.insertions
    }

    /// Error rate, or `None` when the reference is empty.
    ///
    /// A rate above `1.0` is possible and is reported as measured: a
    /// hypothesis may contain more insertions than the reference has units.
    #[must_use]
    pub fn rate(&self) -> Option<f64> {
        ratio(self.edits(), self.reference_length)
    }

    /// Pool another item's counts into this one.
    pub fn absorb(&mut self, other: Self) {
        self.substitutions += other.substitutions;
        self.deletions += other.deletions;
        self.insertions += other.insertions;
        self.reference_length += other.reference_length;
    }
}

/// One cell of the alignment lattice.
#[derive(Clone, Copy, Default)]
struct Cell {
    cost: usize,
    substitutions: usize,
    deletions: usize,
    insertions: usize,
}

impl Cell {
    const fn counts(self, reference_length: usize) -> EditCounts {
        EditCounts {
            substitutions: self.substitutions,
            deletions: self.deletions,
            insertions: self.insertions,
            reference_length,
        }
    }
}

/// Levenshtein alignment over two unit streams, keeping the operation mix.
///
/// Two rows of lattice are held at a time, so a three-hour transcript aligns
/// in memory proportional to the shorter stream rather than their product.
/// Ties are broken substitution, then deletion, then insertion, which makes
/// the reported operation mix deterministic for equal-cost alignments.
fn align<T: PartialEq>(reference: &[T], hypothesis: &[T]) -> EditCounts {
    let mut previous: Vec<Cell> = (0..=hypothesis.len())
        .map(|index| Cell {
            cost: index,
            insertions: index,
            ..Cell::default()
        })
        .collect();
    let mut current = vec![Cell::default(); hypothesis.len() + 1];

    for (row, reference_unit) in reference.iter().enumerate() {
        current[0] = Cell {
            cost: row + 1,
            deletions: row + 1,
            ..Cell::default()
        };
        for column in 1..=hypothesis.len() {
            if *reference_unit == hypothesis[column - 1] {
                current[column] = previous[column - 1];
                continue;
            }
            let substitute = Cell {
                cost: previous[column - 1].cost + 1,
                substitutions: previous[column - 1].substitutions + 1,
                ..previous[column - 1]
            };
            let delete = Cell {
                cost: previous[column].cost + 1,
                deletions: previous[column].deletions + 1,
                ..previous[column]
            };
            let insert = Cell {
                cost: current[column - 1].cost + 1,
                insertions: current[column - 1].insertions + 1,
                ..current[column - 1]
            };
            let best = if substitute.cost <= delete.cost && substitute.cost <= insert.cost {
                substitute
            } else if delete.cost <= insert.cost {
                delete
            } else {
                insert
            };
            current[column] = best;
        }
        std::mem::swap(&mut previous, &mut current);
    }

    previous[hypothesis.len()].counts(reference.len())
}

/// Word error rate over the normalized token streams.
#[must_use]
pub fn word_error_rate(reference: &str, hypothesis: &str) -> EditCounts {
    align(&normalize_tokens(reference), &normalize_tokens(hypothesis))
}

/// Character error rate over the normalized, space-joined character streams.
#[must_use]
pub fn character_error_rate(reference: &str, hypothesis: &str) -> EditCounts {
    let reference: Vec<char> = normalize_text(reference).chars().collect();
    let hypothesis: Vec<char> = normalize_text(hypothesis).chars().collect();
    align(&reference, &hypothesis)
}

/// Per-term outcome of the domain vocabulary check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainTermOutcome {
    /// The term as supplied, before normalization.
    pub term: String,
    /// Non-overlapping occurrences in the normalized reference.
    pub reference_occurrences: usize,
    /// Non-overlapping occurrences in the normalized hypothesis.
    pub hypothesis_occurrences: usize,
}

impl DomainTermOutcome {
    /// Recall for this term, or `None` when the reference never says it.
    #[must_use]
    pub fn recall(&self) -> Option<f64> {
        ratio(
            self.hypothesis_occurrences.min(self.reference_occurrences),
            self.reference_occurrences,
        )
    }
}

/// Domain-term accuracy over a supplied term list (spec 17.2).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainTermAccuracy {
    /// Outcome per term that the reference actually contains.
    pub terms: Vec<DomainTermOutcome>,
    /// Terms the caller supplied that the reference never says. These are
    /// excluded from recall instead of being scored as misses.
    pub absent_from_reference: Vec<String>,
}

impl DomainTermAccuracy {
    /// Occurrences recovered by the hypothesis.
    #[must_use]
    pub fn matched(&self) -> usize {
        self.terms
            .iter()
            .map(|term| {
                term.hypothesis_occurrences
                    .min(term.reference_occurrences)
            })
            .sum()
    }

    /// Occurrences the reference contains.
    #[must_use]
    pub fn expected(&self) -> usize {
        self.terms
            .iter()
            .map(|term| term.reference_occurrences)
            .sum()
    }

    /// Pooled recall, or `None` when no supplied term occurs in the reference.
    #[must_use]
    pub fn recall(&self) -> Option<f64> {
        ratio(self.matched(), self.expected())
    }

    /// Pool another item's outcome into this one, merging per-term counts.
    pub fn absorb(&mut self, other: Self) {
        for incoming in other.terms {
            match self
                .terms
                .iter_mut()
                .find(|existing| existing.term == incoming.term)
            {
                Some(existing) => {
                    existing.reference_occurrences += incoming.reference_occurrences;
                    existing.hypothesis_occurrences += incoming.hypothesis_occurrences;
                }
                None => self.terms.push(incoming),
            }
        }
        for term in other.absent_from_reference {
            if !self.absent_from_reference.contains(&term)
                && !self.terms.iter().any(|existing| existing.term == term)
            {
                self.absent_from_reference.push(term);
            }
        }
        self.absent_from_reference
            .retain(|term| !self.terms.iter().any(|existing| existing.term == *term));
    }
}


/// Count non-overlapping occurrences of a token sequence inside a stream.
fn count_sequence(stream: &[String], needle: &[String]) -> usize {
    if needle.is_empty() || needle.len() > stream.len() {
        return 0;
    }
    let mut count = 0;
    let mut index = 0;
    while index + needle.len() <= stream.len() {
        if stream[index..index + needle.len()] == *needle {
            count += 1;
            index += needle.len();
        } else {
            index += 1;
        }
    }
    count
}

/// Per-term recall of a supplied domain vocabulary (spec 17.2).
///
/// Terms are normalized with the same pipeline as the transcripts, so a
/// multi-word term such as `slump test` is matched as a contiguous token
/// sequence rather than as two independent words.
#[must_use]
pub fn domain_term_accuracy(reference: &str, hypothesis: &str, terms: &[String]) -> DomainTermAccuracy {
    let reference_tokens = normalize_tokens(reference);
    let hypothesis_tokens = normalize_tokens(hypothesis);
    let mut outcomes = Vec::new();
    let mut absent = Vec::new();
    for term in terms {
        let needle = normalize_tokens(term);
        let reference_occurrences = count_sequence(&reference_tokens, &needle);
        if reference_occurrences == 0 {
            absent.push(term.clone());
            continue;
        }
        outcomes.push(DomainTermOutcome {
            term: term.clone(),
            reference_occurrences,
            hypothesis_occurrences: count_sequence(&hypothesis_tokens, &needle),
        });
    }
    DomainTermAccuracy {
        terms: outcomes,
        absent_from_reference: absent,
    }
}

/// Class of a numeric entity (spec 17.2 "number/date/amount accuracy").
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NumericKind {
    /// A bare quantity.
    Number,
    /// A calendar date, or a day number next to a month name.
    Date,
    /// A currency amount.
    Amount,
}

impl NumericKind {
    /// The exact token used in reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Number => "number",
            Self::Date => "date",
            Self::Amount => "amount",
        }
    }
}

/// A number, date or amount found in a transcript.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NumericEntity {
    /// Canonical text: normalized token with digit-group separators removed.
    pub text: String,
    /// Class of the entity.
    pub kind: NumericKind,
}

/// Matched/expected counts for one numeric class.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NumericTally {
    /// Reference entities recovered by the hypothesis.
    pub matched: usize,
    /// Reference entities of this class.
    pub expected: usize,
}

impl NumericTally {
    /// Recall for this class, or `None` when the reference has none.
    #[must_use]
    pub fn recall(&self) -> Option<f64> {
        ratio(self.matched, self.expected)
    }
}

/// Number/date/amount accuracy (spec 17.2).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NumericAccuracy {
    /// Counts per class.
    pub per_kind: BTreeMap<NumericKind, NumericTally>,
    /// Hypothesis entities with no counterpart in the reference: a number the
    /// engine produced that nobody said.
    pub invented: Vec<NumericEntity>,
}

impl NumericAccuracy {
    /// Reference entities recovered by the hypothesis.
    #[must_use]
    pub fn matched(&self) -> usize {
        self.per_kind.values().map(|tally| tally.matched).sum()
    }

    /// Reference entities across all classes.
    #[must_use]
    pub fn expected(&self) -> usize {
        self.per_kind.values().map(|tally| tally.expected).sum()
    }

    /// Pooled recall, or `None` when the reference contains no numerics.
    #[must_use]
    pub fn recall(&self) -> Option<f64> {
        ratio(self.matched(), self.expected())
    }

    /// Pool another item's outcome into this one.
    pub fn absorb(&mut self, other: Self) {
        for (kind, tally) in other.per_kind {
            let entry = self.per_kind.entry(kind).or_default();
            entry.matched += tally.matched;
            entry.expected += tally.expected;
        }
        self.invented.extend(other.invented);
    }
}

fn is_numeric_token(token: &str) -> bool {
    token.chars().any(|c| c.is_ascii_digit())
}

fn canonical_numeric(token: &str) -> String {
    // Remove digit-group separators (`1,250` -> `1250`) but keep a decimal
    // point, a date separator and any currency symbol.
    let characters: Vec<char> = token.chars().collect();
    let mut out = String::with_capacity(characters.len());
    for (index, character) in characters.iter().enumerate() {
        let is_group_separator = *character == ','
            && index > 0
            && characters[index - 1].is_ascii_digit()
            && characters
                .get(index + 1)
                .is_some_and(char::is_ascii_digit);
        if !is_group_separator {
            out.push(*character);
        }
    }
    out
}

fn looks_like_date(token: &str) -> bool {
    let parts: Vec<&str> = if token.contains('-') {
        token.split('-').collect()
    } else if token.contains('/') {
        token.split('/').collect()
    } else {
        return false;
    };
    (parts.len() == 2 || parts.len() == 3)
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
}

fn classify(tokens: &[String], index: usize) -> NumericKind {
    let token = &tokens[index];
    let neighbour = |offset: isize| -> Option<&str> {
        let position = usize::try_from(isize::try_from(index).ok()? + offset).ok()?;
        tokens.get(position).map(String::as_str)
    };
    if token.starts_with(KEPT_EDGE_CHARACTERS)
        || neighbour(-1).is_some_and(|word| CURRENCY_WORDS.contains(&word))
        || neighbour(1).is_some_and(|word| CURRENCY_WORDS.contains(&word))
    {
        return NumericKind::Amount;
    }
    if looks_like_date(token)
        || neighbour(-1).is_some_and(|word| MONTH_WORDS.contains(&word))
        || neighbour(1).is_some_and(|word| MONTH_WORDS.contains(&word))
    {
        return NumericKind::Date;
    }
    NumericKind::Number
}

/// Extract numbers, dates and amounts from a transcript.
///
/// A token is numeric when it contains an ASCII digit. Classification uses the
/// token's own shape plus one neighbouring token: a leading currency symbol or
/// an adjacent currency word makes an amount, an ISO or slash-separated date
/// shape or an adjacent month name makes a date, everything else is a number.
/// Spelled-out numbers are not extracted, matching the normalization rule that
/// leaves number spelling untouched.
#[must_use]
pub fn extract_numeric_entities(text: &str) -> Vec<NumericEntity> {
    let tokens = normalize_tokens(text);
    tokens
        .iter()
        .enumerate()
        .filter(|(_, token)| is_numeric_token(token))
        .map(|(index, token)| NumericEntity {
            text: canonical_numeric(token),
            kind: classify(&tokens, index),
        })
        .collect()
}

/// Number/date/amount accuracy as multiset recall of the reference entities.
#[must_use]
pub fn numeric_accuracy(reference: &str, hypothesis: &str) -> NumericAccuracy {
    let expected = extract_numeric_entities(reference);
    let mut remaining = extract_numeric_entities(hypothesis);
    let mut accuracy = NumericAccuracy::default();
    for entity in expected {
        let tally = accuracy.per_kind.entry(entity.kind).or_default();
        tally.expected += 1;
        if let Some(position) = remaining.iter().position(|candidate| *candidate == entity) {
            remaining.remove(position);
            tally.matched += 1;
        }
    }
    accuracy.invented = remaining;
    accuracy
}

/// A half-open `[start, end)` span of source time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TimeSpanMs {
    /// Inclusive start, in milliseconds from the start of the media.
    pub start_ms: u64,
    /// Exclusive end, in milliseconds from the start of the media.
    pub end_ms: u64,
}

impl TimeSpanMs {
    /// Span length, zero when the span is empty or inverted.
    #[must_use]
    pub const fn duration_ms(self) -> u64 {
        self.end_ms.saturating_sub(self.start_ms)
    }

    /// Overlap with another span, in milliseconds.
    #[must_use]
    pub fn overlap_ms(self, other: Self) -> u64 {
        let start = self.start_ms.max(other.start_ms);
        let end = self.end_ms.min(other.end_ms);
        end.saturating_sub(start)
    }
}

/// A hypothesis word with engine-reported times.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimedWord {
    /// Word text as the engine produced it.
    pub text: String,
    /// Word start in milliseconds from the start of the media.
    pub start_ms: u64,
    /// Word end in milliseconds from the start of the media.
    pub end_ms: u64,
}

impl TimedWord {
    /// The word's span.
    #[must_use]
    pub const fn span(&self) -> TimeSpanMs {
        TimeSpanMs {
            start_ms: self.start_ms,
            end_ms: self.end_ms,
        }
    }
}

/// How much of a word must fall inside reference silence to count.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SilencePolicy {
    /// Fraction of the word's duration that must overlap a silent span.
    ///
    /// A zero-duration word is judged by containment of its instant instead.
    pub min_overlap_ratio: f64,
}

impl Default for SilencePolicy {
    /// A word counts when the majority of its duration is inside silence,
    /// which tolerates ordinary boundary jitter without excusing a word that
    /// sits entirely in a silent passage.
    fn default() -> Self {
        Self {
            min_overlap_ratio: 0.5,
        }
    }
}

/// Hallucinated content in reference silence (spec 17.2, 8.6A).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SilenceHallucination {
    /// Total reference silence considered, in milliseconds.
    pub silent_ms: u64,
    /// Words the hypothesis places inside that silence.
    pub words_in_silence: usize,
    /// Words in the hypothesis overall.
    pub hypothesis_words: usize,
    /// Silent spans containing at least one hypothesis word.
    pub spans_with_content: usize,
    /// Silent spans considered.
    pub spans: usize,
    /// The offending words, so a reviewer can see what was invented.
    pub offending_words: Vec<TimedWord>,
}

impl SilenceHallucination {
    /// Words per minute of reference silence, or `None` when no silence was
    /// marked in the reference.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn words_per_silent_minute(&self) -> Option<f64> {
        if self.silent_ms == 0 {
            return None;
        }
        Some(self.words_in_silence as f64 * 60_000.0 / self.silent_ms as f64)
    }

    /// Share of the hypothesis that falls in silence, or `None` when the
    /// hypothesis is empty.
    #[must_use]
    pub fn fraction_of_hypothesis_words(&self) -> Option<f64> {
        ratio(self.words_in_silence, self.hypothesis_words)
    }

    /// Pool another item's outcome into this one.
    pub fn absorb(&mut self, other: Self) {
        self.silent_ms += other.silent_ms;
        self.words_in_silence += other.words_in_silence;
        self.hypothesis_words += other.hypothesis_words;
        self.spans_with_content += other.spans_with_content;
        self.spans += other.spans;
        self.offending_words.extend(other.offending_words);
    }
}

/// Measure hypothesis words that fall inside reference silence.
///
/// The reference supplies the silent spans: using the engine's own output to
/// decide where silence is would be circular.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn silence_hallucination(
    silent_spans: &[TimeSpanMs],
    words: &[TimedWord],
    policy: SilencePolicy,
) -> SilenceHallucination {
    let mut offending = Vec::new();
    let mut spans_with_content = 0;
    for span in silent_spans {
        let mut hit = false;
        for word in words {
            let word_span = word.span();
            let inside = if word_span.duration_ms() == 0 {
                word.start_ms >= span.start_ms && word.start_ms < span.end_ms
            } else {
                let overlap = word_span.overlap_ms(*span) as f64;
                overlap / word_span.duration_ms() as f64 >= policy.min_overlap_ratio
            };
            if inside {
                hit = true;
                if !offending.contains(word) {
                    offending.push(word.clone());
                }
            }
        }
        if hit {
            spans_with_content += 1;
        }
    }
    SilenceHallucination {
        silent_ms: silent_spans.iter().map(|span| span.duration_ms()).sum(),
        words_in_silence: offending.len(),
        hypothesis_words: words.len(),
        spans_with_content,
        spans: silent_spans.len(),
        offending_words: offending,
    }
}

/// Timestamp error against reference segment boundaries (spec 17.2).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TimestampError {
    /// Mean absolute boundary error, in milliseconds.
    pub mean_abs_ms: f64,
    /// Median absolute boundary error, in milliseconds.
    pub median_abs_ms: f64,
    /// Largest absolute boundary error, in milliseconds.
    pub max_abs_ms: u64,
    /// Number of reference boundaries compared.
    pub compared_boundaries: usize,
}

impl TimestampError {
    /// Summarize a pooled set of absolute boundary errors.
    ///
    /// Returns `None` for an empty set: no comparison happened, and `0.0`
    /// would read as perfect alignment.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn from_errors(errors: &[u64]) -> Option<Self> {
        if errors.is_empty() {
            return None;
        }
        let mut sorted = errors.to_vec();
        sorted.sort_unstable();
        let total: u64 = sorted.iter().sum();
        let middle = sorted.len() / 2;
        let median = if sorted.len() % 2 == 0 {
            (sorted[middle - 1] as f64 + sorted[middle] as f64) / 2.0
        } else {
            sorted[middle] as f64
        };
        Some(Self {
            mean_abs_ms: total as f64 / sorted.len() as f64,
            median_abs_ms: median,
            max_abs_ms: *sorted.last().expect("non-empty"),
            compared_boundaries: sorted.len(),
        })
    }
}

/// Absolute boundary errors, or `None` when the hypothesis carries no usable
/// alignment.
///
/// Spec 8.6A forbids surfacing a seek position the engine never produced, and
/// the same rule applies to measurement: an `unavailable` alignment yields
/// `None`, never `0.0`. Each reference boundary is paired with its nearest
/// hypothesis boundary, so an engine that segments differently is not punished
/// for an off-by-one segment count.
#[must_use]
pub fn boundary_errors_ms(
    alignment: HypothesisAlignment,
    reference_boundaries: &[u64],
    hypothesis_boundaries: &[u64],
) -> Option<Vec<u64>> {
    if !alignment.carries_times() {
        return None;
    }
    if reference_boundaries.is_empty() || hypothesis_boundaries.is_empty() {
        return None;
    }
    Some(
        reference_boundaries
            .iter()
            .map(|reference| {
                hypothesis_boundaries
                    .iter()
                    .map(|hypothesis| reference.abs_diff(*hypothesis))
                    .min()
                    .expect("non-empty")
            })
            .collect(),
    )
}

/// Timestamp error for one item, or `None` when no alignment exists.
#[must_use]
pub fn timestamp_error(
    alignment: HypothesisAlignment,
    reference_boundaries: &[u64],
    hypothesis_boundaries: &[u64],
) -> Option<TimestampError> {
    let errors = boundary_errors_ms(alignment, reference_boundaries, hypothesis_boundaries)?;
    TimestampError::from_errors(&errors)
}

/// Runtime figures recorded by the process that produced a hypothesis.
///
/// These are inputs, not measurements this crate can make: the runner consumes
/// pre-produced transcripts and never executes an engine. A field the producer
/// did not record stays `None` and is published as "not recorded".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeMeasurement {
    /// Length of the audio that was transcribed, in milliseconds.
    pub audio_duration_ms: u64,
    /// Wall-clock time the transcription took, in milliseconds.
    pub wall_clock_ms: u64,
    /// Time from job start to the first emitted result, in milliseconds.
    #[serde(default)]
    pub time_to_first_result_ms: Option<u64>,
    /// Peak resident set size observed during the run, in bytes.
    #[serde(default)]
    pub peak_rss_bytes: Option<u64>,
    /// Metered provider cost in micro-USD, for cloud engines only.
    #[serde(default)]
    pub metered_cost_micro_usd: Option<u64>,
}

impl RuntimeMeasurement {
    /// Real-time factor: wall-clock time divided by audio duration.
    ///
    /// Below `1.0` is faster than real time. `None` when no audio duration was
    /// recorded, because the ratio would be undefined.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn real_time_factor(&self) -> Option<f64> {
        if self.audio_duration_ms == 0 {
            return None;
        }
        Some(self.wall_clock_ms as f64 / self.audio_duration_ms as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(left: f64, right: f64) {
        assert!(
            (left - right).abs() < 1e-9,
            "expected {right}, measured {left}"
        );
    }

    #[test]
    fn normalization_lowercases_and_strips_edge_punctuation_only() {
        assert_eq!(
            normalize_tokens("The  contractor, poured 25 cubic yards."),
            vec!["the", "contractor", "poured", "25", "cubic", "yards"]
        );
        assert_eq!(normalize_tokens("isn't 2026-03-14!"), vec!["isn't", "2026-03-14"]);
        assert_eq!(normalize_tokens("(\"quoted\")"), vec!["quoted"]);
    }

    #[test]
    fn normalization_keeps_currency_symbols_at_the_token_edge() {
        assert_eq!(normalize_tokens("$1,250.00."), vec!["$1,250.00"]);
        assert_eq!(normalize_tokens("12%"), vec!["12%"]);
    }

    #[test]
    fn normalization_leaves_number_spelling_alone() {
        assert_eq!(normalize_tokens("twenty five"), vec!["twenty", "five"]);
        assert_eq!(normalize_tokens("25"), vec!["25"]);
    }

    #[test]
    fn an_identical_pair_has_no_edits() {
        let counts = word_error_rate("the slab cured overnight", "The slab cured overnight.");
        assert_eq!(counts.edits(), 0);
        assert_eq!(counts.reference_length, 4);
        close(counts.rate().expect("rate"), 0.0);
    }

    #[test]
    fn word_error_rate_matches_the_hand_computed_pair() {
        // reference: 10 tokens.
        // hypothesis splits "contractor" into "contract or" (1 substitution +
        // 1 insertion) and says "yard" for "yards" (1 substitution).
        let reference = "The contractor poured 25 cubic yards of concrete on 2026-03-14.";
        let hypothesis = "The contract or poured 25 cubic yard of concrete on 2026-03-14";
        let counts = word_error_rate(reference, hypothesis);
        assert_eq!(counts.reference_length, 10);
        assert_eq!(counts.substitutions, 2);
        assert_eq!(counts.insertions, 1);
        assert_eq!(counts.deletions, 0);
        close(counts.rate().expect("rate"), 0.3);
    }

    #[test]
    fn word_error_rate_counts_a_dropped_word_as_a_deletion() {
        let counts = word_error_rate("pour the slab today", "pour slab today");
        assert_eq!(counts.deletions, 1);
        assert_eq!(counts.substitutions, 0);
        assert_eq!(counts.insertions, 0);
        close(counts.rate().expect("rate"), 0.25);
    }

    #[test]
    fn character_error_rate_matches_the_hand_computed_pair() {
        // normalized reference: "invoice 1,250.00 usd" = 20 characters.
        // the hypothesis drops the group separator: exactly one deletion.
        let counts = character_error_rate("Invoice 1,250.00 USD", "Invoice 1250.00 USD");
        assert_eq!(counts.reference_length, 20);
        assert_eq!(counts.deletions, 1);
        assert_eq!(counts.substitutions, 0);
        assert_eq!(counts.insertions, 0);
        close(counts.rate().expect("rate"), 0.05);
    }

    #[test]
    fn character_error_rate_counts_a_single_substitution() {
        let counts = character_error_rate("cat", "cot");
        assert_eq!(counts.substitutions, 1);
        assert_eq!(counts.reference_length, 3);
        close(counts.rate().expect("rate"), 1.0 / 3.0);
    }

    #[test]
    fn an_empty_reference_has_no_rate_rather_than_a_perfect_one() {
        let counts = word_error_rate("", "invented words appear");
        assert_eq!(counts.insertions, 3);
        assert_eq!(counts.reference_length, 0);
        assert!(counts.rate().is_none());
    }

    #[test]
    fn edit_counts_pool_over_items() {
        let mut pooled = word_error_rate("pour the slab today", "pour slab today");
        pooled.absorb(word_error_rate("cat", "cot"));
        assert_eq!(pooled.reference_length, 5);
        assert_eq!(pooled.edits(), 2);
        close(pooled.rate().expect("rate"), 0.4);
    }

    #[test]
    fn domain_terms_are_scored_by_recall_including_multiword_terms() {
        let reference = "The slump test failed, so we ordered more rebar and shotcrete.";
        let hypothesis = "The slump test failed so we ordered more rebar and show crete.";
        let terms = vec![
            "slump test".to_owned(),
            "rebar".to_owned(),
            "shotcrete".to_owned(),
        ];
        let accuracy = domain_term_accuracy(reference, hypothesis, &terms);
        assert_eq!(accuracy.expected(), 3);
        assert_eq!(accuracy.matched(), 2);
        close(accuracy.recall().expect("recall"), 2.0 / 3.0);
        let shotcrete = accuracy
            .terms
            .iter()
            .find(|outcome| outcome.term == "shotcrete")
            .expect("term");
        assert_eq!(shotcrete.hypothesis_occurrences, 0);
        close(shotcrete.recall().expect("recall"), 0.0);
    }

    #[test]
    fn a_term_absent_from_the_reference_is_excluded_not_scored_as_a_miss() {
        let accuracy = domain_term_accuracy("we poured concrete", "we poured concrete", &[
            "concrete".to_owned(),
            "caisson".to_owned(),
        ]);
        assert_eq!(accuracy.absent_from_reference, vec!["caisson".to_owned()]);
        assert_eq!(accuracy.expected(), 1);
        close(accuracy.recall().expect("recall"), 1.0);
    }

    #[test]
    fn numeric_entities_are_classified_by_shape_and_neighbour() {
        let entities = extract_numeric_entities(
            "On 2026-03-14 we poured 25 cubic yards for $1,250.00 and 300 USD of fuel.",
        );
        assert_eq!(
            entities,
            vec![
                NumericEntity {
                    text: "2026-03-14".to_owned(),
                    kind: NumericKind::Date
                },
                NumericEntity {
                    text: "25".to_owned(),
                    kind: NumericKind::Number
                },
                NumericEntity {
                    text: "$1250.00".to_owned(),
                    kind: NumericKind::Amount
                },
                NumericEntity {
                    text: "300".to_owned(),
                    kind: NumericKind::Amount
                },
            ]
        );
    }

    #[test]
    fn a_day_next_to_a_month_name_is_a_date() {
        let entities = extract_numeric_entities("due on March 14 at the latest");
        assert_eq!(entities[0].kind, NumericKind::Date);
    }

    #[test]
    fn numeric_accuracy_reports_recall_and_invented_numbers() {
        let reference = "we poured 25 yards on 2026-03-14 for $1,250.00";
        let hypothesis = "we poured 25 yards on 2026-03-04 for $1250.00 plus 99 extra";
        let accuracy = numeric_accuracy(reference, hypothesis);
        assert_eq!(accuracy.expected(), 3);
        assert_eq!(accuracy.matched(), 2);
        close(accuracy.recall().expect("recall"), 2.0 / 3.0);
        assert_eq!(accuracy.per_kind[&NumericKind::Date].matched, 0);
        assert_eq!(accuracy.per_kind[&NumericKind::Amount].matched, 1);
        assert_eq!(
            accuracy.invented,
            vec![
                NumericEntity {
                    text: "2026-03-04".to_owned(),
                    kind: NumericKind::Date
                },
                NumericEntity {
                    text: "99".to_owned(),
                    kind: NumericKind::Number
                }
            ]
        );
    }

    #[test]
    fn a_reference_without_numbers_has_no_numeric_recall() {
        let accuracy = numeric_accuracy("no figures here", "no figures here");
        assert!(accuracy.recall().is_none());
    }

    fn word(text: &str, start_ms: u64, end_ms: u64) -> TimedWord {
        TimedWord {
            text: text.to_owned(),
            start_ms,
            end_ms,
        }
    }

    #[test]
    fn words_inside_reference_silence_are_counted_as_hallucinations() {
        let silence = [TimeSpanMs {
            start_ms: 5_000,
            end_ms: 20_000,
        }];
        let words = vec![
            word("pour", 1_000, 1_400),
            word("thank", 6_000, 6_400),
            word("you", 6_400, 6_800),
            word("for", 6_800, 7_000),
            word("watching", 7_000, 7_600),
            word("next", 21_000, 21_500),
        ];
        let outcome = silence_hallucination(&silence, &words, SilencePolicy::default());
        assert_eq!(outcome.words_in_silence, 4);
        assert_eq!(outcome.hypothesis_words, 6);
        assert_eq!(outcome.spans_with_content, 1);
        assert_eq!(outcome.silent_ms, 15_000);
        close(outcome.words_per_silent_minute().expect("rate"), 16.0);
        close(
            outcome.fraction_of_hypothesis_words().expect("fraction"),
            4.0 / 6.0,
        );
        assert_eq!(outcome.offending_words[0].text, "thank");
    }

    #[test]
    fn a_word_straddling_a_boundary_counts_only_on_majority_overlap() {
        let silence = [TimeSpanMs {
            start_ms: 1_000,
            end_ms: 2_000,
        }];
        let mostly_speech = vec![word("edge", 800, 1_100)];
        let mostly_silence = vec![word("edge", 900, 1_400)];
        assert_eq!(
            silence_hallucination(&silence, &mostly_speech, SilencePolicy::default())
                .words_in_silence,
            0
        );
        assert_eq!(
            silence_hallucination(&silence, &mostly_silence, SilencePolicy::default())
                .words_in_silence,
            1
        );
    }

    #[test]
    fn a_clean_hypothesis_over_silence_scores_zero_not_none() {
        let silence = [TimeSpanMs {
            start_ms: 5_000,
            end_ms: 11_000,
        }];
        let outcome =
            silence_hallucination(&silence, &[word("pour", 100, 500)], SilencePolicy::default());
        assert_eq!(outcome.words_in_silence, 0);
        close(outcome.words_per_silent_minute().expect("rate"), 0.0);
    }

    #[test]
    fn a_reference_without_silence_has_no_silence_rate() {
        let outcome = silence_hallucination(&[], &[word("pour", 100, 500)], SilencePolicy::default());
        assert!(outcome.words_per_silent_minute().is_none());
    }

    #[test]
    fn timestamp_error_is_none_for_an_unavailable_alignment() {
        assert!(timestamp_error(
            HypothesisAlignment::Unavailable,
            &[1_000, 2_000],
            &[1_020, 2_050]
        )
        .is_none());
        assert!(boundary_errors_ms(
            HypothesisAlignment::Unavailable,
            &[1_000],
            &[1_000]
        )
        .is_none());
    }

    #[test]
    fn timestamp_error_summarizes_native_and_derived_alignments() {
        for alignment in [HypothesisAlignment::Native, HypothesisAlignment::Derived] {
            let error = timestamp_error(alignment, &[1_000, 2_000, 3_000], &[1_020, 2_050, 3_300])
                .expect("alignment carries times");
            assert_eq!(error.compared_boundaries, 3);
            assert_eq!(error.max_abs_ms, 300);
            close(error.mean_abs_ms, 370.0 / 3.0);
            close(error.median_abs_ms, 50.0);
        }
    }

    #[test]
    fn timestamp_error_is_none_when_nothing_was_compared() {
        assert!(timestamp_error(HypothesisAlignment::Native, &[], &[10]).is_none());
        assert!(TimestampError::from_errors(&[]).is_none());
    }

    #[test]
    fn the_median_of_an_even_sample_is_the_midpoint() {
        let error = TimestampError::from_errors(&[10, 20, 30, 40]).expect("errors");
        close(error.median_abs_ms, 25.0);
        close(error.mean_abs_ms, 25.0);
    }

    #[test]
    fn the_real_time_factor_comes_from_recorded_figures_only() {
        let measurement = RuntimeMeasurement {
            audio_duration_ms: 60_000,
            wall_clock_ms: 15_000,
            time_to_first_result_ms: Some(2_000),
            peak_rss_bytes: Some(512 * 1024 * 1024),
            metered_cost_micro_usd: None,
        };
        close(measurement.real_time_factor().expect("rtf"), 0.25);

        let unmeasured = RuntimeMeasurement {
            audio_duration_ms: 0,
            wall_clock_ms: 15_000,
            time_to_first_result_ms: None,
            peak_rss_bytes: None,
            metered_cost_micro_usd: None,
        };
        assert!(unmeasured.real_time_factor().is_none());
    }
}
