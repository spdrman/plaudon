//! Turning [`Tokens`] into an [`egui::Style`], and installing both themes.
//!
//! The technique is the one photocraft and wordcraft both use and the plan
//! records (`docs/EGUI_REFERENCE_COMPONENTS.md` §2.4): build a style for the
//! light theme and a style for the dark one, install **both** with
//! [`egui::Context::set_style_of`], and leave the choice between them to
//! [`egui::ThemePreference`]. An operating-system appearance change then flips
//! the window without the application noticing, which is what parity row S4
//! means by "follows OS live".

use egui::{
    style::{Selection, WidgetVisuals, Widgets},
    Color32, CornerRadius, FontFamily, FontId, Margin, Stroke, Style, TextStyle, Vec2, Visuals,
};

use super::tokens::{ThemeKind, Tokens, MIN_TARGET_SIZE, SPACE};

/// The style for one theme.
#[must_use]
pub fn style_for(theme: ThemeKind) -> Style {
    let tokens = Tokens::for_theme(theme);
    let color = tokens.color;
    let radius = CornerRadius::same(tokens.radius.medium as u8);

    let widget = |fill: Color32, stroke: Stroke, text: Color32, expansion: f32| WidgetVisuals {
        bg_fill: fill,
        weak_bg_fill: fill,
        bg_stroke: stroke,
        corner_radius: radius,
        fg_stroke: Stroke::new(1.0, text),
        expansion,
    };

    let mut visuals = if theme.is_dark() {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    visuals.dark_mode = theme.is_dark();
    visuals.widgets = Widgets {
        noninteractive: widget(
            color.bg_surface,
            Stroke::new(1.0, color.border_subtle),
            color.text_secondary,
            0.0,
        ),
        inactive: widget(
            color.bg_surface,
            Stroke::new(1.0, color.border_subtle),
            color.text_primary,
            0.0,
        ),
        hovered: widget(
            color.bg_hover,
            Stroke::new(1.0, color.border_strong),
            color.text_primary,
            1.0,
        ),
        // egui gives a focused widget the `active` visuals, so this is where
        // the focus ring lives: the token colour at the token width, which is
        // 3 points in the high-contrast themes.
        active: widget(
            color.bg_selected,
            Stroke::new(tokens.focus.width, color.focus_ring),
            color.text_primary,
            1.0,
        ),
        open: widget(
            color.bg_raised,
            Stroke::new(1.0, color.border_strong),
            color.text_primary,
            0.0,
        ),
    };
    visuals.selection = Selection {
        bg_fill: color.bg_selected,
        stroke: Stroke::new(1.0, color.text_primary),
    };
    visuals.panel_fill = color.bg_canvas;
    visuals.window_fill = color.bg_raised;
    visuals.window_stroke = Stroke::new(1.0, color.border_subtle);
    visuals.window_corner_radius = CornerRadius::same(tokens.radius.large as u8);
    visuals.menu_corner_radius = radius;
    visuals.faint_bg_color = color.bg_hover;
    visuals.extreme_bg_color = color.bg_surface;
    visuals.code_bg_color = color.bg_hover;
    visuals.hyperlink_color = color.accent;
    visuals.warn_fg_color = color.status_warning;
    visuals.error_fg_color = color.status_danger;
    visuals.text_cursor.stroke = Stroke::new(2.0, color.text_primary);
    visuals.striped = false;
    visuals.button_frame = true;

    let mut style = Style {
        visuals,
        ..Style::default()
    };
    style.text_styles = [
        (
            TextStyle::Small,
            FontId::new(tokens.text.sm, FontFamily::Proportional),
        ),
        (
            TextStyle::Body,
            FontId::new(tokens.text.md, FontFamily::Proportional),
        ),
        (
            TextStyle::Button,
            FontId::new(tokens.text.md, FontFamily::Proportional),
        ),
        (
            TextStyle::Heading,
            FontId::new(tokens.text.xl, FontFamily::Proportional),
        ),
        (
            TextStyle::Monospace,
            FontId::new(tokens.text.md, FontFamily::Monospace),
        ),
    ]
    .into();
    style.spacing.item_spacing = Vec2::new(SPACE[3], SPACE[2]);
    style.spacing.button_padding = Vec2::new(SPACE[4], SPACE[2]);
    style.spacing.menu_margin = Margin::same(SPACE[2] as i8);
    style.spacing.window_margin = Margin::same(SPACE[5] as i8);
    style.spacing.indent = SPACE[6];
    // WCAG 2.2 AA target size: every control is at least 24 points high.
    style.spacing.interact_size = Vec2::new(40.0, MIN_TARGET_SIZE);
    style.animation_time = tokens.motion.fast;
    style
}

/// Install both themes and select between them.
///
/// `dark` chooses which of the two installed styles is active now; the OS
/// preference does the choosing when the user asked for
/// [`egui::ThemePreference::System`].
pub fn install(ctx: &egui::Context, contrast_is_high: bool, preference: egui::ThemePreference) {
    let (light, dark) = if contrast_is_high {
        (ThemeKind::HighContrastLight, ThemeKind::HighContrastDark)
    } else {
        (ThemeKind::Light, ThemeKind::Dark)
    };
    ctx.set_style_of(egui::Theme::Light, style_for(light));
    ctx.set_style_of(egui::Theme::Dark, style_for(dark));
    // When the platform reports no appearance at all, follow the React
    // shell's initial state (`systemDark: false`) rather than egui's default
    // of dark, so "follow the system" means the same thing in both shells.
    ctx.options_mut(|options| options.fallback_theme = egui::Theme::Light);
    ctx.set_theme(preference);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::tokens::ColorRole;

    #[test]
    fn the_style_paints_with_the_tokens_and_not_with_eguis_defaults() {
        for theme in ThemeKind::ALL {
            let style = style_for(theme);
            let tokens = Tokens::for_theme(theme);
            assert_eq!(style.visuals.panel_fill, tokens.role(ColorRole::BgCanvas));
            assert_eq!(style.visuals.window_fill, tokens.role(ColorRole::BgRaised));
            assert_eq!(
                style.visuals.selection.bg_fill,
                tokens.role(ColorRole::BgSelected)
            );
            assert_eq!(style.visuals.dark_mode, theme.is_dark());
        }
    }

    #[test]
    fn a_focused_widget_gets_the_focus_ring_at_the_token_width() {
        let standard = style_for(ThemeKind::Light);
        assert_eq!(
            standard.visuals.widgets.active.bg_stroke.color,
            Tokens::for_theme(ThemeKind::Light).color.focus_ring
        );
        let high = style_for(ThemeKind::HighContrastLight);
        assert!(
            high.visuals.widgets.active.bg_stroke.width
                > standard.visuals.widgets.active.bg_stroke.width
        );
    }

    #[test]
    fn controls_are_at_least_the_minimum_target_size() {
        assert!(style_for(ThemeKind::Light).spacing.interact_size.y >= MIN_TARGET_SIZE);
    }

    #[test]
    fn both_themes_are_installed_so_the_os_can_switch_without_us() {
        let ctx = egui::Context::default();
        install(&ctx, false, egui::ThemePreference::System);
        let light = ctx.style_of(egui::Theme::Light);
        let dark = ctx.style_of(egui::Theme::Dark);
        assert_eq!(
            light.visuals.panel_fill,
            Tokens::for_theme(ThemeKind::Light).color.bg_canvas
        );
        assert_eq!(
            dark.visuals.panel_fill,
            Tokens::for_theme(ThemeKind::Dark).color.bg_canvas
        );
    }

    #[test]
    fn following_a_system_that_reports_nothing_means_light() {
        let ctx = egui::Context::default();
        install(&ctx, false, egui::ThemePreference::System);
        assert_eq!(ctx.system_theme(), None);
        assert_eq!(ctx.theme(), egui::Theme::Light);
    }

    #[test]
    fn high_contrast_installs_the_high_contrast_pair() {
        let ctx = egui::Context::default();
        install(&ctx, true, egui::ThemePreference::Dark);
        assert_eq!(
            ctx.style_of(egui::Theme::Dark).visuals.panel_fill,
            Tokens::for_theme(ThemeKind::HighContrastDark)
                .color
                .bg_canvas
        );
    }
}
