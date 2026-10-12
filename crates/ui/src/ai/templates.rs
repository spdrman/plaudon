//! The templates surface (matrix row A12).
//!
//! There is no template backend: no published command lists templates,
//! stores a user-authored one or selects one for a run, and the summarization
//! pipeline uses the template its own configuration names. The React shell
//! shipped a placeholder screen for this, and the honest thing is to keep a
//! placeholder that says precisely what is missing rather than render a picker
//! that would write nowhere.
//!
//! What a real screen needs, and does not have:
//!
//! * a command that lists the available templates with their versions;
//! * a command that stores a user-authored template; and
//! * a field on `start_processing` that selects one for a run.
//!
//! Until those exist, the template facts a user can see are the ones a
//! produced output carries, which the summarization panel and the outputs
//! viewer already show.

use egui::Ui;

/// Why the templates screen has nothing to offer yet.
pub const TEMPLATES_UNAVAILABLE: &str =
    "Templates are fixed in this build. No command publishes the template catalogue, stores a \
     custom template or selects one for a run, so there is nothing here to choose between. Each \
     generated output shows the template and template version it was produced with.";

/// The honest placeholder.
#[derive(Debug, Default)]
pub struct TemplatesPlaceholder;

impl TemplatesPlaceholder {
    /// Draw the placeholder.
    pub fn ui(&mut self, ui: &mut Ui) {
        ui.label(egui::RichText::new("Templates").heading());
        ui.label(TEMPLATES_UNAVAILABLE);
        ui.label(
            "A template picker needs three things that do not exist yet: a command that lists \
             templates, a command that stores one, and a way to select one when a stage is \
             started.",
        );
    }
}
