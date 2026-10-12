# ADR-0060: The PLAUD Cloud CLI connector is version-locked and ships with an empty compatibility table

- **Status:** Accepted
- **Date:** 2026-10-11
- **Spec:** SPEC v1.3 §2.1, §2.6, §7.1, §7.2, §7.6, §24 (see "Spec references")

## Status

Accepted — 2026-10-11. Implemented in `crates/source-plaud-cli`
(`compatibility.rs`, `cli-compatibility.yaml`, `parser.rs`, `connector.rs`),
with golden fixtures under `tests/integration/fixtures/plaud-cli/`.

## Context

SPEC §2.1 records what PLAUD's help center actually documents: a
`@plaud-ai/cli` package whose `plaud login` authenticates through a browser,
whose `plaud files` enumerates recordings, and whose `plaud audio <id>` yields
a temporary download URL. It then lists what we are **not** permitted to
assume: JSON mode, stable tabular output, an embeddable library API,
documented exit codes, pagination, refresh-token access, a stable download URL
format, or an entitlement to redistribute the package.

§7.2 narrows the design space to two acceptable options: ship a version-locked
parser with golden test snapshots and a compatibility warning, or postpone
automated enumeration while preserving manual import. It explicitly forbids
treating brittle scraping as a permanently supported API, and forbids guessing
pagination behaviour.

Two facts about this build decide the rest. No release of the official CLI has
been installed against it — the npm package listing declares no license, so we
neither bundle nor redistribute it, and nobody has produced a captured
`plaud files` transcript from an identified release. And §24 makes manual
import a release blocker while the cloud connector is explicitly conditional.

The failure mode we are designing against is a mis-read listing. If a future
CLI release renames a column, reorders a table, changes a duration unit or
starts paginating, a parser that "mostly works" will import the wrong file,
attach the wrong external identity to a recording, or silently show a partial
account as complete. None of those is detectable by the user.

## Decision

1. **Exact-version matching, fail closed.** `CliCompatibilityTable::verdict`
   matches the reported version string exactly. A release one patch newer than
   a tested one is `NotValidated`, which the connector turns into
   `INCOMPATIBLE_VERSION` and the §7.6 error `CliUnsupported`. There is no
   nearest-neighbour fallback and no floor-based range check, because what
   breaks between releases is the output contract, not a semantic-version
   ordering.

2. **The table is data, compiled in, and currently empty.**
   `crates/source-plaud-cli/cli-compatibility.yaml` is the single source of
   truth, loaded with `include_str!` so the shipped table is the table the
   tests validated. It has zero rows. That is a recorded finding, not an
   omission: `the_committed_table_declares_no_usable_release_yet` asserts it,
   so promoting a release is a deliberate edit with a test change.

3. **Promotion requires evidence.** `CliCompatibilityRow::validate` enforces
   that a row names an exact dotted release, a golden fixture directory, the
   SHA-256 of that fixture's captured stdout, a `verified_on` date and at least
   one `host_os` before `evidence_level: observed_on_identified_release` is
   accepted. `golden_snapshots_match_the_parser_for_every_tested_release`
   re-hashes the capture and re-parses it against a recorded `expected.json`,
   so parser drift, fixture drift and digest drift each fail CI separately.

4. **Pagination only where observed.** A row carries either
   `pagination: observed` with a cursor flag and page size that were seen in a
   real session, or `not_observed`. In the second case the connector asks for
   exactly one page and labels the listing `PossiblyIncomplete`, with user-
   facing copy that says so. A cursor flag must additionally match one of the
   `SUPPORTED_CURSOR_FLAGS` constants, so a table row cannot introduce a new
   command-line flag without a code change and a test.

5. **Scraping is permitted, under a lock and a warning.**
   `ListingFormat::DelimitedTable` exists and is implemented, keyed on header
   text rather than column position, with line and record caps. Any row whose
   `machine_readable` is not `yes` sets `needs_scraping_warning`, which the
   diagnostics panel renders verbatim.

6. **Fixture releases are structurally separable from shipped ones.** The test
   suite drives the connector through
   `tests/integration/fixtures/plaud-cli/cli-compatibility.fixture.yaml`, and
   `the_shipped_table_is_not_the_fixture_table` asserts that no fixture version
   appears in the committed table.

### How to add a tested release

1. Install the exact release and capture `plaud --version` and `plaud files`
   verbatim into `tests/integration/fixtures/plaud-cli/<fixture_id>/`
   (`files.stdout`, `fixture.json`, `expected.json`).
2. Record the capture's SHA-256 in a new row of
   `crates/source-plaud-cli/cli-compatibility.yaml`, with the observed format,
   observed pagination, `verified_on` and the host OSes it was exercised on.
3. Record the download-URL observations required by §2.6 in a `UrlContract`
   and add the observed host to the `DownloadPolicy` allowlist. Until a host is
   recorded there, every download is refused — the default allowlist is empty.
4. Update the `plaud_cloud_cli` row in `compatibility.yaml` from
   `not_evaluated`, and expect
   `the_committed_table_declares_no_usable_release_yet` to fail until it is
   rewritten to assert the new, intended contents.

## Consequences

- The connector is inert in this build: discovery and the version probe run,
  and everything past them answers `CliUnsupported` with copy that points the
  user at manual import. That is the §7.2(b) branch, chosen honestly rather
  than by omission.
- Every piece of machinery the §7.2(a) branch needs is implemented and
  exercised against recorded fixtures, so promoting a release is a data change
  plus a capture, not a development project.
- A user running a brand-new CLI release gets a clear, specific message naming
  their version instead of a corrupted import.

## Rejected alternatives

- **A minimum-version floor (`>= 1.4.0`).** Cheap to write and wrong for this
  problem: it asserts that output formats are monotonic in version order, which
  §2.1 forbids assuming and which no vendor promises. It also silently accepts
  every future release, which is exactly the population we have the least
  evidence about.
- **Best-effort scraping with a "may be inaccurate" banner.** Rejected because
  §7.2 forbids treating brittle scraping as a permanently supported API, and
  because a banner does not prevent the concrete harm: a wrong external
  identity attached to a recording is invisible to the user and survives into
  deduplication and reconciliation.
- **Shipping a speculative row for the current npm release.** We could read the
  package's README and guess. §2.5B's evidence levels exist precisely to stop
  this: a documentation claim is `proposed`, not
  `observed_on_identified_release`, and `permits_use` is false for everything
  below the latter.
- **Bundling the vendor CLI so the version is always known.** The package
  listing declares no license (§2.6), so redistribution is not ours to assume,
  and bundling would also make us responsible for a Node.js runtime we do not
  otherwise need.

## Spec references

- §2.1 — what PLAUD documents, and the explicit list of things we may not
  assume about the CLI.
- §2.6 — the spike findings that must be recorded before relying on CLI
  behaviour or on the download URL contract.
- §7.1 — the connector state machine and the hardened process runner.
- §7.2 — version-locked parser with golden snapshots, or postponement; no
  guessed pagination; identity is `(source_kind, account_scope, external_id)`.
- §7.6 — the stable error set, and the rule that raw vendor payloads never
  reach the user.
- §24 — manual import is a release blocker; the cloud connector is conditional.
