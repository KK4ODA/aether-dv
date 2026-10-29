//! DDT2 frame: 25-byte big-endian header + optional zlib payload.
//!
//! ```text
//! off size field
//!   0   1  magic     0xDD = payload zlib-compressed, 0x22 = raw
//!   1   2  seq       u16 BE
//!   3   1  session   u8
//!   4   1  type      u8
//!   5   2  checksum  CRC-16/XMODEM over header (checksum = 0) + wire payload
//!   7   2  length    u16 BE, length of the wire (possibly compressed) payload
//!   9   8  src       ASCII, right-padded with '~'
//!  17   8  dst       ASCII, right-padded with '~'
//!  25   n  payload
//! ```
//! Source: D-Rats `d_rats/ddt2.py@d0e7a39`. Unlike the legacy code,
//! serialisation here never mutates the frame (legacy defect P1).

use std::io::{Read, Write};

use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;

use crate::crc::crc16_xmodem;

pub const MAGIC_COMPRESSED: u8 = 0xDD;
pub const MAGIC_RAW: u8 = 0x22;
pub const HEADER_LEN: usize = 25;
pub const STATION_LEN: usize = 8;
const PAD: u8 = b'~';

/// Upper bound on a decompressed payload, to stop zlib bombs from a hostile peer.
pub const MAX_PAYLOAD: usize = 64 * 1024;

/// How the payload is carried on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadEncoding {
    /// Always zlib (legacy default for most sessions).
    Always,
    /// Never (legacy chat frames, ping, warm-up).
    Never,
    /// zlib only if that makes the wire payload smaller (legacy defect P3).
    IfSmaller,
}

/// A decoded DDT2 frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub seq: u16,
    pub session: u8,
    pub kind: u8,
    /// Station fields as transmitted, padding removed (≤ 8 bytes).
    pub src: Vec<u8>,
    pub dst: Vec<u8>,
    /// Application payload (decompressed).
    pub payload: Vec<u8>,
    /// Whether the wire form was compressed (informational after decode).
    pub compressed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    TooShort(usize),
    BadMagic(u8),
    LengthMismatch { header: u16, actual: usize },
    Checksum { header: u16, computed: u16 },
    Decompress,
    PayloadTooLarge,
    StationTooLong(usize),
    PayloadLengthOverflow(usize),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for FrameError {}

fn pad_station(s: &[u8]) -> Result<[u8; STATION_LEN], FrameError> {
    if s.len() > STATION_LEN {
        return Err(FrameError::StationTooLong(s.len()));
    }
    let mut out = [PAD; STATION_LEN];
    out[..s.len()].copy_from_slice(s);
    Ok(out)
}

/// Legacy receivers remove *every* '~' (not only trailing padding).
fn unpad_station(s: &[u8]) -> Vec<u8> {
    s.iter().copied().filter(|&b| b != PAD).collect()
}

fn zlib(data: &[u8]) -> Vec<u8> {
    let mut e = ZlibEncoder::new(Vec::new(), Compression::new(9));
    e.write_all(data).expect("write to Vec cannot fail");
    e.finish().expect("finish to Vec cannot fail")
}

fn unzlib(data: &[u8]) -> Result<Vec<u8>, FrameError> {
    let mut out = Vec::new();
    let mut d = ZlibDecoder::new(data).take(MAX_PAYLOAD as u64 + 1);
    d.read_to_end(&mut out)
        .map_err(|_| FrameError::Decompress)?;
    if out.len() > MAX_PAYLOAD {
        return Err(FrameError::PayloadTooLarge);
    }
    Ok(out)
}

impl Frame {
    /// Convenience constructor.
    pub fn new(
        seq: u16,
        session: u8,
        kind: u8,
        src: &str,
        dst: &str,
        payload: impl Into<Vec<u8>>,
    ) -> Self {
        Frame {
            seq,
            session,
            kind,
            src: src.as_bytes().to_vec(),
            dst: dst.as_bytes().to_vec(),
            payload: payload.into(),
            compressed: false,
        }
    }

    /// Serialise to the raw (un-enveloped, un-escaped) DDT2 bytes.
    pub fn encode(&self, enc: PayloadEncoding) -> Result<Vec<u8>, FrameError> {
        let (magic, wire) = match enc {
            PayloadEncoding::Never => (MAGIC_RAW, self.payload.clone()),
            PayloadEncoding::Always => (MAGIC_COMPRESSED, zlib(&self.payload)),
            PayloadEncoding::IfSmaller => {
                let z = zlib(&self.payload);
                if z.len() < self.payload.len() {
                    (MAGIC_COMPRESSED, z)
                } else {
                    (MAGIC_RAW, self.payload.clone())
                }
            }
        };
        let len: u16 = wire
            .len()
            .try_into()
            .map_err(|_| FrameError::PayloadLengthOverflow(wire.len()))?;
        let mut out = Vec::with_capacity(HEADER_LEN + wire.len());
        out.push(magic);
        out.extend_from_slice(&self.seq.to_be_bytes());
        out.push(self.session);
        out.push(self.kind);
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(&pad_station(&self.src)?);
        out.extend_from_slice(&pad_station(&self.dst)?);
        out.extend_from_slice(&wire);
        let crc = crc16_xmodem(&out);
        out[5..7].copy_from_slice(&crc.to_be_bytes());
        Ok(out)
    }

    /// Parse raw DDT2 bytes (already un-escaped, without the envelope).
    pub fn decode(raw: &[u8]) -> Result<Frame, FrameError> {
        if raw.len() < HEADER_LEN {
            return Err(FrameError::TooShort(raw.len()));
        }
        let magic = raw[0];
        let compressed = match magic {
            MAGIC_COMPRESSED => true,
            MAGIC_RAW => false,
            m => return Err(FrameError::BadMagic(m)),
        };
        let seq = u16::from_be_bytes([raw[1], raw[2]]);
        let checksum = u16::from_be_bytes([raw[5], raw[6]]);
        let length = u16::from_be_bytes([raw[7], raw[8]]);
        let wire = &raw[HEADER_LEN..];
        // Legacy ignores the length field (it checksums whatever follows the
        // header). We check it but only after the CRC, so the error says why.
        let mut zeroed = raw.to_vec();
        zeroed[5] = 0;
        zeroed[6] = 0;
        let computed = crc16_xmodem(&zeroed);
        if computed != checksum {
            return Err(FrameError::Checksum {
                header: checksum,
                computed,
            });
        }
        if length as usize != wire.len() {
            return Err(FrameError::LengthMismatch {
                header: length,
                actual: wire.len(),
            });
        }
        let payload = if compressed {
            unzlib(wire)?
        } else {
            wire.to_vec()
        };
        if payload.len() > MAX_PAYLOAD {
            return Err(FrameError::PayloadTooLarge);
        }
        Ok(Frame {
            seq,
            session: raw[3],
            kind: raw[4],
            src: unpad_station(&raw[9..17]),
            dst: unpad_station(&raw[17..25]),
            payload,
            compressed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// Upstream D-RATS self-test vector (seq 3, session 2, type 1, FOO->BAR),
    /// recorded in w9fyi/macrats `Tests/MacRatsCoreTests/DDT2FrameTests.swift`.
    const UPSTREAM_RAW: &str =
        "2200030201b557000e464f4f7e7e7e7e7e4241527e7e7e7e7e5468697320697320612074657374";
    const UPSTREAM_ZLIB: &str = "dd00030201c3ab0014464f4f7e7e7e7e7e4241527e7e7e7e7e78da0bc9c82c5600a2448592d4e21200247304f6";

    #[test]
    fn upstream_uncompressed_vector_byte_exact() {
        let f = Frame::new(3, 2, 1, "FOO", "BAR", b"This is a test".to_vec());
        assert_eq!(f.encode(PayloadEncoding::Never).unwrap(), hex(UPSTREAM_RAW));
        let d = Frame::decode(&hex(UPSTREAM_RAW)).unwrap();
        assert_eq!((d.seq, d.session, d.kind), (3, 2, 1));
        assert_eq!(d.src, b"FOO");
        assert_eq!(d.payload, b"This is a test");
        assert!(!d.compressed);
    }

    #[test]
    fn upstream_compressed_vector_decodes() {
        let d = Frame::decode(&hex(UPSTREAM_ZLIB)).unwrap();
        assert_eq!(d.payload, b"This is a test");
        assert!(d.compressed);
    }

    #[test]
    fn compressed_encode_roundtrips() {
        // Byte equality with Python zlib is not guaranteed across deflate
        // implementations (macrats found Apple's differs); decode equivalence is.
        let f = Frame::new(3, 2, 1, "FOO", "BAR", b"This is a test".to_vec());
        let enc = f.encode(PayloadEncoding::Always).unwrap();
        let d = Frame::decode(&enc).unwrap();
        assert_eq!(d.payload, f.payload);
    }

    #[test]
    fn serialisation_is_pure_p1() {
        // Legacy P1: get_packed() on an uncompressed frame flips magic each call.
        let f = Frame::new(0, 1, 0, "N0CALL", "CQCQCQ", b"Hello".to_vec());
        let a = f.encode(PayloadEncoding::Never).unwrap();
        let b = f.encode(PayloadEncoding::Never).unwrap();
        assert_eq!(a, b);
        assert_eq!(a[0], MAGIC_RAW);
    }

    #[test]
    fn if_smaller_avoids_inflating_small_frames_p3() {
        let f = Frame::new(0, 4, 5, "N0CALL", "W1AW", vec![0, 1, 2, 3]);
        let enc = f.encode(PayloadEncoding::IfSmaller).unwrap();
        assert_eq!(enc[0], MAGIC_RAW);
        assert_eq!(enc.len(), HEADER_LEN + 4);
        let big = Frame::new(0, 4, 4, "N0CALL", "W1AW", vec![b'A'; 500]);
        assert_eq!(
            big.encode(PayloadEncoding::IfSmaller).unwrap()[0],
            MAGIC_COMPRESSED
        );
    }

    #[test]
    fn checksum_and_length_errors() {
        let mut raw = hex(UPSTREAM_RAW);
        raw[30] ^= 1;
        assert!(matches!(
            Frame::decode(&raw),
            Err(FrameError::Checksum { .. })
        ));
        assert_eq!(Frame::decode(&raw[..10]), Err(FrameError::TooShort(10)));
        let mut bad = hex(UPSTREAM_RAW);
        bad[0] = 0x55;
        assert_eq!(Frame::decode(&bad), Err(FrameError::BadMagic(0x55)));
    }

    #[test]
    fn station_too_long_rejected() {
        let f = Frame::new(0, 1, 0, "N0CALL-123", "CQCQCQ", b"x".to_vec());
        assert_eq!(
            f.encode(PayloadEncoding::Never),
            Err(FrameError::StationTooLong(10))
        );
    }

    #[test]
    fn zlib_bomb_rejected() {
        let f = Frame::new(0, 4, 4, "A", "B", vec![0u8; MAX_PAYLOAD + 10]);
        // Encoding a too-large payload is the caller's bug; decoding must refuse it.
        let enc = f.encode(PayloadEncoding::Always).unwrap();
        assert_eq!(Frame::decode(&enc), Err(FrameError::PayloadTooLarge));
    }
}
