//! Local API: JSON-RPC 2.0 over WebSocket (subset of `spec/api/aether-api-v0`).
//!
//! The first request on a connection must be `system.hello` with the token
//! from the token file. Events are JSON-RPC notifications:
//! `{"jsonrpc":"2.0","method":"event","params":{"cursor":N,"topic":"chat","data":{...}}}`.

use std::net::SocketAddr;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

use crate::engine::{ApiError, Cmd, EngineHandle, MsgCmd, E_INVALID};

pub const API_VERSION: &str = "0.1.0";
const E_METHOD: i64 = -32601;
const E_PARSE: i64 = -32700;
const E_UNAUTH: i64 = 1401;
const E_VERSION: i64 = 1426;

/// Bind and serve in the background; returns the bound address.
pub async fn serve(
    listen: &str,
    token: String,
    engine: EngineHandle,
) -> std::io::Result<SocketAddr> {
    let l = TcpListener::bind(listen).await?;
    let addr = l.local_addr()?;
    tokio::spawn(async move {
        while let Ok((sock, _)) = l.accept().await {
            let (token, engine) = (token.clone(), engine.clone());
            tokio::spawn(async move {
                if let Ok(ws) = tokio_tungstenite::accept_async(sock).await {
                    session(ws, token, engine).await;
                }
            });
        }
    });
    Ok(addr)
}

fn ok(id: &Value, result: Value) -> Message {
    Message::text(json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string())
}

fn err(id: &Value, e: ApiError) -> Message {
    Message::text(
        json!({ "jsonrpc": "2.0", "id": id, "error": { "code": e.code, "message": e.message } })
            .to_string(),
    )
}

fn s(params: &Value, k: &str) -> Result<String, ApiError> {
    params
        .get(k)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| ApiError::new(E_INVALID, format!("missing string parameter '{k}'")))
}

fn opt_s(params: &Value, k: &str) -> Option<String> {
    params.get(k).and_then(Value::as_str).map(str::to_string)
}

fn obj(params: &Value, k: &str) -> Result<Option<serde_json::Map<String, Value>>, ApiError> {
    match params.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(m)) => Ok(Some(m.clone())),
        Some(_) => Err(ApiError::new(E_INVALID, format!("'{k}' must be an object"))),
    }
}

fn msg_cmd(method: &str, params: &Value) -> Result<MsgCmd, ApiError> {
    Ok(match method {
        "forms.listTemplates" => MsgCmd::Templates,
        "messages.list" => MsgCmd::List {
            folder: opt_s(params, "folder").filter(|f| f != "all"),
            limit: params.get("limit").and_then(Value::as_u64).unwrap_or(100) as u32,
            offset: params.get("offset").and_then(Value::as_u64).unwrap_or(0) as u32,
        },
        "messages.get" => MsgCmd::Get {
            id: s(params, "id")?,
        },
        "messages.events" => MsgCmd::Events {
            id: s(params, "id")?,
        },
        "messages.createDraft" => MsgCmd::CreateDraft {
            form_id: s(params, "formId")?,
            fields: obj(params, "fields")?.unwrap_or_default(),
            dst: opt_s(params, "dst"),
        },
        "messages.updateDraft" => MsgCmd::UpdateDraft {
            id: s(params, "id")?,
            fields: obj(params, "fields")?,
            dst: opt_s(params, "dst"),
        },
        "messages.send" | "messages.retry" => MsgCmd::Send {
            id: s(params, "id")?,
            port: opt_s(params, "portId"),
        },
        "messages.cancel" => MsgCmd::Cancel {
            id: s(params, "id")?,
        },
        _ => {
            return Err(ApiError::new(
                E_METHOD,
                format!("method not found: {method}"),
            ))
        }
    })
}

async fn ask(
    engine: &EngineHandle,
    make: impl FnOnce(oneshot::Sender<Result<Value, ApiError>>) -> Cmd,
) -> Result<Value, ApiError> {
    let (tx, rx) = oneshot::channel();
    engine
        .cmd
        .send(make(tx))
        .await
        .map_err(|_| ApiError::new(-32603, "engine stopped"))?;
    rx.await
        .map_err(|_| ApiError::new(-32603, "engine stopped"))?
}

async fn session(
    ws: tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    token: String,
    engine: EngineHandle,
) {
    let (mut sink, mut stream) = ws.split();
    let (out_tx, mut out_rx) = mpsc::channel::<Message>(1024);
    let writer = tokio::spawn(async move {
        while let Some(m) = out_rx.recv().await {
            if sink.send(m).await.is_err() {
                break;
            }
        }
    });
    let mut authed = false;
    let mut subscribed = false;
    while let Some(Ok(msg)) = stream.next().await {
        let text = match msg {
            Message::Text(t) => t.to_string(),
            Message::Close(_) => break,
            _ => continue,
        };
        let req: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => {
                let _ = out_tx
                    .send(err(&Value::Null, ApiError::new(E_PARSE, "parse error")))
                    .await;
                continue;
            }
        };
        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let method = req
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let params = req.get("params").cloned().unwrap_or(json!({}));
        if !authed && method != "system.hello" {
            let _ = out_tx
                .send(err(
                    &id,
                    ApiError::new(E_UNAUTH, "call system.hello with the token first"),
                ))
                .await;
            continue;
        }
        let result: Result<Value, ApiError> = match method.as_str() {
            "system.hello" => {
                let client_major = params
                    .get("apiVersion")
                    .and_then(Value::as_str)
                    .and_then(|v| v.split('.').next().map(str::to_string));
                if params.get("token").and_then(Value::as_str) != Some(token.as_str()) {
                    Err(ApiError::new(E_UNAUTH, "bad token"))
                } else if client_major.as_deref() != API_VERSION.split('.').next() {
                    Err(ApiError::new(
                        E_VERSION,
                        format!("API major version mismatch: engine {API_VERSION}"),
                    ))
                } else {
                    authed = true;
                    ask(&engine, Cmd::Hello).await
                }
            }
            "ports.list" => ask(&engine, Cmd::Ports).await,
            "stations.list" => ask(&engine, Cmd::Stations).await,
            "chat.send" => match (s(&params, "portId"), s(&params, "dst"), s(&params, "body")) {
                (Ok(port), Ok(dst), Ok(body)) => {
                    ask(&engine, |reply| Cmd::ChatSend {
                        port,
                        dst,
                        body,
                        reply,
                    })
                    .await
                }
                (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => Err(e),
            },
            "chat.history" => {
                let port = params
                    .get("portId")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(100) as u32;
                ask(&engine, |reply| Cmd::ChatHistory { port, limit, reply }).await
            }
            "stations.ping" => match (s(&params, "portId"), s(&params, "callsign")) {
                (Ok(port), Ok(callsign)) => {
                    ask(&engine, |reply| Cmd::Ping {
                        port,
                        callsign,
                        reply,
                    })
                    .await
                }
                (Err(e), _) | (_, Err(e)) => Err(e),
            },
            "stations.queryCapabilities" => match (s(&params, "portId"), s(&params, "callsign")) {
                (Ok(port), Ok(callsign)) => {
                    ask(&engine, |reply| Cmd::QueryCapabilities {
                        port,
                        callsign,
                        reply,
                    })
                    .await
                }
                (Err(e), _) | (_, Err(e)) => Err(e),
            },
            "transfers.sendFile" => {
                match (s(&params, "portId"), s(&params, "peer"), s(&params, "path")) {
                    (Ok(port), Ok(peer), Ok(path)) => {
                        ask(&engine, |reply| Cmd::SendFile {
                            port,
                            peer,
                            path: path.into(),
                            reply,
                        })
                        .await
                    }
                    (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => Err(e),
                }
            }
            "transfers.list" => {
                let include_finished = params
                    .get("includeFinished")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                ask(&engine, |reply| Cmd::Transfers {
                    include_finished,
                    reply,
                })
                .await
            }
            "transfers.cancel" => match s(&params, "id") {
                Ok(id) => ask(&engine, |reply| Cmd::CancelTransfer { id, reply }).await,
                Err(e) => Err(e),
            },
            m if m.starts_with("messages.") || m == "forms.listTemplates" => {
                match msg_cmd(m, &params) {
                    Ok(c) => ask(&engine, |reply| Cmd::Message(c, reply)).await,
                    Err(e) => Err(e),
                }
            }
            "events.subscribe" => {
                let topics: Vec<String> = params
                    .get("topics")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|t| t.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                if !subscribed {
                    subscribed = true;
                    let mut rx = engine.events.subscribe();
                    let tx = out_tx.clone();
                    tokio::spawn(async move {
                        loop {
                            match rx.recv().await {
                                Ok(ev) => {
                                    if !topics.is_empty() && !topics.iter().any(|t| t == ev.topic) {
                                        continue;
                                    }
                                    let n = json!({ "jsonrpc": "2.0", "method": "event", "params": ev });
                                    if tx.send(Message::text(n.to_string())).await.is_err() {
                                        break;
                                    }
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                    let n = json!({ "jsonrpc": "2.0", "method": "event", "params": { "topic": "lagged", "data": { "missed": n } } });
                                    let _ = tx.send(Message::text(n.to_string())).await;
                                }
                                Err(_) => break,
                            }
                        }
                    });
                }
                Ok(json!({ "subscribed": true }))
            }
            _ => Err(ApiError::new(
                E_METHOD,
                format!("method not found: {method}"),
            )),
        };
        let reply = match result {
            Ok(v) => ok(&id, v),
            Err(e) => err(&id, e),
        };
        if out_tx.send(reply).await.is_err() {
            break;
        }
    }
    writer.abort();
}
