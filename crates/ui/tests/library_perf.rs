//! The library's scale case: ten thousand rows (risk R15).
//!
//! Two things are asserted, and they are different claims:
//!
//! 1. **Virtualization is structural.** However many rows the library holds,
//!    only the rows inside the viewport are laid out. This is a count, it is
//!    deterministic, and it fails the same way on every machine.
//! 2. **The frame cost does not grow with the library.** A hundredfold more
//!    rows must not cost noticeably more per frame. This is a ratio measured
//!    on the machine running the test, so it is meaningful everywhere,
//!    unlike an absolute millisecond figure.
//!
//! 3. **The measured budget**, which is only enforced where it was measured:
//!    the MARS runner signals that with `PLAUDON_PERF=1`. Everywhere else the
//!    cost is printed, so a developer sees the number without the suite
//!    failing on a machine the budget does not describe
//!    (`docs/EGUI_TESTING.md` §6.1).

use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use plaudon_app::command::{AppCommand, AppResponse, CommandId};
use plaudon_ui::screens::library::table::{RecordingTable, TableState};
use plaudon_ui::screens::library::testing::{page, rows};
use plaudon_ui::screens::library::LibraryScreen;

/// Rows in the scale case.
const SCALE_ROWS: usize = 10_000;

/// Frames measured after the warm-up.
const MEASURED_FRAMES: usize = 60;

/// Environment variable that enforces the budget.
const ENFORCE_ENV: &str = "PLAUDON_PERF";

/// Mean frame cost allowed for a ten-thousand-row library, in milliseconds.
///
/// Measured on the MARS runner (`rust:1-bookworm`, 8 CPUs, software raster)
/// on 2026-10-12: 4.930 ms for 100 rows and 5.848 ms for 10,000 rows in the
/// `test` profile. The budget is that measurement with headroom, so it
/// catches a change in the order of growth rather than runner noise.
/// `docs/EGUI_TESTING.md` §6.1 records the run.
const FRAME_BUDGET_MS: f64 = 10.0;

fn enforced() -> bool {
    std::env::var(ENFORCE_ENV).is_ok_and(|value| value == "1" || value == "true")
}

struct TableApp {
    rows: Vec<plaudon_app::dto::RecordingSummary>,
    state: TableState,
}

fn table_harness(count: usize) -> Harness<'static, TableApp> {
    let mut harness = plaudon_ui_testkit::builder().build_ui_state(
        |ui: &mut egui::Ui, app: &mut TableApp| {
            RecordingTable {
                label: "Recordings",
                rows: &app.rows,
                row_count: app.rows.len() as u64,
                sort_field: plaudon_app::dto::RecordingSortField::RecordedAt,
                sort_direction: plaudon_app::dto::SortDirection::Desc,
                has_more: false,
                loading_more: false,
            }
            .show(ui, &mut app.state);
        },
        TableApp {
            rows: rows(count),
            state: TableState::default(),
        },
    );
    harness.run();
    harness
}

#[test]
fn ten_thousand_rows_lay_out_only_the_visible_ones() {
    let harness = table_harness(SCALE_ROWS);
    let rendered = harness
        .query_all_by_role(egui::accesskit::Role::Row)
        .count();
    assert!(rendered > 0, "nothing was rendered");
    assert!(
        rendered < 100,
        "{rendered} of {SCALE_ROWS} rows were laid out; the table is not virtualized"
    );
}

#[test]
fn the_cost_of_a_frame_does_not_grow_with_the_library() {
    let small = mean_frame_ms(&mut table_harness(100));
    let large = mean_frame_ms(&mut table_harness(SCALE_ROWS));
    println!("library frame cost: 100 rows {small:.3} ms, {SCALE_ROWS} rows {large:.3} ms");

    // Immediate mode makes a naive table linear in the row count; the claim
    // here is that it is not. A hundredfold more rows must not cost
    // noticeably more per frame.
    let ratio = large / small.max(f64::EPSILON);
    if enforced() {
        assert!(
            large <= FRAME_BUDGET_MS,
            "a {SCALE_ROWS}-row frame took {large:.3} ms, over the {FRAME_BUDGET_MS} ms budget"
        );
    }

    assert!(
        ratio < 3.0,
        "a {SCALE_ROWS}-row frame cost {ratio:.1}x a 100-row frame \
         ({small:.3} ms vs {large:.3} ms): the table is not virtualized"
    );
}

#[test]
fn ten_thousand_rows_arrive_as_a_hundred_pages_and_render_once_each() {
    let mut harness = plaudon_ui_testkit::builder().build_ui_state(
        |ui: &mut egui::Ui, app: &mut ScreenApp| app.view(ui),
        ScreenApp::new(),
    );
    harness.run();

    let page_size = 100;
    for index in 0..(SCALE_ROWS / page_size) {
        let start = index * page_size;
        let items: Vec<_> = (start..start + page_size)
            .map(plaudon_ui::screens::library::testing::row_at)
            .collect();
        let next = (index + 1 < SCALE_ROWS / page_size).then(|| format!("{:04x}", index + 1));
        let id = harness.state().next_id;
        harness.state_mut().respond(
            id,
            Ok(AppResponse::ListRecordings(page(
                items,
                next.as_deref(),
                SCALE_ROWS as u64,
            ))),
        );
        harness.state_mut().load_more();
        harness.run();
    }

    assert_eq!(
        harness.state().screen.list().rows().len(),
        SCALE_ROWS,
        "every page must be kept, and no recording may be dropped or repeated"
    );
    let rendered = harness
        .query_all_by_role(egui::accesskit::Role::Row)
        .count();
    assert!(
        rendered < 100,
        "{rendered} rows were laid out after loading {SCALE_ROWS}"
    );
}

/// Mean cost of a frame, in milliseconds, after a warm-up frame.
fn mean_frame_ms<State>(harness: &mut Harness<'_, State>) -> f64 {
    harness.run();
    let start = std::time::Instant::now();
    for _ in 0..MEASURED_FRAMES {
        harness.step();
    }
    start.elapsed().as_secs_f64() * 1_000.0 / MEASURED_FRAMES as f64
}

struct ScreenApp {
    screen: LibraryScreen,
    next_id: u64,
}

impl ScreenApp {
    fn new() -> Self {
        Self {
            screen: LibraryScreen::new(),
            next_id: 0,
        }
    }

    fn view(&mut self, ui: &mut egui::Ui) {
        let Self { screen, next_id } = self;
        let mut send = |_command: AppCommand| {
            *next_id += 1;
            CommandId::from_raw(*next_id)
        };
        let _ = screen.ui(ui, &mut send);
    }

    fn respond(
        &mut self,
        id: u64,
        result: Result<AppResponse, plaudon_domain::error::ErrorEnvelope>,
    ) {
        let Self { screen, next_id } = self;
        let mut send = |_command: AppCommand| {
            *next_id += 1;
            CommandId::from_raw(*next_id)
        };
        screen.on_response(CommandId::from_raw(id), &result, &mut send);
    }

    fn load_more(&mut self) {
        let Self { screen, next_id } = self;
        let mut send = |_command: AppCommand| {
            *next_id += 1;
            CommandId::from_raw(*next_id)
        };
        screen.load_more(&mut send);
    }
}
