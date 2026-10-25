//! RollVault desktop app: Tauri commands over the `rollvault-core` engine.

#![allow(clippy::result_large_err)]

use base64::Engine as _;
use rollvault_core::job::{
    Engine, EngineConfig, HistoryEntry, JobRequest, JobSnapshot, Preflight, VerifySnapshot,
};
use rollvault_core::preset::{self, RollMeta};
use rollvault_core::volumes::{self, VolumeInfo};
use rollvault_core::{darwin_io, tools};
use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};

struct AppState {
    engine: Arc<Engine>,
    data_dir: PathBuf,
    settings_path: PathBuf,
    settings: Mutex<Value>,
    allow_close: AtomicBool,
}

type Shared<'a> = State<'a, AppState>;

#[derive(Serialize)]
struct AppInfo {
    version: String,
    data_dir: String,
    ffmpeg: Option<String>,
    ffprobe: Option<String>,
    chrome: Option<String>,
    hostname: String,
    username: String,
}

#[derive(Serialize)]
struct Started {
    job_id: String,
    next_roll: String,
}

/// Run blocking work off the main thread. Tauri runs non-async commands on
/// the main (UI) thread, so every command here is async and anything that
/// touches a disk goes through this.
async fn off_main<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| e.to_string())
}

fn apply_language(settings: &Value) {
    let vi = settings.get("language").and_then(Value::as_str) == Some("vi");
    rollvault_core::l10n::set_vietnamese(vi);
}

fn debug_log(msg: &str) {
    if std::env::var_os("RV_DEBUG").is_some() {
        eprintln!("[rollvault] {msg}");
    }
}

#[tauri::command]
async fn app_info(state: Shared<'_>) -> Result<AppInfo, String> {
    debug_log("ui called app_info");
    let data_dir = state.data_dir.display().to_string();
    off_main(move || AppInfo {
        version: env!("CARGO_PKG_VERSION").into(),
        data_dir,
        ffmpeg: tools::ffmpeg().map(|p| p.display().to_string()),
        ffprobe: tools::ffprobe().map(|p| p.display().to_string()),
        chrome: tools::find_chromium().map(|p| p.display().to_string()),
        hostname: rollvault_core::ascmhl::hostname(),
        username: tools::username(),
    })
    .await
}

#[tauri::command]
async fn list_volumes(state: Shared<'_>) -> Result<Vec<VolumeInfo>, String> {
    let engine = state.engine.clone();
    let list = tauri::async_runtime::spawn_blocking(move || engine.list_volumes()).await.map_err(|e| e.to_string())?;
    debug_log(&format!(
        "ui called list_volumes: {}",
        list.iter().map(|v| format!("{} ({:?})", v.name, v.kind)).collect::<Vec<_>>().join(", ")
    ));
    Ok(list)
}

#[tauri::command]
async fn preflight(state: Shared<'_>, req: JobRequest) -> Result<Preflight, String> {
    let engine = state.engine.clone();
    tauri::async_runtime::spawn_blocking(move || engine.preflight(&req)).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn start_job(state: Shared<'_>, req: JobRequest) -> Result<Started, Preflight> {
    let engine = state.engine.clone();
    let roll = req.meta.roll_id.clone();
    let res = tauri::async_runtime::spawn_blocking(move || engine.start(req)).await;
    match res {
        Ok(Ok(job_id)) => Ok(Started { job_id, next_roll: preset::increment_roll(&roll) }),
        Ok(Err(pf)) => Err(pf),
        Err(e) => Err(Preflight { errors: vec![e.to_string()], ..Default::default() }),
    }
}

#[tauri::command]
async fn cancel_job(state: Shared<'_>, id: String) -> Result<bool, String> {
    Ok(state.engine.cancel(&id))
}

#[tauri::command]
async fn dismiss_job(state: Shared<'_>, id: String) -> Result<bool, String> {
    Ok(state.engine.dismiss(&id))
}

#[tauri::command]
async fn resume_job(state: Shared<'_>, id: String) -> Result<String, String> {
    let engine = state.engine.clone();
    tauri::async_runtime::spawn_blocking(move || engine.resume(&id)).await.map_err(|e| e.to_string())?
}

#[tauri::command]
async fn jobs(state: Shared<'_>) -> Result<Vec<JobSnapshot>, String> {
    let engine = state.engine.clone();
    off_main(move || engine.snapshots()).await
}

#[tauri::command]
async fn history(state: Shared<'_>) -> Result<Vec<HistoryEntry>, String> {
    Ok(state.engine.history())
}

#[tauri::command]
async fn eject_source(state: Shared<'_>, id: String) -> Result<(), String> {
    let engine = state.engine.clone();
    tauri::async_runtime::spawn_blocking(move || engine.eject_source(&id)).await.map_err(|e| e.to_string())?
}

#[tauri::command]
async fn eject_volume(state: Shared<'_>, mount: String) -> Result<(), String> {
    let engine = state.engine.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let r = volumes::eject(&mount);
        engine.volumes.forget();
        r
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn reveal(path: String) -> Result<(), String> {
    off_main(move || {
        let p = PathBuf::from(&path);
        let target = if p.exists() { p } else { p.parent().map(Path::to_path_buf).unwrap_or(p) };
        tools::reveal_in_finder(&target)
    })
    .await?
}

#[tauri::command]
async fn open_path(path: String) -> Result<(), String> {
    off_main(move || tools::open_path(Path::new(&path))).await?
}

#[tauri::command]
async fn load_settings(state: Shared<'_>) -> Result<Value, String> {
    debug_log("ui called load_settings");
    Ok(state.settings.lock().map(|s| s.clone()).unwrap_or(Value::Null))
}

#[tauri::command]
async fn save_settings(state: Shared<'_>, settings: Value) -> Result<(), String> {
    let data = serde_json::to_vec_pretty(&settings).map_err(|e| e.to_string())?;
    let path = state.settings_path.clone();
    off_main(move || -> Result<(), String> {
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, data).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
    })
    .await??;
    apply_language(&settings);
    let sounds = settings.get("sounds").and_then(Value::as_bool).unwrap_or(true);
    let notifications = settings.get("notifications").and_then(Value::as_bool).unwrap_or(true);
    state.engine.set_alerts(sounds, notifications);
    if let Ok(mut s) = state.settings.lock() {
        *s = settings;
    }
    Ok(())
}

#[tauri::command]
async fn read_logo(path: String) -> Result<String, String> {
    off_main(move || read_logo_blocking(path)).await?
}

fn read_logo_blocking(path: String) -> Result<String, String> {
    let p = PathBuf::from(&path);
    let meta = std::fs::metadata(&p).map_err(|e| e.to_string())?;
    if meta.len() > 2 * 1024 * 1024 {
        return Err(rollvault_core::l!("The logo must be smaller than 2 MB", "Logo phải nhỏ hơn 2 MB"));
    }
    let mime = match p.extension().and_then(|e| e.to_str()).map(str::to_lowercase).as_deref() {
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("svg") => "image/svg+xml",
        Some("webp") => "image/webp",
        _ => return Err(rollvault_core::l!("Only PNG, JPG, SVG or WebP are supported", "Chỉ hỗ trợ PNG, JPG, SVG hoặc WebP")),
    };
    let data = std::fs::read(&p).map_err(|e| e.to_string())?;
    Ok(format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(data)))
}

#[tauri::command]
async fn render_roll_path(template: String, meta: RollMeta) -> Result<String, String> {
    preset::render_template(&template, &meta).map(|p| p.display().to_string())
}

#[tauri::command]
async fn next_roll(roll: String) -> Result<String, String> {
    Ok(preset::increment_roll(&roll))
}

/// Export a PDF next to an existing HTML report (via a private temp copy,
/// so Chrome never touches removable volumes directly).
#[tauri::command]
async fn export_pdf(html: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let src = PathBuf::from(&html);
        let tmp = std::env::temp_dir().join(format!("rollvault-pdf-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
        let name = src.file_stem().and_then(|s| s.to_str()).unwrap_or("report").to_string();
        let tmp_html = tmp.join(format!("{name}.html"));
        let tmp_pdf = tmp.join(format!("{name}.pdf"));
        std::fs::copy(&src, &tmp_html).map_err(|e| e.to_string())?;
        tools::html_to_pdf(&tmp_html, &tmp_pdf)?;
        let dst = src.with_extension("pdf");
        std::fs::copy(&tmp_pdf, &dst).map_err(|e| e.to_string())?;
        let _ = std::fs::remove_dir_all(&tmp);
        Ok(dst.display().to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn start_verify(state: Shared<'_>, path: String, author: String) -> Result<String, String> {
    let engine = state.engine.clone();
    off_main(move || engine.start_verify(PathBuf::from(path), author)).await?
}

#[tauri::command]
async fn verify_tasks(state: Shared<'_>) -> Result<Vec<VerifySnapshot>, String> {
    Ok(state.engine.verify_snapshots())
}

#[tauri::command]
async fn cancel_verify(state: Shared<'_>, id: String) -> Result<(), String> {
    state.engine.cancel_verify(&id);
    Ok(())
}

#[tauri::command]
async fn dismiss_verify(state: Shared<'_>, id: String) -> Result<(), String> {
    state.engine.dismiss_verify(&id);
    Ok(())
}

#[tauri::command]
async fn any_active(state: Shared<'_>) -> Result<bool, String> {
    Ok(state.engine.any_active())
}

/// Quit, stopping running jobs cleanly first (they become resumable).
#[tauri::command]
async fn quit_app(app: AppHandle, state: Shared<'_>) -> Result<(), String> {
    state.allow_close.store(true, Ordering::Relaxed);
    state.engine.cancel_all();
    let engine = state.engine.clone();
    std::thread::spawn(move || {
        let start = Instant::now();
        while engine.any_active() && start.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(100));
        }
        app.exit(0);
    });
    Ok(())
}

fn mount_signature() -> String {
    let mut m: Vec<String> = darwin_io::mounted_filesystems()
        .into_iter()
        .filter(|s| s.mount_point.starts_with("/Volumes/"))
        .map(|s| format!("{}|{}", s.mount_point, s.device))
        .collect();
    m.sort();
    m.join(";")
}

/// Push job snapshots to the UI (only when something changed) and notice
/// drives being plugged or unplugged.
fn start_watchers(app: AppHandle, engine: Arc<Engine>) {
    std::thread::Builder::new()
        .name("rv-ui-ticker".into())
        .spawn(move || {
            let mut last_jobs = String::new();
            let mut last_verifies = String::new();
            let mut last_mounts = mount_signature();
            let mut tick: u64 = 0;
            loop {
                std::thread::sleep(Duration::from_millis(400));
                tick += 1;
                let snaps = engine.snapshots();
                if let Ok(json) = serde_json::to_string(&snaps)
                    && json != last_jobs
                {
                    let _ = app.emit("jobs", &snaps);
                    last_jobs = json;
                }
                let verifies = engine.verify_snapshots();
                if let Ok(json) = serde_json::to_string(&verifies)
                    && json != last_verifies
                {
                    let _ = app.emit("verifies", &verifies);
                    last_verifies = json;
                }
                if tick.is_multiple_of(4) {
                    let sig = mount_signature();
                    if sig != last_mounts {
                        last_mounts = sig;
                        engine.volumes.forget();
                        let _ = app.emit("volumes-changed", ());
                    }
                }
            }
        })
        .expect("spawn ui ticker");
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let data_dir = app
                .path()
                .app_data_dir()
                .unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".rollvault"));
            std::fs::create_dir_all(&data_dir)?;
            let settings_path = data_dir.join("settings.json");
            let settings: Value = std::fs::read(&settings_path)
                .ok()
                .and_then(|d| serde_json::from_slice(&d).ok())
                .unwrap_or(Value::Object(Default::default()));
            let sounds = settings.get("sounds").and_then(Value::as_bool).unwrap_or(true);
            let notifications = settings.get("notifications").and_then(Value::as_bool).unwrap_or(true);
            apply_language(&settings);
            let engine = Engine::new(EngineConfig {
                data_dir: data_dir.clone(),
                tool_version: env!("CARGO_PKG_VERSION").into(),
                sounds,
                notifications,
            });
            start_watchers(app.handle().clone(), engine.clone());
            app.manage(AppState {
                engine,
                data_dir,
                settings_path,
                settings: Mutex::new(settings),
                allow_close: AtomicBool::new(false),
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let state = window.state::<AppState>();
                if state.engine.any_active() && !state.allow_close.load(Ordering::Relaxed) {
                    api.prevent_close();
                    let _ = window.emit("close-requested", ());
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            app_info,
            list_volumes,
            preflight,
            start_job,
            cancel_job,
            dismiss_job,
            resume_job,
            jobs,
            history,
            eject_source,
            eject_volume,
            reveal,
            open_path,
            load_settings,
            save_settings,
            read_logo,
            render_roll_path,
            next_roll,
            export_pdf,
            any_active,
            start_verify,
            verify_tasks,
            cancel_verify,
            dismiss_verify,
            quit_app
        ])
        .run(tauri::generate_context!())
        .expect("error while running RollVault");
}
