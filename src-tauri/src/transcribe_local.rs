use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

/// Logs which GPU backend whisper.cpp was compiled against. The Cargo features
/// on `whisper-rs` are wired in `Cargo.toml` per-target, so we infer the same
/// way here. whisper.cpp itself prints its actual device pick during model
/// load (e.g. `metal: device 'Apple M-series'`, `ggml_cuda_init: ...`) — that
/// log line is the runtime confirmation.
pub fn log_compiled_backend() {
    // Mirrors the per-target whisper-rs feature wiring in Cargo.toml. The
    // ground-truth runtime device pick is whatever whisper.cpp prints during
    // model load (e.g. `ggml_cuda_init: found N CUDA devices`).
    #[cfg(target_os = "macos")]
    let backend = "Metal";
    #[cfg(not(target_os = "macos"))]
    let backend = "CUDA";

    println!("[typwrtr] Whisper backend: {}", backend);
}

/// Long-lived whisper model. Loaded once, reused across dictations.
/// On settings change we reload only when the model path differs.
pub struct LocalTranscriber {
    inner: Mutex<Option<Loaded>>,
}

struct Loaded {
    path: PathBuf,
    ctx: Arc<WhisperContext>,
}

impl LocalTranscriber {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(None),
        }
    }

    /// Ensure the given model is loaded. Reloads if a different model is currently held.
    pub fn ensure_loaded(&self, model_path: &Path) -> Result<Arc<WhisperContext>, String> {
        if !model_path.exists() {
            return Err("Whisper model not found. Please download a model first.".to_string());
        }

        let mut guard = self.inner.lock().unwrap();
        if let Some(loaded) = guard.as_ref() {
            if loaded.path == model_path {
                return Ok(loaded.ctx.clone());
            }
        }

        let path_str = model_path.to_str().ok_or("Model path is not valid UTF-8")?;
        println!("[typwrtr] Loading whisper model: {}", path_str);
        let ctx = WhisperContext::new_with_params(path_str, WhisperContextParameters::default())
            .map_err(|e| format!("Failed to load whisper model: {}", e))?;
        let ctx = Arc::new(ctx);
        *guard = Some(Loaded {
            path: model_path.to_path_buf(),
            ctx: ctx.clone(),
        });
        Ok(ctx)
    }

    /// Transcribe 16kHz mono f32 samples. Runs CPU-bound inference on a blocking thread.
    pub async fn transcribe(
        &self,
        model_path: &Path,
        samples: Vec<f32>,
        opts: TranscribeOptions,
    ) -> Result<String, String> {
        let ctx = self.ensure_loaded(model_path)?;

        tokio::task::spawn_blocking(move || transcribe_blocking(ctx, samples, opts))
            .await
            .map_err(|e| format!("Transcription join error: {}", e))?
    }
}

/// Knobs for a single transcription. Designed to grow as Phase 2 (vocabulary
/// biasing) and §3 step 8 (streaming) come online — adding fields here will
/// not break existing call sites if they go through `TranscribeOptions::default`.
#[derive(Debug, Clone)]
pub struct TranscribeOptions {
    /// ISO-639-1 code, or `"auto"` to let whisper detect.
    pub language: String,
    /// Free-form text fed to whisper as the initial prompt — biases decoding
    /// toward this vocabulary. Empty string means no biasing.
    pub initial_prompt: String,
    /// CPU threads. `None` = auto-detect.
    pub threads: Option<i32>,
    /// Beam-of-N for `BeamSearch`; `1` = greedy.
    pub beam_size: i32,
}

impl Default for TranscribeOptions {
    fn default() -> Self {
        Self {
            language: "en".to_string(),
            initial_prompt: String::new(),
            threads: None,
            beam_size: 1,
        }
    }
}

fn transcribe_blocking(
    ctx: Arc<WhisperContext>,
    samples: Vec<f32>,
    opts: TranscribeOptions,
) -> Result<String, String> {
    let mut state = ctx
        .create_state()
        .map_err(|e| format!("Failed to create whisper state: {}", e))?;

    let threads = opts.threads.unwrap_or_else(|| {
        std::thread::available_parallelism()
            .map(|n| n.get() as i32)
            .unwrap_or(4)
    });

    let strategy = if opts.beam_size > 1 {
        SamplingStrategy::BeamSearch {
            beam_size: opts.beam_size,
            patience: 0.0,
        }
    } else {
        SamplingStrategy::Greedy { best_of: 1 }
    };

    let mut params = FullParams::new(strategy);
    params.set_n_threads(threads);

    let language_for_whisper: Option<&str> = match opts.language.as_str() {
        "" | "auto" => None,
        other => Some(other),
    };
    params.set_language(language_for_whisper);

    if !opts.initial_prompt.is_empty() {
        params.set_initial_prompt(&opts.initial_prompt);
    }

    params.set_translate(false);
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_suppress_blank(true);

    state
        .full(params, &samples)
        .map_err(|e| format!("whisper full inference failed: {}", e))?;

    let n = state.full_n_segments();
    let mut text = String::new();
    for i in 0..n {
        let seg = state
            .get_segment(i)
            .ok_or_else(|| format!("missing whisper segment {}", i))?;
        let chunk = seg
            .to_str_lossy()
            .map_err(|e| format!("segment {} to_str_lossy: {}", i, e))?;
        text.push_str(&chunk);
    }
    Ok(text.trim().to_string())
}

pub fn model_filename(model_size: &str) -> String {
    format!("ggml-{}.bin", model_size)
}

pub fn model_download_url(model_size: &str) -> String {
    format!(
        "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-{}.bin",
        model_size
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_filename() {
        assert_eq!(model_filename("small"), "ggml-small.bin");
        assert_eq!(model_filename("medium"), "ggml-medium.bin");
    }

    #[test]
    fn test_model_download_url() {
        assert_eq!(
            model_download_url("small"),
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.bin"
        );
    }

    #[test]
    fn test_ensure_loaded_missing_model() {
        let tx = LocalTranscriber::new();
        let err = tx
            .ensure_loaded(&PathBuf::from("/nonexistent/model.bin"))
            .unwrap_err();
        assert!(err.contains("model not found"));
    }
}
