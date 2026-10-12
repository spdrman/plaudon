//! The results writer: JSON plus markdown, with the host that produced them.
//!
//! A latency number without the machine, the OS build, the commit, the
//! toolchain and the dataset it came from is not evidence of anything. That is
//! the same rule `docs/benchmarks/README.md` already states for the A1.1
//! whisper spike, and [`RunReport::is_citable`] enforces it here: a report
//! missing any identifying field is written with a prominent warning and must
//! not be quoted as a cross-machine claim.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Append a formatted fragment to the document under construction.
///
/// Writing to a `String` is infallible; the macro exists so the builder reads
/// as a sequence of fragments without a `Result` at every line.
macro_rules! md {
    ($out:expr, $($arg:tt)*) => {
        write!($out, $($arg)*).expect("writing to a String cannot fail")
    };
}

use serde::Serialize;

use crate::load::LoadStats;
use crate::metrics::Verdict;
use crate::thresholds::Policy;
use crate::DatasetSpec;

/// Base name of the emitted documents.
pub const DOCUMENT_STEM: &str = "N1.4-scale";

/// Machine and build identification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Host {
    /// Vendor hardware model identifier, for example `Mac16,6`.
    pub hardware_model: Option<String>,
    /// CPU brand string.
    pub cpu: Option<String>,
    /// Logical CPUs visible to the process.
    pub logical_cpus: Option<usize>,
    /// Operating system name and version.
    pub os: Option<String>,
    /// Operating system build identifier.
    pub os_build: Option<String>,
    /// Target architecture of this build.
    pub arch: String,
    /// Rust toolchain that compiled the harness.
    pub toolchain: Option<String>,
    /// Commit the measurement was taken at.
    pub commit: Option<String>,
    /// Whether the working tree had uncommitted changes.
    pub working_tree_dirty: Option<bool>,
}

impl Host {
    /// Probe the current machine.
    ///
    /// Every probe is a local command or file read; nothing here touches the
    /// network, and a probe that fails yields `None` rather than a guess.
    #[must_use]
    pub fn detect() -> Self {
        Self {
            hardware_model: hardware_model(),
            cpu: cpu_brand(),
            logical_cpus: std::thread::available_parallelism().ok().map(Into::into),
            os: os_name(),
            os_build: os_build(),
            arch: std::env::consts::ARCH.to_owned(),
            toolchain: run("rustc", &["--version"]),
            commit: run("git", &["rev-parse", "HEAD"]),
            working_tree_dirty: run("git", &["status", "--porcelain"])
                .map(|output| !output.trim().is_empty()),
        }
    }

    /// Fields that were not established, by name.
    #[must_use]
    pub fn missing_fields(&self) -> Vec<&'static str> {
        let mut missing = Vec::new();
        if self.hardware_model.is_none() {
            missing.push("hardware_model");
        }
        if self.cpu.is_none() {
            missing.push("cpu");
        }
        if self.logical_cpus.is_none() {
            missing.push("logical_cpus");
        }
        if self.os.is_none() {
            missing.push("os");
        }
        if self.os_build.is_none() {
            missing.push("os_build");
        }
        if self.toolchain.is_none() {
            missing.push("toolchain");
        }
        if self.commit.is_none() {
            missing.push("commit");
        }
        missing
    }
}

/// The dataset a run measured.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DatasetReport {
    /// Tier name.
    pub tier: String,
    /// Seed every row derives from.
    pub seed: u64,
    /// Ordinary recordings plus the marathon one.
    pub recordings: u64,
    /// Segments across the library.
    pub segments: u64,
    /// Segments in the marathon transcript.
    pub marathon_segments: u32,
    /// SHA-256 of the generated dataset.
    pub fingerprint: String,
    /// What the load actually wrote.
    pub load: LoadStats,
}

impl DatasetReport {
    /// Build from a spec, a fingerprint and the load statistics.
    #[must_use]
    pub fn new(spec: &DatasetSpec, fingerprint: String, load: LoadStats) -> Self {
        Self {
            tier: spec.tier.as_str().to_owned(),
            seed: spec.seed,
            recordings: spec.total_recordings(),
            segments: spec.total_segments(),
            marathon_segments: spec.marathon_segments,
            fingerprint,
            load,
        }
    }
}

/// Outcome of the memory-bound harness.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MemoryReport {
    /// Where the RSS numbers came from.
    pub rss_source: String,
    /// Reason the measurement was skipped, when it was.
    pub skipped_reason: Option<String>,
    /// Bytes the active chunk window may hold.
    pub window_bytes: u64,
    /// Allowed peak growth.
    pub allowed_growth_bytes: u64,
    /// One row per audio length exercised.
    pub observations: Vec<MemoryObservation>,
    /// Peak growth of the deliberately unbounded control, for comparison.
    pub control_growth_bytes: Option<u64>,
    /// Total decoded PCM of the control run.
    pub control_total_pcm_bytes: Option<u64>,
}

/// One memory observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct MemoryObservation {
    /// Audio length decoded.
    pub audio_minutes: u64,
    /// Bytes of PCM a full decode would have held.
    pub total_pcm_bytes: u64,
    /// Peak working-set growth observed over the run.
    pub peak_growth_bytes: u64,
}

/// One complete benchmark run.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RunReport {
    /// Schema of this document.
    pub schema: &'static str,
    /// When the run finished, RFC 3339 UTC.
    pub generated_at: String,
    /// Machine and build identification.
    pub host: Host,
    /// Dataset identification.
    pub dataset: DatasetReport,
    /// Threshold policy in force.
    pub policy: Policy,
    /// One verdict per latency benchmark.
    pub benchmarks: Vec<Verdict>,
    /// Memory-bound harness outcome.
    pub memory: Option<MemoryReport>,
}

impl RunReport {
    /// Schema identifier written into every document.
    pub const SCHEMA: &'static str = "plaudon.scale-benchmark.v1";

    /// Assemble a report.
    #[must_use]
    pub fn new(
        host: Host,
        dataset: DatasetReport,
        policy: Policy,
        benchmarks: Vec<Verdict>,
        memory: Option<MemoryReport>,
    ) -> Self {
        Self {
            schema: Self::SCHEMA,
            generated_at: plaudon_domain::time::Timestamp::now()
                .to_rfc3339()
                .unwrap_or_else(|| "unknown".to_owned()),
            host,
            dataset,
            policy,
            benchmarks,
            memory,
        }
    }

    /// Whether this report may be quoted as a cross-machine claim.
    #[must_use]
    pub fn is_citable(&self) -> bool {
        self.host.missing_fields().is_empty()
    }

    /// Benchmarks that breached their budget.
    #[must_use]
    pub fn failures(&self) -> Vec<&Verdict> {
        self.benchmarks
            .iter()
            .filter(|verdict| !verdict.passed())
            .collect()
    }

    /// Pretty JSON form.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Write `<directory>/N1.4-scale.json` and `<directory>/N1.4-scale.md`.
    pub fn write_to(&self, directory: &Path) -> std::io::Result<(PathBuf, PathBuf)> {
        std::fs::create_dir_all(directory)?;
        let json_path = directory.join(format!("{DOCUMENT_STEM}.json"));
        let markdown_path = directory.join(format!("{DOCUMENT_STEM}.md"));
        let json = self
            .to_json()
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        std::fs::write(&json_path, format!("{json}\n"))?;
        std::fs::write(&markdown_path, self.to_markdown())?;
        Ok((json_path, markdown_path))
    }

    /// The published markdown document.
    #[must_use]
    #[allow(clippy::too_many_lines)]
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("# N1.4 — performance and scale (spec §17.5, §18.6A UI row)\n\n");
        out.push_str(
            "Generated by `cargo test -p plaudon-scale-tests`. Everything below the protocol \
             section is measured output, not a target.\n\n",
        );

        if self.is_citable() {
            out.push_str(
                "> This run identifies its machine, OS build, commit, toolchain, dataset seed \
                 and dataset size, so it may be cited — for this machine.\n\n",
            );
        } else {
            md!(
                out,
                "> **Not citable.** The following identifying fields were not established: {}. \
                 Per `docs/benchmarks/README.md`, a result whose host is not identified is \
                 evidence of nothing.\n\n",
                self.host.missing_fields().join(", ")
            );
        }

        out.push_str("## Protocol\n\n");
        out.push_str(PROTOCOL);

        out.push_str("\n## Machine\n\n");
        out.push_str("| Field | Value |\n|---|---|\n");
        push_row(
            &mut out,
            "Hardware model",
            &opt(self.host.hardware_model.as_ref()),
        );
        push_row(&mut out, "CPU", &opt(self.host.cpu.as_ref()));
        push_row(
            &mut out,
            "Logical CPUs",
            &self
                .host
                .logical_cpus
                .map_or_else(|| "not established".to_owned(), |value| value.to_string()),
        );
        push_row(&mut out, "OS", &opt(self.host.os.as_ref()));
        push_row(&mut out, "OS build", &opt(self.host.os_build.as_ref()));
        push_row(&mut out, "Architecture", &self.host.arch);
        push_row(&mut out, "Toolchain", &opt(self.host.toolchain.as_ref()));
        push_row(&mut out, "Commit", &opt(self.host.commit.as_ref()));
        push_row(
            &mut out,
            "Working tree",
            match self.host.working_tree_dirty {
                None => "not established",
                Some(true) => "dirty (uncommitted changes present)",
                Some(false) => "clean",
            },
        );
        push_row(&mut out, "Run finished", &self.generated_at);

        let dataset = &self.dataset;
        out.push_str("\n## Dataset\n\n");
        out.push_str("| Field | Value |\n|---|---|\n");
        push_row(&mut out, "Tier", &dataset.tier);
        push_row(&mut out, "Seed", &dataset.seed.to_string());
        push_row(&mut out, "Recordings", &dataset.recordings.to_string());
        push_row(
            &mut out,
            "Transcript segments",
            &dataset.segments.to_string(),
        );
        push_row(
            &mut out,
            "Marathon transcript segments",
            &dataset.marathon_segments.to_string(),
        );
        push_row(
            &mut out,
            "Dataset fingerprint",
            &format!("`{}`", dataset.fingerprint),
        );
        push_row(
            &mut out,
            "Media archive",
            &format!(
                "{} blobs, {}",
                dataset.load.media_assets,
                bytes_human(dataset.load.archive_bytes)
            ),
        );
        push_row(
            &mut out,
            "SQLite file",
            &bytes_human(dataset.load.database_bytes),
        );
        push_row(
            &mut out,
            "Load wall time",
            &format!(
                "{} (of which {} in the media archive)",
                millis_human(dataset.load.load_wall_ms),
                millis_human(dataset.load.archive_wall_ms)
            ),
        );
        push_row(&mut out, "Speaker rows", &dataset.load.speakers.to_string());

        out.push_str("\n## Latency results\n\n");
        out.push_str(
            "Percentiles are nearest-rank, so each figure is a latency that actually occurred. \
             `budget used` is the worst of p50/p95/max against the committed budget; anything \
             above 100% fails the test run.\n\n",
        );
        out.push_str(
            "| Benchmark | samples | unit | p50 ms | p95 ms | max ms | budget p95 ms | \
             budget used | verdict |\n|---|--:|---|--:|--:|--:|--:|--:|---|\n",
        );
        for verdict in &self.benchmarks {
            let measurement = &verdict.measurement;
            md!(
                out,
                "| `{}` | {} | {} | {:.3} | {:.3} | {:.3} | {:.3} | {:.0}% | {} |\n",
                measurement.name,
                measurement.samples,
                measurement.unit,
                measurement.p50_ms,
                measurement.p95_ms,
                measurement.max_ms,
                verdict.budget.p95_ms,
                verdict.worst_budget_fraction * 100.0,
                if verdict.passed() { "pass" } else { "**FAIL**" },
            );
        }
        for verdict in self.failures() {
            md!(out, "\n- **{}**\n", verdict.failure_report());
        }

        out.push_str("\n## Memory bound\n\n");
        match &self.memory {
            None => out.push_str("The memory harness did not run in this invocation.\n"),
            Some(memory) => {
                md!(out, "RSS source: `{}`.\n\n", memory.rss_source);
                if let Some(reason) = &memory.skipped_reason {
                    md!(
                        out,
                        "**Skipped on this host:** {reason}. No memory figure is published for \
                         this platform.\n"
                    );
                } else {
                    md!(
                        out,
                        "Active chunk window: {}. Allowed peak growth: {}.\n\n",
                        bytes_human(memory.window_bytes),
                        bytes_human(memory.allowed_growth_bytes)
                    );
                    out.push_str(
                        "| audio | full decode would hold | observed peak RSS growth |\n\
                         |--:|--:|--:|\n",
                    );
                    for observation in &memory.observations {
                        md!(
                            out,
                            "| {} min | {} | {} |\n",
                            observation.audio_minutes,
                            bytes_human(observation.total_pcm_bytes),
                            bytes_human(observation.peak_growth_bytes)
                        );
                    }
                    if let (Some(growth), Some(total)) =
                        (memory.control_growth_bytes, memory.control_total_pcm_bytes)
                    {
                        md!(
                            out,
                            "\nControl (deliberately unbounded decode, {} of PCM retained): peak \
                             growth {}. The control exists so the bound above is known to be a \
                             detectable property and not a vacuous one.\n",
                            bytes_human(total),
                            bytes_human(growth)
                        );
                    }
                }
            }
        }

        out.push_str("\n## Threshold delta policy\n\n");
        md!(
            out,
            "- Budget file: `tests/scale/thresholds.toml`, schema version {}.\n\
             - Budgets are set about {:.0}% above a healthy reference measurement; that headroom \
             absorbs machine noise and the gap between this workstation and a slower hosted \
             runner. It is deliberately too coarse to catch a 20% slowdown and deliberately \
             sharp enough to catch a lost index or an N+1 query, which move these numbers by an \
             order of magnitude.\n\
             - A single commit may raise one budget by at most {:.0}%. A larger raise needs an \
             ADR explaining what got slower and why that is acceptable.\n\
             - Rule: {}\n",
            self.policy.schema_version,
            self.policy.headroom_ratio * 100.0,
            self.policy.max_single_raise_ratio * 100.0,
            self.policy.rule
        );

        out.push_str("\n## Reproducing this run\n\n");
        md!(
            out,
            "```sh\n\
             # CI tier (the default; this is what `cargo test` runs)\n\
             cargo test -p plaudon-scale-tests\n\n\
             # Full spec 17.5 tier: 10,000 recordings, 180,000 segments.\n\
             # Results land in target/scale-results/ as N1.4-scale.json and .md.\n\
             PLAUDON_SCALE_TIER=full \\\n  \
             PLAUDON_SCALE_SEED={} \\\n  \
             cargo test -p plaudon-scale-tests --test scale_benchmarks -- --nocapture\n\n\
             # Publishing a new reference run replaces this document with the generated one:\n\
             cp target/scale-results/N1.4-scale.md docs/benchmarks/N1.4-scale.md\n\
             ```\n\n\
             The dataset is reproducible from the seed above, so a later run on the same \
             machine measures the same library. A run on a different machine is a different \
             data point, never a comparison, unless the machine rows match.\n",
            dataset.seed
        );

        out.push_str("\n## Machine-readable appendix\n\n");
        out.push_str(
            "The same run as JSON. `write_to` also emits this as a standalone \
             `N1.4-scale.json` next to the markdown.\n\n```json\n",
        );
        out.push_str(&self.to_json().unwrap_or_else(|_| "{}".to_owned()));
        out.push_str("\n```\n");
        out
    }
}

/// The static protocol section, reproduced in every generated document.
const PROTOCOL: &str = r"Spec §17.5 asks for archive and search performance tests with 10,000
recordings and at least 100,000 transcript segments, bounded memory growth
proportional to the active chunk window, and UI interaction latency under
100 ms for ordinary navigation. Spec §18.6A's UI row adds a 100,000-segment
transcript as a hostile case. This suite is the executable form of those
requirements.

**Dataset.** `plaudon-scale-tests` generates the library from a seed with its
own PRNG (`SyntheticLibrary`), then writes every row through the shipping
persistence layer: the committed SQL migrations, the repositories in
`plaudon_persistence::repo`, and the content-addressed `MediaArchive`
(stage, hash, atomic commit, one blob per content hash). No benchmark table
exists. Two generations of one seed are byte-identical, which is asserted by
`dataset_determinism.rs` and recorded as the dataset fingerprint below.

**Tiers.** `PLAUDON_SCALE_TIER=ci` (default) is a reduced library that runs on
every push. `PLAUDON_SCALE_TIER=full` is the spec §17.5 size: 10,000 ordinary
recordings with 8 segments each plus one 100,000-segment marathon transcript,
180,000 segments in total. `PLAUDON_SCALE_SEED` overrides the seed. An
unparsable tier or seed is a hard error, never a silent downgrade.

**What is measured.**

1. `library_page_offset` / `library_page_keyset` — library listing and
   pagination, newest first, 50 rows per page, by `LIMIT/OFFSET` and by
   keyset. This is the ordinary-navigation path §17.5 budgets at 100 ms.
2. `transcript_search` — cross-recording transcript search for a rare term
   over every segment in the library, joined back to its recording.
3. `transcript_viewport` — a 200-segment window fetched at arbitrary offsets
   inside the marathon transcript, the §18.6A UI case. Each fetch asserts the
   returned slice is exactly the requested range in sequence order.
4. `archive_accounting` — full media-archive enumeration reconciled against
   `media_assets` (count and total bytes must agree exactly).
5. `export_manifest` — per-recording export accounting: recording, tags, media
   candidates, archive presence and segment count, as an export would gather
   them.

**What the schema gives these queries.** Migrations 0001 and 0002 index
`transcript_segments(transcript_revision_id, start_ms)` and declare
`UNIQUE (transcript_revision_id, seq_index)`, so the viewport fetch is an
index range scan whatever offset it starts at. `recordings` has no index on
`captured_at`, so the library sort is performed at query time; that is why the
suite measures offset and keyset pagination at identical depths rather than
assuming either is cheap. `transcript_segments.text` has no full-text index,
so cross-recording search is a scan of every segment — the measured number is
the cost of that scan at the dataset size, and it is the number to compare
against if an FTS5 index is ever proposed.

**How it judges.** Each benchmark records a latency distribution and is
compared against `tests/scale/thresholds.toml` on p50, p95 and max at once. A
breach fails the test run and names the file to edit. Latency budgets are
machine-dependent by nature: the committed numbers are the ones this
repository's reference machine sustains, and the delta policy below governs
changing them.

**Memory.** The memory harness decodes synthetic audio of varying length
through a bounded chunk window and samples the operating system's own resident
set size (`mach_task_basic_info` on macOS, `/proc/self` on Linux,
`GetProcessMemoryInfo` on Windows). The assertion is that peak working-set
growth tracks the window, not the total decoded PCM. A deliberately unbounded
control run proves the harness can detect the failure it guards against. On a
platform with no RSS source the harness skips and prints the reason; it never
estimates a number.
";

fn push_row(out: &mut String, field: &str, value: &str) {
    md!(out, "| {field} | {value} |\n");
}

fn opt(value: Option<&String>) -> String {
    value
        .cloned()
        .unwrap_or_else(|| "not established".to_owned())
}

/// Human-readable byte count, exact value preserved in the JSON appendix.
#[must_use]
pub fn bytes_human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    #[allow(clippy::cast_precision_loss)]
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Human-readable wall time.
#[must_use]
pub fn millis_human(millis: u64) -> String {
    if millis < 1_000 {
        return format!("{millis} ms");
    }
    let seconds = millis / 1_000;
    let remainder = millis % 1_000;
    if seconds < 60 {
        return format!("{seconds}.{remainder:03} s");
    }
    format!("{} min {} s", seconds / 60, seconds % 60)
}

fn run(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let trimmed = text.trim().to_owned();
    (!trimmed.is_empty()).then_some(trimmed)
}

fn hardware_model() -> Option<String> {
    #[cfg(target_vendor = "apple")]
    {
        run("sysctl", &["-n", "hw.model"])
    }
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/sys/devices/virtual/dmi/id/product_name")
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .or_else(|| run("uname", &["-m"]))
    }
    #[cfg(windows)]
    {
        std::env::var("PROCESSOR_IDENTIFIER").ok()
    }
    #[cfg(not(any(target_vendor = "apple", target_os = "linux", windows)))]
    {
        None
    }
}

fn cpu_brand() -> Option<String> {
    #[cfg(target_vendor = "apple")]
    {
        run("sysctl", &["-n", "machdep.cpu.brand_string"])
    }
    #[cfg(target_os = "linux")]
    {
        let info = std::fs::read_to_string("/proc/cpuinfo").ok()?;
        info.lines()
            .find(|line| line.starts_with("model name"))
            .and_then(|line| line.split_once(':'))
            .map(|(_, value)| value.trim().to_owned())
    }
    #[cfg(windows)]
    {
        std::env::var("PROCESSOR_IDENTIFIER").ok()
    }
    #[cfg(not(any(target_vendor = "apple", target_os = "linux", windows)))]
    {
        None
    }
}

fn os_name() -> Option<String> {
    #[cfg(target_vendor = "apple")]
    {
        let product = run("sw_vers", &["-productName"])?;
        let version = run("sw_vers", &["-productVersion"])?;
        Some(format!("{product} {version}"))
    }
    #[cfg(target_os = "linux")]
    {
        let release = std::fs::read_to_string("/etc/os-release").ok()?;
        release
            .lines()
            .find_map(|line| line.strip_prefix("PRETTY_NAME="))
            .map(|value| value.trim_matches('"').to_owned())
    }
    #[cfg(windows)]
    {
        run("cmd", &["/C", "ver"])
    }
    #[cfg(not(any(target_vendor = "apple", target_os = "linux", windows)))]
    {
        None
    }
}

fn os_build() -> Option<String> {
    #[cfg(target_vendor = "apple")]
    {
        let build = run("sw_vers", &["-buildVersion"])?;
        let kernel = run("uname", &["-r"]).unwrap_or_else(|| "unknown kernel".to_owned());
        Some(format!("{build} (Darwin {kernel})"))
    }
    #[cfg(target_os = "linux")]
    {
        run("uname", &["-r"])
    }
    #[cfg(windows)]
    {
        run(
            "powershell",
            &[
                "-NoProfile",
                "-Command",
                "[System.Environment]::OSVersion.Version.ToString()",
            ],
        )
    }
    #[cfg(not(any(target_vendor = "apple", target_os = "linux", windows)))]
    {
        None
    }
}

/// Directory results are written to.
///
/// `PLAUDON_SCALE_RESULTS_DIR` wins; otherwise results land in the workspace
/// `target/` directory so an ordinary `cargo test` never rewrites a committed
/// document.
#[must_use]
pub fn results_directory() -> PathBuf {
    if let Ok(dir) = std::env::var("PLAUDON_SCALE_RESULTS_DIR") {
        if !dir.trim().is_empty() {
            return PathBuf::from(dir);
        }
    }
    // `tests/scale` -> workspace root, without a `..` in the printed path.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map_or_else(
            || PathBuf::from("target/scale-results"),
            |root| root.join("target/scale-results"),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::load::LoadStats;
    use crate::metrics::{Samples, Verdict};
    use crate::thresholds::{Budget, Thresholds};
    use std::time::Duration;

    fn stats() -> LoadStats {
        LoadStats {
            recordings: 2,
            media_assets: 2,
            transcript_revisions: 2,
            transcript_segments: 12,
            speakers: 1,
            source_items: 2,
            archive_bytes: 1_024,
            database_bytes: 4_096,
            load_wall_ms: 10,
            archive_wall_ms: 2,
        }
    }

    fn report(host: Host) -> RunReport {
        let mut samples = Samples::new("library_page_offset", "page");
        samples.push(Duration::from_micros(500));
        let verdict = Verdict::judge(
            samples.summarize(),
            Budget {
                p50_ms: 1.0,
                p95_ms: 1.0,
                max_ms: 1.0,
            },
        );
        RunReport::new(
            host,
            DatasetReport::new(&DatasetSpec::ci(7), "abc123".to_owned(), stats()),
            Thresholds::load().unwrap().policy().unwrap(),
            vec![verdict],
            None,
        )
    }

    fn full_host() -> Host {
        Host {
            hardware_model: Some("Test,1".to_owned()),
            cpu: Some("Test CPU".to_owned()),
            logical_cpus: Some(8),
            os: Some("TestOS 1.0".to_owned()),
            os_build: Some("build-1".to_owned()),
            arch: "aarch64".to_owned(),
            toolchain: Some("rustc 1.90.0".to_owned()),
            commit: Some("deadbeef".to_owned()),
            working_tree_dirty: Some(false),
        }
    }

    #[test]
    fn a_report_without_host_identification_is_not_citable() {
        let mut host = full_host();
        host.commit = None;
        let report = report(host);
        assert!(!report.is_citable());
        assert_eq!(report.host.missing_fields(), vec!["commit"]);
        assert!(report.to_markdown().contains("Not citable"));
    }

    #[test]
    fn a_fully_identified_report_is_citable_and_carries_every_field() {
        let report = report(full_host());
        assert!(report.is_citable());
        let markdown = report.to_markdown();
        for needle in [
            "Test,1",
            "TestOS 1.0",
            "build-1",
            "rustc 1.90.0",
            "deadbeef",
            "abc123",
            "library_page_offset",
            "Threshold delta policy",
        ] {
            assert!(markdown.contains(needle), "markdown is missing {needle}");
        }
        let json: serde_json::Value = serde_json::from_str(&report.to_json().unwrap()).unwrap();
        assert_eq!(json["schema"], RunReport::SCHEMA);
        assert_eq!(json["dataset"]["seed"], 7);
    }

    #[test]
    fn writing_emits_both_documents() {
        let dir = tempfile::tempdir().unwrap();
        let (json_path, markdown_path) = report(full_host()).write_to(dir.path()).unwrap();
        assert!(json_path.exists() && markdown_path.exists());
        let markdown = std::fs::read_to_string(&markdown_path).unwrap();
        assert!(markdown.starts_with("# N1.4"));
    }

    #[test]
    fn byte_and_time_formatting_is_readable_and_exact_at_the_boundaries() {
        assert_eq!(bytes_human(512), "512 B");
        assert_eq!(bytes_human(1024), "1.0 KiB");
        assert_eq!(bytes_human(1_572_864), "1.5 MiB");
        assert_eq!(millis_human(999), "999 ms");
        assert_eq!(millis_human(1_500), "1.500 s");
        assert_eq!(millis_human(125_000), "2 min 5 s");
    }

    #[test]
    fn this_machine_can_be_identified() {
        // The suite publishes results from this host, so the probes must work
        // here; a platform without probes fails loudly rather than publishing
        // an unidentified document.
        let host = Host::detect();
        assert!(
            host.missing_fields().is_empty(),
            "host probes failed for: {:?}",
            host.missing_fields()
        );
    }
}
