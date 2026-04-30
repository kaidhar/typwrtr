#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Emitter, Listener, Manager, State, WebviewUrl, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

use typwrtr_lib::audio;
use typwrtr_lib::context;
use typwrtr_lib::clipboard::copy;
use typwrtr_lib::db::{
    AppProfileRow, CorrectionRow, Db, DbHealth, RecentTranscription, SnippetRow, VocabularyRow,
};
use typwrtr_lib::downloader;
use typwrtr_lib::learning::apply_correction_pairs;
use typwrtr_lib::recorder::{Recorder, RecordingState};
use typwrtr_lib::settings::Settings;
use typwrtr_lib::transcribe_local::{self, LocalTranscriber};

struct AppState {
    recorder: Recorder,
    settings: Mutex<Settings>,
    registered_hotkeys: Mutex<RegisteredHotkeys>,
    hotkey_capture_active: Mutex<bool>,
    app_dir: PathBuf,
    db: Arc<Db>,
    /// Holds the most recent FixupStart so the fix-up window can pull it on
    /// mount even if the hotkey fired before its listener was attached.
    pending_fixup: Mutex<Option<FixupStart>>,
}

#[derive(Default)]
struct RegisteredHotkeys {
    toggle_hotkey: Option<String>,
    push_to_talk_hotkey: Option<String>,
    fixup_hotkey: Option<String>,
}

fn show_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        if let Err(e) = window.show() {
            eprintln!("[typwrtr] Failed to show main window: {}", e);
        }
        if let Err(e) = window.set_focus() {
            eprintln!("[typwrtr] Failed to focus main window: {}", e);
        }
    }
}

fn get_app_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("com.typwrtr.app")
}

fn model_storage_dir(settings: &Settings, app_dir: &PathBuf) -> PathBuf {
    if settings.model_dir.trim().is_empty() {
        app_dir.clone()
    } else {
        PathBuf::from(settings.model_dir.trim())
    }
}

#[tauri::command]
fn set_hotkey_capture_active(state: State<AppState>, active: bool) {
    *state.hotkey_capture_active.lock().unwrap() = active;
}

#[tauri::command]
fn get_settings(state: State<AppState>) -> Settings {
    state.settings.lock().unwrap().clone()
}

#[tauri::command]
fn save_settings(
    app: tauri::AppHandle,
    state: State<AppState>,
    settings: Settings,
) -> Result<(), String> {
    if settings.toggle_hotkey == settings.push_to_talk_hotkey
        || settings.toggle_hotkey == settings.fixup_hotkey
        || settings.push_to_talk_hotkey == settings.fixup_hotkey
    {
        return Err("Toggle, push-to-talk, and fix-up hotkeys must all be different".to_string());
    }

    settings.save(&state.app_dir)?;
    *state.settings.lock().unwrap() = settings;
    register_hotkeys(&app, state.inner())?;
    Ok(())
}

#[tauri::command]
fn list_microphones() -> Vec<audio::MicDevice> {
    audio::list_microphones()
}

#[tauri::command]
fn get_recording_state(state: State<AppState>) -> RecordingState {
    state.recorder.get_state()
}

/// Returns the resolved model directory — either the user override from
/// settings (when set) or the default app config path. The Engine tab shows
/// this whenever the override field is blank so the user knows where models
/// will land.
#[tauri::command]
fn resolved_model_dir(state: State<AppState>) -> String {
    let settings = state.settings.lock().unwrap().clone();
    model_storage_dir(&settings, &state.app_dir)
        .to_string_lossy()
        .to_string()
}

#[tauri::command]
fn check_model_downloaded(state: State<AppState>, model_size: String) -> bool {
    let settings = state.settings.lock().unwrap().clone();
    let model_dir = model_storage_dir(&settings, &state.app_dir);
    let model_file = transcribe_local::model_filename(&model_size);
    model_dir.join(&model_file).exists()
}

#[tauri::command]
async fn download_model(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    model_size: String,
) -> Result<(), String> {
    let settings = state.settings.lock().unwrap().clone();
    let model_dir = model_storage_dir(&settings, &state.app_dir);
    let url = transcribe_local::model_download_url(&model_size);
    let model_file = transcribe_local::model_filename(&model_size);
    let dest = model_dir.join(&model_file);
    downloader::download_model(app, &url, &dest).await
}

#[tauri::command]
fn db_health(state: State<AppState>) -> Result<DbHealth, String> {
    state.db.health()
}

#[tauri::command]
fn wipe_learning_data(state: State<AppState>) -> Result<(), String> {
    state.db.wipe_learning_data()?;
    // Drop any retained audio clips alongside the DB rows that pointed at them.
    let audio_dir = state.app_dir.join("audio");
    if audio_dir.exists() {
        if let Err(e) = std::fs::remove_dir_all(&audio_dir) {
            // Don't fail the whole wipe — the DB is the canonical store. Log
            // and let the user retry from disk if it matters.
            eprintln!(
                "[typwrtr] Failed to remove audio dir {:?}: {}",
                audio_dir, e
            );
        }
    }
    Ok(())
}

#[tauri::command]
fn list_app_profiles(state: State<AppState>) -> Result<Vec<AppProfileRow>, String> {
    state.db.list_app_profiles()
}

#[tauri::command]
fn save_app_profile(state: State<AppState>, profile: AppProfileRow) -> Result<(), String> {
    state.db.upsert_app_profile(&profile)
}

#[tauri::command]
fn delete_app_profile(state: State<AppState>, bundle_id: String) -> Result<(), String> {
    state.db.delete_app_profile(&bundle_id)
}

/// Result returned to the fix-up window when the hotkey fires. Either the user
/// had a selection that fuzzy-matched a recent transcription, or we report the
/// failure mode so the UI can show the right toast.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum FixupStart {
    Match {
        transcription: RecentTranscription,
        selection: String,
    },
    NoSelection,
    NoMatch {
        selection: String,
    },
}

#[tauri::command]
fn take_pending_fixup(state: State<AppState>) -> Option<FixupStart> {
    state.pending_fixup.lock().unwrap().take()
}

#[tauri::command]
async fn start_fixup(state: State<'_, AppState>) -> Result<FixupStart, String> {
    // Sniffing the foreground app *before* we synthesise Cmd+C — once we
    // surface our own window the focus has already shifted.
    let ctx = context::current();
    let bundle = ctx.as_ref().map(|c| c.bundle_id.clone());

    let captured = tokio::task::spawn_blocking(copy::capture_selection)
        .await
        .map_err(|e| format!("copy join: {}", e))??;
    let Some(selection) = captured
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    else {
        return Ok(FixupStart::NoSelection);
    };

    let cutoff = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
        - 30 * 60; // 30-minute window per the plan
    let m = state
        .db
        .find_recent_match(&selection, bundle.as_deref(), cutoff, 50, 0.6)?;
    Ok(match m {
        Some(t) => FixupStart::Match {
            transcription: t,
            selection,
        },
        None => FixupStart::NoMatch { selection },
    })
}

#[tauri::command]
fn save_correction(
    app: tauri::AppHandle,
    state: State<AppState>,
    transcription_id: i64,
    final_text: String,
    cleaned_text: String,
    app_bundle_id: Option<String>,
) -> Result<u32, String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let applied = apply_correction_pairs(
        &state.db,
        transcription_id,
        &cleaned_text,
        &final_text,
        app_bundle_id.as_deref(),
        now,
        "manual",
    )?;
    if applied > 0 {
        let _ = app.emit("learning://changed", applied);
    }
    Ok(applied)
}

#[tauri::command]
fn list_top_corrections(state: State<AppState>, limit: i64) -> Result<Vec<CorrectionRow>, String> {
    state.db.top_corrections_global(limit)
}

#[tauri::command]
fn list_top_vocabulary(state: State<AppState>, limit: i64) -> Result<Vec<VocabularyRow>, String> {
    state.db.top_vocab_combined(limit)
}

/// Convert a UI-friendly "since N days" filter into a unix-secs threshold.
/// `0` or negative `since_days` means "lifetime" — return 0 so the SQL
/// query filters nothing.
fn since_unix_secs(since_days: i64) -> i64 {
    if since_days <= 0 {
        return 0;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    now.saturating_sub(since_days.saturating_mul(86_400))
}

#[tauri::command]
fn usage_window(
    state: State<AppState>,
    since_days: i64,
) -> Result<typwrtr_lib::db::UsageWindow, String> {
    state.db.usage_window(since_unix_secs(since_days))
}

#[tauri::command]
fn daily_buckets(
    state: State<AppState>,
    days: i64,
) -> Result<Vec<typwrtr_lib::db::DailyBucket>, String> {
    state.db.daily_buckets(days)
}

#[tauri::command]
fn app_breakdown(
    state: State<AppState>,
    since_days: i64,
    limit: i64,
) -> Result<Vec<typwrtr_lib::db::AppBreakdown>, String> {
    state.db.app_breakdown(since_unix_secs(since_days), limit)
}

#[tauri::command]
fn forget_correction(app: tauri::AppHandle, state: State<AppState>, id: i64) -> Result<(), String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    state.db.forget_correction(id, now)?;
    let _ = app.emit("learning://changed", 0);
    Ok(())
}

#[tauri::command]
fn list_snippets(state: State<AppState>) -> Result<Vec<SnippetRow>, String> {
    state.db.list_snippets()
}

#[tauri::command]
fn save_snippet(state: State<AppState>, snippet: SnippetRow) -> Result<i64, String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    state.db.upsert_snippet(&snippet, now)
}

#[tauri::command]
fn delete_snippet(state: State<AppState>, id: i64) -> Result<(), String> {
    state.db.delete_snippet(id)
}

#[tauri::command]
fn forget_vocabulary(app: tauri::AppHandle, state: State<AppState>, id: i64) -> Result<(), String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    state.db.forget_vocabulary(id, now)?;
    let _ = app.emit("learning://changed", 0);
    Ok(())
}

#[tauri::command]
async fn toggle_recording(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<String, String> {
    do_toggle_recording(&app, &state).await
}

fn register_toggle_shortcut(app: &tauri::AppHandle, shortcut: &str) -> Result<(), String> {
    let toggle_handle = app.clone();
    app.global_shortcut()
        .on_shortcut(shortcut, move |_app, _shortcut, event| {
            if *toggle_handle
                .state::<AppState>()
                .hotkey_capture_active
                .lock()
                .unwrap()
            {
                return;
            }

            if event.state != ShortcutState::Pressed {
                return;
            }

            let handle = toggle_handle.clone();
            tauri::async_runtime::spawn(async move {
                let state = handle.state::<AppState>();
                match do_toggle_recording(&handle, state.inner()).await {
                    Ok(result) => println!("[typwrtr] Toggle result: {}", result),
                    Err(e) => eprintln!("[typwrtr] Toggle error: {}", e),
                }
            });
        })
        .map_err(|e| format!("Failed to register toggle hotkey: {}", e))
}

fn register_push_to_talk_shortcut(app: &tauri::AppHandle, shortcut: &str) -> Result<(), String> {
    let ptt_handle = app.clone();
    app.global_shortcut()
        .on_shortcut(shortcut, move |_app, _shortcut, event| {
            if *ptt_handle
                .state::<AppState>()
                .hotkey_capture_active
                .lock()
                .unwrap()
            {
                return;
            }

            let handle = ptt_handle.clone();
            match event.state {
                ShortcutState::Pressed => {
                    tauri::async_runtime::spawn(async move {
                        let state = handle.state::<AppState>();
                        let current = state.recorder.get_state();
                        println!("[typwrtr] PTT pressed, current state: {:?}", current);
                        if current == RecordingState::Ready {
                            let settings = state.settings.lock().unwrap().clone();
                            let model_dir = model_storage_dir(&settings, &state.app_dir);
                            // PTT releases will fire stop_and_transcribe directly,
                            // so we explicitly disable VAD auto-stop for this session.
                            let mut ptt_settings = settings;
                            ptt_settings.vad_silence_ms = 0;
                            match state
                                .recorder
                                .start_recording(&handle, &ptt_settings, &model_dir)
                            {
                                Ok(_) => println!("[typwrtr] Recording started"),
                                Err(e) => eprintln!("[typwrtr] Start recording error: {}", e),
                            }
                        }
                    });
                }
                ShortcutState::Released => {
                    tauri::async_runtime::spawn(async move {
                        let state = handle.state::<AppState>();
                        let current = state.recorder.get_state();
                        if current == RecordingState::Recording {
                            let settings = state.settings.lock().unwrap().clone();
                            let model_dir = model_storage_dir(&settings, &state.app_dir);
                            match state
                                .recorder
                                .stop_and_transcribe(&handle, &settings, &state.app_dir, &model_dir)
                                .await
                            {
                                Ok(result) => println!("[typwrtr] Transcription: {}", result),
                                Err(e) => eprintln!("[typwrtr] Transcription error: {}", e),
                            }
                        }
                    });
                }
            }
        })
        .map_err(|e| format!("Failed to register push-to-talk hotkey: {}", e))
}

fn register_fixup_shortcut(app: &tauri::AppHandle, shortcut: &str) -> Result<(), String> {
    let fixup_handle = app.clone();
    app.global_shortcut()
        .on_shortcut(shortcut, move |_app, _shortcut, event| {
            if *fixup_handle
                .state::<AppState>()
                .hotkey_capture_active
                .lock()
                .unwrap()
            {
                return;
            }
            if event.state != ShortcutState::Pressed {
                return;
            }
            let handle = fixup_handle.clone();
            tauri::async_runtime::spawn(async move {
                let state = handle.state::<AppState>();
                match start_fixup(state.clone()).await {
                    Ok(result) => {
                        // Park the result so the window can pull it on mount —
                        // covers the race where we emit before the listener
                        // is attached (cold-window first launch).
                        *state.pending_fixup.lock().unwrap() = Some(result.clone());
                        if let Some(window) = handle.get_webview_window("fixup") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                        // Also emit so an already-open window updates without
                        // re-mounting.
                        if let Err(e) = handle.emit("fixup://event", &result) {
                            eprintln!("[typwrtr] Failed to emit fixup event: {}", e);
                        }
                    }
                    Err(e) => eprintln!("[typwrtr] Fixup error: {}", e),
                }
            });
        })
        .map_err(|e| format!("Failed to register fix-up hotkey: {}", e))
}

fn register_hotkeys(app: &tauri::AppHandle, state: &AppState) -> Result<(), String> {
    let settings = state.settings.lock().unwrap().clone();
    let mut registered = state.registered_hotkeys.lock().unwrap();
    let old_toggle = registered.toggle_hotkey.clone();
    let old_ptt = registered.push_to_talk_hotkey.clone();
    let old_fixup = registered.fixup_hotkey.clone();

    if let Some(existing) = registered.toggle_hotkey.take() {
        let _ = app.global_shortcut().unregister(existing.as_str());
    }
    if let Some(existing) = registered.push_to_talk_hotkey.take() {
        let _ = app.global_shortcut().unregister(existing.as_str());
    }
    if let Some(existing) = registered.fixup_hotkey.take() {
        let _ = app.global_shortcut().unregister(existing.as_str());
    }

    let toggle_hotkey = settings.toggle_hotkey.clone();
    if let Err(e) = register_toggle_shortcut(app, toggle_hotkey.as_str()) {
        registered.toggle_hotkey = old_toggle.clone();
        registered.push_to_talk_hotkey = old_ptt.clone();
        if let Some(existing) = old_toggle {
            let _ = register_toggle_shortcut(app, existing.as_str());
        }
        if let Some(existing) = old_ptt {
            let _ = register_push_to_talk_shortcut(app, existing.as_str());
        }
        return Err(e);
    }

    let ptt_hotkey = settings.push_to_talk_hotkey.clone();
    if let Err(e) = register_push_to_talk_shortcut(app, ptt_hotkey.as_str()) {
        let _ = app.global_shortcut().unregister(toggle_hotkey.as_str());
        registered.toggle_hotkey = old_toggle.clone();
        registered.push_to_talk_hotkey = old_ptt.clone();
        registered.fixup_hotkey = old_fixup.clone();
        if let Some(existing) = old_toggle {
            let _ = register_toggle_shortcut(app, existing.as_str());
        }
        if let Some(existing) = old_ptt {
            let _ = register_push_to_talk_shortcut(app, existing.as_str());
        }
        if let Some(existing) = old_fixup {
            let _ = register_fixup_shortcut(app, existing.as_str());
        }
        return Err(e);
    }

    let fixup_hotkey = settings.fixup_hotkey.clone();
    if !fixup_hotkey.is_empty() {
        if let Err(e) = register_fixup_shortcut(app, fixup_hotkey.as_str()) {
            // Fix-up is a power-user feature; don't take down the recorder if
            // its hotkey collides — log and keep going.
            eprintln!(
                "[typwrtr] Failed to register fix-up hotkey '{}': {}",
                fixup_hotkey, e
            );
        } else {
            registered.fixup_hotkey = Some(fixup_hotkey);
        }
    }

    registered.toggle_hotkey = Some(toggle_hotkey);
    registered.push_to_talk_hotkey = Some(ptt_hotkey);
    Ok(())
}

/// Shared logic for toggle recording, used by both the Tauri command and hotkey handler.
async fn do_toggle_recording(app: &tauri::AppHandle, state: &AppState) -> Result<String, String> {
    let current_state = state.recorder.get_state();
    match current_state {
        RecordingState::Ready => {
            let settings = state.settings.lock().unwrap().clone();
            let model_dir = model_storage_dir(&settings, &state.app_dir);
            state.recorder.start_recording(app, &settings, &model_dir)?;
            Ok("recording".to_string())
        }
        RecordingState::Recording => {
            let settings = state.settings.lock().unwrap().clone();
            let model_dir = model_storage_dir(&settings, &state.app_dir);
            let result = state
                .recorder
                .stop_and_transcribe(app, &settings, &state.app_dir, &model_dir)
                .await?;
            Ok(result)
        }
        RecordingState::Transcribing => Err("Currently transcribing, please wait".to_string()),
    }
}

fn main() {
    let app_dir = get_app_dir();
    let settings = Settings::load(&app_dir);
    let db = Db::open(&app_dir).unwrap_or_else(|e| {
        // A SQLite open/migrate failure is non-recoverable for self-learning, but
        // we shouldn't take the app down — fall back to a memory-only DB so the
        // dictation hot path keeps working. Logging here is the only visible
        // signal until we wire structured logs (eval §1 #8).
        eprintln!(
            "[typwrtr] WARNING: failed to open learning DB at {:?}: {}. \
             Falling back to in-memory DB; learning data will not persist this session.",
            app_dir, e
        );
        Db::open_at(std::path::Path::new(":memory:")).expect("in-memory sqlite must succeed")
    });

    let db = Arc::new(db);

    tauri::Builder::default()
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState {
            recorder: Recorder::new(Arc::new(LocalTranscriber::new()), db.clone()),
            registered_hotkeys: Mutex::new(RegisteredHotkeys::default()),
            hotkey_capture_active: Mutex::new(false),
            settings: Mutex::new(settings),
            app_dir,
            db,
            pending_fixup: Mutex::new(None),
        })
        .invoke_handler(tauri::generate_handler![
            get_settings,
            save_settings,
            set_hotkey_capture_active,
            list_microphones,
            get_recording_state,
            check_model_downloaded,
            resolved_model_dir,
            download_model,
            toggle_recording,
            db_health,
            wipe_learning_data,
            list_app_profiles,
            save_app_profile,
            delete_app_profile,
            start_fixup,
            take_pending_fixup,
            save_correction,
            list_top_corrections,
            list_top_vocabulary,
            usage_window,
            daily_buckets,
            app_breakdown,
            forget_correction,
            forget_vocabulary,
            list_snippets,
            save_snippet,
            delete_snippet,
        ])
        .on_window_event(|window, event| {
            let label = window.label();
            if label == "main" || label == "fixup" {
                if let WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    if let Err(e) = window.hide() {
                        eprintln!("[typwrtr] Failed to hide {} window: {}", label, e);
                    } else {
                        println!("[typwrtr] {} window hidden", label);
                    }
                }
            }
        })
        .setup(move |app| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.maximize();
            }

            let show_item = MenuItem::with_id(app, "show", "Show typwrtr", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "Quit typwrtr", true, None::<&str>)?;
            let tray_menu = Menu::with_items(app, &[&show_item, &quit_item])?;

            let mut tray_builder = TrayIconBuilder::with_id("typwrtr-tray")
                .tooltip("typwrtr")
                .menu(&tray_menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "show" => show_main_window(app),
                    "quit" => {
                        println!("[typwrtr] Quit requested from tray");
                        app.exit(0);
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| match event {
                    TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    }
                    | TrayIconEvent::DoubleClick {
                        button: MouseButton::Left,
                        ..
                    } => show_main_window(tray.app_handle()),
                    _ => {}
                });

            if let Some(icon) = app.default_window_icon().cloned() {
                tray_builder = tray_builder.icon(icon);
            }

            match tray_builder.build(app) {
                Ok(_) => println!("[typwrtr] Tray icon created"),
                Err(e) => eprintln!("[typwrtr] Failed to create tray icon: {}", e),
            }

            // Create the overlay window (tiny heartbeat mark, bottom-center, always on top)
            let overlay_size = 48.0;
            #[cfg(target_os = "windows")]
            let overlay_bottom_gap = 96.0;
            #[cfg(target_os = "macos")]
            let overlay_bottom_gap = 112.0;
            #[cfg(not(any(target_os = "windows", target_os = "macos")))]
            let overlay_bottom_gap = 88.0;
            let monitor = app.primary_monitor().ok().flatten();
            let (x, y) = if let Some(m) = monitor {
                let size = m.size();
                let scale = m.scale_factor();
                let logical_h = size.height as f64 / scale;
                let logical_w = size.width as f64 / scale;
                (
                    ((logical_w - overlay_size) / 2.0) as i32,
                    (logical_h - overlay_bottom_gap) as i32,
                )
            } else {
                (680, 830)
            };

            let overlay = WebviewWindowBuilder::new(
                app,
                "overlay",
                WebviewUrl::App("src/overlay.html".into()),
            )
            .title("")
            .inner_size(overlay_size, overlay_size)
            .position(x as f64, y as f64)
            .resizable(false)
            .decorations(false)
            .transparent(true)
            .always_on_top(true)
            .skip_taskbar(true)
            .focusable(false)
            .focused(false)
            .shadow(false)
            .build();

            match overlay {
                Ok(_) => println!("[typwrtr] Overlay window created"),
                Err(e) => eprintln!("[typwrtr] Failed to create overlay: {}", e),
            }

            // Fix-up window — created hidden, shown on hotkey. Hides instead of
            // closing so we don't pay the bring-up cost on every fix-up.
            let fixup_window =
                WebviewWindowBuilder::new(app, "fixup", WebviewUrl::App("src/fixup.html".into()))
                    .title("Teach typwrtr a correction")
                    .inner_size(640.0, 480.0)
                    .resizable(true)
                    .decorations(true)
                    .always_on_top(true)
                    .skip_taskbar(false)
                    .visible(false)
                    .build();

            match fixup_window {
                Ok(_) => println!("[typwrtr] Fix-up window created (hidden)"),
                Err(e) => eprintln!("[typwrtr] Failed to create fix-up window: {}", e),
            }

            // Phase 5 captions window — transparent, click-through, near
            // screen-bottom-center. The window itself decides whether to show
            // based on `streaming://partial` events; we only register it here.
            let caption_w = 720.0;
            let caption_h = 110.0;
            let monitor2 = app.primary_monitor().ok().flatten();
            let (cx, cy) = if let Some(m) = monitor2 {
                let size = m.size();
                let scale = m.scale_factor();
                let logical_h = size.height as f64 / scale;
                let logical_w = size.width as f64 / scale;
                (
                    ((logical_w - caption_w) / 2.0) as i32,
                    (logical_h - 220.0) as i32,
                )
            } else {
                (300, 700)
            };
            let captions_window = WebviewWindowBuilder::new(
                app,
                "captions",
                WebviewUrl::App("src/captions.html".into()),
            )
            .title("")
            .inner_size(caption_w, caption_h)
            .position(cx as f64, cy as f64)
            .resizable(false)
            .decorations(false)
            .transparent(true)
            .always_on_top(true)
            .skip_taskbar(true)
            .focused(false)
            .shadow(false)
            .visible(false)
            .build();
            match captions_window {
                Ok(w) => {
                    // Click-through; the window is a HUD, not interactive.
                    let _ = w.set_ignore_cursor_events(true);
                    println!("[typwrtr] Captions window created (hidden)");
                }
                Err(e) => eprintln!("[typwrtr] Failed to create captions window: {}", e),
            }

            let app_handle = app.handle().clone();
            let state = app.state::<AppState>();
            match register_hotkeys(&app_handle, state.inner()) {
                Ok(_) => println!("[typwrtr] Global shortcuts registered successfully"),
                Err(e) => eprintln!("[typwrtr] ERROR: {}", e),
            }

            // VAD auto-stop: streaming task posts `recording://auto-stop` on
            // detected end-of-speech; we finalize as if the user had hit toggle.
            let stop_handle = app_handle.clone();
            app_handle.listen("recording://auto-stop", move |_event| {
                let h = stop_handle.clone();
                tauri::async_runtime::spawn(async move {
                    let st = h.state::<AppState>();
                    if st.recorder.get_state() == RecordingState::Recording {
                        match do_toggle_recording(&h, st.inner()).await {
                            Ok(result) => println!("[typwrtr] VAD auto-stop: {}", result),
                            Err(e) => eprintln!("[typwrtr] VAD auto-stop error: {}", e),
                        }
                    }
                });
            });

            transcribe_local::log_compiled_backend();

            // Hygiene log: the prior on-device T5 grammar corrector wrote
            // ~240 MB of model files into <app_dir>/grammar-corrector/. The
            // corrector is gone (replaced by deterministic post-processing
            // in cleanup/scrub.rs); flag the directory once at startup so
            // the user knows they can reclaim the disk space.
            {
                let dir = state.app_dir.join("grammar-corrector");
                if dir.exists() {
                    println!(
                        "[typwrtr] grammar-corrector model files at {} are no longer used and can be deleted",
                        dir.display()
                    );
                }
            }

            // Pre-warm the whisper model so the first dictation doesn't pay load cost.
            let prewarm_settings = state.settings.lock().unwrap().clone();
            let model_root = model_storage_dir(&prewarm_settings, &state.app_dir);
            let model_path = model_root.join(transcribe_local::model_filename(
                &prewarm_settings.whisper_model,
            ));
            let local = state.recorder.local_transcriber();
            std::thread::spawn(move || {
                if model_path.exists() {
                    match local.ensure_loaded(&model_path) {
                        Ok(_) => println!("[typwrtr] Whisper model pre-warmed"),
                        Err(e) => eprintln!("[typwrtr] Pre-warm failed: {}", e),
                    }
                } else {
                    println!("[typwrtr] Skipping pre-warm: model not downloaded yet");
                }
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
