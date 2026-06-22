//! Offline STT via whisper.cpp (whisper-rs bindings).
//!
//! Compiled only with the `whisper` cargo feature. The model file is expected
//! at `src-tauri/models/ggml-<model>.bin` or in the runtime config directory
//! (`~/.config/spoke/models/` on Linux, `~/Library/Application Support/spoke/models/`
//! on macOS). Audio is resampled to 16 kHz mono, which whisper.cpp requires.

use crate::audio;
use crate::config::Config;
use anyhow::{anyhow, Result};
use std::path::PathBuf;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

const WHISPER_RATE: u32 = 16_000;

pub struct WhisperStt {
    ctx: WhisperContext,
}

impl WhisperStt {
    pub fn from_config(cfg: &Config) -> Result<Self> {
        let path = resolve_model_path(&cfg.offline.model)
            .ok_or_else(|| anyhow!("whisper model '{}' not found (download it first)", cfg.offline.model))?;

        // Toggle the CoreML bundle so whisper.cpp selects the right backend.
        #[cfg(feature = "coreml")]
        {
            let bundle = coreml_bundle_path(&cfg.offline.model);
            let mut disabled = bundle.as_os_str().to_os_string();
            disabled.push(".disabled");
            let disabled = PathBuf::from(disabled);
            let use_coreml = cfg.offline.mac_accel == "coreml" || cfg.offline.mac_accel == "auto";

            if use_coreml && disabled.exists() {
                std::fs::rename(&disabled, &bundle)
                    .map_err(|e| anyhow!("failed to restore CoreML bundle: {e}"))?;
            } else if !use_coreml && bundle.exists() {
                std::fs::rename(&bundle, &disabled)
                    .map_err(|e| anyhow!("failed to disable CoreML bundle: {e}"))?;
            }
        }

        let mut params = WhisperContextParameters::default();
        params.use_gpu(wants_gpu(cfg));
        let ctx = WhisperContext::new_with_params(
            path.to_str().ok_or_else(|| anyhow!("bad model path"))?,
            params,
        )
        .map_err(|e| anyhow!("failed to load whisper model: {e}"))?;
        Ok(Self { ctx })
    }

    /// Synchronous (whisper.cpp is blocking); callers should run it off the
    /// async executor's core threads.
    pub fn transcribe(&self, mono: &[f32], sample_rate: u32, language: &str) -> Result<String> {
        let audio = audio::resample_mono(mono, sample_rate, WHISPER_RATE);
        let mut audio = audio::strip_internal_silence(&audio, WHISPER_RATE);
        // Whisper's decoder truncates the last segment when audio ends mid-speech.
        // Appending 500 ms of silence gives it room to finalize the last tokens.
        let pad = WHISPER_RATE as usize / 2;
        audio.extend(std::iter::repeat(0.0f32).take(pad));

        let mut state = self
            .ctx
            .create_state()
            .map_err(|e| anyhow!("whisper state: {e}"))?;

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(Some(if language.is_empty() { "auto" } else { language }));
        params.set_no_speech_thold(0.6);
        params.set_suppress_blank(true);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);

        state
            .full(params, &audio)
            .map_err(|e| anyhow!("whisper inference failed: {e}"))?;

        let segments = state.full_n_segments();
        let mut out = String::new();
        for i in 0..segments {
            if let Some(segment) = state.get_segment(i) {
                if let Ok(text) = segment.to_str() {
                    out.push_str(text.trim());
                    out.push(' ');
                }
            }
        }
        Ok(out.trim().to_string())
    }
}

/// Resolve whether GPU should be enabled given the current config and build.
/// On macOS, `mac_accel` takes precedence. "none" always disables GPU.
fn wants_gpu(cfg: &crate::config::Config) -> bool {
    #[cfg(target_os = "macos")]
    {
        cfg.offline.mac_accel != "none"
    }
    #[cfg(not(target_os = "macos"))]
    {
        cfg.offline.use_gpu
    }
}

/// Hugging Face URL for a whisper.cpp ggml model.
pub fn model_download_url(model: &str) -> String {
    format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-{model}.bin")
}

/// Hugging Face URL for the CoreML encoder bundle zip.
#[cfg(feature = "coreml")]
pub fn coreml_bundle_url(model: &str) -> String {
    format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-{model}-encoder.mlmodelc.zip")
}

/// Path where the CoreML bundle lives, co-located with the GGML model file.
/// Falls back to the runtime models dir if the GGML model hasn't been downloaded yet.
#[cfg(feature = "coreml")]
pub fn coreml_bundle_path(model: &str) -> PathBuf {
    let bundle_name = format!("ggml-{model}-encoder.mlmodelc");
    if let Some(model_path) = resolve_model_path(model) {
        if let Some(parent) = model_path.parent() {
            return parent.join(&bundle_name);
        }
    }
    models_dir().join(bundle_name)
}

#[cfg(feature = "coreml")]
pub fn coreml_bundle_exists(model: &str) -> bool {
    coreml_bundle_path(model).exists()
}

/// Runtime directory where downloaded models are stored (under the OS config dir).
pub fn models_dir() -> PathBuf {
    let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join("spoke").join("models")
}

/// Check whether the given model file exists in either the build or runtime dir.
pub fn model_exists(model: &str) -> bool {
    resolve_model_path(model).is_some()
}

/// Resolve the model path checking the build dir first, then the runtime dir.
pub fn resolve_model_path(model: &str) -> Option<PathBuf> {
    let name = format!("ggml-{model}.bin");
    let build = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("models").join(&name);
    if build.exists() {
        return Some(build);
    }
    let runtime = models_dir().join(&name);
    if runtime.exists() {
        return Some(runtime);
    }
    None
}
