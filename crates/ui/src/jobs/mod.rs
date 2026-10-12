//! The jobs screen: every job in the archive, filtered, paged, corrected.
//!
//! The screen owns the list — the filters, the page, the deep link and the
//! capacity comparison a full disk needs — and nothing else. Each row is a
//! [`crate::ai::JobCard`], so the state label, the glyph, the progress rule,
//! the cancel disclosure, the cost-acknowledged retry and the §5.4 corrective
//! controls are the same words here as in the processing history on a
//! recording, and they are decided once, in
//! [`plaudon_app::job_presentation`].
//!
//! Nothing on this screen is computed from the data: no total that was not
//! sent, no percentage that was not reported, no free-space figure this
//! process guessed.

pub mod shell_adapter;

use egui::{RichText, Ui};
use plaudon_app::command::AppCommand;
use plaudon_app::dto::{
    JobKind, JobPage, JobState, JobSummary, ListJobsRequest, StorageCapability,
};
use plaudon_app::job_presentation::{
    error_guidance, job_kind_label, job_state_presentation, ControlIntent,
};

use crate::ai::{fact, AiIntent, AiOutput, JobCard};

/// Rows requested per page (§J1).
pub const PAGE_SIZE: u32 = 50;

/// The jobs screen.
///
/// Holds what survives a frame: the filters, the cursor of the page on
/// screen, the deep link being honoured, the storage facts the capacity
/// comparison needs, and one [`JobCard`] per row so a half-finished
/// cancellation or a ticked cost acknowledgement is not lost on redraw.
#[derive(Debug, Default)]
pub struct JobsScreen {
    states: Vec<JobState>,
    kinds: Vec<JobKind>,
    cursor: Option<String>,
    storage: Option<StorageCapability>,
    cards: Vec<(String, JobCard)>,
    focus: Option<String>,
    focus_pending: bool,
}

impl JobsScreen {
    /// Give the screen the storage facts `get_capabilities` reported.
    ///
    /// Passed in, never read: the UI reaches no port. `None` means this
    /// process has not been told, and the capacity comparison then says the
    /// free space is unknown instead of inventing one (§8.6A).
    pub fn set_storage(&mut self, storage: Option<StorageCapability>) {
        self.storage = storage;
    }

    /// Focus and scroll to one job, from a deep link (§J8).
    ///
    /// Honoured on the first frame that renders the row; if the job is not on
    /// the page yet, the request is kept until a page containing it arrives.
    pub fn focus_job(&mut self, job_id: &str) {
        self.focus = Some(job_id.to_owned());
        self.focus_pending = true;
    }

    /// The request that reloads the page on screen under the current filters.
    ///
    /// This is the recovery for a dropped `job.updated` event (§J9): the same
    /// cursor and the same filters, so the user keeps the page they were
    /// looking at.
    #[must_use]
    pub fn resync_request(&self) -> ListJobsRequest {
        ListJobsRequest {
            cursor: self.cursor.clone(),
            limit: PAGE_SIZE,
            states: self.states.clone(),
            kinds: self.kinds.clone(),
        }
    }

    /// The request for the first page under the current filters.
    #[must_use]
    pub fn first_page_request(&self) -> ListJobsRequest {
        ListJobsRequest {
            cursor: None,
            limit: PAGE_SIZE,
            states: self.states.clone(),
            kinds: self.kinds.clone(),
        }
    }

    /// Draw the screen.
    ///
    /// `page` is the last `list_jobs` response; `None` before one arrives.
    /// `now_ms` is the wall clock elapsed labels are measured against, passed
    /// in so a test is not at the mercy of the host's clock.
    pub fn ui(&mut self, ui: &mut Ui, page: Option<&JobPage>, now_ms: i128) -> AiOutput {
        let mut output = AiOutput::default();
        ui.label(RichText::new("Jobs").heading());

        if self.filters(ui, page) {
            // A filter change starts the list again from the first page: a
            // cursor taken under the old filters means nothing under the new
            // ones.
            self.cursor = None;
            output.command(AppCommand::ListJobs(self.first_page_request()));
        }

        let Some(page) = page else {
            ui.label("The job list has not been read yet.");
            return output;
        };

        // Counts as the backend reported them, per state. There is no total:
        // `list_jobs` does not send one, so the screen does not show one.
        if page.state_counts.is_empty() {
            ui.label("No per-state counts were reported for this page.");
        } else {
            for (state, count) in &page.state_counts {
                ui.label(format!("{}: {count}", job_state_presentation(*state).label));
            }
        }

        if page.items.is_empty() {
            ui.label(
                "No job matches these filters. Imports and transcriptions appear here while they \
                 run, and stay here afterwards.",
            );
            return output;
        }

        let mut focused_row_drawn = false;
        for job in &page.items {
            focused_row_drawn |= self.row(ui, job, now_ms, &mut output);
        }
        if focused_row_drawn {
            self.focus_pending = false;
        }

        if let Some(cursor) = page.next_cursor.as_deref() {
            if ui.button("Show more jobs").clicked() {
                self.cursor = Some(cursor.to_owned());
                output.command(AppCommand::ListJobs(self.resync_request()));
            }
        }
        output
    }

    /// Draw the filters; `true` when the user changed one.
    fn filters(&mut self, ui: &mut Ui, page: Option<&JobPage>) -> bool {
        let mut changed = false;

        ui.label("Filter by state");
        // Only the states this page actually has rows in, plus whatever is
        // already selected: offering all eighteen would be a wall of controls
        // claiming jobs that do not exist.
        let offered: Vec<JobState> = JobState::ALL
            .iter()
            .copied()
            .filter(|state| {
                self.states.contains(state)
                    || page.is_some_and(|page| {
                        page.state_counts
                            .get(state)
                            .copied()
                            .is_some_and(|count| count > 0)
                    })
            })
            .collect();
        if offered.is_empty() {
            ui.label("No job states to filter yet.");
        } else {
            ui.horizontal_wrapped(|ui| {
                for state in offered {
                    let mut on = self.states.contains(&state);
                    if ui
                        .checkbox(&mut on, job_state_presentation(state).label)
                        .changed()
                    {
                        toggle(&mut self.states, state, on);
                        changed = true;
                    }
                }
            });
        }

        ui.label("Filter by kind");
        ui.horizontal_wrapped(|ui| {
            for kind in JobKind::ALL {
                let mut on = self.kinds.contains(kind);
                if ui.checkbox(&mut on, job_kind_label(*kind)).changed() {
                    toggle(&mut self.kinds, *kind, on);
                    changed = true;
                }
            }
        });

        changed
    }

    /// Draw one row; `true` when it was the deep-linked one.
    fn row(&mut self, ui: &mut Ui, job: &JobSummary, now_ms: i128, output: &mut AiOutput) -> bool {
        let job_id = job.job_id.to_string();
        let deep_linked = self.focus.as_deref() == Some(job_id.as_str());

        // The row's keyboard anchor. A deep link lands here, and it says so in
        // words, because an outline alone does not reach a screen reader.
        let anchor = if deep_linked {
            format!("Job: {} — opened from a link", job.label)
        } else {
            format!("Job: {}", job.label)
        };
        let response =
            ui.add(egui::Label::new(RichText::new(anchor).strong()).sense(egui::Sense::click()));
        if deep_linked && self.focus_pending {
            response.scroll_to_me(Some(egui::Align::Center));
            response.request_focus();
        }

        let slot = if let Some(index) = self.cards.iter().position(|(id, _)| *id == job_id) {
            index
        } else {
            self.cards.push((job_id.clone(), JobCard::default()));
            self.cards.len() - 1
        };
        let card_output = self.cards[slot].1.ui(ui, job, now_ms);
        output.extend(card_output);

        self.capacity(ui, job, output);

        deep_linked
    }

    /// The §J7 capacity comparison, for a row whose failure demands one.
    fn capacity(&self, ui: &mut Ui, job: &JobSummary, output: &mut AiOutput) {
        let compares_capacity = job
            .error
            .as_ref()
            .and_then(|envelope| error_guidance(envelope.code))
            .is_some_and(|guidance| guidance.show_capacity);
        if !compares_capacity {
            return;
        }

        if let Some(storage) = self.storage.as_ref() {
            fact(ui, "Library volume", &storage.library_path);
            // Verbatim byte counts, both of them: a rounded "0.1 GB free" is a
            // figure the backend never sent.
            ui.label(format!(
                "Free space: {} bytes of {} bytes",
                storage.free_bytes, storage.total_bytes
            ));
        } else {
            ui.label(
                "Free space on the library volume is unknown: this screen has not been given the \
                 storage capability.",
            );
        }
        if ui
            .button("Open storage settings for the library volume")
            .clicked()
        {
            output.push(AiIntent::Navigate(ControlIntent::OpenStorageSettings));
        }
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
