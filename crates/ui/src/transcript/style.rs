//! Metrics and colours for the transcript editor.
//!
//! Every colour is derived from the active [`egui::Visuals`], so the editor
//! follows whatever theme the shell installs and this module never becomes a
//! second, competing palette.

use egui::{Color32, FontId, TextStyle, Ui, Vec2};

/// Metrics and derived colours for one transcript editor.
#[derive(Debug, Clone)]
pub struct TranscriptStyle {
    /// Width of the timestamp gutter.
    pub gutter_width: f32,
    /// Width of the speaker column.
    pub speaker_width: f32,
    /// Padding inside a segment row.
    pub row_padding: Vec2,
    /// Vertical gap between rows.
    pub row_spacing: f32,
    /// Font used for segment text.
    pub font: FontId,
    /// Colour of segment text.
    pub text_color: Color32,
    /// Background of a search match.
    pub match_background: Color32,
    /// Background of the current search match.
    pub current_match_background: Color32,
    /// Background of the selection.
    pub selection_background: Color32,
    /// Background of the segment under the playhead.
    pub playback_background: Color32,
    /// Colour of the caret.
    pub caret_color: Color32,
    /// Colour of the "edited" marker.
    pub edited_color: Color32,
}

impl TranscriptStyle {
    /// Derive the style from the current `ui`.
    #[must_use]
    pub fn from_ui(ui: &Ui) -> Self {
        let visuals = ui.visuals();
        let font = TextStyle::Body.resolve(ui.style());
        Self {
            gutter_width: 64.0,
            speaker_width: 132.0,
            row_padding: Vec2::new(8.0, 4.0),
            row_spacing: 4.0,
            text_color: visuals.text_color(),
            match_background: visuals.selection.bg_fill.gamma_multiply(0.4),
            current_match_background: visuals.selection.bg_fill.gamma_multiply(0.8),
            selection_background: visuals.selection.bg_fill,
            playback_background: visuals.faint_bg_color,
            caret_color: visuals.text_cursor.stroke.color,
            edited_color: visuals.widgets.active.fg_stroke.color,
            font,
        }
    }

    /// Width available for segment text at a given row width.
    #[must_use]
    pub fn wrap_width(&self, row_width: f32) -> f32 {
        (row_width - self.gutter_width - self.speaker_width - self.row_padding.x * 2.0).max(32.0)
    }
}
