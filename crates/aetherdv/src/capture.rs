//! Wire capture (`.aecap`): what a port received and handed to the link,
//! byte for byte, with timestamps. The evidence format for hardware and
//! interop tests (07 §3), and replayable through the codec.
//!
//! Format: JSON Lines. The first line is a header, every following line one
//! record:
//!
//! ```text
//! {"aecap":1,"port":"IC-9700","kind":"serial","pathKind":"rf_slow","callsign":"KK4ODA","engine":"0.0.1","startedAt":1793247535513}
//! {"t":1520,"d":"rx","x":"5b534f425d..."}
//! {"t":1733,"d":"tx","x":"5b534f425d..."}
//! {"t":9001,"d":"note","m":"port lost: device disconnected"}
//! ```
//!
//! `t` is milliseconds since the capture started (monotonic clock); `x` is
//! the bytes in hex. `rx` is bytes as read from the link; `tx` is bytes as
//! handed to the link (for a serial port, when the scheduler releases a
//! burst to the writer). Ratflector ports record only traffic after the
//! login, so the ratflector password never appears in a capture.
//!
//! Radio ports also record, on the same clock (docs/10 §7 tests):
//!
//! ```text
//! {"t":1800,"d":"civ_tx","x":"fefea2e0200202fd"}      CI-V frame sent
//! {"t":1812,"d":"civ_rx","x":"fefee0a220020240fd"}    CI-V frame received
//! {"t":1813,"d":"ev","k":"channel","v":{"source":"civ_status","busy":"voice"}}
//! ```
//!
//! `ev` records are the engine's timeline: channel reports, scheduler
//! waits, bursts, the radio's TX/RX state, route readings, received call
//! signs, transfer pauses. Repeated identical CI-V polls are recorded once
//! per change to keep files small.

use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde_json::{json, Value};

/// Stop a capture that grows beyond this (a slow-data port fills ~1 MB/day).
pub const MAX_CAPTURE_BYTES: u64 = 64 << 20;

struct Writer {
    out: BufWriter<File>,
    path: PathBuf,
    t0: Instant,
    written: u64,
}

impl Writer {
    fn line(&mut self, v: &Value) -> bool {
        let s = v.to_string();
        let ok = writeln!(self.out, "{s}")
            .and_then(|_| self.out.flush())
            .is_ok();
        self.written += s.len() as u64 + 1;
        ok && self.written < MAX_CAPTURE_BYTES
    }
}

/// A port's capture switch. Cheap to clone and to call when off.
#[derive(Clone, Default)]
pub struct Tap(Arc<Mutex<Option<Writer>>>);

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

impl Tap {
    /// Start capturing into `path` (created; never overwritten).
    pub fn start(&self, path: &Path, header: Value) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        let mut w = Writer {
            out: BufWriter::new(f),
            path: path.to_path_buf(),
            t0: Instant::now(),
            written: 0,
        };
        let mut h = header;
        h["aecap"] = json!(1);
        w.line(&h);
        *self.0.lock().expect("tap lock") = Some(w);
        Ok(())
    }

    /// Stop; returns the file that was being written.
    pub fn stop(&self) -> Option<PathBuf> {
        self.0.lock().expect("tap lock").take().map(|w| w.path)
    }

    pub fn active(&self) -> Option<PathBuf> {
        self.0
            .lock()
            .expect("tap lock")
            .as_ref()
            .map(|w| w.path.clone())
    }

    fn record(&self, make: impl FnOnce(u64) -> Value) {
        let mut g = self.0.lock().expect("tap lock");
        if let Some(w) = g.as_mut() {
            let t = w.t0.elapsed().as_millis() as u64;
            let v = make(t);
            if !w.line(&v) {
                let _ = w.line(&json!({ "t": t, "d": "note", "m": "capture stopped: size limit or write error" }));
                *g = None;
            }
        }
    }

    pub fn rx(&self, b: &[u8]) {
        self.record(|t| json!({ "t": t, "d": "rx", "x": hex(b) }));
    }

    pub fn tx(&self, b: &[u8]) {
        self.record(|t| json!({ "t": t, "d": "tx", "x": hex(b) }));
    }

    pub fn note(&self, m: &str) {
        self.record(|t| json!({ "t": t, "d": "note", "m": m }));
    }

    /// A CI-V frame sent to (`tx = true`) or received from the radio.
    pub fn civ(&self, tx: bool, b: &[u8]) {
        let d = if tx { "civ_tx" } else { "civ_rx" };
        self.record(|t| json!({ "t": t, "d": d, "x": hex(b) }));
    }

    /// A timeline event of kind `k`.
    pub fn event(&self, k: &str, v: Value) {
        self.record(|t| json!({ "t": t, "d": "ev", "k": k, "v": v }));
    }

    /// Whether a capture is running (to skip building event payloads).
    pub fn on(&self) -> bool {
        self.0.lock().expect("tap lock").is_some()
    }
}

/// One record of a capture file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Record {
    Rx { t: u64, bytes: Vec<u8> },
    Tx { t: u64, bytes: Vec<u8> },
    Note { t: u64, text: String },
    Civ { t: u64, tx: bool, bytes: Vec<u8> },
    Event { t: u64, kind: String, data: Value },
}

/// Read a capture: (header, records). Malformed lines are errors with
/// their line number; a truncated last line (crash while writing) is
/// skipped.
pub fn read(path: &Path) -> Result<(Value, Vec<Record>), String> {
    let f = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let lines: Vec<String> = BufReader::new(f)
        .lines()
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    let header: Value = lines
        .first()
        .and_then(|l| serde_json::from_str(l).ok())
        .filter(|h: &Value| h["aecap"] == 1)
        .ok_or("not an .aecap file (bad or missing header)")?;
    let mut recs = Vec::new();
    let last = lines.len().saturating_sub(1);
    for (i, l) in lines.iter().enumerate().skip(1) {
        let v: Value = match serde_json::from_str(l) {
            Ok(v) => v,
            Err(_) if i == last => break,
            Err(e) => return Err(format!("line {}: {e}", i + 1)),
        };
        let t = v["t"].as_u64().ok_or(format!("line {}: no time", i + 1))?;
        let bytes = || {
            v["x"]
                .as_str()
                .and_then(unhex)
                .ok_or(format!("line {}: bad hex", i + 1))
        };
        recs.push(match v["d"].as_str() {
            Some("rx") => Record::Rx { t, bytes: bytes()? },
            Some("tx") => Record::Tx { t, bytes: bytes()? },
            Some("note") => Record::Note {
                t,
                text: v["m"].as_str().unwrap_or("").to_string(),
            },
            Some(d @ ("civ_tx" | "civ_rx")) => Record::Civ {
                t,
                tx: d == "civ_tx",
                bytes: bytes()?,
            },
            Some("ev") => Record::Event {
                t,
                kind: v["k"].as_str().unwrap_or("").to_string(),
                data: v["v"].clone(),
            },
            _ => return Err(format!("line {}: unknown record", i + 1)),
        });
    }
    Ok((header, recs))
}

/// Plain-text report of one capture for a test summary: what the port
/// did, transmission timing (key-up and real air time from CI-V), time
/// spent waiting by reason, and the timeline events worth reading first.
pub fn report(header: &Value, records: &[Record]) -> String {
    use std::fmt::Write as _;
    let mut o = String::new();
    let secs = |t: u64| t as f64 / 1000.0;
    let _ = writeln!(
        o,
        "Port {} ({}, {}), engine {}",
        header["port"].as_str().unwrap_or("?"),
        header["kind"].as_str().unwrap_or("?"),
        header["pathKind"].as_str().unwrap_or("?"),
        header["engine"].as_str().unwrap_or("?")
    );
    let end = records.iter().map(Record::t).max().unwrap_or(0);
    let count = |f: &dyn Fn(&Record) -> bool| records.iter().filter(|r| f(r)).count();
    let bytes = |rx: bool| -> usize {
        records
            .iter()
            .map(|r| match r {
                Record::Rx { bytes, .. } if rx => bytes.len(),
                Record::Tx { bytes, .. } if !rx => bytes.len(),
                _ => 0,
            })
            .sum()
    };
    let _ = writeln!(
        o,
        "Recorded {:.0} s: {} B received, {} B sent, {} CI-V frames, {} timeline events",
        secs(end),
        bytes(true),
        bytes(false),
        count(&|r| matches!(r, Record::Civ { .. })),
        count(&|r| matches!(r, Record::Event { .. }))
    );
    // Transmission timing.
    let mut rows = Vec::new();
    for (i, r) in records.iter().enumerate() {
        let Record::Event { t, kind, data } = r else {
            continue;
        };
        if kind != "burst" {
            continue;
        }
        let (mut on, mut off) = (None, None);
        for r2 in &records[i + 1..] {
            match r2 {
                Record::Event { kind, .. } if kind == "burst" => break,
                Record::Event { t: tt, kind, data } if kind == "tx" => {
                    if data["on"] == true && on.is_none() {
                        on = Some(*tt);
                    } else if data["on"] == false && on.is_some() {
                        off = Some(*tt);
                        break;
                    }
                }
                _ => {}
            }
        }
        rows.push((
            *t,
            data["bytes"].as_u64().unwrap_or(0),
            data["airEstMs"].as_u64().unwrap_or(0),
            on.map(|o| o - t),
            on.zip(off).map(|(a, b)| b - a),
        ));
    }
    if rows.is_empty() {
        let _ = writeln!(o, "No transmissions.");
    } else {
        let _ = writeln!(o, "\nTransmissions ({}):", rows.len());
        let _ = writeln!(
            o,
            "  {:>9} {:>6} {:>7} {:>8} {:>8} {:>7}",
            "at s", "bytes", "est s", "keyup s", "air s", "diff s"
        );
        let f = |x: Option<u64>| x.map_or("-".into(), |v| format!("{:.2}", secs(v)));
        for (t, b, est, k, a) in rows.iter().take(40) {
            let diff = a.map_or("-".into(), |a| {
                format!("{:+.2}", (a as f64 - *est as f64) / 1000.0)
            });
            let _ = writeln!(
                o,
                "  {:>9.1} {:>6} {:>7.2} {:>8} {:>8} {:>7}",
                secs(*t),
                b,
                secs(*est),
                f(*k),
                f(*a),
                diff
            );
        }
        if rows.len() > 40 {
            let _ = writeln!(o, "  … {} more", rows.len() - 40);
        }
        let ks: Vec<u64> = rows.iter().filter_map(|r| r.3).collect();
        let ds: Vec<f64> = rows
            .iter()
            .filter_map(|r| r.4.map(|a| a as f64 - r.2 as f64))
            .collect();
        if ks.is_empty() {
            let _ = writeln!(
                o,
                "  No TX/RX state recorded (no CI-V, or the radio didn't answer 1C 00)."
            );
        } else {
            let _ = writeln!(
                o,
                "  Key-up: mean {:.2} s, max {:.2} s. Real minus estimated air time: mean {:+.2} s. (+/- 0.1 s: TX state is polled every 100 ms.)",
                ks.iter().sum::<u64>() as f64 / ks.len() as f64 / 1000.0,
                secs(*ks.iter().max().unwrap_or(&0)),
                if ds.is_empty() { 0.0 } else { ds.iter().sum::<f64>() / ds.len() as f64 / 1000.0 }
            );
        }
    }
    // Waiting by reason.
    let mut waits: Vec<(String, u64)> = Vec::new();
    let mut cur: Option<(String, u64)> = None;
    for r in records {
        if let Record::Event { t, kind, data } = r {
            if kind == "sched" {
                if let Some((why, since)) = cur.take() {
                    match waits.iter_mut().find(|(w, _)| *w == why) {
                        Some((_, v)) => *v += t - since,
                        None => waits.push((why, t - since)),
                    }
                }
                if data["state"] == "waiting" {
                    cur = Some((data["why"].as_str().unwrap_or("?").to_string(), *t));
                }
            }
        }
    }
    if !waits.is_empty() {
        let _ = writeln!(
            o,
            "\nWaiting, by reason: {}",
            waits
                .iter()
                .map(|(w, v)| format!("{w} {:.1} s", secs(*v)))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    // Timeline highlights.
    let interesting = [
        "marker",
        "state",
        "gate",
        "route",
        "route_error",
        "civ_open",
        "my_call",
        "sources",
        "radio_changed",
        "voice_inference_off",
        "access_check",
        "pause",
        "resume",
        "rx_calls",
        "rx_message",
    ];
    let mut lines = Vec::new();
    let mut last_route = String::new();
    for r in records {
        match r {
            Record::Event { t, kind, data } if interesting.contains(&kind.as_str()) => {
                // Route readings repeat every minute: show changes only.
                if kind == "route" {
                    let s = data.to_string();
                    if s == last_route {
                        continue;
                    }
                    last_route = s;
                }
                if kind == "civ_open" && data["error"].is_null() {
                    continue;
                }
                lines.push(format!("  {:>9.1}  {kind} {data}", secs(*t)));
            }
            Record::Note { t, text } => lines.push(format!("  {:>9.1}  note {text}", secs(*t))),
            _ => {}
        }
    }
    if !lines.is_empty() {
        let _ = writeln!(
            o,
            "\nTimeline ({} entries{}):",
            lines.len(),
            if lines.len() > 80 { ", last 80" } else { "" }
        );
        for l in lines.iter().skip(lines.len().saturating_sub(80)) {
            let _ = writeln!(o, "{l}");
        }
    }
    o
}

impl Record {
    pub fn t(&self) -> u64 {
        match self {
            Record::Rx { t, .. }
            | Record::Tx { t, .. }
            | Record::Note { t, .. }
            | Record::Civ { t, .. }
            | Record::Event { t, .. } => *t,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_read_round_trip_and_stop() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("caps/net.aecap");
        let tap = Tap::default();
        tap.rx(b"ignored while off");
        tap.start(&p, json!({ "port": "net" })).unwrap();
        assert!(tap.start(&p, json!({})).is_err(), "never overwrites");
        tap.rx(b"[SOB]abc[EOB]");
        tap.tx(&[0, 0xff, 0x11]);
        tap.note("lost");
        tap.civ(true, &[0xFE, 0xFE, 0xA2, 0xE0, 0x1C, 0x00, 0xFD]);
        tap.event("tx", json!({ "on": true }));
        assert!(tap.on());
        assert_eq!(tap.active().as_deref(), Some(p.as_path()));
        assert_eq!(tap.stop().as_deref(), Some(p.as_path()));
        tap.tx(b"after stop");
        let (h, r) = read(&p).unwrap();
        assert_eq!(
            (h["aecap"].as_u64(), h["port"].as_str()),
            (Some(1), Some("net"))
        );
        assert_eq!(r.len(), 5);
        assert!(matches!(&r[3], Record::Civ { tx: true, bytes, .. } if bytes[4] == 0x1C));
        assert!(
            matches!(&r[4], Record::Event { kind, data, .. } if kind == "tx" && data["on"] == true)
        );
        assert!(!tap.on());
        assert!(matches!(&r[0], Record::Rx { bytes, .. } if bytes == b"[SOB]abc[EOB]"));
        assert!(matches!(&r[1], Record::Tx { bytes, .. } if bytes == &[0, 0xff, 0x11]));
        assert!(matches!(&r[2], Record::Note { text, .. } if text == "lost"));
    }

    #[test]
    fn truncated_last_line_is_tolerated_but_corruption_is_not() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("x.aecap");
        std::fs::write(
            &p,
            "{\"aecap\":1}\n{\"t\":1,\"d\":\"rx\",\"x\":\"41\"}\n{\"t\":2,\"d\":\"rx\",\"x\":\"4",
        )
        .unwrap();
        assert_eq!(read(&p).unwrap().1.len(), 1);
        std::fs::write(&p, "{\"aecap\":1}\n{\"t\":1,\"d\":\"rx\",\"x\":\"4g\"}\n{\"t\":2,\"d\":\"rx\",\"x\":\"41\"}\n").unwrap();
        assert!(read(&p).unwrap_err().contains("line 2"));
        std::fs::write(&p, "hello\n").unwrap();
        assert!(read(&p).is_err());
    }
}
