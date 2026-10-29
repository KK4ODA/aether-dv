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
//!   tail                          print chat, port and station events live
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
        "tail" => {
            c.call("events.subscribe", json!({ "topics": ["chat", "chat_state", "port", "station"] }));
            eprintln!("connected to aetherd {} as {}", hello["engineVersion"], hello["profile"]);
            loop {
                let m = c.ws.read().unwrap_or_else(|e| die(&format!("read: {e}")));
                let Message::Text(t) = m else { continue };
                let v: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
                let p = &v["params"];
                match p["topic"].as_str() {
                    Some("chat") => println!("{}", line(&p["data"])),
                    Some("chat_state") => println!("  #{} on {}: {}", p["data"]["id"], p["data"]["portId"].as_str().unwrap_or(""), p["data"]["txState"].as_str().unwrap_or("")),
                    Some(topic) => println!("  [{topic}] {}", p["data"]),
                    None => {}
                }
            }
        }
        _ => die("usage: aether-cli [--api ADDR] [--token-file PATH] ports|stations|send PORT DST TEXT|history [PORT] [N]|ping PORT CALL|version PORT CALL|tail"),
    }
}
