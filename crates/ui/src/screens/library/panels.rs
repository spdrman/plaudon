//! How the recording detail view's three working panels are supplied.
//!
//! The container owns the frame; the panels are plugged in. Until a panel's
//! widget exists, the registry convention of [`crate::shell::screen`] applies:
//! the panel says which task owns it, so "not built yet" can never be read as
//! "built and empty".

use egui::Ui;
use plaudon_app::dto::RecordingDetail;

use crate::screens::recording::DetailPanels;

/// The panels a host that has no player, transcript or review widget yet can
/// supply. Each one names the task that owns it.
#[derive(Debug, Default)]
pub struct UnbuiltPanels;

fn pending(ui: &mut Ui, what: &str, owner: &str) {
    ui.label(format!("{what} is not part of this build yet ({owner})."));
}

impl DetailPanels for UnbuiltPanels {
    fn player(&mut self, ui: &mut Ui, _detail: &RecordingDetail) {
        pending(ui, "Playback", "EPIC O task O1.5, issue #203");
    }

    fn transcript(&mut self, ui: &mut Ui, _detail: &RecordingDetail) {
        pending(ui, "The transcript pane", "EPIC O task O2.1, issue #205");
    }

    fn outputs(&mut self, ui: &mut Ui, _detail: &RecordingDetail) {
        pending(
            ui,
            "Summary, actions and decisions",
            "EPIC O task O2.2, issue #206",
        );
    }
}
