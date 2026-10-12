//! Diarization and tool quality metrics emitter (spec 17.4).
//!
//! Scores DER on hand-authored fixtures and optionally scores tool quality
//! on a labeled corpus. Emits a JSON report with full provenance for the
//! release evidence bundle. Never fabricates measurements.

use clap::Parser;
use plaudon_ai_diarization::contract::SpeakerTurn;
use plaudon_ai_diarization::der::{diarization_error_rate, DerOptions, DerReport, ReferenceTurn};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

#[derive(Parser)]
#[command(name = "quality_metrics")]
#[command(about = "Emit diarization and tool quality metrics for CI")]
struct Args {
    /// Output path for the JSON report
    #[arg(long, required = true)]
    out: PathBuf,

    /// Directory containing test fixtures
    #[arg(long, default_value = "tests/fixtures/diarization")]
    fixtures: PathBuf,

    /// Optional labeled corpus for tool quality scoring
    #[arg(long)]
    tool_corpus: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
struct FixtureTurn {
    #[serde(default)]
    speaker: Option<String>,
    #[serde(default)]
    cluster: Option<String>,
    start_ms: u64,
    end_ms: u64,
}

#[derive(Debug, Deserialize)]
struct Fixture {
    name: String,
    provenance: String,
    #[allow(dead_code, reason = "fixture documents its own duration for readers")]
    duration_ms: u64,
    reference: Vec<FixtureTurn>,
    hypothesis: Vec<FixtureTurn>,
}

#[derive(Debug, Serialize)]
struct Provenance {
    hardware: String,
    os_build: String,
    toolchain: String,
    commit: String,
    fixture_source: String,
}

/// Roll-up across every fixture. Speaker and cluster counts are deliberately
/// absent here: identifiers are per-fixture, so a cross-fixture count would be
/// a number with no meaning (spec 17.6 forbids unsupported aggregate claims).
#[derive(Debug, Serialize)]
struct DerAggregate {
    der: f64,
    missed_ms: u64,
    false_alarm_ms: u64,
    confusion_ms: u64,
    total_reference_ms: u64,
    overlapping_reference_ms: u64,
    scored_frames: usize,
    excluded_frames: usize,
    overlap_regions_skipped: bool,
    collar_ms: u64,
}

/// One fixture's measurement with the provenance of the annotation.
#[derive(Debug, Serialize)]
struct FixtureMeasurement {
    name: String,
    provenance: String,
    report: DerReport,
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum DiarizationBlock {
    Measured {
        fixture_count: usize,
        aggregate: DerAggregate,
        fixtures: Vec<FixtureMeasurement>,
    },
    Skipped {
        reason: String,
    },
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum ToolQualityBlock {
    Measured {
        corpus: CorpusAvailability,
        factual_correctness: Option<ScoredMetric>,
        coverage: Option<ScoredMetric>,
        redundant_question_rate: Option<ScoredMetric>,
        grounding_precision: Option<ScoredMetric>,
    },
    Skipped {
        reason: String,
    },
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum CorpusAvailability {
    Present {
        name: String,
        provenance: String,
        key_point_count: usize,
    },
}

#[derive(Debug, Serialize)]
struct ScoredMetric {
    value: f64,
    sample_size: usize,
}

#[derive(Debug, Serialize)]
struct QualityReport {
    schema_version: u32,
    generated_at: String,
    provenance: Provenance,
    diarization: DiarizationBlock,
    tool_quality: ToolQualityBlock,
}

fn load_fixture(path: &Path) -> Result<Fixture, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;
    serde_json::from_str(&content).map_err(|e| format!("Failed to parse {}: {}", path.display(), e))
}

fn score_fixture(fixture: &Fixture) -> Result<DerReport, String> {
    let reference: Vec<ReferenceTurn> = fixture
        .reference
        .iter()
        .map(|t| {
            Ok(ReferenceTurn::new(
                t.speaker
                    .as_ref()
                    .ok_or_else(|| format!("Reference turn missing speaker in {}", fixture.name))?,
                t.start_ms,
                t.end_ms,
            ))
        })
        .collect::<Result<Vec<_>, String>>()?;

    let hypothesis: Vec<SpeakerTurn> = fixture
        .hypothesis
        .iter()
        .map(|t| {
            Ok(SpeakerTurn::new(
                t.cluster.as_ref().ok_or_else(|| {
                    format!("Hypothesis turn missing cluster in {}", fixture.name)
                })?,
                t.start_ms,
                t.end_ms,
            ))
        })
        .collect::<Result<Vec<_>, String>>()?;

    Ok(diarization_error_rate(
        &reference,
        &hypothesis,
        DerOptions::default(),
    ))
}

fn aggregate_der_reports(measurements: Vec<FixtureMeasurement>) -> DiarizationBlock {
    if measurements.is_empty() {
        return DiarizationBlock::Skipped {
            reason: "no_fixtures_found".to_string(),
        };
    }

    let mut missed_ms = 0u64;
    let mut false_alarm_ms = 0u64;
    let mut confusion_ms = 0u64;
    let mut total_reference_ms = 0u64;
    let mut overlapping_reference_ms = 0u64;
    let mut scored_frames = 0usize;
    let mut excluded_frames = 0usize;

    for measurement in &measurements {
        missed_ms += measurement.report.missed_ms;
        false_alarm_ms += measurement.report.false_alarm_ms;
        confusion_ms += measurement.report.confusion_ms;
        total_reference_ms += measurement.report.total_reference_ms;
        overlapping_reference_ms += measurement.report.overlapping_reference_ms;
        scored_frames += measurement.report.scored_frames;
        excluded_frames += measurement.report.excluded_frames;
    }

    let der = if total_reference_ms == 0 {
        0.0
    } else {
        (missed_ms + false_alarm_ms + confusion_ms) as f64 / total_reference_ms as f64
    };

    let first = &measurements[0].report;
    DiarizationBlock::Measured {
        fixture_count: measurements.len(),
        aggregate: DerAggregate {
            der,
            missed_ms,
            false_alarm_ms,
            confusion_ms,
            total_reference_ms,
            overlapping_reference_ms,
            scored_frames,
            excluded_frames,
            overlap_regions_skipped: first.overlap_regions_skipped,
            collar_ms: first.collar_ms,
        },
        fixtures: measurements,
    }
}

fn gather_provenance(fixture_dir: &Path, fixture_names: &[String]) -> Provenance {
    let hardware = gather_hardware();
    let os_build = gather_os_build();
    let toolchain = gather_toolchain();
    let commit = gather_commit();
    let fixture_source = format!("{} ({})", fixture_dir.display(), fixture_names.join(", "));

    Provenance {
        hardware,
        os_build,
        toolchain,
        commit,
        fixture_source,
    }
}

fn gather_hardware() -> String {
    let arch = std::env::consts::ARCH;

    #[cfg(target_os = "macos")]
    {
        let brand = Command::new("sysctl")
            .args(["-n", "machdep.cpu.brand_string"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string());

        let memsize = Command::new("sysctl")
            .args(["-n", "hw.memsize"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| s.trim().parse::<u64>().ok())
            .map(|bytes| format!("{} GB", bytes / (1024 * 1024 * 1024)));

        match (brand, memsize) {
            (Some(cpu), Some(mem)) => format!("{arch} {cpu} {mem}"),
            (Some(cpu), None) => format!("{arch} {cpu}"),
            (None, Some(mem)) => format!("{arch} {mem}"),
            (None, None) => arch.to_string(),
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        arch.to_string()
    }
}

fn gather_os_build() -> String {
    let os = std::env::consts::OS;

    #[cfg(target_os = "macos")]
    {
        if let Some(build) = Command::new("sw_vers")
            .arg("-buildVersion")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
        {
            return format!("{os} {build}");
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Some(ver) = Command::new("cmd")
            .args(["/C", "ver"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
        {
            return format!("{} {}", os, ver);
        }
    }

    os.to_string()
}

fn gather_toolchain() -> String {
    Command::new("rustc")
        .arg("-V")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map_or_else(|| "unknown".to_string(), |s| s.trim().to_string())
}

fn gather_commit() -> String {
    Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map_or_else(|| "unknown".to_string(), |s| s.trim().to_string())
}

/// An opt-in tool-quality fixture.
///
/// Spec 17.4 scores flashcards/Q&A "using an opt-in labeled educational
/// corpus". Scoring needs three things together: the labelled key points, the
/// transcript the artifacts were generated from, and the artifacts. They live
/// in one file so a measurement can never be assembled from mismatched parts.
#[derive(Debug, Deserialize)]
struct ToolFixture {
    corpus: plaudon_ai_tools::metrics::LabeledCorpus,
    /// Segment texts of the transcript the artifacts cite, in order.
    segments: Vec<String>,
    /// The generated artifacts, before anchor verification.
    artifacts: serde_json::Value,
}

fn score_tool_quality(corpus_path: Option<&PathBuf>) -> ToolQualityBlock {
    let Some(path) = corpus_path else {
        return ToolQualityBlock::Skipped {
            reason: "corpus_not_available".to_string(),
        };
    };
    if !path.exists() {
        return ToolQualityBlock::Skipped {
            reason: "corpus_not_available".to_string(),
        };
    }

    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) => {
            return ToolQualityBlock::Skipped {
                reason: format!("corpus_unreadable: {error}"),
            }
        }
    };
    let fixture: ToolFixture = match serde_json::from_str(&text) {
        Ok(fixture) => fixture,
        Err(error) => {
            // A corpus without the transcript and artifacts cannot be scored.
            // Reporting that is the honest outcome; emitting null scores under
            // a "measured" status would claim a measurement we did not make.
            return ToolQualityBlock::Skipped {
                reason: format!("corpus_incomplete: {error}"),
            };
        }
    };

    let revision = plaudon_ai_tools::testing::revision_from_segments(
        plaudon_domain::transcript::AlignmentOrigin::Native,
        fixture
            .segments
            .iter()
            .enumerate()
            .map(|(index, text)| {
                let seq = u32::try_from(index).unwrap_or(0);
                plaudon_domain::transcript::TranscriptSegment::timed(
                    seq,
                    u64::from(seq) * 1_000,
                    u64::from(seq) * 1_000 + 900,
                    text.clone(),
                )
            })
            .collect(),
    );

    let validation =
        match plaudon_ai_tools::study::study_artifacts_from_value(&fixture.artifacts, &revision) {
            Ok(validation) => validation,
            Err(error) => {
                return ToolQualityBlock::Skipped {
                    reason: format!("artifacts_rejected: {error}"),
                }
            }
        };

    let report = plaudon_ai_tools::metrics::score_study_artifacts(
        &validation.accepted,
        &revision,
        Some(&fixture.corpus),
    );

    let convert = |metric: Option<plaudon_ai_tools::metrics::ScoredMetric>| {
        metric.map(|m| ScoredMetric {
            value: m.value,
            sample_size: m.sample_size,
        })
    };

    ToolQualityBlock::Measured {
        corpus: CorpusAvailability::Present {
            name: fixture.corpus.name.clone(),
            provenance: fixture.corpus.provenance.clone(),
            key_point_count: fixture.corpus.key_points.len(),
        },
        factual_correctness: convert(report.factual_correctness),
        coverage: convert(report.coverage),
        redundant_question_rate: convert(report.redundant_question_rate),
        grounding_precision: convert(report.grounding_precision),
    }
}

fn main() -> ExitCode {
    let args = Args::parse();

    // Load all fixtures
    let fixture_paths: Vec<PathBuf> = match std::fs::read_dir(&args.fixtures) {
        Ok(entries) => entries
            .filter_map(std::result::Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|ext| ext == "json"))
            .collect(),
        Err(e) => {
            eprintln!("Failed to read fixture directory: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut measurements = Vec::new();
    let mut fixture_names = Vec::new();

    let mut fixture_paths = fixture_paths;
    fixture_paths.sort();
    for path in &fixture_paths {
        match load_fixture(path) {
            Ok(fixture) => {
                fixture_names.push(fixture.name.clone());
                match score_fixture(&fixture) {
                    Ok(report) => measurements.push(FixtureMeasurement {
                        name: fixture.name.clone(),
                        provenance: fixture.provenance.clone(),
                        report,
                    }),
                    Err(e) => {
                        eprintln!("Failed to score fixture {}: {}", path.display(), e);
                        return ExitCode::FAILURE;
                    }
                }
            }
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::FAILURE;
            }
        }
    }

    let diarization = aggregate_der_reports(measurements);
    let tool_quality = score_tool_quality(args.tool_corpus.as_ref());
    let provenance = gather_provenance(&args.fixtures, &fixture_names);

    let report = QualityReport {
        schema_version: 1,
        generated_at: time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|_| "unknown".to_string()),
        provenance,
        diarization,
        tool_quality,
    };

    // Create parent directories if needed
    if let Some(parent) = args.out.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("Failed to create output directory: {e}");
            return ExitCode::FAILURE;
        }
    }

    // Write the report
    let json = match serde_json::to_string_pretty(&report) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("Failed to serialize report: {e}");
            return ExitCode::FAILURE;
        }
    };

    if let Err(e) = std::fs::write(&args.out, json) {
        eprintln!("Failed to write output file: {e}");
        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}
