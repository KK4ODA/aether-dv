//! `aether-rfl`: ratflector client connection state machine (SM-6), sans-I/O.
//!
//! Handshake per D-RATS `comm.py` `SocketDataPath.do_auth` (@d0e7a39):
//! server line `100 …` = no auth; `101 …` = send `USER <call>`, expect `102`,
//! send `PASS <pw>`, expect `200`. A server that sends no banner, or a
//! non-numeric first line, is an "old-school" ratflector: proceed without auth.
//!
//! Differences from legacy (client-side only):
//! * banner wait 5 s (legacy 30 s), configurable;
//! * bytes that arrive instead of a banner are kept as stream data, not dropped;
//! * a `200` reply to USER ends authentication (legacy sends PASS anyway);
//! * reconnect uses exponential back-off with jitter (legacy: fixed 5 s).
//!
//! The caller owns the socket: it performs [`Action`]s and feeds results back.

/// Monotonic milliseconds supplied by the caller.
pub type Millis = u64;

#[derive(Debug, Clone)]
pub struct Config {
    pub callsign: String,
    pub password: Option<String>,
    pub banner_timeout: Millis,
    pub backoff_initial: Millis,
    pub backoff_max: Millis,
    /// Uptime after which the back-off resets to its initial value.
    pub stable_after: Millis,
    /// Longest banner/auth line accepted before treating input as stream data.
    pub max_line: usize,
    /// Seed for back-off jitter (deterministic in tests).
    pub jitter_seed: u64,
}

impl Config {
    pub fn new(callsign: &str) -> Self {
        Config {
            callsign: callsign.into(),
            password: None,
            banner_timeout: 5_000,
            backoff_initial: 1_000,
            backoff_max: 300_000,
            stable_after: 60_000,
            max_line: 512,
            jitter_seed: 0x5EED_1234_ABCD_0001,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Idle,
    Connecting,
    Banner,
    AuthUser,
    AuthPass,
    Up,
    Backoff,
    /// Authentication was refused; no automatic retry (operator action needed).
    AuthFailed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpReason {
    NoAuthRequired,
    Authenticated,
    OldSchoolNoBanner,
    OldSchoolUnrecognisedBanner,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Open the TCP connection (with the caller's own connect timeout).
    Connect,
    /// Write these bytes to the socket.
    Send(Vec<u8>),
    /// Stream bytes for the DDT2 deframer.
    Deliver(Vec<u8>),
    /// Close the socket.
    Close,
    /// Connection is usable.
    Up(UpReason),
    /// Server refused our credentials or violated the protocol.
    AuthFailed(String),
    /// Next connection attempt is scheduled at this time.
    Retrying { at: Millis, delay: Millis },
}

#[derive(Debug)]
pub struct Client {
    cfg: Config,
    state: State,
    line: Vec<u8>,
    deadline: Option<Millis>,
    delay: Millis,
    up_since: Option<Millis>,
    rng: u64,
}

impl Client {
    pub fn new(cfg: Config) -> Self {
        let delay = cfg.backoff_initial;
        let rng = cfg.jitter_seed | 1;
        Client {
            cfg,
            state: State::Idle,
            line: Vec::new(),
            deadline: None,
            delay,
            up_since: None,
            rng,
        }
    }

    pub fn state(&self) -> State {
        self.state
    }

    /// When the caller should call [`Client::on_timeout`] next.
    pub fn next_deadline(&self) -> Option<Millis> {
        self.deadline
    }

    pub fn start(&mut self) -> Vec<Action> {
        match self.state {
            State::Idle | State::AuthFailed => {
                self.state = State::Connecting;
                vec![Action::Connect]
            }
            _ => vec![],
        }
    }

    pub fn on_connected(&mut self, now: Millis) -> Vec<Action> {
        if self.state != State::Connecting {
            return vec![];
        }
        self.state = State::Banner;
        self.line.clear();
        self.deadline = Some(now + self.cfg.banner_timeout);
        vec![]
    }

    pub fn on_connect_failed(&mut self, now: Millis) -> Vec<Action> {
        self.backoff(now)
    }

    /// Socket closed, reset, or keepalive failure.
    pub fn on_disconnected(&mut self, now: Millis) -> Vec<Action> {
        match self.state {
            State::Idle | State::AuthFailed | State::Backoff => vec![],
            _ => self.backoff(now),
        }
    }

    pub fn disable(&mut self) -> Vec<Action> {
        let was_open = !matches!(self.state, State::Idle | State::Backoff | State::AuthFailed);
        self.state = State::Idle;
        self.deadline = None;
        if was_open {
            vec![Action::Close]
        } else {
            vec![]
        }
    }

    pub fn on_timeout(&mut self, now: Millis) -> Vec<Action> {
        match self.deadline {
            Some(d) if now >= d => {}
            _ => return vec![],
        }
        self.deadline = None;
        match self.state {
            State::Banner => {
                let mut a = self.go_up(now, UpReason::OldSchoolNoBanner);
                if !self.line.is_empty() {
                    a.push(Action::Deliver(std::mem::take(&mut self.line)));
                }
                a
            }
            State::AuthUser | State::AuthPass => {
                self.state = State::AuthFailed;
                vec![
                    Action::Close,
                    Action::AuthFailed("authentication timed out".into()),
                ]
            }
            State::Backoff => {
                self.state = State::Connecting;
                vec![Action::Connect]
            }
            _ => vec![],
        }
    }

    pub fn on_bytes(&mut self, now: Millis, bytes: &[u8]) -> Vec<Action> {
        match self.state {
            State::Up => {
                if let Some(since) = self.up_since {
                    if now.saturating_sub(since) >= self.cfg.stable_after {
                        self.delay = self.cfg.backoff_initial;
                    }
                }
                vec![Action::Deliver(bytes.to_vec())]
            }
            State::Banner | State::AuthUser | State::AuthPass => self.handshake_bytes(now, bytes),
            _ => vec![],
        }
    }

    fn handshake_bytes(&mut self, now: Millis, bytes: &[u8]) -> Vec<Action> {
        let mut actions = Vec::new();
        self.line.extend_from_slice(bytes);
        loop {
            if !matches!(
                self.state,
                State::Banner | State::AuthUser | State::AuthPass
            ) {
                // Left the handshake: anything after the last line is stream data.
                if self.state == State::Up && !self.line.is_empty() {
                    actions.push(Action::Deliver(std::mem::take(&mut self.line)));
                }
                return actions;
            }
            let Some(nl) = self.line.iter().position(|&b| b == b'\n') else {
                if self.line.len() > self.cfg.max_line {
                    // No line structure: an old-school server already streaming.
                    actions.extend(self.go_up(now, UpReason::OldSchoolUnrecognisedBanner));
                    actions.push(Action::Deliver(std::mem::take(&mut self.line)));
                }
                return actions;
            };
            let raw: Vec<u8> = self.line.drain(..=nl).collect();
            let text = String::from_utf8_lossy(&raw).trim().to_string();
            let code: Option<u16> = text
                .split_once(' ')
                .map(|(c, _)| c)
                .unwrap_or(&text)
                .parse()
                .ok()
                .filter(|c| (100..1000).contains(c));
            match (self.state, code) {
                (State::Banner, Some(100)) => {
                    actions.extend(self.go_up(now, UpReason::NoAuthRequired))
                }
                (State::Banner, Some(101)) => {
                    self.state = State::AuthUser;
                    self.deadline = Some(now + self.cfg.banner_timeout * 2);
                    actions.push(Action::Send(
                        format!("USER {}\r\n", self.cfg.callsign).into_bytes(),
                    ));
                }
                (State::Banner, Some(c)) => {
                    self.state = State::AuthFailed;
                    self.deadline = None;
                    actions.push(Action::Close);
                    actions.push(Action::AuthFailed(format!(
                        "unexpected banner code {c}: {text}"
                    )));
                }
                (State::Banner, None) => {
                    // Not a banner at all: keep these bytes as stream data.
                    actions.extend(self.go_up(now, UpReason::OldSchoolUnrecognisedBanner));
                    let mut rest = raw;
                    rest.append(&mut self.line);
                    self.line = rest;
                }
                (State::AuthUser, Some(102)) => match &self.cfg.password {
                    Some(pw) => {
                        self.state = State::AuthPass;
                        actions.push(Action::Send(format!("PASS {pw}\r\n").into_bytes()));
                    }
                    None => {
                        self.state = State::AuthFailed;
                        self.deadline = None;
                        actions.push(Action::Close);
                        actions.push(Action::AuthFailed(
                            "server requires a password; none configured".into(),
                        ));
                    }
                },
                (State::AuthUser, Some(200)) | (State::AuthPass, Some(200)) => {
                    actions.extend(self.go_up(now, UpReason::Authenticated))
                }
                (State::AuthUser | State::AuthPass, _) => {
                    self.state = State::AuthFailed;
                    self.deadline = None;
                    actions.push(Action::Close);
                    actions.push(Action::AuthFailed(format!("server said: {text}")));
                }
                _ => unreachable!("handshake loop runs only in handshake states"),
            }
        }
    }

    fn go_up(&mut self, now: Millis, why: UpReason) -> Vec<Action> {
        self.state = State::Up;
        self.deadline = None;
        self.up_since = Some(now);
        vec![Action::Up(why)]
    }

    fn jitter(&mut self) -> f64 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        0.8 + 0.4 * ((self.rng >> 11) as f64 / (1u64 << 53) as f64)
    }

    fn backoff(&mut self, now: Millis) -> Vec<Action> {
        let was_open = matches!(
            self.state,
            State::Banner | State::AuthUser | State::AuthPass | State::Up
        );
        if let Some(since) = self.up_since.take() {
            if now.saturating_sub(since) >= self.cfg.stable_after {
                self.delay = self.cfg.backoff_initial;
            }
        }
        let delay = ((self.delay as f64) * self.jitter()) as Millis;
        self.delay = (self.delay * 2).min(self.cfg.backoff_max);
        self.state = State::Backoff;
        self.deadline = Some(now + delay);
        let mut a = Vec::new();
        if was_open {
            a.push(Action::Close);
        }
        a.push(Action::Retrying {
            at: now + delay,
            delay,
        });
        a
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connected(cfg: Config) -> Client {
        let mut c = Client::new(cfg);
        assert_eq!(c.start(), vec![Action::Connect]);
        c.on_connected(0);
        c
    }

    #[test]
    fn open_server_banner_100() {
        let mut c = connected(Config::new("KK4ODA"));
        let a = c.on_bytes(10, b"100 Authentication not required\r\n");
        assert_eq!(a, vec![Action::Up(UpReason::NoAuthRequired)]);
        assert_eq!(c.state(), State::Up);
    }

    #[test]
    fn banner_split_across_segments_and_trailing_data_kept() {
        let mut c = connected(Config::new("KK4ODA"));
        assert!(c.on_bytes(10, b"100 Authenti").is_empty());
        let a = c.on_bytes(20, b"cation not required\r\n[SOB]abc");
        assert_eq!(
            a,
            vec![
                Action::Up(UpReason::NoAuthRequired),
                Action::Deliver(b"[SOB]abc".to_vec())
            ]
        );
    }

    #[test]
    fn full_auth_flow() {
        let mut cfg = Config::new("KK4ODA");
        cfg.password = Some("s3cret".into());
        let mut c = connected(cfg);
        assert_eq!(
            c.on_bytes(1, b"101 Authorization required\r\n"),
            vec![Action::Send(b"USER KK4ODA\r\n".to_vec())]
        );
        assert_eq!(
            c.on_bytes(2, b"102 USER okay\r\n"),
            vec![Action::Send(b"PASS s3cret\r\n".to_vec())]
        );
        assert_eq!(
            c.on_bytes(3, b"200 Authorized\r\n"),
            vec![Action::Up(UpReason::Authenticated)]
        );
    }

    #[test]
    fn rejected_password_does_not_retry() {
        let mut cfg = Config::new("KK4ODA");
        cfg.password = Some("wrong".into());
        let mut c = connected(cfg);
        c.on_bytes(1, b"101 Authorization required\r\n");
        c.on_bytes(2, b"102 USER okay\r\n");
        let a = c.on_bytes(3, b"500 Not authorized\r\n");
        assert_eq!(a[0], Action::Close);
        assert!(matches!(a[1], Action::AuthFailed(_)));
        assert_eq!(c.state(), State::AuthFailed);
        assert!(
            c.on_disconnected(4).is_empty(),
            "no automatic retry after auth failure"
        );
    }

    #[test]
    fn password_required_but_missing() {
        let mut c = connected(Config::new("KK4ODA"));
        c.on_bytes(1, b"101 Authorization required\r\n");
        let a = c.on_bytes(2, b"102 USER okay\r\n");
        assert!(matches!(a.last(), Some(Action::AuthFailed(m)) if m.contains("password")));
    }

    #[test]
    fn two_hundred_after_user_does_not_send_pass_n6_4() {
        let mut cfg = Config::new("KK4ODA");
        cfg.password = Some("pw".into());
        let mut c = connected(cfg);
        c.on_bytes(1, b"101 Authorization required\r\n");
        assert_eq!(
            c.on_bytes(2, b"200 Authorized\r\n"),
            vec![Action::Up(UpReason::Authenticated)]
        );
    }

    #[test]
    fn old_school_silent_server_times_out_to_up() {
        let mut c = connected(Config::new("KK4ODA"));
        assert_eq!(c.next_deadline(), Some(5_000));
        assert!(c.on_timeout(4_999).is_empty());
        assert_eq!(
            c.on_timeout(5_000),
            vec![Action::Up(UpReason::OldSchoolNoBanner)]
        );
    }

    #[test]
    fn old_school_server_that_streams_immediately_keeps_bytes() {
        let mut c = connected(Config::new("KK4ODA"));
        let a = c.on_bytes(5, b"[SOB]\"=@=@\x01=@ET=@\x05N0CALL~~CQCQCQ~~Hello[EOB]\n");
        assert_eq!(a[0], Action::Up(UpReason::OldSchoolUnrecognisedBanner));
        assert!(matches!(&a[1], Action::Deliver(d) if d.starts_with(b"[SOB]")));
    }

    #[test]
    fn backoff_grows_with_jitter_caps_and_resets_after_stable_uptime() {
        let mut c = connected(Config::new("KK4ODA"));
        let mut now = 0;
        let mut delays = vec![];
        for _ in 0..12 {
            let a = c.on_connect_failed(now);
            let Some(Action::Retrying { delay, at }) = a.last().cloned() else {
                panic!("{a:?}")
            };
            delays.push(delay);
            now = at;
            assert_eq!(c.on_timeout(now), vec![Action::Connect]);
        }
        // grows roughly geometrically, never above cap * 1.2
        assert!(delays[1] > delays[0] && delays[4] > delays[2]);
        assert!(delays.iter().all(|&d| d <= 360_000));
        assert!(delays[11] >= 240_000, "cap reached: {delays:?}");
        // jitter: two clients with different seeds don't reconnect in lock-step (N6.2)
        let mut cfg2 = Config::new("KK4ODA");
        cfg2.jitter_seed = 42;
        let mut c2 = connected(cfg2);
        let Some(Action::Retrying { delay: d2, .. }) = c2.on_connect_failed(0).last().cloned()
        else {
            panic!()
        };
        assert_ne!(d2, delays[0]);
        // stable uptime resets back-off
        c.on_connected(now);
        c.on_bytes(now, b"100 ok\r\n");
        let Some(Action::Retrying { delay, .. }) = c.on_disconnected(now + 120_000).last().cloned()
        else {
            panic!()
        };
        assert!(
            delay <= 1_200,
            "back-off should reset after stable uptime, got {delay}"
        );
    }

    #[test]
    fn disconnect_while_up_closes_and_retries() {
        let mut c = connected(Config::new("KK4ODA"));
        c.on_bytes(1, b"100 ok\r\n");
        let a = c.on_disconnected(10);
        assert_eq!(a[0], Action::Close);
        assert!(matches!(a[1], Action::Retrying { .. }));
        assert_eq!(c.state(), State::Backoff);
    }

    #[test]
    fn garbage_without_newlines_is_bounded() {
        let mut c = connected(Config::new("KK4ODA"));
        let a = c.on_bytes(1, &vec![b'x'; 600]);
        assert_eq!(a[0], Action::Up(UpReason::OldSchoolUnrecognisedBanner));
        assert_eq!(c.state(), State::Up);
    }
}
