//! Blocking client for the local API, used by `aetherdv-cli` and
//! `aetherdv-bench`. Event notifications that arrive while waiting for a
//! reply are kept, in order, for [`Client::next_event`].

use std::collections::VecDeque;
use std::net::TcpStream;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tungstenite::{connect, stream::MaybeTlsStream, Message, WebSocket};

pub struct Client {
    ws: WebSocket<MaybeTlsStream<TcpStream>>,
    next: u64,
    events: VecDeque<Value>,
    pub hello: Value,
}

#[derive(Debug, Clone)]
pub struct CallError {
    pub code: i64,
    pub message: String,
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "error {}: {}", self.code, self.message)
    }
}

impl Client {
    /// Connect to `addr` (host:port) and authenticate with `token`.
    pub fn connect(addr: &str, token: &str, client_name: &str) -> Result<Client, String> {
        let (ws, _) = connect(format!("ws://{addr}"))
            .map_err(|e| format!("cannot connect to aetherdv at {addr}: {e}"))?;
        let mut c = Client {
            ws,
            next: 0,
            events: VecDeque::new(),
            hello: Value::Null,
        };
        c.hello = c
            .call(
                "system.hello",
                json!({ "clientName": client_name, "apiVersion": crate::api::API_VERSION, "token": token }),
            )
            .map_err(|e| e.to_string())?;
        Ok(c)
    }

    fn set_timeout(&mut self, t: Option<Duration>) {
        if let MaybeTlsStream::Plain(s) = self.ws.get_mut() {
            let _ = s.set_read_timeout(t);
        }
    }

    /// Read one message; `Ok(None)` on timeout.
    fn read(&mut self) -> Result<Option<Value>, CallError> {
        match self.ws.read() {
            Ok(Message::Text(t)) => Ok(serde_json::from_str(&t).ok()),
            Ok(_) => Ok(None),
            Err(tungstenite::Error::Io(e))
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                Ok(None)
            }
            Err(e) => Err(CallError {
                code: -32000,
                message: format!("connection: {e}"),
            }),
        }
    }

    pub fn call(&mut self, method: &str, params: Value) -> Result<Value, CallError> {
        self.next += 1;
        let id = self.next;
        let req = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        self.set_timeout(None);
        self.ws
            .send(Message::text(req.to_string()))
            .map_err(|e| CallError {
                code: -32000,
                message: format!("send: {e}"),
            })?;
        loop {
            let Some(v) = self.read()? else { continue };
            if v.get("id") == Some(&json!(id)) {
                if let Some(e) = v.get("error") {
                    return Err(CallError {
                        code: e["code"].as_i64().unwrap_or(0),
                        message: e["message"].as_str().unwrap_or("").to_string(),
                    });
                }
                return Ok(v["result"].clone());
            }
            if v.get("method").and_then(Value::as_str) == Some("event") {
                self.events.push_back(v["params"].clone());
            }
        }
    }

    /// The next event (`{cursor, topic, data}`), waiting up to `timeout`.
    pub fn next_event(&mut self, timeout: Duration) -> Result<Option<Value>, CallError> {
        if let Some(e) = self.events.pop_front() {
            return Ok(Some(e));
        }
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            self.set_timeout(Some(left.max(Duration::from_millis(1))));
            if let Some(v) = self.read()? {
                if v.get("method").and_then(Value::as_str) == Some("event") {
                    return Ok(Some(v["params"].clone()));
                }
            }
        }
    }

    /// Wait for an event matching `pred`; other events are dropped.
    pub fn wait_event(
        &mut self,
        timeout: Duration,
        pred: impl Fn(&str, &Value) -> bool,
    ) -> Result<Option<Value>, CallError> {
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.next_event(left)? {
                None => return Ok(None),
                Some(e) => {
                    if pred(e["topic"].as_str().unwrap_or(""), &e["data"]) {
                        return Ok(Some(e["data"].clone()));
                    }
                }
            }
        }
    }

    /// Drop every event received so far, including any still unread on
    /// the connection.
    pub fn drain_events(&mut self) -> Result<(), CallError> {
        self.events.clear();
        while self.next_event(Duration::from_millis(5))?.is_some() {}
        Ok(())
    }
}
