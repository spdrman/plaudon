# ADR-0050: The PLAUD Cloud CLI connector is version-locked to the releases in its compatibility table

- **Status:** Accepted
- **Date:** 2026-10-11, updated 2026-10-12
- **Spec:** SPEC v1.3 §2.1, §2.6, §7.1, §7.2, §7.6, §24 (see "Spec references")

## Status

Accepted — 2026-10-11. Updated 2026-10-12: the table is no longer empty. One
release, `@plaud-ai/cli` 0.3.15, was exercised against a real PLAUD account on
macOS 27.0 (arm64) and is the single committed row. Implemented in
`crates/source-plaud-cli` (`compatibility.rs`, `cli-compatibility.yaml`,
`parser.rs`, `invocation.rs`, `connector.rs`), with golden fixtures under
`tests/integration/fixtures/plaud-cli/`.

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

Two facts about this build decide the rest. Only one release has been
installed against it — the npm package listing declares no license, so we
neither bundle nor redistribute it, and every other release is a population we
have no capture for. And §24 makes manual import a release blocker while the
cloud connector is explicitly conditional.

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

2. **The table is data, compiled in, and holds exactly the exercised
   releases.** `crates/source-plaud-cli/cli-compatibility.yaml` is the single
   source of truth, loaded with `include_str!` so the shipped table is the
   table the tests validated. It has one row today, `0.3.15`.
   `the_committed_table_declares_exactly_the_exercised_release` pins that
   list, so adding a release is a deliberate edit with a test change. The
   version probe that produces the string being matched is `plaud version`
   (first output line, `plaud 0.3.15`); `plaud --version` is rejected by this
   tool as an unknown option and is not used.

3. **Promotion requires evidence.** `CliCompatibilityRow::validate` enforces
   that a row names an exact dotted release, a golden fixture directory, the
   SHA-256 of that fixture's captured stdout, a `verified_on` date and at least
   one `host_os` before `evidence_level: observed_on_identified_release` is
   accepted. `golden_snapshots_match_the_parser_for_every_tested_release`
   re-hashes the capture and re-parses it against a recorded `expected.json`,
   so parser drift, fixture drift and digest drift each fail CI separately.

4. **Pagination only where observed.** A row carries either
   `pagination: observed` with the paging form that was seen in a real
   session, or `not_observed`. In the second case the connector asks for
   exactly one page and labels the listing `PossiblyIncomplete`, with user-
   facing copy that says so. Observed paging has two forms: a cursor, whose
   flag must match one of `SUPPORTED_CURSOR_FLAGS`, and a numbered page, whose
   page flag must match `SUPPORTED_PAGE_FLAGS` and whose page-size flag must
   match `SUPPORTED_PAGE_SIZE_FLAGS` (all three constants live in
   `crates/source-plaud-cli/src/invocation.rs`). A table row therefore cannot
   introduce a new command-line flag without a code change and a test. The
   0.3.15 row uses the numbered form, `-p <page>` with `-s <page-size>`, which
   is what that release documents in `plaud files --help`; enumeration stops
   when a page past the end reports zero files.

5. **Scraping is permitted, under a lock and a warning.**
   `ListingFormat::PlaudTable03` (`plaud_table_0_3` in the table) is the
   version-locked scrape of the 0.3.x human-readable listing: a count line, an
   aligned `ID NAME DATE DURATION` header, a rule, padded rows and a page
   footer. It is keyed on header text rather than column position, with line
   and record caps. `ListingFormat::DelimitedTable` remains for other layouts.
   0.3.15 offers no JSON mode, so its row sets `machine_readable: no`, which
   sets `needs_scraping_warning` and the diagnostics panel renders it verbatim.

6. **Fixture releases are structurally separable from shipped ones.** The test
   suite drives the connector through
   `tests/integration/fixtures/plaud-cli/cli-compatibility.fixture.yaml`, and
   `the_shipped_table_carries_only_the_release_that_was_exercised` asserts
   that the committed table is exactly `["0.3.15"]` and that no synthetic
   fixture version appears in it.

### How to add a tested release

1. Install the exact release and capture `plaud version` and `plaud files`
   verbatim into `tests/integration/fixtures/plaud-cli/<fixture_id>/`
   (`files.stdout`, `fixture.json`, `expected.json`). Committed fixtures must
   be sanitised before they land: the real layout, with synthetic identifiers,
   no recording titles and no URLs. A capture straight from an account is
   evidence for the person who took it, never a committed file.
2. Record the capture's SHA-256 in a new row of
   `crates/source-plaud-cli/cli-compatibility.yaml`, with the observed listing
   format, the observed paging form and its flags, `verified_on` and the host
   OSes it was exercised on, then extend
   `the_committed_table_declares_exactly_the_exercised_release` and
   `the_shipped_table_carries_only_the_release_that_was_exercised` to the new
   list.
3. Record the download-URL observations required by §2.6 in a `UrlContract`
   and add the observed host to the `DownloadPolicy` allowlist. Until a host is
   recorded there, every download is refused — the default allowlist is empty.
4. Run the opt-in real-CLI test on the host being claimed and require it to
   pass: `PLAUDON_PLAUD_CLI_TESTS=1 cargo test -p plaudon-app --test
   plaud_real_cli -- --ignored`. It is ignored by default because it launches
   the vendor tool against a real account.
5. Update the `plaud_cloud_cli` row in `compatibility.yaml` for the host and
   OS version actually exercised, with `last_verified_at`,
   `verified_import_recorded` and a rewritten `known_limitations` list, then
   regenerate `docs/public/compatibility.md` with
   `plaudon-release compat-matrix`.

## Consequences

- The connector works on exactly one release, `@plaud-ai/cli` 0.3.15, on
  macOS: discovery, the `plaud version` probe, enumeration by numbered page
  and a download all run, under a scraping warning that cannot be turned off
  while the release reports no machine-readable mode. That is the §7.2(a)
  branch for that release; everything else is still the §7.2(b) branch, with
  copy that points the user at manual import.
- Every piece of machinery the §7.2(a) branch needs is implemented and
  exercised against recorded fixtures, so promoting a further release is a
  data change plus a capture, not a development project.
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
