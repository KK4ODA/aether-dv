//! `aether-cli`: minimal terminal client for a running `aetherd`.
//!
//! ```text
//! aether-cli [--api 127.0.0.1:7373] [--token-file PATH] COMMAND
//!   ports                         list ports and their state
//!   stations                      list heard stations
//!   send PORT DST TEXT...         send chat (DST = CQCQCQ or a callsign)
//!   history [PORT] [N]            recent chat lines (all ports if PORT omitted)
//!   ping PORT CALL                legacy ping
//!   version PORT CALL             ask a station for its version/capabilities
//!   send-file PORT CALL PATH      send a file (legacy D-RATS file transfer)
//!   transfers [all]               active transfers (or all, newest first)
//!   cancel ID                     cancel an active transfer
//!   ics213 CALL SUBJECT MESSAGE [field=value...]
//!                                 create an ICS-213 draft to CALL (prints its id)
//!   msg-send ID [PORT]            send (or retry) a message; PORT defaults to
//!                                 where CALL was last heard
//!   messages [FOLDER]             inbox, outbox, sent, drafts (default: all)
//!   msg ID                        show a message's fields and delivery events
//!   positions [CALL]              latest position of every station, or CALL's history
//!   send-position PORT [dprs|nmea]  send our configured [position] once
//!   capture-start PORT            start a wire capture (.aecap) on PORT
//!   capture-stop PORT             stop it; prints the file (read it with `aecap show`)
//!   log [N]                       the last N engine log entries (default 50)
//!   bundle [captures]             write a support bundle (.tar), optionally with captures
//!   tail                          print chat, port, station and transfer events live
//! ```

use std::net::TcpStream;

use serde_json::{json, Value};
use tungstenite::{connect, stream::MaybeTlsStream, Message, WebSocket};

type Ws = WebSocket<MaybeTlsStream<TcpStream>>;

fn die(msg: &str) -> ! {
    eprintln!("{msg}");
    std::process::exit(1)
}

struct Client {
    ws: Ws,
    next: u64,
}

impl Client {
    fn call(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let id = self.next;
        let req = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        self.ws
            .send(Message::text(req.to_string()))
            .unwrap_or_else(|e| die(&format!("send: {e}")));
        loop {
            let m = self
                .ws
                .read()
                .unwrap_or_else(|e| die(&format!("read: {e}")));
            let Message::Text(t) = m else { continue };
            let v: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
            if v.get("id") == Some(&json!(id)) {
                if let Some(e) = v.get("error") {
                    die(&format!(
                        "error {}: {}",
                        e["code"],
                        e["message"].as_str().unwrap_or("")
                    ));
                }
                return v["result"].clone();
            }
        }
    }
}

fn line(c: &Value) -> String {
    let dir = if c["direction"] == "out" { ">" } else { "<" };
    let state = c["txState"]
        .as_str()
        .map(|s| format!("  [{s}]"))
        .unwrap_or_default();
    format!(
        "{} {:<10} {} {:<8} -> {:<8} {}{}",
        dir,
        c["portId"].as_str().unwrap_or("-"),
        c["kind"].as_str().unwrap_or(""),
        c["src"].as_str().unwrap_or(""),
        c["dst"].as_str().unwrap_or(""),
        c["body"].as_str().unwrap_or(""),
        state
    )
}

fn transfer(t: &Value) -> String {
    let total = t["totalBytes"].as_u64().unwrap_or(0);
    let done = t["doneBytes"].as_u64().unwrap_or(0);
    let pct = (done * 100).checked_div(total).unwrap_or(0);
    format!(
        "{}  {:<7} {:<8} {:<10} {:<9} {:>3}%  {}{}{}",
        t["id"].as_str().unwrap_or(""),
        t["direction"].as_str().unwrap_or(""),
        t["peer"].as_str().unwrap_or(""),
        t["portId"].as_str().unwrap_or("-"),
        t["state"].as_str().unwrap_or(""),
        pct,
        t["filename"].as_str().unwrap_or(""),
        t["stateReason"]
            .as_str()
            .map(|r| format!("  ({r})"))
            .unwrap_or_default(),
        if t["wrapWarning"] == true {
            "  [>256 blocks: delivery unconfirmable with legacy peers]"
        } else {
            ""
        },
    )
}

fn message(m: &Value) -> String {
    format!(
        "{}  {:<6} {:<10} {:<8} -> {:<8} {}",
        m["id"].as_str().unwrap_or(""),
        m["folder"].as_str().unwrap_or(""),
        m["state"].as_str().unwrap_or(""),
        m["src"].as_str().unwrap_or(""),
        m["dst"].as_str().unwrap_or(""),
        m["subject"].as_str().unwrap_or(""),
    )
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut api = "127.0.0.1:7373".to_string();
    let mut token_file = std::env::temp_dir().join("aetherd.token");
    while args.first().is_some_and(|a| a.starts_with("--")) {
        let flag = args.remove(0);
        let val = if args.is_empty() {
            die("missing flag value")
        } else {
            args.remove(0)
        };
        match flag.as_str() {
            "--api" => api = val,
            "--token-file" => token_file = val.into(),
            _ => die(&format!("unknown flag {flag}")),
        }
    }
    let token = std::fs::read_to_string(&token_file)
        .unwrap_or_else(|e| die(&format!("cannot read token {}: {e}", token_file.display())))
        .trim()
        .to_string();
    let (ws, _) = connect(format!("ws://{api}"))
        .unwrap_or_else(|e| die(&format!("cannot connect to aetherd at {api}: {e}")));
    let mut c = Client { ws, next: 0 };
    let hello = c.call(
        "system.hello",
        json!({ "clientName": "aether-cli", "apiVersion": "0.1.0", "token": token }),
    );
    let cmd = args.first().cloned().unwrap_or_default();
    match cmd.as_str() {
        "ports" => {
            for p in c.call("ports.list", json!({})).as_array().cloned().unwrap_or_default() {
                println!(
                    "{:<14} {:<11} {:<9} {:<12} {}",
                    p["name"].as_str().unwrap_or(""),
                    p["kind"].as_str().unwrap_or(""),
                    p["pathKind"].as_str().unwrap_or(""),
                    p["state"].as_str().unwrap_or(""),
                    p["detail"].as_str().unwrap_or("")
                );
            }
        }
        "stations" => {
            for s in c.call("stations.list", json!({})).as_array().cloned().unwrap_or_default() {
                println!("{:<9} {}", s["callsign"].as_str().unwrap_or(""), s["software"].as_str().unwrap_or("-"));
            }
        }
        "send" if args.len() >= 4 => {
            let r = c.call("chat.send", json!({ "portId": args[1], "dst": args[2], "body": args[3..].join(" ") }));
            println!("{}", line(&r));
        }
        "history" => {
            let port = args.get(1).filter(|a| a.parse::<u32>().is_err()).cloned();
            let n: u32 = args.iter().skip(1).find_map(|a| a.parse().ok()).unwrap_or(20);
            let mut v = c.call("chat.history", json!({ "portId": port, "limit": n })).as_array().cloned().unwrap_or_default();
            v.reverse();
            for l in v {
                println!("{}", line(&l));
            }
        }
        "ping" if args.len() == 3 => {
            c.call("stations.ping", json!({ "portId": args[1], "callsign": args[2] }));
            println!("ping sent; replies appear in `tail`");
        }
        "version" if args.len() == 3 => {
            c.call("stations.queryCapabilities", json!({ "portId": args[1], "callsign": args[2] }));
            println!("version request sent; the answer appears in `tail` and `stations`");
        }
        "send-file" if args.len() == 4 => {
            let path = std::fs::canonicalize(&args[3]).unwrap_or_else(|e| die(&format!("{}: {e}", args[3])));
            let r = c.call("transfers.sendFile", json!({ "portId": args[1], "peer": args[2], "path": path }));
            println!("{}", transfer(&r));
        }
        "transfers" => {
            let all = args.get(1).is_some_and(|a| a == "all");
            for t in c.call("transfers.list", json!({ "includeFinished": all })).as_array().cloned().unwrap_or_default() {
                println!("{}", transfer(&t));
            }
        }
        "cancel" if args.len() == 2 => {
            c.call("transfers.cancel", json!({ "id": args[1] }));
            println!("cancelled");
        }
        "ics213" if args.len() >= 4 => {
            let mut fields = serde_json::Map::new();
            fields.insert("subject".into(), json!(args[2]));
            fields.insert("message".into(), json!(args[3]));
            for kv in &args[4..] {
                let (k, v) = kv.split_once('=').unwrap_or_else(|| die(&format!("expected field=value, got {kv}")));
                fields.insert(k.into(), json!(v));
            }
            let r = c.call("messages.createDraft", json!({ "formId": "ICS213_US_OS", "fields": fields, "dst": args[1] }));
            println!("{}", message(&r));
        }
        "msg-send" if args.len() == 2 || args.len() == 3 => {
            let r = c.call("messages.send", json!({ "id": args[1], "portId": args.get(2) }));
            println!("{}", message(&r));
        }
        "messages" => {
            let folder = args.get(1).cloned();
            for m in c.call("messages.list", json!({ "folder": folder })).as_array().cloned().unwrap_or_default() {
                println!("{}", message(&m));
            }
        }
        "msg" if args.len() == 2 => {
            let m = c.call("messages.get", json!({ "id": args[1] }));
            println!("{}\nmid {}  form {}", message(&m), m["mid"].as_str().unwrap_or(""), m["formId"].as_str().unwrap_or(""));
            if let Some(f) = m["fields"].as_object() {
                for (k, v) in f {
                    if v.as_str().is_some_and(|v| !v.is_empty()) {
                        println!("  {k:<12} {}", v.as_str().unwrap_or("").replace('\n', "\n               "));
                    }
                }
            }
            for e in c.call("messages.events", json!({ "id": args[1] })).as_array().cloned().unwrap_or_default() {
                println!(
                    "  event {:<11} {:<9} hop {:<8} {}",
                    e["state"].as_str().unwrap_or(""),
                    e["portId"].as_str().unwrap_or("-"),
                    e["hop"].as_str().unwrap_or("-"),
                    e["evidence"].as_str().unwrap_or("")
                );
            }
        }
        "positions" => {
            let call = args.get(1).cloned();
            for p in c.call("stations.positions", json!({ "callsign": call })).as_array().cloned().unwrap_or_default() {
                println!(
                    "{:<10} {:>10.5} {:>11.5} {:<4} {:<5} {}",
                    p["callsign"].as_str().unwrap_or(""),
                    p["lat"].as_f64().unwrap_or(0.0),
                    p["lon"].as_f64().unwrap_or(0.0),
                    p["sourceFmt"].as_str().unwrap_or(""),
                    p["symbol"].as_str().unwrap_or(""),
                    p["comment"].as_str().unwrap_or("")
                );
            }
        }
        "send-position" if args.len() == 2 || args.len() == 3 => {
            let r = c.call("stations.sendPosition", json!({ "portId": args[1], "format": args.get(2) }));
            println!("sent: {}", r["text"].as_str().unwrap_or("").trim_end());
        }
        "capture-start" if args.len() == 2 => {
            let r = c.call("diag.captureStart", json!({ "portId": args[1] }));
            println!("capturing {} to {}", args[1], r["path"].as_str().unwrap_or(""));
        }
        "capture-stop" if args.len() == 2 => {
            let r = c.call("diag.captureStop", json!({ "portId": args[1] }));
            println!("{} ({} bytes)", r["path"].as_str().unwrap_or(""), r["bytes"]);
        }
        "log" => {
            let n: u32 = args.get(1).and_then(|a| a.parse().ok()).unwrap_or(50);
            let mut v = c.call("diag.events", json!({ "limit": n })).as_array().cloned().unwrap_or_default();
            v.reverse();
            for e in v {
                println!(
                    "{} {:<5} {:<10} {}",
                    e["at"],
                    e["level"].as_str().unwrap_or(""),
                    e["portId"].as_str().unwrap_or("-"),
                    e["message"].as_str().unwrap_or("")
                );
            }
        }
        "bundle" => {
            let caps = args.get(1).is_some_and(|a| a == "captures");
            let r = c.call("diag.supportBundle", json!({ "includeCaptures": caps }));
            println!("{}", r["path"].as_str().unwrap_or(""));
        }
        "tail" => {
            c.call("events.subscribe", json!({ "topics": ["chat.line", "port.state", "station.heard", "transfer.progress", "message.state", "station.position", "engine.event"] }));
            eprintln!("connected to aetherd {} as {}", hello["engineVersion"], hello["profile"]);
            loop {
                let m = c.ws.read().unwrap_or_else(|e| die(&format!("read: {e}")));
                let Message::Text(t) = m else { continue };
                let v: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
                let p = &v["params"];
                match p["topic"].as_str() {
                    Some("chat.line") => println!("{}", line(&p["data"])),
                    Some("transfer.progress") => println!("  {}", transfer(&p["data"])),
                    Some("message.state") => println!("  message {}", message(&p["data"]["message"])),
                    Some(topic) => println!("  [{topic}] {}", p["data"]),
                    None => {}
                }
            }
        }
        _ => die("usage: aether-cli [--api ADDR] [--token-file PATH] ports|stations|send PORT DST TEXT|history [PORT] [N]|ping PORT CALL|version PORT CALL|send-file PORT CALL PATH|transfers [all]|cancel ID|ics213 CALL SUBJECT MESSAGE|msg-send ID [PORT]|messages [FOLDER]|msg ID|positions [CALL]|send-position PORT [dprs|nmea]|capture-start PORT|capture-stop PORT|log [N]|bundle [captures]|tail"),
    }
}
