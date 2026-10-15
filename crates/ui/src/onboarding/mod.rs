//! First-run setup (matrix rows C8 and S15).
//!
//! A step-for-step port of `features/onboarding/OnboardingFlow.tsx`. The
//! ordering, skippability and the choices that end up in `complete_onboarding`
//! live in [`plaudon_app::onboarding`], which has its own unit tests; this
//! module is the renderer and nothing else.
//!
//! What the flow guarantees, and what every step is written to keep true:
//!
//! * **Setup touches nothing.** No network request, no operating-system
//!   permission prompt and no model download happens while these six questions
//!   are answered. Each command is raised only in direct response to a click.
//! * **Local-only is the default posture.** The machine starts with it on and
//!   records separately whether the question was actually answered, so "never
//!   asked" is never stored as "chose cloud".
//! * **The key is never a value.** The OpenRouter key is taken out of the
//!   draft field in the same statement that raises `set_openrouter_key`. It is
//!   not in [`Debug`], and the field that holds it is a password field, so it
//!   is not in the accessibility tree either.
//! * **Escape does not discard setup.** The key is consumed here so a
//!   shell-wide "Escape closes the overlay" rule cannot throw away a half
//!   answered first run.
//!
//! The screen is shaped like [`crate::settings::SettingsScreen`] — `ui`,
//! `ui_with_tokens`, `on_response`, and a [`crate::settings::SettingsOutput`]
//! of intents — so the shell wires both the same way.

use std::fmt;

use egui::{Label, RichText, Sense, Ui};
use plaudon_app::command::{AppCommand, AppResponse};
use plaudon_app::dto::{
    Capabilities, EngineStage, Platform, SetLocalOnlyModeRequest, SetOpenRouterKeyRequest,
};
use plaudon_app::onboarding::{
    engines_download_note, mode_summary, OnboardingAction, OnboardingState, OnboardingStep,
};
use plaudon_app::settings_presentation::{engine_rows, status_glyph, status_label, EngineRow};
use plaudon_domain::capability::CapabilityStatus;
use plaudon_domain::error::ErrorEnvelope;

use crate::permissions::{PermissionsPanel, PermissionsReport};
use crate::settings::engine_setup::EngineSetup;
use crate::settings::section;
use crate::settings::{
    PickedPath, SettingsIntent, SettingsOutput, SettingsTokens, Tone, VisualsTokens,
};

/// The heading that stands above every step.
const TITLE: &str = "Set up Plaudon";

/// What stays on this machine.
const PRIVACY_PROSE_1: &str = "Your recordings, transcripts and summaries stay on this computer \
unless you explicitly choose a cloud AI provider. Cloud processing is opt-in, per purpose, and \
never turned on for you.";

/// What setup itself does, which is nothing.
const PRIVACY_PROSE_2: &str = "Plaudon asks for no network access and no operating-system \
permission during setup. Audio is read from files you pick; nothing is uploaded while you answer \
these questions.";

/// Summary of the privacy disclosure.
const PRIVACY_DISCLOSURE: &str = "What would leave this machine if you opt in later";

/// The three bullets behind the disclosure.
const PRIVACY_BULLETS: &[&str] = &[
    "Transcript text you send to a cloud text model, for that request only.",
    "The model and provider you chose, so the request can be routed.",
    "Never: audio files, file paths, your API key, or anything you did not select.",
];

/// The local-only choice.
const MODE_LOCAL: &str =
    "Local only (recommended) — all outbound network access is refused by the backend.";

/// The cloud-later choice, which grants nothing by itself.
const MODE_CLOUD: &str = "Allow cloud AI later — choosing this now grants nothing by itself. No \
request is ever sent until you pick a provider and consent for that purpose.";

/// Heading of the network-policy radio group.
const MODE_LEGEND: &str = "Network policy";

/// Heading of the engine radio group.
const ENGINE_LEGEND: &str = "Default speech-to-text engine";

/// What the engines step says when no engine can be chosen.
const NO_ENGINE: &str = "No usable speech-to-text engine was detected yet.";

/// Said of an engine the backend lists with no installed model.
const NO_MODELS: &str = "No models installed for this engine.";

/// The key is optional, and having one grants nothing.
const KEY_PROSE: &str = "An OpenRouter key is optional. Transcription runs locally without one, \
and adding a key does not send anything anywhere by itself.";

/// Where the key goes.
const KEY_HINT: &str = "Stored in the system keychain by the backend. It is never shown again.";

/// Live status of the key field, when a key was handed over this run.
const KEY_SAVED: &str = "Key saved to the system keychain.";

/// Live status of the key field, when none was.
const KEY_NOT_SAVED: &str = "No key saved.";

/// Standing line of the sources step.
const SOURCES_PROSE: &str =
    "Every source below is optional. You can add audio at any time from the Library.";

/// The import entry, which this build supports end to end.
const SOURCE_IMPORT: &str = "Import audio";

/// What the import entry does.
const SOURCE_IMPORT_PROSE: &str =
    "Pick files from this computer. This is the path this build supports end to end.";

/// The direct-device entry.
const SOURCE_DEVICE: &str = "Connect PLAUD device (direct)";

/// The cloud-account entry.
const SOURCE_CLOUD: &str = "Connect PLAUD account (cloud)";

/// Integration key of the direct device connector.
const INTEGRATION_DEVICE: &str = "plaud_device";

/// Integration key of the cloud connector.
const INTEGRATION_CLOUD: &str = "plaud_cloud";

/// Said of a connector the backend does not report at all.
const SOURCE_ABSENT: &str = "Not in this build";

/// Said of a connector the backend reports but cannot select yet.
///
/// Spec §6.6 is fail-closed: anything short of `supported` is not chosen, and
/// nothing signs you in behind your back.
const SOURCE_NOT_SELECTABLE: &str =
    "Plaudon will not use this connector until it reports support, and nothing signs you in \
behind your back.";

/// Said of a connector the backend reports as supported.
const SOURCE_SELECTABLE: &str = "Available from the Sources panel once setup is done.";

/// What finishing does.
const FINISH_PROSE: &str =
    "Plaudon will save these choices and open your library, where you can import audio.";

/// Shown in the summary when no engine was chosen.
const NO_ENGINE_CHOSEN: &str = "Not chosen";

/// The skip button of the permissions step, which leaves every permission
/// for later and never blocks setup (#307).
pub const SKIP_LATER: &str = "Skip — do later";

/// The first-run setup screen.
pub struct OnboardingScreen {
    state: OnboardingState,
    key_draft: String,
    mode_pending: bool,
    key_pending: bool,
    complete_pending: bool,
    error: Option<ErrorEnvelope>,
    /// Step whose heading should take focus on the next frame, so that a step
    /// change is announced instead of being a silent repaint.
    focus_heading: Option<usize>,
    /// Installing what the engines need (issue #303), shared with Settings.
    setup: EngineSetup,
    /// The macOS permissions panel of the permissions step (#307).
    permissions_panel: PermissionsPanel,
    /// The permission statuses the shell last read.
    os_permissions: PermissionsReport,
}

impl Default for OnboardingScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for OnboardingScreen {
    /// Written by hand: the draft key must not reach a log line, a panic
    /// message or a test's `{:?}`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OnboardingScreen")
            .field("state", &self.state)
            .field("key_draft", &"[redacted]")
            .field("mode_pending", &self.mode_pending)
            .field("key_pending", &self.key_pending)
            .field("complete_pending", &self.complete_pending)
            .field("error", &self.error)
            .field("focus_heading", &self.focus_heading)
            .field("setup", &self.setup)
            .field("os_permissions", &self.os_permissions)
            .finish()
    }
}

impl OnboardingScreen {
    /// A fresh first run, with the steps of the platform this binary runs on.
    #[must_use]
    pub fn new() -> Self {
        Self::for_platform(Platform::host())
    }

    /// A fresh first run with the steps `platform` shows.
    #[must_use]
    pub fn for_platform(platform: Platform) -> Self {
        Self {
            state: OnboardingState::for_platform(platform),
            permissions_panel: PermissionsPanel::new(platform),
            os_permissions: PermissionsReport::default(),
            key_draft: String::new(),
            mode_pending: false,
            key_pending: false,
            complete_pending: false,
            error: None,
            focus_heading: None,
            setup: EngineSetup::default(),
        }
    }

    /// The step machine behind the screen.
    #[must_use]
    pub const fn state(&self) -> &OnboardingState {
        &self.state
    }

    /// Hand over the permission statuses the shell last read.
    pub fn set_os_permissions(&mut self, report: PermissionsReport) {
        self.os_permissions = report;
    }

    /// Render with the tokens of the theme `egui` is painting with.
    pub fn ui(&mut self, ui: &mut Ui, capabilities: &Capabilities, out: &mut SettingsOutput) {
        let tokens = VisualsTokens::from_ui(ui);
        self.ui_with_tokens(ui, &tokens, capabilities, out);
    }

    /// Render with tokens the caller supplies.
    pub fn ui_with_tokens(
        &mut self,
        ui: &mut Ui,
        tokens: &dyn SettingsTokens,
        capabilities: &Capabilities,
        out: &mut SettingsOutput,
    ) {
        // Spec §5.3: Escape closes an overlay, but first-run setup is not an
        // overlay. Consuming the key here keeps a stray press from discarding
        // answers the user has already given.
        ui.input_mut(|input| {
            input.consume_key(egui::Modifiers::NONE, egui::Key::Escape);
        });

        ui.label(RichText::new(TITLE).heading().strong().color(tokens.text()));
        // Progress is text, never a bar alone.
        section::note(ui, tokens, &self.state.progress_label());
        ui.add_space(8.0);

        let step = self.state.step();
        let heading = ui.add(
            Label::new(RichText::new(step.title()).heading().color(tokens.text()))
                .sense(Sense::click()),
        );
        if self.focus_heading == Some(self.state.step_index()) {
            heading.request_focus();
            self.focus_heading = None;
        }
        ui.add_space(8.0);

        match step {
            OnboardingStep::Privacy => Self::privacy_step(ui, tokens),
            OnboardingStep::Mode => self.mode_step(ui, tokens, out),
            OnboardingStep::Engines => self.engines_step(ui, tokens, capabilities, out),
            OnboardingStep::Key => self.key_step(ui, tokens, out),
            OnboardingStep::Sources => self.sources_step(ui, tokens, capabilities),
            OnboardingStep::Permissions => self.permissions_step(ui, tokens, out),
            OnboardingStep::Finish => self.finish_step(ui, tokens, capabilities),
        }

        if let Some(envelope) = &self.error {
            ui.add_space(8.0);
            section::status_badge(ui, tokens, Tone::Danger, "✕", &envelope.message);
        }

        ui.add_space(16.0);
        self.navigation(ui, tokens, capabilities, out);
    }

    /// Back, Skip and Next — or Finish, on the last step.
    fn navigation(
        &mut self,
        ui: &mut Ui,
        tokens: &dyn SettingsTokens,
        capabilities: &Capabilities,
        out: &mut SettingsOutput,
    ) {
        let step = self.state.step();
        let first_step = self.state.step_index() == 0;
        let last_step = self.state.is_last_step();
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(!first_step, egui::Button::new("Back"))
                .clicked()
            {
                self.navigate(OnboardingAction::Back);
            }
            if step.skippable() && ui.button("Skip this step").clicked() {
                self.navigate(OnboardingAction::Skip);
            }
            if last_step {
                let mut finish = false;
                ui.scope(|ui| {
                    if self.complete_pending {
                        ui.disable();
                    }
                    finish = section::primary_button(ui, tokens, "Finish setup").clicked();
                });
                if finish {
                    self.complete_pending = true;
                    self.error = None;
                    out.command(AppCommand::CompleteOnboarding(
                        self.state
                            .completion_request(capabilities.preferences.notifications.clone()),
                    ));
                }
            } else if section::primary_button(ui, tokens, "Next").clicked() {
                self.navigate(OnboardingAction::Next);
            }
        });
    }

    /// Apply a navigation action and announce the step that follows it.
    fn navigate(&mut self, action: OnboardingAction) {
        self.state.apply(action);
        self.focus_heading = Some(self.state.step_index());
    }

    /// Step one: what stays on this machine.
    fn privacy_step(ui: &mut Ui, tokens: &dyn SettingsTokens) {
        section::prose(ui, tokens, PRIVACY_PROSE_1);
        ui.add_space(8.0);
        section::prose(ui, tokens, PRIVACY_PROSE_2);
        ui.add_space(8.0);
        egui::CollapsingHeader::new(PRIVACY_DISCLOSURE)
            .id_salt("onboarding-privacy-disclosure")
            .show(ui, |ui| {
                for bullet in PRIVACY_BULLETS {
                    section::prose(ui, tokens, &format!("• {bullet}"));
                }
            });
    }

    /// Step two: local only, or allow cloud AI later.
    fn mode_step(&mut self, ui: &mut Ui, tokens: &dyn SettingsTokens, out: &mut SettingsOutput) {
        ui.label(RichText::new(MODE_LEGEND).strong().color(tokens.text()));
        let local_only = self.state.local_only();
        let mut chosen: Option<bool> = None;
        ui.scope(|ui| {
            if self.mode_pending {
                ui.disable();
            }
            if ui.radio(local_only, MODE_LOCAL).clicked() {
                chosen = Some(true);
            }
            if ui.radio(!local_only, MODE_CLOUD).clicked() {
                chosen = Some(false);
            }
        });
        if let Some(enabled) = chosen {
            self.state.apply(OnboardingAction::ChooseMode {
                local_only: enabled,
            });
            self.mode_pending = true;
            self.error = None;
            out.command(AppCommand::SetLocalOnlyMode(SetLocalOnlyModeRequest {
                enabled,
                // The radio *is* the confirmation: the user is answering the
                // question the step asks.
                confirmed: true,
            }));
        }
    }

    /// Step three: the engines this machine actually has, and what each
    /// needs installed (issue #303).
    fn engines_step(
        &mut self,
        ui: &mut Ui,
        tokens: &dyn SettingsTokens,
        capabilities: &Capabilities,
        out: &mut SettingsOutput,
    ) {
        // The answer to step two, not the stored preference: until the user
        // has answered, setup's posture is Local-only.
        let local_only = self.state.local_only();
        section::note(ui, tokens, engines_download_note(local_only));
        ui.add_space(8.0);

        let stt = engine_rows(capabilities, EngineStage::Stt);
        let text = engine_rows(capabilities, EngineStage::Text);
        for (rows, stage) in [(&stt, EngineStage::Stt), (&text, EngineStage::Text)] {
            for row in rows {
                engine_entry(ui, tokens, row, stage);
                if let Some(engine) = capabilities
                    .engines
                    .iter()
                    .find(|engine| engine.engine_id == row.engine_id)
                {
                    self.setup
                        .engine_actions(ui, tokens, engine, local_only, out);
                }
            }
        }
        if stt.is_empty() && text.is_empty() {
            section::note(ui, tokens, "No engine is reported for this machine yet.");
        }

        ui.add_space(8.0);
        self.setup.plaud_cli(ui, tokens, local_only, out);
        ui.add_space(8.0);

        ui.label(RichText::new(ENGINE_LEGEND).strong().color(tokens.text()));
        let selectable: Vec<&EngineRow> = stt.iter().filter(|row| row.selectable).collect();
        if selectable.is_empty() {
            section::note(ui, tokens, NO_ENGINE);
            return;
        }
        let selected = self.state.default_stt_engine_id().map(str::to_owned);
        let mut choice: Option<String> = None;
        for row in selectable {
            let is_selected = selected.as_deref() == Some(row.engine_id.as_str());
            if ui.radio(is_selected, &row.display_name).clicked() {
                choice = Some(row.engine_id.clone());
            }
        }
        if let Some(engine_id) = choice {
            self.state.choose_stt_engine(Some(engine_id));
        }
    }

    /// Step four: the optional key.
    fn key_step(&mut self, ui: &mut Ui, tokens: &dyn SettingsTokens, out: &mut SettingsOutput) {
        section::prose(ui, tokens, KEY_PROSE);
        ui.add_space(8.0);
        section::secret_field(ui, "OpenRouter API key", &mut self.key_draft);
        section::note(ui, tokens, KEY_HINT);
        ui.add_space(8.0);
        if self.state.key_saved() {
            section::status_badge(ui, tokens, Tone::Success, "✓", KEY_SAVED);
        } else {
            section::status_badge(ui, tokens, Tone::Neutral, "–", KEY_NOT_SAVED);
        }
        ui.add_space(8.0);
        let mut save = false;
        ui.scope(|ui| {
            if self.key_pending {
                ui.disable();
            }
            save = section::primary_button(ui, tokens, "Save key").clicked();
        });
        if save && !self.key_draft.trim().is_empty() {
            // The secret leaves the screen in the same statement that raises
            // the command; nothing holds it afterwards.
            let key = std::mem::take(&mut self.key_draft);
            self.key_pending = true;
            self.error = None;
            out.command(AppCommand::SetOpenRouterKey(SetOpenRouterKeyRequest {
                key,
            }));
        }
    }

    /// Step five: where audio comes from.
    fn sources_step(
        &mut self,
        ui: &mut Ui,
        tokens: &dyn SettingsTokens,
        capabilities: &Capabilities,
    ) {
        section::prose(ui, tokens, SOURCES_PROSE);
        ui.add_space(8.0);

        ui.label(RichText::new(SOURCE_IMPORT).strong().color(tokens.text()));
        section::prose(ui, tokens, SOURCE_IMPORT_PROSE);
        if section::primary_button(ui, tokens, "Continue to import audio").clicked() {
            self.navigate(OnboardingAction::Next);
            return;
        }
        ui.add_space(12.0);

        // The connectors report their own status; the step renders what the
        // backend says rather than repeating a claim about the build.
        for (title, key) in [
            (SOURCE_DEVICE, INTEGRATION_DEVICE),
            (SOURCE_CLOUD, INTEGRATION_CLOUD),
        ] {
            ui.label(RichText::new(title).strong().color(tokens.text()));
            match capabilities.integrations.get(key).copied() {
                None => {
                    section::status_badge(ui, tokens, Tone::Neutral, "–", SOURCE_ABSENT);
                    section::prose(ui, tokens, SOURCE_NOT_SELECTABLE);
                }
                Some(status) => {
                    let word = status_word(status);
                    section::status_badge(
                        ui,
                        tokens,
                        tone_for_status(status),
                        status_glyph(word),
                        &status_label(word),
                    );
                    if status.is_selectable() {
                        section::prose(ui, tokens, SOURCE_SELECTABLE);
                    } else {
                        section::prose(ui, tokens, SOURCE_NOT_SELECTABLE);
                    }
                }
            }
            ui.add_space(12.0);
        }
    }

    /// The macOS Privacy & Security step (#307).
    fn permissions_step(
        &mut self,
        ui: &mut Ui,
        tokens: &dyn SettingsTokens,
        out: &mut SettingsOutput,
    ) {
        let _ = (ui, tokens, out, &mut self.permissions_panel);
    }

    /// The last step: review, then finish.
    fn finish_step(
        &mut self,
        ui: &mut Ui,
        tokens: &dyn SettingsTokens,
        capabilities: &Capabilities,
    ) {
        section::prose(ui, tokens, FINISH_PROSE);
        ui.add_space(8.0);
        section::fact_row(
            ui,
            tokens,
            "Network policy",
            mode_summary(self.state.local_only()),
        );
        let engine = self.state.default_stt_engine_id().and_then(|id| {
            capabilities
                .engines
                .iter()
                .find(|engine| engine.engine_id == id)
                .map(|engine| engine.display_name.clone())
        });
        section::fact_row(
            ui,
            tokens,
            "Default speech-to-text engine",
            engine.as_deref().unwrap_or(NO_ENGINE_CHOSEN),
        );
        let skipped = self.state.skipped();
        if !skipped.is_empty() {
            let titles: Vec<&str> = skipped.iter().map(|step| step.title()).collect();
            section::fact_row(ui, tokens, "Steps you skipped", &titles.join(", "));
        }
    }

    /// Receive the path a file dialog this screen opened returned.
    pub fn path_picked(&mut self, picked: PickedPath, path: &str, out: &mut SettingsOutput) {
        self.setup.path_picked(picked, path, out);
    }

    /// Report download progress from a `model.install_progress` event.
    pub fn install_progress(&mut self, received_bytes: u64, total_bytes: u64) {
        self.setup.install_progress(received_bytes, total_bytes);
    }

    /// Fold a command outcome into the screen.
    pub fn on_response(
        &mut self,
        command: &'static str,
        result: &Result<AppResponse, ErrorEnvelope>,
        out: &mut SettingsOutput,
    ) {
        if self.setup.on_response(command, result) {
            // A model was installed: re-read the inventory, so the engine
            // becomes choosable on this very step.
            out.push(SettingsIntent::RefreshCapabilities);
        }
        match command {
            "set_local_only_mode" => {
                self.mode_pending = false;
                match result {
                    Ok(AppResponse::SetLocalOnlyMode(policy)) => {
                        self.error = None;
                        // The enforced policy, not the request, is what the
                        // flow carries forward.
                        self.state.apply(OnboardingAction::ChooseMode {
                            local_only: policy.enabled,
                        });
                    }
                    Err(envelope) => self.error = Some(envelope.clone()),
                    Ok(_) => {}
                }
            }
            "set_openrouter_key" => {
                self.key_pending = false;
                match result {
                    Ok(AppResponse::SetOpenRouterKey(_)) => {
                        self.error = None;
                        self.state.apply(OnboardingAction::KeySaved);
                    }
                    Err(envelope) => self.error = Some(envelope.clone()),
                    Ok(_) => {}
                }
            }
            "complete_onboarding" => {
                self.complete_pending = false;
                match result {
                    Ok(AppResponse::CompleteOnboarding(_)) => self.error = None,
                    Err(envelope) => self.error = Some(envelope.clone()),
                    Ok(_) => {}
                }
            }
            _ => {}
        }
    }
}

/// One engine of the inventory, with its status, reason, models and cost.
fn engine_entry(ui: &mut Ui, tokens: &dyn SettingsTokens, row: &EngineRow, stage: EngineStage) {
    ui.horizontal_wrapped(|ui| {
        // The stage is part of the name so that the row and the radio below
        // are two different accessible names for two different controls.
        ui.label(
            RichText::new(format!("{} — {}", row.display_name, stage_label(stage)))
                .strong()
                .color(tokens.text()),
        );
        section::status_badge(
            ui,
            tokens,
            tone_for_glyph(row.status_glyph),
            row.status_glyph,
            &row.status_label,
        );
    });
    if let Some(reason) = &row.unavailable_reason {
        // Verbatim: why an engine is unavailable is the backend's sentence.
        section::prose(ui, tokens, reason);
    }
    if row.installed_models.is_empty() {
        section::note(ui, tokens, NO_MODELS);
    } else {
        for model in &row.installed_models {
            section::note(ui, tokens, model);
        }
    }
    section::note(ui, tokens, &row.download_requirement);
    ui.add_space(8.0);
}

/// Label of a pipeline stage.
const fn stage_label(stage: EngineStage) -> &'static str {
    match stage {
        EngineStage::Stt => "Speech to text",
        EngineStage::Text => "Text",
    }
}

/// The wire word of a capability status, which is what the shared glyph and
/// label helpers key off.
const fn status_word(status: CapabilityStatus) -> &'static str {
    match status {
        CapabilityStatus::Supported => "supported",
        CapabilityStatus::Experimental => "experimental",
        CapabilityStatus::Unsupported => "unsupported",
        CapabilityStatus::Unknown => "unknown",
    }
}

/// Tone of a capability status.
const fn tone_for_status(status: CapabilityStatus) -> Tone {
    match status {
        CapabilityStatus::Supported => Tone::Success,
        CapabilityStatus::Experimental => Tone::Warning,
        CapabilityStatus::Unsupported => Tone::Danger,
        CapabilityStatus::Unknown => Tone::Neutral,
    }
}

/// Tone matching the glyph the application chose for a status word.
fn tone_for_glyph(glyph: &str) -> Tone {
    match glyph {
        "✓" => Tone::Success,
        "~" => Tone::Warning,
        "✕" => Tone::Danger,
        _ => Tone::Neutral,
    }
}
