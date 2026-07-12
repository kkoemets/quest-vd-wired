use std::sync::Arc;
use std::time::{Duration, Instant};

use gnirehtet_vd::{
    socks::{SocksCommandPolicy, SocksConfig, SocksServer, CMD_FWD_UDP},
    udp::{Endpoint, FwdUdpConfig, HevUdpFrame},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket},
    sync::Semaphore,
};

async fn start_echo_and_proxy(max_queue_age: Duration) -> (std::net::SocketAddr, TcpStream) {
    let echo = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let echo_address = echo.local_addr().unwrap();
    tokio::spawn(async move {
        let mut buffer = [0; 65_507];
        loop {
            let Ok((length, peer)) = echo.recv_from(&mut buffer).await else {
                return;
            };
            if echo.send_to(&buffer[..length], peer).await.is_err() {
                return;
            }
        }
    });

    let client = start_proxy(max_queue_age).await;
    (echo_address, client)
}

async fn start_proxy(max_queue_age: Duration) -> TcpStream {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_address = listener.local_addr().unwrap();
    let server = SocksServer::new(SocksConfig {
        bind: proxy_address,
        command_policy: SocksCommandPolicy::FwdUdpOnly,
        fwd_udp: FwdUdpConfig {
            queue_capacity: 512,
            max_queue_age,
            idle_timeout: Duration::from_secs(30),
            association_byte_budget: 2 * 1024 * 1024,
        },
        ..Default::default()
    })
    .unwrap();
    tokio::spawn(server.serve_on(listener));

    let mut client = TcpStream::connect(proxy_address).await.unwrap();
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

#[tokio::test]
async fn hev_fwd_udp_echoes_over_the_socks_stream() {
    let (echo_address, mut client) = start_echo_and_proxy(Duration::from_millis(10)).await;
    let sent = HevUdpFrame {
        endpoint: Endpoint::Socket(echo_address),
        payload: b"virtual-desktop-datagram".to_vec(),
    };
    sent.write_to(&mut client).await.unwrap();
    let received = HevUdpFrame::read_from(&mut client).await.unwrap();
    assert_eq!(received.payload, sent.payload);
    assert_eq!(received.endpoint, Endpoint::Socket(echo_address));
}

#[tokio::test]
async fn hev_fwd_udp_accepts_full_cone_replies_from_a_second_endpoint() {
    let primary = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let primary_address = primary.local_addr().unwrap();
    let secondary = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let secondary_address = secondary.local_addr().unwrap();
    tokio::spawn(async move {
        let mut buffer = [0; 64];
        let (length, relay_address) = primary.recv_from(&mut buffer).await.unwrap();
        assert_eq!(&buffer[..length], b"open-cone");
        primary.send_to(b"primary", relay_address).await.unwrap();
        secondary
            .send_to(b"secondary", relay_address)
            .await
            .unwrap();
    });

    let mut client = start_proxy(Duration::from_millis(50)).await;
    HevUdpFrame {
        endpoint: Endpoint::Socket(primary_address),
        payload: b"open-cone".to_vec(),
    }
    .write_to(&mut client)
    .await
    .unwrap();

    let mut replies = Vec::new();
    for _ in 0..2 {
        replies.push(
            tokio::time::timeout(Duration::from_secs(1), HevUdpFrame::read_from(&mut client))
                .await
                .unwrap()
                .unwrap(),
        );
    }
    assert!(replies.iter().any(|reply| {
        reply.endpoint == Endpoint::Socket(primary_address) && reply.payload == b"primary"
    }));
    assert!(replies.iter().any(|reply| {
        reply.endpoint == Endpoint::Socket(secondary_address) && reply.payload == b"secondary"
    }));
}

/// Synthetic host-side ceiling harness. It is ignored because release gating
/// requires a physical Quest/ADB run for 60 minutes; this loopback pass only
/// catches large regressions in framing, queues, and socket handling.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "manual performance harness"]
async fn hev_fwd_udp_synthetic_throughput() {
    let packet_count = std::env::var("GNR4_BENCH_PACKETS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(50_000usize);
    let payload_size = 1_400usize;
    let (echo_address, client) = start_echo_and_proxy(Duration::from_secs(10)).await;
    let (mut reader, mut writer) = client.into_split();
    let payload = vec![0x5a; payload_size];
    let window = Arc::new(Semaphore::new(256));
    let started = Instant::now();
    let send_window = window.clone();
    let send = tokio::spawn(async move {
        for _ in 0..packet_count {
            send_window.clone().acquire_owned().await.unwrap().forget();
            HevUdpFrame {
                endpoint: Endpoint::Socket(echo_address),
                payload: payload.clone(),
            }
            .write_to(&mut writer)
            .await
            .unwrap();
        }
        writer
    });
    let mut received = 0usize;
    let mut timed_out = false;
    while received < packet_count {
        let frame =
            tokio::time::timeout(Duration::from_secs(2), HevUdpFrame::read_from(&mut reader)).await;
        let Ok(Ok(frame)) = frame else {
            timed_out = true;
            break;
        };
        assert_eq!(frame.payload.len(), payload_size);
        received += 1;
        window.add_permits(1);
    }
    if timed_out {
        send.abort();
        let _ = send.await;
    } else {
        let writer = send.await.unwrap();
        drop(writer);
    }
    let elapsed = started.elapsed();
    let megabits = received as f64 * payload_size as f64 * 8.0 / 1_000_000.0;
    let loss_percent = 100.0 * (packet_count - received) as f64 / packet_count as f64;
    eprintln!(
        "HEV FWD UDP loopback: {:.1} Mbit/s, {}/{} replies, {:.3}% loss in {:.3}s",
        megabits / elapsed.as_secs_f64(),
        received,
        packet_count,
        loss_percent,
        elapsed.as_secs_f64()
    );
}
