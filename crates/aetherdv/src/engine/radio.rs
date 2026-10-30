//! Radio Fast Data setting over CI-V (ADR-0005): checked at start-up
//! against the port's configured mode, read on request, and changed only
//! on request with `allow_set` in the configuration, an explicit confirm,
//! no transfer running on the port, and a read-back.

use std::time::Duration;

use super::*;

const CIV_TIMEOUT: Duration = Duration::from_millis(1500);

impl Engine {
    fn civ_job(
        &self,
        port: &str,
        set: Option<bool>,
        action: &'static str,
    ) -> Option<tokio::task::JoinHandle<Result<bool, String>>> {
        let p = self.ports.get(port)?;
        let (c, model) = p.civ.clone()?;
        let addr = c.addr_byte().unwrap_or(model.default_addr);
        let (evt, name) = (self.evt_tx.clone(), port.to_string());
        Some(tokio::spawn(async move {
            let result = tokio::task::spawn_blocking(move || {
                crate::civ::fast_data(&c.path, c.baud, model, addr, set, CIV_TIMEOUT)
            })
            .await
            .unwrap_or_else(|e| Err(format!("CI-V task failed: {e}")));
            let _ = evt
                .send(PortEvt::RadioMode {
                    port: name,
                    action,
                    result: result.clone(),
                })
                .await;
            result
        }))
    }

    pub(super) fn check_radio_modes_at_start(&mut self) {
        let names: Vec<String> = self
            .ports
            .iter()
            .filter(|(_, p)| p.civ.is_some())
            .map(|(n, _)| n.clone())
            .collect();
        for n in names {
            let (c, model) = self.ports[&n].civ.clone().expect("filtered");
            if c.addr.is_none() && !model.addr_verified {
                self.log(
                    "warn",
                    Some(&n),
                    &format!(
                        "CI-V address {:02X}h for the {} is a default we could not confirm from Icom documents; set civ.addr to the radio's CI-V address if this fails",
                        model.default_addr, model.name
                    ),
                );
            }
            let _ = self.civ_job(&n, None, "startup");
        }
    }

    pub(super) fn on_radio_mode(
        &mut self,
        port: &str,
        action: &'static str,
        result: Result<bool, String>,
    ) {
        let Some(p) = self.ports.get_mut(port) else {
            return;
        };
        let configured_fast = p.path_kind == "rf_fast";
        match result {
            Ok(fast) => {
                p.radio_fast = Some(fast);
                let word = if fast {
                    "Fast Data ON"
                } else {
                    "Fast Data OFF (slow data)"
                };
                let what = match action {
                    "set" => format!("radio set to {word} (confirmed by reading it back)"),
                    _ => format!("radio reports {word}"),
                };
                if fast != configured_fast {
                    self.log(
                        "warn",
                        Some(port),
                        &format!(
                            "{what}, but this port is configured for {} data. Stations in different modes cannot decode each other: change the radio setting or the port's mode.",
                            if configured_fast { "fast" } else { "slow" }
                        ),
                    );
                } else {
                    self.log(
                        "info",
                        Some(port),
                        &format!("{what}; matches the port configuration"),
                    );
                }
                self.publish(
                    "port.state",
                    json!({ "portId": port, "state": self.ports[port].state, "detail": self.ports[port].detail, "radioFastData": fast }),
                );
            }
            Err(e) => self.log("warn", Some(port), &format!("CI-V {action}: {e}")),
        }
    }

    pub(super) fn radio_mode_cmd(
        &mut self,
        port: String,
        set: Option<bool>,
        confirm: bool,
        reply: Reply<Value>,
    ) {
        let refuse = |reply: Reply<Value>, code, msg: String| {
            let _ = reply.send(Err(ApiError::new(code, msg)));
        };
        let Some(p) = self.ports.get(&port) else {
            return refuse(reply, E_NOT_FOUND, format!("no port {port}"));
        };
        let Some((c, _)) = &p.civ else {
            return refuse(
                reply,
                E_INVALID,
                format!("{port} has no CI-V channel configured ([port.civ]); set the radio's Fast Data menu by hand"),
            );
        };
        if set.is_some() {
            if !c.allow_set {
                return refuse(
                    reply,
                    E_INVALID,
                    format!("changing the radio is not enabled for {port} (civ.allow_set = false)"),
                );
            }
            if !confirm {
                return refuse(
                    reply,
                    E_INVALID,
                    "changing the radio's mode needs confirm: while in Fast Data it cannot be decoded by slow-only stations on the channel".into(),
                );
            }
            let busy = self
                .xfers
                .snapshots()
                .iter()
                .any(|t| t.port == port && !matches!(t.state, "done" | "failed"));
            if busy {
                return refuse(
                    reply,
                    E_INVALID,
                    format!("a transfer is running on {port}; change the mode after it ends"),
                );
            }
        }
        let action = if set.is_some() { "set" } else { "read" };
        let Some(job) = self.civ_job(&port, set, action) else {
            return refuse(reply, E_INTERNAL, "CI-V unavailable".into());
        };
        let configured = p.path_kind == "rf_fast";
        tokio::spawn(async move {
            let res = match job.await {
                Ok(Ok(fast)) => {
                    let mut v = json!({ "portId": port, "fastData": fast, "via": "civ", "configuredFast": configured, "matches": fast == configured });
                    v[if set.is_some() {
                        "readBackAt"
                    } else {
                        "readAt"
                    }] = json!(unix_ms());
                    Ok(v)
                }
                Ok(Err(e)) => Err(ApiError::new(E_INTERNAL, e)),
                Err(e) => Err(ApiError::new(E_INTERNAL, e.to_string())),
            };
            let _ = reply.send(res);
        });
    }
}
