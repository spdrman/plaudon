//! The account's recordings, with selection, import and the completeness
//! disclosure (§4.2.4–5, §7.2, §7.5).

use std::collections::{BTreeMap, BTreeSet};

use egui::Ui;
use plaudon_app::dto::{
    PlaudImportOutcome, PlaudItemSummary, PlaudListResponse, PlaudListingCompleteness,
};

use crate::screens::devices::badge::badge_inline;
use crate::screens::devices::presentation::{Badge, Tone};

use super::presentation::{duration_label, listing_badge, outcome_badge};

/// What the user did in the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordingAction {
    /// Select or deselect one recording.
    Toggle(String),
    /// Select or deselect everything that can still be imported.
    ToggleAll,
    /// Import the selection.
    Import,
    /// Fetch the next page.
    LoadMore(String),
    /// Open the manual file import instead.
    ImportManually,
    /// Reconcile the library against this listing.
    Reconcile,
}

/// Render the listing.
pub fn recordings_list(
    ui: &mut Ui,
    items: &[PlaudItemSummary],
    listing: &PlaudListResponse,
    chosen: &BTreeSet<String>,
    outcomes: &BTreeMap<String, PlaudImportOutcome>,
    busy: bool,
) -> Vec<RecordingAction> {
    let mut actions = Vec::new();

    if items.is_empty() {
        ui.separator();
        ui.heading("No recordings found");
        // §7.5: an empty account is advice, not an error.
        if let Some(advisory) = listing.empty_account_advisory.as_deref() {
            ui.label(advisory);
        }
        if ui.button("Import audio files instead").clicked() {
            actions.push(RecordingAction::ImportManually);
        }
        return actions;
    }

    ui.separator();
    ui.horizontal(|ui| {
        ui.heading("PLAUD recordings");
        let (badge, word) = listing_badge(listing.completeness);
        badge_inline(ui, badge, word);
    });

    let selectable: Vec<&PlaudItemSummary> = items
        .iter()
        .filter(|item| item.imported_recording_id.is_none())
        .collect();
    let mut all_selected = !selectable.is_empty()
        && selectable
            .iter()
            .all(|item| chosen.contains(&item.external_id));
    if ui
        .add_enabled_ui(!selectable.is_empty() && !busy, |ui| {
            ui.checkbox(&mut all_selected, "Select all")
        })
        .inner
        .changed()
    {
        actions.push(RecordingAction::ToggleAll);
    }

    for item in items {
        let in_library = item.imported_recording_id.is_some();
        ui.horizontal(|ui| {
            let mut selected = chosen.contains(&item.external_id);
            if ui
                .add_enabled_ui(!in_library && !busy, |ui| {
                    ui.checkbox(&mut selected, item.display_name.clone())
                })
                .inner
                .changed()
            {
                actions.push(RecordingAction::Toggle(item.external_id.clone()));
            }
            if let Some(duration) = duration_label(item.duration_ms) {
                ui.label(duration);
            } else {
                ui.label("Duration unknown");
            }
            if in_library {
                badge_inline(
                    ui,
                    Badge {
                        glyph: "✓",
                        tone: Tone::Success,
                    },
                    "In library",
                );
            }
            if let Some(outcome) = outcomes.get(&item.external_id) {
                let (badge, word) = outcome_badge(outcome);
                badge_inline(ui, badge, word);
            }
        });
        if let Some(PlaudImportOutcome::Failed { error, .. }) = outcomes.get(&item.external_id) {
            ui.label(error.message.clone());
        }
    }

    ui.horizontal(|ui| {
        if ui
            .add_enabled(
                !busy && !chosen.is_empty(),
                egui::Button::new(format!("Import selected ({})", chosen.len())),
            )
            .clicked()
        {
            actions.push(RecordingAction::Import);
        }
        if let Some(cursor) = listing.next_cursor.clone() {
            if ui
                .add_enabled(!busy, egui::Button::new("Load more recordings"))
                .clicked()
            {
                actions.push(RecordingAction::LoadMore(cursor));
            }
        }
        if listing.completeness == PlaudListingCompleteness::Complete
            && ui
                .add_enabled(!busy, egui::Button::new("Reconcile with my library"))
                .clicked()
        {
            actions.push(RecordingAction::Reconcile);
        }
    });

    actions
}
