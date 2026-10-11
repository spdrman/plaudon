# ADR-0020: Desktop command surface and the two additions to the §15.2 table

- Status: accepted
- Date: 2026-10-11
- Deciders: EPIC E (desktop shell)
- Spec: §15.1–§15.4, §5.5, §22(13)

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

The desktop exposes exactly 16 commands: the 14 §15.2 commands used by Phase 2
(`get_capabilities`, `list_recordings`, `import_files`, `get_recording`,
`get_transcript`, `save_transcript_revision`, `list_jobs`, `cancel_job`,
`retry_job`, `export_recording`, `set_notification_settings`,
`set_local_only_mode`, `set_openrouter_key`, `remove_openrouter_key`) plus two
additions:

- `list_activity(cursor, limit) -> ActivityPage`
- `complete_onboarding(defaults) -> Preferences`

Both are read/write paths for state the spec already mandates; neither widens
the trust boundary: they accept no paths, no URLs and no free-form commands.

Commands reachable from JavaScript are enumerated in one
`tauri::generate_handler!` list, and a test fails the build if a registered
command is outside this allowlist or matches a generic-execution pattern
(`^exec`, `^run_`, `^shell`, `^http_`, `^read_file$`, `^write_file$`).

Media playback does not add a command: Rust registers the `plaudon-media`
URI scheme, resolves a recording id to its archived blob and serves it
read-only with range support. The Tauri asset protocol stays disabled and the
capability set contains no filesystem permission, so JavaScript never sees or
supplies a path.

## Consequences

- The frontend contract (`apps/desktop/src/ipc/contract.ts`) and the versioned
  JSON schemas under `schemas/commands/v1/` enumerate the same 16 commands;
  drift fails CI on both sides.
- Later EPICs that need device, cloud, model, consent, usage, tool, backup or
  restore commands add them from the §15.2 table; they must not reuse these two
  additions as a general-purpose escape hatch.
- If a future revision of the specification adds activity and onboarding
  commands with different names, this ADR is superseded and the names change in
  one place.
