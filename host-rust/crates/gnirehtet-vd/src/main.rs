use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream as StdTcpStream},
    path::PathBuf,
    process::{Child, Command as ProcessCommand, Stdio},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

#[cfg(target_os = "windows")]
use std::io::Write;
#[cfg(any(target_os = "windows", test))]
use std::sync::Mutex as StdMutex;

#[cfg(any(target_os = "windows", test))]
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use gnirehtet_vd::{
    adb::{
        repair_adb_if_missing, resolve_adb_program, AdbController, SystemAdb,
        ADB_MAPPING_COMMAND_TIMEOUT, CONTROL_PORT, SOCKS_PORT, UDP_STREAM_PORT,
        VIRTUAL_DESKTOP_PACKAGE,
    },
    diagnostics::Diagnostics,
    embedded,
    protocol::SessionId,
    runtime::{
        admin_command, doctor, process_is_running, read_daemon_pid, terminate_daemon,
        AdminResponse, AppPaths, HostRuntime, OperationGuard, RuntimeConfig, StateStore,
    },
    state::{HostState, StateSnapshot},
};
#[cfg(any(target_os = "windows", test))]
use serde::{Deserialize, Serialize};
use serde_json::json;
#[cfg(any(target_os = "windows", test))]
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
#[derive(Debug, Parser)]
#[command(
    name = "gnirehtet-vd",
    version,
    about = "Quest 3 Virtual Desktop wired link"
)]
struct Cli {
    /// Override local state/log directory (or set GNIREHTET_VD_HOME).
    #[arg(long, global = true, hide = true)]
    home: Option<PathBuf>,

    /// ADB executable; the same configured path is used for every operation.
    #[arg(long, global = true, hide = true)]
    adb: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Start the host and the Quest VPN transactionally.
    Start(StartArgs),
    /// Explicitly stop the Quest VPN, verify closure, then remove mappings.
    Stop,
    /// Show current lifecycle and host health.
    Status(StatusArgs),
    /// Recreate only the product-owned ADB reverse mappings.
    Repair,
    /// Diagnose ADB, mappings, and Virtual Desktop Streamer without changing VD.
    Doctor,
    /// Capture or export local-only diagnostics.
    Diagnostics(DiagnosticsArgs),
    /// Print the native host version.
    Version,
    /// Internal foreground runtime used by `start`.
    #[command(hide = true)]
    Daemon(DaemonArgs),
}

#[derive(Debug, Args)]
struct StartArgs {
    /// Route all Quest traffic; default routing is Virtual Desktop only.
    #[arg(long)]
    all_traffic: bool,
}

#[derive(Debug, Args)]
struct StatusArgs {
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct DiagnosticsArgs {
    #[command(subcommand)]
    command: DiagnosticsCommand,
}

#[derive(Debug, Subcommand)]
enum DiagnosticsCommand {
    /// Capture process and relay health counters locally.
    Capture {
        #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u64).range(1..=3600))]
        duration: u64,
    },
    /// Export a redacted JSONL support bundle. Nothing is uploaded.
    Export { path: PathBuf },
}

#[derive(Debug, Args)]
struct DaemonArgs {
    #[arg(long)]
    session: SessionId,
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let paths = AppPaths::discover(cli.home.clone()).context("locating host state directory")?;
    let configured_adb = cli
        .adb
        .clone()
        .or_else(|| std::env::var_os("GNIREHTET_VD_ADB").map(PathBuf::from));
    let adb_program = resolve_adb_program(configured_adb, &paths.root);
    let adb = AdbController::new(Arc::new(SystemAdb::new(adb_program.clone())));

    match cli.command {
        None => no_argument_entry(paths, adb_program, adb).await,
        Some(Command::Daemon(args)) => run_foreground(args.session, paths, adb).await,
        Some(command) => {
            #[cfg(target_os = "windows")]
            {
                let command = BrokerCommand::try_from(command)?;
                let response = send_broker_command(&paths, &adb_program, command).await?;
                emit_broker_response(response)
            }
            #[cfg(not(target_os = "windows"))]
            {
                let output = execute_public_command(&adb_program, &paths, &adb, command).await?;
                println!("{output}");
                Ok(())
            }
        }
    }
}

async fn execute_public_command(
    adb_program: &std::path::Path,
    paths: &AppPaths,
    adb: &AdbController,
    command: Command,
) -> Result<String> {
    match command {
        Command::Start(args) => start(adb_program, paths, adb, args).await,
        Command::Stop => stop(paths, adb).await,
        Command::Status(args) => status(paths, adb, args).await,
        Command::Repair => {
            let repaired_program = repair_adb_if_missing(adb_program.to_owned(), &paths.root)?;
            let repaired_adb = AdbController::new(Arc::new(SystemAdb::new(repaired_program)));
            repair(paths, &repaired_adb)
        }
        Command::Doctor => Ok(serde_json::to_string_pretty(&doctor(adb))?),
        Command::Diagnostics(args) => diagnostics(paths, adb, args).await,
        Command::Version => Ok(format!("gnirehtet-vd {} (GNR4)", env!("CARGO_PKG_VERSION"))),
        Command::Daemon(_) => bail!("internal daemon commands cannot enter the public broker"),
    }
}

#[cfg(any(target_os = "windows", test))]
const BROKER_PROTOCOL_VERSION: u16 = 1;
#[cfg(any(target_os = "windows", test))]
const MAX_BROKER_FRAME: usize = 256 * 1024;
#[cfg(any(target_os = "windows", test))]
const MAX_BROKER_TEXT: usize = 32 * 1024;
#[cfg(any(target_os = "windows", test))]
const BROKER_IO_TIMEOUT: Duration = Duration::from_secs(3);

#[cfg(any(target_os = "windows", test))]
#[derive(Default)]
struct BrokerGateState {
    active: bool,
    shutting_down: bool,
}

#[cfg(any(target_os = "windows", test))]
#[derive(Clone, Default)]
struct BrokerGate(Arc<StdMutex<BrokerGateState>>);

#[cfg(any(target_os = "windows", test))]
impl BrokerGate {
    fn begin_command(&self) -> Option<BrokerCommandGuard> {
        let mut state = self.0.lock().ok()?;
        if state.active || state.shutting_down {
            return None;
        }
        state.active = true;
        Some(BrokerCommandGuard(self.clone()))
    }

    fn begin_shutdown(&self) -> bool {
        let Ok(mut state) = self.0.lock() else {
            return false;
        };
        if state.active || state.shutting_down {
            return false;
        }
        state.shutting_down = true;
        true
    }

    fn cancel_shutdown(&self) {
        if let Ok(mut state) = self.0.lock() {
            state.shutting_down = false;
        }
    }

    fn finish_command(&self) {
        if let Ok(mut state) = self.0.lock() {
            state.active = false;
        }
    }

    #[cfg(test)]
    fn snapshot(&self) -> (bool, bool) {
        self.0
            .lock()
            .map(|state| (state.active, state.shutting_down))
            .unwrap()
    }
}

#[cfg(any(target_os = "windows", test))]
struct BrokerCommandGuard(BrokerGate);

#[cfg(any(target_os = "windows", test))]
impl Drop for BrokerCommandGuard {
    fn drop(&mut self) {
        self.0.finish_command();
    }
}

#[cfg(any(target_os = "windows", test))]
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "command", rename_all = "snake_case")]
enum BrokerCommand {
    Start { all_traffic: bool },
    Stop,
    Status { json: bool },
    Repair,
    Doctor,
    DiagnosticsCapture { duration: u64 },
    DiagnosticsExport { path: PathBuf },
    Version,
}

#[cfg(any(target_os = "windows", test))]
impl BrokerCommand {
    fn into_public_command(self) -> Result<Command> {
        Ok(match self {
            Self::Start { all_traffic } => Command::Start(StartArgs { all_traffic }),
            Self::Stop => Command::Stop,
            Self::Status { json } => Command::Status(StatusArgs { json }),
            Self::Repair => Command::Repair,
            Self::Doctor => Command::Doctor,
            Self::DiagnosticsCapture { duration } => {
                if !(1..=3600).contains(&duration) {
                    bail!("diagnostics capture duration must be between 1 and 3600 seconds");
                }
                Command::Diagnostics(DiagnosticsArgs {
                    command: DiagnosticsCommand::Capture { duration },
                })
            }
            Self::DiagnosticsExport { path } => Command::Diagnostics(DiagnosticsArgs {
                command: DiagnosticsCommand::Export { path },
            }),
            Self::Version => Command::Version,
        })
    }

    fn response_timeout(&self) -> Duration {
        match self {
            Self::Start { .. } => Duration::from_secs(180),
            Self::Stop => Duration::from_secs(30),
            Self::Status { .. } | Self::Doctor => Duration::from_secs(15),
            // The checksum-verified platform-tools bootstrap has its own
            // ten-minute subprocess deadline.
            Self::Repair => Duration::from_secs(660),
            Self::DiagnosticsCapture { duration } => {
                Duration::from_secs((*duration).min(3600) + 30)
            }
            Self::DiagnosticsExport { .. } => Duration::from_secs(60),
            Self::Version => Duration::from_secs(5),
        }
    }
}

#[cfg(any(target_os = "windows", test))]
impl TryFrom<Command> for BrokerCommand {
    type Error = anyhow::Error;

    fn try_from(command: Command) -> Result<Self> {
        Ok(match command {
            Command::Start(args) => Self::Start {
                all_traffic: args.all_traffic,
            },
            Command::Stop => Self::Stop,
            Command::Status(args) => Self::Status { json: args.json },
            Command::Repair => Self::Repair,
            Command::Doctor => Self::Doctor,
            Command::Diagnostics(args) => match args.command {
                DiagnosticsCommand::Capture { duration } => Self::DiagnosticsCapture { duration },
                DiagnosticsCommand::Export { path } => Self::DiagnosticsExport { path },
            },
            Command::Version => Self::Version,
            Command::Daemon(_) => bail!("the internal daemon cannot be forwarded to the broker"),
        })
    }
}

#[cfg(any(target_os = "windows", test))]
#[derive(Debug, Deserialize, Serialize)]
struct BrokerRequest {
    protocol_version: u16,
    instance_root: PathBuf,
    adb_program: PathBuf,
    command: BrokerCommand,
}

#[cfg(any(target_os = "windows", test))]
#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
struct BrokerResponse {
    protocol_version: u16,
    exit_code: u8,
    stdout: String,
    stderr: String,
}

#[cfg(any(target_os = "windows", test))]
impl BrokerResponse {
    fn success(stdout: String) -> Self {
        Self {
            protocol_version: BROKER_PROTOCOL_VERSION,
            exit_code: 0,
            stdout: bounded_broker_text(stdout),
            stderr: String::new(),
        }
    }

    fn failure(error: impl std::fmt::Display) -> Self {
        Self {
            protocol_version: BROKER_PROTOCOL_VERSION,
            exit_code: 1,
            stdout: String::new(),
            stderr: bounded_broker_text(format!("Error: {error}")),
        }
    }
}

#[cfg(any(target_os = "windows", test))]
fn bounded_broker_text(mut text: String) -> String {
    const SUFFIX: &str = "\n[broker output truncated]\n";
    if text.len() > MAX_BROKER_TEXT {
        let mut boundary = MAX_BROKER_TEXT.saturating_sub(SUFFIX.len());
        while !text.is_char_boundary(boundary) {
            boundary = boundary.saturating_sub(1);
        }
        text.truncate(boundary);
        text.push_str(SUFFIX);
        return text;
    }
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

#[cfg(any(target_os = "windows", test))]
async fn read_broker_frame<T, S>(stream: &mut S) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
    S: AsyncRead + Unpin,
{
    let length = stream.read_u32().await? as usize;
    if length > MAX_BROKER_FRAME {
        bail!("broker frame length {length} exceeds the {MAX_BROKER_FRAME}-byte limit");
    }
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes).await?;
    serde_json::from_slice(&bytes).context("decoding broker message")
}

#[cfg(any(target_os = "windows", test))]
async fn write_broker_frame<T, S>(stream: &mut S, value: &T) -> Result<()>
where
    T: Serialize,
    S: AsyncWrite + Unpin,
{
    let bytes = serde_json::to_vec(value).context("encoding broker message")?;
    if bytes.len() > MAX_BROKER_FRAME {
        bail!(
            "broker frame length {} exceeds the {MAX_BROKER_FRAME}-byte limit",
            bytes.len()
        );
    }
    stream.write_u32(bytes.len() as u32).await?;
    stream.write_all(&bytes).await?;
    stream.flush().await?;
    Ok(())
}

#[cfg(any(target_os = "windows", test))]
#[derive(Clone)]
struct BrokerContext {
    paths: AppPaths,
    environment: Arc<StdMutex<BrokerEnvironment>>,
    gate: BrokerGate,
}

#[cfg(any(target_os = "windows", test))]
#[derive(Clone)]
struct BrokerEnvironment {
    adb_program: PathBuf,
    adb: AdbController,
}

#[cfg(any(target_os = "windows", test))]
impl BrokerContext {
    fn new(paths: AppPaths, adb_program: PathBuf, adb: AdbController, gate: BrokerGate) -> Self {
        Self {
            paths,
            environment: Arc::new(StdMutex::new(BrokerEnvironment { adb_program, adb })),
            gate,
        }
    }

    fn environment(&self) -> Result<BrokerEnvironment> {
        self.environment
            .lock()
            .map(|environment| environment.clone())
            .map_err(|_| anyhow::Error::msg("broker ADB context lock is poisoned"))
    }

    fn replace_adb(&self, adb_program: PathBuf) -> Result<AdbController> {
        let adb = AdbController::new(Arc::new(SystemAdb::new(adb_program.clone())));
        let mut environment = self
            .environment
            .lock()
            .map_err(|_| anyhow::Error::msg("broker ADB context lock is poisoned"))?;
        environment.adb_program = adb_program;
        environment.adb = adb.clone();
        Ok(adb)
    }

    async fn execute(&self, command: Command) -> Result<String> {
        let environment = self.environment()?;
        match command {
            Command::Repair => {
                let repaired_program =
                    repair_adb_if_missing(environment.adb_program, &self.paths.root)?;
                // Publish the verified executable before mapping repair. Even
                // if the device-side repair then fails, later broker requests
                // must use the ADB installation that was verified/downloaded.
                let repaired_adb = self.replace_adb(repaired_program)?;
                repair(&self.paths, &repaired_adb)
            }
            command => {
                execute_public_command(
                    &environment.adb_program,
                    &self.paths,
                    &environment.adb,
                    command,
                )
                .await
            }
        }
    }
}

#[cfg(any(target_os = "windows", test))]
async fn handle_broker_connection<S>(mut stream: S, context: &BrokerContext) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let _command_guard = context
        .gate
        .begin_command()
        .context("per-user broker is busy or shutting down")?;
    let request: BrokerRequest =
        tokio::time::timeout(BROKER_IO_TIMEOUT, read_broker_frame(&mut stream))
            .await
            .context("broker request timed out")??;
    let response = if request.protocol_version != BROKER_PROTOCOL_VERSION {
        BrokerResponse::failure(format!(
            "broker protocol mismatch: client {}, broker {}",
            request.protocol_version, BROKER_PROTOCOL_VERSION
        ))
    } else if request.instance_root != context.paths.root {
        BrokerResponse::failure(format!(
            "this per-user broker owns {}; exit its tray before using --home {}",
            context.paths.root.display(),
            request.instance_root.display()
        ))
    } else if request.adb_program != context.environment()?.adb_program {
        BrokerResponse::failure(
            "this per-user broker owns a different ADB executable; exit its tray before changing --adb",
        )
    } else {
        match request.command.into_public_command() {
            Ok(command) => match context.execute(command).await {
                Ok(output) => BrokerResponse::success(output),
                Err(error) => BrokerResponse::failure(format!("{error:#}")),
            },
            Err(error) => BrokerResponse::failure(error),
        }
    };
    tokio::time::timeout(
        BROKER_IO_TIMEOUT,
        write_broker_frame(&mut stream, &response),
    )
    .await
    .context("broker response timed out")??;
    Ok(())
}

#[cfg(target_os = "windows")]
fn prepare_broker_pipe() -> Result<tokio::net::windows::named_pipe::NamedPipeServer> {
    let name = gnirehtet_vd::runtime::windows_broker_pipe_name()?;
    gnirehtet_vd::runtime::create_secure_named_pipe(&name, true, 1)
        .context("creating the per-user broker pipe")
}

#[cfg(target_os = "windows")]
async fn serve_broker(
    mut server: tokio::net::windows::named_pipe::NamedPipeServer,
    context: BrokerContext,
) -> Result<()> {
    let name = gnirehtet_vd::runtime::windows_broker_pipe_name()?;
    loop {
        server.connect().await?;
        // Deliberately await the complete command before creating the next pipe
        // instance. This is the broker's one-command-at-a-time ownership point.
        let _ = handle_broker_connection(server, &context).await;
        server = gnirehtet_vd::runtime::create_secure_named_pipe(&name, false, 1)
            .context("recreating the per-user broker pipe")?;
    }
}

#[cfg(target_os = "windows")]
fn broker_is_present() -> Result<bool> {
    use tokio::net::windows::named_pipe::ClientOptions;
    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY};

    let name = gnirehtet_vd::runtime::windows_broker_pipe_name()?;
    match ClientOptions::new().open(&name) {
        Ok(client) => {
            drop(client);
            Ok(true)
        }
        Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => Ok(true),
        Err(error) if error.raw_os_error() == Some(ERROR_FILE_NOT_FOUND as i32) => Ok(false),
        Err(error) => Err(error).context("probing the per-user broker pipe"),
    }
}

#[cfg(target_os = "windows")]
fn spawn_broker_process(paths: &AppPaths, adb_program: &std::path::Path) -> Result<()> {
    use std::os::windows::process::CommandExt;

    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let executable = std::env::current_exe().context("locating current executable")?;
    ProcessCommand::new(executable)
        .arg("--home")
        .arg(&paths.root)
        .arg("--adb")
        .arg(adb_program)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS | CREATE_NO_WINDOW)
        .spawn()
        .context("starting the per-user command broker")?;
    Ok(())
}

#[cfg(target_os = "windows")]
async fn open_broker_client(
    wait: Duration,
) -> Result<tokio::net::windows::named_pipe::NamedPipeClient> {
    use tokio::net::windows::named_pipe::ClientOptions;
    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY};

    let name = gnirehtet_vd::runtime::windows_broker_pipe_name()?;
    let operation = async {
        loop {
            match ClientOptions::new().open(&name) {
                Ok(client) => return Ok(client),
                Err(error)
                    if matches!(
                        error.raw_os_error(),
                        Some(code)
                            if code == ERROR_FILE_NOT_FOUND as i32
                                || code == ERROR_PIPE_BUSY as i32
                    ) =>
                {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                Err(error) => return Err(error),
            }
        }
    };
    tokio::time::timeout(wait, operation)
        .await
        .context("timed out waiting for the per-user command broker")?
        .context("opening the per-user broker pipe")
}

#[cfg(target_os = "windows")]
async fn send_broker_command(
    paths: &AppPaths,
    adb_program: &std::path::Path,
    command: BrokerCommand,
) -> Result<BrokerResponse> {
    if !broker_is_present()? {
        spawn_broker_process(paths, adb_program)?;
    }
    let response_timeout = command.response_timeout();
    let request = BrokerRequest {
        protocol_version: BROKER_PROTOCOL_VERSION,
        instance_root: paths.root.clone(),
        adb_program: adb_program.to_owned(),
        command,
    };
    let operation = async {
        let mut stream = open_broker_client(Duration::from_secs(5)).await?;
        write_broker_frame(&mut stream, &request).await?;
        let response: BrokerResponse = read_broker_frame(&mut stream).await?;
        if response.protocol_version != BROKER_PROTOCOL_VERSION {
            bail!(
                "broker response protocol mismatch: expected {}, got {}",
                BROKER_PROTOCOL_VERSION,
                response.protocol_version
            );
        }
        Ok(response)
    };
    tokio::time::timeout(response_timeout + Duration::from_secs(5), operation)
        .await
        .context("the per-user broker command exceeded its bounded deadline")?
}

#[cfg(target_os = "windows")]
fn emit_broker_response(response: BrokerResponse) -> Result<()> {
    std::io::stdout().write_all(response.stdout.as_bytes())?;
    std::io::stdout().flush()?;
    std::io::stderr().write_all(response.stderr.as_bytes())?;
    std::io::stderr().flush()?;
    if response.exit_code != 0 {
        std::process::exit(response.exit_code.into());
    }
    Ok(())
}

async fn run_foreground(session_id: SessionId, paths: AppPaths, adb: AdbController) -> Result<()> {
    HostRuntime::new(RuntimeConfig::new(session_id, paths, adb))?
        .run()
        .await?;
    Ok(())
}

async fn start(
    daemon_adb: &std::path::Path,
    paths: &AppPaths,
    adb: &AdbController,
    args: StartArgs,
) -> Result<String> {
    let _operation = OperationGuard::acquire(&paths.operation_lock)
        .context("another start/stop/repair operation is active")?;
    if let Some(pid) = read_daemon_pid(&paths.daemon_pid).filter(|pid| process_is_running(*pid)) {
        bail!("host daemon is already running as PID {pid}");
    }
    if runtime_may_be_active(paths) {
        bail!("host runtime activity exists without a verified daemon identity; refusing APK install/start");
    }
    let apk_path = paths.root.join("gnirehtet-v4.apk");
    embedded::materialize(&apk_path)?;
    adb.install_matching_apk(&apk_path)?;
    let session = SessionId::random();
    let mut daemon = spawn_daemon(daemon_adb, paths, session)?;
    let daemon_pid = daemon.id();
    if let Err(error) = wait_for_listeners(paths, daemon_pid).await {
        let _ = terminate_spawned_daemon(&mut daemon);
        return Err(error);
    }

    let start_result = adb.start(session, args.all_traffic);
    if let Err(error) = start_result {
        let _ = terminate_spawned_daemon(&mut daemon);
        let snapshot = StateSnapshot {
            state: HostState::Error,
            session_id: Some(session.to_string()),
            missed_heartbeats: 0,
            reason: Some("Android start transaction failed".into()),
        };
        let _ = StateStore::new(&paths.status).write(&snapshot, None);
        return Err(error.into());
    }
    if let Err(error) = wait_for_connected(paths, session, &mut daemon) {
        terminate_spawned_daemon(&mut daemon)
            .context("quiescing host monitor before startup rollback")?;
        let rollback_error = adb.stop().err();
        let reason = if rollback_error.is_some() {
            "Android did not acknowledge GNR4 STARTED; rollback could not verify VPN closure"
        } else {
            "Android did not acknowledge GNR4 STARTED; rollback completed"
        };
        let snapshot = StateSnapshot {
            state: HostState::Error,
            session_id: Some(session.to_string()),
            missed_heartbeats: 0,
            reason: Some(reason.into()),
        };
        let _ = StateStore::new(&paths.status).write(&snapshot, None);
        if let Some(rollback_error) = rollback_error {
            return Err(error.context(format!(
                "startup rollback failed and VPN state remains unverified: {rollback_error}"
            )));
        }
        return Err(error);
    }
    Ok(format!(
        "wired link connected (session {session}, package {VIRTUAL_DESKTOP_PACKAGE}, daemon PID {daemon_pid})"
    ))
}

async fn stop(paths: &AppPaths, adb: &AdbController) -> Result<String> {
    let _operation = OperationGuard::acquire(&paths.operation_lock)
        .context("another start/stop/repair operation is active")?;
    let control_stop = admin_command(paths, "stop", Duration::from_secs(8)).await;
    let stop_result = match control_stop {
        Ok(response) if admin_stop_acknowledges_suppression(&response) => {
            match adb.finish_control_stop() {
                Ok(()) => Ok(()),
                Err(_) => adb.stop(),
            }
        }
        // Only this typed acknowledgement proves the stop handler suppressed
        // and drained repairs. Authentication/unknown-command errors do not.
        Ok(response) if response.repairs_suppressed => adb.stop(),
        command_failure => {
            let command_error = match command_failure {
                Ok(response) => response
                    .error
                    .unwrap_or_else(|| "repairs were not suppressed".into()),
                Err(error) => error.to_string(),
            };
            // No authenticated response means suppression is unproven. If an
            // exact daemon identity exists, terminating that verified handle
            // closes its mandatory kill-on-close Job Object and quiesces every
            // inherited ADB child before teardown continues. Never infer
            // safety from a missing/corrupt PID file while listeners/lock live.
            if read_daemon_pid(&paths.daemon_pid).is_some() {
                terminate_daemon(&paths.daemon_pid, Duration::from_secs(3))
                    .context("quiescing verified daemon after command-lane failure")?;
                let _ = std::fs::remove_file(&paths.daemon_pid);
                // The daemon Job Object kills its ADB client descendants, but
                // the persistent ADB server may already have accepted the last
                // reverse request. Let that bounded command window drain, then
                // require a fresh Stop transaction to remove and verify.
                thread::sleep(ADB_MAPPING_COMMAND_TIMEOUT);
                bail!(
                    "host command lane failed ({command_error}); the verified daemon and ADB clients were quiesced and the mapping-command drain elapsed without changing the VPN; run `gnirehtet-vd stop` again"
                );
            } else if runtime_may_be_active(paths) {
                bail!(
                    "host command lane failed ({command_error}) and daemon identity is unavailable while runtime activity is still visible; refusing ADB teardown to avoid racing mapping repair"
                );
            }
            adb.stop()
        }
    };
    if read_daemon_pid(&paths.daemon_pid).is_some() {
        if stop_result.is_ok() {
            let shutdown = admin_command(paths, "shutdown", Duration::from_secs(3)).await;
            if shutdown.is_ok_and(|response| response.ok) {
                if wait_for_daemon_exit(paths, Duration::from_secs(3))
                    .await
                    .is_err()
                {
                    terminate_daemon(&paths.daemon_pid, Duration::from_secs(3))
                        .context("terminating verified daemon after graceful timeout")?;
                }
            } else {
                terminate_daemon(&paths.daemon_pid, Duration::from_secs(3))
                    .context("terminating verified stopped daemon")?;
            }
        } else {
            terminate_daemon(&paths.daemon_pid, Duration::from_secs(3))
                .context("terminating verified daemon after failed stop")?;
        }
    }
    let _ = std::fs::remove_file(&paths.daemon_pid);
    let store = StateStore::new(&paths.status);
    match stop_result {
        Ok(()) => {
            store.write(
                &StateSnapshot {
                    state: HostState::Stopped,
                    session_id: None,
                    missed_heartbeats: 0,
                    reason: None,
                },
                None,
            )?;
            Ok("wired link stopped; Quest VPN closure verified and mappings removed".into())
        }
        Err(error) => {
            store.write(
                &StateSnapshot {
                    state: HostState::Error,
                    session_id: None,
                    missed_heartbeats: 0,
                    reason: Some("explicit stop could not be verified".into()),
                },
                None,
            )?;
            Err(error.into())
        }
    }
}

fn admin_stop_acknowledges_suppression(response: &AdminResponse) -> bool {
    response.ok && response.repairs_suppressed
}

async fn status(paths: &AppPaths, adb: &AdbController, args: StatusArgs) -> Result<String> {
    let mut status = StateStore::new(&paths.status).read_or_stopped();
    status.daemon_running = read_daemon_pid(&paths.daemon_pid).is_some();
    if status.daemon_running {
        if let Ok(response) = admin_command(paths, "status", Duration::from_secs(1)).await {
            if response.ok {
                if let Some(live) = response.status {
                    status.lifecycle = live;
                }
            }
        }
    } else {
        reflect_missing_runtime(&mut status.lifecycle);
    }
    let android = adb.android_status();
    if args.json {
        Ok(serde_json::to_string_pretty(&json!({
            "host": status,
            "android": android.as_ref().ok(),
            "android_error": android.as_ref().err().map(ToString::to_string),
        }))?)
    } else {
        let mut lines = vec![
            format!("state: {:?}", status.lifecycle.state),
            format!(
                "daemon: {}",
                if status.daemon_running {
                    "running"
                } else {
                    "not running"
                }
            ),
        ];
        if let Some(reason) = status.lifecycle.reason {
            lines.push(format!("reason: {reason}"));
        }
        if let Some(telemetry) = status.telemetry {
            lines.push(format!(
                "queue residence: p99={}us max={}us; queued={} bytes",
                telemetry.relay.udp.queue_residence_us.p99_us,
                telemetry.relay.udp.queue_residence_us.max_us,
                telemetry.relay.udp.queued_bytes
            ));
            lines.push(format!(
                "control: generation={} reconnects={} echo-p99={}us",
                telemetry.control.connection_generation,
                telemetry.control.reconnects,
                telemetry.control.heartbeat_echo_service_us.p99_us
            ));
            lines.push(format!(
                "adb: device={} mappings={} reconnect-generation={}{}",
                telemetry.adb.device_available,
                telemetry.adb.mappings_healthy,
                telemetry.adb.reconnect_generation,
                telemetry
                    .adb
                    .last_error
                    .as_deref()
                    .map(|category| format!(" category={category}"))
                    .unwrap_or_default()
            ));
        }
        match android {
            Ok(android) => lines.push(format!(
                "android: {} fd_open={:?} rtt_p99={:?}us rtt_max={:?}us",
                android.state.as_deref().unwrap_or("not running"),
                android.vpn_fd_open,
                android.control_rtt_p99_us,
                android.control_rtt_max_us
            )),
            Err(error) => lines.push(format!("android status unavailable: {error}")),
        }
        Ok(lines.join("\n"))
    }
}

fn reflect_missing_runtime(snapshot: &mut StateSnapshot) {
    match snapshot.state {
        HostState::Preparing | HostState::Connected | HostState::Degraded => {
            snapshot.state = HostState::Degraded;
            snapshot.reason =
                Some("host runtime is not running; Quest VPN state is retained".into());
        }
        HostState::Stopping => {
            snapshot.state = HostState::Error;
            snapshot.reason =
                Some("host runtime exited during Stop; VPN state is unverified".into());
        }
        HostState::Stopped | HostState::Error => {}
    }
}

fn repair(paths: &AppPaths, adb: &AdbController) -> Result<String> {
    let _operation = OperationGuard::acquire(&paths.operation_lock)
        .context("another start/stop/repair operation is active")?;
    if read_daemon_pid(&paths.daemon_pid).is_some() || runtime_may_be_active(paths) {
        bail!("host daemon is active; its serialized ADB monitor owns mapping repair");
    }
    adb.repair_mappings()?;
    Ok("product ADB reverse mappings repaired".into())
}

async fn diagnostics(
    paths: &AppPaths,
    adb: &AdbController,
    args: DiagnosticsArgs,
) -> Result<String> {
    let diagnostics = Diagnostics::open(&paths.logs)?;
    match args.command {
        DiagnosticsCommand::Capture { duration } => {
            let deadline = Instant::now() + Duration::from_secs(duration);
            while Instant::now() < deadline {
                diagnostics.capture_process_sample()?;
                diagnostics.record("doctor_snapshot", serde_json::to_value(doctor(adb))?)?;
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            Ok(format!("captured {duration}s of local diagnostics"))
        }
        DiagnosticsCommand::Export { path } => {
            let exported = diagnostics.export(path)?;
            Ok(format!(
                "redacted support bundle written to {}",
                exported.display()
            ))
        }
    }
}

fn spawn_daemon(adb: &std::path::Path, paths: &AppPaths, session: SessionId) -> Result<Child> {
    let executable = std::env::current_exe().context("locating current executable")?;
    let mut command = ProcessCommand::new(executable);
    command
        .arg("--home")
        .arg(&paths.root)
        .arg("daemon")
        .arg("--session")
        .arg(session.to_string())
        .arg("--adb")
        .arg(adb)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        command.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS);
    }
    command.spawn().context("starting host daemon")
}

fn terminate_spawned_daemon(child: &mut Child) -> std::io::Result<()> {
    if child.try_wait()?.is_none() {
        child.kill()?;
    }
    let _ = child.wait()?;
    Ok(())
}

async fn wait_for_listeners(paths: &AppPaths, pid: u32) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(3);
    let addresses = [
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), CONTROL_PORT),
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), SOCKS_PORT),
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), UDP_STREAM_PORT),
    ];
    while Instant::now() < deadline {
        if !process_is_running(pid) {
            bail!("host daemon exited before binding listeners");
        }
        let data_ready = addresses.iter().all(|address| {
            StdTcpStream::connect_timeout(address, Duration::from_millis(100)).is_ok()
        });
        let admin_ready = admin_command(paths, "status", Duration::from_millis(150))
            .await
            .is_ok_and(|response| response.ok);
        if data_ready && admin_ready {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    bail!("host daemon did not bind its loopback listeners within 3 seconds")
}

fn runtime_may_be_active(paths: &AppPaths) -> bool {
    let lock_owner_running = std::fs::read_to_string(&paths.runtime_lock)
        .ok()
        .and_then(|value| value.trim().parse::<u32>().ok())
        .is_some_and(process_is_running);
    lock_owner_running
        || [CONTROL_PORT, SOCKS_PORT, UDP_STREAM_PORT]
            .iter()
            .any(|port| {
                StdTcpStream::connect_timeout(
                    &SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), *port),
                    Duration::from_millis(50),
                )
                .is_ok()
            })
}

async fn wait_for_daemon_exit(paths: &AppPaths, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if read_daemon_pid(&paths.daemon_pid).is_none() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    bail!("daemon did not exit before {timeout:?}")
}

async fn no_argument_entry(
    paths: AppPaths,
    adb_program: PathBuf,
    adb: AdbController,
) -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        // Keep the console subsystem for CLI use, but detach the no-argument
        // Explorer/tray path from any inherited console without hiding a
        // caller's shared terminal window.
        unsafe {
            windows_sys::Win32::System::Console::FreeConsole();
        }
        let Some(instance) = acquire_tray_instance()? else {
            return Ok(());
        };
        let class_name = instance.class_name.clone();
        // Prepare the first, ACL-restricted pipe before exposing the tray icon.
        // A visible primary instance therefore always has a listening broker.
        let server = prepare_broker_pipe()?;
        let gate = BrokerGate::default();
        let context = BrokerContext::new(paths, adb_program, adb, gate.clone());
        let mut broker = tokio::spawn(serve_broker(server, context));
        let mut tray = tokio::task::spawn_blocking(move || run_windows_tray(instance, gate));
        tokio::select! {
            tray_result = &mut tray => {
                broker.abort();
                let _ = broker.await;
                tray_result
                    .context("notification-area task failed")?
                    .context("running Windows notification-area shell")
            }
            broker_result = &mut broker => {
                let error = match broker_result {
                    Ok(Ok(())) => anyhow::Error::msg("per-user command broker exited unexpectedly"),
                    Ok(Err(error)) => error,
                    Err(error) => anyhow::Error::new(error).context("per-user broker task failed"),
                };
                request_tray_exit(&class_name);
                let _ = tray.await;
                Err(error)
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (adb_program, adb);
        if let Ok(response) = admin_command(&paths, "status", Duration::from_secs(1)).await {
            println!("{}", serde_json::to_string_pretty(&response)?);
            return Ok(());
        }
        bail!("no-argument notification-area mode is Windows-only; use `gnirehtet-vd start`")
    }
}

#[cfg(any(target_os = "windows", test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TrayCommand {
    Status,
    Start,
    Stop,
    Repair,
    Exit,
}

#[cfg(any(target_os = "windows", test))]
const TRAY_STATUS_ID: usize = 1_001;
#[cfg(any(target_os = "windows", test))]
const TRAY_START_ID: usize = 1_002;
#[cfg(any(target_os = "windows", test))]
const TRAY_STOP_ID: usize = 1_003;
#[cfg(any(target_os = "windows", test))]
const TRAY_EXIT_ID: usize = 1_004;
#[cfg(any(target_os = "windows", test))]
const TRAY_REPAIR_ID: usize = 1_005;

#[cfg(any(target_os = "windows", test))]
fn tray_command_from_id(id: usize) -> Option<TrayCommand> {
    match id {
        TRAY_STATUS_ID => Some(TrayCommand::Status),
        TRAY_START_ID => Some(TrayCommand::Start),
        TRAY_STOP_ID => Some(TrayCommand::Stop),
        TRAY_REPAIR_ID => Some(TrayCommand::Repair),
        TRAY_EXIT_ID => Some(TrayCommand::Exit),
        _ => None,
    }
}

#[cfg(any(target_os = "windows", test))]
fn tray_command_arguments(command: TrayCommand) -> Option<&'static [&'static str]> {
    match command {
        TrayCommand::Status => Some(&["status"]),
        TrayCommand::Start => Some(&["start"]),
        TrayCommand::Stop => Some(&["stop"]),
        TrayCommand::Repair => Some(&["repair"]),
        TrayCommand::Exit => None,
    }
}

#[cfg(any(target_os = "windows", test))]
fn begin_tray_command(in_flight: &AtomicBool) -> bool {
    in_flight
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
}

#[cfg(any(target_os = "windows", test))]
fn begin_tray_shutdown(in_flight: &AtomicBool, gate: &BrokerGate) -> bool {
    !in_flight.load(Ordering::Acquire) && gate.begin_shutdown()
}

#[cfg(target_os = "windows")]
static TRAY_COMMAND_IN_FLIGHT: AtomicBool = AtomicBool::new(false);
#[cfg(target_os = "windows")]
static WINDOWS_BROKER_GATE: std::sync::OnceLock<BrokerGate> = std::sync::OnceLock::new();

#[cfg(target_os = "windows")]
const TRAY_CALLBACK_MESSAGE: u32 = 0x8000 + 41;
#[cfg(target_os = "windows")]
const TRAY_SHOW_EXISTING_MESSAGE: u32 = 0x8000 + 42;

#[cfg(target_os = "windows")]
struct TrayInstance {
    mutex: usize,
    class_name: Vec<u16>,
}

#[cfg(target_os = "windows")]
impl Drop for TrayInstance {
    fn drop(&mut self) {
        // SAFETY: `mutex` owns one successful CreateMutexW handle.
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(
                self.mutex as windows_sys::Win32::Foundation::HANDLE,
            );
        }
    }
}

#[cfg(target_os = "windows")]
fn wide_windows(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(target_os = "windows")]
fn acquire_tray_instance() -> Result<Option<TrayInstance>> {
    use std::ptr::null;
    use windows_sys::Win32::{
        Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS},
        System::Threading::CreateMutexW,
        UI::WindowsAndMessaging::{FindWindowW, PostMessageW},
    };

    let sid = gnirehtet_vd::runtime::windows_current_user_sid()?;
    let class_name = wide_windows(&format!("GnirehtetVdTray_{sid}"));
    let mutex_name = wide_windows(&format!(r"Local\GnirehtetVdTray_{sid}"));
    let mutex = unsafe { CreateMutexW(null(), 0, mutex_name.as_ptr()) };
    if mutex.is_null() {
        return Err(std::io::Error::last_os_error()).context("creating tray singleton mutex");
    }
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe {
            CloseHandle(mutex);
            let existing = FindWindowW(class_name.as_ptr(), null());
            if !existing.is_null() {
                PostMessageW(existing, TRAY_SHOW_EXISTING_MESSAGE, 0, 0);
            }
        }
        return Ok(None);
    }
    Ok(Some(TrayInstance {
        mutex: mutex as usize,
        class_name,
    }))
}

#[cfg(target_os = "windows")]
fn request_tray_exit(class_name: &[u16]) {
    use std::ptr::null;
    use windows_sys::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW, WM_CLOSE};

    for _ in 0..40 {
        let window = unsafe { FindWindowW(class_name.as_ptr(), null()) };
        if !window.is_null() {
            unsafe {
                PostMessageW(window, WM_CLOSE, 0, 0);
            }
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(target_os = "windows")]
fn run_windows_tray(instance_guard: TrayInstance, broker_gate: BrokerGate) -> std::io::Result<()> {
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Shell::{
                Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
                NOTIFYICONDATAW,
            },
            WindowsAndMessaging::{
                AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
                DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW, LoadIconW,
                PostMessageW, PostQuitMessage, RegisterClassW, SetForegroundWindow, TrackPopupMenu,
                TranslateMessage, IDI_APPLICATION, MF_SEPARATOR, MF_STRING, MSG, TPM_RETURNCMD,
                TPM_RIGHTBUTTON, WM_CLOSE, WM_COMMAND, WM_DESTROY, WM_LBUTTONDBLCLK, WM_NULL,
                WM_RBUTTONUP, WNDCLASSW,
            },
        },
    };

    const ICON_ID: u32 = 1;

    fn show_menu(hwnd: HWND) {
        let menu = unsafe { CreatePopupMenu() };
        if menu.is_null() {
            return;
        }
        for (flags, id, label) in [
            (MF_STRING, TRAY_STATUS_ID, Some("Status")),
            (MF_STRING, TRAY_START_ID, Some("Start wired link")),
            (MF_STRING, TRAY_STOP_ID, Some("Stop wired link")),
            (MF_STRING, TRAY_REPAIR_ID, Some("Repair")),
            (MF_SEPARATOR, 0, None),
            (MF_STRING, TRAY_EXIT_ID, Some("Exit tray")),
        ] {
            let label = label.map(wide_windows);
            unsafe {
                AppendMenuW(
                    menu,
                    flags,
                    id,
                    label.as_ref().map_or(null(), |value| value.as_ptr()),
                );
            }
        }
        let mut point = POINT::default();
        unsafe {
            GetCursorPos(&mut point);
            SetForegroundWindow(hwnd);
        }
        let selected = unsafe {
            TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_RIGHTBUTTON,
                point.x,
                point.y,
                0,
                hwnd,
                null(),
            )
        } as usize;
        unsafe {
            DestroyMenu(menu);
            PostMessageW(hwnd, WM_NULL, 0, 0);
        }
        if let Some(command) = tray_command_from_id(selected) {
            dispatch_tray_command(command, hwnd);
        }
    }

    fn dispatch_tray_command(command: TrayCommand, hwnd: HWND) {
        if command == TrayCommand::Exit {
            let Some(gate) = WINDOWS_BROKER_GATE.get() else {
                return;
            };
            if !begin_tray_shutdown(&TRAY_COMMAND_IN_FLIGHT, gate) {
                return;
            }
            if unsafe { DestroyWindow(hwnd) } == 0 {
                gate.cancel_shutdown();
            }
            return;
        }
        let Some(arguments) = tray_command_arguments(command) else {
            return;
        };
        if !begin_tray_command(&TRAY_COMMAND_IN_FLIGHT) {
            return;
        }
        let worker = std::thread::Builder::new()
            .name("gnirehtet-vd-tray-command".into())
            .spawn(move || {
                use std::os::windows::process::CommandExt;
                use windows_sys::Win32::UI::WindowsAndMessaging::{
                    MessageBoxW, MB_ICONINFORMATION, MB_OK,
                };

                const CREATE_NO_WINDOW: u32 = 0x0800_0000;
                let outcome = std::env::current_exe()
                    .and_then(|executable| {
                        ProcessCommand::new(executable)
                            .args(arguments)
                            .creation_flags(CREATE_NO_WINDOW)
                            .stdout(Stdio::piped())
                            .stderr(Stdio::piped())
                            // `output()` drains both pipes concurrently. Mutation
                            // deadlines and rollback belong to the CLI transaction;
                            // the tray must not kill it at an arbitrary UI timeout.
                            .output()
                    })
                    .map(|output| {
                        let bytes = if output.status.success() || output.stderr.is_empty() {
                            output.stdout
                        } else {
                            output.stderr
                        };
                        let mut text = String::from_utf8_lossy(&bytes).trim().to_owned();
                        if text.is_empty() {
                            text = if output.status.success() {
                                "Command completed".into()
                            } else {
                                "Command failed without details".into()
                            };
                        }
                        text.chars().take(4 * 1024).collect()
                    })
                    .unwrap_or_else(|error| format!("Could not run command: {error}"));
                let message = wide_windows(&outcome);
                let title = wide_windows("Gnirehtet VD wired link");
                unsafe {
                    MessageBoxW(
                        null_mut(),
                        message.as_ptr(),
                        title.as_ptr(),
                        MB_OK | MB_ICONINFORMATION,
                    );
                }
                TRAY_COMMAND_IN_FLIGHT.store(false, Ordering::Release);
            });
        if worker.is_err() {
            TRAY_COMMAND_IN_FLIGHT.store(false, Ordering::Release);
        }
    }

    unsafe extern "system" fn window_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match message {
            TRAY_CALLBACK_MESSAGE => {
                if matches!(lparam as u32, WM_RBUTTONUP | WM_LBUTTONDBLCLK) {
                    show_menu(hwnd);
                }
                0
            }
            TRAY_SHOW_EXISTING_MESSAGE => {
                show_menu(hwnd);
                0
            }
            WM_COMMAND => {
                if let Some(command) = tray_command_from_id(wparam & 0xffff) {
                    dispatch_tray_command(command, hwnd);
                }
                0
            }
            WM_CLOSE => {
                if let Some(gate) = WINDOWS_BROKER_GATE.get() {
                    if begin_tray_shutdown(&TRAY_COMMAND_IN_FLIGHT, gate)
                        && DestroyWindow(hwnd) == 0
                    {
                        gate.cancel_shutdown();
                    }
                }
                0
            }
            WM_DESTROY => {
                let icon = NOTIFYICONDATAW {
                    cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                    hWnd: hwnd,
                    uID: ICON_ID,
                    ..Default::default()
                };
                Shell_NotifyIconW(NIM_DELETE, &icon);
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(hwnd, message, wparam, lparam),
        }
    }

    WINDOWS_BROKER_GATE
        .set(broker_gate)
        .map_err(|_| std::io::Error::other("broker gate was initialized more than once"))?;
    let class_name = instance_guard.class_name.clone();

    let instance = unsafe { GetModuleHandleW(null()) };
    if instance.is_null() {
        return Err(std::io::Error::last_os_error());
    }
    let window_class = WNDCLASSW {
        lpfnWndProc: Some(window_proc),
        hInstance: instance,
        hIcon: unsafe { LoadIconW(null_mut(), IDI_APPLICATION) },
        lpszClassName: class_name.as_ptr(),
        ..Default::default()
    };
    if unsafe { RegisterClassW(&window_class) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class_name.as_ptr(),
            class_name.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            instance,
            null(),
        )
    };
    if hwnd.is_null() {
        return Err(std::io::Error::last_os_error());
    }
    let mut icon = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: ICON_ID,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
        uCallbackMessage: TRAY_CALLBACK_MESSAGE,
        hIcon: unsafe { LoadIconW(null_mut(), IDI_APPLICATION) },
        ..Default::default()
    };
    let tooltip = "Gnirehtet VD wired link".encode_utf16();
    for (destination, source) in icon.szTip.iter_mut().zip(tooltip) {
        *destination = source;
    }
    if unsafe { Shell_NotifyIconW(NIM_ADD, &icon) } == 0 {
        unsafe {
            DestroyWindow(hwnd);
        }
        return Err(std::io::Error::last_os_error());
    }

    let mut message = MSG::default();
    loop {
        let result = unsafe { GetMessageW(&mut message, null_mut(), 0, 0) };
        if result == 0 {
            break;
        }
        if result == -1 {
            unsafe {
                DestroyWindow(hwnd);
            }
            return Err(std::io::Error::last_os_error());
        }
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    drop(instance_guard);
    Ok(())
}

fn wait_for_connected(paths: &AppPaths, session: SessionId, daemon: &mut Child) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(30);
    let store = StateStore::new(&paths.status);
    let session_text = session.to_string();
    while Instant::now() < deadline {
        let status = store.read_or_stopped();
        if status.lifecycle.session_id.as_deref() == Some(session_text.as_str()) {
            match status.lifecycle.state {
                HostState::Connected => return Ok(()),
                HostState::Error => {
                    bail!(
                        "Android v4 reported an error before STARTED: {}",
                        status
                            .lifecycle
                            .reason
                            .as_deref()
                            .unwrap_or("unknown error")
                    )
                }
                _ => {}
            }
        }
        if daemon.try_wait()?.is_some() {
            bail!("host daemon exited before Android acknowledged STARTED");
        }
        thread::sleep(Duration::from_millis(50));
    }
    bail!("timed out after 30 seconds waiting for Android GNR4 STARTED")
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn public_start_cli_does_not_allow_a_package_override() {
        assert!(Cli::try_parse_from([
            "gnirehtet-vd",
            "start",
            "--vd-package",
            "com.example.unexpected",
        ])
        .is_err());
        assert!(Cli::try_parse_from(["gnirehtet-vd", "start", "--all-traffic"]).is_ok());
    }

    #[test]
    fn ok_without_typed_repair_suppression_is_not_a_safe_stop_ack() {
        let response = AdminResponse {
            ok: true,
            repairs_suppressed: false,
            error: None,
            status: None,
        };
        assert!(!admin_stop_acknowledges_suppression(&response));
    }

    #[test]
    fn missing_runtime_never_reports_a_stale_connected_or_stopping_state() {
        let mut connected = StateSnapshot {
            state: HostState::Connected,
            session_id: Some("00112233-4455-6677-8899-aabbccddeeff".into()),
            missed_heartbeats: 0,
            reason: None,
        };
        reflect_missing_runtime(&mut connected);
        assert_eq!(connected.state, HostState::Degraded);
        assert!(connected.reason.as_deref().unwrap().contains("not running"));

        let mut stopping = connected;
        stopping.state = HostState::Stopping;
        reflect_missing_runtime(&mut stopping);
        assert_eq!(stopping.state, HostState::Error);
        assert!(stopping.reason.as_deref().unwrap().contains("unverified"));
    }

    #[test]
    fn daemon_cli_rejects_zero_session_nonce() {
        assert!(Cli::try_parse_from([
            "gnirehtet-vd",
            "daemon",
            "--session",
            "00000000-0000-0000-0000-000000000000",
        ])
        .is_err());
    }

    #[test]
    fn normal_help_hides_development_and_daemon_controls() {
        let help = Cli::command().render_long_help().to_string();
        assert!(!help.contains("--home"));
        assert!(!help.contains("--adb"));
        assert!(!help.contains("daemon"));
        for command in [
            "start",
            "stop",
            "status",
            "repair",
            "doctor",
            "diagnostics",
            "version",
        ] {
            assert!(help.contains(command), "public help omitted {command}");
        }
    }

    #[test]
    fn tray_menu_ids_map_only_to_the_public_commands() {
        assert_eq!(
            tray_command_arguments(tray_command_from_id(TRAY_STATUS_ID).unwrap()),
            Some(&["status"][..])
        );
        assert_eq!(
            tray_command_arguments(tray_command_from_id(TRAY_START_ID).unwrap()),
            Some(&["start"][..])
        );
        assert_eq!(
            tray_command_arguments(tray_command_from_id(TRAY_STOP_ID).unwrap()),
            Some(&["stop"][..])
        );
        assert_eq!(
            tray_command_arguments(tray_command_from_id(TRAY_REPAIR_ID).unwrap()),
            Some(&["repair"][..])
        );
        assert_eq!(
            tray_command_arguments(tray_command_from_id(TRAY_EXIT_ID).unwrap()),
            None
        );
        assert_eq!(tray_command_from_id(0), None);
    }

    #[test]
    fn tray_allows_only_one_command_in_flight() {
        let in_flight = AtomicBool::new(false);
        assert!(begin_tray_command(&in_flight));
        assert!(!begin_tray_command(&in_flight));
        in_flight.store(false, Ordering::Release);
        assert!(begin_tray_command(&in_flight));
    }

    #[test]
    fn broker_accepts_only_typed_public_commands() {
        assert_eq!(
            BrokerCommand::try_from(Command::Start(StartArgs { all_traffic: true })).unwrap(),
            BrokerCommand::Start { all_traffic: true }
        );
        assert_eq!(
            BrokerCommand::try_from(Command::Status(StatusArgs { json: true })).unwrap(),
            BrokerCommand::Status { json: true }
        );
        assert_eq!(
            BrokerCommand::try_from(Command::Diagnostics(DiagnosticsArgs {
                command: DiagnosticsCommand::Capture { duration: 42 },
            }))
            .unwrap(),
            BrokerCommand::DiagnosticsCapture { duration: 42 }
        );
        assert!(BrokerCommand::try_from(Command::Daemon(DaemonArgs {
            session: SessionId([0x11; 16]),
        }))
        .is_err());
        assert!(BrokerCommand::DiagnosticsCapture { duration: 0 }
            .into_public_command()
            .is_err());
    }

    #[test]
    fn broker_deadlines_are_bounded_by_command_shape() {
        assert_eq!(
            BrokerCommand::Version.response_timeout(),
            Duration::from_secs(5)
        );
        assert_eq!(
            BrokerCommand::Start { all_traffic: false }.response_timeout(),
            Duration::from_secs(180)
        );
        assert_eq!(
            BrokerCommand::DiagnosticsCapture { duration: u64::MAX }.response_timeout(),
            Duration::from_secs(3630)
        );
    }

    #[test]
    fn broker_response_preserves_stream_and_exit_semantics() {
        let success = BrokerResponse::success("done".into());
        assert_eq!(success.exit_code, 0);
        assert_eq!(success.stdout, "done\n");
        assert!(success.stderr.is_empty());

        let failure = BrokerResponse::failure("failed");
        assert_eq!(failure.exit_code, 1);
        assert!(failure.stdout.is_empty());
        assert_eq!(failure.stderr, "Error: failed\n");
    }

    #[tokio::test]
    async fn broker_frames_round_trip_and_reject_oversized_lengths() {
        let request = BrokerRequest {
            protocol_version: BROKER_PROTOCOL_VERSION,
            instance_root: PathBuf::from("test-root"),
            adb_program: PathBuf::from("test-adb"),
            command: BrokerCommand::Status { json: true },
        };
        let (mut sender, mut receiver) = tokio::io::duplex(1024);
        let write = tokio::spawn(async move { write_broker_frame(&mut sender, &request).await });
        let decoded: BrokerRequest = read_broker_frame(&mut receiver).await.unwrap();
        write.await.unwrap().unwrap();
        assert_eq!(decoded.protocol_version, BROKER_PROTOCOL_VERSION);
        assert_eq!(decoded.command, BrokerCommand::Status { json: true });

        let (mut sender, mut receiver) = tokio::io::duplex(16);
        sender
            .write_u32((MAX_BROKER_FRAME + 1) as u32)
            .await
            .unwrap();
        assert!(read_broker_frame::<BrokerRequest, _>(&mut receiver)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn broker_executes_in_process_and_propagates_output() {
        let directory = tempfile::tempdir().unwrap();
        let paths = AppPaths::discover(Some(directory.path().to_owned())).unwrap();
        let instance_root = paths.root.clone();
        let adb_program = directory.path().join("unused-adb");
        let context = BrokerContext::new(
            paths,
            adb_program.clone(),
            AdbController::new(Arc::new(SystemAdb::new(adb_program.clone()))),
            BrokerGate::default(),
        );
        let (mut client, server) = tokio::io::duplex(4096);
        let handler = tokio::spawn(async move { handle_broker_connection(server, &context).await });
        write_broker_frame(
            &mut client,
            &BrokerRequest {
                protocol_version: BROKER_PROTOCOL_VERSION,
                instance_root,
                adb_program,
                command: BrokerCommand::Version,
            },
        )
        .await
        .unwrap();
        let response: BrokerResponse = read_broker_frame(&mut client).await.unwrap();
        handler.await.unwrap().unwrap();
        assert_eq!(response.exit_code, 0);
        assert!(response.stdout.contains(env!("CARGO_PKG_VERSION")));
        assert!(response.stderr.is_empty());
    }

    #[test]
    fn broker_gate_makes_command_and_tray_shutdown_atomic() {
        let gate = BrokerGate::default();
        let tray_command = AtomicBool::new(false);
        let command = gate.begin_command().unwrap();
        assert_eq!(gate.snapshot(), (true, false));
        assert!(!begin_tray_shutdown(&tray_command, &gate));
        drop(command);
        assert_eq!(gate.snapshot(), (false, false));

        assert!(begin_tray_shutdown(&tray_command, &gate));
        assert_eq!(gate.snapshot(), (false, true));
        assert!(gate.begin_command().is_none());
        gate.cancel_shutdown();

        tray_command.store(true, Ordering::Release);
        assert!(!begin_tray_shutdown(&tray_command, &gate));
        assert_eq!(gate.snapshot(), (false, false));
    }

    #[test]
    fn broker_replaces_stale_adb_context_after_bootstrap() {
        let directory = tempfile::tempdir().unwrap();
        let paths = AppPaths::discover(Some(directory.path().to_owned())).unwrap();
        let original = PathBuf::from("adb");
        let context = BrokerContext::new(
            paths,
            original.clone(),
            AdbController::new(Arc::new(SystemAdb::new(original))),
            BrokerGate::default(),
        );
        let verified = directory.path().join("platform-tools/adb.exe");
        context.replace_adb(verified.clone()).unwrap();
        assert_eq!(context.environment().unwrap().adb_program, verified);
    }
}
