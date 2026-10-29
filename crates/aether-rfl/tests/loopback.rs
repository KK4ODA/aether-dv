//! Drive the SM-6 client over real TCP (127.0.0.1) against a mock server that
//! reproduces d-rats_repeater.py's handshake replies (@d0e7a39 lines 313-370).

use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::{Duration, Instant};

use aether_rfl::{Action, Client, Config, State, UpReason};

#[derive(Clone, Copy)]
enum Mode {
    Open,
    Auth,
    Silent,
}

/// One-connection mock: performs the legacy handshake, then echoes a frame.
fn mock_server(mode: Mode) -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        let mut r = BufReader::new(s.try_clone().unwrap());
        let mut line = String::new();
        match mode {
            Mode::Open => s.write_all(b"100 Authentication not required\r\n").unwrap(),
            Mode::Silent => {}
            Mode::Auth => {
                s.write_all(b"101 Authorization required\r\n").unwrap();
                r.read_line(&mut line).unwrap();
                assert!(line.starts_with("USER "), "{line}");
                s.write_all(b"102 USER okay\r\n").unwrap();
                line.clear();
                r.read_line(&mut line).unwrap();
                if line.trim_end() == "PASS letmein" {
                    s.write_all(b"200 Authorized\r\n").unwrap();
                } else {
                    s.write_all(b"500 Not authorized\r\n").unwrap();
                    return;
                }
            }
        }
        s.write_all(b"[SOB]frame-from-server[EOB]").unwrap();
        thread::sleep(Duration::from_millis(300));
    });
    port
}

/// Minimal blocking driver: executes actions, feeds events back.
fn drive(cfg: Config, port: u16, until: Duration) -> (Client, Vec<Action>, Vec<u8>) {
    let t0 = Instant::now();
    let now = || t0.elapsed().as_millis() as u64;
    let mut c = Client::new(cfg);
    let mut log = Vec::new();
    let mut stream: Option<TcpStream> = None;
    let mut data = Vec::new();
    let mut pending = c.start();
    while t0.elapsed() < until {
        for a in std::mem::take(&mut pending) {
            log.push(a.clone());
            match a {
                Action::Connect => match TcpStream::connect(("127.0.0.1", port)) {
                    Ok(s) => {
                        s.set_read_timeout(Some(Duration::from_millis(20))).unwrap();
                        stream = Some(s);
                        pending.extend(c.on_connected(now()));
                    }
                    Err(_) => pending.extend(c.on_connect_failed(now())),
                },
                Action::Send(b) => stream.as_mut().unwrap().write_all(&b).unwrap(),
                Action::Deliver(b) => data.extend(b),
                Action::Close => stream = None,
                _ => {}
            }
        }
        if c.state() == State::AuthFailed || (c.state() == State::Up && data.ends_with(b"[EOB]")) {
            break;
        }
        if let Some(s) = stream.as_mut() {
            let mut buf = [0u8; 512];
            match s.read(&mut buf) {
                Ok(0) => pending.extend(c.on_disconnected(now())),
                Ok(n) => pending.extend(c.on_bytes(now(), &buf[..n])),
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(_) => pending.extend(c.on_disconnected(now())),
            }
        }
        pending.extend(c.on_timeout(now()));
    }
    (c, log, data)
}

#[test]
fn open_ratflector() {
    let (c, log, data) = drive(
        Config::new("KK4ODA"),
        mock_server(Mode::Open),
        Duration::from_secs(3),
    );
    assert_eq!(c.state(), State::Up);
    assert!(log.contains(&Action::Up(UpReason::NoAuthRequired)));
    assert_eq!(data, b"[SOB]frame-from-server[EOB]");
}

#[test]
fn authenticated_ratflector() {
    let mut cfg = Config::new("KK4ODA");
    cfg.password = Some("letmein".into());
    let (c, log, data) = drive(cfg, mock_server(Mode::Auth), Duration::from_secs(3));
    assert_eq!(c.state(), State::Up);
    assert!(log.contains(&Action::Up(UpReason::Authenticated)));
    assert_eq!(data, b"[SOB]frame-from-server[EOB]");
}

#[test]
fn wrong_password_is_reported_not_retried() {
    let mut cfg = Config::new("KK4ODA");
    cfg.password = Some("nope".into());
    let (c, log, _) = drive(cfg, mock_server(Mode::Auth), Duration::from_secs(3));
    assert_eq!(c.state(), State::AuthFailed);
    assert!(log
        .iter()
        .any(|a| matches!(a, Action::AuthFailed(m) if m.contains("500"))));
}

#[test]
fn old_school_silent_ratflector_after_banner_timeout() {
    let mut cfg = Config::new("KK4ODA");
    cfg.banner_timeout = 200;
    let (c, log, data) = drive(cfg, mock_server(Mode::Silent), Duration::from_secs(3));
    assert_eq!(c.state(), State::Up);
    // The silent server's first bytes are the frame: kept as stream data, not lost.
    assert!(log.iter().any(|a| matches!(
        a,
        Action::Up(UpReason::OldSchoolUnrecognisedBanner | UpReason::OldSchoolNoBanner)
    )));
    assert!(
        data.ends_with(b"[EOB]"),
        "{:?}",
        String::from_utf8_lossy(&data)
    );
}

#[test]
fn refused_connection_backs_off() {
    // Bind then drop a listener to get a closed port.
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    // Windows takes ~2 s to report "connection refused" for a closed local
    // port (it retries the SYN); Linux and macOS refuse immediately.
    let (c, log, _) = drive(Config::new("KK4ODA"), port, Duration::from_secs(5));
    assert_eq!(c.state(), State::Backoff);
    assert!(log.iter().any(|a| matches!(a, Action::Retrying { .. })));
}
