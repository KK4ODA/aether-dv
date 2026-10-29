//! `[SOB]` + yEnc(frame) + `[EOB]` envelope and a streaming, bounded deframer.
//!
//! Legacy reference: `d_rats/transport.py` `parse_blocks` and
//! `ddt2.DDT2EncodedFrame`. Differences (receiver-side only, wire-compatible):
//! * when an `[EOB]` was lost, legacy concatenates two frames from the first
//!   `[SOB]` and loses both; we restart at the *last* `[SOB]` before the
//!   `[EOB]`, recovering the second frame;
//! * the buffer is bounded; legacy grows it without limit;
//! * bytes outside frames are surfaced (for NMEA/DPRS parsing and the
//!   optional plain-text monitor) instead of being polled with regexes.

use crate::frame::{Frame, FrameError};
use crate::yenc::{self, EscapeSet, YencError};

pub const SOB: &[u8; 5] = b"[SOB]";
pub const EOB: &[u8; 5] = b"[EOB]";

/// Default cap on bytes buffered after an `[SOB]` without an `[EOB]`.
/// Legacy frames are at most ~1.1 KiB raw (1024 B blocks) before escaping.
pub const DEFAULT_MAX_PENDING: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvelopeError {
    /// The escaped frame contains an envelope marker, so a legacy receiver
    /// would cut it short on every retransmission (legacy defect P2).
    /// Re-chunk or change compression and try again.
    PoisonMarker,
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn rfind(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).rposition(|w| w == needle)
}

/// Wrap raw DDT2 bytes for the wire.
pub fn wrap(raw_frame: &[u8], escapes: &EscapeSet) -> Result<Vec<u8>, EnvelopeError> {
    let body = yenc::encode(raw_frame, escapes);
    if find(&body, SOB).is_some() || find(&body, EOB).is_some() {
        return Err(EnvelopeError::PoisonMarker);
    }
    let mut out = Vec::with_capacity(body.len() + 10);
    out.extend_from_slice(SOB);
    out.extend_from_slice(&body);
    out.extend_from_slice(EOB);
    Ok(out)
}

/// What the deframer found in the byte stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A complete envelope that decoded to a valid frame.
    Frame(Frame),
    /// A complete envelope that failed to decode; raw envelope body included
    /// for diagnostics.
    BadFrame { error: BadFrameError, body: Vec<u8> },
    /// Bytes that were not inside any envelope (radio NMEA/DPRS output,
    /// plain text, noise). Delivered in arrival order.
    Outside(Vec<u8>),
    /// The pending buffer exceeded its bound and was discarded up to here.
    Overflow { discarded: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BadFrameError {
    Yenc(YencError),
    Frame(FrameError),
    /// A frame whose `[EOB]` was lost; superseded by a later `[SOB]`.
    Truncated,
}

/// Incremental deframer. Feed arbitrary chunks; collect events.
#[derive(Debug)]
pub struct Deframer {
    buf: Vec<u8>,
    max_pending: usize,
}

impl Default for Deframer {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_PENDING)
    }
}

impl Deframer {
    pub fn new(max_pending: usize) -> Self {
        Deframer {
            buf: Vec::new(),
            max_pending,
        }
    }

    /// Bytes currently held (for diagnostics and tests).
    pub fn pending(&self) -> usize {
        self.buf.len()
    }

    pub fn feed(&mut self, chunk: &[u8], out: &mut Vec<Event>) {
        self.buf.extend_from_slice(chunk);
        loop {
            let Some(sob) = find(&self.buf, SOB) else {
                // No frame start: everything except a possible partial
                // marker at the tail is outside data.
                let keep = partial_marker_tail(&self.buf);
                let emit = self.buf.len() - keep;
                if emit > 0 {
                    out.push(Event::Outside(self.buf.drain(..emit).collect()));
                }
                return;
            };
            if sob > 0 {
                // Text before the frame start. An orphan [EOB] here (start
                // lost) is excised like legacy does, rather than reported.
                let pre: Vec<u8> = self.buf.drain(..sob).collect();
                let cleaned = strip_markers(&pre);
                if !cleaned.is_empty() {
                    out.push(Event::Outside(cleaned));
                }
                continue;
            }
            // buf starts with [SOB]
            match find(&self.buf[SOB.len()..], EOB) {
                None => {
                    if self.buf.len() > self.max_pending {
                        // Resync at the next [SOB] after the first, if any.
                        let next = find(&self.buf[SOB.len()..], SOB).map(|i| i + SOB.len());
                        let discarded = next.unwrap_or(self.buf.len());
                        self.buf.drain(..discarded);
                        out.push(Event::Overflow { discarded });
                        continue;
                    }
                    return;
                }
                Some(rel) => {
                    let eob = SOB.len() + rel;
                    // Lost-[EOB] recovery: start from the last [SOB] before this [EOB].
                    let start = rfind(&self.buf[..eob], SOB).unwrap_or(0);
                    if start > 0 {
                        let dropped: Vec<u8> = self.buf[..start].to_vec();
                        out.push(Event::BadFrame {
                            error: BadFrameError::Truncated,
                            body: dropped,
                        });
                    }
                    let body = self.buf[start + SOB.len()..eob].to_vec();
                    self.buf.drain(..eob + EOB.len());
                    out.push(decode_body(body));
                }
            }
        }
    }
}

fn decode_body(body: Vec<u8>) -> Event {
    match yenc::decode(&body) {
        Err(e) => Event::BadFrame {
            error: BadFrameError::Yenc(e),
            body,
        },
        Ok(raw) => match Frame::decode(&raw) {
            Ok(f) => Event::Frame(f),
            Err(e) => Event::BadFrame {
                error: BadFrameError::Frame(e),
                body,
            },
        },
    }
}

/// How many trailing bytes could be the start of a `[SOB]` marker.
fn partial_marker_tail(buf: &[u8]) -> usize {
    (1..SOB.len())
        .rev()
        .find(|&k| buf.len() >= k && buf[buf.len() - k..] == SOB[..k])
        .unwrap_or(0)
}

fn strip_markers(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i..].starts_with(EOB) {
            i += EOB.len();
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::PayloadEncoding;

    fn wire(f: &Frame) -> Vec<u8> {
        wrap(
            &f.encode(PayloadEncoding::Never).unwrap(),
            &EscapeSet::legacy(),
        )
        .unwrap()
    }

    #[test]
    fn chat_hello_vector() {
        // Same bytes as docs/tools/model_ddt2.py (independent model).
        let f = Frame::new(0, 1, 0, "N0CALL", "CQCQCQ", b"Hello".to_vec());
        assert_eq!(
            wire(&f),
            b"[SOB]\"=@=@\x01=@ET=@\x05N0CALL~~CQCQCQ~~Hello[EOB]".to_vec()
        );
    }

    #[test]
    fn frames_split_at_every_byte_boundary() {
        let a = Frame::new(1, 1, 0, "W1AW", "CQCQCQ", b"first".to_vec());
        let b = Frame::new(2, 1, 0, "K7ABC", "CQCQCQ", b"second [ and ] text".to_vec());
        let mut stream = b"$GPGGA,123*00\r\n".to_vec();
        stream.extend(wire(&a));
        stream.extend(b"noise");
        stream.extend(wire(&b));
        for split in 1..stream.len() {
            let mut d = Deframer::default();
            let mut ev = Vec::new();
            d.feed(&stream[..split], &mut ev);
            d.feed(&stream[split..], &mut ev);
            let frames: Vec<_> = ev
                .iter()
                .filter_map(|e| {
                    if let Event::Frame(f) = e {
                        Some(f.clone())
                    } else {
                        None
                    }
                })
                .collect();
            assert_eq!(frames, vec![a.clone(), b.clone()], "split at {split}");
            let outside: Vec<u8> = ev
                .iter()
                .filter_map(|e| {
                    if let Event::Outside(o) = e {
                        Some(o.clone())
                    } else {
                        None
                    }
                })
                .flatten()
                .collect();
            assert_eq!(
                outside,
                b"$GPGGA,123*00\r\nnoise".to_vec(),
                "split at {split}"
            );
        }
    }

    #[test]
    fn lost_eob_recovers_second_frame() {
        let a = Frame::new(1, 1, 0, "W1AW", "CQCQCQ", b"first".to_vec());
        let b = Frame::new(2, 1, 0, "K7ABC", "CQCQCQ", b"second".to_vec());
        let mut wa = wire(&a);
        wa.truncate(wa.len() - EOB.len()); // [EOB] lost on air
        let mut stream = wa;
        stream.extend(wire(&b));
        let mut d = Deframer::default();
        let mut ev = Vec::new();
        d.feed(&stream, &mut ev);
        assert!(
            ev.iter().any(|e| matches!(e, Event::Frame(f) if f == &b)),
            "{ev:?}"
        );
    }

    #[test]
    fn lost_sob_orphan_eob_is_excised() {
        let a = Frame::new(1, 1, 0, "W1AW", "CQCQCQ", b"first".to_vec());
        let b = Frame::new(2, 1, 0, "K7ABC", "CQCQCQ", b"second".to_vec());
        let mut stream = wire(&a)[SOB.len()..].to_vec(); // [SOB] lost
        stream.extend(wire(&b));
        let mut d = Deframer::default();
        let mut ev = Vec::new();
        d.feed(&stream, &mut ev);
        assert!(ev.iter().any(|e| matches!(e, Event::Frame(f) if f == &b)));
        assert!(!ev
            .iter()
            .any(|e| matches!(e, Event::Outside(o) if o.windows(5).any(|w| w == EOB))));
    }

    #[test]
    fn poison_marker_refused_p2() {
        let f = Frame::new(0, 1, 0, "N0CALL", "CQCQCQ", b"see [EOB] here".to_vec());
        let raw = f.encode(PayloadEncoding::Never).unwrap();
        assert_eq!(
            wrap(&raw, &EscapeSet::legacy()),
            Err(EnvelopeError::PoisonMarker)
        );
    }

    #[test]
    fn overflow_is_bounded_and_resyncs() {
        let mut d = Deframer::new(64);
        let mut ev = Vec::new();
        d.feed(b"[SOB]", &mut ev);
        d.feed(&[b'x'; 200], &mut ev);
        assert!(ev.iter().any(|e| matches!(e, Event::Overflow { .. })));
        assert!(d.pending() <= 64);
        let good = Frame::new(9, 1, 0, "A", "B", b"ok".to_vec());
        d.feed(&wire(&good), &mut ev);
        assert!(ev
            .iter()
            .any(|e| matches!(e, Event::Frame(f) if f == &good)));
    }

    #[test]
    fn garbage_never_panics() {
        // Deterministic xorshift garbage including marker fragments.
        let mut s: u64 = 0x9E3779B97F4A7C15;
        let mut d = Deframer::new(256);
        let mut ev = Vec::new();
        for _ in 0..20_000 {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            let chunk: Vec<u8> = match s % 7 {
                0 => SOB.to_vec(),
                1 => EOB.to_vec(),
                2 => b"=".to_vec(),
                _ => (0..(s % 40)).map(|i| (s >> (i % 56)) as u8).collect(),
            };
            d.feed(&chunk, &mut ev);
            ev.clear();
            assert!(d.pending() <= 256 + 40);
        }
    }
}
