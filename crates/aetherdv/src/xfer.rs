//! Legacy D-RATS file and form transfer (stateful sessions), sans-I/O.
//!
//! Wire behaviour follows D-Rats @d0e7a39:
//! * `sessions/control.py`: NEW (type 3 + class, payload `[requester id] +
//!   name`), ACK (`[requester id, responder id]`), END (the receiver's
//!   session id as ASCII decimal; the receiver answers with END carrying the
//!   opener's id, and an END for an unknown id is ignored, which stops the
//!   echo).
//! * `sessionmgr.py`: frames of a stateful session carry the *peer's*
//!   session id and are accepted only from the session's station.
//! * `sessions/file.py`: offer (`<I` zlib length + base name) as the
//!   session's first block, answer `OK` or `RESUME:<n>`, then the
//!   `zlib.compress(data, 9)` stream; end of file is signalled by END.
//! * `sessions/stateful.py`: ARQ-1 ([`aether_link::legacy_arq`]); one block
//!   sequence per session, so the stream starts at block 1.
//!
//! Aether differences (all invisible to a legacy peer):
//! * incoming offers are bounded (`Limits`) and the name must be a plain
//!   base name (legacy joins it onto the download directory, P11 class);
//! * no `RESUME:` from our side: legacy resume matches `.part` files by name
//!   only (P8), so we always answer `OK` and restart;
//! * a received stream is accepted only if its length matches the offer and
//!   it inflates within the size limit;
//! * a finished send means every block was acknowledged by the peer
//!   (I4.2: "handed off", not "delivered").

use std::collections::HashMap;
use std::io::{Read, Write};

use aether_ddt2::session::{self, control, id};
use aether_ddt2::{Frame, PayloadEncoding};
use aether_link::legacy_arq::{
    LegacyReceiver, LegacySender, Millis, RxDiag, RxEvent, SenderConfig, SenderState, SessionAddr,
    ENCODING,
};
use aether_link::scheduler::Priority;

/// Kind of legacy stateful session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    File,
    Form,
}

impl Class {
    pub fn control_kind(self) -> u8 {
        control::NEW_BASE
            + match self {
                Class::File => control::CLASS_FILE,
                Class::Form => control::CLASS_FORM,
            }
    }

    pub fn protocol(self) -> &'static str {
        match self {
            Class::File => "legacy_file",
            Class::Form => "legacy_form",
        }
    }

    fn priority(self) -> Priority {
        match self {
            Class::File => Priority::File,
            Class::Form => Priority::Form,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Send,
    Receive,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Phase {
    /// Sender: NEW sent, waiting for the control ACK.
    Opening,
    /// Sender: offer queued, waiting for OK/RESUME. Receiver: waiting for
    /// the offer.
    Offering,
    /// The zlib stream is flowing.
    Streaming,
    /// Sender: END sent, waiting for the peer's END.
    Closing,
    Done,
    Failed(String),
}

/// Bounds applied to incoming transfers.
#[derive(Debug, Clone)]
pub struct Limits {
    /// Largest zlib stream accepted from an offer.
    pub max_wire_bytes: u32,
    /// Largest inflated object accepted.
    pub max_object_bytes: usize,
    /// Incoming transfers running at once, per station.
    pub max_incoming: usize,
    /// Session idle time before giving up (legacy `IDLE_TIMEOUT` = 90 s).
    pub idle_ms: Millis,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_wire_bytes: 4 << 20,
            max_object_bytes: 32 << 20,
            max_incoming: 8,
            idle_ms: 90_000,
        }
    }
}

/// Externally visible state of one transfer (maps onto the API `Transfer`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub key: u64,
    pub dir: Dir,
    pub class: Class,
    pub port: String,
    pub peer: String,
    pub name: String,
    /// Legacy progress is counted on the zlib stream, as D-RATS does.
    pub wire_total: u64,
    pub wire_done: u64,
    pub block_size: usize,
    pub state: &'static str,
    pub reason: Option<String>,
    pub retries: u32,
    pub wrap_warning: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XEffect {
    Send {
        port: String,
        frame: Frame,
        encoding: PayloadEncoding,
        priority: Priority,
    },
    /// State or progress changed.
    Update(Snapshot),
    /// A complete, verified incoming object.
    Received {
        key: u64,
        class: Class,
        port: String,
        peer: String,
        name: String,
        data: Vec<u8>,
    },
    Log {
        level: &'static str,
        message: String,
    },
}

#[derive(Debug)]
struct Xfer {
    key: u64,
    dir: Dir,
    class: Class,
    port: String,
    peer: String,
    local_id: u8,
    remote_id: u8,
    name: String,
    phase: Phase,
    cfg: SenderConfig,
    ctl_attempts: u32,
    ctl_deadline: Millis,
    tx: Option<LegacySender>,
    rx: Option<LegacyReceiver>,
    /// Send: the zlib stream. Receive: bytes delivered so far.
    stream: Vec<u8>,
    /// Send: bytes acknowledged before the stream started (offer + resume).
    stream_base: usize,
    resume_at: usize,
    wire_total: u64,
    inbuf: Vec<u8>,
    last_activity: Millis,
    wrap_diag: bool,
    last_snapshot: Option<Snapshot>,
}

fn zlib(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    e.write_all(data).expect("in-memory write");
    e.finish().expect("in-memory write")
}

/// Inflate a zlib stream, refusing output beyond `limit` bytes.
pub fn inflate_limited(data: &[u8], limit: usize) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut d = flate2::read::ZlibDecoder::new(data).take(limit as u64 + 1);
    d.read_to_end(&mut out)
        .map_err(|e| format!("not a valid zlib stream: {e}"))?;
    if out.len() > limit {
        return Err(format!("object larger than the {limit}-byte limit"));
    }
    Ok(out)
}

impl Xfer {
    fn addr(&self, me: &str) -> SessionAddr {
        SessionAddr {
            session: self.remote_id,
            src: me.into(),
            dst: self.peer.clone(),
        }
    }

    fn finished(&self) -> bool {
        matches!(self.phase, Phase::Done | Phase::Failed(_))
    }

    fn wire_done(&self) -> u64 {
        match self.dir {
            Dir::Send => {
                let acked = self.tx.as_ref().map(|t| t.bytes_acked()).unwrap_or(0);
                (acked.saturating_sub(self.stream_base) + self.resume_at) as u64
            }
            Dir::Receive => self.stream.len() as u64,
        }
    }

    fn snapshot(&self) -> Snapshot {
        let (state, reason) = match &self.phase {
            Phase::Opening => ("opening", None),
            Phase::Offering => ("offering", None),
            Phase::Streaming => ("sending", None),
            Phase::Closing => ("sending", None),
            Phase::Done => (
                "done",
                (self.dir == Dir::Send).then(|| "all blocks acknowledged by the peer".to_string()),
            ),
            Phase::Failed(r) => ("failed", Some(r.clone())),
        };
        Snapshot {
            key: self.key,
            dir: self.dir,
            class: self.class,
            port: self.port.clone(),
            peer: self.peer.clone(),
            name: self.name.clone(),
            wire_total: self.wire_total,
            wire_done: self.wire_done(),
            block_size: self.cfg.block_size,
            state,
            reason,
            retries: self.tx.as_ref().map(|t| t.stats.repolls).unwrap_or(0)
                + self.ctl_attempts.saturating_sub(1),
            wrap_warning: self.wrap_diag
                || (self.wire_total as usize).div_ceil(self.cfg.block_size.max(1)) + 1 > 256,
        }
    }
}

/// What to send in a new outgoing transfer.
#[derive(Debug, Clone)]
pub struct SendRequest<'a> {
    pub port: &'a str,
    pub peer: &'a str,
    pub class: Class,
    pub name: &'a str,
    pub content: &'a [u8],
    pub cfg: SenderConfig,
}

/// All legacy stateful sessions of this station.
#[derive(Debug)]
pub struct Transfers {
    pub callsign: String,
    pub limits: Limits,
    xfers: HashMap<u64, Xfer>,
    next_key: u64,
    next_id: u8,
    /// Ports whose transfer timers are stopped, since when, and why (a
    /// set of [`Hold`] bits): the channel is held (docs/10 §4.5) or a
    /// burst is still being written to the radio.
    paused: HashMap<String, (Millis, u8)>,
}

/// Why a port's transfer timers are stopped. Each reason is held and
/// released on its own; the timers run again when none is left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hold {
    /// Voice, courtesy pause, operator pause, route hold.
    Channel = 1,
    /// A burst is being written; the radio takes it piece by piece and
    /// over by over (bench large-file test: up to 20 s for one burst).
    Write = 2,
}

impl Transfers {
    pub fn new(callsign: &str, limits: Limits) -> Self {
        Transfers {
            callsign: callsign.to_ascii_uppercase(),
            limits,
            xfers: HashMap::new(),
            next_key: 1,
            next_id: id::FIRST_DYNAMIC,
            paused: HashMap::new(),
        }
    }

    /// Stop the timers of every transfer on `port` (voice, courtesy pause,
    /// operator pause, route hold): waiting is not the peer's fault.
    pub fn pause_port(&mut self, port: &str, now: Millis) {
        self.hold(port, Hold::Channel, now);
    }

    /// Restart them, later by the time they were stopped.
    pub fn resume_port(&mut self, port: &str, now: Millis) {
        self.release(port, Hold::Channel, now);
    }

    pub fn port_paused(&self, port: &str) -> bool {
        self.held(port, Hold::Channel)
    }

    /// Stop the port's transfer timers for `why`.
    pub fn hold(&mut self, port: &str, why: Hold, now: Millis) {
        let e = self.paused.entry(port.to_string()).or_insert((now, 0));
        e.1 |= why as u8;
    }

    /// Drop the `why` hold; with none left, restart the timers, later by
    /// the time they were stopped.
    pub fn release(&mut self, port: &str, why: Hold, now: Millis) {
        let Some(e) = self.paused.get_mut(port) else {
            return;
        };
        e.1 &= !(why as u8);
        if e.1 == 0 {
            self.rebase(port, now);
            self.paused.remove(port);
        }
        if why == Hold::Write {
            for x in self.xfers.values_mut().filter(|x| x.port == port) {
                if let Some(tx) = x.tx.as_mut() {
                    tx.burst_written(now);
                }
            }
        }
    }

    pub fn held(&self, port: &str, why: Hold) -> bool {
        self.paused.get(port).is_some_and(|e| e.1 & why as u8 != 0)
    }

    /// Credit the time paused so far to the port's transfers and restart
    /// the pause clock at `now`, so that timers set from now on (a frame
    /// arrived, a transfer started) are not shifted again.
    fn rebase(&mut self, port: &str, now: Millis) {
        let Some((since, _)) = self.paused.get_mut(port) else {
            return;
        };
        let delta = now.saturating_sub(*since);
        *since = now;
        if delta == 0 {
            return;
        }
        for x in self.xfers.values_mut().filter(|x| x.port == port) {
            x.ctl_deadline += delta;
            x.last_activity += delta;
            if let Some(tx) = x.tx.as_mut() {
                tx.shift(delta);
            }
        }
    }

    fn alloc_id(&mut self) -> Option<u8> {
        let live: Vec<u8> = self
            .xfers
            .values()
            .filter(|x| !x.finished())
            .map(|x| x.local_id)
            .collect();
        for _ in 0..=(255 - id::FIRST_DYNAMIC) {
            let c = self.next_id;
            self.next_id = if c == 255 { id::FIRST_DYNAMIC } else { c + 1 };
            if !live.contains(&c) {
                return Some(c);
            }
        }
        None
    }

    fn ctl(&self, peer: &str, kind: u8, payload: Vec<u8>) -> Frame {
        Frame::new(0, id::CONTROL, kind, &self.callsign, peer, payload)
    }

    fn send(out: &mut Vec<XEffect>, x: &Xfer, frame: Frame) {
        let priority = if frame.session == id::CONTROL {
            Priority::Control
        } else {
            x.class.priority()
        };
        out.push(XEffect::Send {
            port: x.port.clone(),
            frame,
            encoding: ENCODING,
            priority,
        });
    }

    fn push_update(out: &mut Vec<XEffect>, x: &mut Xfer) {
        let s = x.snapshot();
        if x.last_snapshot.as_ref() != Some(&s) {
            x.last_snapshot = Some(s.clone());
            out.push(XEffect::Update(s));
        }
    }

    /// Start sending `content` to `peer`. Returns the transfer key.
    pub fn start_send(
        &mut self,
        now: Millis,
        req: SendRequest<'_>,
    ) -> Result<(u64, Vec<XEffect>), String> {
        self.rebase(req.port, now);
        let SendRequest {
            port,
            peer,
            class,
            name,
            content,
            cfg,
        } = req;
        if name.is_empty() || name.contains('/') || name.contains('\\') || name == ".." {
            return Err("file name must be a plain base name".into());
        }
        let local_id = self.alloc_id().ok_or("no free session ids")?;
        let stream = zlib(content);
        if stream.len() > u32::MAX as usize {
            return Err("file too large for a legacy transfer".into());
        }
        let key = self.next_key;
        self.next_key += 1;
        let mut x = Xfer {
            key,
            dir: Dir::Send,
            class,
            port: port.into(),
            peer: peer.to_ascii_uppercase(),
            local_id,
            remote_id: 0,
            name: name.into(),
            phase: Phase::Opening,
            cfg,
            ctl_attempts: 0,
            ctl_deadline: now,
            tx: None,
            rx: None,
            wire_total: stream.len() as u64,
            stream,
            stream_base: 0,
            resume_at: 0,
            inbuf: Vec::new(),
            last_activity: now,
            wrap_diag: false,
            last_snapshot: None,
        };
        let mut out = Vec::new();
        Self::push_update(&mut out, &mut x);
        self.xfers.insert(key, x);
        out.extend(self.poll(now));
        Ok((key, out))
    }

    /// Abort a transfer (best-effort END to the peer).
    pub fn cancel(&mut self, key: u64) -> Option<Vec<XEffect>> {
        let me = self.callsign.clone();
        let x = self.xfers.get_mut(&key)?;
        let mut out = Vec::new();
        if !x.finished() {
            if x.remote_id != 0 {
                let f = Frame::new(
                    0,
                    id::CONTROL,
                    control::END,
                    &me,
                    &x.peer,
                    session::encode_control_end(x.remote_id),
                );
                Self::send(&mut out, x, f);
            }
            x.phase = Phase::Failed("cancelled".into());
            Self::push_update(&mut out, x);
        }
        Some(out)
    }

    pub fn snapshots(&self) -> Vec<Snapshot> {
        let mut v: Vec<Snapshot> = self.xfers.values().map(|x| x.snapshot()).collect();
        v.sort_by_key(|s| s.key);
        v
    }

    /// Forget finished transfers (their record lives in the store).
    pub fn reap(&mut self) {
        self.xfers.retain(|_, x| !x.finished());
    }

    /// Earliest time [`Self::poll`] has work to do.
    pub fn next_deadline(&self) -> Option<Millis> {
        self.xfers
            .values()
            .filter(|x| !x.finished() && !self.paused.contains_key(&x.port))
            .map(|x| {
                let t = x.tx.as_ref().and_then(|t| t.next_deadline());
                let c =
                    matches!(x.phase, Phase::Opening | Phase::Closing).then_some(x.ctl_deadline);
                let idle = x.last_activity + self.limits.idle_ms;
                [t, c, Some(idle)]
                    .into_iter()
                    .flatten()
                    .min()
                    .unwrap_or(idle)
            })
            .min()
    }

    /// Timers: control retries, ARQ bursts and re-polls, idle timeouts.
    pub fn poll(&mut self, now: Millis) -> Vec<XEffect> {
        let me = self.callsign.clone();
        let idle_ms = self.limits.idle_ms;
        let mut out = Vec::new();
        for x in self.xfers.values_mut() {
            if x.finished() || self.paused.contains_key(&x.port) {
                continue;
            }
            match x.phase {
                Phase::Opening if now >= x.ctl_deadline => {
                    if x.ctl_attempts >= 10 {
                        x.phase = Phase::Failed(format!(
                            "{} did not accept the session (no answer to 10 requests)",
                            x.peer
                        ));
                    } else {
                        x.ctl_attempts += 1;
                        // control.py waits 5 s, then 15 s per attempt; the
                        // first wait is a little longer here to cover a slow
                        // DV round trip.
                        x.ctl_deadline = now + if x.ctl_attempts == 1 { 8_000 } else { 15_000 };
                        let f = Frame::new(
                            0,
                            id::CONTROL,
                            x.class.control_kind(),
                            &me,
                            &x.peer,
                            session::encode_control_new(x.local_id, &x.name),
                        );
                        Self::send(&mut out, x, f);
                    }
                }
                Phase::Closing if now >= x.ctl_deadline => {
                    if x.ctl_attempts >= 3 {
                        x.phase = Phase::Done; // data was all acknowledged
                        out.push(XEffect::Log {
                            level: "info",
                            message: format!(
                                "{}: {} did not confirm the session close",
                                x.name, x.peer
                            ),
                        });
                    } else {
                        x.ctl_attempts += 1;
                        x.ctl_deadline = now + 15_000;
                        let f = Frame::new(
                            0,
                            id::CONTROL,
                            control::END,
                            &me,
                            &x.peer,
                            session::encode_control_end(x.remote_id),
                        );
                        Self::send(&mut out, x, f);
                    }
                }
                _ => {}
            }
            if let Some(tx) = x.tx.as_mut() {
                for f in tx.poll_transmit(now) {
                    out.push(XEffect::Send {
                        port: x.port.clone(),
                        frame: f,
                        encoding: ENCODING,
                        priority: x.class.priority(),
                    });
                }
                if tx.state() == SenderState::Failed && !x.finished() {
                    x.phase = Phase::Failed(format!(
                        "{} stopped acknowledging (10 polls unanswered)",
                        x.peer
                    ));
                    let f = Frame::new(
                        0,
                        id::CONTROL,
                        control::END,
                        &me,
                        &x.peer,
                        session::encode_control_end(x.remote_id),
                    );
                    Self::send(&mut out, x, f);
                }
            }
            if x.dir == Dir::Send
                && x.phase == Phase::Streaming
                && x.tx.as_ref().map(|t| t.state()) == Some(SenderState::Done)
            {
                // Every block is acknowledged: close now (file.py closes
                // as soon as `write` returns).
                x.phase = Phase::Closing;
                x.ctl_attempts = 1;
                x.ctl_deadline = now + 15_000;
                let f = Frame::new(
                    0,
                    id::CONTROL,
                    control::END,
                    &me,
                    &x.peer,
                    session::encode_control_end(x.remote_id),
                );
                Self::send(&mut out, x, f);
            }
            let waiting_on_peer = matches!(x.phase, Phase::Offering | Phase::Streaming)
                && x.tx.as_ref().map(|t| t.state()) != Some(SenderState::Sending);
            if waiting_on_peer && now.saturating_sub(x.last_activity) > idle_ms {
                x.phase = Phase::Failed(format!(
                    "no traffic from {} for {} s",
                    x.peer,
                    idle_ms / 1000
                ));
                if x.remote_id != 0 {
                    let f = Frame::new(
                        0,
                        id::CONTROL,
                        control::END,
                        &me,
                        &x.peer,
                        session::encode_control_end(x.remote_id),
                    );
                    Self::send(&mut out, x, f);
                }
            }
            Self::push_update(&mut out, x);
        }
        out
    }

    /// Handle a decoded frame from `port`. Frames not addressed to us, or not
    /// belonging to the control session or one of our stateful sessions,
    /// are ignored (the caller has already dropped looped frames).
    pub fn on_frame(
        &mut self,
        now: Millis,
        port: &str,
        f: &Frame,
        receive_cfg: impl FnOnce() -> SenderConfig,
    ) -> Vec<XEffect> {
        let src = String::from_utf8_lossy(&f.src).to_ascii_uppercase();
        let dst = String::from_utf8_lossy(&f.dst).to_ascii_uppercase();
        let mut out = Vec::new();
        if dst != self.callsign || src == self.callsign {
            return out;
        }
        self.rebase(port, now);
        match f.session {
            id::CONTROL => self.on_control(now, port, &src, f, receive_cfg, &mut out),
            id::CHAT | id::RPC => {}
            sid => {
                let me = self.callsign.clone();
                let limits = self.limits.clone();
                if let Some(x) = self
                    .xfers
                    .values_mut()
                    .find(|x| x.local_id == sid && x.peer == src && !x.finished())
                {
                    x.last_activity = now;
                    Self::on_session_frame(now, &me, &limits, x, f, &mut out);
                    Self::push_update(&mut out, x);
                }
            }
        }
        out
    }

    fn on_control(
        &mut self,
        now: Millis,
        port: &str,
        src: &str,
        f: &Frame,
        receive_cfg: impl FnOnce() -> SenderConfig,
        out: &mut Vec<XEffect>,
    ) {
        match f.kind {
            control::ACK => {
                let Ok((mine, theirs)) = session::decode_control_ack(&f.payload) else {
                    return;
                };
                let me = self.callsign.clone();
                if let Some(x) = self
                    .xfers
                    .values_mut()
                    .find(|x| x.local_id == mine && x.peer == src && x.phase == Phase::Opening)
                {
                    x.remote_id = theirs;
                    x.last_activity = now;
                    let offer = session::encode_file_offer(x.stream.len() as u32, &x.name);
                    x.stream_base = offer.len();
                    let addr = x.addr(&me);
                    x.tx = Some(LegacySender::new(x.cfg.clone(), addr.clone(), &offer));
                    x.rx = Some(LegacyReceiver::new(addr));
                    x.phase = Phase::Offering;
                    let frames =
                        x.tx.as_mut()
                            .map(|t| t.poll_transmit(now))
                            .unwrap_or_default();
                    for fr in frames {
                        Self::send(out, x, fr);
                    }
                    Self::push_update(out, x);
                }
            }
            control::END => {
                let Ok(sid) = session::decode_control_end(&f.payload) else {
                    return;
                };
                let me = self.callsign.clone();
                let limits = self.limits.clone();
                let Some(x) = self
                    .xfers
                    .values_mut()
                    .find(|x| x.local_id == sid && x.peer == src && !x.finished())
                else {
                    return; // unknown or already closed: no reply (stops the END echo)
                };
                x.last_activity = now;
                if x.phase != Phase::Closing {
                    // Peer-initiated close: confirm it (control.py ctl_end).
                    let fr = Frame::new(
                        0,
                        id::CONTROL,
                        control::END,
                        &me,
                        &x.peer,
                        session::encode_control_end(x.remote_id),
                    );
                    Self::send(out, x, fr);
                }
                match (x.dir, &x.phase) {
                    (Dir::Send, Phase::Closing) => x.phase = Phase::Done,
                    (Dir::Send, _) => {
                        x.phase = Phase::Failed(format!("{} closed the session", x.peer))
                    }
                    (Dir::Receive, Phase::Streaming) => Self::finish_receive(&limits, x, out),
                    (Dir::Receive, _) => {
                        x.phase = Phase::Failed(format!(
                            "{} closed the session before sending the file",
                            x.peer
                        ))
                    }
                }
                Self::push_update(out, x);
            }
            k if k >= control::NEW_BASE => {
                let Ok((rid, name)) = session::decode_control_new(&f.payload) else {
                    return;
                };
                let class = match k - control::NEW_BASE {
                    control::CLASS_FILE => Class::File,
                    control::CLASS_FORM => Class::Form,
                    other => {
                        out.push(XEffect::Log {
                            level: "warn",
                            message: format!(
                                "session request class {other} from {src} refused (only file and form transfers are supported)"
                            ),
                        });
                        return;
                    }
                };
                // A repeated NEW re-ACKs the existing session (control.py).
                if let Some(x) = self.xfers.values().find(|x| {
                    x.dir == Dir::Receive && x.peer == src && x.remote_id == rid && !x.finished()
                }) {
                    let fr = self.ctl(
                        src,
                        control::ACK,
                        session::encode_control_ack(rid, x.local_id),
                    );
                    let mut tmp = Vec::new();
                    Self::send(&mut tmp, x, fr);
                    out.extend(tmp);
                    return;
                }
                let running = self
                    .xfers
                    .values()
                    .filter(|x| x.dir == Dir::Receive && !x.finished())
                    .count();
                if running >= self.limits.max_incoming {
                    out.push(XEffect::Log {
                        level: "warn",
                        message: format!(
                            "transfer from {src} refused: {running} incoming transfers already running"
                        ),
                    });
                    return;
                }
                let Some(local_id) = self.alloc_id() else {
                    return;
                };
                let key = self.next_key;
                self.next_key += 1;
                let cfg = receive_cfg();
                let addr = SessionAddr {
                    session: rid,
                    src: self.callsign.clone(),
                    dst: src.to_string(),
                };
                let mut x = Xfer {
                    key,
                    dir: Dir::Receive,
                    class,
                    port: port.into(),
                    peer: src.to_string(),
                    local_id,
                    remote_id: rid,
                    name,
                    phase: Phase::Offering,
                    cfg: cfg.clone(),
                    ctl_attempts: 0,
                    ctl_deadline: now,
                    tx: Some(LegacySender::new(cfg, addr.clone(), &[])),
                    rx: Some(LegacyReceiver::new(addr)),
                    stream: Vec::new(),
                    stream_base: 0,
                    resume_at: 0,
                    wire_total: 0,
                    inbuf: Vec::new(),
                    last_activity: now,
                    wrap_diag: false,
                    last_snapshot: None,
                };
                let fr = self.ctl(
                    src,
                    control::ACK,
                    session::encode_control_ack(rid, local_id),
                );
                Self::send(out, &x, fr);
                Self::push_update(out, &mut x);
                self.xfers.insert(key, x);
            }
            _ => {} // PING: legacy ignores it too
        }
    }

    fn on_session_frame(
        now: Millis,
        me: &str,
        limits: &Limits,
        x: &mut Xfer,
        f: &Frame,
        out: &mut Vec<XEffect>,
    ) {
        if let Some(tx) = x.tx.as_mut() {
            tx.on_frame(now, f);
            for fr in tx.poll_transmit(now) {
                out.push(XEffect::Send {
                    port: x.port.clone(),
                    frame: fr,
                    encoding: ENCODING,
                    priority: x.class.priority(),
                });
            }
        }
        let mut evs = Vec::new();
        if let Some(rx) = x.rx.as_mut() {
            rx.on_frame(f, &mut evs);
        }
        for e in evs {
            match e {
                RxEvent::Transmit(fr) => out.push(XEffect::Send {
                    port: x.port.clone(),
                    frame: fr,
                    encoding: ENCODING,
                    priority: Priority::Control,
                }),
                RxEvent::Diag(RxDiag::PossibleWrapAmbiguity { seq }) => {
                    x.wrap_diag = true;
                    out.push(XEffect::Log {
                        level: "warn",
                        message: format!(
                            "{}: block {seq} repeated after a sequence wrap with different content (legacy defect P19); the file may be corrupt",
                            x.name
                        ),
                    });
                }
                RxEvent::Deliver(d) => Self::on_deliver(now, me, limits, x, d, out),
            }
        }
    }

    fn on_deliver(
        now: Millis,
        me: &str,
        limits: &Limits,
        x: &mut Xfer,
        d: Vec<u8>,
        out: &mut Vec<XEffect>,
    ) {
        let end = |x: &mut Xfer, out: &mut Vec<XEffect>, reason: String| {
            let fr = Frame::new(
                0,
                id::CONTROL,
                control::END,
                me,
                &x.peer,
                session::encode_control_end(x.remote_id),
            );
            Self::send(out, x, fr);
            x.phase = Phase::Failed(reason);
        };
        match (x.dir, &x.phase) {
            (Dir::Send, Phase::Offering) => {
                x.inbuf.extend(d);
                match session::decode_offer_reply(&x.inbuf) {
                    Ok(reply) => {
                        let offset = match reply {
                            session::OfferReply::Ok => 0,
                            session::OfferReply::Resume(n) => n.min(x.stream.len() as u64) as usize,
                        };
                        x.inbuf.clear();
                        x.resume_at = offset;
                        x.phase = Phase::Streaming;
                        if let Some(tx) = x.tx.as_mut() {
                            tx.append(&x.stream[offset..]);
                            for fr in tx.poll_transmit(now) {
                                out.push(XEffect::Send {
                                    port: x.port.clone(),
                                    frame: fr,
                                    encoding: ENCODING,
                                    priority: x.class.priority(),
                                });
                            }
                        }
                        if offset > 0 {
                            out.push(XEffect::Log {
                                level: "info",
                                message: format!(
                                    "{}: {} asked to resume at byte {offset}",
                                    x.name, x.peer
                                ),
                            });
                        }
                    }
                    Err(_) if x.inbuf.len() > 64 => {
                        // file.py ignores unknown answers; don't grow forever.
                        x.inbuf.clear();
                    }
                    Err(_) => {}
                }
            }
            (Dir::Receive, Phase::Offering) => {
                x.inbuf.extend(d);
                match session::decode_file_offer(&x.inbuf) {
                    Ok((len, name)) => {
                        x.inbuf.clear();
                        if len > limits.max_wire_bytes {
                            out.push(XEffect::Log {
                                level: "warn",
                                message: format!(
                                    "refused {name} from {}: {len} bytes exceeds the {}-byte limit",
                                    x.peer, limits.max_wire_bytes
                                ),
                            });
                            x.name = name;
                            x.wire_total = len as u64;
                            end(x, out, "offer exceeds the incoming size limit".into());
                            return;
                        }
                        x.name = name;
                        x.wire_total = len as u64;
                        x.phase = Phase::Streaming;
                        if let Some(tx) = x.tx.as_mut() {
                            tx.append(&session::encode_offer_reply(&session::OfferReply::Ok));
                            for fr in tx.poll_transmit(now) {
                                out.push(XEffect::Send {
                                    port: x.port.clone(),
                                    frame: fr,
                                    encoding: ENCODING,
                                    priority: x.class.priority(),
                                });
                            }
                        }
                    }
                    Err(e) => {
                        let reason = format!("unusable offer from {}: {e:?}", x.peer);
                        out.push(XEffect::Log {
                            level: "warn",
                            message: reason.clone(),
                        });
                        end(x, out, reason);
                    }
                }
            }
            (Dir::Receive, Phase::Streaming) => {
                x.stream.extend(d);
                if x.stream.len() as u64 > x.wire_total {
                    let reason = format!(
                        "{} sent more than the {} bytes it offered",
                        x.peer, x.wire_total
                    );
                    end(x, out, reason);
                }
            }
            _ => {} // data outside the expected phase: ignore, as legacy does
        }
    }

    fn finish_receive(limits: &Limits, x: &mut Xfer, out: &mut Vec<XEffect>) {
        if (x.stream.len() as u64) != x.wire_total {
            x.phase = Phase::Failed(format!(
                "incomplete: {} of {} bytes received",
                x.stream.len(),
                x.wire_total
            ));
            return;
        }
        match inflate_limited(&x.stream, limits.max_object_bytes) {
            Ok(data) => {
                x.phase = Phase::Done;
                out.push(XEffect::Received {
                    key: x.key,
                    class: x.class,
                    port: x.port.clone(),
                    peer: x.peer.clone(),
                    name: x.name.clone(),
                    data,
                });
            }
            Err(e) => x.phase = Phase::Failed(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> SenderConfig {
        let mut c = SenderConfig::legacy_defaults();
        c.block_size = 256;
        c.rto_initial = 5_000;
        c
    }

    /// Run two stations against each other with an optional frame filter.
    /// Returns (sender effects, receiver effects) in order.
    fn run(
        a: &mut Transfers,
        b: &mut Transfers,
        first: Vec<XEffect>,
        mut drop: impl FnMut(&Frame) -> bool,
        until: Millis,
    ) -> (Vec<XEffect>, Vec<XEffect>) {
        let (mut ea, mut eb) = (Vec::new(), Vec::new());
        let mut q: Vec<(bool, Frame)> = Vec::new(); // (to_b, frame)
        let handle = |effs: Vec<XEffect>,
                      from_a: bool,
                      q: &mut Vec<(bool, Frame)>,
                      log: &mut Vec<XEffect>| {
            for e in effs {
                if let XEffect::Send { frame, .. } = &e {
                    q.push((from_a, frame.clone()));
                }
                log.push(e);
            }
        };
        handle(first, true, &mut q, &mut ea);
        let mut now = 0;
        while now < until {
            while !q.is_empty() {
                let (to_b, f) = q.remove(0);
                // Every frame goes through the real codec.
                let raw = f.encode(ENCODING).unwrap();
                let f = Frame::decode(&raw).unwrap();
                if drop(&f) {
                    continue;
                }
                if to_b {
                    let e = b.on_frame(now, "net", &f, cfg);
                    handle(e, false, &mut q, &mut eb);
                } else {
                    let e = a.on_frame(now, "net", &f, cfg);
                    handle(e, true, &mut q, &mut ea);
                }
            }
            now += 500;
            let e = a.poll(now);
            handle(e, true, &mut q, &mut ea);
            let e = b.poll(now);
            handle(e, false, &mut q, &mut eb);
        }
        (ea, eb)
    }

    fn last_state(effs: &[XEffect]) -> Snapshot {
        effs.iter()
            .rev()
            .find_map(|e| match e {
                XEffect::Update(s) => Some(s.clone()),
                _ => None,
            })
            .expect("an update")
    }

    fn received(effs: &[XEffect]) -> Option<(String, Vec<u8>)> {
        effs.iter().find_map(|e| match e {
            XEffect::Received { name, data, .. } => Some((name.clone(), data.clone())),
            _ => None,
        })
    }

    fn content(n: usize) -> Vec<u8> {
        let mut s = 7u64;
        (0..n)
            .map(|i| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                if i % 3 == 0 {
                    s as u8
                } else {
                    b'a' + (i % 26) as u8
                }
            })
            .collect()
    }

    #[test]
    fn file_transfer_end_to_end_through_the_codec() {
        let mut a = Transfers::new("KK4ODA", Limits::default());
        let mut b = Transfers::new("W4DRS", Limits::default());
        let data = content(10_000);
        let (_, first) = a
            .start_send(
                0,
                SendRequest {
                    port: "net",
                    peer: "W4DRS",
                    class: Class::File,
                    name: "report.txt",
                    content: &data,
                    cfg: cfg(),
                },
            )
            .unwrap();
        let (ea, eb) = run(&mut a, &mut b, first, |_| false, 600_000);
        assert_eq!(received(&eb), Some(("report.txt".into(), data)));
        let sa = last_state(&ea);
        assert_eq!(sa.state, "done");
        assert_eq!(sa.wire_done, sa.wire_total);
        assert_eq!(last_state(&eb).state, "done");
        // Exactly one END from each side, and no echo.
        let ends = |effs: &[XEffect]| {
            effs.iter()
                .filter(|e| matches!(e, XEffect::Send { frame, .. } if frame.session == 0 && frame.kind == control::END))
                .count()
        };
        assert_eq!(ends(&ea), 1);
        assert_eq!(ends(&eb), 1);
    }

    #[test]
    fn wire_shape_matches_legacy() {
        let mut a = Transfers::new("KK4ODA", Limits::default());
        let (_, first) = a
            .start_send(
                0,
                SendRequest {
                    port: "net",
                    peer: "w4drs",
                    class: Class::Form,
                    name: "ICS-213",
                    content: b"<form/>",
                    cfg: cfg(),
                },
            )
            .unwrap();
        let XEffect::Send { frame: new, .. } = first
            .iter()
            .find(|e| matches!(e, XEffect::Send { .. }))
            .unwrap()
        else {
            unreachable!()
        };
        // NEW for a form session is control type 3 + 6 = 9, payload id + name.
        assert_eq!((new.session, new.kind), (0, 9));
        assert_eq!(new.payload[0], id::FIRST_DYNAMIC);
        assert_eq!(&new.payload[1..], b"ICS-213");
        assert_eq!(&new.dst[..5], b"W4DRS");
        // The peer ACKs with (our id, its id); the offer goes to ITS id as block 0.
        let ack = Frame::new(
            0,
            0,
            control::ACK,
            "W4DRS",
            "KK4ODA",
            vec![id::FIRST_DYNAMIC, 11],
        );
        let e = a.on_frame(1_000, "net", &ack, cfg);
        let frames: Vec<&Frame> = e
            .iter()
            .filter_map(|e| match e {
                XEffect::Send { frame, .. } => Some(frame),
                _ => None,
            })
            .collect();
        assert_eq!(frames.len(), 2);
        assert_eq!(
            (frames[0].session, frames[0].kind, frames[0].seq),
            (11, 4, 0)
        );
        let z = zlib(b"<form/>");
        assert_eq!(&frames[0].payload[..4], &(z.len() as u32).to_le_bytes());
        assert_eq!(&frames[0].payload[4..], b"ICS-213");
        assert_eq!((frames[1].kind, frames[1].payload.clone()), (5, vec![0]));
    }

    #[test]
    fn transfer_survives_loss_of_every_fifth_frame() {
        let mut a = Transfers::new("KK4ODA", Limits::default());
        let mut b = Transfers::new("W4DRS", Limits::default());
        let data = content(6_000);
        let (_, first) = a
            .start_send(
                0,
                SendRequest {
                    port: "net",
                    peer: "W4DRS",
                    class: Class::File,
                    name: "x.bin",
                    content: &data,
                    cfg: cfg(),
                },
            )
            .unwrap();
        let mut n = 0;
        let (ea, eb) = run(
            &mut a,
            &mut b,
            first,
            |_| {
                n += 1;
                n % 5 == 0
            },
            1_800_000,
        );
        assert_eq!(received(&eb).map(|r| r.1), Some(data));
        assert_eq!(last_state(&ea).state, "done");
    }

    #[test]
    fn oversized_offer_is_refused_with_end() {
        let mut a = Transfers::new("KK4ODA", Limits::default());
        let mut b = Transfers::new(
            "W4DRS",
            Limits {
                max_wire_bytes: 100,
                ..Limits::default()
            },
        );
        let (_, first) = a
            .start_send(
                0,
                SendRequest {
                    port: "net",
                    peer: "W4DRS",
                    class: Class::File,
                    name: "big.bin",
                    content: &content(5_000),
                    cfg: cfg(),
                },
            )
            .unwrap();
        let (ea, eb) = run(&mut a, &mut b, first, |_| false, 120_000);
        assert!(received(&eb).is_none());
        assert_eq!(last_state(&eb).state, "failed");
        let sa = last_state(&ea);
        assert_eq!(sa.state, "failed");
        assert!(sa.reason.unwrap().contains("closed the session"));
    }

    #[test]
    fn unanswered_session_request_fails_after_ten_attempts() {
        let mut a = Transfers::new("KK4ODA", Limits::default());
        let mut sent = 0;
        let (_, first) = a
            .start_send(
                0,
                SendRequest {
                    port: "net",
                    peer: "W4DRS",
                    class: Class::File,
                    name: "f",
                    content: b"x",
                    cfg: cfg(),
                },
            )
            .unwrap();
        sent += first
            .iter()
            .filter(|e| matches!(e, XEffect::Send { .. }))
            .count();
        let mut now = 0;
        let mut last = None;
        while now < 400_000 {
            now += 1_000;
            for e in a.poll(now) {
                match e {
                    XEffect::Send { .. } => sent += 1,
                    XEffect::Update(s) => last = Some(s),
                    _ => {}
                }
            }
        }
        assert_eq!(sent, 10);
        assert_eq!(last.unwrap().state, "failed");
    }

    #[test]
    fn paused_port_stops_retries_and_resume_shifts_them() {
        let mut a = Transfers::new("KK4ODA", Limits::default());
        let req = || SendRequest {
            port: "rf",
            peer: "W4DRS",
            class: Class::File,
            name: "f",
            content: b"x",
            cfg: cfg(),
        };
        let (_, first) = a.start_send(0, req()).unwrap();
        assert!(first.iter().any(|e| matches!(e, XEffect::Send { .. })));
        let due = a.next_deadline().unwrap();
        a.pause_port("rf", 1_000);
        assert!(a.port_paused("rf"));
        // Ten minutes of voice: no retries, no failure.
        for t in (1_000..601_000).step_by(1_000) {
            assert!(!a.poll(t).iter().any(|e| matches!(e, XEffect::Send { .. })));
        }
        a.resume_port("rf", 601_000);
        assert_eq!(a.next_deadline(), Some(due + 600_000));
        assert_eq!(a.snapshots()[0].state, "opening");
        let sends = a
            .poll(due + 600_000)
            .iter()
            .filter(|e| matches!(e, XEffect::Send { .. }))
            .count();
        assert_eq!(sends, 1, "the next retry, not a burst of missed ones");
    }

    #[test]
    fn timers_wait_while_a_burst_is_written_and_holds_nest() {
        // A burst took 19 s to write (bench large-file test); its retry
        // fired mid-write and collided with the peer's answer.
        let mut a = Transfers::new("KK4ODA", Limits::default());
        let req = SendRequest {
            port: "rf",
            peer: "W4DRS",
            class: Class::File,
            name: "f",
            content: b"x",
            cfg: cfg(),
        };
        a.start_send(0, req).unwrap();
        let due = a.next_deadline().unwrap();
        a.hold("rf", Hold::Write, 0);
        assert!(
            !a.poll(due + 30_000)
                .iter()
                .any(|e| matches!(e, XEffect::Send { .. })),
            "no retry while writing"
        );
        // Voice starts during the write and outlasts it.
        a.pause_port("rf", 10_000);
        a.release("rf", Hold::Write, 19_000);
        assert!(a.held("rf", Hold::Channel) && !a.held("rf", Hold::Write));
        assert_eq!(a.next_deadline(), None, "still held for voice");
        a.resume_port("rf", 25_000);
        assert_eq!(a.next_deadline(), Some(due + 25_000));
        assert!(a
            .poll(due + 25_000)
            .iter()
            .any(|e| matches!(e, XEffect::Send { .. })));
    }

    #[test]
    fn repeated_new_is_re_acked_with_the_same_id_and_other_classes_refused() {
        let mut b = Transfers::new("W4DRS", Limits::default());
        let new = Frame::new(
            0,
            0,
            8,
            "KK4ODA",
            "W4DRS",
            session::encode_control_new(6, "a.txt"),
        );
        let acks = |e: Vec<XEffect>| -> Vec<Vec<u8>> {
            e.into_iter()
                .filter_map(|e| match e {
                    XEffect::Send { frame, .. } if frame.kind == control::ACK => {
                        Some(frame.payload)
                    }
                    _ => None,
                })
                .collect()
        };
        let first = acks(b.on_frame(0, "net", &new, cfg));
        let again = acks(b.on_frame(5_000, "net", &new, cfg));
        assert_eq!(first, vec![vec![6, id::FIRST_DYNAMIC]]);
        assert_eq!(again, first);
        let socket = Frame::new(
            0,
            0,
            7,
            "KK4ODA",
            "W4DRS",
            session::encode_control_new(7, "s"),
        );
        assert!(acks(b.on_frame(0, "net", &socket, cfg)).is_empty());
        // Not for us: ignored.
        let other = Frame::new(
            0,
            0,
            8,
            "KK4ODA",
            "N0CALL",
            session::encode_control_new(8, "b"),
        );
        assert!(b.on_frame(0, "net", &other, cfg).is_empty());
    }

    #[test]
    fn end_for_an_unknown_session_is_not_answered() {
        let mut b = Transfers::new("W4DRS", Limits::default());
        let end = Frame::new(0, 0, control::END, "KK4ODA", "W4DRS", b"42".to_vec());
        assert!(b.on_frame(0, "net", &end, cfg).is_empty());
    }

    #[test]
    fn receives_from_a_scripted_legacy_sender() {
        // Exactly what D-RATS 0.4.x file.py + stateful.py put on the wire:
        // NEW type 8, offer as block 0, `zlib.compress(data, 9)` in 512-byte
        // blocks from seq 1, every frame compressed (magic 0xDD), one
        // REQACK per window of 4, then END with our session id.
        let mut b = Transfers::new("W4DRS", Limits::default());
        let wire = |f: Frame| {
            let raw = f.encode(PayloadEncoding::Always).unwrap();
            assert_eq!(raw[0], 0xDD);
            Frame::decode(&raw).unwrap()
        };
        let sends = |e: &[XEffect]| -> Vec<Frame> {
            e.iter()
                .filter_map(|e| match e {
                    XEffect::Send { frame, .. } => Some(frame.clone()),
                    _ => None,
                })
                .collect()
        };
        let data = content(3_000);
        let z = zlib(&data);
        let new = wire(Frame::new(
            0,
            0,
            8,
            "KK4ODA",
            "W4DRS",
            session::encode_control_new(5, "log.txt"),
        ));
        let e = b.on_frame(0, "rf", &new, cfg);
        let ack = &sends(&e)[0];
        assert_eq!((ack.session, ack.kind), (0, control::ACK));
        let ours = ack.payload[1];
        assert_eq!(ack.payload[0], 5);
        // Offer, block 0.
        let offer = session::encode_file_offer(z.len() as u32, "log.txt");
        let e1 = b.on_frame(
            1_000,
            "rf",
            &wire(Frame::new(0, ours, 4, "KK4ODA", "W4DRS", offer)),
            cfg,
        );
        let e2 = b.on_frame(
            1_100,
            "rf",
            &wire(Frame::new(0, ours, 5, "KK4ODA", "W4DRS", vec![0])),
            cfg,
        );
        let mut out = sends(&e1);
        out.extend(sends(&e2));
        // ACK of block 0, plus our OK as OUR block 0 to THEIR session 5.
        assert!(out
            .iter()
            .any(|f| f.session == 5 && f.kind == 1 && f.payload == vec![0]));
        let ok = out.iter().find(|f| f.kind == 4).expect("OK block");
        assert_eq!(
            (ok.session, ok.seq, ok.payload.as_slice()),
            (5, 0, &b"OK"[..])
        );
        // The sender's ACK of our OK.
        b.on_frame(
            20_000,
            "rf",
            &wire(Frame::new(0, ours, 1, "KK4ODA", "W4DRS", vec![0])),
            cfg,
        );
        // The stream from seq 1.
        let blocks: Vec<&[u8]> = z.chunks(512).collect();
        let mut t = 21_000;
        for (w, window) in blocks.chunks(4).enumerate() {
            let mut seqs = Vec::new();
            for (k, blk) in window.iter().enumerate() {
                let seq = (1 + w * 4 + k) as u16;
                seqs.push(seq as u8);
                b.on_frame(
                    t,
                    "rf",
                    &wire(Frame::new(seq, ours, 4, "KK4ODA", "W4DRS", blk.to_vec())),
                    cfg,
                );
                t += 100;
            }
            let e = b.on_frame(
                t,
                "rf",
                &wire(Frame::new(0, ours, 5, "KK4ODA", "W4DRS", seqs.clone())),
                cfg,
            );
            let acked = sends(&e).into_iter().find(|f| f.kind == 1).unwrap();
            assert_eq!(acked.payload, seqs);
            t += 5_000;
        }
        let e = b.on_frame(
            t,
            "rf",
            &wire(Frame::new(
                0,
                0,
                control::END,
                "KK4ODA",
                "W4DRS",
                ours.to_string().into_bytes(),
            )),
            cfg,
        );
        // END confirmed with the sender's id; the file is complete.
        let end = sends(&e)
            .into_iter()
            .find(|f| f.kind == control::END)
            .unwrap();
        assert_eq!(end.payload, b"5");
        assert_eq!(received(&e), Some(("log.txt".into(), data)));
        // The sender's own END echo (for an id we no longer have open) is ignored.
        let echo = wire(Frame::new(
            0,
            0,
            control::END,
            "KK4ODA",
            "W4DRS",
            ours.to_string().into_bytes(),
        ));
        assert!(sends(&b.on_frame(t + 1, "rf", &echo, cfg)).is_empty());
    }

    #[test]
    fn inflate_limit_is_enforced() {
        let z = zlib(&vec![0u8; 100_000]);
        assert!(inflate_limited(&z, 1_000).is_err());
        assert_eq!(inflate_limited(&z, 100_000).unwrap().len(), 100_000);
        assert!(inflate_limited(b"not zlib", 10).is_err());
    }
}
