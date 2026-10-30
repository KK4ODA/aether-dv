//! `aether-link`: sans-I/O link-layer state machines.
//!
//! * [`legacy_arq`]: ARQ-1, the D-RATS stateful-session protocol (SM-4)
//! * [`scheduler`]: per-port half-duplex channel scheduler (SM-2)
//! * [`channel`]: channel sense aggregation for the scheduler (docs/10)
//!
//! Planned: ALP (SM-5).

pub mod channel;
pub mod legacy_arq;
pub mod scheduler;
