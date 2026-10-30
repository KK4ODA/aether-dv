//! Messages: D-RATS form documents (ICS-213 first) carried by legacy form
//! transfers, with the delivery states of SM-7.
//!
//! This slice delivers directly to the destination station over one port;
//! there is no store-and-forward routing yet. With a legacy peer the best
//! outcome is `handed_off` (every block acknowledged, evidence `arq_ack`),
//! never `delivered`: D-RATS sends no end-to-end receipt, and the store
//! refuses `delivered` without one.

use aether_forms::Form;
use aether_store::{MessageEvent, MessageRow};
use serde_json::{Map, Value};

use super::*;

/// A `messages.*` / `forms.*` request.
#[derive(Debug)]
pub enum MsgCmd {
    Templates,
    List {
        folder: Option<String>,
        limit: u32,
        offset: u32,
    },
    Get {
        id: String,
    },
    Events {
        id: String,
    },
    CreateDraft {
        form_id: String,
        fields: Map<String, Value>,
        dst: Option<String>,
    },
    UpdateDraft {
        id: String,
        fields: Option<Map<String, Value>>,
        dst: Option<String>,
    },
    /// Send a draft, or retry a failed message. `port`: where to send it;
    /// default is the port the destination was last heard on.
    Send {
        id: String,
        port: Option<String>,
    },
    Cancel {
        id: String,
    },
}

pub(super) fn summary_json(m: &MessageRow) -> Value {
    json!({
        "id": m.id, "mid": m.mid, "folder": m.folder, "formId": m.form_id, "subject": m.subject,
        "src": m.src, "dst": m.dst, "priority": m.priority, "state": m.state,
        "stateReason": m.state_reason, "updatedAt": m.updated_at,
    })
}

fn message_json(m: &MessageRow) -> Value {
    let mut v = summary_json(m);
    let fields: Map<String, Value> = Form::parse(&m.xml)
        .map(|f| {
            f.fields
                .into_iter()
                .map(|x| (x.id, Value::String(x.value)))
                .collect()
        })
        .unwrap_or_default();
    v["xml"] = json!(m.xml);
    v["fields"] = Value::Object(fields);
    v["attachments"] = json!([]);
    v
}

fn event_json(e: &MessageEvent) -> Value {
    json!({
        "at": e.at, "state": e.state, "hop": e.hop, "portId": e.port_id, "pathKind": e.path_kind,
        "evidence": e.evidence,
        "detail": e.detail.as_deref().and_then(|d| serde_json::from_str::<Value>(d).ok()),
    })
}

/// Where a message state change happened.
#[derive(Clone, Copy)]
struct Via<'a> {
    port: &'a str,
    path_kind: &'a str,
    /// (next hop, evidence) when the message left our hands.
    handoff: Option<(&'a str, &'a str)>,
}

fn rand_u32() -> u32 {
    let mut b = [0u8; 4];
    getrandom::fill(&mut b).expect("OS random source");
    u32::from_le_bytes(b)
}

fn str_fields(fields: &Map<String, Value>) -> Result<Vec<(String, String)>, ApiError> {
    fields
        .iter()
        .map(|(k, v)| match v {
            Value::String(s) => Ok((k.clone(), s.clone())),
            Value::Number(n) => Ok((k.clone(), n.to_string())),
            _ => Err(ApiError::new(
                E_INVALID,
                format!("field '{k}' must be a string"),
            )),
        })
        .collect()
}

fn form_err(e: aether_forms::FormError) -> ApiError {
    ApiError::new(E_INVALID, e.to_string())
}

impl Engine {
    pub(super) async fn on_message_cmd(&mut self, c: MsgCmd) -> Result<Value, ApiError> {
        let internal = |e: aether_store::Error| ApiError::new(E_INTERNAL, e.to_string());
        match c {
            MsgCmd::Templates => Ok(Value::Array(
                aether_forms::TEMPLATES
                    .iter()
                    .filter_map(|(id, _)| Form::from_template(id).ok())
                    .map(|f| json!({ "id": f.id, "title": f.title, "version": "1" }))
                    .collect(),
            )),
            MsgCmd::List {
                folder,
                limit,
                offset,
            } => Ok(Value::Array(
                self.store
                    .messages(folder.as_deref(), limit.min(1000), offset)
                    .map_err(internal)?
                    .iter()
                    .map(summary_json)
                    .collect(),
            )),
            MsgCmd::Get { id } => self
                .store
                .message(&id)
                .map_err(internal)?
                .map(|m| message_json(&m))
                .ok_or_else(|| ApiError::new(E_NOT_FOUND, format!("no message {id}"))),
            MsgCmd::Events { id } => {
                if self.store.message(&id).map_err(internal)?.is_none() {
                    return Err(ApiError::new(E_NOT_FOUND, format!("no message {id}")));
                }
                Ok(Value::Array(
                    self.store
                        .message_events(&id)
                        .map_err(internal)?
                        .iter()
                        .map(event_json)
                        .collect(),
                ))
            }
            MsgCmd::CreateDraft {
                form_id,
                fields,
                dst,
            } => self.create_draft(&form_id, &fields, dst),
            MsgCmd::UpdateDraft { id, fields, dst } => self.update_draft(&id, fields, dst),
            MsgCmd::Send { id, port } => self.send_message(&id, port).await,
            MsgCmd::Cancel { id } => {
                let m = self
                    .store
                    .message(&id)
                    .map_err(internal)?
                    .ok_or_else(|| ApiError::new(E_NOT_FOUND, format!("no message {id}")))?;
                let key = self
                    .xfer_meta
                    .iter()
                    .find(|(_, x)| x.message_id.as_deref() == Some(id.as_str()))
                    .map(|(k, _)| *k);
                match key.and_then(|k| self.xfers.cancel(k)) {
                    Some(effs) => {
                        self.apply_xfer(effs).await;
                    }
                    None if m.state == "queued" => {
                        self.message_state(&id, "failed", None, Some("cancelled"), None);
                    }
                    None => {
                        return Err(ApiError::new(
                            E_INVALID,
                            format!("message is {}, nothing to cancel", m.state),
                        ))
                    }
                }
                self.store
                    .message(&id)
                    .map_err(internal)?
                    .map(|m| summary_json(&m))
                    .ok_or_else(|| ApiError::new(E_NOT_FOUND, "message vanished"))
            }
        }
    }

    fn check_dst(&self, dst: &str) -> Result<String, ApiError> {
        let dst = dst.trim().to_ascii_uppercase();
        if dst == self.station.callsign {
            return Err(ApiError::new(E_SELF, "self-addressed messages are refused"));
        }
        if dst.len() < 3
            || dst.len() > 8
            || dst == aether_ddt2::session::CQCQCQ
            || !dst
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '/')
        {
            return Err(ApiError::new(
                E_INVALID,
                "destination must be one station's callsign (multi-hop routes are not supported yet)",
            ));
        }
        Ok(dst)
    }

    fn create_draft(
        &mut self,
        form_id: &str,
        fields: &Map<String, Value>,
        dst: Option<String>,
    ) -> Result<Value, ApiError> {
        let dst = dst
            .filter(|d| !d.trim().is_empty())
            .map(|d| self.check_dst(&d))
            .transpose()?;
        let mut form = Form::from_template(form_id).map_err(form_err)?;
        let now_ms = unix_ms();
        let secs = now_ms / 1000;
        // formgui fills date and time widgets with the current time.
        for f in &mut form.fields {
            match f.kind.as_str() {
                "date" if f.id == "date" => f.value = aether_forms::legacy_date(secs),
                "time" if f.id == "time" => f.value = aether_forms::legacy_time(secs),
                _ => {}
            }
        }
        for (k, v) in str_fields(fields)? {
            form.set(&k, &v).map_err(form_err)?;
        }
        let me = self.station.callsign.clone();
        form.path.src = Some(me.clone());
        form.path.dst = dst.clone();
        form.path.mid = Some(aether_forms::message_id(&me, secs, rand_u32()));
        form.path.hops = vec![me.clone()];
        let m = MessageRow {
            id: uuid::Uuid::now_v7().to_string(),
            mid: form.path.mid.clone().unwrap_or_default(),
            folder: "drafts".into(),
            form_id: Some(form.id.clone()),
            subject: Some(form.subject()),
            src: me,
            dst: dst.unwrap_or_default(),
            priority: "routine".into(),
            xml: form.to_xml(),
            state: "draft".into(),
            state_reason: None,
            attempts: 0,
            created_at: now_ms,
            updated_at: now_ms,
        };
        let ev = MessageEvent {
            at: now_ms,
            state: "draft".into(),
            evidence: Some("operator".into()),
            ..MessageEvent::default()
        };
        self.store
            .insert_message(&m, &ev)
            .map_err(|e| ApiError::new(E_INTERNAL, e.to_string()))?;
        self.publish(
            "message.state",
            json!({ "message": summary_json(&m), "event": event_json(&ev) }),
        );
        Ok(message_json(&m))
    }

    fn update_draft(
        &mut self,
        id: &str,
        fields: Option<Map<String, Value>>,
        dst: Option<String>,
    ) -> Result<Value, ApiError> {
        let m = self
            .store
            .message(id)
            .map_err(|e| ApiError::new(E_INTERNAL, e.to_string()))?
            .ok_or_else(|| ApiError::new(E_NOT_FOUND, format!("no message {id}")))?;
        if m.state != "draft" {
            return Err(ApiError::new(
                E_INVALID,
                format!("message is {}, only drafts can be edited", m.state),
            ));
        }
        let mut form = Form::parse(&m.xml).map_err(form_err)?;
        if let Some(fields) = fields {
            for (k, v) in str_fields(&fields)? {
                form.set(&k, &v).map_err(form_err)?;
            }
        }
        let dst = match dst {
            Some(d) if !d.trim().is_empty() => self.check_dst(&d)?,
            Some(_) => String::new(),
            None => m.dst.clone(),
        };
        form.path.dst = (!dst.is_empty()).then(|| dst.clone());
        let xml = form.to_xml();
        self.store
            .update_draft(id, &xml, Some(&form.subject()), &dst, unix_ms())
            .map_err(|e| ApiError::new(E_INTERNAL, e.to_string()))?;
        let m = self
            .store
            .message(id)
            .map_err(|e| ApiError::new(E_INTERNAL, e.to_string()))?
            .ok_or_else(|| ApiError::new(E_NOT_FOUND, "message vanished"))?;
        Ok(message_json(&m))
    }

    async fn send_message(&mut self, id: &str, port: Option<String>) -> Result<Value, ApiError> {
        let internal = |e: aether_store::Error| ApiError::new(E_INTERNAL, e.to_string());
        let m = self
            .store
            .message(id)
            .map_err(internal)?
            .ok_or_else(|| ApiError::new(E_NOT_FOUND, format!("no message {id}")))?;
        if !matches!(m.state.as_str(), "draft" | "failed") {
            return Err(ApiError::new(
                E_INVALID,
                format!(
                    "message is {}; only drafts and failed messages can be sent",
                    m.state
                ),
            ));
        }
        if m.dst.is_empty() {
            return Err(ApiError::new(E_INVALID, "the message has no destination"));
        }
        let dst = self.check_dst(&m.dst)?;
        let port = match port {
            Some(p) => p,
            None => self
                .store
                .station_last_port(&dst)
                .map_err(internal)?
                .filter(|p| self.ports.contains_key(p))
                .ok_or_else(|| {
                    ApiError::new(
                        E_INVALID,
                        format!("{dst} has not been heard on any port; choose a port to send on"),
                    )
                })?,
        };
        if !self.ports.contains_key(&port) {
            return Err(ApiError::new(E_NOT_FOUND, format!("no port {port}")));
        }
        let form = Form::parse(&m.xml).map_err(form_err)?;
        // The session name is only shown in the peer's transfer list.
        let name: String = form
            .title
            .chars()
            .map(|c| if c == '/' || c == '\\' { '-' } else { c })
            .collect();
        let name = if name.trim().is_empty() {
            "Form".to_string()
        } else {
            name
        };
        let path_kind = self.ports[&port].path_kind;
        let retry = m.state == "failed";
        self.message_state(
            id,
            "queued",
            Some("outbox"),
            None,
            Some(Via {
                port: port.as_str(),
                path_kind,
                handoff: None,
            }),
        );
        if let Err(e) = self
            .start_transfer(
                &port,
                &dst,
                Class::Form,
                &name,
                m.xml.as_bytes(),
                Some(id.to_string()),
            )
            .await
        {
            self.message_state(id, "failed", None, Some(&e.message), None);
            return Err(e);
        }
        if retry {
            self.log(
                "info",
                Some(&port),
                &format!("retrying message {} to {dst}", m.mid),
            );
        }
        self.store
            .message(id)
            .map_err(internal)?
            .map(|m| summary_json(&m))
            .ok_or_else(|| ApiError::new(E_NOT_FOUND, "message vanished"))
    }

    /// Record and publish a message state change.
    fn message_state(
        &mut self,
        id: &str,
        state: &str,
        folder: Option<&str>,
        reason: Option<&str>,
        via: Option<Via<'_>>,
    ) {
        let ev = MessageEvent {
            at: unix_ms(),
            state: state.into(),
            port_id: via.map(|v| v.port.to_string()),
            path_kind: via.map(|v| v.path_kind.to_string()),
            hop: via.and_then(|v| v.handoff).map(|h| h.0.to_string()),
            evidence: via.and_then(|v| v.handoff).map(|h| h.1.to_string()),
            detail: reason.map(|r| json!({ "reason": r }).to_string()),
            ..MessageEvent::default()
        };
        if let Err(e) = self
            .store
            .set_message_state(id, folder, reason, state == "sending", &ev)
        {
            self.log("error", None, &format!("message {id}: {e}"));
            return;
        }
        if let Ok(Some(m)) = self.store.message(id) {
            self.publish(
                "message.state",
                json!({ "message": summary_json(&m), "event": event_json(&ev) }),
            );
        }
    }

    /// A form transfer carrying message `id` changed state.
    pub(super) fn on_message_transfer(&mut self, id: &str, s: &Snapshot) {
        let path_kind = self
            .ports
            .get(&s.port)
            .map(|p| p.path_kind)
            .unwrap_or("rf_slow");
        let first = self
            .xfer_meta
            .get_mut(&s.key)
            .map(|m| !std::mem::replace(&mut m.message_sending, true))
            .unwrap_or(false);
        match s.state {
            "done" => self.message_state(
                id,
                "handed_off",
                Some("sent"),
                Some("every block acknowledged by the station; D-RATS sends no delivery receipt"),
                Some(Via {
                    port: s.port.as_str(),
                    path_kind,
                    handoff: Some((s.peer.as_str(), "arq_ack")),
                }),
            ),
            "failed" | "cancelled" => self.message_state(
                id,
                "failed",
                None,
                s.reason.as_deref(),
                Some(Via {
                    port: s.port.as_str(),
                    path_kind,
                    handoff: None,
                }),
            ),
            _ if first => self.message_state(
                id,
                "sending",
                None,
                None,
                Some(Via {
                    port: s.port.as_str(),
                    path_kind,
                    handoff: None,
                }),
            ),
            _ => {}
        }
    }

    /// A complete form arrived from `peer`.
    pub(super) fn on_form_received(&mut self, port: &str, peer: &str, data: &[u8]) {
        let me = self.station.callsign.clone();
        let parsed = std::str::from_utf8(data)
            .map_err(|e| e.to_string())
            .and_then(|x| Form::parse(x).map(|f| (x, f)).map_err(|e| e.to_string()));
        let (raw, form) = match parsed {
            Ok(v) => v,
            Err(e) => {
                // Keep what arrived so nothing is lost.
                let saved = crate::inbox::save_unique(
                    &self.files_dir.join("forms"),
                    &format!("form_{}_{peer}.xml", unix_ms()),
                    data,
                );
                let at = saved
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|e| format!("(not saved: {e})"));
                self.log(
                    "warn",
                    Some(port),
                    &format!("form from {peer} is not a readable D-RATS form ({e}); kept as {at}"),
                );
                return;
            }
        };
        // Append our call to the path, as FormRecvThread does, without
        // re-serialising the rest of the document.
        let xml = match raw.rfind("</path>") {
            Some(i) => format!("{}<e>{me}</e>{}", &raw[..i], &raw[i..]),
            None => match raw.rfind("</form>") {
                Some(i) => format!("{}<path><e>{me}</e></path>\n{}", &raw[..i], &raw[i..]),
                None => raw.to_string(),
            },
        };
        let now = unix_ms();
        let mid = form
            .path
            .mid
            .clone()
            .unwrap_or_else(|| aether_forms::message_id(peer, now / 1000, rand_u32()));
        if let Ok(Some(existing)) = self.store.message_by_mid(&mid) {
            self.log(
                "info",
                Some(port),
                &format!(
                    "message {mid} from {peer} arrived again; already in {} (not stored twice)",
                    existing.folder
                ),
            );
            return;
        }
        let src = match form.sender() {
            s if s == "Unknown" => peer.to_string(),
            s => s,
        };
        let dst = match form.recipient() {
            s if s == "Unknown" => me.clone(),
            s => s,
        };
        if !dst.eq_ignore_ascii_case(&me) {
            self.log(
                "warn",
                Some(port),
                &format!("message {mid} is addressed to {dst}; forwarding is not supported yet, kept in the inbox"),
            );
        }
        let path_kind = self
            .ports
            .get(port)
            .map(|p| p.path_kind)
            .unwrap_or("rf_slow");
        let m = MessageRow {
            id: uuid::Uuid::now_v7().to_string(),
            mid,
            folder: "inbox".into(),
            form_id: Some(form.id.clone()),
            subject: Some(form.subject()),
            src,
            dst,
            priority: "routine".into(),
            xml,
            state: "received".into(),
            state_reason: None,
            attempts: 0,
            created_at: now,
            updated_at: now,
        };
        let ev = MessageEvent {
            at: now,
            state: "received".into(),
            hop: Some(peer.to_string()),
            port_id: Some(port.to_string()),
            path_kind: Some(path_kind.into()),
            ..MessageEvent::default()
        };
        match self.store.insert_message(&m, &ev) {
            Ok(()) => {
                self.log(
                    "info",
                    Some(port),
                    &format!(
                        "message from {} received: {}",
                        m.src,
                        m.subject.as_deref().unwrap_or("")
                    ),
                );
                self.publish(
                    "message.state",
                    json!({ "message": summary_json(&m), "event": event_json(&ev) }),
                );
            }
            Err(e) => self.log("error", Some(port), &format!("cannot store message: {e}")),
        }
    }
}
