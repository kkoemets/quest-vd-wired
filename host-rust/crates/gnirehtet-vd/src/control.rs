use std::{
    net::{Ipv4Addr, SocketAddr},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, RwLock,
    },
    time::{Duration, Instant},
};

use serde_json::json;
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{mpsc, oneshot, Mutex, Semaphore},
    time,
};

const CONTROL_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(3);
const CONTROL_FRAME_TIMEOUT: Duration = Duration::from_secs(4);

use crate::{
    diagnostics::{Diagnostics, LatencyHistogram, LatencyHistogramSnapshot},
    protocol::{AndroidMetricsV1, Frame, MessageType, ProtocolError, SessionId, VERSION},
    state::{HostState, StateMachine, StateSnapshot, TransitionError, HEARTBEAT_INTERVAL},
};

pub type StateObserver = Arc<dyn Fn(&StateSnapshot) + Send + Sync>;
pub type SuspendObserver = Arc<dyn Fn() + Send + Sync>;

struct ControlCommand {
    deadline: Instant,
    result: oneshot::Sender<Result<(), String>>,
}

#[derive(Clone)]
pub struct ControlHandle {
    sender: mpsc::Sender<ControlCommand>,
    state: Arc<Mutex<StateMachine>>,
    observer: Arc<RwLock<Option<StateObserver>>>,
    publication: Arc<Mutex<()>>,
}

#[derive(Clone, Default)]
struct ControlMetrics(Arc<ControlMetricsInner>);

#[derive(Default)]
struct ControlMetricsInner {
    connection_generation: AtomicU64,
    heartbeats_echoed: AtomicU64,
    invalid_heartbeats: AtomicU64,
    invalid_metrics: AtomicU64,
    heartbeat_echo_service: LatencyHistogram,
    android: RwLock<Option<AndroidMetricsV1>>,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct ControlMetricsSnapshot {
    pub connection_generation: u64,
    pub reconnects: u64,
    pub heartbeats_echoed: u64,
    pub invalid_heartbeats: u64,
    #[serde(default)]
    pub invalid_metrics: u64,
    pub heartbeat_echo_service_us: LatencyHistogramSnapshot,
}

impl ControlMetrics {
    fn snapshot(&self) -> ControlMetricsSnapshot {
        let connection_generation = self.0.connection_generation.load(Ordering::Relaxed);
        ControlMetricsSnapshot {
            connection_generation,
            reconnects: connection_generation.saturating_sub(1),
            heartbeats_echoed: self.0.heartbeats_echoed.load(Ordering::Relaxed),
            invalid_heartbeats: self.0.invalid_heartbeats.load(Ordering::Relaxed),
            invalid_metrics: self.0.invalid_metrics.load(Ordering::Relaxed),
            heartbeat_echo_service_us: self.0.heartbeat_echo_service.snapshot(),
        }
    }

    fn android_snapshot(&self) -> Option<AndroidMetricsV1> {
        self.0
            .android
            .read()
            .ok()
            .and_then(|metrics| metrics.clone())
    }
}

impl ControlHandle {
    pub async fn request_stop(&self, timeout: std::time::Duration) -> Result<(), ControlError> {
        let (result_tx, result_rx) = oneshot::channel();
        let deadline = Instant::now() + timeout;
        time::timeout(
            timeout,
            self.sender.send(ControlCommand {
                deadline,
                result: result_tx,
            }),
        )
        .await
        .map_err(|_| ControlError::StopTimeout)?
        .map_err(|_| ControlError::CommandUnavailable)?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        match time::timeout(remaining, result_rx).await {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(error))) => Err(ControlError::CommandRejected(error)),
            Ok(Err(_)) => Err(ControlError::CommandUnavailable),
            Err(_) => Err(ControlError::StopTimeout),
        }
    }

    pub async fn snapshot(&self) -> StateSnapshot {
        self.state.lock().await.snapshot()
    }

    /// Records loss of the host-side carrier without stopping the Android VPN.
    /// A later authenticated heartbeat returns the lifecycle to `connected`.
    pub async fn transport_lost(&self, reason: impl Into<String>) -> StateSnapshot {
        let _publication = self.publication.lock().await;
        let mut state = self.state.lock().await;
        state.transport_lost(reason);
        let snapshot = state.snapshot();
        drop(state);
        if let Some(observer) = self
            .observer
            .read()
            .ok()
            .and_then(|observer| observer.clone())
        {
            observer(&snapshot);
        }
        snapshot
    }
}

#[derive(Clone, Debug)]
pub struct ControlConfig {
    pub bind: SocketAddr,
    pub session_id: SessionId,
}

impl ControlConfig {
    pub fn new(session_id: SessionId) -> Self {
        Self {
            bind: SocketAddr::new(Ipv4Addr::LOCALHOST.into(), crate::adb::CONTROL_PORT),
            session_id,
        }
    }
}

#[derive(Clone)]
pub struct ControlServer {
    config: ControlConfig,
    state: Arc<Mutex<StateMachine>>,
    diagnostics: Option<Diagnostics>,
    observer: Arc<RwLock<Option<StateObserver>>>,
    suspend_observer: Arc<RwLock<Option<SuspendObserver>>>,
    publication: Arc<Mutex<()>>,
    commands: Arc<Mutex<mpsc::Receiver<ControlCommand>>>,
    handle: ControlHandle,
    metrics: ControlMetrics,
}

impl ControlServer {
    pub fn new(config: ControlConfig) -> Result<Self, ControlError> {
        if !config.bind.ip().is_loopback() {
            return Err(ControlError::NonLoopbackBind(config.bind));
        }
        let mut machine = StateMachine::new();
        machine.begin_start(config.session_id)?;
        let state = Arc::new(Mutex::new(machine));
        let (command_tx, command_rx) = mpsc::channel(4);
        let observer = Arc::new(RwLock::new(None));
        let suspend_observer = Arc::new(RwLock::new(None));
        let publication = Arc::new(Mutex::new(()));
        Ok(Self {
            config,
            state: state.clone(),
            diagnostics: None,
            observer: observer.clone(),
            suspend_observer,
            commands: Arc::new(Mutex::new(command_rx)),
            handle: ControlHandle {
                sender: command_tx,
                state,
                observer,
                publication: publication.clone(),
            },
            publication,
            metrics: ControlMetrics::default(),
        })
    }

    pub fn with_diagnostics(mut self, diagnostics: Diagnostics) -> Self {
        self.diagnostics = Some(diagnostics);
        self
    }

    pub fn with_observer(self, observer: StateObserver) -> Self {
        if let Ok(mut slot) = self.observer.write() {
            *slot = Some(observer);
        }
        self
    }

    pub fn with_suspend_observer(self, observer: SuspendObserver) -> Self {
        if let Ok(mut slot) = self.suspend_observer.write() {
            *slot = Some(observer);
        }
        self
    }

    pub fn state(&self) -> Arc<Mutex<StateMachine>> {
        self.state.clone()
    }

    pub fn command_handle(&self) -> ControlHandle {
        self.handle.clone()
    }

    pub fn metrics(&self) -> ControlMetricsSnapshot {
        self.metrics.snapshot()
    }

    pub fn android_metrics(&self) -> Option<AndroidMetricsV1> {
        self.metrics.android_snapshot()
    }

    pub async fn serve(self) -> Result<(), ControlError> {
        let listener = TcpListener::bind(self.config.bind).await?;
        self.serve_on(listener).await
    }

    pub async fn serve_on(self, listener: TcpListener) -> Result<(), ControlError> {
        let address = listener.local_addr()?;
        if !address.ip().is_loopback() {
            return Err(ControlError::NonLoopbackBind(address));
        }
        self.publish().await;
        let connection = Arc::new(Semaphore::new(1));
        loop {
            let (stream, _) = listener.accept().await?;
            let permit = match connection.clone().try_acquire_owned() {
                Ok(permit) => permit,
                Err(_) => continue,
            };
            let server = self.clone();
            tokio::spawn(async move {
                let result = server.handle(stream).await;
                drop(permit);
                if let Err(error) = result {
                    if let Some(diagnostics) = &server.diagnostics {
                        let _ = diagnostics.record(
                            "control_connection_error",
                            json!({"category": error.category()}),
                        );
                    }
                }
            });
        }
    }

    async fn handle(&self, stream: TcpStream) -> Result<(), ControlError> {
        stream.set_nodelay(true)?;
        let (mut reader, mut writer) = stream.into_split();
        let first =
            read_frame_with_timeout(&mut reader, CONTROL_HANDSHAKE_TIMEOUT, "initial HELLO")
                .await?;
        self.validate_session(&first)?;
        if first.message_type != MessageType::Hello {
            return Err(ControlError::ExpectedHello);
        }
        if let Ok(mut latest) = self.metrics.0.android.write() {
            *latest = None;
        }
        self.metrics
            .0
            .connection_generation
            .fetch_add(1, Ordering::Relaxed);
        Frame::new(
            MessageType::HelloAck,
            self.config.session_id,
            serde_json::to_vec(&json!({
                "protocol": VERSION,
                "capabilities": ["heartbeat", "status", "explicit_stop", "explicit_suspend"]
            }))?,
        )
        .write_to(&mut writer)
        .await?;

        let (frame_tx, mut frame_rx) = mpsc::channel::<Result<Frame, ControlError>>(16);
        let reader_task = tokio::spawn(async move {
            loop {
                match read_frame_with_timeout(
                    &mut reader,
                    CONTROL_FRAME_TIMEOUT,
                    "authenticated control frame",
                )
                .await
                {
                    Ok(frame) => {
                        if frame_tx.send(Ok(frame)).await.is_err() {
                            return;
                        }
                    }
                    Err(error) => {
                        let _ = frame_tx.send(Err(error)).await;
                        return;
                    }
                }
            }
        });

        let mut heartbeat = time::interval(HEARTBEAT_INTERVAL);
        heartbeat.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
        let mut pending_stop = PendingStop::default();
        let result = loop {
            let stop_deadline = pending_stop
                .deadline()
                .unwrap_or_else(|| Instant::now() + Duration::from_secs(24 * 60 * 60));
            tokio::select! {
                _ = heartbeat.tick() => {
                    let before = self.state.lock().await.state();
                    self.state.lock().await.tick(Instant::now());
                    let after = self.state.lock().await.state();
                    if before != after {
                        self.publish().await;
                    }
                }
                frame = frame_rx.recv() => {
                    let Some(frame) = frame else { break Ok(()); };
                    let frame = match frame {
                        Ok(frame) => frame,
                        Err(ControlError::Protocol(ProtocolError::Io(error))) if matches!(
                            error.kind(),
                            std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::ConnectionReset
                        ) => break Ok(()),
                        Err(error) => break Err(error),
                    };
                    self.validate_session(&frame)?;
                    match frame.message_type {
                        MessageType::Started => {
                            let mut state = self.state.lock().await;
                            // STARTED may already be queued when an explicit stop
                            // is issued during preparation. It must not undo the
                            // stopping transaction or tear down the control lane.
                            if state.state() != HostState::Stopping {
                                state.peer_started(frame.session_id, Instant::now())?;
                            }
                            drop(state);
                            self.publish().await;
                        }
                        MessageType::Heartbeat => {
                            let received_at = Instant::now();
                            if frame.payload.len() != 16 {
                                self.metrics.0.invalid_heartbeats.fetch_add(1, Ordering::Relaxed);
                                break Err(ControlError::InvalidHeartbeatPayload(frame.payload.len()));
                            }
                            let before = self.state.lock().await.state();
                            self.state.lock().await.heartbeat(frame.session_id, Instant::now())?;
                            let after = self.state.lock().await.state();
                            Frame::new(MessageType::Heartbeat, frame.session_id, frame.payload)
                                .write_to(&mut writer).await?;
                            self.metrics.0.heartbeats_echoed.fetch_add(1, Ordering::Relaxed);
                            self.metrics
                                .0
                                .heartbeat_echo_service
                                .record(received_at.elapsed());
                            if before != after {
                                self.publish().await;
                            }
                        }
                        MessageType::Stopped => {
                            let mut state = self.state.lock().await;
                            if !pending_stop.is_pending() || state.state() != HostState::Stopping {
                                break Err(ControlError::UnsolicitedStopped);
                            }
                            state.stopped()?;
                            drop(state);
                            pending_stop.complete();
                            self.publish().await;
                        }
                        MessageType::Status => {
                            let snapshot = self.state.lock().await.snapshot();
                            Frame::new(
                                MessageType::Status,
                                frame.session_id,
                                serde_json::to_vec(&snapshot)?,
                            ).write_to(&mut writer).await?;
                        }
                        MessageType::Metrics => {
                            match AndroidMetricsV1::decode(&frame.payload) {
                                Ok(metrics) => {
                                    if let Ok(mut latest) = self.metrics.0.android.write() {
                                        *latest = Some(metrics);
                                    }
                                }
                                Err(error) => {
                                    self.metrics.0.invalid_metrics.fetch_add(1, Ordering::Relaxed);
                                    if let Some(diagnostics) = &self.diagnostics {
                                        let _ = diagnostics.record(
                                            "android_metrics_malformed",
                                            json!({"category": metrics_error_category(&error)}),
                                        );
                                    }
                                }
                            }
                        }
                        MessageType::Suspend => {
                            if !frame.payload.is_empty() {
                                break Err(ControlError::InvalidSuspendPayload(frame.payload.len()));
                            }
                            self.state.lock().await.peer_suspended(frame.session_id)?;
                            if let Some(observer) = self
                                .suspend_observer
                                .read()
                                .ok()
                                .and_then(|observer| observer.clone())
                            {
                                observer();
                            }
                            self.publish().await;
                            Frame::new(MessageType::Suspended, frame.session_id, Vec::new())
                                .write_to(&mut writer).await?;
                            break Ok(());
                        }
                        MessageType::Error => {
                            self.state.lock().await.fail("Android peer reported an error");
                            self.publish().await;
                        }
                        MessageType::Hello
                        | MessageType::HelloAck
                        | MessageType::Suspended
                        | MessageType::Stop => break Err(ControlError::UnexpectedMessage(frame.message_type)),
                    }
                }
                command = async { self.commands.lock().await.recv().await } => {
                    let Some(command) = command else { continue; };
                    if Instant::now() > command.deadline {
                        let _ = command.result.send(Err("stop request expired".into()));
                        continue;
                    }
                    if pending_stop.is_pending() {
                        let _ = command.result.send(Err("stop is already pending".into()));
                        continue;
                    }
                    {
                        let mut state = self.state.lock().await;
                        if let Err(error) = state.begin_stop() {
                            let _ = command.result.send(Err(error.to_string()));
                            continue;
                        }
                    }
                    pending_stop.set(command.result, command.deadline);
                    self.publish().await;
                    Frame::new(MessageType::Stop, self.config.session_id, Vec::new())
                        .write_to(&mut writer).await?;
                }
                _ = time::sleep_until(time::Instant::from_std(stop_deadline)), if pending_stop.is_pending() => {
                    pending_stop.fail("timed out waiting for Android STOPPED");
                    self.state
                        .lock()
                        .await
                        .fail("explicit stop timed out; VPN state is unverified");
                    self.publish().await;
                }
            }
        };
        reader_task.abort();
        let _ = writer.shutdown().await;
        if let Ok(mut latest) = self.metrics.0.android.write() {
            *latest = None;
        }
        {
            let mut state = self.state.lock().await;
            if pending_stop.is_pending() {
                pending_stop.fail("control connection closed before STOPPED");
                state.fail(
                    "control transport was lost during explicit stop; VPN state is unverified",
                );
            } else {
                state.transport_lost("control transport lost; VPN remains active");
            }
        }
        self.publish().await;
        result
    }

    fn validate_session(&self, frame: &Frame) -> Result<(), ControlError> {
        if frame.session_id == self.config.session_id {
            Ok(())
        } else {
            Err(ControlError::StaleSession(frame.session_id))
        }
    }

    async fn publish(&self) {
        // Serialize publication and re-read authoritative state so an older
        // Connected snapshot can never arrive after a newer degradation.
        let _publication = self.publication.lock().await;
        let snapshot = self.state.lock().await.snapshot();
        if let Some(observer) = self
            .observer
            .read()
            .ok()
            .and_then(|observer| observer.clone())
        {
            observer(&snapshot);
        }
        if let Some(diagnostics) = &self.diagnostics {
            if let Ok(fields) = serde_json::to_value(&snapshot) {
                let _ = diagnostics.record("lifecycle_state", fields);
            }
        }
    }
}

fn metrics_error_category(error: &crate::protocol::MetricsError) -> &'static str {
    match error {
        crate::protocol::MetricsError::InvalidLength(_) => "invalid_length",
        crate::protocol::MetricsError::UnsupportedVersion(_) => "unsupported_version",
        crate::protocol::MetricsError::UnsupportedFlags(_) => "unsupported_flags",
    }
}

#[derive(Default)]
struct PendingStop {
    sender: Option<oneshot::Sender<Result<(), String>>>,
    deadline: Option<Instant>,
}

impl PendingStop {
    fn is_pending(&self) -> bool {
        self.sender.is_some()
    }

    fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    fn set(&mut self, sender: oneshot::Sender<Result<(), String>>, deadline: Instant) {
        self.sender = Some(sender);
        self.deadline = Some(deadline);
    }

    fn complete(&mut self) {
        self.deadline = None;
        if let Some(sender) = self.sender.take() {
            let _ = sender.send(Ok(()));
        }
    }

    fn fail(&mut self, reason: &str) {
        self.deadline = None;
        if let Some(sender) = self.sender.take() {
            let _ = sender.send(Err(reason.into()));
        }
    }
}

impl Drop for PendingStop {
    fn drop(&mut self) {
        self.fail("control connection closed before STOPPED");
    }
}

async fn read_frame_with_timeout<R>(
    reader: &mut R,
    timeout: Duration,
    phase: &'static str,
) -> Result<Frame, ControlError>
where
    R: AsyncRead + Unpin,
{
    time::timeout(timeout, Frame::read_from(reader))
        .await
        .map_err(|_| ControlError::ReadTimeout(phase))?
        .map_err(ControlError::Protocol)
}

#[derive(Debug, Error)]
pub enum ControlError {
    #[error("control listener must be loopback, got {0}")]
    NonLoopbackBind(SocketAddr),
    #[error("first control message was not HELLO")]
    ExpectedHello,
    #[error("control frame belongs to stale session {0}")]
    StaleSession(SessionId),
    #[error("unexpected control message {0:?}")]
    UnexpectedMessage(MessageType),
    #[error("received STOPPED without a matching host STOP transaction")]
    UnsolicitedStopped,
    #[error("timed out reading {0}")]
    ReadTimeout(&'static str),
    #[error("HEARTBEAT payload must be 16 bytes, got {0}")]
    InvalidHeartbeatPayload(usize),
    #[error("SUSPEND payload must be empty, got {0} bytes")]
    InvalidSuspendPayload(usize),
    #[error("control command lane is unavailable")]
    CommandUnavailable,
    #[error("control stop was rejected: {0}")]
    CommandRejected(String),
    #[error("timed out waiting for Android STOPPED")]
    StopTimeout,
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error(transparent)]
    Transition(#[from] TransitionError),
    #[error("control JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("control I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

impl ControlError {
    fn category(&self) -> &'static str {
        match self {
            Self::NonLoopbackBind(_) => "non_loopback_bind",
            Self::ExpectedHello => "expected_hello",
            Self::StaleSession(_) => "stale_session",
            Self::UnexpectedMessage(_) => "unexpected_message",
            Self::UnsolicitedStopped => "unsolicited_stopped",
            Self::ReadTimeout(_) => "read_timeout",
            Self::InvalidHeartbeatPayload(_) => "invalid_heartbeat_payload",
            Self::InvalidSuspendPayload(_) => "invalid_suspend_payload",
            Self::CommandUnavailable => "command_unavailable",
            Self::CommandRejected(_) => "command_rejected",
            Self::StopTimeout => "stop_timeout",
            Self::Protocol(_) => "protocol",
            Self::Transition(_) => "state_transition",
            Self::Json(_) => "json",
            Self::Io(_) => "io",
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;

    use tokio::{io::AsyncReadExt, net::TcpStream};

    use super::*;

    #[test]
    fn rejects_non_loopback_bind() {
        let config = ControlConfig {
            bind: "0.0.0.0:31416".parse().unwrap(),
            session_id: SessionId([1; 16]),
        };
        assert!(matches!(
            ControlServer::new(config),
            Err(ControlError::NonLoopbackBind(_))
        ));
    }

    #[tokio::test]
    async fn carrier_loss_notifies_the_shared_lifecycle_observer() {
        let session = SessionId([0x31; 16]);
        let observed = Arc::new(StdMutex::new(Vec::new()));
        let observer_values = observed.clone();
        let server = ControlServer::new(ControlConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            session_id: session,
        })
        .unwrap()
        .with_observer(Arc::new(move |snapshot| {
            observer_values.lock().unwrap().push(snapshot.state);
        }));
        server
            .state()
            .lock()
            .await
            .peer_started(session, Instant::now())
            .unwrap();

        let snapshot = server
            .command_handle()
            .transport_lost("USB carrier unavailable")
            .await;
        assert_eq!(snapshot.state, HostState::Degraded);
        assert_eq!(*observed.lock().unwrap(), vec![HostState::Degraded]);
    }

    #[tokio::test]
    async fn delayed_publication_rereads_authoritative_lifecycle_state() {
        let session = SessionId([0x32; 16]);
        let observed = Arc::new(StdMutex::new(Vec::new()));
        let observer_values = observed.clone();
        let server = ControlServer::new(ControlConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            session_id: session,
        })
        .unwrap()
        .with_observer(Arc::new(move |snapshot| {
            observer_values.lock().unwrap().push(snapshot.state);
        }));
        server
            .state()
            .lock()
            .await
            .peer_started(session, Instant::now())
            .unwrap();

        let publication = server.publication.clone().lock_owned().await;
        let delayed_server = server.clone();
        let delayed = tokio::spawn(async move { delayed_server.publish().await });
        tokio::task::yield_now().await;
        server
            .state()
            .lock()
            .await
            .transport_lost("newer carrier loss");
        drop(publication);
        delayed.await.unwrap();

        assert_eq!(*observed.lock().unwrap(), vec![HostState::Degraded]);
    }

    #[tokio::test]
    async fn hello_started_and_heartbeat_connect() {
        let session = SessionId([9; 16]);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bind = listener.local_addr().unwrap();
        let server = ControlServer::new(ControlConfig {
            bind,
            session_id: session,
        })
        .unwrap();
        let state = server.state();
        let task = tokio::spawn(server.serve_on(listener));

        let mut client = TcpStream::connect(bind).await.unwrap();
        Frame::new(MessageType::Hello, session, Vec::new())
            .write_to(&mut client)
            .await
            .unwrap();
        assert_eq!(
            Frame::read_from(&mut client).await.unwrap().message_type,
            MessageType::HelloAck
        );
        Frame::new(MessageType::Started, session, Vec::new())
            .write_to(&mut client)
            .await
            .unwrap();
        let heartbeat_payload = vec![0x5a; 16];
        Frame::new(MessageType::Heartbeat, session, heartbeat_payload.clone())
            .write_to(&mut client)
            .await
            .unwrap();
        let echo = Frame::read_from(&mut client).await.unwrap();
        assert_eq!(echo.message_type, MessageType::Heartbeat);
        assert_eq!(echo.payload, heartbeat_payload);
        assert_eq!(state.lock().await.state(), HostState::Connected);
        task.abort();
    }

    #[tokio::test]
    async fn metrics_are_stored_and_malformed_metrics_do_not_close_control() {
        let session = SessionId([0x4d; 16]);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bind = listener.local_addr().unwrap();
        let server = ControlServer::new(ControlConfig {
            bind,
            session_id: session,
        })
        .unwrap();
        let observed = server.clone();
        let task = tokio::spawn(server.serve_on(listener));

        let mut client = TcpStream::connect(bind).await.unwrap();
        Frame::new(MessageType::Hello, session, Vec::new())
            .write_to(&mut client)
            .await
            .unwrap();
        let acknowledgement = Frame::read_from(&mut client).await.unwrap();
        assert!(!String::from_utf8(acknowledgement.payload)
            .unwrap()
            .contains("metrics_v1"));
        Frame::new(MessageType::Started, session, Vec::new())
            .write_to(&mut client)
            .await
            .unwrap();

        Frame::new(MessageType::Metrics, session, vec![0; 3])
            .write_to(&mut client)
            .await
            .unwrap();
        let metrics = AndroidMetricsV1 {
            tx_packets: 11,
            tx_bytes: 12,
            rx_packets: 13,
            rx_bytes: 14,
            control_rtt_samples: 15,
            control_rtt_p99_us: 16,
            control_rtt_max_us: 17,
        };
        Frame::new(MessageType::Metrics, session, metrics.encode().to_vec())
            .write_to(&mut client)
            .await
            .unwrap();
        let heartbeat_payload = vec![0x4d; 16];
        Frame::new(MessageType::Heartbeat, session, heartbeat_payload.clone())
            .write_to(&mut client)
            .await
            .unwrap();
        let echo = Frame::read_from(&mut client).await.unwrap();
        assert_eq!(echo.message_type, MessageType::Heartbeat);
        assert_eq!(echo.payload, heartbeat_payload);

        time::timeout(Duration::from_secs(1), async {
            while observed.android_metrics().is_none() {
                time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(observed.android_metrics(), Some(metrics));
        assert_eq!(observed.metrics().invalid_metrics, 1);
        drop(client);
        time::timeout(Duration::from_secs(1), async {
            while observed.android_metrics().is_some() {
                time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        task.abort();
    }

    #[tokio::test]
    async fn authenticated_suspend_is_acknowledged_and_notifies_the_flow_gate() {
        let session = SessionId([0x3a; 16]);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bind = listener.local_addr().unwrap();
        let notifications = Arc::new(AtomicU64::new(0));
        let observed_notifications = notifications.clone();
        let server = ControlServer::new(ControlConfig {
            bind,
            session_id: session,
        })
        .unwrap()
        .with_suspend_observer(Arc::new(move || {
            observed_notifications.fetch_add(1, Ordering::Relaxed);
        }));
        let state = server.state();
        let task = tokio::spawn(server.serve_on(listener));

        let mut client = TcpStream::connect(bind).await.unwrap();
        Frame::new(MessageType::Hello, session, Vec::new())
            .write_to(&mut client)
            .await
            .unwrap();
        Frame::read_from(&mut client).await.unwrap();
        Frame::new(MessageType::Started, session, Vec::new())
            .write_to(&mut client)
            .await
            .unwrap();
        Frame::new(MessageType::Suspend, session, Vec::new())
            .write_to(&mut client)
            .await
            .unwrap();

        let acknowledgement = Frame::read_from(&mut client).await.unwrap();
        assert_eq!(acknowledgement.message_type, MessageType::Suspended);
        assert!(acknowledgement.payload.is_empty());
        let snapshot = state.lock().await.snapshot();
        assert_eq!(snapshot.state, HostState::Degraded);
        assert_eq!(
            snapshot.reason.as_deref(),
            Some(crate::state::HEADSET_SUSPENDED_REASON)
        );
        assert_eq!(notifications.load(Ordering::Relaxed), 1);
        task.abort();
    }

    #[tokio::test]
    async fn malformed_suspend_does_not_notify_the_flow_gate() {
        let session = SessionId([0x3b; 16]);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bind = listener.local_addr().unwrap();
        let notifications = Arc::new(AtomicU64::new(0));
        let observed_notifications = notifications.clone();
        let server = ControlServer::new(ControlConfig {
            bind,
            session_id: session,
        })
        .unwrap()
        .with_suspend_observer(Arc::new(move || {
            observed_notifications.fetch_add(1, Ordering::Relaxed);
        }));
        let task = tokio::spawn(server.serve_on(listener));
        let mut client = TcpStream::connect(bind).await.unwrap();
        Frame::new(MessageType::Hello, session, Vec::new())
            .write_to(&mut client)
            .await
            .unwrap();
        Frame::read_from(&mut client).await.unwrap();
        Frame::new(MessageType::Started, session, Vec::new())
            .write_to(&mut client)
            .await
            .unwrap();
        Frame::new(MessageType::Suspend, session, vec![1])
            .write_to(&mut client)
            .await
            .unwrap();

        let closed = time::timeout(Duration::from_secs(1), client.read_u8())
            .await
            .expect("malformed suspend left the control connection open");
        assert!(closed.is_err());
        assert_eq!(notifications.load(Ordering::Relaxed), 0);
        task.abort();
    }

    #[tokio::test]
    async fn explicit_stop_waits_for_stopped_acknowledgement() {
        let session = SessionId([0x44; 16]);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bind = listener.local_addr().unwrap();
        let server = ControlServer::new(ControlConfig {
            bind,
            session_id: session,
        })
        .unwrap();
        let handle = server.command_handle();
        let task = tokio::spawn(server.serve_on(listener));
        let mut client = TcpStream::connect(bind).await.unwrap();
        Frame::new(MessageType::Hello, session, Vec::new())
            .write_to(&mut client)
            .await
            .unwrap();
        Frame::read_from(&mut client).await.unwrap();
        Frame::new(MessageType::Started, session, Vec::new())
            .write_to(&mut client)
            .await
            .unwrap();

        let stop = tokio::spawn({
            let handle = handle.clone();
            async move { handle.request_stop(std::time::Duration::from_secs(1)).await }
        });
        let request = Frame::read_from(&mut client).await.unwrap();
        assert_eq!(request.message_type, MessageType::Stop);
        assert!(!stop.is_finished());
        Frame::new(MessageType::Stopped, session, Vec::new())
            .write_to(&mut client)
            .await
            .unwrap();
        stop.await.unwrap().unwrap();
        assert_eq!(handle.snapshot().await.state, HostState::Stopped);
        task.abort();
    }

    #[tokio::test]
    async fn unsolicited_stopped_cannot_mark_the_host_stopped() {
        let session = SessionId([0x45; 16]);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bind = listener.local_addr().unwrap();
        let server = ControlServer::new(ControlConfig {
            bind,
            session_id: session,
        })
        .unwrap();
        let handle = server.command_handle();
        let task = tokio::spawn(server.serve_on(listener));
        let mut client = TcpStream::connect(bind).await.unwrap();
        Frame::new(MessageType::Hello, session, Vec::new())
            .write_to(&mut client)
            .await
            .unwrap();
        Frame::read_from(&mut client).await.unwrap();
        Frame::new(MessageType::Started, session, Vec::new())
            .write_to(&mut client)
            .await
            .unwrap();
        Frame::new(MessageType::Stopped, session, Vec::new())
            .write_to(&mut client)
            .await
            .unwrap();

        time::timeout(Duration::from_secs(1), async {
            loop {
                if handle.snapshot().await.state == HostState::Degraded {
                    break;
                }
                time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert_ne!(handle.snapshot().await.state, HostState::Stopped);
        task.abort();
    }

    #[tokio::test]
    async fn transport_loss_during_stop_does_not_leave_stopping_state() {
        let session = SessionId([0x46; 16]);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bind = listener.local_addr().unwrap();
        let server = ControlServer::new(ControlConfig {
            bind,
            session_id: session,
        })
        .unwrap();
        let handle = server.command_handle();
        let task = tokio::spawn(server.serve_on(listener));
        let mut client = TcpStream::connect(bind).await.unwrap();
        Frame::new(MessageType::Hello, session, Vec::new())
            .write_to(&mut client)
            .await
            .unwrap();
        Frame::read_from(&mut client).await.unwrap();
        Frame::new(MessageType::Started, session, Vec::new())
            .write_to(&mut client)
            .await
            .unwrap();

        let stop = tokio::spawn({
            let handle = handle.clone();
            async move { handle.request_stop(Duration::from_secs(1)).await }
        });
        assert_eq!(
            Frame::read_from(&mut client).await.unwrap().message_type,
            MessageType::Stop
        );
        drop(client);
        assert!(stop.await.unwrap().is_err());
        time::timeout(Duration::from_secs(1), async {
            loop {
                if handle.snapshot().await.state != HostState::Stopping {
                    break;
                }
                time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(handle.snapshot().await.state, HostState::Error);
        task.abort();
    }

    #[tokio::test]
    async fn frame_reader_enforces_its_deadline() {
        let (_client, mut server) = tokio::io::duplex(64);
        let result =
            read_frame_with_timeout(&mut server, Duration::from_millis(10), "test frame").await;
        assert!(matches!(
            result,
            Err(ControlError::ReadTimeout("test frame"))
        ));
    }
}
