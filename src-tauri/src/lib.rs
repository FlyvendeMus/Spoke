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
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

/// Identifies the config fields that determine how an `SttEngine` is built.
/// When these are unchanged we reuse the cached engine instead of rebuilding —
/// rebuilding the offline engine reloads the whole Whisper model into RAM, which
/// is the dominant memory cost per transcription.
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
#[tauri::command]
fn nudge_repaint(app: AppHandle) {
    #[cfg(target_os = "linux")]
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
    #[cfg(not(target_os = "linux"))]
    let _ = app;
}

// ---- Recording lifecycle --------------------------------------------------

fn begin_recording(app: &AppHandle, state: &Arc<SpokeState>) {
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
        // Per-run scratch (resampled audio, whisper KV-cache state, base64
        // buffers) is freed by now, but glibc keeps it in its arenas rather than
        // returning it to the OS, so RSS ratchets up after each transcription.
        // Force the freed pages back to the OS so memory stays flat.
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
    let transcript = engine
        .transcribe(&mono, sample_rate, &cfg.general.language)
        .await?;
    drop(mono);

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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // WEBKIT_DISABLE_COMPOSITING_MODE=1 is required on Linux to prevent WebKitGTK
    // from using GPU compositing, which on many setups causes the window to render
    // blank or invisible.  Trade-off: some visual rendering artifacts.
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::env::set_var("WEBKIT_DISABLE_COMPOSITING_MODE", "1");
        if std::env::var("GDK_BACKEND").is_err() {
            if std::env::var("WAYLAND_DISPLAY").is_ok() {
                std::env::set_var("GDK_BACKEND", "wayland,x11");
            }
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
            get_build_info,
            check_permissions,
            open_permission_settings,
            #[cfg(feature = "whisper")]
            check_model,
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

            build_tray(app)?;
            position_bubble(&handle);

            if let Some(win) = handle.get_webview_window("bubble") {
                // Ensure the window receives pointer (mouse) events. On Linux,
                // transparent windows with decorations=false may default to
                // ignoring cursor events, making clicks pass through silently.
                let _ = win.set_ignore_cursor_events(false);

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
    let quit = MenuItem::with_id(app, "quit", "Quit Spoke", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&quit])?;
    TrayIconBuilder::new()
        .icon(app.default_window_icon().unwrap().clone())
        .menu(&menu)
        .tooltip("Spoke")
        .on_menu_event(|app, event| {
            if event.id().as_ref() == "quit" {
                app.exit(0);
            }
        })
        .build(app)?;
    Ok(())
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
