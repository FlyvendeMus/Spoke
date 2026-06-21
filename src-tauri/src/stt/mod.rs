//! Speech-to-text engines.
//!
//! Two backends behind one enum so the core pipeline doesn't care which is
//! active: `Google` (online, REST) and `Whisper` (offline, feature-gated
//! because it pulls in a heavy native build).

mod google;
#[cfg(feature = "whisper")]
mod whisper;

pub use google::GoogleStt;

use crate::config::{Config, Mode};
use anyhow::Result;

/// A ready-to-use transcription backend.
pub enum SttEngine {
    Google(GoogleStt),
    #[cfg(feature = "whisper")]
    Whisper(whisper::WhisperStt),
}

impl SttEngine {
    /// Build the engine the config selects. Offline mode needs the `whisper`
    /// cargo feature; without it we fail loudly rather than silently degrade.
    pub fn from_config(cfg: &Config) -> Result<Self> {
        match cfg.general.mode {
            Mode::Online => Ok(SttEngine::Google(GoogleStt::new(cfg.online.api_key.clone())?)),
            Mode::Offline => {
                #[cfg(feature = "whisper")]
                {
                    Ok(SttEngine::Whisper(whisper::WhisperStt::from_config(cfg)?))
                }
                #[cfg(not(feature = "whisper"))]
                {
                    let _ = cfg;
                    Err(anyhow::anyhow!(
                        "offline mode requires building with `--features whisper`"
                    ))
                }
            }
        }
    }

    /// Transcribe mono `f32` audio. `sample_rate` is the rate of `mono`;
    /// `language` is "auto" or a BCP-47-ish code from the config.
    pub async fn transcribe(
        &self,
        mono: &[f32],
        sample_rate: u32,
        language: &str,
    ) -> Result<String> {
        match self {
            SttEngine::Google(g) => g.transcribe(mono, sample_rate, language).await,
            #[cfg(feature = "whisper")]
            SttEngine::Whisper(w) => w.transcribe(mono, sample_rate, language),
        }
    }
}
