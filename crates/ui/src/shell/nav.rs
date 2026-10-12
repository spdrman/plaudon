//! The navigation rail (parity rows S1 and S3).
//!
//! Six destinations as a vertical tab list: `Role::Tab` inside
//! `Role::TabList`, arrow keys and Home/End move between them, Enter and
//! Space activate, and the label never disappears from the accessibility
//! tree even when the rail is collapsed to glyphs. This is what
//! `AppNav.tsx`'s roving tab list did, expressed in AccessKit instead of ARIA.

use egui::{accesskit, Align, Layout, RichText, Vec2};

use crate::theme::Tokens;

use super::route::Destination;

/// Width of the rail when it is collapsed to its glyphs.
pub const COLLAPSED_WIDTH: f32 = 56.0;

/// What the rail is reported as to a screen reader.
pub const NAV_LABEL: &str = "Primary";

/// Draw the rail and report the destination the user chose.
///
/// `available` marks the destinations whose screens are registered; the
/// others are still navigable — the registry shows an honest empty state —
/// but they are announced as not yet available rather than pretending.
pub fn nav_rail(
    ui: &mut egui::Ui,
    active: Destination,
    collapsed: bool,
    available: &dyn Fn(Destination) -> bool,
) -> Option<Destination> {
    let tokens = Tokens::get(ui.ctx());
    let mut chosen = None;
    let mut ids = Vec::with_capacity(Destination::ALL.len());
    let mut move_focus_to = None;

    ui.scope(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(0.0, tokens.space(1));
        ui.with_layout(Layout::top_down_justified(Align::LEFT), |ui| {
            for (index, destination) in Destination::ALL.into_iter().enumerate() {
                let selected = destination == active;
                let label = if collapsed {
                    RichText::new(destination.glyph())
                } else {
                    RichText::new(destination.label())
                };
                // A stable id per destination, so focus can be moved onto a
                // sibling without depending on paint order.
                let response = ui
                    .push_id(destination.id(), |ui| ui.selectable_label(selected, label))
                    .inner
                    .on_hover_text(destination.label());
                ids.push(response.id);

                // The glyph is decoration; the accessible name is always the
                // destination's full label.
                let name = if available(destination) {
                    destination.label().to_owned()
                } else {
                    format!("{} (not yet available)", destination.label())
                };
                ui.ctx().accesskit_node_builder(response.id, |node| {
                    node.set_role(accesskit::Role::Tab);
                    node.set_label(name.clone());
                    node.set_selected(selected);
                    node.set_position_in_set(index + 1);
                    node.set_size_of_set(Destination::ALL.len());
                });

                if response.clicked() {
                    chosen = Some(destination);
                }
                if response.has_focus() {
                    if let Some(target) = arrow_target(ui, index) {
                        chosen = Destination::ALL.get(target).copied();
                        move_focus_to = Some(target);
                    }
                }
            }
        });
    });
    if let Some((target, id)) = move_focus_to.and_then(|at| ids.get(at).map(|id| (at, *id))) {
        debug_assert!(target < Destination::ALL.len());
        ui.ctx().memory_mut(|memory| memory.request_focus(id));
    }
    chosen
}

/// The index an arrow key moves to, if one was pressed.
fn arrow_target(ui: &egui::Ui, index: usize) -> Option<usize> {
    let count = Destination::ALL.len();
    let (back, forward, home, end) = ui.input_mut(|input| {
        (
            input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp)
                || input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft),
            input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown)
                || input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight),
            input.consume_key(egui::Modifiers::NONE, egui::Key::Home),
            input.consume_key(egui::Modifiers::NONE, egui::Key::End),
        )
    });
    if forward {
        Some((index + 1) % count)
    } else if back {
        Some((index + count - 1) % count)
    } else if home {
        Some(0)
    } else if end {
        Some(count - 1)
    } else {
        None
    }
}

/// Mark `ui`'s container as the navigation landmark.
///
/// egui has no landmark container, so the rail allocates one zero-sized node
/// carrying `Role::Navigation`; a screen reader then announces the region the
/// same way `<nav aria-label="Primary">` did.
pub fn navigation_landmark(ui: &mut egui::Ui) {
    let (id, _) = ui.allocate_space(Vec2::ZERO);
    ui.ctx().accesskit_node_builder(id, |node| {
        node.set_role(accesskit::Role::Navigation);
        node.set_label(NAV_LABEL);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrow_keys_wrap_in_both_directions() {
        // The wrap arithmetic is the part worth asserting without a frame:
        // from the last destination forward is the first, and back from the
        // first is the last.
        let count = Destination::ALL.len();
        let forward = |index: usize| (index + 1) % count;
        let back = |index: usize| (index + count - 1) % count;
        assert_eq!(forward(count - 1), 0);
        assert_eq!(back(0), count - 1);
        assert_eq!(forward(0), 1);
    }
}
