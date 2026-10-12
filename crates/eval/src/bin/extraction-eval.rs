//! Extraction evaluation harness binary (EPIC I task I2.5).
//!
//! Usage:
//!   extraction-eval --out <path> [--check]
//!
//! Runs the hermetic evaluation suite over corpus fixtures, computes §17.3 metrics,
//! and writes a machine-readable JSON report. With `--check`, additionally applies
//! the threshold gate and exits non-zero on regression.

use std::path::{Path, PathBuf};
use std::process;
use std::sync::Arc;

use plaudon_ai_contracts::job::JobContext;
use plaudon_ai_contracts::text::TextModel;
use plaudon_ai_contracts::validation::RepairPolicy;
use plaudon_app_core::chunking::ChunkPolicy;
use plaudon_app_core::pipeline::{
    PipelineRequest, SteppingClock, SummarizationPipeline, PIPELINE_VERSION,
};
use plaudon_app_core::templates::TemplateRegistry;
use plaudon_domain::ids::JobId;
use plaudon_domain::ids::RecordingId;
use plaudon_domain::media::Sha256Digest;
use plaudon_eval::{
    apply_gate, compute_metrics, generate_report, load_corpus_file, CorpusFixture, MutationProfile,
    ReferenceModel, Thresholds,
};

fn main() {
    let args: Vec<String> = std::env::args().collect();

    let mut out_path: Option<PathBuf> = None;
    let mut check_mode = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--out" => {
                if i + 1 < args.len() {
                    out_path = Some(PathBuf::from(&args[i + 1]));
                    i += 2;
                } else {
                    eprintln!("--out requires a path");
                    process::exit(1);
                }
            }
            "--check" => {
                check_mode = true;
                i += 1;
            }
            _ => {
                eprintln!("Unknown argument: {}", args[i]);
                process::exit(1);
            }
        }
    }

    let out_path = out_path.unwrap_or_else(|| {
        eprintln!("Usage: extraction-eval --out <path> [--check]");
        process::exit(1);
    });

    // Run evaluation
    match run_evaluation(&out_path) {
        Ok(()) => {
            println!(
                "Evaluation complete. Report written to: {}",
                out_path.display()
            );

            // Apply gate if --check is specified
            if check_mode {
                match check_thresholds(&out_path) {
                    Ok(true) => {
                        println!("✓ All threshold checks passed");
                        process::exit(0);
                    }
                    Ok(false) => {
                        eprintln!("✗ Threshold checks failed");
                        process::exit(1);
                    }
                    Err(e) => {
                        eprintln!("Error checking thresholds: {}", e);
                        process::exit(1);
                    }
                }
            }
        }
        Err(e) => {
            eprintln!("Evaluation failed: {}", e);
            process::exit(1);
        }
    }
}

fn run_evaluation(out_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    println!("Loading corpus fixtures...");
    let corpus_dir = PathBuf::from("tests/fixtures/ai/corpus");
    let corpus_files: Vec<PathBuf> = std::fs::read_dir(&corpus_dir)?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();
            if path.extension()? == "json" {
                Some(path)
            } else {
                None
            }
        })
        .collect();

    if corpus_files.is_empty() {
        return Err("No corpus files found".into());
    }

    println!("Found {} corpus files", corpus_files.len());

    // Load template registry
    let registry = TemplateRegistry::with_builtins();

    // Create report
    let mut report = generate_report(
        PIPELINE_VERSION,
        "mixed", // Multiple templates
        "1.0.0",
        "1.0.0",
        "en-US",
    );

    // Process each corpus
    for corpus_path in &corpus_files {
        let file_name = corpus_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown");
        println!("Processing {}...", file_name);

        let fixture = load_corpus_file(corpus_path)?;
        let sha256 = fixture.content_sha256();

        // Run pipeline
        let result = run_corpus_pipeline(&fixture, &registry)?;

        // Add to report
        report.add_corpus(
            fixture.id.clone(),
            fixture.template_id.clone(),
            result,
            file_name,
            sha256,
        );
    }

    // Compute aggregate
    report.compute_aggregate();

    // Ensure output directory exists
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Write report
    report.write_to_file(out_path)?;

    Ok(())
}

fn run_corpus_pipeline(
    fixture: &CorpusFixture,
    registry: &TemplateRegistry,
) -> Result<plaudon_eval::EvalMetrics, Box<dyn std::error::Error>> {
    // Build transcript revision
    let revision = fixture.to_transcript_revision();
    let revision_id = revision.id;

    // Get template
    let template = registry
        .get(&fixture.template_id)
        .ok_or_else(|| format!("Template {} not found", fixture.template_id))?;

    // Get schemas
    let extraction_schema = registry.extraction_schema(template)?;
    let summary_schema = registry.summary_schema();

    // Create reference model with Perfect profile
    let model = Arc::new(ReferenceModel::new(
        Arc::new(fixture.clone()),
        revision_id,
        MutationProfile::Perfect,
    ));

    // Get capabilities
    let capabilities = tokio::runtime::Runtime::new()?.block_on(model.as_ref().capabilities());

    // Create speakers
    let speakers = plaudon_ai_contracts::validation::SpeakerDirectory::unknown();

    // Build pipeline request
    let request = PipelineRequest {
        recording_id: RecordingId::new(),
        revision: revision.clone(),
        template: template.clone(),
        extraction_schema,
        summary_schema,
        capabilities: capabilities.clone(),
        chunk_policy: ChunkPolicy {
            reserved_output_tokens: 4000,
            prompt_overhead_tokens: 2000,
            overlap_segments: 2,
            unknown_context: plaudon_app_core::chunking::UnknownContext::FailClosed,
        },
        repair_policy: RepairPolicy::default(),
        speakers,
        reusable_sections: vec![],
        privacy_policy_snapshot: None,
        audio_sha256: Sha256Digest::from_bytes([0; 32]),
    };

    // Create pipeline with stepping clock for determinism
    let clock = Arc::new(SteppingClock::new(1_700_000_000_000, 1000));
    let pipeline = SummarizationPipeline::new(model.clone(), clock);

    // Run pipeline
    let ctx = JobContext::new(JobId::new());
    let outcome = tokio::runtime::Runtime::new()?
        .block_on(pipeline.run(request, &ctx))
        .map_err(|e| format!("Pipeline failed: {}", e))?;

    // Compute metrics
    let schema_valid_count = outcome.claims.len() as u32;
    let total_item_count = schema_valid_count + outcome.rejected.len() as u32;

    let metrics = compute_metrics(
        fixture,
        &outcome.claims,
        &outcome.grounding,
        &outcome.body,
        schema_valid_count,
        total_item_count,
    );

    Ok(metrics)
}

fn check_thresholds(report_path: &Path) -> Result<bool, Box<dyn std::error::Error>> {
    // Load report
    let report_json = std::fs::read_to_string(report_path)?;
    let report: plaudon_eval::EvalReport = serde_json::from_str(&report_json)?;

    // Load thresholds
    let threshold_path = PathBuf::from("docs/benchmarks/extraction-eval-thresholds.json");
    let thresholds = if threshold_path.exists() {
        Thresholds::load_from_file(&threshold_path)?
    } else {
        eprintln!("Warning: threshold file not found, using defaults");
        Thresholds::default_thresholds()
    };

    // Apply gate
    let result = apply_gate(&report, &thresholds);

    if !result.passed() {
        eprintln!("\nGate failures:");
        for reason in result.reasons() {
            eprintln!("  - {}", reason);
        }
    }

    Ok(result.passed())
}
