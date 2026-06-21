//! Online STT via Google Cloud Speech-to-Text v1 `speech:recognize`.
//!
//! The v1 endpoint accepts a simple API key (`?key=`), unlike v2/Chirp which
//! requires OAuth. Audio is sent as base64 LINEAR16 (16-bit PCM mono) in a
//! single batch request — no streaming. The full transcript comes back at once.

use crate::audio;
use anyhow::{anyhow, Result};
use base64::Engine as _;
use serde::{Deserialize, Serialize};

const ENDPOINT: &str = "https://speech.googleapis.com/v1/speech:recognize";

pub struct GoogleStt {
    api_key: String,
    client: reqwest::Client,
}

impl GoogleStt {
    pub fn new(api_key: String) -> Result<Self> {
        if api_key.trim().is_empty() {
            return Err(anyhow!("Google API key is not set (online mode)"));
        }
        Ok(Self {
            api_key,
            client: reqwest::Client::new(),
        })
    }

    pub async fn transcribe(
        &self,
        mono: &[f32],
        sample_rate: u32,
        language: &str,
    ) -> Result<String> {
        let stripped = audio::strip_internal_silence(mono, sample_rate);
        let mono = if stripped.is_empty() { mono } else { &stripped };
        let pcm = audio::mono_to_pcm16_le(mono);
        let content = base64::engine::general_purpose::STANDARD.encode(&pcm);

        let body = RecognizeRequest {
            config: RecognitionConfig {
                encoding: "LINEAR16",
                sample_rate_hertz: sample_rate,
                language_code: to_bcp47(language),
                enable_automatic_punctuation: true,
            },
            audio: RecognitionAudio { content },
        };

        let resp = self
            .client
            .post(ENDPOINT)
            .query(&[("key", self.api_key.as_str())])
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("Google STT HTTP {status}: {text}"));
        }

        let parsed: RecognizeResponse = resp.json().await?;
        Ok(parsed.best_transcript())
    }
}

/// Map a config language ("auto" | "en" | "da" | full BCP-47) to a code the
/// Google API accepts. v1 has no true auto-detect, so "auto" defaults to en-US.
fn to_bcp47(language: &str) -> String {
    match language {
        "auto" | "" => "en-US".to_string(),
        "en" => "en-US".to_string(),
        "da" => "da-DK".to_string(),
        "de" => "de-DE".to_string(),
        "es" => "es-ES".to_string(),
        "fr" => "fr-FR".to_string(),
        // Already a region-qualified tag (e.g. "en-GB") — pass through.
        other => other.to_string(),
    }
}

#[derive(Serialize)]
struct RecognizeRequest {
    config: RecognitionConfig,
    audio: RecognitionAudio,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RecognitionConfig {
    encoding: &'static str,
    sample_rate_hertz: u32,
    language_code: String,
    enable_automatic_punctuation: bool,
}

#[derive(Serialize)]
struct RecognitionAudio {
    content: String,
}

#[derive(Deserialize, Default)]
struct RecognizeResponse {
    #[serde(default)]
    results: Vec<SpeechResult>,
}

impl RecognizeResponse {
    /// Concatenate the top alternative of each result segment.
    fn best_transcript(&self) -> String {
        self.results
            .iter()
            .filter_map(|r| r.alternatives.first())
            .map(|a| a.transcript.trim())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[derive(Deserialize)]
struct SpeechResult {
    #[serde(default)]
    alternatives: Vec<Alternative>,
}

#[derive(Deserialize)]
struct Alternative {
    #[serde(default)]
    transcript: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_key_rejected() {
        assert!(GoogleStt::new("   ".into()).is_err());
        assert!(GoogleStt::new("abc".into()).is_ok());
    }

    #[test]
    fn language_mapping() {
        assert_eq!(to_bcp47("auto"), "en-US");
        assert_eq!(to_bcp47("da"), "da-DK");
        assert_eq!(to_bcp47("en-GB"), "en-GB");
    }

    #[test]
    fn transcript_joins_segments() {
        let json = r#"{"results":[
            {"alternatives":[{"transcript":"hello"}]},
            {"alternatives":[{"transcript":" world"}]}
        ]}"#;
        let r: RecognizeResponse = serde_json::from_str(json).unwrap();
        assert_eq!(r.best_transcript(), "hello world");
    }

    #[test]
    fn empty_results_yields_empty_string() {
        let r: RecognizeResponse = serde_json::from_str("{}").unwrap();
        assert_eq!(r.best_transcript(), "");
    }
}
