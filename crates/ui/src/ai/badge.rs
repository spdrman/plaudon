//! Status badges.
//!
//! §5.3: status is never communicated by colour alone. A badge therefore
//! always paints its glyph **and** its label as text, and the colour only
//! reinforces them. That is also what makes the badge legible to AccessKit: a
//! screen reader reads "Not verified", not "orange".

use egui::{Color32, Response, RichText, Ui};
use plaudon_app::tone::Tone;

/// The colour a tone takes in the current theme.
///
/// Derived from the active visuals so that light, dark and high-contrast
/// themes all stay legible without this module owning a palette.
#[must_use]
pub fn tone_color(ui: &Ui, tone: Tone) -> Color32 {
    let visuals = ui.visuals();
    match tone {
        Tone::Neutral => visuals.weak_text_color(),
        Tone::Running => visuals.strong_text_color(),
        Tone::Waiting | Tone::Warning => visuals.warn_fg_color,
        Tone::Success => {
            if visuals.dark_mode {
                Color32::from_rgb(0x4a, 0xc0, 0x7a)
            } else {
                Color32::from_rgb(0x12, 0x6b, 0x3f)
            }
        }
        Tone::Danger => visuals.error_fg_color,
    }
}

/// A glyph plus a label, optionally with a sentence under it.
#[derive(Debug, Clone)]
pub struct Badge<'a> {
    glyph: &'a str,
    label: &'a str,
    tone: Tone,
    description: Option<&'a str>,
}

impl<'a> Badge<'a> {
    /// A badge with a glyph, a label and a tone.
    #[must_use]
    pub fn new(glyph: &'a str, label: &'a str, tone: Tone) -> Self {
        Self {
            glyph,
            label,
            tone,
            description: None,
        }
    }

    /// Add the sentence that explains the state.
    #[must_use]
    pub fn description(mut self, description: &'a str) -> Self {
        self.description = Some(description);
        self
    }

    /// Paint the badge. The returned response carries the accessible label,
    /// which is the glyph and the label together — the same two things a
    /// sighted user sees.
    pub fn ui(self, ui: &mut Ui) -> Response {
        let color = tone_color(ui, self.tone);
        let text = format!("{} {}", self.glyph, self.label);
        let mut response = ui.label(RichText::new(text).color(color).strong());
        if let Some(description) = self.description {
            response = response.union(ui.label(RichText::new(description).color(color)));
        }
        response
    }
}
