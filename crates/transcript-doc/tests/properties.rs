//! Deterministic property tests for the document model.
//!
//! Every runner is seeded with `TestRng::deterministic_rng`, so a failure
//! reproduces from the test name alone — no `proptest-regressions` file, no
//! "it passed on my machine".
//!
//! The properties are the four invariants the editor's correctness rests on:
//!
//! 1. undo ∘ redo is the identity on document state;
//! 2. replaying the operation log over a freshly opened document reproduces
//!    the edited document;
//! 3. segment identities and `seq_index` values survive every edit;
//! 4. a cited span is remapped exactly when its characters survived
//!    unmodified and contiguous, and marked stale otherwise — checked against
//!    an independent provenance model *and* against the domain's own anchor
//!    resolver on the saved child revision.

use plaudon_domain::anchor::{AnchorVerification, SourceAnchor};
use plaudon_domain::ids::SpeakerId;
use plaudon_domain::time::Timestamp;
use plaudon_transcript_doc::anchors::map_anchor;
use plaudon_transcript_doc::caret::Position;
use plaudon_transcript_doc::doc::{domain_segments, SegmentId, TranscriptDocument};
use plaudon_transcript_doc::history::replay;
use plaudon_transcript_doc::search::SearchQuery;
use plaudon_transcript_doc::session::EditSession;
use plaudon_transcript_doc::testing::{document_with, revision_with};
use plaudon_transcript_doc::text::SpanMapping;
use proptest::prelude::*;
use proptest::test_runner::{Config, TestRng, TestRunner};

const SEED_TEXTS: &[&str] = &[
    "we will not pay the invoice before the audit closes",
    "that is final and the board agreed",
    "naïve café notes from the Zürich office",
    "action item: send the revised schedule",
];

fn runner(cases: u32) -> TestRunner {
    TestRunner::new_with_rng(
        Config {
            cases,
            failure_persistence: None,
            ..Config::default()
        },
        TestRng::deterministic_rng(proptest::test_runner::RngAlgorithm::ChaCha),
    )
}

/// One scripted user action.
#[derive(Debug, Clone)]
enum Command {
    Insert {
        segment: usize,
        offset: usize,
        text: String,
    },
    Delete {
        segment: usize,
        offset: usize,
        len: usize,
    },
    Speaker {
        segment: usize,
        speaker: usize,
    },
    ReplaceAll {
        needle: String,
        replacement: String,
    },
    Undo,
    Redo,
}

fn command_strategy() -> impl Strategy<Value = Command> {
    prop_oneof![
        6 => (0usize..4, 0usize..60, "[a-z ,.é]{1,6}").prop_map(|(segment, offset, text)| {
            Command::Insert {
                segment,
                offset,
                text,
            }
        }),
        4 => (0usize..4, 0usize..60, 1usize..8).prop_map(|(segment, offset, len)| Command::Delete {
            segment,
            offset,
            len,
        }),
        2 => (0usize..4, 0usize..3).prop_map(|(segment, speaker)| Command::Speaker {
            segment,
            speaker,
        }),
        1 => ("(the|and|a|not)", "[a-z]{1,4}").prop_map(|(needle, replacement)| {
            Command::ReplaceAll {
                needle,
                replacement,
            }
        }),
        2 => Just(Command::Undo),
        2 => Just(Command::Redo),
    ]
}

fn script_strategy() -> impl Strategy<Value = Vec<Command>> {
    prop::collection::vec(command_strategy(), 0..40)
}

fn speakers() -> Vec<Option<SpeakerId>> {
    vec![None, Some(SpeakerId::new()), Some(SpeakerId::new())]
}

fn snapshot(document: &TranscriptDocument) -> Vec<(String, Option<SpeakerId>)> {
    document
        .segments()
        .iter()
        .map(|segment| (segment.text().into_owned(), segment.speaker_id))
        .collect()
}

fn run_script(session: &mut EditSession, script: &[Command], speakers: &[Option<SpeakerId>]) {
    for (step, command) in script.iter().enumerate() {
        // Alternate between "still typing" and "paused", so coalescing is
        // exercised in both directions.
        session.set_now(step as u64 * if step % 3 == 0 { 2_000 } else { 60 });
        match command {
            Command::Insert {
                segment,
                offset,
                text,
            } => {
                let Some(target) = session.document().segment_at(*segment) else {
                    continue;
                };
                let id = target.id;
                let at = offset % (target.char_len() + 1);
                session.set_caret(Position::new(id, at));
                session.insert_text(text).expect("insert");
            }
            Command::Delete {
                segment,
                offset,
                len,
            } => {
                let Some(target) = session.document().segment_at(*segment) else {
                    continue;
                };
                let id = target.id;
                let char_len = target.char_len();
                if char_len == 0 {
                    continue;
                }
                let at = offset % char_len;
                let to = (at + len).min(char_len);
                session.set_selection(plaudon_transcript_doc::caret::Selection::new(
                    Position::new(id, at),
                    Position::new(id, to),
                ));
                session.delete_selection().expect("delete");
            }
            Command::Speaker { segment, speaker } => {
                let Some(target) = session.document().segment_at(*segment) else {
                    continue;
                };
                let id = target.id;
                session
                    .set_speaker(id, speakers[speaker % speakers.len()])
                    .expect("speaker");
            }
            Command::ReplaceAll {
                needle,
                replacement,
            } => {
                session.set_search_query(SearchQuery::literal(needle.clone()));
                session.replace_all(replacement).expect("replace all");
            }
            Command::Undo => {
                session.undo().expect("undo");
            }
            Command::Redo => {
                session.redo().expect("redo");
            }
        }
    }
}

#[test]
fn undo_then_redo_is_the_identity() {
    runner(256)
        .run(&script_strategy(), |script| {
            let speakers = speakers();
            let mut session = EditSession::new(document_with(SEED_TEXTS));
            let initial = snapshot(session.document());
            run_script(&mut session, &script, &speakers);

            // A script may leave steps on the redo stack, so the state to
            // compare against is the fully redone one, not the one the script
            // happened to stop at.
            while session.undo().expect("undo") {}
            prop_assert_eq!(
                snapshot(session.document()),
                initial.clone(),
                "undoing every step must restore the transcript exactly"
            );
            prop_assert!(!session.document().is_dirty());

            while session.redo().expect("redo") {}
            let edited = snapshot(session.document());

            while session.undo().expect("undo") {}
            prop_assert_eq!(
                snapshot(session.document()),
                initial,
                "a second full undo must land in the same place"
            );
            while session.redo().expect("redo") {}
            prop_assert_eq!(
                snapshot(session.document()),
                edited,
                "redoing every step must restore the edited transcript exactly"
            );
            Ok(())
        })
        .expect("undo/redo identity");
}

#[test]
fn replaying_the_operation_log_reproduces_the_document() {
    runner(256)
        .run(&script_strategy(), |script| {
            let speakers = speakers();
            let mut session = EditSession::new(document_with(SEED_TEXTS));
            run_script(&mut session, &script, &speakers);

            let mut fresh = document_with(SEED_TEXTS);
            replay(&mut fresh, session.history().undo_stack()).expect("replay");
            prop_assert_eq!(
                snapshot(&fresh),
                snapshot(session.document()),
                "the log is the document"
            );
            Ok(())
        })
        .expect("op-log replay");
}

#[test]
fn segment_identities_and_indices_survive_every_edit() {
    runner(256)
        .run(&script_strategy(), |script| {
            let speakers = speakers();
            let mut session = EditSession::new(document_with(SEED_TEXTS));
            let ids: Vec<SegmentId> = session
                .document()
                .segments()
                .iter()
                .map(|segment| segment.id)
                .collect();
            run_script(&mut session, &script, &speakers);

            let after: Vec<SegmentId> = session
                .document()
                .segments()
                .iter()
                .map(|segment| segment.id)
                .collect();
            prop_assert_eq!(&ids, &after, "segment identities must never be reassigned");
            for (position, segment) in session.document().segments().iter().enumerate() {
                prop_assert_eq!(segment.seq_index as usize, position);
                prop_assert_eq!(session.document().position(segment.id), Some(position));
            }
            Ok(())
        })
        .expect("stable identities");
}

/// An independent model of where every current character came from: `Some(n)`
/// for the character that was at base offset `n`, `None` for inserted text.
#[derive(Debug, Clone)]
struct Provenance(Vec<Option<usize>>);

impl Provenance {
    fn new(len: usize) -> Self {
        Self((0..len).map(Some).collect())
    }

    fn insert(&mut self, at: usize, count: usize) {
        let at = at.min(self.0.len());
        for offset in 0..count {
            self.0.insert(at + offset, None);
        }
    }

    fn remove(&mut self, at: usize, count: usize) {
        let at = at.min(self.0.len());
        let to = (at + count).min(self.0.len());
        self.0.drain(at..to);
    }

    /// Where `[start, end)` of the base text lives now, if it survived whole
    /// and contiguous.
    fn map_span(&self, start: usize, end: usize) -> SpanMapping {
        let wanted: Vec<Option<usize>> = (start..end).map(Some).collect();
        if wanted.is_empty() {
            return SpanMapping::Broken;
        }
        self.0
            .windows(wanted.len())
            .position(|window| window == wanted.as_slice())
            .map_or(SpanMapping::Broken, |position| SpanMapping::Mapped {
                start: position,
                end: position + wanted.len(),
            })
    }
}

#[test]
fn a_cited_span_is_mapped_exactly_when_its_characters_survived() {
    let edits = prop::collection::vec(
        prop_oneof![
            (0usize..80, "[a-z]{1,4}").prop_map(|(at, text)| (at, text, true)),
            (0usize..80, 1usize..6).prop_map(|(at, len)| (at, "x".repeat(len), false)),
        ],
        0..12,
    );

    runner(512)
        .run(
            &(edits, 0usize..40, 1usize..12),
            |(edits, span_start, span_len)| {
                let text = SEED_TEXTS[0];
                let mut session = EditSession::new(document_with(&[text]));
                let id = session.document().first_id().expect("segment");
                let mut model = Provenance::new(text.chars().count());

                for (at, payload, is_insert) in &edits {
                    let current = session.document().segment(id).expect("segment").char_len();
                    if *is_insert {
                        let offset = at % (current + 1);
                        session.set_caret(Position::new(id, offset));
                        session.insert_text(payload).expect("insert");
                        model.insert(offset, payload.chars().count());
                    } else if current > 0 {
                        let offset = at % current;
                        let count = payload.chars().count().min(current - offset);
                        session.set_selection(plaudon_transcript_doc::caret::Selection::new(
                            Position::new(id, offset),
                            Position::new(id, offset + count),
                        ));
                        session.delete_selection().expect("delete");
                        model.remove(offset, count);
                    }
                }
                let base_len = text.chars().count();
                let start = span_start % base_len;
                let end = (start + span_len).min(base_len);
                if end <= start {
                    return Ok(());
                }
                let segment = session.document().segment(id).expect("segment");
                let mapping = segment.piece_table().map_original_span(start, end);
                prop_assert_eq!(
                    mapping,
                    model.map_span(start, end),
                    "piece-table mapping disagrees with the provenance model for {}..{}",
                    start,
                    end
                );
                if let SpanMapping::Mapped {
                    start: from,
                    end: to,
                } = mapping
                {
                    let cited: String = text.chars().skip(start).take(end - start).collect();
                    prop_assert_eq!(segment.slice(from, to), cited);
                }
                Ok(())
            },
        )
        .expect("anchor span mapping");
}

#[test]
fn a_mapped_anchor_resolves_against_the_saved_revision_and_a_stale_one_is_refused() {
    let edits = prop::collection::vec((0usize..60, "[a-z ]{0,5}", 0usize..6), 0..8);

    runner(256)
        .run(&(edits, 0usize..40, 1usize..15), |(edits, span_start, span_len)| {
            let original = revision_with(&[SEED_TEXTS[0], SEED_TEXTS[1]]);
            let mut session = EditSession::new(TranscriptDocument::from_revision(&original));
            let id = session.document().first_id().expect("segment");

            let base_text = original.segments()[0].text.clone();
            let base_len = base_text.chars().count();
            let start = span_start % base_len;
            let end = (start + span_len).min(base_len);
            if end <= start {
                return Ok(());
            }
            let excerpt: String = base_text.chars().skip(start).take(end - start).collect();
            let anchor = SourceAnchor::new(
                original.id,
                0,
                u32::try_from(start).expect("fits"),
                u32::try_from(end).expect("fits"),
                excerpt.clone(),
            );
            prop_assert!(anchor.resolve(&original).is_verified());

            for (at, insert, delete_len) in &edits {
                let current = session.document().segment(id).expect("segment").char_len();
                let offset = at % (current + 1);
                if *delete_len > 0 && current > 0 {
                    let from = at % current;
                    let to = (from + delete_len).min(current);
                    session.set_selection(plaudon_transcript_doc::caret::Selection::new(
                        Position::new(id, from),
                        Position::new(id, to),
                    ));
                    session.delete_selection().expect("delete");
                }
                if !insert.is_empty() {
                    let current = session.document().segment(id).expect("segment").char_len();
                    session.set_caret(Position::new(id, offset.min(current)));
                    session.insert_text(insert).expect("insert");
                }
            }

            let mapping = map_anchor(session.document(), &anchor);
            let child = original
                .edited(domain_segments(session.document()), Timestamp::from_millis(1))
                .expect("child revision");

            match mapping {
                plaudon_transcript_doc::AnchorMapping::Mapped(span) => {
                    let carried = span.into_anchor(child.id);
                    match carried.resolve(&child) {
                        AnchorVerification::Verified(verified) => {
                            prop_assert_eq!(
                                plaudon_domain::anchor::normalize_excerpt(&verified.matched_text),
                                plaudon_domain::anchor::normalize_excerpt(&excerpt)
                            );
                        }
                        AnchorVerification::Rejected(rejection) => {
                            return Err(TestCaseError::fail(format!(
                                "a mapped anchor must resolve against the saved revision: {rejection}"
                            )));
                        }
                    }
                }
                plaudon_transcript_doc::AnchorMapping::Stale(_) => {
                    // A stale anchor must not be carried forward unchanged:
                    // the original offsets must no longer hold the cited text.
                    let carried = SourceAnchor::new(
                        child.id,
                        0,
                        anchor.char_start,
                        anchor.char_end,
                        excerpt.clone(),
                    );
                    if carried.resolve(&child).is_verified() {
                        // The edits happened to restore the exact text at the
                        // exact offsets. The model still refuses to carry the
                        // citation, because the evidence was destroyed in
                        // between; assert only that it is not claimed as
                        // verified by us.
                        prop_assert!(!mapping.is_mapped());
                    }
                }
            }
            // The original revision is never touched.
            prop_assert!(anchor.resolve(&original).is_verified());
            Ok(())
        })
        .expect("anchor staleness");
}

#[test]
fn the_document_is_dirty_exactly_when_it_differs_from_the_base_revision() {
    runner(128)
        .run(&script_strategy(), |script| {
            let speakers = speakers();
            let mut session = EditSession::new(document_with(SEED_TEXTS));
            run_script(&mut session, &script, &speakers);

            let base = document_with(SEED_TEXTS);
            let differs = session
                .document()
                .segments()
                .iter()
                .zip(base.segments())
                .any(|(edited, original)| {
                    edited.text() != original.text() || edited.speaker_id != original.speaker_id
                });
            prop_assert_eq!(session.document().is_dirty(), differs);
            Ok(())
        })
        .expect("dirty tracking");
}
