//! Processing history: every job that ran for this recording.
//!
//! The rows are [`super::JobCard`]s, so the state labels, the glyphs, the
//! progress rule and the cancel and retry gates are the same here as
//! everywhere else. The list itself is a page of `list_jobs`; this panel adds
//! the filters and asks for the next page, and never invents a total it was
//! not given.

use egui::Ui;
use plaudon_app::command::AppCommand;
use plaudon_app::dto::{JobKind, JobPage, JobState, ListJobsRequest};
use plaudon_app::job_presentation::{job_kind_label, job_state_presentation};

use super::{AiOutput, JobCard};

/// Page size of the history list, matching the jobs screen (§J1).
pub const PAGE_SIZE: u32 = 50;

/// The processing history of one recording.
#[derive(Debug, Default)]
pub struct ProcessingHistory {
    states: Vec<JobState>,
    kinds: Vec<JobKind>,
    cards: Vec<(String, JobCard)>,
}

impl ProcessingHistory {
    /// The request that reloads the first page under the current filters.
    #[must_use]
    pub fn refresh_request(&self) -> ListJobsRequest {
        ListJobsRequest {
            cursor: None,
            limit: PAGE_SIZE,
            states: self.states.clone(),
            kinds: self.kinds.clone(),
        }
    }

    /// Draw the history.
    ///
    /// `page` is the last `list_jobs` response, already filtered to this
    /// recording by the caller.
    pub fn ui(&mut self, ui: &mut Ui, page: Option<&JobPage>, now_ms: i128) -> AiOutput {
        let mut output = AiOutput::default();
        ui.label(egui::RichText::new("Processing history").heading());

        let mut filters_changed = false;
        ui.horizontal_wrapped(|ui| {
            ui.label("Kind:");
            for kind in JobKind::ALL {
                let mut on = self.kinds.contains(kind);
                if ui.checkbox(&mut on, job_kind_label(*kind)).changed() {
                    toggle(&mut self.kinds, *kind, on);
                    filters_changed = true;
                }
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("State:");
            for state in JobState::ALL {
                let mut on = self.states.contains(state);
                if ui
                    .checkbox(&mut on, job_state_presentation(*state).label)
                    .changed()
                {
                    toggle(&mut self.states, *state, on);
                    filters_changed = true;
                }
            }
        });
        if filters_changed {
            output.command(AppCommand::ListJobs(self.refresh_request()));
        }

        let Some(page) = page else {
            ui.label("The processing history has not been read yet.");
            return output;
        };
        if page.items.is_empty() {
            ui.label("No job has run for this recording with these filters.");
            return output;
        }

        for (state, count) in &page.state_counts {
            ui.label(format!("{}: {count}", job_state_presentation(*state).label));
        }

        for job in &page.items {
            let job_id = job.job_id.to_string();
            let slot = match self.cards.iter().position(|(id, _)| *id == job_id) {
                Some(index) => index,
                None => {
                    self.cards.push((job_id.clone(), JobCard::default()));
                    self.cards.len() - 1
                }
            };
            let card_output = self.cards[slot].1.ui(ui, job, now_ms);
            output.extend(card_output);
        }

        if let Some(cursor) = page.next_cursor.as_deref() {
            if ui.button("Show older jobs").clicked() {
                output.command(AppCommand::ListJobs(ListJobsRequest {
                    cursor: Some(cursor.to_owned()),
                    limit: PAGE_SIZE,
                    states: self.states.clone(),
                    kinds: self.kinds.clone(),
                }));
            }
        }
        output
    }
}

fn toggle<T: PartialEq>(values: &mut Vec<T>, value: T, on: bool) {
    if on {
        if !values.contains(&value) {
            values.push(value);
        }
    } else {
        values.retain(|existing| *existing != value);
    }
}
