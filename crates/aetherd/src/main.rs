//! `aetherd --config aether.toml`
//!
//! Starts the engine, its ports and the local API, writes the API token to
//! the token file (mode 0600 on Unix) and runs until Ctrl-C / SIGTERM.

use std::path::PathBuf;

use aetherd::config::Config;

fn usage() -> ! {
    eprintln!("usage: aetherd --config FILE");
    std::process::exit(2)
}

fn write_token(path: &PathBuf, token: &str) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(token.as_bytes())
    }
    #[cfg(not(unix))]
    std::fs::write(path, token)
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().with_target(false).init();
    let mut args = std::env::args().skip(1);
    let mut cfg_path = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--config" | "-c" => cfg_path = args.next(),
            _ => usage(),
        }
    }
    let cfg_path = cfg_path.unwrap_or_else(|| usage());
    let text = std::fs::read_to_string(&cfg_path).unwrap_or_else(|e| {
        eprintln!("cannot read {cfg_path}: {e}");
        std::process::exit(1)
    });
    let cfg = Config::parse(&text).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1)
    });
    let token_path = cfg.token_file.clone().unwrap_or_else(|| match &cfg.db {
        Some(db) => db.with_file_name("aetherd.token"),
        None => std::env::temp_dir().join("aetherd.token"),
    });
    let engine = aetherd::engine::start(&cfg).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1)
    });
    let token = aetherd::new_token();
    if let Err(e) = write_token(&token_path, &token) {
        eprintln!("cannot write token file {}: {e}", token_path.display());
        std::process::exit(1)
    }
    let addr = aetherd::api::serve(&cfg.api_listen, token, engine)
        .await
        .unwrap_or_else(|e| {
            eprintln!("cannot listen on {}: {e}", cfg.api_listen);
            std::process::exit(1)
        });
    tracing::info!(
        "aetherd {} for {}: API ws://{addr} (token in {})",
        aetherd::station::VERSION,
        cfg.callsign,
        token_path.display()
    );
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutting down");
}
