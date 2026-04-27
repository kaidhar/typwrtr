//! Phase 5 streaming partials.
//!
//! Spawned by the recorder when capture begins. Every `partial_interval_ms`
//! it snapshots the audio buffer, runs whisper on it, and emits a partial
//! transcription event for the captions overlay. Also runs an energy-based
//! VAD; if the trailing silence exceeds `silence_threshold_ms` AND the user
//! has spoken, it asks the main app to auto-stop the recording.
//!
//! The task exits when `stop_signal` fires (set by `Recorder::stop_*`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter};

use crate::audio::{self, AudioRecorder};
use crate::transcribe_local::{LocalTranscriber, TranscribeOptions};
use crate::vad;

/// Configuration for one streaming session. Lifted from `Settings` at
/// `Recorder::start_recording` time so changes don't take effect mid-recording.
#[derive(Debug, Clone)]
pub struct StreamingConfig {
    /// Whisper model file (resolved path).
    pub model_path: std::path::PathBuf,
    /// Initial prompt to bias each partial inference.
    pub initial_prompt: String,
    pub language: String,
    /// How often to fire a partial inference (ms). 700 ms is a good trade-off
    /// between perceived snappiness and GPU load.
    pub partial_interval_ms: u64,
    /// Trailing-silence threshold for auto-finalize. `0` disables VAD stop.
    pub silence_threshold_ms: u64,
    /// RMS threshold below which a frame counts as silence. ~0.005 catches
    /// office-quiet without false-tripping on breaths.
    pub rms_threshold: f32,
    /// When false, skip the per-tick whisper inference and only run VAD.
    /// Set this from `settings.streaming_captions` so a VAD-only session
    /// (the default) doesn't pay an inference per 700 ms tick.
    pub emit_partials: bool,
}

#[derive(Clone)]
pub struct StreamingHandle {
    stop: Arc<AtomicBool>,
}

impl StreamingHandle {
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PartialPayload {
    pub text: String,
    pub elapsed_ms: u64,
}

/// Spawn the streaming inference loop. Returns a handle the recorder uses to
/// stop the task at finalize-time.
pub fn spawn(
    app: AppHandle,
    audio_recorder: Arc<std::sync::Mutex<AudioRecorder>>,
    local: Arc<LocalTranscriber>,
    config: StreamingConfig,
) -> StreamingHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = stop.clone();

    tauri::async_runtime::spawn(async move {
        let mut last_partial_text = String::new();
        let mut last_buffer_ms: u64 = 0;
        let started = std::time::Instant::now();

        loop {
            tokio::time::sleep(Duration::from_millis(config.partial_interval_ms)).await;
            if stop_clone.load(Ordering::Relaxed) {
                break;
            }

            // Snapshot the in-flight audio.
            let snapshot = {
                let rec = match audio_recorder.lock() {
                    Ok(r) => r,
                    Err(_) => break, // poisoned — bail
                };
                rec.peek_samples()
            };
            let Some(buf) = snapshot else { continue };

            let elapsed_ms = (buf.samples.len() as u64 * 1000)
                / (buf.sample_rate as u64 * buf.channels as u64).max(1);
            // Skip if no growth since last tick (silence padding only) — saves
            // a full inference when the user paused.
            if elapsed_ms == last_buffer_ms {
                continue;
            }
            last_buffer_ms = elapsed_ms;

            // Resample/mixdown happens on the blocking pool to keep the runtime
            // responsive. Same path the final transcription uses.
            let processed =
                match tokio::task::spawn_blocking(move || audio::to_whisper_format(buf)).await {
                    Ok(Ok(s)) => s,
                    Ok(Err(e)) => {
                        eprintln!("[typwrtr] streaming resample error: {}", e);
                        continue;
                    }
                    Err(e) => {
                        eprintln!("[typwrtr] streaming join error: {}", e);
                        continue;
                    }
                };

            // VAD finalize check — done on the resampled 16 kHz mono so the
            // RMS threshold makes sense regardless of the device sample rate.
            if config.silence_threshold_ms > 0
                && vad::has_speech(&processed, 30, config.rms_threshold)
            {
                let trailing = vad::trailing_silence_ms(&processed, 30, config.rms_threshold);
                if trailing >= config.silence_threshold_ms {
                    let _ = app.emit("recording://auto-stop", trailing);
                    break;
                }
            }

            // Skip the per-tick whisper inference when the user only wants
            // VAD auto-stop (captions off). Each inference is 100s of ms even
            // on GPU and seconds on CPU — pure waste when nothing in the UI
            // consumes the partial.
            if !config.emit_partials {
                continue;
            }

            // Run whisper on the snapshot. Reuses the persistent context from
            // §3.1 so this is a state-only allocation, not a model reload.
            let opts = TranscribeOptions {
                language: config.language.clone(),
                initial_prompt: config.initial_prompt.clone(),
                ..TranscribeOptions::default()
            };
            let model_path = config.model_path.clone();
            let local_clone = local.clone();
            let result = local_clone.transcribe(&model_path, processed, opts).await;

            match result {
                Ok(text) => {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() && trimmed != last_partial_text {
                        last_partial_text = trimmed.to_string();
                        let payload = PartialPayload {
                            text: trimmed.to_string(),
                            elapsed_ms: started.elapsed().as_millis() as u64,
                        };
                        let _ = app.emit("transcription://partial", &payload);
                    }
                }
                Err(e) => {
                    eprintln!("[typwrtr] streaming inference error: {}", e);
                }
            }
        }
    });

    StreamingHandle { stop }
}
