//! Deterministic end-to-end simulation of ARQ-1 through the full DDT2 codec.
//! Validation level: Simulated. Channel: half-duplex, one transmission at a
//! time, per-over cost + bytes/rate air time, frame-level loss.

use std::collections::VecDeque;

use aether_ddt2::{wrap, Deframer, EscapeSet, Event, Frame};
use aether_link::legacy_arq::*;

const RATE: u32 = 119; // B/s, DV slow-data ceiling
const PER_OVER: Millis = 1_100; // carrier sense + key-up + tail (assumed)

struct Channel {
    free_at: Millis,
    in_flight: VecDeque<(Millis, bool, Vec<u8>)>, // (arrival, to_receiver, wire bytes)
}

impl Channel {
    fn transmit(
        &mut self,
        now: Millis,
        to_receiver: bool,
        frames: &[Frame],
        drop: &mut dyn FnMut(&Frame) -> bool,
    ) -> usize {
        if frames.is_empty() {
            return 0;
        }
        let mut wire_total = 0;
        let mut pieces = Vec::new();
        for f in frames {
            let raw = f.encode(ENCODING).unwrap();
            let w = wrap(&raw, &EscapeSet::legacy()).unwrap();
            wire_total += w.len();
            if !drop(f) {
                pieces.push(w);
            }
        }
        let start = now.max(self.free_at);
        let end = start + PER_OVER + wire_total as u64 * 1000 / RATE as u64;
        self.free_at = end;
        for p in pieces {
            self.in_flight.push_back((end, to_receiver, p));
        }
        wire_total
    }
}

struct Outcome {
    delivered: Vec<u8>,
    sender_state: SenderState,
    diags: Vec<RxDiag>,
    elapsed: Millis,
    stats: SenderStats,
    air_bytes: usize,
}

fn run(
    data: &[u8],
    cfg: SenderConfig,
    mut drop: impl FnMut(&Frame, bool) -> bool,
    limit: Millis,
) -> Outcome {
    let a = SessionAddr {
        session: 4,
        src: "KK4ODA".into(),
        dst: "W4DRS".into(),
    };
    let b = SessionAddr {
        session: 7,
        src: "W4DRS".into(),
        dst: "KK4ODA".into(),
    };
    let mut tx = LegacySender::new(cfg, a, data);
    let mut rx = LegacyReceiver::new(b);
    let mut ch = Channel {
        free_at: 0,
        in_flight: VecDeque::new(),
    };
    let (mut dfa, mut dfb) = (Deframer::default(), Deframer::default());
    let mut now: Millis = 0;
    let mut delivered = Vec::new();
    let mut diags = Vec::new();
    let mut air_bytes = 0;
    while now < limit {
        let burst = tx.poll_transmit(now);
        air_bytes += ch.transmit(now, true, &burst, &mut |f| drop(f, true));
        if tx.state() != SenderState::Sending && ch.in_flight.is_empty() {
            break;
        }
        // advance to the next event
        let next_arrival = ch.in_flight.front().map(|x| x.0);
        let next_deadline = tx.next_deadline();
        now = match (next_arrival, next_deadline) {
            (Some(x), Some(y)) => x.min(y).max(now),
            (Some(x), None) => x.max(now),
            (None, Some(y)) => y.max(now),
            (None, None) => break,
        };
        while let Some(&(t, _, _)) = ch.in_flight.front() {
            if t > now {
                break;
            }
            let (_, to_rx, bytes) = ch.in_flight.pop_front().unwrap();
            let mut ev = Vec::new();
            if to_rx {
                dfb.feed(&bytes, &mut ev)
            } else {
                dfa.feed(&bytes, &mut ev)
            }
            for e in ev {
                let Event::Frame(f) = e else {
                    panic!("codec error in sim: {e:?}")
                };
                if to_rx {
                    let mut out = Vec::new();
                    rx.on_frame(&f, &mut out);
                    let mut replies = Vec::new();
                    for o in out {
                        match o {
                            RxEvent::Deliver(d) => delivered.extend(d),
                            RxEvent::Transmit(fr) => replies.push(fr),
                            RxEvent::Diag(d) => diags.push(d),
                        }
                    }
                    air_bytes += ch.transmit(now, false, &replies, &mut |f| drop(f, false));
                } else {
                    tx.on_frame(now, &f);
                }
            }
        }
    }
    Outcome {
        delivered,
        sender_state: tx.state(),
        diags,
        elapsed: now,
        stats: tx.stats.clone(),
        air_bytes,
    }
}

fn data(n: usize, seed: u64) -> Vec<u8> {
    let mut s = seed;
    (0..n)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s as u8
        })
        .collect()
}

#[test]
fn clean_transfer_delivers_everything() {
    let d = data(10_000, 1);
    let o = run(&d, SenderConfig::legacy_defaults(), |_, _| false, 3_600_000);
    assert_eq!(o.sender_state, SenderState::Done);
    assert_eq!(o.delivered, d);
    assert_eq!(o.stats.blocks_retransmitted, 0);
    let goodput = d.len() as f64 / (o.elapsed as f64 / 1000.0);
    println!(
        "clean 10 kB, 512x4: {:.1} s, goodput {goodput:.1} B/s, air bytes {}",
        o.elapsed as f64 / 1000.0,
        o.air_bytes
    );
    assert!(goodput > 60.0 && goodput < RATE as f64);
}

#[test]
fn random_frame_loss_is_recovered() {
    let d = data(8_000, 2);
    let mut s: u64 = 0xDEADBEEF;
    let o = run(
        &d,
        SenderConfig::legacy_defaults(),
        move |_, _| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s % 100 < 10 // 10 % of frames lost, both directions
        },
        6 * 3_600_000,
    );
    assert_eq!(o.sender_state, SenderState::Done, "{:?}", o.stats);
    assert_eq!(o.delivered, d);
    assert!(o.stats.blocks_retransmitted > 0);
    println!(
        "10% loss: {:.1} s, {:?}",
        o.elapsed as f64 / 1000.0,
        o.stats
    );
}

#[test]
fn p19_post_wrap_loss_is_reproduced_and_flagged() {
    // 300 blocks of 64 B: sequence wraps after block 255. Drop only the first
    // transmission of block #256 (seq 0 of the second cycle).
    let cfg = SenderConfig {
        block_size: 64,
        ..SenderConfig::legacy_defaults()
    };
    let d = data(300 * 64, 3);
    let mut dropped = false;
    let o = run(
        &d,
        cfg,
        move |f, to_rx| {
            // #256 is the first DATA frame with seq 0 after the cycle; it is
            // the only seq-0 block sent once the first 256 are acknowledged.
            if to_rx
                && f.kind == 4
                && f.seq == 0
                && !dropped
                && f.payload == data(300 * 64, 3)[256 * 64..257 * 64]
            {
                dropped = true;
                return true;
            }
            false
        },
        6 * 3_600_000,
    );
    // Legacy semantics: the sender believes it is done...
    assert_eq!(o.sender_state, SenderState::Done);
    // ...but the receiver is missing the tail: silent loss at the protocol level.
    assert!(
        o.delivered.len() < d.len(),
        "expected P19 loss, got full delivery"
    );
    assert_eq!(
        o.delivered,
        d[..o.delivered.len()].to_vec(),
        "delivered prefix must be correct"
    );
    // Aether's receiver flags the signature.
    assert!(o
        .diags
        .iter()
        .any(|x| matches!(x, RxDiag::PossibleWrapAmbiguity { .. })));
    println!(
        "P19: delivered {} of {} bytes; diags {:?}",
        o.delivered.len(),
        d.len(),
        &o.diags[..o.diags.len().min(3)]
    );
}

#[test]
fn no_wrap_no_problem_with_same_loss_pattern() {
    // Control: the same single loss in a transfer under 256 blocks is recovered.
    let cfg = SenderConfig {
        block_size: 64,
        ..SenderConfig::legacy_defaults()
    };
    let d = data(200 * 64, 4);
    let mut dropped = false;
    let o = run(
        &d,
        cfg,
        move |f, to_rx| {
            if to_rx && f.kind == 4 && f.seq == 100 && !dropped {
                dropped = true;
                return true;
            }
            false
        },
        6 * 3_600_000,
    );
    assert_eq!(o.sender_state, SenderState::Done);
    assert_eq!(o.delivered, d);
    assert!(o.diags.is_empty());
}

#[test]
fn early_ack_does_not_end_the_wait_p20() {
    let a = SessionAddr {
        session: 4,
        src: "A".into(),
        dst: "B".into(),
    };
    let mut tx = LegacySender::new(SenderConfig::legacy_defaults(), a, &data(2048, 5));
    let burst = tx.poll_transmit(0);
    assert_eq!(burst.len(), 5); // 4 blocks + REQACK
                                // A stale ACK (from an earlier poll) arrives 1 s later: far too early to
                                // answer a ~20 s burst. It acknowledges nothing outstanding.
    let stale = Frame::new(0, 7, aether_ddt2::session::stateful::ACK, "B", "A", vec![9]);
    tx.on_frame(1_000, &stale);
    assert_eq!(tx.stats.early_acks_ignored, 1);
    // Still waiting: no immediate duplicate burst.
    assert!(tx.poll_transmit(1_001).is_empty());
}

#[test]
fn wrap_warning_threshold() {
    let a = SessionAddr {
        session: 4,
        src: "A".into(),
        dst: "B".into(),
    };
    let cfg = SenderConfig::legacy_defaults();
    assert!(!LegacySender::new(cfg.clone(), a.clone(), &vec![0; 256 * 512]).wrap_warning());
    assert!(LegacySender::new(cfg, a, &vec![0; 256 * 512 + 1]).wrap_warning());
}
