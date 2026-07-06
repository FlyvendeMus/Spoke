//! Spoke core: glues hotkey → capture → STT → injection together and exposes
//! a small command/event surface to the HTML/JS bubble UI.

mod audio;
mod config;
mod hotkey;
mod inject;
mod permissions;
mod platform;
mod stt;

use audio::AudioEngine;
use config::{Config, Mode, Trigger};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use stt::SttEngine;
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State};

/// Stable id so commands can fetch the tray back via `tray_by_id`.
const TRAY_ID: &str = "spoke-tray";
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

/// Identifies the config fields that determine how an `SttEngine` is built.
/// When these are unchanged we reuse the cached engine instead of rebuilding —
/// rebuilding the offline engine reloads the whole Whisper model into RAM and
/// re-initializes the Metal/CoreML backends (the CoreML ANE specialization can
/// take minutes on first use), so the engine — including its whisper state —
/// must be built once and reused across transcriptions.
#[derive(PartialEq, Eq)]
struct EngineKey {
    mode: Mode,
    model: String,
    use_gpu: bool,
    accel: String,
    provider: String,
    api_key: String,
}

impl EngineKey {
    fn from_config(cfg: &Config) -> Self {
        Self {
            mode: cfg.general.mode,
            model: cfg.offline.model.clone(),
            use_gpu: cfg.offline.use_gpu,
            accel: cfg.offline.accel.clone(),
            provider: cfg.online.provider.clone(),
            api_key: cfg.online.api_key.clone(),
        }
    }
}

/// Shared application state, managed by Tauri as `Arc<SpokeState>`.
pub struct SpokeState {
    config: Mutex<Config>,
    audio: AudioEngine,
    recording: AtomicBool,
    /// Monotonically increasing counter incremented each time a new recording
    /// begins. The pipeline captures the current value when it starts and checks
    /// it before injecting text — if a newer session has started the result is
    /// discarded, effectively cancelling the stale in-flight transcription.
    session: AtomicU64,
    /// Cached STT engine + the config signature it was built for. Built lazily
    /// on first transcription and reused until the relevant config changes.
    engine: Mutex<Option<(EngineKey, Arc<SttEngine>)>>,
}

impl SpokeState {
    fn config_snapshot(&self) -> Config {
        self.config.lock().unwrap().clone()
    }

    /// Return the cached STT engine, rebuilding it only when the engine-relevant
    /// config has changed since it was last built. The returned `Arc` is cloned
    /// out so the lock isn't held across the (possibly async) transcription.
    fn engine(&self, cfg: &Config) -> anyhow::Result<Arc<SttEngine>> {
        let key = EngineKey::from_config(cfg);
        let mut slot = self.engine.lock().unwrap();
        if let Some((cached_key, engine)) = slot.as_ref() {
            if *cached_key == key {
                return Ok(engine.clone());
            }
        }
        let engine = Arc::new(SttEngine::from_config(cfg)?);
        *slot = Some((key, engine.clone()));
        Ok(engine)
    }
}

/// Build the STT engine in the background so the first recording doesn't pay
/// model load + Metal/CoreML init on the critical path (the first-ever ANE
/// specialization of a CoreML model on a device can take minutes). No-op when
/// the cached engine already matches the config. Only warms offline mode —
/// the online engine is trivial to build lazily, and warming it would surface
/// "missing API key" errors at launch for unconfigured setups.
fn prewarm_engine(app: &AppHandle, state: &Arc<SpokeState>) {
    let cfg = state.config_snapshot();
    if cfg.general.mode != Mode::Offline {
        return;
    }
    #[cfg(feature = "whisper")]
    {
        // Don't warm (and thus error) when the model isn't downloaded yet.
        if !stt::whisper::model_exists(&cfg.offline.model) {
            return;
        }
        let app = app.clone();
        let state = Arc::clone(state);
        tauri::async_runtime::spawn(async move {
            emit_state(&app, "processing", None);
            let build = {
                let state = Arc::clone(&state);
                tokio::task::spawn_blocking(move || state.engine(&cfg).map(|_| ())).await
            };
            match build {
                Ok(Ok(())) => {}
                Ok(Err(e)) => emit_state(&app, "error", Some(format!("engine init: {e}"))),
                Err(e) => emit_state(&app, "error", Some(format!("engine init panicked: {e}"))),
            }
            emit_state(&app, "idle", None);
        });
    }
    #[cfg(not(feature = "whisper"))]
    let _ = app;
}

/// UI-facing recording state, sent on the `spoke:state` event.
fn emit_state(app: &AppHandle, state: &str, message: Option<String>) {
    let _ = app.emit(
        "spoke:state",
        serde_json::json!({ "state": state, "message": message }),
    );
}

// ---- Tauri commands -------------------------------------------------------

#[tauri::command]
fn get_config(state: State<'_, Arc<SpokeState>>) -> Config {
    state.config_snapshot()
}

#[tauri::command]
fn set_config(
    app: AppHandle,
    state: State<'_, Arc<SpokeState>>,
    new_config: Config,
) -> Result<(), String> {
    new_config.save().map_err(|e| e.to_string())?;
    *state.config.lock().unwrap() = new_config.clone();
    // Re-register the hotkey in case it changed.
    register_hotkey(&app, &new_config).map_err(|e| e.to_string())?;
    // Rebuild the engine in the background if engine-relevant config changed
    // (no-op otherwise), so the next recording doesn't pay the init cost.
    prewarm_engine(&app, &state);
    Ok(())
}

#[tauri::command]
fn list_audio_devices() -> Vec<String> {
    audio::list_input_devices()
}

#[tauri::command]
fn get_build_info() -> serde_json::Value {
    platform::build_info()
}

#[tauri::command]
fn check_permissions() -> permissions::Permissions {
    permissions::check()
}

#[tauri::command]
fn open_permission_settings(which: String) {
    permissions::open_settings(&which);
}

#[tauri::command]
fn request_accessibility_permission() {
    #[cfg(target_os = "macos")]
    permissions::request_accessibility();
    #[cfg(not(target_os = "macos"))]
    {}
}

/// Show the native mic permission prompt (if undetermined) and resolve with the
/// resulting status. A grant here applies to the running process immediately.
#[tauri::command]
async fn request_microphone_permission() -> bool {
    #[cfg(target_os = "macos")]
    {
        let (tx, rx) = tokio::sync::oneshot::channel::<bool>();
        let tx = std::sync::Mutex::new(Some(tx));
        permissions::request_microphone(move |granted| {
            if let Some(tx) = tx.lock().unwrap().take() {
                let _ = tx.send(granted);
            }
        });
        rx.await.unwrap_or(false)
    }
    #[cfg(not(target_os = "macos"))]
    true
}

/// Clear this app's TCC entry for a permission ("microphone" | "accessibility")
/// so it can be re-requested cleanly. Recovers from the stale-grant state where
/// System Settings shows Spoke enabled but the OS denies the running binary
/// (the entry was recorded for a previous build's code signature).
#[tauri::command]
fn reset_permission(app: AppHandle, which: String) {
    permissions::reset(&which, &app.config().identifier);
}

/// Relaunch the process. Some permission changes only apply to a fresh process
/// (e.g. macOS revokes-then-regrants via System Settings while running); the UI
/// offers this as a one-click fallback when a grant doesn't register live.
#[tauri::command]
fn restart_app(app: tauri::AppHandle) {
    app.restart();
}

#[cfg(feature = "whisper")]
#[tauri::command]
fn check_model(model: String) -> Result<serde_json::Value, String> {
    let mut info = serde_json::json!({
        "exists": stt::whisper::model_exists(&model),
        "url": stt::whisper::model_download_url(&model),
        "coreml_exists": false,
    });
    #[cfg(feature = "coreml")]
    {
        info["coreml_exists"] = stt::whisper::coreml_bundle_exists(&model).into();
        info["coreml_url"] = stt::whisper::coreml_bundle_url(&model).into();
    }
    Ok(info)
}

#[cfg(feature = "whisper")]
#[tauri::command]
fn check_models() -> Result<Vec<String>, String> {
    let known = ["tiny", "base", "small", "medium", "large-v3-turbo", "large-v3"];
    Ok(known
        .iter()
        .filter(|m| stt::whisper::model_exists(m))
        .map(|s| s.to_string())
        .collect())
}

#[cfg(feature = "coreml")]
#[tauri::command]
async fn download_coreml_bundle(app: AppHandle, model: String) -> Result<(), String> {
    use futures_util::StreamExt;
    use tokio::io::AsyncWriteExt;

    let url = stt::whisper::coreml_bundle_url(&model);
    let dest_dir = stt::whisper::models_dir();
    std::fs::create_dir_all(&dest_dir)
        .map_err(|e| format!("failed to create models dir: {e}"))?;

    let bundle_path = dest_dir.join(format!("ggml-{model}-encoder.mlmodelc"));
    if bundle_path.exists() {
        let _ = app.emit("spoke:coreml-complete", serde_json::json!({ "model": model }));
        return Ok(());
    }

    let tmp_path = dest_dir.join(format!("ggml-{model}-encoder.mlmodelc.zip.tmp"));

    let client = reqwest::Client::builder()
        .user_agent("Spoke/0.1")
        .build()
        .map_err(|e| e.to_string())?;

    let response = client.get(&url).send().await.map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("server returned {}", response.status()));
    }

    let total = response.content_length().unwrap_or(0);
    let mut downloaded: u64 = 0;
    let mut file = tokio::fs::File::create(&tmp_path).await.map_err(|e| e.to_string())?;
    let mut stream = response.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        file.write_all(&chunk).await.map_err(|e| e.to_string())?;
        downloaded += chunk.len() as u64;
        if total > 0 {
            let percent = (downloaded as f64 / total as f64 * 100.0) as u32;
            let _ = app.emit("spoke:coreml-progress", serde_json::json!({
                "model": &model,
                "percent": percent,
                "phase": "download",
            }));
        }
    }
    file.flush().await.map_err(|e| e.to_string())?;
    drop(file);

    // Extract on a blocking thread (zip::ZipArchive requires Seek).
    let tmp_clone = tmp_path.clone();
    let dest_clone = dest_dir.clone();
    let model_clone = model.clone();
    let app_clone = app.clone();

    tokio::task::spawn_blocking(move || -> Result<(), String> {
        let _ = app_clone.emit("spoke:coreml-progress", serde_json::json!({
            "model": &model_clone,
            "percent": 100,
            "phase": "extract",
        }));

        let f = std::fs::File::open(&tmp_clone)
            .map_err(|e| format!("open zip: {e}"))?;
        let mut archive = zip::ZipArchive::new(f)
            .map_err(|e| format!("read zip: {e}"))?;

        for i in 0..archive.len() {
            let mut entry = archive.by_index(i).map_err(|e| format!("zip entry: {e}"))?;
            // enclosed_name() rejects absolute paths and `..` traversal.
            let rel = entry
                .enclosed_name()
                .ok_or_else(|| format!("unsafe zip path: {}", entry.name()))?;
            let out = dest_clone.join(rel);
            if entry.is_dir() {
                std::fs::create_dir_all(&out).map_err(|e| format!("mkdir: {e}"))?;
            } else {
                if let Some(p) = out.parent() {
                    std::fs::create_dir_all(p).map_err(|e| format!("mkdir: {e}"))?;
                }
                let mut out_f = std::fs::File::create(&out).map_err(|e| format!("create: {e}"))?;
                std::io::copy(&mut entry, &mut out_f).map_err(|e| format!("extract: {e}"))?;
            }
        }

        let _ = std::fs::remove_file(&tmp_clone);
        Ok(())
    })
    .await
    .map_err(|e| format!("extraction panicked: {e}"))??;

    let _ = app.emit("spoke:coreml-complete", serde_json::json!({ "model": model }));
    Ok(())
}

#[cfg(feature = "whisper")]
#[tauri::command]
async fn download_model(
    app: AppHandle,
    model: String,
) -> Result<(), String> {
    use futures_util::StreamExt;
    use tokio::io::AsyncWriteExt;

    let url = stt::whisper::model_download_url(&model);
    let dest_dir = stt::whisper::models_dir();
    std::fs::create_dir_all(&dest_dir).map_err(|e| format!("failed to create models directory: {e}"))?;
    let dest_path = dest_dir.join(format!("ggml-{model}.bin"));

    if dest_path.exists() {
        let _ = app.emit("spoke:download-complete", serde_json::json!({ "model": model }));
        return Ok(());
    }

    // Stream to a temp file and rename on success, so an interrupted download
    // never leaves a truncated file that later passes the "model exists" check.
    let tmp_path = dest_dir.join(format!("ggml-{model}.bin.tmp"));

    let client = reqwest::Client::builder()
        .user_agent("Spoke/0.1")
        .build()
        .map_err(|e| e.to_string())?;

    let response = client.get(&url).send().await.map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("server returned {}", response.status()));
    }

    let total = response.content_length().unwrap_or(0);
    let mut downloaded: u64 = 0;
    let mut file = tokio::fs::File::create(&tmp_path).await.map_err(|e| e.to_string())?;
    let mut stream = response.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        file.write_all(&chunk).await.map_err(|e| e.to_string())?;
        downloaded += chunk.len() as u64;
        if total > 0 {
            let percent = (downloaded as f64 / total as f64 * 100.0) as u32;
            let _ = app.emit("spoke:download-progress", serde_json::json!({
                "model": model,
                "downloaded": downloaded,
                "total": total,
                "percent": percent,
            }));
        }
    }

    file.flush().await.map_err(|e| e.to_string())?;
    drop(file);
    tokio::fs::rename(&tmp_path, &dest_path)
        .await
        .map_err(|e| format!("failed to finalize download: {e}"))?;
    let _ = app.emit("spoke:download-complete", serde_json::json!({ "model": model }));
    Ok(())
}

#[tauri::command]
fn start_recording(app: AppHandle, state: State<'_, Arc<SpokeState>>) {
    begin_recording(&app, state.inner());
}

#[tauri::command]
fn stop_recording(app: AppHandle, state: State<'_, Arc<SpokeState>>) {
    let inner = state.inner().clone();
    finish_recording(app, inner);
}

#[tauri::command]
fn is_recording(state: State<'_, Arc<SpokeState>>) -> bool {
    state.recording.load(Ordering::SeqCst)
}

/// Force the bubble window to present its current content.
///
/// On Wayland, WebKitGTK never re-commits a transparent, unfocused window's
/// surface on content change, so toggling the settings panel updates the DOM but
/// not the pixels until an unrelated ~20s refresh. A real window resize triggers a
/// surface reconfigure + commit, which is the only reliable way to present the new
/// frame. We flip the width by 1px (oscillating, so it never drifts and the
/// compositor can't coalesce it away); the change is imperceptible.
/// Move and resize the bubble window together (boot placement, non-Linux
/// open/close). Issued back-to-back so both land in the same event-loop turn.
#[tauri::command]
fn set_window_bounds(app: AppHandle, x: f64, y: f64, w: f64, h: f64) {
    if let Some(win) = app.get_webview_window("bubble") {
        let _ = win.set_size(tauri::LogicalSize::new(w, h));
        let _ = win.set_position(tauri::LogicalPosition::new(x, y));
    }
}

/// Linux menu open/close: resize the window anchored to the bubble's corner.
///
/// Keeping the bubble fixed through a resize naively takes a resize plus a
/// move, but any move request is validated by the WM against the workarea
/// using whatever size it believes at that instant — closing the menu near a
/// screen/monitor edge gets the position clamped (the still-menu-sized window
/// wouldn't fit) and the bubble visibly walks. Setting the ICCCM win-gravity
/// to the bubble's corner and sending a resize *only* removes the move
/// entirely: the WM itself keeps that corner pinned. (Don't reach for gdk's
/// move_resize instead — it resizes the X window behind GTK's back and the
/// webview keeps painting only the old area.)
#[tauri::command]
fn set_window_size_anchored(app: AppHandle, w: f64, h: f64, gravity: String) {
    #[cfg(target_os = "linux")]
    if let Some(win) = app.get_webview_window("bubble") {
        use gtk::prelude::*;
        let win2 = win.clone();
        let _ = win.run_on_main_thread(move || {
            let Ok(gtk_win) = win2.gtk_window() else { return };
            let g = match gravity.as_str() {
                "nw" => gtk::gdk::Gravity::NorthWest,
                "ne" => gtk::gdk::Gravity::NorthEast,
                "sw" => gtk::gdk::Gravity::SouthWest,
                _ => gtk::gdk::Gravity::SouthEast,
            };
            gtk_win.set_gravity(g);
            gtk_win.resize(w as i32, h as i32);
        });
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (app, w, h, gravity);
}

#[tauri::command]
fn nudge_repaint(app: AppHandle) {
    #[cfg(target_os = "linux")]
    {
        // Only the Wayland backend needs the nudge (run() defaults GDK_BACKEND
        // to x11, so this is only hit on an explicit override). On X11 the
        // webview presents on its own and the oscillating resize just adds
        // visible flicker.
        let wayland = std::env::var("GDK_BACKEND")
            .map(|v| v.contains("wayland"))
            .unwrap_or(false);
        if !wayland {
            return;
        }
        if let Some(win) = app.get_webview_window("bubble") {
            if let Ok(size) = win.outer_size() {
                let width = if size.width % 2 == 0 {
                    size.width + 1
                } else {
                    size.width - 1
                };
                let _ = win.set_size(tauri::PhysicalSize::new(width, size.height));
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = app;
}

// ---- Recording lifecycle --------------------------------------------------

fn begin_recording(app: &AppHandle, state: &Arc<SpokeState>) {
    // Refuse to start capturing without mic permission. Without this guard the
    // OS prompt fires mid-recording on first use: CoreAudio captures silence
    // while the dialog is up and the dictation "fails" confusingly. Triggering
    // the prompt here instead means the user grants once and the next attempt
    // works — no restart needed.
    #[cfg(target_os = "macos")]
    match permissions::check().microphone {
        permissions::PermissionState::Granted | permissions::PermissionState::Unknown => {}
        permissions::PermissionState::Undetermined => {
            permissions::request_microphone(|_| {});
            emit_state(
                app,
                "error",
                Some("Microphone permission needed — grant it in the dialog, then dictate again".into()),
            );
            return;
        }
        permissions::PermissionState::Denied => {
            emit_state(
                app,
                "error",
                Some("Microphone access denied — open the Microphone card to fix it".into()),
            );
            return;
        }
    }

    if state.recording.swap(true, Ordering::SeqCst) {
        return; // already recording
    }
    // Bump the session counter so any in-flight pipeline can detect
    // it has been superseded and skip injecting stale text.
    state.session.fetch_add(1, Ordering::SeqCst);

    let device = state.config_snapshot().recording.input_device.clone();
    if let Err(e) = state.audio.start(&device) {
        state.recording.store(false, Ordering::SeqCst);
        emit_state(app, "error", Some(e.to_string()));
        return;
    }
    emit_state(app, "recording", None);
}

/// Stop capture, transcribe, and inject. Runs the heavy work off the UI path.
fn finish_recording(app: AppHandle, state: Arc<SpokeState>) {
    if !state.recording.swap(false, Ordering::SeqCst) {
        return; // wasn't recording
    }
    let session = state.session.load(Ordering::SeqCst);
    emit_state(&app, "processing", None);

    let app_clone = app.clone();
    tauri::async_runtime::spawn(async move {
        match run_pipeline(&app_clone, &state, session).await {
            Ok(_) => emit_state(&app, "idle", None),
            Err(e) => {
                emit_state(&app, "error", Some(e.to_string()));
                // Settle back to idle so the bubble doesn't stay stuck.
                emit_state(&app, "idle", None);
            }
        }
        // Per-run scratch (resampled audio, base64 buffers) is freed by now —
        // the whisper state (KV cache, Metal/CoreML contexts) is deliberately
        // retained inside the cached engine because re-initializing it per run
        // costs seconds (Metal) to minutes (CoreML/ANE). glibc keeps freed
        // scratch in its arenas rather than returning it to the OS, so force
        // the freed pages back so RSS stays flat.
        release_heap();
    });
}

/// Return freed heap pages to the OS. glibc's allocator caches freed blocks in
/// per-thread arenas indefinitely; `malloc_trim` releases the top of the heap
/// back to the kernel so RSS drops after each transcription instead of climbing.
/// No-op on non-glibc targets (musl, macOS, Windows).
fn release_heap() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        extern "C" {
            fn malloc_trim(pad: usize) -> i32;
        }
        // Safety: malloc_trim is always safe to call; it only frees unused heap.
        unsafe {
            malloc_trim(0);
        }
    }
}

async fn run_pipeline(app: &AppHandle, state: &Arc<SpokeState>, session: u64) -> anyhow::Result<()> {
    let cfg = state.config_snapshot();
    let rec = state.audio.stop()?;

    let mono = audio::to_mono(&rec.samples, rec.channels);
    if mono.is_empty() {
        return Err(anyhow::anyhow!(
            "No audio captured – check microphone permissions and device selection"
        ));
    }

    let sample_rate = rec.sample_rate;

    // Optional audio save.
    if cfg.recording.save_audio {
        let dir = cfg.resolved_save_path();
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let path = dir.join(format!("spoke-{ts}.wav"));
        if cfg.recording.save_processed {
            let processed = audio::strip_internal_silence(&mono, sample_rate);
            let processed = audio::resample_mono(&processed, sample_rate, 16000);
            if let Err(e) = audio::save_wav_mono(&path, &processed, 16000) {
                eprintln!("[spoke] failed to save processed audio: {e}");
            }
        } else if let Err(e) = audio::save_wav(&path, &rec) {
            eprintln!("[spoke] failed to save audio: {e}");
        }
    }

    // Raw recording (interleaved samples at device rate) is no longer needed once
    // downmixed; drop it before transcription so it isn't resident during inference.
    drop(rec);

    let engine = state.engine(&cfg)?;
    // `mono` is moved in and freed as soon as inference completes; the whisper
    // arm runs on a blocking thread so this await doesn't stall the executor.
    let transcript = engine
        .transcribe(mono, sample_rate, cfg.general.language.clone())
        .await?;

    let transcript = transcript.trim().to_string();
    if transcript.is_empty() {
        return Ok(());
    }

    // If a new recording session has started since this pipeline was launched,
    // discard the stale transcript instead of injecting it.
    if state.session.load(Ordering::SeqCst) != session {
        return Ok(());
    }

    let dest = cfg.general.output_dest;

    let _ = app.emit("spoke:transcript", serde_json::json!({ "text": &transcript }));

    if dest.copies() {
        let text = transcript.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let mut cb = arboard::Clipboard::new()
                .map_err(|e| anyhow::anyhow!("clipboard init: {e}"))?;
            cb.set_text(&text)
                .map_err(|e| anyhow::anyhow!("clipboard set: {e}"))?;
            println!("[spoke] copied to clipboard: {text}");
            Ok(())
        })
        .await
        .map_err(|e| anyhow::anyhow!("clipboard task panicked: {e}"))??;
    }
    if dest.types() {
        // enigo is not Send; run it on a blocking thread.
        tokio::task::spawn_blocking(move || inject::inject_text(&transcript))
            .await
            .map_err(|e| anyhow::anyhow!("inject task panicked: {e}"))??;
    }

    Ok(())
}

// ---- Hotkey ---------------------------------------------------------------

fn register_hotkey(app: &AppHandle, cfg: &Config) -> anyhow::Result<()> {
    let shortcut = hotkey::parse_shortcut(&cfg.general.hotkey)?;
    let gs = app.global_shortcut();
    let _ = gs.unregister_all();
    gs.register(shortcut)?;
    Ok(())
}

// ---- App entry ------------------------------------------------------------

/// WebKitGTK ≥ 2.50 enables "damage propagation" by default: instead of
/// presenting a fully repainted frame, only the damaged rectangles are pushed
/// to the windowing system. On a transparent window this is broken (at least
/// on NVIDIA/XWayland): the damaged region's translucent pixels get blended
/// OVER the previous frame instead of replacing it, so drop shadows re-draw
/// on top of themselves and darken with every repaint until the next full
/// redraw (~10 s) resets the cycle. Turn the feature off for our webview.
///
/// The feature-list API (webkit 2.42+) postdates the webkit2gtk crate's
/// bindings, so the C symbols are declared here directly; the library is
/// already linked.
#[cfg(target_os = "linux")]
fn disable_damage_propagation(win: &tauri::WebviewWindow) {
    use std::ffi::{c_char, c_void, CStr};
    extern "C" {
        fn webkit_settings_get_all_features() -> *mut c_void;
        fn webkit_feature_list_get_length(list: *mut c_void) -> usize;
        fn webkit_feature_list_get(list: *mut c_void, index: usize) -> *mut c_void;
        fn webkit_feature_get_identifier(feature: *mut c_void) -> *const c_char;
        fn webkit_settings_set_feature_enabled(
            settings: *mut c_void,
            feature: *mut c_void,
            enabled: i32,
        );
        fn webkit_feature_list_unref(list: *mut c_void);
    }
    let _ = win.with_webview(|webview| {
        use gtk::glib::translate::ToGlibPtr;
        use webkit2gtk::WebViewExt;
        let Some(settings) = webview.inner().settings() else {
            return;
        };
        let settings_ptr: *mut webkit2gtk::ffi::WebKitSettings = settings.to_glib_none().0;
        unsafe {
            let list = webkit_settings_get_all_features();
            if list.is_null() {
                return;
            }
            for i in 0..webkit_feature_list_get_length(list) {
                let feature = webkit_feature_list_get(list, i);
                let id = webkit_feature_get_identifier(feature);
                if !id.is_null()
                    && CStr::from_ptr(id).to_bytes() == b"PropagateDamagingInformation"
                {
                    webkit_settings_set_feature_enabled(settings_ptr.cast(), feature, 0);
                }
            }
            webkit_feature_list_unref(list);
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Linux windowing: force the X11 GDK backend (XWayland on Wayland sessions)
    // unless the user overrides it. Native Wayland can neither report nor set
    // global window positions, which breaks the bubble's edge-aware menu
    // flipping (outerPosition/setPosition silently no-op, so the menu always
    // grows down-right and gets clipped), and WebKitGTK's Wayland path needs
    // GPU compositing disabled to show transparent windows at all — software
    // rendering that ghosts and mangles drop shadows. Under X11, stock
    // WebKitGTK compositing (DMABUF renderer included) presents the transparent
    // window cleanly. Do NOT set WEBKIT_DISABLE_COMPOSITING_MODE or
    // WEBKIT_DISABLE_DMABUF_RENDERER here: both drop WebKit to a fallback path
    // that never clears the window's alpha buffer between frames, so
    // translucent pixels (drop shadows) accumulate darker every repaint and
    // moving elements leave trails. If the webview ever comes up blank
    // (older NVIDIA driver combos), export WEBKIT_DISABLE_DMABUF_RENDERER=1
    // manually as a last resort and expect those artifacts.
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if std::env::var("GDK_BACKEND").is_err() {
            std::env::set_var("GDK_BACKEND", "x11");
        }
    }

    let config = Config::load().unwrap_or_default();

    // Audio amplitude channel: capture thread → UI events.
    let (amp_tx, amp_rx) = std::sync::mpsc::channel::<f32>();
    let audio = AudioEngine::spawn(amp_tx);

    let state = Arc::new(SpokeState {
        config: Mutex::new(config),
        audio,
        recording: AtomicBool::new(false),
        session: AtomicU64::new(0),
        engine: Mutex::new(None),
    });

    tauri::Builder::default()
        .plugin(build_shortcut_plugin())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            get_config,
            set_config,
            start_recording,
            stop_recording,
            is_recording,
            list_audio_devices,
            nudge_repaint,
            minimize_to_tray,
            set_tray_state,
            set_window_bounds,
            set_window_size_anchored,
            get_build_info,
            check_permissions,
            open_permission_settings,
            request_accessibility_permission,
            request_microphone_permission,
            reset_permission,
            restart_app,
            #[cfg(feature = "whisper")]
            check_model,
            #[cfg(feature = "whisper")]
            check_models,
            #[cfg(feature = "whisper")]
            download_model,
            #[cfg(feature = "coreml")]
            download_coreml_bundle,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();

            // Forward amplitude values to the UI.
            let amp_handle = handle.clone();
            std::thread::spawn(move || {
                while let Ok(v) = amp_rx.recv() {
                    let _ = amp_handle.emit("spoke:amplitude", v);
                }
            });

            // Register the configured hotkey.
            let cfg = app.state::<Arc<SpokeState>>().config_snapshot();
            if let Err(e) = register_hotkey(&handle, &cfg) {
                eprintln!("[spoke] hotkey registration failed: {e}");
            }

            // Warm the STT engine so the first recording is fast.
            prewarm_engine(&handle, &app.state::<Arc<SpokeState>>());

            build_tray(app)?;
            position_bubble(&handle);

            if let Some(win) = handle.get_webview_window("bubble") {
                // Ensure the window receives pointer (mouse) events. On Linux,
                // transparent windows with decorations=false may default to
                // ignoring cursor events, making clicks pass through silently.
                let _ = win.set_ignore_cursor_events(false);

                // GTK snaps a non-resizable window back to its child's natural
                // size (the webview reports 200×200), silently overriding the
                // 80×80 bubble-only size and stranding the bubble mid-window.
                // Marking the window resizable makes programmatic resizes
                // stick; with no decorations the user still can't drag-resize.
                #[cfg(target_os = "linux")]
                {
                    disable_damage_propagation(&win);
                    let _ = win.set_resizable(true);
                    // After a resize, the X server fills not-yet-painted
                    // regions with the window's background before WebKit's
                    // first frame lands; the default fill is black, which
                    // flashes as a dark rectangle when the menu opens. Stop
                    // GTK painting a default background and set the X-level
                    // background to transparent black (valid for the ARGB
                    // visual) so the exposed region is simply invisible.
                    if let Ok(gtk_win) = win.gtk_window() {
                        use gtk::glib::translate::ToGlibPtr;
                        use gtk::prelude::*;
                        gtk_win.set_app_paintable(true);
                        if let Some(gdk_win) = gtk_win.window() {
                            let rgba = gtk::gdk::RGBA::new(0.0, 0.0, 0.0, 0.0);
                            // Deprecated in GTK 3.22 but still functional; the
                            // rust binding gates it away, so call the C symbol.
                            unsafe {
                                gtk::gdk::ffi::gdk_window_set_background_rgba(
                                    gdk_win.to_glib_none().0,
                                    rgba.to_glib_none().0,
                                );
                            }
                        }
                    }
                }

                // Keep the bubble visible when the user switches Spaces on macOS.
                #[cfg(target_os = "macos")]
                let _ = win.set_visible_on_all_workspaces(true);
            }

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Spoke");
}

fn build_shortcut_plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    tauri_plugin_global_shortcut::Builder::new()
        .with_handler(|app, _shortcut, event| {
            let state = app.state::<Arc<SpokeState>>();
            let trigger = state.config_snapshot().general.trigger;
            match event.state() {
                ShortcutState::Pressed => match trigger {
                    Trigger::PushToTalk => begin_recording(app, &state),
                    Trigger::Toggle => {
                        if state.recording.load(Ordering::SeqCst) {
                            finish_recording(app.clone(), state.inner().clone());
                        } else {
                            begin_recording(app, &state);
                        }
                    }
                },
                ShortcutState::Released => {
                    if trigger == Trigger::PushToTalk {
                        finish_recording(app.clone(), state.inner().clone());
                    }
                }
            }
        })
        .build()
}

fn build_tray(app: &tauri::App) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Show Spoke", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Spoke", true, None::<&str>)?;
    // The menu is the reliable restore path on Linux, where appindicator trays
    // don't emit left-click events.
    let menu = Menu::with_items(app, &[&show, &quit])?;
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(app.default_window_icon().unwrap().clone())
        .menu(&menu)
        // Left click restores the window; the menu is right-click only.
        .show_menu_on_left_click(false)
        .tooltip("Spoke")
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => restore_from_tray(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                restore_from_tray(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

/// Show the bubble again and reset the tray to its neutral icon.
fn restore_from_tray(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("bubble") {
        let _ = win.show();
        let _ = win.set_focus();
    }
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        if let Some(icon) = app.default_window_icon().cloned() {
            let _ = tray.set_icon(Some(icon));
        }
    }
    // Tell the UI it's no longer minimized so it stops pushing tray colors.
    let _ = app.emit("spoke:restored", ());
}

/// Hide every window; the app lives only in the tray until restored.
#[tauri::command]
fn minimize_to_tray(app: AppHandle) {
    if let Some(win) = app.get_webview_window("bubble") {
        let _ = win.hide();
    }
}

/// Recolor the tray icon to reflect app state while minimized.
/// gray = idle, red = recording, blue = processing, yellow = warning.
#[tauri::command]
fn set_tray_state(app: AppHandle, state: String) {
    let color: [u8; 3] = match state.as_str() {
        "recording" => [0xE5, 0x39, 0x35],
        "processing" => [0x1E, 0x88, 0xE5],
        "warning" => [0xFD, 0xD8, 0x35],
        _ => [0x9E, 0x9E, 0x9E],
    };
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_icon(Some(colored_tray_icon(color)));
    }
}

/// Build a filled, anti-aliased circle of the given RGB as a tray icon.
fn colored_tray_icon(rgb: [u8; 3]) -> Image<'static> {
    const SIZE: u32 = 32;
    let r = SIZE as f32 / 2.0;
    let edge = r - 1.5; // start of the anti-aliased rim
    let mut buf = vec![0u8; (SIZE * SIZE * 4) as usize];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = x as f32 + 0.5 - r;
            let dy = y as f32 + 0.5 - r;
            let dist = (dx * dx + dy * dy).sqrt();
            let a = if dist <= edge {
                1.0
            } else if dist >= r {
                0.0
            } else {
                (r - dist) / (r - edge)
            };
            let i = ((y * SIZE + x) * 4) as usize;
            buf[i] = rgb[0];
            buf[i + 1] = rgb[1];
            buf[i + 2] = rgb[2];
            buf[i + 3] = (a * 255.0) as u8;
        }
    }
    Image::new_owned(buf, SIZE, SIZE)
}

/// Park the bubble in the bottom-right of the primary monitor.
fn position_bubble(app: &AppHandle) {
    use tauri::{LogicalPosition, PhysicalSize};
    if let Some(win) = app.get_webview_window("bubble") {
        if let (Ok(Some(monitor)), Ok(PhysicalSize { width, height })) =
            (win.current_monitor(), win.outer_size())
        {
            let scale = monitor.scale_factor();
            let screen = monitor.size().to_logical::<f64>(scale);
            let win_w = width as f64 / scale;
            let win_h = height as f64 / scale;
            let margin = 24.0;
            let x = screen.width - win_w - margin;
            let y = screen.height - win_h - margin;
            let _ = win.set_position(LogicalPosition::new(x, y));
        }
    }
}
