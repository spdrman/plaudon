//! The toolbar, with overflow (parity row S6).
//!
//! New in O1.3: the shell being replaced had a header with two fixed buttons
//! and nothing that adapted. Here the toolbar is driven by the command
//! registry, and when the window is too narrow for every action the ones that
//! do not fit move into an overflow menu rather than being clipped — which is
//! what keeps them reachable at the 880-point minimum width.
//!
//! The fitting rule is a pure function so it is tested without a window;
//! craft-libs has an equivalent (`fit_toolbar`) but it is not part of the
//! vendored subset (`third_party/craft-ui`, "the toolbar-fitting module is
//! not vendored").

use egui::{accesskit, Align, Layout};

use super::commands::CommandRegistry;
use super::shortcuts::{display, KeyMap};

/// What the toolbar is reported as to a screen reader.
pub const TOOLBAR_LABEL: &str = "Toolbar";

/// Label of the overflow menu.
pub const OVERFLOW_LABEL: &str = "More actions";

/// How many of `widths` fit in `available`, leaving room for the overflow
/// control when at least one item has to move into it.
///
/// Returns `0` for invalid input (a non-finite width, a negative width)
/// rather than guessing: the caller then shows everything in the overflow
/// menu, which is still a usable toolbar.
#[must_use]
pub fn fit(widths: &[f32], spacing: f32, available: f32, overflow_width: f32) -> usize {
    if !available.is_finite()
        || !spacing.is_finite()
        || !overflow_width.is_finite()
        || available <= 0.0
        || spacing < 0.0
        || overflow_width < 0.0
        || widths
            .iter()
            .any(|width| !width.is_finite() || *width < 0.0)
    {
        return 0;
    }
    let total: f32 = widths.iter().sum::<f32>() + spacing * widths.len().saturating_sub(1) as f32;
    if total <= available {
        return widths.len();
    }
    let mut used = 0.0;
    let mut count = 0;
    for (index, width) in widths.iter().enumerate() {
        let gap = if index == 0 { 0.0 } else { spacing };
        let with_overflow = used + gap + width + spacing + overflow_width;
        if with_overflow > available {
            break;
        }
        used += gap + width;
        count += 1;
    }
    count
}

/// One toolbar action: a command the registry knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolbarItem {
    /// Command identifier.
    pub command_id: &'static str,
    /// Whether the action can be used right now.
    pub enabled: bool,
}

impl ToolbarItem {
    /// An enabled item.
    #[must_use]
    pub const fn new(command_id: &'static str) -> Self {
        Self {
            command_id,
            enabled: true,
        }
    }

    /// The same item, disabled.
    #[must_use]
    pub const fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
}

/// Draw the toolbar and report the command the user ran.
///
/// `trailing` paints at the right-hand end — the status text and the activity
/// indicator live there — and is given the remaining width.
pub fn toolbar(
    ui: &mut egui::Ui,
    registry: &CommandRegistry,
    keymap: &KeyMap,
    items: &[ToolbarItem],
    trailing: impl FnOnce(&mut egui::Ui),
) -> Option<String> {
    let os = ui.ctx().os();
    let mut invoked = None;

    let labels: Vec<&'static str> = items
        .iter()
        .filter_map(|item| registry.get(item.command_id).map(|spec| spec.label))
        .collect();
    let padding = ui.spacing().button_padding.x * 2.0;
    let spacing = ui.spacing().item_spacing.x;
    let widths: Vec<f32> = labels
        .iter()
        .map(|label| text_width(ui, label) + padding)
        .collect();
    // Half the width is reserved for the status text on the right.
    let available = (ui.available_width() * 0.5).max(0.0);
    let overflow_width = text_width(ui, OVERFLOW_LABEL) + padding;
    let visible = fit(&widths, spacing, available, overflow_width);

    ui.horizontal(|ui| {
        let (landmark, _) = ui.allocate_space(egui::Vec2::ZERO);
        ui.ctx().accesskit_node_builder(landmark, |node| {
            node.set_role(accesskit::Role::Toolbar);
            node.set_label(TOOLBAR_LABEL);
        });

        for item in items.iter().take(visible) {
            if action_button(ui, registry, keymap, os, *item).clicked() {
                invoked = Some(item.command_id.to_owned());
            }
        }
        if visible < items.len() {
            egui::containers::menu::MenuButton::new(OVERFLOW_LABEL).ui(ui, |ui| {
                for item in items.iter().skip(visible) {
                    if action_button(ui, registry, keymap, os, *item).clicked() {
                        invoked = Some(item.command_id.to_owned());
                        ui.close();
                    }
                }
            });
        }
        ui.with_layout(Layout::right_to_left(Align::Center), trailing);
    });
    invoked
}

fn action_button(
    ui: &mut egui::Ui,
    registry: &CommandRegistry,
    keymap: &KeyMap,
    os: egui::os::OperatingSystem,
    item: ToolbarItem,
) -> egui::Response {
    let Some(spec) = registry.get(item.command_id) else {
        // A toolbar that names a command nobody registered is a bug in the
        // caller; show nothing rather than an unlabelled control.
        return ui.allocate_response(egui::Vec2::ZERO, egui::Sense::hover());
    };
    let response = ui.add_enabled(item.enabled, egui::Button::new(spec.label));
    match keymap.primary_chord(spec) {
        Some(chord) => response.on_hover_text(format!("{} ({})", spec.label, display(&chord, os))),
        None => response.on_hover_text(spec.label),
    }
}

fn text_width(ui: &egui::Ui, text: &str) -> f32 {
    let font = egui::TextStyle::Button.resolve(ui.style());
    ui.painter()
        .layout_no_wrap(text.to_owned(), font, egui::Color32::PLACEHOLDER)
        .rect
        .width()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn everything_fits_when_there_is_room() {
        assert_eq!(fit(&[100.0, 100.0, 100.0], 8.0, 1000.0, 90.0), 3);
    }

    #[test]
    fn the_overflow_control_is_accounted_for_when_something_has_to_move() {
        // 3 × 100 + 2 × 8 = 316 > 300, so items move into the overflow menu
        // and the menu button's own width is reserved.
        let visible = fit(&[100.0, 100.0, 100.0], 8.0, 300.0, 90.0);
        assert_eq!(visible, 1);
    }

    #[test]
    fn nothing_fits_in_no_space() {
        assert_eq!(fit(&[100.0], 8.0, 10.0, 90.0), 0);
    }

    #[test]
    fn invalid_input_puts_everything_in_the_overflow_menu_rather_than_guessing() {
        assert_eq!(fit(&[f32::NAN], 8.0, 500.0, 90.0), 0);
        assert_eq!(fit(&[100.0], f32::INFINITY, 500.0, 90.0), 0);
        assert_eq!(fit(&[100.0], 8.0, -1.0, 90.0), 0);
        assert_eq!(fit(&[-10.0], 8.0, 500.0, 90.0), 0);
    }

    #[test]
    fn an_empty_toolbar_fits_trivially() {
        assert_eq!(fit(&[], 8.0, 500.0, 90.0), 0);
    }
}
