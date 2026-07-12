use std::{
    fs,
    net::SocketAddr,
    num::NonZeroU64,
    path::{Path, PathBuf},
    time::Instant,
};

use anyhow::{Context, Result, ensure};
use chrono::{SecondsFormat, Utc};
use clap::{Args, Parser, Subcommand};
use gnirehtet_tun2proxy_comparator::{
    CANDIDATE, UPSTREAM_REVISION, UPSTREAM_VERSION,
    adapter::{AdapterConfig, UdpMode, run_adapter},
    result::{Capture, HarnessMetrics, Termination, render_result},
};
use serde::Serialize;
use tun2proxy::CancellationToken;

const MAX_JSON_INPUT_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Parser)]
#[command(about = "Non-production tun2proxy dataplane comparator")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Describe a validated adapter plan without opening a TUN descriptor.
    Plan(PlanArgs),
    /// Run against an already-established Android/Unix TUN descriptor.
    Run(RunArgs),
    /// Combine a capture and external harness metrics into result.schema.json vocabulary.
    RenderResult(RenderResultArgs),
}

#[derive(Debug, Args)]
struct TransportArgs {
    #[arg(long)]
    proxy: String,
    #[arg(long, value_enum)]
    udp_mode: UdpMode,
    #[arg(long)]
    udpgw_server: Option<SocketAddr>,
    #[arg(long, default_value_t = 1_500)]
    mtu: u16,
    #[arg(long, default_value_t = 600)]
    tcp_timeout_seconds: u64,
    #[arg(long, default_value_t = 10)]
    udp_timeout_seconds: u64,
    #[arg(long, default_value_t = 256)]
    max_sessions: usize,
}

#[derive(Debug, Args)]
struct PlanArgs {
    #[command(flatten)]
    transport: TransportArgs,
    #[arg(long)]
    output: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct RunArgs {
    #[command(flatten)]
    transport: TransportArgs,
    #[arg(long)]
    tun_fd: i32,
    /// Let the adapter close the descriptor when the native device drops. Defaults to false.
    #[arg(long, default_value_t = false)]
    close_fd_on_drop: bool,
    #[arg(long, default_value_t = false)]
    packet_information: bool,
    /// Cancel automatically after this duration; Ctrl-C also cancels.
    #[arg(long)]
    duration_seconds: Option<NonZeroU64>,
    #[arg(long)]
    capture: PathBuf,
    #[arg(long)]
    result: Option<PathBuf>,
    #[arg(long)]
    harness_metrics: Option<PathBuf>,
    #[arg(long)]
    commit: Option<String>,
}

#[derive(Debug, Args)]
struct RenderResultArgs {
    #[arg(long)]
    capture: PathBuf,
    #[arg(long)]
    harness_metrics: PathBuf,
    #[arg(long)]
    commit: String,
    #[arg(long)]
    output: PathBuf,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AdapterPlan {
    candidate: &'static str,
    upstream_version: &'static str,
    upstream_revision: &'static str,
    udp_mode: UdpMode,
    requires_android_or_unix_tun_fd: bool,
    closes_tun_fd_by_default: bool,
    udp_uses_stream_transport: bool,
    adb_reverse_ready: bool,
    lifecycle_cancellation: &'static str,
    hardware_validated: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Plan(args) => plan(args),
        Command::Run(args) => run(args).await,
        Command::RenderResult(args) => render(args),
    }
}

fn plan(args: PlanArgs) -> Result<()> {
    let config = adapter_config(&args.transport, 0, false, false);
    config.validate()?;
    let stream_udp = args.transport.udp_mode == UdpMode::UdpGwOverTcp;
    let plan = AdapterPlan {
        candidate: CANDIDATE,
        upstream_version: UPSTREAM_VERSION,
        upstream_revision: UPSTREAM_REVISION,
        udp_mode: args.transport.udp_mode,
        requires_android_or_unix_tun_fd: true,
        closes_tun_fd_by_default: false,
        udp_uses_stream_transport: stream_udp,
        // A matching UdpGW server and Android packaging do not exist in this experiment.
        adb_reverse_ready: false,
        lifecycle_cancellation: "tun2proxy CancellationToken",
        hardware_validated: false,
    };
    write_json_or_stdout(args.output.as_deref(), &plan)
}

async fn run(args: RunArgs) -> Result<()> {
    let result_inputs = (&args.result, &args.harness_metrics, &args.commit);
    ensure!(
        matches!(
            result_inputs,
            (None, None, None) | (Some(_), Some(_), Some(_))
        ),
        "--result, --harness-metrics, and --commit must be supplied together"
    );
    let config = adapter_config(
        &args.transport,
        args.tun_fd,
        args.close_fd_on_drop,
        args.packet_information,
    );
    config.validate()?;

    let cancellation = CancellationToken::new();
    let signal_task = {
        let cancellation = cancellation.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                cancellation.cancel();
            }
        })
    };
    let duration_task = args.duration_seconds.map(|duration| {
        let cancellation = cancellation.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(duration.get())).await;
            cancellation.cancel();
        })
    });

    let started_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let started = Instant::now();
    let outcome = run_adapter(config, cancellation.clone()).await;
    let was_cancelled = cancellation.is_cancelled();
    signal_task.abort();
    if let Some(task) = duration_task {
        task.abort();
    }
    let outcome = outcome?;
    let capture = Capture {
        format_version: 1,
        candidate: CANDIDATE.into(),
        upstream_version: UPSTREAM_VERSION.into(),
        upstream_revision: UPSTREAM_REVISION.into(),
        udp_mode: args.transport.udp_mode,
        started_at,
        duration_seconds: started.elapsed().as_secs().max(1),
        forward_bytes: outcome.forward_bytes,
        reverse_bytes: outcome.reverse_bytes,
        sessions_at_exit: outcome.sessions_at_exit,
        termination: if was_cancelled {
            Termination::Cancelled
        } else {
            Termination::EngineCompleted
        },
    };
    write_json(&args.capture, &capture)?;

    if let (Some(result), Some(harness), Some(commit)) = result_inputs {
        render_to_file(&args.capture, harness, commit, result)?;
    }
    Ok(())
}

fn render(args: RenderResultArgs) -> Result<()> {
    render_to_file(
        &args.capture,
        &args.harness_metrics,
        &args.commit,
        &args.output,
    )
}

fn render_to_file(
    capture_path: &Path,
    harness_path: &Path,
    commit: &str,
    output: &Path,
) -> Result<()> {
    let capture: Capture = read_json(capture_path)?;
    let harness: HarnessMetrics = read_json(harness_path)?;
    let artifact_base = harness_path.parent().unwrap_or_else(|| Path::new("."));
    let result = render_result(&capture, &harness, commit, artifact_base)?;
    write_json(output, &result)
}

fn adapter_config(
    args: &TransportArgs,
    tun_fd: i32,
    close_fd_on_drop: bool,
    packet_information: bool,
) -> AdapterConfig {
    AdapterConfig {
        tun_fd,
        close_fd_on_drop,
        packet_information,
        proxy: args.proxy.clone(),
        udp_mode: args.udp_mode,
        udpgw_server: args.udpgw_server,
        mtu: args.mtu,
        tcp_timeout_seconds: args.tcp_timeout_seconds,
        udp_timeout_seconds: args.udp_timeout_seconds,
        max_sessions: args.max_sessions,
    }
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let metadata = path
        .metadata()
        .with_context(|| format!("could not inspect {}", path.display()))?;
    ensure!(
        metadata.len() <= MAX_JSON_INPUT_BYTES,
        "JSON input exceeds 16 MiB"
    );
    let bytes = fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("invalid JSON in {}", path.display()))
}

fn write_json_or_stdout<T: Serialize>(path: Option<&Path>, value: &T) -> Result<()> {
    match path {
        Some(path) => write_json(path, value),
        None => {
            println!("{}", serde_json::to_string_pretty(value)?);
            Ok(())
        }
    }
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(value)?)?;
    fs::rename(&temporary, path)?;
    Ok(())
}
