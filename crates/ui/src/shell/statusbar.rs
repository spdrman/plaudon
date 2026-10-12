//! The status bar and the activity indicator (parity rows S11 and S14).
//!
//! One line at the foot of the window: what the shell is doing, whether
//! anything is in flight, and — below the 880-point minimum the window can
//! still be dragged to — the same notice the React shell showed.

use egui::{accesskit, Align, Layout, RichText};

use crate::theme::{Tokens, MIN_DESKTOP_WIDTH};

/// The notice shown when the window is narrower than the supported minimum.
///
/// Wording preserved from `App.tsx`: the layout is not broken, it is
/// scrollable, and resizing restores it.
#[must_use]
pub fn narrow_notice(window_width: f32) -> Option<String> {
    (window_width.is_finite() && window_width < MIN_DESKTOP_WIDTH).then(|| {
        format!(
            "This window is narrower than the {}pt minimum. Columns are scrollable; \
             resize the window for the full layout.",
            MIN_DESKTOP_WIDTH as i32
        )
    })
}

/// Draw the status bar.
///
/// `in_flight` is the number of commands waiting for an answer; it drives the
/// activity indicator, which is a spinner **and** a count, because a spinner
/// alone says nothing to a screen reader.
pub fn status_bar(ui: &mut egui::Ui, status: &str, in_flight: usize, notice: Option<&str>) {
    let tokens = Tokens::get(ui.ctx());
    ui.horizontal(|ui| {
        let label = ui.label(RichText::new(status).color(tokens.color.text_secondary));
        ui.ctx().accesskit_node_builder(label.id, |node| {
            node.set_role(accesskit::Role::Status);
            node.set_live(accesskit::Live::Polite);
            node.set_label(format!("Status: {status}"));
        });

        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if in_flight > 0 {
                let text = if in_flight == 1 {
                    "1 task running".to_owned()
                } else {
                    format!("{in_flight} tasks running")
                };
                ui.add(egui::Spinner::new().size(tokens.text.md));
                let indicator = ui.label(RichText::new(&text).color(tokens.color.status_running));
                ui.ctx().accesskit_node_builder(indicator.id, |node| {
                    node.set_role(accesskit::Role::Status);
                    node.set_live(accesskit::Live::Polite);
                    node.set_label(text.clone());
                });
            }
        });
    });
    if let Some(notice) = notice {
        let response = ui.label(RichText::new(notice).color(tokens.color.status_warning));
        ui.ctx().accesskit_node_builder(response.id, |node| {
            node.set_role(accesskit::Role::Status);
            node.set_live(accesskit::Live::Polite);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_notice_appears_only_below_the_supported_minimum() {
        assert!(narrow_notice(1280.0).is_none());
        assert!(narrow_notice(MIN_DESKTOP_WIDTH).is_none());
        let notice = narrow_notice(700.0).expect("a narrow window is explained");
        assert!(notice.contains("880"));
    }

    #[test]
    fn a_nonsense_width_does_not_produce_a_notice() {
        assert!(narrow_notice(f32::NAN).is_none());
    }
}
