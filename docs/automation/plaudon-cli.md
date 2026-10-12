# plaudon CLI — User-local scripting bridge

Spec: §14.5 (native automation), App. A.5 (automation and backup policy), App. A.6 (rejected alternatives).

The `plaudon` CLI is a user-local scripting bridge that exposes `list`, `export`, `process`, and `jobs status` commands. It calls the same Rust application services (`plaudon_app_core`, `plaudon_jobs`) the GUI uses. It opens no network port, runs no server, and is disabled by default.

---

## Enable the bridge

The bridge is disabled by default. To enable:

```bash
plaudon bridge enable --confirm
```

The `--confirm` flag prevents accidental enablement. This stores an enable record with the current OS user's username. Every subsequent `plaudon` invocation checks that the calling user matches the stored owner; if not, commands are refused.

To check status:

```bash
plaudon bridge status
```

To disable:

```bash
plaudon bridge disable
```

The bridge can also be enabled or disabled in Settings → Automation in the GUI.

---

## Global options

- **`--library <PATH>`**: Override the default library path. Can also be set via `$PLAUDON_LIBRARY` environment variable. Default: the application's configured library location.
- **`--pretty`**: Print indented JSON instead of one line.

---

## Output format

Every command prints exactly one JSON document to stdout. No progress bars, spinners, or log messages to stdout. Logs (if any) go to stderr.

### Success envelope

```json
{
  "status": "ok",
  "contract_version": 1,
  "command": "list",
  "data": { /* command-specific payload */ }
}
```

### Error envelope

```json
{
  "status": "error",
  "contract_version": 1,
  "command": "process",
  "code": "CONSENT_REQUIRED",
  "message": "Cloud processing for this model requires consent; grant it in Settings → AI Providers",
  "retryable": false
}
```

- **`status`**: `"ok"` or `"error"`.
- **`contract_version`**: Integer version of the envelope schema. Current version is `1`.
- **`command`**: Name of the command that ran (`"list"`, `"export"`, `"process"`, `"jobs"`, `"bridge"`).
- **`data`**: Command-specific payload (present only in `status: "ok"`).
- **`code`**: Stable error code (present only in `status: "error"`). Examples: `BRIDGE_DISABLED`, `CONSENT_REQUIRED`, `LOCAL_ONLY_MODE`, `RECORDING_NOT_FOUND`, `INVALID_MODEL`.
- **`message`**: Human-readable error message (present only in `status: "error"`).
- **`retryable`**: Boolean indicating whether retrying the same command is safe (present only in `status: "error"`).

### Exit codes

- **0**: success.
- **2**: refusal or failure (consent denied, Local-only Mode, bridge disabled, invalid argument, not found).

Exit code 2 (not 1) distinguishes "the CLI refused the command" from "the CLI could not start at all" (e.g., missing library, corrupted binary).

---

## Commands

### `plaudon list`

List recordings in the library.

#### Usage

```bash
plaudon list [OPTIONS]
```

#### Options

- **`--limit <N>`**: Maximum number of recordings to return. Default: 50. Range: 1–1000.
- **`--offset <N>`**: Number of recordings to skip. Default: 0.
- **`--sort <ORDER>`**: Ordering. Values: `newest` (most recently recorded first), `oldest`, `title` (alphabetical). Default: `newest`.

#### Example

```bash
plaudon list --limit 10 --sort newest
```

#### Example output

```json
{
  "status": "ok",
  "contract_version": 1,
  "command": "list",
  "data": {
    "recordings": [
      {
        "id": "123e4567-e89b-12d3-a456-426614174000",
        "title": "Team standup 2026-10-11",
        "captured_at": "2026-10-11T09:00:00Z",
        "duration_ms": 1800000,
        "source": "manual_import",
        "import_status": "completed",
        "transcript_status": "completed"
      },
      {
        "id": "223e4567-e89b-12d3-a456-426614174001",
        "title": "Client call notes",
        "captured_at": "2026-10-10T14:30:00Z",
        "duration_ms": 2700000,
        "source": "plaud_cloud",
        "import_status": "completed",
        "transcript_status": "pending"
      }
    ],
    "total": 2,
    "limit": 10,
    "offset": 0
  }
}
```

- **`recordings`**: Array of recording summaries.
  - **`id`**: UUID of the recording.
  - **`title`**: User-visible title.
  - **`captured_at`**: ISO-8601 timestamp when the recording was captured (nullable).
  - **`duration_ms`**: Duration in milliseconds (nullable).
  - **`source`**: Import source (`manual_import`, `plaud_cloud`, `plaud_ble`, `plaud_usb`, `watched_folder`).
  - **`import_status`**: `pending`, `in_progress`, `completed`, `failed`.
  - **`transcript_status`**: `pending`, `queued`, `in_progress`, `completed`, `failed`, `none`.
- **`total`**: Total number of recordings matching the query.
- **`limit`**: Limit used in the query.
- **`offset`**: Offset used in the query.

---

### `plaudon export`

Export a recording's transcript.

#### Usage

```bash
plaudon export --recording <UUID> --format <FORMAT> [OPTIONS]
```

#### Options

- **`--recording <UUID>`**: Recording identifier (required).
- **`--format <FORMAT>`**: Output shape (required). Values:
  - **`json`**: Full segment list with times, speakers, and provenance.
  - **`text`**: One line per segment, plain text.
- **`--out <PATH>`**: Write to this file instead of stdout.

#### Example

```bash
plaudon export --recording 123e4567-e89b-12d3-a456-426614174000 --format json
```

#### Example output (format: json)

```json
{
  "status": "ok",
  "contract_version": 1,
  "command": "export",
  "data": {
    "recording_id": "123e4567-e89b-12d3-a456-426614174000",
    "title": "Team standup 2026-10-11",
    "transcript_revision_id": "rev-001",
    "engine": "whisper.cpp",
    "model_id": "base.en",
    "locale": "en-US",
    "created_at": "2026-10-11T09:05:23Z",
    "segments": [
      {
        "index": 0,
        "start_ms": 0,
        "end_ms": 3500,
        "speaker_label": "Speaker 1",
        "text": "Good morning everyone. Let's start with yesterday's progress.",
        "confidence": 0.92
      },
      {
        "index": 1,
        "start_ms": 3600,
        "end_ms": 7200,
        "speaker_label": "Speaker 2",
        "text": "I finished the API integration and pushed the PR.",
        "confidence": 0.89
      }
    ]
  }
}
```

- **`recording_id`**: UUID of the recording.
- **`title`**: Recording title.
- **`transcript_revision_id`**: Revision identifier.
- **`engine`**: Transcription engine used (`whisper.cpp`, `apple_speech`, `openrouter`).
- **`model_id`**: Model identifier (nullable).
- **`locale`**: Detected or configured locale (nullable).
- **`created_at`**: ISO-8601 timestamp when the transcript was created.
- **`segments`**: Array of transcript segments.
  - **`index`**: Segment index (0-based).
  - **`start_ms`**: Start time in milliseconds (nullable).
  - **`end_ms`**: End time in milliseconds (nullable).
  - **`speaker_label`**: Speaker label (nullable; e.g., `"Speaker 1"`, `"Alice"`, `null`).
  - **`text`**: Transcript text.
  - **`confidence`**: Confidence score 0.0–1.0 (nullable).

#### Example output (format: text, written to stdout)

```
Good morning everyone. Let's start with yesterday's progress.
I finished the API integration and pushed the PR.
```

---

### `plaudon process`

Queue a processing job for a recording.

#### Usage

```bash
plaudon process --recording <UUID> --stage <STAGE> --run <WHERE> [OPTIONS]
```

#### Options

- **`--recording <UUID>`**: Recording identifier (required).
- **`--stage <STAGE>`**: Processing stage to queue (required). Values:
  - **`transcribe`**: Speech to text.
  - **`summarize`**: Summarization and extraction (requires existing transcript).
  - **`diarize`**: Speaker diarization.
- **`--run <WHERE>`**: Where the work runs (required). Values:
  - **`local`**: On this machine.
  - **`cloud`**: Through the configured cloud provider (requires consent, refused in Local-only Mode).
- **`--model <MODEL>`**: Model identifier to use (optional). If omitted, uses the default for the stage/destination.

#### Example

```bash
plaudon process --recording 123e4567-e89b-12d3-a456-426614174000 --stage transcribe --run local
```

#### Example output

```json
{
  "status": "ok",
  "contract_version": 1,
  "command": "process",
  "data": {
    "job_id": "job-abc123",
    "recording_id": "123e4567-e89b-12d3-a456-426614174000",
    "stage": "transcribe",
    "destination": "local",
    "model": "whisper.cpp/base.en",
    "status": "queued",
    "created_at": "2026-10-11T10:00:00Z"
  }
}
```

- **`job_id`**: Unique job identifier.
- **`recording_id`**: UUID of the recording.
- **`stage`**: Processing stage (`transcribe`, `summarize`, `diarize`).
- **`destination`**: Where the job runs (`local`, `cloud`).
- **`model`**: Model identifier used (nullable).
- **`status`**: Initial job status (`queued`).
- **`created_at`**: ISO-8601 timestamp when the job was created.

#### Cloud processing and consent

If `--run cloud` is used:

1. The CLI checks that consent has been granted for the selected model/provider in the GUI.
2. If consent is missing, the command fails with `CONSENT_REQUIRED`.
3. If Local-only Mode is enabled, the command fails with `LOCAL_ONLY_MODE`.

Revoking consent in the GUI immediately revokes it for CLI commands.

---

### `plaudon jobs status`

Show background job status.

#### Usage

```bash
plaudon jobs status [OPTIONS]
```

#### Options

- **`--recording <UUID>`**: Show only jobs for this recording (optional).
- **`--limit <N>`**: Maximum number of jobs to return. Default: 50. Range: 1–1000.

#### Example

```bash
plaudon jobs status --limit 10
```

#### Example output

```json
{
  "status": "ok",
  "contract_version": 1,
  "command": "jobs",
  "data": {
    "jobs": [
      {
        "id": "job-abc123",
        "type": "transcribe",
        "recording_id": "123e4567-e89b-12d3-a456-426614174000",
        "state": "completed",
        "created_at": "2026-10-11T10:00:00Z",
        "started_at": "2026-10-11T10:00:05Z",
        "finished_at": "2026-10-11T10:03:42Z",
        "retry_count": 0,
        "last_error": null
      },
      {
        "id": "job-def456",
        "type": "summarize",
        "recording_id": "223e4567-e89b-12d3-a456-426614174001",
        "state": "failed",
        "created_at": "2026-10-11T09:30:00Z",
        "started_at": "2026-10-11T09:30:10Z",
        "finished_at": "2026-10-11T09:30:45Z",
        "retry_count": 2,
        "last_error": {
          "code": "TRANSCRIPT_NOT_FOUND",
          "message": "Cannot summarize: recording has no completed transcript"
        }
      }
    ],
    "total": 2,
    "limit": 10
  }
}
```

- **`jobs`**: Array of job summaries.
  - **`id`**: Job identifier.
  - **`type`**: Job type (`transcribe`, `summarize`, `diarize`, `import`).
  - **`recording_id`**: UUID of the recording (nullable).
  - **`state`**: Job state (`queued`, `running`, `completed`, `failed`, `cancelled`).
  - **`created_at`**: ISO-8601 timestamp when the job was created.
  - **`started_at`**: ISO-8601 timestamp when the job started (nullable).
  - **`finished_at`**: ISO-8601 timestamp when the job finished (nullable).
  - **`retry_count`**: Number of retries attempted.
  - **`last_error`**: Last error (nullable). Object with:
    - **`code`**: Error code.
    - **`message`**: Human-readable message.
- **`total`**: Total number of jobs matching the query.
- **`limit`**: Limit used in the query.

---

### `plaudon bridge status`

Report whether the scripting bridge is enabled and who owns it.

#### Usage

```bash
plaudon bridge status
```

#### Example output (enabled)

```json
{
  "status": "ok",
  "contract_version": 1,
  "command": "bridge",
  "data": {
    "enabled": true,
    "owner": "alice",
    "enabled_at": "2026-10-11T08:00:00Z"
  }
}
```

#### Example output (disabled)

```json
{
  "status": "ok",
  "contract_version": 1,
  "command": "bridge",
  "data": {
    "enabled": false,
    "owner": null,
    "enabled_at": null
  }
}
```

- **`enabled`**: Boolean.
- **`owner`**: OS username of the user who enabled the bridge (nullable).
- **`enabled_at`**: ISO-8601 timestamp when the bridge was enabled (nullable).

---

### `plaudon bridge enable`

Enable the scripting bridge for the current OS user.

#### Usage

```bash
plaudon bridge enable --confirm
```

The `--confirm` flag is required to prevent accidental enablement.

#### Example output

```json
{
  "status": "ok",
  "contract_version": 1,
  "command": "bridge",
  "data": {
    "enabled": true,
    "owner": "alice",
    "enabled_at": "2026-10-11T10:15:23Z"
  }
}
```

---

### `plaudon bridge disable`

Disable the scripting bridge.

#### Usage

```bash
plaudon bridge disable
```

#### Example output

```json
{
  "status": "ok",
  "contract_version": 1,
  "command": "bridge",
  "data": {
    "enabled": false,
    "owner": null,
    "enabled_at": null
  }
}
```

---

## Error codes

Common error codes:

- **`BRIDGE_DISABLED`**: The scripting bridge is disabled; run `plaudon bridge enable --confirm` to enable.
- **`BRIDGE_WRONG_USER`**: The current OS user does not own the bridge; only the user who enabled it can use it.
- **`CONSENT_REQUIRED`**: Cloud processing requires consent for the selected model/provider; grant it in Settings → AI Providers.
- **`LOCAL_ONLY_MODE`**: The requested operation requires network access, but Local-only Mode is enabled.
- **`RECORDING_NOT_FOUND`**: The specified recording UUID does not exist.
- **`TRANSCRIPT_NOT_FOUND`**: The recording has no completed transcript; run `plaudon process --stage transcribe` first.
- **`INVALID_MODEL`**: The specified model is not available or not supported for this stage.
- **`LIBRARY_NOT_FOUND`**: The library database could not be opened at the specified path.
- **`PERSISTENCE_ERROR`**: A database error occurred (e.g., disk full, corrupted database).

---

## No secrets in output

The CLI output envelope **never contains**:

- API keys (OpenRouter, PLAUD Cloud).
- Session tokens or signed URLs.
- Encryption keys or keychain material.
- Full transcript text in error messages (only recording IDs and metadata).

This is enforced by construction: every payload is a typed Rust struct, and the `output.rs` module is the single place where output is produced.

---

## No network port, no server, no daemon

The `plaudon` CLI is a command-line program, not a server:

- It opens no network port.
- It runs no HTTP listener.
- It installs no background daemon.
- Each invocation starts the CLI, executes one command, prints the result, and exits.

If the desktop application is not running, the CLI can still run (it opens the library database directly). If the application is running, both share the same SQLite database via WAL mode.

---

## Signature verification

The `plaudon` binary is part of the signed application bundle on macOS (code-signed and notarized) and the signed installer on Windows (Authenticode). A release gate (`scripts/verify-cli-signature.sh`) ensures that the distributed binary is signed and that the signature is valid.

If the binary is copied to another location, the signature may be invalidated (macOS Gatekeeper quarantine, Windows SmartScreen). Users should invoke the binary from its installed location or verify the signature after copying.

---

## Scripting examples

### List all recordings and count completed transcripts

```bash
plaudon list --limit 1000 | jq '.data.recordings | map(select(.transcript_status == "completed")) | length'
```

### Export a transcript to a file

```bash
plaudon export --recording 123e4567-e89b-12d3-a456-426614174000 --format text --out transcript.txt
```

### Queue transcription for all pending recordings

```bash
plaudon list --limit 1000 | jq -r '.data.recordings[] | select(.transcript_status == "pending") | .id' | while read id; do
  plaudon process --recording "$id" --stage transcribe --run local
done
```

### Check job status and retry failed jobs

```bash
plaudon jobs status --limit 100 | jq -r '.data.jobs[] | select(.state == "failed") | .id' | while read job_id; do
  # Note: job retry is not implemented; re-queue the recording instead
  recording_id=$(plaudon jobs status --limit 100 | jq -r ".data.jobs[] | select(.id == \"$job_id\") | .recording_id")
  echo "Re-queueing recording $recording_id"
  plaudon process --recording "$recording_id" --stage transcribe --run local
done
```

---

## Platform notes

- **macOS:** The CLI is in the application bundle at `plaudon.app/Contents/MacOS/plaudon`. Add it to `$PATH` or invoke via full path.
- **Windows:** The CLI is `plaudon.exe` in the installation directory (e.g., `C:\Program Files\plaudon\plaudon.exe`). Add to `%PATH%` or invoke via full path.
- **Environment variable `$PLAUDON_LIBRARY` / `%PLAUDON_LIBRARY%`:** Overrides the default library path. Useful for scripting against a non-default library.

---

## Limitations and future work

- **No job retry command.** If a job fails, re-queue the recording with `plaudon process`.
- **No bulk operations.** Each command operates on one recording at a time; use shell loops for bulk work.
- **No progress updates.** The CLI prints one result at the end; for long jobs, check `plaudon jobs status` periodically.
- **No watch mode.** The CLI does not poll or watch for changes; it executes one command and exits.
- **No server mode.** Spec §1.3 and ADR-0403 explicitly reject a loopback HTTP API or webhook receiver; the CLI is invoke-once-and-exit only.

Future improvements (not in V1):

- Bulk export (all recordings to one archive).
- Job retry/cancel commands.
- Streaming output for long-running commands.
- Watch mode for `jobs status`.
