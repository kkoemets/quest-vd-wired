use std::{collections::BTreeSet, fs, process::Command};

use gnirehtet_tun2proxy_comparator::{
    CANDIDATE, UPSTREAM_REVISION, UPSTREAM_VERSION,
    adapter::UdpMode,
    result::{Capture, HarnessMetrics, HostMetrics, Termination},
};
use serde_json::Value;
use tempfile::tempdir;

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_gnirehtet-tun2proxy-comparator")
}

#[test]
fn upstream_pin_is_consistent_across_manifest_lock_and_notice() {
    let manifest = include_str!("../Cargo.toml");
    let lock = include_str!("../Cargo.lock");
    let notice = include_str!("../THIRD_PARTY_NOTICES.md");
    for source in [manifest, lock, notice] {
        assert!(source.contains(UPSTREAM_REVISION));
    }
    assert!(notice.contains("8cddc80ccbbb14a8a3d7fee1fc1795d7fcd647f4c7063ad95246f9ff24b407c7"));
    assert!(lock.contains("name = \"tun2proxy\"\nversion = \"0.8.2\""));
}

#[test]
fn plan_is_explicitly_not_hardware_validated() {
    let output = Command::new(binary())
        .args([
            "plan",
            "--proxy",
            "socks5://127.0.0.1:31416",
            "--udp-mode",
            "socks5-udp-associate",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let document: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["upstreamRevision"], UPSTREAM_REVISION);
    assert_eq!(document["hardwareValidated"], false);
    assert_eq!(document["adbReverseReady"], false);
}

#[test]
fn render_result_matches_shared_required_vocabulary() {
    let directory = tempdir().unwrap();
    let capture_path = directory.path().join("capture.json");
    let harness_path = directory.path().join("harness.json");
    let artifact_path = directory.path().join("redacted-counters.json");
    let result_path = directory.path().join("result.json");

    let capture = Capture {
        format_version: 1,
        candidate: CANDIDATE.into(),
        upstream_version: UPSTREAM_VERSION.into(),
        upstream_revision: UPSTREAM_REVISION.into(),
        udp_mode: UdpMode::UdpGwOverTcp,
        started_at: "2026-07-12T12:00:00Z".into(),
        duration_seconds: 60,
        forward_bytes: 3_000_000_000,
        reverse_bytes: 375_000_000,
        sessions_at_exit: 0,
        termination: Termination::Cancelled,
    };
    let harness = HarnessMetrics {
        raw_adb_ceiling_bits_per_second: 500_000_000,
        relay_drops: 0,
        stale_drops: 2,
        control_rtt_ms_samples: vec![0.5, 1.0, 1.5, 2.0],
        udp_queue_residence_ms_samples: vec![0.25, 1.0, 4.5, 9.0],
        host: HostMetrics {
            cpu_core_percent: 7.5,
            rss_bytes: 20_000_000,
            rss_growth_bytes: 100_000,
        },
        artifact_paths: vec!["redacted-counters.json".into()],
        hardware_evidence: None,
    };
    fs::write(&capture_path, serde_json::to_vec_pretty(&capture).unwrap()).unwrap();
    fs::write(&harness_path, serde_json::to_vec_pretty(&harness).unwrap()).unwrap();
    fs::write(&artifact_path, b"{\"drops\":2}\n").unwrap();

    let output = Command::new(binary())
        .args([
            "render-result",
            "--capture",
            capture_path.to_str().unwrap(),
            "--harness-metrics",
            harness_path.to_str().unwrap(),
            "--commit",
            "0000000000000000000000000000000000000000",
            "--output",
            result_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let result: Value = serde_json::from_slice(&fs::read(result_path).unwrap()).unwrap();
    let schema: Value = serde_json::from_str(include_str!("../../../result.schema.json")).unwrap();
    let expected: BTreeSet<&str> = schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect();
    let actual: BTreeSet<&str> = result
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(actual, expected);
    assert_eq!(result["candidate"], "tun2proxy-rust");
    assert_eq!(result["hardwareEvidence"], Value::Null);
    assert_eq!(result["artifacts"].as_array().unwrap().len(), 1);
}

#[test]
fn render_result_rejects_missing_latency_samples() {
    let directory = tempdir().unwrap();
    let capture_path = directory.path().join("capture.json");
    let harness_path = directory.path().join("harness.json");
    let result_path = directory.path().join("result.json");
    fs::write(
        &capture_path,
        format!(
            r#"{{"formatVersion":1,"candidate":"{CANDIDATE}","upstreamVersion":"{UPSTREAM_VERSION}","upstreamRevision":"{UPSTREAM_REVISION}","udpMode":"socks5-udp-associate","startedAt":"2026-07-12T12:00:00Z","durationSeconds":1,"forwardBytes":0,"reverseBytes":0,"sessionsAtExit":0,"termination":"cancelled"}}"#
        ),
    )
    .unwrap();
    fs::write(
        &harness_path,
        r#"{"rawAdbCeilingBitsPerSecond":1,"relayDrops":0,"staleDrops":0,"controlRttMsSamples":[],"udpQueueResidenceMsSamples":[1],"host":{"cpuCorePercent":0,"rssBytes":0,"rssGrowthBytes":0},"artifactPaths":[],"hardwareEvidence":null}"#,
    )
    .unwrap();
    let output = Command::new(binary())
        .args([
            "render-result",
            "--capture",
            capture_path.to_str().unwrap(),
            "--harness-metrics",
            harness_path.to_str().unwrap(),
            "--commit",
            "0000000000000000000000000000000000000000",
            "--output",
            result_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!result_path.exists());
}
