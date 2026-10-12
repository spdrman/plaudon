//! The Templates destination (matrix row A12).
//!
//! The screen is the adapter between the shell and
//! [`TemplateManager`](crate::ai::TemplateManager): it owns the three
//! responses the manager renders — the catalogue, the open document and the
//! version history — dispatches what the manager asks for, and turns a refused
//! command into a toast. The manager itself knows nothing about the shell,
//! which is what lets a kittest test drive every state of it with no
//! dispatcher at all.
//!
//! Reads happen on an edge, never on a frame: the catalogue is read the first
//! time the screen is shown and again after a resync, and a document and its
//! history are read when the selection changes. A response that invalidates
//! another read — a copy was created, a save stored a new version — triggers
//! exactly the follow-ups it invalidated.

use plaudon_app::command::{AppCommand, AppResponse, CommandId};
use plaudon_app::dto::{
    ListTemplateVersionsRequest, ListTemplateVersionsResponse, ListTemplatesResponse,
    TemplateDocumentDto,
};
use plaudon_app::event::ResyncReason;
use plaudon_domain::error::ErrorEnvelope;

use crate::ai::{AiIntent, TemplateManager};
use crate::shell::route::Destination;
use crate::shell::screen::{Screen, ScreenCtx};
use crate::widgets::toast::ToastTone;

/// The Templates destination, as the shell sees it.
#[derive(Debug, Default)]
pub struct TemplatesScreen {
    panel: TemplateManager,
    templates: Option<ListTemplatesResponse>,
    document: Option<TemplateDocumentDto>,
    versions: Option<ListTemplateVersionsResponse>,
    opened: bool,
}

impl TemplatesScreen {
    /// A screen that has not read the catalogue yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// What the screen reads when it opens, and again after a resync.
    ///
    /// Published so a test can assert the opening reads without standing up a
    /// dispatcher, the same way the Settings screen does.
    #[must_use]
    pub fn initial_commands() -> Vec<AppCommand> {
        vec![AppCommand::ListTemplates]
    }

    /// The manager underneath, for a host or a test that inspects its state.
    #[must_use]
    pub fn panel(&self) -> &TemplateManager {
        &self.panel
    }

    /// Fold a command outcome into the screen and report what it invalidated.
    ///
    /// Separate from [`Screen::on_response`] because it needs no shell: the
    /// returned commands are the follow-up reads, which the caller dispatches.
    pub fn absorb(
        &mut self,
        command: &'static str,
        result: &Result<AppResponse, ErrorEnvelope>,
    ) -> Vec<AppCommand> {
        self.panel.on_response(command, result);
        let Ok(response) = result else {
            return Vec::new();
        };
        match response {
            AppResponse::ListTemplates(templates) => {
                self.templates = Some(templates.clone());
                Vec::new()
            }
            AppResponse::GetTemplate(document) => {
                self.document = Some(document.clone());
                Vec::new()
            }
            AppResponse::CreateTemplateCopy(document) => {
                // A new template exists and has a history of its own, and the
                // catalogue the user is looking at does not list it yet.
                let template_id = document.template_id.clone();
                self.document = Some(document.clone());
                self.versions = None;
                vec![
                    AppCommand::ListTemplates,
                    AppCommand::ListTemplateVersions(ListTemplateVersionsRequest { template_id }),
                ]
            }
            AppResponse::SaveTemplate(saved) => {
                // The current version moved, so both the catalogue row and the
                // history are stale.
                let template_id = saved.template.template_id.clone();
                self.document = Some(saved.template.clone());
                vec![
                    AppCommand::ListTemplates,
                    AppCommand::ListTemplateVersions(ListTemplateVersionsRequest { template_id }),
                ]
            }
            AppResponse::ListTemplateVersions(versions) => {
                self.versions = Some(versions.clone());
                Vec::new()
            }
            // `validate_template` stores nothing: the manager holds the
            // problems it returned and renders them next to the editor.
            _ => Vec::new(),
        }
    }

    /// Draw the manager and report the commands one frame asked for.
    ///
    /// The manager raises commands and nothing else — it has no playback, no
    /// navigation and no file dialog. A read the user just asked for makes the
    /// answer it replaces stale, so that answer is dropped here rather than
    /// left on screen under the question it no longer answers.
    ///
    /// Public because it is the whole of the screen minus the shell: a test
    /// drives it with no dispatcher and reads the commands back.
    pub fn render(&mut self, ui: &mut egui::Ui) -> Vec<AppCommand> {
        let output = self.panel.ui(
            ui,
            self.templates.as_ref(),
            self.document.as_ref(),
            self.versions.as_ref(),
        );
        let mut commands = Vec::with_capacity(output.intents.len());
        for intent in output.intents {
            let AiIntent::Command(command) = intent else {
                continue;
            };
            match &command {
                AppCommand::GetTemplate(_) => self.document = None,
                AppCommand::ListTemplateVersions(_) => self.versions = None,
                _ => {}
            }
            commands.push(command);
        }
        commands
    }
}

impl Screen for TemplatesScreen {
    fn destination(&self) -> Destination {
        Destination::Templates
    }

    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut ScreenCtx<'_>) {
        if !self.opened {
            self.opened = true;
            for command in Self::initial_commands() {
                cx.send(command);
            }
        }
        for command in self.render(ui) {
            cx.send(command);
        }
    }

    fn on_response(
        &mut self,
        _id: CommandId,
        command: &'static str,
        result: &Result<AppResponse, ErrorEnvelope>,
        cx: &mut ScreenCtx<'_>,
    ) {
        if let Err(envelope) = result {
            cx.toast(
                ToastTone::Danger,
                refusal_title(command),
                Some(envelope.message.clone()),
            );
        }
        for follow_up in self.absorb(command, result) {
            cx.send(follow_up);
        }
    }

    fn on_resync(&mut self, _reason: ResyncReason, cx: &mut ScreenCtx<'_>) {
        for command in Self::initial_commands() {
            cx.send(command);
        }
        if let Some(template_id) = self.panel.selected().map(str::to_owned) {
            cx.send(AppCommand::ListTemplateVersions(
                ListTemplateVersionsRequest { template_id },
            ));
        }
    }
}

/// The toast headline a refused template command gets.
///
/// Named for what the user tried to do, because the command name is the
/// application's vocabulary and not theirs.
fn refusal_title(command: &str) -> &'static str {
    match command {
        "list_templates" => "Templates could not be listed",
        "get_template" => "That template could not be read",
        "create_template_copy" => "The copy was not created",
        "save_template" => "The template was not saved",
        "validate_template" => "The document could not be checked",
        "list_template_versions" => "The version history could not be read",
        _ => "The application refused the request",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_screen_opens_by_reading_the_catalogue_and_nothing_else() {
        let names: Vec<&str> = TemplatesScreen::initial_commands()
            .iter()
            .map(AppCommand::name)
            .collect();
        assert_eq!(names, vec!["list_templates"]);
    }

    #[test]
    fn every_template_command_has_a_refusal_headline_of_its_own() {
        let commands = [
            "list_templates",
            "get_template",
            "create_template_copy",
            "save_template",
            "validate_template",
            "list_template_versions",
        ];
        let mut seen = std::collections::BTreeSet::new();
        for command in commands {
            assert!(
                seen.insert(refusal_title(command)),
                "{command} shares a headline with another command"
            );
        }
    }
}
