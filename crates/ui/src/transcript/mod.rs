//! The transcript editor widget.
//!
//! Thin by construction: every rule it renders — what may carry a timestamp,
//! what an undo step is, when a citation goes stale, when to save — lives in
//! `plaudon-transcript-doc`. This module turns an [`EditSession`] into pixels
//! and events into session commands, and owns nothing else.
//!
//! Scale: only the rows inside the viewport are laid out, and their galleys
//! are cached (see [`layout`]), so a 30,000-segment transcript costs the same
//! per frame as a 30-segment one.

mod chips;
mod find;
mod input;
mod layout;
mod paint;
mod style;

use egui::{Align, Rect, Sense, Ui, Vec2};
use plaudon_domain::ids::SpeakerId;
use plaudon_transcript_doc::autosave::SaveRequest;
use plaudon_transcript_doc::caret::Position;
use plaudon_transcript_doc::doc::SegmentId;
use plaudon_transcript_doc::playback::PlaybackPosition;
use plaudon_transcript_doc::search::SearchMatch;
use plaudon_transcript_doc::session::EditSession;

pub use chips::{SpeakerChoice, SPEAKER_EDIT_HINT};
pub use find::{FindBarState, FindOutcome};
pub use style::TranscriptStyle;

use layout::{GalleyCache, RowMetrics};

/// How the user chose to resolve a save conflict.
///
/// Neither option discards their text: one reloads the newer revision into a
/// fresh document, the other copies the user's version out first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictChoice {
    /// Reload the newest revision from the backend.
    ReloadNewest,
    /// Copy the user's current text to the clipboard.
    CopyMyChanges,
}

/// Frame-to-frame state of one editor. Owned by the caller.
#[derive(Debug)]
pub struct TranscriptEditorState {
    rows: RowMetrics,
    galleys: GalleyCache,
    /// The find and replace bar.
    pub find: FindBarState,
    preedit: Option<String>,
    focused: bool,
    pending_scroll_offset: Option<f32>,
    scroll_to_caret: bool,
    follow_playback: bool,
    last_highlight: Option<SegmentId>,
}

impl Default for TranscriptEditorState {
    fn default() -> Self {
        Self {
            rows: RowMetrics::default(),
            galleys: GalleyCache::default(),
            find: FindBarState::default(),
            preedit: None,
            focused: false,
            pending_scroll_offset: None,
            scroll_to_caret: false,
            follow_playback: true,
            last_highlight: None,
        }
    }
}

impl TranscriptEditorState {
    /// Whether the editor holds keyboard focus.
    #[must_use]
    pub const fn has_focus(&self) -> bool {
        self.focused
    }

    /// Whether the viewport follows the playhead while audio plays.
    #[must_use]
    pub const fn follows_playback(&self) -> bool {
        self.follow_playback
    }

    /// Follow, or stop following, the playhead.
    pub const fn set_follow_playback(&mut self, follow: bool) {
        self.follow_playback = follow;
    }

    /// Scroll the caret into view on the next frame.
    pub const fn scroll_to_caret(&mut self) {
        self.scroll_to_caret = true;
    }

    /// Number of galleys currently cached, for tests and diagnostics.
    #[must_use]
    pub fn cached_galleys(&self) -> usize {
        self.galleys.len()
    }
}

/// What the editor produced this frame.
#[derive(Debug, Default, Clone)]
pub struct TranscriptEditorOutput {
    /// The user activated a timestamp: seek the player here.
    pub seek_to_ms: Option<u64>,
    /// Autosave wants this saved through the transcript port.
    pub save_request: Option<SaveRequest>,
    /// Text the host should put on the clipboard.
    pub copied: Option<String>,
    /// The document changed.
    pub edited: bool,
    /// The user resolved a save conflict.
    pub conflict_choice: Option<ConflictChoice>,
}

/// The transcript editor.
pub struct TranscriptEditor<'a> {
    id_salt: egui::Id,
    session: &'a mut EditSession,
    state: &'a mut TranscriptEditorState,
    speakers: &'a [SpeakerChoice],
    playback: Option<&'a dyn PlaybackPosition>,
    style: Option<TranscriptStyle>,
}

impl std::fmt::Debug for TranscriptEditor<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TranscriptEditor")
            .field("id_salt", &self.id_salt)
            .field("segments", &self.session.document().len())
            .field("speakers", &self.speakers.len())
            .field("playback", &self.playback.is_some())
            .finish_non_exhaustive()
    }
}

impl<'a> TranscriptEditor<'a> {
    /// An editor over a session.
    pub fn new(
        id_salt: impl std::hash::Hash + std::fmt::Debug,
        session: &'a mut EditSession,
        state: &'a mut TranscriptEditorState,
    ) -> Self {
        Self {
            id_salt: egui::Id::new(id_salt),
            session,
            state,
            speakers: &[],
            playback: None,
            style: None,
        }
    }

    /// The speakers the user may assign.
    #[must_use]
    pub fn speakers(mut self, speakers: &'a [SpeakerChoice]) -> Self {
        self.speakers = speakers;
        self
    }

    /// The playhead to synchronise the highlight with.
    #[must_use]
    pub fn playback(mut self, playback: &'a dyn PlaybackPosition) -> Self {
        self.playback = Some(playback);
        self
    }

    /// Override the derived style.
    #[must_use]
    pub fn style(mut self, style: TranscriptStyle) -> Self {
        self.style = Some(style);
        self
    }

    /// Draw the editor.
    #[allow(clippy::too_many_lines)]
    pub fn show(mut self, ui: &mut Ui) -> TranscriptEditorOutput {
        let style = self
            .style
            .clone()
            .unwrap_or_else(|| TranscriptStyle::from_ui(ui));
        let mut output = TranscriptEditorOutput::default();

        self.session.set_now(milliseconds_since_start(ui.ctx()));

        self.status_bar(ui, &mut output);

        let find_outcome = find::show(ui, &mut self.state.find, self.session);
        if find_outcome.replaced > 0 {
            output.edited = true;
            self.state.scroll_to_caret = true;
        }
        if find_outcome.moved_to_match {
            self.state.scroll_to_caret = true;
        }

        let highlight = self.playback.and_then(|playback| {
            plaudon_transcript_doc::playback::highlight(self.session.document(), &playback)
        });
        if let Some(found) = highlight {
            let changed = self.state.last_highlight != Some(found.segment);
            self.state.last_highlight = Some(found.segment);
            if changed
                && self.state.follow_playback
                && self.playback.is_some_and(PlaybackPosition::is_playing)
            {
                self.state.scroll_to_caret = false;
                self.scroll_to_segment(found.segment);
            }
        }

        let mut scroll = egui::ScrollArea::vertical()
            .id_salt(self.id_salt)
            .auto_shrink([false, false]);
        if let Some(offset) = self.state.pending_scroll_offset.take() {
            scroll = scroll.vertical_scroll_offset(offset);
        }

        let mut row_ids: Vec<egui::Id> = Vec::new();
        let mut caret_rect: Option<Rect> = None;
        let area = scroll.show_viewport(ui, |ui, viewport| {
            let row_width = ui.available_width();
            let wrap_width = style.wrap_width(row_width);
            let line_height = ui.text_style_height(&egui::TextStyle::Body);
            let average_char = ui
                .painter()
                .layout_no_wrap(
                    "xxxxxxxxxx".to_owned(),
                    style.font.clone(),
                    style.text_color,
                )
                .size()
                .x
                / 10.0;
            let average_char = average_char.max(1.0);
            let overhead = style.row_padding.y.mul_add(2.0, style.row_spacing);
            self.state.rows.prepare(
                self.session.document(),
                wrap_width,
                line_height,
                average_char,
                overhead,
            );
            ui.set_height(self.state.rows.total_height());
            let top = ui.min_rect().top();
            let left = ui.min_rect().left();

            let range = self
                .state
                .rows
                .visible_range(viewport.min.y, viewport.max.y);
            let selection = self.session.selection();
            let (selection_start, selection_end) = selection.ordered(self.session.document());
            let current_match = self.session.search().current();

            for index in range {
                let y = top + self.state.rows.offset(index);
                let height = self.state.rows.height(index);
                let row_rect =
                    Rect::from_min_size(egui::pos2(left, y), Vec2::new(row_width, height));

                // Everything the row needs is copied out of the document here,
                // so the rest of the row can call back into the session.
                let Some(row) = RowData::read(self.session, index) else {
                    continue;
                };
                let segment_id = row.id;
                let matches: Vec<SearchMatch> = self
                    .session
                    .search()
                    .matches_in(segment_id)
                    .copied()
                    .collect();
                let galley = {
                    let Some(segment) = self.session.document().segment_at(index) else {
                        continue;
                    };
                    self.state.galleys.galley(
                        ui,
                        &style,
                        segment,
                        wrap_width,
                        &matches,
                        current_match,
                    )
                };
                self.state.rows.measure(index, galley.size().y + overhead);

                if highlight.is_some_and(|found| found.segment == segment_id) {
                    ui.painter().rect_filled(
                        row_rect,
                        ui.visuals().widgets.inactive.corner_radius,
                        style.playback_background,
                    );
                }

                let text_origin = egui::pos2(
                    left + style.gutter_width + style.speaker_width + style.row_padding.x,
                    y + style.row_padding.y,
                );

                // Selection, under the text.
                if !selection.is_empty() {
                    let (from, to) = segment_selection(
                        self.session,
                        segment_id,
                        selection_start,
                        selection_end,
                        row.char_len,
                    );
                    for rect in paint::selection_rects(&galley, text_origin, from, to) {
                        ui.painter().rect_filled(
                            rect,
                            ui.visuals().widgets.inactive.corner_radius,
                            style.selection_background,
                        );
                    }
                }

                ui.painter()
                    .galley(text_origin, galley.clone(), style.text_color);

                let row_id = self.id_salt.with(segment_id.get());
                row_ids.push(row_id);
                let response = ui.interact(
                    Rect::from_min_size(
                        text_origin,
                        Vec2::new(
                            (row_width - style.gutter_width - style.speaker_width).max(16.0),
                            height,
                        ),
                    ),
                    row_id,
                    Sense::click_and_drag(),
                );
                let label = format!("Segment {}", row.seq_index + 1);
                let text_value = row.text.clone();
                let is_edited = row.is_edited;
                response.widget_info(|| egui::WidgetInfo {
                    label: Some(if is_edited {
                        format!("{label} (edited)")
                    } else {
                        label.clone()
                    }),
                    ..egui::WidgetInfo::text_edit(
                        self.session.document().is_editable(),
                        "",
                        &text_value,
                        "",
                    )
                });

                // Press places the caret immediately — egui only reports a
                // drag after a threshold, and a text caret must not wait for
                // it — and holding the button extends the selection.
                if let Some(pointer) = ui.ctx().pointer_interact_pos() {
                    let pressed_here =
                        response.hovered() && ui.input(|input| input.pointer.any_pressed());
                    if pressed_here {
                        response.request_focus();
                        self.state.focused = true;
                        input::caret_from_pointer(
                            self.session,
                            &self.state.galleys,
                            segment_id,
                            text_origin,
                            pointer,
                            ui.input(|input| input.modifiers.shift),
                        );
                    } else if response.is_pointer_button_down_on() {
                        input::caret_from_pointer(
                            self.session,
                            &self.state.galleys,
                            segment_id,
                            text_origin,
                            pointer,
                            true,
                        );
                    }
                    if response.double_clicked() {
                        input::select_word_at(
                            self.session,
                            &self.state.galleys,
                            segment_id,
                            text_origin,
                            pointer,
                        );
                    }
                }
                if response.clicked() && !self.state.focused {
                    // Activation through AccessKit, which sends no pointer.
                    response.request_focus();
                    self.state.focused = true;
                }

                // Caret, over the text.
                if self.state.focused && selection.focus.segment == segment_id {
                    let rect = paint::caret_rect(&galley, text_origin, selection.focus.offset);
                    ui.painter().rect_filled(
                        Rect::from_min_size(rect.min, Vec2::new(1.5, rect.height())),
                        0.0,
                        style.caret_color,
                    );
                    caret_rect = Some(rect);
                }

                // Gutter and speaker chip.
                let gutter_rect = Rect::from_min_size(
                    egui::pos2(left + style.row_padding.x, y + style.row_padding.y),
                    Vec2::new(style.gutter_width - style.row_padding.x, line_height),
                );
                if let Some(seek) = chips::timestamp(
                    ui,
                    gutter_rect,
                    row.start_ms,
                    self.session.document().alignment_origin(),
                ) {
                    output.seek_to_ms = Some(seek);
                }

                let speaker_rect = Rect::from_min_size(
                    egui::pos2(left + style.gutter_width, y + style.row_padding.y),
                    Vec2::new(style.speaker_width - style.row_padding.x, line_height),
                );
                chips::speaker(ui, speaker_rect, row.speaker_id, self.speakers);

                if is_edited {
                    ui.painter().rect_filled(
                        Rect::from_min_size(egui::pos2(left, y), Vec2::new(2.0, height)),
                        0.0,
                        style.edited_color,
                    );
                }
            }

            if self.state.rows.is_dirty() {
                // Measured heights replaced estimates: the scrollbar is one
                // frame behind, so draw again before anyone looks at it.
                ui.ctx().request_repaint();
            }
        });

        self.state.galleys.end_frame();
        self.update_focus(ui, &row_ids, area.inner_rect);

        if self.state.focused {
            let outcome = input::handle(
                ui,
                self.session,
                &self.state.galleys,
                &mut self.state.preedit,
            );
            if let Some(text) = outcome.copy {
                ui.ctx().copy_text(text.clone());
                output.copied = Some(text);
            }
            output.edited |= outcome.edited;
            if outcome.caret_moved {
                self.state.scroll_to_caret = true;
            }
            if outcome.open_find {
                self.state.find.open();
            }
            if outcome.open_replace {
                self.state.find.open_replace();
            }
            if let Some(forward) = outcome.step_match {
                if forward {
                    self.session.focus_next_match();
                } else {
                    self.session.focus_previous_match();
                }
                self.state.scroll_to_caret = true;
            }
            if outcome.escaped {
                if self.state.find.open {
                    self.state.find.close(self.session);
                } else {
                    let focus = self.session.selection().focus;
                    self.session.set_caret(focus);
                }
            }
            self.lock_focus(ui, &row_ids);
            Self::report_ime(ui, caret_rect, area.inner_rect);
        }

        if self.state.scroll_to_caret {
            self.state.scroll_to_caret = false;
            let focus = self.session.selection().focus;
            if let Some(rect) = caret_rect {
                ui.scroll_to_rect(rect, None);
            } else {
                self.scroll_to_segment(focus.segment);
            }
        }

        output.save_request = self.session.poll_autosave();
        output
    }

    fn status_bar(&mut self, ui: &mut Ui, output: &mut TranscriptEditorOutput) {
        ui.horizontal(|ui| {
            let state = self.session.autosave_state();
            ui.label(state.label());
            if !self.session.document().is_editable() {
                ui.separator();
                ui.label("Read-only: this is not the newest revision");
            }
            if let Some(label) = self.session.history().undo_label() {
                ui.separator();
                if ui.button(format!("Undo {label}")).clicked() {
                    let _ = self.session.undo();
                    output.edited = true;
                    self.state.scroll_to_caret = true;
                }
            }
            if let Some(label) = self.session.history().redo_label() {
                if ui.button(format!("Redo {label}")).clicked() {
                    let _ = self.session.redo();
                    output.edited = true;
                    self.state.scroll_to_caret = true;
                }
            }
        });

        if self.session.autosave().has_conflict() {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.label(
                    "Someone saved a newer revision of this transcript. Your edits are still here.",
                );
                ui.horizontal(|ui| {
                    if ui.button("Reload newest revision").clicked() {
                        output.conflict_choice = Some(ConflictChoice::ReloadNewest);
                    }
                    if ui.button("Copy my changes").clicked() {
                        let text = self.session.copy_passage();
                        ui.ctx().copy_text(text.clone());
                        output.copied = Some(text);
                        output.conflict_choice = Some(ConflictChoice::CopyMyChanges);
                    }
                });
            });
        }
    }

    fn update_focus(&mut self, ui: &Ui, row_ids: &[egui::Id], editor_rect: Rect) {
        match ui.memory(egui::Memory::focused) {
            Some(id) if row_ids.contains(&id) => self.state.focused = true,
            Some(_) => self.state.focused = false,
            None => {}
        }
        let clicked_away = ui.input(|input| {
            input.pointer.any_pressed()
                && input
                    .pointer
                    .interact_pos()
                    .is_some_and(|pos| !editor_rect.contains(pos))
        });
        if clicked_away {
            self.state.focused = false;
        }
    }

    fn lock_focus(&self, ui: &Ui, row_ids: &[egui::Id]) {
        let focus = self.session.selection().focus;
        let caret_row = self.id_salt.with(focus.segment.get());
        if row_ids.contains(&caret_row) {
            ui.memory_mut(|memory| {
                if memory.focused() != Some(caret_row) {
                    memory.request_focus(caret_row);
                }
                memory.set_focus_lock_filter(caret_row, input::event_filter());
            });
        }
    }

    fn report_ime(ui: &Ui, caret_rect: Option<Rect>, editor_rect: Rect) {
        let Some(caret) = caret_rect else {
            return;
        };
        ui.ctx().output_mut(|out| {
            out.ime = Some(egui::output::IMEOutput {
                purpose: egui::IMEPurpose::Normal,
                rect: editor_rect,
                cursor_rect: caret,
                should_interrupt_composition: false,
            });
        });
    }

    fn scroll_to_segment(&mut self, segment: SegmentId) {
        let Some(index) = self.session.document().position(segment) else {
            return;
        };
        self.state.pending_scroll_offset = Some((self.state.rows.offset(index) - 48.0).max(0.0));
    }
}

/// Everything one row needs from the document, copied out so the rest of the
/// row can call back into the session without holding a borrow.
#[derive(Debug, Clone)]
struct RowData {
    id: SegmentId,
    seq_index: u32,
    char_len: usize,
    text: String,
    is_edited: bool,
    speaker_id: Option<SpeakerId>,
    start_ms: Option<plaudon_domain::timerange::SourceTimeMs>,
}

impl RowData {
    fn read(session: &EditSession, index: usize) -> Option<Self> {
        let segment = session.document().segment_at(index)?;
        Some(Self {
            id: segment.id,
            seq_index: segment.seq_index,
            char_len: segment.char_len(),
            text: segment.text().into_owned(),
            is_edited: segment.is_text_edited(),
            speaker_id: segment.speaker_id,
            start_ms: segment.start,
        })
    }
}

/// The part of a cross-segment selection that falls inside one segment.
fn segment_selection(
    session: &EditSession,
    segment: SegmentId,
    start: Position,
    end: Position,
    char_len: usize,
) -> (usize, usize) {
    let document = session.document();
    let (Some(index), Some(first), Some(last)) = (
        document.position(segment),
        document.position(start.segment),
        document.position(end.segment),
    ) else {
        return (0, 0);
    };
    if index < first || index > last {
        return (0, 0);
    }
    let from = if index == first { start.offset } else { 0 };
    let to = if index == last { end.offset } else { char_len };
    (from.min(char_len), to.min(char_len))
}

/// Session time in milliseconds, taken from egui's frame clock.
fn milliseconds_since_start(ctx: &egui::Context) -> u64 {
    let seconds = ctx.input(|input| input.time);
    if seconds <= 0.0 {
        0
    } else {
        (seconds * 1_000.0) as u64
    }
}

/// Align the viewport so a segment is visible. Public so a host can jump to a
/// citation or a search result from outside the widget.
pub fn reveal_segment(
    state: &mut TranscriptEditorState,
    session: &EditSession,
    segment: SegmentId,
) {
    if let Some(index) = session.document().position(segment) {
        state.pending_scroll_offset = Some((state.rows.offset(index) - 48.0).max(0.0));
    }
}

/// Place the caret at the start of a segment and reveal it.
pub fn focus_segment(
    state: &mut TranscriptEditorState,
    session: &mut EditSession,
    segment: SegmentId,
) {
    session.set_caret(Position::start_of(segment));
    state.focused = true;
    reveal_segment(state, session, segment);
}

/// Alignment used when revealing a row, exposed so callers can reason about
/// the viewport without reaching into egui.
pub const REVEAL_ALIGN: Align = Align::Center;
