# ADR-0020: Desktop command surface and the two additions to the §15.2 table

- **Status:** accepted
- **Date:** 2026-10-11
- **Deciders:** EPIC E (desktop shell)
- **Spec:** §15.1–§15.4, §5.5, §22(13)

## Context

§15.1 requires a narrow, typed command surface: no generic shell execution, no
generic HTTP proxy, no arbitrary filesystem access from JavaScript. §15.2 lists
the *proposed* commands. Two Phase-2 acceptance requirements have no read or
write path in that table:

1. §5.5 and §22(13) require an **in-app activity history** that stays complete
   even when OS notifications are denied or disabled. Nothing in §15.2 returns
   activity entries; `list_jobs` is not a substitute because attention events
   are not jobs and completion entries must survive job pruning.
2. §4.1/§5.2 require first-run onboarding to persist the user's chosen default
   engines and privacy posture. `set_local_only_mode` covers only the network
   policy; nothing persists "onboarding completed" plus the chosen defaults, and
   inferring completion from other state would re-run onboarding after an
   unrelated settings change.

## Decision

The desktop exposed exactly 16 commands at the end of EPIC E: the 14 §15.2
commands used by Phase 2
(`get_capabilities`, `list_recordings`, `import_files`, `get_recording`,
`get_transcript`, `save_transcript_revision`, `list_jobs`, `cancel_job`,
`retry_job`, `export_recording`, `set_notification_settings`,
`set_local_only_mode`, `set_openrouter_key`, `remove_openrouter_key`) plus two
additions:

- `list_activity(cursor, limit) -> ActivityPage`
- `complete_onboarding(defaults) -> Preferences`

Both are read/write paths for state the spec already mandates; neither widens
the trust boundary: they accept no paths, no URLs and no free-form commands.

**Amended by EPIC K (2026-10-11), source and connector commands.** EPIC K adds
nine commands, all of which are already in the §15.2 table, so the amendment
widens the implemented surface without widening the *proposed* one:

| Command | Request | Response |
|---|---|---|
| `plaud_probe` | none | `PlaudConnectorSnapshot` (status, diagnostics, disconnect copy) |
| `plaud_connect` | user intent | `PlaudConnectorSnapshot` |
| `plaud_disconnect` | none | disconnect variant + snapshot |
| `plaud_list` | cursor | items, cursor, completeness, empty-account advisory |
| `plaud_import` | external ids | per-item outcome, job id, recording id |
| `source_add_folder` | OS-approved path, recursive, rule | source id and stored policy |
| `source_sync` | source id, trigger | job id, source state, counts |
| `source_reconcile` | source id | job id, decisions, listing completeness |
| `list_source_artifacts` | recording id | vendor exports with provenance |

Three properties hold for all nine:

- They accept no URL and no free-form command. `source_add_folder` is the only
  one that takes a path, and it takes one the OS picker already approved and
  canonicalizes it through `plaudon_security::fsguard` before storing it.
- `plaud_import` is not a general fetch: it refuses any external id that was
  not in the caller's most recent `plaud_list` page, so the IPC boundary cannot
  be used to name an arbitrary remote asset.
- Nothing in a response carries vendor text. Connector copy comes from
  `plaudon_source_plaud_cli::diagnostics`, which composes from a stable error
  code, so §7.6 ("no raw vendor error payloads") is enforced by the data that
  exists rather than by a rule the frontend has to follow.

`plaud_disconnect` is the one command whose *wording* is load-bearing: §4.2.7
forbids claiming to have revoked a PLAUD session we cannot revoke, so it
returns a `DisconnectCopy` variant and the frontend renders that label and body
verbatim.

EPIC K also emits three further §15.3 events — `source.auth_required`,
`source.sync_finished` and `recording.source_updated` — all of which are in the
published §15.3 list.

Commands reachable from JavaScript are enumerated in one
`tauri::generate_handler!` list, and a test fails the build if a registered
command is outside this allowlist or matches a generic-execution pattern
(`^exec`, `^run_`, `^shell`, `^http_`, `^read_file$`, `^write_file$`).

Media playback does not add a command: Rust registers the `plaudon-media`
URI scheme, resolves a recording id to its archived blob and serves it
read-only with range support. The Tauri asset protocol stays disabled and the
capability set contains no filesystem permission, so JavaScript never sees or
supplies a path.

One event is emitted beyond the §15.3 list: `ui.activation_requested`. §5.5
requires that clicking a native notification opens the affected recording or
job, and §15.3 has no event for that. It is emitted only from the OS
activation callback, carries nothing but a target kind and id, and — like
every other event — is a hint: the destination refetches committed state. The
alternative (polling a "pending activation" row from the frontend) would add a
command and still not express the one-shot nature of an activation.

## Rejected alternatives

- **A generic `invoke(name, args)` dispatcher.** Rejected: it turns the command allowlist into a convention and defeats the generic-execution pattern test.
- **Enabling the Tauri asset protocol for media playback.** Rejected: it requires a filesystem permission and lets JavaScript supply paths; the `plaudon-media` scheme keeps paths in Rust.
- **Polling a pending-activation row for notification clicks.** Rejected: it adds a command and does not express the one-shot nature of an activation.

## Consequences

- The frontend contract (`apps/desktop/src/ipc/contract.ts`) and the versioned
  JSON schemas under `schemas/commands/v1/` enumerate the same commands as the
  Rust allowlist — 16 after EPIC E, 25 after EPIC K's amendment; drift fails CI
  on both sides.
- Later EPICs that need device, cloud, model, consent, usage, tool, backup or
  restore commands add them from the §15.2 table; they must not reuse these two
  additions as a general-purpose escape hatch.
- If a future revision of the specification adds activity and onboarding
  commands with different names, this ADR is superseded and the names change in
  one place.
