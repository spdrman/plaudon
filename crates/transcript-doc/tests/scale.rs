//! Latency budgets at the scale the parity matrix sets (T14, issue #205):
//! a three-hour transcript of 30,000 segments.
//!
//! The budgets below are what the *editor* needs, not what the machine can do:
//! an edit must land inside one 60 Hz frame, and a find-bar keystroke must not
//! stall typing. They are deliberately set with headroom over the measured
//! numbers (recorded in `docs/TRANSCRIPT_EDITOR_PERFORMANCE.md`) so the test
//! catches an algorithmic regression — a per-edit full-document rebuild, a
//! quadratic search — rather than the noise of a loaded machine.
//!
//! The test runs in the ordinary `cargo test` profile, so the budgets are
//! debug-profile budgets; the release numbers measured on the MARS runner are
//! an order of magnitude lower.

use std::time::{Duration, Instant};

use plaudon_transcript_doc::caret::Position;
use plaudon_transcript_doc::search::SearchQuery;
use plaudon_transcript_doc::session::EditSession;
use plaudon_transcript_doc::testing::synthetic_transcript;

/// Segments in a three-hour transcript.
const SEGMENTS: usize = 30_000;

/// Opening the document, including building every piece table.
const OPEN_BUDGET: Duration = Duration::from_millis(300);

/// One keystroke, measured as the worst of 200.
const KEYSTROKE_BUDGET: Duration = Duration::from_millis(16);

/// One full-document literal search, as the find bar runs per keystroke.
const SEARCH_BUDGET: Duration = Duration::from_millis(250);

/// One replace-all over every match, as a single undoable step.
const REPLACE_ALL_BUDGET: Duration = Duration::from_millis(600);

/// Undoing that replace-all.
const UNDO_BUDGET: Duration = Duration::from_millis(600);

/// Collecting the autosave payload from a lightly edited transcript.
const AUTOSAVE_PAYLOAD_BUDGET: Duration = Duration::from_millis(100);

/// Locating the segment under the playhead, as the highlight does per frame.
const SEEK_LOOKUP_BUDGET: Duration = Duration::from_micros(200);

fn report(label: &str, elapsed: Duration, budget: Duration) {
    println!(
        "{label}: {:.3} ms (budget {:.3} ms)",
        elapsed.as_secs_f64() * 1_000.0,
        budget.as_secs_f64() * 1_000.0
    );
    assert!(
        elapsed <= budget,
        "{label} took {:.3} ms, over the {:.3} ms budget",
        elapsed.as_secs_f64() * 1_000.0,
        budget.as_secs_f64() * 1_000.0
    );
}

#[test]
fn a_three_hour_transcript_stays_inside_the_editing_budgets() {
    let started = Instant::now();
    let document = synthetic_transcript(SEGMENTS);
    let open = started.elapsed();
    assert_eq!(document.len(), SEGMENTS);
    report("open 30k segments", open, OPEN_BUDGET);

    let mut session = EditSession::new(document);

    // Typing in the middle of the transcript.
    let target = session
        .document()
        .segment_at(SEGMENTS / 2)
        .expect("a middle segment")
        .id;
    session.set_caret(Position::new(target, 4));
    let mut worst = Duration::ZERO;
    for step in 0..200u64 {
        session.set_now(step * 10);
        let at = Instant::now();
        session.insert_text("x").expect("type");
        worst = worst.max(at.elapsed());
    }
    report("worst keystroke of 200", worst, KEYSTROKE_BUDGET);

    // The find bar: one full-document scan per keystroke.
    let at = Instant::now();
    session.set_search_query(SearchQuery::literal("schedule"));
    let search = at.elapsed();
    let matches = session.search().len();
    assert!(matches > 1_000, "expected a transcript-wide match set, got {matches}");
    report("literal search over 30k segments", search, SEARCH_BUDGET);

    let at = Instant::now();
    session.set_search_query(SearchQuery {
        needle: "schedule".to_owned(),
        case_sensitive: false,
        whole_word: true,
    });
    report("whole-word search over 30k segments", at.elapsed(), SEARCH_BUDGET);

    // Replace-all, which is one undo step covering every match.
    let at = Instant::now();
    let replaced = session.replace_all("timetable").expect("replace all");
    report("replace-all", at.elapsed(), REPLACE_ALL_BUDGET);
    assert_eq!(replaced, matches);

    let at = Instant::now();
    let edits = session.document().text_edits();
    report("autosave payload", at.elapsed(), AUTOSAVE_PAYLOAD_BUDGET);
    assert!(edits.len() >= replaced.min(SEGMENTS) / 2);

    let at = Instant::now();
    assert!(session.undo().expect("undo"));
    report("undo replace-all", at.elapsed(), UNDO_BUDGET);

    // The playback highlight, resolved every frame.
    let at = Instant::now();
    let found = session.document().segment_at_ms(5_400_000);
    report("segment at playhead", at.elapsed(), SEEK_LOOKUP_BUDGET);
    assert!(found.is_some());
}
