//! Engine configuration (TOML).
//!
//! ```toml
//! callsign = "KK4ODA"
//! db = "aether.db"                 # optional; in-memory if absent
//! api_listen = "127.0.0.1:7373"
//! files_dir = "received"           # optional; default "received" next to the DB
//! max_incoming_kib = 4096          # optional; largest incoming transfer (compressed)
//!
//! [[port]]
//! kind = "ratflector"
//! name = "sewx"
//! host = "sewx.ratflector.com"
//! tcp_port = 9000
//! # password = "..."               # only for ratflectors that require it
//!
//! [[port]]
//! kind = "serial"
//! name = "IC-9700"
//! path = "/dev/serial/by-id/usb-Icom_Inc._IC-9700_...-if01"
//! baud = 9600
//! profile = "icom-ic9700"
//! mode = "slow"                     # slow | fast (radio setting must match)
//! flow = "none"                     # none | software (XON/XOFF), per HW-3
//! ```

use std::path::PathBuf;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub callsign: String,
    pub db: Option<PathBuf>,
    #[serde(default = "default_api")]
    pub api_listen: String,
    /// Where the API token is written (default: next to the DB, or a temp dir).
    pub token_file: Option<PathBuf>,
    /// Received files and forms (default: `received/` next to the DB, or a
    /// temp dir without a DB).
    pub files_dir: Option<PathBuf>,
    /// Largest incoming legacy transfer, in KiB of zlib stream.
    #[serde(default = "default_max_incoming")]
    pub max_incoming_kib: u32,
    #[serde(default, rename = "port")]
    pub ports: Vec<PortConfig>,
}

fn default_max_incoming() -> u32 {
    4096
}

fn default_api() -> String {
    "127.0.0.1:7373".into()
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum PortConfig {
    Ratflector {
        name: String,
        host: String,
        #[serde(default = "default_rfl_port")]
        tcp_port: u16,
        password: Option<String>,
    },
    Serial {
        name: String,
        path: String,
        #[serde(default = "default_baud")]
        baud: u32,
        profile: String,
        #[serde(default = "default_mode")]
        mode: String,
        /// `none` (default) or `software` (driver XON/XOFF). Decided per radio
        /// by hardware test HW-3; legacy handling of this lost bytes (DR#253).
        #[serde(default = "default_flow")]
        flow: String,
    },
}

fn default_rfl_port() -> u16 {
    9000
}
fn default_baud() -> u32 {
    9600
}
fn default_mode() -> String {
    "slow".into()
}
fn default_flow() -> String {
    "none".into()
}

impl PortConfig {
    pub fn name(&self) -> &str {
        match self {
            PortConfig::Ratflector { name, .. } | PortConfig::Serial { name, .. } => name,
        }
    }
}

#[derive(Debug)]
pub enum ConfigError {
    Parse(String),
    Invalid(String),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Parse(e) => write!(f, "config parse error: {e}"),
            ConfigError::Invalid(e) => write!(f, "invalid config: {e}"),
        }
    }
}
impl std::error::Error for ConfigError {}

impl Config {
    pub fn parse(text: &str) -> Result<Config, ConfigError> {
        let mut c: Config = toml::from_str(text).map_err(|e| ConfigError::Parse(e.to_string()))?;
        c.callsign = c.callsign.trim().to_ascii_uppercase();
        if c.callsign.len() < 3 || c.callsign.len() > 8 {
            return Err(ConfigError::Invalid(
                "callsign must be 3-8 characters (DDT2 station field)".into(),
            ));
        }
        let mut names: Vec<&str> = c.ports.iter().map(PortConfig::name).collect();
        names.sort();
        names.dedup();
        if names.len() != c.ports.len() {
            return Err(ConfigError::Invalid("port names must be unique".into()));
        }
        for p in &c.ports {
            if let PortConfig::Serial { mode, flow, .. } = p {
                if mode != "slow" && mode != "fast" {
                    return Err(ConfigError::Invalid(format!(
                        "port {}: mode must be slow or fast",
                        p.name()
                    )));
                }
                if flow != "none" && flow != "software" {
                    return Err(ConfigError::Invalid(format!(
                        "port {}: flow must be none or software",
                        p.name()
                    )));
                }
            }
        }
        Ok(c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_example() {
        let c = Config::parse(
            r#"
            callsign = "kk4oda"
            [[port]]
            kind = "ratflector"
            name = "sewx"
            host = "sewx.ratflector.com"
            [[port]]
            kind = "serial"
            name = "IC-9700"
            path = "/dev/ttyUSB0"
            profile = "icom-ic9700"
            "#,
        )
        .unwrap();
        assert_eq!(c.callsign, "KK4ODA");
        assert_eq!(c.api_listen, "127.0.0.1:7373");
        assert_eq!(c.ports.len(), 2);
        assert!(matches!(
            &c.ports[0],
            PortConfig::Ratflector { tcp_port: 9000, .. }
        ));
        assert!(
            matches!(&c.ports[1], PortConfig::Serial { baud: 9600, mode, .. } if mode == "slow")
        );
    }

    #[test]
    fn rejects_bad_configs() {
        assert!(Config::parse("callsign = \"TOOLONGCALL\"").is_err());
        assert!(Config::parse("callsign = \"KK4ODA\"\nbogus = 1").is_err());
        let dup = "callsign=\"KK4ODA\"\n[[port]]\nkind=\"ratflector\"\nname=\"a\"\nhost=\"h\"\n[[port]]\nkind=\"ratflector\"\nname=\"a\"\nhost=\"h\"";
        assert!(Config::parse(dup).is_err());
    }
}
