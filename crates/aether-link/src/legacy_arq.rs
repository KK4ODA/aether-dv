//! ARQ-1: the D-RATS stateful-session reliability protocol, wire-compatible
//! with D-RATS 0.4.x (`d_rats/sessions/stateful.py@d0e7a39`), as specified in
//! `docs/phase-b/state-machines.md` SM-4.
//!
//! Sans-I/O: callers pass a monotonic time in milliseconds and frames in and
//! out. Nothing here touches clocks, ports or sockets.
//!
//! Kept from legacy (required for interop): 8-bit block sequence, blocks sent
//! in ascending order followed by one REQACK listing their seqs, re-poll with
//! REQACK only while waiting, the rollover pause (seq 0 never joins a
//! non-empty window), 10 attempts.
//! Changed (sender-local, wire-compatible):
//! * RTO from measured round trips (RFC 6298 form, Karn's rule) instead of
//!   serial-write rates and a fixed 12 s floor;
//! * an ACK arriving before the current burst could possibly have been
//!   answered is applied as information only and does not end the wait
//!   (prevents stale-ACK duplicate bursts, defect P20);
//! * the receiver keeps legacy semantics but reports the signature of the
//!   post-wrap false-ACK defect (P19) instead of silently delivering wrong data.

use std::collections::HashMap;

use aether_ddt2::session::stateful;
use aether_ddt2::{Frame, PayloadEncoding};

/// Monotonic milliseconds supplied by the caller.
pub type Millis = u64;

/// Addressing for one direction of a stateful session.
#[derive(Debug, Clone)]
pub struct SessionAddr {
    /// The session id to put in outgoing frames (the peer's id for us).
    pub session: u8,
    pub src: String,
    pub dst: String,
}

#[derive(Debug, Clone)]
pub struct SenderConfig {
    pub block_size: usize,
    pub window: usize,
    pub max_attempts: u32,
    pub rto_initial: Millis,
    pub rto_min: Millis,
    pub rto_max: Millis,
    /// Estimated payload throughput of the path, bytes per second (ModeProfile).
    pub path_rate_bps: u32,
    /// Fixed per-over cost: carrier sense + key-up + tail (ModeProfile).
    pub per_over_ms: Millis,
}

impl SenderConfig {
    /// D-RATS defaults: 512 B blocks, window 4 (`config_defaults.py:129-130`).
    pub fn legacy_defaults() -> Self {
        SenderConfig {
            block_size: 512,
            window: 4,
            max_attempts: 10,
            rto_initial: 30_000,
            rto_min: 4_000,
            rto_max: 60_000,
            path_rate_bps: 119,
            per_over_ms: 1_100,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SenderState {
    Sending,
    Done,
    Failed,
}

#[derive(Debug, Default, Clone)]
pub struct SenderStats {
    pub bursts: u32,
    pub repolls: u32,
    pub blocks_sent: u32,
    pub blocks_retransmitted: u32,
    pub early_acks_ignored: u32,
}

/// Sender side of one stateful transfer.
#[derive(Debug)]
pub struct LegacySender {
    cfg: SenderConfig,
    addr: SessionAddr,
    blocks: Vec<Vec<u8>>,
    next: usize,
    outstanding: Vec<usize>,
    ever_sent: Vec<bool>,
    waiting: Option<Vec<u8>>,
    attempts: u32,
    deadline: Millis,
    burst_sent_at: Millis,
    earliest_answer: Millis,
    repolled: bool,
    srtt: Option<f64>,
    rttvar: f64,
    rto: Millis,
    state: SenderState,
    pub stats: SenderStats,
}

fn seq_of(idx: usize) -> u8 {
    (idx % 256) as u8
}

impl LegacySender {
    pub fn new(cfg: SenderConfig, addr: SessionAddr, data: &[u8]) -> Self {
        let blocks: Vec<Vec<u8>> = data
            .chunks(cfg.block_size.max(1))
            .map(|c| c.to_vec())
            .collect();
        let n = blocks.len();
        let rto = cfg.rto_initial;
        LegacySender {
            cfg,
            addr,
            blocks,
            next: 0,
            outstanding: Vec::new(),
            ever_sent: vec![false; n],
            waiting: None,
            attempts: 0,
            deadline: 0,
            burst_sent_at: 0,
            earliest_answer: 0,
            repolled: false,
            srtt: None,
            rttvar: 0.0,
            rto,
            state: if n == 0 {
                SenderState::Done
            } else {
                SenderState::Sending
            },
            stats: SenderStats::default(),
        }
    }

    pub fn state(&self) -> SenderState {
        self.state
    }

    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }

    /// True when a legacy receiver could hit the post-wrap false ACK (P19).
    pub fn wrap_warning(&self) -> bool {
        self.blocks.len() > 256
    }

    pub fn current_rto(&self) -> Millis {
        self.rto
    }

    /// When the caller should next call [`Self::poll_transmit`].
    pub fn next_deadline(&self) -> Option<Millis> {
        (self.state == SenderState::Sending).then_some(self.deadline)
    }

    fn frame(&self, kind: u8, seq: u16, payload: Vec<u8>) -> Frame {
        Frame::new(
            seq,
            self.addr.session,
            kind,
            &self.addr.src,
            &self.addr.dst,
            payload,
        )
    }

    fn air_ms(&self, bytes: usize) -> Millis {
        self.cfg.per_over_ms + (bytes as u64 * 1000) / self.cfg.path_rate_bps.max(1) as u64
    }

    /// Produce the next burst if it is time. Frames are returned in
    /// transmission order; encode them with [`PayloadEncoding::IfSmaller`].
    pub fn poll_transmit(&mut self, now: Millis) -> Vec<Frame> {
        if self.state != SenderState::Sending || now < self.deadline {
            return vec![];
        }
        if let (Some(list), false) = (self.waiting.clone(), self.outstanding.is_empty()) {
            if self.attempts >= self.cfg.max_attempts {
                self.state = SenderState::Failed;
                return vec![];
            }
            self.attempts += 1;
            self.repolled = true;
            self.stats.repolls += 1;
            let backoff = (4_000 + 4_000 * self.attempts as u64).max(self.rto);
            self.deadline = now + backoff;
            return vec![self.frame(stateful::REQACK, 0, list)];
        }
        // Fill the window, honouring the legacy rollover pause.
        while self.outstanding.len() < self.cfg.window && self.next < self.blocks.len() {
            if seq_of(self.next) == 0 && !self.outstanding.is_empty() {
                break;
            }
            self.outstanding.push(self.next);
            self.next += 1;
        }
        if self.outstanding.is_empty() {
            self.state = SenderState::Done;
            return vec![];
        }
        let mut frames = Vec::with_capacity(self.outstanding.len() + 1);
        let mut bytes = 0;
        for &i in &self.outstanding {
            if self.ever_sent[i] {
                self.stats.blocks_retransmitted += 1;
            }
            self.ever_sent[i] = true;
            self.stats.blocks_sent += 1;
            bytes += self.blocks[i].len() + 35;
            frames.push(self.frame(stateful::DATA, seq_of(i) as u16, self.blocks[i].clone()));
        }
        let list: Vec<u8> = self.outstanding.iter().map(|&i| seq_of(i)).collect();
        frames.push(self.frame(stateful::REQACK, 0, list.clone()));
        self.waiting = Some(list);
        self.stats.bursts += 1;
        self.burst_sent_at = now;
        self.repolled = false;
        let air = self.air_ms(bytes + 40);
        // An answer needs our burst on air plus at least one over back.
        self.earliest_answer = now + air + self.cfg.per_over_ms;
        self.deadline = now + air + self.rto;
        frames
    }

    fn rtt_sample(&mut self, sample: f64) {
        match self.srtt {
            None => {
                self.srtt = Some(sample);
                self.rttvar = sample / 2.0;
            }
            Some(s) => {
                self.rttvar = 0.75 * self.rttvar + 0.25 * (s - sample).abs();
                self.srtt = Some(0.875 * s + 0.125 * sample);
            }
        }
        let rto = self.srtt.unwrap() + 4.0 * self.rttvar;
        self.rto = (rto as Millis).clamp(self.cfg.rto_min, self.cfg.rto_max);
    }

    /// Handle a frame received on this session.
    pub fn on_frame(&mut self, now: Millis, f: &Frame) {
        if self.state != SenderState::Sending || f.kind != stateful::ACK {
            return;
        }
        let acked: Vec<u8> = f.payload.clone();
        self.outstanding.retain(|&i| !acked.contains(&seq_of(i)));
        if now < self.earliest_answer {
            // Too early to be the answer to the current REQACK: a stale ACK
            // from an earlier poll. Keep its information, keep waiting (P20).
            self.stats.early_acks_ignored += 1;
            return;
        }
        if !self.repolled {
            // Karn: only sample unambiguous round trips.
            let air_end = self.earliest_answer.saturating_sub(self.cfg.per_over_ms);
            self.rtt_sample(now.saturating_sub(air_end) as f64);
        }
        self.waiting = None;
        self.attempts = 0;
        // Selective repeat: resend what's missing (or move on) right away.
        self.deadline = now;
        if self.outstanding.is_empty() && self.next >= self.blocks.len() {
            self.state = SenderState::Done;
        }
    }
}

/// Diagnostics emitted by the receiver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RxDiag {
    /// A block arrived after a completed sequence cycle with a seq the legacy
    /// rule treats as a duplicate, but its content differs from the stored
    /// block: the signature of the post-wrap false ACK (P19).
    PossibleWrapAmbiguity { seq: u8 },
}

/// What the receiver wants done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RxEvent {
    Deliver(Vec<u8>),
    Transmit(Frame),
    Diag(RxDiag),
}

/// Receiver side, legacy semantics (cycle-based `recv_list`).
#[derive(Debug)]
pub struct LegacyReceiver {
    addr: SessionAddr,
    recv_list: [bool; 256],
    iseq: i16,
    oob: HashMap<u8, Vec<u8>>,
    content_hash: [u64; 256],
}

fn fnv1a(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
        (h ^ b as u64).wrapping_mul(0x0100_0000_01b3)
    })
}

impl LegacyReceiver {
    pub fn new(addr: SessionAddr) -> Self {
        LegacyReceiver {
            addr,
            recv_list: [false; 256],
            iseq: -1,
            oob: HashMap::new(),
            content_hash: [0; 256],
        }
    }

    pub fn on_frame(&mut self, f: &Frame, out: &mut Vec<RxEvent>) {
        match f.kind {
            stateful::DATA => {
                let seq = (f.seq & 0xFF) as u8;
                if seq == 0 && self.iseq == 255 {
                    self.recv_list = [false; 256];
                }
                if !self.recv_list[seq as usize] {
                    self.recv_list[seq as usize] = true;
                    self.content_hash[seq as usize] = fnv1a(&f.payload);
                    self.oob.insert(seq, f.payload.clone());
                } else if self.iseq == 255
                    && self.oob.is_empty()
                    && fnv1a(&f.payload) != self.content_hash[seq as usize]
                {
                    out.push(RxEvent::Diag(RxDiag::PossibleWrapAmbiguity { seq }));
                }
                loop {
                    let next = ((self.iseq + 1) % 256) as u8;
                    match self.oob.remove(&next) {
                        Some(d) => {
                            self.iseq = next as i16;
                            out.push(RxEvent::Deliver(d));
                        }
                        None => break,
                    }
                }
            }
            stateful::REQACK => {
                let ack: Vec<u8> = f
                    .payload
                    .iter()
                    .copied()
                    .filter(|&s| self.recv_list[s as usize])
                    .collect();
                out.push(RxEvent::Transmit(Frame::new(
                    0,
                    self.addr.session,
                    stateful::ACK,
                    &self.addr.src,
                    &self.addr.dst,
                    ack,
                )));
            }
            _ => {}
        }
    }
}

/// Encoding to use for ARQ-1 frames (compress only when it helps; P3).
pub const ENCODING: PayloadEncoding = PayloadEncoding::IfSmaller;
