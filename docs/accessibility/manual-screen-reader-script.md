# Manual screen-reader verification script (VoiceOver / Narrator)

Spec: §5.3 (WCAG 2.2 AA target, VoiceOver and Narrator testing), §18.7 #18,
§22(7). Automated axe checks run in CI (`scripts/ui-a11y.sh`); this script
covers what automation cannot judge: announcement quality, virtualized list
navigation and focus behaviour in a real screen reader.

Record the result of every step in the release evidence bundle: tester, OS
build, screen-reader version, app version, pass/fail and verbatim announcement
where it matters.

## Environments

| Platform | Screen reader | Start | Stop |
|---|---|---|---|
| macOS 14+ | VoiceOver | `Cmd+F5` | `Cmd+F5` |
| Windows 11 | Narrator | `Ctrl+Win+Enter` | `Ctrl+Win+Enter` |

Navigation keys used below: VoiceOver `VO = Ctrl+Option`; Narrator `Caps Lock`
is the Narrator key. "Next item" is `VO+Right` / `Narrator+Right`.

## 1. Shell and navigation

1. Launch the app. **Expect:** the window title "Plaudon" is announced and the
   first focus stop is the "Skip to main content" link.
2. Press `Tab` once, then `Enter` on the skip link. **Expect:** focus moves to
   the main region; the screen reader announces the current destination name.
3. `Tab` to the primary navigation. **Expect:** it is announced as a tab list
   with five tabs (Library, Inbox, Jobs, Templates, Settings) and the selected
   tab is reported as selected.
4. Use `Down`/`Up` inside the navigation. **Expect:** each destination is
   announced on arrival and the main region content changes; only one tab is in
   the tab order.
5. Toggle the OS appearance (macOS: System Settings → Appearance; Windows:
   Settings → Colors) while the app is open. **Expect:** the UI theme follows
   immediately, focus is not lost and the focus indicator stays visible.
6. Enable high contrast (macOS: Increase Contrast; Windows: Contrast themes).
   **Expect:** the app switches to the high-contrast theme and focus indicators
   remain at least as visible.

## 2. Library (virtualized list)

1. Go to Library with at least 1,000 recordings.
2. `Tab` to the list. **Expect:** it is announced as a grid/table with the total
   row count — not the number of rendered rows.
3. Arrow `Down` ten times. **Expect:** each row announces title, date, duration
   (or "Unknown" — never "0:00"), source and both statuses as text; the row
   position reflects the full set, not the virtual window.
4. Press `End`, then `Home`. **Expect:** focus reaches the last and first row;
   the list scrolls and announces them.
5. Press `Space` on three rows. **Expect:** selection state is announced per row
   and the live region reports the new selection count.
6. Press `Cmd/Ctrl+A`. **Expect:** "all N selected" is announced once.
7. Type in the search field. **Expect:** the result count is announced politely
   without stealing focus from the field.
8. Press `Enter` on a row. **Expect:** the recording detail opens and focus
   lands on the recording heading.

## 3. Import

1. From Library press the Import button with the keyboard. **Expect:** the OS
   file picker opens and is fully operable; cancelling returns focus to the
   Import button.
2. Import a file that duplicates an existing recording. **Expect:** the
   duplicate dialog is announced as a dialog, focus is trapped inside it, the
   three choices are reachable, `Esc` cancels and focus returns to the invoking
   control.
3. Drag a file onto the window (mouse). **Expect:** the drop affordance is
   announced through the live region and is described by text, not colour only.

## 4. Player

1. Open a recording with audio. `Tab` to the transport.
2. **Expect:** play/pause announces its state change; the scrubber is announced
   as a slider with a time value ("42 seconds of 12 minutes 5 seconds").
3. Operate `Left`/`Right`, `-10 s`, `+10 s`, speed. **Expect:** each change is
   announced; position and duration are readable as text.
4. Open a recording whose duration is unknown. **Expect:** "Duration unknown" is
   announced and no slider is present — there must be no invented timeline.

## 5. Transcript

1. Arrow through transcript segments. **Expect:** each segment announces its
   timestamp chip (when present), its text, and any flag such as "low
   confidence" or "possible hallucination".
2. For a transcript with no alignment, **expect** no timestamp is announced and
   no seek control exists.
3. Enter edit mode on a segment, change text, `Cmd/Ctrl+Z`. **Expect:** edit
   mode, the change and the undo are announced; the textarea has an accessible
   name tied to the segment.
4. Save. **Expect:** the new revision is announced; the revision switcher
   distinguishes "Original (machine)" from the edited revision.
5. Force a conflict (edit the same recording in a second window, save both).
   **Expect:** the conflict is announced with both resolution choices and the
   editor still contains the user's text.

## 6. Jobs and activity

1. Open Jobs with at least one job in each of running, waiting, failed and
   `Cloud outcome unknown`. **Expect:** every state is announced as text, never
   by colour; progress is announced as a progress bar, and an unknown ratio is
   announced as indeterminate.
2. On the `Cloud outcome unknown` job, attempt retry. **Expect:** the cost
   acknowledgement is announced and must be confirmed before Retry is enabled.
3. Deny notification permission at the OS level, then complete an import.
   **Expect:** the activity history still lists the completion with its
   suppression reason.

## 7. Destructive actions

1. Remove the stored API key from Settings. **Expect:** an alert dialog is
   announced, states the consequence, traps focus and returns focus on cancel.

## Recording results

Create one row per step in the evidence bundle:

```
| step | macOS VoiceOver | Windows Narrator | notes |
```

A step that cannot be completed with the keyboard alone is a release blocker
(§19 Phase 2 exit gate).
