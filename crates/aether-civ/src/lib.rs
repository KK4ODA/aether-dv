//! `aether-civ`: Icom CI-V frames and the documented DV Fast Data commands.
//! Sans-I/O: build request bytes, feed received bytes, get replies.
//!
//! Frame: `FE FE <to> <from> <cmd> [sub…] [data…] FD`. Replies come from
//! the radio to the controller; `FB` is OK, `FA` is NG (refused). With
//! "CI-V USB Echo Back" on, the radio also echoes our own request, which
//! [`Deframer`] reports like any other frame (callers match on addresses).
//!
//! DV Fast Data commands (verified in the Icom CI-V reference guides; see
//! `docs/03-dstar-rf-transport.md` §2):
//!
//! | Model | Read / set Fast Data |
//! |---|---|
//! | ID-52, ID-52 PLUS, ID-50, ID-5200 | `22 03` + `00`/`01` |
//! | IC-705 | `1A 05 00 93` + `00`/`01` |
//! | IC-9700 | `1A 05 00 77` + `00`/`01` |
//! | IC-905 | `1A 05 00 98` + `00`/`01` |
//!
//! D-STAR call signs, receive status, squelch and duplex: see [`dstar`].

pub mod dstar;

/// The controller (computer) address conventionally used with Icom radios.
pub const CONTROLLER: u8 = 0xE0;

const PREAMBLE: u8 = 0xFE;
const END: u8 = 0xFD;
const OK: u8 = 0xFB;
const NG: u8 = 0xFA;
const COLLISION: u8 = 0xFC;

/// A radio model's CI-V facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Model {
    pub id: &'static str,
    pub name: &'static str,
    /// Factory-default CI-V address.
    pub default_addr: u8,
    /// Whether that default was confirmed against an Icom document.
    pub addr_verified: bool,
    /// Command + sub-command bytes that read (and, with a data byte, set)
    /// the DV Fast Data setting.
    pub fast_data_cmd: &'static [u8],
    /// The D-STAR commands in [`dstar`] (`1F`, `20`, `15 01`, `0F`).
    pub dstar: Support,
    /// Writing UR/R1/R2 (`1F 01`) has passed hardware test HW-R1 on this
    /// model. Until then the engine only reads them (ADR-0009).
    pub route_write_tested: bool,
}

/// How well a command set is known for a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Support {
    /// Read in the model's Icom CI-V reference guide.
    Documented,
    /// Expected (same radio generation) but not checked against the
    /// model's guide: reads may be tried, and a refusal or silence means
    /// "not supported" for the session. Never written.
    Unverified,
}

/// Profiles with a documented Fast Data command. Addresses: IC-9700 A2h is
/// from Icom's CI-V reference guide; the others are from secondary sources
/// and must be checked on the radio (MENU > SET > Function/Connectors > CI-V).
pub const MODELS: &[Model] = &[
    Model {
        id: "icom-ic9700",
        name: "IC-9700",
        default_addr: 0xA2,
        addr_verified: true,
        fast_data_cmd: &[0x1A, 0x05, 0x00, 0x77],
        dstar: Support::Documented,
        route_write_tested: false,
    },
    Model {
        id: "icom-id52plus",
        name: "ID-52 PLUS",
        default_addr: 0xB4,
        addr_verified: false,
        fast_data_cmd: &[0x22, 0x03],
        dstar: Support::Unverified,
        route_write_tested: false,
    },
    Model {
        id: "icom-id52",
        name: "ID-52",
        default_addr: 0xB4,
        addr_verified: false,
        fast_data_cmd: &[0x22, 0x03],
        dstar: Support::Unverified,
        route_write_tested: false,
    },
    Model {
        id: "icom-ic705",
        name: "IC-705",
        default_addr: 0xA4,
        addr_verified: false,
        fast_data_cmd: &[0x1A, 0x05, 0x00, 0x93],
        dstar: Support::Unverified,
        route_write_tested: false,
    },
    Model {
        id: "icom-ic905",
        name: "IC-905",
        default_addr: 0xAC,
        addr_verified: false,
        fast_data_cmd: &[0x1A, 0x05, 0x00, 0x98],
        dstar: Support::Unverified,
        route_write_tested: false,
    },
];

pub fn model(id: &str) -> Option<&'static Model> {
    MODELS.iter().find(|m| m.id == id)
}

/// Build one CI-V frame. `body` = command, sub-command and data bytes.
pub fn frame(to: u8, from: u8, body: &[u8]) -> Vec<u8> {
    let mut v = vec![PREAMBLE, PREAMBLE, to, from];
    v.extend_from_slice(body);
    v.push(END);
    v
}

/// Request to read the Fast Data setting.
pub fn read_fast_data(m: &Model, radio: u8) -> Vec<u8> {
    frame(radio, CONTROLLER, m.fast_data_cmd)
}

/// Request to set the Fast Data setting.
pub fn set_fast_data(m: &Model, radio: u8, on: bool) -> Vec<u8> {
    let mut body = m.fast_data_cmd.to_vec();
    body.push(on as u8);
    frame(radio, CONTROLLER, &body)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub to: u8,
    pub from: u8,
    /// Command, sub-command and data bytes.
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    Ok,
    Ng,
    /// A reply to a read: the bytes after the echoed command.
    Data(Vec<u8>),
}

impl Frame {
    /// Unsolicited data from `radio` (CI-V transceive: addressed to `00`
    /// or to the controller) that starts with `cmd`: the bytes after it.
    pub fn push(&self, cmd: &[u8], radio: u8) -> Option<&[u8]> {
        (self.from == radio && (self.to == 0x00 || self.to == CONTROLLER))
            .then_some(())
            .and_then(|_| self.body.strip_prefix(cmd))
    }

    /// Interpret as the radio's answer to `cmd` (sent by the controller).
    pub fn reply_to(&self, cmd: &[u8], radio: u8) -> Option<Reply> {
        if self.from != radio || self.to != CONTROLLER {
            return None;
        }
        match self.body.as_slice() {
            [OK] => Some(Reply::Ok),
            [NG] => Some(Reply::Ng),
            b if b.starts_with(cmd) => Some(Reply::Data(b[cmd.len()..].to_vec())),
            _ => None,
        }
    }
}

/// Parse a Fast Data read reply (`00` off, `01` on).
pub fn fast_data_state(r: &Reply) -> Option<bool> {
    match r {
        Reply::Data(d) if d.as_slice() == [0] => Some(false),
        Reply::Data(d) if d.as_slice() == [1] => Some(true),
        _ => None,
    }
}

/// Splits a CI-V byte stream into frames. Collisions (`FC`) and junk
/// between frames are dropped; the buffer is bounded.
#[derive(Debug, Default)]
pub struct Deframer {
    buf: Vec<u8>,
}

impl Deframer {
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Frame> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        loop {
            let Some(start) = self.buf.windows(2).position(|w| w == [PREAMBLE, PREAMBLE]) else {
                // keep a trailing FE that may start the next preamble
                let keep = usize::from(self.buf.last() == Some(&PREAMBLE));
                self.buf.drain(..self.buf.len() - keep);
                break;
            };
            self.buf.drain(..start);
            // skip extra preamble bytes
            let mut i = 2;
            while self.buf.get(i) == Some(&PREAMBLE) {
                i += 1;
            }
            let Some(end) = self.buf[i..].iter().position(|&b| b == END) else {
                if self.buf.len() > 256 {
                    self.buf.clear();
                }
                break;
            };
            let body = self.buf[i..i + end].to_vec();
            self.buf.drain(..i + end + 1);
            if body.contains(&COLLISION) || body.len() < 3 {
                continue;
            }
            out.push(Frame {
                to: body[0],
                from: body[1],
                body: body[2..].to_vec(),
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_match_the_icom_guides() {
        let ic9700 = model("icom-ic9700").unwrap();
        assert_eq!(
            read_fast_data(ic9700, 0xA2),
            vec![0xFE, 0xFE, 0xA2, 0xE0, 0x1A, 0x05, 0x00, 0x77, 0xFD]
        );
        assert_eq!(
            set_fast_data(ic9700, 0xA2, true),
            vec![0xFE, 0xFE, 0xA2, 0xE0, 0x1A, 0x05, 0x00, 0x77, 0x01, 0xFD]
        );
        let id52 = model("icom-id52plus").unwrap();
        assert_eq!(
            set_fast_data(id52, 0xB4, false),
            vec![0xFE, 0xFE, 0xB4, 0xE0, 0x22, 0x03, 0x00, 0xFD]
        );
        assert!(ic9700.addr_verified && !id52.addr_verified);
    }

    #[test]
    fn replies_echo_and_noise() {
        let m = model("icom-ic9700").unwrap();
        let mut d = Deframer::default();
        let mut stream = vec![0x00, 0x13];
        stream.extend(read_fast_data(m, 0xA2)); // USB echo of our request
        stream.extend([0xFE, 0xFE, 0xE0, 0xA2, 0xFC, 0xFD]); // collision
        stream.extend([
            0xFE, 0xFE, 0xFE, 0xE0, 0xA2, 0x1A, 0x05, 0x00, 0x77, 0x01, 0xFD,
        ]);
        let mut frames = Vec::new();
        for chunk in stream.chunks(3) {
            frames.extend(d.feed(chunk));
        }
        assert_eq!(frames.len(), 2);
        assert_eq!(
            frames[0].reply_to(m.fast_data_cmd, 0xA2),
            None,
            "our own echo is not a reply"
        );
        let r = frames[1].reply_to(m.fast_data_cmd, 0xA2).unwrap();
        assert_eq!(fast_data_state(&r), Some(true));
        let ok = d.feed(&[0xFE, 0xFE, 0xE0, 0xA2, 0xFB, 0xFD]);
        assert_eq!(ok[0].reply_to(m.fast_data_cmd, 0xA2), Some(Reply::Ok));
        let ng = d.feed(&[0xFE, 0xFE, 0xE0, 0xA2, 0xFA, 0xFD]);
        assert_eq!(ng[0].reply_to(m.fast_data_cmd, 0xA2), Some(Reply::Ng));
        // A reply from another radio address is not ours.
        let other = d.feed(&[0xFE, 0xFE, 0xE0, 0x94, 0xFB, 0xFD]);
        assert_eq!(other[0].reply_to(m.fast_data_cmd, 0xA2), None);
        assert!(d.feed(&[0x55; 1000]).is_empty());
    }
}
