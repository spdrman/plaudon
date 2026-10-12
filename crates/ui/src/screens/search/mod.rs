//! Cross-recording search (§3.1, §9.8, §17.5) over the `search_library`
//! command, which is the only path to the full-text index.
//!
//! Results are grouped by recording, newest group first in result order, and
//! a seek control appears **only** when the hit carries a position (§8.6A).
//! A derived position says so on the control itself rather than pretending to
//! be as exact as an engine-reported one.

use egui::{RichText, Ui};
use plaudon_app::command::{AppCommand, AppResponse, CommandId};
use plaudon_app::dto::{SearchLibraryRequest, SearchResultDto};
use plaudon_domain::error::ErrorEnvelope;
use plaudon_domain::ids::{RecordingId, TranscriptRevisionId};
use plaudon_domain::transcript::AlignmentOrigin;

use crate::screens::library::format::format_duration;
use crate::screens::library::list::Sender;
use crate::widgets::error_notice::{error_notice, ErrorNoticeAction};

/// Results requested per page.
pub const SEARCH_PAGE_SIZE: u32 = 50;

/// Something the search screen needs the host to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchIntent {
    /// Open this recording at this transcript segment.
    OpenSegment {
        /// Recording to open.
        recording_id: RecordingId,
        /// Revision the hit belongs to.
        revision_id: TranscriptRevisionId,
        /// Segment to reveal.
        seq_index: u32,
    },
    /// Open this recording and start playback at this offset.
    Seek {
        /// Recording to open.
        recording_id: RecordingId,
        /// Offset in milliseconds.
        seek_ms: u64,
    },
    /// Open settings, because an envelope asked for it.
    OpenSettings,
}

/// Results grouped by the recording they came from, in first-seen order.
#[must_use]
pub fn group_by_recording(results: &[SearchResultDto]) -> Vec<(String, Vec<&SearchResultDto>)> {
    let mut groups: Vec<(RecordingId, String, Vec<&SearchResultDto>)> = Vec::new();
    for result in results {
        match groups
            .iter_mut()
            .find(|(id, _, _)| *id == result.recording_id)
        {
            Some((_, _, hits)) => hits.push(result),
            None => groups.push((
                result.recording_id,
                result.recording_title.clone(),
                vec![result],
            )),
        }
    }
    groups
        .into_iter()
        .map(|(_, title, hits)| (title, hits))
        .collect()
}

/// The label of a hit's seek control, or `None` when the hit has no position
/// and therefore gets no control at all.
#[must_use]
pub fn seek_label(result: &SearchResultDto) -> Option<String> {
    let seek_ms = result.seek_ms?;
    if !result.alignment_origin.permits_seek() {
        return None;
    }
    let at = format_duration(Some(seek_ms));
    Some(match result.alignment_origin {
        AlignmentOrigin::Derived => format!("Play from about {at}"),
        _ => format!("Play from {at}"),
    })
}

/// The search screen.
#[derive(Debug, Default)]
pub struct SearchScreen {
    text: String,
    submitted: Option<String>,
    results: Vec<SearchResultDto>,
    next_cursor: Option<String>,
    pending: Option<CommandId>,
    error: Option<ErrorEnvelope>,
}

impl SearchScreen {
    /// A screen with nothing searched yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Results of the last completed search.
    #[must_use]
    pub fn results(&self) -> &[SearchResultDto] {
        &self.results
    }

    /// The query the current results answer.
    #[must_use]
    pub fn submitted(&self) -> Option<&str> {
        self.submitted.as_deref()
    }

    /// Route a dispatcher answer. Returns whether it was ours.
    pub fn on_response(
        &mut self,
        id: CommandId,
        result: &Result<AppResponse, ErrorEnvelope>,
    ) -> bool {
        if self.pending != Some(id) {
            return false;
        }
        self.pending = None;
        match result {
            Ok(AppResponse::SearchLibrary(response)) => {
                self.error = None;
                self.results.extend(response.results.iter().cloned());
                self.next_cursor.clone_from(&response.next_cursor);
            }
            Ok(_) => return false,
            Err(envelope) => self.error = Some(envelope.clone()),
        }
        true
    }

    fn submit(&mut self, send: Sender<'_>) {
        let query = self.text.trim().to_owned();
        if query.is_empty() {
            return;
        }
        self.submitted = Some(query.clone());
        self.results.clear();
        self.next_cursor = None;
        self.error = None;
        self.dispatch(query, None, send);
    }

    fn dispatch(&mut self, query: String, cursor: Option<String>, send: Sender<'_>) {
        let id = send(AppCommand::SearchLibrary(SearchLibraryRequest {
            query,
            cursor,
            limit: SEARCH_PAGE_SIZE,
        }));
        self.pending = Some(id);
    }

    /// Draw the screen.
    pub fn ui(&mut self, ui: &mut Ui, send: Sender<'_>) -> Vec<SearchIntent> {
        let mut intents = Vec::new();

        ui.horizontal(|ui| {
            let label = ui.label("Search transcripts");
            let field = ui.add(
                egui::TextEdit::singleline(&mut self.text)
                    .hint_text("Words spoken in any recording")
                    .desired_width(280.0),
            );
            let field = field.labelled_by(label.id);
            let submitted = field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
            if (ui.button("Search").clicked() || submitted) && self.pending.is_none() {
                self.submit(send);
            }
        });

        if let Some(envelope) = self.error.clone() {
            match error_notice(ui, &envelope, "The search could not be run.") {
                Some(ErrorNoticeAction::Retry) => {
                    if let Some(query) = self.submitted.clone() {
                        self.results.clear();
                        self.dispatch(query, None, send);
                    }
                }
                Some(ErrorNoticeAction::OpenSettings) => {
                    intents.push(SearchIntent::OpenSettings);
                }
                None => {}
            }
            return intents;
        }

        if self.pending.is_some() && self.results.is_empty() {
            ui.label("Searching…");
            return intents;
        }

        let Some(query) = self.submitted.clone() else {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.heading("Search the library");
                ui.label("Enter a search term to find words spoken across every recording.");
            });
            return intents;
        };

        if self.results.is_empty() {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.heading("No results");
                ui.label(format!("No transcript segment matches “{query}”."));
            });
            return intents;
        }

        intents.extend(self.results_list(ui));

        if self.next_cursor.is_some() {
            let busy = self.pending.is_some();
            let label = if busy { "Loading more…" } else { "More results" };
            if ui
                .add_enabled(!busy, egui::Button::new(label))
                .clicked()
            {
                if let (Some(query), Some(cursor)) =
                    (self.submitted.clone(), self.next_cursor.clone())
                {
                    self.dispatch(query, Some(cursor), send);
                }
            }
        }
        intents
    }

    fn results_list(&self, ui: &mut Ui) -> Vec<SearchIntent> {
        let mut intents = Vec::new();
        let groups = group_by_recording(&self.results);
        let summary = format!(
            "{} {} in {} {}",
            self.results.len(),
            if self.results.len() == 1 {
                "result"
            } else {
                "results"
            },
            groups.len(),
            if groups.len() == 1 {
                "recording"
            } else {
                "recordings"
            }
        );
        ui.label(RichText::new(&summary).strong());

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for (title, hits) in groups {
                    ui.heading(&title);
                    for hit in hits {
                        egui::Frame::group(ui.style()).show(ui, |ui| {
                            ui.label(&hit.excerpt);
                            ui.horizontal(|ui| {
                                if ui
                                    .button(format!("Open segment {}", hit.seq_index + 1))
                                    .clicked()
                                {
                                    intents.push(SearchIntent::OpenSegment {
                                        recording_id: hit.recording_id,
                                        revision_id: hit.transcript_revision_id,
                                        seq_index: hit.seq_index,
                                    });
                                }
                                // §8.6A: no position, no seek control — and
                                // no invented one.
                                if let (Some(label), Some(seek_ms)) =
                                    (seek_label(hit), hit.seek_ms)
                                {
                                    if ui.button(label).clicked() {
                                        intents.push(SearchIntent::Seek {
                                            recording_id: hit.recording_id,
                                            seek_ms,
                                        });
                                    }
                                }
                            });
                        });
                    }
                }
            });
        intents
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screens::library::testing::{recording_id, revision_id};

    fn hit(index: usize, seek_ms: Option<u64>, origin: AlignmentOrigin) -> SearchResultDto {
        SearchResultDto {
            recording_id: recording_id(index),
            recording_title: format!("Recording {index}"),
            transcript_revision_id: revision_id(index),
            seq_index: 3,
            excerpt: "we agreed to postpone".to_owned(),
            seek_ms,
            alignment_origin: origin,
        }
    }

    #[test]
    fn hits_are_grouped_by_recording_in_first_seen_order() {
        let results = vec![
            hit(1, None, AlignmentOrigin::None),
            hit(2, None, AlignmentOrigin::None),
            hit(1, None, AlignmentOrigin::None),
        ];
        let groups = group_by_recording(&results);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].0, "Recording 1");
        assert_eq!(groups[0].1.len(), 2);
        assert_eq!(groups[1].1.len(), 1);
    }

    #[test]
    fn a_text_only_hit_gets_no_seek_control() {
        assert_eq!(seek_label(&hit(1, None, AlignmentOrigin::None)), None);
        assert_eq!(
            seek_label(&hit(1, Some(1_000), AlignmentOrigin::None)),
            None,
            "a position without an alignment that permits seeking is not a position"
        );
    }

    #[test]
    fn a_derived_position_is_labelled_approximate() {
        assert_eq!(
            seek_label(&hit(1, Some(61_000), AlignmentOrigin::Derived)).as_deref(),
            Some("Play from about 1:01")
        );
        assert_eq!(
            seek_label(&hit(1, Some(61_000), AlignmentOrigin::Native)).as_deref(),
            Some("Play from 1:01")
        );
    }
}
