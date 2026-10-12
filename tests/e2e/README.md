# Desktop end-to-end suite

Spec 18.7 acceptance scenarios. `src/lib.rs` is the harness: it builds a real
`plaudon_app::AppPorts` over a temporary library, with no user interface, no
network and no OS notification; `tests/acceptance.rs` holds the scenarios that
run on it. `scenarios.toml` is the registry of all 44 required scenarios and is
gated by `plaudon-release scenarios`.

A registry entry names the runner that executes it:

| `harness` | Where the named test lives |
| --- | --- |
| `rust` | `tests/e2e/tests/acceptance.rs`, in this crate |
| `kittest` | an `egui_kittest` test under `apps/plaudon-egui/tests` |
| `gate` | a pipeline gate rather than a test function |

`the_registry_is_complete_and_its_claims_resolve_to_tests` resolves the first
two kinds against the actual sources, so a renamed test fails the suite.

Until the EPIC O cutover (ADR-0024) there was a fourth runner, `playwright`,
pointing at spec files under `apps/desktop/e2e`. The Tauri/React shell and its
browser-driven suite were deleted with it; nothing in this repository starts a
browser or a node process any more.
