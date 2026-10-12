//! Watched folders (matrix row D16, spec §4.3).
//!
//! The commands exist — `source_add_folder`, `source_sync`,
//! `source_reconcile` — and until now nothing rendered them. This panel adds a
//! folder with its rule, runs a sync and a reconcile, and shows the counts the
//! pass returned, including whether the bounded sweep actually saw the whole
//! folder.
//!
//! One honest limitation is stated in the panel itself: this build publishes no
//! command that lists the watched folders stored on disk, so the list shows the
//! folders this session registered or synced. Adding a folder again is safe —
//! `source_add_folder` returns the row already registered for that root and
//! restates its policy — which is the way to bring an older folder back into
//! the list.

use egui::Ui;
use plaudon_app::dto::{
    SourceReconcileResponse, SourceSyncResponse, SyncTrigger, WatchedFolderRule,
};

use super::presentation::{rule_label, RULES};
use super::{SourceIntent, SourceOp, SourcesState};

/// One watched folder as this session knows it.
#[derive(Debug, Clone)]
pub struct WatchedFolderCard {
    /// The watched folder's source identifier.
    pub source_id: String,
    /// The folder the user picked.
    pub path: String,
    /// What the folder may do with the audio it imports.
    pub rule: WatchedFolderRule,
    /// Whether subdirectories are included.
    pub recursive: bool,
    /// The last sync of this folder, if one ran in this session.
    pub last_sync: Option<SourceSyncResponse>,
    /// The last reconcile of this folder, if one ran in this session.
    pub last_reconcile: Option<SourceReconcileResponse>,
}

/// Render the watched-folder section.
pub fn watched_folders(ui: &mut Ui, state: &mut SourcesState, intents: &mut Vec<SourceIntent>) {
    ui.separator();
    ui.heading("Watched folders");
    ui.label(
        "A watched folder imports audio that appears in it. Nothing is deleted or moved: the files stay where they are.",
    );
    ui.label(
        "This list shows the folders registered or synced in this session. This build has no command that lists the folders already stored, so add a folder again to bring it back into the list — doing so returns the same folder and restates the rule you choose.",
    );

    /* ------------------------------------------------------------- add */

    if ui
        .add_enabled(
            !state.is_pending(SourceOp::AddFolder),
            egui::Button::new("Choose a folder to watch"),
        )
        .clicked()
    {
        intents.push(SourceIntent::ChooseFolder);
    }

    if let Some(path) = state.folder_path.clone() {
        ui.label(format!("Folder: {path}"));
        let mut recursive = state.folder_recursive;
        if ui.checkbox(&mut recursive, "Include subfolders").changed() {
            state.folder_recursive = recursive;
        }
        ui.label("What may this folder do with the audio it imports?");
        for rule in RULES {
            if ui
                .radio(state.folder_rule == Some(rule), rule_label(rule))
                .clicked()
            {
                state.folder_rule = Some(rule);
            }
        }
        let rule = state.folder_rule;
        if ui
            .add_enabled(
                rule.is_some() && !state.is_pending(SourceOp::AddFolder),
                egui::Button::new("Watch this folder"),
            )
            .clicked()
        {
            if let Some(rule) = rule {
                intents.push(SourceIntent::AddFolder {
                    path,
                    recursive: state.folder_recursive,
                    rule,
                });
            }
        }
    }

    if let Some(error) = state.error(SourceOp::AddFolder) {
        ui.label(error.message.clone());
    }

    /* ----------------------------------------------------------- rows */

    if state.folders.is_empty() {
        ui.label("No folder is being watched in this session.");
        return;
    }

    let busy_sync = state.is_pending(SourceOp::Sync);
    let busy_reconcile = state.is_pending(SourceOp::Reconcile);
    let folders = state.folders.clone();
    for folder in &folders {
        ui.group(|ui| {
            ui.label(folder.path.clone());
            ui.label(rule_label(folder.rule));
            ui.label(if folder.recursive {
                "Subfolders are included."
            } else {
                "Only the folder itself is watched."
            });
            if let Some(sync) = folder.last_sync.as_ref() {
                if let Some(counts) = sync.folder.as_ref() {
                    ui.label(format!(
                        "Last sync: {} inspected · {} imported · {} already in the library · {} deferred · {} refused · {} rejected · {} no longer present",
                        counts.scanned,
                        counts.imported,
                        counts.already_imported,
                        counts.deferred,
                        counts.refused_tombstoned,
                        counts.rejected,
                        counts.vanished
                    ));
                    if !counts.inventory_complete {
                        ui.label(
                            "The sweep is bounded and did not see the whole folder, so this is not a complete picture of it. Sync again to continue.",
                        );
                    }
                }
                ui.label(format!("Last sync finished as: {:?}", sync.state));
            }
            if let Some(reconcile) = folder.last_reconcile.as_ref() {
                ui.label(format!(
                    "Last reconcile: {} decision(s)",
                    reconcile.decisions.len()
                ));
                for decision in &reconcile.decisions {
                    ui.label(format!(
                        "{} → {}",
                        decision.event,
                        decision.actions.join(", ")
                    ));
                }
            }
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!busy_sync, egui::Button::new("Sync now"))
                    .clicked()
                {
                    intents.push(SourceIntent::Sync {
                        source_id: folder.source_id.clone(),
                        trigger: SyncTrigger::Manual,
                    });
                }
                if ui
                    .add_enabled(!busy_reconcile, egui::Button::new("Reconcile with my library"))
                    .clicked()
                {
                    intents.push(SourceIntent::Reconcile {
                        source_id: folder.source_id.clone(),
                    });
                }
            });
        });
    }

    if let Some(error) = state.error(SourceOp::Sync) {
        ui.label(error.message.clone());
    }
    if let Some(error) = state.error(SourceOp::Reconcile) {
        ui.label(error.message.clone());
    }
}
