pub mod audio;
pub mod cleanup;
pub mod commands;
pub mod context;
pub mod copy;
pub mod db;
pub mod downloader;
pub mod focused_text;
pub mod learning;
pub mod llm_cleanup;
pub mod paste;
pub mod postprocess;
pub mod recorder;
pub mod settings;
pub mod streaming;
pub mod transcribe_groq;
pub mod transcribe_local;
pub mod vad;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
