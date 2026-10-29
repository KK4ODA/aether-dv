//! Engine runtime: one task owns the store and protocol state; port drivers
//! and API sessions talk to it through channels.

use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aether_ddt2::{wrap, EnvelopeError, EscapeSet, Frame, PayloadEncoding};
use aether_link::scheduler::Priority;
use aether_store::{ChatLine, Direction, PortRow, Store};
use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::config::{Config, PortConfig};
use crate::ports::{run_ratflector, run_serial, PortCmd, PortEvt, RflParams, SerialParams};
use crate::station::{Effect, Station};

pub type Reply<T> = oneshot::Sender<Result<T, ApiError>>;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ApiError {
    pub code: i64,
    pub message: String,
}

impl ApiError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        ApiError {
            code,
            message: message.into(),
        }
    }
}

pub const E_NOT_FOUND: i64 = 1404;
pub const E_SELF: i64 = 1422;
pub const E_INVALID: i64 = -32602;
pub const E_INTERNAL: i64 = -32603;

#[derive(Debug)]
pub enum Cmd {
    Hello(Reply<Value>),
    Ports(Reply<Value>),
    ChatSend {
        port: String,
        dst: String,
        body: String,
        reply: Reply<Value>,
    },
    ChatHistory {
        port: Option<String>,
        limit: u32,
        reply: Reply<Value>,
    },
    Stations(Reply<Value>),
    Ping {
        port: String,
        callsign: String,
        reply: Reply<Value>,
    },
    QueryCapabilities {
        port: String,
        callsign: String,
        reply: Reply<Value>,
    },
}

/// Event pushed to API subscribers.
#[derive(Debug, Clone, Serialize)]
pub struct ApiEvent {
    pub cursor: u64,
    pub topic: &'static str,
    pub data: Value,
}

#[derive(Clone)]
pub struct EngineHandle {
    pub cmd: mpsc::Sender<Cmd>,
    pub events: broadcast::Sender<ApiEvent>,
}

struct PortState {
    kind: &'static str,
    path_kind: &'static str,
    state: &'static str,
    detail: Option<String>,
    tx: mpsc::Sender<PortCmd>,
}

fn unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn chat_json(c: &ChatLine) -> Value {
    json!({
        "id": c.id, "portId": c.port_id, "direction": if c.direction == Direction::In { "in" } else { "out" },
        "src": c.src, "dst": c.dst, "kind": c.kind, "body": c.body, "at": c.at, "txState": c.tx_state,
    })
}

/// Encode a frame for the wire; if the escaped bytes would contain an
/// envelope marker (legacy defect P2), retry compressed (legacy receivers
/// accept compressed frames on every session).
fn to_wire(f: &Frame, enc: PayloadEncoding) -> Result<Vec<u8>, ApiError> {
    let try_enc = |e| {
        let raw = f
            .encode(e)
            .map_err(|err| ApiError::new(E_INVALID, format!("cannot encode: {err}")))?;
        Ok::<_, ApiError>(wrap(&raw, &EscapeSet::legacy()))
    };
    match try_enc(enc)? {
        Ok(w) => Ok(w),
        Err(EnvelopeError::PoisonMarker) => try_enc(PayloadEncoding::Always)?.map_err(|_| {
            ApiError::new(
                E_INVALID,
                "text contains a D-RATS frame marker ([SOB]/[EOB]); please rephrase",
            )
        }),
    }
}

struct Engine {
    station: Station,
    store: Store,
    ports: HashMap<String, PortState>,
    events: broadcast::Sender<ApiEvent>,
    cursor: u64,
    started: Instant,
    rpc_job: u16,
}

impl Engine {
    fn publish(&mut self, topic: &'static str, data: Value) {
        self.cursor += 1;
        let _ = self.events.send(ApiEvent {
            cursor: self.cursor,
            topic,
            data,
        });
    }

    fn log(&mut self, level: &str, port: Option<&str>, msg: &str) {
        match level {
            "warn" | "error" => tracing::warn!(port, "{msg}"),
            "debug" => tracing::debug!(port, "{msg}"),
            _ => tracing::info!(port, "{msg}"),
        }
        if level != "debug" {
            let _ = self.store.log_event(unix_ms(), level, "engine", port, msg);
        }
    }

    async fn send_frame(
        &mut self,
        port: &str,
        f: &Frame,
        enc: PayloadEncoding,
        prio: Priority,
        token: Option<i64>,
    ) -> Result<(), ApiError> {
        let bytes = to_wire(f, enc)?;
        let p = self
            .ports
            .get(port)
            .ok_or_else(|| ApiError::new(E_NOT_FOUND, format!("no port {port}")))?;
        p.tx.send(PortCmd::Send {
            bytes,
            priority: prio,
            token,
        })
        .await
        .map_err(|_| ApiError::new(E_INTERNAL, "port stopped"))
    }

    async fn on_port_evt(&mut self, e: PortEvt) {
        match e {
            PortEvt::State {
                port,
                state,
                detail,
            } => {
                if let Some(p) = self.ports.get_mut(&port) {
                    p.state = state;
                    p.detail = detail.clone();
                }
                self.log(
                    "info",
                    Some(&port),
                    &format!(
                        "{state}{}",
                        detail
                            .as_deref()
                            .map(|d| format!(": {d}"))
                            .unwrap_or_default()
                    ),
                );
                self.publish(
                    "port",
                    json!({ "portId": port, "state": state, "detail": detail }),
                );
            }
            PortEvt::Frame { port, frame } => {
                let now = self.started.elapsed().as_millis() as u64;
                let effects = self.station.on_frame(now, &port, &frame);
                for eff in effects {
                    self.apply(eff).await;
                }
            }
            PortEvt::Outside { port, bytes } => {
                self.log(
                    "debug",
                    Some(&port),
                    &format!("{} bytes outside frames", bytes.len()),
                );
            }
            PortEvt::Transmitted { port, token } => {
                let _ = self.store.set_chat_tx_state(token, "transmitted");
                self.publish(
                    "chat_state",
                    json!({ "id": token, "portId": port, "txState": "transmitted" }),
                );
            }
            PortEvt::NotTransmitted {
                port,
                token,
                reason,
            } => {
                let _ = self.store.set_chat_tx_state(token, "failed");
                self.publish(
                    "chat_state",
                    json!({ "id": token, "portId": port, "txState": "failed", "reason": reason }),
                );
            }
        }
    }

    async fn apply(&mut self, eff: Effect) {
        match eff {
            Effect::Send {
                port,
                frame,
                encoding,
                delay_ms,
            } => {
                let prio = if frame.session == aether_ddt2::session::id::CHAT {
                    Priority::Chat
                } else {
                    Priority::Control
                };
                if delay_ms == 0 {
                    let _ = self.send_frame(&port, &frame, encoding, prio, None).await;
                } else if let (Some(p), Ok(bytes)) =
                    (self.ports.get(&port), to_wire(&frame, encoding))
                {
                    let tx = p.tx.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                        let _ = tx
                            .send(PortCmd::Send {
                                bytes,
                                priority: prio,
                                token: None,
                            })
                            .await;
                    });
                }
            }
            Effect::Chat {
                port,
                src,
                dst,
                kind,
                body,
            } => {
                let mut line = ChatLine {
                    id: 0,
                    port_id: Some(port),
                    direction: Direction::In,
                    src,
                    dst,
                    kind: kind.into(),
                    body,
                    at: unix_ms(),
                    tx_state: None,
                    frame_crc: None,
                };
                match self.store.insert_chat(&line) {
                    Ok(id) => {
                        line.id = id;
                        self.publish("chat", chat_json(&line));
                    }
                    Err(e) => self.log("error", None, &format!("store: {e}")),
                }
            }
            Effect::Heard { port, callsign } => {
                let path_kind = self
                    .ports
                    .get(&port)
                    .map(|p| p.path_kind)
                    .unwrap_or("rf_unknown");
                let _ = self
                    .store
                    .station_heard(&callsign, &port, path_kind, unix_ms());
                self.publish(
                    "station",
                    json!({ "callsign": callsign, "portId": port, "pathKind": path_kind }),
                );
            }
            Effect::Software { callsign, text } => {
                let _ = self.store.station_software(&callsign, &text);
                self.publish("station", json!({ "callsign": callsign, "software": text }));
            }
            Effect::Log { level, message } => self.log(level, None, &message),
        }
    }

    async fn on_cmd(&mut self, c: Cmd) {
        match c {
            Cmd::Hello(r) => {
                let _ = r.send(Ok(json!({
                    "engineVersion": crate::station::VERSION, "apiVersion": crate::api::API_VERSION,
                    "profile": self.station.callsign, "uptimeMs": self.started.elapsed().as_millis() as u64,
                })));
            }
            Cmd::Ports(r) => {
                let mut v: Vec<Value> = self
                    .ports
                    .iter()
                    .map(|(name, p)| json!({ "id": name, "name": name, "kind": p.kind, "pathKind": p.path_kind, "state": p.state, "detail": p.detail }))
                    .collect();
                v.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
                let _ = r.send(Ok(Value::Array(v)));
            }
            Cmd::ChatSend {
                port,
                dst,
                body,
                reply,
            } => {
                let res = self.chat_send(port, dst, body).await;
                let _ = reply.send(res);
            }
            Cmd::ChatHistory { port, limit, reply } => {
                let res = self
                    .store
                    .chat_history(port.as_deref(), None, limit.min(1000))
                    .map(|v| Value::Array(v.iter().map(chat_json).collect()))
                    .map_err(|e| ApiError::new(E_INTERNAL, e.to_string()));
                let _ = reply.send(res);
            }
            Cmd::Stations(r) => {
                let res = self
                    .store
                    .stations()
                    .map(|v| Value::Array(v.into_iter().map(|(c, t, s)| json!({ "callsign": c, "lastHeard": t, "software": s })).collect()))
                    .map_err(|e| ApiError::new(E_INTERNAL, e.to_string()));
                let _ = r.send(res);
            }
            Cmd::Ping {
                port,
                callsign,
                reply,
            } => {
                let (f, enc) = self.station.ping_frame(&callsign.to_ascii_uppercase());
                let res = self
                    .send_frame(&port, &f, enc, Priority::Chat, None)
                    .await
                    .map(|_| json!(true));
                let _ = reply.send(res);
            }
            Cmd::QueryCapabilities {
                port,
                callsign,
                reply,
            } => {
                self.rpc_job = self.rpc_job.wrapping_add(1);
                let (f, enc) = self
                    .station
                    .version_request(&callsign.to_ascii_uppercase(), self.rpc_job);
                let res = self
                    .send_frame(&port, &f, enc, Priority::Control, None)
                    .await
                    .map(|_| json!(true));
                let _ = reply.send(res);
            }
        }
    }

    async fn chat_send(
        &mut self,
        port: String,
        dst: String,
        body: String,
    ) -> Result<Value, ApiError> {
        let dst = dst.trim().to_ascii_uppercase();
        if !self.ports.contains_key(&port) {
            return Err(ApiError::new(E_NOT_FOUND, format!("no port {port}")));
        }
        if dst == self.station.callsign {
            return Err(ApiError::new(E_SELF, "self-addressed traffic refused"));
        }
        if dst.is_empty() || dst.len() > 8 {
            return Err(ApiError::new(
                E_INVALID,
                "destination must be CQCQCQ or a callsign of up to 8 characters",
            ));
        }
        let (frame, enc) = self.station.chat_frame(&dst, &body);
        let bytes = to_wire(&frame, enc)?;
        let mut line = ChatLine {
            id: 0,
            port_id: Some(port.clone()),
            direction: Direction::Out,
            src: self.station.callsign.clone(),
            dst,
            kind: "text".into(),
            body,
            at: unix_ms(),
            tx_state: Some("queued".into()),
            frame_crc: None,
        };
        line.id = self
            .store
            .insert_chat(&line)
            .map_err(|e| ApiError::new(E_INTERNAL, e.to_string()))?;
        let tx = self.ports[&port].tx.clone();
        tx.send(PortCmd::Send {
            bytes,
            priority: Priority::Chat,
            token: Some(line.id),
        })
        .await
        .map_err(|_| ApiError::new(E_INTERNAL, "port stopped"))?;
        let v = chat_json(&line);
        self.publish("chat", v.clone());
        Ok(v)
    }
}

/// Start the engine and its ports. The API server is started separately.
pub fn start(cfg: &Config) -> Result<EngineHandle, String> {
    let store = Store::open(cfg.db.as_deref()).map_err(|e| format!("database: {e}"))?;
    let (evt_tx, mut evt_rx) = mpsc::channel::<PortEvt>(1024);
    let (cmd_tx, mut cmd_rx) = mpsc::channel::<Cmd>(256);
    let (events, _) = broadcast::channel::<ApiEvent>(1024);
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(1);
    let mut ports = HashMap::new();
    let now = unix_ms();
    for pc in &cfg.ports {
        let (tx, rx) = mpsc::channel::<PortCmd>(256);
        let (kind, path_kind, row_kind, profile) = match pc {
            PortConfig::Ratflector { .. } => ("ratflector", "internet", "ratflector", None),
            PortConfig::Serial { mode, profile, .. } => (
                "serial",
                if mode == "fast" { "rf_fast" } else { "rf_slow" },
                "radio_serial",
                Some(profile.clone()),
            ),
        };
        let row = PortRow {
            id: pc.name().into(),
            kind: row_kind.into(),
            name: pc.name().into(),
            enabled: true,
            profile_id: profile,
            device_ident: None,
            config_json: "{}".into(),
            mode_policy: if path_kind == "rf_fast" {
                "fast".into()
            } else {
                "slow".into()
            },
        };
        store
            .upsert_port(&row, now)
            .map_err(|e| format!("database: {e}"))?;
        match pc.clone() {
            PortConfig::Ratflector {
                name,
                host,
                tcp_port,
                password,
            } => {
                let params = RflParams {
                    name,
                    host,
                    tcp_port,
                    password,
                    callsign: cfg.callsign.clone(),
                };
                tokio::spawn(run_ratflector(params, rx, evt_tx.clone()));
            }
            PortConfig::Serial {
                name,
                path,
                baud,
                mode,
                flow,
                ..
            } => {
                let params = SerialParams {
                    name,
                    path,
                    baud,
                    mode,
                    software_flow: flow == "software",
                };
                tokio::spawn(run_serial(params, rx, evt_tx.clone()));
            }
        }
        ports.insert(
            pc.name().to_string(),
            PortState {
                kind,
                path_kind,
                state: "starting",
                detail: None,
                tx,
            },
        );
    }
    let mut engine = Engine {
        station: Station::new(&cfg.callsign, seed),
        store,
        ports,
        events: events.clone(),
        cursor: 0,
        started: Instant::now(),
        rpc_job: 0,
    };
    tokio::spawn(async move {
        loop {
            tokio::select! {
                Some(e) = evt_rx.recv() => engine.on_port_evt(e).await,
                c = cmd_rx.recv() => match c {
                    Some(c) => engine.on_cmd(c).await,
                    None => break,
                },
            }
        }
    });
    Ok(EngineHandle {
        cmd: cmd_tx,
        events,
    })
}
