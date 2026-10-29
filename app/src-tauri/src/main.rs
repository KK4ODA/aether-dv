// Aether DV desktop shell. The UI talks to the engine (aetherd) over its
// local API; the shell never owns serial ports itself (ADR-0001). Its
// native jobs: find (or start) the engine, and update the app.
//
// Updates: checked on request (the UI also checks once at start-up),
// installed only when the operator clicks Install. Every package is
// verified against the public key built into the app, and the signature
// must carry the version the update feed announces (no downgrades).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::{Emitter, Manager};
use tauri_plugin_opener::OpenerExt;
use tauri_plugin_updater::{Update, UpdaterExt};

#[derive(serde::Serialize)]
struct Endpoint {
    api: String,
    token: String,
}

#[derive(Default)]
struct Shell {
    /// The engine this app started, if any (stopped when the app exits).
    engine: Mutex<Option<Child>>,
    /// The update found by the last check.
    pending: Mutex<Option<Update>>,
}

const DEFAULT_API: &str = "127.0.0.1:7373";

fn loopback_only(api: &str) -> Result<(), String> {
    let host = api.rsplit_once(':').map(|(h, _)| h).unwrap_or("");
    if matches!(host, "127.0.0.1" | "localhost" | "[::1]") {
        Ok(())
    } else {
        Err("The engine address must be on this computer (127.0.0.1, localhost or [::1]).".into())
    }
}

fn read_token(path: &std::path::Path) -> Option<String> {
    if path.extension().and_then(|e| e.to_str()) != Some("token") {
        return None;
    }
    let m = std::fs::metadata(path).ok()?;
    if m.len() > 256 {
        return None;
    }
    std::fs::read_to_string(path)
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

/// Token file used for an engine this app starts.
fn app_token_file(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("no app data directory: {e}"))?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok(dir.join("aetherd.token"))
}

/// The engine's API address (loopback only) and access token. The token is
/// read from, in order: the engine this app started, the file the user
/// chose, `AETHER_TOKEN_FILE`, and `aetherd.token` in the temp directory
/// (aetherd's default without a database). Only small `*.token` files.
#[tauri::command]
fn engine_endpoint(
    app: tauri::AppHandle,
    state: tauri::State<'_, Shell>,
    token_file: Option<String>,
    api: Option<String>,
) -> Result<Endpoint, String> {
    let api = api
        .filter(|a| !a.trim().is_empty())
        .or_else(|| std::env::var("AETHER_API").ok())
        .unwrap_or_else(|| DEFAULT_API.into());
    loopback_only(&api)?;
    let ours = state.engine.lock().map(|e| e.is_some()).unwrap_or(false);
    let candidates: Vec<PathBuf> = ours
        .then(|| app_token_file(&app).ok())
        .flatten()
        .into_iter()
        .chain(
            token_file
                .filter(|t| !t.trim().is_empty())
                .map(|t| PathBuf::from(t.trim())),
        )
        .chain(std::env::var_os("AETHER_TOKEN_FILE").map(PathBuf::from))
        .chain([std::env::temp_dir().join("aetherd.token")])
        .collect();
    for path in &candidates {
        if let Some(token) = read_token(path) {
            return Ok(Endpoint { api, token });
        }
    }
    Err(format!(
        "No engine token found (looked at {}). Start the engine below, or enter the path of a running engine's aetherd.token file.",
        candidates.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")
    ))
}

/// The engine binary shipped inside this app, if present.
fn bundled_engine() -> Option<PathBuf> {
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let p = dir.join(format!("aetherd{}", std::env::consts::EXE_SUFFIX));
    p.is_file().then_some(p)
}

/// Start the bundled engine with the operator's configuration file. Its
/// token goes to the app data directory; its console output to
/// `aetherd-console.log` there. Relative paths in the configuration are
/// relative to the configuration file.
#[tauri::command]
fn engine_start(
    app: tauri::AppHandle,
    state: tauri::State<'_, Shell>,
    config: String,
    api: Option<String>,
) -> Result<Endpoint, String> {
    let api = api
        .filter(|a| !a.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_API.into());
    loopback_only(&api)?;
    let exe = bundled_engine().ok_or("This installation has no bundled engine (aetherd).")?;
    let config = PathBuf::from(config.trim());
    if !config.is_file() {
        return Err(format!("{} is not a file", config.display()));
    }
    let mut guard = state.engine.lock().map_err(|_| "engine state poisoned")?;
    if let Some(child) = guard.as_mut() {
        if child.try_wait().ok().flatten().is_none() {
            return Err("The engine started by this app is already running.".into());
        }
    }
    let token = app_token_file(&app)?;
    let _ = std::fs::remove_file(&token);
    let log_path = token.with_file_name("aetherd-console.log");
    let log =
        std::fs::File::create(&log_path).map_err(|e| format!("{}: {e}", log_path.display()))?;
    let mut cmd = Command::new(&exe);
    cmd.arg("--config")
        .arg(&config)
        .arg("--token-file")
        .arg(&token)
        .arg("--api-listen")
        .arg(&api)
        .stdin(Stdio::null())
        .stdout(log.try_clone().map_err(|e| e.to_string())?)
        .stderr(log);
    if let Some(dir) = config.parent().filter(|d| !d.as_os_str().is_empty()) {
        cmd.current_dir(dir);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", exe.display()))?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(t) = read_token(&token) {
            *guard = Some(child);
            return Ok(Endpoint { api, token: t });
        }
        if let Ok(Some(status)) = child.try_wait() {
            let out = std::fs::read_to_string(&log_path).unwrap_or_default();
            let tail: Vec<&str> = out.lines().rev().take(5).collect();
            return Err(format!(
                "The engine stopped ({status}): {}",
                tail.into_iter().rev().collect::<Vec<_>>().join(" / ")
            ));
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            return Err(format!(
                "The engine did not start within 10 s; see {}",
                log_path.display()
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn stop_engine(state: &Shell) {
    if let Ok(mut g) = state.engine.lock() {
        if let Some(mut c) = g.take() {
            // The store is WAL + synchronous=FULL: a hard stop loses nothing
            // that was acknowledged (ADR-0007).
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// Open an https link (radio setup guides) in the system browser, so the
/// app window itself never navigates away.
#[tauri::command]
fn open_link(app: tauri::AppHandle, url: String) -> Result<(), String> {
    let u = url.trim();
    let rest = u
        .strip_prefix("https://")
        .ok_or("Only https links can be opened.")?;
    if rest.is_empty() || u.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("Not a valid link.".into());
    }
    app.opener()
        .open_url(u, None::<&str>)
        .map_err(|e| format!("Could not open the link: {e}"))
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct AppInfo {
    version: String,
    /// This build can verify updates (it carries the release public key).
    updates_configured: bool,
    bundled_engine: bool,
    engine_started_here: bool,
}

fn updates_configured(app: &tauri::AppHandle) -> bool {
    app.config()
        .plugins
        .0
        .get("updater")
        .and_then(|u| u.get("pubkey"))
        .and_then(|k| k.as_str())
        .is_some_and(|k| !k.trim().is_empty())
}

#[tauri::command]
fn app_info(app: tauri::AppHandle, state: tauri::State<'_, Shell>) -> AppInfo {
    AppInfo {
        version: app.package_info().version.to_string(),
        updates_configured: updates_configured(&app),
        bundled_engine: bundled_engine().is_some(),
        engine_started_here: state.engine.lock().map(|e| e.is_some()).unwrap_or(false),
    }
}

#[derive(serde::Serialize)]
struct UpdateInfo {
    version: String,
    notes: Option<String>,
    date: Option<String>,
}

#[tauri::command]
async fn update_check(
    app: tauri::AppHandle,
    state: tauri::State<'_, Shell>,
) -> Result<Option<UpdateInfo>, String> {
    if !updates_configured(&app) {
        return Err(
            "This build was made without the release key, so it cannot check for updates.".into(),
        );
    }
    let update = app
        .updater()
        .map_err(|e| e.to_string())?
        .check()
        .await
        .map_err(|e| format!("Could not check for updates: {e}"))?;
    let info = update.as_ref().map(|u| UpdateInfo {
        version: u.version.clone(),
        notes: u.body.clone(),
        date: u.date.map(|d| d.to_string()),
    });
    *state.pending.lock().map_err(|_| "update state poisoned")? = update;
    Ok(info)
}

#[derive(Clone, serde::Serialize)]
struct Progress {
    downloaded: u64,
    total: Option<u64>,
}

/// Download, verify and install the update found by `update_check`, then
/// restart. An engine started by this app is stopped first (an installer
/// cannot replace a running engine binary on Windows).
#[tauri::command]
async fn update_install(
    app: tauri::AppHandle,
    state: tauri::State<'_, Shell>,
) -> Result<(), String> {
    let update = state
        .pending
        .lock()
        .map_err(|_| "update state poisoned")?
        .take()
        .ok_or("No update to install; check for updates first.")?;
    let mut downloaded = 0u64;
    let bytes = update
        .download(
            |chunk, total| {
                downloaded += chunk as u64;
                let _ = app.emit("update-progress", Progress { downloaded, total });
            },
            || {},
        )
        .await
        .map_err(|e| format!("Download or signature check failed: {e}"))?;
    stop_engine(&state);
    update.install(bytes).map_err(|e| {
        format!(
            "Install failed: {e}. You can download the new version from the releases page instead."
        )
    })?;
    app.restart()
}

/// Unattended update run for the end-to-end test: records the running
/// version, and if it is `AETHER_SELFTEST_FROM`, checks, installs and
/// restarts; otherwise exits.
#[cfg(feature = "selftest")]
fn selftest(app: tauri::AppHandle) {
    use std::io::Write;
    let out = std::env::var("AETHER_SELFTEST_OUT").unwrap_or_else(|_| "selftest.log".into());
    let log = move |m: String| {
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&out)
        {
            let _ = writeln!(f, "{m}");
        }
    };
    let version = app.package_info().version.to_string();
    log(format!("running {version}"));
    if std::env::var("AETHER_SELFTEST_FROM").ok().as_deref() != Some(version.as_str()) {
        app.exit(0);
        return;
    }
    tauri::async_runtime::spawn(async move {
        let state = app.state::<Shell>();
        match update_check(app.clone(), state.clone()).await {
            Ok(Some(u)) => log(format!("found {}", u.version)),
            Ok(None) => {
                log("no update".into());
                app.exit(1);
                return;
            }
            Err(e) => {
                log(format!("check failed: {e}"));
                app.exit(1);
                return;
            }
        }
        if let Err(e) = update_install(app.clone(), state).await {
            log(format!("install failed: {e}"));
            app.exit(1);
        }
    });
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_opener::init())
        .manage(Shell::default())
        .setup(|_app| {
            #[cfg(feature = "selftest")]
            selftest(_app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            engine_endpoint,
            engine_start,
            open_link,
            app_info,
            update_check,
            update_install
        ])
        .build(tauri::generate_context!())
        .expect("error while building Aether DV")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                stop_engine(&app.state::<Shell>());
            }
        });
}
