use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub microphone: String,
    #[serde(rename = "whisperModel")]
    pub whisper_model: String,
    #[serde(rename = "modelDir")]
    pub model_dir: String,
    #[serde(rename = "toggleHotkey", alias = "hotkey")]
    pub toggle_hotkey: String,
    #[serde(rename = "pushToTalkHotkey")]
    pub push_to_talk_hotkey: String,
    /// ISO-639-1 code passed to whisper. "en" for English, "auto" lets
    /// whisper detect (transcription is always local now).
    pub language: String,
    /// Free-form text fed to whisper as the initial prompt to bias decoding
    /// toward expected vocabulary. Phase-2 self-learning will populate this
    /// programmatically from app-profile vocabularies.
    #[serde(rename = "initialPrompt")]
    pub initial_prompt: String,
    /// When true, every successful transcription is logged to the SQLite DB
    /// for the self-learning loop. Off ≈ "incognito" by default.
    #[serde(rename = "saveTranscriptions")]
    pub save_transcriptions: bool,
    /// When true, the captured WAV is also written to `<app_dir>/audio/`
    /// alongside the row. Default off — text-only is the privacy-preserving choice.
    #[serde(rename = "keepAudioClips")]
    pub keep_audio_clips: bool,
    /// Phase-2 fix-up shortcut. Default `Cmd/Ctrl+Shift+Semicolon`. The user
    /// presses this with text selected to teach typwrtr the correction.
    #[serde(rename = "fixupHotkey")]
    pub fixup_hotkey: String,
    /// Phase-5 §5.1/5.2: when true, run partial whisper inferences during
    /// recording and surface them in the captions overlay. Off by default —
    /// streaming costs extra inference cycles per second.
    #[serde(rename = "streamingCaptions", default)]
    pub streaming_captions: bool,
    /// Phase-5 §5.3: trailing-silence (ms) after which the recorder auto-
    /// finalizes a toggle-mode session. `0` disables auto-stop. Default 800
    /// per the plan; configurable 400–2000.
    #[serde(rename = "vadSilenceMs", default = "default_vad_silence_ms")]
    pub vad_silence_ms: u32,
}

fn default_vad_silence_ms() -> u32 {
    800
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            microphone: "default".to_string(),
            whisper_model: "medium.en".to_string(),
            model_dir: String::new(),
            toggle_hotkey: "CmdOrCtrl+Shift+Space".to_string(),
            push_to_talk_hotkey: "CmdOrCtrl+Shift+Enter".to_string(),
            language: "en".to_string(),
            initial_prompt: String::new(),
            save_transcriptions: true,
            keep_audio_clips: false,
            fixup_hotkey: "CmdOrCtrl+Shift+Semicolon".to_string(),
            streaming_captions: false,
            vad_silence_ms: 800,
        }
    }
}

impl Settings {
    pub fn config_path(app_dir: &PathBuf) -> PathBuf {
        app_dir.join("config.json")
    }

    /// Which ASR engine the currently-selected model belongs to. Drives
    /// dispatch in the recorder + streaming, plus UI visibility for
    /// engine-specific features (streaming captions today).
    pub fn engine(&self) -> &'static str {
        engine_for_model(&self.whisper_model)
    }

    pub fn load(app_dir: &PathBuf) -> Self {
        let path = Self::config_path(app_dir);
        let raw = fs::read_to_string(&path).ok();
        let settings: Self = raw
            .as_deref()
            .and_then(|s| serde_json::from_str::<Self>(s).ok())
            .unwrap_or_default();

        // One-shot migration for configs written by retired cleanup-backend
        // builds. The `engine`/`groqApiKey`/`llmCleanup`/`ollama*` keys date
        // from the cloud-Groq → self-hosted-Ollama era; the `grammar*` keys
        // date from the on-device T5 corrector era. Both are gone now —
        // sniff for any of them and re-save to strip them, so subsequent
        // loads see a clean schema. Serde's `#[serde(default)]` already
        // tolerates the unknown keys at deserialise time; this branch only
        // exists to keep `config.json` tidy.
        let has_legacy_keys = raw
            .as_deref()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
            .and_then(|v| {
                v.as_object().map(|m| {
                    m.contains_key("engine")
                        || m.contains_key("groqApiKey")
                        || m.contains_key("llmCleanup")
                        || m.contains_key("ollamaUrl")
                        || m.contains_key("ollamaModel")
                        || m.contains_key("ollamaTimeoutMs")
                        || m.contains_key("grammarCorrection")
                        || m.contains_key("grammarSkipAboveLogprob")
                })
            })
            .unwrap_or(false);
        if has_legacy_keys {
            if let Err(e) = settings.save(app_dir) {
                eprintln!("[typwrtr] Failed to scrub legacy keys from config: {}", e);
            }
        }

        settings
    }

    pub fn save(&self, app_dir: &PathBuf) -> Result<(), String> {
        let path = Self::config_path(app_dir);
        fs::create_dir_all(app_dir).map_err(|e| e.to_string())?;
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        fs::write(&path, json).map_err(|e| e.to_string())
    }
}

/// Static mapping from model id to engine name. Centralised so the
/// recorder, streaming task, and download command all agree. Returns
/// `"parakeet"` for Parakeet TDT model ids; `"whisper"` for everything
/// else (the default keeps unknown ids on the safer whisper path).
pub fn engine_for_model(model_id: &str) -> &'static str {
    if model_id.starts_with("parakeet") {
        "parakeet"
    } else {
        "whisper"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env::temp_dir;

    #[test]
    fn test_default_settings() {
        let settings = Settings::default();
        assert_eq!(settings.microphone, "default");
        assert_eq!(settings.whisper_model, "medium.en");
        assert_eq!(settings.model_dir, "");
        assert_eq!(settings.toggle_hotkey, "CmdOrCtrl+Shift+Space");
        assert_eq!(settings.push_to_talk_hotkey, "CmdOrCtrl+Shift+Enter");
        assert_eq!(settings.language, "en");
        assert_eq!(settings.initial_prompt, "");
        assert!(settings.save_transcriptions);
        assert!(!settings.keep_audio_clips);
        assert_eq!(settings.fixup_hotkey, "CmdOrCtrl+Shift+Semicolon");
        assert!(!settings.streaming_captions);
        assert_eq!(settings.vad_silence_ms, 800);
    }

    #[test]
    fn test_load_missing_file_returns_default() {
        let dir = temp_dir().join("typwrtr_test_missing");
        let _ = fs::remove_dir_all(&dir);
        let settings = Settings::load(&dir);
        assert_eq!(settings, Settings::default());
    }

    #[test]
    fn test_load_corrupt_json_returns_default() {
        let dir = temp_dir().join("typwrtr_test_corrupt");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("config.json"), "not json").unwrap();

        let settings = Settings::load(&dir);
        assert_eq!(settings, Settings::default());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_load_ignores_legacy_fields() {
        // Old configs serialised `engine` + `groqApiKey`. With #[serde(default)]
        // on the struct, deserialising must succeed and just drop them.
        let dir = temp_dir().join("typwrtr_test_legacy_fields");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("config.json"),
            r#"{"engine":"cloud","groqApiKey":"gsk_old","whisperModel":"small.en"}"#,
        )
        .unwrap();
        let settings = Settings::load(&dir);
        assert_eq!(settings.whisper_model, "small.en");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_legacy_grammar_keys_are_scrubbed_on_save() {
        // A config carrying any of the retired cleanup-backend keys
        // (`engine`/`groqApiKey`/`llmCleanup`/`ollama*`/`grammarCorrection`/
        // `grammarSkipAboveLogprob`) must trigger a re-save that strips
        // them, so subsequent loads see a clean schema.
        let dir = temp_dir().join("typwrtr_test_legacy_grammar_reset");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("config.json"),
            r#"{"grammarCorrection":true,"grammarSkipAboveLogprob":-0.7,"whisperModel":"small.en"}"#,
        )
        .unwrap();

        let settings = Settings::load(&dir);
        // Non-legacy fields survive deserialisation untouched.
        assert_eq!(settings.whisper_model, "small.en");

        let raw = fs::read_to_string(Settings::config_path(&dir)).unwrap();
        assert!(
            !raw.contains("\"grammarCorrection\""),
            "legacy grammarCorrection still in {}",
            raw
        );
        assert!(
            !raw.contains("\"grammarSkipAboveLogprob\""),
            "legacy grammarSkipAboveLogprob still in {}",
            raw
        );
        assert!(raw.contains("\"whisperModel\": \"small.en\""));

        // Second load: legacy keys are gone, no re-save loop.
        let _ = Settings::load(&dir);
        let raw_second = fs::read_to_string(Settings::config_path(&dir)).unwrap();
        assert_eq!(raw, raw_second, "second load mutated the on-disk config");

        let _ = fs::remove_dir_all(&dir);
    }
}
