use std::{
    future::Future,
    io::Error as StdIoError,
    net::{Ipv4Addr, SocketAddr},
    pin::Pin,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex as StdMutex,
    },
    task::{Context, Poll},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;
use tokio::{
    io::{self, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpSocket, TcpStream},
    sync::{watch, Notify, Semaphore},
    time,
};

use crate::{
    diagnostics::Diagnostics,
    udp::{
        decode_endpoint, encode_endpoint, relay_fwd_udp, Endpoint, FwdUdpConfig, HevFrameError,
        UdpStats, VIRTUAL_DESKTOP_STREAMER_PORTS,
    },
};

pub const SOCKS_VERSION: u8 = 0x05;
pub const AUTH_NONE: u8 = 0x00;
pub const AUTH_UNACCEPTABLE: u8 = 0xff;
pub const CMD_CONNECT: u8 = 0x01;
pub const CMD_FWD_UDP: u8 = 0x05;
#[derive(Clone, Default)]
pub struct VirtualDesktopFlowMonitor {
    active: Arc<[AtomicUsize; VIRTUAL_DESKTOP_STREAMER_PORTS.len()]>,
    forwarding: Arc<[AtomicUsize; VIRTUAL_DESKTOP_STREAMER_PORTS.len()]>,
}

impl VirtualDesktopFlowMonitor {
    fn begin(&self, destination_port: u16) -> Option<VirtualDesktopFlowGuard> {
        let index = VIRTUAL_DESKTOP_STREAMER_PORTS
            .iter()
            .position(|port| *port == destination_port)?;
        self.active[index].fetch_add(1, Ordering::Relaxed);
        Some(VirtualDesktopFlowGuard {
            monitor: self.clone(),
            index,
            forwarded: AtomicBool::new(false),
        })
    }

    pub fn snapshot(&self) -> VirtualDesktopFlowSnapshot {
        let active_flows = std::array::from_fn(|index| self.active[index].load(Ordering::Relaxed));
        let forwarding_flows: [usize; VIRTUAL_DESKTOP_STREAMER_PORTS.len()] =
            std::array::from_fn(|index| self.forwarding[index].load(Ordering::Relaxed));
        VirtualDesktopFlowSnapshot {
            connected: forwarding_flows.iter().all(|count| *count > 0),
            active_ports: active_flows.iter().filter(|count| **count > 0).count(),
            active_flows,
        }
    }
}

struct VirtualDesktopFlowGuard {
    monitor: VirtualDesktopFlowMonitor,
    index: usize,
    forwarded: AtomicBool,
}

impl VirtualDesktopFlowGuard {
    fn mark_forwarded(&self) {
        if !self.forwarded.swap(true, Ordering::AcqRel) {
            self.monitor.forwarding[self.index].fetch_add(1, Ordering::Relaxed);
        }
    }
}

impl Drop for VirtualDesktopFlowGuard {
    fn drop(&mut self) {
        if self.forwarded.load(Ordering::Acquire) {
            self.monitor.forwarding[self.index].fetch_sub(1, Ordering::Relaxed);
        }
        self.monitor.active[self.index].fetch_sub(1, Ordering::Relaxed);
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct VirtualDesktopFlowSnapshot {
    pub connected: bool,
    pub active_ports: usize,
    pub active_flows: [usize; VIRTUAL_DESKTOP_STREAMER_PORTS.len()],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FlowEndpoint {
    Quest,
    Streamer,
}

impl FlowEndpoint {
    fn as_str(self) -> &'static str {
        match self {
            Self::Quest => "quest",
            Self::Streamer => "streamer",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FlowTerminalEvent {
    ReadEof,
    ReadError,
    WriteError,
}

impl FlowTerminalEvent {
    fn as_str(self) -> &'static str {
        match self {
            Self::ReadEof => "read_eof",
            Self::ReadError => "read_error",
            Self::WriteError => "write_error",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FlowTerminal {
    endpoint: FlowEndpoint,
    event: FlowTerminalEvent,
}

#[derive(Default)]
struct FlowObservation {
    first_terminal: StdMutex<Option<FlowTerminal>>,
}

impl FlowObservation {
    fn record(&self, endpoint: FlowEndpoint, event: FlowTerminalEvent) {
        let Ok(mut first_terminal) = self.first_terminal.lock() else {
            return;
        };
        if first_terminal.is_none() {
            *first_terminal = Some(FlowTerminal { endpoint, event });
        }
    }

    fn first_terminal(&self) -> Option<FlowTerminal> {
        self.first_terminal.lock().ok().and_then(|value| *value)
    }
}

struct ObservedIo<'a, T> {
    inner: T,
    endpoint: FlowEndpoint,
    observation: &'a FlowObservation,
    virtual_desktop_flow: Option<&'a VirtualDesktopFlowGuard>,
}

impl<'a, T> ObservedIo<'a, T> {
    fn new(
        inner: T,
        endpoint: FlowEndpoint,
        observation: &'a FlowObservation,
        virtual_desktop_flow: Option<&'a VirtualDesktopFlowGuard>,
    ) -> Self {
        Self {
            inner,
            endpoint,
            observation,
            virtual_desktop_flow,
        }
    }

    fn record_error(&self, event: FlowTerminalEvent, result: &Poll<Result<(), StdIoError>>) {
        if matches!(result, Poll::Ready(Err(_))) {
            self.observation.record(self.endpoint, event);
        }
    }
}

impl<T> AsyncRead for ObservedIo<'_, T>
where
    T: AsyncRead + Unpin,
{
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut io::ReadBuf<'_>,
    ) -> Poll<Result<(), StdIoError>> {
        let this = self.get_mut();
        let filled_before = buffer.filled().len();
        let result = Pin::new(&mut this.inner).poll_read(context, buffer);
        match &result {
            Poll::Ready(Ok(())) if buffer.filled().len() == filled_before => this
                .observation
                .record(this.endpoint, FlowTerminalEvent::ReadEof),
            Poll::Ready(Err(_)) => this
                .observation
                .record(this.endpoint, FlowTerminalEvent::ReadError),
            _ => {}
        }
        result
    }
}

impl<T> AsyncWrite for ObservedIo<'_, T>
where
    T: AsyncWrite + Unpin,
{
    fn poll_write(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<Result<usize, StdIoError>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.inner).poll_write(context, buffer);
        match result {
            Poll::Ready(Ok(written)) if written > 0 => {
                if let Some(flow) = this.virtual_desktop_flow {
                    flow.mark_forwarded();
                }
            }
            Poll::Ready(Err(_)) => this
                .observation
                .record(this.endpoint, FlowTerminalEvent::WriteError),
            _ => {}
        }
        result
    }

    fn poll_flush(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Result<(), StdIoError>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.inner).poll_flush(context);
        this.record_error(FlowTerminalEvent::WriteError, &result);
        result
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Result<(), StdIoError>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.inner).poll_shutdown(context);
        this.record_error(FlowTerminalEvent::WriteError, &result);
        result
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SocksCommand {
    Connect,
    FwdUdp,
}

/// Commands accepted by one listener. The two commands are intentionally
/// never multiplexed onto the same host acceptor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SocksCommandPolicy {
    ConnectOnly,
    FwdUdpOnly,
}

impl SocksCommandPolicy {
    fn allows(self, command: SocksCommand) -> bool {
        matches!(
            (self, command),
            (Self::ConnectOnly, SocksCommand::Connect) | (Self::FwdUdpOnly, SocksCommand::FwdUdp)
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SocksRequest {
    pub command: SocksCommand,
    pub destination: Endpoint,
}

impl SocksRequest {
    /// Pure request parser for tests and fuzzers. Authentication negotiation is
    /// intentionally parsed separately by the bounded stream handler.
    pub fn decode(input: &[u8]) -> Result<Self, SocksError> {
        if input.len() < 4 {
            return Err(SocksError::Malformed("truncated request"));
        }
        if input[0] != SOCKS_VERSION || input[2] != 0 {
            return Err(SocksError::Malformed("invalid version or reserved byte"));
        }
        let command = match input[1] {
            CMD_CONNECT => SocksCommand::Connect,
            CMD_FWD_UDP => SocksCommand::FwdUdp,
            other => return Err(SocksError::UnsupportedCommand(other)),
        };
        let (destination, consumed) = decode_endpoint(&input[3..])?;
        if consumed + 3 != input.len() {
            return Err(SocksError::Malformed("trailing request bytes"));
        }
        Ok(Self {
            command,
            destination,
        })
    }
}

#[derive(Clone, Debug)]
pub struct SocksConfig {
    pub bind: SocketAddr,
    pub command_policy: SocksCommandPolicy,
    pub max_connections: usize,
    pub handshake_timeout: Duration,
    pub connect_timeout: Duration,
    pub fwd_udp: FwdUdpConfig,
}

impl Default for SocksConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::new(Ipv4Addr::LOCALHOST.into(), crate::adb::SOCKS_PORT),
            command_policy: SocksCommandPolicy::ConnectOnly,
            max_connections: 64,
            handshake_timeout: Duration::from_secs(3),
            connect_timeout: Duration::from_secs(10),
            fwd_udp: FwdUdpConfig::default(),
        }
    }
}

#[derive(Clone)]
pub struct SocksServer {
    config: SocksConfig,
    stats: SocksStats,
    udp_stats: UdpStats,
    virtual_desktop_flows: VirtualDesktopFlowMonitor,
    streamer_candidates: StreamerCandidates,
    diagnostics: Option<Diagnostics>,
    relay_gate: Option<RelayGate>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct RelayGateState {
    enabled: bool,
    awaiting_activation: bool,
    generation: u64,
}

/// Invalidates active relay flows when the transport becomes unusable while
/// keeping the listeners alive for a fresh connection.
#[derive(Clone, Debug)]
pub struct RelayGate {
    sender: watch::Sender<RelayGateState>,
}

impl Default for RelayGate {
    fn default() -> Self {
        let (sender, _) = watch::channel(RelayGateState::default());
        Self { sender }
    }
}

impl RelayGate {
    pub fn set_enabled(&self, enabled: bool) {
        self.sender.send_if_modified(|state| {
            if state.enabled == enabled && !state.awaiting_activation {
                return false;
            }
            state.enabled = enabled;
            state.awaiting_activation = false;
            state.generation = state.generation.saturating_add(1);
            true
        });
    }

    pub fn wait_for_activation(&self) {
        self.sender.send_if_modified(|state| {
            if state.enabled || state.awaiting_activation {
                return false;
            }
            state.awaiting_activation = true;
            state.generation = state.generation.saturating_add(1);
            true
        });
    }

    pub fn invalidate_and_wait_for_activation(&self) {
        self.sender.send_if_modified(|state| {
            if !state.enabled && state.awaiting_activation {
                return false;
            }
            state.enabled = false;
            state.awaiting_activation = true;
            state.generation = state.generation.saturating_add(1);
            true
        });
    }

    #[cfg(test)]
    pub(crate) fn is_enabled(&self) -> bool {
        self.sender.borrow().enabled
    }

    #[cfg(test)]
    pub(crate) fn is_awaiting_activation(&self) -> bool {
        self.sender.borrow().awaiting_activation
    }

    #[cfg(test)]
    pub(crate) fn generation(&self) -> u64 {
        self.sender.borrow().generation
    }

    fn active_generation(&self) -> Option<u64> {
        let state = *self.sender.borrow();
        state.enabled.then_some(state.generation)
    }

    async fn wait_for_invalidation(&self, generation: u64) {
        let mut receiver = self.sender.subscribe();
        loop {
            let state = *receiver.borrow_and_update();
            if !state.enabled || state.generation != generation {
                return;
            }
            if receiver.changed().await.is_err() {
                return;
            }
        }
    }
}

impl SocksServer {
    pub fn new(config: SocksConfig) -> Result<Self, SocksError> {
        if !config.bind.ip().is_loopback() {
            return Err(SocksError::NonLoopbackBind(config.bind));
        }
        if config.max_connections == 0
            || config.fwd_udp.queue_capacity == 0
            || config.fwd_udp.association_byte_budget == 0
        {
            return Err(SocksError::Malformed("capacity must be non-zero"));
        }
        Ok(Self {
            config,
            stats: SocksStats::default(),
            udp_stats: UdpStats::default(),
            virtual_desktop_flows: VirtualDesktopFlowMonitor::default(),
            streamer_candidates: StreamerCandidates::default(),
            diagnostics: None,
            relay_gate: None,
        })
    }

    pub fn with_diagnostics(mut self, diagnostics: Diagnostics) -> Self {
        self.diagnostics = Some(diagnostics);
        self
    }

    pub fn with_relay_gate(mut self, relay_gate: RelayGate) -> Self {
        self.relay_gate = Some(relay_gate);
        self
    }

    pub fn stats(&self) -> SocksStatsSnapshot {
        let mut snapshot = self.stats.snapshot();
        snapshot.udp = self.udp_stats.snapshot();
        snapshot.virtual_desktop = self.virtual_desktop_flows.snapshot();
        snapshot
    }

    pub fn virtual_desktop_flows(&self) -> VirtualDesktopFlowMonitor {
        self.virtual_desktop_flows.clone()
    }

    pub async fn serve(self) -> Result<(), SocksError> {
        let listener = TcpListener::bind(self.config.bind).await?;
        self.serve_on(listener).await
    }

    pub async fn serve_on(self, listener: TcpListener) -> Result<(), SocksError> {
        let address = listener.local_addr()?;
        if !address.ip().is_loopback() {
            return Err(SocksError::NonLoopbackBind(address));
        }
        let permits = Arc::new(Semaphore::new(self.config.max_connections));
        loop {
            let (stream, _) = listener.accept().await?;
            let relay_lease = match &self.relay_gate {
                Some(gate) => match gate.active_generation() {
                    Some(generation) => Some((gate.clone(), generation)),
                    None => {
                        self.stats.rejected.fetch_add(1, Ordering::Relaxed);
                        if let Some(diagnostics) = &self.diagnostics {
                            let _ = diagnostics.record(
                                "socks_connection_error",
                                json!({"category": "relay_inactive"}),
                            );
                        }
                        continue;
                    }
                },
                None => None,
            };
            let permit = match permits.clone().try_acquire_owned() {
                Ok(permit) => permit,
                Err(_) => {
                    self.stats.rejected.fetch_add(1, Ordering::Relaxed);
                    if let Some(diagnostics) = &self.diagnostics {
                        let _ = diagnostics
                            .record("socks_connection_error", json!({"category": "capacity"}));
                    }
                    continue;
                }
            };
            self.stats.accepted.fetch_add(1, Ordering::Relaxed);
            let server = self.clone();
            tokio::spawn(async move {
                server.stats.active.fetch_add(1, Ordering::Relaxed);
                let result = match relay_lease {
                    Some((gate, generation)) => {
                        tokio::select! {
                            result = server.handle(stream, Some(generation)) => result,
                            _ = gate.wait_for_invalidation(generation) => Err(SocksError::RelayInactive),
                        }
                    }
                    None => server.handle(stream, None).await,
                };
                server.stats.active.fetch_sub(1, Ordering::Relaxed);
                drop(permit);
                if let (Err(error), Some(diagnostics)) = (result, &server.diagnostics) {
                    let _ = diagnostics.record("socks_connection_error", error.diagnostic_fields());
                }
            });
        }
    }

    async fn handle(
        &self,
        mut client: TcpStream,
        _relay_generation: Option<u64>,
    ) -> Result<(), SocksError> {
        let handshake = time::timeout(self.config.handshake_timeout, async {
            negotiate_auth(&mut client).await?;
            read_request(&mut client).await
        })
        .await
        .map_err(|_| SocksError::HandshakeTimeout)?;
        let request = match handshake {
            Ok(request) => request,
            Err(error @ SocksError::UnsupportedCommand(_)) => {
                let _ = write_reply(
                    &mut client,
                    0x07,
                    Endpoint::Socket(SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 0)),
                )
                .await;
                return Err(error);
            }
            Err(error @ SocksError::UnsupportedAddressType(_)) => {
                let _ = write_reply(
                    &mut client,
                    0x08,
                    Endpoint::Socket(SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 0)),
                )
                .await;
                return Err(error);
            }
            Err(error) => return Err(error),
        };
        if !self.config.command_policy.allows(request.command) {
            let _ = write_reply(
                &mut client,
                0x07,
                Endpoint::Socket(SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 0)),
            )
            .await;
            return Err(SocksError::CommandNotAllowed {
                command: request.command,
                policy: self.config.command_policy,
            });
        }
        match request.command {
            SocksCommand::Connect => self.handle_connect(client, request.destination).await,
            SocksCommand::FwdUdp => {
                write_reply(
                    &mut client,
                    0,
                    Endpoint::Socket(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0)),
                )
                .await?;
                let (reader, writer) = client.into_split();
                relay_fwd_udp(
                    reader,
                    writer,
                    self.config.fwd_udp.clone(),
                    self.udp_stats.clone(),
                )
                .await?;
                Ok(())
            }
        }
    }

    async fn handle_connect(
        &self,
        mut client: TcpStream,
        destination: Endpoint,
    ) -> Result<(), SocksError> {
        let destination_port = endpoint_port(&destination);
        let vd_streamer_flow = VIRTUAL_DESKTOP_STREAMER_PORTS.contains(&destination_port);
        let flow_started = time::Instant::now();
        let upstream = time::timeout(self.config.connect_timeout, async {
            let resolved_target = destination.resolve().await.map_err(SocksError::Resolve)?;
            let local_target = socket_address_is_local(resolved_target);
            let canonical_local_target = vd_streamer_flow && local_target;
            let connect_target = if canonical_local_target {
                physical_lan_ipv4_target(destination_port).unwrap_or(resolved_target)
            } else {
                resolved_target
            };
            let canonicalized = connect_target.ip() != resolved_target.ip();
            let upstream = connect_upstream(connect_target, local_target)
                .await
                .map_err(|error| SocksError::Connect {
                    kind: error.kind(),
                    local_port: local_target.then_some(destination_port),
                })?;
            Ok::<_, SocksError>((upstream, canonicalized, local_target))
        })
        .await;
        let (upstream, canonicalized, local_target) = match upstream {
            Ok(Ok(stream)) => stream,
            Ok(Err(error)) => {
                if vd_streamer_flow && error.is_local_refusal() {
                    if let Some(group) = self.streamer_candidates.joinable(destination_port) {
                        return join_streamer_candidate(client, &group, error).await;
                    }
                }
                let _ = write_reply(
                    &mut client,
                    0x05,
                    Endpoint::Socket(SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 0)),
                )
                .await;
                return Err(error);
            }
            Err(_) => {
                let _ = write_reply(
                    &mut client,
                    0x04,
                    Endpoint::Socket(SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 0)),
                )
                .await;
                return Err(SocksError::ConnectTimeout);
            }
        };
        if canonicalized {
            if let Some(diagnostics) = &self.diagnostics {
                let _ = diagnostics.record(
                    "vd_streamer_local_target_canonicalized",
                    json!({"destination_port": destination_port}),
                );
            }
        }
        client.set_nodelay(true)?;
        upstream.set_nodelay(true)?;
        let bound = upstream.local_addr()?;
        // Opened before the reply so a duplicate refused right after this
        // accept already finds it.
        let candidates = if vd_streamer_flow && local_target {
            self.streamer_candidates.open(destination_port, bound)
        } else {
            None
        };
        write_reply(&mut client, 0, Endpoint::Socket(bound)).await?;
        let virtual_desktop_flow = self.virtual_desktop_flows.begin(destination_port);
        if vd_streamer_flow {
            if let Some(diagnostics) = &self.diagnostics {
                let _ = diagnostics.record(
                    "vd_streamer_flow_opened",
                    json!({"destination_port": destination_port}),
                );
            }
        }
        let flow_observation = FlowObservation::default();
        let transfer = match candidates {
            Some(candidates) => {
                let settlement =
                    settle_candidates(&candidates, &upstream, client, &flow_observation).await;
                drop(candidates);
                self.record_candidate_settlement(destination_port, &settlement);
                match settlement.chosen {
                    Some(chosen) => {
                        if chosen.delivered > 0 {
                            if let Some(flow) = &virtual_desktop_flow {
                                flow.mark_forwarded();
                            }
                        }
                        relay_flow(
                            chosen.client,
                            upstream,
                            &flow_observation,
                            virtual_desktop_flow.as_ref(),
                            &chosen.client_prefix,
                            &chosen.upstream_suffix,
                        )
                        .await
                        .map(|(from_client, from_upstream)| {
                            (from_client, from_upstream + chosen.delivered as u64)
                        })
                    }
                    None => Ok((0, 0)),
                }
            }
            None => {
                relay_flow(
                    client,
                    upstream,
                    &flow_observation,
                    virtual_desktop_flow.as_ref(),
                    &[],
                    &[],
                )
                .await
            }
        };
        if vd_streamer_flow {
            if let Some(diagnostics) = &self.diagnostics {
                let duration_ms = flow_started.elapsed().as_millis().min(u64::MAX as u128) as u64;
                let first_terminal = flow_observation.first_terminal();
                let first_terminal_side = first_terminal
                    .map(|terminal| terminal.endpoint.as_str())
                    .unwrap_or("unknown");
                let first_terminal_event = first_terminal
                    .map(|terminal| terminal.event.as_str())
                    .unwrap_or("unknown");
                let fields = match &transfer {
                    Ok((from_client, from_upstream)) => json!({
                        "destination_port": destination_port,
                        "duration_ms": duration_ms,
                        "first_terminal_event": first_terminal_event,
                        "first_terminal_side": first_terminal_side,
                        "outcome": "eof",
                        "tx_bytes": from_client,
                        "rx_bytes": from_upstream,
                    }),
                    Err(error) => json!({
                        "destination_port": destination_port,
                        "duration_ms": duration_ms,
                        "error_category": format!("{:?}", error.kind()),
                        "first_terminal_event": first_terminal_event,
                        "first_terminal_side": first_terminal_side,
                        "outcome": "io_error",
                    }),
                };
                let _ = diagnostics.record("vd_streamer_flow_ended", fields);
            }
        }
        let (from_client, from_upstream) = transfer?;
        self.stats
            .tcp_tx_bytes
            .fetch_add(from_client, Ordering::Relaxed);
        self.stats
            .tcp_rx_bytes
            .fetch_add(from_upstream, Ordering::Relaxed);
        Ok(())
    }

    fn record_candidate_settlement(&self, destination_port: u16, settlement: &CandidateSettlement) {
        if settlement.arrivals < 2 {
            return;
        }
        if let Some(diagnostics) = &self.diagnostics {
            let _ = diagnostics.record(
                "vd_streamer_candidates_settled",
                json!({
                    "destination_port": destination_port,
                    "candidates": settlement.arrivals,
                    "chosen_arrival": settlement.chosen.as_ref().map(|chosen| chosen.arrival),
                    "outcome": settlement.outcome,
                }),
            );
        }
    }
}

async fn relay_flow(
    client: TcpStream,
    upstream: TcpStream,
    observation: &FlowObservation,
    virtual_desktop_flow: Option<&VirtualDesktopFlowGuard>,
    client_prefix: &[u8],
    upstream_suffix: &[u8],
) -> Result<(u64, u64), StdIoError> {
    let mut client = ObservedIo::new(
        client,
        FlowEndpoint::Quest,
        observation,
        virtual_desktop_flow,
    );
    let mut upstream = ObservedIo::new(
        upstream,
        FlowEndpoint::Streamer,
        observation,
        virtual_desktop_flow,
    );
    upstream.write_all(client_prefix).await?;
    client.write_all(upstream_suffix).await?;
    let (from_client, from_upstream) = io::copy_bidirectional(&mut client, &mut upstream).await?;
    Ok((
        from_client + client_prefix.len() as u64,
        from_upstream + upstream_suffix.len() as u64,
    ))
}

/// Virtual Desktop dials every IPv4 address the PC advertises for each Streamer
/// port at once. HEV completes all of those handshakes on the Quest before the
/// PC connects, while the Streamer accepts one connect per port and refuses the
/// rest, so VD can be left holding a refused duplicate as its chosen socket.
/// A refused duplicate therefore joins the accepted flow, which sends the
/// Streamer's bytes to every candidate and keeps the one VD writes on, the last
/// one VD leaves open, or failing both, the first arrival.
const CANDIDATE_JOIN_WINDOW: Duration = Duration::from_millis(500);
const CANDIDATE_DECISION_TIMEOUT: Duration = Duration::from_millis(250);
const CANDIDATE_UNRESOLVED_LIMIT: Duration = Duration::from_secs(10);
const CANDIDATE_REPLAY_LIMIT: usize = 64 * 1024;

#[derive(Clone, Default)]
struct StreamerCandidates {
    groups: Arc<StdMutex<[Option<Arc<CandidateGroup>>; VIRTUAL_DESKTOP_STREAMER_PORTS.len()]>>,
}

impl StreamerCandidates {
    fn open(&self, destination_port: u16, bound: SocketAddr) -> Option<CandidateLease> {
        let index = streamer_port_index(destination_port)?;
        let group = Arc::new(CandidateGroup::new(bound));
        let previous = self.groups.lock().ok()?[index].replace(group.clone());
        // A newer accept on the same port belongs to a new VD attempt.
        if let Some(previous) = previous {
            previous.close();
        }
        Some(CandidateLease(group))
    }

    fn joinable(&self, destination_port: u16) -> Option<Arc<CandidateGroup>> {
        let index = streamer_port_index(destination_port)?;
        let group = self.groups.lock().ok()?[index].clone()?;
        group.is_joinable().then_some(group)
    }
}

fn streamer_port_index(destination_port: u16) -> Option<usize> {
    VIRTUAL_DESKTOP_STREAMER_PORTS
        .iter()
        .position(|port| *port == destination_port)
}

struct CandidateGroup {
    bound: SocketAddr,
    join_deadline: time::Instant,
    /// `None` once the accepted flow has chosen or ended.
    joins: StdMutex<Option<Vec<TcpStream>>>,
    joined: Notify,
}

impl CandidateGroup {
    fn new(bound: SocketAddr) -> Self {
        Self {
            bound,
            join_deadline: time::Instant::now() + CANDIDATE_JOIN_WINDOW,
            joins: StdMutex::new(Some(Vec::new())),
            joined: Notify::new(),
        }
    }

    fn is_joinable(&self) -> bool {
        time::Instant::now() < self.join_deadline
            && self.joins.lock().is_ok_and(|joins| joins.is_some())
    }

    fn join(&self, client: TcpStream) -> Result<(), TcpStream> {
        if time::Instant::now() >= self.join_deadline {
            return Err(client);
        }
        let Ok(mut joins) = self.joins.lock() else {
            return Err(client);
        };
        let Some(pending) = joins.as_mut() else {
            return Err(client);
        };
        pending.push(client);
        drop(joins);
        self.joined.notify_one();
        Ok(())
    }

    fn take_joined(&self) -> Vec<TcpStream> {
        self.joins
            .lock()
            .ok()
            .and_then(|mut joins| joins.as_mut().map(std::mem::take))
            .unwrap_or_default()
    }

    fn close(&self) {
        if let Ok(mut joins) = self.joins.lock() {
            joins.take();
        }
    }
}

/// Stops further joins when the accepted flow settles or is dropped.
struct CandidateLease(Arc<CandidateGroup>);

impl std::ops::Deref for CandidateLease {
    type Target = CandidateGroup;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Drop for CandidateLease {
    fn drop(&mut self) {
        self.0.close();
    }
}

async fn join_streamer_candidate(
    mut client: TcpStream,
    group: &CandidateGroup,
    refusal: SocksError,
) -> Result<(), SocksError> {
    client.set_nodelay(true)?;
    write_reply(&mut client, 0, Endpoint::Socket(group.bound)).await?;
    // Too late to join: VD sees this duplicate close, as it would a refusal.
    group.join(client).map_err(|_| refusal)
}

struct Candidate {
    client: TcpStream,
    arrival: usize,
    delivered: usize,
}

struct ChosenCandidate {
    client: TcpStream,
    arrival: usize,
    /// Bytes VD sent on this candidate before it was chosen.
    client_prefix: Vec<u8>,
    /// Streamer bytes already sent to this candidate.
    delivered: usize,
    /// Streamer bytes read for the candidates but not yet sent to this one.
    upstream_suffix: Vec<u8>,
}

struct CandidateSettlement {
    arrivals: usize,
    outcome: &'static str,
    chosen: Option<ChosenCandidate>,
}

enum CandidateEvent {
    Joined,
    Upstream,
    Readable(usize),
    Writable(usize),
    Deadline,
}

fn choose_candidate(
    mut candidates: Vec<Candidate>,
    index: usize,
    client_prefix: Vec<u8>,
    replay: &[u8],
    arrivals: usize,
    outcome: &'static str,
) -> CandidateSettlement {
    // Dropping the rest closes every duplicate VD did not use.
    let chosen = candidates.remove(index);
    CandidateSettlement {
        arrivals,
        outcome,
        chosen: Some(ChosenCandidate {
            upstream_suffix: replay[chosen.delivered..].to_vec(),
            delivered: chosen.delivered,
            arrival: chosen.arrival,
            client: chosen.client,
            client_prefix,
        }),
    }
}

async fn settle_candidates(
    group: &CandidateGroup,
    upstream: &TcpStream,
    first: TcpStream,
    observation: &FlowObservation,
) -> CandidateSettlement {
    let give_up = time::Instant::now() + CANDIDATE_UNRESOLVED_LIMIT;
    let mut candidates = vec![Candidate {
        client: first,
        arrival: 0,
        delivered: 0,
    }];
    let mut arrivals = 1;
    let mut replay = Vec::new();
    let mut upstream_closed = false;
    let mut decision_deadline = None;
    let mut buffer = vec![0; 16 * 1024];
    loop {
        for client in group.take_joined() {
            candidates.push(Candidate {
                client,
                arrival: arrivals,
                delivered: 0,
            });
            arrivals += 1;
        }
        let now = time::Instant::now();
        let joining = now < group.join_deadline;
        let exhausted = upstream_closed || replay.len() >= CANDIDATE_REPLAY_LIMIT;
        let deadline = match candidates.len() {
            0 if !joining => {
                observation.record(FlowEndpoint::Quest, FlowTerminalEvent::ReadEof);
                return CandidateSettlement {
                    arrivals,
                    outcome: "all_closed",
                    chosen: None,
                };
            }
            1 if !joining || exhausted => {
                let outcome = if arrivals > 1 {
                    "sole_survivor"
                } else {
                    "single"
                };
                return choose_candidate(candidates, 0, Vec::new(), &replay, arrivals, outcome);
            }
            0 | 1 => group.join_deadline,
            _ => {
                if exhausted {
                    decision_deadline.get_or_insert(now + CANDIDATE_DECISION_TIMEOUT);
                }
                let deadline = decision_deadline.map_or(give_up, |decision| decision.min(give_up));
                if now >= deadline {
                    return choose_candidate(
                        candidates,
                        0,
                        Vec::new(),
                        &replay,
                        arrivals,
                        "timeout",
                    );
                }
                deadline
            }
        };

        let read_upstream = !upstream_closed && replay.len() < CANDIDATE_REPLAY_LIMIT;
        let event = {
            let joined = group.joined.notified();
            let sleep = time::sleep_until(deadline);
            tokio::pin!(joined, sleep);
            std::future::poll_fn(|context| {
                if joined.as_mut().poll(context).is_ready() {
                    return Poll::Ready(CandidateEvent::Joined);
                }
                if read_upstream && upstream.poll_read_ready(context).is_ready() {
                    return Poll::Ready(CandidateEvent::Upstream);
                }
                for (index, candidate) in candidates.iter().enumerate() {
                    if candidate.client.poll_read_ready(context).is_ready() {
                        return Poll::Ready(CandidateEvent::Readable(index));
                    }
                    if candidate.delivered < replay.len()
                        && candidate.client.poll_write_ready(context).is_ready()
                    {
                        return Poll::Ready(CandidateEvent::Writable(index));
                    }
                }
                if sleep.as_mut().poll(context).is_ready() {
                    return Poll::Ready(CandidateEvent::Deadline);
                }
                Poll::Pending
            })
            .await
        };

        match event {
            CandidateEvent::Joined | CandidateEvent::Deadline => {}
            CandidateEvent::Upstream => {
                let room = (CANDIDATE_REPLAY_LIMIT - replay.len()).min(buffer.len());
                match upstream.try_read(&mut buffer[..room]) {
                    Ok(0) => {
                        upstream_closed = true;
                        observation.record(FlowEndpoint::Streamer, FlowTerminalEvent::ReadEof);
                    }
                    Ok(read) => replay.extend_from_slice(&buffer[..read]),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(_) => {
                        upstream_closed = true;
                        observation.record(FlowEndpoint::Streamer, FlowTerminalEvent::ReadError);
                    }
                }
            }
            CandidateEvent::Readable(index) => match candidates[index].client.try_read(&mut buffer)
            {
                // VD closed a duplicate it is not using.
                Ok(0) => {
                    candidates.remove(index);
                }
                Ok(read) => {
                    let prefix = buffer[..read].to_vec();
                    return choose_candidate(
                        candidates,
                        index,
                        prefix,
                        &replay,
                        arrivals,
                        "client_data",
                    );
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => {
                    candidates.remove(index);
                }
            },
            CandidateEvent::Writable(index) => {
                let candidate = &mut candidates[index];
                match candidate.client.try_write(&replay[candidate.delivered..]) {
                    Ok(written) => candidate.delivered += written,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(_) => {
                        candidates.remove(index);
                    }
                }
            }
        }
    }
}

fn endpoint_port(endpoint: &Endpoint) -> u16 {
    match endpoint {
        Endpoint::Socket(address) => address.port(),
        Endpoint::Domain(_, port) => *port,
    }
}

fn socket_address_is_local(mut target: SocketAddr) -> bool {
    target.set_port(0);
    let socket = match target {
        SocketAddr::V4(_) => TcpSocket::new_v4(),
        SocketAddr::V6(_) => TcpSocket::new_v6(),
    };
    socket.and_then(|socket| socket.bind(target)).is_ok()
}

/// HEV has already completed the Quest-side TCP handshake before this connect
/// starts, so a slow refusal surfaces to the Quest app as a dead connection.
/// Windows retries a refused SYN for about two seconds; a host-local target
/// cannot lose SYNs, so fail it immediately like the Wi-Fi path would.
async fn connect_upstream(target: SocketAddr, local_target: bool) -> Result<TcpStream, StdIoError> {
    let socket = match target {
        SocketAddr::V4(_) => TcpSocket::new_v4()?,
        SocketAddr::V6(_) => TcpSocket::new_v6()?,
    };
    if local_target {
        disable_syn_retransmissions(&socket);
    }
    socket.connect(target).await
}

#[cfg(not(windows))]
fn disable_syn_retransmissions(_socket: &TcpSocket) {}

#[cfg(windows)]
fn disable_syn_retransmissions(socket: &TcpSocket) {
    use std::{mem::size_of, os::windows::io::AsRawSocket};
    use windows_sys::Win32::Networking::WinSock::{
        WSAIoctl, SIO_TCP_INITIAL_RTO, SOCKET, TCP_INITIAL_RTO_NO_SYN_RETRANSMISSIONS,
        TCP_INITIAL_RTO_PARAMETERS,
    };

    let parameters = TCP_INITIAL_RTO_PARAMETERS {
        // TCP_INITIAL_RTO_UNSPECIFIED_RTT keeps the stack's default RTO.
        Rtt: u16::MAX,
        MaxSynRetransmissions: TCP_INITIAL_RTO_NO_SYN_RETRANSMISSIONS as u8,
    };
    let mut returned = 0u32;
    // Best effort: a stack without this ioctl keeps its default retry policy.
    unsafe {
        WSAIoctl(
            socket.as_raw_socket() as SOCKET,
            SIO_TCP_INITIAL_RTO,
            (&parameters as *const TCP_INITIAL_RTO_PARAMETERS).cast(),
            size_of::<TCP_INITIAL_RTO_PARAMETERS>() as u32,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
            None,
        );
    }
}

#[cfg(not(windows))]
fn physical_lan_ipv4_target(_destination_port: u16) -> Option<SocketAddr> {
    None
}

#[cfg(windows)]
fn physical_lan_ipv4_target(destination_port: u16) -> Option<SocketAddr> {
    use std::mem::size_of;
    use windows_sys::Win32::{
        Foundation::ERROR_BUFFER_OVERFLOW,
        NetworkManagement::{
            IpHelper::{
                GetAdaptersAddresses, GAA_FLAG_INCLUDE_GATEWAYS, GAA_FLAG_SKIP_ANYCAST,
                GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST, IF_TYPE_ETHERNET_CSMACD,
                IF_TYPE_IEEE80211, IP_ADAPTER_ADDRESSES_LH,
            },
            Ndis::IfOperStatusUp,
        },
        Networking::WinSock::{AF_INET, SOCKADDR_IN},
    };

    let flags = GAA_FLAG_INCLUDE_GATEWAYS
        | GAA_FLAG_SKIP_ANYCAST
        | GAA_FLAG_SKIP_MULTICAST
        | GAA_FLAG_SKIP_DNS_SERVER;
    let mut bytes = 0u32;
    let result = unsafe {
        GetAdaptersAddresses(
            AF_INET as u32,
            flags,
            std::ptr::null(),
            std::ptr::null_mut(),
            &mut bytes,
        )
    };
    if result != ERROR_BUFFER_OVERFLOW || bytes == 0 {
        return None;
    }

    let words = (bytes as usize).div_ceil(size_of::<usize>());
    let mut storage = vec![0usize; words];
    let mut adapter = storage.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
    if unsafe { GetAdaptersAddresses(AF_INET as u32, flags, std::ptr::null(), adapter, &mut bytes) }
        != 0
    {
        return None;
    }

    let mut selected: Option<(u32, Ipv4Addr)> = None;
    while !adapter.is_null() {
        let current = unsafe { &*adapter };
        let physical = matches!(current.IfType, IF_TYPE_ETHERNET_CSMACD | IF_TYPE_IEEE80211);
        if current.OperStatus == IfOperStatusUp
            && physical
            && !current.FirstGatewayAddress.is_null()
        {
            let mut unicast = current.FirstUnicastAddress;
            while !unicast.is_null() {
                let address = unsafe { &*unicast }.Address;
                if !address.lpSockaddr.is_null()
                    && address.iSockaddrLength as usize >= size_of::<SOCKADDR_IN>()
                {
                    let ipv4 = unsafe { &*address.lpSockaddr.cast::<SOCKADDR_IN>() };
                    if ipv4.sin_family == AF_INET {
                        let octets = unsafe { ipv4.sin_addr.S_un.S_un_b };
                        let candidate =
                            Ipv4Addr::new(octets.s_b1, octets.s_b2, octets.s_b3, octets.s_b4);
                        if !candidate.is_loopback() && !candidate.is_unspecified() {
                            let replace = selected
                                .as_ref()
                                .is_none_or(|(metric, _)| current.Ipv4Metric < *metric);
                            if replace {
                                selected = Some((current.Ipv4Metric, candidate));
                            }
                        }
                    }
                }
                unicast = unsafe { (*unicast).Next };
            }
        }
        adapter = current.Next;
    }

    selected.map(|(_, address)| SocketAddr::new(address.into(), destination_port))
}

async fn negotiate_auth<S>(stream: &mut S) -> Result<(), SocksError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut header = [0; 2];
    stream.read_exact(&mut header).await?;
    if header[0] != SOCKS_VERSION || header[1] == 0 {
        return Err(SocksError::Malformed("invalid authentication greeting"));
    }
    let mut methods = vec![0; header[1] as usize];
    stream.read_exact(&mut methods).await?;
    let selected = if methods.contains(&AUTH_NONE) {
        AUTH_NONE
    } else {
        AUTH_UNACCEPTABLE
    };
    stream.write_all(&[SOCKS_VERSION, selected]).await?;
    stream.flush().await?;
    if selected == AUTH_UNACCEPTABLE {
        return Err(SocksError::NoAcceptableAuthentication);
    }
    Ok(())
}

async fn read_request<R>(reader: &mut R) -> Result<SocksRequest, SocksError>
where
    R: AsyncRead + Unpin,
{
    let mut prefix = [0; 4];
    reader.read_exact(&mut prefix).await?;
    let tail_len = match prefix[3] {
        crate::udp::SOCKS_ATYP_IPV4 => 4 + 2,
        crate::udp::SOCKS_ATYP_IPV6 => 16 + 2,
        crate::udp::SOCKS_ATYP_DOMAIN => {
            let length = reader.read_u8().await? as usize;
            if length == 0 || length > crate::udp::MAX_DOMAIN_LEN {
                return Err(SocksError::Malformed("invalid domain length"));
            }
            let mut encoded = prefix.to_vec();
            encoded.push(length as u8);
            encoded.resize(encoded.len() + length + 2, 0);
            reader.read_exact(&mut encoded[5..]).await?;
            return SocksRequest::decode(&encoded);
        }
        other => return Err(SocksError::UnsupportedAddressType(other)),
    };
    let mut encoded = prefix.to_vec();
    encoded.resize(encoded.len() + tail_len, 0);
    reader.read_exact(&mut encoded[4..]).await?;
    SocksRequest::decode(&encoded)
}

async fn write_reply<W>(writer: &mut W, reply: u8, endpoint: Endpoint) -> Result<(), SocksError>
where
    W: AsyncWrite + Unpin,
{
    let address = encode_endpoint(&endpoint)?;
    let mut encoded = Vec::with_capacity(3 + address.len());
    encoded.extend_from_slice(&[SOCKS_VERSION, reply, 0]);
    encoded.extend_from_slice(&address);
    writer.write_all(&encoded).await?;
    writer.flush().await?;
    Ok(())
}

#[derive(Clone, Default)]
struct SocksStats(Arc<SocksStatsInner>);

#[derive(Default)]
struct SocksStatsInner {
    accepted: AtomicU64,
    rejected: AtomicU64,
    active: AtomicUsize,
    tcp_tx_bytes: AtomicU64,
    tcp_rx_bytes: AtomicU64,
}

impl std::ops::Deref for SocksStats {
    type Target = SocksStatsInner;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl SocksStats {
    fn snapshot(&self) -> SocksStatsSnapshot {
        SocksStatsSnapshot {
            accepted_connections: self.accepted.load(Ordering::Relaxed),
            rejected_connections: self.rejected.load(Ordering::Relaxed),
            active_connections: self.active.load(Ordering::Relaxed),
            tcp_tx_bytes: self.tcp_tx_bytes.load(Ordering::Relaxed),
            tcp_rx_bytes: self.tcp_rx_bytes.load(Ordering::Relaxed),
            udp: Default::default(),
            virtual_desktop: Default::default(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct SocksStatsSnapshot {
    pub accepted_connections: u64,
    pub rejected_connections: u64,
    pub active_connections: usize,
    pub tcp_tx_bytes: u64,
    pub tcp_rx_bytes: u64,
    pub udp: crate::udp::UdpStatsSnapshot,
    #[serde(default)]
    pub virtual_desktop: VirtualDesktopFlowSnapshot,
}

#[derive(Debug, Error)]
pub enum SocksError {
    #[error("SOCKS listener must be loopback, got {0}")]
    NonLoopbackBind(SocketAddr),
    #[error("malformed SOCKS request: {0}")]
    Malformed(&'static str),
    #[error("SOCKS client offered no acceptable authentication method")]
    NoAcceptableAuthentication,
    #[error("unsupported SOCKS command {0:#04x}")]
    UnsupportedCommand(u8),
    #[error("SOCKS command {command:?} is not allowed on the {policy:?} listener")]
    CommandNotAllowed {
        command: SocksCommand,
        policy: SocksCommandPolicy,
    },
    #[error("unsupported SOCKS address type {0:#04x}")]
    UnsupportedAddressType(u8),
    #[error("SOCKS CONNECT timed out")]
    ConnectTimeout,
    #[error("SOCKS destination could not be resolved: {0}")]
    Resolve(crate::udp::UdpRelayError),
    #[error("SOCKS upstream connect failed: {kind:?}")]
    Connect {
        kind: std::io::ErrorKind,
        /// Recorded only for the PC's own addresses, never for remote hosts.
        local_port: Option<u16>,
    },
    #[error("SOCKS authentication or request timed out")]
    HandshakeTimeout,
    #[error("relay session became inactive")]
    RelayInactive,
    #[error(transparent)]
    Frame(#[from] HevFrameError),
    #[error(transparent)]
    Udp(#[from] crate::udp::UdpRelayError),
    #[error("SOCKS I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

impl SocksError {
    fn category(&self) -> &'static str {
        match self {
            Self::NonLoopbackBind(_) => "non_loopback_bind",
            Self::Malformed(_) => "malformed",
            Self::NoAcceptableAuthentication => "authentication",
            Self::UnsupportedCommand(_) => "unsupported_command",
            Self::CommandNotAllowed { .. } => "command_not_allowed",
            Self::UnsupportedAddressType(_) => "unsupported_address_type",
            Self::ConnectTimeout => "connect_timeout",
            Self::Resolve(_) => "resolve",
            Self::Connect { .. } => "tcp_connect",
            Self::HandshakeTimeout => "handshake_timeout",
            Self::RelayInactive => "relay_inactive",
            Self::Frame(_) => "udp_frame",
            Self::Udp(_) => "udp_relay",
            Self::Io(_) => "io",
        }
    }

    fn is_local_refusal(&self) -> bool {
        matches!(
            self,
            Self::Connect {
                kind: std::io::ErrorKind::ConnectionRefused,
                local_port: Some(_),
            }
        )
    }

    fn diagnostic_fields(&self) -> serde_json::Value {
        match self {
            Self::Connect { kind, local_port } => json!({
                "category": self.category(),
                "error_kind": format!("{kind:?}"),
                "local_destination_port": local_port,
            }),
            _ => json!({"category": self.category()}),
        }
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use tokio::net::UdpSocket;

    use super::*;
    use crate::udp::HevUdpFrame;

    #[test]
    fn rejects_non_loopback_listener() {
        let config = SocksConfig {
            bind: "0.0.0.0:1080".parse().unwrap(),
            ..Default::default()
        };
        assert!(matches!(
            SocksServer::new(config),
            Err(SocksError::NonLoopbackBind(_))
        ));
    }

    #[test]
    fn parses_fwd_udp_request() {
        let bytes = [5, CMD_FWD_UDP, 0, 1, 127, 0, 0, 1, 0x12, 0x34];
        let request = SocksRequest::decode(&bytes).unwrap();
        assert_eq!(request.command, SocksCommand::FwdUdp);
        assert_eq!(
            request.destination,
            Endpoint::Socket("127.0.0.1:4660".parse().unwrap())
        );
    }

    #[test]
    fn virtual_desktop_health_requires_forwarding_on_all_four_live_ports() {
        let monitor = VirtualDesktopFlowMonitor::default();
        let mut guards = Vec::new();
        for port in VIRTUAL_DESKTOP_STREAMER_PORTS {
            guards.push(monitor.begin(port).unwrap());
        }
        assert_eq!(monitor.snapshot().active_ports, 4);
        assert!(!monitor.snapshot().connected);

        for guard in &guards[..3] {
            guard.mark_forwarded();
        }
        assert!(!monitor.snapshot().connected);

        guards[3].mark_forwarded();
        assert_eq!(
            monitor.snapshot(),
            VirtualDesktopFlowSnapshot {
                connected: true,
                active_ports: 4,
                active_flows: [1, 1, 1, 1],
            }
        );

        let duplicate = monitor.begin(VIRTUAL_DESKTOP_STREAMER_PORTS[3]).unwrap();
        drop(guards.pop());
        assert_eq!(monitor.snapshot().active_ports, 4);
        assert!(!monitor.snapshot().connected);
        drop(duplicate);
    }

    #[tokio::test]
    async fn flow_observation_records_the_endpoint_that_closes_first() {
        let (quest_peer, quest_relay) = io::duplex(64);
        let observation = FlowObservation::default();
        let mut observed = ObservedIo::new(quest_relay, FlowEndpoint::Quest, &observation, None);

        drop(quest_peer);
        let mut byte = [0; 1];
        assert_eq!(observed.read(&mut byte).await.unwrap(), 0);
        assert_eq!(
            observation.first_terminal(),
            Some(FlowTerminal {
                endpoint: FlowEndpoint::Quest,
                event: FlowTerminalEvent::ReadEof,
            })
        );
    }

    #[tokio::test]
    async fn forwarded_payload_marks_the_live_virtual_desktop_flow() {
        let monitor = VirtualDesktopFlowMonitor::default();
        let mut guards: Vec<_> = VIRTUAL_DESKTOP_STREAMER_PORTS
            .iter()
            .map(|port| monitor.begin(*port).unwrap())
            .collect();
        for guard in &guards[..3] {
            guard.mark_forwarded();
        }

        let (_peer, relay) = io::duplex(64);
        let observation = FlowObservation::default();
        let mut observed =
            ObservedIo::new(relay, FlowEndpoint::Quest, &observation, Some(&guards[3]));
        assert!(!monitor.snapshot().connected);

        observed.write_all(b"payload").await.unwrap();
        assert!(monitor.snapshot().connected);

        drop(observed);
        drop(guards.pop());
        assert!(!monitor.snapshot().connected);
    }

    async fn connect_through_socks(address: SocketAddr, destination: SocketAddr) -> TcpStream {
        let mut client = TcpStream::connect(address).await.unwrap();
        client.write_all(&[5, 1, 0]).await.unwrap();
        let mut auth = [0; 2];
        client.read_exact(&mut auth).await.unwrap();
        assert_eq!(auth, [5, 0]);
        let mut request = vec![5, CMD_CONNECT, 0];
        request.extend_from_slice(&encode_endpoint(&Endpoint::Socket(destination)).unwrap());
        client.write_all(&request).await.unwrap();
        let mut reply = [0; 10];
        client.read_exact(&mut reply).await.unwrap();
        assert_eq!(reply[1], 0);
        client
    }

    async fn connect_udp_through_socks(address: SocketAddr) -> TcpStream {
        let mut client = TcpStream::connect(address).await.unwrap();
        client.write_all(&[5, 1, 0]).await.unwrap();
        let mut auth = [0; 2];
        client.read_exact(&mut auth).await.unwrap();
        assert_eq!(auth, [5, 0]);
        client
            .write_all(&[5, CMD_FWD_UDP, 0, 1, 0, 0, 0, 0, 0, 0])
            .await
            .unwrap();
        let mut reply = [0; 10];
        client.read_exact(&mut reply).await.unwrap();
        assert_eq!(reply[1], 0);
        client
    }

    async fn assert_udp_echo(client: &mut TcpStream, echo_address: SocketAddr, payload: &[u8]) {
        HevUdpFrame {
            endpoint: Endpoint::Socket(echo_address),
            payload: payload.to_vec(),
        }
        .write_to(client)
        .await
        .unwrap();
        let response = HevUdpFrame::read_from(client).await.unwrap();
        assert_eq!(response.endpoint, Endpoint::Socket(echo_address));
        assert_eq!(response.payload, payload);
    }

    #[tokio::test]
    async fn connect_preserves_a_large_bidirectional_stream() {
        let echo = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let echo_address = echo.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = echo.accept().await.unwrap();
            let (mut reader, mut writer) = stream.split();
            io::copy(&mut reader, &mut writer).await.unwrap();
        });

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = SocksServer::new(SocksConfig {
            bind: address,
            ..Default::default()
        })
        .unwrap();
        let stats = server.clone();
        let task = tokio::spawn(server.serve_on(listener));

        let mut client = TcpStream::connect(address).await.unwrap();
        client.write_all(&[5, 1, 0]).await.unwrap();
        let mut auth = [0; 2];
        client.read_exact(&mut auth).await.unwrap();
        assert_eq!(auth, [5, 0]);
        let mut request = vec![5, CMD_CONNECT, 0];
        request.extend_from_slice(&encode_endpoint(&Endpoint::Socket(echo_address)).unwrap());
        client.write_all(&request).await.unwrap();
        let mut reply = [0; 10];
        client.read_exact(&mut reply).await.unwrap();
        assert_eq!(reply[1], 0);
        let payload: Vec<u8> = (0..(2 * 1024 * 1024 + 1))
            .map(|index| (index % 251) as u8)
            .collect();
        let expected = payload.clone();
        let byte_count = payload.len() as u64;
        let (mut reader, mut writer) = client.into_split();
        let sender = tokio::spawn(async move {
            writer.write_all(&payload).await.unwrap();
            writer.shutdown().await.unwrap();
        });
        let mut echoed = vec![0; expected.len()];
        reader.read_exact(&mut echoed).await.unwrap();
        sender.await.unwrap();
        assert_eq!(echoed, expected);

        time::timeout(Duration::from_secs(5), async {
            while stats.stats().active_connections != 0 {
                time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(stats.stats().tcp_tx_bytes, byte_count);
        assert_eq!(stats.stats().tcp_rx_bytes, byte_count);
        task.abort();
    }

    #[tokio::test]
    async fn allows_overlapping_virtual_desktop_flows_on_the_same_port() {
        let vd_port = VIRTUAL_DESKTOP_STREAMER_PORTS[0];
        let echo = TcpListener::bind((Ipv4Addr::UNSPECIFIED, vd_port))
            .await
            .unwrap();
        let echo_task = tokio::spawn(async move {
            loop {
                let (mut stream, _) = echo.accept().await.unwrap();
                tokio::spawn(async move {
                    let (mut reader, mut writer) = stream.split();
                    let _ = io::copy(&mut reader, &mut writer).await;
                });
            }
        });

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = SocksServer::new(SocksConfig {
            bind: address,
            ..Default::default()
        })
        .unwrap();
        let stats = server.clone();
        let task = tokio::spawn(server.serve_on(listener));
        let destination = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), vd_port);

        let mut first = connect_through_socks(address, destination).await;
        let mut second = connect_through_socks(address, destination).await;
        first.write_all(b"first").await.unwrap();
        second.write_all(b"second").await.unwrap();
        let mut first_echo = [0; 5];
        let mut second_echo = [0; 6];
        first.read_exact(&mut first_echo).await.unwrap();
        second.read_exact(&mut second_echo).await.unwrap();

        assert_eq!(&first_echo, b"first");
        assert_eq!(&second_echo, b"second");
        assert_eq!(stats.stats().virtual_desktop.active_flows[0], 2);

        task.abort();
        echo_task.abort();
    }

    #[tokio::test]
    async fn gate_disable_closes_old_tcp_flows_and_reopens_the_same_listener() {
        let echo = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let echo_address = echo.local_addr().unwrap();
        let echo_task = tokio::spawn(async move {
            loop {
                let (mut stream, _) = echo.accept().await.unwrap();
                tokio::spawn(async move {
                    let (mut reader, mut writer) = stream.split();
                    let _ = io::copy(&mut reader, &mut writer).await;
                });
            }
        });

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let gate = RelayGate::default();
        gate.set_enabled(true);
        let server = SocksServer::new(SocksConfig {
            bind: address,
            ..Default::default()
        })
        .unwrap()
        .with_relay_gate(gate.clone());
        let stats = server.clone();
        let task = tokio::spawn(server.serve_on(listener));

        let mut first = connect_through_socks(address, echo_address).await;
        first.write_all(b"before-sleep").await.unwrap();
        let mut echoed = [0; 12];
        first.read_exact(&mut echoed).await.unwrap();
        assert_eq!(&echoed, b"before-sleep");
        assert_eq!(stats.stats().active_connections, 1);

        gate.set_enabled(false);
        let closed = time::timeout(Duration::from_millis(500), first.read_u8())
            .await
            .expect("old flow stayed open after explicit suspend");
        assert!(closed.is_err());
        time::timeout(Duration::from_millis(500), async {
            while stats.stats().active_connections != 0 {
                time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();

        let mut rejected = TcpStream::connect(address).await.unwrap();
        rejected.write_all(&[5, 1, 0]).await.unwrap();
        let mut auth = [0; 2];
        let rejected_read =
            time::timeout(Duration::from_millis(500), rejected.read_exact(&mut auth))
                .await
                .expect("degraded relay left a new connection pending");
        assert!(rejected_read.is_err());

        gate.set_enabled(true);
        let mut second = connect_through_socks(address, echo_address).await;
        second.write_all(b"after-wake").await.unwrap();
        let mut echoed = [0; 10];
        second.read_exact(&mut echoed).await.unwrap();
        assert_eq!(&echoed, b"after-wake");

        task.abort();
        echo_task.abort();
    }

    #[tokio::test]
    async fn recovery_state_rejects_new_connections_promptly() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let gate = RelayGate::default();
        gate.wait_for_activation();
        let server = SocksServer::new(SocksConfig {
            bind: address,
            ..Default::default()
        })
        .unwrap()
        .with_relay_gate(gate.clone());
        let stats = server.clone();
        let task = tokio::spawn(server.serve_on(listener));

        let mut client = TcpStream::connect(address).await.unwrap();
        client.write_all(&[5, 1, 0]).await.unwrap();
        let mut auth = [0; 2];
        let rejected_read = time::timeout(Duration::from_millis(200), client.read_exact(&mut auth))
            .await
            .expect("inactive relay left a new connection pending");
        assert!(rejected_read.is_err());
        assert_eq!(stats.stats().accepted_connections, 0);
        assert_eq!(stats.stats().rejected_connections, 1);

        task.abort();
    }

    #[test]
    fn recovery_transition_invalidates_without_an_inactive_gap() {
        let gate = RelayGate::default();
        gate.set_enabled(true);
        let connected_generation = gate.generation();
        let mut state = gate.sender.subscribe();

        gate.invalidate_and_wait_for_activation();

        assert!(state.has_changed().unwrap());
        let recovery = *state.borrow_and_update();
        assert!(!recovery.enabled);
        assert!(recovery.awaiting_activation);
        assert_eq!(recovery.generation, connected_generation + 1);
    }

    #[tokio::test]
    async fn gate_disable_closes_udp_flow_and_reenable_accepts_a_fresh_one() {
        let echo = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let echo_address = echo.local_addr().unwrap();
        let echo_task = tokio::spawn(async move {
            let mut buffer = [0; 2048];
            loop {
                let (length, peer) = echo.recv_from(&mut buffer).await.unwrap();
                echo.send_to(&buffer[..length], peer).await.unwrap();
            }
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let gate = RelayGate::default();
        gate.set_enabled(true);
        let server = SocksServer::new(SocksConfig {
            bind: address,
            command_policy: SocksCommandPolicy::FwdUdpOnly,
            ..Default::default()
        })
        .unwrap()
        .with_relay_gate(gate.clone());
        let task = tokio::spawn(server.serve_on(listener));

        let mut first = connect_udp_through_socks(address).await;
        assert_udp_echo(&mut first, echo_address, b"before-sleep").await;

        gate.set_enabled(false);
        let closed = time::timeout(Duration::from_millis(500), first.read_u8())
            .await
            .expect("UDP flow stayed open after explicit suspend");
        assert!(closed.is_err());

        let mut rejected = TcpStream::connect(address).await.unwrap();
        rejected.write_all(&[5, 1, 0]).await.unwrap();
        let mut auth = [0; 2];
        let rejected_read =
            time::timeout(Duration::from_millis(500), rejected.read_exact(&mut auth))
                .await
                .expect("suspended UDP listener left a new connection pending");
        assert!(rejected_read.is_err());

        gate.set_enabled(true);
        let mut second = connect_udp_through_socks(address).await;
        assert_udp_echo(&mut second, echo_address, b"after-wake").await;

        task.abort();
        echo_task.abort();
    }

    async fn assert_command_rejected(policy: SocksCommandPolicy, command: u8) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = SocksServer::new(SocksConfig {
            bind: address,
            command_policy: policy,
            ..Default::default()
        })
        .unwrap();
        let task = tokio::spawn(server.serve_on(listener));

        let mut client = TcpStream::connect(address).await.unwrap();
        client.write_all(&[5, 1, 0]).await.unwrap();
        let mut auth = [0; 2];
        client.read_exact(&mut auth).await.unwrap();
        assert_eq!(auth, [5, 0]);
        client
            .write_all(&[5, command, 0, 1, 127, 0, 0, 1, 0, 9])
            .await
            .unwrap();
        let mut reply = [0; 10];
        client.read_exact(&mut reply).await.unwrap();
        assert_eq!(reply[1], 0x07);
        task.abort();
    }

    #[tokio::test]
    async fn split_listeners_reject_the_other_lane_command() {
        assert_command_rejected(SocksCommandPolicy::ConnectOnly, CMD_FWD_UDP).await;
        assert_command_rejected(SocksCommandPolicy::FwdUdpOnly, CMD_CONNECT).await;
    }

    #[tokio::test]
    async fn stalled_handshake_releases_the_bounded_connection_slot() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = SocksServer::new(SocksConfig {
            bind: address,
            max_connections: 1,
            handshake_timeout: Duration::from_millis(30),
            ..Default::default()
        })
        .unwrap();
        let task = tokio::spawn(server.serve_on(listener));

        let stalled = TcpStream::connect(address).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        let mut client = TcpStream::connect(address).await.unwrap();
        client.write_all(&[5, 1, 0]).await.unwrap();
        let mut auth = [0; 2];
        time::timeout(Duration::from_millis(100), client.read_exact(&mut auth))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(auth, [5, 0]);

        drop(stalled);
        task.abort();
    }

    #[test]
    fn connect_failures_report_their_own_category_and_only_local_ports() {
        let local = SocksError::Connect {
            kind: std::io::ErrorKind::ConnectionRefused,
            local_port: Some(38_850),
        };
        assert_eq!(
            local.diagnostic_fields(),
            json!({
                "category": "tcp_connect",
                "error_kind": "ConnectionRefused",
                "local_destination_port": 38_850,
            })
        );
        let remote = SocksError::Connect {
            kind: std::io::ErrorKind::TimedOut,
            local_port: None,
        };
        assert_eq!(
            remote.diagnostic_fields()["local_destination_port"],
            serde_json::Value::Null
        );
        assert_eq!(
            SocksError::Resolve(crate::udp::UdpRelayError::Unresolved("x".into()))
                .diagnostic_fields(),
            json!({"category": "resolve"})
        );
    }

    #[tokio::test]
    async fn refused_local_connect_replies_promptly_and_records_the_local_port() {
        let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let destination = closed.local_addr().unwrap();
        drop(closed);
        let directory = tempfile::tempdir().unwrap();
        let diagnostics = Diagnostics::open(directory.path()).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = SocksServer::new(SocksConfig {
            bind: address,
            ..Default::default()
        })
        .unwrap()
        .with_diagnostics(diagnostics);
        let task = tokio::spawn(server.serve_on(listener));

        let mut client = TcpStream::connect(address).await.unwrap();
        client.write_all(&[5, 1, 0]).await.unwrap();
        let mut auth = [0; 2];
        client.read_exact(&mut auth).await.unwrap();
        let mut request = vec![5, CMD_CONNECT, 0];
        request.extend_from_slice(&encode_endpoint(&Endpoint::Socket(destination)).unwrap());
        client.write_all(&request).await.unwrap();
        let mut reply = [0; 10];
        time::timeout(Duration::from_millis(500), client.read_exact(&mut reply))
            .await
            .expect("refused CONNECT must be answered promptly")
            .unwrap();
        assert_eq!(reply[1], 0x05);

        let expected = format!("\"local_destination_port\":{}", destination.port());
        time::timeout(Duration::from_secs(1), async {
            loop {
                let logged = std::fs::read_dir(directory.path())
                    .unwrap()
                    .filter_map(|entry| std::fs::read_to_string(entry.unwrap().path()).ok())
                    .collect::<String>();
                if logged.contains("\"category\":\"tcp_connect\"") && logged.contains(&expected) {
                    return;
                }
                time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("refused local CONNECT was not recorded as tcp_connect");
        task.abort();
    }

    async fn start_connect_server(
        diagnostics: Option<Diagnostics>,
    ) -> (SocketAddr, tokio::task::JoinHandle<Result<(), SocksError>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let mut server = SocksServer::new(SocksConfig {
            bind: address,
            ..Default::default()
        })
        .unwrap();
        if let Some(diagnostics) = diagnostics {
            server = server.with_diagnostics(diagnostics);
        }
        (address, tokio::spawn(server.serve_on(listener)))
    }

    async fn socks_connect_reply(address: SocketAddr, destination: SocketAddr) -> (TcpStream, u8) {
        let mut client = TcpStream::connect(address).await.unwrap();
        client.write_all(&[5, 1, 0]).await.unwrap();
        let mut auth = [0; 2];
        client.read_exact(&mut auth).await.unwrap();
        let mut request = vec![5, CMD_CONNECT, 0];
        request.extend_from_slice(&encode_endpoint(&Endpoint::Socket(destination)).unwrap());
        client.write_all(&request).await.unwrap();
        let mut reply = [0; 10];
        client.read_exact(&mut reply).await.unwrap();
        (client, reply[1])
    }

    /// Accepts one connection and stops listening, like VD's Streamer.
    async fn single_accept_streamer(
        port: u16,
    ) -> (SocketAddr, tokio::sync::oneshot::Receiver<TcpStream>) {
        let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, port))
            .await
            .unwrap();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            drop(listener);
            let _ = sender.send(stream);
        });
        (SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port), receiver)
    }

    async fn wait_for_logged(directory: &std::path::Path, needles: &[&str]) {
        time::timeout(Duration::from_secs(2), async {
            loop {
                let logged = std::fs::read_dir(directory)
                    .unwrap()
                    .filter_map(|entry| std::fs::read_to_string(entry.unwrap().path()).ok())
                    .collect::<String>();
                if logged
                    .lines()
                    .any(|line| needles.iter().all(|needle| line.contains(needle)))
                {
                    return;
                }
                time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("no log line contained {needles:?}"));
    }

    #[tokio::test]
    async fn refused_duplicate_serves_the_candidate_vd_writes_on() {
        let directory = tempfile::tempdir().unwrap();
        let diagnostics = Diagnostics::open(directory.path()).unwrap();
        let (destination, accepted) =
            single_accept_streamer(VIRTUAL_DESKTOP_STREAMER_PORTS[1]).await;
        let (address, task) = start_connect_server(Some(diagnostics)).await;

        let (mut first, reply) = socks_connect_reply(address, destination).await;
        assert_eq!(reply, 0);
        let mut streamer = accepted.await.unwrap();
        streamer.write_all(b"HELO").await.unwrap();
        let mut greeting = [0; 4];
        first.read_exact(&mut greeting).await.unwrap();
        assert_eq!(&greeting, b"HELO");

        let (mut second, reply) = socks_connect_reply(address, destination).await;
        assert_eq!(reply, 0, "a refused duplicate must stay connected");
        let mut replayed = [0; 4];
        second.read_exact(&mut replayed).await.unwrap();
        assert_eq!(&replayed, b"HELO");
        second.write_all(b"PICK").await.unwrap();
        let mut picked = [0; 4];
        streamer.read_exact(&mut picked).await.unwrap();
        assert_eq!(&picked, b"PICK");
        streamer.write_all(b"OK").await.unwrap();
        let mut answer = [0; 2];
        second.read_exact(&mut answer).await.unwrap();
        assert_eq!(&answer, b"OK");

        let mut unused = Vec::new();
        time::timeout(Duration::from_secs(1), first.read_to_end(&mut unused))
            .await
            .expect("the duplicate VD did not use stayed open")
            .unwrap();
        assert!(unused.is_empty());
        let (_late, reply) = socks_connect_reply(address, destination).await;
        assert_eq!(reply, 0x05, "duplicates after the choice are refused");
        wait_for_logged(
            directory.path(),
            &[
                "\"vd_streamer_candidates_settled\"",
                "\"candidates\":2",
                "\"chosen_arrival\":1",
                "\"outcome\":\"client_data\"",
            ],
        )
        .await;
        task.abort();
    }

    #[tokio::test]
    async fn surviving_duplicate_receives_the_whole_streamer_stream() {
        let (destination, accepted) =
            single_accept_streamer(VIRTUAL_DESKTOP_STREAMER_PORTS[2]).await;
        let (address, task) = start_connect_server(None).await;

        let (first, reply) = socks_connect_reply(address, destination).await;
        assert_eq!(reply, 0);
        let mut streamer = accepted.await.unwrap();
        let (mut second, reply) = socks_connect_reply(address, destination).await;
        assert_eq!(reply, 0);
        drop(first);

        let payload: Vec<u8> = (0..(4 * CANDIDATE_REPLAY_LIMIT + 7))
            .map(|index| (index % 251) as u8)
            .collect();
        let expected = payload.clone();
        let sender = tokio::spawn(async move {
            streamer.write_all(&payload).await.unwrap();
            streamer.shutdown().await.unwrap();
            streamer
        });
        let mut received = Vec::new();
        time::timeout(Duration::from_secs(5), second.read_to_end(&mut received))
            .await
            .expect("the surviving candidate never got the stream")
            .unwrap();
        assert_eq!(received, expected);
        drop(sender.await.unwrap());
        task.abort();
    }

    #[tokio::test]
    async fn undecided_duplicates_fall_back_to_the_first_arrival() {
        let directory = tempfile::tempdir().unwrap();
        let diagnostics = Diagnostics::open(directory.path()).unwrap();
        let (destination, accepted) =
            single_accept_streamer(VIRTUAL_DESKTOP_STREAMER_PORTS[3]).await;
        let (address, task) = start_connect_server(Some(diagnostics)).await;

        let (mut first, reply) = socks_connect_reply(address, destination).await;
        assert_eq!(reply, 0);
        let mut streamer = accepted.await.unwrap();
        let (mut second, reply) = socks_connect_reply(address, destination).await;
        assert_eq!(reply, 0);
        // The join lands right after the reply; let it settle before data flows.
        time::sleep(Duration::from_millis(50)).await;

        let payload = vec![7; 2 * CANDIDATE_REPLAY_LIMIT];
        streamer.write_all(&payload).await.unwrap();
        let mut received = vec![0; payload.len()];
        time::timeout(Duration::from_secs(5), first.read_exact(&mut received))
            .await
            .expect("the first arrival never got the stream")
            .unwrap();
        assert_eq!(received, payload);
        let mut duplicate = Vec::new();
        time::timeout(Duration::from_secs(1), second.read_to_end(&mut duplicate))
            .await
            .expect("the undecided duplicate stayed open")
            .unwrap();
        assert!(duplicate.len() <= CANDIDATE_REPLAY_LIMIT);

        first.write_all(b"late").await.unwrap();
        let mut late = [0; 4];
        streamer.read_exact(&mut late).await.unwrap();
        assert_eq!(&late, b"late");
        wait_for_logged(
            directory.path(),
            &[
                "\"vd_streamer_candidates_settled\"",
                "\"chosen_arrival\":0",
                "\"outcome\":\"timeout\"",
            ],
        )
        .await;
        task.abort();
    }

    proptest! {
        #[test]
        fn arbitrary_request_never_panics(input in prop::collection::vec(any::<u8>(), 0..1024)) {
            let _ = SocksRequest::decode(&input);
        }
    }
}
