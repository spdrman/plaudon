//! The token seam.
//!
//! The design tokens belong to the shell (`plaudon_ui::theme::Tokens`, task
//! O1.3). The settings screens must not re-derive them and must not hard-code
//! a colour, so they render through this trait: one small set of roles, which
//! the shell's token struct satisfies and which a test can satisfy without the
//! shell being present.
//!
//! [`VisualsTokens`] is the standing implementation over `egui::Visuals`. It
//! is not a placeholder: `Visuals` is the theme the shell installed, so a
//! section rendered through it follows light, dark and high-contrast correctly
//! on its own. The shell's own `Tokens` carry the full 20-role palette and
//! take over where a role is more specific than anything `Visuals` has.

use egui::{Color32, Ui};

/// The colour roles the settings sections use.
pub trait SettingsTokens {
    /// Background of a settings card.
    fn surface(&self) -> Color32;
    /// Primary text.
    fn text(&self) -> Color32;
    /// Secondary text: notes, hints, facts.
    fn muted_text(&self) -> Color32;
    /// Hairline between rows and around cards.
    fn border(&self) -> Color32;
    /// Primary action.
    fn accent(&self) -> Color32;
    /// Destructive action and failure status.
    fn danger(&self) -> Color32;
    /// A caution that is not a failure.
    fn warning(&self) -> Color32;
    /// A confirmed, healthy state.
    fn success(&self) -> Color32;
}

/// Tokens derived from the theme `egui` is currently painting with.
///
/// Dark mode flips `Visuals::dark_mode`, which is what the status colours key
/// off, so the same code gives readable statuses in both themes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisualsTokens {
    surface: Color32,
    text: Color32,
    muted_text: Color32,
    border: Color32,
    accent: Color32,
    danger: Color32,
    warning: Color32,
    success: Color32,
}

impl VisualsTokens {
    /// Read the tokens out of the `Ui`'s current visuals.
    #[must_use]
    pub fn from_ui(ui: &Ui) -> Self {
        Self::from_visuals(ui.visuals())
    }

    /// Read the tokens out of `visuals`.
    #[must_use]
    pub fn from_visuals(visuals: &egui::Visuals) -> Self {
        let dark = visuals.dark_mode;
        Self {
            surface: visuals.panel_fill,
            text: visuals.strong_text_color(),
            muted_text: visuals.weak_text_color(),
            border: visuals.widgets.noninteractive.bg_stroke.color,
            accent: visuals.selection.bg_fill,
            danger: if dark {
                Color32::from_rgb(0xFF, 0x8A, 0x80)
            } else {
                Color32::from_rgb(0xB3, 0x26, 0x1E)
            },
            warning: if dark {
                Color32::from_rgb(0xFF, 0xC1, 0x07)
            } else {
                Color32::from_rgb(0x8A, 0x5A, 0x00)
            },
            success: if dark {
                Color32::from_rgb(0x81, 0xC7, 0x84)
            } else {
                Color32::from_rgb(0x1B, 0x5E, 0x20)
            },
        }
    }
}

impl SettingsTokens for VisualsTokens {
    fn surface(&self) -> Color32 {
        self.surface
    }
    fn text(&self) -> Color32 {
        self.text
    }
    fn muted_text(&self) -> Color32 {
        self.muted_text
    }
    fn border(&self) -> Color32 {
        self.border
    }
    fn accent(&self) -> Color32 {
        self.accent
    }
    fn danger(&self) -> Color32 {
        self.danger
    }
    fn warning(&self) -> Color32 {
        self.warning
    }
    fn success(&self) -> Color32 {
        self.success
    }
}

/// Tone of a status badge.
///
/// A tone always travels with a glyph and a label: the cross-cutting rule is
/// that status is never communicated by colour alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// Nothing is wrong and nothing is confirmed.
    Neutral,
    /// Confirmed, healthy.
    Success,
    /// Worth reading, not a failure.
    Warning,
    /// A failure or a destructive consequence.
    Danger,
}

impl Tone {
    /// The colour for this tone.
    #[must_use]
    pub fn color(self, tokens: &dyn SettingsTokens) -> Color32 {
        match self {
            Self::Neutral => tokens.muted_text(),
            Self::Success => tokens.success(),
            Self::Warning => tokens.warning(),
            Self::Danger => tokens.danger(),
        }
    }
}
