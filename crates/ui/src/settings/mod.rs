//! The Settings destination (EPIC O task O2.4, matrix rows C1–C5, C9, A9,
//! A13, J16 and the N2.2 update state).
//!
//! The screen is a list of labelled sections, each one a [`Section`]. A
//! section renders from [`SettingsData`] — the cached answers to
//! `get_capabilities`, `get_shortcuts`, `list_local_models` and
//! `get_update_status` — and raises [`SettingsIntent`]s. It never dispatches a
//! command, never opens a file dialog and never reads the keychain, so a
//! kittest test can render any section, press its controls and assert on what
//! it asked for without a runtime, a database or a window manager.
//!
//! Three obligations are structural rather than conventional:
//!
//! * **The key is never a value.** The OpenRouter section knows one bit —
//!   present or not — because that is all `get_capabilities` returns.
//! * **The backend owns the words.** Where the application has a sentence for
//!   something (the enforced egress policy, a protection disclosure, an
//!   engine's unavailability reason), the section renders that sentence; it
//!   does not write its own.
//! * **Status is never colour alone.** Every badge carries a glyph and a word
//!   as well as a tone.

pub mod ai_key;
pub mod appearance;
pub mod backup;
pub mod consent;
pub mod destination;
pub mod engines;
pub mod export;
pub mod intent;
pub mod links;
pub mod models;
pub mod notifications;
pub mod privacy;
pub mod section;
pub mod shell_adapter;
pub mod shortcuts;
pub mod storage;
pub mod templates;
pub mod theme;
pub mod updater;

use egui::Ui;
use plaudon_app::command::{AppCommand, AppResponse};
use plaudon_app::dto::{
    Capabilities, ListLocalModelsResponse, LocalOnlyModeResponse, OsNotificationPermission,
    Preferences,
};
use plaudon_app::settings_presentation::UpdateStatus;
use plaudon_app::shortcuts::{ShortcutBindings, ShortcutSpec};
use plaudon_domain::error::ErrorEnvelope;

pub use destination::SettingsDestinationScreen;
pub use intent::{FilePurpose, FolderPurpose, SettingsDestination, SettingsIntent, SettingsOutput};
pub use shell_adapter::{OnboardingShellScreen, SettingsShellScreen};
pub use theme::{SettingsTokens, Tone, VisualsTokens};

/// A path the user chose in a native dialog, on its way back to the section
/// that asked for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickedPath {
    /// A folder.
    Folder(FolderPurpose),
    /// A file.
    File(FilePurpose),
}

/// Everything the settings screen has been told by the application.
///
/// Plain data: no handles, no futures, no database rows. `None` means "not
/// answered yet" and is rendered as such — never as a zero or an empty list
/// that would read as a fact.
#[derive(Debug, Default, Clone)]
pub struct SettingsData {
    /// The last `get_capabilities` answer.
    pub capabilities: Option<Capabilities>,
    /// The command registry the shortcut editor edits bindings for.
    pub shortcut_specs: Vec<ShortcutSpec>,
    /// The last `get_shortcuts` answer.
    pub shortcuts: ShortcutBindings,
    /// The last `list_local_models` answer.
    pub local_models: Option<ListLocalModelsResponse>,
    /// The last `get_update_status` answer.
    pub update: Option<UpdateStatus>,
    /// The policy the gate reported the last time Local-only Mode changed.
    /// `None` until it has, because before that the enforced policy is not
    /// known, only the stored preference.
    pub enforced_policy: Option<LocalOnlyModeResponse>,
    /// What the operating system said about notifications the last time a
    /// preference was written.
    pub notification_permission: Option<OsNotificationPermission>,
}

impl SettingsData {
    /// The stored preferences, when capabilities have arrived.
    #[must_use]
    pub fn preferences(&self) -> Option<&Preferences> {
        self.capabilities.as_ref().map(|c| &c.preferences)
    }

    /// Replace the preferences inside the cached capabilities.
    ///
    /// Every settings command answers with the stored preferences, so the
    /// screen stays current without a second round trip.
    pub fn apply_preferences(&mut self, preferences: Preferences) {
        if let Some(capabilities) = self.capabilities.as_mut() {
            capabilities.preferences = preferences;
        }
    }

    /// The commands that fill this cache, in the order the screen sends them.
    #[must_use]
    pub fn initial_commands() -> Vec<AppCommand> {
        vec![
            AppCommand::GetCapabilities,
            AppCommand::GetShortcuts,
            AppCommand::ListLocalModels,
            AppCommand::GetUpdateStatus,
        ]
    }

    /// Fold a command outcome into the cache.
    ///
    /// Returns `true` when the response was one of the screen's own reads, so
    /// the caller knows whether to repaint.
    pub fn absorb(&mut self, result: &Result<AppResponse, ErrorEnvelope>) -> bool {
        let Ok(response) = result else {
            return false;
        };
        match response {
            AppResponse::GetCapabilities(capabilities) => {
                self.capabilities = Some(capabilities.clone());
                true
            }
            AppResponse::GetShortcuts(bindings) | AppResponse::SetShortcuts(bindings) => {
                self.shortcuts = bindings.clone();
                true
            }
            AppResponse::ListLocalModels(models) => {
                self.local_models = Some(models.clone());
                true
            }
            AppResponse::GetUpdateStatus(status) => {
                self.update = Some(status.clone());
                true
            }
            AppResponse::SetLocalOnlyMode(policy) => {
                self.enforced_policy = Some(policy.clone());
                if let Some(capabilities) = self.capabilities.as_mut() {
                    capabilities.preferences.local_only_mode = policy.enabled;
                }
                true
            }
            AppResponse::SetNotificationSettings(response) => {
                self.notification_permission = Some(response.os_permission);
                if let Some(capabilities) = self.capabilities.as_mut() {
                    capabilities.preferences.notifications = response.settings.clone();
                }
                true
            }
            AppResponse::SetAppearance(preferences)
            | AppResponse::SetEnginePreferences(preferences)
            | AppResponse::SetExportDefaults(preferences)
            | AppResponse::CompleteOnboarding(preferences) => {
                self.apply_preferences(preferences.clone());
                true
            }
            _ => false,
        }
    }
}

/// One labelled section of the settings screen.
#[allow(unused_variables)]
pub trait Section: std::fmt::Debug {
    /// Heading, which is also the section's accessible name.
    fn title(&self) -> &'static str;

    /// Render the section.
    fn ui(
        &mut self,
        ui: &mut Ui,
        tokens: &dyn SettingsTokens,
        data: &SettingsData,
        out: &mut SettingsOutput,
    );

    /// Fold a command outcome into the section's own state.
    ///
    /// The shared cache is updated by [`SettingsData::absorb`] before this is
    /// called, so a section only has to handle what is private to it: its
    /// pending flag, its error envelope, its result panel.
    fn on_response(
        &mut self,
        command: &'static str,
        result: &Result<AppResponse, ErrorEnvelope>,
        out: &mut SettingsOutput,
    ) {
    }

    /// Receive a path the user chose in a native dialog.
    fn on_path_picked(&mut self, picked: PickedPath, path: &str, out: &mut SettingsOutput) {}
}

/// The Settings screen.
#[derive(Debug)]
pub struct SettingsScreen {
    data: SettingsData,
    sections: Vec<Box<dyn Section>>,
}

impl Default for SettingsScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl SettingsScreen {
    /// Every section, in the order the screen lists them.
    ///
    /// Appearance and privacy come first because they are what a first-time
    /// reader is looking for; the destructive and irreversible sections
    /// (backup and restore, local models) come last.
    #[must_use]
    pub fn new() -> Self {
        Self {
            data: SettingsData::default(),
            sections: vec![
                Box::new(appearance::AppearanceSection),
                Box::new(privacy::PrivacySection::default()),
                Box::new(ai_key::AiKeySection::default()),
                Box::new(consent::ConsentSection::default()),
                Box::new(notifications::NotificationsSection),
                Box::new(engines::EnginesSection),
                Box::new(templates::TemplatesSection),
                Box::new(export::ExportSection::default()),
                Box::new(shortcuts::ShortcutsSection::default()),
                Box::new(storage::StorageSection),
                Box::new(links::LinksSection),
                Box::new(models::LocalModelsSection::default()),
                Box::new(backup::BackupSection::default()),
                Box::new(updater::UpdateSection),
            ],
        }
    }

    /// The cached application state the sections render from.
    #[must_use]
    pub const fn data(&self) -> &SettingsData {
        &self.data
    }

    /// Mutable access, for the host that seeds the command registry.
    pub fn data_mut(&mut self) -> &mut SettingsData {
        &mut self.data
    }

    /// The reads the screen issues when it opens.
    #[must_use]
    pub fn initial_commands() -> Vec<AppCommand> {
        SettingsData::initial_commands()
    }

    /// Fold a command outcome into the screen.
    pub fn on_response(
        &mut self,
        command: &'static str,
        result: &Result<AppResponse, ErrorEnvelope>,
        out: &mut SettingsOutput,
    ) {
        self.data.absorb(result);
        for section in &mut self.sections {
            section.on_response(command, result, out);
        }
    }

    /// Hand a chosen path back to the section that asked for it.
    pub fn path_picked(&mut self, picked: PickedPath, path: &str, out: &mut SettingsOutput) {
        for section in &mut self.sections {
            section.on_path_picked(picked, path, out);
        }
    }

    /// Render the whole screen.
    pub fn ui(&mut self, ui: &mut Ui, out: &mut SettingsOutput) {
        let tokens = VisualsTokens::from_ui(ui);
        self.ui_with_tokens(ui, &tokens, out);
    }

    /// Render with tokens the caller supplies, which is how the shell passes
    /// its own palette in.
    pub fn ui_with_tokens(
        &mut self,
        ui: &mut Ui,
        tokens: &dyn SettingsTokens,
        out: &mut SettingsOutput,
    ) {
        ui.label(
            egui::RichText::new("Settings")
                .heading()
                .strong()
                .color(tokens.text()),
        );
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for index in 0..self.sections.len() {
                    let title = self.sections[index].title();
                    let data = &self.data;
                    let section = &mut self.sections[index];
                    section::section(ui, tokens, title, |ui| {
                        section.ui(ui, tokens, data, out);
                    });
                }
            });
    }

    /// The section titles, in order. For tests and for the in-page nav.
    #[must_use]
    pub fn section_titles(&self) -> Vec<&'static str> {
        self.sections
            .iter()
            .map(|section| section.title())
            .collect()
    }
}
