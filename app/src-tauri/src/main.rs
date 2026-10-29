// Aether DV desktop shell. The UI talks to the separately running engine
// (aetherd) over its local API; the shell never owns serial ports itself
// (ADR-0001). Its one native job is finding the engine: address and token.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;

#[derive(serde::Serialize)]
struct Endpoint {
    api: String,
    token: String,
}

/// The engine's API address (loopback only) and access token. The token is
/// read from, in order: the file the user chose, `AETHER_TOKEN_FILE`, and
/// `aetherd.token` in the temp directory (aetherd's default without a
/// database). Only small `*.token` files are read.
#[tauri::command]
fn engine_endpoint(token_file: Option<String>, api: Option<String>) -> Result<Endpoint, String> {
    let api = api
        .filter(|a| !a.trim().is_empty())
        .or_else(|| std::env::var("AETHER_API").ok())
        .unwrap_or_else(|| "127.0.0.1:7373".into());
    let host = api.rsplit_once(':').map(|(h, _)| h).unwrap_or("");
    if !matches!(host, "127.0.0.1" | "localhost" | "[::1]") {
        return Err(
            "The engine address must be on this computer (127.0.0.1, localhost or [::1]).".into(),
        );
    }
    let candidates: Vec<PathBuf> = token_file
        .filter(|t| !t.trim().is_empty())
        .map(|t| PathBuf::from(t.trim()))
        .into_iter()
        .chain(std::env::var_os("AETHER_TOKEN_FILE").map(PathBuf::from))
        .chain([std::env::temp_dir().join("aetherd.token")])
        .collect();
    let mut tried = Vec::new();
    for path in candidates {
        if path.extension().and_then(|e| e.to_str()) != Some("token") {
            tried.push(format!("{} (not a .token file)", path.display()));
            continue;
        }
        match std::fs::metadata(&path) {
            Ok(m) if m.len() <= 256 => {
                if let Ok(t) = std::fs::read_to_string(&path) {
                    return Ok(Endpoint {
                        api,
                        token: t.trim().to_string(),
                    });
                }
            }
            _ => {}
        }
        tried.push(path.display().to_string());
    }
    Err(format!(
        "No engine token found (looked at {}). Start aetherd, then enter the path of its aetherd.token file (next to its database).",
        tried.join(", ")
    ))
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![engine_endpoint])
        .run(tauri::generate_context!())
        .expect("error while running Aether DV");
}
