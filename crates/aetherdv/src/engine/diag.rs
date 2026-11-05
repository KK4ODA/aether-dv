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
    /// Radio test session: start captures everywhere, take markers, then
    /// write one compressed bundle with a plain-text summary.
    TestStart {
        description: String,
    },
    TestMark {
        text: String,
    },
    TestFinish,
    TestStatus,
}

/// A radio test in progress.
#[derive(Debug, Clone)]
pub struct TestSession {
    pub description: String,
    pub started_at: i64,
    pub markers: Vec<(i64, String)>,
    /// Captures this test started (port, file).
    pub captures: Vec<(String, std::path::PathBuf)>,
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
            DiagCmd::TestStart { description } => self.test_start(description),
            DiagCmd::TestMark { text } => self.test_mark(text),
            DiagCmd::TestFinish => self.test_finish(),
            DiagCmd::TestStatus => Ok(self.test_status()),
        }
    }

    fn test_status(&self) -> Value {
        match &self.test {
            None => json!({ "running": false }),
            Some(t) => json!({
                "running": true, "description": t.description, "startedAt": t.started_at,
                "markers": t.markers.iter().map(|(at, m)| json!({ "at": at, "text": m })).collect::<Vec<_>>(),
                "ports": t.captures.iter().map(|(p, _)| p).collect::<Vec<_>>(),
            }),
        }
    }

    fn test_start(&mut self, description: String) -> Result<Value, ApiError> {
        if self.test.is_some() {
            return Err(ApiError::new(
                E_INVALID,
                "a test is already running; finish it first",
            ));
        }
        let description = description.trim().chars().take(500).collect::<String>();
        let names: Vec<String> = self.ports.keys().cloned().collect();
        let mut captures = Vec::new();
        for n in names {
            match self.ports[&n].tap.active() {
                // Already capturing (capture = true): use that file.
                Some(path) => captures.push((n, path)),
                None => {
                    let v = self.start_capture(&n)?;
                    if let Some(p) = v["path"].as_str() {
                        captures.push((n, std::path::PathBuf::from(p)));
                    }
                }
            }
        }
        let started_at = unix_ms();
        self.test = Some(TestSession {
            description: description.clone(),
            started_at,
            markers: Vec::new(),
            captures,
        });
        let names: Vec<String> = self.ports.keys().cloned().collect();
        for n in names {
            let state = json!({ "route": self.route_json(&n), "channel": self.channel_json(&n) });
            let p = &self.ports[&n];
            p.tap.event(
                "marker",
                json!({ "text": format!("test started: {description}") }),
            );
            // Where things stood when the test began.
            p.tap.event("state", state);
        }
        self.log("info", None, &format!("radio test started: {description}"));
        Ok(self.test_status())
    }

    fn test_mark(&mut self, text: String) -> Result<Value, ApiError> {
        let text = text.trim().chars().take(300).collect::<String>();
        if text.is_empty() {
            return Err(ApiError::new(E_INVALID, "the marker needs some text"));
        }
        let Some(t) = self.test.as_mut() else {
            return Err(ApiError::new(
                E_INVALID,
                "no test is running; start one first",
            ));
        };
        let at = unix_ms();
        t.markers.push((at, text.clone()));
        for p in self.ports.values() {
            p.tap.event("marker", json!({ "text": text }));
        }
        self.log("info", None, &format!("test marker: {text}"));
        Ok(json!({ "at": at, "text": text }))
    }

    fn test_finish(&mut self) -> Result<Value, ApiError> {
        let Some(t) = self.test.take() else {
            return Err(ApiError::new(E_INVALID, "no test is running"));
        };
        let finished_at = unix_ms();
        for p in self.ports.values() {
            p.tap.event("marker", json!({ "text": "test finished" }));
        }
        for (port, path) in &t.captures {
            if self.ports.get(port).and_then(|p| p.tap.active()).as_ref() == Some(path) {
                let _ = self.ports[port].tap.stop();
            }
        }
        let summary = self.test_summary(&t, finished_at);
        let mut files = self.bundle_files()?;
        files.insert(0, ("summary.txt".into(), summary.clone().into_bytes()));
        files.push((
            "test.json".into(),
            serde_json::to_vec_pretty(&json!({
                "description": t.description, "startedAt": t.started_at, "finishedAt": finished_at,
                "markers": t.markers.iter().map(|(at, m)| json!({ "at": at, "text": m })).collect::<Vec<_>>(),
            }))
            .unwrap_or_default(),
        ));
        for (_, path) in &t.captures {
            if let (Some(name), Ok(data)) = (path.file_name(), std::fs::read(path)) {
                files.push((format!("captures/{}", name.to_string_lossy()), data));
            }
        }
        let path = self
            .capture_dir
            .join(format!("aether-test-{}.tar.gz", stamp()));
        crate::bundle::write_tar_gz(&path, &files, (finished_at / 1000) as u64)
            .map_err(|e| ApiError::new(E_INTERNAL, format!("{}: {e}", path.display())))?;
        let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        self.log(
            "info",
            None,
            &format!("radio test finished; bundle written: {}", path.display()),
        );
        Ok(json!({ "path": path, "bytes": bytes, "summary": summary }))
    }

    /// The plain-text summary: short enough to paste into a chat.
    fn test_summary(&self, t: &TestSession, finished_at: i64) -> String {
        use std::fmt::Write as _;
        let mut o = String::new();
        let at = |ms: i64| format!("+{:.1} s", (ms - t.started_at) as f64 / 1000.0);
        let _ = writeln!(o, "Aether DV radio test summary");
        let _ = writeln!(
            o,
            "Test: {}",
            if t.description.is_empty() {
                "(no description)"
            } else {
                &t.description
            }
        );
        let _ = writeln!(
            o,
            "Engine {} on {} {}; station {}; {:.0} s from {} (unix ms)",
            crate::station::VERSION,
            std::env::consts::OS,
            std::env::consts::ARCH,
            self.station.callsign,
            (finished_at - t.started_at) as f64 / 1000.0,
            t.started_at
        );
        if !t.markers.is_empty() {
            let _ = writeln!(o, "\nYour markers:");
            for (m_at, m) in &t.markers {
                let _ = writeln!(o, "  {:>9}  {m}", at(*m_at));
            }
        }
        let mut names: Vec<&String> = self.ports.keys().collect();
        names.sort();
        let _ = writeln!(o, "\nPorts now:");
        for n in names {
            let p = &self.ports[n];
            let _ = writeln!(o, "  {n}: {} {}", p.kind, p.state);
            let r = self.route_json(n);
            if !r.is_null() {
                let calls = &r["observed"]["calls"];
                let radio = if calls.is_null() {
                    "radio's call signs unknown (no CI-V reading)".to_string()
                } else {
                    let c = |k: &str| calls[k].as_str().unwrap_or("").trim_end().to_string();
                    // Icom radios keep R1/R2 with duplex off but do not send them.
                    let (dup, sent) = match r["observed"]["duplex"].as_bool() {
                        Some(true) => ("duplex on", ""),
                        Some(false) => ("simplex", ", R1/R2 stored, not sent on simplex"),
                        None => ("duplex unknown", ""),
                    };
                    format!(
                        "radio UR \"{}\" R1 \"{}\" R2 \"{}\" ({dup}{sent}; read over CI-V)",
                        c("ur"),
                        c("r1"),
                        c("r2")
                    )
                };
                let _ = writeln!(
                    o,
                    "    route: {}{}, sending {}{}; {radio}",
                    r["scope"].as_str().unwrap_or("?"),
                    r["repeater"]
                        .as_str()
                        .map(|x| format!(" via {x}"))
                        .unwrap_or_default(),
                    r["gate"].as_str().unwrap_or("?"),
                    r["code"]
                        .as_str()
                        .map(|c| format!(" ({c})"))
                        .unwrap_or_default(),
                );
            }
            let c = self.channel_json(n);
            if !c.is_null() {
                let used = c["airUsedMs"].as_u64().unwrap_or(0) as f64 / 1000.0;
                let air = match c["airBudgetMs"].as_u64() {
                    Some(b) if b > 0 => {
                        format!("air {used:.1} s of {:.0} s allowed", b as f64 / 1000.0)
                    }
                    _ => format!("air {used:.1} s (no air-time limit on this path)"),
                };
                let sources: Vec<&str> = c["sources"]
                    .as_array()
                    .map(|a| a.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                let _ = writeln!(
                    o,
                    "    channel: {}{}; sensing {}; {air}",
                    c["state"].as_str().unwrap_or("?"),
                    c["why"]
                        .as_str()
                        .map(|w| format!(" ({w})"))
                        .unwrap_or_default(),
                    if sources.is_empty() {
                        "nothing".into()
                    } else {
                        sources.join(", ")
                    },
                );
            }
        }
        // Warnings and errors from the engine log during the test.
        if let Ok(evs) = self.store.events(None, 5_000) {
            let bad: Vec<_> = evs
                .iter()
                .rev()
                .filter(|(_, at, level, ..)| {
                    *at >= t.started_at && matches!(level.as_str(), "warn" | "error")
                })
                .collect();
            if !bad.is_empty() {
                let _ = writeln!(o, "\nWarnings and errors ({}):", bad.len());
                for (_, ev_at, level, _, port, msg) in bad.iter().take(40) {
                    let _ = writeln!(
                        o,
                        "  {:>9}  {level} {}{msg}",
                        at(*ev_at),
                        port.as_deref()
                            .map(|p| format!("[{p}] "))
                            .unwrap_or_default()
                    );
                }
            }
        }
        for (_, path) in &t.captures {
            match crate::capture::read(path) {
                Ok((h, recs)) => {
                    let _ = writeln!(o, "\n----------------------------------------");
                    o.push_str(&crate::capture::report(&h, &recs));
                }
                Err(e) => {
                    let _ = writeln!(o, "\ncapture {}: {e}", path.display());
                }
            }
        }
        o
    }

    fn support_bundle(&mut self, include_captures: bool) -> Result<Value, ApiError> {
        let mut files = self.bundle_files()?;
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

    /// Everything a bundle holds except captures.
    fn bundle_files(&mut self) -> Result<Vec<(String, Vec<u8>)>, ApiError> {
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
            .map(|t| transfer_json(t, self.saved_files.get(&t.id)))
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
        Ok(files)
    }
}
