//! Tiny energy-based voice activity detector.
//!
//! The plan calls for the `voice_activity_detector` crate (Silero ONNX). We
//! ship a simpler RMS-threshold detector first — zero new deps and good
//! enough for "did the user stop talking" silence detection at the 800 ms
//! granularity Phase 5 §5.3 needs. Swappable later.

/// Walks the tail of `samples` (assumed 16 kHz mono f32) and returns the
/// length in milliseconds of the trailing silence — i.e. consecutive frames
/// whose RMS is under `rms_threshold`. The user is "still talking" if this
/// value is small.
///
/// `frame_ms` controls the granularity of the silence walk; 30 ms is a
/// reasonable default — small enough to detect pauses precisely, large
/// enough that one quiet sample doesn't reset the timer.
pub fn trailing_silence_ms(samples: &[f32], frame_ms: u32, rms_threshold: f32) -> u64 {
    if samples.is_empty() {
        return 0;
    }
    let frame_size = ((frame_ms as f32 / 1000.0) * 16_000.0) as usize;
    if frame_size == 0 || frame_size > samples.len() {
        return 0;
    }

    let mut silent_frames = 0u64;
    let mut idx = samples.len();
    while idx >= frame_size {
        let start = idx - frame_size;
        let frame = &samples[start..idx];
        if rms(frame) < rms_threshold {
            silent_frames += 1;
            idx = start;
        } else {
            break;
        }
    }
    silent_frames * frame_ms as u64
}

/// True if the buffer contains at least one frame above the speech threshold.
/// Used to gate auto-stop: we don't want to fire on a recording that started
/// silent and stayed silent (e.g. the user pressed the toggle key by accident).
pub fn has_speech(samples: &[f32], frame_ms: u32, rms_threshold: f32) -> bool {
    let frame_size = ((frame_ms as f32 / 1000.0) * 16_000.0) as usize;
    if frame_size == 0 {
        return false;
    }
    samples
        .chunks(frame_size)
        .any(|frame| rms(frame) >= rms_threshold)
}

fn rms(frame: &[f32]) -> f32 {
    if frame.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = frame.iter().map(|s| s * s).sum();
    (sum_sq / frame.len() as f32).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn silent_buffer(secs: f32) -> Vec<f32> {
        vec![0.0; (secs * 16_000.0) as usize]
    }

    fn loud_buffer(secs: f32) -> Vec<f32> {
        vec![0.5; (secs * 16_000.0) as usize]
    }

    #[test]
    fn full_silence_has_no_speech() {
        assert!(!has_speech(&silent_buffer(2.0), 30, 0.005));
    }

    #[test]
    fn loud_buffer_has_speech() {
        assert!(has_speech(&loud_buffer(0.5), 30, 0.005));
    }

    #[test]
    fn trailing_silence_after_loud_returns_silence_duration() {
        let mut buf = loud_buffer(1.0);
        buf.extend(silent_buffer(1.0));
        let ms = trailing_silence_ms(&buf, 30, 0.005);
        // Rounded down by frame_ms; should be near 1000ms (a few frames slack).
        assert!(ms >= 900 && ms <= 1020, "got {} ms", ms);
    }

    #[test]
    fn trailing_silence_zero_when_still_loud() {
        let buf = loud_buffer(1.0);
        assert_eq!(trailing_silence_ms(&buf, 30, 0.005), 0);
    }
}
