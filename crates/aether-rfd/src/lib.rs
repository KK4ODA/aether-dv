//! `aether-rfd` core: legacy-compatible ratflector forwarding (sans-I/O).
//!
//! Legacy behaviour (`d-rats_repeater.py` `__repeat`, @d0e7a39 lines 205-246):
//! drop warm-up frames (dst `!`); learn `src -> link`; unicast to the link a
//! destination was last heard on within 600 s, otherwise flood to every link
//! except the one it came from.
//!
//! Additions (spec `docs/04-ratflector-analysis.md` §5.2):
//! * the original wire bytes are forwarded unchanged (no decode/re-encode);
//! * cross-link duplicate suppression: identical wire bytes arriving on a
//!   *different* link within a short window are dropped (loop/multipath
//!   signature); repeats on the *same* link always pass, because legacy
//!   frames carry no id and a user may legitimately send "ok" twice.

use std::collections::{HashMap, VecDeque};

use aether_ddt2::Frame;

pub mod server;

pub type Millis = u64;
pub type LinkId = u32;

#[derive(Debug, Clone)]
pub struct Config {
    /// Legacy `__call_timeout`.
    pub route_ttl: Millis,
    /// Cross-link duplicate window; 0 disables suppression (legacy behaviour).
    pub dedup_window: Millis,
    /// Legacy sends a unicast frame back on the link it arrived on when the
    /// destination was last heard there. On an RF bridge port that re-transmits
    /// RF-to-RF traffic on air. Off for Aether: it doubles channel use.
    pub echo_same_link: bool,
}

impl Config {
    pub fn legacy() -> Self {
        Config {
            route_ttl: 600_000,
            dedup_window: 0,
            echo_same_link: true,
        }
    }
    pub fn aether() -> Self {
        Config {
            route_ttl: 600_000,
            dedup_window: 3_000,
            echo_same_link: false,
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct Stats {
    pub frames_in: u64,
    pub frames_out: u64,
    pub unicast: u64,
    pub flooded: u64,
    pub dropped_warmup: u64,
    pub dropped_cross_link_dup: u64,
}

fn fnv1a(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
        (h ^ b as u64).wrapping_mul(0x0100_0000_01b3)
    })
}

#[derive(Debug)]
pub struct Hub {
    cfg: Config,
    links: Vec<LinkId>,
    routes: HashMap<Vec<u8>, (LinkId, Millis)>,
    seen: HashMap<u64, (LinkId, Millis)>,
    seen_order: VecDeque<(u64, Millis)>,
    pub stats: Stats,
}

impl Hub {
    pub fn new(cfg: Config) -> Self {
        Hub {
            cfg,
            links: Vec::new(),
            routes: HashMap::new(),
            seen: HashMap::new(),
            seen_order: VecDeque::new(),
            stats: Stats::default(),
        }
    }

    pub fn add_link(&mut self, id: LinkId) {
        if !self.links.contains(&id) {
            self.links.push(id);
        }
    }

    pub fn remove_link(&mut self, id: LinkId) {
        self.links.retain(|&l| l != id);
        self.routes.retain(|_, (l, _)| *l != id);
    }

    fn expire_seen(&mut self, now: Millis) {
        while let Some(&(h, t)) = self.seen_order.front() {
            if now.saturating_sub(t) <= self.cfg.dedup_window {
                break;
            }
            self.seen_order.pop_front();
            if self.seen.get(&h).is_some_and(|&(_, st)| st == t) {
                self.seen.remove(&h);
            }
        }
    }

    /// A complete, valid frame arrived on `from`. `wire` is its exact
    /// on-the-wire envelope. Returns `(link, bytes)` pairs to send.
    pub fn on_frame(
        &mut self,
        now: Millis,
        from: LinkId,
        frame: &Frame,
        wire: &[u8],
    ) -> Vec<(LinkId, Vec<u8>)> {
        self.stats.frames_in += 1;
        if frame.dst == b"!" {
            self.stats.dropped_warmup += 1;
            return vec![];
        }
        if self.cfg.dedup_window > 0 {
            self.expire_seen(now);
            let h = fnv1a(wire);
            match self.seen.get(&h) {
                Some(&(first, _)) if first != from => {
                    self.stats.dropped_cross_link_dup += 1;
                    return vec![];
                }
                _ => {
                    self.seen.insert(h, (from, now));
                    self.seen_order.push_back((h, now));
                }
            }
        }
        if frame.src != b"CQCQCQ" {
            self.routes.insert(frame.src.clone(), (from, now));
        }
        if let Some(&(link, at)) = self.routes.get(&frame.dst) {
            if now.saturating_sub(at) < self.cfg.route_ttl && self.links.contains(&link) {
                if link == from && !self.cfg.echo_same_link {
                    return vec![];
                }
                self.stats.unicast += 1;
                self.stats.frames_out += 1;
                return vec![(link, wire.to_vec())];
            }
        }
        self.stats.flooded += 1;
        let out: Vec<(LinkId, Vec<u8>)> = self
            .links
            .iter()
            .filter(|&&l| l != from)
            .map(|&l| (l, wire.to_vec()))
            .collect();
        self.stats.frames_out += out.len() as u64;
        out
    }
}
