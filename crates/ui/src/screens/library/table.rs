//! The recordings grid (§5.2, §5.3, §5.6).
//!
//! Virtualized from the first line: [`egui_extras::TableBody::rows`] lays out
//! only the rows inside the viewport, so a ten-thousand-row library costs the
//! same per frame as a ten-row one. Nothing here walks the whole row slice
//! during a paint except to count it.
//!
//! The accessibility model is the React grid's, expressed in AccessKit rather
//! than ARIA: the grid reports the **whole** matching set as its row count,
//! not the rendered window; the active column — and only the active column —
//! reports a sort direction; rows carry their index, their selected state and
//! a label that names the recording; and the grid is a single tab stop whose
//! active row is reported as the active descendant, which is the roving tab
//! index expressed the way AccessKit expresses it.

use egui::{accesskit, Key, Modifiers, RichText, Sense, Ui, UiBuilder};
use egui_extras::{Column, TableBuilder};
use plaudon_app::dto::{RecordingSortField, RecordingSummary, SortDirection};
use plaudon_domain::ids::RecordingId;

use super::format::{format_duration, format_timestamp, spoken_duration};
use super::selection::{range_ids, Selection};
use super::status::{import_status, status_badge, transcription_status};

/// Height of one row, in points. Owned here because the virtualizer needs it
/// before anything is laid out.
pub const ROW_HEIGHT: f32 = 28.0;

/// Height of the header row, in points.
const HEADER_HEIGHT: f32 = 24.0;

struct ColumnSpec {
    key: &'static str,
    label: &'static str,
    sort: Option<RecordingSortField>,
    width: Column,
}

fn columns() -> [ColumnSpec; 10] {
    [
        ColumnSpec {
            key: "title",
            label: "Title",
            sort: Some(RecordingSortField::Title),
            width: Column::initial(220.0).at_least(120.0).clip(true),
        },
        ColumnSpec {
            key: "recorded",
            label: "Recorded",
            sort: Some(RecordingSortField::RecordedAt),
            width: Column::initial(150.0).at_least(100.0).clip(true),
        },
        ColumnSpec {
            key: "imported",
            label: "Imported",
            sort: Some(RecordingSortField::ImportedAt),
            width: Column::initial(150.0).at_least(100.0).clip(true),
        },
        ColumnSpec {
            key: "duration",
            label: "Duration",
            sort: Some(RecordingSortField::DurationMs),
            width: Column::initial(80.0).at_least(60.0).clip(true),
        },
        ColumnSpec {
            key: "source",
            label: "Source",
            sort: None,
            width: Column::initial(120.0).at_least(80.0).clip(true),
        },
        ColumnSpec {
            key: "import",
            label: "Import status",
            sort: None,
            width: Column::initial(130.0).at_least(90.0).clip(true),
        },
        ColumnSpec {
            key: "transcription",
            label: "Transcription",
            sort: None,
            width: Column::initial(150.0).at_least(90.0).clip(true),
        },
        ColumnSpec {
            key: "engine",
            label: "Engine",
            sort: None,
            width: Column::initial(110.0).at_least(70.0).clip(true),
        },
        ColumnSpec {
            key: "tags",
            label: "Tags",
            sort: None,
            width: Column::initial(120.0).at_least(70.0).clip(true),
        },
        ColumnSpec {
            key: "error",
            label: "Last error",
            sort: None,
            width: Column::remainder().at_least(120.0).clip(true),
        },
    ]
}

/// A batch operation over the selected recordings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchOperation {
    /// Queue transcription for each recording.
    Transcribe(Vec<RecordingId>),
    /// Put the recordings' identities on the clipboard, one per line, for a
    /// support report. Identities are not secrets; titles, paths and
    /// transcripts are never copied.
    CopyIdentities(Vec<RecordingId>),
}

/// What one frame of the grid produced.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TableOutput {
    /// The user chose a column to sort by.
    pub sort: Option<RecordingSortField>,
    /// The user opened a recording.
    pub open: Option<RecordingId>,
    /// The last loaded row came into view, or Load more was pressed.
    pub load_more: bool,
    /// A batch operation over the current selection.
    pub batch: Option<BatchOperation>,
}

impl TableOutput {
    fn merge(&mut self, other: Self) {
        self.sort = other.sort.or(self.sort);
        self.open = other.open.or(self.open);
        self.load_more |= other.load_more;
        if other.batch.is_some() {
            self.batch = other.batch;
        }
    }
}

/// Frame-to-frame state of one grid, owned by the screen.
#[derive(Debug, Default)]
pub struct TableState {
    /// Selected recordings and the keyboard cursor.
    pub selection: Selection,
    scroll_to: Option<usize>,
}

/// Everything the grid needs that it does not own.
pub struct RecordingTable<'a> {
    /// Accessible name of the grid.
    pub label: &'a str,
    /// Rows loaded so far.
    pub rows: &'a [RecordingSummary],
    /// Rows matching the query at query time, which is what assistive
    /// technology is told the grid contains.
    pub row_count: u64,
    /// Column the list is ordered by.
    pub sort_field: RecordingSortField,
    /// Direction of that order.
    pub sort_direction: SortDirection,
    /// Whether another page exists.
    pub has_more: bool,
    /// Whether a page is in flight.
    pub loading_more: bool,
}

impl std::fmt::Debug for RecordingTable<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecordingTable")
            .field("label", &self.label)
            .field("rows", &self.rows.len())
            .field("row_count", &self.row_count)
            .finish()
    }
}

impl RecordingTable<'_> {
    /// Draw the grid and run its keyboard and pointer model.
    pub fn show(self, ui: &mut Ui, state: &mut TableState) -> TableOutput {
        let columns = columns();
        state.selection.reconcile(self.rows);

        let grid_id = ui.make_persistent_id(("library_grid", self.label));
        let focused = ui.memory(|memory| memory.has_focus(grid_id));

        let mut output = TableOutput::default();
        if focused {
            let before = state.selection.active();
            output.merge(self.keyboard(ui, &mut state.selection));
            if state.selection.active() != before {
                state.scroll_to = Some(state.selection.active());
            }
        }

        let mut active_row_id = None;
        let mut request_focus = false;
        let body = ui.scope_builder(UiBuilder::new().accessibility_parent(grid_id), |ui| {
            self.header_and_body(
                ui,
                state,
                &columns,
                &mut output,
                &mut active_row_id,
                &mut request_focus,
            );
        });

        // The grid's focus anchor. It is a zero-sized interaction so that it
        // carries the single tab stop and the grid's accessible identity
        // without lying on top of the rows and swallowing their clicks; the
        // node is given the body's real rectangle below.
        let anchor_rect = egui::Rect::from_min_size(body.response.rect.min, egui::Vec2::ZERO);
        let grid = ui.interact(anchor_rect, grid_id, Sense::focusable_noninteractive());
        if request_focus {
            grid.request_focus();
        }
        if grid.has_focus() {
            // Up and Down move the cursor inside the grid; they must not move
            // focus out of it. Tab and Escape still leave, which is what makes
            // the grid one tab stop rather than a focus trap.
            ui.memory_mut(|memory| {
                memory.set_focus_lock_filter(
                    grid_id,
                    egui::EventFilter {
                        tab: false,
                        horizontal_arrows: false,
                        vertical_arrows: true,
                        escape: false,
                    },
                );
            });
        }
        let label = self.label.to_owned();
        let row_count = self.row_count as usize + 1;
        let column_count = columns.len();
        let bounds = body.response.rect;
        ui.ctx().accesskit_node_builder(grid_id, |node| {
            node.set_role(accesskit::Role::Grid);
            node.set_label(label);
            node.set_multiselectable();
            node.set_bounds(accesskit::Rect {
                x0: bounds.min.x.into(),
                y0: bounds.min.y.into(),
                x1: bounds.max.x.into(),
                y1: bounds.max.y.into(),
            });
            // The whole matching set, plus the header row: assistive
            // technology must hear "row 4 of 10,000", not "row 4 of 32
            // rendered".
            node.set_row_count(row_count);
            node.set_column_count(column_count);
            if let Some(id) = active_row_id {
                node.set_active_descendant(id);
            }
        });

        self.footer(ui, &state.selection, &mut output);
        output
    }

    fn header_and_body(
        &self,
        ui: &mut Ui,
        state: &mut TableState,
        columns: &[ColumnSpec; 10],
        output: &mut TableOutput,
        active_row_id: &mut Option<accesskit::NodeId>,
        request_focus: &mut bool,
    ) {
        let mut builder = TableBuilder::new(ui)
            .id_salt(("library_table", self.label))
            .striped(true)
            .sense(Sense::CLICK)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .auto_shrink([false, false]);
        if let Some(row) = state.scroll_to.take() {
            builder = builder.scroll_to_row(row, Some(egui::Align::Center));
        }
        for column in columns {
            builder = builder.column(column.width);
        }

        let mut sort_clicked = None;
        let table = builder.header(HEADER_HEIGHT, |mut header| {
            for (index, column) in columns.iter().enumerate() {
                header.col(|ui| {
                    if self.header_cell(ui, column, index, columns.len()) {
                        sort_clicked = column.sort;
                    }
                });
            }
        });
        output.sort = sort_clicked.or(output.sort);

        let rows = self.rows;
        let total = rows.len();
        let active = state.selection.active();
        let selection = &state.selection;
        let mut events: Vec<RowEvent> = Vec::new();
        let mut load_more = false;

        table.body(|body| {
            body.rows(ROW_HEIGHT, total, |mut table_row| {
                let index = table_row.index();
                let Some(row) = rows.get(index) else {
                    return;
                };
                let selected = selection.contains(row.recording_id);
                table_row.set_selected(selected);
                // The row's own `Response` is the union of the cell `Ui`
                // responses, and a `Ui` response does not know its rectangle
                // until the `Ui` is dropped — so the row node's bounds come
                // from the rectangles the cells actually used. Without this a
                // row has no position and nothing can click it.
                let mut row_rect = egui::Rect::NOTHING;
                for column in columns {
                    let (used, _) = table_row.col(|ui| cell(ui, column.key, row));
                    row_rect = row_rect.union(used);
                }
                let response = table_row.response();
                if index == active {
                    *active_row_id = Some(response.id.accesskit_id());
                }
                annotate_row(&response, row, index, total, selected, row_rect);

                if response.clicked() {
                    let modifiers = response.ctx.input(|input| input.modifiers);
                    events.push(RowEvent::Click {
                        index,
                        shift: modifiers.shift,
                        command: modifiers.command,
                    });
                    *request_focus = true;
                }
                if response.double_clicked() {
                    events.push(RowEvent::Open(index));
                }

                response.context_menu(|ui| {
                    if ui.button("Open recording").clicked() {
                        events.push(RowEvent::Open(index));
                        ui.close();
                    }
                    let toggle_label = if selected {
                        "Remove from selection"
                    } else {
                        "Add to selection"
                    };
                    if ui.button(toggle_label).clicked() {
                        events.push(RowEvent::Toggle(index));
                        ui.close();
                    }
                    if ui.button("Select all loaded recordings").clicked() {
                        events.push(RowEvent::SelectAll);
                        ui.close();
                    }
                    if ui.button("Clear selection").clicked() {
                        events.push(RowEvent::ClearSelection);
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Transcribe selected").clicked() {
                        events.push(RowEvent::Transcribe(index));
                        ui.close();
                    }
                    if ui.button("Copy recording identities").clicked() {
                        events.push(RowEvent::CopyIdentities(index));
                        ui.close();
                    }
                });

                if self.has_more && !self.loading_more && index + 1 == total {
                    load_more = true;
                }
            });
        });

        output.load_more |= load_more;
        for event in events {
            self.apply_row_event(event, &mut state.selection, output);
        }
    }

    fn header_cell(&self, ui: &mut Ui, column: &ColumnSpec, index: usize, count: usize) -> bool {
        let active = column.sort == Some(self.sort_field);
        let glyph = match (column.sort.is_some(), active, self.sort_direction) {
            (false, _, _) => "",
            (true, false, _) => " ↕",
            (true, true, SortDirection::Asc) => " ▲",
            (true, true, SortDirection::Desc) => " ▼",
        };
        let response = if column.sort.is_some() {
            ui.button(RichText::new(format!("{}{glyph}", column.label)).strong())
        } else {
            ui.label(RichText::new(column.label).strong())
        };
        let label = column.label.to_owned();
        let direction = self.sort_direction;
        ui.ctx().accesskit_node_builder(response.id, |node| {
            node.set_role(accesskit::Role::ColumnHeader);
            node.set_label(label);
            node.set_column_index(index);
            node.set_column_count(count);
            // Only the active column reports a direction; the others report
            // none at all, exactly as `aria-sort` requires.
            if active {
                node.set_sort_direction(match direction {
                    SortDirection::Asc => accesskit::SortDirection::Ascending,
                    SortDirection::Desc => accesskit::SortDirection::Descending,
                });
            }
        });
        response.clicked()
    }

    fn keyboard(&self, ui: &Ui, selection: &mut Selection) -> TableOutput {
        let mut output = TableOutput::default();
        let rows = self.rows;
        if rows.is_empty() {
            return output;
        }
        ui.input_mut(|input| {
            // `consume_key` ignores Shift when the pattern does not name it,
            // so each movement key is consumed once and Shift is read from the
            // modifier state rather than from a second pattern.
            let extend = input.modifiers.shift;
            if input.consume_key(Modifiers::COMMAND, Key::A) {
                selection.select_all(rows);
            }
            if input.consume_key(Modifiers::NONE, Key::ArrowDown) {
                selection.move_down(extend, rows);
            }
            if input.consume_key(Modifiers::NONE, Key::ArrowUp) {
                selection.move_up(extend, rows);
            }
            if input.consume_key(Modifiers::NONE, Key::Home) {
                selection.move_to(0, extend, rows);
            }
            if input.consume_key(Modifiers::NONE, Key::End) {
                selection.move_to(rows.len() - 1, extend, rows);
            }
            if input.consume_key(Modifiers::NONE, Key::Enter) {
                if let Some(row) = rows.get(selection.active()) {
                    output.open = Some(row.recording_id);
                }
            }
            if input.consume_key(Modifiers::NONE, Key::Space) {
                if let Some(row) = rows.get(selection.active()) {
                    selection.toggle(row.recording_id);
                }
            }
        });
        output
    }

    fn apply_row_event(
        &self,
        event: RowEvent,
        selection: &mut Selection,
        output: &mut TableOutput,
    ) {
        match event {
            RowEvent::Click {
                index,
                shift,
                command,
            } => {
                if shift {
                    for id in range_ids(self.rows, selection.anchor(), index) {
                        if !selection.contains(id) {
                            selection.toggle(id);
                        }
                    }
                    selection.focus(index);
                } else if command {
                    if let Some(row) = self.rows.get(index) {
                        selection.move_to(index, false, self.rows);
                        selection.toggle(row.recording_id);
                    }
                } else if let Some(row) = self.rows.get(index) {
                    selection.move_to(index, false, self.rows);
                    selection.select_only(row.recording_id);
                }
            }
            RowEvent::Open(index) => {
                if let Some(row) = self.rows.get(index) {
                    output.open = Some(row.recording_id);
                }
            }
            RowEvent::Toggle(index) => {
                if let Some(row) = self.rows.get(index) {
                    selection.toggle(row.recording_id);
                }
            }
            RowEvent::SelectAll => selection.select_all(self.rows),
            RowEvent::ClearSelection => selection.clear(),
            RowEvent::Transcribe(index) => {
                output.batch = Some(BatchOperation::Transcribe(self.targets(selection, index)));
            }
            RowEvent::CopyIdentities(index) => {
                output.batch = Some(BatchOperation::CopyIdentities(
                    self.targets(selection, index),
                ));
            }
        }
    }

    /// A context-menu operation applies to the selection, or to the row it
    /// was opened on when nothing is selected.
    fn targets(&self, selection: &Selection, index: usize) -> Vec<RecordingId> {
        if selection.is_empty() {
            self.rows
                .get(index)
                .map(|row| vec![row.recording_id])
                .unwrap_or_default()
        } else {
            selection.ids()
        }
    }

    fn footer(&self, ui: &mut Ui, selection: &Selection, output: &mut TableOutput) {
        ui.horizontal(|ui| {
            let count = format!(
                "Showing {} of {} recordings (snapshot at query time)",
                self.rows.len(),
                self.row_count
            );
            status_line(ui, &count);
            if self.has_more {
                let label = if self.loading_more {
                    "Loading more…"
                } else {
                    "Load more recordings"
                };
                if ui
                    .add_enabled(!self.loading_more, egui::Button::new(label))
                    .clicked()
                {
                    output.load_more = true;
                }
            }
        });
        status_line(ui, &selection.describe(self.rows.len()));
    }
}

/// A polite live region: one line of text that assistive technology announces
/// when it changes.
fn status_line(ui: &mut Ui, text: &str) {
    let response = ui.label(text);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, text));
    let announcement = text.to_owned();
    ui.ctx().accesskit_node_builder(response.id, |node| {
        node.set_role(accesskit::Role::Status);
        node.set_label(announcement);
        node.set_live(accesskit::Live::Polite);
    });
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowEvent {
    Click {
        index: usize,
        shift: bool,
        command: bool,
    },
    Open(usize),
    Toggle(usize),
    SelectAll,
    ClearSelection,
    Transcribe(usize),
    CopyIdentities(usize),
}

fn cell(ui: &mut Ui, key: &str, row: &RecordingSummary) {
    // egui's selectable labels sense click **and drag**, so a label inside a
    // cell wins the hit test over the row behind it and the row stops being
    // clickable. A grid row is the interactive element here, so cell text is
    // not individually selectable; copying is offered by the row's context
    // menu instead.
    ui.style_mut().interaction.selectable_labels = false;
    match key {
        "title" => {
            ui.label(&row.title);
        }
        "recorded" => {
            ui.label(format_timestamp(row.recorded_at.as_deref()));
        }
        "imported" => {
            ui.label(format_timestamp(Some(&row.imported_at)));
        }
        "duration" => {
            ui.label(format_duration(row.duration_ms));
        }
        "source" => {
            ui.label(&row.source.label);
        }
        "import" => {
            status_badge(ui, import_status(row.import_status));
        }
        "transcription" => {
            status_badge(ui, transcription_status(row.transcription_status));
        }
        "engine" => {
            ui.label(row.processing_engine.as_deref().unwrap_or("Not set"));
        }
        "tags" => {
            ui.label(if row.tags.is_empty() {
                "None".to_owned()
            } else {
                row.tags.join(", ")
            });
        }
        "error" => match &row.last_error {
            Some(envelope) => {
                ui.label(RichText::new(&envelope.message).color(ui.visuals().error_fg_color));
            }
            None => {
                ui.label("None");
            }
        },
        other => unreachable!("unknown library column {other}"),
    }
}

/// What assistive technology hears for one row.
#[must_use]
pub fn row_label(row: &RecordingSummary) -> String {
    format!(
        "{}. Recorded {}. Duration {}. {}. {}. {}.",
        row.title,
        format_timestamp(row.recorded_at.as_deref()),
        spoken_duration(row.duration_ms),
        row.source.label,
        import_status(row.import_status).label,
        transcription_status(row.transcription_status).label,
    )
}

fn annotate_row(
    response: &egui::Response,
    row: &RecordingSummary,
    index: usize,
    loaded: usize,
    selected: bool,
    rect: egui::Rect,
) {
    let label = row_label(row);
    // `widget_info` first: it is what registers the node and its click
    // action. The builder below then gives it the role a grid row actually
    // has — which `WidgetType` cannot express — and the rectangle the cells
    // occupied.
    let info_label = label.clone();
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, &info_label));
    response.ctx.accesskit_node_builder(response.id, |node| {
        node.set_role(accesskit::Role::Row);
        node.set_label(label);
        node.set_bounds(accesskit::Rect {
            x0: rect.min.x.into(),
            y0: rect.min.y.into(),
            x1: rect.max.x.into(),
            y1: rect.max.y.into(),
        });
        // Index within the grid, counting the header as row one.
        node.set_row_index(index + 1);
        node.set_position_in_set(index + 1);
        node.set_size_of_set(loaded);
        node.set_selected(selected);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screens::library::testing::row_with;

    #[test]
    fn a_row_announces_its_title_source_and_both_lifecycle_states() {
        let row = row_with("Board meeting", |row| {
            row.duration_ms = Some(61_000);
            row.recorded_at = Some("2026-03-12T14:03:22Z".to_owned());
        });
        let label = row_label(&row);
        assert!(label.starts_with("Board meeting."));
        assert!(label.contains("12 Mar 2026, 14:03 UTC"));
        assert!(label.contains("1 minute 1 second"));
    }

    #[test]
    fn an_unmeasured_row_announces_unknown_rather_than_zero() {
        let row = row_with("Unmeasured", |row| {
            row.duration_ms = None;
            row.recorded_at = None;
        });
        let label = row_label(&row);
        assert!(label.contains("duration unknown"), "{label}");
        assert!(label.contains("Recorded Unknown"), "{label}");
        assert!(!label.contains("0 second"), "{label}");
    }

    #[test]
    fn the_column_set_is_the_spec_set_and_every_key_renders() {
        let keys: Vec<&str> = columns().iter().map(|column| column.key).collect();
        assert_eq!(
            keys,
            [
                "title",
                "recorded",
                "imported",
                "duration",
                "source",
                "import",
                "transcription",
                "engine",
                "tags",
                "error",
            ]
        );
        let sortable: Vec<&str> = columns()
            .iter()
            .filter(|column| column.sort.is_some())
            .map(|column| column.key)
            .collect();
        assert_eq!(sortable, ["title", "recorded", "imported", "duration"]);
    }
}
