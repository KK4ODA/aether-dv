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
}

/// One record of a capture file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Record {
    Rx { t: u64, bytes: Vec<u8> },
    Tx { t: u64, bytes: Vec<u8> },
    Note { t: u64, text: String },
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
            _ => return Err(format!("line {}: unknown record", i + 1)),
        });
    }
    Ok((header, recs))
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
        assert_eq!(tap.active().as_deref(), Some(p.as_path()));
        assert_eq!(tap.stop().as_deref(), Some(p.as_path()));
        tap.tx(b"after stop");
        let (h, r) = read(&p).unwrap();
        assert_eq!(
            (h["aecap"].as_u64(), h["port"].as_str()),
            (Some(1), Some("net"))
        );
        assert_eq!(r.len(), 3);
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
