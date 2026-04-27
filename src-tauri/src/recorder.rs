use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager};

use crate::audio::{self, AudioRecorder, WHISPER_SAMPLE_RATE};
use crate::cleanup::cleanup_text;
use crate::commands::{apply_voice_commands_with_snippets, Snippet};
use crate::context;
use crate::db::{Db, NewTranscription};
use crate::focused_text;
use crate::learning::{apply_correction_pairs, similar_ratio};
use crate::llm_cleanup::{cleanup as llm_cleanup, CleanupBackend};
use crate::paste::paste_text;
use crate::postprocess::{self, CodeCase, Mode};
use crate::settings::Settings;
use crate::streaming::{self, StreamingConfig, StreamingHandle};
use crate::transcribe_groq;
use crate::transcribe_local::{self, LocalTranscriber, TranscribeOptions};

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub enum RecordingState {
    Ready,
    Recording,
    Transcribing,
}

fn update_overlay(app: &AppHandle, state: &RecordingState) {
    if let Some(overlay) = app.get_webview_window("overlay") {
        let class = match state {
            RecordingState::Ready => "mic",
            RecordingState::Recording => "mic recording",
            RecordingState::Transcribing => "mic transcribing",
        };
        let js = format!("document.getElementById('mic').className = '{}';", class);
        let _ = overlay.eval(&js);
    }
}

pub struct Recorder {
    state: Arc<Mutex<RecordingState>>,
    audio_recorder: Arc<Mutex<AudioRecorder>>,
    local: Arc<LocalTranscriber>,
    db: Arc<Db>,
    streaming: Mutex<Option<StreamingHandle>>,
}

impl Recorder {
    pub fn new(local: Arc<LocalTranscriber>, db: Arc<Db>) -> Self {
        Self {
            state: Arc::new(Mutex::new(RecordingState::Ready)),
            audio_recorder: Arc::new(Mutex::new(AudioRecorder::new())),
            local,
            db,
            streaming: Mutex::new(None),
        }
    }

    pub fn local_transcriber(&self) -> Arc<LocalTranscriber> {
        self.local.clone()
    }

    pub fn get_state(&self) -> RecordingState {
        self.state.lock().unwrap().clone()
    }

    pub fn start_recording(
        &self,
        app: &AppHandle,
        settings: &Settings,
        model_dir: &PathBuf,
    ) -> Result<(), String> {
        {
            let mut state = self.state.lock().unwrap();
            if *state != RecordingState::Ready {
                return Err("Already recording or transcribing".to_string());
            }
            let mut recorder = self.audio_recorder.lock().unwrap();
            recorder.start(&settings.microphone)?;
            *state = RecordingState::Recording;
        }
        let _ = app.emit("recording-state", RecordingState::Recording);
        update_overlay(app, &RecordingState::Recording);

        // Phase 5 — kick the streaming + VAD task if the user opted in. Stops
        // automatically on the next stop_and_transcribe call.
        if settings.streaming_captions || settings.vad_silence_ms > 0 {
            let model_path =
                model_dir.join(transcribe_local::model_filename(&settings.whisper_model));
            let cfg = StreamingConfig {
                model_path,
                initial_prompt: settings.initial_prompt.clone(),
                language: settings.language.clone(),
                partial_interval_ms: 700,
                silence_threshold_ms: settings.vad_silence_ms as u64,
                rms_threshold: 0.005,
                emit_partials: settings.streaming_captions,
            };
            let handle = streaming::spawn(
                app.clone(),
                self.audio_recorder.clone(),
                self.local.clone(),
                cfg,
            );
            *self.streaming.lock().unwrap() = Some(handle);
        }

        Ok(())
    }

    pub async fn stop_and_transcribe(
        &self,
        app: &AppHandle,
        settings: &Settings,
        app_dir: &PathBuf,
        model_dir: &PathBuf,
    ) -> Result<String, String> {
        // Total user-perceived latency: from "released the hotkey" until the
        // pasted text is on screen. Captured at function entry, used to tag the
        // DB row at the end.
        let started = Instant::now();

        {
            let mut state = self.state.lock().unwrap();
            if *state != RecordingState::Recording {
                return Err("Not currently recording".to_string());
            }
            *state = RecordingState::Transcribing;
            let _ = app.emit("recording-state", RecordingState::Transcribing);
            update_overlay(app, &RecordingState::Transcribing);
        }

        // Stop the streaming task (if any) so it doesn't race the final pass.
        if let Some(h) = self.streaming.lock().unwrap().take() {
            h.stop();
        }

        let raw = {
            let mut recorder = self.audio_recorder.lock().unwrap();
            recorder.stop_and_take()?
        };

        let samples = tokio::task::spawn_blocking(move || audio::to_whisper_format(raw))
            .await
            .map_err(|e| format!("Audio processing join error: {}", e))??;

        let duration_ms = (samples.len() as i64).saturating_mul(1000) / WHISPER_SAMPLE_RATE as i64;

        // If the user opted in to keeping audio, persist the WAV before we kick
        // off transcription so a crash mid-inference still leaves the clip on
        // disk for replay/debugging.
        let audio_path = if settings.keep_audio_clips {
            persist_audio_clip(&samples, app_dir).unwrap_or_else(|e| {
                eprintln!("[typwrtr] Failed to persist audio clip: {}", e);
                None
            })
        } else {
            None
        };

        // Probe the foreground app *before* transcription so we can look up the
        // matching app profile and use its prompt template / model override.
        // The same `ctx` is reused for the post-paste DB insert below — the
        // user can't switch focus mid-dictation anyway.
        let ctx = context::current();
        let raw_profile = ctx
            .as_ref()
            .and_then(|c| self.db.get_app_profile(&c.bundle_id).ok().flatten());
        // A profile that exists with `enabled = false` means the user has
        // explicitly opted this app out of self-learning. Phase 2 will deepen
        // this; here we already need to skip the prompt template AND skip
        // logging for that app.
        let app_disabled = matches!(raw_profile.as_ref(), Some(p) if !p.enabled);
        let profile = raw_profile.as_ref().filter(|p| p.enabled);

        let mut effective_prompt = merge_prompt(
            profile.and_then(|p| p.prompt_template.as_deref()),
            &settings.initial_prompt,
        );
        // Phase 2 §2.3: extend the prompt with learned vocabulary + top
        // corrections from this app. We don't tokenize against whisper's BPE
        // here — instead we cap the appended slice at PROMPT_BUDGET_CHARS,
        // which empirically stays under the ~224-token whisper.cpp limit even
        // for dense English. Adjust if you measure trimming.
        if let Some(bundle) = ctx.as_ref().map(|c| c.bundle_id.as_str()) {
            let mut extras: Vec<String> = Vec::new();
            if let Ok(rows) = self.db.top_vocab_for_app(bundle, 20) {
                extras.extend(rows.into_iter().map(|r| r.term));
            }
            if let Ok(rows) = self.db.top_vocab_global(10) {
                extras.extend(rows.into_iter().map(|r| r.term));
            }
            if let Ok(rows) = self.db.top_corrections_for_app(bundle, 10) {
                extras.extend(rows.into_iter().filter_map(|r| {
                    let t = r.right.trim().to_string();
                    if t.is_empty() {
                        None
                    } else {
                        Some(t)
                    }
                }));
            }
            if !extras.is_empty() {
                let appended = budgeted_join(&effective_prompt, &extras, PROMPT_BUDGET_CHARS);
                effective_prompt = appended;
            }
        }

        let effective_model = profile
            .and_then(|p| p.preferred_model.as_deref())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(settings.whisper_model.as_str())
            .to_string();

        let raw_text = match settings.engine.as_str() {
            "local" => {
                let model_path = model_dir.join(transcribe_local::model_filename(&effective_model));
                let opts = TranscribeOptions {
                    language: settings.language.clone(),
                    initial_prompt: effective_prompt.clone(),
                    ..TranscribeOptions::default()
                };
                self.local.transcribe(&model_path, samples, opts).await?
            }
            "cloud" => {
                let wav = audio::encode_wav_16k_mono(&samples)?;
                transcribe_groq::transcribe_groq(
                    &settings.groq_api_key,
                    wav,
                    &settings.language,
                    &effective_prompt,
                )
                .await?
            }
            _ => return Err(format!("Unknown engine: {}", settings.engine)),
        };

        let mut cleaned = cleanup_text(&raw_text);

        // Phase 2 §2.4 replacement table — apply learned (wrong → right) pairs
        // with `count >= 3`, scoped to this app, when the profile permits it.
        let auto_apply = profile.map(|p| p.auto_apply_replacements).unwrap_or(true);
        if auto_apply && !cleaned.is_empty() {
            if let Some(bundle) = ctx.as_ref().map(|c| c.bundle_id.as_str()) {
                if let Ok(rows) = self.db.top_corrections_for_app(bundle, 200) {
                    cleaned = apply_replacements(&cleaned, &rows);
                }
            }
        }

        // Phase 3 voice commands + Phase 6 snippet expansion. Pull snippets
        // from the DB on every dictation; the table is small and the lookup is
        // an indexed scan. Pre-fetching the user selection only happens if a
        // snippet's expansion actually references {{selection}} — the copy
        // trick costs ~400 ms.
        let snippets: Vec<Snippet> = match self.db.list_snippets() {
            Ok(rows) => rows
                .into_iter()
                .map(|s| Snippet {
                    trigger: s.trigger,
                    expansion: s.expansion,
                    is_dynamic: s.is_dynamic,
                })
                .collect(),
            Err(e) => {
                eprintln!("[typwrtr] list_snippets failed: {}", e);
                Vec::new()
            }
        };
        let needs_selection = snippets
            .iter()
            .any(|s| s.is_dynamic && s.expansion.contains("{{selection}}"));
        let preloaded_selection = if needs_selection {
            tokio::task::spawn_blocking(crate::copy::capture_selection)
                .await
                .ok()
                .and_then(|r| r.ok())
                .flatten()
        } else {
            None
        };
        let resolver = move |key: &str| -> Option<String> {
            use chrono::Local;
            match key {
                "date" => Some(Local::now().format("%Y-%m-%d").to_string()),
                "time" => Some(Local::now().format("%H:%M").to_string()),
                "day" => Some(Local::now().format("%A").to_string()),
                "clipboard" => arboard::Clipboard::new()
                    .ok()
                    .and_then(|mut cb| cb.get_text().ok()),
                "selection" => preloaded_selection.clone(),
                _ => None,
            }
        };
        let cmd_result = apply_voice_commands_with_snippets(&cleaned, &snippets, &resolver);
        cleaned = cmd_result.text;

        // Phase 4.1 postprocess — profile selects the mode; `code` only fires
        // when the user said the activator.
        let pp_mode = profile
            .map(|p| Mode::from_str(&p.postprocess_mode))
            .unwrap_or(Mode::Default);
        let pp_case = profile
            .map(|p| CodeCase::from_str(&p.code_case))
            .unwrap_or(CodeCase::Snake);
        cleaned = postprocess::apply(&cleaned, pp_mode, cmd_result.code_mode, pp_case);

        // Phase 4.2 optional LLM cleanup — final pass, time-budgeted.
        let backend = CleanupBackend::from_str(&settings.llm_cleanup);
        if backend != CleanupBackend::Off && !cleaned.is_empty() {
            cleaned = llm_cleanup(
                backend,
                &settings.groq_api_key,
                &cleaned,
                Duration::from_millis(800),
            )
            .await;
        }

        // Final caption — overlay shows this for 500 ms then hides.
        let _ = app.emit("transcription://final", &cleaned);

        if !cleaned.is_empty() {
            if cmd_result.clipboard_only {
                // "clipboard instead" — set clipboard, skip the paste keystroke.
                let mut clipboard = arboard::Clipboard::new().map_err(|e| e.to_string())?;
                clipboard.set_text(&cleaned).map_err(|e| e.to_string())?;
                let _ = app.emit(
                    "toast",
                    "Copied to clipboard (skipped paste — said 'clipboard instead').",
                );
            } else {
                paste_text(&cleaned)?;
            }
        }

        let latency_ms = started.elapsed().as_millis() as i64;

        if settings.save_transcriptions && !cleaned.is_empty() && !app_disabled {
            let model_label = match settings.engine.as_str() {
                "local" => effective_model.clone(),
                "cloud" => "groq-whisper-large-v3-turbo".to_string(),
                other => other.to_string(),
            };
            let now_secs = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);

            let insert_result = self.db.insert_transcription(NewTranscription {
                created_at: now_secs,
                audio_path: audio_path.as_deref(),
                raw_text: &raw_text,
                cleaned_text: &cleaned,
                app_bundle_id: ctx.as_ref().map(|c| c.bundle_id.as_str()),
                app_title: ctx.as_ref().and_then(|c| c.window_title.as_deref()),
                model: &model_label,
                duration_ms,
                latency_ms,
                source: settings.engine.as_str(),
            });

            match insert_result {
                Ok(transcription_id) => {
                    // Tell the main window to refresh Learning tab counts
                    // without polling.
                    let _ = app.emit("learning://changed", 1u32);
                    if !cmd_result.clipboard_only {
                        schedule_auto_correction_check(
                            app.clone(),
                            self.db.clone(),
                            transcription_id,
                            cleaned.clone(),
                            ctx.clone(),
                        );
                    }
                }
                Err(e) => eprintln!("[typwrtr] Failed to log transcription: {}", e),
            }
        } else if let Some(path) = audio_path.as_ref() {
            // We persisted audio above but won't be inserting a row to point
            // at it. Drop the orphan rather than leak it.
            let _ = std::fs::remove_file(path);
        }

        {
            let mut state = self.state.lock().unwrap();
            *state = RecordingState::Ready;
            let _ = app.emit("recording-state", RecordingState::Ready);
            update_overlay(app, &RecordingState::Ready);
        }

        Ok(cleaned)
    }
}

/// Watcher tick interval. Short enough to feel responsive when the user
/// finishes editing; long enough that the focused-text capture (a UI
/// Automation cross-process call) is dirt-cheap on a per-second basis.
const AUTO_LEARN_TICK_MS: u64 = 1_500;
/// Hard cap on watcher lifetime per paste. 60 s comfortably covers the
/// "user typed a quick fix" case; longer than that and the user has
/// genuinely moved on.
const AUTO_LEARN_MAX_LIFETIME_MS: u64 = 60_000;
/// Tightened from the previous 0.78. Below 0.85 is "vaguely related text",
/// not "the same paragraph with edits."
const AUTO_LEARN_MIN_RATIO: f64 = 0.85;
const AUTO_LEARN_MAX_CAPTURE_CHARS: usize = 10_000;
const AUTO_LEARN_MAX_PAIR_COUNT: usize = 8;
/// Number of consecutive ticks where the captured text is unchanged to
/// count as "user paused editing." 2 ticks ≈ 3 seconds of idle.
const AUTO_LEARN_IDLE_TICKS: u32 = 2;
/// Bail out after this many consecutive captures that clearly aren't the
/// user's edited paste (anchor guard rejects on non-empty text). Apps like
/// VS Code, Cursor, and other Electron editors expose only their status
/// bar / palette via UI Automation, so polling them never converges. Toast
/// the user once and stop wasting cycles.
const AUTO_LEARN_MAX_ANCHOR_REJECTS: u32 = 5;

#[derive(Debug, serde::Serialize, Clone)]
struct AutoLearnApplied {
    count: u32,
    app: String,
    sample_wrong: String,
    sample_right: String,
}

fn schedule_auto_correction_check(
    app: AppHandle,
    db: Arc<Db>,
    transcription_id: i64,
    cleaned_text: String,
    paste_ctx: Option<context::AppContext>,
) {
    let Some(paste_ctx) = paste_ctx else {
        eprintln!("[autolearn] no paste_ctx — skipping watcher");
        return;
    };
    if cleaned_text.trim().len() < 4 {
        eprintln!(
            "[autolearn] cleaned_text too short ({} chars) — skipping",
            cleaned_text.trim().len()
        );
        return;
    }

    // Per-app gate — same as the manual fix-up flow at the top of
    // stop_and_transcribe. If the user has flipped Learning off for this
    // app, neither path writes corrections.
    if let Ok(Some(profile)) = db.get_app_profile(&paste_ctx.bundle_id) {
        if !profile.enabled {
            eprintln!(
                "[autolearn] profile {} has Learning disabled — skipping",
                paste_ctx.bundle_id
            );
            return;
        }
    }

    println!(
        "[autolearn] watcher spawned for `{}` (paste len={} chars, target={})",
        cleaned_text.chars().take(60).collect::<String>(),
        cleaned_text.chars().count(),
        paste_ctx.bundle_id,
    );

    tauri::async_runtime::spawn(async move {
        let started = std::time::Instant::now();
        let max_lifetime = Duration::from_millis(AUTO_LEARN_MAX_LIFETIME_MS);
        let tick = Duration::from_millis(AUTO_LEARN_TICK_MS);

        // Most-recent text we observed while still focused on the paste
        // target. Used as the finalize input both on idle and on focus loss.
        let mut last_in_target: Option<String> = None;
        let mut prev_capture: Option<String> = None;
        let mut idle_ticks: u32 = 0;
        let mut focus_loss_pending_finalize = false;
        let mut tick_no: u32 = 0;
        let mut consec_anchor_rejects: u32 = 0;

        while started.elapsed() < max_lifetime {
            tokio::time::sleep(tick).await;
            tick_no += 1;

            let current_ctx = context::current();
            // Bundle-ID alone is enough to recognise "still in the same app".
            // The window title shifts when the user types (Notepad adds a
            // modified marker, browsers update tab titles, etc.) so checking
            // it caused false focus-loss on every keystroke.
            let same_target = current_ctx
                .as_ref()
                .is_some_and(|c| c.bundle_id == paste_ctx.bundle_id);

            if !same_target {
                println!(
                    "[autolearn] tick {}: focus is in {:?} (paste was in {}); waiting for return or finalize",
                    tick_no,
                    current_ctx.as_ref().map(|c| c.bundle_id.as_str()),
                    paste_ctx.bundle_id,
                );
                // Only finalize-on-focus-loss if we already saw an in-target
                // edit. Otherwise let the user come back — the 60 s cap will
                // end the watcher eventually if they don't.
                let have_pending_edit = last_in_target.as_ref().is_some_and(|t| {
                    normalize_for_match(t) != normalize_for_match(&cleaned_text)
                });
                if have_pending_edit {
                    println!("[autolearn] tick {}: have a pending edit, finalizing now", tick_no);
                    focus_loss_pending_finalize = true;
                    break;
                }
                continue;
            }

            let captured =
                match tokio::task::spawn_blocking(focused_text::capture_focused_text).await {
                    Ok(Ok(Some(t))) => t.text,
                    Ok(Ok(None)) => {
                        println!("[autolearn] tick {}: capture returned None (no focused text element)", tick_no);
                        continue;
                    }
                    Ok(Err(e)) => {
                        eprintln!("[autolearn] tick {}: focused text read failed: {}", tick_no, e);
                        continue;
                    }
                    Err(e) => {
                        eprintln!("[autolearn] tick {}: focused text join failed: {}", tick_no, e);
                        continue;
                    }
                };

            let preview: String = captured.chars().take(80).collect();
            println!(
                "[autolearn] tick {}: captured {} chars: `{}`",
                tick_no,
                captured.chars().count(),
                preview,
            );

            // Anchor guard: the captured field must contain a recognisable
            // chunk of the original paste, otherwise the user is typing into
            // a different field that just happens to be focused (or the app
            // doesn't expose its editor text via UIA — VS Code, Cursor, and
            // other Electron editors hand back the status bar instead).
            if !has_shared_anchor(&cleaned_text, &captured) {
                consec_anchor_rejects += 1;
                println!(
                    "[autolearn] tick {}: anchor guard rejected ({}/{}) — captured doesn't contain a {}-char chunk of the paste",
                    tick_no,
                    consec_anchor_rejects,
                    AUTO_LEARN_MAX_ANCHOR_REJECTS,
                    (cleaned_text.chars().count() / 3).max(8),
                );
                if consec_anchor_rejects >= AUTO_LEARN_MAX_ANCHOR_REJECTS {
                    println!(
                        "[autolearn] giving up — `{}` doesn't expose editor text via UIA. \
                         Use the fix-up hotkey to teach typwrtr in this app.",
                        paste_ctx.bundle_id,
                    );
                    let _ = app.emit(
                        "toast",
                        format!(
                            "Auto-learn can't read text in {}. Use the fix-up hotkey to teach typwrtr edits there.",
                            paste_ctx.display_name,
                        ),
                    );
                    return;
                }
                continue;
            }
            consec_anchor_rejects = 0;

            last_in_target = Some(captured.clone());

            // Idle-debounce: two consecutive captures match → user paused.
            // If the captured text differs from the original cleaned text,
            // finalize now.
            let unchanged_since_prev = prev_capture
                .as_deref()
                .map(|p| p == captured.as_str())
                .unwrap_or(false);
            if unchanged_since_prev {
                idle_ticks += 1;
                println!("[autolearn] tick {}: idle tick {}/{}", tick_no, idle_ticks, AUTO_LEARN_IDLE_TICKS);
            } else {
                if idle_ticks > 0 {
                    println!("[autolearn] tick {}: edits resumed, idle counter reset", tick_no);
                }
                idle_ticks = 0;
            }
            prev_capture = Some(captured.clone());

            if idle_ticks >= AUTO_LEARN_IDLE_TICKS {
                let differs = normalize_for_match(&captured) != normalize_for_match(&cleaned_text);
                println!(
                    "[autolearn] tick {}: idle threshold reached (differs={})",
                    tick_no, differs
                );
                if differs {
                    if try_finalize_auto_learn(
                        &app,
                        &db,
                        transcription_id,
                        &cleaned_text,
                        &captured,
                        &paste_ctx,
                    ) {
                        return;
                    }
                }
                // Idle but no qualifying edit (or the gate rejected). Stop —
                // the user clearly isn't going to edit further; keep polling
                // wastes cycles.
                return;
            }
        }

        // Ran out of lifetime, or focus was lost. Try to finalize on the
        // last in-target capture.
        if focus_loss_pending_finalize || started.elapsed() >= max_lifetime {
            if let Some(captured) = last_in_target {
                try_finalize_auto_learn(
                    &app,
                    &db,
                    transcription_id,
                    &cleaned_text,
                    &captured,
                    &paste_ctx,
                );
            }
        }
    });
}

/// Run the two-stage gate, write corrections if the diff qualifies, and
/// emit the user-visible toast event. Returns `true` if at least one
/// correction was written (caller should stop polling).
fn try_finalize_auto_learn(
    app: &AppHandle,
    db: &Arc<Db>,
    transcription_id: i64,
    cleaned_text: &str,
    captured: &str,
    paste_ctx: &context::AppContext,
) -> bool {
    let Some(final_text) = candidate_edited_text(cleaned_text, captured) else {
        println!("[autolearn] finalize: candidate_edited_text returned None (ratio < {} or no plausible substring)", AUTO_LEARN_MIN_RATIO);
        return false;
    };
    println!(
        "[autolearn] finalize: candidate matched — final_text=`{}`",
        final_text.chars().take(80).collect::<String>()
    );
    let pairs = crate::learning::diff::pairs_from_diff(cleaned_text, &final_text, 4);
    println!("[autolearn] finalize: diff produced {} pair(s)", pairs.len());
    if pairs.is_empty() || pairs.len() > AUTO_LEARN_MAX_PAIR_COUNT {
        println!("[autolearn] finalize: pair count out of range (0..={}), skipping", AUTO_LEARN_MAX_PAIR_COUNT);
        return false;
    }
    // Reject if any single pair replaces more than half the original — that
    // pattern is "user wiped and re-typed", not "user fixed a typo".
    let half = (cleaned_text.chars().count() / 2).max(8);
    if let Some(p) = pairs
        .iter()
        .find(|p| p.wrong.chars().count() > half || p.right.chars().count() > half)
    {
        println!(
            "[autolearn] finalize: longest-pair-half guard rejected (pair `{}` -> `{}` exceeds half={})",
            p.wrong, p.right, half
        );
        return false;
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    match apply_correction_pairs(
        db,
        transcription_id,
        cleaned_text,
        &final_text,
        Some(&paste_ctx.bundle_id),
        now,
        "auto",
    ) {
        Ok(applied) if applied > 0 => {
            println!(
                "[typwrtr] Auto-learned {} correction(s) from edit in {}",
                applied, paste_ctx.bundle_id
            );
            let _ = app.emit("learning://changed", applied);
            // Pick the largest non-empty pair as the toast sample.
            let sample = pairs
                .iter()
                .filter(|p| !p.right.trim().is_empty())
                .max_by_key(|p| p.right.len())
                .or_else(|| pairs.first());
            if let Some(p) = sample {
                let _ = app.emit(
                    "auto-learn://applied",
                    AutoLearnApplied {
                        count: applied,
                        app: paste_ctx.display_name.clone(),
                        sample_wrong: p.wrong.clone(),
                        sample_right: p.right.clone(),
                    },
                );
            }
            true
        }
        Ok(_) => false,
        Err(e) => {
            eprintln!("[typwrtr] Auto-learn correction write failed: {}", e);
            false
        }
    }
}

/// Cheap false-positive guard. The captured field must contain at least one
/// contiguous chunk of `cleaned_text` of length max(8, cleaned_len / 3) — if
/// the user has typed something completely unrelated after pasting, no anchor
/// matches and we reject the capture before running the expensive ratio
/// check. Edits like "Bob → Robert" leave anchors like "send the report to"
/// intact so the typical correction path passes.
fn has_shared_anchor(cleaned: &str, captured: &str) -> bool {
    let cleaned = cleaned.trim();
    let captured = captured.trim();
    if cleaned.is_empty() || captured.is_empty() {
        return false;
    }
    let cleaned_len = cleaned.chars().count();
    if cleaned_len <= 8 {
        return captured.contains(cleaned);
    }
    let anchor_chars = (cleaned_len / 3).max(8);

    // Walk char boundaries by `anchor_chars / 2` so anchors overlap by ~50 %.
    let chars: Vec<(usize, char)> = cleaned.char_indices().collect();
    let step = (anchor_chars / 2).max(1);
    let mut i = 0;
    while i + anchor_chars <= chars.len() {
        let start = chars[i].0;
        let end = if i + anchor_chars < chars.len() {
            chars[i + anchor_chars].0
        } else {
            cleaned.len()
        };
        let anchor = &cleaned[start..end];
        if !anchor.trim().is_empty() && captured.contains(anchor) {
            return true;
        }
        i += step;
    }
    false
}

fn candidate_edited_text(cleaned_text: &str, captured_text: &str) -> Option<String> {
    let cleaned = normalize_for_match(cleaned_text);
    if cleaned.is_empty() {
        return None;
    }

    let captured_trimmed = captured_text.trim();
    if captured_trimmed.is_empty()
        || captured_trimmed.chars().count() > AUTO_LEARN_MAX_CAPTURE_CHARS
    {
        return None;
    }

    let mut candidates = vec![captured_trimmed.to_string()];
    candidates.extend(
        captured_trimmed
            .split(['\n', '\r'])
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToString::to_string),
    );
    candidates.extend(
        captured_trimmed
            .split("\n\n")
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToString::to_string),
    );

    let cleaned_len = cleaned.chars().count();
    candidates
        .into_iter()
        .filter(|c| {
            let len = normalize_for_match(c).chars().count();
            len >= cleaned_len.saturating_div(2) && len <= cleaned_len.saturating_mul(2).max(16)
        })
        .filter_map(|c| {
            let normalized = normalize_for_match(&c);
            if normalized == cleaned {
                return None;
            }
            let ratio = similar_ratio(&cleaned, &normalized);
            if ratio >= AUTO_LEARN_MIN_RATIO {
                Some((ratio, c))
            } else {
                None
            }
        })
        .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(_, c)| c)
}

fn normalize_for_match(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[allow(dead_code)] // kept for future re-introduction — see auto-learn watcher
fn window_titles_match(current: Option<&str>, pasted: Option<&str>) -> bool {
    match (current, pasted) {
        (Some(a), Some(b)) => {
            let a = a.trim().trim_start_matches('*');
            let b = b.trim().trim_start_matches('*');
            a == b || a.contains(b) || b.contains(a)
        }
        _ => true,
    }
}

/// Whisper.cpp's prompt budget is roughly 224 tokens. English averages ~4
/// chars/token, so 800 chars stays comfortably inside even a noisy budget while
/// leaving room for the user-supplied prompt.
const PROMPT_BUDGET_CHARS: usize = 800;

/// Append `extras` to `base`, separating with spaces, until the total reaches
/// `budget` chars. Extras are deduped (case-sensitive) so spamming the same
/// term across vocab+corrections doesn't waste budget.
fn budgeted_join(base: &str, extras: &[String], budget: usize) -> String {
    let base = base.trim();
    let mut out = base.to_string();
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for s in base.split_whitespace() {
        seen.insert(s);
    }
    for term in extras {
        let t = term.trim();
        if t.is_empty() || seen.contains(t) {
            continue;
        }
        let needed = if out.is_empty() { t.len() } else { 1 + t.len() };
        if out.len() + needed > budget {
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(t);
        seen.insert(t);
    }
    out
}

/// Apply learned corrections to a transcript. We only consider rows with
/// `count >= 3` (the plan's threshold) and substitute case-insensitively in
/// whole-word position. The `context` field is *advisory*; if it exists we
/// require *some* token from it to also appear in the surrounding text — that
/// guards against blindly replacing a homophone that happens to match.
fn apply_replacements(text: &str, rows: &[crate::db::CorrectionRow]) -> String {
    let mut out = text.to_string();
    for r in rows {
        if r.count < 3 || r.wrong.is_empty() || r.right.is_empty() {
            continue;
        }
        let context_ok = match r.context.as_deref() {
            Some(ctx) if !ctx.trim().is_empty() => {
                let hay_lower = out.to_lowercase();
                ctx.split_whitespace()
                    .map(|w| w.trim_matches(|c: char| c.is_ascii_punctuation()))
                    .filter(|w| {
                        w.len() >= 4
                            && !crate::learning::diff::STOPWORDS
                                .iter()
                                .any(|sw| sw.eq_ignore_ascii_case(w))
                    })
                    .any(|w| hay_lower.contains(&w.to_lowercase()))
            }
            _ => true,
        };
        if !context_ok {
            continue;
        }
        out = case_insensitive_word_replace(&out, &r.wrong, &r.right);
    }
    out
}

fn case_insensitive_word_replace(haystack: &str, needle: &str, replacement: &str) -> String {
    let needle_lower = needle.to_lowercase();
    if needle_lower.is_empty() {
        return haystack.to_string();
    }
    let hay_lower = haystack.to_lowercase();
    let mut out = String::with_capacity(haystack.len());
    let mut i = 0usize;
    let bytes = haystack.as_bytes();
    let n_len = needle_lower.len();
    while i < haystack.len() {
        if i + n_len <= haystack.len() && &hay_lower[i..i + n_len] == needle_lower {
            // Word-boundary check on both sides.
            let prev_ok = i == 0 || !bytes[i - 1].is_ascii_alphanumeric() && bytes[i - 1] != b'_';
            let next_ok = i + n_len == haystack.len()
                || !bytes[i + n_len].is_ascii_alphanumeric() && bytes[i + n_len] != b'_';
            if prev_ok && next_ok {
                out.push_str(replacement);
                i += n_len;
                continue;
            }
        }
        // Push the next UTF-8 char and advance.
        let ch_end = next_char_boundary(haystack, i);
        out.push_str(&haystack[i..ch_end]);
        i = ch_end;
    }
    out
}

fn next_char_boundary(s: &str, i: usize) -> usize {
    let mut j = i + 1;
    while j < s.len() && !s.is_char_boundary(j) {
        j += 1;
    }
    j
}

/// Combine an app-profile prompt template with the global initial prompt.
/// Profile template comes first because per-app vocabulary is the more specific
/// signal; the global prompt is a baseline.
fn merge_prompt(profile_template: Option<&str>, global: &str) -> String {
    let pt = profile_template.unwrap_or("").trim();
    let g = global.trim();
    match (pt.is_empty(), g.is_empty()) {
        (true, true) => String::new(),
        (false, true) => pt.to_string(),
        (true, false) => g.to_string(),
        (false, false) => format!("{} {}", pt, g),
    }
}

/// Write the 16 kHz mono WAV under `<app_dir>/audio/<unix_ms>.wav`. Returns the
/// path on success. Any error is bubbled up so the caller can decide whether to
/// silently drop or surface (today: log + treat as no audio).
fn persist_audio_clip(samples: &[f32], app_dir: &PathBuf) -> Result<Option<PathBuf>, String> {
    let dir = app_dir.join("audio");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let path = dir.join(format!("{}.wav", ts));
    let bytes = audio::encode_wav_16k_mono(samples)?;
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    Ok(Some(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_db() -> Arc<Db> {
        Arc::new(Db::open_at(std::path::Path::new(":memory:")).unwrap())
    }

    #[test]
    fn test_initial_state_is_ready() {
        let recorder = Recorder::new(Arc::new(LocalTranscriber::new()), fresh_db());
        assert_eq!(recorder.get_state(), RecordingState::Ready);
    }

    use crate::db::CorrectionRow;

    fn cr(wrong: &str, right: &str, count: i64, context: Option<&str>) -> CorrectionRow {
        CorrectionRow {
            id: 1,
            wrong: wrong.into(),
            right: right.into(),
            context: context.map(|s| s.into()),
            app_bundle_id: Some("code".into()),
            count,
            last_seen_at: 0,
            source: "manual".into(),
        }
    }

    #[test]
    fn replacements_require_min_count_3() {
        let rows = vec![cr("Kaidhar", "KD", 2, None)];
        assert_eq!(
            apply_replacements("send to Kaidhar tomorrow", &rows),
            "send to Kaidhar tomorrow"
        );
    }

    #[test]
    fn replacements_apply_case_insensitively_with_word_boundaries() {
        let rows = vec![cr("Kaidhar", "KD", 3, None)];
        assert_eq!(
            apply_replacements("send to Kaidhar tomorrow", &rows),
            "send to KD tomorrow"
        );
        // Word-boundary: don't munge "Kaidhardian" or "preKaidhar" if they ever appear.
        assert_eq!(
            apply_replacements("Kaidhardian things", &rows),
            "Kaidhardian things"
        );
    }

    #[test]
    fn replacements_respect_context_filter() {
        // Context demands one of {report, send}; "ship the Kaidhar" doesn't qualify.
        let rows = vec![cr(
            "Kaidhar",
            "KD",
            5,
            Some("send the report to KD tomorrow"),
        )];
        assert_eq!(
            apply_replacements("ship the Kaidhar plan", &rows),
            "ship the Kaidhar plan"
        );
        assert_eq!(
            apply_replacements("send the report to Kaidhar", &rows),
            "send the report to KD"
        );
    }

    #[test]
    fn auto_learn_candidate_accepts_related_manual_edit() {
        let candidate = candidate_edited_text(
            "Please send this to John Smith tomorrow.",
            "Please send this to Jon Smyth tomorrow.",
        );
        assert_eq!(
            candidate.as_deref(),
            Some("Please send this to Jon Smyth tomorrow.")
        );
    }

    #[test]
    fn auto_learn_candidate_ignores_unchanged_text() {
        assert!(candidate_edited_text("No changes here.", "No changes here.").is_none());
    }

    #[test]
    fn auto_learn_candidate_ignores_unrelated_text() {
        assert!(candidate_edited_text(
            "Please send this to John Smith tomorrow.",
            "Completely unrelated text from a different field."
        )
        .is_none());
    }

    #[test]
    fn auto_learn_window_title_allows_dirty_marker() {
        assert!(window_titles_match(
            Some("*Notes - Project"),
            Some("Notes - Project")
        ));
        assert!(!window_titles_match(Some("Other"), Some("Notes - Project")));
    }

    #[test]
    fn shared_anchor_accepts_edited_paste() {
        // Bob → Robert; the rest is intact and should match the anchor walk.
        assert!(has_shared_anchor(
            "Please send the report to Bob tomorrow.",
            "Please send the report to Robert tomorrow.",
        ));
    }

    #[test]
    fn shared_anchor_rejects_unrelated_text() {
        assert!(!has_shared_anchor(
            "Please send the report to Bob tomorrow.",
            "What is the meaning of life and other questions?",
        ));
    }

    #[test]
    fn shared_anchor_short_cleaned_uses_substring_match() {
        assert!(has_shared_anchor("hello", "say hello there"));
        assert!(!has_shared_anchor("hello", "good morning"));
    }

    #[test]
    fn budgeted_join_dedupes_and_trims() {
        let extras = vec!["rusqlite".into(), "Tauri".into(), "rusqlite".into()];
        let out = budgeted_join("hello", &extras, 100);
        assert_eq!(out, "hello rusqlite Tauri");
    }

    #[test]
    fn budgeted_join_respects_budget() {
        let extras = vec!["aaaa".into(), "bbbb".into(), "cccc".into()];
        let out = budgeted_join("", &extras, 9); // fits "aaaa bbbb" (9 chars), not "cccc"
        assert_eq!(out, "aaaa bbbb");
    }

    #[test]
    fn merge_prompt_combinations() {
        assert_eq!(merge_prompt(None, ""), "");
        assert_eq!(merge_prompt(Some(""), ""), "");
        assert_eq!(merge_prompt(Some("rusqlite"), ""), "rusqlite");
        assert_eq!(merge_prompt(None, "global"), "global");
        assert_eq!(merge_prompt(Some("rusqlite"), "global"), "rusqlite global");
        // Whitespace-only treated as empty.
        assert_eq!(merge_prompt(Some("   "), "global"), "global");
    }
}
