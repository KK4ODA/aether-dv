//! Repeater operation end to end (docs/10): the engine with a simulated
//! IC-9700 on two pseudo-terminals (DV Data port and CI-V port).
//! Validation level: Simulated. The radio model follows the IC-9700 CI-V
//! Reference Guide; real radios may differ (docs/10 §7, HW-R1..R4).
#![cfg(target_os = "linux")]

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::AsFd;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aether_ddt2::{wrap, Deframer, EscapeSet, Event, Frame, PayloadEncoding};
use aetherdv::config::Config;
use aetherdv::engine::{self, ApiEvent, Cmd, EngineHandle, RouteCmd};
use nix::pty::openpty;
use nix::unistd::ttyname;
use serde_json::Value;
use tokio::sync::{broadcast, oneshot};
use tokio::time::timeout;

const ADDR: u8 = 0xA2;

async fn ask(
    h: &EngineHandle,
    make: impl FnOnce(oneshot::Sender<Result<Value, engine::ApiError>>) -> Cmd,
) -> Result<Value, engine::ApiError> {
    let (tx, rx) = oneshot::channel();
    h.cmd.send(make(tx)).await.unwrap();
    rx.await.unwrap()
}

async fn wait_for(
    rx: &mut broadcast::Receiver<ApiEvent>,
    what: &str,
    pred: impl Fn(&ApiEvent) -> bool,
) -> ApiEvent {
    timeout(Duration::from_secs(10), async {
        loop {
            let ev = rx.recv().await.expect("event stream");
            if pred(&ev) {
                return ev;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

/// What the simulated radio holds.
struct Radio {
    ur: String,
    r1: String,
    r2: String,
    duplex: u8,
    status: u8,
    rx_calls: Option<Vec<u8>>,
    rx_msg: Option<Vec<u8>>,
    /// Frames to push unsolicited (CI-V transceive) on the next read.
    push: Vec<Vec<u8>>,
    writes: u32,
}

type Shared = Arc<Mutex<Radio>>;

fn pad(s: &str) -> String {
    format!("{s:<8}")
}

fn radio(ur: &str, r1: &str, r2: &str) -> Shared {
    Arc::new(Mutex::new(Radio {
        ur: pad(ur),
        r1: pad(r1),
        r2: pad(r2),
        duplex: 0x11,
        status: 0,
        rx_calls: None,
        rx_msg: None,
        push: Vec::new(),
        writes: 0,
    }))
}

/// The CI-V side of the simulated IC-9700 (USB echo on, like the radio's
/// default).
fn civ_side(mut f: File, r: Shared) {
    std::thread::spawn(move || {
        let mut d = aether_civ::Deframer::default();
        let mut buf = [0u8; 256];
        loop {
            let Ok(n) = f.read(&mut buf) else { return };
            if n == 0 {
                return;
            }
            for fr in d.feed(&buf[..n]) {
                let _ = f.write_all(&aether_civ::frame(fr.to, fr.from, &fr.body));
                if fr.to != ADDR {
                    continue;
                }
                let mut st = r.lock().unwrap();
                for p in std::mem::take(&mut st.push) {
                    let _ = f.write_all(&p);
                }
                let b = fr.body.as_slice();
                let reply: Vec<u8> = match b {
                    [0x1A, 0x05, 0x00, 0x77] => vec![0x1A, 0x05, 0x00, 0x77, 0],
                    [0x1F, 0x00] => [&[0x1F, 0x00][..], b"KK4ODA  9700"].concat(),
                    [0x1F, 0x01] => [
                        &[0x1F, 0x01][..],
                        st.ur.as_bytes(),
                        st.r1.as_bytes(),
                        st.r2.as_bytes(),
                    ]
                    .concat(),
                    [0x1F, 0x01, rest @ ..] if rest.len() == 24 => {
                        st.ur = String::from_utf8_lossy(&rest[..8]).into();
                        st.r1 = String::from_utf8_lossy(&rest[8..16]).into();
                        st.r2 = String::from_utf8_lossy(&rest[16..]).into();
                        st.writes += 1;
                        vec![0xFB]
                    }
                    [0x0F] => vec![0x0F, st.duplex],
                    [0x20, 0x02, 0x02] => vec![0x20, 0x02, 0x02, st.status],
                    [0x15, 0x01] => vec![0x15, 0x01, (st.status & 0x10 != 0) as u8],
                    [0x20, 0x00, 0x02] => match &st.rx_calls {
                        Some(c) => [&[0x20, 0x00, 0x02][..], c].concat(),
                        None => vec![0x20, 0x00, 0x02, 0xFF],
                    },
                    [0x20, 0x01, 0x02] => match &st.rx_msg {
                        Some(m) => [&[0x20, 0x01, 0x02][..], m].concat(),
                        None => vec![0x20, 0x01, 0x02, 0xFF],
                    },
                    _ => vec![0xFA],
                };
                let _ = f.write_all(&aether_civ::frame(0xE0, ADDR, &reply));
            }
        }
    });
}

/// Collects every frame the engine writes to the DV Data port.
struct DataSide {
    frames: Arc<Mutex<Vec<(Instant, Frame)>>>,
    writer: File,
}

fn data_side(f: File) -> DataSide {
    let frames = Arc::new(Mutex::new(Vec::new()));
    let (out, mut rd) = (frames.clone(), f.try_clone().unwrap());
    std::thread::spawn(move || {
        let mut d = Deframer::default();
        let mut buf = [0u8; 1024];
        loop {
            let Ok(n) = rd.read(&mut buf) else { return };
            if n == 0 {
                return;
            }
            let mut ev = Vec::new();
            d.feed(&buf[..n], &mut ev);
            for e in ev {
                if let Event::Frame { frame, .. } = e {
                    out.lock().unwrap().push((Instant::now(), frame));
                }
            }
        }
    });
    DataSide { frames, writer: f }
}

impl DataSide {
    fn sent(&self) -> Vec<Frame> {
        self.frames
            .lock()
            .unwrap()
            .iter()
            .map(|(_, f)| f.clone())
            .collect()
    }

    async fn wait_frame(&self, what: &str, pred: impl Fn(&Frame) -> bool) -> Instant {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some((t, _)) = self.frames.lock().unwrap().iter().find(|(_, f)| pred(f)) {
                return *t;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Bytes the radio outputs as if received over the air.
    fn receive(&mut self, f: &Frame) {
        let w = wrap(
            &f.encode(PayloadEncoding::Never).unwrap(),
            &EscapeSet::legacy(),
        )
        .unwrap();
        self.writer.write_all(&w).unwrap();
    }
}

struct Bench {
    h: EngineHandle,
    ev: broadcast::Receiver<ApiEvent>,
    data: DataSide,
    _keep: Vec<std::os::fd::OwnedFd>,
}

/// Start an engine with one IC-9700 port. `civ = None` = no CI-V channel.
async fn bench(route: &str, civ: Option<Shared>) -> Bench {
    let data = openpty(None, None).unwrap();
    let data_path = ttyname(data.slave.as_fd()).unwrap();
    let mut keep = vec![data.slave];
    let mut civ_cfg = String::new();
    if let Some(r) = civ {
        let c = openpty(None, None).unwrap();
        let civ_path = ttyname(c.slave.as_fd()).unwrap();
        civ_side(File::from(c.master), r);
        keep.push(c.slave);
        civ_cfg = format!(
            "[port.civ]\npath = \"{}\"\nallow_set = true\n",
            civ_path.display()
        );
    }
    let cfg = Config::parse(&format!(
        "callsign = \"KK4ODA\"\n[[port]]\nkind = \"serial\"\nname = \"rf\"\npath = \"{}\"\nprofile = \"icom-ic9700\"\n{civ_cfg}[port.route]\n{route}",
        data_path.display()
    ))
    .unwrap();
    let h = engine::start(&cfg).unwrap();
    let mut ev = h.events.subscribe();
    wait_for(&mut ev, "serial ready", |e| {
        e.topic == "port.state" && e.data["state"] == "ready"
    })
    .await;
    Bench {
        h,
        ev,
        data: data_side(File::from(data.master)),
        _keep: keep,
    }
}

const REPEATER: &str = "via = \"repeater\"\nrepeater = \"W4DOC B\"\n";

async fn chat(b: &Bench, body: &str) -> Result<Value, engine::ApiError> {
    let body = body.to_string();
    ask(&b.h, move |reply| Cmd::ChatSend {
        port: "rf".into(),
        dst: "W4DRS".into(),
        body,
        reply,
    })
    .await
}

fn has_text(f: &Frame, text: &str) -> bool {
    f.session == 1 && f.payload == text.as_bytes()
}

async fn route_state(b: &Bench) -> Value {
    ask(&b.h, Cmd::Ports).await.unwrap()[0]["route"].clone()
}

#[tokio::test]
async fn route_guard_holds_until_the_radio_is_set_for_the_repeater() {
    let r = radio("CQCQCQ", "W4DOC  B", "W4DOC  G");
    let mut b = bench(REPEATER, Some(r.clone())).await;
    let held = wait_for(&mut b.ev, "route held", |e| {
        e.topic == "port.route" && e.data["route"]["gate"] == "held"
    })
    .await;
    assert_eq!(held.data["route"]["code"], "would_reach_reflector");
    assert_eq!(held.data["route"]["source"], "civ");
    chat(&b, "held back").await.unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(b.data.sent().is_empty(), "nothing goes out while held");
    // The operator can't confirm past what the radio reports.
    let e = ask(&b.h, |reply| {
        Cmd::Route(RouteCmd::Confirm { port: "rf".into() }, reply)
    })
    .await
    .unwrap_err();
    assert!(e.message.contains("different route"), "{}", e.message);
    // The operator selects Local CQ; the radio announces the change.
    {
        let mut st = r.lock().unwrap();
        st.r2 = pad("");
        st.push.push(aether_civ::frame(
            0x00,
            ADDR,
            &[0x00, 0x00, 0x00, 0x15, 0x45, 0x04],
        ));
    }
    wait_for(&mut b.ev, "route open", |e| {
        e.topic == "port.route" && e.data["route"]["gate"] == "open"
    })
    .await;
    b.data
        .wait_frame("held chat", |f| has_text(f, "held back"))
        .await;
    let route = route_state(&b).await;
    assert_eq!(route["observed"]["calls"]["r2"], "");
    assert_eq!(route["myCallMatches"], true);
}

#[tokio::test]
async fn gateway_commands_in_ur_are_blocked() {
    let r = radio("REF030CL", "W4DOC  B", "W4DOC  G");
    let mut b = bench(&format!("{REPEATER}scope = \"reflector\"\n"), Some(r)).await;
    let e = wait_for(&mut b.ev, "blocked", |e| {
        e.topic == "port.route" && e.data["route"]["gate"] == "blocked"
    })
    .await;
    assert_eq!(e.data["route"]["code"], "ur_command");
}

#[tokio::test]
async fn voice_on_the_repeater_holds_traffic_until_the_voice_gap_ends() {
    let r = radio("CQCQCQ", "W4DOC  B", "");
    let mut b = bench(&format!("{REPEATER}voice_gap_ms = 2000\n"), Some(r.clone())).await;
    wait_for(&mut b.ev, "route open", |e| {
        e.topic == "port.route" && e.data["route"]["gate"] == "open"
    })
    .await;
    r.lock().unwrap().status = 0x50; // receiving a voice call
    wait_for(&mut b.ev, "voice sensed", |e| {
        e.topic == "port.channel" && e.data["channel"]["why"] == "voice"
    })
    .await;
    chat(&b, "after the QSO").await.unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(b.data.sent().is_empty(), "no transmission over voice");
    r.lock().unwrap().status = 0;
    let cleared = Instant::now();
    let sent = b
        .data
        .wait_frame("chat after voice", |f| has_text(f, "after the QSO"))
        .await;
    let gap = sent.duration_since(cleared);
    assert!(
        gap >= Duration::from_millis(1700),
        "voice gap respected ({gap:?})"
    );
}

#[tokio::test]
async fn radio_without_civ_needs_operator_confirmation() {
    let mut b = bench(REPEATER, None).await;
    chat(&b, "confirmed first").await.unwrap();
    let r = route_state(&b).await;
    assert_eq!(r["gate"], "confirm");
    assert_eq!(r["needs"]["route"], true);
    assert_eq!(r["required"]["r1"], "W4DOC  B");
    assert_eq!(r["required"]["r2"], "");
    tokio::time::sleep(Duration::from_millis(1000)).await;
    assert!(b.data.sent().is_empty());
    let after = ask(&b.h, |reply| {
        Cmd::Route(RouteCmd::Confirm { port: "rf".into() }, reply)
    })
    .await
    .unwrap();
    assert_eq!(after["gate"], "open");
    assert_eq!(after["source"], "operator");
    b.data
        .wait_frame("chat after confirm", |f| has_text(f, "confirmed first"))
        .await;
    // Without CI-V the channel sense is data only, plus inferred voice.
    let ch = ask(&b.h, Cmd::Ports).await.unwrap()[0]["channel"].clone();
    assert_eq!(ch["sensesVoice"], false);
    // Relayed D-PRS from a voice user's transmission starts the voice gap.
    b.data
        .writer
        .write_all(b"$$CRC6CD1,W4XYZ>API52,DSTAR*:!3345.00N/08423.00W>\r\n")
        .unwrap();
    wait_for(&mut b.ev, "inferred voice", |e| {
        e.topic == "port.channel" && e.data["channel"]["why"] == "voice_gap"
    })
    .await;
}

#[tokio::test]
async fn reflector_scope_confirmation_file_limit_and_no_broadcast_replies() {
    let r = radio("CQCQCQ", "W4DOC  B", "W4DOC  G");
    let mut b = bench(&format!("{REPEATER}scope = \"reflector\"\n"), Some(r)).await;
    let e = wait_for(&mut b.ev, "needs reflector confirmation", |e| {
        e.topic == "port.route" && e.data["route"]["gate"] == "confirm"
    })
    .await;
    assert_eq!(e.data["route"]["needs"]["reflector"], true);
    assert_eq!(e.data["route"]["answerBroadcast"], false);
    let dir = tempfile::tempdir().unwrap();
    let big = dir.path().join("big.bin");
    std::fs::write(&big, vec![7u8; 10_000]).unwrap();
    let small = dir.path().join("small.txt");
    std::fs::write(&small, b"short report").unwrap();
    let send = |path: std::path::PathBuf, confirm: bool| {
        ask(&b.h, move |reply| Cmd::SendFile {
            port: "rf".into(),
            peer: "W4DRS".into(),
            path,
            confirm_reflector: confirm,
            reply,
        })
    };
    let e = send(big, true).await.unwrap_err();
    assert!(e.message.contains("8 KiB"), "{}", e.message);
    let e = send(small.clone(), false).await.unwrap_err();
    assert!(e.message.contains("confirmReflector"), "{}", e.message);
    // A broadcast ping heard on the reflector is not answered.
    b.data.receive(&Frame::new(
        0,
        1,
        1,
        "W4DRS",
        "CQCQCQ",
        b"Ping Request".to_vec(),
    ));
    wait_for(&mut b.ev, "ping not answered", |e| {
        e.topic == "engine.event"
            && e.data["message"]
                .as_str()
                .unwrap_or("")
                .contains("broadcast request not answered")
    })
    .await;
    ask(&b.h, |reply| {
        Cmd::Route(RouteCmd::Confirm { port: "rf".into() }, reply)
    })
    .await
    .unwrap();
    send(small, true).await.unwrap();
    b.data.wait_frame("file offer", |f| f.session == 0).await;
}

#[tokio::test]
async fn operator_pause_holds_and_resume_releases() {
    let r = radio("CQCQCQ", "W4DOC  B", "");
    let mut b = bench(REPEATER, Some(r)).await;
    wait_for(&mut b.ev, "route open", |e| {
        e.topic == "port.route" && e.data["route"]["gate"] == "open"
    })
    .await;
    ask(&b.h, |reply| {
        Cmd::Route(
            RouteCmd::Pause {
                port: "rf".into(),
                minutes: None,
            },
            reply,
        )
    })
    .await
    .unwrap();
    chat(&b, "after the net").await.unwrap();
    wait_for(&mut b.ev, "paused", |e| {
        e.topic == "port.channel" && e.data["channel"]["why"] == "paused"
    })
    .await;
    tokio::time::sleep(Duration::from_millis(1000)).await;
    assert!(b.data.sent().is_empty());
    ask(&b.h, |reply| {
        Cmd::Route(RouteCmd::Resume { port: "rf".into() }, reply)
    })
    .await
    .unwrap();
    b.data
        .wait_frame("chat after resume", |f| has_text(f, "after the net"))
        .await;
}

#[tokio::test]
async fn access_check_reports_the_repeater_ack_and_link() {
    let r = radio("CQCQCQ", "W4DOC  B", "");
    let mut b = bench(REPEATER, Some(r.clone())).await;
    wait_for(&mut b.ev, "route open", |e| {
        e.topic == "port.route" && e.data["route"]["gate"] == "open"
    })
    .await;
    ask(&b.h, |reply| {
        Cmd::Route(RouteCmd::AccessCheck { port: "rf".into() }, reply)
    })
    .await
    .unwrap();
    b.data
        .wait_frame("check frame", |f| {
            f.dst.as_slice() == b"KK4ODA" && f.src.as_slice() == b"KK4ODA"
        })
        .await;
    // The repeater acknowledges (MMDVMHost style) and names its link.
    {
        let mut st = r.lock().unwrap();
        let mut calls = vec![0x00, 0x01];
        calls.extend_from_slice(b"W4DOC  B    KK4ODA  W4DOC  GW4DOC  B");
        st.rx_calls = Some(calls);
        let mut msg = b"REF001 C  BER: 0.0% ".to_vec();
        msg.extend_from_slice(b"W4DOC  B    ");
        st.rx_msg = Some(msg);
        st.status = 0x10;
    }
    tokio::time::sleep(Duration::from_millis(800)).await;
    r.lock().unwrap().status = 0;
    let e = wait_for(&mut b.ev, "check result", |e| e.topic == "port.accessCheck").await;
    let res = &e.data["result"];
    assert_eq!(res["ok"], true, "{res}");
    assert_eq!(res["ber"], "0.0%");
    assert_eq!(res["link"], "REF001 C");
    assert_eq!(route_state(&b).await["link"]["reflector"], "REF001 C");
}

#[tokio::test]
async fn route_writes_wait_for_hardware_testing() {
    let r = radio("CQCQCQ", "W4DOC  B", "W4DOC  G");
    let b = bench(REPEATER, Some(r.clone())).await;
    let e = ask(&b.h, |reply| {
        Cmd::Route(
            RouteCmd::Apply {
                port: "rf".into(),
                confirm: true,
            },
            reply,
        )
    })
    .await
    .unwrap_err();
    assert!(e.message.contains("HW-R1"), "{}", e.message);
    assert_eq!(r.lock().unwrap().writes, 0, "nothing written to the radio");
}
