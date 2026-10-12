//! The design tokens, ported value-for-value from `design/tokens.ts`.
//!
//! Twenty semantic colour roles in four themes, plus the spacing, radius,
//! typography, motion and focus scales. Spec §5.6 makes this module the single
//! source of truth: no widget in `plaudon-ui` may hard-code a colour or a
//! spacing value, exactly as no React component was allowed to.
//!
//! The values are the ones the shell being replaced shipped, so a screen
//! rendered in egui is the same colour as the screen it replaces. The contrast
//! guarantees `tokens.test.ts` asserted are asserted here, over the same four
//! themes (see the tests at the bottom of this file).

use egui::Color32;

/// Minimum usable desktop width in points (§5.6). Below it the window shows a
/// notice; the layout is never stacked mobile-style.
pub const MIN_DESKTOP_WIDTH: f32 = 880.0;

/// Minimum usable desktop height in points.
pub const MIN_DESKTOP_HEIGHT: f32 = 560.0;

/// Below this width the navigation sidebar collapses to an icon rail.
pub const COMPACT_LAYOUT_WIDTH: f32 = 1040.0;

/// WCAG 2.2 AA minimum pointer target size, in points.
pub const MIN_TARGET_SIZE: f32 = 24.0;

/// The four installable themes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ThemeKind {
    /// Standard light.
    Light,
    /// Standard dark.
    Dark,
    /// Increased-contrast light.
    HighContrastLight,
    /// Increased-contrast dark.
    HighContrastDark,
}

impl ThemeKind {
    /// Every theme, in the order `tokens.ts` declares them.
    pub const ALL: [Self; 4] = [
        Self::Light,
        Self::Dark,
        Self::HighContrastLight,
        Self::HighContrastDark,
    ];

    /// The identifier `tokens.ts` wrote into `data-theme`.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
            Self::HighContrastLight => "hc-light",
            Self::HighContrastDark => "hc-dark",
        }
    }

    /// Whether this theme is a dark one, which is what egui's own
    /// [`egui::Theme`] distinguishes.
    #[must_use]
    pub const fn is_dark(self) -> bool {
        matches!(self, Self::Dark | Self::HighContrastDark)
    }

    /// The egui theme slot this one is installed into.
    #[must_use]
    pub const fn egui_theme(self) -> egui::Theme {
        if self.is_dark() {
            egui::Theme::Dark
        } else {
            egui::Theme::Light
        }
    }
}

/// One of the twenty semantic colour roles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColorRole {
    /// Application background behind every surface.
    BgCanvas,
    /// Ordinary surface: panels, cards, rows.
    BgSurface,
    /// Raised surface: dialogs, popovers, toasts.
    BgRaised,
    /// Selected row or item background.
    BgSelected,
    /// Hovered row or item background.
    BgHover,
    /// Primary text.
    TextPrimary,
    /// Secondary and supporting text.
    TextSecondary,
    /// Text drawn on an accent fill.
    TextInverse,
    /// Hairline separators.
    BorderSubtle,
    /// Visible control borders.
    BorderStrong,
    /// Accent fill.
    Accent,
    /// Text on the accent fill.
    AccentText,
    /// Status: nothing is happening.
    StatusNeutral,
    /// Status: work is running.
    StatusRunning,
    /// Status: work is queued or blocked.
    StatusWaiting,
    /// Status: work finished.
    StatusSuccess,
    /// Status: finished with something the user must read.
    StatusWarning,
    /// Status: failed.
    StatusDanger,
    /// Keyboard focus ring.
    FocusRing,
    /// Scrim behind a modal.
    Overlay,
}

impl ColorRole {
    /// Every role, in the order `COLOR_ROLES` declares them.
    pub const ALL: [Self; 20] = [
        Self::BgCanvas,
        Self::BgSurface,
        Self::BgRaised,
        Self::BgSelected,
        Self::BgHover,
        Self::TextPrimary,
        Self::TextSecondary,
        Self::TextInverse,
        Self::BorderSubtle,
        Self::BorderStrong,
        Self::Accent,
        Self::AccentText,
        Self::StatusNeutral,
        Self::StatusRunning,
        Self::StatusWaiting,
        Self::StatusSuccess,
        Self::StatusWarning,
        Self::StatusDanger,
        Self::FocusRing,
        Self::Overlay,
    ];

    /// The CSS custom-property name the React shell used, kept so the two
    /// token sets can be compared during the migration.
    #[must_use]
    pub const fn css_name(self) -> &'static str {
        match self {
            Self::BgCanvas => "color-bg-canvas",
            Self::BgSurface => "color-bg-surface",
            Self::BgRaised => "color-bg-raised",
            Self::BgSelected => "color-bg-selected",
            Self::BgHover => "color-bg-hover",
            Self::TextPrimary => "color-text-primary",
            Self::TextSecondary => "color-text-secondary",
            Self::TextInverse => "color-text-inverse",
            Self::BorderSubtle => "color-border-subtle",
            Self::BorderStrong => "color-border-strong",
            Self::Accent => "color-accent",
            Self::AccentText => "color-accent-text",
            Self::StatusNeutral => "color-status-neutral",
            Self::StatusRunning => "color-status-running",
            Self::StatusWaiting => "color-status-waiting",
            Self::StatusSuccess => "color-status-success",
            Self::StatusWarning => "color-status-warning",
            Self::StatusDanger => "color-status-danger",
            Self::FocusRing => "color-focus-ring",
            Self::Overlay => "color-overlay",
        }
    }
}

/// The twenty colour roles of one theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorRoles {
    /// See [`ColorRole::BgCanvas`].
    pub bg_canvas: Color32,
    /// See [`ColorRole::BgSurface`].
    pub bg_surface: Color32,
    /// See [`ColorRole::BgRaised`].
    pub bg_raised: Color32,
    /// See [`ColorRole::BgSelected`].
    pub bg_selected: Color32,
    /// See [`ColorRole::BgHover`].
    pub bg_hover: Color32,
    /// See [`ColorRole::TextPrimary`].
    pub text_primary: Color32,
    /// See [`ColorRole::TextSecondary`].
    pub text_secondary: Color32,
    /// See [`ColorRole::TextInverse`].
    pub text_inverse: Color32,
    /// See [`ColorRole::BorderSubtle`].
    pub border_subtle: Color32,
    /// See [`ColorRole::BorderStrong`].
    pub border_strong: Color32,
    /// See [`ColorRole::Accent`].
    pub accent: Color32,
    /// See [`ColorRole::AccentText`].
    pub accent_text: Color32,
    /// See [`ColorRole::StatusNeutral`].
    pub status_neutral: Color32,
    /// See [`ColorRole::StatusRunning`].
    pub status_running: Color32,
    /// See [`ColorRole::StatusWaiting`].
    pub status_waiting: Color32,
    /// See [`ColorRole::StatusSuccess`].
    pub status_success: Color32,
    /// See [`ColorRole::StatusWarning`].
    pub status_warning: Color32,
    /// See [`ColorRole::StatusDanger`].
    pub status_danger: Color32,
    /// See [`ColorRole::FocusRing`].
    pub focus_ring: Color32,
    /// See [`ColorRole::Overlay`]; the only role with an alpha channel.
    pub overlay: Color32,
}

impl ColorRoles {
    /// One role by name, for tests and for the design-system documentation.
    #[must_use]
    pub const fn get(&self, role: ColorRole) -> Color32 {
        match role {
            ColorRole::BgCanvas => self.bg_canvas,
            ColorRole::BgSurface => self.bg_surface,
            ColorRole::BgRaised => self.bg_raised,
            ColorRole::BgSelected => self.bg_selected,
            ColorRole::BgHover => self.bg_hover,
            ColorRole::TextPrimary => self.text_primary,
            ColorRole::TextSecondary => self.text_secondary,
            ColorRole::TextInverse => self.text_inverse,
            ColorRole::BorderSubtle => self.border_subtle,
            ColorRole::BorderStrong => self.border_strong,
            ColorRole::Accent => self.accent,
            ColorRole::AccentText => self.accent_text,
            ColorRole::StatusNeutral => self.status_neutral,
            ColorRole::StatusRunning => self.status_running,
            ColorRole::StatusWaiting => self.status_waiting,
            ColorRole::StatusSuccess => self.status_success,
            ColorRole::StatusWarning => self.status_warning,
            ColorRole::StatusDanger => self.status_danger,
            ColorRole::FocusRing => self.focus_ring,
            ColorRole::Overlay => self.overlay,
        }
    }
}

/// `--space-0` … `--space-8`, in points.
pub const SPACE: [f32; 9] = [0.0, 2.0, 4.0, 8.0, 12.0, 16.0, 24.0, 32.0, 48.0];

/// Corner radii: small, medium, large.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Radius {
    /// `--radius-sm`.
    pub small: f32,
    /// `--radius-md`.
    pub medium: f32,
    /// `--radius-lg`.
    pub large: f32,
}

/// The type scale, in points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TypeScale {
    /// `--font-size-xs`.
    pub xs: f32,
    /// `--font-size-sm`.
    pub sm: f32,
    /// `--font-size-md`: the body size.
    pub md: f32,
    /// `--font-size-lg`.
    pub lg: f32,
    /// `--font-size-xl`.
    pub xl: f32,
    /// `--font-size-2xl`.
    pub xxl: f32,
    /// `--line-height-tight`.
    pub line_height_tight: f32,
    /// `--line-height-normal`.
    pub line_height_normal: f32,
}

/// Motion durations, in seconds (`tokens.ts` states them in milliseconds).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Motion {
    /// `--duration-fast`: 90 ms.
    pub fast: f32,
    /// `--duration-normal`: 160 ms.
    pub normal: f32,
}

/// The focus ring, which widens in high contrast and is never removed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FocusRing {
    /// `--focus-ring-width`, in points.
    pub width: f32,
    /// `--focus-ring-offset`, in points.
    pub offset: f32,
}

/// Every token of one theme.
///
/// `Copy`, so a widget reads it once per frame and passes it by value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tokens {
    /// Which theme these tokens belong to.
    pub theme: ThemeKind,
    /// The twenty colour roles.
    pub color: ColorRoles,
    /// Corner radii.
    pub radius: Radius,
    /// Type scale.
    pub text: TypeScale,
    /// Motion durations.
    pub motion: Motion,
    /// Focus ring geometry.
    pub focus: FocusRing,
}

const RADIUS: Radius = Radius {
    small: 3.0,
    medium: 6.0,
    large: 10.0,
};

const TYPE_SCALE: TypeScale = TypeScale {
    xs: 11.0,
    sm: 12.0,
    md: 13.0,
    lg: 15.0,
    xl: 19.0,
    xxl: 24.0,
    line_height_tight: 1.25,
    line_height_normal: 1.5,
};

const MOTION: Motion = Motion {
    fast: 0.09,
    normal: 0.16,
};

const STANDARD_FOCUS: FocusRing = FocusRing {
    width: 2.0,
    offset: 2.0,
};

const HIGH_CONTRAST_FOCUS: FocusRing = FocusRing {
    width: 3.0,
    offset: 2.0,
};

const LIGHT: ColorRoles = ColorRoles {
    bg_canvas: Color32::from_rgb(0xf6, 0xf6, 0xf7),
    bg_surface: Color32::from_rgb(0xff, 0xff, 0xff),
    bg_raised: Color32::from_rgb(0xff, 0xff, 0xff),
    bg_selected: Color32::from_rgb(0xdb, 0xe7, 0xfb),
    bg_hover: Color32::from_rgb(0xec, 0xee, 0xf1),
    text_primary: Color32::from_rgb(0x14, 0x16, 0x1a),
    text_secondary: Color32::from_rgb(0x53, 0x58, 0x5f),
    text_inverse: Color32::from_rgb(0xff, 0xff, 0xff),
    border_subtle: Color32::from_rgb(0xd7, 0xda, 0xde),
    border_strong: Color32::from_rgb(0x76, 0x7c, 0x84),
    accent: Color32::from_rgb(0x1a, 0x4f, 0xa0),
    accent_text: Color32::from_rgb(0xff, 0xff, 0xff),
    status_neutral: Color32::from_rgb(0x53, 0x58, 0x5f),
    status_running: Color32::from_rgb(0x1a, 0x4f, 0xa0),
    status_waiting: Color32::from_rgb(0x7a, 0x4a, 0x00),
    status_success: Color32::from_rgb(0x0f, 0x6b, 0x3f),
    status_warning: Color32::from_rgb(0x8a, 0x4b, 0x00),
    status_danger: Color32::from_rgb(0xa1, 0x12, 0x1f),
    focus_ring: Color32::from_rgb(0x0b, 0x57, 0xd0),
    // rgba(16, 18, 20, 0.45)
    overlay: Color32::from_rgba_premultiplied(7, 8, 9, 115),
};

const DARK: ColorRoles = ColorRoles {
    bg_canvas: Color32::from_rgb(0x17, 0x19, 0x1c),
    bg_surface: Color32::from_rgb(0x1e, 0x21, 0x25),
    bg_raised: Color32::from_rgb(0x26, 0x2a, 0x2f),
    bg_selected: Color32::from_rgb(0x2b, 0x3b, 0x55),
    bg_hover: Color32::from_rgb(0x2a, 0x2e, 0x34),
    text_primary: Color32::from_rgb(0xf2, 0xf4, 0xf6),
    text_secondary: Color32::from_rgb(0xb3, 0xba, 0xc2),
    text_inverse: Color32::from_rgb(0x14, 0x16, 0x1a),
    border_subtle: Color32::from_rgb(0x3a, 0x3f, 0x45),
    border_strong: Color32::from_rgb(0x6c, 0x74, 0x7d),
    accent: Color32::from_rgb(0x8f, 0xb4, 0xf2),
    accent_text: Color32::from_rgb(0x10, 0x14, 0x18),
    status_neutral: Color32::from_rgb(0xb3, 0xba, 0xc2),
    status_running: Color32::from_rgb(0x8f, 0xb4, 0xf2),
    status_waiting: Color32::from_rgb(0xe3, 0xb6, 0x67),
    status_success: Color32::from_rgb(0x67, 0xc3, 0x91),
    status_warning: Color32::from_rgb(0xe8, 0xa3, 0x5c),
    status_danger: Color32::from_rgb(0xf0, 0x8a, 0x92),
    focus_ring: Color32::from_rgb(0x9e, 0xc4, 0xff),
    // rgba(0, 0, 0, 0.6)
    overlay: Color32::from_rgba_premultiplied(0, 0, 0, 153),
};

const HIGH_CONTRAST_LIGHT: ColorRoles = ColorRoles {
    bg_canvas: Color32::from_rgb(0xff, 0xff, 0xff),
    bg_surface: Color32::from_rgb(0xff, 0xff, 0xff),
    bg_raised: Color32::from_rgb(0xff, 0xff, 0xff),
    bg_selected: Color32::from_rgb(0xcf, 0xe0, 0xff),
    bg_hover: Color32::from_rgb(0xe6, 0xe6, 0xe6),
    text_primary: Color32::from_rgb(0x00, 0x00, 0x00),
    text_secondary: Color32::from_rgb(0x1c, 0x1c, 0x1c),
    text_inverse: LIGHT.text_inverse,
    border_subtle: Color32::from_rgb(0x00, 0x00, 0x00),
    border_strong: Color32::from_rgb(0x00, 0x00, 0x00),
    accent: Color32::from_rgb(0x00, 0x34, 0x9b),
    accent_text: LIGHT.accent_text,
    status_neutral: Color32::from_rgb(0x1c, 0x1c, 0x1c),
    status_running: Color32::from_rgb(0x00, 0x34, 0x9b),
    status_waiting: Color32::from_rgb(0x5c, 0x31, 0x00),
    status_success: Color32::from_rgb(0x00, 0x4d, 0x2b),
    status_warning: Color32::from_rgb(0x6b, 0x2f, 0x00),
    status_danger: Color32::from_rgb(0x8a, 0x00, 0x10),
    focus_ring: Color32::from_rgb(0x00, 0x34, 0x9b),
    overlay: LIGHT.overlay,
};

const HIGH_CONTRAST_DARK: ColorRoles = ColorRoles {
    bg_canvas: Color32::from_rgb(0x00, 0x00, 0x00),
    bg_surface: Color32::from_rgb(0x00, 0x00, 0x00),
    bg_raised: Color32::from_rgb(0x0d, 0x0d, 0x0d),
    bg_selected: Color32::from_rgb(0x00, 0x3c, 0x74),
    bg_hover: Color32::from_rgb(0x1a, 0x1a, 0x1a),
    text_primary: Color32::from_rgb(0xff, 0xff, 0xff),
    text_secondary: Color32::from_rgb(0xf0, 0xf0, 0xf0),
    text_inverse: DARK.text_inverse,
    border_subtle: Color32::from_rgb(0xff, 0xff, 0xff),
    border_strong: Color32::from_rgb(0xff, 0xff, 0xff),
    accent: Color32::from_rgb(0xb8, 0xd4, 0xff),
    accent_text: Color32::from_rgb(0x00, 0x00, 0x00),
    status_neutral: Color32::from_rgb(0xf0, 0xf0, 0xf0),
    status_running: Color32::from_rgb(0xb8, 0xd4, 0xff),
    status_waiting: Color32::from_rgb(0xff, 0xd4, 0x8a),
    status_success: Color32::from_rgb(0x86, 0xe3, 0xad),
    status_warning: Color32::from_rgb(0xff, 0xc0, 0x78),
    status_danger: Color32::from_rgb(0xff, 0xb3, 0xb8),
    focus_ring: Color32::from_rgb(0xff, 0xff, 0xff),
    overlay: DARK.overlay,
};

impl Tokens {
    /// The tokens of `theme`.
    #[must_use]
    pub const fn for_theme(theme: ThemeKind) -> Self {
        let (color, focus) = match theme {
            ThemeKind::Light => (LIGHT, STANDARD_FOCUS),
            ThemeKind::Dark => (DARK, STANDARD_FOCUS),
            ThemeKind::HighContrastLight => (HIGH_CONTRAST_LIGHT, HIGH_CONTRAST_FOCUS),
            ThemeKind::HighContrastDark => (HIGH_CONTRAST_DARK, HIGH_CONTRAST_FOCUS),
        };
        Self {
            theme,
            color,
            radius: RADIUS,
            text: TYPE_SCALE,
            motion: MOTION,
            focus,
        }
    }

    /// `--space-N`, in points. `N` beyond the scale saturates at `--space-8`
    /// rather than panicking: a layout bug must not take the window down.
    #[must_use]
    pub fn space(self, step: usize) -> f32 {
        SPACE[step.min(SPACE.len() - 1)]
    }

    /// One colour role by name.
    #[must_use]
    pub const fn role(self, role: ColorRole) -> Color32 {
        self.color.get(role)
    }

    /// Where the tokens live in [`egui::Context`] memory.
    fn memory_id() -> egui::Id {
        egui::Id::new("plaudon-tokens")
    }

    /// Publish these tokens so any widget can read them with [`Tokens::get`].
    ///
    /// The shell does this once per frame, right after applying the style.
    pub fn store(self, ctx: &egui::Context) {
        ctx.data_mut(|data| data.insert_temp(Self::memory_id(), self));
    }

    /// The tokens the shell published this frame.
    ///
    /// Falls back to the tokens matching egui's own light/dark choice, so a
    /// widget rendered in isolation — a kittest case for one control, say —
    /// still gets coherent colours instead of a panic.
    #[must_use]
    pub fn get(ctx: &egui::Context) -> Self {
        ctx.data(|data| data.get_temp::<Self>(Self::memory_id()))
            .unwrap_or_else(|| {
                Self::for_theme(if ctx.theme() == egui::Theme::Dark {
                    ThemeKind::Dark
                } else {
                    ThemeKind::Light
                })
            })
    }
}

/// WCAG 2.1 relative luminance of an opaque colour.
#[must_use]
pub fn relative_luminance(color: Color32) -> f32 {
    fn channel(value: u8) -> f32 {
        let c = f32::from(value) / 255.0;
        if c <= 0.03928 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * channel(color.r()) + 0.7152 * channel(color.g()) + 0.0722 * channel(color.b())
}

/// WCAG 2.1 contrast ratio between two opaque colours, 1.0 … 21.0.
///
/// Alpha is ignored: every role but [`ColorRole::Overlay`] is opaque, and the
/// overlay is a scrim, not a text or border colour.
#[must_use]
pub fn contrast_ratio(a: Color32, b: Color32) -> f32 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BACKGROUNDS: [ColorRole; 3] = [
        ColorRole::BgCanvas,
        ColorRole::BgSurface,
        ColorRole::BgRaised,
    ];

    #[test]
    fn every_theme_defines_every_role() {
        for theme in ThemeKind::ALL {
            let tokens = Tokens::for_theme(theme);
            for role in ColorRole::ALL {
                let color = tokens.role(role);
                assert!(
                    color.a() > 0,
                    "{}.{} is fully transparent",
                    theme.id(),
                    role.css_name()
                );
            }
        }
    }

    #[test]
    fn light_and_dark_differ_where_it_matters() {
        let light = Tokens::for_theme(ThemeKind::Light);
        let dark = Tokens::for_theme(ThemeKind::Dark);
        assert_ne!(light.color.bg_canvas, dark.color.bg_canvas);
        assert_ne!(light.color.text_primary, dark.color.text_primary);
    }

    #[test]
    fn text_meets_four_and_a_half_to_one_on_every_background_in_every_theme() {
        for theme in ThemeKind::ALL {
            let tokens = Tokens::for_theme(theme);
            for background in BACKGROUNDS {
                let bg = tokens.role(background);
                for text in [ColorRole::TextPrimary, ColorRole::TextSecondary] {
                    let ratio = contrast_ratio(tokens.role(text), bg);
                    assert!(
                        ratio >= 4.5,
                        "{} {} on {} is {ratio:.2}:1",
                        theme.id(),
                        text.css_name(),
                        background.css_name()
                    );
                }
            }
        }
    }

    #[test]
    fn every_status_colour_meets_four_and_a_half_to_one_on_the_surface() {
        let statuses = [
            ColorRole::StatusNeutral,
            ColorRole::StatusRunning,
            ColorRole::StatusWaiting,
            ColorRole::StatusSuccess,
            ColorRole::StatusWarning,
            ColorRole::StatusDanger,
        ];
        for theme in ThemeKind::ALL {
            let tokens = Tokens::for_theme(theme);
            let surface = tokens.color.bg_surface;
            for status in statuses {
                let ratio = contrast_ratio(tokens.role(status), surface);
                assert!(
                    ratio >= 4.5,
                    "{} {} is {ratio:.2}:1",
                    theme.id(),
                    status.css_name()
                );
            }
        }
    }

    #[test]
    fn borders_and_the_focus_ring_meet_three_to_one() {
        for theme in ThemeKind::ALL {
            let tokens = Tokens::for_theme(theme);
            let surface = tokens.color.bg_surface;
            for role in [ColorRole::BorderStrong, ColorRole::FocusRing] {
                let ratio = contrast_ratio(tokens.role(role), surface);
                assert!(
                    ratio >= 3.0,
                    "{} {} is {ratio:.2}:1",
                    theme.id(),
                    role.css_name()
                );
            }
        }
    }

    #[test]
    fn text_on_the_accent_fill_meets_four_and_a_half_to_one() {
        for theme in ThemeKind::ALL {
            let tokens = Tokens::for_theme(theme);
            let ratio = contrast_ratio(tokens.color.accent_text, tokens.color.accent);
            assert!(ratio >= 4.5, "{} accent text is {ratio:.2}:1", theme.id());
        }
    }

    #[test]
    fn the_focus_ring_is_never_removed_and_widens_in_high_contrast() {
        for theme in ThemeKind::ALL {
            assert!(Tokens::for_theme(theme).focus.width >= 2.0);
        }
        assert!(
            (Tokens::for_theme(ThemeKind::HighContrastLight).focus.width - 3.0).abs()
                < f32::EPSILON
        );
        assert!(
            (Tokens::for_theme(ThemeKind::HighContrastDark).focus.width - 3.0).abs() < f32::EPSILON
        );
    }

    #[test]
    fn the_spacing_scale_is_the_one_the_stylesheet_shipped() {
        let tokens = Tokens::for_theme(ThemeKind::Light);
        assert!((tokens.space(5) - 16.0).abs() < f32::EPSILON);
        assert!((tokens.space(8) - 48.0).abs() < f32::EPSILON);
        // Out of range saturates instead of panicking.
        assert!((tokens.space(99) - 48.0).abs() < f32::EPSILON);
    }

    #[test]
    fn the_breakpoints_stay_desktop_class() {
        // The values the stylesheet shipped: no mobile viewport assumption,
        // and the compact rail appears before the minimum is reached.
        assert_eq!(
            [MIN_DESKTOP_WIDTH, MIN_DESKTOP_HEIGHT, COMPACT_LAYOUT_WIDTH],
            [880.0, 560.0, 1040.0]
        );
    }

    #[test]
    fn tokens_round_trip_through_context_memory() {
        let ctx = egui::Context::default();
        let dark = Tokens::for_theme(ThemeKind::HighContrastDark);
        dark.store(&ctx);
        assert_eq!(Tokens::get(&ctx), dark);
    }

    #[test]
    fn a_widget_rendered_without_the_shell_still_gets_tokens() {
        let ctx = egui::Context::default();
        ctx.set_theme(egui::Theme::Light);
        assert_eq!(Tokens::get(&ctx).theme, ThemeKind::Light);
    }
}
