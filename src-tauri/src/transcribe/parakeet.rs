//! Parakeet TDT 0.6B v2 backend via the `parakeet-rs` crate (ONNX Runtime).
//!
//! Compiled into the binary only when the `parakeet` Cargo feature is on —
//! the CPU portable Windows artifact enables it; the CUDA artifact does
//! not, because GPU users already have whisper-large-v3-turbo and the
//! extra ~50 MB of ONNX Runtime native libs would bloat the build for no
//! perceptible win.
//!
//! When the feature is off, `ParakeetTranscriber` is a stub that returns
//! "engine not enabled in this build" errors so the trait surface stays
//! identical across feature flavours.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::{TranscribeOptions, TranscribeResult, Transcriber};

/// Long-lived Parakeet model. Loaded once via `Parakeet::from_pretrained`
/// (mmaps the ONNX files) and reused across dictations. The streaming
/// path doesn't use this — Parakeet's streaming variant lives in the
/// `parakeet-rs::ParakeetEOU` / `Nemotron` types and is wired separately
/// once we have a dedicated streaming engine path.
pub struct ParakeetTranscriber {
    #[cfg_attr(not(feature = "parakeet"), allow(dead_code))]
    inner: Mutex<Option<Loaded>>,
}

#[allow(dead_code)] // `path` is read on the next ensure_loaded for swap detection.
struct Loaded {
    path: PathBuf,
    #[cfg(feature = "parakeet")]
    model: parakeet_rs::ParakeetTDT,
}

impl ParakeetTranscriber {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(None),
        }
    }
}

impl Default for ParakeetTranscriber {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "parakeet")]
impl Transcriber for ParakeetTranscriber {
    fn ensure_loaded(&self, model_path: &Path) -> Result<(), String> {
        if !model_path.exists() {
            return Err(
                "Parakeet model directory not found. Please download a model first.".to_string(),
            );
        }
        let mut guard = self.inner.lock().unwrap();
        if let Some(loaded) = guard.as_ref() {
            if loaded.path == model_path {
                return Ok(());
            }
        }
        println!(
            "[typwrtr] Loading parakeet TDT model: {}",
            model_path.display()
        );
        let model = parakeet_rs::ParakeetTDT::from_pretrained(model_path, None)
            .map_err(|e| format!("Failed to load parakeet model: {}", e))?;
        *guard = Some(Loaded {
            path: model_path.to_path_buf(),
            model,
        });
        Ok(())
    }

    fn transcribe_blocking(
        &self,
        samples: Vec<f32>,
        _opts: TranscribeOptions,
    ) -> Result<TranscribeResult, String> {
        // `transcribe_samples` is on parakeet-rs's own `Transcriber` trait.
        // Bring it into scope under an alias so the local trait import
        // (already named `Transcriber`) doesn't collide.
        use parakeet_rs::Transcriber as ParakeetTranscriberExt;

        let mut guard = self.inner.lock().unwrap();
        let loaded = guard
            .as_mut()
            .ok_or_else(|| "Parakeet model not loaded. Call ensure_loaded first.".to_string())?;
        // Audio enters the recorder pipeline already resampled to 16 kHz mono
        // f32 by `audio::to_whisper_format`, so we hand it through unchanged.
        let result = loaded
            .model
            .transcribe_samples(samples, 16_000, 1, None)
            .map_err(|e| format!("Parakeet inference failed: {}", e))?;
        Ok(TranscribeResult {
            text: result.text.trim().to_string(),
            // Parakeet TDT doesn't expose per-token logprobs through the
            // current `parakeet-rs` API. `0.0` is the sentinel that the
            // recorder treats as "no signal" so the rest of the pipeline
            // stays correct.
            avg_logprob: 0.0,
        })
    }

    fn supports_streaming(&self) -> bool {
        // Parakeet has streaming via `ParakeetEOU` / `Nemotron`, but those
        // are different types and we haven't wired the streaming path
        // through them yet. False for now — the streaming-captions UI
        // toggle is hidden when the active engine is Parakeet.
        false
    }

    fn supports_initial_prompt(&self) -> bool {
        // Hot-word biasing is gated behind NeMo Python toolkit; the Rust
        // path through `parakeet-rs` doesn't expose it.
        false
    }
}

#[cfg(not(feature = "parakeet"))]
impl Transcriber for ParakeetTranscriber {
    fn ensure_loaded(&self, _model_path: &Path) -> Result<(), String> {
        Err(
            "Parakeet engine not enabled in this build. Rebuild with `--features parakeet`."
                .to_string(),
        )
    }

    fn transcribe_blocking(
        &self,
        _samples: Vec<f32>,
        _opts: TranscribeOptions,
    ) -> Result<TranscribeResult, String> {
        Err(
            "Parakeet engine not enabled in this build. Rebuild with `--features parakeet`."
                .to_string(),
        )
    }

    fn supports_streaming(&self) -> bool {
        false
    }

    fn supports_initial_prompt(&self) -> bool {
        false
    }
}

/// Filename / directory id for a Parakeet model on disk. Parakeet models
/// ship as a directory of ONNX files + tokenizer + vocab, so the
/// "filename" is actually the directory name. Mirrors the whisper
/// `model_filename` shape so the model picker can dispatch on the
/// engine field.
pub fn model_filename(model_id: &str) -> String {
    model_id.to_string()
}

/// Hugging Face mirror that hosts the pre-converted Parakeet ONNX export.
/// Used for the human-facing "where does this come from" display only —
/// actual downloads go through [`required_files`] which resolves each
/// file's blob URL.
pub fn model_download_url(model_id: &str) -> String {
    format!("https://huggingface.co/istupakov/{}-onnx", model_id)
}

/// The minimum file set `parakeet-rs::ParakeetTDT::from_pretrained`
/// needs in the model directory. We ship the int8-quantised variant by
/// default — the encoder fp32 weights are >2 GB and require an extra
/// `.onnx.data` external-data file, while the int8 encoder is ~150 MB
/// self-contained with <1 % WER hit on this model family.
///
/// Returned as `(local_filename, hf_path)` pairs. The HF download URL
/// is the istupakov mirror because the upstream NVIDIA repo ships
/// `.nemo` archives, not ONNX.
pub fn required_files(model_id: &str) -> Vec<(String, String)> {
    let base = format!("https://huggingface.co/istupakov/{}-onnx/resolve/main", model_id);
    vec![
        (
            "encoder-model.int8.onnx".to_string(),
            format!("{}/encoder-model.int8.onnx", base),
        ),
        (
            "decoder_joint-model.int8.onnx".to_string(),
            format!("{}/decoder_joint-model.int8.onnx", base),
        ),
        ("vocab.txt".to_string(), format!("{}/vocab.txt", base)),
    ]
}

/// True when every required file is present in `dir`. The recorder
/// short-circuits on this so a half-finished download doesn't load.
pub fn all_files_present(dir: &Path, model_id: &str) -> bool {
    if !dir.exists() {
        return false;
    }
    required_files(model_id)
        .iter()
        .all(|(name, _)| dir.join(name).exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_loaded_errors_on_missing_path() {
        let tx = ParakeetTranscriber::new();
        let err = tx
            .ensure_loaded(&PathBuf::from("/nonexistent/parakeet"))
            .unwrap_err();
        // Both feature-on and feature-off paths produce a clear error so
        // the recorder can show the right toast.
        assert!(!err.is_empty());
    }

    #[cfg(not(feature = "parakeet"))]
    #[test]
    fn stub_does_not_claim_capabilities() {
        let tx = ParakeetTranscriber::new();
        assert!(!tx.supports_streaming());
        assert!(!tx.supports_initial_prompt());
    }

    #[test]
    fn model_filename_passthrough() {
        assert_eq!(
            model_filename("parakeet-tdt-0.6b-v2"),
            "parakeet-tdt-0.6b-v2"
        );
    }
}
