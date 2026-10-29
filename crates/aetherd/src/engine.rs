//! Engine runtime: one task owns the store and protocol state; port drivers
//! and API sessions talk to it through channels.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aether_ddt2::{wrap, EnvelopeError, EscapeSet, Frame, PayloadEncoding};
use aether_link::legacy_arq::SenderConfig;
use aether_link::scheduler::Priority;
use aether_store::{ChatLine, Direction, PortRow, Store, TransferRow};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::config::{Config, PortConfig};
use crate::ports::{run_ratflector, run_serial, PortCmd, PortEvt, RflParams, SerialParams};
use crate::station::{Effect, Station};
use crate::xfer::{self, Class, Dir, Snapshot, Transfers, XEffect};

mod messages;
pub use messages::MsgCmd;

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
    SendFile {
        port: String,
        peer: String,
        path: PathBuf,
        reply: Reply<Value>,
    },
    /// `messages.*` and `forms.*`.
    Message(MsgCmd, Reply<Value>),
    Transfers {
        include_finished: bool,
        reply: Reply<Value>,
    },
    CancelTransfer {
        id: String,
        reply: Reply<Value>,
    },
}

/// Largest file accepted by `transfers.sendFile` (legacy transfers of this
/// size already take hours on DV slow data).
pub const MAX_SEND_BYTES: u64 = 16 << 20;

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

fn transfer_json(t: &TransferRow) -> Value {
    json!({
        "id": t.id, "direction": t.direction, "protocol": t.protocol, "peer": t.peer,
        "portId": t.port_id, "filename": t.filename, "totalBytes": t.total_bytes,
        "doneBytes": t.done_bytes, "blockSize": t.block_size, "state": t.state,
        "stateReason": t.state_reason, "priority": 50, "retries": t.retries,
        "rateBps": null, "wrapWarning": t.wrap_warning,
    })
}

/// ARQ-1 sender parameters per path. Blocks start at 256 B on DV slow data
/// and 512 B on Fast Data (SM-4); every choice stays within the legacy
/// 4 KiB in-flight cap. The RTO starts near the legacy 12 s floor on RF so a
/// lost offer is re-polled before a D-RATS receiver's ~20 s start wait ends.
pub fn arq_config(path_kind: &str) -> SenderConfig {
    let mut c = SenderConfig::legacy_defaults();
    match path_kind {
        "internet" => {
            // No air time: writes reach the socket at once, so an ACK can
            // legitimately arrive within milliseconds. A non-zero estimate
            // here would make the stale-ACK guard (P20) treat every fresh
            // ACK as stale and stall each window until the RTO.
            c.block_size = 1024;
            c.path_rate_bps = u32::MAX;
            c.per_over_ms = 0;
            c.rto_initial = 5_000;
            c.rto_min = 1_000;
        }
        "rf_fast" => {
            c.block_size = 512;
            c.path_rate_bps = 350;
            c.rto_initial = 12_000;
        }
        _ => {
            c.block_size = 256;
            c.rto_initial = 12_000;
        }
    }
    c
}

/// Per-transfer bookkeeping the pure state machine doesn't need.
struct XferMeta {
    id: String,
    created_at: i64,
    sha256: Option<String>,
    /// The message this form transfer carries.
    message_id: Option<String>,
    /// Whether the message has been moved to `sending` for this transfer.
    message_sending: bool,
}

impl XferMeta {
    fn new(message_id: Option<String>) -> Self {
        XferMeta {
            id: uuid::Uuid::now_v7().to_string(),
            created_at: unix_ms(),
            sha256: None,
            message_id,
            message_sending: false,
        }
    }
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
    xfers: Transfers,
    xfer_meta: HashMap<u64, XferMeta>,
    files_dir: PathBuf,
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
            let at = unix_ms();
            let id = self.store.log_event(at, level, "engine", port, msg).ok();
            self.publish(
                "engine.event",
                json!({ "id": id, "at": at, "level": level, "category": "engine", "portId": port, "message": msg }),
            );
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
                    "port.state",
                    json!({ "portId": port, "state": state, "detail": detail }),
                );
            }
            PortEvt::Frame { port, frame } => {
                let now = self.now();
                let effects = self.station.on_frame(now, &port, &frame);
                let looped = frame
                    .src
                    .eq_ignore_ascii_case(self.station.callsign.as_bytes());
                for eff in effects {
                    self.apply(eff).await;
                }
                if !looped {
                    let path_kind = self
                        .ports
                        .get(&port)
                        .map(|p| p.path_kind)
                        .unwrap_or("rf_slow");
                    let effs = self
                        .xfers
                        .on_frame(now, &port, &frame, || arq_config(path_kind));
                    self.apply_xfer(effs).await;
                }
            }
            PortEvt::Outside { port, bytes } => {
                self.log(
                    "debug",
                    Some(&port),
                    &format!("{} bytes outside frames", bytes.len()),
                );
            }
            PortEvt::Transmitted { port: _, token } => self.chat_state(token, "transmitted", None),
            PortEvt::NotTransmitted {
                port: _,
                token,
                reason,
            } => self.chat_state(token, "failed", Some(reason)),
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
                        self.publish("chat.line", chat_json(&line));
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
                    "station.heard",
                    json!({ "callsign": callsign, "portId": port, "pathKind": path_kind }),
                );
            }
            Effect::Software { callsign, text } => {
                let _ = self.store.station_software(&callsign, &text);
                self.publish(
                    "station.heard",
                    json!({ "callsign": callsign, "software": text }),
                );
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
            Cmd::SendFile {
                port,
                peer,
                path,
                reply,
            } => {
                let res = match std::fs::metadata(&path) {
                    Err(e) => Err(ApiError::new(
                        E_NOT_FOUND,
                        format!("{}: {e}", path.display()),
                    )),
                    Ok(m) if !m.is_file() => Err(ApiError::new(E_INVALID, "not a regular file")),
                    Ok(m) if m.len() > MAX_SEND_BYTES => Err(ApiError::new(
                        E_INVALID,
                        format!("file is larger than the {MAX_SEND_BYTES}-byte limit"),
                    )),
                    Ok(_) => match std::fs::read(&path) {
                        Err(e) => Err(ApiError::new(E_INVALID, format!("{}: {e}", path.display()))),
                        Ok(data) => {
                            let name = path
                                .file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_default();
                            self.start_transfer(&port, &peer, Class::File, &name, &data, None)
                                .await
                        }
                    },
                };
                let _ = reply.send(res);
            }
            Cmd::Message(m, reply) => {
                let res = self.on_message_cmd(m).await;
                let _ = reply.send(res);
            }
            Cmd::Transfers {
                include_finished,
                reply,
            } => {
                let res = self
                    .store
                    .transfers(include_finished, 500)
                    .map(|v| Value::Array(v.iter().map(transfer_json).collect()))
                    .map_err(|e| ApiError::new(E_INTERNAL, e.to_string()));
                let _ = reply.send(res);
            }
            Cmd::CancelTransfer { id, reply } => {
                let key = self
                    .xfer_meta
                    .iter()
                    .find(|(_, m)| m.id == id)
                    .map(|(k, _)| *k);
                let res = match key.and_then(|k| self.xfers.cancel(k)) {
                    Some(effs) => {
                        self.apply_xfer(effs).await;
                        Ok(json!({ "id": id, "state": "failed", "stateReason": "cancelled" }))
                    }
                    None => Err(ApiError::new(
                        E_NOT_FOUND,
                        format!("no active transfer {id}"),
                    )),
                };
                let _ = reply.send(res);
            }
        }
    }

    fn now(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    /// An outgoing chat line changed state: republish the whole line
    /// (`chat.line` consumers upsert by id).
    fn chat_state(&mut self, id: i64, state: &str, reason: Option<String>) {
        let _ = self.store.set_chat_tx_state(id, state);
        if let Some(reason) = reason {
            self.log(
                "warn",
                None,
                &format!("chat line {id} not transmitted: {reason}"),
            );
        }
        if let Ok(Some(line)) = self.store.chat_line(id) {
            self.publish("chat.line", chat_json(&line));
        }
    }

    async fn tick(&mut self) {
        let effs = self.xfers.poll(self.now());
        self.apply_xfer(effs).await;
        self.xfers.reap();
    }

    fn transfer_row(&mut self, s: &Snapshot) -> TransferRow {
        let now = unix_ms();
        let meta = self
            .xfer_meta
            .entry(s.key)
            .or_insert_with(|| XferMeta::new(None));
        TransferRow {
            id: meta.id.clone(),
            message_id: meta.message_id.clone(),
            direction: if s.dir == Dir::Send {
                "send"
            } else {
                "receive"
            }
            .into(),
            protocol: s.class.protocol().into(),
            peer: s.peer.clone(),
            port_id: Some(s.port.clone()),
            filename: s.name.clone(),
            sha256: meta.sha256.clone(),
            total_bytes: s.wire_total as i64,
            wire_bytes: Some(s.wire_total as i64),
            block_size: s.block_size as i64,
            done_bytes: s.wire_done as i64,
            state: s.state.into(),
            state_reason: s.reason.clone(),
            retries: s.retries as i64,
            wrap_warning: s.wrap_warning,
            created_at: meta.created_at,
            updated_at: now,
        }
    }

    async fn apply_xfer(&mut self, effs: Vec<XEffect>) {
        for e in effs {
            match e {
                XEffect::Send {
                    port,
                    frame,
                    encoding,
                    priority,
                } => {
                    if let Err(err) = self
                        .send_frame(&port, &frame, encoding, priority, None)
                        .await
                    {
                        self.log(
                            "warn",
                            Some(&port),
                            &format!("transfer frame not sent: {}", err.message),
                        );
                    }
                }
                XEffect::Update(s) => {
                    let row = self.transfer_row(&s);
                    if let Err(err) = self.store.upsert_transfer(&row) {
                        self.log("error", None, &format!("store: {err}"));
                    }
                    if let Some(mid) = row.message_id.clone() {
                        self.on_message_transfer(&mid, &s);
                    }
                    if matches!(s.state, "done" | "failed") {
                        let verb = match (s.dir, s.state) {
                            (Dir::Send, "done") => "sent",
                            (Dir::Receive, "done") => "received",
                            _ => "failed",
                        };
                        let detail = s
                            .reason
                            .as_deref()
                            .map(|r| format!(" ({r})"))
                            .unwrap_or_default();
                        let level = if s.state == "failed" { "warn" } else { "info" };
                        self.log(
                            level,
                            Some(&s.port),
                            &format!(
                                "transfer {} {verb}: {} with {}{detail}",
                                row.id, s.name, s.peer
                            ),
                        );
                        self.xfer_meta.remove(&s.key);
                    }
                    self.publish("transfer.progress", transfer_json(&row));
                }
                XEffect::Received {
                    key,
                    class,
                    port,
                    peer,
                    name,
                    data,
                } => {
                    let sha = Sha256::digest(&data)
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect::<String>();
                    if let Some(m) = self.xfer_meta.get_mut(&key) {
                        m.sha256 = Some(sha);
                    }
                    if class == Class::Form {
                        self.on_form_received(&port, &peer, &data);
                        continue;
                    }
                    match crate::inbox::save_unique(&self.files_dir, &name, &data) {
                        Ok(path) => self.log(
                            "info",
                            Some(&port),
                            &format!(
                                "saved {name} from {peer} ({} bytes) to {}",
                                data.len(),
                                path.display()
                            ),
                        ),
                        Err(err) => self.log(
                            "error",
                            Some(&port),
                            &format!(
                                "cannot save {name} from {peer} in {}: {err}",
                                self.files_dir.display()
                            ),
                        ),
                    }
                }
                XEffect::Log { level, message } => self.log(level, None, &message),
            }
        }
    }

    fn check_peer(&self, port: &str, peer: &str) -> Result<String, ApiError> {
        let peer = peer.trim().to_ascii_uppercase();
        if !self.ports.contains_key(port) {
            return Err(ApiError::new(E_NOT_FOUND, format!("no port {port}")));
        }
        if peer == self.station.callsign {
            return Err(ApiError::new(E_SELF, "self-addressed traffic refused"));
        }
        if peer.len() < 3 || peer.len() > 8 || peer == aether_ddt2::session::CQCQCQ {
            return Err(ApiError::new(
                E_INVALID,
                "a transfer needs one station's callsign (3-8 characters)",
            ));
        }
        Ok(peer)
    }

    async fn start_transfer(
        &mut self,
        port: &str,
        peer: &str,
        class: Class,
        name: &str,
        content: &[u8],
        message_id: Option<String>,
    ) -> Result<Value, ApiError> {
        let peer = self.check_peer(port, peer)?;
        let cfg = arq_config(self.ports[port].path_kind);
        let now = self.now();
        let (key, effs) = self
            .xfers
            .start_send(
                now,
                xfer::SendRequest {
                    port,
                    peer: &peer,
                    class,
                    name,
                    content,
                    cfg,
                },
            )
            .map_err(|e| ApiError::new(E_INVALID, e))?;
        self.xfer_meta.insert(key, XferMeta::new(message_id));
        self.apply_xfer(effs).await;
        let id = self
            .xfer_meta
            .get(&key)
            .map(|m| m.id.clone())
            .unwrap_or_default();
        Ok(self
            .store
            .transfers(true, 50)
            .ok()
            .and_then(|v| v.into_iter().find(|t| t.id == id))
            .map(|t| transfer_json(&t))
            .unwrap_or(json!({ "id": id })))
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
        self.publish("chat.line", v.clone());
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
    let files_dir = cfg.files_dir.clone().unwrap_or_else(|| match &cfg.db {
        Some(db) => db.with_file_name("received"),
        None => std::env::temp_dir().join("aether-received"),
    });
    let limits = xfer::Limits {
        max_wire_bytes: cfg.max_incoming_kib.saturating_mul(1024),
        ..xfer::Limits::default()
    };
    let mut engine = Engine {
        xfers: Transfers::new(&cfg.callsign, limits),
        xfer_meta: HashMap::new(),
        files_dir,
        station: Station::new(&cfg.callsign, seed),
        store,
        ports,
        events: events.clone(),
        cursor: 0,
        started: Instant::now(),
        rpc_job: 0,
    };
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(200));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = tick.tick() => engine.tick().await,
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
