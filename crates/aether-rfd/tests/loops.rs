//! Loop / amplification simulation for ratflector meshes (risk R7/R8).
//! Validation level: Simulated. Each link has a fixed latency; frames are
//! processed in time order. A time limit and a forward cap bound the legacy
//! explosion.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use aether_ddt2::{wrap, EscapeSet, Frame, PayloadEncoding};
use aether_rfd::{Config, Hub, LinkId, Millis};

#[derive(Clone, Copy, PartialEq)]
enum Node {
    Hub(usize),
    Client(usize),
}

/// (node a, link id at a, node b, link id at b, latency, one-way a->b only)
type Link = (Node, LinkId, Node, LinkId, Millis, bool);
/// (arrival time, tie-break sequence, hub index, link id at that hub, frame index)
type Arrival = Reverse<(Millis, u64, usize, LinkId, usize)>;

struct Net {
    hubs: Vec<Hub>,
    links: Vec<Link>,
    client_rx: Vec<u32>,
}

impl Net {
    fn new(n_hubs: usize, n_clients: usize, cfg: &Config) -> Self {
        Net {
            hubs: (0..n_hubs).map(|_| Hub::new(cfg.clone())).collect(),
            links: vec![],
            client_rx: vec![0; n_clients],
        }
    }

    fn connect(&mut self, a: Node, b: Node, latency: Millis) {
        self.connect_dir(a, b, latency, false)
    }

    /// RF paths can be asymmetric: `b` hears `a` but `a` does not hear `b`.
    fn connect_one_way(&mut self, a: Node, b: Node, latency: Millis) {
        self.connect_dir(a, b, latency, true)
    }

    fn connect_dir(&mut self, a: Node, b: Node, latency: Millis, one_way: bool) {
        let id_a = self.links.len() as LinkId * 2;
        let id_b = id_a + 1;
        if let Node::Hub(h) = a {
            self.hubs[h].add_link(id_a);
        }
        if let Node::Hub(h) = b {
            self.hubs[h].add_link(id_b);
        }
        self.links.push((a, id_a, b, id_b, latency, one_way));
    }

    /// Where does sending on `link` at `node` arrive, after how long?
    /// `None` when nobody hears it (the reverse of a one-way RF path).
    fn far_end(&self, node: Node, link: LinkId) -> Option<(Node, LinkId, Millis)> {
        for &(a, la, b, lb, lat, one_way) in &self.links {
            if a == node && la == link {
                return Some((b, lb, lat));
            }
            if b == node && lb == link {
                return (!one_way).then_some((a, la, lat));
            }
        }
        unreachable!()
    }

    fn client_link(&self, client: usize) -> (Node, LinkId) {
        self.links
            .iter()
            .find_map(|&(a, la, b, lb, _, _)| {
                if a == Node::Client(client) {
                    Some((a, la))
                } else if b == Node::Client(client) {
                    Some((b, lb))
                } else {
                    None
                }
            })
            .expect("client has a link")
    }

    /// Inject frames from clients; run until quiet, `until`, or `cap` forwards.
    fn run(&mut self, injections: &[(Millis, usize, Frame)], until: Millis, cap: u64) -> u64 {
        let mut q: BinaryHeap<Arrival> = BinaryHeap::new();
        let mut frames: Vec<(Frame, Vec<u8>)> = vec![];
        let mut seq = 0;
        for (t, client, f) in injections {
            let wire = wrap(
                &f.encode(PayloadEncoding::Never).unwrap(),
                &EscapeSet::legacy(),
            )
            .unwrap();
            frames.push((f.clone(), wire));
            let (cl_node, cl_link) = self.client_link(*client);
            let (to, to_link, lat) = self.far_end(cl_node, cl_link).unwrap();
            let Node::Hub(h) = to else {
                panic!("client must connect to a hub")
            };
            q.push(Reverse((t + lat, seq, h, to_link, frames.len() - 1)));
            seq += 1;
        }
        let mut forwards = 0;
        while let Some(Reverse((t, _, h, link, fi))) = q.pop() {
            if t > until || forwards >= cap {
                break;
            }
            let (f, wire) = frames[fi].clone();
            for (out_link, _) in self.hubs[h].on_frame(t, link, &f, &wire) {
                forwards += 1;
                let Some((to, to_link, lat)) = self.far_end(Node::Hub(h), out_link) else {
                    continue;
                };
                match to {
                    Node::Client(c) => self.client_rx[c] += 1,
                    Node::Hub(h2) => {
                        q.push(Reverse((t + lat, seq, h2, to_link, fi)));
                        seq += 1;
                    }
                }
            }
        }
        forwards
    }
}

fn chat(src: &str, text: &str) -> Frame {
    Frame::new(0, 1, 0, src, "CQCQCQ", text.as_bytes().to_vec())
}

/// Three ratflectors linked to each other (a common "mesh for redundancy").
fn triangle(cfg: &Config) -> Net {
    let mut n = Net::new(3, 2, cfg);
    n.connect(Node::Client(0), Node::Hub(0), 20);
    n.connect(Node::Client(1), Node::Hub(2), 20);
    n.connect(Node::Hub(0), Node::Hub(1), 40);
    n.connect(Node::Hub(1), Node::Hub(2), 40);
    n.connect(Node::Hub(0), Node::Hub(2), 40);
    n
}

#[test]
fn legacy_triangle_amplifies_without_bound() {
    let mut n = triangle(&Config::legacy());
    let fwd = n.run(&[(0, 0, chat("W4DRS", "net check-in"))], 60_000, 100_000);
    println!(
        "legacy triangle, 60 s: {fwd} forwards; client 1 received {} copies of one message",
        n.client_rx[1]
    );
    // Still circulating when the simulation ends.
    assert!(fwd > 1_000);
    assert!(n.client_rx[1] > 100);
}

#[test]
fn aether_triangle_delivers_once_and_stops() {
    let mut n = triangle(&Config::aether());
    let fwd = n.run(&[(0, 0, chat("W4DRS", "net check-in"))], 60_000, 100_000);
    println!(
        "aether triangle: {fwd} forwards, client 1 received {} copies",
        n.client_rx[1]
    );
    assert_eq!(n.client_rx[1], 1);
    assert!(fwd <= 6, "bounded: each hub forwards once, got {fwd}");
}

#[test]
fn same_link_repeats_are_not_suppressed() {
    // "ok" twice from the same client, 1 s apart: legacy frames carry no id,
    // so both are legitimate and must both arrive.
    let mut n = triangle(&Config::aether());
    n.run(
        &[(0, 0, chat("W4DRS", "ok")), (1_000, 0, chat("W4DRS", "ok"))],
        60_000,
        100_000,
    );
    assert_eq!(n.client_rx[1], 2);
}

/// Two RF bridges, both linked to one Internet ratflector. Bridge B hears
/// bridge A on air but A does not hear B (asymmetric RF). That makes a single
/// cycle: ratflector -> A -> (RF) -> B -> ratflector -> A -> ...
/// With symmetric hearing the two directions collide inside the window and
/// cancel out, so the asymmetric case is the dangerous one.
fn bridges(cfg: &Config, air_ms: Millis) -> Net {
    let mut n = Net::new(3, 2, cfg);
    n.connect(Node::Client(0), Node::Hub(2), 30); // Internet user on the ratflector
    n.connect(Node::Client(1), Node::Hub(0), 30); // listener on bridge A's side
    n.connect(Node::Hub(0), Node::Hub(2), 50); // bridge A <-> ratflector
    n.connect(Node::Hub(1), Node::Hub(2), 50); // bridge B <-> ratflector
    n.connect_one_way(Node::Hub(0), Node::Hub(1), air_ms); // B hears A on RF
    n
}

#[test]
fn legacy_asymmetric_rf_bridge_loop_circulates() {
    let mut n = bridges(&Config::legacy(), 2_000);
    let fwd = n.run(&[(0, 0, chat("K4EOC", "sitrep"))], 120_000, 100_000);
    println!(
        "legacy bridges, 2 s air: {fwd} forwards, listener got {} copies",
        n.client_rx[1]
    );
    assert!(n.client_rx[1] > 20);
}

#[test]
fn rf_bridge_loop_suppressed_when_loop_is_faster_than_window() {
    let mut n = bridges(&Config::aether(), 2_000);
    let fwd = n.run(&[(0, 0, chat("K4EOC", "sitrep"))], 120_000, 100_000);
    println!(
        "aether bridges, 2 s air, 3 s window: {fwd} forwards, listener got {}",
        n.client_rx[1]
    );
    assert_eq!(n.client_rx[1], 1);
    assert!(fwd < 20);
}

#[test]
fn rf_bridge_loop_slower_than_window_needs_a_longer_window() {
    // A 5 s RF hop makes the loop slower than the 3 s window: copies return
    // after the window and circulate. Bridge (RF) links therefore need a
    // window longer than the worst-case loop, or Aether peer envelopes with
    // hop limits (spec 04 §5.3).
    let mut short = bridges(&Config::aether(), 5_000);
    let fwd_short = short.run(&[(0, 0, chat("K4EOC", "sitrep"))], 120_000, 100_000);
    let mut long = bridges(
        &Config {
            dedup_window: 30_000,
            ..Config::aether()
        },
        5_000,
    );
    let fwd_long = long.run(&[(0, 0, chat("K4EOC", "sitrep"))], 120_000, 100_000);
    println!(
        "bridges, 5 s air: 3 s window -> {fwd_short} forwards ({} copies); 30 s window -> {fwd_long} ({} copies)",
        short.client_rx[1], long.client_rx[1]
    );
    assert!(
        short.client_rx[1] > 10,
        "expected circulation with a too-short window"
    );
    assert_eq!(long.client_rx[1], 1);
}

#[test]
fn warmup_frames_are_never_forwarded() {
    let mut n = triangle(&Config::aether());
    let warm = Frame::new(0, 0, 254, "!", "!", vec![1; 16]);
    assert_eq!(n.run(&[(0, 0, warm)], 10_000, 100), 0);
}

#[test]
fn unicast_follows_learned_route() {
    let mut h = Hub::new(Config::aether());
    for l in [1, 2, 3] {
        h.add_link(l);
    }
    let from_b = Frame::new(0, 1, 0, "K7ABC", "CQCQCQ", b"hi".to_vec());
    h.on_frame(0, 2, &from_b, b"w1");
    let to_b = Frame::new(0, 1, 0, "W4DRS", "K7ABC", b"hello".to_vec());
    let out = h.on_frame(10, 1, &to_b, b"w2");
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].0, 2);
    // after the route ages out (600 s) it floods again
    let out = h.on_frame(700_000, 1, &to_b, b"w3");
    assert_eq!(out.len(), 2);
}

#[test]
fn legacy_echoes_on_the_arrival_link_aether_does_not() {
    for (cfg, expect) in [(Config::legacy(), 1), (Config::aether(), 0)] {
        let mut h = Hub::new(cfg);
        h.add_link(1);
        h.add_link(2);
        let a = Frame::new(0, 1, 0, "K7ABC", "CQCQCQ", b"hi".to_vec());
        h.on_frame(0, 1, &a, b"x1");
        // W4DRS on the same RF link sends to K7ABC (last heard on link 1).
        let b = Frame::new(0, 1, 0, "W4DRS", "K7ABC", b"yo".to_vec());
        let out = h.on_frame(10, 1, &b, b"x2");
        assert_eq!(out.len(), expect);
    }
}
