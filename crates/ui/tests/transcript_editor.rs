//! Interaction tests for the transcript editor, driven through AccessKit by
//! `egui_kittest`.
//!
//! Every assertion is made the way a screen reader or a user would make it:
//! find a row by its accessible label, act on it, and then check the document.
//! No test reaches into the widget's private state to decide what happened.

use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use plaudon_domain::ids::SpeakerId;
use plaudon_transcript_doc::caret::{Position, Selection};
use plaudon_transcript_doc::playback::FixedPosition;
use plaudon_transcript_doc::session::EditSession;
use plaudon_transcript_doc::testing::{document_with, synthetic_transcript, timed_document};
use plaudon_ui::transcript::{
    SpeakerChoice, TranscriptEditor, TranscriptEditorOutput, TranscriptEditorState,
};

struct EditorApp {
    session: EditSession,
    state: TranscriptEditorState,
    speakers: Vec<SpeakerChoice>,
    playback: FixedPosition,
    output: TranscriptEditorOutput,
}

impl EditorApp {
    fn new(session: EditSession) -> Self {
        Self {
            session,
            state: TranscriptEditorState::default(),
            speakers: Vec::new(),
            playback: FixedPosition::default(),
            output: TranscriptEditorOutput::default(),
        }
    }

    /// Outputs are per-frame; `Harness::run` draws several frames, so the
    /// test keeps the last non-empty value of each field instead of whatever
    /// the final idle frame happened to produce.
    fn record(&mut self, output: TranscriptEditorOutput) {
        if output.seek_to_ms.is_some() {
            self.output.seek_to_ms = output.seek_to_ms;
        }
        if output.copied.is_some() {
            self.output.copied = output.copied;
        }
        if output.save_request.is_some() {
            self.output.save_request = output.save_request;
        }
        if output.speaker_changed.is_some() {
            self.output.speaker_changed = output.speaker_changed;
        }
        if output.conflict_choice.is_some() {
            self.output.conflict_choice = output.conflict_choice;
        }
        self.output.edited |= output.edited;
    }

    fn clear_output(&mut self) {
        self.output = TranscriptEditorOutput::default();
    }

    fn text(&self, index: usize) -> String {
        self.session
            .document()
            .segment_at(index)
            .expect("segment")
            .text()
            .into_owned()
    }
}

fn harness(app: EditorApp) -> Harness<'static, EditorApp> {
    Harness::builder()
        .with_size(egui::Vec2::new(900.0, 600.0))
        .build_ui_state(
            |ui, app: &mut EditorApp| {
                let output = TranscriptEditor::new("transcript", &mut app.session, &mut app.state)
                    .speakers(&app.speakers)
                    .playback(&app.playback)
                    .show(ui);
                app.record(output);
            },
            app,
        )
}

fn focus_segment(harness: &mut Harness<'_, EditorApp>, label: &str) {
    harness.get_by_label(label).focus();
    harness.run();
}

#[test]
fn typing_into_a_focused_segment_edits_the_document() {
    let mut session = EditSession::new(document_with(&["we will pay", "that is final"]));
    let id = session.document().first_id().expect("segment");
    session.set_caret(Position::new(id, 8));
    let mut harness = harness(EditorApp::new(session));

    focus_segment(&mut harness, "Segment 1");
    harness.get_by_label("Segment 1").type_text("not ");
    harness.run();

    assert_eq!(harness.state().text(0), "we will not pay");
    assert_eq!(harness.state().text(1), "that is final");
    // The edited row says so in its accessible name, not only in colour.
    harness.get_by_label("Segment 1 (edited)");
}

#[test]
fn a_read_only_revision_refuses_typing_and_says_so() {
    let session = EditSession::new(plaudon_transcript_doc::testing::read_only_document(&[
        "machine transcript",
    ]));
    let mut harness = harness(EditorApp::new(session));

    harness.get_by_label("Read-only: this is not the newest revision");
    focus_segment(&mut harness, "Segment 1");
    harness.get_by_label("Segment 1").type_text("x");
    harness.run();

    assert_eq!(harness.state().text(0), "machine transcript");
}

#[test]
fn keyboard_selection_then_typing_replaces_the_selection() {
    let mut session = EditSession::new(document_with(&["we will not pay"]));
    let id = session.document().first_id().expect("segment");
    session.set_caret(Position::new(id, 8));
    let mut harness = harness(EditorApp::new(session));
    focus_segment(&mut harness, "Segment 1");

    // Shift+Right three times selects "not".
    for _ in 0..3 {
        harness.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::ArrowRight);
    }
    harness.run();
    assert_eq!(harness.state().session.selected_text(), "not");

    harness.get_by_label("Segment 1").type_text("never");
    harness.run();
    assert_eq!(harness.state().text(0), "we will never pay");
}

#[test]
fn undo_and_redo_run_from_the_keyboard() {
    let mut session = EditSession::new(document_with(&["we will pay"]));
    let id = session.document().first_id().expect("segment");
    session.set_caret(Position::new(id, 11));
    let mut harness = harness(EditorApp::new(session));
    focus_segment(&mut harness, "Segment 1");

    harness.get_by_label("Segment 1").type_text(" today");
    harness.run();
    assert_eq!(harness.state().text(0), "we will pay today");

    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    harness.run();
    assert_eq!(harness.state().text(0), "we will pay");

    harness.key_press_modifiers(
        egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
        egui::Key::Z,
    );
    harness.run();
    assert_eq!(harness.state().text(0), "we will pay today");
}

#[test]
fn the_find_bar_counts_matches_and_navigates_them() {
    let session = EditSession::new(document_with(&[
        "we will pay the invoice",
        "we will pay again",
        "nothing to see",
    ]));
    let mut app = EditorApp::new(session);
    app.state.find.open();
    let mut harness = harness(app);

    harness.get_by_label("Find in transcript").type_text("pay");
    harness.run();
    harness.get_by_label("1 of 2 matches");

    harness.get_by_label("Next").click();
    harness.run();
    harness.get_by_label("2 of 2 matches");

    // The current match is selected, so the caret is where the reader is.
    assert_eq!(harness.state().session.selected_text(), "pay");
}

#[test]
fn replace_all_from_the_find_bar_is_a_single_undo_step() {
    let session = EditSession::new(document_with(&[
        "we will pay the invoice",
        "we will pay again",
    ]));
    let mut app = EditorApp::new(session);
    app.state.find.open_replace();
    let mut harness = harness(app);

    harness.get_by_label("Find in transcript").focus();
    harness.run();
    harness.get_by_label("Find in transcript").type_text("pay");
    harness.run();
    harness.get_by_label("Replace with").focus();
    harness.run();
    harness.get_by_label("Replace with").type_text("settle");
    harness.run();
    harness.get_by_label("Replace all").click();
    harness.run();

    assert_eq!(harness.state().text(0), "we will settle the invoice");
    assert_eq!(harness.state().text(1), "we will settle again");

    harness.state_mut().session.undo().expect("undo");
    harness.run();
    assert_eq!(harness.state().text(0), "we will pay the invoice");
    assert_eq!(harness.state().text(1), "we will pay again");
}

#[test]
fn a_speaker_is_reassigned_from_the_chip_without_touching_the_text() {
    let dana = SpeakerId::new();
    let sam = SpeakerId::new();
    let mut session = EditSession::new(document_with(&["we will pay", "that is final"]));
    let first = session.document().first_id().expect("segment");
    session
        .set_speaker(first, Some(dana))
        .expect("initial speaker");

    let mut app = EditorApp::new(session);
    app.speakers = vec![
        SpeakerChoice::new(dana, "Speaker 1", "inferred cluster"),
        SpeakerChoice::new(sam, "Sam", "confirmed"),
    ];
    let mut harness = harness(app);

    harness
        .get_by_label("Speaker: Speaker 1 (inferred cluster)")
        .click();
    harness.run();
    harness.get_by_label("Sam (confirmed)").click();
    harness.run();

    assert_eq!(
        harness
            .state()
            .session
            .document()
            .segment(first)
            .expect("segment")
            .speaker_id,
        Some(sam)
    );
    assert_eq!(harness.state().text(0), "we will pay");
    assert_eq!(
        harness.state().output.speaker_changed,
        Some((first, Some(sam)))
    );
}

#[test]
fn copy_and_paste_move_text_through_the_host_clipboard() {
    let mut session = EditSession::new(timed_document(&[
        (0, 1_000, "we keep it local"),
        (42_000, 44_000, "always"),
    ]));
    let ids: Vec<_> = session
        .document()
        .segments()
        .iter()
        .map(|segment| segment.id)
        .collect();
    session.set_selection(Selection::new(
        Position::new(ids[0], 0),
        Position::new(ids[0], 16),
    ));
    let mut harness = harness(EditorApp::new(session));
    focus_segment(&mut harness, "Segment 1");

    harness.state_mut().clear_output();
    harness.event(egui::Event::Copy);
    harness.run();
    assert_eq!(
        harness.state().output.copied.as_deref(),
        Some("we keep it local")
    );

    // Copy as a passage carries the timestamp the segment really has.
    harness.state_mut().clear_output();
    harness.key_press_modifiers(
        egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
        egui::Key::C,
    );
    harness.run();
    assert_eq!(
        harness.state().output.copied.as_deref(),
        Some("[0:00] we keep it local")
    );

    harness.event(egui::Event::Paste("we keep it here".to_owned()));
    harness.run();
    assert_eq!(harness.state().text(0), "we keep it here");
}

#[test]
fn cutting_removes_the_selection_and_hands_it_to_the_host() {
    let mut session = EditSession::new(document_with(&["we will not pay"]));
    let id = session.document().first_id().expect("segment");
    session.set_selection(Selection::new(Position::new(id, 8), Position::new(id, 12)));
    let mut harness = harness(EditorApp::new(session));
    focus_segment(&mut harness, "Segment 1");

    harness.event(egui::Event::Cut);
    harness.run();

    assert_eq!(harness.state().output.copied.as_deref(), Some("not "));
    assert_eq!(harness.state().text(0), "we will pay");
}

#[test]
fn an_unaligned_transcript_offers_no_timestamp_and_no_seek() {
    let session = EditSession::new(document_with(&["no timeline here"]));
    let mut harness = harness(EditorApp::new(session));
    harness.run();

    assert!(
        harness.query_by_label("0:00").is_none(),
        "a text-only revision must not render a clock"
    );
    assert_eq!(harness.state().output.seek_to_ms, None);
}

#[test]
fn a_timestamp_chip_requests_a_seek() {
    let session = EditSession::new(timed_document(&[
        (0, 1_000, "first"),
        (42_000, 44_000, "second"),
    ]));
    let mut harness = harness(EditorApp::new(session));
    harness.run();

    harness.get_by_label("0:42").click();
    harness.run();
    assert_eq!(harness.state().output.seek_to_ms, Some(42_000));
}

#[test]
fn a_derived_alignment_is_labelled_approximate() {
    use plaudon_domain::ids::{RecordingId, TranscriptRevisionId};
    use plaudon_domain::transcript::AlignmentOrigin;
    use plaudon_transcript_doc::doc::{DocumentSource, SegmentSeed, TranscriptDocument};

    let document = TranscriptDocument::new(DocumentSource {
        recording_id: RecordingId::new(),
        revision_id: TranscriptRevisionId::new(),
        alignment_origin: AlignmentOrigin::Derived,
        duration_ms: Some(60_000),
        editable: true,
        segments: vec![SegmentSeed::timed(0, 5_000, 6_000, "aligned later")],
    });
    let mut harness = harness(EditorApp::new(EditSession::new(document)));
    harness.run();

    harness.get_by_label("0:05 (approximate)");
}

#[test]
fn the_playback_highlight_follows_the_playhead() {
    let session = EditSession::new(synthetic_transcript(2_000));
    let mut app = EditorApp::new(session);
    app.playback = FixedPosition::playing_at(0);
    let mut harness = harness(app);
    harness.run();

    harness.get_by_label("Segment 1");
    // 360 ms per segment: 180 s is segment 501.
    harness.state_mut().playback = FixedPosition::playing_at(180_000);
    harness.run();
    harness.run();

    harness.get_by_label("Segment 501");
    assert!(
        harness.query_by_label("Segment 1").is_none(),
        "the viewport must follow the playhead instead of staying at the top"
    );
}

#[test]
fn only_the_visible_rows_of_a_three_hour_transcript_are_laid_out() {
    let session = EditSession::new(synthetic_transcript(30_000));
    let mut harness = harness(EditorApp::new(session));
    harness.run();

    let laid_out = harness.state().state.cached_galleys();
    assert!(
        laid_out > 0 && laid_out < 200,
        "expected a viewport-sized working set, laid out {laid_out} of 30000"
    );
    let rows = harness
        .get_all_by_role(egui::accesskit::Role::TextInput)
        .count();
    assert!(
        rows < 200,
        "expected only visible rows in the accessibility tree, found {rows}"
    );
}

#[test]
fn autosave_asks_the_host_to_save_the_changed_segments() {
    let mut session = EditSession::new(document_with(&["one", "two"]));
    let id = session.document().segment_at(1).expect("segment").id;
    session.set_caret(Position::new(id, 3));
    let mut harness = harness(EditorApp::new(session));
    focus_segment(&mut harness, "Segment 2");

    harness.get_by_label("Segment 2").type_text("!");
    harness.run();

    // The debounce is in session time, which the widget takes from egui's
    // frame clock; step until it elapses.
    let mut request = None;
    for _ in 0..240 {
        harness.step();
        if let Some(found) = harness.state().output.save_request.clone() {
            request = Some(found);
            break;
        }
    }
    let request = request.expect("autosave must eventually ask for a save");
    assert_eq!(request.edits, vec![(1, "two!".to_owned())]);
}

#[test]
fn an_ime_composition_commits_once_and_undoes_once() {
    let mut session = EditSession::new(document_with(&["prefix "]));
    let id = session.document().first_id().expect("segment");
    session.set_caret(Position::new(id, 7));
    let mut harness = harness(EditorApp::new(session));
    focus_segment(&mut harness, "Segment 1");

    // A preedit is a candidate, not an edit: nothing is inserted yet, and a
    // stray Text event during composition is ignored.
    harness.event(egui::Event::Ime(egui::ImeEvent::Preedit {
        text: "にほ".to_owned(),
        active_range_chars: None,
    }));
    harness.run();
    harness.get_by_label("Segment 1").type_text("x");
    harness.run();
    assert_eq!(harness.state().text(0), "prefix ");

    harness.event(egui::Event::Ime(egui::ImeEvent::Commit("日本".to_owned())));
    harness.run();
    assert_eq!(harness.state().text(0), "prefix 日本");

    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    harness.run();
    assert_eq!(
        harness.state().text(0),
        "prefix ",
        "a commit is one undo step, not one per candidate character"
    );
}

#[test]
fn dragging_across_a_segment_selects_the_text_between_the_two_points() {
    let session = EditSession::new(document_with(&["we will not pay the invoice"]));
    let mut harness = harness(EditorApp::new(session));
    harness.run();

    let row = harness.get_by_label("Segment 1").rect();
    let start = egui::pos2(row.left() + 2.0, row.center().y);
    let end = egui::pos2(row.left() + 60.0, row.center().y);
    harness.hover_at(start);
    harness.run();
    harness.drag_at(start);
    harness.run();
    harness.hover_at(end);
    harness.run();
    harness.drop_at(end);
    harness.run();

    let selected = harness.state().session.selected_text();
    assert!(!selected.is_empty(), "a drag must select something");
    assert!(
        "we will not pay the invoice".starts_with(&selected),
        "the selection must be the run the pointer crossed, got {selected:?}"
    );
}

#[test]
fn clicking_inside_a_segment_puts_the_caret_under_the_pointer() {
    // The floor every pointer test above stands on. Placing a caret from a
    // screen position is pure text geometry: `Galley::cursor_from_pos` can
    // only answer it when the glyphs were laid out with a real font. A test
    // build without egui's `default_fonts` lays out every segment with zero
    // glyphs, every position then resolves to offset 0, and a drag selects
    // nothing while still looking like a drag to the widget. This test fails
    // loudly in that case instead of letting the rest pass vacuously.
    let session = EditSession::new(document_with(&["we will not pay the invoice"]));
    let mut harness = harness(EditorApp::new(session));
    harness.run();

    let row = harness.get_by_label("Segment 1").rect();
    let inside = egui::pos2(row.left() + 60.0, row.center().y);
    harness.hover_at(inside);
    harness.run();
    harness.drag_at(inside);
    harness.run();
    harness.drop_at(inside);
    harness.run();

    harness.get_by_label("Segment 1").type_text("!");
    harness.run();

    let text = harness.state().text(0);
    let typed = text
        .find('!')
        .expect("the typed character is in the segment");
    assert!(
        typed > 0 && typed < "we will not pay the invoice".len(),
        "a click 60 points into the row must leave the caret inside the text, \
         not at either end; got {text:?}"
    );
}

#[test]
fn a_three_hour_transcript_stays_inside_a_frame_budget() {
    use std::time::{Duration, Instant};

    // One frame of a 30,000-segment transcript must cost what a 30-segment
    // one costs: only the visible rows are laid out. The budget is a
    // debug-profile budget with headroom; the release numbers measured on the
    // MARS runner are in docs/TRANSCRIPT_EDITOR_PERFORMANCE.md.
    const FRAME_BUDGET: Duration = Duration::from_millis(60);

    let session = EditSession::new(synthetic_transcript(30_000));
    let mut harness = harness(EditorApp::new(session));
    harness.run();

    let started = Instant::now();
    for _ in 0..10 {
        harness.step();
    }
    let per_frame = started.elapsed() / 10;
    println!(
        "transcript editor frame at 30k segments: {:.3} ms",
        per_frame.as_secs_f64() * 1_000.0
    );
    assert!(
        per_frame <= FRAME_BUDGET,
        "a frame took {:.3} ms, over the {:.3} ms budget",
        per_frame.as_secs_f64() * 1_000.0,
        FRAME_BUDGET.as_secs_f64() * 1_000.0
    );
}
