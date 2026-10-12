# ADR-0030: Job engine durability, idempotency and crash recovery

- **Status:** Accepted
- **Date:** 2026-10-11
- **Owner:** EPIC C (job engine)
- **Spec:** PLAUD BYOK Desktop v1.3 — §6.3A, §6.3B, §10.7, §13.1, §13.3A, §14, §15.3

## Context

The job engine is the only component that converts user intent into durable
state. Everything the product promises about safety — no lost recording, no
false completion, no silent double billing, a visible and resumable wait state
instead of a broken import — reduces to how this engine commits and recovers.

Three constraints shape the design:

1. SQLite cannot atomically commit a filesystem rename (§6.3B), so any durable
   work that touches both must be journalled.
2. A cloud request that was sent but whose response never arrived may already
   have been billed (§14.3), so it must never be retried without a human
   decision.
3. The frontend is an untrusted, lossy consumer (§6.3A, §15.3): event delivery
   is a hint, never the system of record.

## Decisions

### 1. One enum covers both state vocabularies

§14.1 states the coarse lifecycle; §14.7 refines the persisted vocabulary.
`JobState` is a single closed enum containing the union (18 variants). The
generic §14.1 `RUNNING` is represented by `RunningLocal` and `CloudSending`;
every other §14.1 name has a 1:1 member. A single enum means one transition
table, one serde contract and one exhaustiveness check for the UI.

Legal edges live in a `const TRANSITIONS` table. Three guards run inside the
same transaction as the write:

- `Completed` requires at least one `job_outputs` row committed by that very
  transaction;
- leaving `CloudOutcomeUnknown` toward another dispatch requires
  `Actor::User` **and** an explicit cost acknowledgement, so neither the
  scheduler nor the startup reconciler can re-bill;
- entering `CloudSending` requires a freshly verified consent grant.

`Committing` is deliberately **not** cancellable: it is the one window where
abandoning work could leave a torn record. A cancel requested during it is
honoured once the commit finishes.

### 2. Single-writer coordinator, concurrent readers

All writes funnel through one serialized task owning a one-connection write
pool (`synchronous = FULL`); reads use a separate WAL pool. `SQLITE_BUSY`
cannot arise from our own concurrency, and the serialization gives the event
sequence allocator a free total order.

The coordinator lives in `crates/jobs`, not `crates/persistence`, because the
post-commit event dispatch it feeds is owned by the job engine and
`plaudon-persistence` must not depend on `plaudon-jobs`.

### 3. Idempotency is a canonical key digest, not a heuristic

`JobKey` encodes every keyed dimension of §14.2 with a length-prefixed,
field-tagged byte encoding, then SHA-256. Length prefixing makes the encoding
injective, so distinct keys cannot collide before hashing. There is no
filename and no timestamp anywhere in the key. `normalization_config` is a
`BTreeMap`, so map ordering cannot perturb the digest.

The digest is stored in `jobs.job_key`, which carries the partial unique index
`ux_jobs_key`. A duplicate enqueue therefore collapses onto the existing row at
the database level, including under concurrency. An intentional reprocessing
carries a `ReprocessToken` that is itself part of the key, so a deliberate
rerun is a new job while an accidental duplicate is not.

### 4. Gap-free event sequence from a transactional counter

`AUTOINCREMENT` consumes a row id even when the transaction rolls back, which
would leave permanent holes and make "did I miss an event?" undecidable for the
UI. The sequence is therefore allocated from an `event_sequence` counter row
inside the committing transaction. A rollback leaves neither an event row nor a
consumed number: the log is strictly monotonic **and** gap-free, so any hole the
frontend observes is proof it must refetch.

Dispatch happens strictly after commit, reads only committed rows, and advances
a durable per-consumer cursor. A failing subscriber does not advance its cursor
and never affects the job.

### 5. The engine keeps its own import journal

`crates/persistence` owns `import_journal`, whose row requires the content
hash, byte length and target path up front — it models a *prepared media
intent* (§13.3A step 3 onward). The engine additionally has to journal the
steps that happen **before** the hash is known (`staged`, `hashed`, `flushed`),
because a crash there must be classified rather than guessed. It therefore owns
`job_import_journal` with the full six-step contract and links to the
persistence intent through `intent_id`.

Recovery is a pure decision table over `(step, observed files)`. `CompleteCommit`
is only ever proposed when the target bytes hash to the recorded digest;
everything else re-stages. Orphan cleanup only ever removes app-owned `*.part`
files beneath the staging root that are past the grace period.

### 6. Additive ledger columns are reconciled from Rust, not from SQL

§10.7 requires the attempt row to carry the content checksum, route, privacy
snapshot, consent id, estimated units, final cost and the provider request id.
`crates/persistence` owns `inference_attempts`. Rather than fork the table or
put an unconditional `ALTER TABLE` into a migration whose bytes are digested
and must never change, `JobStore::ensure_schema` adds each missing column only
when `PRAGMA table_info` reports it absent. The engine therefore works against
either version of the core migration, and the 0100 file stays byte-stable.

`dispatch_state` remains the single shared enum; the engine defines no second
status vocabulary.

### 7. Crash points are compile-time gated

Every durable transition (`transition:<state>`, `transition:<state>:after`),
every journal step, both commit boundaries, the post-commit dispatch window,
the staging write and the three dispatch boundaries are addressable crash
points. With the default feature set `trip` is an empty inline function and the
registry is unreachable: a release build contains no crash points. A coverage
test fails if a transition has no registered point.

The harness arms one point through an environment variable in a child process;
the point calls `abort()`, which skips unwinding, destructors and application
buffer flushing — the in-process equivalent of `SIGKILL` for durability
purposes. One Unix-only test additionally kills the child with a real signal.

## Consequences

- Every `Completed` job provably has at least one committed output row, and
  every file-backed output resolves to bytes whose digest matches.
- A possibly-billed request parks in `CloudOutcomeUnknown` forever until a
  human accepts the risk. Startup recovery can never move it.
- Interruptions (sleep, logout, keychain lock, Bluetooth loss, source loss,
  network loss, missing model asset) map to persisted wait states with a resume
  path; none of them can produce `Completed` or a silent failure.
- The frontend can be arbitrarily lossy: restart or refetch reconstructs full
  job and activity history from the database.
- The engine links no HTTP client, no HTTP server and no socket framework, and
  opens no sockets; this is asserted by a dependency-tree test and a source
  scan.

## Follow-ups owned by other epics

- `crates/persistence/src/migrations.rs` must register
  `0100_job_engine.sql` (one `Migration { version: 100, … }` entry) once that
  runner lands, so the production path applies the engine schema through the
  single digested runner.
- If `import_journal` ever relaxes its `NOT NULL` columns so a pre-hash staged
  row is representable, `job_import_journal` should be folded into it.
