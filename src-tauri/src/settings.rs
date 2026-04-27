use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

const KEYRING_SERVICE: &str = "com.typwrtr.app";
const KEYRING_USER_GROQ: &str = "groq_api_key";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub microphone: String,
    pub engine: String,
    #[serde(rename = "whisperModel")]
    pub whisper_model: String,
    /// Held in the OS keychain, not on disk. The JSON field is kept blank.
    #[serde(rename = "groqApiKey")]
    pub groq_api_key: String,
    #[serde(rename = "modelDir")]
    pub model_dir: String,
    #[serde(rename = "toggleHotkey", alias = "hotkey")]
    pub toggle_hotkey: String,
    #[serde(rename = "pushToTalkHotkey")]
    pub push_to_talk_hotkey: String,
    /// ISO-639-1 code passed to whisper / Groq. "en" for English, "auto" lets
    /// whisper detect (local engine only — Groq requires an explicit code).
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
    /// Phase-4 §4.2: optional LLM cleanup pass after postprocess. One of
    /// `"off"` (default) or `"groq"`. Local llama.cpp sidecar is deferred.
    #[serde(rename = "llmCleanup", default = "default_llm_cleanup")]
    pub llm_cleanup: String,
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

fn default_llm_cleanup() -> String {
    "off".to_string()
}

fn default_vad_silence_ms() -> u32 {
    800
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            microphone: "default".to_string(),
            engine: "local".to_string(),
            whisper_model: "medium.en".to_string(),
            groq_api_key: String::new(),
            model_dir: String::new(),
            toggle_hotkey: "CmdOrCtrl+Shift+Space".to_string(),
            push_to_talk_hotkey: "CmdOrCtrl+Shift+Enter".to_string(),
            language: "en".to_string(),
            initial_prompt: String::new(),
            save_transcriptions: true,
            keep_audio_clips: false,
            fixup_hotkey: "CmdOrCtrl+Shift+Semicolon".to_string(),
            llm_cleanup: "off".to_string(),
            streaming_captions: false,
            vad_silence_ms: 800,
        }
    }
}

impl Settings {
    pub fn config_path(app_dir: &PathBuf) -> PathBuf {
        app_dir.join("config.json")
    }

    pub fn load(app_dir: &PathBuf) -> Self {
        let path = Self::config_path(app_dir);
        let mut settings: Self = match fs::read_to_string(&path) {
            Ok(contents) => serde_json::from_str(&contents).unwrap_or_default(),
            Err(_) => Self::default(),
        };

        // Migrate any plaintext key still on disk into the keychain, then blank
        // it on disk on next save.
        let plaintext = std::mem::take(&mut settings.groq_api_key);
        if !plaintext.is_empty() {
            if let Err(e) = write_groq_key(&plaintext) {
                eprintln!("[typwrtr] Failed to migrate Groq key into keychain: {}", e);
                // Keep the plaintext in memory so the user isn't broken — but the
                // next save will still attempt the migration.
                settings.groq_api_key = plaintext;
            } else {
                settings.groq_api_key = plaintext;
                // Re-save to drop the on-disk copy.
                if let Err(e) = settings.save(app_dir) {
                    eprintln!(
                        "[typwrtr] Failed to scrub plaintext Groq key from disk: {}",
                        e
                    );
                }
            }
        } else {
            // Pull from keychain (may legitimately be empty if user has not set it).
            settings.groq_api_key = read_groq_key().unwrap_or_default();
        }

        settings
    }

    pub fn save(&self, app_dir: &PathBuf) -> Result<(), String> {
        // Persist the secret to the OS keychain, never to disk.
        if self.groq_api_key.is_empty() {
            let _ = delete_groq_key();
        } else {
            write_groq_key(&self.groq_api_key)?;
        }

        let mut on_disk = self.clone();
        on_disk.groq_api_key = String::new();

        let path = Self::config_path(app_dir);
        fs::create_dir_all(app_dir).map_err(|e| e.to_string())?;
        let json = serde_json::to_string_pretty(&on_disk).map_err(|e| e.to_string())?;
        fs::write(&path, json).map_err(|e| e.to_string())
    }
}

fn keyring_entry() -> Result<keyring::Entry, String> {
    keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER_GROQ).map_err(|e| e.to_string())
}

fn read_groq_key() -> Result<String, String> {
    match keyring_entry()?.get_password() {
        Ok(s) => Ok(s),
        Err(keyring::Error::NoEntry) => Ok(String::new()),
        Err(e) => Err(e.to_string()),
    }
}

fn write_groq_key(value: &str) -> Result<(), String> {
    keyring_entry()?
        .set_password(value)
        .map_err(|e| e.to_string())
}

fn delete_groq_key() -> Result<(), String> {
    match keyring_entry()?.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.to_string()),
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
        assert_eq!(settings.engine, "local");
        assert_eq!(settings.whisper_model, "medium.en");
        assert_eq!(settings.groq_api_key, "");
        assert_eq!(settings.model_dir, "");
        assert_eq!(settings.toggle_hotkey, "CmdOrCtrl+Shift+Space");
        assert_eq!(settings.push_to_talk_hotkey, "CmdOrCtrl+Shift+Enter");
        assert_eq!(settings.language, "en");
        assert_eq!(settings.initial_prompt, "");
        assert!(settings.save_transcriptions);
        assert!(!settings.keep_audio_clips);
        assert_eq!(settings.fixup_hotkey, "CmdOrCtrl+Shift+Semicolon");
        assert_eq!(settings.llm_cleanup, "off");
        assert!(!settings.streaming_captions);
        assert_eq!(settings.vad_silence_ms, 800);
    }

    #[test]
    fn test_save_blanks_groq_key_on_disk() {
        let dir = temp_dir().join("typwrtr_test_keyring_scrub");
        let _ = fs::remove_dir_all(&dir);

        let mut settings = Settings::default();
        settings.engine = "cloud".to_string();
        settings.groq_api_key = "test-key-do-not-write-to-disk".to_string();

        // The save may fail in CI environments without a keychain backend; tolerate
        // either outcome but verify that *if* save succeeded, no plaintext key is on disk.
        if settings.save(&dir).is_ok() {
            let raw = fs::read_to_string(Settings::config_path(&dir)).unwrap();
            assert!(
                !raw.contains("test-key-do-not-write-to-disk"),
                "groq key was written to disk: {}",
                raw
            );
            assert!(raw.contains("\"groqApiKey\": \"\""));
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_load_missing_file_returns_default() {
        let dir = temp_dir().join("typwrtr_test_missing");
        let _ = fs::remove_dir_all(&dir);
        let mut settings = Settings::load(&dir);
        // The keyring may legitimately have a value from a previous run; ignore that for
        // this test by zeroing it before comparing.
        settings.groq_api_key = String::new();
        assert_eq!(settings, Settings::default());
    }

    #[test]
    fn test_load_corrupt_json_returns_default() {
        let dir = temp_dir().join("typwrtr_test_corrupt");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("config.json"), "not json").unwrap();

        let mut settings = Settings::load(&dir);
        settings.groq_api_key = String::new();
        assert_eq!(settings, Settings::default());

        let _ = fs::remove_dir_all(&dir);
    }
}
