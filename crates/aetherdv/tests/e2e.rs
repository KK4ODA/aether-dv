//! End-to-end tests of the Phase C slice. Validation level: Simulated
//! (our own engines, server and a PTY "radio"; no legacy D-RATS yet).

use std::time::Duration;

use aether_rfd::server::{Server, ServerConfig};
use aetherdv::config::Config;
use aetherdv::engine::{self, ApiEvent, Cmd, EngineHandle, MsgCmd};
use serde_json::{json, Value};
use tokio::sync::{broadcast, oneshot};
use tokio::time::timeout;

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
            eprintln!("EV {what}: {} {}", ev.topic, ev.data);
            if pred(&ev) {
                return ev;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

fn rfl_config(call: &str, addr: std::net::SocketAddr) -> Config {
    Config::parse(&format!(
        "callsign = \"{call}\"\n[[port]]\nkind = \"ratflector\"\nname = \"net\"\nhost = \"127.0.0.1\"\ntcp_port = {}\n",
        addr.port()
    ))
    .unwrap()
}

#[tokio::test]
async fn two_engines_chat_ping_and_version_through_aether_rfd() {
    let srv = Server::start("127.0.0.1:0", ServerConfig::default())
        .await
        .unwrap();
    let a = engine::start(&rfl_config("KK4ODA", srv.local_addr)).unwrap();
    let b = engine::start(&rfl_config("W4DRS", srv.local_addr)).unwrap();
    let (mut a_ev, mut b_ev) = (a.events.subscribe(), b.events.subscribe());
    let up = |e: &ApiEvent| e.topic == "port.state" && e.data["state"] == "up";
    wait_for(&mut a_ev, "A up", up).await;
    wait_for(&mut b_ev, "B up", up).await;

    // Broadcast chat A -> everyone
    let sent = ask(&a, |reply| Cmd::ChatSend {
        port: "net".into(),
        dst: "CQCQCQ".into(),
        body: "net is open".into(),
        reply,
    })
    .await
    .unwrap();
    assert_eq!(sent["txState"], "queued");
    let got = wait_for(&mut b_ev, "chat at B", |e| {
        e.topic == "chat.line" && e.data["direction"] == "in"
    })
    .await;
    assert_eq!(
        (got.data["src"].as_str(), got.data["body"].as_str()),
        (Some("KK4ODA"), Some("net is open"))
    );
    wait_for(&mut a_ev, "A transmitted", |e| {
        e.topic == "chat.line" && e.data["txState"] == "transmitted"
    })
    .await;

    // Ping B -> A, A answers
    ask(&b, |reply| Cmd::Ping {
        port: "net".into(),
        callsign: "KK4ODA".into(),
        reply,
    })
    .await
    .unwrap();
    let rsp = wait_for(&mut b_ev, "ping reply", |e| {
        e.topic == "chat.line" && e.data["kind"] == "ping_rsp"
    })
    .await;
    assert!(rsp.data["body"]
        .as_str()
        .unwrap()
        .starts_with("Running Aether DV"));

    // Version / capability query B -> A
    ask(&b, |reply| Cmd::QueryCapabilities {
        port: "net".into(),
        callsign: "KK4ODA".into(),
        reply,
    })
    .await
    .unwrap();
    let sw = wait_for(&mut b_ev, "version reply", |e| {
        e.topic == "station.heard" && e.data["software"].is_string()
    })
    .await;
    assert!(
        sw.data["software"].as_str().unwrap().contains("[aether"),
        "{}",
        sw.data
    );

    // History and stations are persisted
    let hist = ask(&b, |reply| Cmd::ChatHistory {
        port: Some("net".into()),
        limit: 10,
        reply,
    })
    .await
    .unwrap();
    assert!(hist
        .as_array()
        .unwrap()
        .iter()
        .any(|l| l["body"] == "net is open"));
    let st = ask(&b, Cmd::Stations).await.unwrap();
    assert!(st
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["callsign"] == "KK4ODA"));

    // Self-addressed traffic is refused (legacy P17)
    let e = ask(&a, |reply| Cmd::ChatSend {
        port: "net".into(),
        dst: "kk4oda".into(),
        body: "me".into(),
        reply,
    })
    .await
    .unwrap_err();
    assert_eq!(e.code, engine::E_SELF);
    srv.shutdown().await;
}

#[tokio::test]
async fn chat_containing_frame_marker_is_sent_compressed_and_arrives_intact() {
    let srv = Server::start("127.0.0.1:0", ServerConfig::default())
        .await
        .unwrap();
    let a = engine::start(&rfl_config("KK4ODA", srv.local_addr)).unwrap();
    let b = engine::start(&rfl_config("W4DRS", srv.local_addr)).unwrap();
    let (mut a_ev, mut b_ev) = (a.events.subscribe(), b.events.subscribe());
    let up = |e: &ApiEvent| e.topic == "port.state" && e.data["state"] == "up";
    wait_for(&mut a_ev, "A up", up).await;
    wait_for(&mut b_ev, "B up", up).await;
    let tricky = "literal [EOB] in a sentence";
    ask(&a, |reply| Cmd::ChatSend {
        port: "net".into(),
        dst: "CQCQCQ".into(),
        body: tricky.into(),
        reply,
    })
    .await
    .unwrap();
    let got = wait_for(&mut b_ev, "tricky chat", |e| {
        e.topic == "chat.line" && e.data["direction"] == "in"
    })
    .await;
    assert_eq!(got.data["body"], tricky);
    srv.shutdown().await;
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn rpc(ws: &mut Ws, id: u64, method: &str, params: Value) -> Value {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let req = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
    ws.send(Message::text(req.to_string())).await.unwrap();
    match timeout(Duration::from_secs(5), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
    {
        Message::Text(t) => serde_json::from_str(&t).unwrap(),
        m => panic!("unexpected {m:?}"),
    }
}

#[tokio::test]
async fn api_requires_token_and_serves_methods() {
    let cfg = Config::parse("callsign = \"KK4ODA\"").unwrap();
    let h = engine::start(&cfg).unwrap();
    let addr = aetherdv::api::serve("127.0.0.1:0", "secret".into(), h)
        .await
        .unwrap();
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
        .await
        .unwrap();
    assert_eq!(
        rpc(&mut ws, 1, "ports.list", json!({})).await["error"]["code"],
        1401
    );
    assert_eq!(
        rpc(
            &mut ws,
            2,
            "system.hello",
            json!({ "token": "wrong", "apiVersion": "0.1.0" })
        )
        .await["error"]["code"],
        1401
    );
    assert_eq!(
        rpc(
            &mut ws,
            3,
            "system.hello",
            json!({ "token": "secret", "apiVersion": "1.0.0" })
        )
        .await["error"]["code"],
        1426,
        "major version mismatch must be refused"
    );
    let hello = rpc(
        &mut ws,
        4,
        "system.hello",
        json!({ "token": "secret", "apiVersion": "0.1.0" }),
    )
    .await;
    assert_eq!(hello["result"]["profile"], "KK4ODA");
    assert_eq!(
        rpc(&mut ws, 5, "ports.list", json!({})).await["result"],
        json!([])
    );
    assert_eq!(
        rpc(&mut ws, 6, "no.such", json!({})).await["error"]["code"],
        -32601
    );
    assert_eq!(
        rpc(
            &mut ws,
            7,
            "chat.send",
            json!({ "portId": "nope", "dst": "CQCQCQ", "body": "x" })
        )
        .await["error"]["code"],
        1404
    );
}

#[cfg(target_os = "linux")]
mod serial {
    use super::*;
    use aether_ddt2::{wrap, Deframer, EscapeSet, Event, Frame, PayloadEncoding};
    use nix::pty::openpty;
    use nix::unistd::ttyname;
    use std::fs::File;
    use std::io::{Read, Write};
    use std::os::fd::AsFd;

    /// A simulated IC-9700 CI-V port: echoes requests (USB echo on, the
    /// radio default) and answers the Fast Data read/set at address A2h.
    fn civ_radio(mut f: File, fast: std::sync::Arc<std::sync::atomic::AtomicBool>) {
        use std::sync::atomic::Ordering;
        std::thread::spawn(move || {
            let mut d = aether_civ::Deframer::default();
            let mut buf = [0u8; 64];
            loop {
                let Ok(n) = f.read(&mut buf) else { return };
                if n == 0 {
                    return;
                }
                for fr in d.feed(&buf[..n]) {
                    let _ = f.write_all(&aether_civ::frame(fr.to, fr.from, &fr.body)); // echo
                    if fr.to != 0xA2 {
                        continue;
                    }
                    if !fr.body.starts_with(&[0x1A, 0x05, 0x00, 0x77]) {
                        // Like a real radio: NG for commands it doesn't take.
                        let _ = f.write_all(&aether_civ::frame(0xE0, 0xA2, &[0xFA]));
                        continue;
                    }
                    let reply = match &fr.body[4..] {
                        [] => vec![0x1A, 0x05, 0x00, 0x77, fast.load(Ordering::SeqCst) as u8],
                        [v @ (0 | 1)] => {
                            fast.store(*v == 1, Ordering::SeqCst);
                            vec![0xFB]
                        }
                        _ => vec![0xFA],
                    };
                    let _ = f.write_all(&aether_civ::frame(0xE0, 0xA2, &reply));
                }
            }
        });
    }

    #[tokio::test]
    async fn civ_startup_check_read_and_guarded_set() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        let data = openpty(None, None).unwrap();
        let civ = openpty(None, None).unwrap();
        let data_path = ttyname(data.slave.as_fd()).unwrap();
        let civ_path = ttyname(civ.slave.as_fd()).unwrap();
        let fast = Arc::new(AtomicBool::new(true)); // radio left in Fast Data
        civ_radio(File::from(civ.master), fast.clone());
        let _keep = (data.master, civ.slave, data.slave);
        let cfg_text = |allow: bool| {
            format!(
                "callsign = \"KK4ODA\"\n[[port]]\nkind = \"serial\"\nname = \"rf\"\npath = \"{}\"\nprofile = \"icom-ic9700\"\nmode = \"slow\"\n[port.civ]\npath = \"{}\"\nallow_set = {allow}\n",
                data_path.display(),
                civ_path.display()
            )
        };
        let h = engine::start(&Config::parse(&cfg_text(true)).unwrap()).unwrap();
        let mut ev = h.events.subscribe();
        // Start-up check: radio in Fast Data, port configured slow -> warning.
        let warn = wait_for(&mut ev, "mode mismatch warning", |e| {
            e.topic == "engine.event"
                && e.data["message"]
                    .as_str()
                    .unwrap_or("")
                    .contains("cannot decode each other")
        })
        .await;
        assert_eq!(warn.data["level"], "warn");
        let read = ask(&h, |reply| Cmd::RadioMode {
            port: "rf".into(),
            set: None,
            confirm: false,
            reply,
        })
        .await
        .unwrap();
        assert_eq!(
            (read["fastData"].as_bool(), read["matches"].as_bool()),
            (Some(true), Some(false))
        );
        // Setting needs an explicit confirm.
        let e = ask(&h, |reply| Cmd::RadioMode {
            port: "rf".into(),
            set: Some(false),
            confirm: false,
            reply,
        })
        .await
        .unwrap_err();
        assert!(e.message.contains("confirm"), "{}", e.message);
        let set = ask(&h, |reply| Cmd::RadioMode {
            port: "rf".into(),
            set: Some(false),
            confirm: true,
            reply,
        })
        .await
        .unwrap();
        assert_eq!(
            (set["fastData"].as_bool(), set["matches"].as_bool()),
            (Some(false), Some(true))
        );
        assert!(
            !fast.load(Ordering::SeqCst),
            "the simulated radio was switched"
        );
        let ports = ask(&h, Cmd::Ports).await.unwrap();
        assert_eq!(ports[0]["radioFastData"], false);
        assert_eq!(ports[0]["civ"], true);
    }

    #[tokio::test]
    async fn civ_set_is_refused_unless_allowed_and_ports_without_civ_say_so() {
        let data = openpty(None, None).unwrap();
        let data_path = ttyname(data.slave.as_fd()).unwrap();
        let cfg = Config::parse(&format!(
            "callsign = \"KK4ODA\"\n[[port]]\nkind = \"serial\"\nname = \"rf\"\npath = \"{}\"\nprofile = \"icom-ic9700\"\n[port.civ]\npath = \"/nonexistent/civ\"\n[[port]]\nkind = \"serial\"\nname = \"plain\"\npath = \"/nonexistent/data\"\nprofile = \"kenwood-tmd750\"\n",
            data_path.display()
        ))
        .unwrap();
        let h = engine::start(&cfg).unwrap();
        let e = ask(&h, |reply| Cmd::RadioMode {
            port: "rf".into(),
            set: Some(true),
            confirm: true,
            reply,
        })
        .await
        .unwrap_err();
        assert!(e.message.contains("allow_set"), "{}", e.message);
        let e = ask(&h, |reply| Cmd::RadioMode {
            port: "plain".into(),
            set: None,
            confirm: false,
            reply,
        })
        .await
        .unwrap_err();
        assert!(e.message.contains("no CI-V channel"), "{}", e.message);
        // An unreachable CI-V port is an honest error, not a guess.
        let e = ask(&h, |reply| Cmd::RadioMode {
            port: "rf".into(),
            set: None,
            confirm: false,
            reply,
        })
        .await
        .unwrap_err();
        assert!(e.message.contains("cannot open CI-V port"), "{}", e.message);
        assert!(Config::parse("callsign = \"KK4ODA\"\n[[port]]\nkind = \"serial\"\nname = \"x\"\npath = \"/dev/null\"\nprofile = \"kenwood-tmd750\"\n[port.civ]\npath = \"/dev/null\"\n").is_err(), "CI-V on a profile without a documented command");
    }

    #[tokio::test]
    async fn radio_gps_tx_output_outside_frames_becomes_a_position() {
        let pty = openpty(None, None).unwrap();
        let slave = ttyname(pty.slave.as_fd()).unwrap();
        let mut radio = File::from(pty.master);
        let cfg = Config::parse(&format!(
            "callsign = \"KK4ODA\"\n[[port]]\nkind = \"serial\"\nname = \"rf\"\npath = \"{}\"\nprofile = \"test-pty\"\n",
            slave.display()
        ))
        .unwrap();
        let h = engine::start(&cfg).unwrap();
        let mut ev = h.events.subscribe();
        wait_for(&mut ev, "serial ready", |e| {
            e.topic == "port.state" && e.data["state"] == "ready"
        })
        .await;
        // What an Icom radio emits for a received DPRS beacon (sample from
        // D-Rats transport.py), in two pieces with noise around it.
        radio
            .write_all(
                b"\x00\x11$$CRC6CD1,Hills-Water-Treat-Plt>APRATS,DSTAR*:@233208h4529.05N/123",
            )
            .unwrap();
        radio
            .write_all(b"05.91W>Washington County ARES;Hills Water Treat Pl\r\nxx")
            .unwrap();
        let got = wait_for(&mut ev, "position", |e| e.topic == "station.position").await;
        assert_eq!(got.data["callsign"], "HILLS-WATER-TREAT-PLT");
        assert_eq!(got.data["sourceFmt"], "dprs");
        assert!((got.data["lat"].as_f64().unwrap() - 45.484166).abs() < 1e-4);
    }

    #[tokio::test]
    async fn engine_talks_to_a_pty_radio_and_detects_unplug() {
        let pty = openpty(None, None).unwrap();
        let slave = ttyname(pty.slave.as_fd()).unwrap();
        let mut radio = File::from(pty.master);
        let cfg = Config::parse(&format!(
            "callsign = \"KK4ODA\"\n[[port]]\nkind = \"serial\"\nname = \"rf\"\npath = \"{}\"\nprofile = \"test-pty\"\n",
            slave.display()
        ))
        .unwrap();
        let h = engine::start(&cfg).unwrap();
        let mut ev = h.events.subscribe();
        wait_for(&mut ev, "serial ready", |e| {
            e.topic == "port.state" && e.data["state"] == "ready"
        })
        .await;

        // A legacy D-RATS station transmits a chat line; the radio outputs it.
        let f = Frame::new(0, 1, 0, "W4DRS", "CQCQCQ", b"QRV on the net".to_vec());
        radio
            .write_all(
                &wrap(
                    &f.encode(PayloadEncoding::Never).unwrap(),
                    &EscapeSet::legacy(),
                )
                .unwrap(),
            )
            .unwrap();
        let got = wait_for(&mut ev, "rx chat", |e| {
            e.topic == "chat.line" && e.data["direction"] == "in"
        })
        .await;
        assert_eq!(got.data["body"], "QRV on the net");

        // We send; after the hold-off (1 s after RX) the radio receives a valid frame.
        ask(&h, |reply| Cmd::ChatSend {
            port: "rf".into(),
            dst: "W4DRS".into(),
            body: "roger".into(),
            reply,
        })
        .await
        .unwrap();
        let bytes = tokio::task::spawn_blocking(move || {
            let mut d = Deframer::default();
            let mut buf = [0u8; 512];
            let mut events = Vec::new();
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while std::time::Instant::now() < deadline {
                let n = radio.read(&mut buf).unwrap();
                d.feed(&buf[..n], &mut events);
                if let Some(Event::Frame { frame, .. }) =
                    events.iter().find(|e| matches!(e, Event::Frame { .. }))
                {
                    return (frame.clone(), radio);
                }
            }
            panic!("no frame reached the radio");
        })
        .await
        .unwrap();
        let (frame, radio) = bytes;
        assert_eq!(
            (
                frame.session,
                frame.kind,
                frame.dst.as_slice(),
                frame.payload.as_slice()
            ),
            (1, 0, b"W4DRS".as_slice(), b"roger".as_slice())
        );
        assert!(!frame.compressed, "legacy chat frames are uncompressed");

        // Unplug: the radio side disappears; the port reports lost.
        drop(radio);
        drop(pty.slave);
        wait_for(&mut ev, "lost", |e| {
            e.topic == "port.state" && e.data["state"] == "lost"
        })
        .await;
    }
}

#[tokio::test]
async fn ratflector_driver_enables_tcp_keepalive() {
    // N6.1: the driver's keepalive settings (60 s idle, 15 s interval).
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let s = tokio::net::TcpStream::connect(l.local_addr().unwrap())
        .await
        .unwrap();
    aetherdv::ports::enable_keepalive(&s).unwrap();
    let sock = socket2::SockRef::from(&s);
    assert!(sock.keepalive().unwrap());
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    assert_eq!(sock.tcp_keepalive_time().unwrap(), Duration::from_secs(60));
    #[cfg(target_os = "linux")]
    assert_eq!(
        sock.tcp_keepalive_interval().unwrap(),
        Duration::from_secs(15)
    );
}

fn rfl_config_files(call: &str, addr: std::net::SocketAddr, dir: &std::path::Path) -> Config {
    Config::parse(&format!(
        "callsign = \"{call}\"\nfiles_dir = {:?}\n[[port]]\nkind = \"ratflector\"\nname = \"net\"\nhost = \"127.0.0.1\"\ntcp_port = {}\n",
        dir.to_str().unwrap(),
        addr.port()
    ))
    .unwrap()
}

#[tokio::test]
async fn file_and_form_transfer_through_aether_rfd() {
    let srv = Server::start("127.0.0.1:0", ServerConfig::default())
        .await
        .unwrap();
    let (da, db) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let a = engine::start(&rfl_config_files("KK4ODA", srv.local_addr, da.path())).unwrap();
    let b = engine::start(&rfl_config_files("W4DRS", srv.local_addr, db.path())).unwrap();
    let (mut a_ev, mut b_ev) = (a.events.subscribe(), b.events.subscribe());
    let up = |e: &ApiEvent| e.topic == "port.state" && e.data["state"] == "up";
    wait_for(&mut a_ev, "A up", up).await;
    wait_for(&mut b_ev, "B up", up).await;

    // A 10 KB file (C1 acceptance size), partly incompressible, with a
    // hostile-looking name that must still land inside B's files dir.
    let mut content = Vec::new();
    let mut x = 1u32;
    for i in 0..10_240u32 {
        x = x.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        content.push(if i % 2 == 0 {
            (x >> 16) as u8
        } else {
            b'A' + (i % 26) as u8
        });
    }
    let src = da.path().join("sitrep 0900.txt");
    std::fs::write(&src, &content).unwrap();
    let t = ask(&a, |reply| Cmd::SendFile {
        confirm_reflector: false,
        port: "net".into(),
        peer: "w4drs".into(),
        path: src.clone(),
        reply,
    })
    .await
    .unwrap();
    assert_eq!(
        (t["direction"].as_str(), t["protocol"].as_str()),
        (Some("send"), Some("legacy_file"))
    );
    let id = t["id"].as_str().unwrap().to_string();

    let rx_done = wait_for(&mut b_ev, "B received", |e| {
        e.topic == "transfer.progress"
            && e.data["direction"] == "receive"
            && e.data["state"] != "offering"
            && e.data["state"] != "sending"
    })
    .await;
    assert_eq!(rx_done.data["state"], "done", "{}", rx_done.data);
    let tx_done = wait_for(&mut a_ev, "A done", |e| {
        e.topic == "transfer.progress" && e.data["id"] == id.as_str() && e.data["state"] == "done"
    })
    .await;
    assert_eq!(tx_done.data["doneBytes"], tx_done.data["totalBytes"]);
    let saved = db.path().join("sitrep 0900.txt");
    assert_eq!(std::fs::read(&saved).unwrap(), content);

    let list = ask(&b, |reply| Cmd::Transfers {
        include_finished: true,
        reply,
    })
    .await
    .unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["filename"], "sitrep 0900.txt");
    let active = ask(&b, |reply| Cmd::Transfers {
        include_finished: false,
        reply,
    })
    .await
    .unwrap();
    assert!(active.as_array().unwrap().is_empty());

    // An ICS-213 message B -> A: draft, send, hand-off, inbox at A.
    let draft = ask(&b, |reply| {
        Cmd::Message(
            MsgCmd::CreateDraft {
                form_id: "ICS213_US_OS".into(),
                fields: json!({
                    "incident": "Flood & Wind",
                    "subject": "Need 40 cots",
                    "message": "Shelter A needs 40 cots <today>.\nCall \"Ops\".",
                })
                .as_object()
                .unwrap()
                .clone(),
                dst: Some("kk4oda".into()),
            },
            reply,
        )
    })
    .await
    .unwrap();
    assert_eq!(
        (draft["state"].as_str(), draft["folder"].as_str()),
        (Some("draft"), Some("drafts"))
    );
    assert!(draft["mid"].as_str().unwrap().starts_with("W4DRS."));
    assert!(draft["xml"]
        .as_str()
        .unwrap()
        .contains("Flood &amp;amp; Wind"));
    let mid = draft["id"].as_str().unwrap().to_string();
    // B heard A on "net" during the file transfer, so no port is needed.
    let sent = ask(&b, |reply| {
        Cmd::Message(
            MsgCmd::Send {
                id: mid.clone(),
                port: None,
            },
            reply,
        )
    })
    .await
    .unwrap();
    assert_eq!(sent["folder"], "outbox");
    let handed = wait_for(&mut b_ev, "B handed off", |e| {
        e.topic == "message.state" && e.data["message"]["state"] == "handed_off"
    })
    .await;
    assert_eq!(handed.data["message"]["folder"], "sent");
    assert_eq!(handed.data["event"]["evidence"], "arq_ack");
    let got = wait_for(&mut a_ev, "A received message", |e| {
        e.topic == "message.state" && e.data["message"]["state"] == "received"
    })
    .await;
    assert_eq!(got.data["message"]["subject"], "Need 40 cots");
    assert_eq!(got.data["message"]["src"], "W4DRS");
    let full = ask(&a, |reply| {
        Cmd::Message(
            MsgCmd::Get {
                id: got.data["message"]["id"].as_str().unwrap().into(),
            },
            reply,
        )
    })
    .await
    .unwrap();
    assert_eq!(
        full["fields"]["message"],
        "Shelter A needs 40 cots <today>.\nCall \"Ops\"."
    );
    assert_eq!(full["fields"]["incident"], "Flood & Wind");
    assert!(full["xml"]
        .as_str()
        .unwrap()
        .contains("<e>W4DRS</e><e>KK4ODA</e>"));
    let events = ask(&b, |reply| {
        Cmd::Message(MsgCmd::Events { id: mid.clone() }, reply)
    })
    .await
    .unwrap();
    let states: Vec<&str> = events
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["state"].as_str().unwrap())
        .collect();
    assert_eq!(states, vec!["draft", "queued", "sending", "handed_off"]);
    // Sending it again is refused; it is no longer a draft.
    assert!(ask(&b, |reply| Cmd::Message(
        MsgCmd::Send {
            id: mid.clone(),
            port: None
        },
        reply
    ))
    .await
    .is_err());
}

#[tokio::test]
async fn transfer_to_an_absent_station_can_be_cancelled_and_bad_requests_fail() {
    let srv = Server::start("127.0.0.1:0", ServerConfig::default())
        .await
        .unwrap();
    let d = tempfile::tempdir().unwrap();
    let a = engine::start(&rfl_config_files("KK4ODA", srv.local_addr, d.path())).unwrap();
    let mut a_ev = a.events.subscribe();
    wait_for(&mut a_ev, "A up", |e| {
        e.topic == "port.state" && e.data["state"] == "up"
    })
    .await;
    let f = d.path().join("x.txt");
    std::fs::write(&f, b"hello").unwrap();
    let send = |peer: &str, path: std::path::PathBuf| {
        let peer = peer.to_string();
        let a = a.clone();
        async move {
            ask(&a, |reply| Cmd::SendFile {
                confirm_reflector: false,
                port: "net".into(),
                peer,
                path,
                reply,
            })
            .await
        }
    };
    assert_eq!(
        send("KK4ODA", f.clone()).await.unwrap_err().code,
        engine::E_SELF
    );
    assert_eq!(
        send("CQCQCQ", f.clone()).await.unwrap_err().code,
        engine::E_INVALID
    );
    assert_eq!(
        send("N0CALL", d.path().join("missing"))
            .await
            .unwrap_err()
            .code,
        engine::E_NOT_FOUND
    );
    let t = send("N0CALL", f).await.unwrap();
    assert_eq!(t["state"], "opening");
    let id = t["id"].as_str().unwrap().to_string();
    ask(&a, |reply| Cmd::CancelTransfer {
        id: id.clone(),
        reply,
    })
    .await
    .unwrap();
    let ev = wait_for(&mut a_ev, "cancelled", |e| {
        e.topic == "transfer.progress" && e.data["id"] == id.as_str() && e.data["state"] == "failed"
    })
    .await;
    assert_eq!(ev.data["stateReason"], "cancelled");
    assert!(ask(&a, |reply| Cmd::CancelTransfer { id, reply })
        .await
        .is_err());
}

#[tokio::test]
async fn wire_capture_decodes_and_support_bundle_excludes_secrets() {
    use aetherdv::capture::{self, Record};
    use aetherdv::engine::DiagCmd;
    let srv = Server::start("127.0.0.1:0", ServerConfig::default())
        .await
        .unwrap();
    let (da, db) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let cfg = |call: &str, dir: &std::path::Path| {
        Config::parse(&format!(
            "callsign = \"{call}\"\nfiles_dir = {:?}\ncapture_dir = {:?}\n[[port]]\nkind = \"ratflector\"\nname = \"net\"\nhost = \"127.0.0.1\"\ntcp_port = {}\npassword = \"s3cret-pass\"\n",
            dir.join("rx").to_str().unwrap(),
            dir.join("caps").to_str().unwrap(),
            srv.local_addr.port()
        ))
        .unwrap()
    };
    let a = engine::start(&cfg("KK4ODA", da.path())).unwrap();
    let b = engine::start(&cfg("W4DRS", db.path())).unwrap();
    let (mut a_ev, mut b_ev) = (a.events.subscribe(), b.events.subscribe());
    let up = |e: &ApiEvent| e.topic == "port.state" && e.data["state"] == "up";
    wait_for(&mut a_ev, "A up", up).await;
    wait_for(&mut b_ev, "B up", up).await;

    let cap = ask(&a, |reply| {
        Cmd::Diag(DiagCmd::CaptureStart { port: "net".into() }, reply)
    })
    .await
    .unwrap();
    assert!(ask(&a, |reply| Cmd::Diag(
        DiagCmd::CaptureStart { port: "net".into() },
        reply
    ))
    .await
    .is_err());
    ask(&a, |reply| Cmd::ChatSend {
        port: "net".into(),
        dst: "CQCQCQ".into(),
        body: "captured line".into(),
        reply,
    })
    .await
    .unwrap();
    ask(&b, |reply| Cmd::ChatSend {
        port: "net".into(),
        dst: "KK4ODA".into(),
        body: "reply into the capture".into(),
        reply,
    })
    .await
    .unwrap();
    wait_for(&mut a_ev, "A got reply", |e| {
        e.topic == "chat.line" && e.data["body"] == "reply into the capture"
    })
    .await;
    let stopped = ask(&a, |reply| {
        Cmd::Diag(
            DiagCmd::CaptureStop {
                id: cap["captureId"].as_str().unwrap().into(),
            },
            reply,
        )
    })
    .await
    .unwrap();
    let path = std::path::PathBuf::from(stopped["path"].as_str().unwrap());
    let (header, recs) = capture::read(&path).unwrap();
    assert_eq!(header["port"], "net");
    assert_eq!(header["callsign"], "KK4ODA");
    // Replay both directions through the codec: the exact frames come back.
    let mut texts = Vec::new();
    let (mut drx, mut dtx) = (
        aether_ddt2::Deframer::default(),
        aether_ddt2::Deframer::default(),
    );
    for r in &recs {
        let mut ev = Vec::new();
        match r {
            Record::Rx { bytes, .. } => drx.feed(bytes, &mut ev),
            Record::Tx { bytes, .. } => dtx.feed(bytes, &mut ev),
            Record::Note { .. } | Record::Civ { .. } | Record::Event { .. } => {}
        }
        for e in ev {
            if let aether_ddt2::Event::Frame { frame, .. } = e {
                texts.push(String::from_utf8_lossy(&frame.payload).into_owned());
            }
        }
    }
    assert!(texts.contains(&"captured line".to_string()), "{texts:?}");
    assert!(
        texts.contains(&"reply into the capture".to_string()),
        "{texts:?}"
    );
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(
        !raw.contains(&hex_of("s3cret-pass")),
        "password must not be captured"
    );

    let bundle = ask(&a, |reply| {
        Cmd::Diag(
            DiagCmd::SupportBundle {
                include_captures: true,
            },
            reply,
        )
    })
    .await
    .unwrap();
    let names: Vec<&str> = bundle["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    for n in [
        "README.txt",
        "about.json",
        "events.jsonl",
        "transfers.json",
        "messages.json",
    ] {
        assert!(names.contains(&n), "{names:?}");
    }
    assert!(names
        .iter()
        .any(|n| n.starts_with("captures/") && n.ends_with(".aecap")));
    let tar = std::fs::read(bundle["path"].as_str().unwrap()).unwrap();
    let tar_text = String::from_utf8_lossy(&tar);
    assert!(
        !tar_text.contains("s3cret-pass"),
        "password must not be in the bundle"
    );
    assert!(tar_text.contains("(set, not included)"));
}

fn hex_of(s: &str) -> String {
    s.bytes().map(|b| format!("{b:02x}")).collect()
}

#[tokio::test]
async fn position_report_sent_by_one_engine_is_mapped_by_the_other() {
    use aetherdv::engine::PosCmd;
    let srv = Server::start("127.0.0.1:0", ServerConfig::default())
        .await
        .unwrap();
    let a = engine::start(&rfl_config("KK4ODA", srv.local_addr)).unwrap();
    let mut bc = rfl_config("W4DRS", srv.local_addr);
    bc.position = Some(aetherdv::config::PositionConfig {
        lat: 33.749,
        lon: -84.388,
        alt_m: Some(320.0),
        comment: "Fulton EOC".into(),
        symbol: "/-".into(),
    });
    let b = engine::start(&bc).unwrap();
    let (mut a_ev, mut b_ev) = (a.events.subscribe(), b.events.subscribe());
    let up = |e: &ApiEvent| e.topic == "port.state" && e.data["state"] == "up";
    wait_for(&mut a_ev, "A up", up).await;
    wait_for(&mut b_ev, "B up", up).await;
    // A has no [position]: asking to send one is an error, not a guess.
    assert!(ask(&a, |reply| Cmd::Position(
        PosCmd::Send {
            port: "net".into(),
            format: None
        },
        reply
    ))
    .await
    .is_err());
    for fmt in ["dprs", "nmea"] {
        let sent = ask(&b, |reply| {
            Cmd::Position(
                PosCmd::Send {
                    port: "net".into(),
                    format: Some(fmt.into()),
                },
                reply,
            )
        })
        .await
        .unwrap();
        assert_eq!(sent["format"], fmt);
        let got = wait_for(&mut a_ev, "position at A", |e| {
            e.topic == "station.position"
                && e.data["callsign"] == "W4DRS"
                && e.data["sourceFmt"] == fmt
        })
        .await;
        assert!(
            (got.data["lat"].as_f64().unwrap() - 33.749).abs() < 0.001,
            "{}",
            got.data
        );
        assert!((got.data["lon"].as_f64().unwrap() + 84.388).abs() < 0.001);
    }
    let latest = ask(&a, |reply| {
        Cmd::Position(
            PosCmd::List {
                callsign: None,
                since: None,
            },
            reply,
        )
    })
    .await
    .unwrap();
    assert_eq!(latest.as_array().unwrap().len(), 1);
    assert_eq!(latest[0]["comment"], "Fulton EOC");
    let hist = ask(&a, |reply| {
        Cmd::Position(
            PosCmd::List {
                callsign: Some("w4drs".into()),
                since: None,
            },
            reply,
        )
    })
    .await
    .unwrap();
    assert_eq!(hist.as_array().unwrap().len(), 2);
}
