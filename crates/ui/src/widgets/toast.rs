//! The toast stack (parity row S13).
//!
//! `ToastRegion.tsx` announced toasts through a polite live region, never
//! stole focus and never auto-dismissed. The same three rules hold here: the
//! stack is an anchored [`egui::Area`] whose container carries
//! `Role::Status` with `Live::Polite`, nothing in it takes focus when it
//! appears, and every toast is dismissed by its own button.
//!
//! Toasts are presentation only — the application already records what
//! happened in the activity history, so losing one costs nothing.

use egui::{accesskit, Align2, Color32, RichText};

use crate::theme::Tokens;

/// How many toasts are kept. Older ones are dropped, oldest first: a stack
/// taller than the window is not a notification, it is a wall.
pub const MAX_TOASTS: usize = 5;

/// The tone of a toast, which selects its colour and its glyph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastTone {
    /// Neutral information.
    Info,
    /// Something finished.
    Success,
    /// Something finished, with a caveat the user must read.
    Warning,
    /// Something failed.
    Danger,
}

impl ToastTone {
    /// The text glyph shown before the title.
    ///
    /// Text, not colour: §5.3 forbids colour from being the only carrier of
    /// meaning, and the React shell used the same four marks.
    #[must_use]
    pub const fn glyph(self) -> &'static str {
        match self {
            Self::Info => "i",
            Self::Success => "OK",
            Self::Warning => "!",
            Self::Danger => "X",
        }
    }

    /// The word a screen reader announces before the title.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Info => "Information",
            Self::Success => "Success",
            Self::Warning => "Warning",
            Self::Danger => "Error",
        }
    }

    fn color(self, tokens: Tokens) -> Color32 {
        match self {
            Self::Info => tokens.color.status_neutral,
            Self::Success => tokens.color.status_success,
            Self::Warning => tokens.color.status_warning,
            Self::Danger => tokens.color.status_danger,
        }
    }
}

/// One message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    /// Identifies this toast for dismissal.
    pub id: u64,
    /// Tone.
    pub tone: ToastTone,
    /// One line, always present.
    pub title: String,
    /// Optional detail.
    pub body: Option<String>,
    /// The corrective controls the toast offers, when it offers any.
    pub action: Option<ToastAction>,
}

/// Corrective controls a toast can carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastAction {
    /// "Install a model" and "Import a model file", for a failure that needs a
    /// speech model (issue #305).
    InstallSpeechModel,
}

/// What the user pressed on a toast, for the shell to carry out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastChoice {
    /// Go to the engine setup, where a model can be installed.
    InstallSpeechModel,
    /// Pick a model file and import it.
    ImportModelFile,
}

/// The stack of messages currently showing.
#[derive(Debug, Default)]
pub struct Toasts {
    items: Vec<Toast>,
    next_id: u64,
}

impl Toasts {
    /// An empty stack.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a toast and return its id.
    pub fn push(&mut self, tone: ToastTone, title: impl Into<String>, body: Option<String>) -> u64 {
        self.push_with_action(tone, title, body, None)
    }

    /// Add a toast that offers corrective controls, and return its id.
    pub fn push_with_action(
        &mut self,
        tone: ToastTone,
        title: impl Into<String>,
        body: Option<String>,
        action: Option<ToastAction>,
    ) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        self.items.push(Toast {
            id,
            tone,
            title: title.into(),
            body,
            action,
        });
        if self.items.len() > MAX_TOASTS {
            self.items.remove(0);
        }
        id
    }

    /// Remove one toast.
    pub fn dismiss(&mut self, id: u64) {
        self.items.retain(|toast| toast.id != id);
    }

    /// Remove all of them.
    pub fn clear(&mut self) {
        self.items.clear();
    }

    /// The toasts currently showing, oldest first.
    #[must_use]
    pub fn items(&self) -> &[Toast] {
        &self.items
    }

    /// Whether anything is showing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Paint the stack in the bottom-right corner of `ctx`, returning what
    /// the user pressed on one, if anything. A pressed control dismisses its
    /// toast: the user has acted on it.
    pub fn ui(&mut self, ctx: &egui::Context) -> Option<ToastChoice> {
        if self.items.is_empty() {
            return None;
        }
        let mut choice = None;
        let tokens = Tokens::get(ctx);
        let mut dismissed = None;
        egui::Area::new(egui::Id::new("plaudon-toasts"))
            .anchor(Align2::RIGHT_BOTTOM, egui::vec2(-16.0, -16.0))
            .interactable(true)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                let (region, _) = ui.allocate_space(egui::Vec2::ZERO);
                ui.ctx().accesskit_node_builder(region, |node| {
                    node.set_role(accesskit::Role::Status);
                    node.set_live(accesskit::Live::Polite);
                    node.set_label("Notifications");
                });
                for toast in &self.items {
                    egui::Frame::new()
                        .fill(tokens.color.bg_raised)
                        .stroke(egui::Stroke::new(1.0, tokens.color.border_subtle))
                        .corner_radius(tokens.radius.medium)
                        .inner_margin(tokens.space(4))
                        .show(ui, |ui| {
                            ui.set_max_width(360.0);
                            ui.vertical(|ui| {
                                ui.label(
                                    RichText::new(format!(
                                        "{} {}: {}",
                                        toast.tone.glyph(),
                                        toast.tone.label(),
                                        toast.title
                                    ))
                                    .color(toast.tone.color(tokens))
                                    .strong(),
                                );
                                if let Some(body) = &toast.body {
                                    ui.label(
                                        RichText::new(body).color(tokens.color.text_secondary),
                                    );
                                }
                                if toast.action == Some(ToastAction::InstallSpeechModel) {
                                    ui.horizontal_wrapped(|ui| {
                                        if ui.button("Install a model").clicked() {
                                            choice = Some(ToastChoice::InstallSpeechModel);
                                            dismissed = Some(toast.id);
                                        }
                                        if ui.button("Import a model file").clicked() {
                                            choice = Some(ToastChoice::ImportModelFile);
                                            dismissed = Some(toast.id);
                                        }
                                    });
                                }
                                if ui.button("Dismiss").clicked() {
                                    dismissed = Some(toast.id);
                                }
                            });
                        });
                    ui.add_space(tokens.space(2));
                }
            });
        if let Some(id) = dismissed {
            self.dismiss(id);
        }
        choice
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_toast_is_dismissed_by_id_and_nothing_else() {
        let mut toasts = Toasts::new();
        let first = toasts.push(ToastTone::Info, "Imported", None);
        let second = toasts.push(ToastTone::Danger, "Failed", Some("details".to_owned()));
        assert_eq!(toasts.items().len(), 2);
        toasts.dismiss(first);
        assert_eq!(toasts.items().len(), 1);
        assert_eq!(toasts.items()[0].id, second);
        toasts.dismiss(first);
        assert_eq!(toasts.items().len(), 1);
    }

    #[test]
    fn the_stack_is_bounded_and_drops_the_oldest() {
        let mut toasts = Toasts::new();
        for index in 0..MAX_TOASTS + 3 {
            toasts.push(ToastTone::Info, format!("toast {index}"), None);
        }
        assert_eq!(toasts.items().len(), MAX_TOASTS);
        assert_eq!(toasts.items()[0].title, "toast 3");
    }

    #[test]
    fn every_tone_carries_a_glyph_and_a_word_so_colour_is_never_the_only_signal() {
        for tone in [
            ToastTone::Info,
            ToastTone::Success,
            ToastTone::Warning,
            ToastTone::Danger,
        ] {
            assert_ne!(tone.glyph(), "");
            assert_ne!(tone.label(), "");
        }
    }
}
