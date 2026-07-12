use std::{
    net::{Ipv4Addr, SocketAddr},
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;
use tokio::{
    io::{self, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Semaphore,
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
        })
    }

    pub fn with_diagnostics(mut self, diagnostics: Diagnostics) -> Self {
        self.diagnostics = Some(diagnostics);
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
                let result = server.handle(stream).await;
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
        let (from_client, from_upstream) =
            io::copy_bidirectional(&mut client, &mut upstream).await?;
        self.stats
            .tcp_tx_bytes
            .fetch_add(from_client, Ordering::Relaxed);
        self.stats
            .tcp_rx_bytes
            .fetch_add(from_upstream, Ordering::Relaxed);
        Ok(())
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
            Self::Frame(_) => "udp_frame",
            Self::Udp(_) => "udp_relay",
            Self::Io(_) => "io",
        }
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

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
        task.abort();
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

    proptest! {
        #[test]
        fn arbitrary_request_never_panics(input in prop::collection::vec(any::<u8>(), 0..1024)) {
            let _ = SocksRequest::decode(&input);
        }
    }
}
