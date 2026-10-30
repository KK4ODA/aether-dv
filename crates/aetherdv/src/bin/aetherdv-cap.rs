//! `aetherdv-cap`: read a wire capture (`.aecap`) written by `aetherdv`.
//!
//! ```text
//! aetherdv-cap show FILE [--hex] [--no-civ]
//!                                  every frame, stray byte run, CI-V frame and
//!                                  timeline event, decoded, in time order
//! aetherdv-cap stats FILE          counts per direction, session and frame type
//! aetherdv-cap timing FILE         the test-summary report: per burst key-up delay
//!                                  and real air time (needs CI-V), waits, timeline
//! ```
//!
//! Each direction is run through its own deframer, exactly as the engine
//! does on receive, so what `show` prints is what the engine saw.

use std::collections::BTreeMap;

use aether_ddt2::session::{chat, control, id, stateful};
use aether_ddt2::{Deframer, Event, Frame};
use aetherdv::capture::{self, Record};

fn die(m: &str) -> ! {
    eprintln!("{m}");
    std::process::exit(1)
}

/// A short name for a CI-V command (D-STAR and Fast Data commands we use).
fn civ_name(body: &[u8]) -> &'static str {
    match body {
        [0x1F, 0x00, ..] => "MY call",
        [0x1F, 0x01, ..] => "UR/R1/R2",
        [0x20, 0x00, ..] => "RX call signs",
        [0x20, 0x01, ..] => "RX message",
        [0x20, 0x02, ..] => "RX status",
        [0x15, 0x01, ..] => "squelch",
        [0x1C, 0x00, ..] => "TX/RX",
        [0x0F, ..] => "duplex",
        [0x00, ..] => "frequency (transceive)",
        [0x01, ..] => "mode (transceive)",
        [0x1A, 0x05, 0x00, 0x77, ..] | [0x22, 0x03, ..] => "Fast Data",
        [0xFB] => "OK",
        [0xFA] => "NG",
        _ => "",
    }
}

fn civ_line(tx: bool, bytes: &[u8]) -> String {
    let mut d = aether_civ::Deframer::default();
    let frames = d.feed(bytes);
    let Some(f) = frames.first() else {
        return format!("civ {} {}", if tx { ">" } else { "<" }, hexs(bytes, 48));
    };
    let who = if tx {
        ">"
    } else if f.from == 0xE0 {
        "< (echo)"
    } else {
        "<"
    };
    format!(
        "civ {who} {:02X}->{:02X} {} {}",
        f.from,
        f.to,
        hexs(&f.body, 48),
        civ_name(&f.body)
    )
}

fn printable(b: &[u8], max: usize) -> String {
    let mut s: String = b
        .iter()
        .take(max)
        .map(|&c| match c {
            b'\n' => '⏎',
            b'\r' => '↵',
            0x20..=0x7e => c as char,
            _ => '·',
        })
        .collect();
    if b.len() > max {
        s.push('…');
    }
    s
}

fn hexs(b: &[u8], max: usize) -> String {
    let mut s: Vec<String> = b.iter().take(max).map(|x| format!("{x:02x}")).collect();
    if b.len() > max {
        s.push("…".into());
    }
    s.join(" ")
}

/// One-line description of a frame's meaning.
fn describe(f: &Frame) -> String {
    let body = |max| format!("\"{}\"", printable(&f.payload, max));
    match (f.session, f.kind) {
        (id::CONTROL, control::PING) => "control PING".into(),
        (id::CONTROL, control::ACK) => format!("control ACK {:?}", f.payload),
        (id::CONTROL, control::END) => format!("control END {}", body(8)),
        (id::CONTROL, k) if k >= control::NEW_BASE => {
            let class = match k - control::NEW_BASE {
                control::CLASS_GENERAL => "general",
                control::CLASS_SOCKET => "socket",
                control::CLASS_FILE => "file",
                control::CLASS_FORM => "form",
                _ => "?",
            };
            format!(
                "control NEW {class} id={} name={}",
                f.payload.first().copied().unwrap_or(0),
                printable(f.payload.get(1..).unwrap_or(&[]), 40)
            )
        }
        (id::CHAT, chat::TEXT) => format!("chat {}", body(120)),
        (id::CHAT, chat::PING_REQ) => format!("ping request {}", body(60)),
        (id::CHAT, chat::PING_RSP) => format!("ping reply {}", body(80)),
        (id::CHAT, chat::ECHO_REQ) => format!("echo request {}B", f.payload.len()),
        (id::CHAT, chat::ECHO_RSP) => format!("echo reply {}B", f.payload.len()),
        (id::CHAT, chat::STATUS) => format!("status {}", body(60)),
        (id::RPC, k) => format!(
            "rpc {} job={} {}",
            if k == 0 { "request" } else { "response" },
            f.seq,
            body(60)
        ),
        (_, stateful::DATA) => format!(
            "data block {} ({} B) {}",
            f.seq,
            f.payload.len(),
            printable(&f.payload, 24)
        ),
        (_, stateful::ACK) => format!("ACK {:?}", f.payload),
        (_, stateful::REQACK) => format!("REQACK {:?}", f.payload),
        (s, k) => format!("session {s} type {k} {}B", f.payload.len()),
    }
}

fn inc(stats: &mut BTreeMap<String, u64>, k: String, n: u64) {
    *stats.entry(k).or_default() += n;
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (cmd, file) = match args.as_slice() {
        [c, f, ..] => (c.as_str(), f.as_str()),
        _ => die(USAGE),
    };
    let show_hex = args.iter().any(|a| a == "--hex");
    let no_civ = args.iter().any(|a| a == "--no-civ");
    let (header, records) = capture::read(std::path::Path::new(file)).unwrap_or_else(|e| die(&e));
    if cmd == "timing" {
        print!("{}", capture::report(&header, &records));
        return;
    }
    let (mut rx, mut tx) = (Deframer::default(), Deframer::default());
    let mut stats: BTreeMap<String, u64> = BTreeMap::new();
    if cmd == "show" {
        println!(
            "capture of {} ({}, {}) by {} {}",
            header["port"].as_str().unwrap_or("?"),
            header["kind"].as_str().unwrap_or("?"),
            header["pathKind"].as_str().unwrap_or("?"),
            header["callsign"].as_str().unwrap_or("?"),
            header["engine"].as_str().unwrap_or("?"),
        );
    }
    for r in records {
        let (t, dir, bytes) = match &r {
            Record::Rx { t, bytes } => (*t, "rx", bytes),
            Record::Tx { t, bytes } => (*t, "tx", bytes),
            Record::Note { t, text } => {
                if cmd == "show" {
                    println!("{:>10.3}  --  {text}", *t as f64 / 1000.0);
                }
                continue;
            }
            Record::Civ { t, tx, bytes } => {
                inc(
                    &mut stats,
                    format!("civ {} frames", if *tx { "sent" } else { "received" }),
                    1,
                );
                if cmd == "show" && !no_civ {
                    println!("{:>10.3}  {}", *t as f64 / 1000.0, civ_line(*tx, bytes));
                }
                continue;
            }
            Record::Event { t, kind, data } => {
                inc(&mut stats, format!("event {kind}"), 1);
                if cmd == "show" {
                    println!("{:>10.3}  ev  {kind} {data}", *t as f64 / 1000.0);
                }
                continue;
            }
        };
        let mut evs = Vec::new();
        if dir == "rx" {
            rx.feed(bytes, &mut evs)
        } else {
            tx.feed(bytes, &mut evs)
        }
        inc(&mut stats, format!("{dir} bytes"), bytes.len() as u64);
        for e in evs {
            let line = match &e {
                Event::Frame { frame, wire } => {
                    inc(
                        &mut stats,
                        format!("{dir} frames, session {}", frame.session),
                        1,
                    );
                    format!(
                        "{:<8}> {:<8} s{} t{} {:>4}B {}{}",
                        String::from_utf8_lossy(&frame.src),
                        String::from_utf8_lossy(&frame.dst),
                        frame.session,
                        frame.kind,
                        wire.len(),
                        if frame.compressed { "z " } else { "" },
                        describe(frame)
                    ) + &if show_hex {
                        format!("\n{:>16}{}", "", hexs(wire, 64))
                    } else {
                        String::new()
                    }
                }
                Event::BadFrame { error, body } => {
                    inc(&mut stats, format!("{dir} bad frames"), 1);
                    format!("BAD FRAME {error:?} ({} B) {}", body.len(), hexs(body, 32))
                }
                Event::Outside(b) => {
                    inc(&mut stats, format!("{dir} outside runs"), 1);
                    format!("outside {} B: {}", b.len(), printable(b, 100))
                }
                Event::Overflow { discarded } => {
                    inc(&mut stats, format!("{dir} overflows"), 1);
                    format!("OVERFLOW, {discarded} B discarded")
                }
            };
            if cmd == "show" {
                println!("{:>10.3}  {dir}  {line}", t as f64 / 1000.0);
            }
        }
    }
    match cmd {
        "show" => {}
        "stats" => {
            for (k, v) in stats {
                println!("{k:<28} {v}");
            }
        }
        _ => die(USAGE),
    }
}

const USAGE: &str =
    "usage: aetherdv-cap show FILE [--hex] [--no-civ] | aetherdv-cap stats FILE | aetherdv-cap timing FILE";
