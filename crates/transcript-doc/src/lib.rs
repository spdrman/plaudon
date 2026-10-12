//! The transcript document model: everything the transcript editor does,
//! with no user interface attached.
//!
//! The crate exists because the editor is the one screen in Plaudon where the
//! hard parts are not rendering. Stable identities, an invertible operation
//! log, citations that survive an edit, and a save path that never overwrites
//! a machine transcript are all properties that must hold whether the pixels
//! are drawn by egui, by a test harness, or by nothing at all. None of it
//! depends on `egui`, `eframe`, `sqlx` or a platform API.
//!
//! # What it guarantees
//!
//! * **Segment identities are stable.** [`doc::SegmentId`] is allocated once
//!   per segment when the document is opened and no edit reassigns it.
//!   Segments are never split, merged, created or deleted, so `seq_index` —
//!   the coordinate anchors, speaker turns and the save command all use —
//!   stays valid.
//! * **The original is preserved.** A [`plaudon_domain::transcript::TranscriptRevision`]
//!   is immutable. The document holds the base text in a piece table and emits
//!   edits; the host persists them as a *child* revision through the existing
//!   `save_transcript_revision` port. This crate never touches SQLite.
//! * **Citations survive or are marked stale, never silently wrong.**
//!   [`anchors::map_anchor`] carries an anchor forward only when every cited
//!   character is still present, unmodified and contiguous; anything else is
//!   [`anchors::AnchorStaleness`], which the host records exactly as the
//!   existing staleness rules require.
//! * **Undo is exact.** Every operation carries its inverse
//!   ([`ops::Op::inverse`]), so undo restores text and selection without
//!   guessing, and replaying the log over a freshly opened document reproduces
//!   the edited one.
//! * **No invented timelines.** A revision whose
//!   [`plaudon_domain::transcript::AlignmentOrigin`] is `None` exposes no
//!   timestamps, no seek positions and no playback highlight (spec 8.6A).
//!
//! # Shape
//!
//! ```text
//! EditSession ── document ──→ TranscriptDocument ──→ Segment ──→ SegmentText (piece table)
//!      │                              ▲
//!      ├── selection ── Selection ────┘
//!      ├── history ──── History ───→ OpGroup ───→ Op (invertible)
//!      ├── search ───── SearchResults
//!      └── autosave ─── Autosave ──→ SaveRequest ──→ (host) save_transcript_revision
//! ```
//!
//! # Example
//!
//! ```
//! use plaudon_transcript_doc::caret::Position;
//! use plaudon_transcript_doc::session::EditSession;
//! use plaudon_transcript_doc::testing::document_with;
//!
//! let mut session = EditSession::new(document_with(&["we will pay"]));
//! let segment = session.document().first_id().expect("a segment");
//! session.set_caret(Position::new(segment, 8));
//! session.insert_text("not ").expect("the document is editable");
//! assert_eq!(
//!     session.document().segment(segment).expect("segment").text(),
//!     "we will not pay"
//! );
//! session.undo().expect("undo");
//! assert_eq!(
//!     session.document().segment(segment).expect("segment").text(),
//!     "we will pay"
//! );
//! ```

#![deny(missing_docs)]

pub mod anchors;
pub mod autosave;
pub mod caret;
pub mod clipboard;
pub mod doc;
pub mod history;
pub mod ops;
pub mod playback;
pub mod search;
pub mod session;
pub mod testing;
pub mod text;

pub use anchors::{
    map_anchor, map_anchors, map_claim, AnchorMapping, AnchorStaleness, ClaimMapping, MappedSpan,
};
pub use autosave::{Autosave, AutosaveState, SaveRequest};
pub use caret::{Motion, Position, Selection};
pub use doc::{DocError, DocumentSource, Segment, SegmentId, SegmentSeed, TranscriptDocument};
pub use history::History;
pub use ops::{EditKind, Op, OpGroup};
pub use playback::{PlaybackHighlight, PlaybackPosition};
pub use search::{SearchMatch, SearchQuery, SearchResults};
pub use session::EditSession;
pub use text::{SegmentText, SpanMapping};
