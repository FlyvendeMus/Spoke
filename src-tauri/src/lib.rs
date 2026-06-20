//! Spoke core: glues hotkey → capture → STT → injection together and exposes
//! a small command/event surface to the HTML/JS bubble UI.

mod audio;
mod config;
mod hotkey;
mod inject;
mod stt;

use audio::AudioEngine;
use config::{Config, Trigger};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

/// Shared application state, managed by Tauri as `Arc<SpokeState>`.
pub struct SpokeState {
    config: Mutex<Config>,
    audio: AudioEngine,
    recording: AtomicBool,
}

impl SpokeState {
    fn config_snapshot(&self) -> Config {
        self.config.lock().unwrap().clone()
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

// ---- Recording lifecycle --------------------------------------------------

fn begin_recording(app: &AppHandle, state: &Arc<SpokeState>) {
    if state.recording.swap(true, Ordering::SeqCst) {
        return; // already recording
    }
    if let Err(e) = state.audio.start() {
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
    emit_state(&app, "processing", None);

    tauri::async_runtime::spawn(async move {
        match run_pipeline(&state).await {
            Ok(_) => emit_state(&app, "idle", None),
            Err(e) => {
                emit_state(&app, "error", Some(e.to_string()));
                // Settle back to idle so the bubble doesn't stay stuck.
                emit_state(&app, "idle", None);
            }
        }
    });
}

async fn run_pipeline(state: &Arc<SpokeState>) -> anyhow::Result<()> {
    let cfg = state.config_snapshot();
    let rec = state.audio.stop()?;

    // Optional raw audio save.
    if cfg.recording.save_audio {
        let dir = cfg.resolved_save_path();
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let path = dir.join(format!("spoke-{ts}.wav"));
        if let Err(e) = audio::save_wav(&path, &rec) {
            eprintln!("[spoke] failed to save audio: {e}");
        }
    }

    let mono = audio::to_mono(&rec.samples, rec.channels);
    if mono.is_empty() {
        return Ok(());
    }

    let engine = stt::SttEngine::from_config(&cfg)?;
    let transcript = engine
        .transcribe(&mono, rec.sample_rate, &cfg.general.language)
        .await?;

    let transcript = transcript.trim().to_string();
    if transcript.is_empty() {
        return Ok(());
    }

    // enigo is not Send; run it on a blocking thread.
    tokio::task::spawn_blocking(move || inject::inject_text(&transcript))
        .await
        .map_err(|e| anyhow::anyhow!("inject task panicked: {e}"))??;

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
    let config = Config::load().unwrap_or_default();

    // Audio amplitude channel: capture thread → UI events.
    let (amp_tx, amp_rx) = std::sync::mpsc::channel::<f32>();
    let audio = AudioEngine::spawn(amp_tx);

    let state = Arc::new(SpokeState {
        config: Mutex::new(config),
        audio,
        recording: AtomicBool::new(false),
    });

    tauri::Builder::default()
        .plugin(build_shortcut_plugin())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            get_config,
            set_config,
            start_recording,
            stop_recording,
            is_recording
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

            // Keep the bubble visible when the user switches Spaces on macOS.
            #[cfg(target_os = "macos")]
            if let Some(win) = handle.get_webview_window("bubble") {
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
