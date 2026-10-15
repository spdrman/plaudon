# Revision playbook

How to carry out the UI revision in this repo without breaking what already works. It summarises the
parts of `docs/EGUI_TESTING.md`, `docs/EGUI_BUILD.md`, `docs/PRECOMMIT.md`, `docs/EGUI_DESIGN_SYSTEM.md`,
`docs/EGUI_ARCHITECTURE.md` and the README that apply here, and adds what I found while reading the
code. Where this file and those documents disagree, they win. Read [`GAP_ANALYSIS.md`](GAP_ANALYSIS.md)
first for the order of work.

## 1. Ground rules

1. **Builds and tests run in a container, not on a workstation.** The repo's own rule
   (`docs/EGUI_BUILD.md` section 1, `docs/EGUI_TESTING.md` section 5, README "Where tests run matters"):
   anything that renders (every snapshot, the perf run, the demo GIF), anything touching `whisper-rs` and
   anything workspace-wide goes through [`scripts/e2e-remote.sh`](../../../scripts/e2e-remote.sh), which
   runs the command in a disposable Docker container with mesa/lavapipe software Vulkan. The docs allow
   scoped logic-only runs natively (`cargo test -j 2 -p <crate>`, one cargo process, never workspace-wide).
   If your own policy is stricter, run those in the container too; the script runs any `cargo` command.
2. **Which host.** `PLAUDON_E2E_HOST` is `auto` (the default: a shared Linux runner when it is reachable
   and not busy, otherwise local Docker), `local`, or any ssh destination of your own. The script lists
   its other knobs in its header (`PLAUDON_E2E_CPUS`, `PLAUDON_E2E_MEMORY`, `PLAUDON_E2E_IMAGE`,
   `PLAUDON_E2E_APT_PACKAGES`, `PLAUDON_E2E_RESULTS`).
3. **A baseline from anywhere else is not valid.** The renderer is part of the baseline. A snapshot
   produced on a Mac GPU, or by a different container image, will not compare clean.
4. **No node, no browser.** Every UI test is `egui_kittest` through `cargo test` (ADR-0024). Do not add a
   JavaScript toolchain, a Playwright suite or a design-tool export step to the build.
5. **Never write a credential to disk, never commit one.** The pre-commit gate has a secret scan.
6. GitHub Actions is disabled; the gate is local (`docs/PRECOMMIT.md`). Do not describe a locally green
   commit as "CI green", and note that the Windows legs are unverified.

## 2. How the tests are built (what applies)

- The harness is `plaudon_ui_testkit::builder()`. It fixes: window 1280 by 820 pt
  (`DEFAULT_SIZE`; 1000 by 820 is `COMPACT_SIZE`), `pixels_per_point` 1.0, theme **light**, reported OS
  macOS (so shortcuts render `Cmd`), snapshots in `tests/visual` with kittest's per-pixel threshold 0.6
  and **zero** tolerated failing pixels. A test that needs slack states
  `SnapshotOptions::max_failed_pixels` locally with a comment; never raise it globally.
- A test crate that drives the pointer must enable egui's `default_fonts` in `[dev-dependencies]`
  (the workspace entry is `default-features = false`; without it every galley has zero glyphs and click
  and drag tests pass vacuously). `crates/ui` already does.
- Accessibility is asserted in `plaudon_ui_testkit::a11y`: `nodes`, `has_label`, `focus_order`,
  `assert_every_control_is_named`. The sweep `apps/plaudon-egui/tests/a11y_contract.rs` drives every
  destination, every sub-route, the recording detail, search, first run and the palette, and the
  exemption list is **empty**. A new control that lacks a name fails it.
- Scale: `scale_kittest.rs` renders the shell at rest, the library list and the recording detail at 1.0,
  1.5 and 2.0 and compares nine baselines named `scale-<case>@1x`, `@1_5x`, `@2x`.
- Every test that touches a screen builds it exactly as the binary does
  (`plaudon_egui::screens::register_all`), so the application a test drives is the application that ships.

## 3. The snapshot workflow

Pixel comparison is opt-in with `PLAUDON_VISUAL=1`. Without it `assert_snapshot` returns `Skipped`:
every interaction and accessibility assertion in the same test still runs, nothing pixel-level fails on
a developer machine.

**The 13 committed baselines** (all light theme, all 1280 by 820 pt; the `@1_5x` and `@2x` files are the same
frame at 1920 by 1230 and 2560 by 1640 pixels). Only `shell-ready` and `scale-shell-ready` render the whole
shell. The library and recording-detail cases render the screen alone, with no rail or toolbar:

| File | Produced by |
| --- | --- |
| `crates/ui/tests/visual/shell-ready.png` | `shell_kittest.rs` (`the_ready_shell_matches_its_baseline`) |
| `crates/ui/tests/visual/library_empty.png` | `library_screen.rs` (`an_empty_library_offers_import_and_connect_plaud`) |
| `crates/ui/tests/visual/library_100_rows.png` | `library_screen.rs` (`the_hundred_row_library_matches_its_baseline`) |
| `crates/ui/tests/visual/scale-shell-ready@1x.png`, `@1_5x`, `@2x` | `scale_kittest.rs` |
| `crates/ui/tests/visual/scale-library-100-rows@1x.png`, `@1_5x`, `@2x` | `scale_kittest.rs` |
| `crates/ui/tests/visual/scale-recording-detail@1x.png`, `@1_5x`, `@2x` | `scale_kittest.rs` |
| `apps/plaudon-egui/tests/visual/spike-empty-library.png` | `apps/plaudon-egui/tests/spike_kittest.rs` (the spike view, not the shell) |

There are no dark, high-contrast, Jobs, Devices, Settings, Onboarding, Consent or Recording-tab
baselines. The design is 29 boards; the revision should add snapshot cases for the states it draws
(see section 6). There is no dark-theme helper in the testkit yet; the shell's
`set_appearance(Appearance::Dark)` (`Shell::set_appearance`) is how `switching_appearance_repaints_in_the_other_theme`
changes theme. Suggested name for a dark baseline: `<name>-dark`.

**Compare** (container, `PLAUDON_VISUAL=1`):

```sh
PLAUDON_E2E_APT_PACKAGES=libdbus-1-dev scripts/e2e-remote.sh shell \
  'PLAUDON_VISUAL=1 cargo test -p plaudon-ui --test library_screen'
```

**Regenerate** (and copy the PNGs back; the runner copies `*.png`, `*.new.png` and `*.diff.png` from any
`tests/visual/` directory into `target/e2e-results/`):

```sh
PLAUDON_E2E_APT_PACKAGES=libdbus-1-dev scripts/e2e-remote.sh shell \
  'UPDATE_SNAPSHOTS=1 PLAUDON_VISUAL=1 cargo test -p plaudon-ui --test library_screen'
cp target/e2e-results/crates/ui/tests/visual/*.png crates/ui/tests/visual/
# the spike baseline lives in the app crate:
cp target/e2e-results/apps/plaudon-egui/tests/visual/*.png apps/plaudon-egui/tests/visual/
```

Then prove the committed files compare clean **without** `UPDATE_SNAPSHOTS`:

```sh
PLAUDON_E2E_APT_PACKAGES=libdbus-1-dev scripts/e2e-remote.sh shell \
  'PLAUDON_VISUAL=1 cargo test -p plaudon-ui --test scale_kittest'
```

Rules:

- **A regenerated baseline is committed with the change that caused it**, never on its own. (The one
  exception I recommend is step 0 of the work order: refresh the baselines on unchanged code first,
  because `shell-ready.png` is already stale; it lists six destinations and the shell has seven.)
- **A snapshot may only hold a deterministic frame.** No recording or job UUIDs, no timestamps, no
  durations minted during the test. Snapshot a state that predates them or mask the region with
  `Harness::mask`. The first baseline in this repo failed on its second run because of a fresh job id.
- Keep the snapshot suite small: it covers screens and widgets whose painting is the behaviour
  (waveform, timeline, progress). Everything else is asserted on state and the AccessKit tree.
- `PLAUDON_E2E_APT_PACKAGES=libdbus-1-dev` is needed for any run that builds the shell (AccessKit's
  AT-SPI adapter and the keyring backend reach D-Bus).
- The runner has no `.git` (rsync excludes it). Suites that inspect the committed tree make a throwaway
  snapshot commit inside the container; do not write a test that reads real history.

### 3.1 The demo GIF

`docs/assets/demo.gif` is the README's demo. It is generated, not edited:

```sh
scripts/make-demo-gif.sh                 # host chosen by scripts/e2e-remote.sh
PLAUDON_E2E_HOST=local scripts/make-demo-gif.sh
```

It runs `apps/plaudon-egui/tests/demo_capture.rs` (which is `#[ignore]` and drives the real application
over a fixture library at 1280 by 1000 pt), writes one PNG per step plus an `ffconcat` manifest, and
assembles the GIF with ffmpeg inside the container. Knobs: `PLAUDON_DEMO_WIDTH` (960),
`PLAUDON_DEMO_COLORS` (128), `PLAUDON_DEMO_MAX_MB` (8; the script fails over budget). Regenerate it in
the pull request that changes what the demo shows, and update the long alt text in the README (it names
the screens and the labels, for example "Local only", "Seek to this passage", "Not evaluated") if any of
those words change. The capture asserts each step reached the screen the caption says; a redesign that
renames a control will fail it by label, which is the point.

## 4. Checking contrast with the repo's own function

`plaudon_ui::theme::contrast_ratio(a: Color32, b: Color32) -> f32` (WCAG 2.1 relative luminance,
re-exported from `theme/tokens.rs`) is the function the Tokens board's numbers were computed with. It
ignores alpha, so composite any translucent colour over its background first. Use it in a unit test next
to the existing ones in `theme/tokens.rs`, which already loop over `ThemeKind::ALL` and `ColorRole::ALL`:

```rust
#[test]
fn a_badge_reads_on_its_own_tint_in_every_theme() {
    for theme in ThemeKind::ALL {
        let c = Tokens::for_theme(theme).color;
        for (text, tint) in [
            (c.status_running, c.tint_running),
            (c.status_success, c.tint_success),
            (c.status_warning, c.tint_warning),
            (c.status_danger, c.tint_danger),
            (c.status_waiting, c.tint_waiting),
            (c.status_neutral, c.tint_neutral),
        ] {
            let ratio = contrast_ratio(text, tint);
            assert!(ratio >= 4.5, "{} {ratio:.2}:1", theme.id());
        }
    }
}
```

(`tint_*` do not exist until step 1 of the work order adds them.) Normal text needs 4.5:1; large text and
UI components need 3:1. **Put the measured numbers for the key pairs in the pull request description**,
not "passes". `TOKENS.md` sections 6 and 6.1 list every pair the design relies on; the failing ones are
there too (text over the accent fill, the focus ring on the accent fill). For the four themes, the high
contrast constants must hold the same pairs; the boards give no HC values for the eight new roles.

Existing guarantees that must keep passing: primary and secondary text at 4.5:1 on canvas, surface and
raised; every `status_*` at 4.5:1 on surface; `accent_text` on `accent` at 4.5:1; `border_strong` and
`focus_ring` at 3:1 on surface; the focus ring never below 2 pt and 3 pt in high contrast.

## 5. Conventions that apply to any change

From the repo, checked:

- **Format and lint.** `rustfmt.toml`: edition 2021, `max_width = 100`. The workspace denies
  `clippy::all` and warns on `clippy::pedantic` (with the allowances in the root `Cargo.toml`).
  `scripts/fmt.sh [--check]` and `scripts/clippy.sh` run them workspace-wide; on a workstation scope
  them with `-p` and `-j 2`, workspace-wide goes to the container.
- **Docs on public items.** `crates/ui/src/lib.rs` has `#![deny(missing_docs)]`: every new `pub` item in
  `plaudon-ui` needs a doc comment or the crate does not compile. `missing_debug_implementations` warns.
- **No hard-coded colour or spacing in a widget.** "No widget in `plaudon-ui` may hard-code a colour or a
  spacing value" (`docs/EGUI_DESIGN_SYSTEM.md` section 1). Read `Tokens::get(ui.ctx())`. The four places
  that break this today are listed in `GAP_ANALYSIS.md` 2.2.
- **Copy belongs to the application.** A panel renders what `plaudon-app` gives it
  (`cloud_presentation`, `job_presentation`, `settings_presentation`, the DTOs). Do not reword a string
  from a board unless the board's string is the one the code has; the screen specs flag each case.
- **Layers.** `plaudon-ui` has no operating-system dependency and reaches the app only through the
  dispatcher (`docs/EGUI_ARCHITECTURE.md`). A screen never blocks, never reads the clock for anything it
  asserts on, virtualises anything that can be long, and renders an `ErrorEnvelope` rather than writing
  its own message.
- **Registering and testing a screen.** `docs/EGUI_ARCHITECTURE.md` section 6: register it in
  `apps/plaudon-egui/src/screens.rs::register_all`, test with kittest only, update the parity matrix in
  `docs/EGUI_MIGRATION_PLAN.md` (the `Status` column is the contract).
- **New dependencies.** A new crate or font needs a row in `docs/licensing/third-party-register.md`
  (and `NOTICE` for bundled fonts) and must pass `cargo deny` (a pre-commit check). The design adds two
  candidates: an image loader (`egui_extras` `svg` or `png`) and the Inter font.
- **Pre-commit gate.** Install once with `scripts/install-hooks.sh`. `scripts/pre-commit.sh --list`
  prints the 21 checks and the CI job each stands in for. A partial run (`--only`, `--skip`,
  `--skip-slow`) prints a banner naming what it did not run. `git commit --no-verify` works and is yours to
  take; it is not recorded, which is the reason not to. `docs/PRECOMMIT.md` section 7 lists failures that
  predate this work (including a `plaudon-ui` test, `a_jobs_deep_link_is_announced_on_the_row_it_names`);
  re-run to see the current state before blaming your change.
- **Commits.** The history is short imperative subjects, often prefixed with a task id or an area
  (`P2.6: ...`, `coverage: ...`, `clippy: ...`, `ui tests: ...`) and `Merge #N ...` for merged work. I
  found no commit-message convention document and no conventional-commits tooling, so follow the
  prefix style you see in `git log`.
- **Issues and pull requests.** The README's Contributing section is three points (controlled claims,
  where tests run, the local gate). It does not say one issue per pull request and I found no
  CONTRIBUTING file. The maintainer's own rule: every pull request references the issue it resolves with a
  closing keyword (`Closes #N`), so open the issue first if none exists. Keep each step of the work order
  its own pull request so a regenerated baseline travels with the change that moved it.
- **Claims are controlled.** README: compatibility statements use the six fixed terms and the
  compatibility page is generated; a lint rejects other terms. Device and register words in the UI come
  from the register, never from a board.

## 6. Checklist per screen

Run the generic list for every screen, then the row for that screen.

**Generic**

- [ ] Every colour, size and radius reads `Tokens` (no `Color32::from_rgb` in a widget, no literals).
- [ ] The 2 pt focus ring is visible on every interactive element, not on an accent fill, in all four
      themes (3 pt in high contrast).
- [ ] Keyboard map unchanged (the screen spec lists it); Tab reaches every control; nothing traps focus;
      Escape closes only overlays; typing wins over shortcuts.
- [ ] Status is a glyph and a word, never colour alone; a status you touched still exposes both to
      AccessKit.
- [ ] Every control has an accessible name; `a11y_contract.rs` passes with the exemption list still empty.
- [ ] Contrast of every new text-on-fill pair measured and quoted in the pull request.
- [ ] No string changed unless the screen spec says the board's copy is the app's.
- [ ] Snapshots regenerated in the container, compared clean without `UPDATE_SNAPSHOTS`, committed with
      the change; a new case added for the new state; no ids or timestamps in the frame.
- [ ] `docs/EGUI_MIGRATION_PLAN.md` parity rows and `docs/EGUI_DESIGN_SYSTEM.md` updated where the change
      made them wrong.

**By screen**

| Screen | Keep unchanged | Also check | Tests to run first |
| --- | --- | --- | --- |
| Shell chrome | `Ctrl+1` to `Ctrl+7`, roving arrow keys, skip link first in Tab order, bell name states the count in words, narrow-window notice | rail still `Role::Tab` inside `Role::Navigation`; compact rail keeps the full name; `Ctrl+B` and `Ctrl+Shift+B` | `shell_kittest.rs`, `a11y_contract.rs`, `scale_kittest.rs` |
| Library | grid is one tab stop with active descendant; Enter, Space, Shift, `Ctrl+A`; "Unknown" for unmeasured; ten columns including "Last error"; 200 ms debounce; page size 100 | virtualisation (`library_perf.rs`: under 100 of 10,000 rows laid out, ratio under 3x) | `library_screen.rs`, `library_perf.rs` |
| Library empty, import | drop path classification; **Import an audio file** raises the picker; copy exact | toast stays a polite live region and takes no focus | `library_screen.rs` |
| Recording | player keys (Space, J, K, L, comma, period; arrows 5 s; PageUp and PageDown 30 s; Home, End); Enter or double-click edits; Escape cancels; `Ctrl+Z`; deletion disclosure shows all six limits verbatim and starts on the safe button | the stacked layout below 900 pt still works; the aside width persists; position slider has name and value | `recording_detail.rs`, `player_panel.rs`, `player_interaction.rs`, `transcript_editor.rs` |
| Outputs and review | Confirm and Reject separate; seek only with real timing; regenerate always confirms; every §9.2 field reachable | "Unassigned" and "Not specified" printed as sent | `ai_workflows.rs`, `ai_review.rs`, `ai_summarize.rs` |
| Consent | opens on "Ask every time"; zero-retention refusal; unknown price says "Unknown"; Escape is Cancel; focus returns to the opener | the single approve button label follows the scope | `ai_workflows.rs`, `ai_tools.rs` |
| Jobs | no invented totals or percentages; Retry gated by the acknowledgement; Cancel discloses; deep-link focus | multi-select filters still refetch the first page of fifty | `jobs_screen.rs` |
| Devices | five sections in `SECTION_ORDER`; "Not checked" vs empty; honest words from the register | selection, route and "Check the device again" after the aside change | `devices_screen.rs`, `hardware_radio.rs` |
| Settings | Local-only toggle sends `confirmed: true` and is disabled in flight; key never shown; remove asks first | first-section assertion updated with a stated reason | `settings_screen.rs`, `settings_privacy_group.rs`, `shell_adapters.rs` |
| Onboarding | Escape does nothing; focus moves to the step heading; "Step N of 7" label on macOS ("of 6" elsewhere, where the Privacy & Security step is skipped); no network during setup, and no OS permission prompt unless the user presses Ask macOS on that step (#307) | card scrolls at the 560 pt minimum | `onboarding.rs`, `shell_kittest.rs` |
| Errors | notice is `Role::Alert`; startup states have no rail; "Not checked" never reads as "No devices" | two-control notice keeps one default | `library_screen.rs`, `settings_engines_group.rs` |
| Inbox, search, people, templates, activity, palette | palette Escape and Enter; "Search commands" name; People never auto-merges; Play-from only with timing; unread says "Unread" in words | Inbox keeps selection and keyboard model | `people_screen.rs`, `ai_templates.rs`, `activity.rs`, `shell_kittest.rs` |
| Brand | assets pinned per theme; clear space and minimums | `packaging.rs` (1024 square master, `.ico` sizes) | `packaging.rs` |

## 7. Order of work and review

Follow the order in `GAP_ANALYSIS.md` section 6: refresh baselines, tokens, badge and glyph, buttons and
tabs, progress and notice and toast, chrome, then screens, then brand. One pull request per step. In
each, state which board it implements, list the baselines it regenerated, quote the contrast numbers
for anything new, and say which of the board's disagreements (`GAP_ANALYSIS.md` section 4) you
resolved and how. A reviewer should be able to compare the rendered result with the matching PNG in
`screens/` side by side; the light PNGs include the board's own egui notes in the bottom 280 pt, so crop
them for the comparison.
