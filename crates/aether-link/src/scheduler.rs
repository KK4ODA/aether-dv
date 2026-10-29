//! SM-2: per-port half-duplex channel scheduler (sans-I/O).
//! Spec: `docs/phase-b/state-machines.md` SM-2.
//!
//! The scheduler is the only component that decides *when* bytes go to a
//! radio. It never reports delivery: `PortAccepted` only starts the air-time
//! estimate (N2.2).

use std::collections::VecDeque;

use crate::legacy_arq::Millis;

/// Timing and size limits for one transport mode. Every default below is a
/// placeholder until hardware test HW-4 measures the real radio.
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
}

impl ModeProfile {
    /// DV slow data. UNVALIDATED placeholders (see docs/07 §4.1 assumptions).
    pub fn dv_slow_placeholder() -> Self {
        ModeProfile {
            name: "dv-slow",
            payload_rate_bps: 119,
            keyup_ms: 600,
            tail_ms: 500,
            max_burst_bytes: 2_400,
            max_burst_ms: 25_000,
            holdoff_ms: 1_000,
            jitter_ms: 1_000,
            rx_quiet_ms: 300,
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

    /// Estimated time on air for `bytes` (scheduling only; never evidence).
    pub fn air_ms(&self, bytes: usize) -> Millis {
        self.keyup_ms + (bytes as u64 * 1000) / self.payload_rate_bps.max(1) as u64 + self.tail_ms
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
}

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
        }
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn profile(&self) -> &ModeProfile {
        &self.profile
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

    fn jitter(&mut self) -> Millis {
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
        match self.state {
            State::Idle | State::HoldOff | State::Listen | State::Receiving => {
                self.state = State::Receiving;
                self.deadline = Some(now + self.profile.rx_quiet_ms);
            }
            _ => {} // our own TX period; the radio is half-duplex
        }
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

    /// Advance timers and, if allowed, return the next burst to write.
    pub fn poll(&mut self, now: Millis) -> Option<Burst> {
        loop {
            let due = self.deadline.is_some_and(|d| now >= d);
            match self.state {
                State::Down | State::Writing => return None,
                State::AirDrain if due => {
                    self.in_flight.clear();
                    self.state = State::Listen;
                    self.deadline = Some(now + self.profile.rx_quiet_ms);
                }
                State::Listen | State::Receiving if due => {
                    self.state = State::Idle;
                    self.deadline = None;
                }
                State::HoldOff if due => {
                    self.state = State::Idle;
                    self.deadline = None;
                    return self.assemble(now);
                }
                State::Idle => {
                    if self.queued() == 0 {
                        return None;
                    }
                    if let Some(rx) = self.last_rx {
                        let quiet_until = rx + self.profile.holdoff_ms;
                        if now < quiet_until {
                            self.state = State::HoldOff;
                            self.deadline = Some(quiet_until + self.jitter());
                            return None;
                        }
                    }
                    return self.assemble(now);
                }
                _ => return None,
            }
        }
    }

    fn assemble(&mut self, _now: Millis) -> Option<Burst> {
        let mut items = Vec::new();
        let mut bytes = 0usize;
        'fill: for q in &mut self.queues {
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
            return None;
        }
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
}
