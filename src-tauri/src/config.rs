//! Configuration: the single `spoke.toml` file in the OS config dir.
//!
//! Mirrors the schema documented in SPOKE.md. Every field has a default so a
//! missing or partial file still produces a usable config.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Offline,
    Online,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    PushToTalk,
    Toggle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioFormat {
    Wav,
    Flac,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct General {
    pub mode: Mode,
    pub hotkey: String,
    pub trigger: Trigger,
    /// "auto" | BCP-47 code ("en", "da", ...).
    pub language: String,
    /// When true, copy transcribed text to clipboard instead of injecting.
    pub copy_to_clipboard: bool,
}

/// Platform-appropriate default hotkey: Cmd+Shift+S on macOS, Ctrl+Alt+Space elsewhere.
fn default_hotkey() -> String {
    if cfg!(target_os = "macos") {
        "cmd+shift+s".into()
    } else {
        "ctrl+alt+space".into()
    }
}

impl Default for General {
    fn default() -> Self {
        Self {
            mode: Mode::Offline,
            hotkey: default_hotkey(),
            trigger: Trigger::PushToTalk,
            language: "auto".into(),
            copy_to_clipboard: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Offline {
    /// "tiny" | "base" | "small" | "large-v3-turbo"
    pub model: String,
    pub use_gpu: bool,
    /// macOS acceleration: "auto" | "metal" | "coreml" | "none".
    /// Ignored on non-macOS (use_gpu governs instead).
    pub mac_accel: String,
}

impl Default for Offline {
    fn default() -> Self {
        Self {
            model: "large-v3-turbo".into(),
            use_gpu: true,
            mac_accel: "auto".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Online {
    /// "google" | "openai"
    pub provider: String,
    /// Stored in the system keychain in production; kept here for development.
    pub api_key: String,
}

impl Default for Online {
    fn default() -> Self {
        Self {
            provider: "google".into(),
            api_key: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Recording {
    pub save_audio: bool,
    pub save_path: String,
    pub format: AudioFormat,
    /// When true, save the processed (mono, silence-stripped, 16kHz) audio instead
    /// of the raw multi-channel recording.
    pub save_processed: bool,
    /// Name of the input device to use. Empty means default.
    pub input_device: String,
}

impl Default for Recording {
    fn default() -> Self {
        Self {
            save_audio: false,
            save_path: "~/Documents/Spoke".into(),
            format: AudioFormat::Wav,
            save_processed: false,
            input_device: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Ui {
    pub bubble_position: String,
    pub bubble_opacity_idle: f32,
}

impl Default for Ui {
    fn default() -> Self {
        Self {
            bubble_position: "bottom-right".into(),
            bubble_opacity_idle: 0.4,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub general: General,
    pub offline: Offline,
    pub online: Online,
    pub recording: Recording,
    pub ui: Ui,
}

impl Config {
    /// Path to `spoke.toml` in the OS config dir, e.g.
    /// `~/.config/spoke/spoke.toml` on Linux, `~/Library/Application Support`
    /// on macOS, `%APPDATA%` on Windows.
    pub fn path() -> PathBuf {
        let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
        base.join("spoke").join("spoke.toml")
    }

    /// Load from the default path, falling back to defaults if absent or
    /// unreadable. A parse error is returned so the caller can surface it.
    pub fn load() -> anyhow::Result<Self> {
        Self::load_from(&Self::path())
    }

    pub fn load_from(path: &Path) -> anyhow::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(toml::from_str(&text)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&self) -> anyhow::Result<()> {
        self.save_to(&Self::path())
    }

    pub fn save_to(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self)?;
        std::fs::write(path, text)?;
        Ok(())
    }

    /// Expand a leading `~` in the recording save path to the home dir.
    pub fn resolved_save_path(&self) -> PathBuf {
        expand_tilde(&self.recording.save_path)
    }
}

fn expand_tilde(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_spec() {
        let c = Config::default();
        assert_eq!(c.general.mode, Mode::Offline);
        assert_eq!(c.general.hotkey, default_hotkey());
        assert_eq!(c.general.trigger, Trigger::PushToTalk);
        assert_eq!(c.general.language, "auto");
        assert_eq!(c.offline.model, "large-v3-turbo");
        assert!(c.offline.use_gpu);
        assert_eq!(c.online.provider, "google");
        assert!(!c.recording.save_audio);
        assert_eq!(c.recording.format, AudioFormat::Wav);
        assert_eq!(c.ui.bubble_position, "bottom-right");
        assert!((c.ui.bubble_opacity_idle - 0.4).abs() < f32::EPSILON);
    }

    #[test]
    fn round_trip_through_toml() {
        let mut c = Config::default();
        c.general.mode = Mode::Online;
        c.online.api_key = "secret".into();
        let dir = std::env::temp_dir().join(format!("spoke-test-{}", std::process::id()));
        let path = dir.join("spoke.toml");
        c.save_to(&path).unwrap();
        let loaded = Config::load_from(&path).unwrap();
        assert_eq!(loaded.general.mode, Mode::Online);
        assert_eq!(loaded.online.api_key, "secret");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_file_yields_defaults() {
        let path = std::env::temp_dir().join("spoke-does-not-exist-xyz.toml");
        let c = Config::load_from(&path).unwrap();
        assert_eq!(c.general.mode, Mode::Offline);
    }

    #[test]
    fn partial_toml_fills_defaults() {
        let toml = "[general]\nmode = \"online\"\n";
        let c: Config = toml::from_str(toml).unwrap();
        assert_eq!(c.general.mode, Mode::Online);
        // Untouched fields keep their defaults.
        assert_eq!(c.general.hotkey, default_hotkey());
        assert_eq!(c.offline.model, "large-v3-turbo");
    }

    #[test]
    fn tilde_expands() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(expand_tilde("~/Documents/Spoke"), home.join("Documents/Spoke"));
        assert_eq!(expand_tilde("/abs/path"), PathBuf::from("/abs/path"));
    }
}
