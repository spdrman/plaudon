# Manual screen-reader verification script (VoiceOver / Narrator)

Spec: §5.3 (WCAG 2.2 AA target, VoiceOver and Narrator testing), §18.7 #18, §22(7). Parity row S17
of `docs/EGUI_MIGRATION_PLAN.md` §4.1.

This script covers the egui shell, which after the EPIC O cutover (ADR-0024) is the only desktop
application in this repository. It replaced a script of the same name written for the React
frontend, keeping its shape so verdicts recorded before and after the cutover remain comparable.
Where the native shell differs from the web one — there is no document, no landmarks derived from
HTML, no browser focus ring — this document says so rather than reusing the web wording.

There is no automated axe equivalent to run alongside it: axe needs a DOM. What automation does
cover is listed in §0, and the gap between the two columns is exactly what a human must answer.

> **This script has not been executed.** No VoiceOver and no Narrator run exists for the egui
> shell. The environment this document was written in has no screen reader, no audio output and no
> interactive session, and §4 of `docs/EGUI_TESTING.md` is explicit that a result which was not
> produced where it is claimed to have been produced is not a result. Every cell of the result
> table below is therefore empty, and an empty cell means **not run**, not "pass".

## 0. What is automated, and what only a human can answer

| Question | Answered by |
| --- | --- |
| Does every operable control have a role and a non-empty accessible name, on every destination, on the recording detail, on search, on the first-run flow and in the command palette? | **Automated** — `apps/plaudon-egui/tests/a11y_contract.rs`, through `plaudon_ui_testkit::a11y::assert_every_control_is_named` |
| Are the landmarks present (`Navigation`, `Main`, `Toolbar`, `Status`) and is the skip link the first stop? | **Automated** — `crates/ui/tests/shell_kittest.rs` |
| Is every interactive element reachable by `Tab`, with no focus trap? | **Automated** — `a11y::focus_order`, same file |
| Do the same controls exist, in the same places, at 1.0×, 1.5× and 2.0×? | **Automated** — `crates/ui/tests/scale_kittest.rs` |
| Does the colour contrast of the 20 token roles meet AA in all four themes? | **Automated** — the token contrast unit tests (O1.3) |
| Do the words a screen reader *says* make sense, in the right order, at the right moment? | **This script. Nothing automated can answer it.** |
| Is a live-region announcement actually spoken, and does it interrupt or politely queue? | **This script.** |
| Can a user who cannot see the screen complete each flow with the keyboard alone? | **This script.** |
| Is the virtualized library list navigable with the screen reader's own cursor (not just `Tab`)? | **This script.** |

A step in this script that cannot be completed with the keyboard alone is a release blocker
(§19 Phase 2 exit gate), exactly as in the web script.

## 1. Environments

| Platform | Screen reader | Start | Stop |
|---|---|---|---|
| macOS 14+ | VoiceOver | `Cmd+F5` | `Cmd+F5` |
| Windows 11 | Narrator | `Ctrl+Win+Enter` | `Ctrl+Win+Enter` |

Keys used below: VoiceOver `VO = Ctrl+Option`; Narrator key = `Caps Lock`. "Next item" is
`VO+Right` / `Narrator+Right`. "Mod" is `Cmd` on macOS and `Ctrl` on Windows, which is what the
shell renders in its own menus (it reports the host operating system, so the chords shown match
the chords that work).

Build under test: a packaged `plaudon-egui` bundle (`scripts/package-macos-egui.sh`,
`scripts/package-windows-egui.ps1`). A `cargo run` build is acceptable for a smoke pass but not
for an evidence run: on Windows the Narrator bridge needs the manifest the packaged binary
carries.

Record for the evidence bundle: tester, OS build, screen-reader version, app version, pass/fail
per step, and the verbatim announcement wherever the step says "verbatim".

## 2. Preparation

1. Start the app over a library containing at least 1,000 recordings, at least one of them with
   audio and a transcript, and at least one job in each of running, waiting and failed. (A fresh
   profile is used for §3 only.)
2. Set the display to 100% scaling for the first pass. Repeat §4.1 and §9 at 150% and 200% —
   those are the scales `crates/ui/tests/scale_kittest.rs` covers for layout, and the human
   question there is whether the focus indicator is still visible, not whether the layout moved.
3. Turn the screen reader on **before** the app, so the accessibility tree is built with the
   adapter already listening.

## 3. First run (onboarding)

The first-run flow gates every destination until it is completed; it is the first thing a new user
meets.

1. Launch over an empty profile. **Expect:** the window title "Plaudon" is announced, and the
   first-run flow is announced as the content of the main region — not an empty window.
2. `Tab` through the flow. **Expect:** every choice (default speech-to-text engine, default text
   engine, Local-only Mode, notification categories) announces its own name and its current state;
   no control is announced only as "button".
3. On the Local-only Mode control. **Expect (verbatim):** the announcement states what the mode
   *does*, not merely that it is on or off. Local-only Mode is a security guarantee; a user who
   cannot see the explanatory text must still hear it.
4. Complete the flow with the keyboard alone. **Expect:** the shell appears, focus lands somewhere
   announced, and the first destination (Library) is reported.

## 4. Shell and navigation

### 4.1 Landmarks and the skip link

1. **Expect:** the first focus stop in the window is "Skip to main content".
2. Press `Enter` on it. **Expect:** focus moves to the main region, which announces itself as
   "Main content", and the current destination's content is what the reader then walks.
3. Walk the window with the reader's own cursor (`VO+Right` / `Narrator+Right`). **Expect:** a
   navigation landmark announced as "Primary", a toolbar, the main region and a status line are
   all reachable, in that order.
4. **Expect:** the status line is announced as a status, with the text "Status: Ready" (the role is
   `Status` with a polite live region, so it must not interrupt).

### 4.2 The destination rail

1. `Tab` to the navigation rail. **Expect:** it is announced as a tab list of six tabs — Library,
   Inbox, Jobs, Templates, Settings, Devices — with the selected one reported as selected and each
   one reported as "n of 6".
2. `Down`/`Up`, then `Home`/`End` inside the rail. **Expect:** each destination is announced on
   arrival and the main region content changes; the rail is **one** tab stop, not six.
3. Collapse the rail with `Mod+B`. **Expect:** the destinations still announce their full names.
   The collapsed rail shows a single letter per destination; that letter is decoration and must
   never be what is announced.
4. **Expect:** a destination whose screen is not available in this build announces
   "<name> (not yet available)" rather than being silently identical to the others.

### 4.3 Keyboard-only reach for every destination

Each row must be reachable **without the mouse and without the rail**, which is what a user with
the rail collapsed or a screen reader in forms mode depends on.

| Destination | Chord | Expected announcement on arrival |
| --- | --- | --- |
| Library | `Mod+1` | "Library" |
| Inbox | `Mod+2` | "Inbox" |
| Jobs | `Mod+3` | "Jobs" |
| Templates | `Mod+4` | "Templates" |
| Settings | `Mod+5` | "Settings" |
| Devices | `Mod+6` | "Devices" |
| Search (inside Library) | `Mod+Shift+F` | "Search" |

**Expect** for every row: the destination changes, the rail's selected tab follows, and the new
content is announced without the user having to go looking for it.

### 4.4 Panes and dividers

1. `Tab` to the sidebar divider. **Expect:** it is announced as a splitter named "Resize the
   sidebar", with a value.
2. Operate `Left`/`Right` on it. **Expect:** each step announces the new value; the layout changes
   by the same amount in both directions.
3. On a destination that has a side pane, repeat with "Resize the side pane" (`Mod+Shift+B` shows
   and hides it). **Expect:** hiding the pane removes it from the reading order entirely; it must
   not remain as an empty announced region.

### 4.5 Appearance and contrast

1. Toggle the OS appearance (macOS: System Settings → Appearance; Windows: Settings → Colors)
   while the app is open. **Expect:** the theme follows immediately, focus is not lost, the focus
   indicator stays visible.
2. Enable the OS high-contrast setting. **Expect:** the app switches to the high-contrast theme and
   the focus indicator is at least as visible as before.
3. Run `Appearance: light`, `Appearance: dark`, `Contrast: increased` from the command palette.
   **Expect:** each one announces the change; none of them moves focus.

## 5. Library

With at least 1,000 recordings loaded.

1. `Tab` into the table. **Expect:** it is announced as a grid named "Recordings", reporting the
   **total** row count, not the number of rendered rows. (The list is virtualized; a count of 50
   here is a bug.)
2. Arrow `Down` ten times. **Expect:** each row announces title, date, duration — or "Unknown",
   never "0:00" — source, import status and transcription status, all as text; the row position
   reflects the full set.
3. `End`, then `Home`. **Expect:** focus reaches the last and the first loaded row, the list
   scrolls, and both are announced.
4. `Space` on three rows. **Expect:** the selection state of each row is announced, and the polite
   live region reports the new selection count.
5. `Mod+A`. **Expect:** "all N selected" is announced once, not once per row.
6. `Shift+Down`. **Expect:** the extended range is announced as a count, not as silence.
7. `Tab` to each column header. **Expect:** each is announced as a column header with its name, and
   the sorted one reports its direction. Activate one. **Expect:** the new sort order is announced
   and the table is re-read from the top.
8. Type in the search field. **Expect:** the result count is announced politely and focus stays in
   the field.
9. Choose an import-status filter. **Expect:** the control announces its name and its chosen value,
   and the result count follows.
10. `Enter` on a row. **Expect:** the recording detail opens and focus lands on the recording's
    heading.

### 5.1 Import

1. From Library, run "Import audio…" (`Mod+O`) with the keyboard. **Expect:** the OS file picker
   opens, is fully operable, and cancelling returns focus to where it was and announces
   "Import cancelled" — not silence.
2. Import a file that duplicates an existing recording. **Expect:** the duplicate prompt is
   announced as a dialog with its title, all three choices are reachable, `Esc` cancels, and focus
   returns to the invoking control.
3. Import a file the application rejects. **Expect:** the rejection is announced with its reason,
   and the reason contains no filesystem path (§15.4) — if a path is spoken, that is a security
   defect, not a wording defect.

## 6. Inbox

1. `Mod+2`. **Expect:** the destination is announced, and either the list of unprocessed
   recordings or an empty state that says *why* it is empty.
2. Walk the list with the reader cursor. **Expect:** every row announces the same fields as the
   library's, and every action offered on a row announces what it will do.

## 7. Jobs

1. `Mod+3` with at least one job in each of running, waiting, failed and `Cloud outcome unknown`.
   **Expect:** every state is announced as **text**, never by colour alone.
2. On a running job. **Expect:** progress is announced as a progress indicator; a job whose ratio
   is unknown is announced as indeterminate rather than as "0 percent".
3. On the `Cloud outcome unknown` job, attempt a retry. **Expect:** the cost acknowledgement is
   announced, and Retry stays unavailable until it is confirmed. A retry that can be triggered
   without hearing the acknowledgement is a release blocker: it is a chargeable call.
4. Open a job's detail. **Expect:** the failure reason, the stage and the engine are all announced
   as text.

## 8. Templates

1. `Mod+4`. **Expect:** the destination is announced, and the screen states plainly what it can and
   cannot do in this build. An empty screen with no explanation fails this step.

## 9. Settings, activity and sources

1. `Mod+5`. **Expect:** the settings sections are announced as a list the reader can walk, and each
   section announces its own name.
2. In each section, `Tab` through every control. **Expect:** every control announces a name and its
   current value; no toggle announces only "on".
3. On the engine and model controls. **Expect:** a model that cannot be selected announces *why*
   (unverified capability, Local-only Mode, missing key) rather than being silently unavailable.
4. On the API-key controls. **Expect:** the key is never announced, in part or in full; the control
   announces only whether a key is stored.
5. Remove the stored API key. **Expect:** an alert is announced, states the consequence, traps
   focus, and returns focus on cancel.
6. Activity history (inside Settings). **Expect:** each entry announces what happened and when, and
   a notification that was suppressed announces its suppression reason.
7. Sources (inside Settings). **Expect:** each watched folder and each PLAUD connection announces
   its name and its state; a folder path that is announced must be one the user chose themselves.
8. Backup and restore. **Expect:** every destructive step announces the consequence before it is
   possible to confirm it.

## 10. Devices

1. `Mod+6`. **Expect:** the destination is announced; with no radio available, the refusal is
   announced as text and says what is missing.
2. With a device present, walk the list. **Expect:** each device announces its name and its state,
   and a transfer announces progress as a progress indicator.

## 11. Recording detail

Open a recording with audio and a transcript (§5 step 10).

### 11.1 Header and provenance

1. **Expect:** focus lands on the recording heading and the title is announced.
2. Walk the header. **Expect:** the transcription status badge and the privacy badge are announced
   as text — "Local only" or "Cloud eligible" — and the privacy badge states what is *enforced*.
3. Walk the provenance table. **Expect:** source, original file name and content hash are announced
   as name/value pairs; a recording with no capture instant announces "Unknown" and not a date.

### 11.2 Player

1. `Tab` to the transport. **Expect:** play/pause announces its state change.
2. **Expect:** the scrubber is announced as a slider with a time value ("42 seconds of 12 minutes
   5 seconds"), not as a bare number.
3. Operate `Left`/`Right`, −10 s, +10 s and the speed control. **Expect:** each change is announced
   and position and duration are readable as text.
4. Open a recording whose duration was never measured. **Expect:** "Duration unknown" is announced
   and **no slider exists** — an invented timeline is a defect.
5. On a build with no audio device (or with the device removed mid-session). **Expect:** the panel
   announces the refusal as text and no transport control pretends to work.

### 11.3 Transcript

1. Arrow through the segments. **Expect:** each announces its timestamp chip when it has one, its
   text, and any flag such as "low confidence" or "possible hallucination".
2. On a transcript with no alignment. **Expect:** no timestamp is announced and no seek control
   exists.
3. Enter edit mode on a segment, change the text, `Mod+Z`. **Expect:** edit mode, the change and
   the undo are each announced, and the editable field has a name tied to its segment.
4. Save. **Expect:** the new revision is announced; the revision switcher distinguishes
   "Original (machine)" from the edited revision.
5. Force a conflict (edit the same recording in a second window and save both). **Expect:** the
   conflict is announced with both resolution choices, and the editor still contains the user's
   text.

### 11.4 Outputs column

1. `Tab` to the tab strip. **Expect:** it is announced as tabs, each with its name, and the
   selected one as selected.
2. On the Summarize tab with Local-only Mode on. **Expect:** the refusal is announced *before* any
   control that would start a cloud run is reachable.
3. On the Review tab with no claim. **Expect:** the empty state is announced and says why it is
   empty.
4. On the Export tab. **Expect:** a format that needs timestamps announces that it is unavailable
   **and why**, on a transcript without alignment.

## 11A. People and voice profiles

New with P2.1 (#244), extended by P2.6 (#272); the rules are
`VOICE_PROFILES_SPEC` §9A and VP-096, and ADR-0082's wording rules apply to
every line read aloud here.

1. Press `Mod+7`, or arrow to **People** in the rail. The destination
   announces its name and the sentence "One list of people for the whole
   library."
2. Tab to **Search people**, type part of a name, and confirm the roster list
   changes. Every row announces the person's name, their aliases if they have
   any, and how many recordings they are assigned in — never "0 recordings"
   where the honest answer is "Not assigned in any recording yet".
3. Open **Edit <name>…**. The dialog must announce how many recordings show
   this name *before* the save control is reachable, and that renaming changes
   no assignment. Escape must close it with nothing saved.
4. Open **Merge <name> into…** and choose a person. The confirmation must stay
   unavailable until the counts arrive, and must say that a merge is not in the
   transcript's undo stack.
5. Where entries share a name, the **Possible duplicates** group must read as a
   suggestion: "Nothing is merged automatically — merging is always your
   decision."
6. With biometric matching **off** (the default), confirm that nothing in this
   destination mentions a voiceprint, an enrolment or a match.
7. Turn matching on in Settings → Privacy. The disclosure must be readable in
   full with the keyboard, nothing may be pre-ticked, and the accepted version
   must be announced afterwards.
8. Back on People, open **Suggestions to review**. The queue announces how many
   are waiting and that nothing has been assigned. Walk it with `j`/`k`,
   confirm with Enter, reject with Delete and press `n` to reject and be asked
   who it really was. Every row must read "sounds like <name> — strong match,
   0.82" — never "identified", "verified" or "recognised as".
9. In a recording's **Outputs** tab, the speaker panel's picker must announce
   "Assign all of Speaker 1 to: …" with its current value, open with Enter,
   filter as you type, announce how many people match, and offer
   "Unknown/none" and "Create new person…". The transcript chip opens the same
   list and announces "Speaker: …" with its qualifier.
10. Where a cluster assignment would overwrite segments set individually, the
    question must be announced politely, not shown silently.
11. **Enrol this speaker's voice…** must be unavailable until a person is
    assigned and must say why. Its gate must be readable to the end before the
    attestation can be ticked, and the attestation must never start ticked.
12. While that flow is open, the **clean-speech meter** must read as words and
    not only as a bar: before the measurement arrives it says it is measuring,
    and afterwards it announces "12 s of 10 s clean speech collected, over 3
    stretch(es)." with a glyph-plus-word state ("enough clean speech" /
    "not enough clean speech"). Below the minimum, "Enrol this voice" stays
    unavailable and says why (P2.6 #272, VP-094).
13. On People, **Split <name>'s voiceprint…** (offered only where an enrolment
    has more than one exemplar) must announce each exemplar as its provenance —
    recording, cluster, span and seconds of speech — and never anything about
    the stored vector. Ticking every one must announce that moving all of them
    is a merge, not a split. The attestation is for the person the speech moves
    *to*, and is never pre-ticked (P2.6 #272, VP-091, §3.5).
14. In Settings → Privacy, "Forget all voice profiles" must disclose its counts
    and every limitation sentence, require the typed phrase, and state
    afterwards that the people themselves were kept.

## 12. Search

1. `Mod+Shift+F`. **Expect:** the search field takes focus and announces its name.
2. Type a query. **Expect:** the result count is announced politely; focus stays in the field.
3. Arrow into the results. **Expect:** each result announces the recording it belongs to and the
   matching text, and activating it opens the recording.
4. A query with no results. **Expect:** the empty state is announced as text.

## 13. Overlays

### 13.1 Command palette

1. `Mod+K`. **Expect:** the palette is announced when it opens, focus moves into its input, and the
   input announces its name.
2. Type a few characters. **Expect:** the number of matches is announced politely; the highlighted
   command is announced as the selection moves with `Up`/`Down`.
3. `Enter`. **Expect:** the command runs, the palette closes, and focus goes somewhere announced —
   never to nothing.
4. `Esc`. **Expect:** the palette closes and focus returns to where it was before it opened.

### 13.2 Keyboard reference

1. `?`. **Expect:** the shortcut reference is announced as a dialog; every group and every binding
   is readable as text; `Esc` closes it and restores focus.
2. **Expect:** the chords announced match the chords that work on this platform (`Cmd` on macOS,
   `Ctrl` on Windows).

### 13.3 Dialogs

1. Open any dialog (duplicate import, destructive settings action). **Expect:** it is announced as
   a dialog with its title, focus is trapped inside it, every choice is reachable by `Tab`, `Esc`
   cancels and focus returns to the invoking control.

### 13.4 Toasts

1. Cause a toast (complete an import, run "Refresh"). **Expect:** it is announced through the
   polite live region — it must **not** steal focus and must **not** interrupt the reader
   mid-sentence.
2. **Expect:** the toast is dismissable by its own "Dismiss" button, reachable by `Tab`, and it
   does not disappear on its own while the reader is on it.
3. Cause five toasts. **Expect:** the stack does not read as an endless wall; at most five are
   present.

## 14. Result table

One row per step, per platform. An empty cell is **not run**.

| § | Step | macOS VoiceOver | Windows Narrator | Verbatim announcement (where asked) | Notes |
|---|---|---|---|---|---|
| 3 | 1–4 first run | | | | |
| 4.1 | 1–4 landmarks and skip link | | | | |
| 4.2 | 1–4 destination rail | | | | |
| 4.3 | 7 destinations by chord | | | | |
| 4.4 | 1–3 panes and dividers | | | | |
| 4.5 | 1–3 appearance and contrast | | | | |
| 5 | 1–10 library | | | | |
| 5.1 | 1–3 import | | | | |
| 6 | 1–2 inbox | | | | |
| 7 | 1–4 jobs | | | | |
| 8 | 1 templates | | | | |
| 9 | 1–8 settings, activity, sources | | | | |
| 10 | 1–2 devices | | | | |
| 11.1 | 1–3 header and provenance | | | | |
| 11.2 | 1–5 player | | | | |
| 11.3 | 1–5 transcript | | | | |
| 11.4 | 1–4 outputs | | | | |
| 12 | 1–4 search | | | | |
| 13.1 | 1–4 command palette | | | | |
| 13.2 | 1–2 keyboard reference | | | | |
| 13.3 | 1 dialogs | | | | |
| 13.4 | 1–3 toasts | | | | |

Tester: ______________  OS build: ______________  Screen-reader version: ______________
App version: ______________  Date: ______________

Store the completed table in the release evidence bundle. A run with any cell left empty is an
incomplete run and must be reported as such.
