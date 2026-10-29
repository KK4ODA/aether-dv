//! `aetherd`: the Aether DV engine (Phase C vertical slice).
//!
//! * [`station`]: legacy chat/ping/echo/status/version behaviour (pure)
//! * [`config`]: TOML configuration
//! * [`ports`]: ratflector and serial port drivers
//! * [`engine`]: the engine task wiring ports, store and station
//! * [`xfer`]: legacy file/form transfer sessions (pure)
//! * [`inbox`]: safe storage of received files
//! * [`api`]: local JSON-RPC 2.0 over WebSocket (subset of API v0)

pub mod api;
pub mod config;
pub mod engine;
pub mod inbox;
pub mod ports;
pub mod station;
pub mod xfer;

/// Generate a random API token (hex).
pub fn new_token() -> String {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).expect("OS random source");
    b.iter().map(|x| format!("{x:02x}")).collect()
}
