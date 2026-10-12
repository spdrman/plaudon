//! Spec 18.7 acceptance-scenario registry (task N1.2).
//!
//! Spec 18.7 enumerates 44 **required** end-to-end acceptance scenarios. The
//! danger is not a failing scenario; it is a scenario that quietly stops
//! existing. The canonical titles live here as a constant, the registry of
//! what is actually implemented lives in `tests/e2e/scenarios.toml`, and a
//! test asserts the two agree one-for-one.
//!
//! A scenario may be `implemented`, `opt-in` (hardware or real provider, never
//! run by default and never needing a CI secret) or `blocked-on` another task.
//! `blocked-on` is honest bookkeeping, not an excuse: the release gate counts
//! it as not covered.

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// The 44 required scenarios of spec 18.7, in spec order.
///
/// The text is the spec's own wording, shortened only where it does not change
/// the meaning. Index + 1 is the spec's scenario number.
pub const REQUIRED_SCENARIOS: [&str; 44] = [
    "Fresh macOS install, manual MP3 import, local STT, local output if eligible, export Markdown with network blocked.",
    "Fresh Windows install, manual WAV import, whisper.cpp STT, transcript edit, export JSON with network blocked.",
    "PLAUD CLI absent -> actionable setup and functional manual-import fallback.",
    "PLAUD CLI authenticated -> supported record listing, import, deduplicate same ID/file/hash, view playable original.",
    "PLAUD auth expires mid-poll -> safe pause and re-auth prompt; no dropped work.",
    "Download interrupted at every staged checkpoint -> resume or restart safely; no false-completed record.",
    "Cloud STT denied by policy -> zero outgoing audio bytes.",
    "First OpenRouter cloud job -> explicit consent, price/retention disclosure, successful result, actual usage if supported.",
    "OpenRouter request times out after dispatch -> outcome uncertain; no automatic duplicate-billed retry.",
    "Apple native model unavailable -> clear explanation and whisper.cpp fallback without changing cloud policy.",
    "Transcript corrected -> new revision, old revision retrievable, previous summary labeled stale.",
    "Model fails JSON Schema -> visible failure or bounded repair; no fabricated structured fields.",
    "Action item lacking date -> `not specified`; evidence anchor resolves to original passage.",
    "Restart/kill mid-job -> recovered queue, no partial output presented as final.",
    "User revokes cloud consent during queue -> no new cloud dispatch after revocation.",
    "Delete recording -> removes intended managed derivatives, retains no dangling DB references.",
    "Export/restore -> restored media hashes and structured records match source manifest.",
    "Accessibility -> all critical flows keyboard-completable, tested with screen readers.",
    "Watched folder: file written slowly to an iCloud/OneDrive-style folder -> no premature import, no source deletion, exactly one archived asset.",
    "Watched folder: rename, change and repeat events -> deterministic reconciliation, no duplicates or orphaned derivatives.",
    "PLAUD source metadata changed or remotely trashed -> local audio retained, status reflects source state, user tombstone respected.",
    "Cloud STT request-style mismatch -> model marked incompatible rather than trying invalid endpoint or uploading unexpectedly.",
    "User switches STT model for one recording -> new revision, original human edits and prior results remain intact.",
    "Local OS notice permitted/denied -> correct notification or in-app activity entry with privacy-safe text.",
    "Q&A / study tool groundedness -> invalid anchors rejected, unsupported answers labeled, fixed transcript revision logged.",
    "Portable archive exported and restored across OSes -> full media, transcripts, outputs and SHA matches; secret material absent.",
    "Archive compromised by traversal, duplicate paths, decompression bomb or altered hash -> safe reject and no partial archive mutation.",
    "PLAUD social-login incompatibility or region ambiguity -> actionable fallback without token-copy requests.",
    "Background cloud processing queue + revoked consent -> no billing dispatch, even when a watched-folder event fires.",
    "Delete and rescan -> user-deleted local recording stays deleted until explicit user restore/import confirmation.",
    "On a verified legacy PLAUD Note/firmware, import `.ASR` through direct BLE into a staged object; decode and locally transcribe with external networking disabled.",
    "macOS Bluetooth permission denied or revoked during scan -> actionable recovery; no app crash or unauthorized access attempt.",
    "Windows BLE radio unavailable/reconnect after suspend -> persistent job state and successful clean restart.",
    "Wrong or absent device secret -> no vendor file command; no fallback to anonymous data extraction or PLAUD Cloud.",
    "New encrypted-mode firmware -> `UNSUPPORTED_FIRMWARE`/`AUTH_MODE_UNSUPPORTED`, no legacy downgrade and no token leak.",
    "Device already synced with phone and returns no unsynced recordings -> explicit incompleteness explanation; no fabricated recordings.",
    "Transfer interrupted at every chunk boundary -> verified resume if supported or safe restart; `.part` never becomes committed archive media.",
    "Original-note mounted USB volume (where supported) -> read-only import preserves full WAV source and never mutates the filesystem.",
    "Note Pro/NotePin S unsupported USB -> no misleading instruction to mount them; relevant supported alternatives offered.",
    "An attacker advertises `PLAUD_NOTE` or replays a malformed BLE frame -> identity/auth validation rejects or marks unverified; no crash or out-of-bounds allocation.",
    "Direct-device import under Local-only Mode -> zero HTTP calls, no model downloads, no PLAUD account login, no OpenRouter dispatch, verified local STT and outputs.",
    "Read-only acquisition emits no `deleteFile`, `clearAllFile`, recording control, settings write, device privacy toggle or OTA opcode; a golden opcode trace proves the restriction.",
    "Same recording arrives from BLE, legacy USB and cloud export -> source provenance preserved, content-addressed dedupe by actual bytes; quality variants not incorrectly collapsed.",
    "A firmware update or changed BLE service map invalidates cached capability; rescan requires verification, not unprompted authorization reuse.",
];

/// Implementation status of one scenario.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum ScenarioStatus {
    /// Runs automatically in CI on every matrix leg listed.
    Implemented {
        /// Runner the scenario executes on: `rust` for a `plaudon-e2e`
        /// acceptance test, `kittest` for an `egui_kittest` test in
        /// `apps/plaudon-egui/tests`, or `gate` for a pipeline gate. The
        /// `playwright` runner disappeared with the Tauri/React shell at the
        /// EPIC O cutover (ADR-0024).
        harness: String,
        /// Test identifier the harness dispatches on.
        test: String,
    },
    /// Requires real hardware or a real provider account. Never executes by
    /// default and never requires a CI secret (spec 18.3, 18.3A).
    OptIn {
        /// Environment variable that opts the scenario in.
        opt_in_env: String,
        /// Why it cannot run hermetically.
        reason: String,
    },
    /// Not yet covered because the feature it exercises is not finished.
    BlockedOn {
        /// Task code the coverage waits on, e.g. `L2.3`.
        task: String,
        /// What is missing.
        reason: String,
    },
}

/// One registry entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScenarioEntry {
    /// Spec 18.7 scenario number, 1-based.
    pub number: usize,
    /// Spec wording, asserted to match [`REQUIRED_SCENARIOS`].
    pub title: String,
    /// Operating systems the scenario must run on when implemented.
    #[serde(default)]
    pub platforms: Vec<String>,
    /// Implementation status.
    #[serde(flatten)]
    pub status: ScenarioStatus,
}

/// The registry file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registry {
    /// Registry schema version.
    pub schema_version: u32,
    /// Quarantine policy text; a flaky scenario is quarantined explicitly, so
    /// nobody silences it with a retry loop.
    pub quarantine_policy: String,
    /// Every scenario.
    pub scenarios: Vec<ScenarioEntry>,
}

/// Registry defects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryError {
    /// A required scenario number has no entry.
    MissingScenario(usize),
    /// An entry exists for a number spec 18.7 does not define.
    UnknownScenario(usize),
    /// An entry's title drifted from the spec wording.
    TitleDrift {
        /// Scenario number.
        number: usize,
        /// Title found in the registry.
        found: String,
    },
    /// Two entries claim the same number.
    DuplicateScenario(usize),
    /// An opt-in scenario named no opt-in environment variable.
    OptInWithoutSwitch(usize),
    /// An implemented scenario listed no platform.
    ImplementedWithoutPlatform(usize),
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingScenario(n) => write!(
                f,
                "spec 18.7 scenario {n} has no registry entry; a required scenario may not go missing"
            ),
            Self::UnknownScenario(n) => {
                write!(f, "registry entry {n} is not a spec 18.7 scenario number")
            }
            Self::TitleDrift { number, found } => write!(
                f,
                "scenario {number} title drifted from spec 18.7; registry says: {found}"
            ),
            Self::DuplicateScenario(n) => write!(f, "scenario {n} is listed twice"),
            Self::OptInWithoutSwitch(n) => write!(
                f,
                "scenario {n} is opt-in but names no opt-in environment variable"
            ),
            Self::ImplementedWithoutPlatform(n) => write!(
                f,
                "scenario {n} is implemented but lists no platform to run on"
            ),
        }
    }
}

/// Errors raised while loading the registry.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    /// The registry file could not be read.
    #[error("cannot read {path}: {source}")]
    Io {
        /// Offending path.
        path: String,
        /// Underlying error.
        source: std::io::Error,
    },
    /// The registry file could not be parsed.
    #[error("cannot parse {path}: {detail}")]
    Parse {
        /// Offending path.
        path: String,
        /// Parser message.
        detail: String,
    },
}

/// Loads `tests/e2e/scenarios.toml`.
pub fn load(path: &Path) -> Result<Registry, LoadError> {
    let text = std::fs::read_to_string(path).map_err(|source| LoadError::Io {
        path: path.display().to_string(),
        source,
    })?;
    toml::from_str(&text).map_err(|e| LoadError::Parse {
        path: path.display().to_string(),
        detail: e.to_string(),
    })
}

/// Validates a registry against spec 18.7.
pub fn validate(registry: &Registry) -> Vec<RegistryError> {
    let mut errors = Vec::new();
    let mut seen: BTreeSet<usize> = BTreeSet::new();

    for entry in &registry.scenarios {
        if !seen.insert(entry.number) {
            errors.push(RegistryError::DuplicateScenario(entry.number));
            continue;
        }
        let Some(expected) = REQUIRED_SCENARIOS.get(entry.number.wrapping_sub(1)) else {
            errors.push(RegistryError::UnknownScenario(entry.number));
            continue;
        };
        if entry.title != *expected {
            errors.push(RegistryError::TitleDrift {
                number: entry.number,
                found: entry.title.clone(),
            });
        }
        match &entry.status {
            ScenarioStatus::OptIn { opt_in_env, .. } if opt_in_env.trim().is_empty() => {
                errors.push(RegistryError::OptInWithoutSwitch(entry.number));
            }
            ScenarioStatus::Implemented { .. } if entry.platforms.is_empty() => {
                errors.push(RegistryError::ImplementedWithoutPlatform(entry.number));
            }
            _ => {}
        }
    }

    for number in 1..=REQUIRED_SCENARIOS.len() {
        if !seen.contains(&number) {
            errors.push(RegistryError::MissingScenario(number));
        }
    }

    errors.sort_by_key(|error| match error {
        RegistryError::MissingScenario(n)
        | RegistryError::UnknownScenario(n)
        | RegistryError::DuplicateScenario(n)
        | RegistryError::OptInWithoutSwitch(n)
        | RegistryError::ImplementedWithoutPlatform(n)
        | RegistryError::TitleDrift { number: n, .. } => *n,
    });
    errors
}

/// Coverage counts for the release evidence bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Coverage {
    /// Scenarios running automatically.
    pub implemented: usize,
    /// Scenarios gated behind hardware or a real provider.
    pub opt_in: usize,
    /// Scenarios waiting on unfinished work.
    pub blocked: usize,
}

/// Summarises a registry.
pub fn coverage(registry: &Registry) -> Coverage {
    let mut counts = Coverage::default();
    for entry in &registry.scenarios {
        match entry.status {
            ScenarioStatus::Implemented { .. } => counts.implemented += 1,
            ScenarioStatus::OptIn { .. } => counts.opt_in += 1,
            ScenarioStatus::BlockedOn { .. } => counts.blocked += 1,
        }
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_root() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("repo root")
            .to_path_buf()
    }

    fn entry(number: usize) -> ScenarioEntry {
        ScenarioEntry {
            number,
            title: REQUIRED_SCENARIOS[number - 1].to_owned(),
            platforms: vec!["macos".to_owned()],
            status: ScenarioStatus::Implemented {
                harness: "rust".to_owned(),
                test: format!("scenario_{number}"),
            },
        }
    }

    fn full_registry() -> Registry {
        Registry {
            schema_version: 1,
            quarantine_policy: "documented".to_owned(),
            scenarios: (1..=44).map(entry).collect(),
        }
    }

    #[test]
    fn the_spec_defines_forty_four_scenarios() {
        assert_eq!(REQUIRED_SCENARIOS.len(), 44);
    }

    #[test]
    fn a_complete_registry_validates() {
        let errors = validate(&full_registry());
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn a_missing_scenario_is_reported() {
        let mut registry = full_registry();
        registry.scenarios.retain(|entry| entry.number != 27);
        assert_eq!(
            validate(&registry),
            vec![RegistryError::MissingScenario(27)]
        );
    }

    #[test]
    fn a_drifted_title_is_reported() {
        let mut registry = full_registry();
        registry.scenarios[0].title = "something else".to_owned();
        assert!(matches!(
            validate(&registry).as_slice(),
            [RegistryError::TitleDrift { number: 1, .. }]
        ));
    }

    #[test]
    fn an_opt_in_scenario_without_a_switch_is_reported() {
        let mut registry = full_registry();
        registry.scenarios[30].status = ScenarioStatus::OptIn {
            opt_in_env: String::new(),
            reason: "real hardware".to_owned(),
        };
        assert_eq!(
            validate(&registry),
            vec![RegistryError::OptInWithoutSwitch(31)]
        );
    }

    #[test]
    fn the_committed_registry_covers_every_required_scenario() {
        let registry = load(&repo_root().join("tests/e2e/scenarios.toml")).expect("registry loads");
        let errors = validate(&registry);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(registry.scenarios.len(), 44);
    }

    #[test]
    fn scenarios_one_and_two_run_automatically_on_both_operating_systems() {
        let registry = load(&repo_root().join("tests/e2e/scenarios.toml")).expect("registry loads");
        for number in [1_usize, 2] {
            let entry = registry
                .scenarios
                .iter()
                .find(|entry| entry.number == number)
                .expect("entry");
            assert!(
                matches!(entry.status, ScenarioStatus::Implemented { .. }),
                "spec 18.7 scenario {number} must run in CI"
            );
        }
    }

    #[test]
    fn coverage_counts_each_status() {
        let mut registry = full_registry();
        registry.scenarios[5].status = ScenarioStatus::OptIn {
            opt_in_env: "PLAUDON_HARDWARE_TESTS".to_owned(),
            reason: "device".to_owned(),
        };
        registry.scenarios[6].status = ScenarioStatus::BlockedOn {
            task: "L2.3".to_owned(),
            reason: "feature unfinished".to_owned(),
        };
        assert_eq!(
            coverage(&registry),
            Coverage {
                implemented: 42,
                opt_in: 1,
                blocked: 1
            }
        );
    }
}
