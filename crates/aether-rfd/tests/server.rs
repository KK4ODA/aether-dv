//! aether-rfd server over real TCP (127.0.0.1). Validation level: Simulated
//! (our own clients; no legacy D-RATS involved yet).

use std::time::Duration;

use aether_ddt2::{wrap, EscapeSet, Frame, PayloadEncoding};
use aether_rfd::server::{Server, ServerConfig};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::time::timeout;

fn wire(f: &Frame) -> Vec<u8> {
    wrap(
        &f.encode(PayloadEncoding::Never).unwrap(),
        &EscapeSet::legacy(),
    )
    .unwrap()
}

async fn connect_open(addr: std::net::SocketAddr) -> BufReader<TcpStream> {
    let s = TcpStream::connect(addr).await.unwrap();
    let mut r = BufReader::new(s);
    let mut line = String::new();
    r.read_line(&mut line).await.unwrap();
    assert_eq!(line, "100 Authentication not required\r\n");
    r
}

async fn read_exact_n(r: &mut BufReader<TcpStream>, n: usize) -> Vec<u8> {
    let mut buf = vec![0; n];
    timeout(Duration::from_secs(5), r.read_exact(&mut buf))
        .await
        .expect("timed out")
        .unwrap();
    buf
}

async fn nothing_arrives(r: &mut BufReader<TcpStream>) -> bool {
    let mut b = [0u8; 1];
    timeout(Duration::from_millis(300), r.read(&mut b))
        .await
        .is_err()
}

#[tokio::test]
async fn open_server_forwards_original_bytes_to_others_only() {
    let srv = Server::start("127.0.0.1:0", ServerConfig::default())
        .await
        .unwrap();
    let mut a = connect_open(srv.local_addr).await;
    let mut b = connect_open(srv.local_addr).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    // escaped with the wider JARL set: the relay must not normalise it
    let f = Frame::new(0, 1, 0, "KK4ODA", "CQCQCQ", b"vvv net check".to_vec());
    let w = wrap(
        &f.encode(PayloadEncoding::Never).unwrap(),
        &EscapeSet::legacy_plus_jarl(),
    )
    .unwrap();
    a.get_mut().write_all(&w).await.unwrap();
    assert_eq!(read_exact_n(&mut b, w.len()).await, w);
    assert!(
        nothing_arrives(&mut a).await,
        "sender must not get its own frame back"
    );
    srv.shutdown().await;
}

#[tokio::test]
async fn authentication_accepts_and_rejects() {
    let cfg = ServerConfig {
        users: vec![("KK4ODA".into(), "pw".into())],
        ..Default::default()
    };
    let srv = Server::start("127.0.0.1:0", cfg).await.unwrap();
    for (pass, expect) in [
        ("pw", "200 Authorized\r\n"),
        ("bad", "500 Not authorized\r\n"),
    ] {
        let mut r = BufReader::new(TcpStream::connect(srv.local_addr).await.unwrap());
        let mut line = String::new();
        r.read_line(&mut line).await.unwrap();
        assert_eq!(line, "101 Authorization required\r\n");
        r.get_mut().write_all(b"USER KK4ODA\r\n").await.unwrap();
        line.clear();
        r.read_line(&mut line).await.unwrap();
        assert_eq!(line, "102 USER okay\r\n");
        r.get_mut()
            .write_all(format!("PASS {pass}\r\n").as_bytes())
            .await
            .unwrap();
        line.clear();
        r.read_line(&mut line).await.unwrap();
        assert_eq!(line, expect);
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        srv.counters
            .auth_failures
            .load(std::sync::atomic::Ordering::Relaxed),
        1
    );
    srv.shutdown().await;
}

#[tokio::test]
async fn aether_rfl_client_completes_auth_against_aether_rfd() {
    use aether_rfl::{Action, Client, Config, State, UpReason};
    let cfg = ServerConfig {
        users: vec![("KK4ODA".into(), "letmein".into())],
        ..Default::default()
    };
    let srv = Server::start("127.0.0.1:0", cfg).await.unwrap();
    let mut c = {
        let mut cc = Config::new("KK4ODA");
        cc.password = Some("letmein".into());
        Client::new(cc)
    };
    assert_eq!(c.start(), vec![Action::Connect]);
    let mut s = TcpStream::connect(srv.local_addr).await.unwrap();
    c.on_connected(0);
    let mut buf = [0u8; 256];
    let mut up = None;
    for t in 1..20 {
        let n = timeout(Duration::from_secs(2), s.read(&mut buf))
            .await
            .unwrap()
            .unwrap();
        for a in c.on_bytes(t, &buf[..n]) {
            match a {
                Action::Send(b) => s.write_all(&b).await.unwrap(),
                Action::Up(r) => up = Some(r),
                _ => {}
            }
        }
        if up.is_some() {
            break;
        }
    }
    assert_eq!(up, Some(UpReason::Authenticated));
    assert_eq!(c.state(), State::Up);
    srv.shutdown().await;
}

#[tokio::test]
async fn warmup_frames_are_dropped() {
    let srv = Server::start("127.0.0.1:0", ServerConfig::default())
        .await
        .unwrap();
    let mut a = connect_open(srv.local_addr).await;
    let mut b = connect_open(srv.local_addr).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    a.get_mut()
        .write_all(&wire(&Frame::new(0, 0, 254, "!", "!", vec![1; 16])))
        .await
        .unwrap();
    assert!(nothing_arrives(&mut b).await);
    srv.shutdown().await;
}

#[tokio::test]
async fn stalled_client_does_not_hold_up_others() {
    // 2 MB/s input limit keeps the test quick while still being thousands of
    // times faster than any RF bridge; the queue budget exceeds the burst.
    let cfg = ServerConfig {
        client_queue_bytes: 512 * 1024,
        client_rate: 2_000_000,
        client_burst: 128 * 1024,
        ..Default::default()
    };
    let srv = Server::start("127.0.0.1:0", cfg).await.unwrap();
    let mut sender = connect_open(srv.local_addr).await;
    let _stalled = connect_open(srv.local_addr).await; // never reads
    let mut healthy = connect_open(srv.local_addr).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let n_frames = 6_000usize;
    let mut expected = 0usize;
    let reader = tokio::spawn(async move {
        let mut got = 0usize;
        let mut buf = vec![0u8; 65536];
        while got < 6_000 * 1_060 {
            match timeout(Duration::from_secs(10), healthy.read(&mut buf)).await {
                Ok(Ok(0)) | Err(_) => break,
                Ok(Ok(n)) => got += n,
                Ok(Err(_)) => break,
            }
        }
        got
    });
    for i in 0..n_frames {
        let mut body = format!("{i:06}").into_bytes();
        body.resize(1_000, b'x');
        let w = wire(&Frame::new(0, 1, 0, "KK4ODA", "CQCQCQ", body));
        expected += w.len();
        sender.get_mut().write_all(&w).await.unwrap();
    }
    let got = reader.await.unwrap();
    let drops = srv
        .counters
        .queue_drops
        .load(std::sync::atomic::Ordering::Relaxed);
    println!(
        "healthy client received {got} of {expected} bytes; drops for stalled client: {drops}"
    );
    assert_eq!(got, expected, "healthy client must receive everything");
    assert!(
        drops > 0,
        "stalled client's bounded queue should have dropped frames"
    );
    srv.shutdown().await;
}

#[tokio::test]
async fn shutdown_closes_clients() {
    let srv = Server::start("127.0.0.1:0", ServerConfig::default())
        .await
        .unwrap();
    let mut a = connect_open(srv.local_addr).await;
    srv.shutdown().await;
    let mut b = [0u8; 1];
    let r = timeout(Duration::from_secs(2), a.read(&mut b))
        .await
        .expect("close not seen");
    assert!(matches!(r, Ok(0) | Err(_)));
}
