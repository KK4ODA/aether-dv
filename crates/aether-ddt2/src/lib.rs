//! `aether-ddt2`: byte-exact codec for the legacy D-RATS DDT2 wire protocol.
//!
//! Pure library: no I/O, no clocks, no threads. Normative description:
//! `docs/02-legacy-protocol-analysis.md`. Validation level: **Simulated**
//! (golden vectors from upstream D-RATS self-tests and property tests); not
//! yet checked against a running D-RATS (Phase C, criterion C1).
//!
//! Layers, bottom up:
//! * [`crc`] — CRC-16/XMODEM
//! * [`yenc`] — D-RATS yEnc escaping with configurable escape set
//! * [`frame`] — 25-byte header + optional zlib payload
//! * [`envelope`] — `[SOB]…[EOB]` wrapping and a bounded streaming deframer
//! * [`session`] — positional session ids and session payload encodings

pub mod crc;
pub mod envelope;
pub mod frame;
pub mod session;
pub mod yenc;

pub use envelope::{wrap, Deframer, EnvelopeError, Event};
pub use frame::{Frame, FrameError, PayloadEncoding};
pub use yenc::EscapeSet;
