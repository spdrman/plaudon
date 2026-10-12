//! Selection, caret and hit-testing geometry over a laid-out segment.
//!
//! egui's own `paint_text_selection` mutates the galley, which would defeat
//! the galley cache (every frame would clone a shared galley). The editor
//! therefore paints selection rectangles underneath the cached text instead.

use egui::text::CCursor;
use egui::{Galley, Pos2, Rect, Vec2};

/// Rectangles covering `[start, end)` of a galley, one per wrapped row.
#[must_use]
pub fn selection_rects(galley: &Galley, origin: Pos2, start: usize, end: usize) -> Vec<Rect> {
    if end <= start {
        return Vec::new();
    }
    let mut rects = Vec::new();
    let mut row_start = 0usize;
    for placed in &galley.rows {
        let row_chars = placed.char_count_including_newline().0;
        let row_end = row_start + row_chars;
        let from = start.max(row_start);
        let to = end.min(row_end);
        if from < to {
            let left = x_at(placed, from - row_start);
            let right = x_at(placed, to - row_start);
            rects.push(Rect::from_min_max(
                origin + Vec2::new(left, placed.pos.y),
                origin + Vec2::new(right.max(left + 1.0), placed.pos.y + placed.size.y),
            ));
        }
        row_start = row_end;
        if row_start >= end {
            break;
        }
    }
    rects
}

/// The thin rectangle the caret occupies.
#[must_use]
pub fn caret_rect(galley: &Galley, origin: Pos2, offset: usize) -> Rect {
    galley
        .pos_from_cursor(CCursor::new(offset))
        .translate(origin.to_vec2())
}

/// The `char` offset nearest a screen position.
#[must_use]
pub fn offset_at(galley: &Galley, origin: Pos2, position: Pos2) -> usize {
    galley.cursor_from_pos(position - origin).index.0
}

/// The offset one wrapped row above `offset`, or `None` when the caret is on
/// the first row of the segment and should move to the previous segment.
#[must_use]
pub fn offset_row_above(galley: &Galley, offset: usize) -> Option<usize> {
    let cursor = CCursor::new(offset);
    if galley.rows.len() < 2 {
        return None;
    }
    let (moved, _) = galley.cursor_up_one_row(&cursor, None);
    (moved.index.0 != offset || offset != 0).then_some(moved.index.0)
}

/// The offset one wrapped row below `offset`, or `None` when the caret is on
/// the last row of the segment.
#[must_use]
pub fn offset_row_below(galley: &Galley, offset: usize) -> Option<usize> {
    let cursor = CCursor::new(offset);
    if galley.rows.len() < 2 {
        return None;
    }
    let (moved, _) = galley.cursor_down_one_row(&cursor, None);
    (moved.index.0 != offset).then_some(moved.index.0)
}

fn x_at(placed: &egui::epaint::text::PlacedRow, local: usize) -> f32 {
    placed.glyphs.get(local).map_or_else(
        || {
            placed.glyphs.last().map_or(placed.pos.x, |glyph| {
                placed.pos.x + glyph.pos.x + glyph.advance_width
            })
        },
        |glyph| placed.pos.x + glyph.pos.x,
    )
}
