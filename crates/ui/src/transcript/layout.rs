//! Virtualization: row heights, scroll offsets and the galley cache.
//!
//! A three-hour transcript is 30,000 segments. Laying out all of them once per
//! frame is not a constant factor away from workable, so only the rows inside
//! the viewport are laid out, and their galleys are cached until the segment,
//! the wrap width or the search highlight changes.
//!
//! Heights of rows that have never been laid out are *estimated* from the
//! character count, then replaced by the measured height the first time the
//! row is drawn. The estimate only has to be good enough for the scrollbar to
//! settle; the measured height is what the viewport calculation uses once the
//! row has been seen.

use std::collections::HashMap;
use std::sync::Arc;

use egui::text::LayoutJob;
use egui::{Galley, TextFormat};
use plaudon_transcript_doc::doc::{SegmentId, TranscriptDocument};
use plaudon_transcript_doc::search::SearchMatch;

use super::style::TranscriptStyle;

/// Row geometry for one open document.
#[derive(Debug, Default, Clone)]
pub struct RowMetrics {
    heights: Vec<f32>,
    measured: Vec<bool>,
    offsets: Vec<f32>,
    wrap_width: f32,
    line_height: f32,
    dirty: bool,
}

impl RowMetrics {
    /// Prepare the metrics for a document at a given wrap width.
    ///
    /// Cheap when nothing changed; rebuilds the estimates when the width or
    /// the segment count changes.
    pub fn prepare(
        &mut self,
        document: &TranscriptDocument,
        wrap_width: f32,
        line_height: f32,
        average_char_width: f32,
        row_overhead: f32,
    ) {
        let width_changed = (self.wrap_width - wrap_width).abs() > 0.5;
        let height_changed = (self.line_height - line_height).abs() > 0.5;
        if self.heights.len() != document.len() || width_changed || height_changed {
            self.wrap_width = wrap_width;
            self.line_height = line_height;
            self.heights.clear();
            self.measured.clear();
            self.heights.reserve(document.len());
            self.measured.resize(document.len(), false);
            let per_line = (wrap_width / average_char_width.max(1.0)).max(1.0);
            for segment in document.segments() {
                let lines = (segment.char_len() as f32 / per_line).ceil().max(1.0);
                self.heights.push(lines * line_height + row_overhead);
            }
            self.dirty = true;
        }
        self.rebuild_offsets();
    }

    /// Record the height a row actually had once it was laid out.
    pub fn measure(&mut self, index: usize, height: f32) {
        let Some(slot) = self.heights.get_mut(index) else {
            return;
        };
        if (*slot - height).abs() > 0.5 || !self.measured[index] {
            *slot = height;
            self.measured[index] = true;
            self.dirty = true;
        }
    }

    /// Whether offsets changed since the last frame, so the caller can request
    /// another repaint before the scrollbar is trusted.
    #[must_use]
    pub const fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Total height of every row.
    #[must_use]
    pub fn total_height(&self) -> f32 {
        self.offsets.last().copied().unwrap_or(0.0)
    }

    /// Offset of a row from the top of the list.
    #[must_use]
    pub fn offset(&self, index: usize) -> f32 {
        self.offsets.get(index).copied().unwrap_or(0.0)
    }

    /// Height of a row.
    #[must_use]
    pub fn height(&self, index: usize) -> f32 {
        self.heights.get(index).copied().unwrap_or(self.line_height)
    }

    /// Rows intersecting `[top, bottom]`, as an inclusive-exclusive range.
    #[must_use]
    pub fn visible_range(&self, top: f32, bottom: f32) -> std::ops::Range<usize> {
        if self.heights.is_empty() {
            return 0..0;
        }
        let first = self
            .offsets
            .partition_point(|offset| *offset <= top)
            .saturating_sub(1);
        let last = self.offsets.partition_point(|offset| *offset < bottom);
        first..last.min(self.heights.len())
    }

    fn rebuild_offsets(&mut self) {
        if !self.dirty && self.offsets.len() == self.heights.len() + 1 {
            return;
        }
        self.offsets.clear();
        self.offsets.reserve(self.heights.len() + 1);
        let mut cursor = 0.0;
        self.offsets.push(cursor);
        for height in &self.heights {
            cursor += height;
            self.offsets.push(cursor);
        }
        self.dirty = false;
    }
}

/// Identity of a laid-out row, so a cached galley is only reused when every
/// input that produced it is unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GalleyKey {
    version: u64,
    wrap_width: u32,
    highlight: u64,
}

/// Cached galleys, keyed by segment.
#[derive(Debug, Default)]
pub struct GalleyCache {
    entries: HashMap<SegmentId, (GalleyKey, Arc<Galley>)>,
    used: Vec<SegmentId>,
}

impl GalleyCache {
    /// Lay out one segment, reusing the cached galley when nothing changed.
    pub fn galley(
        &mut self,
        ui: &egui::Ui,
        style: &TranscriptStyle,
        segment: &plaudon_transcript_doc::doc::Segment,
        wrap_width: f32,
        matches: &[SearchMatch],
        current_match: Option<SearchMatch>,
    ) -> Arc<Galley> {
        let key = GalleyKey {
            version: segment.version(),
            wrap_width: wrap_width.to_bits(),
            highlight: highlight_signature(matches, current_match),
        };
        self.used.push(segment.id);
        if let Some((cached_key, galley)) = self.entries.get(&segment.id) {
            if *cached_key == key {
                return Arc::clone(galley);
            }
        }
        let job = layout_job(style, segment, wrap_width, matches, current_match);
        let galley = ui.painter().layout_job(job);
        self.entries.insert(segment.id, (key, Arc::clone(&galley)));
        galley
    }

    /// A galley laid out earlier this frame, for hit testing and caret motion.
    #[must_use]
    pub fn get(&self, segment: SegmentId) -> Option<Arc<Galley>> {
        self.entries
            .get(&segment)
            .map(|(_, galley)| Arc::clone(galley))
    }

    /// Drop galleys for rows that were not drawn this frame.
    ///
    /// Without this a long scroll through a 30,000-segment transcript would
    /// retain every galley it ever laid out.
    pub fn end_frame(&mut self) {
        if self.entries.len() <= self.used.len() * 2 {
            self.used.clear();
            return;
        }
        let used: std::collections::HashSet<SegmentId> = self.used.drain(..).collect();
        self.entries.retain(|segment, _| used.contains(segment));
    }

    /// Number of cached galleys, for tests.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

fn highlight_signature(matches: &[SearchMatch], current: Option<SearchMatch>) -> u64 {
    // A cheap order-sensitive digest; a collision only costs a stale
    // highlight for one frame, never a wrong edit.
    let mut digest = 1_469_598_103_934_665_603u64;
    for hit in matches {
        for value in [hit.start as u64, hit.end as u64] {
            digest ^= value;
            digest = digest.wrapping_mul(1_099_511_628_211);
        }
    }
    if let Some(hit) = current {
        digest ^= (hit.start as u64) << 1 ^ (hit.end as u64);
        digest = digest.wrapping_mul(1_099_511_628_211);
    }
    digest
}

fn layout_job(
    style: &TranscriptStyle,
    segment: &plaudon_transcript_doc::doc::Segment,
    wrap_width: f32,
    matches: &[SearchMatch],
    current_match: Option<SearchMatch>,
) -> LayoutJob {
    let text = segment.text();
    let mut job = LayoutJob::default();
    job.wrap.max_width = wrap_width;
    let plain = TextFormat {
        font_id: style.font.clone(),
        color: style.text_color,
        ..TextFormat::default()
    };

    let mut cursor = 0usize;
    let chars: Vec<char> = text.chars().collect();
    for hit in matches {
        let start = hit.start.min(chars.len());
        let end = hit.end.min(chars.len());
        if start > cursor {
            job.append(
                &chars[cursor..start].iter().collect::<String>(),
                0.0,
                plain.clone(),
            );
        }
        let is_current = current_match
            .is_some_and(|current| current.segment == hit.segment && current.start == hit.start);
        job.append(
            &chars[start..end].iter().collect::<String>(),
            0.0,
            TextFormat {
                background: if is_current {
                    style.current_match_background
                } else {
                    style.match_background
                },
                ..plain.clone()
            },
        );
        cursor = end;
    }
    if cursor < chars.len() {
        job.append(&chars[cursor..].iter().collect::<String>(), 0.0, plain);
    } else if chars.is_empty() {
        // An emptied segment still needs a row with a caret in it.
        job.append("", 0.0, plain);
    }
    job
}
