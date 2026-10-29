//! TCP ratflector server around the [`Hub`](crate::Hub) forwarding core.
//!
//! Client-facing behaviour matches `d-rats_repeater.py` (@d0e7a39):
//! `100 Authentication not required` when no users are configured,
//! otherwise `101` → `USER` → `102 USER okay` → `PASS` → `200 Authorized`
//! or `500 Not authorized`. After that the connection carries the DDT2 byte
//! stream. Additions: per-client bounded output queues (a stalled client
//! loses frames instead of stalling everyone, R8), handshake timeouts and
//! line limits, TCP keepalive left to the OS defaults, graceful shutdown.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use aether_ddt2::{Deframer, Event};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch};

use crate::{Config as HubConfig, Hub, LinkId};

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub hub: HubConfig,
    /// (user, password) pairs; empty = open ratflector.
    pub users: Vec<(String, String)>,
    pub handshake_timeout: Duration,
    pub max_line: usize,
    /// Bytes queued per client before new frames are dropped for that client.
    /// Must exceed `client_burst`, or one legitimate burst can overflow a
    /// healthy client (found in the stalled-client test).
    pub client_queue_bytes: usize,
    pub max_clients: usize,
    /// Sustained input rate allowed per client, bytes/s (token bucket). Reading
    /// pauses when exceeded, so TCP flow control pushes back on a flooding
    /// sender instead of the server dropping frames for healthy clients.
    pub client_rate: u32,
    /// Token-bucket burst size, bytes.
    pub client_burst: u32,
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            hub: HubConfig::aether(),
            users: vec![],
            handshake_timeout: Duration::from_secs(30),
            max_line: 512,
            client_queue_bytes: 1024 * 1024,
            max_clients: 500,
            // DV slow data is ~119 B/s and Fast Data ~435 B/s; 64 KiB/s is far
            // above any legitimate D-RATS client, including bridges.
            client_rate: 64 * 1024,
            client_burst: 256 * 1024,
        }
    }
}

/// Parse a legacy `users.txt`: one `user password` per line, `#` comments.
pub fn parse_users(text: &str) -> Vec<(String, String)> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            l.split_once(char::is_whitespace)
                .map(|(u, p)| (u.to_string(), p.trim().to_string()))
        })
        .collect()
}

#[derive(Debug, Default)]
pub struct Counters {
    pub clients_total: AtomicU64,
    pub auth_failures: AtomicU64,
    pub frames_in: AtomicU64,
    pub bad_frames: AtomicU64,
    pub queue_drops: AtomicU64,
}

/// A client's outbound queue, bounded by bytes rather than frames.
struct ClientOut {
    tx: mpsc::UnboundedSender<Vec<u8>>,
    queued: Arc<std::sync::atomic::AtomicUsize>,
    budget: usize,
}

enum HubMsg {
    Up(LinkId, ClientOut),
    Frame(LinkId, aether_ddt2::Frame, Vec<u8>),
    Down(LinkId),
}

pub struct Server {
    pub local_addr: SocketAddr,
    pub counters: Arc<Counters>,
    shutdown: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

impl Server {
    /// Bind and start serving in the background.
    pub async fn start(bind: &str, cfg: ServerConfig) -> std::io::Result<Server> {
        let listener = TcpListener::bind(bind).await?;
        let local_addr = listener.local_addr()?;
        let counters = Arc::new(Counters::default());
        let (shutdown, shut_rx) = watch::channel(false);
        let (hub_tx, hub_rx) = mpsc::channel::<HubMsg>(4096);
        let epoch = Instant::now();
        tokio::spawn(hub_task(
            HubConfig { ..cfg.hub.clone() },
            hub_rx,
            counters.clone(),
            epoch,
        ));
        let task = tokio::spawn(accept_loop(
            listener,
            cfg,
            hub_tx,
            counters.clone(),
            shut_rx,
            epoch,
        ));
        Ok(Server {
            local_addr,
            counters,
            shutdown,
            task,
        })
    }

    /// Stop accepting and close all client connections.
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(true);
        let _ = self.task.await;
    }
}

async fn hub_task(
    cfg: HubConfig,
    mut rx: mpsc::Receiver<HubMsg>,
    counters: Arc<Counters>,
    epoch: Instant,
) {
    let mut hub = Hub::new(cfg);
    let mut outs: HashMap<LinkId, ClientOut> = HashMap::new();
    while let Some(msg) = rx.recv().await {
        match msg {
            HubMsg::Up(id, tx) => {
                hub.add_link(id);
                outs.insert(id, tx);
            }
            HubMsg::Down(id) => {
                hub.remove_link(id);
                outs.remove(&id);
            }
            HubMsg::Frame(from, frame, wire) => {
                let now = epoch.elapsed().as_millis() as u64;
                for (to, bytes) in hub.on_frame(now, from, &frame, &wire) {
                    if let Some(out) = outs.get(&to) {
                        let len = bytes.len();
                        if out.queued.load(Ordering::Relaxed) + len > out.budget {
                            counters.queue_drops.fetch_add(1, Ordering::Relaxed);
                        } else if out.tx.send(bytes).is_ok() {
                            out.queued.fetch_add(len, Ordering::Relaxed);
                        }
                    }
                }
            }
        }
    }
}

async fn accept_loop(
    listener: TcpListener,
    cfg: ServerConfig,
    hub_tx: mpsc::Sender<HubMsg>,
    counters: Arc<Counters>,
    mut shutdown: watch::Receiver<bool>,
    epoch: Instant,
) {
    let next_id = Arc::new(AtomicU64::new(1));
    let active = Arc::new(AtomicU64::new(0));
    let mut conns = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            acc = listener.accept() => {
                let Ok((sock, peer)) = acc else { continue };
                if active.load(Ordering::Relaxed) as usize >= cfg.max_clients {
                    drop(sock);
                    continue;
                }
                counters.clients_total.fetch_add(1, Ordering::Relaxed);
                let id = next_id.fetch_add(1, Ordering::Relaxed) as LinkId;
                let (cfg, hub_tx, counters, active, shut) = (cfg.clone(), hub_tx.clone(), counters.clone(), active.clone(), shutdown.clone());
                active.fetch_add(1, Ordering::Relaxed);
                conns.spawn(async move {
                    if let Err(e) = client(sock, id, &cfg, &hub_tx, &counters, shut, epoch).await {
                        tracing::debug!(%peer, "client {id} ended: {e}");
                    }
                    let _ = hub_tx.send(HubMsg::Down(id)).await;
                    active.fetch_sub(1, Ordering::Relaxed);
                });
            }
        }
    }
    conns.shutdown().await;
}

async fn read_line(
    r: &mut BufReader<tokio::net::tcp::ReadHalf<'_>>,
    max: usize,
) -> std::io::Result<String> {
    let mut buf = Vec::new();
    let n = (&mut *r)
        .take(max as u64)
        .read_until(b'\n', &mut buf)
        .await?;
    if n == 0 {
        return Err(std::io::ErrorKind::UnexpectedEof.into());
    }
    Ok(String::from_utf8_lossy(&buf).trim().to_string())
}

async fn handshake(sock: &mut TcpStream, cfg: &ServerConfig) -> std::io::Result<bool> {
    if cfg.users.is_empty() {
        sock.write_all(b"100 Authentication not required\r\n")
            .await?;
        return Ok(true);
    }
    sock.write_all(b"101 Authorization required\r\n").await?;
    let (rd, mut wr) = sock.split();
    let mut r = BufReader::new(rd);
    let user = match read_line(&mut r, cfg.max_line).await?.split_once(' ') {
        Some((cmd, u)) if cmd.eq_ignore_ascii_case("USER") => u.to_string(),
        _ => {
            wr.write_all(b"201 Protocol violation\r\n").await?;
            return Ok(false);
        }
    };
    wr.write_all(b"102 USER okay\r\n").await?;
    let pass = match read_line(&mut r, cfg.max_line).await?.split_once(' ') {
        Some((cmd, p)) if cmd.eq_ignore_ascii_case("PASS") => p.to_string(),
        _ => {
            wr.write_all(b"201 Protocol violation\r\n").await?;
            return Ok(false);
        }
    };
    if cfg.users.iter().any(|(u, p)| *u == user && *p == pass) {
        wr.write_all(b"200 Authorized\r\n").await?;
        Ok(true)
    } else {
        wr.write_all(b"500 Not authorized\r\n").await?;
        Ok(false)
    }
}

async fn client(
    mut sock: TcpStream,
    id: LinkId,
    cfg: &ServerConfig,
    hub_tx: &mpsc::Sender<HubMsg>,
    counters: &Counters,
    mut shutdown: watch::Receiver<bool>,
    epoch: Instant,
) -> std::io::Result<()> {
    let _ = epoch;
    sock.set_nodelay(true)?;
    match tokio::time::timeout(cfg.handshake_timeout, handshake(&mut sock, cfg)).await {
        Ok(Ok(true)) => {}
        Ok(Ok(false)) => {
            counters.auth_failures.fetch_add(1, Ordering::Relaxed);
            return Ok(());
        }
        Ok(Err(e)) => return Err(e),
        Err(_) => return Err(std::io::ErrorKind::TimedOut.into()),
    }
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let queued = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let out = ClientOut {
        tx: out_tx,
        queued: queued.clone(),
        budget: cfg.client_queue_bytes,
    };
    hub_tx
        .send(HubMsg::Up(id, out))
        .await
        .map_err(|_| std::io::Error::other("hub stopped"))?;
    let (mut rd, mut wr) = sock.into_split();
    let writer = tokio::spawn(async move {
        while let Some(b) = out_rx.recv().await {
            let len = b.len();
            let ok = wr.write_all(&b).await.is_ok();
            queued.fetch_sub(len, Ordering::Relaxed);
            if !ok {
                break;
            }
        }
    });
    let mut deframer = Deframer::default();
    let mut buf = vec![0u8; 4096];
    let mut bucket = TokenBucket::new(cfg.client_rate, cfg.client_burst);
    let mut events = Vec::new();
    let result = loop {
        tokio::select! {
            _ = shutdown.changed() => break Ok(()),
            n = rd.read(&mut buf) => {
                let n = match n { Ok(0) => break Ok(()), Ok(n) => n, Err(e) => break Err(e) };
                if let Some(wait) = bucket.take(n as u64, Instant::now()) {
                    tokio::time::sleep(wait).await;
                }
                deframer.feed(&buf[..n], &mut events);
                for ev in events.drain(..) {
                    match ev {
                        Event::Frame { frame, wire } => {
                            counters.frames_in.fetch_add(1, Ordering::Relaxed);
                            // Forward the original envelope bytes, not a re-encoding.
                            if hub_tx.send(HubMsg::Frame(id, frame, wire)).await.is_err() {
                                break;
                            }
                        }
                        Event::BadFrame { .. } | Event::Overflow { .. } => {
                            counters.bad_frames.fetch_add(1, Ordering::Relaxed);
                        }
                        Event::Outside(_) => {}
                    }
                }
            }
        }
    };
    writer.abort();
    result
}

/// Simple token bucket: `take` returns how long to wait before continuing.
struct TokenBucket {
    rate: f64,
    burst: f64,
    tokens: f64,
    last: Option<Instant>,
}

impl TokenBucket {
    fn new(rate: u32, burst: u32) -> Self {
        TokenBucket {
            rate: rate as f64,
            burst: burst as f64,
            tokens: burst as f64,
            last: None,
        }
    }

    fn take(&mut self, n: u64, now: Instant) -> Option<Duration> {
        if self.rate <= 0.0 {
            return None;
        }
        if let Some(last) = self.last {
            self.tokens =
                (self.tokens + now.duration_since(last).as_secs_f64() * self.rate).min(self.burst);
        }
        self.last = Some(now);
        self.tokens -= n as f64;
        (self.tokens < 0.0).then(|| Duration::from_secs_f64(-self.tokens / self.rate))
    }
}
