//! Duplicate resolution for manual import (§7.4).
//!
//! The match is by content hash, and the dialog says so out loud: a renamed
//! copy of the same audio still matches, and two different recordings that
//! happen to share a file name do not. Every file needs an explicit decision —
//! nothing is imported or skipped on the user's behalf, which is why there is
//! no default button and why "Decide later" leaves the files listed.

use egui::{Id, RichText, Ui};
use plaudon_app::dto::ImportOutcome;

use super::format::short_hash;
use super::import::{DuplicateChoice, ImportSession};

/// What the user decided in the dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DuplicateDecision {
    /// Apply this choice to this path.
    Chose {
        /// The path the decision is about.
        path: String,
        /// The decision.
        choice: DuplicateChoice,
    },
    /// Close the dialog and leave every file undecided.
    DecideLater,
}

/// Show the dialog when the session has undecided duplicates.
///
/// Returns the user's decision, if they made one this frame.
pub fn duplicate_dialog(ui: &mut Ui, session: &ImportSession) -> Option<DuplicateDecision> {
    let duplicates = session.duplicates();
    if duplicates.is_empty() || !session.wants_duplicate_dialog() {
        return None;
    }
    let title = if duplicates.len() == 1 {
        "This audio is already in your library".to_owned()
    } else {
        format!("{} files are already in your library", duplicates.len())
    };

    let modal = egui::Modal::new(Id::new("library_duplicate_dialog")).show(ui.ctx(), |ui| {
        let mut decision = None;
        let heading = ui.heading(&title);
        heading.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Label, true, title.as_str())
        });
        ui.ctx().accesskit_node_builder(heading.id, |node| {
            node.set_role(egui::accesskit::Role::Dialog);
            node.set_label(title.clone());
        });
        ui.label(
            "Matched by content hash (SHA-256) of the audio itself, not by file name. A renamed \
             copy of the same recording still matches, and a different recording with the same \
             name does not.",
        );
        ui.separator();

        for duplicate in duplicates {
            let ImportOutcome::Duplicate {
                path,
                sha256,
                existing_title,
                ..
            } = duplicate
            else {
                continue;
            };
            ui.label(RichText::new(path).strong());
            ui.label(format!(
                "Matches {existing_title} · content hash {}",
                short_hash(sha256)
            ));
            ui.horizontal(|ui| {
                if ui.button("Import anyway as a separate recording").clicked() {
                    decision = Some(DuplicateDecision::Chose {
                        path: path.clone(),
                        choice: DuplicateChoice::ImportAnyway,
                    });
                }
                if ui.button("Open existing recording").clicked() {
                    decision = Some(DuplicateDecision::Chose {
                        path: path.clone(),
                        choice: DuplicateChoice::OpenExisting,
                    });
                }
                if ui.button("Skip").clicked() {
                    decision = Some(DuplicateDecision::Chose {
                        path: path.clone(),
                        choice: DuplicateChoice::Skip,
                    });
                }
            });
            ui.separator();
        }

        if ui.button("Decide later").clicked() {
            decision = Some(DuplicateDecision::DecideLater);
        }
        decision
    });

    if modal.should_close() {
        return Some(DuplicateDecision::DecideLater);
    }
    modal.inner
}
