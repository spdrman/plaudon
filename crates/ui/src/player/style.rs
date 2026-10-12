//! Colours and sizes for the player views.
//!
//! Derived from `egui::Visuals` so the player is usable before the shell's
//! token system lands (EPIC O task O1.3), and every field is public so the
//! shell can hand over the real semantic roles without this module changing.

use egui::{Color32, Visuals};

/// Everything the player paints with.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerStyle {
    /// Waveform panel background.
    pub background: Color32,
    /// Waveform body before the playhead.
    pub played: Color32,
    /// Waveform body after the playhead.
    pub unplayed: Color32,
    /// Zero-amplitude line through the middle of the waveform.
    pub center_line: Color32,
    /// The playhead itself.
    pub playhead: Color32,
    /// The hovered-position marker.
    pub cursor: Color32,
    /// Ruler tick marks.
    pub tick: Color32,
    /// Ruler labels.
    pub label: Color32,
    /// Speaker band colours, cycled by speaker colour index.
    pub speakers: Vec<Color32>,
    /// Default waveform height, in points.
    pub waveform_height: f32,
    /// Ruler height, in points.
    pub ruler_height: f32,
    /// Height of the speaker band under the waveform, in points.
    pub speaker_band_height: f32,
}

impl PlayerStyle {
    /// A style matching the current egui theme.
    #[must_use]
    pub fn from_visuals(visuals: &Visuals) -> Self {
        let accent = visuals.selection.bg_fill;
        Self {
            background: visuals.extreme_bg_color,
            played: accent,
            unplayed: visuals.weak_text_color(),
            center_line: visuals.widgets.noninteractive.bg_stroke.color,
            playhead: visuals.strong_text_color(),
            cursor: visuals.widgets.hovered.fg_stroke.color,
            tick: visuals.widgets.noninteractive.fg_stroke.color,
            label: visuals.text_color(),
            speakers: speaker_palette(visuals.dark_mode),
            waveform_height: 96.0,
            ruler_height: 20.0,
            speaker_band_height: 8.0,
        }
    }

    /// The colour for a speaker's stable colour index.
    #[must_use]
    pub fn speaker(&self, index: usize) -> Color32 {
        if self.speakers.is_empty() {
            return self.unplayed;
        }
        self.speakers[index % self.speakers.len()]
    }
}

impl Default for PlayerStyle {
    fn default() -> Self {
        Self::from_visuals(&Visuals::dark())
    }
}

/// Six hues that stay distinguishable next to each other, and are never the
/// only carrier of meaning: every region also has a label.
fn speaker_palette(dark_mode: bool) -> Vec<Color32> {
    if dark_mode {
        vec![
            Color32::from_rgb(90, 140, 200),
            Color32::from_rgb(200, 140, 80),
            Color32::from_rgb(110, 180, 130),
            Color32::from_rgb(190, 120, 180),
            Color32::from_rgb(190, 180, 90),
            Color32::from_rgb(130, 130, 190),
        ]
    } else {
        vec![
            Color32::from_rgb(60, 105, 170),
            Color32::from_rgb(170, 105, 45),
            Color32::from_rgb(60, 135, 90),
            Color32::from_rgb(150, 80, 145),
            Color32::from_rgb(145, 130, 45),
            Color32::from_rgb(95, 95, 155),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_speaker_palette_cycles_and_never_panics() {
        let style = PlayerStyle::default();
        assert_eq!(style.speaker(0), style.speaker(style.speakers.len()));
        let bare = PlayerStyle {
            speakers: Vec::new(),
            ..PlayerStyle::default()
        };
        assert_eq!(bare.speaker(3), bare.unplayed);
    }

    #[test]
    fn light_and_dark_styles_differ() {
        let dark = PlayerStyle::from_visuals(&Visuals::dark());
        let light = PlayerStyle::from_visuals(&Visuals::light());
        assert_ne!(dark.background, light.background);
        assert_ne!(dark.speakers, light.speakers);
    }
}
