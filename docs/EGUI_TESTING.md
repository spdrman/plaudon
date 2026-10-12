# egui testing: harness, accessibility and snapshots

How Plaudon's Rust-only user interface is tested. Started in EPIC O task O1.2 (#200), which had to
decide the snapshot strategy (`docs/EGUI_MIGRATION_PLAN.md` §7.2) before any screen existed. O2.5
(#209) extends this document with the full legacy-test mapping; everything below is in force now.

## 1. The rule

All UI, end-to-end and visual tests use [`egui_kittest`](https://github.com/emilk/egui/tree/main/crates/egui_kittest).
No new vitest or Playwright test may be written. The legacy suites stay as a parity oracle until the
cutover and are then deleted with the React frontend.

## 2. The harness

`crates/ui-testkit` (`plaudon-ui-testkit`) owns the harness so no test reinvents it:

```rust
let mut harness = plaudon_ui_testkit::builder()
    .build_ui_state(|ui, app: &mut PlaudonEgui| app.view(ui), app);
harness.run();
```

Fixed by the builder, because a test that depends on the host is not a test:

| Setting | Value | Why |
| --- | --- | --- |
| Window size | 1280×820 (`DEFAULT_SIZE`), 1000×820 (`COMPACT_SIZE`) | the sizes `tauri.conf.json` opened with and the compact breakpoint |
| `pixels_per_point` | 1.0 | a Retina workstation and a CI container must produce one image |
| Theme | light | the dark and high-contrast themes get their own named cases |
| Reported OS | `OperatingSystem::Mac` | shortcut rendering (`Cmd` vs `Ctrl`) must not depend on the runner |
| Snapshot options | `tests/visual`, per-pixel threshold 0.6, **0** tolerated failing pixels | §4 |

**A test crate that drives the pointer must enable egui's `default_fonts`.** The workspace entry
builds egui with `default-features = false`, so a crate that only dev-depends on `egui_kittest`
links no font: every galley lays out with zero glyphs, `Galley::cursor_from_pos` answers offset 0
for every position, and click-to-place-caret, drag-select and double-click-to-select-a-word all
pass vacuously. `plaudon-ui-testkit` turns the feature on for the tests that go through it; a crate
that builds its own harness (`crates/ui`) adds
`egui = { workspace = true, features = ["default_fonts"] }` to its `[dev-dependencies]`.

## 3. Accessibility assertions replace axe

egui has no axe equivalent, and pretending otherwise would be the easiest way to claim a WCAG 2.2 AA
conformance we have not checked (ADR-0023). The replacements, all in `plaudon_ui_testkit::a11y`:

| Legacy check | Rust replacement |
| --- | --- |
| axe role/name violations | `a11y::nodes(&harness)` — every AccessKit node's role, label and value, asserted per screen |
| `getByLabel`, `getByRole` | `harness.get_by_label(..)`, `query_all_by_role(..)` (kittest's own queries) |
| Playwright "no interactive element is unreachable by keyboard" | `a11y::focus_order(&mut harness, max_stops)` — presses Tab, reports the labels reached, stops when the order cycles so a focus trap fails instead of hanging |
| axe colour-contrast | token contrast unit tests over the 20 roles × 4 themes (O1.3) |
| — (nothing automated can replace it) | the manual VoiceOver/Narrator script, run per phase (`docs/accessibility/manual-screen-reader-script.md`) |

Example, from `apps/plaudon-egui/tests/spike_kittest.rs`: the vendored divider must report
`Role::Splitter` with the label "Resize the library pane", and the two toolbar actions and the
divider must all appear in the Tab order.

## 4. Snapshot policy — the O1.2 decision

**Context.** None of the four reference repositories commits baseline PNGs or calls
`Harness::snapshot`; two render to PNG only behind an environment variable, and craft-libs compares a
widget against an independently painted reference on the same machine
(`docs/EGUI_REFERENCE_COMPONENTS.md` §2.5, §3.6, §4.6). There was no precedent to copy, and
`cargo test` on a developer's Mac renders through Metal while CI and MARS render through software
Vulkan — the same frame is not the same image.

**Decision.**

1. **Committed baselines**, as PNGs under each crate's `tests/visual/` directory, next to the test
   that produces them. (`kittest`'s default is `tests/snapshots`; Plaudon overrides it in
   `plaudon_ui_testkit::visual::options`.)
2. **One renderer defines the baseline:** the MARS Docker image with mesa/lavapipe software Vulkan,
   which is what `scripts/e2e-remote.sh cargo` already sets up (`LIBGL_ALWAYS_SOFTWARE=1`,
   `GALLIUM_DRIVER=llvmpipe`, `WGPU_BACKEND=vulkan`). A baseline produced anywhere else is not
   valid.
3. **Pixel comparison is opt-in** through `PLAUDON_VISUAL=1`. Without it,
   `plaudon_ui_testkit::visual::assert_snapshot` returns `Outcome::Skipped`, renders nothing and
   fails nothing, while every interaction and accessibility assertion in the same test still runs.
   A developer machine therefore never fails on a GPU difference, and never reports a comparison it
   did not make.
4. **Tolerance: kittest's default per-pixel threshold (0.6) and zero tolerated failing pixels.** On
   one fixed renderer a differing pixel is a real change — a moved divider, a shifted border, a
   mis-rendered icon — not noise. A test that genuinely needs slack states
   `SnapshotOptions::max_failed_pixels` locally, with a comment saying why; it is never raised
   globally.
5. **A snapshot may only contain a deterministic frame.** Identifiers (recording and job UUIDs),
   timestamps and durations minted during the test are not reproducible, so either snapshot a state
   that predates them — the spike snapshots the empty library, not the post-import activity log — or
   mask the region with `Harness::mask`. This was found the hard way: the first baseline compared
   clean against itself and failed on the next run because the activity log printed a fresh job id.
6. **Scope.** Snapshots cover screens and the few widgets whose *painting* is the behaviour
   (waveform, timeline, progress). Everything else is asserted on state and on the AccessKit tree,
   which is faster, legible in a diff and does not need a GPU. craft-libs' paired-reference
   technique stays available as a fallback for a widget that proves flaky, per §7.2 of the plan.

**MARS image requirement.** The Linux build of the shell reaches D-Bus through AccessKit's AT-SPI
adapter and the secret-service keyring backend, so the container needs `libdbus-1-dev`, which the
runner's default package list does not include. Pass it explicitly:

```sh
PLAUDON_E2E_APT_PACKAGES=libdbus-1-dev scripts/e2e-remote.sh shell '…'
```

**Running and regenerating.** Compare on MARS:

```sh
scripts/e2e-remote.sh cargo test -p plaudon-egui --test spike_kittest
# with comparison enabled:
PLAUDON_E2E_ENV='PLAUDON_VISUAL=1' …   # or use shell mode, below
scripts/e2e-remote.sh shell 'PLAUDON_VISUAL=1 cargo test -p plaudon-egui --test spike_kittest'
```

Regenerate a baseline (and copy it back):

```sh
PLAUDON_E2E_APT_PACKAGES=libdbus-1-dev scripts/e2e-remote.sh shell \
  'UPDATE_SNAPSHOTS=1 PLAUDON_VISUAL=1 cargo test -p plaudon-egui --test spike_kittest'
# artifacts land in target/e2e-results/<crate>/tests/visual/*.png
cp target/e2e-results/apps/plaudon-egui/tests/visual/*.png apps/plaudon-egui/tests/visual/
```

The runner copies back `*.png` under any `tests/visual/` directory as well as `*.new.png` and
`*.diff.png`, so a failed comparison always arrives with the image that failed and the diff. A
regenerated baseline is committed **with the change that caused it**, never on its own.

## 5. Where tests run

| Suite | Where | Command |
| --- | --- | --- |
| `plaudon-app` unit and dispatcher tests | anywhere, no GPU | `cargo test -p plaudon-app` |
| `plaudon-ui-testkit` self-tests | anywhere (software raster) | `cargo test -p plaudon-ui-testkit` |
| `plaudon-craft-ui` vendored widget tests | anywhere, no GPU | `cargo test -p plaudon-craft-ui` |
| Token contrast and theme unit tests (O1.3) | anywhere, no GPU | `cargo test -p plaudon-ui theme::` |
| Shell interaction and accessibility suite (O1.3) | locally for logic, MARS for pixels | `cargo test -p plaudon-ui --test shell_kittest` |
| Window wiring and state persistence (O1.3) | anywhere (software raster) | `cargo test -p plaudon-egui --test shell_app` |
| `plaudon-ui` screen and copy tests (devices, sources) | anywhere (software raster) | `cargo test -p plaudon-ui` |
| Library, inbox, search and recording detail (O1.4) | anywhere (software raster); MARS for pixels | `cargo test -p plaudon-ui --test library_screen` |
| Library 10,000-row scale case (O1.4) | anywhere for the ratio, MARS for the millisecond figure | `cargo test -p plaudon-ui --test library_perf -- --nocapture` |
| kittest interaction tests | locally for logic, MARS for pixels | `cargo test -p plaudon-egui --test spike_kittest` |
| AI workflow panels (O2.2): consent, claim review, stale/regenerate, Local-only refusal | anywhere, no GPU | `cargo test -p plaudon-ui --test ai_workflows --test ai_review --test ai_summarize` |
| Snapshot comparison, scale and performance runs | **MARS only** | `scripts/e2e-remote.sh …` |

The resource policy is part of the test strategy: heavy or renderer-dependent runs go to MARS Docker
(`--rm`, capped CPU and memory), never to a workstation.

## 6. Performance budgets

There are none yet, and none may be invented. `docs/benchmarks/README.md` forbids quoting a target
before a measured baseline. The first measured UI numbers come from the MARS runs of the O1.4 library
and O1.5 player suites; the budgets are written here then, with the run that produced them.

### 6.1 The library's scale case (O1.4, #202)

`crates/ui/tests/library_perf.rs` holds the 10,000-row case. Two of its assertions need no budget,
because they are machine-independent:

| Assertion | Why it is safe to assert anywhere |
| --- | --- |
| fewer than 100 of 10,000 rows are laid out per frame | a count taken from the AccessKit tree, identical on every machine |
| a 10,000-row frame costs less than 3× a 100-row frame | a ratio measured on the machine running it, so it catches a quadratic implementation however fast the host is |

The absolute mean frame cost is **printed, not asserted**, and the millisecond budget belongs here,
next to the MARS run that produced it:

```sh
PLAUDON_E2E_APT_PACKAGES=libdbus-1-dev scripts/e2e-remote.sh shell \
  'cargo test -p plaudon-ui --test library_perf -- --nocapture'
```

**Measured 2026-10-12 on MARS** (`rust:1-bookworm`, 8 CPUs, mesa/lavapipe software raster, `test`
profile), with the library's own fixtures:

| Rows | Mean frame cost |
| --- | --- |
| 100 | 4.930 ms |
| 10,000 | 5.848 ms |

**Budget: 10 ms per frame for 10,000 rows.** The measurement with headroom, enforced only where it
was measured — `PLAUDON_PERF=1`, which `scripts/e2e-remote.sh` runs set:

```sh
PLAUDON_E2E_APT_PACKAGES=libdbus-1-dev scripts/e2e-remote.sh shell \
  'PLAUDON_PERF=1 cargo test -p plaudon-ui --test library_perf -- --nocapture'
```

Raising the number is a decision, not a fix: a frame that grows past it means the table stopped
being virtualized, which the two machine-independent assertions above are there to localise.

The committed baselines `crates/ui/tests/visual/library_empty.png` and
`crates/ui/tests/visual/library_100_rows.png` were produced in the same run.
