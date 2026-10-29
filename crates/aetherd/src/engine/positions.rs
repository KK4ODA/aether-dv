//! Position reports: received in chat lines (NMEA style or DPRS), or
//! emitted by the radio itself outside frames (GPS-TX); and our own fixed
//! position, sent only when asked.

use aether_geo::{Format, Report};
use aether_store::PositionRow;

use super::*;

#[derive(Debug)]
pub enum PosCmd {
    /// History of one station, or the latest of every station.
    List {
        callsign: Option<String>,
        since: Option<i64>,
    },
    /// Send our configured position on `port` as `dprs` (default) or `nmea`.
    Send {
        port: String,
        format: Option<String>,
    },
}

fn position_json(p: &PositionRow) -> Value {
    json!({
        "callsign": p.callsign, "receivedAt": p.received_at, "lat": p.lat, "lon": p.lon,
        "altM": p.alt_m, "courseDeg": p.course, "speedKmh": p.speed_kmh, "symbol": p.symbol,
        "comment": p.comment, "sourceFmt": p.source_fmt,
    })
}

impl Engine {
    /// Parse a possible position sentence. `from` is the sending station
    /// for chat lines, `None` for text the radio emitted itself.
    pub(super) fn on_position_text(&mut self, port: &str, from: Option<&str>, text: &str) {
        let origin = from.unwrap_or("the radio");
        match aether_geo::parse(text) {
            None => {}
            Some(Err(e)) => self.log(
                "info",
                Some(port),
                &format!("position report from {origin} not used: {e}"),
            ),
            Some(Ok(p)) => {
                let now = unix_ms();
                let fix_time = p.fix_utc_secs.map(|s| {
                    // Time of day only: take the most recent such instant.
                    let day = now.div_euclid(86_400_000) * 86_400_000;
                    let t = day + s as i64 * 1000;
                    if t > now + 60_000 {
                        t - 86_400_000
                    } else {
                        t
                    }
                });
                let row = PositionRow {
                    callsign: p.station.to_ascii_uppercase(),
                    port_id: Some(port.to_string()),
                    received_at: now,
                    fix_time,
                    lat: p.lat,
                    lon: p.lon,
                    alt_m: p.alt_m,
                    course: p.course_deg,
                    speed_kmh: p.speed_kmh,
                    symbol: p.symbol.clone(),
                    comment: Some(p.comment.clone()).filter(|c| !c.is_empty()),
                    source_fmt: p.format.as_str().into(),
                    raw: Some(text.trim_end().to_string()),
                };
                match self.store.insert_position(&row) {
                    Ok(_) => self.publish("station.position", position_json(&row)),
                    Err(e) => self.log("error", Some(port), &format!("store position: {e}")),
                }
            }
        }
    }

    pub(super) async fn on_position_cmd(&mut self, c: PosCmd) -> Result<Value, ApiError> {
        match c {
            PosCmd::List { callsign, since } => self
                .store
                .positions(callsign.map(|c| c.to_ascii_uppercase()).as_deref(), since)
                .map(|v| Value::Array(v.iter().map(position_json).collect()))
                .map_err(|e| ApiError::new(E_INTERNAL, e.to_string())),
            PosCmd::Send { port, format } => {
                let pos = self.own_position.clone().ok_or_else(|| {
                    ApiError::new(E_INVALID, "no [position] in the configuration")
                })?;
                if !self.ports.contains_key(&port) {
                    return Err(ApiError::new(E_NOT_FOUND, format!("no port {port}")));
                }
                let fmt = match format.as_deref().unwrap_or("dprs") {
                    "dprs" => Format::Dprs,
                    "nmea" => Format::Nmea,
                    f => {
                        return Err(ApiError::new(
                            E_INVALID,
                            format!("format must be dprs or nmea, not {f}"),
                        ))
                    }
                };
                let now = unix_ms();
                let report = Report {
                    station: &self.station.callsign,
                    lat: pos.lat,
                    lon: pos.lon,
                    alt_m: pos.alt_m,
                    symbol: &pos.symbol,
                    comment: &pos.comment,
                    utc_secs: (now.rem_euclid(86_400_000) / 1000) as u32,
                };
                let text = match fmt {
                    Format::Dprs => aether_geo::format_dprs(&report),
                    Format::Nmea => aether_geo::format_nmea(&report),
                };
                let (frame, enc) = self.station.chat_frame(aether_ddt2::session::CQCQCQ, &text);
                self.send_frame(&port, &frame, enc, Priority::Chat, None)
                    .await?;
                let row = PositionRow {
                    callsign: self.station.callsign.clone(),
                    port_id: Some(port.clone()),
                    received_at: now,
                    fix_time: Some(now),
                    lat: pos.lat,
                    lon: pos.lon,
                    alt_m: pos.alt_m,
                    course: None,
                    speed_kmh: None,
                    symbol: Some(pos.symbol.clone()),
                    comment: Some(pos.comment.clone()),
                    source_fmt: "manual".into(),
                    raw: Some(text.trim_end().to_string()),
                };
                let _ = self.store.insert_position(&row);
                self.publish("station.position", position_json(&row));
                Ok(json!({ "portId": port, "format": fmt.as_str(), "text": text }))
            }
        }
    }
}
