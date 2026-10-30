//! Repeater operation in the engine (docs/10, ADR-0009): route guard,
//! CI-V session events, scheduler reports, operator pause, scope rules for
//! automatic replies and files, and the repeater access check.

use std::sync::mpsc as std_mpsc;

use aether_civ::dstar::{RxCalls, RxMessage};
use aether_link::channel::Source;

use super::*;
use crate::civ::{CivEvt, CivReq};
use crate::ports::SchedReport;
use crate::route::{Gate, Guard, Scope, Verdict};

/// Largest file sent in reflector scope (docs/10 §4.6).
pub const REFLECTOR_FILE_LIMIT: usize = 8 * 1024;

/// How long a network-originated call counts as "the current traffic"
/// for suppressing broadcast replies.
const NET_RX_WINDOW_MS: i64 = 15_000;

/// A repeater access check in progress or finished.
#[derive(Debug, Clone)]
pub struct AccessCheck {
    pub started: i64,
    pub over_end: Option<i64>,
    pub ack_at: Option<i64>,
    pub ack_text: Option<String>,
    pub done: bool,
}

/// Per-radio-port repeater state.
pub(super) struct RadioLink {
    pub guard: Guard,
    pub answer_broadcast: Option<bool>,
    pub civ_tx: Option<std_mpsc::Sender<CivReq>>,
    pub civ_error: Option<String>,
    pub my_call: Option<String>,
    pub civ_sources: Vec<Source>,
    pub sched: Option<SchedReport>,
    pub held_sent: Option<bool>,
    pub last_gate: Option<Gate>,
    pub last_net_rx: Option<i64>,
    pub last_rx_calls: Option<RxCalls>,
    pub last_burst_end: Option<i64>,
    pub last_ack: Option<(i64, Option<String>)>,
    pub link_seen: Option<i64>,
    pub check: Option<AccessCheck>,
}

impl RadioLink {
    pub fn new(guard: Guard, answer_broadcast: Option<bool>) -> Self {
        RadioLink {
            guard,
            answer_broadcast,
            civ_tx: None,
            civ_error: None,
            my_call: None,
            civ_sources: Vec::new(),
            sched: None,
            held_sent: None,
            last_gate: None,
            last_net_rx: None,
            last_rx_calls: None,
            last_burst_end: None,
            last_ack: None,
            link_seen: None,
            check: None,
        }
    }
}

/// A reflector named at the start of a repeater's ack or status text
/// ("REF001 C  BER: 0.0%", "XLX123 B …", "DCS006 F"). MMDVMHost puts the
/// linked reflector there [MMDVMHost `DStarControl.cpp` `sendAck`].
pub fn reflector_in(text: &str) -> Option<String> {
    let t = text.trim_start();
    let head: String = t.chars().take(8).collect();
    let prefix = head.get(..3)?;
    let known = ["REF", "XRF", "DCS", "XLX", "XDX", "DPL", "CCS"];
    if !known.contains(&prefix) || !head[3..6].bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let rest: String = t.chars().skip(6).take(2).collect();
    let module = rest.trim();
    if module.len() == 1 && module.as_bytes()[0].is_ascii_uppercase() {
        Some(format!("{} {}", &head[..6], module))
    } else {
        Some(head[..6].to_string())
    }
}

/// BER from an ack text ("BER: 1.2%" or "REF001 C 1.2% -80dBm").
pub fn ber_in(text: &str) -> Option<String> {
    text.split_whitespace()
        .find(|w| w.ends_with('%') && w[..w.len() - 1].parse::<f32>().is_ok())
        .map(str::to_string)
}

fn calls_json(c: &aether_civ::dstar::TxCalls) -> Value {
    json!({ "ur": c.ur.trim_end(), "r1": c.r1.trim_end(), "r2": c.r2.trim_end() })
}

#[derive(Debug)]
pub enum RouteCmd {
    /// The operator confirms what the gate asks for.
    Confirm {
        port: String,
    },
    /// Read the radio's route now (CI-V).
    Verify {
        port: String,
    },
    /// Set the radio's UR/R1/R2 for this scope (CI-V, `allow_set`).
    Apply {
        port: String,
        confirm: bool,
    },
    Pause {
        port: String,
        minutes: Option<u32>,
    },
    Resume {
        port: String,
    },
    /// Repeater access check (docs/10 §4.8).
    AccessCheck {
        port: String,
    },
}

impl Engine {
    fn to_unix(&self, t: u64) -> i64 {
        unix_ms() - self.now().saturating_sub(t) as i64
    }

    pub(super) fn route_json(&self, port: &str) -> Value {
        let Some(l) = self.ports.get(port).and_then(|p| p.link.as_ref()) else {
            return Value::Null;
        };
        let now = self.now();
        let g = &l.guard;
        let gate = g.gate(now);
        let (gate_s, needs, verdict) = match &gate {
            Gate::Open => ("open", Value::Null, None),
            Gate::Checking => ("checking", Value::Null, None),
            Gate::NeedsConfirm { reflector, route } => (
                "confirm",
                json!({ "reflector": reflector, "route": route }),
                None,
            ),
            Gate::Wrong(v) => (
                if matches!(v, Verdict::Block { .. }) {
                    "blocked"
                } else {
                    "held"
                },
                Value::Null,
                Some(v),
            ),
        };
        let my_call_ok = l
            .my_call
            .as_ref()
            .map(|m| m.eq_ignore_ascii_case(&self.station.callsign));
        json!({
            "via": if g.route.scope == Scope::Direct { "direct" } else { "repeater" },
            "scope": g.route.scope.as_str(),
            "repeater": g.route.repeater.as_ref().map(|r| r.trim_end()),
            "gateway": g.route.gateway().map(|r| r.trim_end().to_string()),
            "gate": gate_s,
            "needs": needs,
            "code": verdict.and_then(|v| v.code()),
            "message": verdict.and_then(|v| v.message()),
            "source": g.source(now).map(|s| match s { crate::route::Source::Civ => "civ", crate::route::Source::Operator => "operator" }),
            "civRoute": g.civ,
            "civError": l.civ_error,
            "observed": g.observed.as_ref().map(|o| json!({
                "calls": o.calls.as_ref().map(calls_json), "duplex": o.duplex,
            })),
            "required": g.route.required().as_ref().map(calls_json),
            "checkedAt": g.checked_at.map(|t| self.to_unix(t)),
            "confirmedAt": g.confirmed_at.map(|t| self.to_unix(t)),
            "recheckMin": g.recheck_ms / 60_000,
            "myCall": l.my_call, "myCallMatches": my_call_ok,
            "link": g.link.as_ref().map(|r| json!({ "reflector": r, "seenAt": l.link_seen })),
            "lastAck": l.last_ack.as_ref().map(|(at, text)| json!({
                "at": at, "text": text, "ber": text.as_deref().and_then(ber_in),
            })),
            "answerBroadcast": self.broadcast_allowed(port),
        })
    }

    pub(super) fn channel_json(&self, port: &str) -> Value {
        let Some(r) = self
            .ports
            .get(port)
            .and_then(|p| p.link.as_ref())
            .and_then(|l| l.sched.as_ref())
        else {
            return Value::Null;
        };
        json!({
            "state": r.state, "why": r.why, "until": r.until, "queued": r.queued,
            "sources": r.sources, "sensesVoice": r.senses_voice,
            "inferenceOff": r.inference_off, "holdsTimers": r.holds_timers,
            "airUsedMs": r.air_used_ms, "airBudgetMs": r.air_budget_ms,
            "maxOverMs": r.max_over_ms,
        })
    }

    /// Whether automatic answers to broadcast requests may go out now.
    pub(super) fn broadcast_allowed(&self, port: &str) -> bool {
        let Some(l) = self.ports.get(port).and_then(|p| p.link.as_ref()) else {
            return true;
        };
        if let Some(v) = l.answer_broadcast {
            return v;
        }
        match l.guard.route.scope {
            Scope::Reflector => false,
            Scope::Repeater => !l
                .last_net_rx
                .is_some_and(|t| unix_ms() - t < NET_RX_WINDOW_MS),
            Scope::Direct => true,
        }
    }

    /// Refuse or allow a file transfer on `port` (reflector scope rules).
    pub(super) fn check_file_scope(
        &self,
        port: &str,
        size: usize,
        confirm_reflector: bool,
    ) -> Result<(), ApiError> {
        let Some(l) = self.ports.get(port).and_then(|p| p.link.as_ref()) else {
            return Ok(());
        };
        if l.guard.route.scope != Scope::Reflector {
            return Ok(());
        }
        let reach = l
            .guard
            .link
            .clone()
            .unwrap_or_else(|| "whatever the repeater is linked to".into());
        if size > REFLECTOR_FILE_LIMIT {
            return Err(ApiError::new(
                E_INVALID,
                format!(
                    "{port} sends to the linked reflector ({reach}); files over {} KiB are not sent there. Use a port limited to the repeater, or a smaller file.",
                    REFLECTOR_FILE_LIMIT / 1024
                ),
            ));
        }
        if !confirm_reflector {
            let secs = (size as u64 * 1000 / 119 + 1_100) / 1000;
            return Err(ApiError::new(
                E_INVALID,
                format!(
                    "{port} sends to the linked reflector ({reach}): this file would occupy every linked repeater for about {secs} s. Confirm to send it (confirmReflector)."
                ),
            ));
        }
        Ok(())
    }

    /// Re-evaluate every route guard; hold or release ports; publish
    /// changes.
    pub(super) async fn route_tick(&mut self) {
        let now = self.now();
        let names: Vec<String> = self
            .ports
            .iter()
            .filter(|(_, p)| p.link.is_some())
            .map(|(n, _)| n.clone())
            .collect();
        for n in names {
            let (gate, held_sent, last_gate, tx) = {
                let p = &self.ports[&n];
                let l = p.link.as_ref().expect("filtered");
                (
                    l.guard.gate(now),
                    l.held_sent,
                    l.last_gate.clone(),
                    p.tx.clone(),
                )
            };
            let hold = !gate.is_open();
            if held_sent != Some(hold) && tx.send(PortCmd::RouteHold(hold)).await.is_ok() {
                if let Some(l) = self.ports.get_mut(&n).and_then(|p| p.link.as_mut()) {
                    l.held_sent = Some(hold);
                }
            }
            if last_gate.as_ref() != Some(&gate) {
                if let Some(l) = self.ports.get_mut(&n).and_then(|p| p.link.as_mut()) {
                    l.last_gate = Some(gate.clone());
                }
                match &gate {
                    Gate::Wrong(v) => {
                        let level = if matches!(v, Verdict::Block { .. }) {
                            "error"
                        } else {
                            "warn"
                        };
                        let msg = format!("sending held: {}", v.message().unwrap_or(""));
                        self.log(level, Some(&n), &msg);
                    }
                    Gate::NeedsConfirm { reflector, route } => {
                        let what = match (reflector, route) {
                            (true, true) => "confirm the radio's route and the reflector reach",
                            (true, false) => "confirm sending to the linked reflector",
                            _ => {
                                "confirm the radio's UR/R1/R2 (they can't be read from this radio)"
                            }
                        };
                        self.log("info", Some(&n), &format!("sending held: {what}"));
                    }
                    Gate::Open if last_gate.is_some() => {
                        self.log("info", Some(&n), "route OK; sending resumes");
                    }
                    _ => {}
                }
                let v = self.route_json(&n);
                self.publish("port.route", json!({ "portId": n, "route": v }));
            }
        }
    }

    pub(super) async fn on_civ_evt(&mut self, port: String, e: CivEvt) {
        let now = self.now();
        let Some(l) = self.ports.get_mut(&port).and_then(|p| p.link.as_mut()) else {
            return;
        };
        match e {
            CivEvt::Open(Ok(())) => {
                l.civ_error = None;
            }
            CivEvt::Open(Err(err)) => {
                let new = l.civ_error.as_ref() != Some(&err);
                l.civ_error = Some(err.clone());
                l.guard.on_read_error(err.clone());
                if new {
                    self.log("warn", Some(&port), &format!("CI-V: {err}"));
                }
            }
            CivEvt::Route(Ok(obs)) => {
                l.civ_error = None;
                l.guard.on_reading(now, obs);
            }
            CivEvt::Route(Err(err)) => {
                let unsupported = err.contains("does not answer the D-STAR");
                if unsupported {
                    l.guard.civ = false;
                }
                let new = l.civ_error.as_ref() != Some(&err);
                l.civ_error = Some(err.clone());
                l.guard.on_read_error(err.clone());
                if new {
                    let msg = if unsupported {
                        format!("{err}; the route will be confirmed by the operator instead")
                    } else {
                        format!("cannot read the radio's route: {err}")
                    };
                    self.log("warn", Some(&port), &msg);
                }
            }
            CivEvt::MyCall(call) => {
                let me = self.station.callsign.clone();
                l.my_call = Some(call.clone());
                if !call.eq_ignore_ascii_case(&me) {
                    self.log(
                        "warn",
                        Some(&port),
                        &format!("the radio's MY call sign is {call}, but this station is {me}. Transmissions are identified by the radio's MY call."),
                    );
                }
            }
            CivEvt::Sources(s) => {
                l.civ_sources = s;
            }
            CivEvt::Channel(_) => {}
            CivEvt::Changed => {
                l.guard.invalidate();
                if let Some(tx) = &l.civ_tx {
                    let _ = tx.send(CivReq::ReadRoute);
                }
            }
            CivEvt::RxCalls(c) => self.on_rx_calls(&port, c),
            CivEvt::RxMessage(m) => self.on_rx_message(&port, m),
        }
    }

    fn on_rx_calls(&mut self, port: &str, c: RxCalls) {
        let me = self.station.callsign.clone();
        let Some(l) = self.ports.get_mut(port).and_then(|p| p.link.as_mut()) else {
            return;
        };
        if l.last_rx_calls.as_ref() == Some(&c) {
            return;
        }
        l.last_rx_calls = Some(c.clone());
        let rpt = l.guard.route.repeater.clone().unwrap_or_default();
        if !rpt.is_empty() && c.is_ack_from(&rpt, &me) {
            let at = unix_ms();
            l.last_ack = Some((at, None));
            if let Some(chk) = l.check.as_mut().filter(|k| !k.done) {
                chk.ack_at = Some(at);
            }
        } else if !rpt.is_empty() && c.from_network(&rpt) {
            l.last_net_rx = Some(unix_ms());
        }
    }

    fn on_rx_message(&mut self, port: &str, m: RxMessage) {
        let me = self.station.callsign.clone();
        let Some(l) = self.ports.get_mut(port).and_then(|p| p.link.as_mut()) else {
            return;
        };
        let rpt = l.guard.route.repeater.clone().unwrap_or_default();
        if rpt.is_empty() || m.caller.trim_end() != rpt.trim_end() {
            return;
        }
        // The repeater's own text: its ack to us (BER, link) or an
        // announcement. Either names the linked reflector when linked.
        let text = m.text.trim_end().to_string();
        let acked_us = l
            .last_rx_calls
            .as_ref()
            .is_some_and(|c| c.is_ack_from(&rpt, &me));
        if acked_us {
            let at = l.last_ack.as_ref().map(|(t, _)| *t).unwrap_or_else(unix_ms);
            l.last_ack = Some((at, Some(text.clone())));
            if let Some(chk) = l.check.as_mut().filter(|k| !k.done) {
                chk.ack_text = Some(text.clone());
            }
        }
        let link = reflector_in(&text);
        let changed = link != l.guard.link;
        if acked_us || link.is_some() {
            l.link_seen = Some(unix_ms());
            l.guard.on_link(link.clone());
        }
        if changed && (acked_us || link.is_some()) {
            let msg = match &link {
                Some(r) => format!("{} is linked to {r}", rpt.trim_end()),
                None => format!("{} is not linked to a reflector", rpt.trim_end()),
            };
            self.log("info", Some(port), &msg);
            let v = self.route_json(port);
            self.publish("port.route", json!({ "portId": port, "route": v }));
        }
    }

    pub(super) async fn on_sched(&mut self, port: String, report: SchedReport) {
        let now = self.now();
        let holds = report.holds_timers;
        if holds && !self.xfers.port_paused(&port) {
            self.xfers.pause_port(&port, now);
        } else if !holds && self.xfers.port_paused(&port) {
            self.xfers.resume_port(&port, now);
        }
        if let Some(l) = self.ports.get_mut(&port).and_then(|p| p.link.as_mut()) {
            l.sched = Some(report);
        }
        let v = self.channel_json(&port);
        self.publish("port.channel", json!({ "portId": port, "channel": v }));
    }

    pub(super) fn on_burst(&mut self, port: &str, air_ms: u64) {
        let now = self.now();
        if let Some(l) = self.ports.get_mut(port).and_then(|p| p.link.as_mut()) {
            l.guard.on_transmitted(now);
            let end = unix_ms() + air_ms as i64;
            l.last_burst_end = Some(end);
            if let Some(chk) = l.check.as_mut().filter(|k| !k.done && k.over_end.is_none()) {
                chk.over_end = Some(end);
            }
        }
    }

    /// The radio port reconnected: the radio may have been changed.
    pub(super) fn on_radio_reconnect(&mut self, port: &str) {
        if let Some(l) = self.ports.get_mut(port).and_then(|p| p.link.as_mut()) {
            l.guard.invalidate();
            if let Some(tx) = &l.civ_tx {
                let _ = tx.send(CivReq::ReadRoute);
            }
        }
    }

    pub(super) async fn route_cmd(&mut self, c: RouteCmd) -> Result<Value, ApiError> {
        let port = match &c {
            RouteCmd::Confirm { port }
            | RouteCmd::Verify { port }
            | RouteCmd::Apply { port, .. }
            | RouteCmd::Pause { port, .. }
            | RouteCmd::Resume { port }
            | RouteCmd::AccessCheck { port } => port.clone(),
        };
        let now = self.now();
        let Some(p) = self.ports.get_mut(&port) else {
            return Err(ApiError::new(E_NOT_FOUND, format!("no port {port}")));
        };
        let tx = p.tx.clone();
        let Some(l) = p.link.as_mut() else {
            return Err(ApiError::new(
                E_INVALID,
                format!("{port} is not a radio port"),
            ));
        };
        match c {
            RouteCmd::Confirm { .. } => {
                if let Gate::Wrong(v) = l.guard.gate(now) {
                    return Err(ApiError::new(
                        E_INVALID,
                        format!(
                            "the radio reports a different route: {}",
                            v.message().unwrap_or("")
                        ),
                    ));
                }
                l.guard.confirm(now);
                let scope = l.guard.route.scope;
                let link = l.guard.link.clone();
                let what = match scope {
                    Scope::Reflector => format!(
                        "operator confirmed sending to the linked reflector{}",
                        link.map(|r| format!(" ({r})")).unwrap_or_default()
                    ),
                    _ => "operator confirmed the radio's route".into(),
                };
                self.log("info", Some(&port), &what);
                self.route_tick().await;
                Ok(self.route_json(&port))
            }
            RouteCmd::Verify { .. } => {
                let Some(civ) = &l.civ_tx else {
                    return Err(ApiError::new(
                        E_INVALID,
                        format!("{port} has no CI-V channel; confirm the route instead"),
                    ));
                };
                let _ = civ.send(CivReq::ReadRoute);
                Ok(self.route_json(&port))
            }
            RouteCmd::Apply { confirm, .. } => {
                let civ = l.civ_tx.clone();
                let Some(required) = l.guard.route.required() else {
                    return Err(ApiError::new(
                        E_INVALID,
                        format!("{port} is direct: there is no repeater route to set"),
                    ));
                };
                let (cfg, model) = p.civ.clone().ok_or_else(|| {
                    ApiError::new(
                        E_INVALID,
                        format!("{port} has no CI-V channel; set UR/R1/R2 on the radio"),
                    )
                })?;
                if !cfg.allow_set {
                    return Err(ApiError::new(
                        E_INVALID,
                        format!(
                            "changing the radio is not enabled for {port} (civ.allow_set = false)"
                        ),
                    ));
                }
                if !model.route_write_tested || model.dstar != aether_civ::Support::Documented {
                    return Err(ApiError::new(
                        E_INVALID,
                        format!(
                            "setting UR/R1/R2 over CI-V has not been tested on the {} yet (hardware test HW-R1); set them on the radio",
                            model.name
                        ),
                    ));
                }
                if !confirm {
                    return Err(ApiError::new(
                        E_INVALID,
                        "setting the radio's call signs needs confirm".to_string(),
                    ));
                }
                let civ =
                    civ.ok_or_else(|| ApiError::new(E_INTERNAL, "CI-V session not running"))?;
                let (rtx, rrx) = oneshot::channel();
                civ.send(CivReq::SetRoute {
                    calls: required,
                    reply: rtx,
                })
                .map_err(|_| ApiError::new(E_INTERNAL, "CI-V session stopped"))?;
                let res = rrx
                    .await
                    .map_err(|_| ApiError::new(E_INTERNAL, "CI-V session stopped"))?;
                match res {
                    Ok(obs) => {
                        if let Some(l) = self.ports.get_mut(&port).and_then(|p| p.link.as_mut()) {
                            l.guard
                                .on_reading(self.started.elapsed().as_millis() as u64, obs);
                        }
                        self.log(
                            "info",
                            Some(&port),
                            "radio UR/R1/R2 set for this port's scope (read back)",
                        );
                        self.route_tick().await;
                        Ok(self.route_json(&port))
                    }
                    Err(e) => Err(ApiError::new(E_INTERNAL, e)),
                }
            }
            RouteCmd::Pause { minutes, .. } => {
                let ms = minutes.map(|m| m as u64 * 60_000);
                tx.send(PortCmd::Pause(ms))
                    .await
                    .map_err(|_| ApiError::new(E_INTERNAL, "port stopped"))?;
                let what = match minutes {
                    Some(m) => format!("sending paused for {m} min"),
                    None => "sending paused until resumed".into(),
                };
                self.log("info", Some(&port), &what);
                Ok(json!({ "portId": port, "paused": true, "minutes": minutes }))
            }
            RouteCmd::Resume { .. } => {
                tx.send(PortCmd::Resume)
                    .await
                    .map_err(|_| ApiError::new(E_INTERNAL, "port stopped"))?;
                self.log("info", Some(&port), "sending resumed");
                Ok(json!({ "portId": port, "paused": false }))
            }
            RouteCmd::AccessCheck { .. } => {
                if l.guard.route.scope == Scope::Direct {
                    return Err(ApiError::new(
                        E_INVALID,
                        format!("{port} is direct: there is no repeater to check"),
                    ));
                }
                if l.civ_tx.is_none() || !l.guard.civ {
                    return Err(ApiError::new(
                        E_INVALID,
                        format!("{port}: the access check needs CI-V call-sign reads, which this radio doesn't provide"),
                    ));
                }
                if !l.guard.gate(now).is_open() {
                    return Err(ApiError::new(
                        E_INVALID,
                        format!("{port}: sending is held; resolve the route first"),
                    ));
                }
                l.check = Some(AccessCheck {
                    started: unix_ms(),
                    over_end: None,
                    ack_at: None,
                    ack_text: None,
                    done: false,
                });
                // One short frame addressed to ourselves: nobody answers it,
                // only the repeater acknowledges the over.
                let me = self.station.callsign.clone();
                let f = aether_ddt2::Frame::new(
                    0,
                    aether_ddt2::session::id::CHAT,
                    aether_ddt2::session::chat::PING_REQ,
                    &me,
                    &me,
                    b"Aether DV access check".to_vec(),
                );
                self.send_frame(&port, &f, PayloadEncoding::Never, Priority::Chat, None)
                    .await?;
                self.log("info", Some(&port), "repeater access check sent");
                Ok(json!({ "portId": port, "started": true }))
            }
        }
    }

    /// Finish access checks whose answer window has passed.
    pub(super) fn access_check_tick(&mut self) {
        let now = unix_ms();
        let mut done = Vec::new();
        for (n, p) in self.ports.iter_mut() {
            let Some(l) = p.link.as_mut() else { continue };
            let Some(chk) = l.check.as_mut().filter(|k| !k.done) else {
                continue;
            };
            let waited_out = chk.over_end.is_some_and(|e| now > e + 6_000);
            let gave_up = now > chk.started + 120_000;
            if waited_out || gave_up || (chk.ack_at.is_some() && chk.ack_text.is_some()) {
                chk.done = true;
                done.push((n.clone(), chk.clone(), l.guard.link.clone()));
            }
        }
        for (n, chk, link) in done {
            let result = match (chk.over_end, chk.ack_at) {
                (None, _) => {
                    json!({ "ok": false, "reason": "not_sent", "message": "the check could not be sent (channel busy or held)" })
                }
                (Some(_), None) => {
                    json!({ "ok": false, "reason": "no_ack", "message": "no acknowledgement from the repeater: check R1, the offset/duplex, tone settings and range. Some repeaters (Icom controllers, or MMDVMHost with AckReply off) never acknowledge." })
                }
                (Some(end), Some(at)) => json!({
                    "ok": true, "ackAfterMs": (at - end).max(0),
                    "text": chk.ack_text, "ber": chk.ack_text.as_deref().and_then(ber_in),
                    "link": link,
                }),
            };
            let msg = match result["ok"].as_bool() {
                Some(true) => format!(
                    "repeater access check: acknowledged{}{}",
                    result["ber"]
                        .as_str()
                        .map(|b| format!(", BER {b}"))
                        .unwrap_or_default(),
                    result["link"]
                        .as_str()
                        .map(|r| format!(", linked to {r}"))
                        .unwrap_or_default()
                ),
                _ => format!(
                    "repeater access check: {}",
                    result["message"].as_str().unwrap_or("")
                ),
            };
            self.log("info", Some(&n), &msg);
            self.publish("port.accessCheck", json!({ "portId": n, "result": result }));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reflector_and_ber_from_ack_text() {
        assert_eq!(
            reflector_in("REF001 C  BER: 0.0%  ").as_deref(),
            Some("REF001 C")
        );
        assert_eq!(
            reflector_in("XLX123 B 1.2% -80dBm").as_deref(),
            Some("XLX123 B")
        );
        assert_eq!(reflector_in("DCS006 F").as_deref(), Some("DCS006 F"));
        assert_eq!(reflector_in("BER: 0.0%"), None);
        assert_eq!(reflector_in("REFLECTOR"), None);
        assert_eq!(ber_in("REF001 C  BER: 1.5%").as_deref(), Some("1.5%"));
        assert_eq!(ber_in("XLX123 B 0.3% -80dBm").as_deref(), Some("0.3%"));
        assert_eq!(ber_in("hello"), None);
    }
}
