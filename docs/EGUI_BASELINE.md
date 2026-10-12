# egui migration baseline: the Tauri 2 + React desktop app as it is today

Baseline measurements of the existing `apps/desktop` frontend and shell, recorded before EPIC O (#197)
replaces them with egui + eframe. Task O1.1 (#199).

Every row is labelled **MEASURED** (with the command that produced it) or **UNMEASURED** (with the
reason). Nothing here is estimated and presented as a measurement. This matters because
`docs/benchmarks/README.md` states that no performance target may be quoted before a baseline exists,
and `docs/spikes/A2.5-compatibility-register.md` risk R13 makes an unmeasured claim a test failure.

**Measured against:** the integration worktree holding every merged EPIC branch, at commit
`4230963` ("Merge epic-j-export-archive"), on macOS arm64. Measurements were taken in a throwaway
copy of `apps/desktop`; the source tree was not modified.

---

## 1. Headline table

| Metric | Value | Status |
| --- | --- | --- |
| Frontend TS/TSX/CSS lines under `apps/desktop/src` | 26,566 across 156 files | MEASURED |
| &nbsp;&nbsp;of which test code | 7,532 across 53 files | MEASURED |
| &nbsp;&nbsp;of which CSS | 2,504 across 14 files | MEASURED |
| Frontend source bytes (`.ts`/`.tsx`/`.css`) | 918,830 B | MEASURED |
| Frontend file counts | 97 `.tsx`, 45 `.ts`, 14 `.css` | MEASURED |
| Exported React components in `.tsx` | 64 | MEASURED |
| Custom hooks (`export … use[A-Z]`) | 14 | MEASURED |
| React built-in hook callsites | 170 | MEASURED |
| `invoke(` callsites / `invokeCommand(` callsites | 47 / 11 | MEASURED |
| IPC commands in the TypeScript contract | 51 | MEASURED |
| `#[tauri::command]` functions in Rust | 54 | MEASURED |
| UI event kinds | 19 | MEASURED |
| Vitest: files / static `it(`+`test(` / collected | 53 / 422 / 446 | MEASURED |
| Vitest result on this tree | 331 pass, 115 fail, 16.21 s | MEASURED (pre-existing failures) |
| Playwright: spec files / tests | 7 / 31 | MEASURED (static) |
| npm `dependencies` / `devDependencies` | 5 / 18 | MEASURED |
| npm transitive packages | 247 resolved in lockfile, 187 installed | MEASURED |
| `node_modules` size | 156 MB | MEASURED (in a throwaway install) |
| Production bundle, excluding sourcemaps | 449,672 B | MEASURED (after a 2-line local syntax fix) |
| &nbsp;&nbsp;main JS chunk | 398,714 B (122.33 kB gzip) | MEASURED |
| &nbsp;&nbsp;CSS | 32,792 B (4.56 kB gzip) | MEASURED |
| &nbsp;&nbsp;Tauri webview shim chunk | 16,483 B | MEASURED |
| `dist/` including sourcemaps (`build.sourcemap: true`) | 2.3 MB | MEASURED |
| `apps/desktop/src-tauri` Rust LOC / files | 26,366 / 45 (21,966 src + 4,370 tests) | MEASURED |
| `src-tauri` Cargo dependencies | 31 deps + 1 build-dep + 8 dev-deps | MEASURED |
| `Cargo.lock` packages (whole workspace) | 702 | MEASURED |
| Workspace crates under `crates/` | 26 | MEASURED |
| Repository tracked files | 1,494 | MEASURED |
| Repository Rust LOC | 214,066 | MEASURED |
| **Cold start / time to first frame** | — | **UNMEASURED** (needs a packaged build; see §7) |
| **Idle CPU, frame time, memory** | — | **UNMEASURED** (no harness exists; see §7) |
| LOC by language via `tokei`/`scc`/`cloc` | — | UNMEASURED (none installed) |

---

## 2. Source size

```
find src -type f \( -name '*.ts' -o -name '*.tsx' -o -name '*.css' \) | xargs wc -l
```

26,566 lines over 156 files; 19,034 non-test lines. Largest files:

| File | LOC |
| --- | --- |
| `src/ipc/contract.ts` | 1,742 |
| `src/testing/fakeBackend.ts` | 1,649 |
| `src/features/recording/TranscriptPane.tsx` | 576 |
| `src/features/recording/RecordingDetailScreen.tsx` | 529 |
| `src/features/devices/DeviceDetail.tsx` | 499 |
| `src/features/settings/BackupRestoreSection.tsx` | 483 |
| `src/features/recording/AudioPlayer.tsx` | 453 |
| `src/testing/fakeDevices.ts` | 440 |
| `src/features/library/libraryQuery.ts` | 404 |
| `src/features/recording/recording.css` | 397 |
| `src/features/jobs/jobPresentation.ts` | 394 |
| `src/features/cloud/ModelPicker.tsx` | 359 |

The six heaviest feature folders are `recording`, `devices`, `settings`, `library`, `cloud`, `jobs`.

Five stale compiled `.js` files sit next to their TypeScript sources (`src/app/App.js`,
`src/app/router.js`, `src/app/router.test.js`, `src/features/settings/SettingsScreen.js`,
`src/testing/fakeBackend.js`). `git ls-files apps/desktop/src | grep '\.js$'` returns nothing, so they
are untracked local `tsc` output that `.gitignore:17-23` already excludes; they are excluded from every
figure above.

## 3. Bundle size

The pristine tree **does not build**: `npm run build` (`tsc -b && vite build`) fails with three
TypeScript syntax errors and `npx vite build` fails in esbuild. The cause is two silent merge-resolution
defects (no conflict markers): `src/ipc/contract.ts` is missing the closing brace of
`interface ListSourceArtifactsResponse`, and `src/features/search/SearchScreen.tsx` has lost the
`{!submitted && !loading && (` guard and its `)}` closer. These are integration defects in the merged
tree, not properties of the UI, and they were reported to the integration owner. The measurement below
was taken after restoring exactly those two constructs in a throwaway copy.

```
vite v7.3.7 building client environment for production...
✓ 116 modules transformed.
dist/index.html                    0.39 kB │ gzip:   0.26 kB
dist/assets/index-BIRSeVyP.css    32.79 kB │ gzip:   4.56 kB
dist/assets/index-DrvFFXnI.js      1.29 kB │ gzip:   0.51 kB │ map:     9.01 kB
dist/assets/webview-do-AzV_G.js   16.48 kB │ gzip:   3.59 kB │ map:   154.53 kB
dist/assets/index-DvrHnuw0.js    398.71 kB │ gzip: 122.33 kB │ map: 1,722.79 kB
✓ built in 495ms
```

Total 449,672 B excluding sourcemaps (~131 kB gzipped); 2.3 MB including them. This is the web payload
only. It does not include the WebKit/WebView2 runtime that the Tauri shell loads at startup, which is
the dominant memory and startup cost of the current architecture and which the egui build removes
entirely.

## 4. Node toolchain footprint

`apps/desktop/package.json`: 5 runtime dependencies
(`@tanstack/react-virtual ^3.13.12`, `@tauri-apps/api ^2.9.0`, `@tauri-apps/plugin-dialog ^2.4.0`,
`react ^19.2.0`, `react-dom ^19.2.0`) and 18 devDependencies
(`@axe-core/playwright`, `@playwright/test`, `@tauri-apps/cli`, `@testing-library/{dom,jest-dom,react,user-event}`,
`@types/{node,react,react-dom}`, `@vitejs/plugin-react`, `ajv`, `ajv-formats`, `axe-core`, `jsdom`,
`typescript`, `vite`, `vitest`).

`package-lock.json` resolves 247 packages; `npm ci --offline` installs 187 and produces a 156 MB
`node_modules` (not installed in the repository). Eight npm scripts exist: `dev`, `build`, `preview`,
`typecheck`, `test`, `test:watch`, `e2e`, `tauri`. All of this disappears at cutover (O2.5, #209).

## 5. Rust shell footprint

`apps/desktop/src-tauri` is 26,366 Rust LOC over 45 files: 21,966 in `src/` and 4,370 in `tests/`.
The largest file is `src/validate.rs` (1,525 LOC). Resolved Tauri-side versions (`cargo tree --offline`):
`tauri 2.12.3`, `tauri-plugin-dialog 2.8.2`, `tauri-build 2.7.2`. The whole workspace resolves 702
packages. Only three dependencies are Tauri-specific; the rest of the crate is reusable (see
`docs/EGUI_MIGRATION_PLAN.md` §3).

## 6. Test inventory — the parity oracle

All UI tests move to `egui_kittest`. The legacy suites stay only as a parity oracle until cutover
(O2.5, #209), then are deleted with the React frontend. No new vitest or Playwright tests may be
written. `docs/EGUI_MIGRATION_PLAN.md` §9 maps each of these files to its kittest replacement.

No real Tauri runtime is used by **any** vitest or Playwright test: everything runs against
`src/testing/fakeBackend.ts`, an in-memory `Transport`. That is why the suites are a usable behavioural
oracle for the egui UI — they describe UI behaviour, not IPC plumbing.

**No snapshot or screenshot assertion exists anywhere in the current suite** (`toMatchSnapshot`,
`toHaveScreenshot`: no occurrences). Visual regression is new work in EPIC O, not a port.

### 6.1 Vitest — 53 files, 422 static `it(`/`test(` declarations, 446 collected

Runner: vitest 3.2.4, `jsdom`, `setupFiles vitest.setup.ts`, include `src/**/*.test.ts|tsx`.
Assertion style: Testing Library role/text queries, fake-backend call-log assertions, and `axe-core`
via `src/testing/axe.ts` (`wcag2a`, `wcag2aa`, `wcag21a`, `wcag21aa`, `wcag22aa`, `best-practice`;
`target-size`, `region`, `scrollable-region-focusable` disabled because jsdom has no layout).

| File | Tests | Kind |
| --- | ---: | --- |
| `app/App.test.tsx` | 8 | shell, onboarding gate, activation routing, min-width notice, axe |
| `app/AppNav.test.tsx` | 5 | tablist, roving tabindex, names-not-icons, axe |
| `app/router.test.ts` | 5 | pure route model |
| `app/shortcuts.test.ts` | 5 | pure shortcut map |
| `app/tauriCapabilities.test.ts` | 8 | reads `tauri.conf.json` + capability JSON — **Tauri config, not UI** |
| `design/theme.test.ts` | 6 | theme reducer, OS appearance watcher |
| `design/tokens-only.test.ts` | 3 (2 `it.each`) | lints every CSS file for literal colours/spacing |
| `design/tokens.test.ts` | 8 | WCAG 2.2 AA contrast of every token in every theme |
| `ipc/client.test.ts` | 6 | transport envelope handling |
| `ipc/contract.schema.test.ts` | 9 | validates fake-backend output against `schemas/commands/**` |
| `ipc/eventBus.test.ts` | 5 | seq-gap detection, monotonicity property |
| `ipc/useCommand.test.tsx` | 4 | query/mutation hooks |
| `features/recording/AudioPlayer.test.tsx` | 12 | playback, peaks, scrubber, keyboard, indeterminate duration, axe |
| `features/recording/playerState.test.ts` | 13 | pure transport + peak-bucket maths |
| `features/recording/TranscriptPane.test.tsx` | 14 | segments, search, copy, edit/undo, conflict, paging, axe |
| `features/recording/transcriptEditing.test.ts` | 12 | pure draft/undo/search/copy model |
| `features/recording/RecordingDetailScreen.test.tsx` | 17 | provenance, player wiring, revisions, export, axe |
| `features/recording/RevisionSwitcher.test.tsx` | 3 | revision list |
| `features/recording/StageRerunPanel.test.tsx` | 8 | estimate, override, consent, budget, stage rerun, axe |
| `features/recording/DeleteRecordingDialog.test.tsx` | 7 | deletion disclosure (component is orphaned, see §8) |
| `features/recording/ExportMenu.test.tsx` | 5 | export formats and SRT/VTT gating (orphaned) |
| `features/library/LibraryScreen.test.tsx` | 11 | list, filters, paging, error envelope, 100k rows |
| `features/library/RecordingTable.test.tsx` | 10 | ARIA grid, roving tabindex, selection, virtualization, axe |
| `features/library/libraryQuery.test.ts` | 18 | pure request assembly, page accumulation, row presentation |
| `features/library/InboxScreen.test.tsx` | 5 | two-branch inbox union |
| `features/jobs/JobsScreen.test.tsx` | 20 | every job state, cost acknowledgement, capacity, empty/error |
| `features/jobs/jobPresentation.test.ts` | 14 | pure state/label/glyph/action mapping |
| `features/activity/ActivityScreen.test.tsx` | 7 | categories, suppression, paging, no transcript text |
| `features/activity/ActivityBell.test.tsx` | 2 | unread count |
| `features/settings/SettingsScreen.test.tsx` | 3 | landmarks, navigation |
| `features/settings/AppearanceSection.test.tsx` | 1 | appearance/contrast application |
| `features/settings/PrivacySection.test.tsx` | 2 | local-only confirmation |
| `features/settings/AiKeySection.test.tsx` | 2 | key handover, destructive removal |
| `features/settings/CloudBudgetSection.test.tsx` | 5 | finalized/estimated/unknown spend, axe |
| `features/settings/CloudConsentSection.test.tsx` | 5 | grants and revocation, axe |
| `features/settings/NotificationSettingsSection.test.tsx` | 4 | categories, denied permission, unsupported platform |
| `features/settings/BackupRestoreSection.test.tsx` | 10 | backup/restore disclosure and conflicts (orphaned) |
| `features/devices/DevicesScreen.test.tsx` | 14 | five sections, denied/unavailable radio, honest labels |
| `features/devices/DeviceDetail.test.tsx` | 24 | facts, identity/authorization, transfer states, ETA honesty |
| `features/devices/SetupWizard.test.tsx` | 5 | three options |
| `features/devices/copy.test.ts` | 4 (1 `it.each`) | banned-copy lint over device sources |
| `features/sources/ConnectorPanel.test.tsx` | 11 | next_action controls, verbatim Rust copy, axe |
| `features/sources/SourcesScreen.test.tsx` | 9 | probe, sign-in, list, import, disconnect |
| `features/cloud/ModelPicker.test.tsx` | 14 | model facts, unknown price, verification, budget stop, axe |
| `features/cloud/CloudConsentDialog.test.tsx` | 7 | §4.4 disclosure, scopes, fail-closed, axe |
| `features/cloud/cloudPresentation.test.ts` | 11 | pure cost/money/model-fact presentation |
| `features/import/useImport.test.tsx` | 9 | path handling, picker, batch summary, hostile names |
| `features/import/ImportDropTarget.test.tsx` | 5 | drag/drop paths, affordance, axe |
| `features/import/DuplicateDialog.test.tsx` | 5 | three choices, content-hash wording |
| `features/onboarding/OnboardingFlow.test.tsx` | 8 | steps, focus, skipping, key handling |
| `features/onboarding/onboardingMachine.test.ts` | 4 | pure step machine |
| `features/search/SearchResults.test.tsx` | 4 | grouping, seek affordance |
| `features/speakers/SpeakerControls.test.tsx` | 6 | rename/confirm/reset (orphaned) |

Two files are not UI tests and must be re-homed rather than ported:
`app/tauriCapabilities.test.ts` (asserts `tauri.conf.json` and the capability manifest — it dies with
Tauri) and `ipc/contract.schema.test.ts` (validates JSON schemas — the Rust-side
`apps/desktop/src-tauri/tests/schema_conformance.rs` already covers the same schemas).

Untested components today: `SearchScreen`, `SearchField`, `TemplatesScreen`, `DeviceList`,
`TransferList`, `RecordingsList`, and the shared primitives (`Dialog`, `Button`, `Field`, `Progress`,
`StatusBadge`) which are only covered indirectly.

### 6.2 Playwright — 7 specs, 31 tests

Harness: `playwright.config.ts`, `testDir ./e2e`, Desktop Chrome 1440×900, `webServer: npm run dev` at
`http://127.0.0.1:5273`, driven against the real React bundle with `?harness=1` (the same in-memory
backend). No Tauri process. `e2e/fixtures.ts` adds an egress log that asserts no external request,
`auditPage` (`@axe-core/playwright`, `wcag22aa`) and `tabOrder`.

| Spec | Tests | Titles |
| --- | ---: | --- |
| `e2e/shell.spec.ts` | 8 | launches and makes no external request; reaches every destination with the keyboard only; jumps to a destination with the platform shortcut; shows the keyboard reference and restores focus; follows the OS appearance and keeps a visible focus ring; stays usable below the minimum desktop width; keeps transcript and list text selectable; no accessibility violations on every destination |
| `e2e/library-import.spec.ts` | 7 | renders required columns and virtualizes a large library (5,000 rows); searches and filters without stalling (400 rows); corrective actions when empty; refetches after a dropped library event; imports files chosen in the OS picker; per-file outcomes for a mixed batch; asks before importing a duplicate |
| `e2e/review.spec.ts` | 4 | plays, seeks and reports position as text using the keyboard only; honest indeterminate state when the duration is unknown; edits create a new revision and keep the original retrievable; non-destructive resolution when the base revision changed |
| `e2e/jobs-activity.spec.ts` | 5 | every job state distinct and never by colour alone; explicit cost acknowledgement before retrying an uncertain cloud request; empty state with a corrective action; records completions when the OS denies notifications; announces unread activity in the header |
| `e2e/onboarding.spec.ts` | 3 | completes with every account step skipped and no egress; never offers a model download in local-only mode; keeps the API key out of the DOM |
| `e2e/devices.spec.ts` | 2 | wizard and list: scan, five sections, honest labels; a denied radio is reported as denied with a way forward |
| `e2e/keyboard-flow.spec.ts` | 2 | the manual file flow is completable with the keyboard only; no interactive element is unreachable by keyboard on the main screens |

### 6.3 Current test state on this tree

`npx vitest run` on the merged tree: **17 files failed, 36 passed (53); 115 tests failed, 331 passed
(446); 15 errors; 16.21 s.** These failures pre-date this work and come from the integration merge
(notably `src/app/App.tsx` renders `RecordingDetailScreen` without importing it, and
`src/app/router.ts` lost its `case "inbox":` label). They bound how far the legacy suite can serve as a
parity oracle until the integration owner repairs them.

Playwright was not executed: it requires downloading a Chromium build, which is outside this task's
resource policy. Its counts above are static.

## 7. What is not measured, and why

| Metric | Why not measured | How to measure it |
| --- | --- | --- |
| Cold start / time to first frame | No packaged build exists; cargo builds were out of scope for this task; `npm run build` fails on the pristine tree. A web-bundle timing would measure webview paint, not app launch. | A MARS (`devshell@192.168.0.100`, Docker) run that builds both shells in release and times process spawn → first frame and → first command response. |
| Idle CPU, frame time, memory, binary size | No benchmark harness for the UI exists anywhere in the repository (`docs/benchmarks/` covers STT and extraction only; a repo-wide docs grep for `fps`, `frame budget`, `startup`, `cold start`, `idle CPU`, `memory budget` found nothing). | Same MARS run, with the egui build instrumented through `egui::Context` frame timing. |
| Playwright wall time | Browsers never downloaded. | MARS Docker runner. |
| LOC by language | `tokei`, `scc`, `cloc` are not installed and installing them was out of scope. | Optional; `wc -l` over `git ls-files` is in the headline table. |

**No UI performance budget exists in the repository today.** EPIC O must therefore *establish* budgets
from a first measured run rather than inherit them, and no O-series document may assert a target before
that run exists.

## 8. Qualitative baseline that the egui UI must match

These are the non-numeric facts the egui build inherits as acceptance criteria.

- **Six top-level destinations**, `Mod+1`…`Mod+6`: Library, Inbox, Jobs, Templates, Settings, Devices.
  `docs/accessibility/manual-screen-reader-script.md` still describes five (it predates Devices) and
  must be updated with the egui work.
- **Window**: 1280×820 default, 880×560 minimum (`tauri.conf.json`); the UI shows a `role=status`
  notice below 880 px wide and switches to a compact layout below 1040 px, and never stacks
  mobile-style.
- **Accessibility**: WCAG 2.2 AA, verified manually with VoiceOver and Narrator against
  `docs/accessibility/manual-screen-reader-script.md`. "A step that cannot be completed with the
  keyboard alone is a release blocker." Token contrast is unit-tested at 4.5:1 for text and 3:1 for
  borders and the focus ring, in four themes (light, dark, hc-light, hc-dark).
- **Honesty rules enforced by tests**: an unknown duration is "Unknown", never `0:00`; an unknown cost
  is "unknown", never zero; an estimate is never rendered as a finalized amount; status is never
  communicated by colour alone; a transcript with no alignment offers no timestamp chip and no seek;
  ETA is labelled an estimate; a delta listing is never presented as a complete archive.
- **Scale targets exercised by existing tests**: 5,000-row library in Playwright, 100,000-row library
  and 100,000-segment transcript in vitest, 1,000-recording list in the screen-reader script.
- **No persisted UI state**: there is no `localStorage`/`sessionStorage`/`indexedDB` use anywhere;
  appearance, contrast, local-only mode, default engines, notification settings and onboarding
  completion live in the Rust `Preferences`. Library sort/filter state, the selected tab and window
  geometry are **not** persisted today — the egui shell is expected to improve on this (O1.3, #201).
- **No i18n**: all strings are hard-coded English; dates use `Intl.DateTimeFormat(undefined, …)`.
- **No icon set**: text glyphs (`▶ ❙❙ ↩ ↪ ✓ ✕ ⟳ ◷ ▲▼↕`) and single letters in the nav rail.
- **No command palette** exists today; `ShortcutsDialog` (`?`) is the only discoverability surface.

Eight components are exported and unit-tested but mounted nowhere in the running app:
`ExportMenu`, `DeleteRecordingDialog`, `BackupRestoreSection`, `SpeakerManagementPanel`,
`SpeakerControls` (plus `useSpeakerCommands`). Four commands have no caller at all
(`source_add_folder`, `source_sync`, `source_reconcile`, `list_source_artifacts`). The egui UI is
expected to *mount* these flows rather than reproduce the gap; they are tracked in the parity matrix
as "feature exists in Rust, no UI today".
