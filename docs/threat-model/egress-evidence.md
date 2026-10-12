# Local-only egress evidence (D2.3)

Spec: §16.2A (local-only egress assurance), §1.5 (zero PLAUD/OpenRouter HTTP in
Local-only Mode, automated network-deny test), §18.6A (Local-only adversarial
cluster), §22(15) (Definition of Done), §24 (release evidence bundle).

This document describes the two ways we check the claim

> In Local-only Mode the application initiates no PLAUD, OpenRouter, telemetry,
> update-check or model-download request, and never launches the PLAUD Cloud
> CLI,

what each of them actually proves, and — just as importantly — what neither of
them proves.

---

## 1. The hermetic in-process fixture (runs in CI)

```bash
cargo test -p plaudon-egress-harness     # the test CI runs
scripts/egress-deny-test.sh              # the same, plus evidence collection
```

No network access is required or used. Every destination used by the fixture is
in a reserved documentation range (`203.0.113.0/24`, RFC 5737) or under the
reserved `.invalid` TLD (RFC 6761).

### How it works

| Component | Role |
|---|---|
| `tests/egress/monitor` | `cdylib` injected into the scenario process; intercepts libc, records every call, refuses every non-loopback internet destination |
| `tests/egress/scenario` | binary that drives one subsystem through the real `plaudon_security::outbound` gate under Local-only Mode, plus `voice-profile-match`, which repeats its attempts against a second, network-permitted policy |
| `tests/egress/harness` | builds both, runs each scenario as a child process, parses the trace, applies the acceptance rules, writes the evidence artifact |

The monitor is injected with `DYLD_INSERT_LIBRARIES` (macOS, via
`__DATA,__interpose` records) or `LD_PRELOAD` (Linux, via exported symbol
overrides resolved with `dlsym(RTLD_NEXT, …)`). It intercepts:

* `connect`, `connectx`, `sendto` — connection initiation;
* `getaddrinfo`, `gethostbyname`, `res_query`/`res_nquery` — name resolution;
* `socket` — socket creation (recorded, never refused: creating a socket
  transmits nothing and the destination is not yet known);
* `execve`, `execvp`, `posix_spawn`, `posix_spawnp` — process execution.

The hooks live behind the monitor's non-default `interpose` feature, so the
injected artifact is built with
`cargo build -p plaudon-egress-monitor --features interpose` (the harness does
this itself). Without the feature the crate is an ordinary library of
wire-format helpers — which is how the harness links it, so that linking the
monitor cannot accidentally interpose the harness's own libc calls. A dylib
built without the feature installs nothing, writes no `monitor_ready` line,
and is therefore reported as `MonitorUnavailable`: it fails loudly rather than
passing vacuously.

Each call appends one JSON line to `$PLAUDON_EGRESS_LOG` with a single
`write(2)` to an `O_APPEND` file descriptor, so concurrent writers stay
line-atomic:

```json
{"ts_unix":1760000000,"call":"connect","family":"inet","host_or_ip":"203.0.113.1","port":443,"denied":true}
{"ts_unix":1760000000,"call":"exec","family":"none","host_or_ip":"","port":0,"denied":false,"program":"/usr/local/bin/plaud"}
```

Loopback (`127.0.0.0/8`, `::1`, IPv4-mapped loopback) and non-internet address
families (`AF_UNIX` and friends) are permitted and recorded as `denied:false`.
Everything else is refused with `EPERM`, or `EAI_FAIL` for resolver calls, and
recorded as `denied:true`. The refusal matters: on a networked CI machine a
regression cannot complete a connection, and the attempt is still recorded.

### Scenarios

| Scenario | §18.6A hostile case | Required outcome |
|---|---|---|
| `local-import` | — | a file is really staged through `fsguard::PrivateTempDir` and committed with `StagedFile::commit_by_rename`, with zero network calls |
| `openrouter-dispatch` | Cloud | gate refuses, capability explained |
| `update-check` | update checker | gate refuses, capability explained |
| `model-refresh`, `model-download` | Apple/model fetch | gate refuses, capability explained |
| `telemetry-flush` | telemetry | gate refuses, capability explained |
| `apple-asset-fetch` | Apple model fetch | gate refuses, capability explained |
| `cloud-cli` | CLI subprocess | launcher refuses **and** no `exec` record exists |
| `folder-placeholder` | folder placeholder | placeholder recognised without being read; capability disabled and labelled |
| `voice-profile-match` | Cloud (VP-110, TM-044) | an enrol-and-match flow runs entirely locally and reaches the gate zero times; the same speaker embedding offered to the `OpenRouter` HTTPS dispatch, the telemetry upload and the PLAUD cloud CLI is refused with `voice_biometric_never_egresses` — in Local-only Mode **and** under a permissive network grant |
| `negative-control` | — | deliberately bypasses the gate; **must** produce recorded attempts |

Each scenario sets `ok` from the deny reason the spec requires of it, and
`verify_scenario` refuses to certify a run whose scenario exited non-zero or
reported `ok: false`. So `voice-profile-match` fails if the gate answers with
any other reason — including `local_only_mode`, which would wrongly imply that
permitting the network would let the embedding out. Its second half runs
against a separate `OutboundPolicy::network_permitted` whose grant allows the
vendor hosts, approves model downloads and opts telemetry in; those decisions
are recorded in the same `decisions` array, which is why rule 5 below also
covers them.

### Acceptance rules

Applied by `plaudon_egress_harness::verify::verify_scenario`, each with its own
unit test over a synthetic trace that must fail:

1. **No observation, no pass.** The monitor writes a `monitor_ready` line from
   its library constructor. If that line is absent, the run is reported as
   `MonitorUnavailable` — a failure, never a skip. A platform with no
   interception mechanism compiled in fails with `MonitorUnsupported` unless
   `PLAUDON_EGRESS_ALLOW_UNSUPPORTED=1` is set explicitly.
2. **Zero app-initiated non-loopback attempts** in every scenario except the
   negative control.
3. **The negative control must be detected.** It opens a TCP connection to
   `203.0.113.1:443`, resolves `egress-negative-control.invalid` and executes a
   stand-in binary whose leaf name is `plaud`. If the monitor records none of
   these, the harness fails with "the egress harness is broken", because every
   other pass in that run would be meaningless.
4. **Zero cloud-CLI `exec` records** (leaf `plaud`/`plaud.exe`) under Local-only
   Mode. The negative control's deliberate `plaud` execution is what keeps this
   assertion from being vacuous.
5. **No authorized decision.** The gate's own decision log must contain no
   allowed egress at all: nothing may be authorized while
   `NetworkMode::LocalOnly` is in force, and the one scenario that also drives
   a network-permitted policy on purpose (`voice-profile-match`) must record
   only refusals there too.
6. **Disabled capabilities are explained**, not silently absent: each hostile
   case must carry a `DisabledCapability` or an `EgressDenied` rendered through
   its §15.4 error envelope (`EGRESS_DENIED`, message, action).
7. **Local work still happens.** `local-import` must commit its file; a fixture
   that passes by doing nothing is not evidence.

### Evidence artifact

Written on every run, pass or fail:

```
target/security-evidence/egress-trace.json
```

```jsonc
{
  "schema_version": "plaudon.egress-trace.v1",
  "generated_at_unix": 1760000000,
  "platform": "macos/aarch64",
  "monitor": "dyld-interpose",
  "scenarios": [
    {
      "scenario": "openrouter-dispatch",
      "monitor": "dyld-interpose",
      "platform": "macos/aarch64",
      "timestamp": 1760000000,
      "monitor_injected": true,
      "attempts": [],
      "non_loopback_attempt_count": 0,
      "socket_records": [],
      "exec_records": [],
      "decisions": [ /* plaudon_security::outbound::EgressRecord */ ],
      "disabled_capabilities": [ /* with explanations */ ],
      "explained_denials": [ /* §15.4 envelopes */ ],
      "failure": null
    }
  ],
  "observation_scope": [ "…" ],
  "not_guaranteed": [ "…" ]
}
```

This file is part of the §24 release evidence bundle.

---

## 2. The privileged packet capture (opt-in, not part of `cargo test`)

```bash
sudo scripts/egress-tcpdump.sh openrouter-dispatch
sudo scripts/egress-tcpdump.sh negative-control   # self-test: packets expected
```

The script captures with `tcpdump` while one scenario runs, then re-reads the
capture and asserts zero packets to non-loopback, non-link-local, non-multicast
destinations attributable to the scenario. The pcap and a JSON report are stored
under `target/security-evidence/`.

* **macOS**: captures on `pktap,all` in pcapng form and attributes packets to
  the scenario with the pktap process-id metadata filter (`-Q "pid = …"`), so
  unrelated host traffic is excluded.
* **Linux**: captures on `any`. The kernel capture carries no process
  attribution, so attribution is by capture window only; unrelated host traffic
  in that window will appear in the report. The report states this.

By default the in-process monitor is **not** injected, so what is measured is
the application's own `plaudon_security::outbound` gate seen from outside the
process. Pass `--with-monitor` to capture with the monitor injected too.

If the script lacks privileges, or `tcpdump` is missing, or the host OS is
unsupported, it prints an explicit error and exits non-zero (3 or 4). It never
reports a silent skip, because "we did not look" must never be recorded as
"nothing happened". Note that running `negative-control` this way deliberately
emits a real packet toward a reserved documentation address.

---

## 3. What this evidence does **not** prove

Stated explicitly, per §16.2A and §16.6; the same list is embedded in the
evidence artifact.

* **It observes this process.** It says nothing about the operating system,
  OS update agents, platform stores, browser activity, or any other
  application on the machine.
* **Injection has platform limits.** dyld strips `DYLD_*` from the environment
  of hardened-runtime, setuid and platform binaries; `LD_PRELOAD` is ignored
  for setuid and statically linked binaries. The monitor therefore observes the
  scenario binary we build, not system daemons. A run in which injection did
  not happen is reported as a failure, not as a clean result.
* **A cloud-synced folder is outside the boundary.** If the user selects a
  folder backed by iCloud Drive, OneDrive, Dropbox or similar, that client is a
  separate process and may transfer data regardless of this application's
  settings. Local-only Mode disables or labels such folders; it cannot stop the
  sync client. The `folder-placeholder` scenario demonstrates the disabled,
  explained capability — it does not demonstrate control over the sync client.
* **Sends on an already-connected socket are not individually recorded.** The
  `connect` that must precede them is, which is what the claim rests on.
* **It is a statement about the build under test**, on the platform named in
  the artifact, for the scenarios listed in it. It is not a general statement
  about any other build or configuration.
* **It is not a cryptographic proof and not an audit.** It is a reproducible
  observation, with the trace retained so that a reader can check it.

---

## 4. Reproducing a failure

To confirm the fixture still has teeth:

```bash
# 1. The negative control proves the monitor sees real traffic.
cargo test -p plaudon-egress-harness negative_control_proves_the_fixture_can_fail

# 2. An un-observed run is reported as a failure, not a pass.
cargo test -p plaudon-egress-harness an_unobserved_run_is_reported_as_monitor_unavailable

# 3. Packet-level self-test (privileged; emits one packet to 203.0.113.1).
sudo scripts/egress-tcpdump.sh negative-control
```

Per-scenario traces from the last run are kept at
`target/egress-fixture/<scenario>-<n>/trace.jsonl` with the matching
`decisions.json`.
