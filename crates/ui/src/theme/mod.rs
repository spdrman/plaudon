//! Themes: the tokens, the egui style they become, and the appearance state.
//!
//! Parity rows S4 and S5. Four themes — light, dark and a high-contrast
//! variant of each — built from the twenty colour roles of `design/tokens.ts`,
//! with the same contrast guarantees asserted as unit tests.
//!
//! The user's choice is two independent preferences, as it was in
//! `design/theme.ts`: an appearance ([`Appearance::System`], `Light`, `Dark`)
//! and a contrast ([`Contrast::Standard`], `High`). The effective theme is the
//! product of the two and of what the operating system currently reports.

pub mod apply;
pub mod tokens;

pub use apply::{install, style_for};
pub use tokens::{
    contrast_ratio, relative_luminance, ColorRole, ColorRoles, FocusRing, Motion, Radius,
    ThemeKind, Tokens, TypeScale, COMPACT_LAYOUT_WIDTH, MIN_DESKTOP_HEIGHT, MIN_DESKTOP_WIDTH,
    MIN_TARGET_SIZE, SPACE,
};

use plaudon_app::dto::{Appearance, Contrast};

/// What the user chose and what the operating system reports.
///
/// The same reducer `design/theme.ts` implemented, minus the browser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemeState {
    /// Follow the system, or force light or dark.
    pub appearance: Appearance,
    /// Standard or increased contrast.
    pub contrast: Contrast,
    /// The last appearance the operating system reported, for the rare case
    /// where it reports nothing at all.
    pub system_dark: bool,
}

impl Default for ThemeState {
    fn default() -> Self {
        Self {
            appearance: Appearance::System,
            contrast: Contrast::Standard,
            system_dark: false,
        }
    }
}

impl ThemeState {
    /// The theme that should be painted right now.
    #[must_use]
    pub const fn effective(self) -> ThemeKind {
        let dark = match self.appearance {
            Appearance::System => self.system_dark,
            Appearance::Light => false,
            Appearance::Dark => true,
        };
        match (dark, self.contrast) {
            (false, Contrast::Standard) => ThemeKind::Light,
            (true, Contrast::Standard) => ThemeKind::Dark,
            (false, Contrast::High) => ThemeKind::HighContrastLight,
            (true, Contrast::High) => ThemeKind::HighContrastDark,
        }
    }

    /// The egui preference matching [`ThemeState::appearance`].
    #[must_use]
    pub const fn preference(self) -> egui::ThemePreference {
        match self.appearance {
            Appearance::System => egui::ThemePreference::System,
            Appearance::Light => egui::ThemePreference::Light,
            Appearance::Dark => egui::ThemePreference::Dark,
        }
    }

    /// Record what the operating system is reporting this frame.
    ///
    /// Returns `true` when it changed, which is the shell's cue to reinstall
    /// the styles and repaint. egui tracks the system appearance itself, so
    /// there is no watcher thread and no polling.
    pub fn observe_system(&mut self, ctx: &egui::Context) -> bool {
        let Some(system) = ctx.system_theme() else {
            return false;
        };
        let dark = system == egui::Theme::Dark;
        if self.system_dark == dark {
            return false;
        }
        self.system_dark = dark;
        self.appearance == Appearance::System
    }

    /// Install this state's styles on `ctx` and publish its tokens.
    pub fn apply(self, ctx: &egui::Context) -> Tokens {
        install(ctx, self.contrast == Contrast::High, self.preference());
        // With `ThemePreference::System` the effective theme is whatever egui
        // resolved this frame, not what we last observed, so read it back
        // rather than guessing.
        let kind = match (self.appearance, self.contrast) {
            (Appearance::System, contrast) => match (ctx.theme(), contrast) {
                (egui::Theme::Dark, Contrast::Standard) => ThemeKind::Dark,
                (egui::Theme::Dark, Contrast::High) => ThemeKind::HighContrastDark,
                (egui::Theme::Light, Contrast::Standard) => ThemeKind::Light,
                (egui::Theme::Light, Contrast::High) => ThemeKind::HighContrastLight,
            },
            _ => self.effective(),
        };
        let tokens = Tokens::for_theme(kind);
        tokens.store(ctx);
        tokens
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appearance_and_contrast_combine_into_the_four_themes() {
        let cases = [
            (
                Appearance::Light,
                Contrast::Standard,
                false,
                ThemeKind::Light,
            ),
            (Appearance::Dark, Contrast::Standard, false, ThemeKind::Dark),
            (
                Appearance::Light,
                Contrast::High,
                true,
                ThemeKind::HighContrastLight,
            ),
            (
                Appearance::Dark,
                Contrast::High,
                false,
                ThemeKind::HighContrastDark,
            ),
            (
                Appearance::System,
                Contrast::Standard,
                true,
                ThemeKind::Dark,
            ),
            (
                Appearance::System,
                Contrast::High,
                false,
                ThemeKind::HighContrastLight,
            ),
        ];
        for (appearance, contrast, system_dark, expected) in cases {
            let state = ThemeState {
                appearance,
                contrast,
                system_dark,
            };
            assert_eq!(state.effective(), expected, "{appearance:?} {contrast:?}");
        }
    }

    #[test]
    fn a_forced_appearance_ignores_the_operating_system() {
        let ctx = egui::Context::default();
        let mut state = ThemeState {
            appearance: Appearance::Light,
            ..ThemeState::default()
        };
        ctx.set_theme(egui::ThemePreference::Dark);
        assert!(!state.observe_system(&ctx) || state.appearance == Appearance::System);
        assert_eq!(state.effective(), ThemeKind::Light);
    }

    #[test]
    fn applying_publishes_the_tokens_the_frame_is_painted_with() {
        let ctx = egui::Context::default();
        let state = ThemeState {
            appearance: Appearance::Dark,
            contrast: Contrast::High,
            system_dark: false,
        };
        let tokens = state.apply(&ctx);
        assert_eq!(tokens.theme, ThemeKind::HighContrastDark);
        assert_eq!(Tokens::get(&ctx), tokens);
    }
}
