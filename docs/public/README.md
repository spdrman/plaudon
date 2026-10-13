# Plaudon documentation

Plaudon is an independent, local-first desktop application for importing,
transcribing and reviewing your own recordings. It is not produced by,
affiliated with or approved by PLAUD, OpenRouter or Apple.

- [Install and open Plaudon on macOS](./install-macos.md) — verifying the
  download, and getting an unsigned, not-notarized build past Gatekeeper.
- [Privacy disclosure](./privacy-disclosure.md) — what leaves your machine, when,
  where to, and under what permission.
- [Vendor dependency matrix](./vendor-matrix.md) — every external service and
  component, its status and its licence.
- [Compatibility matrix](./compatibility.md) — generated from the claim
  register; no cell can claim support without archived evidence.
- [Troubleshooting](./troubleshooting.md) — first run, missing tools, denied
  permissions, cloud failures, backup and restore.
- [Backup and restore](./backup-and-restore.md) — exports versus portable
  backups, and how a restore is validated.
- [Model and offline artifact licensing](./model-licensing.md) — what is
  downloaded, from whom, under which licence.
- [Release notes](./release-notes.md) — what each release contains and what it
  does not.

## How claims in these documents are controlled

Each compatibility statement uses one of six fixed terms — `Documented`,
`Observed in upstream code`, `Validated`, `Supported`, `Experimental`,
`Unsupported` — and the compatibility page is generated from the machine-readable
claim register rather than written by hand. A continuous-integration lint
rejects any other term, any unmeasured performance or quality claim, and any
wording implying vendor endorsement.
