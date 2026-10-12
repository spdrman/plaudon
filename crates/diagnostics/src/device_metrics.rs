//! Direct-device quality metrics and the local report (spec §17.3A, §22(14),
//! App. B.9).
//!
//! Every metric is three-valued ([`Metric`]): a measured value, not
//! applicable, or unknown. An unknown is never rendered or stored as zero.
//!
//! Media fidelity classes ([`MediaFidelity`]) are kept as separate entries;
//! this module has no API that merges or averages them, and a derivative is
//! always labelled as not an original.
//!
//! The report only renders to a string or JSON value for display and for the
//! user to save deliberately. Nothing here sends data anywhere. Every string
//! is passed through the crate [`Redactor`] before it is stored or rendered.

use std::collections::BTreeMap;
use std::fmt::{self, Write as _};

use serde::{Deserialize, Serialize};

use crate::redact::Redactor;

/// A three-valued metric: never a zero standing in for an unknown.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", content = "value", rename_all = "snake_case")]
pub enum Metric<T> {
    /// A value was actually measured.
    Measured(T),
    /// The metric does not apply to this device or transport.
    NotApplicable,
    /// Nothing was measured; the honest answer is "unknown".
    Unknown,
}

impl<T> Metric<T> {
    /// Returns the measured value, if any.
    pub fn measured(&self) -> Option<&T> {
        match self {
            Self::Measured(v) => Some(v),
            _ => None,
        }
    }
}

impl<T: fmt::Display> fmt::Display for Metric<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Measured(v) => v.fmt(f),
            Self::NotApplicable => f.write_str("not applicable"),
            Self::Unknown => f.write_str("unknown"),
        }
    }
}

/// `numerator / denominator`; unknown when the denominator is zero.
fn ratio(numerator: u64, denominator: u64) -> Metric<f64> {
    if denominator == 0 {
        Metric::Unknown
    } else {
        #[allow(clippy::cast_precision_loss)]
        Metric::Measured(numerator as f64 / denominator as f64)
    }
}

fn fmt_ratio(m: &Metric<f64>) -> String {
    match m {
        Metric::Measured(r) => format!("{:.1}%", r * 100.0),
        other => other.to_string(),
    }
}

/// Adds to a counter metric, turning unknown into measured.
fn bump(m: &mut Metric<u64>, by: u64) {
    *m = Metric::Measured(m.measured().copied().unwrap_or(0).saturating_add(by));
}

/// Media fidelity classes. They are never merged or averaged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaFidelity {
    /// Raw WAV read from the USB volume.
    RawUsbWav,
    /// Raw `.ASR` captured over BLE.
    RawBleAsr,
    /// A decoded or remuxed derivative, never an original.
    DecodedDerivative,
}

impl MediaFidelity {
    /// Label used in the report.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::RawUsbWav => "raw USB WAV (as stored on device)",
            Self::RawBleAsr => "raw BLE .ASR (as received, not WAV)",
            Self::DecodedDerivative => "decoded/remuxed derivative (not an original)",
        }
    }
}

/// Totals for one fidelity class.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FidelityEntry {
    /// The class these totals belong to.
    pub class: MediaFidelity,
    /// Number of files recorded.
    pub files: u64,
    /// Total bytes.
    pub bytes: u64,
    /// Total duration in ms; unknown if any recorded file lacked a duration.
    pub duration_ms: Metric<u64>,
}

/// Throughput observation with an explicit-estimate ETA.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Throughput {
    /// Observed bytes per second.
    pub bytes_per_sec: Metric<f64>,
}

/// An ETA, always an estimate.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct EtaEstimate {
    /// Estimated seconds remaining.
    pub seconds: u64,
    /// Always true; the ETA is an estimate, not a promise.
    pub is_estimate: bool,
}

impl Throughput {
    /// ETA for `remaining_bytes`; unknown when throughput is unknown or not positive.
    #[must_use]
    pub fn eta(&self, remaining_bytes: u64) -> Metric<EtaEstimate> {
        match self.bytes_per_sec {
            Metric::Measured(bps) if bps.is_finite() && bps > 0.0 => {
                #[allow(
                    clippy::cast_precision_loss,
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss
                )]
                let seconds = (remaining_bytes as f64 / bps).ceil() as u64;
                Metric::Measured(EtaEstimate {
                    seconds,
                    is_estimate: true,
                })
            }
            Metric::NotApplicable => Metric::NotApplicable,
            _ => Metric::Unknown,
        }
    }
}

/// Summary of connection-latency samples.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LatencySummary {
    /// Number of samples.
    pub samples: u64,
    /// Fastest sample in ms.
    pub min_ms: u64,
    /// Slowest sample in ms.
    pub max_ms: u64,
    /// Mean in ms.
    pub mean_ms: f64,
}

/// Success/attempt pair.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tally {
    /// Successes.
    pub successes: u64,
    /// Attempts.
    pub attempts: u64,
}

impl Tally {
    /// Success rate; unknown with zero attempts.
    #[must_use]
    pub fn rate(&self) -> Metric<f64> {
        ratio(self.successes, self.attempts)
    }
}

/// Whether the profile may be considered passing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ProfileEligibility {
    /// Zero device-side mutations were measured.
    Eligible,
    /// The profile fails.
    Ineligible {
        /// Why.
        reason: String,
    },
    /// Not enough measurement to say.
    Undetermined {
        /// Why.
        reason: String,
    },
}

/// Collects raw observations and builds a [`DeviceMetricsReport`].
#[derive(Debug, Clone)]
pub struct DeviceMetricsCollector {
    profile_label: String,
    discovery_attempts: Metric<u64>,
    candidates_seen: Metric<u64>,
    false_positives: Metric<u64>,
    latencies_ms: Vec<u64>,
    auth_by_profile: BTreeMap<String, Tally>,
    listing_truth: Option<(u64, u64)>,
    throughput: Metric<f64>,
    transfers: Tally,
    reconnects: Tally,
    power_impact: Metric<f64>,
    suspend_recoveries: Tally,
    mutations: Metric<u64>,
    fidelity: BTreeMap<MediaFidelity, (u64, u64, Option<u64>)>,
}

impl DeviceMetricsCollector {
    /// New collector; `profile_label` is an opaque caller-chosen identifier.
    #[must_use]
    pub fn new(profile_label: &str) -> Self {
        Self {
            profile_label: scrub(profile_label),
            discovery_attempts: Metric::Unknown,
            candidates_seen: Metric::Unknown,
            false_positives: Metric::Unknown,
            latencies_ms: Vec::new(),
            auth_by_profile: BTreeMap::new(),
            listing_truth: None,
            throughput: Metric::Unknown,
            transfers: Tally::default(),
            reconnects: Tally::default(),
            power_impact: Metric::Unknown,
            suspend_recoveries: Tally::default(),
            mutations: Metric::Unknown,
            fidelity: BTreeMap::new(),
        }
    }

    /// Records one discovery attempt.
    pub fn record_discovery(&mut self, candidates_seen: u64, false_positives: u64) {
        bump(&mut self.discovery_attempts, 1);
        bump(&mut self.candidates_seen, candidates_seen);
        bump(&mut self.false_positives, false_positives);
    }

    /// Records a connection latency sample in ms.
    pub fn record_connection_latency_ms(&mut self, ms: u64) {
        self.latencies_ms.push(ms);
    }

    /// Records an authentication attempt for a firmware profile.
    pub fn record_auth(&mut self, firmware_profile: &str, success: bool) {
        let t = self
            .auth_by_profile
            .entry(scrub(firmware_profile))
            .or_default();
        t.attempts += 1;
        t.successes += u64::from(success);
    }

    /// Operator-supplied physical-device truth: the device holds
    /// `truth_count` recordings and the listing returned `listed_correct` of them.
    pub fn record_listing_truth(&mut self, truth_count: u64, listed_correct: u64) {
        self.listing_truth = Some((truth_count, listed_correct));
    }

    /// Records observed throughput in bytes per second.
    pub fn record_throughput(&mut self, bytes_per_sec: f64) {
        self.throughput = if bytes_per_sec.is_finite() && bytes_per_sec >= 0.0 {
            Metric::Measured(bytes_per_sec)
        } else {
            Metric::Unknown
        };
    }

    /// Records a transfer attempt and whether it was verified.
    pub fn record_transfer(&mut self, verified: bool) {
        self.transfers.attempts += 1;
        self.transfers.successes += u64::from(verified);
    }

    /// Records a reconnect attempt.
    pub fn record_reconnect(&mut self, success: bool) {
        self.reconnects.attempts += 1;
        self.reconnects.successes += u64::from(success);
    }

    /// Records a host-supplied Bluetooth power measurement (milliwatts).
    pub fn record_power_impact_mw(&mut self, mw: f64) {
        if mw.is_finite() && mw >= 0.0 {
            self.power_impact = Metric::Measured(mw);
        }
    }

    /// Records a mid-suspend recovery attempt.
    pub fn record_suspend_recovery(&mut self, recovered: bool) {
        self.suspend_recoveries.attempts += 1;
        self.suspend_recoveries.successes += u64::from(recovered);
    }

    /// Records observed device-side mutations (zero is a real measurement).
    pub fn record_device_mutations(&mut self, count: u64) {
        bump(&mut self.mutations, count);
    }

    /// Records one file in its own fidelity class; `duration_ms` may be unknown.
    pub fn record_media(&mut self, class: MediaFidelity, bytes: u64, duration_ms: Option<u64>) {
        let e = self.fidelity.entry(class).or_insert((0, 0, Some(0)));
        e.0 += 1;
        e.1 = e.1.saturating_add(bytes);
        e.2 = match (e.2, duration_ms) {
            (Some(a), Some(b)) => Some(a.saturating_add(b)),
            _ => None,
        };
    }

    /// Builds the report.
    #[must_use]
    pub fn build(&self) -> DeviceMetricsReport {
        let connection_latency = if self.latencies_ms.is_empty() {
            Metric::Unknown
        } else {
            let n = self.latencies_ms.len() as u64;
            let sum: u64 = self.latencies_ms.iter().sum();
            #[allow(clippy::cast_precision_loss)]
            let mean_ms = sum as f64 / n as f64;
            Metric::Measured(LatencySummary {
                samples: n,
                min_ms: *self.latencies_ms.iter().min().unwrap_or(&0),
                max_ms: *self.latencies_ms.iter().max().unwrap_or(&0),
                mean_ms,
            })
        };
        let file_list_completeness = match self.listing_truth {
            Some((truth, correct)) => ratio(correct.min(truth), truth),
            None => Metric::Unknown,
        };
        let eligibility = match self.mutations {
            Metric::Measured(0) => ProfileEligibility::Eligible,
            Metric::Measured(n) => ProfileEligibility::Ineligible {
                reason: format!(
                    "{n} device-side mutation(s) observed; a passing profile requires zero"
                ),
            },
            _ => ProfileEligibility::Undetermined {
                reason: "device-side mutation count was not measured".to_owned(),
            },
        };
        DeviceMetricsReport {
            profile_label: self.profile_label.clone(),
            discovery_attempts: self.discovery_attempts,
            candidates_seen: self.candidates_seen,
            false_positive_identifications: self.false_positives,
            connection_latency,
            auth_by_firmware_profile: self
                .auth_by_profile
                .iter()
                .map(|(k, t)| (k.clone(), *t))
                .collect(),
            file_list_completeness,
            throughput: Throughput {
                bytes_per_sec: self.throughput,
            },
            verified_transfer_rate: self.transfers.rate(),
            transfers: self.transfers,
            reconnect_success: self.reconnects.rate(),
            bluetooth_power_impact_mw: self.power_impact,
            mid_suspend_recovery: self.suspend_recoveries.rate(),
            device_mutation_count: self.mutations,
            eligibility,
            fidelity: self
                .fidelity
                .iter()
                .map(|(class, (files, bytes, dur))| FidelityEntry {
                    class: *class,
                    files: *files,
                    bytes: *bytes,
                    duration_ms: dur.map_or(Metric::Unknown, Metric::Measured),
                })
                .collect(),
        }
    }
}

fn scrub(s: &str) -> String {
    Redactor::shared().redact(s).into_owned()
}

/// The local-only direct-device metrics report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceMetricsReport {
    /// Opaque profile identifier supplied by the caller (redacted).
    pub profile_label: String,
    /// Discovery attempts.
    pub discovery_attempts: Metric<u64>,
    /// Candidates seen across attempts.
    pub candidates_seen: Metric<u64>,
    /// Devices wrongly identified as a PLAUD device.
    pub false_positive_identifications: Metric<u64>,
    /// Connection latency summary.
    pub connection_latency: Metric<LatencySummary>,
    /// Authentication tally per firmware profile.
    pub auth_by_firmware_profile: Vec<(String, Tally)>,
    /// Listed-correct / physical truth; unknown without operator truth.
    pub file_list_completeness: Metric<f64>,
    /// Throughput; use [`Throughput::eta`] for an estimate.
    pub throughput: Throughput,
    /// Verified / attempted transfers.
    pub verified_transfer_rate: Metric<f64>,
    /// Raw transfer tally.
    pub transfers: Tally,
    /// Reconnect success rate.
    pub reconnect_success: Metric<f64>,
    /// Bluetooth power impact in mW; unknown unless the host measured it.
    pub bluetooth_power_impact_mw: Metric<f64>,
    /// Mid-suspend recovery rate.
    pub mid_suspend_recovery: Metric<f64>,
    /// Device-side mutations; zero is required for a passing profile.
    pub device_mutation_count: Metric<u64>,
    /// Eligibility derived from the mutation count.
    pub eligibility: ProfileEligibility,
    /// One entry per fidelity class that has data; never merged.
    pub fidelity: Vec<FidelityEntry>,
}

impl DeviceMetricsReport {
    /// ETA for `remaining_bytes` using the report's throughput (an estimate).
    #[must_use]
    pub fn eta(&self, remaining_bytes: u64) -> Metric<EtaEstimate> {
        self.throughput.eta(remaining_bytes)
    }

    /// Pretty JSON for display or deliberate saving by the user.
    ///
    /// # Errors
    /// Returns the serializer error (not expected for this type).
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Parses a report previously produced by [`Self::to_json`].
    ///
    /// # Errors
    /// Returns the deserializer error for malformed input.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// Human-readable text; the whole string is passed through the redactor.
    #[must_use]
    pub fn render_text(&self) -> String {
        let mut o = String::new();
        let _ = writeln!(o, "Direct-device metrics (local report, experimental)");
        let _ = writeln!(o, "profile: {}", self.profile_label);
        let _ = writeln!(o, "eligibility: {}", eligibility_text(&self.eligibility));
        let _ = writeln!(o, "discovery attempts: {}", self.discovery_attempts);
        let _ = writeln!(o, "candidates seen: {}", self.candidates_seen);
        let _ = writeln!(
            o,
            "false-positive identifications: {}",
            self.false_positive_identifications
        );
        let _ = match &self.connection_latency {
            Metric::Measured(l) => writeln!(
                o,
                "connection latency: min {} ms / mean {:.1} ms / max {} ms ({} samples)",
                l.min_ms, l.mean_ms, l.max_ms, l.samples
            ),
            Metric::NotApplicable => writeln!(o, "connection latency: not applicable"),
            Metric::Unknown => writeln!(o, "connection latency: unknown"),
        };
        if self.auth_by_firmware_profile.is_empty() {
            let _ = writeln!(o, "authentication success: unknown");
        }
        for (fw, t) in &self.auth_by_firmware_profile {
            let _ = writeln!(
                o,
                "authentication success [{fw}]: {} ({}/{})",
                fmt_ratio(&t.rate()),
                t.successes,
                t.attempts
            );
        }
        let _ = writeln!(
            o,
            "file-list completeness: {}",
            fmt_ratio(&self.file_list_completeness)
        );
        let _ = match self.throughput.bytes_per_sec {
            Metric::Measured(b) => writeln!(o, "throughput: {b:.0} bytes/sec (ETA is an estimate)"),
            other => writeln!(o, "throughput: {other}"),
        };
        let _ = writeln!(
            o,
            "verified-transfer rate: {} ({}/{})",
            fmt_ratio(&self.verified_transfer_rate),
            self.transfers.successes,
            self.transfers.attempts
        );
        let _ = writeln!(
            o,
            "reconnect success: {}",
            fmt_ratio(&self.reconnect_success)
        );
        let _ = match self.bluetooth_power_impact_mw {
            Metric::Measured(mw) => writeln!(o, "bluetooth power impact: {mw:.1} mW"),
            other => writeln!(o, "bluetooth power impact: {other}"),
        };
        let _ = writeln!(
            o,
            "mid-suspend recovery: {}",
            fmt_ratio(&self.mid_suspend_recovery)
        );
        let _ = writeln!(o, "device-side mutations: {}", self.device_mutation_count);
        let _ = writeln!(o, "media fidelity classes (never merged):");
        if self.fidelity.is_empty() {
            let _ = writeln!(o, "  none recorded");
        }
        for e in &self.fidelity {
            let _ = writeln!(
                o,
                "  {}: {} file(s), {} bytes, duration {}",
                e.class.label(),
                e.files,
                e.bytes,
                match e.duration_ms {
                    Metric::Measured(ms) => format!("{ms} ms"),
                    other => other.to_string(),
                }
            );
        }
        scrub(&o)
    }
}

fn eligibility_text(e: &ProfileEligibility) -> String {
    match e {
        ProfileEligibility::Eligible => "eligible (zero device-side mutations measured)".to_owned(),
        ProfileEligibility::Ineligible { reason } => format!("INELIGIBLE: {reason}"),
        ProfileEligibility::Undetermined { reason } => format!("undetermined: {reason}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_values_render_as_unknown_not_zero() {
        let text = DeviceMetricsCollector::new("p").build().render_text();
        assert!(text.contains("discovery attempts: unknown"));
        assert!(text.contains("file-list completeness: unknown"));
        assert!(text.contains("bluetooth power impact: unknown"));
        assert!(text.contains("device-side mutations: unknown"));
        assert!(text.contains("verified-transfer rate: unknown"));
        assert!(!text.contains("attempts: 0"));
    }

    #[test]
    fn eta_is_an_estimate_and_absent_when_throughput_unknown() {
        let unknown = DeviceMetricsCollector::new("p").build();
        assert_eq!(unknown.eta(1000), Metric::Unknown);
        let mut c = DeviceMetricsCollector::new("p");
        c.record_throughput(100.0);
        let r = c.build();
        assert_eq!(
            r.eta(1000),
            Metric::Measured(EtaEstimate {
                seconds: 10,
                is_estimate: true
            })
        );
        assert!(r.render_text().contains("ETA is an estimate"));
        let mut z = DeviceMetricsCollector::new("p");
        z.record_throughput(0.0);
        assert_eq!(z.build().eta(10), Metric::Unknown);
    }

    #[test]
    fn verified_transfer_rate_arithmetic_and_zero_attempts() {
        let mut c = DeviceMetricsCollector::new("p");
        assert_eq!(c.build().verified_transfer_rate, Metric::Unknown);
        c.record_transfer(true);
        c.record_transfer(true);
        c.record_transfer(true);
        c.record_transfer(false);
        let r = c.build();
        assert_eq!(r.verified_transfer_rate, Metric::Measured(0.75));
        assert!(r.render_text().contains("75.0% (3/4)"));
    }

    #[test]
    fn nonzero_mutation_count_makes_profile_ineligible() {
        let mut c = DeviceMetricsCollector::new("p");
        c.record_device_mutations(0);
        assert_eq!(c.build().eligibility, ProfileEligibility::Eligible);
        c.record_device_mutations(2);
        let r = c.build();
        assert!(matches!(
            r.eligibility,
            ProfileEligibility::Ineligible { .. }
        ));
        assert!(r.render_text().contains("INELIGIBLE"));
        assert!(matches!(
            DeviceMetricsCollector::new("p").build().eligibility,
            ProfileEligibility::Undetermined { .. }
        ));
    }

    #[test]
    fn fidelity_entries_never_collapse() {
        let mut c = DeviceMetricsCollector::new("p");
        c.record_media(MediaFidelity::RawUsbWav, 1000, Some(500));
        c.record_media(MediaFidelity::RawBleAsr, 200, None);
        c.record_media(MediaFidelity::DecodedDerivative, 300, Some(40));
        c.record_media(MediaFidelity::RawBleAsr, 50, Some(10));
        let r = c.build();
        assert_eq!(r.fidelity.len(), 3);
        let asr = r
            .fidelity
            .iter()
            .find(|e| e.class == MediaFidelity::RawBleAsr)
            .unwrap();
        assert_eq!((asr.files, asr.bytes), (2, 250));
        assert_eq!(asr.duration_ms, Metric::Unknown);
        let wav = r
            .fidelity
            .iter()
            .find(|e| e.class == MediaFidelity::RawUsbWav)
            .unwrap();
        assert_eq!((wav.bytes, wav.duration_ms), (1000, Metric::Measured(500)));
        let text = r.render_text();
        assert!(text.contains("derivative (not an original)"));
        assert!(text.contains(".ASR (as received, not WAV)"));
    }

    #[test]
    fn completeness_needs_operator_truth() {
        let mut c = DeviceMetricsCollector::new("p");
        assert_eq!(c.build().file_list_completeness, Metric::Unknown);
        c.record_listing_truth(4, 3);
        assert_eq!(c.build().file_list_completeness, Metric::Measured(0.75));
    }

    #[test]
    fn credentials_in_labels_are_redacted() {
        let secret = "Bearer abcdefghijklmnopqrstuvwxyz0123456789";
        let c = DeviceMetricsCollector::new(secret);
        let r = c.build();
        assert!(!r
            .profile_label
            .contains("abcdefghijklmnopqrstuvwxyz0123456789"));
        assert!(!r
            .render_text()
            .contains("abcdefghijklmnopqrstuvwxyz0123456789"));
    }

    #[test]
    fn module_source_has_no_network_or_telemetry_code() {
        let src = include_str!("device_metrics.rs");
        let production = src.split("#[cfg(test)]").next().unwrap().to_lowercase();
        for needle in [
            ["ht", "tp"],
            ["req", "west"],
            ["up", "load"],
            ["tele", "metry"],
        ] {
            let needle = needle.concat();
            assert!(!production.contains(&needle), "found `{needle}`");
        }
    }

    #[test]
    fn json_round_trip() {
        let mut c = DeviceMetricsCollector::new("p");
        c.record_discovery(3, 1);
        c.record_connection_latency_ms(120);
        c.record_auth("fw-a", true);
        c.record_throughput(1234.5);
        c.record_transfer(true);
        c.record_device_mutations(0);
        c.record_media(MediaFidelity::RawUsbWav, 10, Some(5));
        let r = c.build();
        let back = DeviceMetricsReport::from_json(&r.to_json().unwrap()).unwrap();
        assert_eq!(r, back);
        assert!(r.to_json().unwrap().contains("\"unknown\""));
    }
}
