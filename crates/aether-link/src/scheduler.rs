//! SM-2: per-port half-duplex channel scheduler (sans-I/O).
//! Spec: `docs/phase-b/state-machines.md` SM-2; repeater rules
//! `docs/10-repeater-operation.md` §4.3-4.4.
//!
//! The scheduler is the only component that decides *when* bytes go to a
//! radio. It never reports delivery: `PortAccepted` only starts the air-time
//! estimate (N2.2).
//!
//! An over may start only when all of these hold:
//! * nothing holds the port (operator pause, route guard);
//! * no channel-sense source reports the channel busy;
//! * data reception has been quiet for `rx_quiet_ms`, plus the assumed
//!   repeater tail (when nothing senses the repeater directly), plus the
//!   hold-off and a random jitter;
//! * any sensed or inferred voice ended at least `voice_gap_ms` ago;
//! * our previous over ended at least `min_gap_ms` ago;
//! * the air-time budget allows it (Control and Chat have a reserve).

use std::collections::VecDeque;

use crate::channel::Busy;
use crate::legacy_arq::Millis;

/// Timing and size limits for one transport mode on one kind of path.
/// Every default below is a placeholder until hardware tests (HW-4,
/// HW-R10) measure real radios and repeaters.
#[derive(Debug, Clone)]
pub struct ModeProfile {
    pub name: &'static str,
    /// Application payload throughput on air, bytes per second.
    pub payload_rate_bps: u32,
    /// Carrier sense + key-up + RF header before the first data byte.
    pub keyup_ms: Millis,
    /// Radio holds TX after the last byte (e.g. TH-D75 Menu 614).
    pub tail_ms: Millis,
    pub max_burst_bytes: usize,
    pub max_burst_ms: Millis,
    /// Quiet time required after receive activity before transmitting.
    pub holdoff_ms: Millis,
    /// Upper bound of the random extra back-off after receive activity.
    pub jitter_ms: Millis,
    /// Gap that ends an inbound burst.
    pub rx_quiet_ms: Millis,
    /// Minimum time between the end of our over and the start of the
    /// next, so a voice user can break in.
    pub min_gap_ms: Millis,
    /// Wait after voice ends before transmitting (turnarounds in a QSO).
    pub voice_gap_ms: Millis,
    /// Repeater acknowledgement assumed after received data when nothing
    /// senses the channel directly (MMDVMHost: 750 ms + about 0.5 s).
    pub repeater_tail_ms: Millis,
    /// Air-time budget in permille of `duty_window_ms` (0 = none).
    pub duty_permille: u32,
    pub duty_window_ms: Millis,
    /// Extra allowance above the budget for Control and Chat.
    pub reserve_permille: u32,
}

/// Courtesy presets for the path a port uses (docs/10 §4.4, §4.11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathPreset {
    Direct,
    Repeater,
    Reflector,
}

impl ModeProfile {
    /// DV slow data. Air time measured on an IC-9700 and ID-52 PLUS
    /// (bench Test 5, 2026-11): 0.67 s + 1 s per 122 bytes from TX on to
    /// TX off. Key-up (hand-off to TX on) is radio-specific: about 0.3 s on
    /// the ID-52 PLUS, 0.1–3.4 s on the IC-9700; 600 ms is a typical value.
    pub fn dv_slow_placeholder() -> Self {
        ModeProfile {
            name: "dv-slow",
            payload_rate_bps: 122,
            keyup_ms: 600,
            tail_ms: 670,
            max_burst_bytes: 2_400,
            max_burst_ms: 25_000,
            holdoff_ms: 1_000,
            jitter_ms: 1_000,
            rx_quiet_ms: 300,
            min_gap_ms: 0,
            voice_gap_ms: 0,
            repeater_tail_ms: 0,
            duty_permille: 0,
            duty_window_ms: 600_000,
            reserve_permille: 0,
        }
    }

    /// DV Fast Data. UNVALIDATED placeholders.
    pub fn dv_fast_placeholder() -> Self {
        ModeProfile {
            name: "dv-fast",
            payload_rate_bps: 435,
            max_burst_bytes: 8_800,
            ..Self::dv_slow_placeholder()
        }
    }

    /// Apply a path preset. `senses_voice`: a source on this port can
    /// tell voice from other signals (CI-V DV RX status). Without one,
    /// repeater paths use the "unsensed" limits: shorter overs and longer
    /// gaps, so a voice user can always break in within one short over.
    pub fn for_path(self, preset: PathPreset, senses_voice: bool) -> Self {
        let p = match preset {
            PathPreset::Direct => return self,
            PathPreset::Repeater => ModeProfile {
                holdoff_ms: 1_500,
                jitter_ms: 1_500,
                repeater_tail_ms: 1_500,
                voice_gap_ms: 10_000,
                min_gap_ms: 2_500,
                max_burst_ms: 12_000,
                duty_permille: 250,
                duty_window_ms: 600_000,
                reserve_permille: 20,
                ..self
            },
            PathPreset::Reflector => ModeProfile {
                holdoff_ms: 2_000,
                jitter_ms: 2_000,
                repeater_tail_ms: 1_500,
                voice_gap_ms: 15_000,
                min_gap_ms: 4_000,
                max_burst_ms: 8_000,
                duty_permille: 100,
                duty_window_ms: 600_000,
                reserve_permille: 20,
                ..self
            },
        };
        if senses_voice {
            p
        } else {
            ModeProfile {
                max_burst_ms: p.max_burst_ms.min(8_000),
                min_gap_ms: p.min_gap_ms.max(5_000),
                jitter_ms: p.jitter_ms.max(3_000),
                ..p
            }
        }
    }

    /// Estimated time on air for `bytes` (scheduling only; never evidence).
    pub fn air_ms(&self, bytes: usize) -> Millis {
        self.keyup_ms + (bytes as u64 * 1000) / self.payload_rate_bps.max(1) as u64 + self.tail_ms
    }

    fn budget_ms(&self) -> Millis {
        self.duty_window_ms * self.duty_permille as u64 / 1000
    }

    fn reserve_ms(&self) -> Millis {
        self.duty_window_ms * self.reserve_permille as u64 / 1000
    }
}

/// Traffic classes, highest priority first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    Control = 0,
    Chat = 1,
    Form = 2,
    File = 3,
}

/// One already-encoded wire frame (never split).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub id: u64,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Burst {
    pub items: Vec<Item>,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Idle,
    HoldOff,
    Writing,
    AirDrain,
    Listen,
    Receiving,
    /// Something holds the port or the channel (see [`Wait`]).
    Held,
    Down,
}

/// Why a port with queued traffic is not transmitting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wait {
    /// The operator paused sending (until a time, or until resumed).
    Paused,
    /// The route guard holds the port (radio set for another scope).
    Route,
    /// A channel-sense source reports the channel busy.
    Busy(Busy),
    /// Data is being received.
    Receiving,
    /// Hold-off after receive activity (and the repeater's tail).
    HoldOff,
    /// Waiting out the gap after voice.
    VoiceGap,
    /// Break-in gap after our own over.
    Gap,
    /// Air-time budget used up.
    Courtesy,
}

impl Wait {
    pub fn as_str(self) -> &'static str {
        match self {
            Wait::Paused => "paused",
            Wait::Route => "route",
            Wait::Busy(Busy::Voice) => "voice",
            Wait::Busy(Busy::OtherMode) => "other_mode",
            Wait::Busy(Busy::Signal) => "signal",
            Wait::Receiving => "receiving",
            Wait::HoldOff => "holdoff",
            Wait::VoiceGap => "voice_gap",
            Wait::Gap => "gap",
            Wait::Courtesy => "courtesy",
        }
    }

    /// Someone else (or a deliberate pause) holds the channel for longer
    /// than a normal turnaround: ARQ timers should not run (docs/10 §4.5).
    pub fn holds_timers(self) -> bool {
        !matches!(self, Wait::Receiving | Wait::HoldOff | Wait::Gap)
    }
}

/// What the scheduler is doing, for display and ARQ pausing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Idle,
    Sending,
    Waiting { why: Wait, until: Option<Millis> },
    Down,
}

#[derive(Debug)]
pub struct Scheduler {
    profile: ModeProfile,
    state: State,
    queues: [VecDeque<Item>; 4],
    deadline: Option<Millis>,
    last_rx: Option<Millis>,
    in_flight: Vec<Item>,
    rng: u64,
    /// Current busy report from channel sense, and since when.
    busy: Option<Busy>,
    /// The last external busy period that ended: its kind and end time.
    cleared: Option<(Busy, Millis)>,
    /// When sensed voice last ended. Kept apart from `cleared`: the
    /// repeater's acknowledgement right after a voice over must not cut
    /// the voice gap short.
    voice_end: Option<Millis>,
    /// When the current sensed voice period began.
    voice_start: Option<Millis>,
    /// When a complete data frame last arrived.
    last_frame: Option<Millis>,
    /// Last time voice was inferred from relayed slow data.
    voice_inferred: Option<Millis>,
    /// Whether some source senses the repeater/channel directly (then
    /// the repeater tail is observed, not assumed).
    sensed: bool,
    own_end: Option<Millis>,
    held_route: bool,
    paused: bool,
    paused_until: Option<Millis>,
    /// Jittered start time chosen for the current wait, with the base it
    /// was chosen for (re-drawn when the base moves).
    jitter: Option<(Millis, Millis)>,
    /// Our overs inside the budget window: (start, estimated air time).
    history: VecDeque<(Millis, Millis)>,
}

/// How long before the radio's voice report a frame may complete and still
/// mark the period as data: covers the 250 ms CI-V status poll. Kept short
/// so a voice user keying up right after someone's data still gets the gap.
const FRAME_BEFORE_VOICE_MS: Millis = 300;
/// How long after the radio's clear report a frame may complete.
const FRAME_AFTER_VOICE_MS: Millis = 1_500;

impl Scheduler {
    pub fn new(profile: ModeProfile, seed: u64) -> Self {
        Scheduler {
            profile,
            state: State::Idle,
            queues: Default::default(),
            deadline: None,
            last_rx: None,
            in_flight: Vec::new(),
            rng: seed | 1,
            busy: None,
            cleared: None,
            voice_end: None,
            voice_start: None,
            last_frame: None,
            voice_inferred: None,
            sensed: false,
            own_end: None,
            held_route: false,
            paused: false,
            paused_until: None,
            jitter: None,
            history: VecDeque::new(),
        }
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn profile(&self) -> &ModeProfile {
        &self.profile
    }

    /// Replace the timing profile (path or sensing changed). Queued and
    /// in-flight traffic is kept.
    pub fn set_profile(&mut self, profile: ModeProfile) {
        self.profile = profile;
        self.jitter = None;
    }

    pub fn queued(&self) -> usize {
        self.queues.iter().map(VecDeque::len).sum()
    }

    pub fn next_deadline(&self) -> Option<Millis> {
        self.deadline
    }

    pub fn enqueue(&mut self, p: Priority, item: Item) {
        self.queues[p as usize].push_back(item);
    }

    fn draw_jitter(&mut self) -> Millis {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        if self.profile.jitter_ms == 0 {
            0
        } else {
            self.rng % (self.profile.jitter_ms + 1)
        }
    }

    /// Bytes arrived from the radio (decoded data, so the channel is busy).
    pub fn on_rx(&mut self, now: Millis) {
        self.last_rx = Some(now);
        if !matches!(self.state, State::Writing | State::AirDrain | State::Down) {
            self.state = State::Receiving;
            self.deadline = Some(now + self.profile.rx_quiet_ms);
        }
    }

    /// Combined channel-sense report changed (`None` = clear). A report
    /// that arrives during our own over is kept and acted on once the
    /// over ends.
    pub fn on_channel(&mut self, now: Millis, busy: Option<Busy>) {
        if busy == Some(Busy::Voice) && self.busy != Some(Busy::Voice) {
            self.voice_start = Some(now);
        }
        if let (Some(was), None) = (self.busy, busy) {
            self.cleared = Some((was, now));
        }
        if self.busy == Some(Busy::Voice) && busy != Some(Busy::Voice) {
            if self.frames_since(self.voice_start.unwrap_or(now)) {
                // Another station's data: Icom radios report it as voice
                // (bench Test 5). No voice gap after it.
                if let Some((Busy::Voice, at)) = self.cleared {
                    self.cleared = Some((Busy::Signal, at));
                }
            } else {
                self.voice_end = Some(now);
            }
        }
        self.busy = busy;
        if self.state == State::Idle || self.state == State::HoldOff || self.state == State::Held {
            self.deadline = Some(now);
        }
    }

    /// A complete data frame arrived. If the radio called the transmission
    /// voice and it ended moments ago, it was data after all: drop the
    /// voice gap (the frame's end can follow the radio's clear report).
    pub fn on_frame(&mut self, now: Millis) {
        self.last_frame = Some(now);
        if let Some(end) = self.voice_end {
            if now <= end + FRAME_AFTER_VOICE_MS {
                self.voice_end = None;
                if self.cleared == Some((Busy::Voice, end)) {
                    self.cleared = Some((Busy::Signal, end));
                }
                if matches!(self.state, State::Idle | State::HoldOff | State::Held) {
                    self.deadline = Some(now);
                }
            }
        }
    }

    /// A frame arrived since shortly before `since` (data bytes can reach
    /// the PC before the radio's status report says busy).
    fn frames_since(&self, since: Millis) -> bool {
        self.last_frame
            .is_some_and(|f| f + FRAME_BEFORE_VOICE_MS >= since)
    }

    /// Whether a source senses the channel directly (CI-V status/squelch).
    pub fn set_sensed(&mut self, sensed: bool) {
        self.sensed = sensed;
    }

    /// Relayed slow data from a voice transmission was seen (docs/10 §4.11).
    pub fn on_voice_inferred(&mut self, now: Millis) {
        self.voice_inferred = Some(now);
        if !matches!(self.state, State::Writing | State::AirDrain | State::Down) {
            self.deadline = Some(now);
        }
    }

    /// Route guard: hold or release the port.
    pub fn set_route_held(&mut self, held: bool, now: Millis) {
        self.held_route = held;
        self.deadline = Some(now);
    }

    /// Operator pause: indefinitely (`until = None`) or until a time.
    pub fn pause(&mut self, until: Option<Millis>, now: Millis) {
        self.paused = until.is_none();
        self.paused_until = until;
        self.deadline = Some(now);
    }

    pub fn resume(&mut self, now: Millis) {
        self.paused = false;
        self.paused_until = None;
        self.deadline = Some(now);
    }

    /// The port accepted all bytes of the in-flight burst.
    pub fn on_port_accepted(&mut self, now: Millis) {
        if self.state == State::Writing {
            let bytes: usize = self.in_flight.iter().map(|i| i.bytes.len()).sum();
            self.state = State::AirDrain;
            self.deadline = Some(now + self.profile.air_ms(bytes));
        }
    }

    /// The link went down: in-flight and queued items are returned as
    /// *not transmitted* so higher layers can keep their state honest.
    pub fn on_link_down(&mut self) -> Vec<Item> {
        self.state = State::Down;
        self.deadline = None;
        let mut failed = std::mem::take(&mut self.in_flight);
        for q in &mut self.queues {
            failed.extend(q.drain(..));
        }
        failed
    }

    pub fn on_link_up(&mut self) {
        if self.state == State::Down {
            self.state = State::Idle;
        }
    }

    /// Air time of our overs that started within the budget window.
    pub fn air_used(&self, now: Millis) -> Millis {
        let from = now.saturating_sub(self.profile.duty_window_ms);
        self.history
            .iter()
            .filter(|(s, _)| *s > from || now < self.profile.duty_window_ms)
            .map(|(_, a)| a)
            .sum()
    }

    /// Air-time budget for the window (0 = unlimited).
    pub fn air_budget(&self) -> Millis {
        self.profile.budget_ms()
    }

    /// Earliest time at which `need` more air time fits under `limit`.
    fn budget_free_at(&self, now: Millis, need: Millis, limit: Millis) -> Option<Millis> {
        let window = self.profile.duty_window_ms;
        let mut used = self.air_used(now);
        if used + need <= limit || used == 0 {
            return None;
        }
        for (s, a) in &self.history {
            used = used.saturating_sub(*a);
            if used + need <= limit {
                return Some(s + window);
            }
        }
        self.history.back().map(|(s, _)| s + window)
    }

    /// Why queued traffic can't go yet, and until when (`None` = until an
    /// event). `Ok(allow_bulk)` = an over may start now.
    fn gate(&mut self, now: Millis) -> Result<bool, (Wait, Option<Millis>)> {
        let p = self.profile.clone();
        if let Some(rx) = self.last_rx {
            if now < rx + p.rx_quiet_ms {
                return Err((Wait::Receiving, Some(rx + p.rx_quiet_ms)));
            }
        }
        if self.paused {
            return Err((Wait::Paused, None));
        }
        if let Some(t) = self.paused_until {
            if now < t {
                return Err((Wait::Paused, Some(t)));
            }
            self.paused_until = None;
        }
        if self.held_route {
            return Err((Wait::Route, None));
        }
        if let Some(b) = self.busy {
            return Err((Wait::Busy(b), None));
        }
        // Waits caused by other stations, each with its reason.
        let mut waits: Vec<(Millis, Wait)> = Vec::new();
        if let Some(rx) = self.last_rx {
            let tail = if self.sensed { 0 } else { p.repeater_tail_ms };
            waits.push((rx + p.holdoff_ms.max(p.rx_quiet_ms) + tail, Wait::HoldOff));
        }
        if let Some((kind, at)) = self.cleared {
            let gap = if kind == Busy::Voice {
                p.voice_gap_ms.max(p.holdoff_ms)
            } else {
                p.holdoff_ms
            };
            let why = if kind == Busy::Voice && p.voice_gap_ms > p.holdoff_ms {
                Wait::VoiceGap
            } else {
                Wait::HoldOff
            };
            waits.push((at + gap, why));
        }
        for at in [self.voice_inferred, self.voice_end].into_iter().flatten() {
            if p.voice_gap_ms > p.holdoff_ms {
                waits.push((at + p.voice_gap_ms, Wait::VoiceGap));
            }
        }
        let others = waits.iter().copied().max_by_key(|(t, _)| *t);
        match others {
            Some((base, why)) if now < base || self.jitter.is_some_and(|(b, _)| b == base) => {
                let target = match self.jitter {
                    Some((b, t)) if b == base => t,
                    _ => {
                        let t = base + self.draw_jitter();
                        self.jitter = Some((base, t));
                        t
                    }
                };
                if now < target {
                    return Err((why, Some(target)));
                }
            }
            _ => {}
        }
        if let Some(end) = self.own_end {
            let gap = p.min_gap_ms.max(p.rx_quiet_ms);
            if now < end + gap {
                return Err((Wait::Gap, Some(end + gap)));
            }
        }
        // Air-time budget: bulk must fit under the budget; Control and
        // Chat may use the reserve above it.
        if p.duty_permille > 0 && self.queued() > 0 {
            let budget = p.budget_ms();
            let keyed = p.keyup_ms + p.tail_ms;
            let urgent_queued = !self.queues[0].is_empty() || !self.queues[1].is_empty();
            if urgent_queued {
                if let Some(t) = self.budget_free_at(now, keyed, budget + p.reserve_ms()) {
                    return Err((Wait::Courtesy, Some(t)));
                }
                return Ok(self.budget_free_at(now, keyed, budget).is_none());
            }
            let next = self.queues[2]
                .front()
                .or(self.queues[3].front())
                .map(|i| i.bytes.len())
                .unwrap_or(0);
            let need = p.air_ms(next).min(p.max_burst_ms);
            if let Some(t) = self.budget_free_at(now, need, budget) {
                return Err((Wait::Courtesy, Some(t)));
            }
        }
        Ok(true)
    }

    /// What the scheduler is doing now.
    pub fn status(&mut self, now: Millis) -> Status {
        match self.state {
            State::Down => Status::Down,
            State::Writing | State::AirDrain => Status::Sending,
            _ if self.queued() == 0 => {
                // Report holds and busy even with nothing queued: ARQ
                // timers waiting for a peer should pause too.
                match self.gate(now) {
                    Err((why, until)) if why.holds_timers() => Status::Waiting { why, until },
                    _ => Status::Idle,
                }
            }
            _ => match self.gate(now) {
                Ok(_) => Status::Idle,
                Err((why, until)) => Status::Waiting { why, until },
            },
        }
    }

    /// Advance timers and, if allowed, return the next burst to write.
    pub fn poll(&mut self, now: Millis) -> Option<Burst> {
        match self.state {
            State::Down | State::Writing => return None,
            State::AirDrain => {
                if self.deadline.is_some_and(|d| now >= d) {
                    self.in_flight.clear();
                    self.own_end = Some(now);
                    self.state = State::Listen;
                } else {
                    return None;
                }
            }
            _ => {}
        }
        if self.queued() == 0 {
            let quiet = self
                .last_rx
                .map_or(true, |rx| now >= rx + self.profile.rx_quiet_ms);
            if quiet {
                self.state = State::Idle;
                self.deadline = None;
            } else {
                self.state = State::Receiving;
                self.deadline = self.last_rx.map(|rx| rx + self.profile.rx_quiet_ms);
            }
            return None;
        }
        match self.gate(now) {
            Ok(allow_bulk) => {
                self.jitter = None;
                self.assemble(now, allow_bulk)
            }
            Err((why, until)) => {
                self.state = match why {
                    Wait::Receiving => State::Receiving,
                    Wait::Gap => State::Listen,
                    Wait::HoldOff | Wait::VoiceGap => State::HoldOff,
                    _ => State::Held,
                };
                self.deadline = until;
                None
            }
        }
    }

    fn assemble(&mut self, now: Millis, allow_bulk: bool) -> Option<Burst> {
        let mut items = Vec::new();
        let mut bytes = 0usize;
        let classes = if allow_bulk { 4 } else { 2 };
        'fill: for q in self.queues.iter_mut().take(classes) {
            while let Some(front) = q.front() {
                let len = front.bytes.len();
                let over_bytes = bytes + len > self.profile.max_burst_bytes;
                let over_time = self.profile.air_ms(bytes + len) > self.profile.max_burst_ms;
                if !items.is_empty() && (over_bytes || over_time) {
                    break 'fill;
                }
                bytes += len;
                items.push(q.pop_front().expect("front exists"));
            }
        }
        if items.is_empty() {
            self.state = State::Held;
            self.deadline = None;
            return None;
        }
        let window = self.profile.duty_window_ms;
        while self
            .history
            .front()
            .is_some_and(|(s, _)| now >= window && *s <= now - window)
        {
            self.history.pop_front();
        }
        self.history.push_back((now, self.profile.air_ms(bytes)));
        let wire: Vec<u8> = items.iter().flat_map(|i| i.bytes.iter().copied()).collect();
        self.in_flight = items.clone();
        self.state = State::Writing;
        self.deadline = None;
        Some(Burst { items, bytes: wire })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: u64, n: usize) -> Item {
        Item {
            id,
            bytes: vec![b'x'; n],
        }
    }

    fn profile() -> ModeProfile {
        ModeProfile {
            jitter_ms: 0,
            ..ModeProfile::dv_slow_placeholder()
        }
    }

    #[test]
    fn one_burst_in_the_radio_at_a_time_i2_1() {
        let mut s = Scheduler::new(profile(), 1);
        s.enqueue(Priority::File, item(1, 500));
        s.enqueue(Priority::File, item(2, 500));
        let b = s.poll(0).expect("first burst");
        assert_eq!(b.items.len(), 2);
        s.enqueue(Priority::Chat, item(3, 40));
        assert!(s.poll(1).is_none(), "must not write while the port is busy");
        s.on_port_accepted(100);
        let air = s.profile().air_ms(1000);
        assert!(
            s.poll(100 + air - 1).is_none(),
            "must not write during estimated air time"
        );
        assert!(s.poll(100 + air).is_none(), "listen window after our burst");
        let b2 = s.poll(100 + air + 300).expect("chat after listen window");
        assert_eq!(b2.items[0].id, 3);
    }

    #[test]
    fn priority_order_and_frames_never_split() {
        let mut s = Scheduler::new(
            ModeProfile {
                max_burst_bytes: 1000,
                ..profile()
            },
            1,
        );
        s.enqueue(Priority::File, item(1, 600));
        s.enqueue(Priority::File, item(2, 600));
        s.enqueue(Priority::Chat, item(3, 50));
        s.enqueue(Priority::Control, item(4, 30));
        let b = s.poll(0).unwrap();
        let ids: Vec<u64> = b.items.iter().map(|i| i.id).collect();
        assert_eq!(
            ids,
            vec![4, 3, 1],
            "control, chat, then as much bulk as fits"
        );
        assert_eq!(b.bytes.len(), 680);
    }

    #[test]
    fn oversized_frame_goes_alone() {
        let mut s = Scheduler::new(
            ModeProfile {
                max_burst_bytes: 100,
                ..profile()
            },
            1,
        );
        s.enqueue(Priority::File, item(1, 400));
        s.enqueue(Priority::File, item(2, 10));
        let b = s.poll(0).unwrap();
        assert_eq!(b.items.len(), 1);
        assert_eq!(b.items[0].id, 1);
    }

    #[test]
    fn holdoff_after_receive_activity_n2_1() {
        let mut s = Scheduler::new(
            ModeProfile {
                jitter_ms: 500,
                ..profile()
            },
            7,
        );
        s.on_rx(1_000);
        s.enqueue(Priority::Chat, item(1, 40));
        assert!(s.poll(1_100).is_none(), "receiving");
        assert!(
            s.poll(1_300).is_none(),
            "rx quiet reached, but hold-off not over"
        );
        assert_eq!(s.state(), State::HoldOff);
        let d = s.next_deadline().unwrap();
        assert!(
            (2_000..=2_500).contains(&d),
            "hold-off 1 s after last RX + jitter ≤ 0.5 s, got {d}"
        );
        assert!(s.poll(d - 1).is_none());
        assert!(s.poll(d).is_some());
    }

    #[test]
    fn no_blind_random_delay_on_a_quiet_channel_n2_1() {
        let mut s = Scheduler::new(
            ModeProfile {
                jitter_ms: 2_000,
                ..profile()
            },
            3,
        );
        s.enqueue(Priority::Chat, item(1, 40));
        assert!(s.poll(0).is_some(), "quiet channel: transmit immediately");
    }

    #[test]
    fn link_down_returns_everything_as_not_transmitted() {
        let mut s = Scheduler::new(profile(), 1);
        s.enqueue(Priority::File, item(1, 100));
        s.enqueue(Priority::Chat, item(2, 10));
        let _ = s.poll(0).unwrap(); // both in flight
        s.enqueue(Priority::Form, item(3, 10));
        let failed = s.on_link_down();
        let mut ids: Vec<u64> = failed.iter().map(|i| i.id).collect();
        ids.sort();
        assert_eq!(ids, vec![1, 2, 3]);
        assert!(s.poll(10).is_none());
        s.on_link_up();
        assert_eq!(s.state(), State::Idle);
    }

    #[test]
    fn burst_time_limit_respected() {
        // 25 s cap at 119 B/s ≈ 2.8 kB including key-up and tail.
        let mut s = Scheduler::new(
            ModeProfile {
                max_burst_bytes: 100_000,
                ..profile()
            },
            1,
        );
        for i in 0..20 {
            s.enqueue(Priority::File, item(i, 500));
        }
        let b = s.poll(0).unwrap();
        assert!(s.profile().air_ms(b.bytes.len()) <= 25_000);
        assert!(b.items.len() >= 4);
    }

    fn repeater(sensed: bool) -> ModeProfile {
        ModeProfile {
            jitter_ms: 0,
            ..ModeProfile::dv_slow_placeholder().for_path(PathPreset::Repeater, sensed)
        }
    }

    /// Drive a burst through write and air time; returns when the air
    /// estimate ended.
    fn send_out(s: &mut Scheduler, now: Millis) -> Millis {
        let b = s.poll(now).expect("burst");
        s.on_port_accepted(now);
        let end = now + s.profile().air_ms(b.bytes.len());
        assert!(s.poll(end).is_none() || s.queued() > 0);
        end
    }

    #[test]
    fn voice_gap_after_sensed_voice() {
        let mut s = Scheduler::new(repeater(true), 1);
        s.set_sensed(true);
        s.on_channel(1_000, Some(Busy::Voice));
        s.enqueue(Priority::Chat, item(1, 40));
        assert!(s.poll(1_500).is_none());
        assert_eq!(
            s.status(1_500),
            Status::Waiting {
                why: Wait::Busy(Busy::Voice),
                until: None
            }
        );
        s.on_channel(20_000, None);
        assert!(s.poll(20_001).is_none(), "voice gap");
        assert_eq!(s.next_deadline(), Some(30_000));
        assert!(matches!(
            s.status(25_000),
            Status::Waiting {
                why: Wait::VoiceGap,
                ..
            }
        ));
        assert!(s.poll(29_999).is_none());
        assert!(s.poll(30_000).is_some());
    }

    #[test]
    fn repeater_ack_after_voice_does_not_cut_the_voice_gap() {
        let mut s = Scheduler::new(repeater(true), 1);
        s.set_sensed(true);
        s.on_channel(0, Some(Busy::Voice));
        s.on_channel(10_000, None);
        // MMDVMHost acknowledges the voice over 750 ms later.
        s.on_channel(10_750, Some(Busy::Signal));
        s.on_channel(11_250, None);
        s.enqueue(Priority::Chat, item(1, 40));
        assert!(s.poll(19_999).is_none(), "still inside the voice gap");
        assert!(s.poll(20_000).is_some());
        // Voice going straight to another signal also ends the voice.
        let mut s = Scheduler::new(repeater(true), 1);
        s.on_channel(0, Some(Busy::Voice));
        s.on_channel(5_000, Some(Busy::Signal));
        s.on_channel(5_500, None);
        s.enqueue(Priority::Chat, item(1, 40));
        assert!(s.poll(14_999).is_none());
        assert!(s.poll(15_000).is_some());
    }

    #[test]
    fn data_reported_as_voice_gets_no_voice_gap() {
        // Icom radios report another station's data as voice (bench Test 5).
        let mut s = Scheduler::new(repeater(true), 1);
        s.set_sensed(true);
        s.on_channel(0, Some(Busy::Voice));
        s.on_frame(1_200);
        s.on_channel(1_500, None);
        s.enqueue(Priority::Chat, item(1, 40));
        assert!(s.poll(2_999).is_none(), "holdoff still applies");
        assert!(s.poll(3_000).is_some(), "no 10 s voice gap after data");
        // The frame can complete just after the radio's clear report.
        let mut s = Scheduler::new(repeater(true), 1);
        s.set_sensed(true);
        s.on_channel(0, Some(Busy::Voice));
        s.on_channel(1_000, None);
        s.enqueue(Priority::Chat, item(1, 40));
        assert!(s.poll(1_200).is_none());
        s.on_frame(1_400);
        assert!(s.poll(2_499).is_none());
        assert!(s.poll(2_500).is_some());
    }

    #[test]
    fn voice_right_after_data_keeps_the_voice_gap() {
        let mut s = Scheduler::new(repeater(true), 1);
        s.set_sensed(true);
        s.on_frame(0); // the end of someone's data
        s.on_channel(1_000, Some(Busy::Voice)); // a voice user keys up
        s.on_channel(6_000, None);
        s.on_frame(9_000); // later data doesn't rewrite that voice
        s.enqueue(Priority::Chat, item(1, 40));
        assert!(s.poll(15_999).is_none(), "voice gap");
        assert!(s.poll(16_000).is_some());
    }

    #[test]
    fn signal_clears_with_holdoff_only() {
        let mut s = Scheduler::new(repeater(true), 1);
        s.set_sensed(true);
        s.on_channel(0, Some(Busy::Signal));
        s.on_channel(1_000, None);
        s.enqueue(Priority::Chat, item(1, 40));
        assert!(s.poll(2_499).is_none());
        assert!(s.poll(2_500).is_some());
    }

    #[test]
    fn route_hold_and_operator_pause_keep_the_queue() {
        let mut s = Scheduler::new(profile(), 1);
        s.enqueue(Priority::Chat, item(1, 40));
        s.set_route_held(true, 0);
        assert!(s.poll(0).is_none());
        assert_eq!(
            s.status(0),
            Status::Waiting {
                why: Wait::Route,
                until: None
            }
        );
        s.set_route_held(false, 10);
        s.pause(None, 10);
        assert!(s.poll(20).is_none());
        s.resume(30);
        s.pause(Some(5_000), 30);
        assert!(s.poll(4_999).is_none());
        assert_eq!(s.queued(), 1);
        assert_eq!(s.poll(5_000).unwrap().items[0].id, 1);
    }

    #[test]
    fn break_in_gap_between_our_overs() {
        let mut s = Scheduler::new(repeater(true), 1);
        for i in 0..10 {
            s.enqueue(Priority::File, item(i, 500));
        }
        let end = send_out(&mut s, 0);
        assert!(s.poll(end + 2_499).is_none(), "min gap");
        assert!(s.poll(end + 2_500).is_some());
    }

    #[test]
    fn unsensed_repeater_assumes_tail_and_shorter_overs() {
        let p = repeater(false);
        assert_eq!((p.max_burst_ms, p.min_gap_ms), (8_000, 5_000));
        let mut s = Scheduler::new(p, 1);
        s.on_rx(1_000);
        s.enqueue(Priority::Chat, item(1, 40));
        // 1.5 s hold-off + 1.5 s assumed repeater tail.
        assert!(s.poll(3_999).is_none());
        assert!(s.poll(4_000).is_some());
        let mut sensed = Scheduler::new(repeater(true), 1);
        sensed.set_sensed(true);
        sensed.on_rx(1_000);
        sensed.enqueue(Priority::Chat, item(1, 40));
        assert!(sensed.poll(2_500).is_some(), "tail is sensed, not assumed");
    }

    #[test]
    fn inferred_voice_starts_the_voice_gap() {
        let mut s = Scheduler::new(repeater(false), 1);
        s.enqueue(Priority::Chat, item(1, 40));
        s.on_voice_inferred(100);
        assert!(s.poll(10_099).is_none());
        assert!(s.poll(10_100).is_some());
    }

    #[test]
    fn budget_throttles_bulk_but_not_chat() {
        let p = ModeProfile {
            duty_permille: 100,
            duty_window_ms: 100_000, // 10 s budget, 2 s reserve
            reserve_permille: 20,
            min_gap_ms: 0,
            ..profile()
        };
        let mut s = Scheduler::new(p, 1);
        s.enqueue(Priority::File, item(1, 1_000)); // ≈ 9.5 s on air
        let end = send_out(&mut s, 0);
        s.enqueue(Priority::File, item(2, 1_000));
        assert!(s.poll(end + 300).is_none());
        assert!(matches!(
            s.status(end + 300),
            Status::Waiting {
                why: Wait::Courtesy,
                until: Some(100_000)
            }
        ));
        s.enqueue(Priority::Chat, item(3, 20));
        let b = s.poll(end + 301).expect("chat uses the reserve");
        assert_eq!(
            b.items.iter().map(|i| i.id).collect::<Vec<_>>(),
            vec![3],
            "bulk stays queued"
        );
        s.on_port_accepted(end + 301);
        let end2 = end + 301 + s.profile().air_ms(20);
        assert!(s.poll(end2 + 300).is_none());
        // The file needs 9.5 s: it fits once the chat over leaves the window.
        assert!(s.poll(100_000).is_none());
        assert!(
            s.poll(end + 301 + 100_000).is_some(),
            "budget frees as the window moves"
        );
    }

    #[test]
    fn invariants_under_random_events() {
        // Deterministic pseudo-random event sequences.
        let mut rng = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move |n: u64| {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng % n
        };
        for seed in 0..200 {
            let sensed = seed % 2 == 0;
            let p = ModeProfile::dv_slow_placeholder().for_path(PathPreset::Repeater, sensed);
            let (voice_gap, budget, reserve, max_burst) = (
                p.voice_gap_ms,
                p.budget_ms(),
                p.reserve_ms(),
                p.max_burst_ms,
            );
            let mut s = Scheduler::new(p, seed + 1);
            s.set_sensed(sensed);
            let (mut busy, mut cleared, mut held) = (None::<Busy>, None::<(Busy, Millis)>, false);
            let mut overs: Vec<(Millis, Millis)> = Vec::new();
            let mut now = 0;
            let mut id = 0;
            while now < 3_600_000 {
                now += 50 + next(3_000);
                match next(10) {
                    0 => match busy {
                        Some(kind) => {
                            busy = None;
                            cleared = Some((kind, now));
                            s.on_channel(now, None);
                        }
                        None => {
                            let kind = if next(2) == 0 {
                                Busy::Voice
                            } else {
                                Busy::Signal
                            };
                            busy = Some(kind);
                            s.on_channel(now, Some(kind));
                        }
                    },
                    1 => {
                        held = !held;
                        s.set_route_held(held, now);
                    }
                    2 | 3 => {
                        id += 1;
                        let prio =
                            [Priority::Control, Priority::Chat, Priority::File][next(3) as usize];
                        s.enqueue(prio, item(id, 20 + next(600) as usize));
                    }
                    _ => {}
                }
                let writing = matches!(s.state(), State::Writing);
                if let Some(b) = s.poll(now) {
                    assert!(!writing);
                    assert!(busy.is_none(), "seed {seed}: over started while busy");
                    assert!(!held, "seed {seed}: over started while held");
                    match cleared {
                        Some((Busy::Voice, t)) => {
                            assert!(
                                now >= t + voice_gap,
                                "seed {seed}: over inside the voice gap"
                            )
                        }
                        Some((_, t)) => {
                            assert!(now >= t + 1_500, "seed {seed}: over inside hold-off")
                        }
                        None => {}
                    }
                    let air = s.profile().air_ms(b.bytes.len());
                    assert!(air <= max_burst || b.items.len() == 1);
                    s.on_port_accepted(now);
                    overs.push((now, air));
                    let used: Millis = overs
                        .iter()
                        .filter(|(t, _)| *t + 600_000 > now)
                        .map(|(_, a)| a)
                        .sum();
                    assert!(
                        used <= budget + reserve + max_burst,
                        "seed {seed}: budget {used}"
                    );
                }
            }
        }
    }
}
