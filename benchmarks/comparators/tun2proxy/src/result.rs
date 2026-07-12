use std::{
    collections::BTreeMap,
    fmt::Write as _,
    fs::File,
    io::Read,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use chrono::DateTime;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{CANDIDATE, UPSTREAM_REVISION, UPSTREAM_VERSION, adapter::UdpMode};

const MAX_SAMPLES: usize = 1_000_000;
const MAX_SAMPLE_MS: f64 = 60_000.0;
const MAX_ARTIFACTS: usize = 128;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Capture {
    pub format_version: u8,
    pub candidate: String,
    pub upstream_version: String,
    pub upstream_revision: String,
    pub udp_mode: UdpMode,
    pub started_at: String,
    pub duration_seconds: u64,
    pub forward_bytes: u64,
    pub reverse_bytes: u64,
    pub sessions_at_exit: usize,
    pub termination: Termination,
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Termination {
    Cancelled,
    EngineCompleted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HarnessMetrics {
    pub raw_adb_ceiling_bits_per_second: u64,
    pub relay_drops: u64,
    pub stale_drops: u64,
    pub control_rtt_ms_samples: Vec<f64>,
    pub udp_queue_residence_ms_samples: Vec<f64>,
    pub host: HostMetrics,
    #[serde(default)]
    pub artifact_paths: Vec<PathBuf>,
    #[serde(default)]
    pub hardware_evidence: Option<HardwareEvidence>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostMetrics {
    pub cpu_core_percent: f64,
    pub rss_bytes: u64,
    pub rss_growth_bytes: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HardwareEvidence {
    pub quest_version: String,
    pub windows_version: String,
    pub streamer_version: String,
    pub usb_topology: String,
    pub settings: BTreeMap<String, serde_json::Value>,
    pub thermal_artifact: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BenchmarkResult {
    pub schema_version: u8,
    pub candidate: String,
    pub commit: String,
    pub started_at: String,
    pub duration_seconds: u64,
    pub raw_adb_ceiling_bits_per_second: u64,
    pub forward_bits_per_second: u64,
    pub reverse_bits_per_second: u64,
    pub relay_drops: u64,
    pub stale_drops: u64,
    pub control_rtt_ms: Percentiles,
    pub udp_queue_residence_ms: Percentiles,
    pub host: HostMetrics,
    pub artifacts: Vec<Artifact>,
    pub hardware_evidence: Option<HardwareEvidence>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Percentiles {
    pub p50: f64,
    pub p99: f64,
    pub p999: f64,
    pub max: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub path: String,
    pub sha256: String,
}

pub fn render_result(
    capture: &Capture,
    harness: &HarnessMetrics,
    commit: &str,
    artifact_base: &Path,
) -> Result<BenchmarkResult> {
    validate_capture(capture)?;
    validate_commit(commit)?;
    ensure!(
        harness.raw_adb_ceiling_bits_per_second > 0,
        "raw ADB ceiling must be positive"
    );
    ensure!(
        harness.host.cpu_core_percent.is_finite() && harness.host.cpu_core_percent >= 0.0,
        "host CPU metric must be finite and non-negative"
    );
    if let Some(evidence) = &harness.hardware_evidence {
        validate_hardware_evidence(evidence)?;
    }

    let artifacts = hash_artifacts(&harness.artifact_paths, artifact_base)?;
    Ok(BenchmarkResult {
        schema_version: 1,
        candidate: CANDIDATE.into(),
        commit: commit.into(),
        started_at: capture.started_at.clone(),
        duration_seconds: capture.duration_seconds,
        raw_adb_ceiling_bits_per_second: harness.raw_adb_ceiling_bits_per_second,
        forward_bits_per_second: bits_per_second(capture.forward_bytes, capture.duration_seconds),
        reverse_bits_per_second: bits_per_second(capture.reverse_bytes, capture.duration_seconds),
        relay_drops: harness.relay_drops,
        stale_drops: harness.stale_drops,
        control_rtt_ms: percentiles(&harness.control_rtt_ms_samples)?,
        udp_queue_residence_ms: percentiles(&harness.udp_queue_residence_ms_samples)?,
        host: harness.host.clone(),
        artifacts,
        hardware_evidence: harness.hardware_evidence.clone(),
    })
}

fn validate_capture(capture: &Capture) -> Result<()> {
    ensure!(capture.format_version == 1, "capture format must be 1");
    ensure!(capture.candidate == CANDIDATE, "capture candidate mismatch");
    ensure!(
        capture.upstream_version == UPSTREAM_VERSION,
        "upstream version mismatch"
    );
    ensure!(
        capture.upstream_revision == UPSTREAM_REVISION,
        "upstream revision mismatch"
    );
    ensure!(
        capture.duration_seconds > 0,
        "capture duration must be positive"
    );
    DateTime::parse_from_rfc3339(&capture.started_at)
        .context("capture startedAt is not RFC3339")?;
    Ok(())
}

fn validate_commit(commit: &str) -> Result<()> {
    ensure!(
        commit.len() == 40
            && commit
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "commit must be 40 lowercase hexadecimal characters"
    );
    Ok(())
}

fn validate_hardware_evidence(evidence: &HardwareEvidence) -> Result<()> {
    for (name, value) in [
        ("questVersion", evidence.quest_version.as_str()),
        ("windowsVersion", evidence.windows_version.as_str()),
        ("streamerVersion", evidence.streamer_version.as_str()),
        ("usbTopology", evidence.usb_topology.as_str()),
        ("thermalArtifact", evidence.thermal_artifact.as_str()),
    ] {
        ensure!(
            !value.trim().is_empty(),
            "hardware evidence {name} is empty"
        );
    }
    Ok(())
}

fn bits_per_second(bytes: u64, seconds: u64) -> u64 {
    bytes.saturating_mul(8) / seconds
}

fn percentiles(samples: &[f64]) -> Result<Percentiles> {
    ensure!(!samples.is_empty(), "percentile sample set is empty");
    ensure!(
        samples.len() <= MAX_SAMPLES,
        "percentile sample set exceeds {MAX_SAMPLES}"
    );
    let mut sorted = samples.to_vec();
    for sample in &sorted {
        ensure!(
            sample.is_finite() && (0.0..=MAX_SAMPLE_MS).contains(sample),
            "percentile sample must be finite and within 0..={MAX_SAMPLE_MS} ms"
        );
    }
    sorted.sort_by(f64::total_cmp);
    Ok(Percentiles {
        p50: nearest_rank(&sorted, 0.5),
        p99: nearest_rank(&sorted, 0.99),
        p999: nearest_rank(&sorted, 0.999),
        max: *sorted.last().expect("non-empty sample set"),
    })
}

fn nearest_rank(sorted: &[f64], quantile: f64) -> f64 {
    let rank = (quantile * sorted.len() as f64).ceil() as usize;
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

fn hash_artifacts(paths: &[PathBuf], base: &Path) -> Result<Vec<Artifact>> {
    ensure!(
        paths.len() <= MAX_ARTIFACTS,
        "artifact count exceeds {MAX_ARTIFACTS}"
    );
    paths
        .iter()
        .map(|relative| {
            ensure_safe_relative_path(relative)?;
            let source = base.join(relative);
            let metadata = source
                .metadata()
                .with_context(|| format!("artifact is missing: {}", relative.display()))?;
            ensure!(
                metadata.is_file(),
                "artifact is not a regular file: {}",
                relative.display()
            );
            Ok(Artifact {
                path: relative.to_string_lossy().replace('\\', "/"),
                sha256: sha256(&source)?,
            })
        })
        .collect()
}

fn ensure_safe_relative_path(path: &Path) -> Result<()> {
    ensure!(!path.as_os_str().is_empty(), "artifact path is empty");
    for component in path.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                bail!("artifact path must remain below the harness directory")
            }
        }
    }
    Ok(())
}

fn sha256(path: &Path) -> Result<String> {
    let mut source = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = source.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    let mut output = String::with_capacity(64);
    for byte in digest.finalize() {
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calculates_nearest_rank_percentiles() {
        let values: Vec<f64> = (1..=1_000).map(f64::from).collect();
        let actual = percentiles(&values).unwrap();
        assert_eq!(actual.p50, 500.0);
        assert_eq!(actual.p99, 990.0);
        assert_eq!(actual.p999, 999.0);
        assert_eq!(actual.max, 1_000.0);
    }

    #[test]
    fn rejects_unbounded_samples() {
        assert!(percentiles(&[f64::NAN]).is_err());
        assert!(percentiles(&[MAX_SAMPLE_MS + 1.0]).is_err());
    }

    #[test]
    fn rejects_parent_artifact_path() {
        assert!(ensure_safe_relative_path(Path::new("../secret")).is_err());
    }
}
