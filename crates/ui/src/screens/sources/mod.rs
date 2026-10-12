//! The Sources destination: the PLAUD Cloud connector, the account's
//! recordings, watched folders and manual import.
//!
//! The connector half is rendered from one `plaud_probe` snapshot (§5.4): the
//! headline, the detail, the warnings and the disconnect wording are the
//! application's own strings and are rendered verbatim. The screen decides
//! which control a `next_action` becomes and nothing else.

pub mod connector;
pub mod folders;
pub mod presentation;
pub mod recordings;

use std::collections::{BTreeMap, BTreeSet};

use egui::Ui;
use plaudon_app::command::AppCommand;
use plaudon_app::dto::{PlaudConnectRequest, PlaudListingCompleteness};
use plaudon_app::dto::{
    PlaudConnectorSnapshot, PlaudDisconnectResponse, PlaudImportOutcome, PlaudImportRequest,
    PlaudItemSummary, PlaudListRequest, PlaudListResponse, SourceAddFolderRequest,
    SourceAddFolderResponse, SourceReconcileRequest, SourceReconcileResponse, SourceSyncRequest,
    SourceSyncResponse, SyncTrigger, WatchedFolderRule, PLAUD_CONNECT_INTENT,
};
use plaudon_domain::error::ErrorEnvelope;

use self::folders::WatchedFolderCard;
use self::presentation::{can_list, ControlRun};

/// One operation the screen can have in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SourceOp {
    /// `plaud_probe`.
    Probe,
    /// `plaud_connect`.
    Connect,
    /// `plaud_disconnect`.
    Disconnect,
    /// `plaud_list`.
    List,
    /// `plaud_import`.
    Import,
    /// `source_add_folder`.
    AddFolder,
    /// `source_sync`.
    Sync,
    /// `source_reconcile`.
    Reconcile,
}

/// Something the user asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceIntent {
    /// Look for the official tool and read its version.
    Probe,
    /// Hand authentication to the official tool.
    Connect,
    /// Drop the application's association with the account.
    Disconnect,
    /// List one page of the account's recordings.
    List {
        /// Cursor from the previous page, or `None` for the first.
        cursor: Option<String>,
    },
    /// Import the chosen cloud recordings.
    Import {
        /// External identifiers from the last listing.
        external_ids: Vec<String>,
    },
    /// Ask the host for a folder to watch.
    ChooseFolder,
    /// Register the chosen folder.
    AddFolder {
        /// Folder the host's directory picker returned.
        path: String,
        /// Whether subdirectories are included.
        recursive: bool,
        /// What the folder may do with the audio it imports.
        rule: WatchedFolderRule,
    },
    /// Run one sync of a source.
    Sync {
        /// Watched folder or connector source identifier.
        source_id: String,
        /// Why the sync runs.
        trigger: SyncTrigger,
    },
    /// Reconcile a source against the library.
    Reconcile {
        /// Watched folder or connector source identifier.
        source_id: String,
    },
    /// Open the file dialog and import what the user chooses.
    ImportAudioFiles,
    /// Open the privacy section, where Local-only Mode lives.
    OpenPrivacySettings,
    /// Open a vendor documentation page in the browser.
    OpenUrl(&'static str),
}

impl SourceIntent {
    /// Which operation this intent is, for pending and error bookkeeping.
    #[must_use]
    pub const fn op(&self) -> Option<SourceOp> {
        match self {
            Self::Probe => Some(SourceOp::Probe),
            Self::Connect => Some(SourceOp::Connect),
            Self::Disconnect => Some(SourceOp::Disconnect),
            Self::List { .. } => Some(SourceOp::List),
            Self::Import { .. } => Some(SourceOp::Import),
            Self::AddFolder { .. } => Some(SourceOp::AddFolder),
            Self::Sync { .. } => Some(SourceOp::Sync),
            Self::Reconcile { .. } => Some(SourceOp::Reconcile),
            Self::ChooseFolder
            | Self::ImportAudioFiles
            | Self::OpenPrivacySettings
            | Self::OpenUrl(_) => None,
        }
    }

    /// The command this intent dispatches, if it is one.
    ///
    /// The only place this screen names a command. A malformed identifier is
    /// not a command: `source_sync` and `source_reconcile` take a UUID, and an
    /// identifier that is not one never reaches the application.
    #[must_use]
    pub fn into_command(self) -> Option<AppCommand> {
        match self {
            Self::Probe => Some(AppCommand::PlaudProbe),
            Self::Connect => Some(AppCommand::PlaudConnect(PlaudConnectRequest {
                user_intent: PLAUD_CONNECT_INTENT.to_owned(),
            })),
            Self::Disconnect => Some(AppCommand::PlaudDisconnect),
            Self::List { cursor } => Some(AppCommand::PlaudList(PlaudListRequest { cursor })),
            Self::Import { external_ids } => {
                Some(AppCommand::PlaudImport(PlaudImportRequest { external_ids }))
            }
            Self::AddFolder {
                path,
                recursive,
                rule,
            } => Some(AppCommand::SourceAddFolder(SourceAddFolderRequest {
                path,
                recursive,
                rule,
            })),
            Self::Sync { source_id, trigger } => Some(AppCommand::SourceSync(SourceSyncRequest {
                source_id,
                trigger,
            })),
            Self::Reconcile { source_id } => {
                Some(AppCommand::SourceReconcile(SourceReconcileRequest {
                    source_id,
                }))
            }
            Self::ChooseFolder
            | Self::ImportAudioFiles
            | Self::OpenPrivacySettings
            | Self::OpenUrl(_) => None,
        }
    }
}

/// An answer the host received for an intent.
#[derive(Debug)]
pub enum SourceOutcome {
    /// `plaud_probe` or `plaud_connect` answered.
    Snapshot(PlaudConnectorSnapshot),
    /// `plaud_disconnect` answered.
    Disconnected(PlaudDisconnectResponse),
    /// `plaud_list` answered.
    Listed(PlaudListResponse),
    /// `plaud_import` answered.
    Imported(Vec<PlaudImportOutcome>),
    /// `source_add_folder` answered.
    FolderAdded(SourceAddFolderResponse),
    /// `source_sync` answered.
    Synced(SourceSyncResponse),
    /// `source_reconcile` answered.
    Reconciled(SourceReconcileResponse),
    /// An operation failed with the §15.4 envelope.
    Failed {
        /// Which operation failed.
        op: SourceOp,
        /// The envelope, rendered verbatim.
        error: ErrorEnvelope,
    },
}

/// Everything the Sources destination renders.
#[derive(Debug, Default)]
pub struct SourcesState {
    snapshot: Option<PlaudConnectorSnapshot>,
    listing: Option<PlaudListResponse>,
    items: Vec<PlaudItemSummary>,
    chosen: BTreeSet<String>,
    outcomes: BTreeMap<String, PlaudImportOutcome>,
    folders: Vec<WatchedFolderCard>,
    folder_path: Option<String>,
    folder_rule: Option<WatchedFolderRule>,
    folder_recursive: bool,
    pending: BTreeSet<SourceOp>,
    errors: BTreeMap<SourceOp, ErrorEnvelope>,
}

impl SourcesState {
    /// A screen that has not probed yet.
    #[must_use]
    pub fn new() -> Self {
        Self {
            folder_rule: Some(WatchedFolderRule::ImportAndQueueLocalStt),
            ..Self::default()
        }
    }

    /// The command the screen needs before it can render anything.
    #[must_use]
    pub fn initial_intent() -> SourceIntent {
        SourceIntent::Probe
    }

    /// The connector snapshot, if one has arrived.
    #[must_use]
    pub const fn snapshot(&self) -> Option<&PlaudConnectorSnapshot> {
        self.snapshot.as_ref()
    }

    /// The recordings listed so far.
    #[must_use]
    pub fn items(&self) -> &[PlaudItemSummary] {
        &self.items
    }

    /// The watched folders this session registered or synced.
    #[must_use]
    pub fn folders(&self) -> &[WatchedFolderCard] {
        &self.folders
    }

    /// Whether `op` is waiting for an answer.
    #[must_use]
    pub fn is_pending(&self, op: SourceOp) -> bool {
        self.pending.contains(&op)
    }

    /// Whether anything at all is in flight, which is what disables the
    /// connector's controls.
    #[must_use]
    pub fn is_busy(&self) -> bool {
        !self.pending.is_empty()
    }

    /// The last failure of `op`.
    #[must_use]
    pub fn error(&self, op: SourceOp) -> Option<&ErrorEnvelope> {
        self.errors.get(&op)
    }

    /// The host's directory picker returned `path`.
    pub fn folder_chosen(&mut self, path: impl Into<String>) {
        self.folder_path = Some(path.into());
    }

    /// Feed an answer back into the screen.
    pub fn applied(&mut self, outcome: SourceOutcome) {
        match outcome {
            SourceOutcome::Snapshot(snapshot) => {
                self.pending.remove(&SourceOp::Probe);
                self.pending.remove(&SourceOp::Connect);
                self.snapshot = Some(snapshot);
            }
            SourceOutcome::Disconnected(response) => {
                self.pending.remove(&SourceOp::Disconnect);
                self.snapshot = Some(response.snapshot);
                self.listing = None;
                self.items.clear();
                self.chosen.clear();
                self.outcomes.clear();
            }
            SourceOutcome::Listed(page) => {
                self.pending.remove(&SourceOp::List);
                if page.items.is_empty() || self.listing.is_none() {
                    self.items.clear();
                    self.chosen.clear();
                }
                for item in &page.items {
                    if !self
                        .items
                        .iter()
                        .any(|known| known.external_id == item.external_id)
                    {
                        self.items.push(item.clone());
                    }
                }
                self.listing = Some(page);
            }
            SourceOutcome::Imported(results) => {
                self.pending.remove(&SourceOp::Import);
                self.chosen.clear();
                for result in results {
                    self.outcomes
                        .insert(presentation::outcome_id(&result).to_owned(), result);
                }
            }
            SourceOutcome::FolderAdded(response) => {
                self.pending.remove(&SourceOp::AddFolder);
                let path = self.folder_path.take().unwrap_or_default();
                if let Some(existing) = self
                    .folders
                    .iter_mut()
                    .find(|folder| folder.source_id == response.source_id)
                {
                    existing.rule = response.rule;
                    existing.recursive = response.recursive;
                    if !path.is_empty() {
                        existing.path = path;
                    }
                } else {
                    self.folders.push(WatchedFolderCard {
                        source_id: response.source_id,
                        path,
                        rule: response.rule,
                        recursive: response.recursive,
                        last_sync: None,
                        last_reconcile: None,
                    });
                }
            }
            SourceOutcome::Synced(response) => {
                self.pending.remove(&SourceOp::Sync);
                if let Some(folder) = self
                    .folders
                    .iter_mut()
                    .find(|folder| folder.source_id == response.source_id)
                {
                    folder.last_sync = Some(response);
                }
            }
            SourceOutcome::Reconciled(response) => {
                self.pending.remove(&SourceOp::Reconcile);
                if let Some(folder) = self
                    .folders
                    .iter_mut()
                    .find(|folder| folder.source_id == response.source_id)
                {
                    folder.last_reconcile = Some(response);
                }
            }
            SourceOutcome::Failed { op, error } => {
                self.pending.remove(&op);
                self.errors.insert(op, error);
            }
        }
    }

    /// A `source.auth_required`, `source.sync_finished` or
    /// `recording.source_updated` event arrived: the connector's state and its
    /// warnings changed, so the snapshot is refetched. Nothing polls.
    pub fn source_event(&mut self) -> SourceIntent {
        self.pending.insert(SourceOp::Probe);
        self.errors.remove(&SourceOp::Probe);
        SourceIntent::Probe
    }
}

/// Render the Sources destination and collect what the user asked for.
pub fn sources_screen(ui: &mut Ui, state: &mut SourcesState) -> Vec<SourceIntent> {
    let mut intents = Vec::new();

    ui.heading("PLAUD Cloud and sources");
    ui.label(
        "A direct Bluetooth transfer and this account connector are separate routes. A direct transfer never falls back to the cloud on its own, and this connector only runs when you ask it to.",
    );

    if state.is_pending(SourceOp::Probe) && state.snapshot.is_none() {
        ui.label("Checking the PLAUD connector…");
    }
    if let Some(error) = state.error(SourceOp::Probe) {
        if state.snapshot.is_none() {
            ui.label(error.message.clone());
            if ui.button("Try again").clicked() {
                intents.push(SourceIntent::Probe);
            }
        }
    }

    if let Some(snapshot) = state.snapshot.clone() {
        let busy = state.is_busy();
        for run in connector::connector_panel(ui, &snapshot, busy) {
            match run {
                connector::ConnectorAction::Run(ControlRun::Connect) => {
                    intents.push(SourceIntent::Connect);
                }
                connector::ConnectorAction::Run(ControlRun::Retry) => {
                    intents.push(SourceIntent::Probe);
                }
                connector::ConnectorAction::Run(ControlRun::ReviewLocalOnlyMode) => {
                    intents.push(SourceIntent::OpenPrivacySettings);
                }
                connector::ConnectorAction::Run(ControlRun::ImportManually) => {
                    intents.push(SourceIntent::ImportAudioFiles);
                }
                connector::ConnectorAction::OpenUrl(url) => {
                    intents.push(SourceIntent::OpenUrl(url));
                }
                connector::ConnectorAction::Disconnect => {
                    intents.push(SourceIntent::Disconnect);
                }
            }
        }

        for op in [
            SourceOp::Connect,
            SourceOp::Disconnect,
            SourceOp::List,
            SourceOp::Import,
        ] {
            if let Some(error) = state.error(op) {
                ui.separator();
                ui.label(error.message.clone());
                ui.label(format!("Diagnostic id: {}", error.diagnostic_id));
            }
        }

        if can_list(snapshot.status.state) {
            ui.separator();
            let label = if state.listing.is_some() {
                "Refresh recordings"
            } else {
                "List my PLAUD recordings"
            };
            if ui
                .add_enabled(!state.is_busy(), egui::Button::new(label))
                .clicked()
            {
                intents.push(SourceIntent::List { cursor: None });
            }
        }

        if let Some(listing) = state.listing.clone() {
            let busy = state.is_busy();
            let actions = recordings::recordings_list(
                ui,
                &state.items,
                &listing,
                &state.chosen,
                &state.outcomes,
                busy,
            );
            for action in actions {
                match action {
                    recordings::RecordingAction::Toggle(id) => {
                        if !state.chosen.remove(&id) {
                            state.chosen.insert(id);
                        }
                    }
                    recordings::RecordingAction::ToggleAll => {
                        let selectable: Vec<String> = state
                            .items
                            .iter()
                            .filter(|item| item.imported_recording_id.is_none())
                            .map(|item| item.external_id.clone())
                            .collect();
                        if selectable.iter().all(|id| state.chosen.contains(id)) {
                            state.chosen.clear();
                        } else {
                            state.chosen = selectable.into_iter().collect();
                        }
                    }
                    recordings::RecordingAction::Import => {
                        intents.push(SourceIntent::Import {
                            external_ids: state.chosen.iter().cloned().collect(),
                        });
                    }
                    recordings::RecordingAction::LoadMore(cursor) => {
                        intents.push(SourceIntent::List {
                            cursor: Some(cursor),
                        });
                    }
                    recordings::RecordingAction::ImportManually => {
                        intents.push(SourceIntent::ImportAudioFiles);
                    }
                    recordings::RecordingAction::Reconcile => {
                        intents.push(SourceIntent::Reconcile {
                            source_id: listing.source_id.clone(),
                        });
                    }
                }
            }
            if listing.completeness == PlaudListingCompleteness::PossiblyIncomplete {
                ui.label(
                    "Because this listing may be partial, nothing is reconciled from it: a partial listing cannot prove a recording is gone.",
                );
            }
        }
    }

    folders::watched_folders(ui, state, &mut intents);
    manual_import(ui, &mut intents);

    for intent in &intents {
        if let Some(op) = intent.op() {
            state.pending.insert(op);
            state.errors.remove(&op);
        }
    }
    intents
}

/// Manual import: the route that always works.
fn manual_import(ui: &mut Ui, intents: &mut Vec<SourceIntent>) {
    ui.separator();
    ui.heading("Import files from this computer");
    ui.label(
        "Choose audio files yourself. This works without hardware, an account or a network connection, and it is unaffected by Local-only Mode.",
    );
    if ui.button("Choose audio files").clicked() {
        intents.push(SourceIntent::ImportAudioFiles);
    }
}
