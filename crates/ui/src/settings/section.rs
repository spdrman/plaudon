//! The chrome every settings section shares.
//!
//! Keeping it in one place is what makes the accessibility obligations
//! uniform: a section is a labelled region, a status is a glyph *and* a word,
//! a destructive action is a modal the keyboard can reach and leave, and a
//! fact list is a term/value pair a screen reader reads in order.

use egui::{Align, Color32, Layout, Response, RichText, Ui};
use plaudon_app::settings_presentation::Fact;

use super::theme::{SettingsTokens, Tone};

/// Draw a labelled settings section.
///
/// The heading is rendered before the body so the AccessKit tree reads
/// "heading, then controls", which is the order the manual screen-reader
/// script walks.
pub fn section<R>(
    ui: &mut Ui,
    tokens: &dyn SettingsTokens,
    title: &str,
    body: impl FnOnce(&mut Ui) -> R,
) -> R {
    ui.push_id(title, |ui| {
        ui.add_space(12.0);
        ui.label(RichText::new(title).heading().strong().color(tokens.text()));
        let frame = egui::Frame::new()
            .fill(tokens.surface())
            .stroke(egui::Stroke::new(1.0, tokens.border()))
            .inner_margin(12)
            .corner_radius(6);
        frame
            .show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.set_width(ui.available_width());
                    body(ui)
                })
                .inner
            })
            .inner
    })
    .inner
}

/// Standing prose above a section's controls.
pub fn prose(ui: &mut Ui, tokens: &dyn SettingsTokens, text: &str) {
    ui.label(RichText::new(text).color(tokens.text()));
}

/// A quieter note: a qualification, a disclosure, a status line.
pub fn note(ui: &mut Ui, tokens: &dyn SettingsTokens, text: &str) {
    ui.label(RichText::new(text).small().color(tokens.muted_text()));
}

/// A status badge: tone, glyph and label.
///
/// The glyph is part of the accessible name, so the status survives being
/// read aloud and being looked at in greyscale.
pub fn status_badge(
    ui: &mut Ui,
    tokens: &dyn SettingsTokens,
    tone: Tone,
    glyph: &str,
    label: &str,
) {
    let color = tone.color(tokens);
    ui.label(RichText::new(format!("{glyph} {label}")).color(color));
}

/// A term/value list, the Rust form of the `<dl>` the React settings used.
pub fn facts(ui: &mut Ui, tokens: &dyn SettingsTokens, facts: &[Fact]) {
    for fact in facts {
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(format!("{}:", fact.label))
                    .strong()
                    .color(tokens.muted_text()),
            );
            ui.label(RichText::new(&fact.value).color(tokens.text()));
        });
    }
}

/// A single term/value row with a borrowed value.
pub fn fact_row(ui: &mut Ui, tokens: &dyn SettingsTokens, label: &str, value: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.label(
            RichText::new(format!("{label}:"))
                .strong()
                .color(tokens.muted_text()),
        );
        ui.label(RichText::new(value).color(tokens.text()));
    });
}

/// A primary action button.
pub fn primary_button(ui: &mut Ui, tokens: &dyn SettingsTokens, label: &str) -> Response {
    ui.add(
        egui::Button::new(RichText::new(label).color(contrasting(tokens.accent())))
            .fill(tokens.accent()),
    )
}

/// A destructive action button.
pub fn danger_button(ui: &mut Ui, tokens: &dyn SettingsTokens, label: &str) -> Response {
    ui.add(egui::Button::new(
        RichText::new(label).color(tokens.danger()),
    ))
}

/// Black or white, whichever reads on `background`.
fn contrasting(background: Color32) -> Color32 {
    let [r, g, b, _] = background.to_array();
    let luminance = 0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b);
    if luminance > 140.0 {
        Color32::BLACK
    } else {
        Color32::WHITE
    }
}

/// What a confirmation modal returned this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirmation {
    /// Still open.
    Pending,
    /// The user confirmed.
    Confirmed,
    /// The user backed out, with Escape or with the cancel button.
    Cancelled,
}

/// A destructive confirmation modal.
///
/// Escape cancels, which is the shell-wide rule for an overlay, and the
/// disclosure is always rendered: a destructive confirmation that does not say
/// what is lost is not a confirmation.
pub fn confirm_modal(
    ui: &mut Ui,
    tokens: &dyn SettingsTokens,
    title: &str,
    disclosure: &str,
    confirm_label: &str,
) -> Confirmation {
    let mut outcome = Confirmation::Pending;
    egui::Modal::new(egui::Id::new(("settings-confirm", title))).show(ui.ctx(), |ui| {
        ui.set_min_width(360.0);
        ui.label(RichText::new(title).heading().strong().color(tokens.text()));
        ui.add_space(8.0);
        ui.label(RichText::new(disclosure).color(tokens.text()));
        ui.add_space(12.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if danger_button(ui, tokens, confirm_label).clicked() {
                outcome = Confirmation::Confirmed;
            }
            if ui.button("Cancel").clicked() {
                outcome = Confirmation::Cancelled;
            }
        });
    });
    if ui.input(|input| input.key_pressed(egui::Key::Escape)) {
        outcome = Confirmation::Cancelled;
    }
    outcome
}

/// A single-line secret field.
///
/// `egui::TextEdit::password` keeps the characters off the screen; the value
/// still lives in the caller's `String`, which every caller clears the moment
/// the command that consumes it has been raised.
pub fn secret_field(ui: &mut Ui, label: &str, value: &mut String) -> Response {
    ui.add(
        egui::TextEdit::singleline(value)
            .password(true)
            .hint_text(label)
            .desired_width(280.0),
    )
    .labelled_by(ui.label(label).id)
}
