//! Public compatibility matrix generated from the claim register
//! (spec 1.6, 2.5B, 22(10), 22(17), 24).
//!
//! The register in `compatibility.yaml` is the only source of a compatibility
//! string. Documentation is *generated* from it, so no cell can claim
//! `Supported` in prose while the register says otherwise, and no `Supported`
//! cell can exist without archived evidence.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// The spec 1.6 evidence vocabulary. Documentation may use no other word to
/// describe the state of a capability claim.
pub const EVIDENCE_VOCABULARY: &[&str] = &[
    "Documented",
    "Observed in upstream code",
    "Validated",
    "Supported",
    "Experimental",
    "Unsupported",
];

/// The claim register as stored in `compatibility.yaml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Register {
    /// Register schema version.
    pub schema_version: u32,
    /// Date the register was last updated.
    pub updated_at: String,
    /// Every claim cell.
    pub cells: Vec<Cell>,
}

/// One compatibility claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cell {
    /// Ingestion route, e.g. `plaud_device_ble`.
    pub source_type: String,
    /// Device vendor or `n/a`.
    pub vendor: String,
    /// Device model or `n/a`.
    pub device_model: String,
    /// Hardware revision when the claim is revision specific.
    #[serde(default)]
    pub hardware_revision: Option<String>,
    /// Firmware versions the claim covers.
    pub firmware_range: Vec<String>,
    /// Host operating system.
    pub host_os: String,
    /// Host OS versions the claim covers.
    pub os_version_range: Vec<String>,
    /// Authentication mode exercised.
    pub auth_mode: String,
    /// How authorization material is provisioned.
    pub provisioning_method: String,
    /// Transport used.
    pub transport: String,
    /// Media codec delivered by this route.
    pub codec: String,
    /// Fidelity class of the delivered media.
    pub quality_class: String,
    /// How strong the evidence behind the claim is.
    pub evidence_level: String,
    /// The claim itself.
    pub status: String,
    /// Identifier of the archived test run that supports a `supported` claim.
    #[serde(default)]
    pub test_run_id: Option<String>,
    /// When the claim was last verified.
    #[serde(default)]
    pub last_verified_at: Option<String>,
    /// Whether a real import from this cell was recorded.
    #[serde(default)]
    pub verified_import_recorded: bool,
    /// Limitations that must travel with the claim wherever it is published.
    #[serde(default)]
    pub known_limitations: Vec<String>,
}

impl Cell {
    /// Human-readable route label used as the matrix row key.
    pub fn route(&self) -> String {
        if self.device_model == "n/a" {
            self.source_type.clone()
        } else {
            format!(
                "{} {} / {}",
                self.vendor, self.device_model, self.source_type
            )
        }
    }

    /// Maps the machine status onto the spec 1.6 vocabulary.
    pub fn published_term(&self) -> &'static str {
        match self.status.as_str() {
            "supported" => "Supported",
            "experimental" => "Experimental",
            "known_unsupported" => "Unsupported",
            _ => match self.evidence_level.as_str() {
                "validated" => "Validated",
                "observed_in_upstream_code" => "Observed in upstream code",
                "documented" => "Documented",
                _ => "Unsupported",
            },
        }
    }

    /// Whether this cell makes a user-facing support claim that spec 22(17)
    /// requires archived evidence for.
    pub fn is_support_claim(&self) -> bool {
        self.status == "supported"
    }
}

/// Errors raised while loading or gating the register.
#[derive(Debug, thiserror::Error)]
pub enum CompatError {
    /// The register file could not be read.
    #[error("cannot read {path}: {source}")]
    Io {
        /// Offending path.
        path: String,
        /// Underlying error.
        source: std::io::Error,
    },
    /// The register is not valid YAML for this schema.
    #[error("cannot parse {path}: {detail}")]
    Parse {
        /// Offending path.
        path: String,
        /// Parser message.
        detail: String,
    },
    /// A `supported` cell lacks the evidence spec 22(17) demands.
    #[error("compatibility cell `{route}` claims Supported without evidence: {missing}")]
    UnbackedSupportClaim {
        /// Route label of the offending cell.
        route: String,
        /// What is missing.
        missing: String,
    },
}

/// Loads the register from disk.
pub fn load(path: &Path) -> Result<Register, CompatError> {
    let text = std::fs::read_to_string(path).map_err(|source| CompatError::Io {
        path: path.display().to_string(),
        source,
    })?;
    serde_norway::from_str(&text).map_err(|e| CompatError::Parse {
        path: path.display().to_string(),
        detail: e.to_string(),
    })
}

/// Fails when any `supported` cell lacks a test run id, verification date or
/// recorded verified import (spec 2.5B promotion rules, 22(17), 24).
///
/// `evidence_ids` is the set of evidence entry ids present in the release
/// evidence bundle; a `supported` cell whose `test_run_id` is absent from that
/// set is a release failure, because the claim has no archived proof.
pub fn gate_support_claims(
    register: &Register,
    evidence_ids: &[String],
) -> Result<(), CompatError> {
    for cell in register.cells.iter().filter(|c| c.is_support_claim()) {
        let mut missing = Vec::new();
        if cell.evidence_level != "observed_on_identified_hardware" {
            missing.push(format!("evidence_level is `{}`", cell.evidence_level));
        }
        if cell.firmware_range.is_empty() {
            missing.push("empty firmware_range".to_owned());
        }
        if cell.os_version_range.is_empty() {
            missing.push("empty os_version_range".to_owned());
        }
        if cell.last_verified_at.is_none() {
            missing.push("no last_verified_at".to_owned());
        }
        if !cell.verified_import_recorded {
            missing.push("no verified import recorded".to_owned());
        }
        match cell.test_run_id.as_deref() {
            None => missing.push("no test_run_id".to_owned()),
            Some(id) if !evidence_ids.iter().any(|known| known == id) => {
                missing.push(format!("test_run_id `{id}` has no evidence bundle entry"));
            }
            Some(_) => {}
        }
        if !missing.is_empty() {
            return Err(CompatError::UnbackedSupportClaim {
                route: cell.route(),
                missing: missing.join("; "),
            });
        }
    }
    Ok(())
}

/// Renders the public compatibility matrix as Markdown.
///
/// The output is deterministic: rows are grouped by route and sorted, so a
/// register edit produces a reviewable diff.
pub fn render_markdown(register: &Register) -> String {
    let mut grouped: BTreeMap<String, Vec<&Cell>> = BTreeMap::new();
    for cell in &register.cells {
        grouped.entry(cell.route()).or_default().push(cell);
    }

    let mut out = String::new();
    out.push_str("<!-- GENERATED FILE: run `plaudon-release compat-matrix` to regenerate. -->\n");
    out.push_str(
        "<!-- Source of truth: compatibility.yaml (spec 2.5B). Do not edit by hand. -->\n\n",
    );
    out.push_str("# Compatibility matrix\n\n");
    let _ = writeln!(
        out,
        "Register schema version {}, last updated {}.\n",
        register.schema_version, register.updated_at
    );
    out.push_str(
        "Every cell uses the spec 1.6 evidence vocabulary: `Documented`, `Observed in upstream code`, \
         `Validated`, `Supported`, `Experimental`, `Unsupported`. A `Supported` cell exists only when \
         an archived evidence record on identified hardware backs it; otherwise the honest term is used \
         instead. A route we have not evaluated on identified hardware is published as `Unsupported`: \
         the product fails closed rather than inviting a user to rely on an unproven path.\n\n",
    );

    for (route, cells) in &grouped {
        let _ = writeln!(out, "## {route}\n");
        out.push_str("| Host OS | OS versions | Firmware | Auth mode | Transport | Media | Claim | Evidence |\n");
        out.push_str("|---|---|---|---|---|---|---|---|\n");
        let mut sorted = cells.clone();
        sorted.sort_by(|a, b| {
            (&a.host_os, &a.firmware_range, &a.auth_mode).cmp(&(
                &b.host_os,
                &b.firmware_range,
                &b.auth_mode,
            ))
        });
        for cell in &sorted {
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {} | {} ({}) | **{}** | {} |",
                cell.host_os,
                cell.os_version_range.join(", "),
                cell.firmware_range.join(", "),
                cell.auth_mode,
                cell.transport,
                cell.codec,
                cell.quality_class,
                cell.published_term(),
                cell.test_run_id.as_deref().unwrap_or("none archived"),
            );
        }
        out.push('\n');
        let limitations: Vec<&String> = sorted
            .iter()
            .flat_map(|cell| cell.known_limitations.iter())
            .collect();
        if !limitations.is_empty() {
            out.push_str("Known limitations:\n\n");
            let mut seen = Vec::new();
            for limitation in limitations {
                if seen.contains(&limitation) {
                    continue;
                }
                seen.push(limitation);
                let _ = writeln!(out, "- {limitation}");
            }
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(status: &str) -> Cell {
        Cell {
            source_type: "plaud_device_ble".to_owned(),
            vendor: "PLAUD".to_owned(),
            device_model: "Note".to_owned(),
            hardware_revision: None,
            firmware_range: vec!["V0095".to_owned()],
            host_os: "macos".to_owned(),
            os_version_range: vec!["27.0".to_owned()],
            auth_mode: "legacy_static_token".to_owned(),
            provisioning_method: "imported_existing_token".to_owned(),
            transport: "ble".to_owned(),
            codec: "opus".to_owned(),
            quality_class: "compressed_device_transfer".to_owned(),
            evidence_level: "observed_on_identified_hardware".to_owned(),
            status: status.to_owned(),
            test_run_id: Some("run-1".to_owned()),
            last_verified_at: Some("2026-10-01".to_owned()),
            verified_import_recorded: true,
            known_limitations: vec!["Compressed sidecar, not the original WAV.".to_owned()],
        }
    }

    fn register(cells: Vec<Cell>) -> Register {
        Register {
            schema_version: 1,
            updated_at: "2026-10-11".to_owned(),
            cells,
        }
    }

    #[test]
    fn the_committed_register_parses_and_passes_its_own_gate() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("repo root")
            .to_path_buf();
        let register = load(&root.join("compatibility.yaml")).expect("register parses");
        assert!(!register.cells.is_empty(), "the register must list cells");

        // Every `supported` cell must name a test run, and the release
        // evidence bundle must contain an entry with that id. The release
        // pipeline writes one per claim from the acceptance run; here the
        // claimed ids are collected from the register itself, which proves
        // the gate is satisfiable and that no cell is missing an id.
        let claimed: Vec<String> = register
            .cells
            .iter()
            .filter(|cell| cell.is_support_claim())
            .map(|cell| {
                cell.test_run_id.clone().unwrap_or_else(|| {
                    panic!("`{}` claims Supported with no test_run_id", cell.route())
                })
            })
            .collect();
        gate_support_claims(&register, &claimed).expect("register must satisfy its own gate");

        // …and the same register fails the moment the evidence is absent.
        if !claimed.is_empty() {
            gate_support_claims(&register, &[])
                .expect_err("a Supported cell without archived evidence must fail (spec 22(17))");
        }
    }

    #[test]
    fn a_supported_cell_without_an_evidence_entry_fails_the_gate() {
        let register = register(vec![cell("supported")]);
        let error = gate_support_claims(&register, &[]).expect_err("must fail");
        let CompatError::UnbackedSupportClaim { missing, .. } = error else {
            panic!("expected an unbacked support claim");
        };
        assert!(missing.contains("run-1"), "{missing}");
    }

    #[test]
    fn a_supported_cell_with_its_evidence_entry_passes() {
        let register = register(vec![cell("supported")]);
        gate_support_claims(&register, &["run-1".to_owned()]).expect("passes");
    }

    #[test]
    fn a_supported_cell_without_hardware_evidence_level_fails() {
        let mut supported = cell("supported");
        supported.evidence_level = "implemented_not_verified".to_owned();
        let error = gate_support_claims(&register(vec![supported]), &["run-1".to_owned()])
            .expect_err("must fail");
        assert!(error.to_string().contains("evidence_level"));
    }

    #[test]
    fn an_unevaluated_cell_never_renders_as_supported() {
        let mut unevaluated = cell("not_evaluated");
        unevaluated.evidence_level = "implemented_not_verified".to_owned();
        let markdown = render_markdown(&register(vec![unevaluated]));
        assert!(markdown.contains("**Unsupported**"), "{markdown}");
        assert!(!markdown.contains("**Supported**"));
    }

    #[test]
    fn rendering_is_deterministic_and_carries_limitations() {
        let register = register(vec![cell("supported"), cell("not_evaluated")]);
        let first = render_markdown(&register);
        assert_eq!(first, render_markdown(&register));
        assert!(first.contains("Known limitations:"));
        assert!(first.contains("not the original WAV"));
    }

    #[test]
    fn every_published_term_is_in_the_spec_1_6_vocabulary() {
        for status in [
            "supported",
            "experimental",
            "known_unsupported",
            "not_evaluated",
        ] {
            let term = cell(status).published_term();
            assert!(
                EVIDENCE_VOCABULARY.contains(&term),
                "{term} is not spec 1.6 vocabulary"
            );
        }
    }
}
