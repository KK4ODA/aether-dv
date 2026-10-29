//! `aecap`: read a wire capture (`.aecap`) written by `aetherd`.
//!
//! ```text
//! aecap show FILE [--hex]   every frame and stray byte run, decoded, in time order
//! aecap stats FILE          counts per direction, session and frame type
//! ```
//!
//! Each direction is run through its own deframer, exactly as the engine
//! does on receive, so what `show` prints is what the engine saw.

use std::collections::BTreeMap;

use aether_ddt2::session::{chat, control, id, stateful};
use aether_ddt2::{Deframer, Event, Frame};
use aetherd::capture::{self, Record};

fn die(m: &str) -> ! {
    eprintln!("{m}");
    std::process::exit(1)
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
        _ => die("usage: aecap show FILE [--hex] | aecap stats FILE"),
    };
    let show_hex = args.iter().any(|a| a == "--hex");
    let (header, records) = capture::read(std::path::Path::new(file)).unwrap_or_else(|e| die(&e));
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
        _ => die("usage: aecap show FILE [--hex] | aecap stats FILE"),
    }
}
