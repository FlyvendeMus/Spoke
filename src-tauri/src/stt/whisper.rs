//! Offline STT via whisper.cpp (whisper-rs bindings).
//!
//! Compiled only with the `whisper` cargo feature. The model file is expected
//! at `src-tauri/models/ggml-<model>.bin` (download separately — large). Audio
//! is resampled to 16 kHz mono, which whisper.cpp requires.

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
        let path = model_path(&cfg.offline.model);
        if !path.exists() {
            return Err(anyhow!(
                "whisper model not found at {} (download it first)",
                path.display()
            ));
        }
        let mut params = WhisperContextParameters::default();
        params.use_gpu(cfg.offline.use_gpu);
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
        let audio = audio::trim_silence(&audio, WHISPER_RATE);

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

        let segments = state.full_n_segments().map_err(|e| anyhow!("{e}"))?;
        let mut out = String::new();
        for i in 0..segments {
            if let Ok(text) = state.full_get_segment_text(i) {
                out.push_str(text.trim());
                out.push(' ');
            }
        }
        Ok(out.trim().to_string())
    }
}

/// Resolve the model file path relative to the crate's `models` directory.
fn model_path(model: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("models");
    dir.join(format!("ggml-{model}.bin"))
}
