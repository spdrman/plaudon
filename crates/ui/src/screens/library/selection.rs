//! The grid's selection and focus model, with no egui in it.
//!
//! The React grid kept three pieces of state — the roving tab index, the
//! shift-range anchor and the selected set — and the rules that relate them
//! are the part users notice: Shift+Down extends from the anchor rather than
//! from the cursor, a row that disappears from the loaded set must not stay
//! selected, and the live region has to announce a count against the number
//! of rows actually loaded, not against the total the backend reported.

use std::collections::BTreeSet;

use plaudon_app::dto::RecordingSummary;
use plaudon_domain::ids::RecordingId;

/// Selected recordings plus the keyboard cursor over the loaded rows.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Selection {
    selected: BTreeSet<RecordingId>,
    anchor: usize,
    active: usize,
}

/// Identities covered by a range selection between two row indices.
#[must_use]
pub fn range_ids(rows: &[RecordingSummary], anchor: usize, focus: usize) -> Vec<RecordingId> {
    if rows.is_empty() {
        return Vec::new();
    }
    let (start, end) = if anchor <= focus {
        (anchor, focus)
    } else {
        (focus, anchor)
    };
    rows.iter()
        .skip(start)
        .take(end.saturating_sub(start) + 1)
        .map(|row| row.recording_id)
        .collect()
}

impl Selection {
    /// Row the keyboard cursor is on. Always a valid index while rows exist,
    /// because [`Selection::reconcile`] clamps it when the list shrinks.
    #[must_use]
    pub const fn active(&self) -> usize {
        self.active
    }

    /// Row a Shift range extends from.
    #[must_use]
    pub const fn anchor(&self) -> usize {
        self.anchor
    }

    /// How many recordings are selected.
    #[must_use]
    pub fn len(&self) -> usize {
        self.selected.len()
    }

    /// Whether nothing is selected.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.selected.is_empty()
    }

    /// Whether this recording is selected.
    #[must_use]
    pub fn contains(&self, recording_id: RecordingId) -> bool {
        self.selected.contains(&recording_id)
    }

    /// The selected identities, in identity order so batch operations are
    /// dispatched deterministically.
    #[must_use]
    pub fn ids(&self) -> Vec<RecordingId> {
        self.selected.iter().copied().collect()
    }

    /// Move the keyboard cursor, optionally extending the selection from the
    /// anchor. Moving without extending moves the anchor with the cursor.
    pub fn move_to(&mut self, index: usize, extend: bool, rows: &[RecordingSummary]) {
        if rows.is_empty() {
            return;
        }
        let target = index.min(rows.len() - 1);
        if extend {
            self.selected.extend(range_ids(rows, self.anchor, target));
        } else {
            self.anchor = target;
        }
        self.active = target;
    }

    /// Move the cursor one row up, saturating at the first row.
    pub fn move_up(&mut self, extend: bool, rows: &[RecordingSummary]) {
        self.move_to(self.active.saturating_sub(1), extend, rows);
    }

    /// Move the cursor one row down, saturating at the last row.
    pub fn move_down(&mut self, extend: bool, rows: &[RecordingSummary]) {
        self.move_to(self.active.saturating_add(1), extend, rows);
    }

    /// Put the cursor on `index` without touching the selection.
    pub fn focus(&mut self, index: usize) {
        self.active = index;
    }

    /// Add or remove one recording.
    pub fn toggle(&mut self, recording_id: RecordingId) {
        if !self.selected.remove(&recording_id) {
            self.selected.insert(recording_id);
        }
    }

    /// Replace the selection with exactly this recording.
    pub fn select_only(&mut self, recording_id: RecordingId) {
        self.selected.clear();
        self.selected.insert(recording_id);
    }

    /// Select every loaded row. The grid cannot select rows it has not
    /// loaded, and says so in the announcement.
    pub fn select_all(&mut self, rows: &[RecordingSummary]) {
        self.selected = rows.iter().map(|row| row.recording_id).collect();
    }

    /// Deselect everything, leaving the cursor where it is.
    pub fn clear(&mut self) {
        self.selected.clear();
    }

    /// Drop selected recordings that are no longer loaded and clamp the
    /// cursor and anchor into the new row range.
    pub fn reconcile(&mut self, rows: &[RecordingSummary]) {
        let present: BTreeSet<RecordingId> = rows.iter().map(|row| row.recording_id).collect();
        self.selected.retain(|id| present.contains(id));
        let last = rows.len().saturating_sub(1);
        self.active = self.active.min(last);
        self.anchor = self.anchor.min(last);
    }

    /// What the live region announces.
    #[must_use]
    pub fn describe(&self, loaded: usize) -> String {
        if self.selected.is_empty() {
            format!("No recordings selected. {loaded} loaded.")
        } else {
            format!(
                "{} of {loaded} loaded recordings selected.",
                self.selected.len()
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screens::library::testing::rows;

    #[test]
    fn moving_without_extending_carries_the_anchor() {
        let rows = rows(5);
        let mut selection = Selection::default();
        selection.move_down(false, &rows);
        selection.move_down(false, &rows);
        assert_eq!(selection.active(), 2);
        assert_eq!(selection.anchor(), 2);
        assert!(
            selection.is_empty(),
            "expected selection to be empty, got {selection:?}"
        );
    }

    #[test]
    fn shift_extends_from_the_anchor_not_from_the_cursor() {
        let rows = rows(5);
        let mut selection = Selection::default();
        selection.move_to(1, false, &rows);
        selection.move_down(true, &rows);
        selection.move_down(true, &rows);
        assert_eq!(selection.len(), 3);
        for (index, row) in rows.iter().enumerate().take(4).skip(1) {
            assert!(selection.contains(row.recording_id), "row {index}");
        }
        assert!(!selection.contains(rows[0].recording_id));
        assert_eq!(
            selection.anchor(),
            1,
            "the anchor stays put while extending"
        );
    }

    #[test]
    fn the_cursor_saturates_at_both_ends() {
        let rows = rows(3);
        let mut selection = Selection::default();
        selection.move_up(false, &rows);
        assert_eq!(selection.active(), 0);
        selection.move_to(99, false, &rows);
        assert_eq!(selection.active(), 2);
    }

    #[test]
    fn an_empty_list_has_nothing_to_move_to() {
        let mut selection = Selection::default();
        selection.move_down(false, &[]);
        assert_eq!(selection.active(), 0);
    }

    #[test]
    fn toggling_adds_then_removes() {
        let rows = rows(2);
        let mut selection = Selection::default();
        selection.toggle(rows[0].recording_id);
        assert!(selection.contains(rows[0].recording_id));
        selection.toggle(rows[0].recording_id);
        assert!(
            selection.is_empty(),
            "expected selection to be empty, got {selection:?}"
        );
    }

    #[test]
    fn select_all_covers_the_loaded_rows_only() {
        let rows = rows(4);
        let mut selection = Selection::default();
        selection.select_all(&rows);
        assert_eq!(selection.len(), 4);
        assert_eq!(
            selection.describe(4),
            "4 of 4 loaded recordings selected.",
            "the announcement counts loaded rows, not the backend total"
        );
    }

    #[test]
    fn a_row_that_leaves_the_loaded_set_stops_being_selected() {
        let rows = rows(4);
        let mut selection = Selection::default();
        selection.select_all(&rows);
        selection.move_to(3, false, &rows);
        let shorter = rows[..2].to_vec();
        selection.reconcile(&shorter);
        assert_eq!(selection.len(), 2);
        assert_eq!(selection.active(), 1);
        assert_eq!(selection.anchor(), 1);
    }

    #[test]
    fn nothing_selected_is_announced_as_nothing_selected() {
        assert_eq!(
            Selection::default().describe(12),
            "No recordings selected. 12 loaded."
        );
    }

    #[test]
    fn a_range_is_inclusive_in_both_directions() {
        let rows = rows(5);
        assert_eq!(range_ids(&rows, 1, 3).len(), 3);
        assert_eq!(range_ids(&rows, 3, 1), range_ids(&rows, 1, 3));
        assert_eq!(range_ids(&rows, 2, 2).len(), 1);
        assert!(
            range_ids(&[], 0, 3).is_empty(),
            "an empty list has no range"
        );
    }
}
