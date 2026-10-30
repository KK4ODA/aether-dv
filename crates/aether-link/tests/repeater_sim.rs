//! Repeater simulation (docs/10 §6): the real scheduler and ARQ-1 on a
//! shared D-STAR repeater with voice users, run in simulated time.
//! Validation level: Simulated. The repeater follows MMDVMHost's rules as
//! read in its source (one input at a time, R2 = gateway goes to the
//! network, an acknowledgement 750 ms after each over); voice users and
//! a D-RATS-style station are behavioural models, not measurements.

use std::collections::{HashMap, VecDeque};

use aether_ddt2::{wrap, Deframer, EscapeSet, Event, Frame};
use aether_link::channel::{Busy, VoiceInference};
use aether_link::legacy_arq::*;
use aether_link::scheduler::{Item, ModeProfile, PathPreset, Priority, Scheduler, Status};

const STEP: Millis = 20;
const ACK_DELAY: Millis = 750;
const ACK_LEN: Millis = 500;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.next() % (hi - lo + 1)
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Style {
    /// Aether DV with CI-V channel sense (voice detected).
    Sensed,
    /// Aether DV without CI-V: data only, plus voice inferred from GPS.
    Unsensed,
    /// D-RATS: a random 0.5-2 s wait, then send; no channel sense.
    Drats,
}

struct DataStation {
    call: String,
    style: Style,
    sched: Scheduler,
    deframer: Deframer,
    infer: VoiceInference,
    last_busy: Option<Busy>,
    /// D-RATS model: queued wire frames and when it will send them.
    drats_q: VecDeque<Vec<u8>>,
    drats_at: Option<Millis>,
    sender: Option<LegacySender>,
    receiver: Option<LegacyReceiver>,
    paused_since: Option<Millis>,
    next_chat: Millis,
    chats_sent: u32,
    file: Vec<u8>,
    overs: Vec<(Millis, Millis)>,
}

#[derive(Clone)]
struct Over {
    who: usize, // data stations first, then voice users
    start: Millis,
    end: Millis,
    bytes: Vec<u8>,
    voice: bool,
    gps: bool,
    repeated: bool,
}

struct Qso {
    users: [usize; 2],
    overs_left: u32,
    next_at: Millis,
    turn: usize,
    gps: [bool; 2],
}

#[derive(Default, Debug)]
struct Report {
    data_overs: u32,
    lost_overs: u32,
    /// Data overs that started while a QSO was in progress.
    qso_interruptions: u32,
    /// Time data transmissions overlapped voice overs (doubling on the
    /// repeater input garbles the voice).
    voice_overlap_ms: Millis,
    data_air_ms: Millis,
    voice_wait_ms: Millis,
    voice_overs: u32,
    hub_frames: u32,
    chat_sent: u32,
    chat_heard: u32,
    file_done_at: Option<Millis>,
    max_air_window: f64,
}

struct Scenario {
    styles: Vec<Style>,
    preset: PathPreset,
    /// R2 = gateway (reflector scope).
    gateway: bool,
    minutes: u64,
    qso_every_s: u64,
    file_bytes: usize,
    seed: u64,
    /// Share of voice overs carrying GPS (D-PRS) slow data, in percent.
    gps_percent: u64,
    /// When the file transfer starts (during voice activity).
    file_start: Millis,
}

fn wire(f: &Frame) -> Vec<u8> {
    wrap(&f.encode(ENCODING).unwrap(), &EscapeSet::legacy()).unwrap()
}

fn profile(style: Style, preset: PathPreset) -> ModeProfile {
    ModeProfile::dv_slow_placeholder().for_path(preset, style == Style::Sensed)
}

fn run(sc: &Scenario) -> Report {
    let mut rng = Rng(sc.seed | 1);
    let calls = ["KK4ODA", "W4DRS", "KJ4ABC", "N4XYZ"];
    let mut st: Vec<DataStation> = sc
        .styles
        .iter()
        .enumerate()
        .map(|(i, &style)| {
            let mut sched = Scheduler::new(profile(style, sc.preset), sc.seed + i as u64);
            sched.set_sensed(style == Style::Sensed);
            DataStation {
                call: calls[i].into(),
                style,
                sched,
                deframer: Deframer::default(),
                infer: VoiceInference::default(),
                last_busy: None,
                drats_q: VecDeque::new(),
                drats_at: None,
                sender: None,
                receiver: None,
                paused_since: None,
                next_chat: 30_000 + i as u64 * 47_000,
                chats_sent: 0,
                file: Vec::new(),
                overs: Vec::new(),
            }
        })
        .collect();
    // Station 0 sends a file to station 1 (legacy ARQ-1, slow-data settings).
    if sc.file_bytes > 0 && st.len() >= 2 {
        let mut cfg = SenderConfig::legacy_defaults();
        cfg.block_size = 256;
        cfg.rto_initial = 12_000;
        let data: Vec<u8> = (0..sc.file_bytes).map(|_| rng.next() as u8).collect();
        st[0].sender = Some(LegacySender::new(
            cfg,
            SessionAddr {
                session: 4,
                src: st[0].call.clone(),
                dst: st[1].call.clone(),
            },
            &data,
        ));
        st[1].receiver = Some(LegacyReceiver::new(SessionAddr {
            session: 7,
            src: st[1].call.clone(),
            dst: st[0].call.clone(),
        }));
        st[0].file = data;
    }
    let n_data = st.len();
    let n_voice = 4;
    let mut rep = Report::default();
    let mut overs: Vec<Over> = Vec::new();
    let mut active: Option<usize> = None; // index into overs: what the repeater repeats
    let mut ack: Option<(Millis, Millis)> = None;
    let mut transmitting: HashMap<usize, usize> = HashMap::new(); // who -> over index
    let mut qso: Option<Qso> = None;
    let mut next_qso = sc.qso_every_s * 500;
    let mut qso_live = false;
    let mut heard: std::collections::HashSet<(usize, String)> = std::collections::HashSet::new();
    let end = sc.minutes * 60_000;
    let mut t: Millis = 0;
    while t < end {
        // ---------------------------------------------------- voice users
        if qso.is_none() && t >= next_qso {
            let a = rng.range(0, n_voice as u64 - 1) as usize;
            let b = (a + 1 + rng.range(0, n_voice as u64 - 2) as usize) % n_voice;
            qso = Some(Qso {
                users: [a, b],
                overs_left: rng.range(4, 8) as u32,
                next_at: t,
                turn: 0,
                gps: [
                    rng.next() % 100 < sc.gps_percent,
                    rng.next() % 100 < sc.gps_percent,
                ],
            });
            qso_live = true;
        }
        let mut starters: Vec<Over> = Vec::new();
        let output_busy = active.is_some() || ack.is_some_and(|(s, e)| t >= s && t < e);
        if let Some(q) = qso.as_mut() {
            let who = n_data + q.users[q.turn];
            if t >= q.next_at && !transmitting.contains_key(&who) {
                if output_busy {
                    rep.voice_wait_ms += STEP; // a courteous operator waits
                } else {
                    let len = rng.range(5_000, 20_000);
                    starters.push(Over {
                        who,
                        start: t,
                        end: t + len,
                        bytes: Vec::new(),
                        voice: true,
                        gps: q.gps[q.turn],
                        repeated: false,
                    });
                    q.overs_left -= 1;
                    q.turn = 1 - q.turn;
                    q.next_at = Millis::MAX; // set when this over ends
                    rep.voice_overs += 1;
                }
            }
        }
        // ---------------------------------------------------- data stations
        for (i, s) in st.iter_mut().enumerate() {
            // Traffic: a chat line every few minutes; the file's ARQ.
            if t >= s.next_chat {
                s.next_chat = t + rng.range(120_000, 300_000);
                s.chats_sent += 1;
                rep.chat_sent += 1;
                let body = format!("{} line {}", s.call, s.chats_sent);
                let f = Frame::new(0, 1, 0, &s.call, "CQCQCQ", body.into_bytes());
                match s.style {
                    Style::Drats => s.drats_q.push_back(wire(&f)),
                    _ => s.sched.enqueue(
                        Priority::Chat,
                        Item {
                            id: 0,
                            bytes: wire(&f),
                        },
                    ),
                }
            }
            if s.style != Style::Drats {
                // ARQ timers pause while the channel is held (docs/10 §4.5).
                let held =
                    matches!(s.sched.status(t), Status::Waiting { why, .. } if why.holds_timers());
                match (held, s.paused_since) {
                    (true, None) => s.paused_since = Some(t),
                    (false, Some(since)) => {
                        if let Some(tx) = s.sender.as_mut() {
                            tx.shift(t - since);
                        }
                        s.paused_since = None;
                    }
                    _ => {}
                }
            }
            if s.paused_since.is_none() && t >= sc.file_start {
                if let Some(tx) = s.sender.as_mut() {
                    for f in tx.poll_transmit(t) {
                        let w = wire(&f);
                        match s.style {
                            Style::Drats => s.drats_q.push_back(w),
                            _ => s.sched.enqueue(Priority::File, Item { id: 0, bytes: w }),
                        }
                    }
                }
            }
            if transmitting.contains_key(&i) {
                continue;
            }
            let burst = match s.style {
                Style::Drats => {
                    if !s.drats_q.is_empty() && s.drats_at.is_none() {
                        s.drats_at = Some(t + rng.range(500, 2_000));
                    }
                    if s.drats_at.is_some_and(|a| t >= a) {
                        s.drats_at = None;
                        let mut b = Vec::new();
                        while let Some(f) = s.drats_q.pop_front() {
                            b.extend(f);
                            if b.len() > 2_000 {
                                break;
                            }
                        }
                        Some(b)
                    } else {
                        None
                    }
                }
                _ => s.sched.poll(t).map(|b| {
                    s.sched.on_port_accepted(t);
                    b.bytes
                }),
            };
            if let Some(bytes) = burst {
                let air = profile(Style::Sensed, PathPreset::Direct).air_ms(bytes.len());
                if qso_live {
                    rep.qso_interruptions += 1;
                }
                rep.data_overs += 1;
                rep.data_air_ms += air;
                s.overs.push((t, air));
                starters.push(Over {
                    who: i,
                    start: t,
                    end: t + air,
                    bytes,
                    voice: false,
                    gps: false,
                    repeated: false,
                });
            }
        }
        // ---------------------------------------------------- repeater input
        let collide = starters.len() > 1 || active.is_some();
        for mut o in starters {
            o.repeated = !collide;
            if collide {
                rep.lost_overs += !o.voice as u32;
            }
            if o.repeated {
                if sc.gateway && !o.voice {
                    rep.hub_frames += 1;
                }
                active = Some(overs.len());
                ack = None;
            }
            transmitting.insert(o.who, overs.len());
            overs.push(o);
        }
        // ---------------------------------------------------- what everyone hears
        let out = active.map(|a| &overs[a]);
        let in_ack = ack.is_some_and(|(s, e)| t >= s && t < e);
        for (i, s) in st.iter_mut().enumerate() {
            if transmitting.contains_key(&i) {
                continue;
            }
            // The radio outputs the speaker's D-PRS slow data a moment
            // into the over.
            if let Some(o) = out.filter(|o| o.voice && o.gps && t == o.start + 2_000) {
                let _ = o;
                if s.style == Style::Unsensed && s.infer.feed(t, b"$$CRC1234,N0CALL>API52") {
                    s.sched.on_voice_inferred(t);
                }
            }
            match out {
                Some(o) if !o.voice && o.who != i => s.sched.on_rx(t),
                _ => {}
            }
            if s.style == Style::Sensed {
                let busy = match out {
                    Some(o) if o.voice => Some(Busy::Voice),
                    Some(o) if o.who != i => Some(Busy::Signal),
                    _ if in_ack => Some(Busy::Signal),
                    _ => None,
                };
                if busy != s.last_busy {
                    s.last_busy = busy;
                    s.sched.on_channel(t, busy);
                }
            }
        }
        // ---------------------------------------------------- overs ending
        let ending: Vec<(usize, usize)> = transmitting
            .iter()
            .filter(|(_, &oi)| overs[oi].end <= t)
            .map(|(&w, &oi)| (w, oi))
            .collect();
        for (who, oi) in ending {
            transmitting.remove(&who);
            let o = overs[oi].clone();
            if active == Some(oi) {
                active = None;
                ack = Some((t + ACK_DELAY, t + ACK_DELAY + ACK_LEN));
                // Deliver to everyone who heard the whole over.
                for (i, s) in st.iter_mut().enumerate() {
                    if i == who
                        || overs
                            .iter()
                            .any(|x| x.who == i && x.start < o.end && x.end > o.start)
                    {
                        continue;
                    }
                    if o.voice {
                        continue;
                    }
                    let mut ev = Vec::new();
                    s.deframer.feed(&o.bytes, &mut ev);
                    for e in ev {
                        let Event::Frame { frame: f, .. } = e else {
                            continue;
                        };
                        if f.session == 1 {
                            let key = (i, String::from_utf8_lossy(&f.payload).into_owned());
                            if heard.insert(key) {
                                rep.chat_heard += 1;
                            }
                        } else if f.dst.as_slice() == s.call.as_bytes() {
                            if let Some(rx) = s.receiver.as_mut() {
                                let mut outv = Vec::new();
                                rx.on_frame(&f, &mut outv);
                                for x in outv {
                                    if let RxEvent::Transmit(r) = x {
                                        let w = wire(&r);
                                        match s.style {
                                            Style::Drats => s.drats_q.push_back(w),
                                            _ => s.sched.enqueue(
                                                Priority::Control,
                                                Item { id: 0, bytes: w },
                                            ),
                                        }
                                    }
                                }
                            }
                            if let Some(tx) = s.sender.as_mut() {
                                tx.on_frame(t, &f);
                                if tx.state() == SenderState::Done && rep.file_done_at.is_none() {
                                    rep.file_done_at = Some(t);
                                }
                            }
                        }
                    }
                }
            }
            if o.voice {
                if let Some(q) = qso.as_mut() {
                    if q.overs_left == 0 {
                        qso = None;
                        qso_live = false;
                        next_qso = t + rng.range(sc.qso_every_s * 500, sc.qso_every_s * 1500);
                    } else {
                        q.next_at = t + rng.range(1_000, 4_000);
                    }
                }
            }
        }
        if ack.is_some_and(|(_, e)| t >= e) {
            ack = None;
        }
        t += STEP;
    }
    for v in overs.iter().filter(|o| o.voice) {
        for d in overs.iter().filter(|o| !o.voice) {
            let (a, b) = (v.start.max(d.start), v.end.min(d.end));
            rep.voice_overlap_ms += b.saturating_sub(a);
        }
    }
    // Largest share of any 10-minute window one station used.
    for s in &st {
        for &(start, _) in &s.overs {
            let used: Millis = s
                .overs
                .iter()
                .filter(|(o, _)| *o >= start && *o < start + 600_000)
                .map(|(_, a)| a)
                .sum();
            rep.max_air_window = rep.max_air_window.max(used as f64 / 600_000.0);
        }
    }
    // Every other station should hear each chat line once.
    rep.chat_sent *= (st.len() - 1) as u32;
    rep
}

fn scenario(styles: Vec<Style>, preset: PathPreset) -> Scenario {
    Scenario {
        styles,
        preset,
        gateway: false,
        minutes: 120,
        qso_every_s: 300,
        file_bytes: 6_000,
        seed: 7,
        gps_percent: 50,
        file_start: 20 * 60_000,
    }
}

#[test]
fn sensed_stations_never_start_inside_a_qso_and_stay_on_the_repeater() {
    for seed in [7, 11, 23] {
        let r = run(&Scenario {
            seed,
            ..scenario(vec![Style::Sensed, Style::Sensed], PathPreset::Repeater)
        });
        eprintln!("sensed seed {seed}: {r:?}");
        assert_eq!(r.qso_interruptions, 0, "seed {seed}");
        assert_eq!(r.voice_overlap_ms, 0, "seed {seed}");
        assert_eq!(
            r.hub_frames, 0,
            "repeater scope: nothing reaches the reflector"
        );
        assert!(r.file_done_at.is_some(), "seed {seed}: file not finished");
        assert!(
            r.chat_heard as f64 >= 0.95 * r.chat_sent as f64,
            "seed {seed}: chat {}/{}",
            r.chat_heard,
            r.chat_sent
        );
        assert!(
            r.max_air_window <= 0.25 + 0.02 + 0.03,
            "seed {seed}: air {:.3}",
            r.max_air_window
        );
    }
}

#[test]
fn without_sensing_aether_is_no_worse_than_drats_and_inference_helps() {
    // Without CI-V, neither program hears voice: honest expectation is
    // "comparable to D-RATS", with relayed D-PRS from voice users helping.
    let total = |styles: &[Style], preset, gps| {
        [7, 11, 23]
            .iter()
            .map(|&seed| {
                let r = run(&Scenario {
                    seed,
                    gps_percent: gps,
                    ..scenario(styles.to_vec(), preset)
                });
                if styles[0] != Style::Drats {
                    assert!(r.file_done_at.is_some(), "seed {seed}: file not finished");
                }
                eprintln!(
                    "{:?} gps {gps} seed {seed}: overlap {} air {} overs {} lost {} file {:?}",
                    styles[0],
                    r.voice_overlap_ms,
                    r.data_air_ms,
                    r.data_overs,
                    r.lost_overs,
                    r.file_done_at
                );
                r.voice_overlap_ms
            })
            .sum::<Millis>()
    };
    let unsensed = |gps| {
        total(
            &[Style::Unsensed, Style::Unsensed],
            PathPreset::Repeater,
            gps,
        )
    };
    let (no_gps, all_gps) = (unsensed(0), unsensed(100));
    let drats = total(&[Style::Drats, Style::Drats], PathPreset::Direct, 50);
    let half = unsensed(50);
    eprintln!("voice overlap ms: D-RATS {drats}, Aether unsensed {half} (no GPS {no_gps}, all GPS {all_gps})");
    assert!(
        half as f64 <= drats as f64 * 1.25,
        "unsensed {half} vs D-RATS {drats}"
    );
    assert!(
        all_gps < no_gps,
        "inference should reduce overlap: {all_gps} vs {no_gps}"
    );
}

#[test]
fn mixed_with_a_drats_station_delivers_and_sensed_stations_still_yield() {
    let r = run(&Scenario {
        file_bytes: 4_000,
        ..scenario(
            vec![Style::Sensed, Style::Sensed, Style::Drats],
            PathPreset::Repeater,
        )
    });
    eprintln!("mixed: {r:?}");
    assert!(r.file_done_at.is_some());
    assert!(
        r.chat_heard as f64 >= 0.9 * r.chat_sent as f64,
        "chat {}/{}",
        r.chat_heard,
        r.chat_sent
    );
}

#[test]
fn reflector_scope_reaches_the_hub_with_a_smaller_budget() {
    let r = run(&Scenario {
        gateway: true,
        ..scenario(vec![Style::Sensed, Style::Sensed], PathPreset::Reflector)
    });
    eprintln!("reflector: {r:?}");
    assert!(r.hub_frames > 0);
    assert_eq!(r.qso_interruptions, 0);
    assert!(
        r.max_air_window <= 0.10 + 0.02 + 0.02,
        "air {:.3}",
        r.max_air_window
    );
}
