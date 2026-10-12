# Transcript editor performance at three hours

Task O2.1 (#205) and the parity matrix row **T14** set the scale target: a
three-hour recording, ~30,000 segments, must stay editable — typing, searching,
replacing, scrolling — without the UI stalling. This file records what was
measured, on what, and which budgets the test suite now enforces.

The fixture is `plaudon_transcript_doc::testing::synthetic_transcript(30_000)`:
30,000 timed segments of 8–12 words from a fixed vocabulary, 360 ms of media
each — exactly three hours — and about 2 MB of text. It is deterministic, so
the numbers below compare across machines and across commits.

## What is measured

| Measurement | Why it is the one that matters |
| --- | --- |
| Open 30k segments | Builds every piece table; happens once per transcript. |
| Worst keystroke of 200 | A keystroke must fit in a 60 Hz frame (16.6 ms). |
| Literal search over 30k segments | The find bar re-runs the whole query on every keystroke. |
| Whole-word search | Same, with the boundary test on. |
| Replace-all | Every match, recorded as **one** undoable step. |
| Autosave payload | Collecting the changed `(seq_index, text)` pairs for `save_transcript_revision`. |
| Undo replace-all | Inverting that one step. |
| Segment at playhead | Resolved every frame while audio plays. |
| Editor frame at 30k segments | One full `egui` frame of the widget, virtualized. |

## Measured results

**MARS runner** (`devshell@192.168.0.100`, via `scripts/e2e-remote.sh cargo`,
Docker `rust:1-bookworm`, `--cpus 8 --memory 12g`, `--release`, 2026-10-11):

| Measurement | Measured | Enforced budget |
| --- | ---: | ---: |
| Open 30k segments | 6.0 ms | 300 ms |
| Worst keystroke of 200 | 0.002 ms | 16 ms |
| Literal search over 30k segments | 1.8 ms | 250 ms |
| Whole-word search over 30k segments | 1.8 ms | 250 ms |
| Replace-all (every match, one undo step) | 8.1 ms | 600 ms |
| Autosave payload | 0.9 ms | 100 ms |
| Undo replace-all | 7.1 ms | 600 ms |
| Segment at playhead | < 0.001 ms | 0.2 ms |
| Editor frame at 30k segments | 0.18 ms | 60 ms |

**Developer workstation** (macOS, arm64, **debug** profile, `cargo test -j 5`),
which is the profile the budgets are set for, because that is how the tests run
in CI:

| Measurement | Measured |
| --- | ---: |
| Open 30k segments | 20.5 ms |
| Worst keystroke of 200 | 0.005 ms |
| Literal search over 30k segments | 38.9 ms |
| Whole-word search over 30k segments | 44.6 ms |
| Replace-all | 93.0 ms |
| Autosave payload | 5.4 ms |
| Undo replace-all | 74.1 ms |
| Segment at playhead | 0.001 ms |
| Editor frame at 30k segments | 0.80 ms |

The budgets are roughly an order of magnitude above the debug-profile
measurements. They are regression detectors, not targets: they catch an
accidental full-document rebuild per keystroke, a quadratic search, or a loss
of virtualization, and they do not fail because a laptop was busy.

## Why the numbers look like this

* **Typing is independent of transcript length.** An edit touches one
  segment's piece table and bumps two counters; nothing walks the document.
* **Search allocates almost nothing.** Segment text is stored as a piece table
  over the base-revision string, so an unedited segment is searched as a
  borrowed `&str`. Matching is a literal scan with a first-character filter;
  there is no regular-expression engine on the keystroke path.
* **Replace-all is one `OpGroup`.** The matches are replaced back-to-front so
  the recorded offsets stay valid, which makes the undo a single inversion of
  the same list rather than thousands of separate steps.
* **The editor frame does not depend on the transcript either.** Only rows
  intersecting the viewport are laid out, and their galleys are cached until
  the segment, the wrap width or the search highlight changes. Rows that have
  never been drawn carry an estimated height derived from their character
  count, which is replaced by the measured height the first time they appear.

## Reproducing

```sh
# Core model, on MARS, release:
scripts/e2e-remote.sh cargo test --release -p plaudon-transcript-doc -- --nocapture

# Core model and the egui widget, both profiles, locally:
cargo test -p plaudon-transcript-doc --test scale -- --nocapture
cargo test -p plaudon-ui --test transcript_editor frame_budget -- --nocapture
```

The budgets live in `crates/transcript-doc/tests/scale.rs` and in
`crates/ui/tests/transcript_editor.rs`; update this file whenever they change.
