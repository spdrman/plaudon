//! The activity log: every completion and every call for attention, kept
//! whether or not the operating system ever showed a notification.
//!
//! §5.5 / §J13: the in-app history is the complete record. When the OS
//! suppressed a notification the entry still exists and carries the reason it
//! was suppressed, which this widget renders verbatim. It never claims a
//! notification was delivered, and it never claims one was not when the
//! backend said it was.
//!
//! Titles, bodies and instants are the backend's words and the backend's
//! timestamps, rendered as sent.

pub mod shell_adapter;

use egui::{RichText, Ui};
use plaudon_app::command::AppCommand;
use plaudon_app::dto::{
    ActivityEntry, ActivityPage, ActivityTargetKind, ListActivityRequest, NotificationCategory,
};
use plaudon_app::job_presentation::ControlIntent;
use plaudon_app::tone::Tone;

use crate::ai::{fact, AiIntent, AiOutput, Badge};

/// Entries requested per page.
pub const PAGE_SIZE: u32 = 25;

/// Where an activity entry's deep link points, when it points at something
/// the shell addresses by id.
///
/// `list_activity` also targets the settings screen; that is plain navigation
/// and travels as [`AiIntent::Navigate`], which carries no id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActivityLink {
    /// Open this job on the jobs screen.
    Job(String),
    /// Open this recording.
    Recording(String),
}

/// What one frame of the activity log produced.
#[derive(Debug, Default)]
pub struct ActivityOutput {
    /// Commands and navigation, in the order the user produced them.
    pub ai: AiOutput,
    /// Deep links that name a job or a recording.
    pub links: Vec<ActivityLink>,
}

/// How a notification category is announced: a glyph, a label and a tone.
///
/// Status is never colour alone (§5.3), so the glyph and the label are both
/// text and the tone only reinforces them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CategoryPresentation {
    /// Words for the category.
    pub label: &'static str,
    /// Glyph shown beside the label.
    pub glyph: &'static str,
    /// Tone the badge takes.
    pub tone: Tone,
}

/// The presentation of a notification category.
#[must_use]
pub const fn category_presentation(category: NotificationCategory) -> CategoryPresentation {
    match category {
        NotificationCategory::ImportFinished => CategoryPresentation {
            label: "Import finished",
            glyph: "IN",
            tone: Tone::Success,
        },
        NotificationCategory::TranscriptionFinished => CategoryPresentation {
            label: "Transcription finished",
            glyph: "TR",
            tone: Tone::Success,
        },
        NotificationCategory::RequiresAttention => CategoryPresentation {
            label: "Requires attention",
            glyph: "!",
            tone: Tone::Warning,
        },
        NotificationCategory::CloudBudgetExceeded => CategoryPresentation {
            label: "Cloud budget exceeded",
            glyph: "$",
            tone: Tone::Danger,
        },
    }
}

/// How many of these entries the user has not seen.
///
/// Takes anything that yields borrowed entries — a page's items, or the
/// subset a filter leaves on screen — because the header bell counts the same
/// entries the log does, and two counts of one thing is one count too many.
#[must_use]
pub fn unread<'a>(entries: impl IntoIterator<Item = &'a ActivityEntry>) -> usize {
    entries.into_iter().filter(|entry| !entry.read).count()
}

/// The activity log.
#[derive(Debug, Default)]
pub struct ActivityLog {
    categories: Vec<NotificationCategory>,
    cursor: Option<String>,
}

impl ActivityLog {
    /// The request that reads the first page again.
    #[must_use]
    pub fn refresh_request(&self) -> ListActivityRequest {
        ListActivityRequest {
            cursor: None,
            limit: PAGE_SIZE,
        }
    }

    /// The request that reads the page on screen again.
    #[must_use]
    pub fn resync_request(&self) -> ListActivityRequest {
        ListActivityRequest {
            cursor: self.cursor.clone(),
            limit: PAGE_SIZE,
        }
    }

    /// Draw the log.
    ///
    /// `page` is the last `list_activity` response; `None` before one
    /// arrives.
    pub fn ui(&mut self, ui: &mut Ui, page: Option<&ActivityPage>) -> ActivityOutput {
        let mut output = ActivityOutput::default();
        ui.label(RichText::new("Activity history").heading());
        ui.label(
            "Every completion and attention event is recorded here, even when desktop \
             notifications are turned off or denied by the operating system.",
        );

        // The category filter is applied to the entries already read:
        // `list_activity` takes a cursor and a limit and nothing else, so
        // asking the backend for one category is not something this screen
        // can honestly offer.
        ui.label("Show categories");
        ui.horizontal_wrapped(|ui| {
            for category in NotificationCategory::ALL {
                let mut on = self.categories.contains(category);
                if ui
                    .checkbox(&mut on, category_presentation(*category).label)
                    .changed()
                {
                    if on {
                        if !self.categories.contains(category) {
                            self.categories.push(*category);
                        }
                    } else {
                        self.categories.retain(|existing| existing != category);
                    }
                }
            }
        });

        let Some(page) = page else {
            ui.label("The activity history has not been read yet.");
            if ui.button("Read activity history").clicked() {
                output
                    .ai
                    .command(AppCommand::ListActivity(self.refresh_request()));
            }
            return output;
        };

        let visible: Vec<&ActivityEntry> = page
            .items
            .iter()
            .filter(|entry| self.categories.is_empty() || self.categories.contains(&entry.category))
            .collect();

        fact(
            ui,
            "Unread when this page was read",
            &page.unread_count.to_string(),
        );
        if !self.categories.is_empty() {
            // The backend's count is of everything; this one is of what the
            // filter leaves on screen, and says so.
            fact(
                ui,
                "Unread in the categories shown",
                &unread(visible.iter().copied()).to_string(),
            );
        }

        if visible.is_empty() {
            ui.label(
                "No activity to show. Imports, finished transcriptions and anything needing your \
                 attention are listed here.",
            );
            if ui.button("Refresh activity").clicked() {
                output
                    .ai
                    .command(AppCommand::ListActivity(self.refresh_request()));
            }
            return output;
        }

        for entry in visible {
            entry_ui(ui, entry, &mut output);
        }

        if let Some(cursor) = page.next_cursor.as_deref() {
            if ui.button("Load older activity").clicked() {
                self.cursor = Some(cursor.to_owned());
                output
                    .ai
                    .command(AppCommand::ListActivity(self.resync_request()));
            }
        }

        output
    }
}

fn entry_ui(ui: &mut Ui, entry: &ActivityEntry, output: &mut ActivityOutput) {
    ui.push_id(entry.activity_id.clone(), |ui| {
        ui.group(|ui| {
            let category = category_presentation(entry.category);
            Badge::new(category.glyph, category.label, category.tone).ui(ui);
            // Read state in words, never a colour and never a dot alone.
            if entry.read {
                Badge::new("-", "Read", Tone::Neutral).ui(ui);
            } else {
                Badge::new("*", "Unread", Tone::Running).ui(ui);
            }

            ui.label(RichText::new(&entry.title).strong());
            if !entry.body.is_empty() {
                ui.label(&entry.body);
            }
            fact(ui, "Occurred", &entry.occurred_at);

            // §J13: what the operating system did, as it happened.
            if entry.notification_delivered {
                ui.label("Desktop notification delivered.");
            } else if let Some(reason) = entry.notification_suppressed_reason.as_deref() {
                ui.label(format!("Desktop notification not delivered: {reason}"));
            } else {
                ui.label("Desktop notification not delivered; no reason was reported.");
            }

            let Some(target) = entry.target.as_ref() else {
                return;
            };
            let label = match target.kind {
                ActivityTargetKind::Job => format!("Open the job for {}", entry.title),
                ActivityTargetKind::Recording => {
                    format!("Open the recording for {}", entry.title)
                }
                ActivityTargetKind::Settings => format!("Open settings for {}", entry.title),
            };
            if ui.button(label).clicked() {
                match target.kind {
                    ActivityTargetKind::Job => {
                        output.links.push(ActivityLink::Job(target.id.clone()));
                    }
                    ActivityTargetKind::Recording => {
                        output
                            .links
                            .push(ActivityLink::Recording(target.id.clone()));
                    }
                    ActivityTargetKind::Settings => {
                        output
                            .ai
                            .push(AiIntent::Navigate(ControlIntent::OpenSettings));
                    }
                }
            }
        });
    });
}
