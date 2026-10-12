//! `plaudon-release` — the release engineering entry point used by CI
//! (spec 18.8, 18.9, 24).
//!
//! Every subcommand is a gate: it exits non-zero with an actionable message
//! rather than printing a warning nobody reads.
//!
//! ```text
//! plaudon-release build-metadata [--artifacts <dir>] [--out <file>]
//! plaudon-release scenarios [--json]
//! plaudon-release coverage --llvm-cov <file.json>
//! plaudon-release compat-matrix [--out <file>] [--check]
//! plaudon-release doc-lint
//! plaudon-release evidence assemble --staging <dir> [--out <dir>]
//! plaudon-release evidence verify --bundle <dir> --trusted-key <id>
//! plaudon-release evidence gate --bundle <dir> --trusted-key <id>
//! ```

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use plaudon_release_tools::{buildmeta, compat, coverage, doclint, evidence, scenarios, sign};

const USAGE: &str = "\
plaudon-release <command>

  build-metadata [--artifacts <dir>] [--out <file>]
      Emit reproducible build metadata (spec 18.8).

  scenarios [--json]
      Validate the spec 18.7 acceptance-scenario registry and print coverage.

  coverage --llvm-cov <file.json>
      Enforce the spec 18.9 branch-coverage policy and transition checklist.

  compat-matrix [--out <file>] [--check]
      Generate docs/public/compatibility.md from compatibility.yaml.
      --check fails when the committed file is stale.

  doc-lint
      Lint docs/public for evidence vocabulary, forbidden claims and links.

  evidence assemble --staging <dir>
      Assemble and sign the release evidence bundle (spec 24).

  evidence verify --bundle <dir> --trusted-key <key-id>
      Verify a bundle: signature, hashes, secrets, required categories.

  evidence gate --bundle <dir> --trusted-key <key-id>
      Verify, then fail when a Supported compatibility cell lacks evidence.
";

fn repo_root() -> PathBuf {
    std::env::var_os("PLAUDON_REPO_ROOT").map_or_else(
        || {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .nth(2)
                .expect("repository root")
                .to_path_buf()
        },
        PathBuf::from,
    )
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == name)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

fn has(args: &[String], name: &str) -> bool {
    args.iter().any(|arg| arg == name)
}

fn fail(message: &str) -> ExitCode {
    eprintln!("error: {message}");
    ExitCode::FAILURE
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = args.first().map(String::as_str) else {
        eprint!("{USAGE}");
        return ExitCode::FAILURE;
    };
    let root = repo_root();

    match command {
        "build-metadata" => build_metadata(&root, &args),
        "scenarios" => scenario_gate(&root, &args),
        "coverage" => coverage_gate(&root, &args),
        "compat-matrix" => compat_matrix(&root, &args),
        "doc-lint" => doc_lint(&root),
        "evidence" => match args.get(1).map(String::as_str) {
            Some("assemble") => evidence_assemble(&root, &args),
            Some("verify") => evidence_verify(&args, false),
            Some("gate") => evidence_gate(&root, &args),
            _ => {
                eprint!("{USAGE}");
                ExitCode::FAILURE
            }
        },
        "--help" | "-h" | "help" => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        other => fail(&format!("unknown command `{other}`\n\n{USAGE}")),
    }
}

/// Verifies the bundle, then refuses any `Supported` compatibility claim that
/// has no archived evidence entry (spec 22(17), 24).
fn evidence_gate(root: &Path, args: &[String]) -> ExitCode {
    let verdict = evidence_verify(args, true);
    if verdict != ExitCode::SUCCESS {
        return verdict;
    }
    let Some(bundle) = flag(args, "--bundle") else {
        return fail("--bundle is required");
    };
    let trusted = trusted_keys(args);
    let manifest = match evidence::verify_bundle(Path::new(&bundle), &trusted) {
        Ok(manifest) => manifest,
        Err(error) => return fail(&error.to_string()),
    };
    let register = match compat::load(&root.join("compatibility.yaml")) {
        Ok(register) => register,
        Err(error) => return fail(&error.to_string()),
    };
    match compat::gate_support_claims(&register, &evidence::evidence_ids(&manifest)) {
        Ok(()) => {
            println!("release gate: every compatibility claim is backed by archived evidence");
            ExitCode::SUCCESS
        }
        Err(error) => fail(&error.to_string()),
    }
}

/// Collects every `--trusted-key <id>` occurrence.
fn trusted_keys(args: &[String]) -> Vec<String> {
    args.iter()
        .enumerate()
        .filter(|(_, arg)| arg.as_str() == "--trusted-key")
        .filter_map(|(index, _)| args.get(index + 1).cloned())
        .collect()
}

fn build_metadata(root: &Path, args: &[String]) -> ExitCode {
    let artifacts = flag(args, "--artifacts").map(PathBuf::from);
    let metadata = match buildmeta::collect(root, artifacts.as_deref()) {
        Ok(metadata) => metadata,
        Err(error) => return fail(&error.to_string()),
    };
    let mut json = serde_json::to_string_pretty(&metadata).expect("metadata serializes");
    json.push('\n');
    match flag(args, "--out") {
        Some(out) => {
            let path = PathBuf::from(out);
            if let Some(parent) = path.parent() {
                if let Err(error) = std::fs::create_dir_all(parent) {
                    return fail(&format!("cannot create {}: {error}", parent.display()));
                }
            }
            if let Err(error) = std::fs::write(&path, json) {
                return fail(&format!("cannot write {}: {error}", path.display()));
            }
            println!("build metadata written to {}", path.display());
        }
        None => print!("{json}"),
    }
    ExitCode::SUCCESS
}

/// Writes a registry stub containing every spec 18.7 title, each marked
/// `blocked-on`, so a new entry starts honest and is edited upwards.
fn scenario_scaffold(path: &Path) -> ExitCode {
    let registry = scenarios::Registry {
        schema_version: 1,
        quarantine_policy: "unset".to_owned(),
        scenarios: scenarios::REQUIRED_SCENARIOS
            .iter()
            .enumerate()
            .map(|(index, title)| scenarios::ScenarioEntry {
                number: index + 1,
                title: (*title).to_owned(),
                platforms: Vec::new(),
                status: scenarios::ScenarioStatus::BlockedOn {
                    task: "unassigned".to_owned(),
                    reason: "no coverage yet".to_owned(),
                },
            })
            .collect(),
    };
    let text = match toml::to_string_pretty(&registry) {
        Ok(text) => text,
        Err(error) => return fail(&error.to_string()),
    };
    if let Err(error) = std::fs::write(path, text) {
        return fail(&format!("cannot write {}: {error}", path.display()));
    }
    println!("scaffolded {}", path.display());
    ExitCode::SUCCESS
}

fn scenario_gate(root: &Path, args: &[String]) -> ExitCode {
    let path = root.join("tests/e2e/scenarios.toml");
    if has(args, "--scaffold") {
        return scenario_scaffold(&path);
    }
    let registry = match scenarios::load(&path) {
        Ok(registry) => registry,
        Err(error) => return fail(&error.to_string()),
    };
    let errors = scenarios::validate(&registry);
    if !errors.is_empty() {
        for error in &errors {
            eprintln!("error: {error}");
        }
        return ExitCode::FAILURE;
    }
    let coverage = scenarios::coverage(&registry);
    if has(args, "--json") {
        let mut json = serde_json::to_string_pretty(&serde_json::json!({
            "schema_version": registry.schema_version,
            "required": scenarios::REQUIRED_SCENARIOS.len(),
            "coverage": coverage,
            "quarantine_policy": registry.quarantine_policy,
            "scenarios": registry.scenarios,
        }))
        .expect("coverage serializes");
        json.push('\n');
        print!("{json}");
    } else {
        println!(
            "spec 18.7: {} required, {} implemented, {} opt-in, {} blocked",
            scenarios::REQUIRED_SCENARIOS.len(),
            coverage.implemented,
            coverage.opt_in,
            coverage.blocked
        );
    }
    ExitCode::SUCCESS
}

fn coverage_gate(root: &Path, args: &[String]) -> ExitCode {
    let policy = match coverage::load_policy(&root.join("coverage-policy.toml")) {
        Ok(policy) => policy,
        Err(error) => return fail(&error.to_string()),
    };
    let mut failures = coverage::enforce_transition_checklist(&policy, root);

    match flag(args, "--llvm-cov") {
        Some(path) => {
            let json = match std::fs::read_to_string(&path) {
                Ok(json) => json,
                Err(error) => return fail(&format!("cannot read {path}: {error}")),
            };
            let measured = match coverage::parse_llvm_cov(&json, root) {
                Ok(measured) => measured,
                Err(error) => return fail(&error.to_string()),
            };
            failures.extend(coverage::enforce_thresholds(&policy, &measured));
        }
        None => {
            return fail("--llvm-cov <file.json> is required: an unmeasured critical module is a failure, not a pass");
        }
    }

    if failures.is_empty() {
        println!(
            "coverage policy: {} critical modules at or above {}% branch coverage; {} transitions fully tested",
            policy.critical.len(),
            policy.branch_threshold,
            policy.transitions.len()
        );
        return ExitCode::SUCCESS;
    }
    for failure in &failures {
        eprintln!("error: {failure}");
    }
    ExitCode::FAILURE
}

fn compat_matrix(root: &Path, args: &[String]) -> ExitCode {
    let register = match compat::load(&root.join("compatibility.yaml")) {
        Ok(register) => register,
        Err(error) => return fail(&error.to_string()),
    };
    let rendered = compat::render_markdown(&register);
    let out = flag(args, "--out").map_or_else(
        || root.join("docs/public/compatibility.md"),
        PathBuf::from,
    );
    if has(args, "--check") {
        let committed = std::fs::read_to_string(&out).unwrap_or_default();
        if committed == rendered {
            println!("{} is up to date with compatibility.yaml", out.display());
            return ExitCode::SUCCESS;
        }
        return fail(&format!(
            "{} is stale; run `plaudon-release compat-matrix` and commit the result",
            out.display()
        ));
    }
    if let Some(parent) = out.parent() {
        if let Err(error) = std::fs::create_dir_all(parent) {
            return fail(&format!("cannot create {}: {error}", parent.display()));
        }
    }
    if let Err(error) = std::fs::write(&out, rendered) {
        return fail(&format!("cannot write {}: {error}", out.display()));
    }
    println!("wrote {}", out.display());
    ExitCode::SUCCESS
}

fn doc_lint(root: &Path) -> ExitCode {
    let violations = doclint::lint_public_docs(root);
    if violations.is_empty() {
        println!("documentation lint: clean");
        return ExitCode::SUCCESS;
    }
    for violation in &violations {
        eprintln!("error: {violation}");
    }
    ExitCode::FAILURE
}

fn evidence_assemble(root: &Path, args: &[String]) -> ExitCode {
    let Some(staging) = flag(args, "--staging") else {
        return fail("--staging <dir> is required");
    };
    let staging = PathBuf::from(staging);

    let metadata = match buildmeta::collect(root, None) {
        Ok(metadata) => metadata,
        Err(error) => return fail(&error.to_string()),
    };
    if metadata.dirty {
        return fail("the working tree is dirty; a release bundle must describe a committed tree");
    }

    let key = match sign::signing_key_from_env() {
        Ok(key) => key,
        Err(error) => return fail(&error.to_string()),
    };

    let manifest = match evidence::assemble(
        &staging,
        &metadata.commit,
        &metadata.app_version,
        &metadata.built_at,
    ) {
        Ok(manifest) => manifest,
        Err(error) => return fail(&error.to_string()),
    };
    let signature = match evidence::write_bundle(&staging, &manifest, &key) {
        Ok(signature) => signature,
        Err(error) => return fail(&error.to_string()),
    };
    println!(
        "evidence bundle assembled: {} entries, signed by key {}",
        manifest.entries.len(),
        signature.key_id
    );
    ExitCode::SUCCESS
}

fn evidence_verify(args: &[String], quiet: bool) -> ExitCode {
    let Some(bundle) = flag(args, "--bundle") else {
        return fail("--bundle <dir> is required");
    };
    let trusted: Vec<String> = args
        .iter()
        .enumerate()
        .filter(|(_, arg)| arg.as_str() == "--trusted-key")
        .filter_map(|(index, _)| args.get(index + 1).cloned())
        .collect();
    if trusted.is_empty() {
        return fail("--trusted-key <key-id> is required: a bundle that supplies its own trust decision proves nothing");
    }
    match evidence::verify_bundle(Path::new(&bundle), &trusted) {
        Ok(manifest) => {
            if !quiet {
                println!(
                    "evidence bundle verified: {} entries, commit {}",
                    manifest.entries.len(),
                    manifest.commit
                );
            }
            ExitCode::SUCCESS
        }
        Err(error) => fail(&error.to_string()),
    }
}
