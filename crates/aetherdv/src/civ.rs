//! CI-V session on the configured control port (docs/10 §4.3, §4.9).
//!
//! One blocking thread per radio owns the port while the engine runs:
//! * serves requests (Fast Data read/set, D-STAR route read/set);
//! * polls the DV RX status (`20 02`) four times a second, or the squelch
//!   status (`15 01`) if the radio refuses that, and reports channel
//!   changes straight to the port's scheduler;
//! * reads the route (`1F 01`, `0F`) at start, every minute and after the
//!   operator changes frequency or mode (CI-V transceive);
//! * reads the received call signs and message when a call ends, for
//!   repeater acks and link detection.
//!
//! It only *reads* radio settings unless a request asks to set one; it
//! does not switch on the radio's own automatic outputs.

use std::sync::mpsc as std_mpsc;
use std::time::{Duration, Instant};

use aether_civ::dstar::{self, RxCalls, RxMessage, RxStatus, TxCalls};
use aether_civ::{fast_data_state, Deframer, Frame, Model, Reply, Support};
use aether_link::channel::{Busy, Source};
use tokio::sync::{mpsc, oneshot};

use crate::ports::PortCmd;
use crate::route::Observed;

/// Requests from the engine.
#[derive(Debug)]
pub enum CivReq {
    /// Read (and with `set`, first change) the Fast Data setting.
    FastData {
        set: Option<bool>,
        reply: oneshot::Sender<Result<bool, String>>,
    },
    /// Read UR/R1/R2 and duplex now (result as [`CivEvt::Route`]).
    ReadRoute,
    /// Write UR/R1/R2, then read them back.
    SetRoute {
        calls: TxCalls,
        reply: oneshot::Sender<Result<Observed, String>>,
    },
    /// Read the last received call signs and message now.
    ReadRx,
}

/// Events to the engine.
#[derive(Debug, Clone)]
pub enum CivEvt {
    /// The port opened (or failed to open).
    Open(Result<(), String>),
    /// The radio's route; `Err` when it can't be read.
    Route(Result<Observed, String>),
    /// The radio's MY call sign.
    MyCall(String),
    /// Which channel-sense sources work on this radio.
    Sources(Vec<Source>),
    /// Combined channel state from this radio (for display).
    Channel(Option<Busy>),
    /// The operator changed frequency or mode on the radio.
    Changed,
    RxCalls(RxCalls),
    RxMessage(RxMessage),
}

pub struct SessionParams {
    pub port: String,
    pub path: String,
    pub baud: u32,
    pub model: &'static Model,
    pub addr: u8,
}

const REPLY_TIMEOUT: Duration = Duration::from_millis(400);
const POLL_EVERY: Duration = Duration::from_millis(250);
const ROUTE_EVERY: Duration = Duration::from_secs(60);

#[derive(Debug, PartialEq)]
enum Answer {
    Reply(Reply),
    Timeout,
}

struct Radio {
    port: Box<dyn serialport::SerialPort>,
    d: Deframer,
    addr: u8,
    /// Unsolicited frames seen while waiting for replies.
    pushes: Vec<Frame>,
}

impl Radio {
    fn transact(&mut self, req: &[u8], cmd: &[u8]) -> Result<Answer, String> {
        self.port
            .write_all(req)
            .and_then(|_| self.port.flush())
            .map_err(|e| format!("CI-V write failed: {e}"))?;
        let deadline = Instant::now() + REPLY_TIMEOUT;
        let mut buf = [0u8; 128];
        while Instant::now() < deadline {
            match self.port.read(&mut buf) {
                Ok(n) => {
                    for f in self.d.feed(&buf[..n]) {
                        if let Some(r) = f.reply_to(cmd, self.addr) {
                            return Ok(Answer::Reply(r));
                        }
                        if f.from == self.addr {
                            self.pushes.push(f);
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                Err(e) => return Err(format!("CI-V read failed: {e}")),
            }
        }
        Ok(Answer::Timeout)
    }

    fn read(&mut self, cmd: &[u8]) -> Result<Answer, String> {
        let req = dstar::read(self.addr, cmd);
        self.transact(&req, cmd)
    }

    /// Collect pushes that arrived between transactions.
    fn drain(&mut self) -> Result<(), String> {
        let mut buf = [0u8; 128];
        loop {
            match self.port.read(&mut buf) {
                Ok(0) => return Ok(()),
                Ok(n) => {
                    for f in self.d.feed(&buf[..n]) {
                        if f.from == self.addr {
                            self.pushes.push(f);
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => return Ok(()),
                Err(e) => return Err(format!("CI-V read failed: {e}")),
            }
        }
    }
}

fn no_answer(addr: u8) -> String {
    format!(
        "no answer from the radio at CI-V address {addr:02X}h (check the CI-V address, baud rate and the radio's CI-V/USB settings)"
    )
}

/// Start the session thread. Requests go in through the returned sender;
/// channel changes go to the port (`port_tx`), everything else to `evt`.
pub fn spawn(
    p: SessionParams,
    port_tx: mpsc::Sender<PortCmd>,
    evt: mpsc::Sender<(String, CivEvt)>,
) -> std_mpsc::Sender<CivReq> {
    let (tx, rx) = std_mpsc::channel::<CivReq>();
    std::thread::spawn(move || run(p, rx, port_tx, evt));
    tx
}

struct State {
    /// `None` until tried, then whether the radio answers it.
    status: Option<bool>,
    squelch: Option<bool>,
    route: Option<bool>,
    last_busy: Option<Busy>,
    last_route: Option<Instant>,
    route_due: bool,
    /// Read RX call signs/message this long after the channel changes.
    rx_due: Option<Instant>,
    timeouts: u32,
}

fn run(
    p: SessionParams,
    rx: std_mpsc::Receiver<CivReq>,
    port_tx: mpsc::Sender<PortCmd>,
    evt: mpsc::Sender<(String, CivEvt)>,
) {
    let send = |e: CivEvt| evt.blocking_send((p.port.clone(), e)).is_ok();
    let dstar_ok = matches!(p.model.dstar, Support::Documented | Support::Unverified);
    loop {
        let opened = serialport::new(&p.path, p.baud)
            .timeout(Duration::from_millis(20))
            .open();
        let port = match opened {
            Ok(port) => port,
            Err(e) => {
                let msg = format!("cannot open CI-V port {}: {e}", p.path);
                if !send(CivEvt::Open(Err(msg.clone()))) {
                    return;
                }
                // Answer requests honestly while the port is unavailable.
                let until = Instant::now() + Duration::from_secs(5);
                while Instant::now() < until {
                    match rx.recv_timeout(Duration::from_millis(200)) {
                        Ok(r) => refuse(r, &msg),
                        Err(std_mpsc::RecvTimeoutError::Timeout) => {}
                        Err(std_mpsc::RecvTimeoutError::Disconnected) => return,
                    }
                }
                continue;
            }
        };
        if !send(CivEvt::Open(Ok(()))) {
            return;
        }
        let mut r = Radio {
            port,
            d: Deframer::default(),
            addr: p.addr,
            pushes: Vec::new(),
        };
        let unknown = if dstar_ok { None } else { Some(false) };
        let mut st = State {
            status: unknown,
            squelch: unknown,
            route: unknown,
            last_busy: None,
            last_route: None,
            route_due: dstar_ok,
            rx_due: None,
            timeouts: 0,
        };
        let err = session(&p, &mut r, &mut st, &rx, &port_tx, &send);
        match err {
            None => return, // engine gone
            Some(e) => {
                let _ = port_tx.blocking_send(PortCmd::Sources(Vec::new()));
                if !send(CivEvt::Open(Err(e))) {
                    return;
                }
                std::thread::sleep(Duration::from_secs(2));
            }
        }
    }
}

fn refuse(r: CivReq, msg: &str) {
    match r {
        CivReq::FastData { reply, .. } => {
            let _ = reply.send(Err(msg.to_string()));
        }
        CivReq::SetRoute { reply, .. } => {
            let _ = reply.send(Err(msg.to_string()));
        }
        CivReq::ReadRoute | CivReq::ReadRx => {}
    }
}

/// Run until the port fails (`Some(error)`) or the engine stops (`None`).
fn session(
    p: &SessionParams,
    r: &mut Radio,
    st: &mut State,
    rx: &std_mpsc::Receiver<CivReq>,
    port_tx: &mpsc::Sender<PortCmd>,
    send: &dyn Fn(CivEvt) -> bool,
) -> Option<String> {
    let mut sources_sent: Option<Vec<Source>> = None;
    let mut my_call_read = false;
    loop {
        let tick = Instant::now();
        // Requests first.
        loop {
            match rx.try_recv() {
                Ok(req) => {
                    if let Err(e) = handle(p, r, st, req, send) {
                        return Some(e);
                    }
                }
                Err(std_mpsc::TryRecvError::Empty) => break,
                Err(std_mpsc::TryRecvError::Disconnected) => return None,
            }
        }
        if let Err(e) = r.drain() {
            return Some(e);
        }
        for f in std::mem::take(&mut r.pushes) {
            if f.push(dstar::FREQ_PUSH, p.addr).is_some()
                || f.push(dstar::MODE_PUSH, p.addr).is_some()
            {
                st.route_due = true;
                if !send(CivEvt::Changed) {
                    return None;
                }
            } else if let Some(d) = f.push(dstar::RX_CALLS_PUSH, p.addr) {
                if let Some(c) = RxCalls::parse(d) {
                    send(CivEvt::RxCalls(c));
                }
            } else if let Some(d) = f.push(dstar::RX_MSG_PUSH, p.addr) {
                if let Some(m) = RxMessage::parse(d) {
                    send(CivEvt::RxMessage(m));
                }
            }
        }
        if st.route != Some(false) {
            if !my_call_read {
                my_call_read = true;
                match r.read(dstar::MY_CALL) {
                    Ok(Answer::Reply(Reply::Data(d))) => {
                        if let Some((call, _)) = dstar::parse_my_call(&d) {
                            send(CivEvt::MyCall(call.trim_end().to_string()));
                        }
                    }
                    Ok(_) => {}
                    Err(e) => return Some(e),
                }
            }
            let due = st.route_due || st.last_route.map_or(true, |t| t.elapsed() >= ROUTE_EVERY);
            if due {
                st.route_due = false;
                match read_route(r, st) {
                    Ok(Some(res)) => {
                        if !send(CivEvt::Route(res)) {
                            return None;
                        }
                    }
                    Ok(None) => {}
                    Err(e) => return Some(e),
                }
            }
        }
        // Channel sense.
        let busy = match poll_channel(r, st) {
            Ok(b) => b,
            Err(e) => return Some(e),
        };
        let sources: Vec<Source> = [
            (st.status == Some(true)).then_some(Source::CivStatus),
            (st.status == Some(false) && st.squelch == Some(true)).then_some(Source::CivSquelch),
        ]
        .into_iter()
        .flatten()
        .collect();
        if sources_sent.as_ref() != Some(&sources) {
            sources_sent = Some(sources.clone());
            st.last_busy = None;
            if port_tx
                .blocking_send(PortCmd::Sources(sources.clone()))
                .is_err()
                || !send(CivEvt::Sources(sources.clone()))
            {
                return None;
            }
        }
        if let (Some(src), Some(b)) = (sources.first(), busy) {
            if b != st.last_busy {
                st.last_busy = b;
                st.rx_due = Some(
                    Instant::now() + Duration::from_millis(if b.is_some() { 400 } else { 1_500 }),
                );
                if port_tx
                    .blocking_send(PortCmd::Channel {
                        source: *src,
                        busy: b,
                    })
                    .is_err()
                {
                    return None;
                }
                send(CivEvt::Channel(b));
            }
        }
        if st.rx_due.is_some_and(|t| Instant::now() >= t) && st.route != Some(false) {
            st.rx_due = None;
            if let Err(e) = read_rx(r, send) {
                return Some(e);
            }
        }
        let slow = st.timeouts >= 3;
        let period = if slow {
            Duration::from_secs(2)
        } else {
            POLL_EVERY
        };
        if let Some(rest) = period.checked_sub(tick.elapsed()) {
            match rx.recv_timeout(rest) {
                Ok(req) => {
                    if let Err(e) = handle(p, r, st, req, send) {
                        return Some(e);
                    }
                }
                Err(std_mpsc::RecvTimeoutError::Timeout) => {}
                Err(std_mpsc::RecvTimeoutError::Disconnected) => return None,
            }
        }
    }
}

/// `Ok(Some(busy))` with a fresh reading, `Ok(None)` if nothing works.
fn poll_channel(r: &mut Radio, st: &mut State) -> Result<Option<Option<Busy>>, String> {
    if st.status != Some(false) {
        match r.read(dstar::RX_STATUS_READ)? {
            Answer::Reply(Reply::Data(d)) => {
                if let Some(s) = RxStatus::parse(&d) {
                    st.status = Some(true);
                    st.timeouts = 0;
                    return Ok(Some(status_busy(s)));
                }
                st.status = Some(false);
            }
            Answer::Reply(_) => st.status = Some(false),
            Answer::Timeout => {
                st.timeouts += 1;
                return Ok(None);
            }
        }
    }
    if st.squelch != Some(false) {
        match r.read(dstar::SQUELCH)? {
            Answer::Reply(Reply::Data(d)) => match dstar::parse_squelch(&d) {
                Some(open) => {
                    st.squelch = Some(true);
                    st.timeouts = 0;
                    return Ok(Some(open.then_some(Busy::Signal)));
                }
                None => st.squelch = Some(false),
            },
            Answer::Reply(_) => st.squelch = Some(false),
            Answer::Timeout => {
                st.timeouts += 1;
                return Ok(None);
            }
        }
    }
    Ok(None)
}

fn status_busy(s: RxStatus) -> Option<Busy> {
    if s.voice() {
        Some(Busy::Voice)
    } else if s.other_mode() {
        Some(Busy::OtherMode)
    } else if s.signal() {
        Some(Busy::Signal)
    } else {
        None
    }
}

/// `Ok(None)` when route reads are known unsupported.
fn read_route(r: &mut Radio, st: &mut State) -> Result<Option<Result<Observed, String>>, String> {
    st.last_route = Some(Instant::now());
    let calls = match r.read(dstar::TX_CALLS)? {
        Answer::Reply(Reply::Data(d)) => match TxCalls::parse(&d) {
            Some(c) => c,
            None => {
                return Ok(Some(Err(format!(
                    "unexpected reply to the call-sign read ({} bytes)",
                    d.len()
                ))))
            }
        },
        Answer::Reply(Reply::Ng) | Answer::Reply(Reply::Ok) => {
            st.route = Some(false);
            return Ok(Some(Err(
                "this radio does not answer the D-STAR call-sign read (CI-V 1F 01)".into(),
            )));
        }
        Answer::Timeout => return Ok(Some(Err(no_answer(r.addr)))),
    };
    st.route = Some(true);
    let duplex = match r.read(dstar::DUPLEX)? {
        Answer::Reply(Reply::Data(d)) => dstar::parse_duplex(&d),
        _ => None,
    };
    Ok(Some(Ok(Observed {
        calls: Some(calls),
        duplex,
    })))
}

fn read_rx(r: &mut Radio, send: &dyn Fn(CivEvt) -> bool) -> Result<(), String> {
    if let Answer::Reply(Reply::Data(d)) = r.read(dstar::RX_CALLS_READ)? {
        if let Some(c) = RxCalls::parse(&d) {
            send(CivEvt::RxCalls(c));
        }
    }
    if let Answer::Reply(Reply::Data(d)) = r.read(dstar::RX_MSG_READ)? {
        if let Some(m) = RxMessage::parse(&d) {
            send(CivEvt::RxMessage(m));
        }
    }
    Ok(())
}

fn handle(
    p: &SessionParams,
    r: &mut Radio,
    st: &mut State,
    req: CivReq,
    send: &dyn Fn(CivEvt) -> bool,
) -> Result<(), String> {
    match req {
        CivReq::FastData { set, reply } => {
            let res = fast_data(p.model, r, set);
            let _ = reply.send(res);
        }
        CivReq::ReadRoute => {
            if let Some(res) = read_route(r, st)? {
                send(CivEvt::Route(res));
            }
        }
        CivReq::ReadRx => read_rx(r, send)?,
        CivReq::SetRoute { calls, reply } => {
            let res = match calls.set_request(p.addr) {
                Err(e) => Err(e),
                Ok(req) => match r.transact(&req, dstar::TX_CALLS)? {
                    Answer::Reply(Reply::Ok) => match read_route(r, st)? {
                        Some(Ok(obs)) => {
                            send(CivEvt::Route(Ok(obs.clone())));
                            Ok(obs)
                        }
                        Some(Err(e)) => Err(e),
                        None => {
                            Err("the radio did not report its call signs after the change".into())
                        }
                    },
                    Answer::Reply(Reply::Ng) => Err("the radio refused the call signs (NG)".into()),
                    Answer::Reply(Reply::Data(_)) => {
                        Err("unexpected reply to the call-sign write".into())
                    }
                    Answer::Timeout => Err(no_answer(p.addr)),
                },
            };
            let _ = reply.send(res);
        }
    }
    Ok(())
}

/// Read (and with `set`, first change) the Fast Data setting.
fn fast_data(model: &Model, r: &mut Radio, set: Option<bool>) -> Result<bool, String> {
    let cmd = model.fast_data_cmd;
    let io = |e: String| e;
    if let Some(on) = set {
        let req = aether_civ::set_fast_data(model, r.addr, on);
        match r.transact(&req, cmd).map_err(io)? {
            Answer::Reply(Reply::Ok) => {}
            Answer::Reply(Reply::Ng) => return Err("the radio refused the setting (NG)".into()),
            Answer::Reply(Reply::Data(_)) => {
                return Err("unexpected reply to the set command".into())
            }
            Answer::Timeout => return Err(no_answer(r.addr)),
        }
    }
    let req = aether_civ::read_fast_data(model, r.addr);
    match r.transact(&req, cmd).map_err(io)? {
        Answer::Reply(Reply::Ng) => Err("the radio refused the read (NG)".into()),
        Answer::Reply(rep) => {
            fast_data_state(&rep).ok_or_else(|| format!("unexpected reply {rep:?}"))
        }
        Answer::Timeout => Err(no_answer(r.addr)),
    }
}
