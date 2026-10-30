//! Diagnostics: wire capture control, the engine log, support bundles.

use super::*;

/// A `diag.*` request.
#[derive(Debug)]
pub enum DiagCmd {
    CaptureStart {
        port: String,
    },
    /// `id` is a capture id from `CaptureStart` or a port name.
    CaptureStop {
        id: String,
    },
    Events {
        before_id: Option<i64>,
        limit: u32,
    },
    SupportBundle {
        include_captures: bool,
    },
}

fn stamp() -> String {
    // Sortable UTC stamp for file names: YYYYMMDDTHHMMSSZ.
    let secs = unix_ms() / 1000;
    let (y, m, d) = aether_forms::utc_ymd(secs);
    let t = secs.rem_euclid(86_400);
    format!(
        "{y}{m:02}{d:02}T{:02}{:02}{:02}Z",
        t / 3600,
        (t / 60) % 60,
        t % 60
    )
}

impl Engine {
    pub(super) fn start_capture(&mut self, port: &str) -> Result<Value, ApiError> {
        let p = self
            .ports
            .get(port)
            .ok_or_else(|| ApiError::new(E_NOT_FOUND, format!("no port {port}")))?;
        if let Some(path) = p.tap.active() {
            return Err(ApiError::new(
                E_INVALID,
                format!("{port} is already capturing to {}", path.display()),
            ));
        }
        // Short enough for a tar header (support bundles).
        let base: String = crate::inbox::safe_name(port).chars().take(40).collect();
        let file = format!("{base}-{}.aecap", stamp());
        let path = self.capture_dir.join(&file);
        let header = json!({
            "port": port, "kind": p.kind, "pathKind": p.path_kind,
            "callsign": self.station.callsign, "engine": crate::station::VERSION,
            "startedAt": unix_ms(),
        });
        p.tap
            .start(&path, header)
            .map_err(|e| ApiError::new(E_INTERNAL, format!("{}: {e}", path.display())))?;
        self.log(
            "info",
            Some(port),
            &format!("wire capture started: {}", path.display()),
        );
        Ok(json!({ "captureId": file, "portId": port, "path": path }))
    }

    pub(super) fn on_diag_cmd(&mut self, c: DiagCmd) -> Result<Value, ApiError> {
        match c {
            DiagCmd::CaptureStart { port } => self.start_capture(&port),
            DiagCmd::CaptureStop { id } => {
                let port = self
                    .ports
                    .iter()
                    .find(|(name, p)| {
                        *name == &id
                            || p.tap
                                .active()
                                .and_then(|a| a.file_name().map(|f| f.to_string_lossy() == id))
                                .unwrap_or(false)
                    })
                    .map(|(n, _)| n.clone())
                    .ok_or_else(|| ApiError::new(E_NOT_FOUND, format!("no capture {id}")))?;
                let path = self.ports[&port]
                    .tap
                    .stop()
                    .ok_or_else(|| ApiError::new(E_NOT_FOUND, format!("{port} is not capturing")))?;
                self.log(
                    "info",
                    Some(&port),
                    &format!("wire capture stopped: {}", path.display()),
                );
                let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                Ok(json!({ "portId": port, "path": path, "bytes": size }))
            }
            DiagCmd::Events { before_id, limit } => self
                .store
                .events(before_id, limit.min(5000))
                .map(|v| {
                    Value::Array(
                        v.into_iter()
                            .map(|(id, at, level, category, port, message)| {
                                json!({ "id": id, "at": at, "level": level, "category": category, "portId": port, "message": message })
                            })
                            .collect(),
                    )
                })
                .map_err(|e| ApiError::new(E_INTERNAL, e.to_string())),
            DiagCmd::SupportBundle { include_captures } => self.support_bundle(include_captures),
        }
    }

    fn support_bundle(&mut self, include_captures: bool) -> Result<Value, ApiError> {
        let internal = |e: aether_store::Error| ApiError::new(E_INTERNAL, e.to_string());
        let mut files: Vec<(String, Vec<u8>)> =
            vec![("README.txt".into(), crate::bundle::README.into())];
        let mut ports: Vec<Value> = self
            .ports
            .iter()
            .map(|(n, p)| {
                // Route and channel: the path record (docs/10 §4.8).
                json!({ "name": n, "kind": p.kind, "pathKind": p.path_kind, "state": p.state,
                        "detail": p.detail, "capturing": p.tap.active(),
                        "route": self.route_json(n), "channel": self.channel_json(n) })
            })
            .collect();
        ports.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        let about = json!({
            "engineVersion": crate::station::VERSION, "apiVersion": crate::api::API_VERSION,
            "os": std::env::consts::OS, "arch": std::env::consts::ARCH,
            "uptimeMs": self.started.elapsed().as_millis() as u64, "createdAt": unix_ms(),
            "ports": ports, "config": self.config_summary,
        });
        files.push((
            "about.json".into(),
            serde_json::to_vec_pretty(&about).unwrap_or_default(),
        ));
        let mut events = self.store.events(None, 20_000).map_err(internal)?;
        events.reverse();
        let mut ev = Vec::new();
        for (id, at, level, category, port, message) in events {
            ev.extend(
                json!({ "id": id, "at": at, "level": level, "category": category, "portId": port, "message": message })
                    .to_string()
                    .into_bytes(),
            );
            ev.push(b'\n');
        }
        files.push(("events.jsonl".into(), ev));
        let transfers: Vec<Value> = self
            .store
            .transfers(true, 10_000)
            .map_err(internal)?
            .iter()
            .map(transfer_json)
            .collect();
        files.push((
            "transfers.json".into(),
            serde_json::to_vec_pretty(&transfers).unwrap_or_default(),
        ));
        let mut msgs = Vec::new();
        for m in self.store.messages(None, 10_000, 0).map_err(internal)? {
            let evs: Vec<Value> = self
                .store
                .message_events(&m.id)
                .map_err(internal)?
                .iter()
                .map(|e| json!({ "at": e.at, "state": e.state, "hop": e.hop, "portId": e.port_id, "evidence": e.evidence }))
                .collect();
            msgs.push(json!({
                "id": m.id, "mid": m.mid, "folder": m.folder, "formId": m.form_id, "src": m.src,
                "dst": m.dst, "state": m.state, "stateReason": m.state_reason,
                "createdAt": m.created_at, "updatedAt": m.updated_at, "events": evs,
            }));
        }
        files.push((
            "messages.json".into(),
            serde_json::to_vec_pretty(&msgs).unwrap_or_default(),
        ));
        if include_captures {
            if let Ok(rd) = std::fs::read_dir(&self.capture_dir) {
                let mut caps: Vec<_> = rd
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|x| x == "aecap"))
                    .collect();
                caps.sort();
                for c in caps {
                    if let (Some(name), Ok(data)) = (c.file_name(), std::fs::read(&c)) {
                        files.push((format!("captures/{}", name.to_string_lossy()), data));
                    }
                }
            }
        }
        let path = self
            .capture_dir
            .join(format!("aether-support-{}.tar", stamp()));
        crate::bundle::write_tar(&path, &files, (unix_ms() / 1000) as u64)
            .map_err(|e| ApiError::new(E_INTERNAL, format!("{}: {e}", path.display())))?;
        self.log(
            "info",
            None,
            &format!("support bundle written: {}", path.display()),
        );
        Ok(json!({ "path": path, "files": files.iter().map(|f| &f.0).collect::<Vec<_>>() }))
    }
}
