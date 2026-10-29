//! Legacy session numbering and payload encodings carried inside DDT2 frames.
//! Source: `d_rats/sessions/*.py@d0e7a39`, `d_rats/mainapp.py:718,726`.

/// Session ids are positional, not negotiated: a D-RATS station registers
/// control, then chat, then RPC, so these numbers are fixed on the wire.
pub mod id {
    pub const CONTROL: u8 = 0;
    pub const CHAT: u8 = 1;
    pub const RPC: u8 = 2;
    /// First id available for negotiated (stateful) sessions in Aether.
    pub const FIRST_DYNAMIC: u8 = 4;
}

/// Broadcast destination.
pub const CQCQCQ: &str = "CQCQCQ";

/// Control session (id 0) frame types (`sessions/control.py`).
pub mod control {
    pub const PING: u8 = 0;
    pub const END: u8 = 1;
    pub const ACK: u8 = 2;
    pub const NEW_BASE: u8 = 3;
    /// Session classes added to NEW_BASE (`sessions/base.py`).
    pub const CLASS_GENERAL: u8 = 1;
    pub const CLASS_SOCKET: u8 = 4;
    pub const CLASS_FILE: u8 = 5;
    pub const CLASS_FORM: u8 = 6;
}

/// Chat session (id 1) frame types (`sessions/chat.py`). Sent uncompressed.
pub mod chat {
    pub const TEXT: u8 = 0; // also carries NMEA / DPRS position sentences
    pub const PING_REQ: u8 = 1;
    pub const PING_RSP: u8 = 2;
    pub const ECHO_REQ: u8 = 3;
    pub const ECHO_RSP: u8 = 4;
    pub const STATUS: u8 = 5;
}

/// Stateful session frame types (`sessions/stateful.py`).
pub mod stateful {
    pub const ACK: u8 = 1;
    pub const NAK: u8 = 2; // defined, never sent by legacy
    pub const DATA: u8 = 4;
    pub const REQACK: u8 = 5;
}

/// RPC session (id 2) frame types and separators (`sessions/rpc.py`).
pub mod rpc {
    pub const REQUEST: u8 = 0;
    pub const RESPONSE: u8 = 1;
    pub const GS: u8 = 0x1D;
    pub const RS: u8 = 0x1E;
    pub const US: u8 = 0x1F;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PayloadError {
    Malformed(&'static str),
}

/// Control NEW: requester's session id + UTF-8 session name.
pub fn encode_control_new(local_id: u8, name: &str) -> Vec<u8> {
    let mut v = vec![local_id];
    v.extend_from_slice(name.as_bytes());
    v
}

pub fn decode_control_new(p: &[u8]) -> Result<(u8, String), PayloadError> {
    let (&id, rest) = p
        .split_first()
        .ok_or(PayloadError::Malformed("empty NEW"))?;
    Ok((id, String::from_utf8_lossy(rest).into_owned()))
}

/// Control ACK: (requester's id, responder's id).
pub fn encode_control_ack(requester_id: u8, responder_id: u8) -> Vec<u8> {
    vec![requester_id, responder_id]
}

pub fn decode_control_ack(p: &[u8]) -> Result<(u8, u8), PayloadError> {
    match p {
        [a, b] => Ok((*a, *b)),
        _ => Err(PayloadError::Malformed("ACK must be 2 bytes")),
    }
}

/// Control END: the session id as ASCII decimal.
pub fn encode_control_end(session_id: u8) -> Vec<u8> {
    session_id.to_string().into_bytes()
}

pub fn decode_control_end(p: &[u8]) -> Result<u8, PayloadError> {
    std::str::from_utf8(p)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .ok_or(PayloadError::Malformed("END id"))
}

/// Stateful ACK / REQACK: one byte per 8-bit block sequence number.
pub fn encode_seq_list(seqs: &[u8]) -> Vec<u8> {
    seqs.to_vec()
}

/// File/form offer: **little-endian** u32 length of the zlib stream + UTF-8
/// base filename. Every other DDT2 integer is big-endian (D-Rats#111, P15).
pub fn encode_file_offer(wire_len: u32, filename: &str) -> Vec<u8> {
    let mut v = wire_len.to_le_bytes().to_vec();
    v.extend_from_slice(filename.as_bytes());
    v
}

pub fn decode_file_offer(p: &[u8]) -> Result<(u32, String), PayloadError> {
    if p.len() < 4 {
        return Err(PayloadError::Malformed("offer shorter than 4 bytes"));
    }
    let len = u32::from_le_bytes([p[0], p[1], p[2], p[3]]);
    let name = String::from_utf8_lossy(&p[4..]).into_owned();
    // Only a base name is meaningful; refuse path components (legacy P11 class).
    if name.contains('/') || name.contains('\\') || name == ".." || name.is_empty() {
        return Err(PayloadError::Malformed(
            "offer filename is not a plain base name",
        ));
    }
    Ok((len, name))
}

/// Receiver's answer to an offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OfferReply {
    Ok,
    Resume(u64),
}

pub fn encode_offer_reply(r: &OfferReply) -> Vec<u8> {
    match r {
        OfferReply::Ok => b"OK".to_vec(),
        OfferReply::Resume(n) => format!("RESUME:{n}").into_bytes(),
    }
}

pub fn decode_offer_reply(p: &[u8]) -> Result<OfferReply, PayloadError> {
    if p == b"OK" {
        return Ok(OfferReply::Ok);
    }
    p.strip_prefix(b"RESUME:")
        .and_then(|n| std::str::from_utf8(n).ok())
        .and_then(|n| n.parse().ok())
        .map(OfferReply::Resume)
        .ok_or(PayloadError::Malformed("offer reply"))
}

/// RPC request payload: `JobClass GS key US value RS key US value ...`.
pub fn encode_rpc_request(job: &str, args: &[(&str, &str)]) -> Vec<u8> {
    let mut v = job.as_bytes().to_vec();
    v.push(rpc::GS);
    v.extend(encode_rpc_dict(args));
    v
}

pub fn encode_rpc_dict(args: &[(&str, &str)]) -> Vec<u8> {
    let mut v = Vec::new();
    for (i, (k, val)) in args.iter().enumerate() {
        if i > 0 {
            v.push(rpc::RS);
        }
        v.extend_from_slice(k.as_bytes());
        v.push(rpc::US);
        v.extend_from_slice(val.as_bytes());
    }
    v
}

pub fn decode_rpc_dict(p: &[u8]) -> Result<Vec<(String, String)>, PayloadError> {
    if p.is_empty() {
        return Ok(vec![]);
    }
    p.split(|&b| b == rpc::RS)
        .map(|el| {
            let mut it = el.splitn(2, |&b| b == rpc::US);
            match (it.next(), it.next()) {
                (Some(k), Some(v)) if !v.contains(&rpc::US) => Ok((
                    String::from_utf8_lossy(k).into_owned(),
                    String::from_utf8_lossy(v).into_owned(),
                )),
                _ => Err(PayloadError::Malformed("rpc dict element")),
            }
        })
        .collect()
}

pub fn decode_rpc_request(p: &[u8]) -> Result<(String, Vec<(String, String)>), PayloadError> {
    let gs = p
        .iter()
        .position(|&b| b == rpc::GS)
        .ok_or(PayloadError::Malformed("no GS in RPC"))?;
    Ok((
        String::from_utf8_lossy(&p[..gs]).into_owned(),
        decode_rpc_dict(&p[gs + 1..])?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_offer_is_little_endian_p15() {
        let v = encode_file_offer(0x0102_0304, "a.txt");
        assert_eq!(&v[..4], &[0x04, 0x03, 0x02, 0x01]);
        assert_eq!(
            decode_file_offer(&v).unwrap(),
            (0x0102_0304, "a.txt".into())
        );
    }

    #[test]
    fn offer_rejects_paths() {
        for bad in ["../etc/passwd", "a/b", "..", "c\\d", ""] {
            assert!(
                decode_file_offer(&encode_file_offer(1, bad)).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn offer_reply_roundtrip() {
        for r in [
            OfferReply::Ok,
            OfferReply::Resume(0),
            OfferReply::Resume(123_456),
        ] {
            assert_eq!(decode_offer_reply(&encode_offer_reply(&r)).unwrap(), r);
        }
        assert!(decode_offer_reply(b"RESUME:x").is_err());
    }

    #[test]
    fn control_payloads() {
        assert_eq!(
            decode_control_new(&encode_control_new(7, "xfer")).unwrap(),
            (7, "xfer".into())
        );
        assert_eq!(
            decode_control_ack(&encode_control_ack(7, 4)).unwrap(),
            (7, 4)
        );
        assert_eq!(decode_control_end(&encode_control_end(42)).unwrap(), 42);
        assert!(decode_control_ack(&[1]).is_err());
    }

    #[test]
    fn rpc_get_version_roundtrip() {
        let req = encode_rpc_request("RPCGetVersion", &[]);
        assert_eq!(req, b"RPCGetVersion\x1d");
        assert_eq!(
            decode_rpc_request(&req).unwrap(),
            ("RPCGetVersion".into(), vec![])
        );
        let reply = encode_rpc_dict(&[("version", "0.4.2"), ("aether", "1;alp=1")]);
        assert_eq!(
            decode_rpc_dict(&reply).unwrap(),
            vec![
                ("version".into(), "0.4.2".into()),
                ("aether".into(), "1;alp=1".into())
            ]
        );
    }

    #[test]
    fn session_type_numbers_match_sniffer_table() {
        // d_rats/sessions/sniff.py: {4: General, 5: File(old), 6: Form(old), 7: Socket, 8: PFile, 9: PForm}
        assert_eq!(control::NEW_BASE + control::CLASS_GENERAL, 4);
        assert_eq!(control::NEW_BASE + control::CLASS_SOCKET, 7);
        assert_eq!(control::NEW_BASE + control::CLASS_FILE, 8);
        assert_eq!(control::NEW_BASE + control::CLASS_FORM, 9);
    }
}
