//! Text-cleanup pipeline. Runs after whisper produces raw text, before
//! voice commands and paste. Stages, in order:
//!
//! 1. [`text::cleanup_text`] — whitespace normalize, sentence-case,
//!    trailing-period guarantee. Cheap, always on.
//! 2. [`postprocess`] — per-app mode (`plain`/`markdown`/`code`).
//! 3. [`scrub`] — deterministic post-processing: collapse repeated words and
//!    strip canonical Whisper hallucinations. Replaces the prior T5 grammar
//!    corrector.

pub mod postprocess;
pub mod scrub;
pub mod text;

pub use postprocess::{apply as apply_postprocess, CodeCase, Mode};
pub use scrub::{collapse_repeats, scrub_hallucinations};
pub use text::cleanup_text;
