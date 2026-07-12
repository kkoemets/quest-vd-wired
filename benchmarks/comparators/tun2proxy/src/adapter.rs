use std::{
    io,
    net::SocketAddr,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Poll},
};

use anyhow::{Context as _, Result, bail, ensure};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tun2proxy::{ArgDns, ArgProxy, Args, CancellationToken, ProxyType};

#[derive(Debug, Copy, Clone, Eq, PartialEq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum UdpMode {
    #[value(name = "socks5-udp-associate")]
    #[serde(rename = "socks5-udp-associate")]
    Socks5UdpAssociate,
    #[value(name = "udpgw-over-tcp")]
    #[serde(rename = "udpgw-over-tcp")]
    UdpGwOverTcp,
}

#[derive(Debug, Clone)]
pub struct AdapterConfig {
    pub tun_fd: i32,
    pub close_fd_on_drop: bool,
    pub packet_information: bool,
    pub proxy: String,
    pub udp_mode: UdpMode,
    pub udpgw_server: Option<SocketAddr>,
    pub mtu: u16,
    pub tcp_timeout_seconds: u64,
    pub udp_timeout_seconds: u64,
    pub max_sessions: usize,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct AdapterOutcome {
    pub forward_bytes: u64,
    pub reverse_bytes: u64,
    pub sessions_at_exit: usize,
}

impl AdapterConfig {
    pub fn validate(&self) -> Result<ArgProxy> {
        ensure!(self.tun_fd >= 0, "TUN fd must be non-negative");
        ensure!(
            (576..=9_000).contains(&self.mtu),
            "MTU must be within 576..=9000"
        );
        ensure!(self.tcp_timeout_seconds > 0, "TCP timeout must be positive");
        ensure!(self.udp_timeout_seconds > 0, "UDP timeout must be positive");
        ensure!(
            (1..=65_535).contains(&self.max_sessions),
            "max sessions must be within 1..=65535"
        );

        let proxy = ArgProxy::try_from(self.proxy.as_str()).context("invalid proxy URL")?;
        ensure!(
            proxy.proxy_type == ProxyType::Socks5,
            "comparator requires a SOCKS5 proxy"
        );
        ensure!(
            proxy.addr.ip().is_loopback(),
            "comparator proxy must resolve to loopback"
        );

        match (self.udp_mode, self.udpgw_server) {
            (UdpMode::Socks5UdpAssociate, None) => {}
            (UdpMode::Socks5UdpAssociate, Some(_)) => {
                bail!("UdpGW server must be omitted in SOCKS5 UDP associate mode")
            }
            (UdpMode::UdpGwOverTcp, Some(server)) if server.ip().is_loopback() => {}
            (UdpMode::UdpGwOverTcp, Some(_)) => bail!("UdpGW server must be loopback-only"),
            (UdpMode::UdpGwOverTcp, None) => bail!("UdpGW-over-TCP mode requires a UdpGW server"),
        }
        Ok(proxy)
    }

    fn upstream_args(&self) -> Result<Args> {
        let proxy = self.validate()?;
        Ok(Args {
            proxy,
            setup: false,
            dns: ArgDns::Direct,
            mtu: self.mtu,
            tcp_timeout: self.tcp_timeout_seconds,
            udp_timeout: self.udp_timeout_seconds,
            max_sessions: self.max_sessions,
            udpgw_server: self.udpgw_server,
            udpgw_connections: self.udpgw_server.map(|_| 5),
            udpgw_keepalive: self.udpgw_server.map(|_| 30),
            ..Args::default()
        })
    }
}

#[derive(Default)]
struct ByteCounters {
    forward: AtomicU64,
    reverse: AtomicU64,
}

struct MeteredTun<D> {
    inner: D,
    counters: Arc<ByteCounters>,
}

impl<D: AsyncRead + Unpin> AsyncRead for MeteredTun<D> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buffer.filled().len();
        let result = Pin::new(&mut self.inner).poll_read(cx, buffer);
        if matches!(result, Poll::Ready(Ok(()))) {
            let read = buffer.filled().len().saturating_sub(before);
            self.counters
                .forward
                .fetch_add(read as u64, Ordering::Relaxed);
        }
        result
    }
}

impl<D: AsyncWrite + Unpin> AsyncWrite for MeteredTun<D> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        let result = Pin::new(&mut self.inner).poll_write(cx, buffer);
        if let Poll::Ready(Ok(written)) = result {
            self.counters
                .reverse
                .fetch_add(written as u64, Ordering::Relaxed);
        }
        result
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(unix)]
pub async fn run_adapter(
    config: AdapterConfig,
    shutdown: CancellationToken,
) -> Result<AdapterOutcome> {
    let args = config.upstream_args()?;
    let mut tun_config = tun::Configuration::default();
    tun_config
        .mtu(config.mtu)
        .raw_fd(config.tun_fd)
        .close_fd_on_drop(config.close_fd_on_drop);

    #[cfg(target_os = "linux")]
    tun_config.platform_config(|platform| {
        #[allow(deprecated)]
        platform.packet_information(config.packet_information);
        platform.ensure_root_privileges(false);
    });
    #[cfg(any(target_os = "ios", target_os = "macos"))]
    tun_config.platform_config(|platform| {
        platform.packet_information(config.packet_information);
    });

    let device = tun::create_as_async(&tun_config).context("could not adopt TUN fd")?;
    let counters = Arc::new(ByteCounters::default());
    let metered = MeteredTun {
        inner: device,
        counters: counters.clone(),
    };
    let sessions_at_exit = tun2proxy::run(metered, config.mtu, args, shutdown)
        .await
        .context("tun2proxy engine failed")?;
    Ok(AdapterOutcome {
        forward_bytes: counters.forward.load(Ordering::Relaxed),
        reverse_bytes: counters.reverse.load(Ordering::Relaxed),
        sessions_at_exit,
    })
}

#[cfg(not(unix))]
pub async fn run_adapter(
    _config: AdapterConfig,
    _shutdown: CancellationToken,
) -> Result<AdapterOutcome> {
    bail!("raw Android TUN fd adapter is available only on Unix-family targets")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn valid_config(mode: UdpMode) -> AdapterConfig {
        AdapterConfig {
            tun_fd: 42,
            close_fd_on_drop: false,
            packet_information: false,
            proxy: "socks5://127.0.0.1:31416".into(),
            udp_mode: mode,
            udpgw_server: None,
            mtu: 1_500,
            tcp_timeout_seconds: 600,
            udp_timeout_seconds: 10,
            max_sessions: 256,
        }
    }

    #[test]
    fn accepts_stream_udp_mode_with_loopback_server() {
        let mut config = valid_config(UdpMode::UdpGwOverTcp);
        config.udpgw_server = Some("127.0.0.1:7300".parse().unwrap());
        assert!(config.validate().is_ok());
        assert!(!config.close_fd_on_drop);
    }

    #[test]
    fn rejects_udpgw_without_server() {
        let config = valid_config(UdpMode::UdpGwOverTcp);
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_non_loopback_proxy() {
        let mut config = valid_config(UdpMode::Socks5UdpAssociate);
        config.proxy = "socks5://192.0.2.1:1080".into();
        assert!(config.validate().is_err());
    }

    #[tokio::test]
    async fn meters_both_tun_directions() {
        let (device, mut peer) = tokio::io::duplex(1_024);
        let counters = Arc::new(ByteCounters::default());
        let mut metered = MeteredTun {
            inner: device,
            counters: counters.clone(),
        };

        peer.write_all(b"forward").await.unwrap();
        let mut incoming = [0_u8; 7];
        metered.read_exact(&mut incoming).await.unwrap();
        assert_eq!(&incoming, b"forward");

        metered.write_all(b"reverse").await.unwrap();
        let mut outgoing = [0_u8; 7];
        peer.read_exact(&mut outgoing).await.unwrap();
        assert_eq!(&outgoing, b"reverse");
        assert_eq!(counters.forward.load(Ordering::Relaxed), 7);
        assert_eq!(counters.reverse.load(Ordering::Relaxed), 7);
    }

    #[tokio::test]
    async fn upstream_engine_honors_pre_cancelled_token() {
        let (device, _peer) = tokio::io::duplex(1_024);
        let shutdown = CancellationToken::new();
        shutdown.cancel();
        let completed = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            tun2proxy::run(device, 1_500, Args::default(), shutdown),
        )
        .await
        .expect("cancelled engine did not stop");
        assert_eq!(completed.unwrap(), 0);
    }
}
