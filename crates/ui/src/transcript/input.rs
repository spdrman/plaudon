//! Keyboard, clipboard and IME input.
//!
//! Text never arrives character-by-character from a key event: it arrives as
//! [`egui::Event::Text`] or, for an input method, as
//! [`egui::ImeEvent::Commit`]. A preedit string is *not* inserted — it is held
//! and drawn, and only the commit becomes an edit, so composing Japanese or
//! Chinese in a segment produces one undoable insertion rather than a stream
//! of half-formed candidates.

use egui::{Event, EventFilter, ImeEvent, Key, Modifiers, Ui};
use plaudon_transcript_doc::caret::{Motion, Position, Selection};
use plaudon_transcript_doc::doc::SegmentId;
use plaudon_transcript_doc::session::EditSession;

use super::layout::GalleyCache;
use super::paint;

/// Everything the editor learned from this frame's input.
///
/// Independent facts about one frame, deliberately flat: an enum would force
/// a precedence the editor does not have.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Default, Clone)]
pub struct InputOutcome {
    /// Text the host should put on the clipboard.
    pub copy: Option<String>,
    /// The caret moved, so the viewport should follow it.
    pub caret_moved: bool,
    /// The document changed.
    pub edited: bool,
    /// The user asked for the find bar.
    pub open_find: bool,
    /// The user asked for the replace bar.
    pub open_replace: bool,
    /// The user pressed Escape with no find bar open.
    pub escaped: bool,
    /// The user asked for the next (`true`) or previous (`false`) match.
    pub step_match: Option<bool>,
}

/// The event filter that keeps arrows, tab and escape inside the editor while
/// a segment row holds focus.
#[must_use]
pub fn event_filter() -> EventFilter {
    EventFilter {
        horizontal_arrows: true,
        vertical_arrows: true,
        tab: false,
        escape: false,
    }
}

/// Handle every event for the focused segment.
#[allow(clippy::too_many_lines)]
pub fn handle(
    ui: &Ui,
    session: &mut EditSession,
    galleys: &GalleyCache,
    preedit: &mut Option<String>,
) -> InputOutcome {
    let mut outcome = InputOutcome::default();
    let events = ui.input(|input| input.filtered_events(&event_filter()));
    let editable = session.document().is_editable();

    for event in events {
        match event {
            Event::Text(text) if editable && preedit.is_none() => {
                if !text.chars().all(char::is_control) {
                    apply(&session.insert_text(&text), &mut outcome);
                }
            }
            Event::Paste(text) if editable => {
                apply(&session.paste(&text), &mut outcome);
            }
            Event::Copy => {
                outcome.copy = Some(session.selected_text());
            }
            Event::Cut if editable => {
                if let Ok(text) = session.cut() {
                    outcome.copy = Some(text);
                    outcome.edited = true;
                }
            }
            Event::Ime(ImeEvent::Preedit { text, .. }) => {
                *preedit = (!text.is_empty()).then_some(text);
            }
            Event::Ime(ImeEvent::Commit(text)) => {
                *preedit = None;
                if editable {
                    apply(&session.insert_text(&text), &mut outcome);
                }
            }
            Event::Ime(ImeEvent::DeleteSurrounding {
                before_chars,
                after_chars,
            }) if editable => {
                for _ in 0..before_chars {
                    apply(&session.delete_backward(), &mut outcome);
                }
                for _ in 0..after_chars {
                    apply(&session.delete_forward(), &mut outcome);
                }
            }
            Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } => handle_key(session, galleys, key, modifiers, editable, &mut outcome),
            _ => {}
        }
    }
    outcome
}

fn apply(result: &Result<(), plaudon_transcript_doc::doc::DocError>, outcome: &mut InputOutcome) {
    if result.is_ok() {
        outcome.edited = true;
        outcome.caret_moved = true;
    }
}

fn handle_key(
    session: &mut EditSession,
    galleys: &GalleyCache,
    key: Key,
    modifiers: Modifiers,
    editable: bool,
    outcome: &mut InputOutcome,
) {
    let command = modifiers.command;
    let extend = modifiers.shift;
    match key {
        Key::F if command => {
            if modifiers.alt {
                outcome.open_replace = true;
            } else {
                outcome.open_find = true;
            }
        }
        Key::G if command => outcome.step_match = Some(!modifiers.shift),
        Key::Z if command => {
            let result = if modifiers.shift {
                session.redo()
            } else {
                session.undo()
            };
            if result.unwrap_or(false) {
                outcome.edited = true;
                outcome.caret_moved = true;
            }
        }
        Key::A if command => {
            session.select_all();
            outcome.caret_moved = true;
        }
        Key::C if command && modifiers.shift => {
            outcome.copy = Some(session.copy_passage());
        }
        Key::Escape => outcome.escaped = true,
        Key::Enter => {
            // Segments are paragraphs of a transcript, not of a word
            // processor: Enter moves on, it never splits a segment.
            session.move_caret(Motion::SegmentDown, false);
            session.move_caret(Motion::SegmentStart, false);
            outcome.caret_moved = true;
        }
        Key::Backspace if editable => apply(&session.delete_backward(), outcome),
        Key::Delete if editable => apply(&session.delete_forward(), outcome),
        Key::ArrowLeft => {
            let motion = if modifiers.alt || command {
                Motion::WordLeft
            } else {
                Motion::CharLeft
            };
            session.move_caret(motion, extend);
            outcome.caret_moved = true;
        }
        Key::ArrowRight => {
            let motion = if modifiers.alt || command {
                Motion::WordRight
            } else {
                Motion::CharRight
            };
            session.move_caret(motion, extend);
            outcome.caret_moved = true;
        }
        Key::ArrowUp => {
            vertical(session, galleys, true, extend);
            outcome.caret_moved = true;
        }
        Key::ArrowDown => {
            vertical(session, galleys, false, extend);
            outcome.caret_moved = true;
        }
        Key::Home => {
            let motion = if command {
                Motion::DocumentStart
            } else {
                Motion::SegmentStart
            };
            session.move_caret(motion, extend);
            outcome.caret_moved = true;
        }
        Key::End => {
            let motion = if command {
                Motion::DocumentEnd
            } else {
                Motion::SegmentEnd
            };
            session.move_caret(motion, extend);
            outcome.caret_moved = true;
        }
        _ => {}
    }
}

/// Up and down move by *wrapped row* inside a segment and by segment at its
/// edges, which is what a reader expects from a paragraph editor.
fn vertical(session: &mut EditSession, galleys: &GalleyCache, up: bool, extend: bool) {
    let focus = session.selection().focus;
    let galley = galleys.get(focus.segment);
    let within = galley.as_ref().and_then(|galley| {
        if up {
            paint::offset_row_above(galley, focus.offset)
        } else {
            paint::offset_row_below(galley, focus.offset)
        }
    });
    if let Some(offset) = within {
        let moved = Position::new(focus.segment, offset);
        let anchor = if extend {
            session.selection().anchor
        } else {
            moved
        };
        session.set_selection(Selection::new(anchor, moved));
    } else {
        session.move_caret(
            if up {
                Motion::SegmentUp
            } else {
                Motion::SegmentDown
            },
            extend,
        );
    }
}

/// Place the caret from a pointer position inside a laid-out segment.
pub fn caret_from_pointer(
    session: &mut EditSession,
    galleys: &GalleyCache,
    segment: SegmentId,
    origin: egui::Pos2,
    pointer: egui::Pos2,
    extend: bool,
) {
    let Some(galley) = galleys.get(segment) else {
        return;
    };
    let offset = paint::offset_at(&galley, origin, pointer);
    let moved = Position::new(segment, offset);
    let anchor = if extend {
        session.selection().anchor
    } else {
        moved
    };
    session.set_selection(Selection::new(anchor, moved));
}

/// Select the word under a pointer position.
pub fn select_word_at(
    session: &mut EditSession,
    galleys: &GalleyCache,
    segment: SegmentId,
    origin: egui::Pos2,
    pointer: egui::Pos2,
) {
    let Some(galley) = galleys.get(segment) else {
        return;
    };
    let offset = paint::offset_at(&galley, origin, pointer);
    session.set_caret(Position::new(segment, offset));
    session.move_caret(Motion::WordRight, false);
    let end = session.selection().focus;
    session.move_caret(Motion::WordLeft, false);
    let start = session.selection().focus;
    session.set_selection(Selection::new(start, end));
}
