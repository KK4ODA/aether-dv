//! `aetherdv-rfd`: standalone ratflector server.
//!
//! ```text
//! aetherdv-rfd [--listen ADDR] [--users FILE] [--dedup-ms N] [--legacy]
//! ```
//! Defaults: listen `0.0.0.0:9000`, open (no auth), Aether forwarding rules.
//! `--users` takes a legacy `users.txt` (`user password` per line).
//! `--legacy` reproduces d-rats_repeater forwarding exactly (no duplicate
//! suppression, same-link echo).

use std::time::Duration;

use aether_rfd::server::{parse_users, Server, ServerConfig};
use aether_rfd::Config;

fn usage() -> ! {
    eprintln!("usage: aetherdv-rfd [--listen ADDR] [--users FILE] [--dedup-ms N] [--legacy]");
    std::process::exit(2)
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().with_target(false).init();
    let mut listen = "0.0.0.0:9000".to_string();
    let mut cfg = ServerConfig::default();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--listen" => listen = args.next().unwrap_or_else(|| usage()),
            "--users" => {
                let path = args.next().unwrap_or_else(|| usage());
                let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
                    eprintln!("cannot read {path}: {e}");
                    std::process::exit(1)
                });
                cfg.users = parse_users(&text);
            }
            "--dedup-ms" => {
                cfg.hub.dedup_window = args
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or_else(|| usage());
            }
            "--legacy" => cfg.hub = Config::legacy(),
            "-h" | "--help" => usage(),
            _ => usage(),
        }
    }
    let auth = if cfg.users.is_empty() {
        "open".to_string()
    } else {
        format!("{} users", cfg.users.len())
    };
    let server = match Server::start(&listen, cfg).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot listen on {listen}: {e}");
            std::process::exit(1)
        }
    };
    tracing::info!("aetherdv-rfd listening on {} ({auth})", server.local_addr);
    let counters = server.counters.clone();
    let stats = tokio::spawn(async move {
        let mut t = tokio::time::interval(Duration::from_secs(60));
        loop {
            t.tick().await;
            use std::sync::atomic::Ordering::Relaxed;
            tracing::info!(
                clients = counters.clients_total.load(Relaxed),
                frames_in = counters.frames_in.load(Relaxed),
                bad_frames = counters.bad_frames.load(Relaxed),
                queue_drops = counters.queue_drops.load(Relaxed),
                auth_failures = counters.auth_failures.load(Relaxed),
                "stats"
            );
        }
    });
    shutdown_signal().await;
    tracing::info!("shutting down");
    stats.abort();
    server.shutdown().await;
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("signal handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = term.recv() => {},
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}
