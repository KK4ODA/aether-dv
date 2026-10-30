//! Engine configuration (TOML).
//!
//! ```toml
//! callsign = "KK4ODA"
//! db = "aether.db"                 # optional; in-memory if absent
//! api_listen = "127.0.0.1:7373"
//! files_dir = "received"           # optional; default "received" next to the DB
//! max_incoming_kib = 4096          # optional; largest incoming transfer (compressed)
//! capture = false                  # optional; capture every port's traffic from start
//! capture_dir = "captures"         # optional; default "captures" next to the DB
//!
//! [position]                       # optional; sent only when you ask
//! lat = 33.749
//! lon = -84.388
//! comment = "EOC"
//! symbol = "/-"
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
//!
//! [port.route]                      # optional; without it the port is direct (simplex)
//! via = "repeater"                  # direct | repeater
//! repeater = "W4DOC B"              # call sign + module (R1)
//! scope = "repeater"                # repeater (default) | reflector
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
    /// Start a wire capture (`.aecap`) on every port at start-up.
    #[serde(default)]
    pub capture: bool,
    /// Where captures and support bundles go.
    pub capture_dir: Option<PathBuf>,
    /// This station's fixed position, sent only on request
    /// (`stations.sendPosition`); there is no automatic beacon.
    pub position: Option<PositionConfig>,
    #[serde(default, rename = "port")]
    pub ports: Vec<PortConfig>,
}

/// A CI-V port for reading (and, if allowed, setting) the radio's DV Fast
/// Data setting. Only this configured path is ever opened (no probing).
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CivConfig {
    pub path: String,
    #[serde(default = "default_civ_baud")]
    pub baud: u32,
    /// Radio CI-V address in hex (e.g. "A2"); default from the profile.
    pub addr: Option<String>,
    /// Allow `ports.setRadioMode` to change the radio (default: read only).
    #[serde(default)]
    pub allow_set: bool,
}

impl CivConfig {
    pub fn addr_byte(&self) -> Option<u8> {
        self.addr
            .as_deref()
            .and_then(|a| u8::from_str_radix(a.trim().trim_start_matches("0x"), 16).ok())
    }
}

/// How a radio port reaches other stations (docs/10 §4.1). Advanced
/// timing fields override the preset for `via` + `scope`.
#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct RouteConfig {
    #[serde(default = "default_via")]
    pub via: String,
    /// The repeater (R1): call sign and module, e.g. "W4DOC B".
    pub repeater: Option<String>,
    #[serde(default = "default_scope")]
    pub scope: String,
    /// Minutes after which an operator-confirmed route must be confirmed
    /// again if nothing was sent (radios without CI-V route reads).
    #[serde(default = "default_recheck_min")]
    pub recheck_min: u32,
    /// Treat relayed D-PRS/NMEA slow data as a voice user on the air.
    #[serde(default = "default_true")]
    pub infer_voice: bool,
    /// Answer broadcast pings and echo requests. Default: on except in
    /// reflector scope.
    pub answer_broadcast: Option<bool>,
    pub holdoff_ms: Option<u64>,
    pub jitter_ms: Option<u64>,
    pub voice_gap_ms: Option<u64>,
    pub min_gap_ms: Option<u64>,
    pub max_over_ms: Option<u64>,
    pub duty_percent: Option<u32>,
    pub duty_window_s: Option<u64>,
}

fn default_via() -> String {
    "direct".into()
}
fn default_scope() -> String {
    "repeater".into()
}
fn default_recheck_min() -> u32 {
    30
}
fn default_true() -> bool {
    true
}

impl RouteConfig {
    pub fn validate(&self, port: &str) -> Result<(), String> {
        let bad = |m: String| Err(format!("port {port}: route: {m}"));
        match self.via.as_str() {
            "direct" => {
                if self.repeater.is_some() {
                    return bad("repeater is only used with via = \"repeater\"".into());
                }
                if self.scope == "reflector" {
                    return bad("scope = \"reflector\" needs via = \"repeater\"".into());
                }
            }
            "repeater" => {
                let Some(r) = &self.repeater else {
                    return bad(
                        "via = \"repeater\" needs repeater = \"CALL M\" (call sign and module)"
                            .into(),
                    );
                };
                if let Err(e) = crate::route::parse_repeater(r) {
                    return bad(e);
                }
                if self.scope != "repeater" && self.scope != "reflector" {
                    return bad("scope must be repeater or reflector".into());
                }
            }
            v => return bad(format!("via must be direct or repeater, not '{v}'")),
        }
        if self.recheck_min == 0 || self.recheck_min > 24 * 60 {
            return bad("recheck_min must be 1-1440".into());
        }
        if self.duty_percent.is_some_and(|d| d == 0 || d > 100) {
            return bad("duty_percent must be 1-100".into());
        }
        if self
            .max_over_ms
            .is_some_and(|m| !(2_000..=60_000).contains(&m))
        {
            return bad("max_over_ms must be 2000-60000".into());
        }
        if self
            .duty_window_s
            .is_some_and(|w| !(60..=3_600).contains(&w))
        {
            return bad("duty_window_s must be 60-3600".into());
        }
        for (k, v) in [
            ("holdoff_ms", self.holdoff_ms),
            ("jitter_ms", self.jitter_ms),
            ("voice_gap_ms", self.voice_gap_ms),
            ("min_gap_ms", self.min_gap_ms),
        ] {
            if v.is_some_and(|v| v > 120_000) {
                return bad(format!("{k} must be at most 120000"));
            }
        }
        Ok(())
    }
}

fn default_civ_baud() -> u32 {
    19200
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PositionConfig {
    pub lat: f64,
    pub lon: f64,
    pub alt_m: Option<f64>,
    #[serde(default = "default_pos_comment")]
    pub comment: String,
    /// APRS symbol, table + code (`/-` house, `/>` car, `/a` ambulance).
    #[serde(default = "default_symbol")]
    pub symbol: String,
}

fn default_pos_comment() -> String {
    "Aether DV".into()
}

fn default_symbol() -> String {
    "/-".into()
}

fn default_max_incoming() -> u32 {
    4096
}

fn default_api() -> String {
    "127.0.0.1:7373".into()
}

// Parsed once at start-up; the size difference doesn't matter.
#[allow(clippy::large_enum_variant)]
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
        /// Optional CI-V control channel to the same radio (`[port.civ]`).
        civ: Option<CivConfig>,
        /// How this radio reaches other stations (`[port.route]`).
        #[serde(default)]
        route: Option<RouteConfig>,
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
        if let Some(p) = &c.position {
            if !(-90.0..=90.0).contains(&p.lat) || !(-180.0..=180.0).contains(&p.lon) {
                return Err(ConfigError::Invalid(
                    "position: lat/lon out of range".into(),
                ));
            }
            if p.symbol.chars().count() != 2 {
                return Err(ConfigError::Invalid(
                    "position: symbol must be 2 characters (table + code)".into(),
                ));
            }
        }
        for p in &c.ports {
            if let PortConfig::Serial {
                civ: Some(civ),
                profile,
                ..
            } = p
            {
                if aether_civ::model(profile).is_none() {
                    return Err(ConfigError::Invalid(format!(
                        "port {}: CI-V is configured but profile '{profile}' has no documented Fast Data command (known: {})",
                        p.name(),
                        aether_civ::MODELS.iter().map(|m| m.id).collect::<Vec<_>>().join(", ")
                    )));
                }
                if civ.addr.is_some() && civ.addr_byte().is_none() {
                    return Err(ConfigError::Invalid(format!(
                        "port {}: civ.addr must be a hex byte such as \"A2\"",
                        p.name()
                    )));
                }
            }
            if let PortConfig::Serial {
                route: Some(route), ..
            } = p
            {
                route.validate(p.name()).map_err(ConfigError::Invalid)?;
            }
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
    fn shipped_examples_parse_with_and_without_position() {
        for (name, text) in [
            ("ic9700", include_str!("../../../examples/ic9700.toml")),
            ("id52plus", include_str!("../../../examples/id52plus.toml")),
        ] {
            let c = Config::parse(text).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(c.capture, "{name}");
            let with_pos = text
                .replace("# [position]", "[position]")
                .replace("# lat = ", "lat = ")
                .replace("# lon = ", "lon = ")
                .replace("# comment = ", "comment = ")
                .replace("# symbol = ", "symbol = ");
            let c =
                Config::parse(&with_pos).unwrap_or_else(|e| panic!("{name} with position: {e}"));
            assert_eq!(c.position.unwrap().symbol, "/-");
            let with_route = text
                .replace("# [port.route]", "[port.route]")
                .replace("# via = ", "via = ")
                .replace("# repeater = ", "repeater = ")
                .replace("# scope = ", "scope = ");
            let c = Config::parse(&with_route).unwrap_or_else(|e| panic!("{name} with route: {e}"));
            assert!(
                c.ports.iter().any(|p| matches!(p, PortConfig::Serial { route: Some(r), .. } if r.via == "repeater")),
                "{name}: route attaches to the radio port"
            );
        }
        let bad = "callsign = \"KK4ODA\"\n[position]\nlat = 95.0\nlon = 0.0\n";
        assert!(Config::parse(bad).is_err());
    }

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
    fn route_tables() {
        let base = "callsign = \"KK4ODA\"\n[[port]]\nkind = \"serial\"\nname = \"r\"\npath = \"/dev/x\"\nprofile = \"icom-id51plus\"\n";
        let ok = format!("{base}[port.route]\nvia = \"repeater\"\nrepeater = \"W4DOC B\"\n");
        let c = Config::parse(&ok).unwrap();
        let PortConfig::Serial { route: Some(p), .. } = &c.ports[0] else {
            panic!("route");
        };
        assert_eq!(
            (p.scope.as_str(), p.recheck_min, p.infer_voice),
            ("repeater", 30, true)
        );
        for bad in [
            "[port.route]\nvia = \"repeater\"\n",
            "[port.route]\nvia = \"repeater\"\nrepeater = \"W4DOC\"\n",
            "[port.route]\nvia = \"repeater\"\nrepeater = \"W4DOC B\"\nscope = \"world\"\n",
            "[port.route]\nscope = \"reflector\"\n",
            "[port.route]\nvia = \"satellite\"\n",
            "[port.route]\nvia = \"repeater\"\nrepeater = \"W4DOC B\"\nduty_percent = 0\n",
        ] {
            assert!(Config::parse(&format!("{base}{bad}")).is_err(), "{bad}");
        }
    }

    #[test]
    fn rejects_bad_configs() {
        assert!(Config::parse("callsign = \"TOOLONGCALL\"").is_err());
        assert!(Config::parse("callsign = \"KK4ODA\"\nbogus = 1").is_err());
        let dup = "callsign=\"KK4ODA\"\n[[port]]\nkind=\"ratflector\"\nname=\"a\"\nhost=\"h\"\n[[port]]\nkind=\"ratflector\"\nname=\"a\"\nhost=\"h\"";
        assert!(Config::parse(dup).is_err());
    }
}
