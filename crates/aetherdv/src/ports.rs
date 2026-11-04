//! Port drivers: ratflector (TCP, full duplex) and serial radio (half duplex
//! through the SM-2 scheduler). Each driver owns its link and talks to the
//! engine through channels; neither ever claims delivery.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::time::{Duration, Instant};

use aether_ddt2::{Deframer, Event, Frame};
use aether_link::channel::{Busy, ChannelSense, Source, VoiceInference};
use aether_link::scheduler::{Item, ModeProfile, PathPreset, Priority, Scheduler, Status};

use crate::capture::Tap;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

/// Commands to a port (from the engine, and from the radio's CI-V session).
#[derive(Debug)]
pub enum PortCmd {
    /// Send one enveloped DDT2 frame. `token` identifies it in later events.
    Send {
        bytes: Vec<u8>,
        priority: Priority,
        token: Option<i64>,
    },
    /// A channel-sense source reports the channel (`None` = clear).
    Channel {
        source: Source,
        busy: Option<Busy>,
    },
    /// The channel-sense sources that work on this radio now.
    Sources(Vec<Source>),
    /// Route guard: hold (`true`) or release the port.
    RouteHold(bool),
    /// Operator pause: indefinitely (`None`) or for a number of ms.
    Pause(Option<u64>),
    Resume,
}

/// What a radio port's scheduler is doing (sent when it changes).
#[derive(Debug, Clone, PartialEq)]
pub struct SchedReport {
    /// `idle`, `sending`, `waiting` or `down`.
    pub state: &'static str,
    /// Why it waits (`voice`, `voice_gap`, `courtesy`, `paused`, `route`…).
    pub why: Option<&'static str>,
    /// Until when (unix ms), if known.
    pub until: Option<i64>,
    /// The channel is held by others or deliberately: transfer timers
    /// on this port should be paused.
    pub holds_timers: bool,
    pub queued: usize,
    /// Channel-sense sources in use (`data` is always there).
    pub sources: Vec<&'static str>,
    /// Voice is sensed directly (CI-V), not only inferred.
    pub senses_voice: bool,
    /// Voice inference switched off (radio streams its own GPS).
    pub inference_off: bool,
    pub air_used_ms: u64,
    pub air_budget_ms: u64,
    pub max_over_ms: u64,
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
    /// Result of a CI-V Fast Data read or set (`action`: startup, read, set).
    RadioMode {
        port: String,
        action: &'static str,
        result: Result<bool, String>,
    },
    /// The scheduler's state changed.
    Sched {
        port: String,
        report: SchedReport,
    },
    /// A burst was handed to the radio (estimated air time follows).
    Burst {
        port: String,
        bytes: usize,
        air_ms: u64,
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
    pub tap: Tap,
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
                    Action::Deliver(b) => {
                        p.tap.rx(&b);
                        deliver(&p.name, &mut deframer, &b, &mut out)
                    }
                    Action::Close => {
                        rd = None;
                        wr = None;
                        deframer = Deframer::default();
                    }
                    Action::Up(reason) => {
                        out.push(PortEvt::State {
                            port: p.name.clone(),
                            state: "up",
                            detail: Some(
                                match reason {
                                    aether_rfl::UpReason::NoAuthRequired => "no login required",
                                    aether_rfl::UpReason::Authenticated => "logged in",
                                    aether_rfl::UpReason::OldSchoolNoBanner => {
                                        "older server (no greeting)"
                                    }
                                    aether_rfl::UpReason::OldSchoolUnrecognisedBanner => {
                                        "older server (unrecognised greeting)"
                                    }
                                }
                                .to_string(),
                            ),
                        });
                        p.tap.note(&format!("connected: {reason:?}"));
                        while let Some((b, tok)) = waiting.pop_front() {
                            p.tap.tx(&b);
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
                Ok(0) | Err(_) => {
                    p.tap.note("disconnected");
                    actions.extend(client.on_disconnected(now()))
                }
                Ok(n) => actions.extend(client.on_bytes(now(), &buf[..n])),
            },
            c = cmd.recv() => match c {
                None => return,
                Some(PortCmd::Send { bytes, token, .. }) => {
                    if let (State::Up, Some(w)) = (client.state(), wr.as_mut()) {
                        p.tap.tx(&bytes);
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
                // Channel sense and holds are for radio ports.
                Some(_) => {}
            },
            _ = sleep_until_opt(deadline) => actions.extend(client.on_timeout(now())),
        }
    }
}

// ---------------------------------------------------------------------------
// Serial radio

pub struct SerialParams {
    pub tap: Tap,
    pub name: String,
    pub path: String,
    pub baud: u32,
    pub mode: String,
    /// The OS driver's XON/XOFF (legacy `flow = "software"`).
    pub software_flow: bool,
    /// The engine honours the radio's XOFF/XON itself: writes go out in
    /// small pieces and pause on XOFF; the two bytes are removed from
    /// received data (yEnc escapes them, so they are never payload).
    pub xonxoff: bool,
    /// Courtesy preset for the configured route.
    pub preset: PathPreset,
    /// Operator overrides of the preset (`[port.route]` advanced fields).
    pub tuning: Tuning,
    /// Infer voice from relayed D-PRS/NMEA slow data (docs/10 §4.11).
    pub infer_voice: bool,
    /// Start with the route guard holding the port (until the engine's
    /// first check releases it), so nothing slips out before it.
    pub start_held: bool,
}

/// Optional overrides of a path preset.
#[derive(Debug, Clone, Default)]
pub struct Tuning {
    pub holdoff_ms: Option<u64>,
    pub jitter_ms: Option<u64>,
    pub voice_gap_ms: Option<u64>,
    pub min_gap_ms: Option<u64>,
    pub max_over_ms: Option<u64>,
    pub duty_percent: Option<u32>,
    pub duty_window_s: Option<u64>,
}

/// The scheduler profile for a mode, route preset and sensing.
pub fn profile_for(mode: &str, preset: PathPreset, senses_voice: bool, t: &Tuning) -> ModeProfile {
    let base = if mode == "fast" {
        ModeProfile::dv_fast_placeholder()
    } else {
        ModeProfile::dv_slow_placeholder()
    };
    let mut p = base.for_path(preset, senses_voice);
    if let Some(v) = t.holdoff_ms {
        p.holdoff_ms = v;
    }
    if let Some(v) = t.jitter_ms {
        p.jitter_ms = v;
    }
    if let Some(v) = t.voice_gap_ms {
        p.voice_gap_ms = v;
    }
    if let Some(v) = t.min_gap_ms {
        p.min_gap_ms = v;
    }
    if let Some(v) = t.max_over_ms {
        p.max_burst_ms = v;
    }
    if let Some(v) = t.duty_percent {
        p.duty_permille = v * 10;
    }
    if let Some(v) = t.duty_window_s {
        p.duty_window_ms = v * 1000;
    }
    p
}

enum Io {
    Rx(Vec<u8>),
    Lost(String),
    Accepted(Vec<u64>, usize),
    /// XOFF held longer than [`XOFF_LIMIT`]; writing resumed anyway.
    XoffTimeout,
}

const XON: u8 = 0x11;
const XOFF: u8 = 0x13;
/// Bytes written between XOFF checks: 33 ms at 9600 baud. The ID-52 PLUS
/// sent XOFF about 50 ms into a 120-byte write.
const WRITE_CHUNK: usize = 32;
/// Longest XOFF honoured before writing resumes (D-RATS waits 15 s).
const XOFF_LIMIT: Duration = Duration::from_secs(20);

/// Holds and channel sense that outlive a reconnect.
struct Persist {
    route_hold: bool,
    /// `Some(None)` = paused until resumed; `Some(Some(t))` = until t.
    pause: Option<Option<u64>>,
    sense: ChannelSense,
    infer: VoiceInference,
}

impl Persist {
    /// Apply a non-send command. Returns true if the profile must change.
    fn apply(&mut self, c: &PortCmd, now: u64, sched: Option<&mut Scheduler>, tap: &Tap) -> bool {
        match c {
            PortCmd::Send { .. } => {}
            PortCmd::Channel { source, busy } => tap.event(
                "channel",
                serde_json::json!({ "source": source.as_str(), "busy": busy.map(Busy::as_str) }),
            ),
            PortCmd::Sources(list) => tap.event(
                "sources",
                serde_json::json!(list.iter().map(|s| s.as_str()).collect::<Vec<_>>()),
            ),
            PortCmd::RouteHold(h) => tap.event("route_hold", serde_json::json!({ "held": h })),
            PortCmd::Pause(ms) => tap.event("pause", serde_json::json!({ "ms": ms })),
            PortCmd::Resume => tap.event("resume", serde_json::json!({})),
        }
        match c {
            PortCmd::Send { .. } => false,
            PortCmd::Channel { source, busy } => {
                if let Some(agg) = self.sense.report(*source, *busy) {
                    if let Some(s) = sched {
                        s.on_channel(now, agg);
                    }
                }
                false
            }
            PortCmd::Sources(list) => {
                let had = self.sense.senses_voice();
                for s in self.sense.sources() {
                    if !list.contains(&s) {
                        self.sense.remove(s);
                    }
                }
                for s in list {
                    if !self.sense.sources().contains(s) {
                        self.sense.report(*s, None);
                    }
                }
                if let Some(s) = sched {
                    s.on_channel(now, self.sense.current());
                    s.set_sensed(!self.sense.sources().is_empty());
                }
                had != self.sense.senses_voice()
            }
            PortCmd::RouteHold(h) => {
                self.route_hold = *h;
                if let Some(s) = sched {
                    s.set_route_held(*h, now);
                }
                false
            }
            PortCmd::Pause(ms) => {
                let until = ms.map(|m| now + m);
                self.pause = Some(until);
                if let Some(s) = sched {
                    s.pause(until, now);
                }
                false
            }
            PortCmd::Resume => {
                self.pause = None;
                if let Some(s) = sched {
                    s.resume(now);
                }
                false
            }
        }
    }

    fn restore(&self, s: &mut Scheduler, now: u64) {
        s.set_route_held(self.route_hold, now);
        if let Some(until) = self.pause {
            s.pause(until, now);
        }
        s.set_sensed(!self.sense.sources().is_empty());
        s.on_channel(now, self.sense.current());
    }
}

fn report(s: &mut Scheduler, now: u64, unix_now: i64, p: &Persist) -> SchedReport {
    let status = s.status(now);
    let (state, why, until, holds) = match status {
        Status::Idle => ("idle", None, None, false),
        Status::Sending => ("sending", None, None, false),
        Status::Down => ("down", None, None, false),
        Status::Waiting { why, until } => (
            "waiting",
            Some(why.as_str()),
            until.map(|u| unix_now + u.saturating_sub(now) as i64),
            why.holds_timers(),
        ),
    };
    let mut sources = vec!["data"];
    sources.extend(p.sense.sources().iter().map(|s| s.as_str()));
    if !p.infer.disabled() {
        sources.push("inferred_voice");
    }
    SchedReport {
        state,
        why,
        until,
        holds_timers: holds,
        queued: s.queued(),
        sources,
        senses_voice: p.sense.senses_voice(),
        inference_off: p.infer.disabled(),
        air_used_ms: s.air_used(now),
        air_budget_ms: s.air_budget(),
        max_over_ms: s.profile().max_burst_ms,
    }
}

fn unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub async fn run_serial(
    p: SerialParams,
    mut cmd: mpsc::Receiver<PortCmd>,
    evt: mpsc::Sender<PortEvt>,
) {
    let epoch = Instant::now();
    let now = || epoch.elapsed().as_millis() as u64;
    let mut persist = Persist {
        route_hold: p.start_held,
        pause: None,
        sense: ChannelSense::default(),
        infer: VoiceInference::default(),
    };
    let make_profile =
        |sense: &ChannelSense| profile_for(&p.mode, p.preset, sense.senses_voice(), &p.tuning);
    let mut next_item = 1u64;
    let mut tokens: std::collections::HashMap<u64, i64> = std::collections::HashMap::new();
    let mut last_report: Option<SchedReport> = None;
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
                // Fail queued sends honestly while the radio is absent;
                // keep holds and channel reports for when it returns.
                while let Ok(c) = cmd.try_recv() {
                    match c {
                        PortCmd::Send { token: Some(t), .. } => {
                            let _ = evt
                                .send(PortEvt::NotTransmitted {
                                    port: p.name.clone(),
                                    token: t,
                                    reason: "radio not connected".into(),
                                })
                                .await;
                        }
                        other => {
                            persist.apply(&other, now(), None, &p.tap);
                        }
                    }
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
        let xoff = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (xoff_r, xoff_w, xonxoff) = (xoff.clone(), xoff.clone(), p.xonxoff);
        std::thread::spawn(move || {
            let mut b = [0u8; 1024];
            loop {
                match reader.read(&mut b) {
                    Ok(0) => {}
                    Ok(n) => {
                        // Flow control is acted on here, before the bytes
                        // queue behind anything, so the writer stops fast.
                        if xonxoff {
                            if let Some(&c) = b[..n].iter().rev().find(|&&c| c == XON || c == XOFF)
                            {
                                xoff_r.store(c == XOFF, std::sync::atomic::Ordering::SeqCst);
                            }
                        }
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
            use std::sync::atomic::Ordering::SeqCst;
            while let Ok((ids, bytes)) = wr_rx.recv() {
                let chunk = if xonxoff {
                    WRITE_CHUNK
                } else {
                    bytes.len().max(1)
                };
                for piece in bytes.chunks(chunk) {
                    let since = Instant::now();
                    while xoff_w.load(SeqCst) {
                        if since.elapsed() > XOFF_LIMIT {
                            xoff_w.store(false, SeqCst);
                            let _ = acc_tx.blocking_send(Io::XoffTimeout);
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    if writer
                        .write_all(piece)
                        .and_then(|_| writer.flush())
                        .is_err()
                    {
                        let _ = acc_tx.blocking_send(Io::Lost("write failed".into()));
                        return;
                    }
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
        let mut sched = Scheduler::new(
            make_profile(&persist.sense),
            epoch.elapsed().as_nanos() as u64,
        );
        persist.restore(&mut sched, now());
        let mut deframer = Deframer::default();
        let lost_reason = loop {
            if let Some(burst) = sched.poll(now()) {
                let ids = burst.items.iter().map(|i| i.id).collect();
                let (bytes, air_ms) =
                    (burst.bytes.len(), sched.profile().air_ms(burst.bytes.len()));
                p.tap.event(
                    "burst",
                    serde_json::json!({ "bytes": bytes, "frames": burst.items.len(), "airEstMs": air_ms, "keyupEstMs": sched.profile().keyup_ms }),
                );
                p.tap.tx(&burst.bytes);
                if wr_tx.send((ids, burst.bytes)).is_err() {
                    break "writer stopped".to_string();
                }
                let _ = evt
                    .send(PortEvt::Burst {
                        port: p.name.clone(),
                        bytes,
                        air_ms,
                    })
                    .await;
            }
            let r = report(&mut sched, now(), unix_ms(), &persist);
            if last_report.as_ref() != Some(&r) {
                let changed = last_report.as_ref().map_or(true, |l| {
                    (l.state, l.why, l.queued == 0) != (r.state, r.why, r.queued == 0)
                });
                if changed {
                    p.tap.event(
                        "sched",
                        serde_json::json!({ "state": r.state, "why": r.why, "until": r.until, "queued": r.queued, "airUsedMs": r.air_used_ms }),
                    );
                }
                last_report = Some(r.clone());
                if evt
                    .send(PortEvt::Sched {
                        port: p.name.clone(),
                        report: r,
                    })
                    .await
                    .is_err()
                {
                    return;
                }
            }
            let wake = sched
                .next_deadline()
                .map(|ms| epoch + Duration::from_millis(ms))
                .unwrap_or_else(|| Instant::now() + Duration::from_millis(250))
                .min(Instant::now() + Duration::from_secs(1));
            tokio::select! {
                io = io_rx.recv() => match io {
                    None => break "port threads stopped".to_string(),
                    Some(Io::Lost(e)) => break e,
                    Some(Io::XoffTimeout) => {
                        p.tap.note(&format!("XOFF held over {} s; writing resumed", XOFF_LIMIT.as_secs()));
                    }
                    Some(Io::Rx(mut b)) => {
                        p.tap.rx(&b);
                        if p.xonxoff {
                            for &c in b.iter().filter(|&&c| c == XON || c == XOFF) {
                                p.tap.event("flow", serde_json::json!({ "xoff": c == XOFF }));
                            }
                            b.retain(|&c| c != XON && c != XOFF);
                            if b.is_empty() {
                                continue;
                            }
                        }
                        let t = now();
                        sched.on_rx(t);
                        let mut out = Vec::new();
                        deliver(&p.name, &mut deframer, &b, &mut out);
                        if out.iter().any(|e| matches!(e, PortEvt::Frame { .. })) {
                            sched.on_frame(t);
                        }
                        if p.infer_voice {
                            let was_off = persist.infer.disabled();
                            let voice = out.iter().any(|e| matches!(e, PortEvt::Outside { bytes, .. } if persist.infer.feed(t, bytes)));
                            if voice {
                                p.tap.event("voice_inferred", serde_json::json!({}));
                                sched.on_voice_inferred(t);
                            }
                            if !was_off && persist.infer.disabled() {
                                p.tap.event(
                                    "voice_inference_off",
                                    serde_json::json!({ "reason": "steady GPS output: the radio streams its own position" }),
                                );
                            }
                        }
                        if !emit(&evt, out).await { return; }
                    }
                    Some(Io::Accepted(ids, len)) => {
                        sched.on_port_accepted(now());
                        let air = Duration::from_millis(sched.profile().air_ms(len));
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
                    Some(other) => {
                        if persist.apply(&other, now(), Some(&mut sched), &p.tap) {
                            sched.set_profile(make_profile(&persist.sense));
                        }
                    }
                },
                _ = tokio::time::sleep_until(wake.into()) => {}
            }
        };
        // SM-1 Lost: report everything not yet transmitted, then reopen.
        p.tap.note(&format!("lost: {lost_reason}"));
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
