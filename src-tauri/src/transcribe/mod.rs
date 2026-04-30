//! ASR engine abstraction. The dictation pipeline doesn't care which model
//! produces the text — it just hands raw 16 kHz mono f32 samples to a
//! [`Transcriber`] and pastes whatever comes back. Today only the whisper
//! backend is implemented; the [`parakeet`] module is a stub kept around
//! to make adding a second engine a small, mechanical follow-up.
//!
//! Streaming captions and `initial_prompt` biasing are engine-specific
//! capabilities; callers gate on [`Transcriber::supports_streaming`] and
//! [`Transcriber::supports_initial_prompt`] before relying on either.

use std::path::Path;
use std::sync::Arc;

pub mod parakeet;
pub mod whisper;

pub use parakeet::ParakeetTranscriber;
pub use whisper::WhisperTranscriber;

/// Logs which GPU backend whisper.cpp was compiled against. Re-exported
/// from the whisper module for the startup banner so call sites don't
/// have to name the concrete impl.
pub fn log_compiled_backend() {
    whisper::log_compiled_backend();
}

/// Decoded text plus a confidence proxy. `avg_logprob` is the mean per-token
/// log-prob across all segments; closer to 0 = more confident. Values around
/// -0.3 are clean utterances; below -0.7 are usually noisy / hallucinated.
/// `0.0` means the engine produced no scorable tokens (treat as "no signal",
/// not "perfect"). Engines without per-token logprob (Parakeet) always
/// return `0.0` so consumers can use a single sentinel rule.
#[derive(Debug, Clone)]
pub struct TranscribeResult {
    pub text: String,
    pub avg_logprob: f32,
}

/// Knobs for a single transcription. Some fields are whisper-specific
/// (`beam_size`, `initial_prompt`); engines that don't honour them
/// silently ignore. Callers should not depend on those fields having an
/// effect — the recorder reads the relevant `supports_*` flag first when
/// it matters.
#[derive(Debug, Clone)]
pub struct TranscribeOptions {
    /// ISO-639-1 code, or `"auto"` to let the engine detect.
    pub language: String,
    /// Free-form text fed to the engine as the initial prompt — biases
    /// decoding toward this vocabulary. Empty string = no biasing.
    /// Ignored by engines without prompt biasing (Parakeet).
    pub initial_prompt: String,
    /// CPU threads. `None` = auto-detect.
    pub threads: Option<i32>,
    /// Beam-of-N for whisper's `BeamSearch`; `1` = greedy. Ignored by
    /// non-whisper engines.
    pub beam_size: i32,
}

impl Default for TranscribeOptions {
    fn default() -> Self {
        Self {
            language: "en".to_string(),
            initial_prompt: String::new(),
            threads: None,
            // Beam search (size 5, no patience) cuts WER 5–15 % over greedy
            // on proper nouns and out-of-distribution tokens. CUDA decode of
            // a 5 s utterance stays under ~300 ms even at this beam.
            beam_size: 5,
        }
    }
}

/// One ASR engine. Held behind `Arc<dyn Transcriber>` by the recorder so
/// settings changes can swap engines without touching call sites. Trait
/// methods are synchronous; the async wrapper at module level pushes
/// `transcribe_blocking` onto the tokio blocking pool.
pub trait Transcriber: Send + Sync {
    /// Load the model at `model_path` if it isn't already, or swap to it
    /// if a different model is currently held. Returns `Ok(())` on
    /// success; the engine is then ready for `transcribe_blocking`.
    fn ensure_loaded(&self, model_path: &Path) -> Result<(), String>;

    /// Run inference against the currently-loaded model. Caller must have
    /// invoked `ensure_loaded` first.
    fn transcribe_blocking(
        &self,
        samples: Vec<f32>,
        opts: TranscribeOptions,
    ) -> Result<TranscribeResult, String>;

    /// `true` when the engine can run cheaply enough at a 700 ms cadence
    /// for the streaming-captions path. Whisper-rs reuses persistent
    /// context state and qualifies; Parakeet today does not.
    fn supports_streaming(&self) -> bool {
        false
    }

    /// `true` when `TranscribeOptions::initial_prompt` actually influences
    /// decoding. Whisper-rs honours it; Parakeet ignores it.
    fn supports_initial_prompt(&self) -> bool {
        false
    }
}

/// Async wrapper: run inference on the blocking pool so the tokio runtime
/// stays responsive for streaming + UI events.
pub async fn transcribe(
    engine: Arc<dyn Transcriber>,
    model_path: std::path::PathBuf,
    samples: Vec<f32>,
    opts: TranscribeOptions,
) -> Result<TranscribeResult, String> {
    tokio::task::spawn_blocking(move || {
        engine.ensure_loaded(&model_path)?;
        engine.transcribe_blocking(samples, opts)
    })
    .await
    .map_err(|e| format!("Transcription join error: {}", e))?
}

/// Filename / on-disk path tail for the given model id. Whisper models
/// land at `ggml-<id>.bin`; Parakeet TDT lands at `<id>/` (directory of
/// ONNX files + tokenizer + vocab). The recorder + downloader resolve
/// either with the same join expression.
pub fn model_filename(model_id: &str) -> String {
    match crate::settings::engine_for_model(model_id) {
        "parakeet" => parakeet::model_filename(model_id),
        _ => whisper::model_filename(model_id),
    }
}

/// Hugging Face download URL for the model id.
pub fn model_download_url(model_id: &str) -> String {
    match crate::settings::engine_for_model(model_id) {
        "parakeet" => parakeet::model_download_url(model_id),
        _ => whisper::model_download_url(model_id),
    }
}
