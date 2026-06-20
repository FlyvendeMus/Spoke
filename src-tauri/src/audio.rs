//! Microphone capture via cpal.
//!
//! cpal's `Stream` is not `Send`, so the stream lives on a dedicated OS thread
//! that owns it for its whole lifetime. The rest of the app talks to that
//! thread over channels: send `Cmd::Start` to open the mic, `Cmd::Stop` to
//! close it and receive the captured PCM. While recording, per-buffer RMS
//! amplitude is pushed to `amp_tx` so the UI can animate the bubble ring.

use anyhow::{anyhow, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SampleFormat;
use std::sync::mpsc::{Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex};

/// Raw captured audio: interleaved `f32` samples at the device's native rate.
#[derive(Debug, Clone)]
pub struct Recording {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub channels: u16,
}

enum Cmd {
    Start,
    /// Stop and send the captured recording back.
    Stop(SyncSender<Result<Recording>>),
}

/// Handle to the audio capture thread.
pub struct AudioEngine {
    tx: Sender<Cmd>,
}

impl AudioEngine {
    /// Spawn the capture thread. `amp_tx` receives an RMS amplitude in `0.0..=1.0`
    /// for every captured buffer while recording is active.
    pub fn spawn(amp_tx: Sender<f32>) -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<Cmd>();
        std::thread::Builder::new()
            .name("spoke-audio".into())
            .spawn(move || audio_thread(rx, amp_tx))
            .expect("spawn audio thread");
        Self { tx }
    }

    pub fn start(&self) -> Result<()> {
        self.tx.send(Cmd::Start).map_err(|_| anyhow!("audio thread gone"))
    }

    /// Stop recording and return the captured PCM (blocks until the thread replies).
    pub fn stop(&self) -> Result<Recording> {
        let (reply_tx, reply_rx) = std::sync::mpsc::sync_channel(1);
        self.tx
            .send(Cmd::Stop(reply_tx))
            .map_err(|_| anyhow!("audio thread gone"))?;
        reply_rx.recv().map_err(|_| anyhow!("audio thread dropped reply"))?
    }
}

fn audio_thread(rx: Receiver<Cmd>, amp_tx: Sender<f32>) {
    // Active capture state: the live stream plus the shared sample buffer.
    let mut active: Option<(cpal::Stream, Arc<Mutex<Vec<f32>>>, u32, u16)> = None;

    while let Ok(cmd) = rx.recv() {
        match cmd {
            Cmd::Start => {
                if active.is_some() {
                    continue; // already recording
                }
                match open_stream(amp_tx.clone()) {
                    Ok(state) => {
                        if let Err(e) = state.0.play() {
                            eprintln!("[audio] failed to start stream: {e}");
                        } else {
                            active = Some(state);
                        }
                    }
                    Err(e) => eprintln!("[audio] failed to open input: {e}"),
                }
            }
            Cmd::Stop(reply) => {
                let result = match active.take() {
                    Some((stream, buf, sample_rate, channels)) => {
                        drop(stream); // stop capturing
                        let samples = std::mem::take(&mut *buf.lock().unwrap());
                        Ok(Recording {
                            samples,
                            sample_rate,
                            channels,
                        })
                    }
                    None => Err(anyhow!("not recording")),
                };
                let _ = reply.send(result);
            }
        }
    }
}

fn open_stream(
    amp_tx: Sender<f32>,
) -> Result<(cpal::Stream, Arc<Mutex<Vec<f32>>>, u32, u16)> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| anyhow!("no input device"))?;
    let config = device.default_input_config()?;
    let sample_rate = config.sample_rate().0;
    let channels = config.channels();
    let buffer = Arc::new(Mutex::new(Vec::<f32>::new()));

    let err_fn = |e| eprintln!("[audio] stream error: {e}");

    // One callback factory regardless of the device's native sample format:
    // everything is converted to f32 and appended to the shared buffer.
    let buf = buffer.clone();
    let push = move |data: &[f32]| {
        // RMS amplitude for the UI ring.
        if !data.is_empty() {
            let sum_sq: f32 = data.iter().map(|s| s * s).sum();
            let rms = (sum_sq / data.len() as f32).sqrt();
            let _ = amp_tx.send(rms.clamp(0.0, 1.0));
        }
        buf.lock().unwrap().extend_from_slice(data);
    };

    let stream = match config.sample_format() {
        SampleFormat::F32 => {
            let push = push;
            device.build_input_stream(
                &config.into(),
                move |d: &[f32], _| push(d),
                err_fn,
                None,
            )?
        }
        SampleFormat::I16 => {
            let push = push;
            device.build_input_stream(
                &config.into(),
                move |d: &[i16], _| {
                    let f: Vec<f32> = d.iter().map(|&s| s as f32 / i16::MAX as f32).collect();
                    push(&f)
                },
                err_fn,
                None,
            )?
        }
        SampleFormat::U16 => {
            let push = push;
            device.build_input_stream(
                &config.into(),
                move |d: &[u16], _| {
                    let f: Vec<f32> = d
                        .iter()
                        .map(|&s| (s as f32 / u16::MAX as f32) * 2.0 - 1.0)
                        .collect();
                    push(&f)
                },
                err_fn,
                None,
            )?
        }
        other => return Err(anyhow!("unsupported sample format: {other:?}")),
    };

    Ok((stream, buffer, sample_rate, channels))
}

/// Downmix interleaved audio to a single mono channel by averaging.
pub fn to_mono(samples: &[f32], channels: u16) -> Vec<f32> {
    if channels <= 1 {
        return samples.to_vec();
    }
    let ch = channels as usize;
    samples
        .chunks(ch)
        .map(|frame| frame.iter().sum::<f32>() / ch as f32)
        .collect()
}

/// Resample mono audio to `target_rate` with linear interpolation.
/// Whisper requires 16 kHz mono. (Only used by the `whisper` feature + tests.)
#[allow(dead_code)]
pub fn resample_mono(mono: &[f32], from_rate: u32, target_rate: u32) -> Vec<f32> {
    if from_rate == target_rate || mono.is_empty() {
        return mono.to_vec();
    }
    let ratio = target_rate as f64 / from_rate as f64;
    let out_len = ((mono.len() as f64) * ratio).round() as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src = i as f64 / ratio;
        let idx = src.floor() as usize;
        let frac = (src - idx as f64) as f32;
        let a = mono[idx.min(mono.len() - 1)];
        let b = mono[(idx + 1).min(mono.len() - 1)];
        out.push(a + (b - a) * frac);
    }
    out
}

/// Trim leading and trailing silence from mono audio using an RMS energy threshold.
/// Frames with RMS ≤ 0.01 are considered silence. Operates in 30 ms windows to
/// avoid cutting off plosives or very brief pauses.
pub fn trim_silence(mono: &[f32], sample_rate: u32) -> Vec<f32> {
    let threshold = 0.01;
    let frame = (sample_rate as usize * 30) / 1000; // 30 ms
    if mono.len() < frame * 2 {
        return mono.to_vec();
    }

    let rms = |chunk: &[f32]| -> f32 {
        let sum_sq: f32 = chunk.iter().map(|s| s * s).sum();
        (sum_sq / chunk.len() as f32).sqrt()
    };

    let start = mono
        .chunks(frame)
        .position(|c| rms(c) > threshold)
        .unwrap_or(0)
        * frame;

    let end = {
        let mut pos = mono.len();
        for c in mono.rchunks(frame) {
            if rms(c) > threshold {
                break;
            }
            pos = pos.saturating_sub(c.len());
        }
        pos
    };

    if start < end {
        mono[start..end].to_vec()
    } else {
        mono.to_vec()
    }
}

/// Convert mono `f32` (`-1.0..=1.0`) to little-endian 16-bit PCM bytes.
pub fn mono_to_pcm16_le(mono: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(mono.len() * 2);
    for &s in mono {
        let v = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    bytes
}

/// Write a recording to a 16-bit PCM WAV file.
pub fn save_wav(path: &std::path::Path, rec: &Recording) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let spec = hound::WavSpec {
        channels: rec.channels,
        sample_rate: rec.sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec)?;
    for &s in &rec.samples {
        writer.write_sample((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)?;
    }
    writer.finalize()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mono_averages_stereo() {
        let stereo = [1.0, 0.0, 0.5, 0.5]; // 2 frames, 2 channels
        assert_eq!(to_mono(&stereo, 2), vec![0.5, 0.5]);
    }

    #[test]
    fn mono_passthrough_when_single_channel() {
        let s = [0.1, 0.2, 0.3];
        assert_eq!(to_mono(&s, 1), s.to_vec());
    }

    #[test]
    fn resample_noop_when_rates_match() {
        let s = vec![0.1, 0.2, 0.3];
        assert_eq!(resample_mono(&s, 16000, 16000), s);
    }

    #[test]
    fn resample_halves_length_when_downsampling_2x() {
        let s: Vec<f32> = (0..100).map(|i| i as f32).collect();
        let out = resample_mono(&s, 32000, 16000);
        assert_eq!(out.len(), 50);
    }

    #[test]
    fn pcm16_encodes_full_scale() {
        let bytes = mono_to_pcm16_le(&[1.0, -1.0, 0.0]);
        assert_eq!(bytes.len(), 6);
        assert_eq!(i16::from_le_bytes([bytes[0], bytes[1]]), i16::MAX);
        assert_eq!(i16::from_le_bytes([bytes[2], bytes[3]]), -i16::MAX);
        assert_eq!(i16::from_le_bytes([bytes[4], bytes[5]]), 0);
    }
}
