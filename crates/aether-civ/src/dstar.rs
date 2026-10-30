//! D-STAR call signs, receive status, squelch and duplex over CI-V.
//!
//! Layouts are from the Icom *IC-9700 CI-V Reference Guide* (pp. 13-20,
//! "DV TX call signs", "DV MY call sign", "DV RX call sign data", "DV RX
//! message", "DV RX Status"). Other models are [`crate::Support::Unverified`]
//! until checked against their own guides (docs/10 §1.4, HW-R2).
//!
//! Call-sign fields are fixed width, space padded, characters `0-9 A-Z /`
//! and space.

use crate::{frame, CONTROLLER};

/// `1F 00`: MY call sign (8) + note (4).
pub const MY_CALL: &[u8] = &[0x1F, 0x00];
/// `1F 01`: DV TX call signs UR, R1, R2 (8 each).
pub const TX_CALLS: &[u8] = &[0x1F, 0x01];
/// `20 00 02`: read the last received call signs.
pub const RX_CALLS_READ: &[u8] = &[0x20, 0x00, 0x02];
/// `20 00 01`: the same data, pushed by the radio (CI-V transceive).
pub const RX_CALLS_PUSH: &[u8] = &[0x20, 0x00, 0x01];
/// `20 01 02`: read the last received 20-character message.
pub const RX_MSG_READ: &[u8] = &[0x20, 0x01, 0x02];
/// `20 01 01`: the same, pushed.
pub const RX_MSG_PUSH: &[u8] = &[0x20, 0x01, 0x01];
/// `20 02 02`: read the DV RX status byte.
pub const RX_STATUS_READ: &[u8] = &[0x20, 0x02, 0x02];
/// `20 02 01`: the same, pushed.
pub const RX_STATUS_PUSH: &[u8] = &[0x20, 0x02, 0x01];
/// `15 01`: noise / S-meter squelch status (`00` closed, `01` open).
pub const SQUELCH: &[u8] = &[0x15, 0x01];
/// `0F`: split/duplex (`00` split off, `01` split on, `11` DUP-, `12`
/// DUP+, `13` DD repeater simplex).
pub const DUPLEX: &[u8] = &[0x0F];
/// Transceive: operating frequency changed (`00`) and mode changed (`01`).
pub const FREQ_PUSH: &[u8] = &[0x00];
pub const MODE_PUSH: &[u8] = &[0x01];

/// A read request for `cmd`.
pub fn read(radio: u8, cmd: &[u8]) -> Vec<u8> {
    frame(radio, CONTROLLER, cmd)
}

fn valid_char(c: u8) -> bool {
    c.is_ascii_digit() || c.is_ascii_uppercase() || c == b' ' || c == b'/'
}

/// Pad or check one fixed-width call-sign field.
pub fn field(s: &str, width: usize) -> Result<String, String> {
    let up = s.to_ascii_uppercase();
    if up.len() > width {
        return Err(format!("'{s}' is longer than {width} characters"));
    }
    if let Some(c) = up.bytes().find(|&c| !valid_char(c)) {
        return Err(format!(
            "'{s}' contains '{}', which a D-STAR call sign field cannot hold",
            c as char
        ));
    }
    Ok(format!("{up:<width$}"))
}

fn text(b: &[u8]) -> String {
    b.iter()
        .map(|&c| {
            if c.is_ascii_graphic() || c == b' ' {
                c as char
            } else {
                '?'
            }
        })
        .collect()
}

/// UR / R1 / R2 as the radio holds them (8 characters each, padded).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxCalls {
    pub ur: String,
    pub r1: String,
    pub r2: String,
}

impl TxCalls {
    pub fn parse(data: &[u8]) -> Option<TxCalls> {
        (data.len() == 24).then(|| TxCalls {
            ur: text(&data[0..8]),
            r1: text(&data[8..16]),
            r2: text(&data[16..24]),
        })
    }

    /// The `1F 01` set request. Fails on fields a radio cannot hold.
    pub fn set_request(&self, radio: u8) -> Result<Vec<u8>, String> {
        let mut body = TX_CALLS.to_vec();
        for f in [&self.ur, &self.r1, &self.r2] {
            body.extend_from_slice(field(f, 8)?.as_bytes());
        }
        Ok(frame(radio, CONTROLLER, &body))
    }
}

/// MY call sign and its 4-character note.
pub fn parse_my_call(data: &[u8]) -> Option<(String, String)> {
    (data.len() == 12).then(|| (text(&data[0..8]), text(&data[8..12])))
}

/// The header of the last received call (`20 00`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RxCalls {
    pub flag1: u8,
    pub flag2: u8,
    pub caller: String,
    pub note: String,
    pub called: String,
    /// The repeater the caller accessed. For a call that came through a
    /// gateway and the internet, the gateway call sign of the repeater
    /// we received it from.
    pub r1: String,
    /// The repeater we received the call from.
    pub r2: String,
}

impl RxCalls {
    /// `None` for the radio's "nothing received since power on" (`FF`).
    pub fn parse(data: &[u8]) -> Option<RxCalls> {
        if data.len() != 38 {
            return None;
        }
        Some(RxCalls {
            flag1: data[0],
            flag2: data[1],
            caller: text(&data[2..10]),
            note: text(&data[10..14]),
            called: text(&data[14..22]),
            r1: text(&data[22..30]),
            r2: text(&data[30..38]),
        })
    }

    /// Flag 1 bit 4: a data call (otherwise voice).
    pub fn is_data(&self) -> bool {
        self.flag1 & 0x10 != 0
    }

    /// Flag 1 bit 3: received through a repeater (otherwise direct).
    pub fn via_repeater(&self) -> bool {
        self.flag1 & 0x08 != 0
    }

    /// JARL control code (flag 2 bits 2..0): `000` none, `001` relay
    /// unavailable, `010` no response, `011` ACK, `100` resend, `110`
    /// auto reply, `111` repeater control.
    pub fn control_code(&self) -> u8 {
        self.flag2 & 0x07
    }

    /// The repeater's acknowledgement of our transmission. MMDVMHost sends
    /// it from the repeater's own call sign (MY = repeater, UR = us, R1 =
    /// gateway) [MMDVMHost `DStarControl.cpp` `sendAck`]; flags vary, so
    /// it is recognised by the call signs.
    pub fn is_ack_from(&self, repeater: &str, me: &str) -> bool {
        self.caller.trim_end() == repeater.trim_end()
            && self.called.trim_end().eq_ignore_ascii_case(me.trim_end())
    }

    /// A call that came from the network: R1 is a gateway (`xxxxxxxG`)
    /// and it is not the repeater's own acknowledgement or announcement.
    pub fn from_network(&self, repeater: &str) -> bool {
        self.r1.as_bytes().get(7) == Some(&b'G') && self.caller.trim_end() != repeater.trim_end()
    }
}

/// The last received 20-character message (`20 01`) and its sender.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RxMessage {
    pub text: String,
    pub caller: String,
    pub note: String,
}

impl RxMessage {
    pub fn parse(data: &[u8]) -> Option<RxMessage> {
        (data.len() == 32).then(|| RxMessage {
            text: text(&data[0..20]),
            caller: text(&data[20..28]),
            note: text(&data[28..32]),
        })
    }
}

/// DV RX status (`20 02`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RxStatus(pub u8);

impl RxStatus {
    pub fn parse(data: &[u8]) -> Option<RxStatus> {
        match data {
            [b] if b & 0x80 == 0 => Some(RxStatus(*b)),
            _ => None,
        }
    }
    /// Receiving a digital voice call (whatever DSQL/CSQL say).
    pub fn voice(self) -> bool {
        self.0 & 0x40 != 0
    }
    /// The last call was finished by this station.
    pub fn last_call_ours(self) -> bool {
        self.0 & 0x20 != 0
    }
    /// Receiving a signal (audio would be heard).
    pub fn signal(self) -> bool {
        self.0 & 0x10 != 0
    }
    /// Receiving a signal other than DV ("DV" and "FM" blinking).
    pub fn other_mode(self) -> bool {
        self.0 & 0x02 != 0
    }
    pub fn packet_loss(self) -> bool {
        self.0 & 0x01 != 0
    }
}

/// Squelch status (`15 01`): open = something is being received.
pub fn parse_squelch(data: &[u8]) -> Option<bool> {
    match data {
        [0] => Some(false),
        [1] => Some(true),
        _ => None,
    }
}

/// Duplex (`0F`): `Some(true)` for DUP- / DUP+, `Some(false)` for simplex.
pub fn parse_duplex(data: &[u8]) -> Option<bool> {
    match data {
        [0x00] | [0x01] | [0x13] => Some(false),
        [0x11] | [0x12] => Some(true),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Deframer, Reply};

    fn reply(radio: u8, body: &[u8]) -> Vec<u8> {
        frame(CONTROLLER, radio, body)
    }

    #[test]
    fn tx_calls_round_trip_and_layout() {
        let c = TxCalls {
            ur: "CQCQCQ".into(),
            r1: "W4DOC  B".into(),
            r2: "w4doc  g".into(),
        };
        let req = c.set_request(0xA2).unwrap();
        assert_eq!(&req[..6], &[0xFE, 0xFE, 0xA2, 0xE0, 0x1F, 0x01]);
        assert_eq!(&req[6..30], b"CQCQCQ  W4DOC  BW4DOC  G");
        assert_eq!(*req.last().unwrap(), 0xFD);
        let parsed = TxCalls::parse(&req[6..30]).unwrap();
        assert_eq!(parsed.r2, "W4DOC  G");
        assert!(TxCalls::parse(&req[6..29]).is_none());
        assert!(field("W4DOC-B", 8).is_err());
        assert!(field("TOOLONGCALL", 8).is_err());
        assert_eq!(field("/W4DOCB", 8).unwrap(), "/W4DOCB ");
    }

    #[test]
    fn reads_and_replies() {
        assert_eq!(
            read(0xA2, RX_STATUS_READ),
            vec![0xFE, 0xFE, 0xA2, 0xE0, 0x20, 0x02, 0x02, 0xFD]
        );
        let mut d = Deframer::default();
        let mut body = TX_CALLS.to_vec();
        body.extend_from_slice(b"CQCQCQ  W4DOC  B        ");
        let f = d.feed(&reply(0xA2, &body)).remove(0);
        let Some(Reply::Data(data)) = f.reply_to(TX_CALLS, 0xA2) else {
            panic!("reply");
        };
        let c = TxCalls::parse(&data).unwrap();
        assert_eq!((c.ur.as_str(), c.r2.as_str()), ("CQCQCQ  ", "        "));

        let mut body = MY_CALL.to_vec();
        body.extend_from_slice(b"KK4ODA  52P ");
        let f = d.feed(&reply(0xA2, &body)).remove(0);
        let Some(Reply::Data(data)) = f.reply_to(MY_CALL, 0xA2) else {
            panic!("reply");
        };
        assert_eq!(
            parse_my_call(&data),
            Some(("KK4ODA  ".into(), "52P ".into()))
        );
    }

    #[test]
    fn rx_calls_from_gateway_and_ack() {
        // IC-9700 guide p.20 example: a gateway call received on repeater.
        let mut data = vec![0x08, 0x00];
        data.extend_from_slice(b"JG3LUK  9700JM1ZLK  JP3YHH GJP3YHH A");
        let c = RxCalls::parse(&data).unwrap();
        assert!(c.via_repeater() && !c.is_data() && c.from_network("JP3YHH A"));
        assert_eq!(c.called, "JM1ZLK  ");
        assert_eq!(c.r2, "JP3YHH A");
        // MMDVMHost ack: MY = repeater, UR = us, R1 = gateway, R2 =
        // repeater, flag "relay unavailable" (001).
        let mut ack = vec![0x00, 0x01];
        ack.extend_from_slice(b"W4DOC  B    KK4ODA  W4DOC  GW4DOC  B");
        let a = RxCalls::parse(&ack).unwrap();
        assert!(a.is_ack_from("W4DOC  B", "KK4ODA"));
        assert!(!a.is_ack_from("W4DOC  C", "KK4ODA"));
        assert!(!a.from_network("W4DOC  B"));
        assert_eq!(a.control_code(), 1);
        assert!(RxCalls::parse(&[0xFF]).is_none());
    }

    #[test]
    fn status_squelch_duplex_message() {
        let s = RxStatus::parse(&[0x50]).unwrap();
        assert!(s.voice() && s.signal() && !s.other_mode());
        assert!(RxStatus::parse(&[0x02]).unwrap().other_mode());
        assert!(RxStatus::parse(&[0xFF]).is_none());
        assert_eq!(parse_squelch(&[1]), Some(true));
        assert_eq!(parse_squelch(&[2]), None);
        assert_eq!(parse_duplex(&[0x12]), Some(true));
        assert_eq!(parse_duplex(&[0x00]), Some(false));
        assert_eq!(parse_duplex(&[0x07]), None);
        let mut m = b"REF001 C  BER: 0.0% ".to_vec();
        m.extend_from_slice(b"W4DOC  B    ");
        let msg = RxMessage::parse(&m).unwrap();
        assert!(msg.text.starts_with("REF001 C"));
        assert_eq!(msg.caller, "W4DOC  B");
    }

    #[test]
    fn pushes_are_recognised() {
        let mut d = Deframer::default();
        let push = frame(0x00, 0xA2, &[0x20, 0x02, 0x01, 0x40]);
        let f = d.feed(&push).remove(0);
        assert_eq!(f.push(RX_STATUS_PUSH, 0xA2), Some(&[0x40u8][..]));
        assert_eq!(f.push(RX_STATUS_PUSH, 0x94), None);
        assert_eq!(f.push(RX_CALLS_PUSH, 0xA2), None);
    }
}
