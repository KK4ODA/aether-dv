//! Legacy D-RATS station behaviour for chat (session 1) and RPC (session 2),
//! as a pure function of incoming frames. No I/O; the engine performs the
//! returned [`Effect`]s.
//!
//! Sources (D-Rats @d0e7a39): `sessionmgr.py` `incoming` (address filter,
//! loop drop), `sessions/chat.py` (types 0-5, ping/echo replies with random
//! delays for broadcasts), `sessions/rpc.py` (`RPCGetVersion` reply dict).
//! Aether differences: broadcast ping/echo replies are rate-limited per port
//! (legacy defect P13, reply storms); the version reply carries an `aether`
//! capability key (ADR-0004).

use std::collections::HashMap;

use aether_ddt2::session::{self, chat, id, rpc, CQCQCQ};
use aether_ddt2::{Frame, PayloadEncoding};

pub type Millis = u64;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Capability token advertised to peers (ADR-0004). Nothing optional is
/// implemented yet, so every feature flag is off.
pub fn capability_token() -> String {
    format!("{VERSION};alp=0;rcpt=0;fd=unknown")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Transmit `frame` on `port` after `delay_ms`.
    Send {
        port: String,
        frame: Frame,
        encoding: PayloadEncoding,
        delay_ms: Millis,
    },
    /// A chat-session line to record and show.
    Chat {
        port: String,
        src: String,
        dst: String,
        kind: &'static str,
        body: String,
    },
    /// `callsign` was heard on `port`.
    Heard { port: String, callsign: String },
    /// Software/version learned from a ping or version reply.
    Software { callsign: String, text: String },
    /// Something worth an engine-log entry.
    Log {
        level: &'static str,
        message: String,
    },
}

#[derive(Debug)]
pub struct Station {
    pub callsign: String,
    os: String,
    /// Last time we answered a broadcast ping/echo on each port.
    last_broadcast_reply: HashMap<String, Millis>,
    rng: u64,
    /// Minimum spacing of broadcast ping replies per port (P13).
    pub broadcast_reply_interval: Millis,
}

impl Station {
    pub fn new(callsign: &str, seed: u64) -> Self {
        Station {
            callsign: callsign.to_ascii_uppercase(),
            os: std::env::consts::OS.to_string(),
            last_broadcast_reply: HashMap::new(),
            rng: seed | 1,
            broadcast_reply_interval: 60_000,
        }
    }

    fn rand_below(&mut self, n: u64) -> u64 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        if n == 0 {
            0
        } else {
            self.rng % n
        }
    }

    pub fn ping_text(&self) -> String {
        format!(
            "Running Aether DV {VERSION} ({}), D-RATS compatible",
            self.os
        )
    }

    fn frame(&self, session: u8, kind: u8, seq: u16, dst: &str, payload: Vec<u8>) -> Frame {
        Frame::new(seq, session, kind, &self.callsign, dst, payload)
    }

    /// A chat line to transmit (legacy chat frames are never compressed).
    pub fn chat_frame(&self, dst: &str, body: &str) -> (Frame, PayloadEncoding) {
        (
            self.frame(id::CHAT, chat::TEXT, 0, dst, body.as_bytes().to_vec()),
            PayloadEncoding::Never,
        )
    }

    pub fn ping_frame(&self, dst: &str) -> (Frame, PayloadEncoding) {
        (
            self.frame(id::CHAT, chat::PING_REQ, 0, dst, b"Ping Request".to_vec()),
            PayloadEncoding::Never,
        )
    }

    pub fn version_request(&self, dst: &str, job_id: u16) -> (Frame, PayloadEncoding) {
        let payload = session::encode_rpc_request("RPCGetVersion", &[]);
        (
            self.frame(id::RPC, rpc::REQUEST, job_id, dst, payload),
            PayloadEncoding::IfSmaller,
        )
    }

    fn broadcast_reply_allowed(&mut self, port: &str, now: Millis) -> bool {
        match self.last_broadcast_reply.get(port) {
            Some(&t) if now.saturating_sub(t) < self.broadcast_reply_interval => false,
            _ => {
                self.last_broadcast_reply.insert(port.to_string(), now);
                true
            }
        }
    }

    /// Handle one decoded frame received on `port`.
    pub fn on_frame(&mut self, now: Millis, port: &str, f: &Frame) -> Vec<Effect> {
        let src = String::from_utf8_lossy(&f.src).into_owned();
        let dst = String::from_utf8_lossy(&f.dst).into_owned();
        let mut out = Vec::new();
        if src == "!" {
            return out; // warm-up frame
        }
        if src.eq_ignore_ascii_case(&self.callsign) {
            out.push(Effect::Log {
                level: "debug",
                message: format!("dropped looped frame from {src}"),
            });
            return out;
        }
        out.push(Effect::Heard {
            port: port.into(),
            callsign: src.clone(),
        });
        let for_me = dst.eq_ignore_ascii_case(&self.callsign);
        let broadcast = dst == CQCQCQ;
        // sessionmgr.py: session 1 is accepted whatever the destination.
        if !(for_me || broadcast || f.session == id::CHAT) {
            return out;
        }
        let body = String::from_utf8_lossy(&f.payload).into_owned();
        match (f.session, f.kind) {
            (id::CHAT, chat::TEXT) => {
                let kind = if body.starts_with("$GP") || body.starts_with("$$CRC") {
                    "raw"
                } else {
                    "text"
                };
                out.push(Effect::Chat {
                    port: port.into(),
                    src,
                    dst,
                    kind,
                    body,
                });
            }
            (id::CHAT, chat::PING_REQ) => {
                out.push(Effect::Chat {
                    port: port.into(),
                    src: src.clone(),
                    dst: dst.clone(),
                    kind: "ping_req",
                    body,
                });
                let delay = if broadcast {
                    if !self.broadcast_reply_allowed(port, now) {
                        out.push(Effect::Log {
                            level: "info",
                            message: format!("broadcast ping from {src} not answered (rate limit)"),
                        });
                        return out;
                    }
                    self.rand_below(5_001)
                } else if for_me {
                    0
                } else {
                    return out;
                };
                let reply = self.frame(
                    id::CHAT,
                    chat::PING_RSP,
                    0,
                    &src,
                    self.ping_text().into_bytes(),
                );
                out.push(Effect::Send {
                    port: port.into(),
                    frame: reply,
                    encoding: PayloadEncoding::Never,
                    delay_ms: delay,
                });
            }
            (id::CHAT, chat::PING_RSP) => {
                out.push(Effect::Software {
                    callsign: src.clone(),
                    text: body.clone(),
                });
                out.push(Effect::Chat {
                    port: port.into(),
                    src,
                    dst,
                    kind: "ping_rsp",
                    body,
                });
            }
            (id::CHAT, chat::ECHO_REQ) => {
                let delay = if broadcast {
                    if !self.broadcast_reply_allowed(port, now) {
                        return out;
                    }
                    self.rand_below(10_001)
                } else if for_me {
                    0
                } else {
                    return out;
                };
                let reply = self.frame(id::CHAT, chat::ECHO_RSP, 0, &src, f.payload.clone());
                out.push(Effect::Send {
                    port: port.into(),
                    frame: reply,
                    encoding: PayloadEncoding::Never,
                    delay_ms: delay,
                });
                out.push(Effect::Chat {
                    port: port.into(),
                    src,
                    dst,
                    kind: "echo_req",
                    body: format!("{} bytes", f.payload.len()),
                });
            }
            (id::CHAT, chat::ECHO_RSP) => {
                out.push(Effect::Chat {
                    port: port.into(),
                    src,
                    dst,
                    kind: "echo_rsp",
                    body: format!("{} bytes", f.payload.len()),
                });
            }
            (id::CHAT, chat::STATUS) => {
                out.push(Effect::Chat {
                    port: port.into(),
                    src,
                    dst,
                    kind: "status",
                    body,
                });
            }
            (id::RPC, rpc::REQUEST) if for_me => match session::decode_rpc_request(&f.payload) {
                Ok((job, _args)) if job == "RPCGetVersion" => {
                    let caps = capability_token();
                    let dict = session::encode_rpc_dict(&[
                        ("version", &format!("Aether DV {VERSION}")),
                        ("os", &self.os),
                        ("pyver", "n/a"),
                        ("pygtkver", "None"),
                        ("gtkver", "n/a"),
                        ("aether", &caps),
                    ]);
                    let reply = self.frame(id::RPC, rpc::RESPONSE, f.seq, &src, dict);
                    out.push(Effect::Send {
                        port: port.into(),
                        frame: reply,
                        encoding: PayloadEncoding::IfSmaller,
                        delay_ms: 0,
                    });
                    out.push(Effect::Log {
                        level: "info",
                        message: format!("answered version request from {src}"),
                    });
                }
                Ok((job, _)) => {
                    // Legacy logs unknown/unsupported jobs and does not answer.
                    out.push(Effect::Log {
                        level: "info",
                        message: format!("RPC {job} from {src} not supported yet; not answered"),
                    });
                }
                Err(_) => out.push(Effect::Log {
                    level: "warn",
                    message: format!("malformed RPC from {src}"),
                }),
            },
            (id::RPC, rpc::RESPONSE) if for_me => {
                if let Ok(dict) = session::decode_rpc_dict(&f.payload) {
                    let get = |k: &str| dict.iter().find(|(kk, _)| kk == k).map(|(_, v)| v.clone());
                    if let Some(v) = get("version") {
                        let text = match get("aether") {
                            Some(caps) => format!("{v} [aether {caps}]"),
                            None => format!("D-RATS {v}"),
                        };
                        out.push(Effect::Software {
                            callsign: src,
                            text,
                        });
                    }
                }
            }
            (id::CONTROL, k) if for_me && k >= aether_ddt2::session::control::NEW_BASE => {
                // Stateful sessions (file/form transfer) are not implemented in
                // this slice. Legacy behaviour for an unsupported type is to
                // send no ACK; the opener gives up after its retries (SM-3).
                out.push(Effect::Log {
                    level: "warn",
                    message: format!(
                        "session request type {k} from {src} refused (not implemented yet)"
                    ),
                });
            }
            _ => {}
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(session: u8, kind: u8, src: &str, dst: &str, body: &[u8]) -> Frame {
        Frame::new(0, session, kind, src, dst, body.to_vec())
    }

    fn sends(e: &[Effect]) -> Vec<&Frame> {
        e.iter()
            .filter_map(|x| {
                if let Effect::Send { frame, .. } = x {
                    Some(frame)
                } else {
                    None
                }
            })
            .collect()
    }

    #[test]
    fn broadcast_chat_is_recorded() {
        let mut s = Station::new("KK4ODA", 1);
        let e = s.on_frame(0, "rf", &frame(1, 0, "W4DRS", "CQCQCQ", b"net open"));
        assert!(e.contains(&Effect::Chat {
            port: "rf".into(),
            src: "W4DRS".into(),
            dst: "CQCQCQ".into(),
            kind: "text",
            body: "net open".into()
        }));
        assert!(e.contains(&Effect::Heard {
            port: "rf".into(),
            callsign: "W4DRS".into()
        }));
    }

    #[test]
    fn gps_sentence_in_chat_is_marked_raw() {
        let mut s = Station::new("KK4ODA", 1);
        let e = s.on_frame(
            0,
            "rf",
            &frame(
                1,
                0,
                "N4SHL",
                "CQCQCQ",
                b"$$CRC1234,N4SHL>APRATS,DSTAR*:/120000h3417.88N/08349.44W>\r",
            ),
        );
        assert!(e
            .iter()
            .any(|x| matches!(x, Effect::Chat { kind: "raw", .. })));
    }

    #[test]
    fn looped_and_warmup_frames_are_dropped() {
        let mut s = Station::new("KK4ODA", 1);
        assert!(!s
            .on_frame(0, "rf", &frame(1, 0, "KK4ODA", "CQCQCQ", b"x"))
            .iter()
            .any(|x| matches!(x, Effect::Chat { .. })));
        assert!(s
            .on_frame(0, "rf", &frame(0, 254, "!", "!", &[1; 16]))
            .is_empty());
    }

    #[test]
    fn direct_ping_is_answered_immediately_with_version_text() {
        let mut s = Station::new("KK4ODA", 1);
        let e = s.on_frame(0, "rf", &frame(1, 1, "W4DRS", "KK4ODA", b"Ping Request"));
        let replies = sends(&e);
        assert_eq!(replies.len(), 1);
        assert_eq!(
            (replies[0].kind, replies[0].dst.as_slice()),
            (2, b"W4DRS".as_slice())
        );
        assert!(String::from_utf8_lossy(&replies[0].payload).starts_with("Running Aether DV"));
        assert!(e.iter().any(|x| matches!(
            x,
            Effect::Send {
                delay_ms: 0,
                encoding: PayloadEncoding::Never,
                ..
            }
        )));
    }

    #[test]
    fn ping_for_someone_else_is_not_answered() {
        let mut s = Station::new("KK4ODA", 1);
        let e = s.on_frame(0, "rf", &frame(1, 1, "W4DRS", "N4SHL", b"Ping Request"));
        assert!(sends(&e).is_empty());
    }

    #[test]
    fn broadcast_ping_replies_are_delayed_and_rate_limited_p13() {
        let mut s = Station::new("KK4ODA", 7);
        let e = s.on_frame(0, "rf", &frame(1, 1, "W4DRS", "CQCQCQ", b"Ping Request"));
        let Some(Effect::Send { delay_ms, .. }) =
            e.iter().find(|x| matches!(x, Effect::Send { .. }))
        else {
            panic!()
        };
        assert!(*delay_ms <= 5_000);
        let e2 = s.on_frame(
            30_000,
            "rf",
            &frame(1, 1, "N4SHL", "CQCQCQ", b"Ping Request"),
        );
        assert!(
            sends(&e2).is_empty(),
            "second broadcast ping within 60 s must not be answered"
        );
        let e3 = s.on_frame(
            61_000,
            "rf",
            &frame(1, 1, "N4SHL", "CQCQCQ", b"Ping Request"),
        );
        assert_eq!(sends(&e3).len(), 1);
        // a different port has its own budget
        assert_eq!(
            sends(&s.on_frame(
                30_000,
                "net",
                &frame(1, 1, "K4EOC", "CQCQCQ", b"Ping Request")
            ))
            .len(),
            1
        );
    }

    #[test]
    fn echo_returns_the_same_payload() {
        let mut s = Station::new("KK4ODA", 1);
        let e = s.on_frame(0, "rf", &frame(1, 3, "W4DRS", "KK4ODA", b"0123456789"));
        let r = sends(&e);
        assert_eq!(
            (r[0].kind, r[0].payload.as_slice()),
            (4, b"0123456789".as_slice())
        );
    }

    #[test]
    fn version_request_gets_legacy_fields_plus_aether_caps() {
        let mut s = Station::new("KK4ODA", 1);
        let req = Frame::new(
            42,
            2,
            0,
            "W4DRS",
            "KK4ODA",
            session::encode_rpc_request("RPCGetVersion", &[]),
        );
        let e = s.on_frame(0, "rf", &req);
        let r = sends(&e);
        assert_eq!(
            (r[0].session, r[0].kind, r[0].seq),
            (2, 1, 42),
            "reply carries the job id in seq"
        );
        let dict = session::decode_rpc_dict(&r[0].payload).unwrap();
        let keys: Vec<&str> = dict.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            vec!["version", "os", "pyver", "pygtkver", "gtkver", "aether"]
        );
    }

    #[test]
    fn version_replies_identify_legacy_and_aether_peers() {
        let mut s = Station::new("KK4ODA", 1);
        let legacy = Frame::new(
            1,
            2,
            1,
            "W4DRS",
            "KK4ODA",
            session::encode_rpc_dict(&[("version", "0.4.2"), ("os", "Linux")]),
        );
        assert!(s.on_frame(0, "rf", &legacy).contains(&Effect::Software {
            callsign: "W4DRS".into(),
            text: "D-RATS 0.4.2".into()
        }));
        let aether = Frame::new(
            1,
            2,
            1,
            "KJ4VTR",
            "KK4ODA",
            session::encode_rpc_dict(&[("version", "Aether DV 0.0.1"), ("aether", "0.0.1;alp=0")]),
        );
        assert!(s
            .on_frame(0, "rf", &aether)
            .iter()
            .any(|x| matches!(x, Effect::Software { text, .. } if text.contains("[aether"))));
    }

    #[test]
    fn unsupported_session_request_is_not_acked() {
        let mut s = Station::new("KK4ODA", 1);
        let new_file = Frame::new(
            0,
            0,
            3 + 5,
            "W4DRS",
            "KK4ODA",
            session::encode_control_new(5, "xfer"),
        );
        let e = s.on_frame(0, "rf", &new_file);
        assert!(sends(&e).is_empty());
        assert!(e
            .iter()
            .any(|x| matches!(x, Effect::Log { level: "warn", .. })));
    }
}
