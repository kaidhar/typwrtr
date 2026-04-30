//! Whisper backend via the `whisper-rs` crate. Long-lived `WhisperContext`
//! kept in a Mutex so the model loads once and is reused across dictations
//! and streaming partials. Per-token logprobs are extracted for the
//! confidence-gated stages downstream.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use super::{TranscribeOptions, TranscribeResult, Transcriber};

/// Logs which GPU backend whisper.cpp was compiled against. The Cargo
/// features on `whisper-rs` are wired in `Cargo.toml` per-target;
/// whisper.cpp prints its actual device pick during model load
/// (e.g. `ggml_cuda_init: found 1 CUDA devices`).
pub fn log_compiled_backend() {
    #[cfg(target_os = "macos")]
    let backend = "Metal";
    #[cfg(all(not(target_os = "macos"), feature = "cuda"))]
    let backend = "CUDA";
    #[cfg(all(not(target_os = "macos"), not(feature = "cuda")))]
    let backend = "CPU";

    println!("[typwrtr] Whisper backend: {}", backend);
}

/// Long-lived whisper model. Loaded once, reused across dictations.
/// On settings change we reload only when the model path differs.
pub struct WhisperTranscriber {
    inner: Mutex<Option<Loaded>>,
}

struct Loaded {
    path: PathBuf,
    ctx: Arc<WhisperContext>,
}

impl WhisperTranscriber {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(None),
        }
    }

    fn cached_ctx(&self) -> Option<Arc<WhisperContext>> {
        self.inner.lock().unwrap().as_ref().map(|l| l.ctx.clone())
    }
}

impl Default for WhisperTranscriber {
    fn default() -> Self {
        Self::new()
    }
}

impl Transcriber for WhisperTranscriber {
    fn ensure_loaded(&self, model_path: &Path) -> Result<(), String> {
        if !model_path.exists() {
            return Err("Whisper model not found. Please download a model first.".to_string());
        }

        let mut guard = self.inner.lock().unwrap();
        if let Some(loaded) = guard.as_ref() {
            if loaded.path == model_path {
                return Ok(());
            }
        }

        let path_str = model_path.to_str().ok_or("Model path is not valid UTF-8")?;
        println!("[typwrtr] Loading whisper model: {}", path_str);
        let ctx = WhisperContext::new_with_params(path_str, WhisperContextParameters::default())
            .map_err(|e| format!("Failed to load whisper model: {}", e))?;
        *guard = Some(Loaded {
            path: model_path.to_path_buf(),
            ctx: Arc::new(ctx),
        });
        Ok(())
    }

    fn transcribe_blocking(
        &self,
        samples: Vec<f32>,
        opts: TranscribeOptions,
    ) -> Result<TranscribeResult, String> {
        let ctx = self
            .cached_ctx()
            .ok_or_else(|| "Whisper model not loaded. Call ensure_loaded first.".to_string())?;
        run_whisper(ctx, samples, opts)
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn supports_initial_prompt(&self) -> bool {
        true
    }
}

fn run_whisper(
    ctx: Arc<WhisperContext>,
    samples: Vec<f32>,
    opts: TranscribeOptions,
) -> Result<TranscribeResult, String> {
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
    // Hallucination guardrails. Lower no_speech_thold makes the silence
    // detector more eager to drop confidently-wrong outputs on quiet frames
    // (default 0.6 → 0.3). suppress_nst kills the model's non-speech tokens
    // (the source of `[Music]`, `♪`, and a chunk of canonical hallucinations).
    params.set_no_speech_thold(0.3);
    params.set_suppress_nst(true);

    state
        .full(params, &samples)
        .map_err(|e| format!("whisper full inference failed: {}", e))?;

    let n = state.full_n_segments();
    let mut text = String::new();
    let mut sum_plog = 0.0_f32;
    let mut tok_count = 0_usize;
    for i in 0..n {
        let seg = state
            .get_segment(i)
            .ok_or_else(|| format!("missing whisper segment {}", i))?;
        let chunk = seg
            .to_str_lossy()
            .map_err(|e| format!("segment {} to_str_lossy: {}", i, e))?;
        text.push_str(&chunk);

        let n_tok = seg.n_tokens();
        for j in 0..n_tok {
            if let Some(tok) = seg.get_token(j) {
                let td = tok.token_data();
                if td.id >= 50256 || td.plog == 0.0 {
                    continue;
                }
                sum_plog += td.plog;
                tok_count += 1;
            }
        }
    }
    let avg_logprob = if tok_count > 0 {
        sum_plog / tok_count as f32
    } else {
        0.0
    };
    Ok(TranscribeResult {
        text: text.trim().to_string(),
        avg_logprob,
    })
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
        let tx = WhisperTranscriber::new();
        let err = tx
            .ensure_loaded(&PathBuf::from("/nonexistent/model.bin"))
            .unwrap_err();
        assert!(err.contains("model not found"));
    }
}
