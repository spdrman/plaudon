//! The status badge both destinations use.
//!
//! A state is always a glyph *and* a word. Colour is an extra, never the
//! signal: the cross-cutting invariant "status is never communicated by colour
//! alone" (plan §4.9) is satisfied by the glyph and the label, so the badge
//! still reads correctly in a high-contrast theme or a greyscale screenshot.

use egui::{Color32, Ui};

use super::presentation::{Badge, Tone};

/// Colour of a tone, taken from the theme's own palette.
///
/// Only the two severities egui defines a contrast-checked colour for get one;
/// everything else uses the ordinary text colour, because inventing a palette
/// here would be inventing a contrast claim O1.3 owns.
fn tone_color(ui: &Ui, tone: Tone) -> Color32 {
    match tone {
        Tone::Warning => ui.visuals().warn_fg_color,
        Tone::Danger => ui.visuals().error_fg_color,
        Tone::Neutral | Tone::Running | Tone::Waiting | Tone::Success => ui.visuals().text_color(),
    }
}

/// Render `label` with its glyph.
pub fn badge_ui(ui: &mut Ui, badge: Badge, label: &str) {
    let color = tone_color(ui, badge.tone);
    ui.horizontal(|ui| {
        ui.colored_label(color, badge.glyph);
        ui.colored_label(color, label);
    });
}

/// Render `label` with its glyph on the current row, without opening a layout.
pub fn badge_inline(ui: &mut Ui, badge: Badge, label: &str) {
    let color = tone_color(ui, badge.tone);
    ui.colored_label(color, format!("{} {label}", badge.glyph));
}
