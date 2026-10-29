//! Port drivers: ratflector (TCP, full duplex) and serial radio (half duplex
//! through the SM-2 scheduler). Each driver owns its link and talks to the
//! engine through channels; neither ever claims delivery.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::time::{Duration, Instant};

use aether_ddt2::{Deframer, Event, Frame};
use aether_link::scheduler::{Item, ModeProfile, Priority, Scheduler};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

/// Commands from the engine to a port.
#[derive(Debug)]
pub enum PortCmd {
    /// Send one enveloped DDT2 frame. `token` identifies it in later events.
    Send {
        bytes: Vec<u8>,
        priority: Priority,
        token: Option<i64>,
    },
}

/// Events from a port to the engine.
#[derive(Debug)]
pub enum PortEvt {
    State {
        port: String,
        state: &'static str,
        detail: Option<String>,
    },
    Frame {
        port: String,
        frame: Frame,
    },
    Outside {
        port: String,
        bytes: Vec<u8>,
    },
    /// Handed to the link (TCP accepted, or radio accepted and the estimated
    /// air time has passed). Not delivery.
    Transmitted {
        port: String,
        token: i64,
    },
    NotTransmitted {
        port: String,
        token: i64,
        reason: String,
    },
}

fn deliver(port: &str, d: &mut Deframer, bytes: &[u8], out: &mut Vec<PortEvt>) {
    let mut ev = Vec::new();
    d.feed(bytes, &mut ev);
    for e in ev {
        match e {
            Event::Frame { frame, .. } => out.push(PortEvt::Frame {
                port: port.into(),
                frame,
            }),
            Event::Outside(b) => out.push(PortEvt::Outside {
                port: port.into(),
                bytes: b,
            }),
            Event::BadFrame { .. } | Event::Overflow { .. } => {}
        }
    }
}

async fn emit(tx: &mpsc::Sender<PortEvt>, evs: Vec<PortEvt>) -> bool {
    for e in evs {
        if tx.send(e).await.is_err() {
            return false;
        }
    }
    true
}

// ---------------------------------------------------------------------------
// Ratflector

pub struct RflParams {
    pub name: String,
    pub host: String,
    pub tcp_port: u16,
    pub password: Option<String>,
    pub callsign: String,
}

/// SM-6 / N6.1: detect a silently dead connection (NAT timeout, Wi-Fi roam)
/// within about two minutes: probe after 60 s idle, every 15 s, 4 probes.
pub fn enable_keepalive(s: &TcpStream) -> std::io::Result<()> {
    let ka = socket2::TcpKeepalive::new()
        .with_time(Duration::from_secs(60))
        .with_interval(Duration::from_secs(15));
    #[cfg(not(any(target_os = "windows", target_os = "openbsd", target_os = "haiku")))]
    let ka = ka.with_retries(4);
    socket2::SockRef::from(s).set_tcp_keepalive(&ka)
}

async fn read_opt(rd: &mut Option<OwnedReadHalf>, buf: &mut [u8]) -> std::io::Result<usize> {
    match rd {
        Some(r) => r.read(buf).await,
        None => std::future::pending().await,
    }
}

async fn sleep_until_opt(deadline: Option<Instant>) {
    match deadline {
        Some(d) => tokio::time::sleep_until(d.into()).await,
        None => std::future::pending().await,
    }
}

pub async fn run_ratflector(
    p: RflParams,
    mut cmd: mpsc::Receiver<PortCmd>,
    evt: mpsc::Sender<PortEvt>,
) {
    use aether_rfl::{Action, Client, Config, State};
    let epoch = Instant::now();
    let now = || epoch.elapsed().as_millis() as u64;
    let mut cfg = Config::new(&p.callsign);
    cfg.password = p.password.clone();
    let mut client = Client::new(cfg);
    let mut rd: Option<OwnedReadHalf> = None;
    let mut wr: Option<OwnedWriteHalf> = None;
    let mut deframer = Deframer::default();
    let mut waiting: VecDeque<(Vec<u8>, Option<i64>)> = VecDeque::new();
    let mut actions = client.start();
    let mut buf = vec![0u8; 8192];
    loop {
        // Perform actions until none are left.
        while !actions.is_empty() {
            let mut out = Vec::new();
            for a in std::mem::take(&mut actions) {
                match a {
                    Action::Connect => {
                        out.push(PortEvt::State {
                            port: p.name.clone(),
                            state: "connecting",
                            detail: None,
                        });
                        match tokio::time::timeout(
                            Duration::from_secs(10),
                            TcpStream::connect((p.host.as_str(), p.tcp_port)),
                        )
                        .await
                        {
                            Ok(Ok(s)) => {
                                let _ = s.set_nodelay(true);
                                if let Err(e) = enable_keepalive(&s) {
                                    tracing::warn!("{}: TCP keepalive not enabled: {e}", p.name);
                                }
                                let (r, w) = s.into_split();
                                rd = Some(r);
                                wr = Some(w);
                                actions.extend(client.on_connected(now()));
                            }
                            Ok(Err(e)) => {
                                out.push(PortEvt::State {
                                    port: p.name.clone(),
                                    state: "backoff",
                                    detail: Some(e.to_string()),
                                });
                                actions.extend(client.on_connect_failed(now()));
                            }
                            Err(_) => {
                                out.push(PortEvt::State {
                                    port: p.name.clone(),
                                    state: "backoff",
                                    detail: Some("connect timed out".into()),
                                });
                                actions.extend(client.on_connect_failed(now()));
                            }
                        }
                    }
                    Action::Send(b) => {
                        if let Some(w) = wr.as_mut() {
                            if w.write_all(&b).await.is_err() {
                                actions.extend(client.on_disconnected(now()));
                            }
                        }
                    }
                    Action::Deliver(b) => deliver(&p.name, &mut deframer, &b, &mut out),
                    Action::Close => {
                        rd = None;
                        wr = None;
                        deframer = Deframer::default();
                    }
                    Action::Up(reason) => {
                        out.push(PortEvt::State {
                            port: p.name.clone(),
                            state: "up",
                            detail: Some(format!("{reason:?}")),
                        });
                        while let Some((b, tok)) = waiting.pop_front() {
                            let ok = match wr.as_mut() {
                                Some(w) => w.write_all(&b).await.is_ok(),
                                None => false,
                            };
                            if let Some(t) = tok {
                                out.push(if ok {
                                    PortEvt::Transmitted {
                                        port: p.name.clone(),
                                        token: t,
                                    }
                                } else {
                                    PortEvt::NotTransmitted {
                                        port: p.name.clone(),
                                        token: t,
                                        reason: "write failed".into(),
                                    }
                                });
                            }
                        }
                    }
                    Action::AuthFailed(m) => {
                        out.push(PortEvt::State {
                            port: p.name.clone(),
                            state: "auth_failed",
                            detail: Some(m),
                        });
                    }
                    Action::Retrying { delay, .. } => {
                        out.push(PortEvt::State {
                            port: p.name.clone(),
                            state: "backoff",
                            detail: Some(format!("retry in {} s", delay / 1000)),
                        });
                    }
                }
            }
            if !emit(&evt, out).await {
                return;
            }
        }
        let deadline = client
            .next_deadline()
            .map(|ms| epoch + Duration::from_millis(ms));
        tokio::select! {
            r = read_opt(&mut rd, &mut buf) => match r {
                Ok(0) | Err(_) => actions.extend(client.on_disconnected(now())),
                Ok(n) => actions.extend(client.on_bytes(now(), &buf[..n])),
            },
            c = cmd.recv() => match c {
                None => return,
                Some(PortCmd::Send { bytes, token, .. }) => {
                    if let (State::Up, Some(w)) = (client.state(), wr.as_mut()) {
                        let ok = w.write_all(&bytes).await.is_ok();
                        let ev = match (ok, token) {
                            (true, Some(t)) => vec![PortEvt::Transmitted { port: p.name.clone(), token: t }],
                            (false, Some(t)) => vec![PortEvt::NotTransmitted { port: p.name.clone(), token: t, reason: "write failed".into() }],
                            _ => vec![],
                        };
                        if !ok {
                            actions.extend(client.on_disconnected(now()));
                        }
                        if !emit(&evt, ev).await { return; }
                    } else if waiting.len() < 200 {
                        waiting.push_back((bytes, token));
                    } else if let Some(t) = token {
                        if !emit(&evt, vec![PortEvt::NotTransmitted { port: p.name.clone(), token: t, reason: "not connected; queue full".into() }]).await { return; }
                    }
                }
            },
            _ = sleep_until_opt(deadline) => actions.extend(client.on_timeout(now())),
        }
    }
}

// ---------------------------------------------------------------------------
// Serial radio

pub struct SerialParams {
    pub name: String,
    pub path: String,
    pub baud: u32,
    pub mode: String,
    pub software_flow: bool,
}

enum Io {
    Rx(Vec<u8>),
    Lost(String),
    Accepted(Vec<u64>, usize),
}

pub async fn run_serial(
    p: SerialParams,
    mut cmd: mpsc::Receiver<PortCmd>,
    evt: mpsc::Sender<PortEvt>,
) {
    let epoch = Instant::now();
    let now = || epoch.elapsed().as_millis() as u64;
    let profile = if p.mode == "fast" {
        ModeProfile::dv_fast_placeholder()
    } else {
        ModeProfile::dv_slow_placeholder()
    };
    let mut next_item = 1u64;
    let mut tokens: std::collections::HashMap<u64, i64> = std::collections::HashMap::new();
    loop {
        // SM-1: Opening. Exclusive open; nothing is written on open.
        let path = p.path.clone();
        let (baud, soft) = (p.baud, p.software_flow);
        let opened = tokio::task::spawn_blocking(move || {
            serialport::new(&path, baud)
                .timeout(Duration::from_millis(100))
                .flow_control(if soft {
                    serialport::FlowControl::Software
                } else {
                    serialport::FlowControl::None
                })
                .open()
        })
        .await
        .expect("open task");
        let port = match opened {
            Ok(port) => port,
            Err(e) => {
                let _ = evt
                    .send(PortEvt::State {
                        port: p.name.clone(),
                        state: "absent",
                        detail: Some(e.to_string()),
                    })
                    .await;
                // Fail queued sends honestly while the radio is absent.
                while let Ok(PortCmd::Send { token: Some(t), .. }) = cmd.try_recv() {
                    let _ = evt
                        .send(PortEvt::NotTransmitted {
                            port: p.name.clone(),
                            token: t,
                            reason: "radio not connected".into(),
                        })
                        .await;
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };
        let _ = evt
            .send(PortEvt::State {
                port: p.name.clone(),
                state: "ready",
                detail: Some(format!("{} @ {}", p.path, p.baud)),
            })
            .await;
        let (io_tx, mut io_rx) = mpsc::channel::<Io>(256);
        let (wr_tx, wr_rx) = std::sync::mpsc::channel::<(Vec<u64>, Vec<u8>)>();
        let mut reader = match port.try_clone() {
            Ok(r) => r,
            Err(e) => {
                let _ = evt
                    .send(PortEvt::State {
                        port: p.name.clone(),
                        state: "lost",
                        detail: Some(e.to_string()),
                    })
                    .await;
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };
        let rx_tx = io_tx.clone();
        std::thread::spawn(move || {
            let mut b = [0u8; 1024];
            loop {
                match reader.read(&mut b) {
                    Ok(0) => {}
                    Ok(n) => {
                        if rx_tx.blocking_send(Io::Rx(b[..n].to_vec())).is_err() {
                            return;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                    Err(e) => {
                        let _ = rx_tx.blocking_send(Io::Lost(e.to_string()));
                        return;
                    }
                }
            }
        });
        let mut writer = port;
        let acc_tx = io_tx.clone();
        std::thread::spawn(move || {
            while let Ok((ids, bytes)) = wr_rx.recv() {
                if writer
                    .write_all(&bytes)
                    .and_then(|_| writer.flush())
                    .is_err()
                {
                    let _ = acc_tx.blocking_send(Io::Lost("write failed".into()));
                    return;
                }
                if acc_tx
                    .blocking_send(Io::Accepted(ids, bytes.len()))
                    .is_err()
                {
                    return;
                }
            }
        });
        drop(io_tx);
        let mut sched = Scheduler::new(profile.clone(), epoch.elapsed().as_nanos() as u64);
        let mut deframer = Deframer::default();
        let lost_reason = loop {
            if let Some(burst) = sched.poll(now()) {
                let ids = burst.items.iter().map(|i| i.id).collect();
                if wr_tx.send((ids, burst.bytes)).is_err() {
                    break "writer stopped".to_string();
                }
            }
            let wake = sched
                .next_deadline()
                .map(|ms| epoch + Duration::from_millis(ms))
                .unwrap_or_else(|| Instant::now() + Duration::from_millis(250));
            tokio::select! {
                io = io_rx.recv() => match io {
                    None => break "port threads stopped".to_string(),
                    Some(Io::Lost(e)) => break e,
                    Some(Io::Rx(b)) => {
                        sched.on_rx(now());
                        let mut out = Vec::new();
                        deliver(&p.name, &mut deframer, &b, &mut out);
                        if !emit(&evt, out).await { return; }
                    }
                    Some(Io::Accepted(ids, len)) => {
                        sched.on_port_accepted(now());
                        let air = Duration::from_millis(profile.air_ms(len));
                        for id in ids {
                            if let Some(t) = tokens.remove(&id) {
                                let (evt, name) = (evt.clone(), p.name.clone());
                                tokio::spawn(async move {
                                    tokio::time::sleep(air).await;
                                    let _ = evt.send(PortEvt::Transmitted { port: name, token: t }).await;
                                });
                            }
                        }
                    }
                },
                c = cmd.recv() => match c {
                    None => return,
                    Some(PortCmd::Send { bytes, priority, token }) => {
                        let id = next_item;
                        next_item += 1;
                        if let Some(t) = token { tokens.insert(id, t); }
                        sched.enqueue(priority, Item { id, bytes });
                    }
                },
                _ = tokio::time::sleep_until(wake.into()) => {}
            }
        };
        // SM-1 Lost: report everything not yet transmitted, then reopen.
        let failed = sched.on_link_down();
        let mut out = vec![PortEvt::State {
            port: p.name.clone(),
            state: "lost",
            detail: Some(lost_reason),
        }];
        for item in failed {
            if let Some(t) = tokens.remove(&item.id) {
                out.push(PortEvt::NotTransmitted {
                    port: p.name.clone(),
                    token: t,
                    reason: "radio disconnected".into(),
                });
            }
        }
        if !emit(&evt, out).await {
            return;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}
