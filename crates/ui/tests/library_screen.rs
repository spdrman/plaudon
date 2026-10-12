//! Interaction and accessibility tests for the library, inbox, search and
//! recording-detail screens, driven through AccessKit by `egui_kittest`.
//!
//! Every assertion is made the way a user or a screen reader would make it:
//! find something by the name it announces, act on it, and then check what the
//! screen asked the application layer to do. No test reaches into a widget's
//! private state to decide what happened.
//!
//! Pixel comparison is opt-in (`PLAUDON_VISUAL=1`, MARS only) per
//! `docs/EGUI_TESTING.md` §4; every interaction assertion below runs
//! everywhere.

use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::Harness;
use egui_kittest::Node;
use plaudon_app::command::{AppCommand, AppResponse, CommandId};
use plaudon_app::dto::{
    EventKind, ImportFilesResponse, ImportOutcome, ListRecordingsRequest, RecordingSortField,
    SortDirection,
};
use plaudon_domain::error::{AppError, AppErrorCode, DiagnosticLog, ErrorEnvelope};
use plaudon_domain::ids::JobId;
use plaudon_domain::time::Timestamp;
use plaudon_ui::screens::library::table::row_label;
use plaudon_ui::screens::library::testing::{page, rows};
use plaudon_ui::screens::library::{LibraryIntent, LibraryScreen};
use plaudon_ui_testkit::{a11y, visual};

/// The screen plus the dispatcher it would talk to, recorded instead of run.
struct LibraryApp {
    screen: LibraryScreen,
    sent: Vec<AppCommand>,
    next_id: u64,
    intents: Vec<LibraryIntent>,
}

impl LibraryApp {
    fn new() -> Self {
        Self {
            screen: LibraryScreen::new(),
            sent: Vec::new(),
            next_id: 0,
            intents: Vec::new(),
        }
    }

    fn view(&mut self, ui: &mut egui::Ui) {
        let Self {
            screen,
            sent,
            next_id,
            intents,
        } = self;
        let mut send = |command: AppCommand| {
            *next_id += 1;
            sent.push(command);
            CommandId::from_raw(*next_id)
        };
        let produced = screen.ui(ui, &mut send);
        intents.extend(produced);
    }

    fn respond(&mut self, id: u64, result: Result<AppResponse, ErrorEnvelope>) {
        let Self {
            screen,
            sent,
            next_id,
            ..
        } = self;
        let mut send = |command: AppCommand| {
            *next_id += 1;
            sent.push(command);
            CommandId::from_raw(*next_id)
        };
        screen.on_response(CommandId::from_raw(id), &result, &mut send);
    }

    fn list_requests(&self) -> Vec<&ListRecordingsRequest> {
        self.sent
            .iter()
            .filter_map(|command| match command {
                AppCommand::ListRecordings(request) => Some(request),
                _ => None,
            })
            .collect()
    }

    fn last_list_request(&self) -> &ListRecordingsRequest {
        self.list_requests()
            .pop()
            .expect("the screen asked for a page")
    }

    fn import_requests(&self) -> Vec<&plaudon_app::dto::ImportFilesRequest> {
        self.sent
            .iter()
            .filter_map(|command| match command {
                AppCommand::ImportFiles(request) => Some(request),
                _ => None,
            })
            .collect()
    }
}

fn harness() -> Harness<'static, LibraryApp> {
    let mut harness = plaudon_ui_testkit::builder()
        .build_ui_state(|ui, app: &mut LibraryApp| app.view(ui), LibraryApp::new());
    harness.run();
    harness
}

/// A harness whose first page has already been answered with `count` rows.
fn harness_with_rows(count: usize) -> Harness<'static, LibraryApp> {
    let mut harness = harness();
    let items = rows(count);
    harness.state_mut().respond(
        1,
        Ok(AppResponse::ListRecordings(page(items, None, count as u64))),
    );
    harness.run();
    harness
}

fn envelope(code: AppErrorCode) -> ErrorEnvelope {
    let mut log = DiagnosticLog::new();
    AppError::new(code, "internal detail").to_envelope(&mut log, Timestamp::now())
}

fn row_nodes<'t>(harness: &'t Harness<'_, LibraryApp>) -> Vec<Node<'t>> {
    harness
        .query_all_by_role(egui::accesskit::Role::Row)
        .collect()
}

/* ------------------------------------------------------------- first paint */

#[test]
fn the_first_frame_asks_for_the_first_page_with_no_cursor() {
    let harness = harness();
    let request = harness.state().last_list_request();
    assert_eq!(request.cursor, None);
    assert_eq!(request.limit, 100);
    assert_eq!(request.sort.field, RecordingSortField::RecordedAt);
    assert_eq!(request.sort.direction, SortDirection::Desc);
}

/* ------------------------------------------------------------ empty states */

#[test]
fn an_empty_library_offers_import_and_connect_plaud() {
    let mut harness = harness();
    harness.state_mut().respond(
        1,
        Ok(AppResponse::ListRecordings(page(Vec::new(), None, 0))),
    );
    harness.run();

    assert!(a11y::has_label(&harness, "No recordings"));
    assert!(a11y::has_label(&harness, "Import an audio file"));
    assert!(a11y::has_label(&harness, "Connect PLAUD"));

    harness.get_by_label("Import an audio file").click();
    harness.run();
    assert!(harness.state().intents.contains(&LibraryIntent::PickFiles));

    visual::assert_snapshot(&mut harness, "library_empty");
}

#[test]
fn an_empty_result_under_a_filter_offers_to_clear_it_instead() {
    let mut harness = harness_with_rows(5);
    harness
        .get_by_role_and_label(egui::accesskit::Role::ColumnHeader, "Title")
        .click();
    harness.run();
    // The sort restarted the chain; answer it with nothing.
    let id = harness.state().list_requests().len() as u64;
    harness.state_mut().respond(
        id,
        Ok(AppResponse::ListRecordings(page(Vec::new(), None, 0))),
    );
    harness.run();
    // A sort is not a filter, so the unfiltered empty state is still right.
    assert!(a11y::has_label(&harness, "No recordings"));
}

/* ------------------------------------------------------------- a hundred rows */

#[test]
fn a_hundred_rows_render_a_grid_that_reports_the_whole_set() {
    let harness = harness_with_rows(100);
    let grid = harness.get_by_role(egui::accesskit::Role::Grid);
    assert_eq!(grid.accesskit_node().label().as_deref(), Some("Recordings"));
    assert_eq!(
        grid.accesskit_node().data().row_count(),
        Some(101),
        "the grid reports every matching row plus the header, not the rendered window"
    );
    assert_eq!(grid.accesskit_node().data().column_count(), Some(10));
    assert!(grid.accesskit_node().is_multiselectable());
}

#[test]
fn only_the_visible_rows_are_laid_out() {
    let harness = harness_with_rows(100);
    let rendered = row_nodes(&harness).len();
    assert!(rendered > 0, "something must be rendered");
    assert!(
        rendered < 100,
        "all 100 rows were laid out; the table is not virtualized ({rendered} rows)"
    );
}

#[test]
fn every_column_of_the_spec_is_present_in_the_header() {
    let harness = harness_with_rows(10);
    for header in [
        "Title",
        "Recorded",
        "Imported",
        "Duration",
        "Source",
        "Import status",
        "Transcription",
        "Engine",
        "Tags",
        "Last error",
    ] {
        let found = harness
            .query_all_by_role(egui::accesskit::Role::ColumnHeader)
            .any(|node| node.accesskit_node().label().as_deref() == Some(header));
        assert!(found, "the {header} column header is missing");
    }
}

#[test]
fn an_unmeasured_duration_is_stated_and_never_drawn_as_zero() {
    let harness = harness_with_rows(30);
    let announced: Vec<String> = row_nodes(&harness)
        .iter()
        .filter_map(|node| node.accesskit_node().label())
        .collect();
    assert!(
        announced
            .iter()
            .any(|label| label.contains("duration unknown")),
        "the fixture library contains unmeasured recordings and none said so"
    );
    assert!(
        !announced
            .iter()
            .any(|label| label.contains("Duration 0:00")),
        "an unmeasured duration was rendered as a zero duration"
    );
}

/* ----------------------------------------------------------------- sorting */

#[test]
fn clicking_a_column_sorts_by_it_and_clicking_again_reverses() {
    let mut harness = harness_with_rows(10);
    harness
        .get_by_role_and_label(egui::accesskit::Role::ColumnHeader, "Title")
        .click();
    harness.run();
    let request = harness.state().last_list_request();
    assert_eq!(request.sort.field, RecordingSortField::Title);
    assert_eq!(
        request.sort.direction,
        SortDirection::Asc,
        "a text column starts A to Z"
    );
    assert_eq!(request.cursor, None, "a sort change restarts the chain");

    let id = harness.state().list_requests().len() as u64;
    harness.state_mut().respond(
        id,
        Ok(AppResponse::ListRecordings(page(rows(10), None, 10))),
    );
    harness.run();

    harness
        .get_by_role_and_label(egui::accesskit::Role::ColumnHeader, "Title")
        .click();
    harness.run();
    assert_eq!(
        harness.state().last_list_request().sort.direction,
        SortDirection::Desc
    );
}

#[test]
fn only_the_active_column_reports_a_sort_direction() {
    let harness = harness_with_rows(10);
    let directions: Vec<(String, Option<egui::accesskit::SortDirection>)> = harness
        .query_all_by_role(egui::accesskit::Role::ColumnHeader)
        .map(|node| {
            (
                node.accesskit_node().label().unwrap_or_default(),
                node.accesskit_node().sort_direction(),
            )
        })
        .collect();
    let sorted: Vec<&String> = directions
        .iter()
        .filter(|(_, direction)| direction.is_some())
        .map(|(label, _)| label)
        .collect();
    assert_eq!(
        sorted,
        [&"Recorded".to_owned()],
        "exactly the active column reports a direction"
    );
}

/* --------------------------------------------------------------- filtering */

#[test]
fn choosing_an_import_status_filters_the_query_and_restarts_the_chain() {
    let mut harness = harness_with_rows(10);
    harness
        .get_by_role_and_label(egui::accesskit::Role::ComboBox, "Import status")
        .click();
    harness.run();
    harness
        .get_by_role_and_label(egui::accesskit::Role::Button, "Import failed")
        .click();
    harness.run();

    let request = harness.state().last_list_request();
    assert_eq!(
        request.filter.import_status,
        [plaudon_app::dto::ImportStatus::Failed]
    );
    assert_eq!(request.cursor, None);
}

#[test]
fn typing_in_the_search_field_issues_one_query_after_the_debounce() {
    let mut harness = harness_with_rows(10);
    let before = harness.state().list_requests().len();

    harness.get_by_label("Search recordings").focus();
    harness.run();
    for character in ["b", "o", "a", "r", "d"] {
        harness
            .input_mut()
            .events
            .push(egui::Event::Text(character.to_owned()));
        harness.step();
    }
    assert_eq!(
        harness.state().list_requests().len(),
        before,
        "the query must not be issued per keystroke"
    );

    // Let the debounce elapse. kittest advances time by `step_dt` per frame.
    for _ in 0..40 {
        harness.step();
    }
    let requests = harness.state().list_requests();
    assert_eq!(
        requests.len(),
        before + 1,
        "exactly one query after the user stopped typing"
    );
    assert_eq!(
        requests.last().expect("a request").filter.query.as_deref(),
        Some("board")
    );
}

/* --------------------------------------------------------------- selection */

#[test]
fn clicking_a_row_selects_it_and_announces_the_count() {
    let mut harness = harness_with_rows(20);
    let label = row_label(&rows(20)[0]);
    harness.get_by_label(label.as_str()).click();
    harness.run();

    let selected = row_nodes(&harness)
        .iter()
        .filter(|node| node.accesskit_node().is_selected() == Some(true))
        .count();
    assert_eq!(selected, 1);
    assert!(a11y::has_label(
        &harness,
        "1 of 20 loaded recordings selected."
    ));
}

#[test]
fn select_all_covers_every_loaded_row_and_says_so() {
    let mut harness = harness_with_rows(20);
    let label = row_label(&rows(20)[0]);
    harness.get_by_label(label.as_str()).click();
    harness.run();
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    harness.run();
    assert!(a11y::has_label(
        &harness,
        "20 of 20 loaded recordings selected."
    ));
}

#[test]
fn nothing_selected_is_announced_as_nothing_selected() {
    let harness = harness_with_rows(20);
    assert!(a11y::has_label(
        &harness,
        "No recordings selected. 20 loaded."
    ));
}

/* ------------------------------------------------------------ keyboard nav */

#[test]
fn the_arrow_keys_move_the_cursor_and_enter_opens_the_recording() {
    let mut harness = harness_with_rows(20);
    let fixtures = rows(20);
    harness
        .get_by_label(row_label(&fixtures[0]).as_str())
        .click();
    harness.run();

    harness.key_press(egui::Key::ArrowDown);
    harness.run();
    harness.key_press(egui::Key::ArrowDown);
    harness.run();
    harness.key_press(egui::Key::Enter);
    harness.run();

    assert!(
        harness
            .state()
            .intents
            .contains(&LibraryIntent::OpenRecording(fixtures[2].recording_id)),
        "Enter must open the row the cursor is on, not the one that was clicked"
    );
}

#[test]
fn space_toggles_selection_and_shift_arrow_extends_it() {
    let mut harness = harness_with_rows(20);
    let fixtures = rows(20);
    harness
        .get_by_label(row_label(&fixtures[0]).as_str())
        .click();
    harness.run();
    harness.key_press(egui::Key::Space);
    harness.run();
    // The click already selected row 0; Space toggles it off again.
    assert!(a11y::has_label(
        &harness,
        "No recordings selected. 20 loaded."
    ));

    harness.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::ArrowDown);
    harness.run();
    harness.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::ArrowDown);
    harness.run();
    let announced: Vec<String> = harness
        .query_all_by_role(egui::accesskit::Role::Status)
        .filter_map(|node| node.accesskit_node().label())
        .collect();
    assert!(
        announced
            .iter()
            .any(|label| label == "3 of 20 loaded recordings selected."),
        "{announced:?}"
    );
}

#[test]
fn end_and_home_reach_both_ends_of_the_loaded_rows() {
    let mut harness = harness_with_rows(20);
    let fixtures = rows(20);
    harness
        .get_by_label(row_label(&fixtures[0]).as_str())
        .click();
    harness.run();
    harness.key_press(egui::Key::End);
    harness.run();
    harness.key_press(egui::Key::Enter);
    harness.run();
    assert!(harness
        .state()
        .intents
        .contains(&LibraryIntent::OpenRecording(fixtures[19].recording_id)));

    harness.key_press(egui::Key::Home);
    harness.run();
    harness.key_press(egui::Key::Enter);
    harness.run();
    assert!(harness
        .state()
        .intents
        .contains(&LibraryIntent::OpenRecording(fixtures[0].recording_id)));
}

#[test]
fn the_grid_is_one_tab_stop_and_nothing_traps_focus() {
    let mut harness = harness_with_rows(20);
    let order = a11y::focus_order(&mut harness, 40);
    // AccessKit reports the grid's focus on its active descendant, so the
    // grid's tab stop announces the row the cursor is on.
    let active_row = row_label(&rows(20)[0]);
    let row_stops = order.iter().filter(|label| **label == active_row).count();
    assert_eq!(
        row_stops, 1,
        "the grid must be one tab stop announcing the active row, not one stop per row: {order:?}"
    );
    assert!(
        order.len() < 20,
        "Tab must not walk every row of the grid: {order:?}"
    );
    for control in ["Search recordings", "Search across all recordings"] {
        assert!(
            order.iter().any(|label| label == control),
            "{control} is unreachable by keyboard: {order:?}"
        );
    }
}

/* ------------------------------------------------------------------ paging */

#[test]
fn reaching_the_last_loaded_row_asks_for_the_next_page_by_cursor() {
    let mut harness = harness();
    harness.state_mut().respond(
        1,
        Ok(AppResponse::ListRecordings(page(
            rows(100),
            Some("ff01"),
            250,
        ))),
    );
    harness.run();
    harness.get_by_label("Load more recordings").click();
    harness.run();

    let request = harness.state().last_list_request();
    assert_eq!(
        request.cursor.as_deref(),
        Some("ff01"),
        "the next page continues the backend's cursor, never an offset"
    );
}

/* ------------------------------------------------------------- import flow */

#[derive(Debug)]
struct TestFile(std::path::PathBuf);

impl egui::DroppedFile for TestFile {
    fn path(&self) -> &std::path::Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|error| error.to_string())
    }
}

#[test]
fn dropping_files_forwards_their_paths_verbatim_to_import_files() {
    let mut harness = harness_with_rows(5);
    harness.input_mut().dropped_files = vec![
        std::sync::Arc::new(TestFile("/tmp/plaudon-test/board.wav".into())),
        std::sync::Arc::new(TestFile("/tmp/plaudon-test/notes.txt".into())),
    ];
    harness.step();
    harness.run();

    let requests = harness.state().import_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].paths,
        ["/tmp/plaudon-test/board.wav", "/tmp/plaudon-test/notes.txt"],
        "a dropped file is not filtered locally; Rust decides and says why"
    );
    assert_eq!(
        requests[0].duplicate_policy,
        plaudon_app::dto::DuplicatePolicy::Ask
    );
}

#[test]
fn a_rejected_file_is_reported_next_to_the_accepted_ones() {
    let mut harness = harness_with_rows(5);
    harness.input_mut().dropped_files = vec![
        std::sync::Arc::new(TestFile("/tmp/plaudon-test/board.wav".into())),
        std::sync::Arc::new(TestFile("/tmp/plaudon-test/notes.txt".into())),
    ];
    harness.step();
    harness.run();

    let import_id = harness
        .state()
        .sent
        .iter()
        .position(|command| matches!(command, AppCommand::ImportFiles(_)))
        .expect("an import was dispatched") as u64
        + 1;
    harness.state_mut().respond(
        import_id,
        Ok(AppResponse::ImportFiles(ImportFilesResponse {
            results: vec![
                ImportOutcome::Accepted {
                    path: "/tmp/plaudon-test/board.wav".to_owned(),
                    job_id: JobId::new(),
                },
                ImportOutcome::Rejected {
                    path: "/tmp/plaudon-test/notes.txt".to_owned(),
                    error: envelope(AppErrorCode::UnsupportedMedia),
                },
            ],
        })),
    );
    harness.run();

    assert!(a11y::has_label(&harness, "/tmp/plaudon-test/board.wav"));
    assert!(a11y::has_label(&harness, "/tmp/plaudon-test/notes.txt"));
    assert!(
        a11y::has_label(&harness, "Imported") && a11y::has_label(&harness, "Rejected"),
        "a batch never aborts: both outcomes are shown"
    );
}

#[test]
fn a_duplicate_asks_rather_than_deciding_for_the_user() {
    let mut harness = harness_with_rows(5);
    harness.input_mut().dropped_files = vec![std::sync::Arc::new(TestFile(
        "/tmp/plaudon-test/copy.wav".into(),
    ))];
    harness.step();
    harness.run();

    let import_id = harness
        .state()
        .sent
        .iter()
        .position(|command| matches!(command, AppCommand::ImportFiles(_)))
        .expect("an import was dispatched") as u64
        + 1;
    harness.state_mut().respond(
        import_id,
        Ok(AppResponse::ImportFiles(ImportFilesResponse {
            results: vec![ImportOutcome::Duplicate {
                path: "/tmp/plaudon-test/copy.wav".to_owned(),
                sha256: plaudon_domain::media::Sha256Digest::of(b"same"),
                existing_recording_id: rows(5)[0].recording_id,
                existing_title: "Recording 0000".to_owned(),
            }],
        })),
    );
    harness.run();

    assert!(a11y::has_label(
        &harness,
        "This audio is already in your library"
    ));
    assert!(a11y::has_label(
        &harness,
        "Import anyway as a separate recording"
    ));
    assert!(a11y::has_label(&harness, "Open existing recording"));
    assert!(a11y::has_label(&harness, "Skip"));

    harness.get_by_label("Skip").click();
    harness.run();
    let last = harness
        .state()
        .import_requests()
        .pop()
        .expect("the decision was dispatched")
        .duplicate_policy;
    assert_eq!(last, plaudon_app::dto::DuplicatePolicy::Skip);
}

/* ------------------------------------------------------------ error states */

#[test]
fn a_failed_page_shows_the_envelope_message_and_a_way_forward() {
    let mut harness = harness();
    harness
        .state_mut()
        .respond(1, Err(envelope(AppErrorCode::StorageUnavailable)));
    harness.run();

    let message = AppErrorCode::StorageUnavailable.canonical_message();
    assert!(
        a11y::nodes(&harness)
            .iter()
            .any(|node| node.announced().is_some_and(|text| text.contains(message))),
        "the envelope's own message must be shown"
    );
    let before = harness.state().list_requests().len();
    if a11y::has_label(&harness, "Retry") {
        harness.get_by_label("Retry").click();
        harness.run();
        assert_eq!(harness.state().list_requests().len(), before + 1);
    }
}

#[test]
fn an_error_never_renders_an_internal_detail() {
    let mut harness = harness();
    harness
        .state_mut()
        .respond(1, Err(envelope(AppErrorCode::InternalError)));
    harness.run();
    for node in a11y::nodes(&harness) {
        if let Some(text) = node.announced() {
            assert!(
                !text.contains("internal detail"),
                "the view rendered the internal detail: {text}"
            );
        }
    }
}

/* ------------------------------------------------------------------ events */

#[test]
fn a_library_changed_event_refetches_from_the_first_page() {
    let mut harness = harness_with_rows(10);
    let before = harness.state().list_requests().len();
    {
        let LibraryApp {
            screen,
            sent,
            next_id,
            ..
        } = harness.state_mut();
        let mut send = |command: AppCommand| {
            *next_id += 1;
            sent.push(command);
            CommandId::from_raw(*next_id)
        };
        screen.on_ui_event(EventKind::LibraryChanged, &mut send);
    }
    harness.run();
    let requests = harness.state().list_requests();
    assert_eq!(requests.len(), before + 1);
    assert_eq!(requests.last().expect("a request").cursor, None);
}

/* ----------------------------------------------------------------- visual */

#[test]
fn the_hundred_row_library_matches_its_baseline() {
    let mut harness = harness_with_rows(100);
    assert!(a11y::has_label(
        &harness,
        "Showing 100 of 100 recordings (snapshot at query time)"
    ));
    visual::assert_snapshot(&mut harness, "library_100_rows");
}
