//! End-to-end tests of the Phase C slice. Validation level: Simulated
//! (our own engines, server and a PTY "radio"; no legacy D-RATS yet).

use std::time::Duration;

use aether_rfd::server::{Server, ServerConfig};
use aetherd::config::Config;
use aetherd::engine::{self, ApiEvent, Cmd, EngineHandle};
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
    let up = |e: &ApiEvent| e.topic == "port" && e.data["state"] == "up";
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
        e.topic == "chat" && e.data["direction"] == "in"
    })
    .await;
    assert_eq!(
        (got.data["src"].as_str(), got.data["body"].as_str()),
        (Some("KK4ODA"), Some("net is open"))
    );
    wait_for(&mut a_ev, "A transmitted", |e| {
        e.topic == "chat_state" && e.data["txState"] == "transmitted"
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
        e.topic == "chat" && e.data["kind"] == "ping_rsp"
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
        e.topic == "station" && e.data["software"].is_string()
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
    let up = |e: &ApiEvent| e.topic == "port" && e.data["state"] == "up";
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
        e.topic == "chat" && e.data["direction"] == "in"
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
    let addr = aetherd::api::serve("127.0.0.1:0", "secret".into(), h)
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
            e.topic == "port" && e.data["state"] == "ready"
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
            e.topic == "chat" && e.data["direction"] == "in"
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
            e.topic == "port" && e.data["state"] == "lost"
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
    aetherd::ports::enable_keepalive(&s).unwrap();
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
