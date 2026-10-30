//! `aetherdv-bench`: scripted interop and performance runs against one peer,
//! through a running `aetherdv` (Phase C criteria C1 and C6, hardware tests
//! HW-6/HW-7).
//!
//! ```text
//! aetherdv-bench [--api ADDR] [--token-file PATH] --port PORT --peer CALL
//!              [--rounds N] [--suite ping,version,file1k,file10k,form]
//!              [--gap SECONDS] [--out report.json]
//! ```
//!
//! Every test is one the peer answers on its own, so an unattended D-RATS
//! 0.4.x station works as the peer: it replies to pings and version
//! requests and accepts files and forms automatically. Each test waits for
//! its outcome (or a timeout scaled to the path) before the next starts.
//! Files are pseudo-random (incompressible), so sizes on the wire are
//! close to the nominal size.
//!
//! What "ok" means, per test (never more than the evidence shows):
//! * ping / version: the peer's reply arrived;
//! * file / form: every block was acknowledged by the peer ("handed off").

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aetherdv::client::Client;
use serde_json::{json, Value};

fn die(m: &str) -> ! {
    eprintln!("{m}");
    std::process::exit(2)
}

struct Opts {
    api: String,
    token_file: std::path::PathBuf,
    port: String,
    peer: String,
    rounds: u32,
    suite: Vec<String>,
    gap: Duration,
    out: Option<std::path::PathBuf>,
}

fn opts() -> Opts {
    let mut o = Opts {
        api: "127.0.0.1:7373".into(),
        token_file: std::env::temp_dir().join("aetherdv.token"),
        port: String::new(),
        peer: String::new(),
        rounds: 10,
        suite: ["ping", "version", "file1k", "file10k", "form"]
            .map(String::from)
            .to_vec(),
        gap: Duration::from_secs(3),
        out: None,
    };
    let mut a = std::env::args().skip(1);
    while let Some(f) = a.next() {
        let mut v = || {
            a.next()
                .unwrap_or_else(|| die(&format!("{f} needs a value")))
        };
        match f.as_str() {
            "--api" => o.api = v(),
            "--token-file" => o.token_file = v().into(),
            "--port" => o.port = v(),
            "--peer" => o.peer = v().to_ascii_uppercase(),
            "--rounds" => o.rounds = v().parse().unwrap_or_else(|_| die("--rounds N")),
            "--suite" => o.suite = v().split(',').map(|s| s.trim().to_string()).collect(),
            "--gap" => o.gap = Duration::from_secs_f64(v().parse().unwrap_or_else(|_| die("--gap SECONDS"))),
            "--out" => o.out = Some(v().into()),
            _ => die("usage: aetherdv-bench [--api ADDR] [--token-file PATH] --port PORT --peer CALL [--rounds N] [--suite ping,version,file1k,file10k,form] [--gap S] [--out FILE]"),
        }
    }
    if o.port.is_empty() || o.peer.is_empty() {
        die("--port and --peer are required");
    }
    for t in &o.suite {
        if !["ping", "version", "file1k", "file10k", "file50k", "form"].contains(&t.as_str()) {
            die(&format!("unknown test {t}"));
        }
    }
    o
}

fn noise(n: usize, seed: u64) -> Vec<u8> {
    let mut s = seed | 1;
    (0..n)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s as u8
        })
        .collect()
}

#[derive(Default)]
struct Sample {
    ok: bool,
    secs: f64,
    bytes: u64,
    detail: String,
}

/// Seconds the path needs for `bytes` of payload, generously: DV slow data
/// is ~119 B/s at best; ratflectors are effectively instant.
fn budget(path_kind: &str, bytes: u64) -> Duration {
    let rate = match path_kind {
        "internet" => 50_000.0,
        "rf_fast" => 150.0,
        _ => 40.0,
    };
    Duration::from_secs_f64(60.0 + bytes as f64 / rate)
}

fn run_test(c: &mut Client, o: &Opts, path_kind: &str, test: &str, round: u32) -> Sample {
    let t0 = Instant::now();
    let peer = o.peer.clone();
    let mut s = Sample::default();
    let elapsed = |t0: Instant| t0.elapsed().as_secs_f64();
    match test {
        "ping" => {
            if let Err(e) = c.call(
                "stations.ping",
                json!({ "portId": o.port, "callsign": peer }),
            ) {
                s.detail = e.to_string();
                return s;
            }
            let got = c.wait_event(budget(path_kind, 200), |t, d| {
                t == "chat.line" && d["kind"] == "ping_rsp" && d["src"] == peer.as_str()
            });
            match got {
                Ok(Some(d)) => {
                    s.ok = true;
                    s.detail = d["body"].as_str().unwrap_or("").to_string();
                }
                _ => s.detail = "no reply".into(),
            }
        }
        "version" => {
            if let Err(e) = c.call(
                "stations.queryCapabilities",
                json!({ "portId": o.port, "callsign": peer }),
            ) {
                s.detail = e.to_string();
                return s;
            }
            let got = c.wait_event(budget(path_kind, 300), |t, d| {
                t == "station.heard" && d["callsign"] == peer.as_str() && d["software"].is_string()
            });
            match got {
                Ok(Some(d)) => {
                    s.ok = true;
                    s.detail = d["software"].as_str().unwrap_or("").to_string();
                }
                _ => s.detail = "no reply".into(),
            }
        }
        f if f.starts_with("file") => {
            let n: usize = match f {
                "file1k" => 1024,
                "file10k" => 10 * 1024,
                _ => 50 * 1024,
            };
            let dir = std::env::temp_dir().join(format!("aetherdv-bench-{}", std::process::id()));
            let _ = std::fs::create_dir_all(&dir);
            let path = dir.join(format!("bench-{f}-r{round}.bin"));
            if let Err(e) = std::fs::write(&path, noise(n, round as u64 * 7919 + n as u64)) {
                s.detail = e.to_string();
                return s;
            }
            let t = match c.call(
                "transfers.sendFile",
                json!({ "portId": o.port, "peer": peer, "path": path }),
            ) {
                Ok(t) => t,
                Err(e) => {
                    s.detail = e.to_string();
                    return s;
                }
            };
            let id = t["id"].as_str().unwrap_or("").to_string();
            let got = c.wait_event(budget(path_kind, n as u64), |t, d| {
                t == "transfer.progress"
                    && d["id"] == id.as_str()
                    && matches!(d["state"].as_str(), Some("done" | "failed"))
            });
            let _ = std::fs::remove_file(&path);
            match got {
                Ok(Some(d)) => {
                    s.ok = d["state"] == "done";
                    s.bytes = d["totalBytes"].as_u64().unwrap_or(0);
                    s.detail = format!(
                        "{} retries={}{}",
                        d["state"].as_str().unwrap_or(""),
                        d["retries"],
                        d["stateReason"]
                            .as_str()
                            .map(|r| format!(" ({r})"))
                            .unwrap_or_default()
                    );
                }
                _ => {
                    s.detail = "timed out".into();
                    let _ = c.call("transfers.cancel", json!({ "id": id }));
                }
            }
        }
        "form" => {
            let draft = c.call(
                "messages.createDraft",
                json!({
                    "formId": "ICS213_US_OS", "dst": peer,
                    "fields": {
                        "incident": "Aether bench", "subject": format!("bench round {round}"),
                        "message": "Automated interop test message from aetherdv-bench. Please disregard.",
                    },
                }),
            );
            let id = match draft {
                Ok(d) => d["id"].as_str().unwrap_or("").to_string(),
                Err(e) => {
                    s.detail = e.to_string();
                    return s;
                }
            };
            if let Err(e) = c.call("messages.send", json!({ "id": id, "portId": o.port })) {
                s.detail = e.to_string();
                return s;
            }
            let got = c.wait_event(budget(path_kind, 1500), |t, d| {
                t == "message.state"
                    && d["message"]["id"] == id.as_str()
                    && matches!(
                        d["message"]["state"].as_str(),
                        Some("handed_off" | "failed")
                    )
            });
            match got {
                Ok(Some(d)) => {
                    s.ok = d["message"]["state"] == "handed_off";
                    s.detail = d["message"]["state"].as_str().unwrap_or("").to_string();
                }
                _ => {
                    s.detail = "timed out".into();
                    let _ = c.call("messages.cancel", json!({ "id": id }));
                }
            }
        }
        _ => unreachable!(),
    }
    s.secs = elapsed(t0);
    s
}

fn pct(v: &mut [f64], p: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    v[((v.len() - 1) as f64 * p).round() as usize]
}

fn main() {
    let o = opts();
    let token = std::fs::read_to_string(&o.token_file)
        .unwrap_or_else(|e| die(&format!("token {}: {e}", o.token_file.display())));
    let mut c = Client::connect(&o.api, token.trim(), "aetherdv-bench").unwrap_or_else(|e| die(&e));
    let ports = c
        .call("ports.list", json!({}))
        .unwrap_or_else(|e| die(&e.to_string()));
    let port = ports
        .as_array()
        .and_then(|a| a.iter().find(|p| p["id"] == o.port.as_str()).cloned())
        .unwrap_or_else(|| die(&format!("no port {}", o.port)));
    let path_kind = port["pathKind"].as_str().unwrap_or("rf_slow").to_string();
    c.call(
        "events.subscribe",
        json!({ "topics": ["chat.line", "station.heard", "transfer.progress", "message.state"] }),
    )
    .unwrap_or_else(|e| die(&e.to_string()));
    eprintln!(
        "aetherdv-bench: {} rounds of {} against {} on {} ({path_kind}), engine {}",
        o.rounds,
        o.suite.join(","),
        o.peer,
        o.port,
        c.hello["engineVersion"]
    );
    let started = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let mut results: Vec<(String, Vec<Sample>)> =
        o.suite.iter().map(|t| (t.clone(), Vec::new())).collect();
    for round in 1..=o.rounds {
        for (test, samples) in results.iter_mut() {
            let _ = c.drain_events();
            let s = run_test(&mut c, &o, &path_kind, test, round);
            eprintln!(
                "  round {round:>3} {test:<8} {} {:>7.1}s  {}",
                if s.ok { "ok  " } else { "FAIL" },
                s.secs,
                s.detail
            );
            samples.push(s);
            std::thread::sleep(o.gap);
        }
    }
    println!(
        "\n| test | n | ok | success | median s | p90 s | goodput B/s (median) |\n|---|---|---|---|---|---|---|"
    );
    let mut summary = Vec::new();
    for (test, samples) in &results {
        let ok: Vec<&Sample> = samples.iter().filter(|s| s.ok).collect();
        let mut t: Vec<f64> = ok.iter().map(|s| s.secs).collect();
        let mut g: Vec<f64> = ok
            .iter()
            .filter(|s| s.bytes > 0)
            .map(|s| s.bytes as f64 / s.secs)
            .collect();
        let (med, p90, gp) = (pct(&mut t.clone(), 0.5), pct(&mut t, 0.9), pct(&mut g, 0.5));
        let rate = 100.0 * ok.len() as f64 / samples.len().max(1) as f64;
        println!(
            "| {test} | {} | {} | {rate:.0} % | {med:.1} | {p90:.1} | {} |",
            samples.len(),
            ok.len(),
            if gp.is_nan() {
                "-".into()
            } else {
                format!("{gp:.0}")
            }
        );
        summary.push(json!({
            "test": test, "n": samples.len(), "ok": ok.len(), "medianSecs": med, "p90Secs": p90,
            "goodputBps": if gp.is_nan() { Value::Null } else { json!(gp) },
            "samples": samples.iter().map(|s| json!({ "ok": s.ok, "secs": s.secs, "bytes": s.bytes, "detail": s.detail })).collect::<Vec<_>>(),
        }));
    }
    let report = json!({
        "tool": "aetherdv-bench", "engine": c.hello["engineVersion"], "startedAt": started,
        "port": o.port, "pathKind": path_kind, "peer": o.peer, "rounds": o.rounds,
        "gapSecs": o.gap.as_secs_f64(), "results": summary,
        "validationLevel": if path_kind == "internet" { "Interoperability (via ratflector)" } else { "Hardware/Over-the-air: record the rig and path with this report" },
    });
    if let Some(out) = &o.out {
        std::fs::write(out, serde_json::to_vec_pretty(&report).unwrap_or_default())
            .unwrap_or_else(|e| die(&format!("{}: {e}", out.display())));
        eprintln!("report written to {}", out.display());
    }
    let all_ok = results.iter().all(|(_, s)| s.iter().all(|x| x.ok));
    std::process::exit(if all_ok { 0 } else { 1 });
}
