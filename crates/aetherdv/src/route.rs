//! Route guard (docs/10 §4.2, ADR-0009): does the radio's D-STAR route
//! (UR / R1 / R2, duplex) match the scope this port is configured for?
//! Pure logic; the engine supplies readings and time.

use aether_civ::dstar::TxCalls;

use crate::config::RouteConfig;

pub type Millis = u64;

/// Who should receive this port's transmissions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Stations in range of us (simplex).
    Direct,
    /// Everyone in range of the repeater; the gateway is not used.
    Repeater,
    /// Everyone on whatever the repeater is linked to (R2 = gateway).
    Reflector,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Direct => "direct",
            Scope::Repeater => "repeater",
            Scope::Reflector => "reflector",
        }
    }
}

/// A port's configured route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub scope: Scope,
    /// R1: repeater call sign padded to 7 + module (8 characters).
    pub repeater: Option<String>,
}

impl Route {
    pub fn direct() -> Route {
        Route {
            scope: Scope::Direct,
            repeater: None,
        }
    }

    pub fn from_config(c: Option<&RouteConfig>) -> Route {
        match c {
            Some(c) if c.via == "repeater" => Route {
                scope: if c.scope == "reflector" {
                    Scope::Reflector
                } else {
                    Scope::Repeater
                },
                repeater: c.repeater.as_deref().and_then(|r| parse_repeater(r).ok()),
            },
            _ => Route::direct(),
        }
    }

    /// The repeater's gateway call sign (`xxxxxxxG`).
    pub fn gateway(&self) -> Option<String> {
        self.repeater.as_ref().map(|r| format!("{}G", &r[..7]))
    }

    /// The UR / R1 / R2 this scope needs (`None` for direct).
    pub fn required(&self) -> Option<TxCalls> {
        let r1 = self.repeater.clone()?;
        Some(TxCalls {
            ur: "CQCQCQ  ".into(),
            r2: match self.scope {
                Scope::Reflector => self.gateway()?,
                _ => " ".repeat(8),
            },
            r1,
        })
    }
}

/// Normalise "W4DOC B", "W4DOC  B" or "w4doc b" to the 8-character R1
/// form "W4DOC  B". The module is one letter other than G (the gateway).
pub fn parse_repeater(s: &str) -> Result<String, String> {
    let t = s.trim().to_ascii_uppercase();
    let parts: Vec<&str> = t.split_whitespace().collect();
    let (call, module) = match parts.as_slice() {
        [call, module] => (*call, *module),
        _ => {
            return Err(format!(
                "repeater '{s}' must be a call sign and a module letter, e.g. \"W4DOC B\""
            ))
        }
    };
    if module.len() != 1 || !module.as_bytes()[0].is_ascii_uppercase() || module == "G" {
        return Err(format!(
            "repeater '{s}': the module must be one letter (A, B, C, D…), not '{module}'"
        ));
    }
    if call.len() < 3
        || call.len() > 7
        || !call.bytes().all(|c| c.is_ascii_alphanumeric())
        || !call.bytes().any(|c| c.is_ascii_digit())
    {
        return Err(format!("repeater '{s}': '{call}' is not a call sign"));
    }
    Ok(format!("{call:<7}{module}"))
}

/// What the radio reported over CI-V.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Observed {
    pub calls: Option<TxCalls>,
    /// `Some(true)` = DUP+/DUP- (repeater split), `Some(false)` = simplex.
    pub duplex: Option<bool>,
}

/// The guard's decision for one set of readings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    /// Wait: the radio is set for another scope. Fixable by the operator.
    Hold {
        code: &'static str,
        message: String,
    },
    /// Refuse: the radio would send a gateway command with every over.
    Block {
        code: &'static str,
        message: String,
    },
}

impl Verdict {
    pub fn is_allow(&self) -> bool {
        matches!(self, Verdict::Allow)
    }
    pub fn code(&self) -> Option<&'static str> {
        match self {
            Verdict::Allow => None,
            Verdict::Hold { code, .. } | Verdict::Block { code, .. } => Some(code),
        }
    }
    pub fn message(&self) -> Option<&str> {
        match self {
            Verdict::Allow => None,
            Verdict::Hold { message, .. } | Verdict::Block { message, .. } => Some(message),
        }
    }
}

fn hold(code: &'static str, message: String) -> Verdict {
    Verdict::Hold { code, message }
}

fn blank(s: &str) -> bool {
    s.trim().is_empty()
}

/// Compare the radio's route with the configured one (docs/10 §4.2).
pub fn check(route: &Route, obs: &Observed) -> Verdict {
    if route.scope == Scope::Direct {
        return match obs.duplex {
            Some(true) => hold(
                "duplex_on_direct",
                "The radio is set for a repeater (duplex), but this port is for direct (simplex) contact. Set the radio to simplex, or set this port up for the repeater.".into(),
            ),
            _ => Verdict::Allow,
        };
    }
    let rpt = route.repeater.as_deref().unwrap_or("        ");
    let gw = route.gateway().unwrap_or_default();
    if obs.duplex == Some(false) {
        return hold(
            "simplex",
            format!("The radio is in simplex. The repeater {} ignores simplex transmissions: select the repeater's channel (duplex).", rpt.trim_end()),
        );
    }
    let Some(c) = &obs.calls else {
        return Verdict::Allow;
    };
    let ur_cq = c.ur.starts_with("CQCQ");
    let r2_gateway = c.r2 == gw;
    if r2_gateway && !ur_cq {
        return Verdict::Block {
            code: "ur_command",
            message: format!(
                "UR is set to \"{}\" with R2 on the gateway. Every data transmission would send that to the gateway as a command (link, unlink, echo or call-sign route). Set UR to CQCQCQ.",
                c.ur.trim_end()
            ),
        };
    }
    if blank(&c.r1) {
        return hold(
            "no_repeater",
            format!(
                "The radio has no repeater (R1) set. Select {} on the radio.",
                rpt.trim_end()
            ),
        );
    }
    if c.r1 != rpt {
        return hold(
            "wrong_repeater",
            format!(
                "The radio is set for {} but this port is for {}.",
                c.r1.trim_end(),
                rpt.trim_end()
            ),
        );
    }
    match route.scope {
        Scope::Repeater => {
            if r2_gateway {
                return hold(
                    "would_reach_reflector",
                    format!("R2 is the gateway ({}): data would go to whatever reflector {} is linked to. This port is limited to the repeater: set R2 blank (Local CQ).", gw.trim_end(), rpt.trim_end()),
                );
            }
            if blank(&c.r2) || c.r2 == rpt {
                Verdict::Allow
            } else if c.r2[..7] == rpt[..7] {
                hold(
                    "cross_module",
                    format!("R2 is {}, another module of the same repeater: data could be routed there. Set R2 blank.", c.r2.trim_end()),
                )
            } else {
                hold(
                    "unexpected_r2",
                    format!(
                        "R2 is {}. For this repeater only, set R2 blank.",
                        c.r2.trim_end()
                    ),
                )
            }
        }
        Scope::Reflector => {
            if r2_gateway {
                Verdict::Allow
            } else {
                hold(
                    "local_only",
                    format!("R2 is not the gateway, so data stays on {}. This port is set for the linked reflector: set R2 to {} (Gateway CQ), or change the port to this repeater only.", rpt.trim_end(), gw.trim_end()),
                )
            }
        }
        Scope::Direct => unreachable!("handled above"),
    }
}

/// Where the guard's knowledge of the radio comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Read from the radio over CI-V.
    Civ,
    /// Confirmed by the operator.
    Operator,
}

/// What the port is waiting for before it may transmit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gate {
    Open,
    /// A CI-V reading is pending.
    Checking,
    /// The operator must confirm (route values and/or reflector reach).
    NeedsConfirm {
        reflector: bool,
        route: bool,
    },
    /// The radio is set for another scope (or would send a command).
    Wrong(Verdict),
}

impl Gate {
    pub fn is_open(&self) -> bool {
        matches!(self, Gate::Open)
    }
}

/// Per-port route state (docs/10 §4.2, §4.6, §4.11).
#[derive(Debug, Clone)]
pub struct Guard {
    pub route: Route,
    /// CI-V can read the radio's route on this port.
    pub civ: bool,
    pub recheck_ms: Millis,
    pub observed: Option<Observed>,
    pub verdict: Option<Verdict>,
    pub checked_at: Option<Millis>,
    /// Last CI-V read error (falls back to operator confirmation).
    pub civ_error: Option<String>,
    pub confirmed_at: Option<Millis>,
    /// Reflector reach confirmed this session, for this link (if known).
    pub reflector_ack: Option<Option<String>>,
    pub link: Option<String>,
    pub last_tx: Option<Millis>,
}

/// CI-V readings older than this no longer open the gate.
pub const CIV_STALE_MS: Millis = 180_000;

impl Guard {
    pub fn new(route: Route, civ: bool, recheck_min: u32) -> Guard {
        Guard {
            route,
            civ,
            recheck_ms: recheck_min as Millis * 60_000,
            observed: None,
            verdict: None,
            checked_at: None,
            civ_error: None,
            confirmed_at: None,
            reflector_ack: None,
            link: None,
            last_tx: None,
        }
    }

    /// A CI-V reading arrived.
    pub fn on_reading(&mut self, now: Millis, obs: Observed) {
        self.verdict = Some(check(&self.route, &obs));
        self.observed = Some(obs);
        self.checked_at = Some(now);
        self.civ_error = None;
    }

    /// A CI-V reading failed.
    pub fn on_read_error(&mut self, err: String) {
        self.civ_error = Some(err);
    }

    /// The radio may have changed (frequency/mode change, reconnect):
    /// readings and operator confirmation no longer count.
    pub fn invalidate(&mut self) {
        self.verdict = None;
        self.checked_at = None;
        self.confirmed_at = None;
    }

    /// The operator confirmed what the gate asked for. Ignored while the
    /// radio itself reports a different route.
    pub fn confirm(&mut self, now: Millis) {
        if matches!(self.gate(now), Gate::Wrong(_)) {
            return;
        }
        self.confirmed_at = Some(now);
        if self.route.scope == Scope::Reflector {
            self.reflector_ack = Some(self.link.clone());
        }
    }

    pub fn on_transmitted(&mut self, now: Millis) {
        self.last_tx = Some(now);
    }

    /// The repeater's current link (from its ack text or network traffic).
    /// A change while in reflector scope needs a new confirmation.
    pub fn on_link(&mut self, link: Option<String>) {
        if link != self.link {
            self.link = link;
            if self.route.scope == Scope::Reflector {
                self.reflector_ack = None;
            }
        }
    }

    fn civ_fresh(&self, now: Millis) -> bool {
        self.checked_at
            .is_some_and(|t| now.saturating_sub(t) < CIV_STALE_MS)
    }

    fn operator_fresh(&self, now: Millis) -> bool {
        self.confirmed_at.is_some_and(|c| {
            let since = self.last_tx.map_or(c, |t| t.max(c));
            now.saturating_sub(since) < self.recheck_ms
        })
    }

    pub fn source(&self, now: Millis) -> Option<Source> {
        if self.civ && self.civ_fresh(now) {
            Some(Source::Civ)
        } else if self.operator_fresh(now) {
            Some(Source::Operator)
        } else {
            None
        }
    }

    /// May the port transmit now?
    pub fn gate(&self, now: Millis) -> Gate {
        let civ_ok = self.civ && self.civ_fresh(now);
        if civ_ok {
            if let Some(v) = &self.verdict {
                if !v.is_allow() {
                    return Gate::Wrong(v.clone());
                }
            }
        }
        if self.route.scope == Scope::Direct {
            return Gate::Open;
        }
        let route_known = civ_ok || self.operator_fresh(now);
        let reflector_ok =
            self.route.scope != Scope::Reflector || self.reflector_ack.as_ref() == Some(&self.link);
        if route_known && reflector_ok {
            return Gate::Open;
        }
        if self.civ && self.civ_error.is_none() && !civ_ok && !self.operator_fresh(now) {
            return Gate::Checking;
        }
        Gate::NeedsConfirm {
            reflector: !reflector_ok,
            route: !route_known,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn calls(ur: &str, r1: &str, r2: &str) -> Observed {
        Observed {
            calls: Some(TxCalls {
                ur: format!("{ur:<8}"),
                r1: format!("{r1:<8}"),
                r2: format!("{r2:<8}"),
            }),
            duplex: Some(true),
        }
    }

    fn rpt(scope: Scope) -> Route {
        Route {
            scope,
            repeater: Some(parse_repeater("W4DOC B").unwrap()),
        }
    }

    #[test]
    fn repeater_names() {
        assert_eq!(parse_repeater("w4doc  b").unwrap(), "W4DOC  B");
        assert_eq!(parse_repeater("KK4ODA C").unwrap(), "KK4ODA C");
        for bad in [
            "W4DOC",
            "W4DOC G",
            "W4DOC BB",
            "W4DOC-B",
            "ABCDEFGH B",
            "WDOC B",
        ] {
            assert!(parse_repeater(bad).is_err(), "{bad}");
        }
        assert_eq!(rpt(Scope::Repeater).gateway().unwrap(), "W4DOC  G");
        let req = rpt(Scope::Reflector).required().unwrap();
        assert_eq!((req.ur.as_str(), req.r2.as_str()), ("CQCQCQ  ", "W4DOC  G"));
        assert_eq!(rpt(Scope::Repeater).required().unwrap().r2, "        ");
    }

    #[test]
    fn decision_table() {
        let r = rpt(Scope::Repeater);
        let f = rpt(Scope::Reflector);
        let code = |route: &Route, o: Observed| check(route, &o).code();
        // Repeater only.
        assert_eq!(code(&r, calls("CQCQCQ", "W4DOC  B", "")), None);
        assert_eq!(code(&r, calls("CQCQCQ", "W4DOC  B", "W4DOC  B")), None);
        assert_eq!(
            code(&r, calls("KK4ODA", "W4DOC  B", "")),
            None,
            "UR is harmless locally"
        );
        assert_eq!(
            code(&r, calls("CQCQCQ", "W4DOC  B", "W4DOC  G")),
            Some("would_reach_reflector")
        );
        assert_eq!(
            code(&r, calls("CQCQCQ", "W4DOC  B", "W4DOC  A")),
            Some("cross_module")
        );
        assert_eq!(
            code(&r, calls("CQCQCQ", "W4DOC  B", "WX4GPB B")),
            Some("unexpected_r2")
        );
        assert_eq!(
            code(&r, calls("CQCQCQ", "W4DOC  C", "")),
            Some("wrong_repeater")
        );
        assert_eq!(code(&r, calls("CQCQCQ", "", "")), Some("no_repeater"));
        // Gateway commands are blocked in every scope.
        for ur in ["REF030CL", "       U", "       E", "KK4ODA", "/W4DOCB"] {
            for route in [&r, &f] {
                let v = check(route, &calls(ur, "W4DOC  B", "W4DOC  G"));
                assert!(
                    matches!(
                        v,
                        Verdict::Block {
                            code: "ur_command",
                            ..
                        }
                    ),
                    "{ur}"
                );
            }
        }
        // Reflector.
        assert_eq!(code(&f, calls("CQCQCQ", "W4DOC  B", "W4DOC  G")), None);
        assert_eq!(
            code(&f, calls("CQCQCQ", "W4DOC  B", "")),
            Some("local_only")
        );
        // Duplex.
        let simplex = Observed {
            duplex: Some(false),
            ..calls("CQCQCQ", "W4DOC  B", "")
        };
        assert_eq!(code(&r, simplex.clone()), Some("simplex"));
        assert_eq!(code(&Route::direct(), simplex), None);
        assert_eq!(
            code(&Route::direct(), calls("CQCQCQ", "W4DOC  B", "W4DOC  G")),
            Some("duplex_on_direct")
        );
        assert_eq!(code(&Route::direct(), Observed::default()), None);
    }

    #[test]
    fn gate_with_civ() {
        let mut g = Guard::new(rpt(Scope::Repeater), true, 30);
        assert_eq!(g.gate(0), Gate::Checking);
        g.on_reading(0, calls("CQCQCQ", "W4DOC  B", "W4DOC  G"));
        assert!(matches!(
            g.gate(1),
            Gate::Wrong(Verdict::Hold {
                code: "would_reach_reflector",
                ..
            })
        ));
        g.confirm(2);
        assert!(
            !g.gate(3).is_open(),
            "operator can't override what the radio reports"
        );
        g.on_reading(10, calls("CQCQCQ", "W4DOC  B", ""));
        assert!(g.gate(11).is_open());
        assert_eq!(g.source(11), Some(Source::Civ));
        assert_eq!(g.gate(10 + CIV_STALE_MS), Gate::Checking, "stale reading");
        g.invalidate();
        g.on_read_error("no answer".into());
        assert_eq!(
            g.gate(20),
            Gate::NeedsConfirm {
                reflector: false,
                route: true
            }
        );
        g.confirm(20);
        assert!(
            g.gate(21).is_open(),
            "operator confirmation is the fallback when CI-V fails"
        );
    }

    #[test]
    fn gate_without_civ_rechecks() {
        let mut g = Guard::new(rpt(Scope::Repeater), false, 30);
        assert_eq!(
            g.gate(0),
            Gate::NeedsConfirm {
                reflector: false,
                route: true
            }
        );
        g.confirm(0);
        assert!(g.gate(29 * 60_000).is_open());
        g.on_transmitted(29 * 60_000);
        assert!(
            g.gate(58 * 60_000).is_open(),
            "sending keeps the confirmation fresh"
        );
        assert!(!g.gate(59 * 60_000).is_open(), "30 min idle: confirm again");
        g.confirm(60 * 60_000);
        g.invalidate();
        assert!(!g.gate(60 * 60_000).is_open(), "reconnect: confirm again");
        // Direct ports without CI-V are open, as before.
        assert!(Guard::new(Route::direct(), false, 30).gate(0).is_open());
    }

    #[test]
    fn reflector_needs_its_own_confirmation_per_link() {
        let mut g = Guard::new(rpt(Scope::Reflector), true, 30);
        g.on_reading(0, calls("CQCQCQ", "W4DOC  B", "W4DOC  G"));
        assert_eq!(
            g.gate(1),
            Gate::NeedsConfirm {
                reflector: true,
                route: false
            }
        );
        g.confirm(1);
        assert!(g.gate(2).is_open());
        g.on_link(Some("REF001 C".into()));
        assert_eq!(
            g.gate(3),
            Gate::NeedsConfirm {
                reflector: true,
                route: false
            }
        );
        g.confirm(3);
        assert!(g.gate(4).is_open());
    }
}
