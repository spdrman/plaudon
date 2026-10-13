//! How the People destination plugs into the shell.
//!
//! [`PeopleScreen`] renders DTOs and raises commands; this is the one module
//! that also knows the shell — the dispatcher, the toasts and the route a
//! queue entry opens. Nothing here reads on a timer: the roster is read when
//! the screen opens, when the user filters it, when a write of its own
//! changed it, and when a §15.3 event says somebody else did (`person.changed`,
//! `voiceprofile.suggestions_ready`).

use plaudon_app::command::{AppCommand, AppResponse, CommandId};
use plaudon_app::dto::{EventKind, UiEvent};
use plaudon_app::event::ResyncReason;
use plaudon_domain::error::ErrorEnvelope;

use crate::shell::route::{Destination, Route};
use crate::shell::screen::{Screen, ScreenCtx};
use crate::widgets::toast::ToastTone;

use super::{PeopleData, PeopleScreen, QUEUE_PAGE_SIZE};

/// The People destination, as the shell sees it.
#[derive(Debug, Default)]
pub struct PeopleShellScreen {
    screen: PeopleScreen,
    opened: bool,
}

impl PeopleShellScreen {
    /// A screen that has read nothing yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The screen underneath, for a host or a test that inspects its state.
    #[must_use]
    pub const fn screen(&self) -> &PeopleScreen {
        &self.screen
    }

    /// The screen underneath, mutably, for a test that seeds it.
    pub const fn screen_mut(&mut self) -> &mut PeopleScreen {
        &mut self.screen
    }
}

impl Screen for PeopleShellScreen {
    fn destination(&self) -> Destination {
        Destination::People
    }

    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut ScreenCtx<'_>) {
        if !self.opened {
            self.opened = true;
            for command in PeopleData::initial_commands() {
                cx.send(command);
            }
        }
        let output = self.screen.ui(ui);
        for command in output.commands {
            cx.send(command);
        }
        if let Some(recording_id) = output.open_recording {
            cx.navigate(Route::LibraryRecording { recording_id });
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
            if let Some(title) = refusal_title(command) {
                cx.toast(ToastTone::Danger, title, Some(envelope.message.clone()));
            }
        }
        for follow_up in self.screen.absorb(command, result) {
            cx.send(follow_up);
        }
    }

    fn on_ui_event(&mut self, event: &UiEvent, cx: &mut ScreenCtx<'_>) {
        match event.kind {
            EventKind::PersonChanged => {
                cx.send(self.screen.list_command());
            }
            EventKind::VoiceProfileSuggestionsReady => {
                cx.send(AppCommand::ListSpeakerSuggestions(
                    plaudon_app::dto::ListSpeakerSuggestionsRequest {
                        recording_id: None,
                        limit: Some(QUEUE_PAGE_SIZE),
                    },
                ));
            }
            _ => {}
        }
    }

    fn on_resync(&mut self, _reason: ResyncReason, cx: &mut ScreenCtx<'_>) {
        for command in PeopleData::initial_commands() {
            cx.send(command);
        }
    }
}

/// The toast headline a refused command gets, named for what the user tried
/// to do rather than for the command.
///
/// `None` for a command this screen did not send, so another screen's refusal
/// is not reported twice.
#[must_use]
pub fn refusal_title(command: &str) -> Option<&'static str> {
    Some(match command {
        "list_people" => "The list of people could not be read",
        "create_person" => "That person was not added",
        "update_person" => "That change was not saved",
        "merge_people" => "The two entries were not merged",
        "delete_person" => "That person was not deleted",
        "delete_voice_enrollment" => "That voiceprint was not deleted",
        "list_voice_exemplars" => "What that voiceprint is made of could not be read",
        "split_voice_enrollment" => "That voiceprint was not split",
        "get_voice_profile_settings" => "The voice settings could not be read",
        "list_speaker_suggestions" => "The review queue could not be read",
        "confirm_speaker_suggestion" => "That suggestion was not confirmed",
        "reject_speaker_suggestion" => "That suggestion was not rejected",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_this_screen_sends_has_a_refusal_headline_of_its_own() {
        let commands = [
            "list_people",
            "create_person",
            "update_person",
            "merge_people",
            "delete_person",
            "delete_voice_enrollment",
            "list_voice_exemplars",
            "split_voice_enrollment",
            "get_voice_profile_settings",
            "list_speaker_suggestions",
            "confirm_speaker_suggestion",
            "reject_speaker_suggestion",
        ];
        let mut seen = std::collections::BTreeSet::new();
        for command in commands {
            let title = refusal_title(command).expect("every command has a headline");
            assert!(seen.insert(title), "{command} shares a headline");
        }
        assert_eq!(refusal_title("list_recordings"), None);
    }
}
