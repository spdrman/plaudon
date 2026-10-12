//! Interaction tests for the activity log, driven through AccessKit by
//! `egui_kittest` and the deterministic harness in `plaudon-ui-testkit`.
//!
//! §5.5 / §J13 is the point of most of them: the in-app history is complete
//! whatever the operating system did with the notification, and it says what
//! the operating system did — never that a notification was shown when it was
//! not.

use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use plaudon_app::command::AppCommand;
use plaudon_app::dto::{
    ActivityEntry, ActivityPage, ActivityTarget, ActivityTargetKind, ListActivityRequest,
    NotificationCategory,
};
use plaudon_ui::activity::{unread, ActivityLink, ActivityLog, ActivityOutput};
use plaudon_ui::ai::AiIntent;

/// A window tall enough that no entry is clipped.
const TALL: egui::Vec2 = egui::vec2(1280.0, 6000.0);

/* ------------------------------------------------------------- fixtures */

fn entry(id: &str, category: NotificationCategory, title: &str) -> ActivityEntry {
    ActivityEntry {
        activity_id: id.to_owned(),
        category,
        occurred_at: "2026-02-01T10:00:00Z".to_owned(),
        title: title.to_owned(),
        body: "Two speakers, 42 minutes.".to_owned(),
        notification_delivered: true,
        notification_suppressed_reason: None,
        target: None,
        read: false,
    }
}

fn page(items: Vec<ActivityEntry>) -> ActivityPage {
    let unread_count = unread(&items) as u64;
    ActivityPage {
        items,
        next_cursor: None,
        unread_count,
    }
}

/* ---------------------------------------------------------------- harness */

#[derive(Default)]
struct Collected {
    names: Vec<&'static str>,
    intents: Vec<String>,
    activity_queries: Vec<ListActivityRequest>,
    links: Vec<ActivityLink>,
}

impl Collected {
    fn absorb(&mut self, output: ActivityOutput) {
        self.links.extend(output.links);
        for intent in output.ai.intents {
            match intent {
                AiIntent::Command(command) => {
                    self.names.push(command.name());
                    if let AppCommand::ListActivity(request) = command {
                        self.activity_queries.push(request);
                    }
                }
                AiIntent::Navigate(intent) => self.intents.push(format!("{intent:?}")),
                other => self.intents.push(format!("{other:?}")),
            }
        }
    }
}

struct ActivityApp {
    log: ActivityLog,
    page: Option<ActivityPage>,
    collected: Collected,
}

fn harness(page: Option<ActivityPage>) -> Harness<'static, ActivityApp> {
    plaudon_ui_testkit::builder()
        .with_size(TALL)
        .build_ui_state(
            |ui, app: &mut ActivityApp| {
                let output = app.log.ui(ui, app.page.as_ref());
                app.collected.absorb(output);
            },
            ActivityApp {
                log: ActivityLog::default(),
                page,
                collected: Collected::default(),
            },
        )
}

fn announced(harness: &Harness<'_, ActivityApp>) -> String {
    plaudon_ui_testkit::a11y::nodes(harness)
        .into_iter()
        .filter_map(|node| node.announced().map(str::to_owned))
        .collect::<Vec<_>>()
        .join("\n")
}

/* ------------------------------------------------------------------ J11 */

#[test]
fn every_entry_shows_its_category_its_words_and_its_instant() {
    let mut imported = entry("a1", NotificationCategory::ImportFinished, "Board meeting");
    imported.body = "Copied into the archive.".to_owned();
    let attention = entry(
        "a2",
        NotificationCategory::RequiresAttention,
        "Standup needs a decision",
    );
    let mut harness = harness(Some(page(vec![imported, attention])));
    harness.run();

    let joined = announced(&harness);
    assert!(joined.contains("IN Import finished"), "{joined}");
    assert!(joined.contains("! Requires attention"), "{joined}");
    assert!(joined.contains("Board meeting"), "{joined}");
    assert!(joined.contains("Copied into the archive."), "{joined}");
    // The instant is the backend's, verbatim; no local reformatting invents a
    // precision or a timezone that was not sent.
    assert!(
        joined.contains("Occurred: 2026-02-01T10:00:00Z"),
        "{joined}"
    );
}

#[test]
fn read_and_unread_are_said_in_words_not_in_colour() {
    let mut seen = entry("a1", NotificationCategory::ImportFinished, "Board meeting");
    seen.read = true;
    let fresh = entry("a2", NotificationCategory::ImportFinished, "Standup");
    let mut harness = harness(Some(page(vec![seen, fresh])));
    harness.run();

    let joined = announced(&harness);
    assert!(joined.contains("- Read"), "{joined}");
    assert!(joined.contains("* Unread"), "{joined}");
    assert!(
        joined.contains("Unread when this page was read: 1"),
        "{joined}"
    );
}

#[test]
fn the_unread_count_is_the_same_count_the_header_bell_shows() {
    let mut seen = entry("a1", NotificationCategory::ImportFinished, "Board meeting");
    seen.read = true;
    let entries = vec![
        seen,
        entry("a2", NotificationCategory::ImportFinished, "Standup"),
        entry("a3", NotificationCategory::RequiresAttention, "Retro"),
    ];
    assert_eq!(unread(&entries), 2);
    assert_eq!(unread(entries.iter().filter(|e| e.read)), 0);
}

#[test]
fn a_category_filter_leaves_only_that_category_on_screen() {
    let entries = vec![
        entry("a1", NotificationCategory::ImportFinished, "Board meeting"),
        entry(
            "a2",
            NotificationCategory::TranscriptionFinished,
            "Standup transcript",
        ),
    ];
    let mut harness = harness(Some(page(entries)));
    harness.run();
    assert!(announced(&harness).contains("Standup transcript"));

    harness.get_by_label("Import finished").click();
    harness.run();

    let joined = announced(&harness);
    assert!(joined.contains("Board meeting"), "{joined}");
    assert!(!joined.contains("Standup transcript"), "{joined}");
    // Filtering is applied to the entries already read — `list_activity`
    // takes a cursor and a limit and nothing else — so no page is refetched.
    assert!(
        harness.state().collected.activity_queries.is_empty(),
        "{:?}",
        harness.state().collected.names
    );
    assert!(
        joined.contains("Unread in the categories shown: 1"),
        "{joined}"
    );
}

#[test]
fn a_job_target_produces_a_typed_job_link() {
    let mut entry = entry(
        "a1",
        NotificationCategory::TranscriptionFinished,
        "Standup transcript",
    );
    entry.target = Some(ActivityTarget {
        kind: ActivityTargetKind::Job,
        id: "job-7".to_owned(),
    });
    let mut harness = harness(Some(page(vec![entry])));
    harness.run();

    harness
        .get_by_label("Open the job for Standup transcript")
        .click();
    harness.run();
    assert_eq!(
        harness.state().collected.links,
        vec![ActivityLink::Job("job-7".to_owned())]
    );
}

#[test]
fn a_recording_target_produces_a_typed_recording_link() {
    let mut entry = entry("a1", NotificationCategory::ImportFinished, "Board meeting");
    entry.target = Some(ActivityTarget {
        kind: ActivityTargetKind::Recording,
        id: "rec-3".to_owned(),
    });
    let mut harness = harness(Some(page(vec![entry])));
    harness.run();

    harness
        .get_by_label("Open the recording for Board meeting")
        .click();
    harness.run();
    assert_eq!(
        harness.state().collected.links,
        vec![ActivityLink::Recording("rec-3".to_owned())]
    );
}

#[test]
fn a_settings_target_navigates_instead_of_producing_a_link() {
    let mut entry = entry(
        "a1",
        NotificationCategory::CloudBudgetExceeded,
        "Monthly cap reached",
    );
    entry.target = Some(ActivityTarget {
        kind: ActivityTargetKind::Settings,
        id: "settings".to_owned(),
    });
    let mut harness = harness(Some(page(vec![entry])));
    harness.run();

    harness
        .get_by_label("Open settings for Monthly cap reached")
        .click();
    harness.run();
    assert_eq!(
        harness.state().collected.links.len(),
        0,
        "settings is plain navigation and carries no id"
    );
    assert_eq!(
        harness.state().collected.intents,
        vec!["OpenSettings".to_owned()]
    );
}

#[test]
fn paging_asks_for_the_next_page_from_the_cursor_it_was_given() {
    let mut loaded = page(vec![entry(
        "a1",
        NotificationCategory::ImportFinished,
        "Board meeting",
    )]);
    loaded.next_cursor = Some("cursor-2".to_owned());
    let mut harness = harness(Some(loaded));
    harness.run();

    harness.get_by_label("Load older activity").click();
    harness.run();

    let queries = &harness.state().collected.activity_queries;
    assert_eq!(queries.len(), 1, "{:?}", harness.state().collected.names);
    assert_eq!(queries[0].cursor.as_deref(), Some("cursor-2"));
    assert_eq!(queries[0].limit, 25);
    // That page is also what a resync repeats.
    assert_eq!(
        harness.state().log.resync_request().cursor.as_deref(),
        Some("cursor-2")
    );
}

#[test]
fn an_empty_history_says_so_and_offers_to_read_it_again() {
    let mut harness = harness(Some(page(Vec::new())));
    harness.run();
    assert!(announced(&harness).contains("No activity to show"));

    harness.get_by_label("Refresh activity").click();
    harness.run();
    let queries = &harness.state().collected.activity_queries;
    assert_eq!(queries.len(), 1);
    assert_eq!(queries[0].cursor, None);
    assert_eq!(queries[0].limit, 25);
}

#[test]
fn before_the_first_response_the_log_says_it_has_not_read_the_history() {
    let mut harness = harness(None);
    harness.run();
    assert!(announced(&harness).contains("has not been read yet"));
}

/* ------------------------------------------------------------------ J13 */

#[test]
fn a_suppressed_notification_is_still_an_entry_and_explains_the_suppression() {
    let mut suppressed = entry(
        "a1",
        NotificationCategory::TranscriptionFinished,
        "Standup transcript",
    );
    suppressed.notification_delivered = false;
    suppressed.notification_suppressed_reason =
        Some("macOS has notifications for Plaudon turned off.".to_owned());
    let mut harness = harness(Some(page(vec![suppressed])));
    harness.run();

    let joined = announced(&harness);
    assert!(joined.contains("Standup transcript"), "the entry is kept");
    assert!(
        joined.contains(
            "Desktop notification not delivered: macOS has notifications for Plaudon turned off."
        ),
        "{joined}"
    );
    assert!(
        !joined.contains("Desktop notification delivered."),
        "no delivery is ever claimed: {joined}"
    );
}

#[test]
fn a_suppression_without_a_reported_reason_says_the_reason_is_missing() {
    let mut suppressed = entry("a1", NotificationCategory::ImportFinished, "Board meeting");
    suppressed.notification_delivered = false;
    let mut harness = harness(Some(page(vec![suppressed])));
    harness.run();
    assert!(
        announced(&harness).contains("Desktop notification not delivered; no reason was reported.")
    );
}

/* ------------------------------------------------------------- keyboard */

#[test]
fn every_control_of_the_activity_log_is_reachable_by_keyboard() {
    let mut imported = entry("a1", NotificationCategory::ImportFinished, "Board meeting");
    imported.target = Some(ActivityTarget {
        kind: ActivityTargetKind::Recording,
        id: "rec-3".to_owned(),
    });
    let mut transcribed = entry(
        "a2",
        NotificationCategory::TranscriptionFinished,
        "Standup transcript",
    );
    transcribed.target = Some(ActivityTarget {
        kind: ActivityTargetKind::Job,
        id: "job-7".to_owned(),
    });
    let mut loaded = page(vec![imported, transcribed]);
    loaded.next_cursor = Some("cursor-2".to_owned());

    let mut harness = harness(Some(loaded));
    harness.run_steps(4);

    let order = plaudon_ui_testkit::a11y::focus_order(&mut harness, 40);
    for expected in [
        "Import finished",
        "Transcription finished",
        "Requires attention",
        "Cloud budget exceeded",
        "Open the recording for Board meeting",
        "Open the job for Standup transcript",
        "Load older activity",
    ] {
        assert!(
            order.iter().any(|label| label == expected),
            "{expected:?} is not keyboard reachable: {order:?}"
        );
    }
}
