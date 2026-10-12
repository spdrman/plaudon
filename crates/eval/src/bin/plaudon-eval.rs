//! `plaudon-eval`: the offline evaluation harness (spec 17.1, 17.2, 17.6).
//!
//! ```text
//! plaudon-eval validate-corpus <manifest>
//! plaudon-eval run --manifest <m> --engine <id> --hypotheses <dir|file> --out <report.json>
//!                  [--allow-cloud] [--host-cpu <s>] [--os-version <s>] [--ram-bytes <n>]
//!                  [--commit <sha>] [--measured-on <rfc3339>] [--evidence-bundle <id>]
//! plaudon-eval gate --report <report.json> --claim recommended --manifest <golden manifest>
//!                   [--engine <id>] [--model <id>] [--as-of <rfc3339>] [--max-age-days <n>]
//! ```
//!
//! The tool never invokes an engine and never opens a socket. `run` scores
//! hypothesis transcripts that a separate, explicitly recorded process already
//! produced. Evaluating metered-cloud material additionally requires
//! `--allow-cloud` *plus* the `PLAUDON_EVAL_CLOUD_OPT_IN` and
//! `PLAUDON_EVAL_COST_CAP_MICRO_USD` environment variables (spec 17.6).
//!
//! Exit codes: `0` success, `1` the corpus, run or gate failed, `2` the
//! command line was wrong.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use plaudon_domain::time::Timestamp;
use plaudon_eval::corpus::CorpusManifest;
use plaudon_eval::gate::{evaluate_claim, Claim, GateRequest, GoldenCorpus};
use plaudon_eval::hypothesis::HypothesisSet;
use plaudon_eval::report::{EnvironmentRecord, EvaluationReport, FrozenThresholds};
use plaudon_eval::runner::{self, CloudPolicy, RunContext};

const USAGE: &str = "\
plaudon-eval validate-corpus <manifest>
plaudon-eval run --manifest <m> --engine <id> --hypotheses <dir|file> --out <report.json>
                 [--allow-cloud] [--host-cpu <s>] [--os-version <s>] [--ram-bytes <n>]
                 [--commit <sha>] [--measured-on <rfc3339>] [--evidence-bundle <id>]
plaudon-eval gate --report <report.json> --claim <supported|recommended>
                  [--manifest <golden manifest>] [--engine <id>] [--model <id>]
                  [--as-of <rfc3339>] [--max-age-days <n>]";

const SWITCHES: &[&str] = &["--allow-cloud"];

/// Parsed command line.
#[derive(Debug, Default)]
struct Args {
    positional: Vec<String>,
    values: BTreeMap<String, String>,
    switches: BTreeSet<String>,
}

impl Args {
    fn parse(raw: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut args = Self::default();
        let mut tokens = raw.peekable();
        while let Some(token) = tokens.next() {
            if let Some(name) = token.strip_prefix("--") {
                let name = format!("--{name}");
                if SWITCHES.contains(&name.as_str()) {
                    args.switches.insert(name);
                    continue;
                }
                let value = tokens
                    .next()
                    .ok_or_else(|| format!("{name} expects a value"))?;
                if args.values.insert(name.clone(), value).is_some() {
                    return Err(format!("{name} given more than once"));
                }
            } else {
                args.positional.push(token);
            }
        }
        Ok(args)
    }

    fn require(&self, name: &str) -> Result<&str, String> {
        self.values
            .get(name)
            .map(String::as_str)
            .ok_or_else(|| format!("{name} is required"))
    }

    fn optional(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(String::as_str)
    }

    fn switch(&self, name: &str) -> bool {
        self.switches.contains(name)
    }

    fn reject_unknown(&self, allowed: &[&str]) -> Result<(), String> {
        for name in self.values.keys().chain(self.switches.iter()) {
            if !allowed.contains(&name.as_str()) {
                return Err(format!("unknown option {name}"));
            }
        }
        Ok(())
    }
}

fn main() -> ExitCode {
    let mut raw = std::env::args().skip(1);
    let Some(command) = raw.next() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let args = match Args::parse(raw) {
        Ok(args) => args,
        Err(error) => return usage_error(&error),
    };

    match command.as_str() {
        "validate-corpus" => dispatch(validate_corpus(&args)),
        "run" => dispatch(run(&args)),
        "gate" => dispatch(gate(&args)),
        "--help" | "-h" | "help" => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        other => usage_error(&format!("unknown command `{other}`")),
    }
}

/// Outcome of a subcommand: a failure message, or success.
type Outcome = Result<bool, String>;

fn dispatch(outcome: Outcome) -> ExitCode {
    match outcome {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn usage_error(message: &str) -> ExitCode {
    eprintln!("error: {message}\n\n{USAGE}");
    ExitCode::from(2)
}

fn validate_corpus(args: &Args) -> Outcome {
    args.reject_unknown(&[])?;
    let [path] = args.positional.as_slice() else {
        return Err("validate-corpus expects exactly one manifest path".to_owned());
    };
    let manifest = CorpusManifest::load(Path::new(path)).map_err(|error| error.to_string())?;
    let cleared = manifest.clear();
    println!(
        "corpus {} ({}): {} item(s) declared, {} cleared, {} refused",
        manifest.corpus_id,
        manifest.digest().to_hex(),
        manifest.items.len(),
        cleared.accepted().len(),
        cleared.rejected().len()
    );
    for item in cleared.accepted() {
        println!(
            "  cleared  {} [{} {} {}] consent={} license={}",
            item.id,
            item.locale.as_str(),
            item.duration_bucket.as_str(),
            format_args!("{} speaker(s)", item.speaker_count),
            item.consent.as_str(),
            item.license.as_deref().unwrap_or("none")
        );
    }
    for rejection in cleared.rejected() {
        for reason in &rejection.reasons {
            println!("  REFUSED  {}: {reason}", rejection.item_id);
        }
    }
    Ok(!cleared.has_rejections())
}

fn run(args: &Args) -> Outcome {
    args.reject_unknown(&[
        "--manifest",
        "--engine",
        "--hypotheses",
        "--out",
        "--allow-cloud",
        "--host-cpu",
        "--os-version",
        "--ram-bytes",
        "--commit",
        "--measured-on",
        "--evidence-bundle",
    ])?;
    let manifest_path = PathBuf::from(args.require("--manifest")?);
    let manifest = CorpusManifest::load(&manifest_path).map_err(|error| error.to_string())?;
    let manifest_dir = manifest_path
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let hypotheses = HypothesisSet::load(Path::new(args.require("--hypotheses")?))
        .map_err(|error| error.to_string())?;
    let engine = args.require("--engine")?;
    let out = PathBuf::from(args.require("--out")?);

    let cloud_policy =
        CloudPolicy::resolve(args.switch("--allow-cloud"), |key| std::env::var(key).ok())
            .map_err(|error| error.to_string())?;

    let mut context = RunContext::new(environment(args)?, env!("CARGO_PKG_VERSION"));
    context.cloud_policy = cloud_policy;
    if let Some(bundle) = args.optional("--evidence-bundle") {
        context.frozen_thresholds = FrozenThresholds::deferred().in_evidence_bundle(bundle);
    }

    let report = runner::run(&manifest, &manifest_dir, &hypotheses, engine, &context)
        .map_err(|error| error.to_string())?;
    let json = report.to_json().map_err(|error| error.to_string())?;
    std::fs::write(&out, json)
        .map_err(|error| format!("cannot write {}: {error}", out.display()))?;
    print!("{}", report.render_text());
    println!("report written to {}", out.display());
    Ok(true)
}

fn environment(args: &Args) -> Result<EnvironmentRecord, String> {
    let ram_bytes = match args.optional("--ram-bytes") {
        None => None,
        Some(raw) => Some(
            raw.parse::<u64>()
                .map_err(|_| format!("--ram-bytes expects an integer, got `{raw}`"))?,
        ),
    };
    Ok(EnvironmentRecord {
        os: std::env::consts::OS.to_owned(),
        os_version: args.optional("--os-version").map(ToOwned::to_owned),
        cpu: args.optional("--host-cpu").map(ToOwned::to_owned),
        ram_bytes,
        commit: args.optional("--commit").map(ToOwned::to_owned),
        measured_on: match args.optional("--measured-on") {
            Some(raw) => parse_timestamp(raw)?,
            None => Timestamp::now(),
        },
    })
}

fn parse_timestamp(raw: &str) -> Result<Timestamp, String> {
    let parsed = time::OffsetDateTime::parse(raw, &time::format_description::well_known::Rfc3339)
        .map_err(|error| format!("`{raw}` is not an RFC 3339 instant: {error}"))?;
    let millis = i64::try_from(parsed.unix_timestamp_nanos() / 1_000_000)
        .map_err(|_| format!("`{raw}` is out of range"))?;
    Ok(Timestamp::from_millis(millis))
}

fn gate(args: &Args) -> Outcome {
    args.reject_unknown(&[
        "--report",
        "--claim",
        "--manifest",
        "--engine",
        "--model",
        "--as-of",
        "--max-age-days",
    ])?;
    let report_path = PathBuf::from(args.require("--report")?);
    let text = std::fs::read_to_string(&report_path)
        .map_err(|error| format!("cannot read {}: {error}", report_path.display()))?;
    let report = EvaluationReport::from_json(&text)
        .map_err(|error| format!("cannot parse {}: {error}", report_path.display()))?;
    let claim = Claim::parse(args.require("--claim")?).map_err(|error| error.to_string())?;

    let golden = match args.optional("--manifest") {
        Some(path) => {
            let manifest =
                CorpusManifest::load(Path::new(path)).map_err(|error| error.to_string())?;
            GoldenCorpus {
                corpus_id: manifest.corpus_id.clone(),
                manifest_sha256: manifest.digest(),
            }
        }
        None if claim.requires_evaluation_record() => {
            return Err(
                "--manifest (the golden corpus) is required for a `recommended` claim".to_owned(),
            )
        }
        None => GoldenCorpus {
            corpus_id: report.corpus.corpus_id.clone(),
            manifest_sha256: report.corpus.manifest_sha256,
        },
    };

    let mut request = GateRequest::new(
        claim,
        args.optional("--engine")
            .unwrap_or(report.engine.engine_id.as_str()),
        args.optional("--model")
            .unwrap_or(report.engine.model_id.as_str()),
        golden,
        match args.optional("--as-of") {
            Some(raw) => parse_timestamp(raw)?,
            None => Timestamp::now(),
        },
    );
    if let Some(raw) = args.optional("--max-age-days") {
        request.max_record_age_days = raw
            .parse()
            .map_err(|_| format!("--max-age-days expects an integer, got `{raw}`"))?;
    }

    match evaluate_claim(&request, Some(&report)) {
        Ok(decision) => {
            println!("{}", decision.summary());
            for outcome in &decision.threshold_outcomes {
                println!(
                    "  threshold {:?} {:?} {} satisfied",
                    outcome.threshold.metric, outcome.threshold.comparator, outcome.threshold.value
                );
            }
            Ok(true)
        }
        Err(error) => {
            eprintln!(
                "gate refused `{}` for {} / {}: {error}",
                request.claim.as_str(),
                request.engine_id,
                request.model_id
            );
            Ok(false)
        }
    }
}
