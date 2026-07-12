use std::{
    env, fs, io,
    net::{Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    process::Command,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex as StdMutex,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[cfg(target_os = "windows")]
use wait_timeout::ChildExt;
#[cfg(target_os = "windows")]
use {std::io::Read, std::process::Stdio};

use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::{Mutex, Notify, Semaphore},
    task, time,
};

#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};

#[cfg(target_os = "windows")]
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeServer, ServerOptions};

use crate::{
    adb::{AdbController, AndroidVpnStatus, CONTROL_PORT, SOCKS_PORT, UDP_STREAM_PORT},
    control::{ControlConfig, ControlHandle, ControlServer, StateObserver},
    diagnostics::Diagnostics,
    protocol::SessionId,
    socks::{SocksCommandPolicy, SocksConfig, SocksServer},
    state::{HostState, StateSnapshot},
};

const MAX_ADMIN_MESSAGE: usize = 4 * 1024;
const MAX_ADMIN_CONNECTIONS: usize = 8;
const ADMIN_IO_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone, Debug)]
pub struct AppPaths {
    pub root: PathBuf,
    pub logs: PathBuf,
    pub status: PathBuf,
    pub daemon_pid: PathBuf,
    pub operation_lock: PathBuf,
    pub runtime_lock: PathBuf,
    pub admin_token: PathBuf,
    #[cfg(unix)]
    pub admin_socket: PathBuf,
}

impl AppPaths {
    pub fn discover(override_root: Option<PathBuf>) -> io::Result<Self> {
        let root = if let Some(root) = override_root {
            root
        } else if let Some(root) = env::var_os("GNIREHTET_VD_HOME") {
            PathBuf::from(root)
        } else if cfg!(target_os = "windows") {
            env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .unwrap_or_else(env::temp_dir)
                .join("GnirehtetVD")
        } else {
            env::var_os("XDG_STATE_HOME")
                .map(PathBuf::from)
                .or_else(|| {
                    env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state"))
                })
                .unwrap_or_else(env::temp_dir)
                .join("gnirehtet-vd")
        };
        fs::create_dir_all(&root)?;
        let logs = root.join("logs");
        fs::create_dir_all(&logs)?;
        Ok(Self {
            status: root.join("status.json"),
            daemon_pid: root.join("daemon.pid"),
            operation_lock: root.join("operation.lock"),
            runtime_lock: root.join("runtime.lock"),
            admin_token: root.join("admin.token"),
            #[cfg(unix)]
            admin_socket: root.join("admin.sock"),
            logs,
            root,
        })
    }
}

#[derive(Clone, Debug)]
pub struct StateStore {
    path: PathBuf,
    lock: Arc<StdMutex<()>>,
}

impl StateStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            lock: Arc::new(StdMutex::new(())),
        }
    }

    pub fn write(&self, snapshot: &StateSnapshot, daemon_pid: Option<u32>) -> io::Result<()> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| io::Error::other("state store lock poisoned"))?;
        let telemetry = fs::read(&self.path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<PersistedStatus>(&bytes).ok())
            .and_then(|status| status.telemetry);
        self.write_document(snapshot, daemon_pid, telemetry)
    }

    pub fn write_with_telemetry(
        &self,
        snapshot: &StateSnapshot,
        daemon_pid: Option<u32>,
        telemetry: RuntimeTelemetry,
    ) -> io::Result<()> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| io::Error::other("state store lock poisoned"))?;
        self.write_document(snapshot, daemon_pid, Some(telemetry))
    }

    fn write_document(
        &self,
        snapshot: &StateSnapshot,
        daemon_pid: Option<u32>,
        telemetry: Option<RuntimeTelemetry>,
    ) -> io::Result<()> {
        let document = PersistedStatus {
            lifecycle: snapshot.clone(),
            daemon_pid,
            // The writer owns this process identity already. Probing here
            // would launch a Windows liveness subprocess on every heartbeat
            // and telemetry tick; external reads perform the bounded probe.
            daemon_running: daemon_pid.is_some(),
            updated_unix_ms: unix_millis(),
            telemetry,
        };
        let temporary = self.path.with_extension("json.tmp");
        fs::write(
            &temporary,
            serde_json::to_vec_pretty(&document).map_err(io::Error::other)?,
        )?;
        replace_file(&temporary, &self.path)
    }

    pub fn read(&self) -> io::Result<PersistedStatus> {
        let bytes = fs::read(&self.path)?;
        serde_json::from_slice(&bytes).map_err(io::Error::other)
    }

    pub fn read_or_stopped(&self) -> PersistedStatus {
        self.read().unwrap_or_else(|_| PersistedStatus::stopped())
    }
}

#[cfg(not(target_os = "windows"))]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(target_os = "windows")]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_REPLACE_EXISTING};

    let source: Vec<u16> = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    if unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING,
        )
    } == 0
    {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PersistedStatus {
    pub lifecycle: StateSnapshot,
    pub daemon_pid: Option<u32>,
    pub daemon_running: bool,
    pub updated_unix_ms: u128,
    #[serde(default)]
    pub telemetry: Option<RuntimeTelemetry>,
}

impl PersistedStatus {
    pub fn stopped() -> Self {
        Self {
            lifecycle: StateSnapshot {
                state: HostState::Stopped,
                session_id: None,
                missed_heartbeats: 0,
                reason: None,
            },
            daemon_pid: None,
            daemon_running: false,
            updated_unix_ms: unix_millis(),
            telemetry: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RuntimeTelemetry {
    pub control: crate::control::ControlMetricsSnapshot,
    pub relay: crate::socks::SocksStatsSnapshot,
    #[serde(default)]
    pub adb: AdbMonitorSnapshot,
    pub process: crate::diagnostics::ProcessSample,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct AdbMonitorSnapshot {
    pub active: bool,
    pub repair_suppressed: bool,
    pub device_available: bool,
    pub mappings_healthy: bool,
    pub reconnect_generation: u64,
    pub last_error: Option<String>,
}

#[derive(Clone, Default)]
pub struct AdbHealthMonitor {
    stopping: Arc<AtomicBool>,
    reconnect_generation: Arc<AtomicU64>,
    status: Arc<StdMutex<AdbMonitorSnapshot>>,
    operation: Arc<Mutex<()>>,
}

impl AdbHealthMonitor {
    pub fn snapshot(&self) -> AdbMonitorSnapshot {
        let mut snapshot = self
            .status
            .lock()
            .map(|status| status.clone())
            .unwrap_or_default();
        snapshot.repair_suppressed = self.stopping.load(Ordering::Acquire);
        snapshot.reconnect_generation = self.reconnect_generation.load(Ordering::Relaxed);
        snapshot
    }

    /// Prevents any new repair and waits for an in-flight bounded ADB command
    /// to finish before the explicit STOP transaction is sent to Android.
    pub async fn suppress_repairs(&self) {
        self.stopping.store(true, Ordering::Release);
        let _operation = self.operation.lock().await;
        if let Ok(mut status) = self.status.lock() {
            status.active = false;
            status.repair_suppressed = true;
        }
    }

    async fn run(
        self,
        adb: AdbController,
        control: ControlHandle,
        store: StateStore,
        diagnostics: Diagnostics,
    ) {
        const HEALTHY_INTERVAL: Duration = Duration::from_secs(1);
        const INITIAL_BACKOFF: Duration = Duration::from_millis(250);
        // A one-second cap leaves room for bounded ADB commands and mapping
        // recreation inside the three-second reconnect acceptance window.
        const MAX_BACKOFF: Duration = Duration::from_secs(1);

        let mut backoff = INITIAL_BACKOFF;
        loop {
            if self.stopping.load(Ordering::Acquire) {
                return;
            }
            let lifecycle = control.snapshot().await;
            if !matches!(lifecycle.state, HostState::Connected | HostState::Degraded) {
                self.update_status(false, false, false, None);
                time::sleep(HEALTHY_INTERVAL).await;
                continue;
            }

            let _operation = self.operation.lock().await;
            if self.stopping.load(Ordering::Acquire) {
                return;
            }
            let probe_adb = adb.clone();
            let probe = task::spawn_blocking(move || probe_adb_health(&probe_adb)).await;
            let probe = match probe {
                Ok(probe) => probe,
                Err(_) => AdbHealthProbe::DeviceUnavailable,
            };
            match probe {
                AdbHealthProbe::Healthy => {
                    self.update_status(true, true, true, None);
                    backoff = INITIAL_BACKOFF;
                    drop(_operation);
                    time::sleep(HEALTHY_INTERVAL).await;
                }
                AdbHealthProbe::DeviceUnavailable => {
                    self.record_loss(&control, &store, &diagnostics, false, "device_unavailable")
                        .await;
                    drop(_operation);
                    time::sleep(backoff).await;
                    backoff = next_backoff(backoff, MAX_BACKOFF);
                }
                AdbHealthProbe::MappingsMissing(missing) => {
                    self.record_loss(
                        &control,
                        &store,
                        &diagnostics,
                        true,
                        "reverse_mapping_unhealthy",
                    )
                    .await;
                    if self.stopping.load(Ordering::Acquire) {
                        return;
                    }
                    let repair_adb = adb.clone();
                    let repaired =
                        task::spawn_blocking(move || repair_adb.repair_missing_mappings(&missing))
                            .await;
                    match repaired {
                        Ok(Ok(())) => {
                            let generation = self
                                .reconnect_generation
                                .fetch_add(1, Ordering::Relaxed)
                                .saturating_add(1);
                            self.update_status(true, true, true, None);
                            let _ = diagnostics.record(
                                "adb_mapping_repaired",
                                json!({"reconnect_generation": generation}),
                            );
                            backoff = INITIAL_BACKOFF;
                        }
                        Ok(Err(_)) => {
                            self.update_status(
                                true,
                                true,
                                false,
                                Some("mapping_repair_failed".into()),
                            );
                            backoff = next_backoff(backoff, MAX_BACKOFF);
                        }
                        Err(_) => {
                            self.update_status(
                                true,
                                true,
                                false,
                                Some("mapping_repair_worker_failed".into()),
                            );
                            backoff = next_backoff(backoff, MAX_BACKOFF);
                        }
                    }
                    drop(_operation);
                    time::sleep(backoff).await;
                }
            }
        }
    }

    fn update_status(
        &self,
        active: bool,
        device_available: bool,
        mappings_healthy: bool,
        last_error: Option<String>,
    ) {
        if let Ok(mut status) = self.status.lock() {
            *status = AdbMonitorSnapshot {
                active,
                repair_suppressed: self.stopping.load(Ordering::Acquire),
                device_available,
                mappings_healthy,
                reconnect_generation: self.reconnect_generation.load(Ordering::Relaxed),
                last_error,
            };
        }
    }

    async fn record_loss(
        &self,
        control: &ControlHandle,
        store: &StateStore,
        diagnostics: &Diagnostics,
        device_available: bool,
        category: &'static str,
    ) {
        self.update_status(true, device_available, false, Some(category.into()));
        let snapshot = control
            .transport_lost(format!("ADB carrier unavailable ({category})"))
            .await;
        let _ = store.write(&snapshot, Some(std::process::id()));
        let _ = diagnostics.record(
            "adb_health_degraded",
            json!({
                "device_available": device_available,
                "mapping_state": "unhealthy",
                "category": if device_available { "mapping" } else { "device" },
            }),
        );
    }
}

enum AdbHealthProbe {
    Healthy,
    DeviceUnavailable,
    MappingsMissing(Vec<crate::adb::ReverseMapping>),
}

fn probe_adb_health(adb: &AdbController) -> AdbHealthProbe {
    match adb.device_state() {
        Ok(state) if state == "device" => {}
        Ok(_) | Err(_) => return AdbHealthProbe::DeviceUnavailable,
    }
    match adb.mapping_health() {
        Ok(health) if health.is_healthy() => AdbHealthProbe::Healthy,
        Ok(health) => AdbHealthProbe::MappingsMissing(health.missing),
        Err(_) => AdbHealthProbe::DeviceUnavailable,
    }
}

fn next_backoff(current: Duration, maximum: Duration) -> Duration {
    current.saturating_mul(2).min(maximum)
}

pub struct OperationGuard {
    path: PathBuf,
    _file: fs::File,
}

impl OperationGuard {
    pub fn acquire(path: impl Into<PathBuf>) -> io::Result<Self> {
        let path = path.into();
        let open = || {
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
        };
        let mut file = match open() {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let owner = fs::read_to_string(&path)
                    .ok()
                    .and_then(|value| value.trim().parse::<u32>().ok());
                if owner.is_some_and(process_is_running) {
                    return Err(error);
                }
                fs::remove_file(&path)?;
                open()?
            }
            Err(error) => return Err(error),
        };
        use std::io::Write;
        writeln!(file, "{}", std::process::id())?;
        Ok(Self { path, _file: file })
    }
}

impl Drop for OperationGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[derive(Clone, Debug)]
pub struct RuntimeConfig {
    pub control_bind: SocketAddr,
    pub socks_bind: SocketAddr,
    pub udp_bind: SocketAddr,
    pub session_id: SessionId,
    pub paths: AppPaths,
    pub adb: AdbController,
}

impl RuntimeConfig {
    pub fn new(session_id: SessionId, paths: AppPaths, adb: AdbController) -> Self {
        Self {
            control_bind: SocketAddr::new(Ipv4Addr::LOCALHOST.into(), CONTROL_PORT),
            socks_bind: SocketAddr::new(Ipv4Addr::LOCALHOST.into(), SOCKS_PORT),
            udp_bind: SocketAddr::new(Ipv4Addr::LOCALHOST.into(), UDP_STREAM_PORT),
            session_id,
            paths,
            adb,
        }
    }
}

pub struct HostRuntime {
    config: RuntimeConfig,
}

impl HostRuntime {
    pub fn new(config: RuntimeConfig) -> Result<Self, RuntimeError> {
        if !config.control_bind.ip().is_loopback() {
            return Err(RuntimeError::NonLoopback(config.control_bind));
        }
        if !config.socks_bind.ip().is_loopback() {
            return Err(RuntimeError::NonLoopback(config.socks_bind));
        }
        if !config.udp_bind.ip().is_loopback() {
            return Err(RuntimeError::NonLoopback(config.udp_bind));
        }
        Ok(Self { config })
    }

    pub async fn run(self) -> Result<(), RuntimeError> {
        install_runtime_kill_job()?;
        let _runtime_guard = OperationGuard::acquire(&self.config.paths.runtime_lock)?;
        let diagnostics = Diagnostics::open(&self.config.paths.logs)?;
        let store = StateStore::new(&self.config.paths.status);
        write_daemon_identity(&self.config.paths.daemon_pid, self.config.session_id)?;
        let observer_store = store.clone();
        let observer_diagnostics = diagnostics.clone();
        let observer: StateObserver = Arc::new(move |snapshot| {
            if let Err(error) = observer_store.write(snapshot, Some(std::process::id())) {
                let _ = observer_diagnostics.record(
                    "state_persistence_error",
                    json!({"error_kind": format!("{:?}", error.kind())}),
                );
            }
        });

        let control = ControlServer::new(ControlConfig {
            bind: self.config.control_bind,
            session_id: self.config.session_id,
        })?
        .with_diagnostics(diagnostics.clone())
        .with_observer(observer);
        let control_handle = control.command_handle();
        let adb_monitor = AdbHealthMonitor::default();
        let shutdown = Arc::new(Notify::new());
        let admin = AdminServer::new(
            self.config.paths.clone(),
            load_or_create_admin_token(&self.config.paths.admin_token)?,
            control_handle.clone(),
            adb_monitor.clone(),
            shutdown.clone(),
        );
        let tcp_socks = SocksServer::new(SocksConfig {
            bind: self.config.socks_bind,
            command_policy: SocksCommandPolicy::ConnectOnly,
            ..Default::default()
        })?
        .with_diagnostics(diagnostics.clone());
        let udp_socks = SocksServer::new(SocksConfig {
            bind: self.config.udp_bind,
            command_policy: SocksCommandPolicy::FwdUdpOnly,
            ..Default::default()
        })?
        .with_diagnostics(diagnostics.clone());
        let metrics_tcp_socks = tcp_socks.clone();
        let metrics_udp_socks = udp_socks.clone();
        let metrics_control = control.clone();
        let metrics_control_handle = control_handle.clone();
        let metrics_adb = adb_monitor.clone();
        let metrics_store = store.clone();
        let metrics_diagnostics = diagnostics.clone();
        let metrics = tokio::spawn(async move {
            let mut interval = time::interval(Duration::from_secs(1));
            interval.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                let process = metrics_diagnostics
                    .capture_process_sample()
                    .unwrap_or_default();
                let mut relay = metrics_udp_socks.stats();
                let tcp = metrics_tcp_socks.stats();
                relay.accepted_connections = relay
                    .accepted_connections
                    .saturating_add(tcp.accepted_connections);
                relay.rejected_connections = relay
                    .rejected_connections
                    .saturating_add(tcp.rejected_connections);
                relay.active_connections = relay
                    .active_connections
                    .saturating_add(tcp.active_connections);
                relay.tcp_tx_bytes = relay.tcp_tx_bytes.saturating_add(tcp.tcp_tx_bytes);
                relay.tcp_rx_bytes = relay.tcp_rx_bytes.saturating_add(tcp.tcp_rx_bytes);
                let control = metrics_control.metrics();
                if let Ok(fields) = serde_json::to_value(&relay) {
                    let _ = metrics_diagnostics.record("relay_counters", fields);
                }
                if let Ok(fields) = serde_json::to_value(&control) {
                    let _ = metrics_diagnostics.record("control_counters", fields);
                }
                let telemetry = RuntimeTelemetry {
                    control,
                    relay,
                    adb: metrics_adb.snapshot(),
                    process,
                };
                let snapshot = metrics_control_handle.snapshot().await;
                let _ = metrics_store.write_with_telemetry(
                    &snapshot,
                    Some(std::process::id()),
                    telemetry,
                );
            }
        });

        let monitor = tokio::spawn(adb_monitor.clone().run(
            self.config.adb.clone(),
            control_handle.clone(),
            store.clone(),
            diagnostics.clone(),
        ));

        let result = tokio::select! {
            result = control.serve() => result.map_err(RuntimeError::from),
            result = tcp_socks.serve() => result.map_err(RuntimeError::from),
            result = udp_socks.serve() => result.map_err(RuntimeError::from),
            result = admin.serve() => result,
            _ = shutdown.notified() => Ok(()),
            result = tokio::signal::ctrl_c() => result.map_err(RuntimeError::Io),
        };
        adb_monitor.suppress_repairs().await;
        monitor.abort();
        metrics.abort();
        let _ = fs::remove_file(&self.config.paths.daemon_pid);
        result
    }
}

#[derive(Clone)]
pub struct AdminServer {
    #[cfg(unix)]
    paths: AppPaths,
    token: String,
    control: ControlHandle,
    adb_monitor: AdbHealthMonitor,
    shutdown: Arc<Notify>,
    max_connections: usize,
    io_timeout: Duration,
}

impl AdminServer {
    pub fn new(
        paths: AppPaths,
        token: String,
        control: ControlHandle,
        adb_monitor: AdbHealthMonitor,
        shutdown: Arc<Notify>,
    ) -> Self {
        #[cfg(target_os = "windows")]
        let _ = paths;
        Self {
            #[cfg(unix)]
            paths,
            token,
            control,
            adb_monitor,
            shutdown,
            max_connections: MAX_ADMIN_CONNECTIONS,
            io_timeout: ADMIN_IO_TIMEOUT,
        }
    }

    #[cfg(all(test, unix))]
    fn with_limits(mut self, max_connections: usize, io_timeout: Duration) -> Self {
        self.max_connections = max_connections;
        self.io_timeout = io_timeout;
        self
    }

    pub async fn serve(self) -> Result<(), RuntimeError> {
        #[cfg(unix)]
        {
            return self.serve_unix().await;
        }
        #[cfg(target_os = "windows")]
        {
            return self.serve_windows().await;
        }
        #[allow(unreachable_code)]
        Err(RuntimeError::AdminTransportUnsupported)
    }

    #[cfg(unix)]
    async fn serve_unix(self) -> Result<(), RuntimeError> {
        if self.paths.admin_socket.exists() {
            match UnixStream::connect(&self.paths.admin_socket).await {
                Ok(_) => {
                    return Err(RuntimeError::Io(io::Error::new(
                        io::ErrorKind::AddrInUse,
                        "per-user admin socket is already serving",
                    )))
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
                    ) =>
                {
                    fs::remove_file(&self.paths.admin_socket)?;
                }
                Err(error) => return Err(RuntimeError::Io(error)),
            }
        }
        let listener = UnixListener::bind(&self.paths.admin_socket)?;
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&self.paths.admin_socket, fs::Permissions::from_mode(0o600))?;
        let _cleanup = UnixSocketCleanup(self.paths.admin_socket.clone());
        self.serve_unix_on(listener).await
    }

    #[cfg(unix)]
    async fn serve_unix_on(self, listener: UnixListener) -> Result<(), RuntimeError> {
        let permits = Arc::new(Semaphore::new(self.max_connections));
        loop {
            let (stream, _) = listener.accept().await?;
            let permit = match permits.clone().try_acquire_owned() {
                Ok(permit) => permit,
                Err(_) => continue,
            };
            let server = self.clone();
            tokio::spawn(async move {
                let _ = server.handle(stream).await;
                drop(permit);
            });
        }
    }

    #[cfg(target_os = "windows")]
    async fn serve_windows(self) -> Result<(), RuntimeError> {
        let pipe_name = windows_admin_pipe_name()?;
        let permits = Arc::new(Semaphore::new(self.max_connections));
        let mut first = true;
        loop {
            // Reserve userspace capacity before allocating the next kernel
            // instance. Creating a ninth instance while eight handlers are
            // active would otherwise hit the pipe instance limit and tear down
            // the complete host runtime.
            let permit = permits
                .clone()
                .acquire_owned()
                .await
                .map_err(|_| RuntimeError::AdminTransportClosed)?;
            let server = match create_secure_named_pipe(&pipe_name, first, self.max_connections) {
                Ok(server) => server,
                Err(error)
                    if error.raw_os_error()
                        == Some(windows_sys::Win32::Foundation::ERROR_PIPE_BUSY as i32) =>
                {
                    drop(permit);
                    time::sleep(Duration::from_millis(25)).await;
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            first = false;
            server.connect().await?;
            let service = self.clone();
            tokio::spawn(async move {
                let _ = service.handle(server).await;
                drop(permit);
            });
        }
    }

    async fn handle<S>(&self, mut stream: S) -> Result<(), RuntimeError>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let request: AdminRequest = time::timeout(self.io_timeout, read_admin_message(&mut stream))
            .await
            .map_err(|_| RuntimeError::AdminTimeout)??;
        if !constant_time_eq(request.token.as_bytes(), self.token.as_bytes()) {
            time::timeout(
                self.io_timeout,
                write_admin_message(
                    &mut stream,
                    &AdminResponse {
                        ok: false,
                        repairs_suppressed: false,
                        error: Some("authentication failed".into()),
                        status: None,
                    },
                ),
            )
            .await
            .map_err(|_| RuntimeError::AdminTimeout)??;
            return Ok(());
        }
        let response = match request.command.as_str() {
            "stop" => {
                self.adb_monitor.suppress_repairs().await;
                match self.control.request_stop(Duration::from_secs(6)).await {
                    Ok(()) => AdminResponse {
                        ok: true,
                        repairs_suppressed: true,
                        error: None,
                        status: Some(self.control.snapshot().await),
                    },
                    Err(error) => AdminResponse {
                        ok: false,
                        repairs_suppressed: true,
                        error: Some(error.to_string()),
                        status: Some(self.control.snapshot().await),
                    },
                }
            }
            "status" => AdminResponse {
                ok: true,
                repairs_suppressed: false,
                error: None,
                status: Some(self.control.snapshot().await),
            },
            "shutdown" => {
                let status = self.control.snapshot().await;
                let stopped = status.state == HostState::Stopped;
                AdminResponse {
                    ok: stopped,
                    repairs_suppressed: self.adb_monitor.snapshot().repair_suppressed,
                    error: (!stopped).then(|| "daemon shutdown requires stopped state".into()),
                    status: Some(status),
                }
            }
            _ => AdminResponse {
                ok: false,
                repairs_suppressed: false,
                error: Some("unknown command".into()),
                status: None,
            },
        };
        time::timeout(self.io_timeout, write_admin_message(&mut stream, &response))
            .await
            .map_err(|_| RuntimeError::AdminTimeout)??;
        if request.command == "shutdown" && response.ok {
            self.shutdown.notify_one();
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct AdminRequest {
    token: String,
    command: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct AdminResponse {
    pub ok: bool,
    #[serde(default)]
    pub repairs_suppressed: bool,
    pub error: Option<String>,
    pub status: Option<StateSnapshot>,
}

pub async fn admin_command(
    paths: &AppPaths,
    command: &str,
    timeout: Duration,
) -> Result<AdminResponse, RuntimeError> {
    let token = fs::read_to_string(&paths.admin_token)?.trim().to_owned();
    let operation = async {
        #[cfg(unix)]
        let mut stream = UnixStream::connect(&paths.admin_socket).await?;
        #[cfg(target_os = "windows")]
        let mut stream = open_named_pipe_client(&windows_admin_pipe_name()?).await?;
        #[cfg(not(any(unix, target_os = "windows")))]
        return Err(RuntimeError::AdminTransportUnsupported);
        write_admin_message(
            &mut stream,
            &AdminRequest {
                token,
                command: command.to_owned(),
            },
        )
        .await?;
        read_admin_message(&mut stream).await
    };
    time::timeout(timeout, operation)
        .await
        .map_err(|_| RuntimeError::AdminTimeout)?
}

async fn read_admin_message<T, S>(stream: &mut S) -> Result<T, RuntimeError>
where
    T: for<'de> Deserialize<'de>,
    S: AsyncRead + Unpin,
{
    let length = stream.read_u32().await? as usize;
    if length > MAX_ADMIN_MESSAGE {
        return Err(RuntimeError::AdminMessageTooLarge(length));
    }
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes).await?;
    serde_json::from_slice(&bytes).map_err(RuntimeError::Json)
}

async fn write_admin_message<T, S>(stream: &mut S, value: &T) -> Result<(), RuntimeError>
where
    T: Serialize,
    S: AsyncWrite + Unpin,
{
    let bytes = serde_json::to_vec(value).map_err(RuntimeError::Json)?;
    if bytes.len() > MAX_ADMIN_MESSAGE {
        return Err(RuntimeError::AdminMessageTooLarge(bytes.len()));
    }
    stream.write_u32(bytes.len() as u32).await?;
    stream.write_all(&bytes).await?;
    stream.flush().await?;
    Ok(())
}

#[cfg(unix)]
struct UnixSocketCleanup(PathBuf);

#[cfg(unix)]
impl Drop for UnixSocketCleanup {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[cfg(target_os = "windows")]
async fn open_named_pipe_client(
    pipe_name: &str,
) -> io::Result<tokio::net::windows::named_pipe::NamedPipeClient> {
    const ERROR_PIPE_BUSY_CODE: i32 = windows_sys::Win32::Foundation::ERROR_PIPE_BUSY as i32;
    loop {
        match ClientOptions::new().open(pipe_name) {
            Ok(client) => return Ok(client),
            Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY_CODE) => {
                time::sleep(Duration::from_millis(25)).await;
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(target_os = "windows")]
pub fn create_secure_named_pipe(
    pipe_name: &str,
    first: bool,
    max_instances: usize,
) -> io::Result<NamedPipeServer> {
    use std::ffi::c_void;
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;

    let descriptor = current_user_only_security_descriptor()?;
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    let mut options = ServerOptions::new();
    options
        .first_pipe_instance(first)
        .max_instances(max_instances)
        .reject_remote_clients(true);
    // SAFETY: `attributes` and its LocalAlloc-owned security descriptor stay
    // alive for the complete CreateNamedPipeW call. The handle does not retain
    // the pointer after creation.
    unsafe {
        options.create_with_security_attributes_raw(
            pipe_name,
            (&mut attributes as *mut SECURITY_ATTRIBUTES).cast::<c_void>(),
        )
    }
}

#[cfg(target_os = "windows")]
fn windows_admin_pipe_name() -> io::Result<String> {
    Ok(format!(
        r"\\.\pipe\gnirehtet-vd-{}",
        windows_current_user_sid()?.replace('-', "_")
    ))
}

/// Returns the separate per-user command-broker pipe name.
///
/// The data-plane daemon keeps using `windows_admin_pipe_name()`: separating
/// these lanes lets the always-on product instance serialize public commands
/// without weakening or overloading the daemon's authenticated control pipe.
#[cfg(target_os = "windows")]
pub fn windows_broker_pipe_name() -> io::Result<String> {
    Ok(format!(
        r"\\.\pipe\gnirehtet-vd-broker-{}",
        windows_current_user_sid()?.replace('-', "_")
    ))
}

#[cfg(target_os = "windows")]
struct LocalSecurityDescriptor(*mut std::ffi::c_void);

#[cfg(target_os = "windows")]
impl Drop for LocalSecurityDescriptor {
    fn drop(&mut self) {
        // SAFETY: the descriptor was allocated by LocalAlloc inside the SDDL
        // conversion API and is released exactly once here.
        unsafe {
            windows_sys::Win32::Foundation::LocalFree(self.0);
        }
    }
}

#[cfg(target_os = "windows")]
fn current_user_only_security_descriptor() -> io::Result<LocalSecurityDescriptor> {
    use std::ptr::null_mut;
    use windows_sys::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;

    let sddl = user_only_pipe_sddl(&windows_current_user_sid()?);
    let wide: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
    let mut descriptor = null_mut();
    // SAFETY: `wide` is NUL-terminated and `descriptor` is a valid output
    // pointer. Revision 1 is SDDL_REVISION_1.
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            wide.as_ptr(),
            1,
            &mut descriptor,
            null_mut(),
        )
    };
    if converted == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(LocalSecurityDescriptor(descriptor))
}

#[cfg(any(target_os = "windows", test))]
fn user_only_pipe_sddl(sid: &str) -> String {
    format!("D:P(A;;GA;;;{sid})")
}

#[cfg(target_os = "windows")]
pub fn windows_current_user_sid() -> io::Result<String> {
    use std::ptr::null_mut;
    use windows_sys::{
        core::PWSTR,
        Win32::{
            Foundation::{CloseHandle, GetLastError, LocalFree, ERROR_INSUFFICIENT_BUFFER, HANDLE},
            Security::{
                Authorization::ConvertSidToStringSidW, GetTokenInformation, TokenUser, TOKEN_QUERY,
                TOKEN_USER,
            },
            System::Threading::{GetCurrentProcess, OpenProcessToken},
        },
    };

    struct Token(HANDLE);
    impl Drop for Token {
        fn drop(&mut self) {
            // SAFETY: this owns one successful OpenProcessToken handle.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    let mut token = null_mut();
    // SAFETY: the pseudo process handle is always valid and `token` is an
    // initialized output pointer.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = Token(token);
    let mut required = 0;
    // The sizing call is expected to fail with insufficient buffer while
    // returning the exact required length.
    let sized = unsafe { GetTokenInformation(token.0, TokenUser, null_mut(), 0, &mut required) };
    if sized != 0
        || unsafe { GetLastError() } != ERROR_INSUFFICIENT_BUFFER
        || required < std::mem::size_of::<TOKEN_USER>() as u32
    {
        return Err(io::Error::last_os_error());
    }
    let word_count = (required as usize).div_ceil(std::mem::size_of::<usize>());
    let mut aligned = vec![0usize; word_count];
    // SAFETY: `aligned` has native pointer alignment and at least the byte size
    // returned by the API. It remains live while reading TOKEN_USER.User.Sid.
    if unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            aligned.as_mut_ptr().cast(),
            required,
            &mut required,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let user = unsafe { &*(aligned.as_ptr().cast::<TOKEN_USER>()) };
    let mut string_sid: PWSTR = null_mut();
    // SAFETY: TOKEN_USER owns a valid SID for the lifetime of `aligned` and the
    // output pointer is released with LocalFree below.
    if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut string_sid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let length = (0..)
        .position(|index| unsafe { *string_sid.add(index) == 0 })
        .ok_or_else(|| io::Error::other("Windows SID string was not terminated"))?;
    let sid = String::from_utf16(unsafe { std::slice::from_raw_parts(string_sid, length) })
        .map_err(io::Error::other);
    unsafe {
        LocalFree(string_sid.cast());
    }
    sid
}

fn load_or_create_admin_token(path: &Path) -> io::Result<String> {
    if let Ok(token) = fs::read_to_string(path) {
        let token = token.trim();
        if token
            .parse::<SessionId>()
            .is_ok_and(|session| session != SessionId::ZERO)
        {
            return Ok(token.to_owned());
        }
    }
    let token = SessionId::random().to_string();
    #[cfg(unix)]
    {
        use std::{
            io::Write,
            os::unix::fs::{OpenOptionsExt, PermissionsExt},
        };
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        file.write_all(token.as_bytes())?;
        file.sync_all()?;
    }
    #[cfg(not(unix))]
    fs::write(path, &token)?;
    Ok(token)
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    for index in 0..left.len().max(right.len()) {
        difference |= usize::from(
            left.get(index).copied().unwrap_or_default()
                ^ right.get(index).copied().unwrap_or_default(),
        );
    }
    difference == 0
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum StreamerProbe {
    Unsupported,
    CheckFailed,
    Unknown,
    NotRunning,
    RunningNotListening,
    Listening {
        tcp_listener_count: usize,
        udp_endpoint_count: usize,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DoctorReport {
    pub adb_state: Result<String, String>,
    pub reverse_mappings_healthy: bool,
    pub reverse_mapping_error: Option<String>,
    pub virtual_desktop_streamer: StreamerProbe,
    pub android_vpn: Result<AndroidVpnStatus, String>,
    pub tunnel_available: bool,
}

pub fn doctor(adb: &AdbController) -> DoctorReport {
    let adb_state = adb.device_state().map_err(|error| error.to_string());
    let mapping = adb.mapping_health();
    let (reverse_mappings_healthy, reverse_mapping_error) = match mapping {
        Ok(health) => (health.is_healthy(), None),
        Err(error) => (false, Some(error.to_string())),
    };
    let virtual_desktop_streamer = probe_virtual_desktop_streamer();
    let streamer_ready = matches!(virtual_desktop_streamer, StreamerProbe::Listening { .. });
    let android_vpn = adb.android_status().map_err(|error| error.to_string());
    let android_ready = android_vpn.as_ref().is_ok_and(|status| {
        status.vpn_fd_open == Some(true)
            && matches!(status.state.as_deref(), Some("connected" | "degraded"))
    });
    DoctorReport {
        tunnel_available: adb_state.as_deref() == Ok("device")
            && reverse_mappings_healthy
            && streamer_ready
            && android_ready,
        adb_state,
        reverse_mappings_healthy,
        reverse_mapping_error,
        virtual_desktop_streamer,
        android_vpn,
    }
}

#[cfg(target_os = "windows")]
fn probe_virtual_desktop_streamer() -> StreamerProbe {
    // Read-only: this intentionally does not start, stop, or kill VD.
    let script = "try {$p=Get-Process -Name 'VirtualDesktop.Streamer' -ErrorAction SilentlyContinue; if($null -eq $p){'NOT_RUNNING';exit}; $ids=@($p.Id); $tcp=@(Get-NetTCPConnection -State Listen -ErrorAction Stop | Where-Object {$ids -contains $_.OwningProcess}).Count; $udp=@(Get-NetUDPEndpoint -ErrorAction Stop | Where-Object {$ids -contains $_.OwningProcess}).Count; if(($tcp+$udp) -eq 0){'NOT_LISTENING'}else{'LISTENING '+$tcp+' '+$udp}} catch {'CHECK_FAILED'}";
    let output = command_text_with_timeout(
        "powershell.exe",
        &["-NoProfile", "-NonInteractive", "-Command", script],
        Duration::from_secs(3),
    );
    let Ok((status, text)) = output else {
        return StreamerProbe::CheckFailed;
    };
    if !status.success() {
        return StreamerProbe::CheckFailed;
    }
    let text = text.trim();
    if text == "NOT_RUNNING" {
        StreamerProbe::NotRunning
    } else if text == "NOT_LISTENING" {
        StreamerProbe::RunningNotListening
    } else if text == "CHECK_FAILED" {
        StreamerProbe::CheckFailed
    } else if let Some(value) = text.strip_prefix("LISTENING ") {
        let mut counts = value.split_whitespace();
        let tcp_listener_count = counts.next().and_then(|value| value.parse().ok());
        let udp_endpoint_count = counts.next().and_then(|value| value.parse().ok());
        match (tcp_listener_count, udp_endpoint_count) {
            (Some(tcp_listener_count), Some(udp_endpoint_count)) => StreamerProbe::Listening {
                tcp_listener_count,
                udp_endpoint_count,
            },
            _ => StreamerProbe::Unknown,
        }
    } else {
        StreamerProbe::Unknown
    }
}

#[cfg(not(target_os = "windows"))]
fn probe_virtual_desktop_streamer() -> StreamerProbe {
    StreamerProbe::Unsupported
}

pub fn process_is_running(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(target_os = "windows")]
    {
        windows_process_snapshot(pid).is_ok_and(|snapshot| snapshot.active)
    }
    #[cfg(unix)]
    {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .is_ok_and(|status| status.success())
    }
    #[cfg(not(any(unix, target_os = "windows")))]
    {
        false
    }
}

#[cfg(not(target_os = "windows"))]
fn install_runtime_kill_job() -> io::Result<()> {
    Ok(())
}

#[cfg(target_os = "windows")]
fn install_runtime_kill_job() -> io::Result<()> {
    use std::{ffi::c_void, ptr::null};
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::{
            JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
                SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            },
            Threading::GetCurrentProcess,
        },
    };

    static JOB_HANDLE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    if JOB_HANDLE.get().is_some() {
        return Ok(());
    }
    let job = unsafe { CreateJobObjectW(null(), null()) };
    if job.is_null() {
        return Err(io::Error::last_os_error());
    }
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    if unsafe {
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast::<c_void>(),
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    } == 0
    {
        let error = io::Error::last_os_error();
        unsafe {
            CloseHandle(job);
        }
        return Err(error);
    }
    if unsafe { AssignProcessToJobObject(job, GetCurrentProcess()) } == 0 {
        let error = io::Error::last_os_error();
        unsafe {
            CloseHandle(job);
        }
        return Err(error);
    }
    // Intentionally retain the handle until process teardown. When the daemon
    // is terminated, Windows closes this last handle and atomically kills any
    // in-flight ADB descendants before a later explicit-stop retry can remove
    // mappings.
    JOB_HANDLE
        .set(job as usize)
        .map_err(|_| io::Error::other("runtime Job Object was installed concurrently"))?;
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn terminate_process(pid: u32) -> io::Result<()> {
    #[cfg(unix)]
    let status = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()?;
    #[cfg(not(unix))]
    return Err(io::Error::new(io::ErrorKind::Unsupported, "unsupported OS"));
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other("daemon termination command failed"))
    }
}

#[cfg(target_os = "windows")]
fn command_text_with_timeout(
    program: &str,
    arguments: &[&str],
    timeout: Duration,
) -> io::Result<(std::process::ExitStatus, String)> {
    const MAX_COMMAND_OUTPUT: u64 = 64 * 1024;
    let mut child = Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let status = match child.wait_timeout(timeout)? {
        Some(status) => status,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("{program} exceeded {timeout:?}"),
            ));
        }
    };
    let mut output = Vec::new();
    child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("subprocess stdout was not captured"))?
        .take(MAX_COMMAND_OUTPUT)
        .read_to_end(&mut output)?;
    Ok((status, String::from_utf8_lossy(&output).into_owned()))
}

fn unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("host listeners must be loopback, got {0}")]
    NonLoopback(SocketAddr),
    #[error(transparent)]
    Control(#[from] crate::control::ControlError),
    #[error(transparent)]
    Socks(#[from] crate::socks::SocksError),
    #[error("host runtime I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("admin message length {0} exceeds the bound")]
    AdminMessageTooLarge(usize),
    #[error("admin command timed out")]
    AdminTimeout,
    #[error("per-user admin transport is unsupported on this platform")]
    AdminTransportUnsupported,
    #[error("per-user admin transport capacity was closed")]
    AdminTransportClosed,
    #[error("admin JSON failed: {0}")]
    Json(serde_json::Error),
}

pub fn read_daemon_pid(path: &Path) -> Option<u32> {
    #[cfg(target_os = "windows")]
    {
        let identity: DaemonIdentity = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
        let current = windows_process_snapshot(identity.pid).ok()?;
        daemon_identity_matches(&identity, &current).then_some(identity.pid)
    }
    #[cfg(not(target_os = "windows"))]
    {
        fs::read_to_string(path).ok()?.trim().parse().ok()
    }
}

#[cfg(any(target_os = "windows", test))]
#[derive(Clone, Debug, Deserialize, Serialize)]
struct DaemonIdentity {
    role: String,
    pid: u32,
    creation_time_100ns: u64,
    session_id: String,
    executable_path: String,
}

#[cfg(any(target_os = "windows", test))]
#[derive(Clone, Debug)]
struct WindowsProcessSnapshot {
    active: bool,
    creation_time_100ns: u64,
    executable_path: String,
}

#[cfg(any(target_os = "windows", test))]
fn daemon_identity_matches(identity: &DaemonIdentity, snapshot: &WindowsProcessSnapshot) -> bool {
    identity.role == "gnirehtet-vd-daemon"
        && identity
            .session_id
            .parse::<SessionId>()
            .is_ok_and(|session| session != SessionId::ZERO)
        && snapshot.active
        && snapshot.creation_time_100ns == identity.creation_time_100ns
        && snapshot
            .executable_path
            .eq_ignore_ascii_case(&identity.executable_path)
}

#[cfg(target_os = "windows")]
struct WindowsProcessHandle(windows_sys::Win32::Foundation::HANDLE);

#[cfg(target_os = "windows")]
impl Drop for WindowsProcessHandle {
    fn drop(&mut self) {
        // SAFETY: this owns one successful OpenProcess handle.
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

#[cfg(target_os = "windows")]
fn open_windows_process(pid: u32, access: u32) -> io::Result<WindowsProcessHandle> {
    let handle = unsafe { windows_sys::Win32::System::Threading::OpenProcess(access, 0, pid) };
    if handle.is_null() {
        Err(io::Error::last_os_error())
    } else {
        Ok(WindowsProcessHandle(handle))
    }
}

#[cfg(target_os = "windows")]
fn windows_process_snapshot(pid: u32) -> io::Result<WindowsProcessSnapshot> {
    let handle = open_windows_process(
        pid,
        windows_sys::Win32::System::Threading::PROCESS_QUERY_LIMITED_INFORMATION,
    )?;
    windows_process_snapshot_from_handle(handle.0)
}

#[cfg(target_os = "windows")]
fn windows_process_snapshot_from_handle(
    handle: windows_sys::Win32::Foundation::HANDLE,
) -> io::Result<WindowsProcessSnapshot> {
    use windows_sys::Win32::{
        Foundation::{FILETIME, STILL_ACTIVE},
        System::Threading::{GetExitCodeProcess, GetProcessTimes, QueryFullProcessImageNameW},
    };
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    if unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut exit_code = 0;
    if unsafe { GetExitCodeProcess(handle, &mut exit_code) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut executable = vec![0u16; 32_768];
    let mut executable_length = executable.len() as u32;
    if unsafe {
        QueryFullProcessImageNameW(handle, 0, executable.as_mut_ptr(), &mut executable_length)
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let executable_path =
        String::from_utf16(&executable[..executable_length as usize]).map_err(io::Error::other)?;
    Ok(WindowsProcessSnapshot {
        active: exit_code == STILL_ACTIVE as u32,
        creation_time_100ns: (u64::from(creation.dwHighDateTime) << 32)
            | u64::from(creation.dwLowDateTime),
        executable_path,
    })
}

#[cfg(target_os = "windows")]
fn write_daemon_identity(path: &Path, session_id: SessionId) -> io::Result<()> {
    let pid = std::process::id();
    let snapshot = windows_process_snapshot(pid)?;
    if !snapshot.active {
        return Err(io::Error::other("current host process is not active"));
    }
    fs::write(
        path,
        serde_json::to_vec(&DaemonIdentity {
            role: "gnirehtet-vd-daemon".into(),
            pid,
            creation_time_100ns: snapshot.creation_time_100ns,
            session_id: session_id.to_string(),
            executable_path: snapshot.executable_path,
        })
        .map_err(io::Error::other)?,
    )
}

#[cfg(not(target_os = "windows"))]
fn write_daemon_identity(path: &Path, _session_id: SessionId) -> io::Result<()> {
    fs::write(path, std::process::id().to_string())
}

/// Terminates only the exact daemon instance described by the identity file.
/// On Windows the creation time, non-zero GNR4 session, and full executable
/// path are verified on the same process handle used for termination, closing
/// the PID-reuse/TOCTOU window.
pub fn terminate_daemon(identity_path: &Path, timeout: Duration) -> io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        use windows_sys::Win32::{
            Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT},
            System::Threading::{
                TerminateProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION,
                PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
            },
        };

        let identity: DaemonIdentity =
            serde_json::from_slice(&fs::read(identity_path)?).map_err(io::Error::other)?;
        let handle = open_windows_process(
            identity.pid,
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE | PROCESS_TERMINATE,
        )?;
        let snapshot = windows_process_snapshot_from_handle(handle.0)?;
        if !daemon_identity_matches(&identity, &snapshot) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "daemon identity no longer matches the live process",
            ));
        }
        if unsafe { TerminateProcess(handle.0, 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let timeout_ms = timeout.as_millis().min(u128::from(u32::MAX)) as u32;
        match unsafe { WaitForSingleObject(handle.0, timeout_ms) } {
            WAIT_OBJECT_0 => Ok(()),
            WAIT_TIMEOUT => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "verified daemon did not exit before the deadline",
            )),
            _ => Err(io::Error::last_os_error()),
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = timeout;
        let pid = read_daemon_pid(identity_path).ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "daemon identity is unavailable")
        })?;
        terminate_process(pid)
    }
}

#[cfg(test)]
mod tests {
    use crate::adb::{AdbError, AdbExecutor, AdbOutput, REVERSE_MAPPINGS};

    use super::*;

    #[derive(Default)]
    struct MonitorMockAdb {
        calls: StdMutex<Vec<Vec<String>>>,
        mapping_adds: AtomicU64,
    }

    impl AdbExecutor for MonitorMockAdb {
        fn execute(&self, args: &[String], _timeout: Duration) -> Result<AdbOutput, AdbError> {
            self.calls.lock().unwrap().push(args.to_vec());
            if args.iter().any(|argument| argument == "get-state") {
                return Ok(AdbOutput::success("device\n"));
            }
            if args.iter().any(|argument| argument == "--list") {
                if self.mapping_adds.load(Ordering::Relaxed) >= REVERSE_MAPPINGS.len() as u64 {
                    let mappings = REVERSE_MAPPINGS
                        .iter()
                        .map(|mapping| {
                            format!("UsbFfs tcp:{} tcp:{}\n", mapping.remote, mapping.local)
                        })
                        .collect::<String>();
                    return Ok(AdbOutput::success(mappings));
                }
                return Ok(AdbOutput::success(""));
            }
            if args.get(1).is_some_and(|argument| argument == "reverse")
                && !args.iter().any(|argument| argument == "--remove")
            {
                self.mapping_adds.fetch_add(1, Ordering::Relaxed);
            }
            Ok(AdbOutput::success(""))
        }
    }

    #[test]
    fn status_store_round_trip() {
        let directory = tempfile::tempdir().unwrap();
        let store = StateStore::new(directory.path().join("status.json"));
        let snapshot = StateSnapshot {
            state: HostState::Degraded,
            session_id: Some("0011".into()),
            missed_heartbeats: 3,
            reason: Some("test".into()),
        };
        store.write(&snapshot, None).unwrap();
        assert_eq!(store.read().unwrap().lifecycle.state, HostState::Degraded);
    }

    #[test]
    fn operation_lock_serializes_mutations() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("operation.lock");
        let first = OperationGuard::acquire(&path).unwrap();
        assert_eq!(
            OperationGuard::acquire(&path).err().unwrap().kind(),
            io::ErrorKind::AlreadyExists
        );
        drop(first);
        OperationGuard::acquire(path).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn admin_endpoint_requires_token_and_returns_live_status() {
        let directory = tempfile::tempdir().unwrap();
        let paths = AppPaths::discover(Some(directory.path().to_owned())).unwrap();
        fs::write(&paths.admin_token, "correct").unwrap();
        let session = SessionId([0x55; 16]);
        let control = ControlServer::new(ControlConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            session_id: session,
        })
        .unwrap();
        let server = AdminServer::new(
            paths.clone(),
            "correct".into(),
            control.command_handle(),
            AdbHealthMonitor::default(),
            Arc::new(Notify::new()),
        );
        let task = tokio::spawn(server.serve());
        wait_for_unix_socket(&paths.admin_socket).await;

        let mut stream = UnixStream::connect(&paths.admin_socket).await.unwrap();
        write_admin_message(
            &mut stream,
            &AdminRequest {
                token: "wrong".into(),
                command: "status".into(),
            },
        )
        .await
        .unwrap();
        let response: AdminResponse = read_admin_message(&mut stream).await.unwrap();
        assert!(!response.ok);
        assert!(!response.repairs_suppressed);

        let response = admin_command(&paths, "status", Duration::from_secs(1))
            .await
            .unwrap();
        assert!(response.ok);
        assert_eq!(response.status.unwrap().state, HostState::Preparing);
        task.abort();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn admin_lane_bounds_idle_connections_and_releases_permits() {
        let directory = tempfile::tempdir().unwrap();
        let paths = AppPaths::discover(Some(directory.path().to_owned())).unwrap();
        fs::write(&paths.admin_token, "bounded").unwrap();
        let session = SessionId([0x66; 16]);
        let control = ControlServer::new(ControlConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            session_id: session,
        })
        .unwrap();
        let server = AdminServer::new(
            paths.clone(),
            "bounded".into(),
            control.command_handle(),
            AdbHealthMonitor::default(),
            Arc::new(Notify::new()),
        )
        .with_limits(1, Duration::from_millis(100));
        let task = tokio::spawn(server.serve());
        wait_for_unix_socket(&paths.admin_socket).await;

        let stalled = UnixStream::connect(&paths.admin_socket).await.unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        let mut rejected = UnixStream::connect(&paths.admin_socket).await.unwrap();
        let rejected_read = time::timeout(Duration::from_millis(50), rejected.read_u8())
            .await
            .expect("saturated connection should be closed promptly");
        assert!(rejected_read.is_err());

        tokio::time::sleep(Duration::from_millis(110)).await;
        let response = admin_command(&paths, "status", Duration::from_secs(1))
            .await
            .unwrap();
        assert!(response.ok);

        drop(stalled);
        task.abort();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn graceful_shutdown_notifies_only_after_stopped_state() {
        let directory = tempfile::tempdir().unwrap();
        let paths = AppPaths::discover(Some(directory.path().to_owned())).unwrap();
        let token = SessionId([0x69; 16]).to_string();
        fs::write(&paths.admin_token, &token).unwrap();
        let control = ControlServer::new(ControlConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            session_id: SessionId([0x6a; 16]),
        })
        .unwrap();
        let state = control.state();
        let shutdown = Arc::new(Notify::new());
        let server = AdminServer::new(
            paths.clone(),
            token,
            control.command_handle(),
            AdbHealthMonitor::default(),
            shutdown.clone(),
        );
        let task = tokio::spawn(server.serve());
        wait_for_unix_socket(&paths.admin_socket).await;

        let rejected = admin_command(&paths, "shutdown", Duration::from_secs(1))
            .await
            .unwrap();
        assert!(!rejected.ok);
        assert!(
            time::timeout(Duration::from_millis(20), shutdown.notified())
                .await
                .is_err()
        );

        {
            let mut state = state.lock().await;
            state.begin_stop().unwrap();
            state.stopped().unwrap();
        }
        let accepted = admin_command(&paths, "shutdown", Duration::from_secs(1))
            .await
            .unwrap();
        assert!(accepted.ok);
        time::timeout(Duration::from_secs(1), shutdown.notified())
            .await
            .unwrap();
        task.abort();
    }

    #[cfg(target_os = "windows")]
    #[tokio::test]
    async fn windows_named_pipe_survives_concurrent_capacity() {
        let directory = tempfile::tempdir().unwrap();
        let paths = AppPaths::discover(Some(directory.path().to_owned())).unwrap();
        let token = SessionId([0x67; 16]).to_string();
        fs::write(&paths.admin_token, &token).unwrap();
        let control = ControlServer::new(ControlConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            session_id: SessionId([0x68; 16]),
        })
        .unwrap();
        let server = AdminServer::new(
            paths.clone(),
            token,
            control.command_handle(),
            AdbHealthMonitor::default(),
            Arc::new(Notify::new()),
        );
        let server_task = tokio::spawn(server.serve());

        time::timeout(Duration::from_secs(2), async {
            loop {
                if admin_command(&paths, "status", Duration::from_millis(250))
                    .await
                    .is_ok_and(|response| response.ok)
                {
                    break;
                }
            }
        })
        .await
        .unwrap();

        let mut clients = Vec::new();
        for _ in 0..(MAX_ADMIN_CONNECTIONS * 2) {
            let paths = paths.clone();
            clients.push(tokio::spawn(async move {
                admin_command(&paths, "status", Duration::from_secs(3)).await
            }));
        }
        for client in clients {
            assert!(client.await.unwrap().unwrap().ok);
        }
        assert!(!server_task.is_finished());
        server_task.abort();
    }

    #[cfg(unix)]
    async fn wait_for_unix_socket(path: &Path) {
        time::timeout(Duration::from_secs(1), async {
            while !path.exists() {
                time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }

    #[test]
    fn invalid_persisted_admin_token_is_replaced() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("admin.token");
        fs::write(&path, "predictable").unwrap();
        let token = load_or_create_admin_token(&path).unwrap();
        assert_ne!(token, "predictable");
        assert!(token
            .parse::<SessionId>()
            .is_ok_and(|session| session != SessionId::ZERO));
    }

    #[test]
    fn token_comparison_handles_length_and_content_mismatches() {
        assert!(constant_time_eq(b"same", b"same"));
        assert!(!constant_time_eq(b"same", b"samf"));
        assert!(!constant_time_eq(b"same", b"same-longer"));
    }

    #[test]
    fn daemon_identity_rejects_pid_reuse_and_wrong_image_or_session() {
        let session = SessionId([0x44; 16]).to_string();
        let identity = DaemonIdentity {
            role: "gnirehtet-vd-daemon".into(),
            pid: 42,
            creation_time_100ns: 100,
            session_id: session,
            executable_path: r"C:\Program Files\Gnirehtet\gnirehtet-vd.exe".into(),
        };
        let valid = WindowsProcessSnapshot {
            active: true,
            creation_time_100ns: 100,
            executable_path: r"c:\program files\gnirehtet\GNIREHTET-VD.EXE".into(),
        };
        assert!(daemon_identity_matches(&identity, &valid));

        let mut reused = valid.clone();
        reused.creation_time_100ns += 1;
        assert!(!daemon_identity_matches(&identity, &reused));
        let mut wrong_image = valid.clone();
        wrong_image.executable_path = r"C:\Other\gnirehtet-vd.exe".into();
        assert!(!daemon_identity_matches(&identity, &wrong_image));
        let mut zero_session = identity.clone();
        zero_session.session_id = SessionId::ZERO.to_string();
        assert!(!daemon_identity_matches(&zero_session, &valid));
    }

    #[test]
    fn named_pipe_dacl_grants_only_the_current_user_sid() {
        assert_eq!(
            user_only_pipe_sddl("S-1-5-21-1-2-3-1001"),
            "D:P(A;;GA;;;S-1-5-21-1-2-3-1001)"
        );
    }

    #[test]
    fn adb_health_backoff_caps_at_one_second() {
        let maximum = Duration::from_secs(1);
        let first = next_backoff(Duration::from_millis(250), maximum);
        let second = next_backoff(first, maximum);
        let third = next_backoff(second, maximum);
        assert_eq!(first, Duration::from_millis(500));
        assert_eq!(second, maximum);
        assert_eq!(third, maximum);
    }

    #[tokio::test]
    async fn adb_monitor_repairs_all_three_lanes_and_counts_generation() {
        let directory = tempfile::tempdir().unwrap();
        let paths = AppPaths::discover(Some(directory.path().to_owned())).unwrap();
        let diagnostics = Diagnostics::open(&paths.logs).unwrap();
        let store = StateStore::new(&paths.status);
        let session = SessionId([0x77; 16]);
        let control = ControlServer::new(ControlConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            session_id: session,
        })
        .unwrap();
        control
            .state()
            .lock()
            .await
            .peer_started(session, std::time::Instant::now())
            .unwrap();
        let executor = Arc::new(MonitorMockAdb::default());
        let adb = AdbController::with_timing(
            executor.clone(),
            Duration::from_millis(20),
            Duration::from_millis(20),
            Duration::ZERO,
        );
        let monitor = AdbHealthMonitor::default();
        let task = tokio::spawn(monitor.clone().run(
            adb,
            control.command_handle(),
            store,
            diagnostics,
        ));

        time::timeout(Duration::from_secs(1), async {
            while monitor.snapshot().reconnect_generation == 0 {
                time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        monitor.suppress_repairs().await;
        task.await.unwrap();

        assert_eq!(monitor.snapshot().reconnect_generation, 1);
        assert!(monitor.snapshot().mappings_healthy);
        let calls = executor.calls.lock().unwrap();
        for mapping in REVERSE_MAPPINGS {
            assert!(calls.iter().any(|args| {
                args.iter()
                    .any(|argument| argument == &format!("tcp:{}", mapping.remote))
                    && !args.iter().any(|argument| argument == "--remove")
            }));
        }
    }

    #[tokio::test]
    async fn adb_monitor_never_repairs_after_explicit_suppression() {
        let directory = tempfile::tempdir().unwrap();
        let paths = AppPaths::discover(Some(directory.path().to_owned())).unwrap();
        let diagnostics = Diagnostics::open(&paths.logs).unwrap();
        let store = StateStore::new(&paths.status);
        let session = SessionId([0x78; 16]);
        let control = ControlServer::new(ControlConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            session_id: session,
        })
        .unwrap();
        control
            .state()
            .lock()
            .await
            .peer_started(session, std::time::Instant::now())
            .unwrap();
        let executor = Arc::new(MonitorMockAdb::default());
        let adb = AdbController::new(executor.clone());
        let monitor = AdbHealthMonitor::default();
        monitor.suppress_repairs().await;
        monitor
            .clone()
            .run(adb, control.command_handle(), store, diagnostics)
            .await;

        assert!(executor.calls.lock().unwrap().is_empty());
        assert!(monitor.snapshot().repair_suppressed);
    }
}
