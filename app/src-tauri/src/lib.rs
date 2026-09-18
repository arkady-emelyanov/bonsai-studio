//! Bonsai Studio: a start/stop button and an options dialog in front of
//! llama-server, which does the actual serving.

mod config;
mod models;
mod server;
mod vram;

use config::Settings;
use server::Supervisor;
use std::sync::Mutex;
use tauri::Manager;

struct AppState {
    settings: Mutex<Settings>,
    supervisor: Supervisor,
    downloader: models::Downloader,
}

// --- settings ----------------------------------------------------------------

#[tauri::command]
fn get_settings(state: tauri::State<AppState>) -> Settings {
    state.settings.lock().unwrap().clone()
}

#[tauri::command]
fn save_settings(mut next: Settings, state: tauri::State<AppState>) -> Result<Settings, String> {
    next.apply_profile();
    config::save(&next)?;
    *state.settings.lock().unwrap() = next.clone();
    Ok(next)
}

#[tauri::command]
fn estimate_vram(state: tauri::State<AppState>) -> vram::VramEstimate {
    let s = state.settings.lock().unwrap();
    vram::estimate(s.ctx, &s.kv_type, s.ngl, s.mmproj_cpu, s.kv_offload)
}

/// A key worth pasting: 32 hex characters from the OS random source.
#[tauri::command]
fn generate_api_key() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes)
        .map_err(|e| format!("Could not read random bytes: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

// --- models ------------------------------------------------------------------

#[tauri::command]
fn list_models(state: tauri::State<AppState>) -> Vec<models::VariantStatus> {
    models::survey(&state.settings.lock().unwrap())
}

/// Point the settings at an installed variant.
#[tauri::command]
fn select_model(id: String, state: tauri::State<AppState>) -> Result<Settings, String> {
    let variant = models::variant_by_id(&id).ok_or("Unknown model variant.")?;
    let mut settings = state.settings.lock().unwrap();
    let (_, files) = models::locate(&settings, variant)
        .ok_or("That model is not downloaded yet.")?;
    settings.model = Some(files);
    config::save(&settings)?;
    Ok(settings.clone())
}

/// `force` discards what is on disk and fetches again; otherwise complete files
/// are skipped and partial ones resume.
#[tauri::command]
fn download_model(
    id: String,
    force: bool,
    state: tauri::State<AppState>,
) -> Result<(), String> {
    let variant = models::variant_by_id(&id).ok_or("Unknown model variant.")?;

    // Deleting weights out from under a running server leaves it holding an
    // unlinked inode and the next start loading a half-written file.
    if force && !matches!(state.supervisor.status().state, server::State::Stopped) {
        return Err("Stop the server before re-downloading its weights.".into());
    }

    // Re-download has to replace the copy actually in use, and a retry after a
    // purge has to land in the same place as the partial file it is resuming.
    let settings = state.settings.lock().unwrap().clone();
    let dir = models::download_dir(&settings, variant);
    state.downloader.start(variant, dir, force)
}

#[tauri::command]
fn download_progress(state: tauri::State<AppState>) -> models::Progress {
    state.downloader.progress()
}

#[tauri::command]
fn cancel_download(state: tauri::State<AppState>) {
    state.downloader.cancel();
}

// --- server ------------------------------------------------------------------

#[tauri::command]
fn start_server(app: tauri::AppHandle, state: tauri::State<AppState>) -> Result<(), String> {
    let settings = state.settings.lock().unwrap().clone();
    let resource_dir = app.path().resource_dir().ok();
    let binary = server::resolve_binary(&settings, resource_dir)?;
    state.supervisor.start(&settings, binary)
}

#[tauri::command]
fn stop_server(state: tauri::State<AppState>) -> Result<(), String> {
    state.supervisor.stop()
}

#[tauri::command]
fn server_status(state: tauri::State<AppState>) -> server::ServerStatus {
    state.supervisor.status()
}

/// Open llama-server's own chat UI in the user's browser.
///
/// The server already serves a chat page with image upload, a reasoning-effort
/// picker, history and an MCP client. Handing it to the browser -- rather than
/// hosting it in a webview here -- keeps this app to what it is good at, and
/// means no chat window to lose behind the settings window.
#[tauri::command]
fn open_chat(state: tauri::State<AppState>) -> Result<(), String> {
    let url = state.supervisor.status().chat_url.ok_or("Start the server first.")?;
    let result = if cfg!(target_os = "windows") {
        // `start` is a cmd builtin, not an executable; the empty argument is the
        // window title, which start would otherwise take from the URL.
        std::process::Command::new("cmd").args(["/C", "start", "", &url]).spawn()
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(&url).spawn()
    } else {
        std::process::Command::new("xdg-open").arg(&url).spawn()
    };
    result.map(|_| ()).map_err(|e| format!("Could not open {url}: {e}"))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let mut settings = config::load();
    settings.apply_profile();

    // First run with nothing configured: adopt whichever variant is already on
    // disk rather than presenting an empty dialog.
    if settings.model.is_none() {
        for variant in models::VARIANTS {
            if let Some((_, files)) = models::locate(&settings, variant) {
                settings.model = Some(files);
                break;
            }
        }
    }

    let state = AppState {
        settings: Mutex::new(settings),
        supervisor: Supervisor::new(),
        downloader: models::Downloader::default(),
    };

    tauri::Builder::default()
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            get_settings,
            save_settings,
            estimate_vram,
            generate_api_key,
            list_models,
            select_model,
            download_model,
            download_progress,
            cancel_download,
            start_server,
            stop_server,
            server_status,
            open_chat,
        ])
        .on_window_event(|window, event| {
            // Closing the settings window quits the app, which must take the
            // server with it -- PDEATHSIG covers a crash, this covers the
            // ordinary case.
            if let tauri::WindowEvent::Destroyed = event {
                if window.label() == "main" {
                    if let Some(state) = window.app_handle().try_state::<AppState>() {
                        let _ = state.supervisor.stop();
                    }
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
