# Vendored: `craft-ui` layout primitives

| | |
| --- | --- |
| Upstream | <https://github.com/storytold/craft-libs> |
| Upstream crate | `craft-ui` (`crates/ui`) |
| Revision | `1aa81a773aa9198a0e4c0daeda954d622475af38` |
| Licence | MIT OR Apache-2.0 (`LICENSE-MIT`, `LICENSE-APACHE`, `NOTICE` copied verbatim) |
| Vendored on | 2026-10-11, for EPIC O task O1.2 (#200) |

## Why vendored rather than depended on

`deny.toml` sets `[sources] unknown-git = "deny"`, and `craft-ui` is not published to crates.io —
the upstream applications consume it as a git dependency pinned to a commit. `docs/EGUI_MIGRATION_PLAN.md`
§9.2 and `docs/EGUI_REFERENCE_COMPONENTS.md` §6.3 record the decision: vendor a snapshot with
provenance, rather than add a policy exception for a crate that has no CI, no changelog and no
versioning policy.

## What is vendored

| File | Upstream path | Lines |
| --- | --- | --- |
| `src/layout/geometry.rs` | `crates/ui/src/layout/geometry.rs` | split geometry: `SplitAxis`, `SplitSize`, `SplitLayout`, `split_rect` |
| `src/layout/resize.rs` | `crates/ui/src/layout/resize.rs` | `ResizeHandle`: `Role::Splitter`, numeric value/min/max/step, `Increment`/`Decrement`/`SetValue`, axis arrow keys with focus lock |
| `src/layout/tests.rs` | `crates/ui/src/layout/tests.rs` | the geometry and resize-handle tests (the toolbar tests are not vendored) |

Not vendored: `toolbar.rs`, `tabs`, `docking`, `theme`, the icon catalogue and every asset. No
brand material (`ArtCraft`, `PhotoCraft`, `FilmCraft`, `WordCraft` names or `docs/brand/`) is
copied; upstream's `crates/ui/src` contains none.

## Modifications

Each file states its own modifications in its header. In summary:

1. `resize.rs`: the two let-chains (`if let … && …`) are rewritten as nested `if`s, because
   let-chains are an edition-2024 feature and Plaudon is edition 2021 with `rust-version = "1.90"`.
2. `geometry.rs`: the optional `serde` derives are dropped; Plaudon persists its own layout model.
3. `mod.rs`: the toolbar module is not vendored.
4. Imports reordered and bodies re-wrapped to satisfy Plaudon's `rustfmt.toml` (100 columns) and
   `clippy::pedantic`.

## Updating

Do not track upstream automatically. To refresh, diff against the recorded revision, re-apply the
modifications above, and update this file, `NOTICE` and
`docs/licensing/third-party-register.md` together.
