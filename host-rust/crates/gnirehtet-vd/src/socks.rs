use std::{
    io::ErrorKind,
    net::{Ipv4Addr, SocketAddr},
    pin::Pin,
    sync::{
        atomic::{AtomicU64, AtomicU8, AtomicUsize, Ordering},
        Arc,
    },
    task::{Context, Poll},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;
use tokio::{
    io::{self, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf},
    net::{TcpListener, TcpStream},
    sync::{watch, Semaphore},
    time,
};

use crate::{
    diagnostics::Diagnostics,
    udp::{
        decode_endpoint, encode_endpoint, relay_fwd_udp, Endpoint, FwdUdpConfig, HevFrameError,
        UdpStats,
    },
};

pub const SOCKS_VERSION: u8 = 0x05;
pub const AUTH_NONE: u8 = 0x00;
pub const AUTH_UNACCEPTABLE: u8 = 0xff;
pub const CMD_CONNECT: u8 = 0x01;
pub const CMD_FWD_UDP: u8 = 0x05;
const TCP_COPY_BUFFER_SIZE: usize = 64 * 1024;

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
    diagnostics: Option<Diagnostics>,
    relay_gate: Option<RelayGate>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct RelayGateState {
    enabled: bool,
    generation: u64,
}

/// Invalidates every active relay flow when the authenticated control session
/// degrades, while keeping the listeners alive for a fresh wake connection.
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
            if state.enabled == enabled {
                return false;
            }
            state.enabled = enabled;
            state.generation = state.generation.saturating_add(1);
            true
        });
    }

    #[cfg(test)]
    pub(crate) fn is_enabled(&self) -> bool {
        self.sender.borrow().enabled
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
        snapshot
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
                        continue;
                    }
                },
                None => None,
            };
            let permit = match permits.clone().try_acquire_owned() {
                Ok(permit) => permit,
                Err(_) => {
                    self.stats.rejected.fetch_add(1, Ordering::Relaxed);
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
                            result = server.handle(stream) => result,
                            _ = gate.wait_for_invalidation(generation) => Err(SocksError::RelayInactive),
                        }
                    }
                    None => server.handle(stream).await,
                };
                server.stats.active.fetch_sub(1, Ordering::Relaxed);
                drop(permit);
                if let (Err(error), Some(diagnostics)) = (result, &server.diagnostics) {
                    let _ = diagnostics.record(
                        "socks_connection_error",
                        json!({"category": error.category()}),
                    );
                }
            });
        }
    }

    async fn handle(&self, mut client: TcpStream) -> Result<(), SocksError> {
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
        let upstream = time::timeout(self.config.connect_timeout, async {
            let target = destination.resolve().await?;
            TcpStream::connect(target)
                .await
                .map_err(crate::udp::UdpRelayError::from)
        })
        .await;
        let mut upstream = match upstream {
            Ok(Ok(stream)) => stream,
            Ok(Err(error)) => {
                let _ = write_reply(
                    &mut client,
                    0x05,
                    Endpoint::Socket(SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 0)),
                )
                .await;
                return Err(error.into());
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
        client.set_nodelay(true)?;
        upstream.set_nodelay(true)?;
        let bound = upstream.local_addr()?;
        write_reply(&mut client, 0, Endpoint::Socket(bound)).await?;
        let mut report = TcpFlowReport::new(self.diagnostics.clone(), endpoint_kind(&destination));
        let result = relay_tcp(
            &mut client,
            &mut upstream,
            self.stats.clone(),
            report.client_to_network_bytes.clone(),
            report.network_to_client_bytes.clone(),
            report.terminal_direction.clone(),
        )
        .await;
        match &result {
            Ok(()) => report.set_close_reason("clean_eof"),
            Err(error) => report.set_close_reason(io_close_reason(error.kind())),
        }
        result.map_err(SocksError::from)
    }
}

fn endpoint_kind(endpoint: &Endpoint) -> &'static str {
    match endpoint {
        Endpoint::Socket(SocketAddr::V4(_)) => "ipv4",
        Endpoint::Socket(SocketAddr::V6(_)) => "ipv6",
        Endpoint::Domain(_, _) => "domain",
    }
}

fn io_close_reason(kind: ErrorKind) -> &'static str {
    match kind {
        ErrorKind::ConnectionReset => "connection_reset",
        ErrorKind::ConnectionAborted => "connection_aborted",
        ErrorKind::BrokenPipe => "broken_pipe",
        ErrorKind::NotConnected => "not_connected",
        ErrorKind::UnexpectedEof => "unexpected_eof",
        ErrorKind::TimedOut => "timed_out",
        _ => "io_error",
    }
}

async fn relay_tcp<C, U>(
    client: &mut C,
    upstream: &mut U,
    stats: SocksStats,
    client_to_network_bytes: Arc<AtomicU64>,
    network_to_client_bytes: Arc<AtomicU64>,
    terminal_direction: Arc<AtomicU8>,
) -> io::Result<()>
where
    C: AsyncRead + AsyncWrite + Unpin,
    U: AsyncRead + AsyncWrite + Unpin,
{
    let mut client = MeteredIo::new(
        client,
        TrafficCounter::new(
            stats.clone(),
            TcpDirection::ClientToNetwork,
            client_to_network_bytes,
            terminal_direction.clone(),
        ),
    );
    let mut upstream = MeteredIo::new(
        upstream,
        TrafficCounter::new(
            stats,
            TcpDirection::NetworkToClient,
            network_to_client_bytes,
            terminal_direction,
        ),
    );
    io::copy_bidirectional_with_sizes(
        &mut client,
        &mut upstream,
        TCP_COPY_BUFFER_SIZE,
        TCP_COPY_BUFFER_SIZE,
    )
    .await
    .map(|_| ())
}

#[derive(Clone, Copy)]
#[repr(u8)]
enum TcpDirection {
    ClientToNetwork = 1,
    NetworkToClient = 2,
}

impl TcpDirection {
    fn opposite(self) -> Self {
        match self {
            Self::ClientToNetwork => Self::NetworkToClient,
            Self::NetworkToClient => Self::ClientToNetwork,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::ClientToNetwork => "client_to_network",
            Self::NetworkToClient => "network_to_client",
        }
    }
}

struct TrafficCounter {
    stats: SocksStats,
    direction: TcpDirection,
    flow_bytes: Arc<AtomicU64>,
    terminal_direction: Arc<AtomicU8>,
}

impl TrafficCounter {
    fn new(
        stats: SocksStats,
        direction: TcpDirection,
        flow_bytes: Arc<AtomicU64>,
        terminal_direction: Arc<AtomicU8>,
    ) -> Self {
        Self {
            stats,
            direction,
            flow_bytes,
            terminal_direction,
        }
    }

    fn record(&self, bytes: usize) {
        let bytes = bytes as u64;
        if bytes == 0 {
            return;
        }
        match self.direction {
            TcpDirection::ClientToNetwork => {
                self.stats.tcp_tx_bytes.fetch_add(bytes, Ordering::Relaxed)
            }
            TcpDirection::NetworkToClient => {
                self.stats.tcp_rx_bytes.fetch_add(bytes, Ordering::Relaxed)
            }
        };
        self.flow_bytes.fetch_add(bytes, Ordering::Relaxed);
    }

    fn mark_read_terminal(&self) {
        self.mark_terminal(self.direction);
    }

    fn mark_write_terminal(&self) {
        self.mark_terminal(self.direction.opposite());
    }

    fn mark_terminal(&self, direction: TcpDirection) {
        let _ = self.terminal_direction.compare_exchange(
            0,
            direction as u8,
            Ordering::Release,
            Ordering::Relaxed,
        );
    }
}

struct MeteredIo<T> {
    inner: T,
    counter: TrafficCounter,
}

impl<T> MeteredIo<T> {
    fn new(inner: T, counter: TrafficCounter) -> Self {
        Self { inner, counter }
    }
}

impl<T> AsyncRead for MeteredIo<T>
where
    T: AsyncRead + Unpin,
{
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let before = buffer.filled().len();
        match Pin::new(&mut this.inner).poll_read(context, buffer) {
            Poll::Ready(Ok(())) => {
                let bytes = buffer.filled().len() - before;
                this.counter.record(bytes);
                if bytes == 0 {
                    this.counter.mark_read_terminal();
                }
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(error)) => {
                this.counter.mark_read_terminal();
                Poll::Ready(Err(error))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<T> AsyncWrite for MeteredIo<T>
where
    T: AsyncWrite + Unpin,
{
    fn poll_write(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        match Pin::new(&mut this.inner).poll_write(context, buffer) {
            Poll::Ready(Ok(0)) if !buffer.is_empty() => {
                this.counter.mark_write_terminal();
                Poll::Ready(Ok(0))
            }
            Poll::Ready(Err(error)) => {
                this.counter.mark_write_terminal();
                Poll::Ready(Err(error))
            }
            result => result,
        }
    }

    fn poll_flush(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        match Pin::new(&mut this.inner).poll_flush(context) {
            Poll::Ready(Err(error)) => {
                this.counter.mark_write_terminal();
                Poll::Ready(Err(error))
            }
            result => result,
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        match Pin::new(&mut this.inner).poll_shutdown(context) {
            Poll::Ready(Err(error)) => {
                this.counter.mark_write_terminal();
                Poll::Ready(Err(error))
            }
            result => result,
        }
    }
}

struct TcpFlowReport {
    diagnostics: Option<Diagnostics>,
    started_at: Instant,
    target_kind: &'static str,
    close_reason: &'static str,
    client_to_network_bytes: Arc<AtomicU64>,
    network_to_client_bytes: Arc<AtomicU64>,
    terminal_direction: Arc<AtomicU8>,
}

impl TcpFlowReport {
    fn new(diagnostics: Option<Diagnostics>, target_kind: &'static str) -> Self {
        Self {
            diagnostics,
            started_at: Instant::now(),
            target_kind,
            close_reason: "cancelled",
            client_to_network_bytes: Arc::new(AtomicU64::new(0)),
            network_to_client_bytes: Arc::new(AtomicU64::new(0)),
            terminal_direction: Arc::new(AtomicU8::new(0)),
        }
    }

    fn set_close_reason(&mut self, close_reason: &'static str) {
        self.close_reason = close_reason;
    }

    fn terminal_direction(&self) -> &'static str {
        match self.terminal_direction.load(Ordering::Acquire) {
            value if value == TcpDirection::ClientToNetwork as u8 => {
                TcpDirection::ClientToNetwork.label()
            }
            value if value == TcpDirection::NetworkToClient as u8 => {
                TcpDirection::NetworkToClient.label()
            }
            _ => "external",
        }
    }
}

impl Drop for TcpFlowReport {
    fn drop(&mut self) {
        let Some(diagnostics) = &self.diagnostics else {
            return;
        };
        let duration_ms = self.started_at.elapsed().as_millis().min(u64::MAX as u128) as u64;
        let _ = diagnostics.record(
            "tcp_flow_closed",
            json!({
                "duration_ms": duration_ms,
                "target_kind": self.target_kind,
                "client_to_network_bytes": self.client_to_network_bytes.load(Ordering::Relaxed),
                "network_to_client_bytes": self.network_to_client_bytes.load(Ordering::Relaxed),
                "close_reason": self.close_reason,
                "terminal_direction": self.terminal_direction(),
            }),
        );
    }
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
            Self::HandshakeTimeout => "handshake_timeout",
            Self::RelayInactive => "relay_inactive",
            Self::Frame(_) => "udp_frame",
            Self::Udp(_) => "udp_relay",
            Self::Io(_) => "io",
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
    async fn connect_proxies_bidirectionally() {
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
        client.write_all(b"ping").await.unwrap();
        let mut echoed = [0; 4];
        client.read_exact(&mut echoed).await.unwrap();
        assert_eq!(&echoed, b"ping");
        assert_eq!(stats.stats().tcp_tx_bytes, 4);
        assert_eq!(stats.stats().tcp_rx_bytes, 4);
        assert_eq!(stats.stats().active_connections, 1);

        client.shutdown().await.unwrap();
        time::timeout(Duration::from_millis(500), async {
            while stats.stats().active_connections != 0 {
                time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(stats.stats().tcp_tx_bytes, 4);
        assert_eq!(stats.stats().tcp_rx_bytes, 4);
        task.abort();
    }

    #[tokio::test]
    async fn explicit_suspend_closes_old_tcp_flows_and_reopens_the_same_listener() {
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
        assert_eq!(stats.stats().tcp_tx_bytes, 12);
        assert_eq!(stats.stats().tcp_rx_bytes, 12);

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
        assert_eq!(stats.stats().tcp_tx_bytes, 12);
        assert_eq!(stats.stats().tcp_rx_bytes, 12);

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
    async fn explicit_suspend_closes_udp_flow_and_wake_accepts_a_fresh_one() {
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

    struct PartialThenError {
        payload: Option<Vec<u8>>,
    }

    impl AsyncRead for PartialThenError {
        fn poll_read(
            self: Pin<&mut Self>,
            _context: &mut Context<'_>,
            buffer: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            let this = self.get_mut();
            if let Some(payload) = this.payload.take() {
                buffer.put_slice(&payload);
                return Poll::Ready(Ok(()));
            }
            Poll::Ready(Err(std::io::Error::new(
                ErrorKind::ConnectionReset,
                "test reset",
            )))
        }
    }

    impl AsyncWrite for PartialThenError {
        fn poll_write(
            self: Pin<&mut Self>,
            _context: &mut Context<'_>,
            buffer: &[u8],
        ) -> Poll<io::Result<usize>> {
            Poll::Ready(Ok(buffer.len()))
        }

        fn poll_flush(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn partial_bytes_survive_io_error() {
        let stats = SocksStats::default();
        let client_to_network = Arc::new(AtomicU64::new(0));
        let network_to_client = Arc::new(AtomicU64::new(0));
        let terminal_direction = Arc::new(AtomicU8::new(0));
        let mut client = PartialThenError {
            payload: Some(b"partial".to_vec()),
        };
        let mut upstream = tokio::io::join(tokio::io::empty(), tokio::io::sink());

        let error = relay_tcp(
            &mut client,
            &mut upstream,
            stats.clone(),
            client_to_network.clone(),
            network_to_client.clone(),
            terminal_direction.clone(),
        )
        .await
        .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::ConnectionReset);
        assert_eq!(stats.snapshot().tcp_tx_bytes, 7);
        assert_eq!(stats.snapshot().tcp_rx_bytes, 0);
        assert_eq!(client_to_network.load(Ordering::Relaxed), 7);
        assert_eq!(network_to_client.load(Ordering::Relaxed), 0);
        assert_eq!(
            terminal_direction.load(Ordering::Acquire),
            TcpDirection::ClientToNetwork as u8
        );
    }

    #[test]
    fn first_terminal_direction_is_stable_and_bounded() {
        let stats = SocksStats::default();
        let terminal_direction = Arc::new(AtomicU8::new(0));
        let network_to_client = TrafficCounter::new(
            stats.clone(),
            TcpDirection::NetworkToClient,
            Arc::new(AtomicU64::new(0)),
            terminal_direction.clone(),
        );
        let client_to_network = TrafficCounter::new(
            stats,
            TcpDirection::ClientToNetwork,
            Arc::new(AtomicU64::new(0)),
            terminal_direction.clone(),
        );

        network_to_client.mark_read_terminal();
        client_to_network.mark_read_terminal();

        assert_eq!(
            terminal_direction.load(Ordering::Acquire),
            TcpDirection::NetworkToClient as u8
        );
        assert_eq!(TcpDirection::NetworkToClient.label(), "network_to_client");
        assert_eq!(TcpDirection::ClientToNetwork.label(), "client_to_network");
    }

    #[tokio::test]
    async fn partial_bytes_survive_relay_cancellation() {
        let stats = SocksStats::default();
        let task_stats = stats.clone();
        let client_to_network = Arc::new(AtomicU64::new(0));
        let network_to_client = Arc::new(AtomicU64::new(0));
        let terminal_direction = Arc::new(AtomicU8::new(0));
        let task_client_to_network = client_to_network.clone();
        let task_network_to_client = network_to_client.clone();
        let task_terminal_direction = terminal_direction.clone();
        let (mut client_peer, mut client_relay) = tokio::io::duplex(1024);
        let (mut upstream_relay, mut upstream_peer) = tokio::io::duplex(1024);
        let task = tokio::spawn(async move {
            relay_tcp(
                &mut client_relay,
                &mut upstream_relay,
                task_stats,
                task_client_to_network,
                task_network_to_client,
                task_terminal_direction,
            )
            .await
        });

        client_peer.write_all(b"before-stop").await.unwrap();
        let mut received = [0; 11];
        upstream_peer.read_exact(&mut received).await.unwrap();
        assert_eq!(&received, b"before-stop");
        assert_eq!(stats.snapshot().tcp_tx_bytes, 11);

        task.abort();
        let _ = task.await;
        assert_eq!(stats.snapshot().tcp_tx_bytes, 11);
        assert_eq!(client_to_network.load(Ordering::Relaxed), 11);
        assert_eq!(network_to_client.load(Ordering::Relaxed), 0);
        assert_eq!(terminal_direction.load(Ordering::Acquire), 0);
    }

    #[test]
    fn flow_report_is_bounded_and_contains_no_endpoint() {
        let directory = tempfile::tempdir().unwrap();
        let diagnostics = Diagnostics::open(directory.path()).unwrap();
        {
            let mut report = TcpFlowReport::new(Some(diagnostics), "domain");
            report.client_to_network_bytes.store(123, Ordering::Relaxed);
            report.network_to_client_bytes.store(456, Ordering::Relaxed);
            report.set_close_reason("clean_eof");
        }

        let encoded =
            std::fs::read_to_string(directory.path().join("gnirehtet-vd.0.jsonl")).unwrap();
        let event: serde_json::Value = serde_json::from_str(encoded.trim()).unwrap();
        assert_eq!(event["kind"], "tcp_flow_closed");
        assert_eq!(event["fields"]["target_kind"], "domain");
        assert_eq!(event["fields"]["client_to_network_bytes"], 123);
        assert_eq!(event["fields"]["network_to_client_bytes"], 456);
        assert_eq!(event["fields"]["close_reason"], "clean_eof");
        assert_eq!(event["fields"]["terminal_direction"], "external");
        assert!(event["fields"]["duration_ms"].as_u64().is_some());
        assert!(!encoded.contains("address"));
        assert!(!encoded.contains("endpoint"));
    }

    proptest! {
        #[test]
        fn arbitrary_request_never_panics(input in prop::collection::vec(any::<u8>(), 0..1024)) {
            let _ = SocksRequest::decode(&input);
        }
    }
}
