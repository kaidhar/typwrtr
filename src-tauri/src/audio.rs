use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use hound::{WavSpec, WavWriter};
use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};
use std::io::Cursor;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

pub const WHISPER_SAMPLE_RATE: u32 = 16000;

/// Hard cap on a single recording session. Beyond this, the audio callback drops
/// further samples — protects against the "user hit toggle and walked away" case
/// flagged in the eval (~23 MB/min at 48 kHz stereo).
pub const MAX_RECORDING_SECS: u32 = 600; // 10 minutes

#[derive(Debug, Clone, serde::Serialize)]
pub struct MicDevice {
    pub name: String,
    pub is_default: bool,
}

pub fn list_microphones() -> Vec<MicDevice> {
    let host = cpal::default_host();
    let default_name = match host.default_input_device().and_then(|d| d.name().ok()) {
        Some(n) => n,
        None => {
            eprintln!("[typwrtr] No default input device reported by host");
            String::new()
        }
    };

    let mut devices = Vec::new();
    if let Ok(input_devices) = host.input_devices() {
        for device in input_devices {
            if let Ok(name) = device.name() {
                devices.push(MicDevice {
                    is_default: !default_name.is_empty() && name == default_name,
                    name,
                });
            }
        }
    }
    devices
}

/// Snapshot of a finished recording: raw interleaved samples + capture format.
/// Resampling/mixdown is done off the lock-holding path (see `to_whisper_format`).
pub struct CaptureBuffer {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub channels: u16,
    pub hit_cap: bool,
}

pub struct AudioRecorder {
    samples: Arc<Mutex<Vec<f32>>>,
    hit_cap: Arc<AtomicBool>,
    stop_tx: Option<mpsc::Sender<()>>,
    join: Option<JoinHandle<()>>,
    sample_rate: u32,
    channels: u16,
}

impl AudioRecorder {
    pub fn new() -> Self {
        Self {
            samples: Arc::new(Mutex::new(Vec::new())),
            hit_cap: Arc::new(AtomicBool::new(false)),
            stop_tx: None,
            join: None,
            sample_rate: 48000,
            channels: 1,
        }
    }

    /// Start capture. The cpal stream lives on a dedicated thread for its entire
    /// lifetime — it never crosses thread boundaries, so no `unsafe Send/Sync`
    /// is needed. The thread exits when we send on `stop_tx`.
    pub fn start(&mut self, mic_name: &str) -> Result<(), String> {
        if self.stop_tx.is_some() {
            return Err("Recorder is already running".to_string());
        }

        self.samples.lock().unwrap().clear();
        self.hit_cap.store(false, Ordering::Relaxed);

        let mic = mic_name.to_string();
        let samples = self.samples.clone();
        let hit_cap = self.hit_cap.clone();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(u32, u16), String>>();
        let (stop_tx, stop_rx) = mpsc::channel::<()>();

        let join = std::thread::spawn(move || {
            run_capture_thread(mic, samples, hit_cap, ready_tx, stop_rx);
        });

        let (sample_rate, channels) = ready_rx
            .recv()
            .map_err(|_| "Audio capture thread exited before signaling ready".to_string())??;

        self.sample_rate = sample_rate;
        self.channels = channels;
        self.stop_tx = Some(stop_tx);
        self.join = Some(join);
        println!(
            "[typwrtr] Audio recording started ({} Hz, {} ch)",
            sample_rate, channels
        );
        Ok(())
    }

    /// Snapshot the in-flight buffer without stopping capture. Used by the
    /// streaming partials task to run inference on what the user has spoken
    /// so far. Returns `None` until at least 0.5 s of audio has accumulated —
    /// shorter buffers fool whisper into emitting noise tokens.
    pub fn peek_samples(&self) -> Option<CaptureBuffer> {
        let buf = self.samples.lock().unwrap();
        let min_samples = (self.sample_rate as usize / 2) * self.channels as usize;
        if buf.len() < min_samples {
            return None;
        }
        Some(CaptureBuffer {
            samples: buf.clone(),
            sample_rate: self.sample_rate,
            channels: self.channels,
            hit_cap: self.hit_cap.load(Ordering::Relaxed),
        })
    }

    /// Length in milliseconds of audio captured so far. Cheap; lock-only on
    /// the underlying Vec length.
    pub fn buffer_ms(&self) -> u64 {
        let len = self.samples.lock().unwrap().len() as u64;
        let denom = self.sample_rate as u64 * self.channels as u64;
        if denom == 0 {
            0
        } else {
            len * 1000 / denom
        }
    }

    /// Stop capture and return the raw buffer. Cheap — no resample, no I/O —
    /// safe to call while holding a `std::sync::Mutex` on an async path.
    pub fn stop_and_take(&mut self) -> Result<CaptureBuffer, String> {
        let Some(stop) = self.stop_tx.take() else {
            return Err("Recorder is not running".to_string());
        };
        let _ = stop.send(());
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }

        let samples = std::mem::take(&mut *self.samples.lock().unwrap());
        if samples.is_empty() {
            return Err("No audio captured".to_string());
        }

        let hit_cap = self.hit_cap.swap(false, Ordering::Relaxed);
        if hit_cap {
            eprintln!(
                "[typwrtr] WARNING: recording hit the {}s cap; later audio was dropped",
                MAX_RECORDING_SECS
            );
        }

        println!("[typwrtr] Captured {} raw samples", samples.len());
        Ok(CaptureBuffer {
            samples,
            sample_rate: self.sample_rate,
            channels: self.channels,
            hit_cap,
        })
    }
}

fn run_capture_thread(
    mic_name: String,
    samples: Arc<Mutex<Vec<f32>>>,
    hit_cap: Arc<AtomicBool>,
    ready_tx: mpsc::Sender<Result<(u32, u16), String>>,
    stop_rx: mpsc::Receiver<()>,
) {
    let host = cpal::default_host();

    let device = if mic_name == "default" {
        host.default_input_device()
            .ok_or_else(|| "No default input device found".to_string())
    } else {
        host.input_devices()
            .map_err(|e| e.to_string())
            .and_then(|mut iter| {
                iter.find(|d| d.name().map(|n| n == mic_name).unwrap_or(false))
                    .ok_or_else(|| format!("Microphone '{}' not found", mic_name))
            })
    };

    let device = match device {
        Ok(d) => d,
        Err(e) => {
            let _ = ready_tx.send(Err(e));
            return;
        }
    };

    let default_config = match device.default_input_config() {
        Ok(c) => c,
        Err(e) => {
            let _ = ready_tx.send(Err(format!("Failed to get default input config: {}", e)));
            return;
        }
    };

    let sample_rate = default_config.sample_rate().0;
    let channels = default_config.channels();
    let max_samples = (sample_rate as u64 * channels as u64 * MAX_RECORDING_SECS as u64) as usize;

    let config = cpal::StreamConfig {
        channels,
        sample_rate: cpal::SampleRate(sample_rate),
        buffer_size: cpal::BufferSize::Default,
    };

    let cb_samples = samples.clone();
    let cb_cap = hit_cap.clone();
    let stream = device.build_input_stream(
        &config,
        move |data: &[f32], _: &cpal::InputCallbackInfo| {
            let mut buf = cb_samples.lock().unwrap();
            let remaining = max_samples.saturating_sub(buf.len());
            if remaining == 0 {
                cb_cap.store(true, Ordering::Relaxed);
                return;
            }
            if data.len() > remaining {
                buf.extend_from_slice(&data[..remaining]);
                cb_cap.store(true, Ordering::Relaxed);
            } else {
                buf.extend_from_slice(data);
            }
        },
        |err| {
            eprintln!("[typwrtr] Audio stream error: {}", err);
        },
        None,
    );

    let stream = match stream {
        Ok(s) => s,
        Err(e) => {
            let _ = ready_tx.send(Err(e.to_string()));
            return;
        }
    };

    if let Err(e) = stream.play() {
        let _ = ready_tx.send(Err(e.to_string()));
        return;
    }

    let _ = ready_tx.send(Ok((sample_rate, channels)));

    // Park here until someone asks us to stop. Stream is dropped (and capture
    // ends) at the end of this scope.
    let _ = stop_rx.recv();
    drop(stream);
    println!("[typwrtr] Audio recording stopped");
}

/// CPU-bound transform from raw multi-channel input to 16 kHz mono f32. Designed
/// to be called from `tokio::task::spawn_blocking` so it doesn't hold a lock or
/// stall the runtime.
pub fn to_whisper_format(buf: CaptureBuffer) -> Result<Vec<f32>, String> {
    let CaptureBuffer {
        samples,
        sample_rate,
        channels,
        ..
    } = buf;

    let mono: Vec<f32> = if channels > 1 {
        samples
            .chunks(channels as usize)
            .map(|frame| frame.iter().sum::<f32>() / frame.len() as f32)
            .collect()
    } else {
        samples
    };

    let resampled = resample_to_whisper(&mono, sample_rate)?;
    println!(
        "[typwrtr] Resampled to {} samples at {}Hz",
        resampled.len(),
        WHISPER_SAMPLE_RATE
    );
    Ok(resampled)
}

/// Encode 16kHz mono f32 samples as a WAV byte buffer (for cloud upload).
pub fn encode_wav_16k_mono(samples: &[f32]) -> Result<Vec<u8>, String> {
    let spec = WavSpec {
        channels: 1,
        sample_rate: WHISPER_SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };

    let mut buf: Vec<u8> = Vec::with_capacity(samples.len() * 2 + 44);
    {
        let cursor = Cursor::new(&mut buf);
        let mut writer = WavWriter::new(cursor, spec).map_err(|e| e.to_string())?;
        for &s in samples {
            let amp = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
            writer.write_sample(amp).map_err(|e| e.to_string())?;
        }
        writer.finalize().map_err(|e| e.to_string())?;
    }
    Ok(buf)
}

fn resample_to_whisper(samples: &[f32], from_rate: u32) -> Result<Vec<f32>, String> {
    if from_rate == WHISPER_SAMPLE_RATE {
        return Ok(samples.to_vec());
    }

    let ratio = WHISPER_SAMPLE_RATE as f64 / from_rate as f64;
    let chunk_size = 1024usize;
    let params = SincInterpolationParameters {
        sinc_len: 128,
        f_cutoff: 0.95,
        interpolation: SincInterpolationType::Linear,
        oversampling_factor: 256,
        window: WindowFunction::BlackmanHarris2,
    };

    let mut resampler = SincFixedIn::<f32>::new(ratio, 2.0, params, chunk_size, 1)
        .map_err(|e| format!("rubato init: {}", e))?;

    let mut output: Vec<f32> =
        Vec::with_capacity((samples.len() as f64 * ratio) as usize + chunk_size);

    let mut idx = 0;
    while idx + chunk_size <= samples.len() {
        let chunk = [&samples[idx..idx + chunk_size]];
        let out = resampler
            .process(&chunk, None)
            .map_err(|e| format!("rubato process: {}", e))?;
        output.extend_from_slice(&out[0]);
        idx += chunk_size;
    }

    if idx < samples.len() {
        let tail: Vec<f32> = samples[idx..].to_vec();
        let tail_ref = [tail.as_slice()];
        let out = resampler
            .process_partial(Some(&tail_ref), None)
            .map_err(|e| format!("rubato process_partial: {}", e))?;
        let kept = ((samples.len() - idx) as f64 * ratio).round() as usize;
        let take = kept.min(out[0].len());
        output.extend_from_slice(&out[0][..take]);
    }

    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resample_passthrough_at_target_rate() {
        let samples = vec![0.1, -0.2, 0.3, -0.4];
        let out = resample_to_whisper(&samples, WHISPER_SAMPLE_RATE).unwrap();
        assert_eq!(out, samples);
    }

    #[test]
    fn resample_48k_to_16k_length_is_roughly_third() {
        let samples = vec![0.0_f32; 48000];
        let out = resample_to_whisper(&samples, 48000).unwrap();
        let expected = 16000;
        let diff = (out.len() as i64 - expected as i64).abs();
        assert!(diff < 1024, "expected ~{}, got {}", expected, out.len());
    }

    #[test]
    fn encode_wav_has_riff_header() {
        let samples = vec![0.0_f32; 16000];
        let bytes = encode_wav_16k_mono(&samples).unwrap();
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
    }

    #[test]
    fn to_whisper_format_mixes_stereo_to_mono() {
        // 16 kHz stereo, simple LR pattern — should average to mono.
        let buf = CaptureBuffer {
            samples: vec![1.0, -1.0, 0.5, -0.5],
            sample_rate: WHISPER_SAMPLE_RATE,
            channels: 2,
            hit_cap: false,
        };
        let out = to_whisper_format(buf).unwrap();
        assert_eq!(out, vec![0.0, 0.0]);
    }
}
