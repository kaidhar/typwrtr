//! Audio capture pipeline. `capture` owns the cpal stream + resampler that
//! produces 16 kHz mono f32 buffers; `vad` is the energy-based silence
//! detector that drives streaming auto-stop.

pub mod capture;
pub mod vad;

pub use capture::{
    encode_wav_16k_mono, list_microphones, to_whisper_format, AudioRecorder, CaptureBuffer,
    MicDevice, MAX_RECORDING_SECS, WHISPER_SAMPLE_RATE,
};
