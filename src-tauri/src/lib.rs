pub mod audio;
pub mod cleanup;
pub mod clipboard;
pub mod commands;
pub mod context;
pub mod db;
pub mod downloader;
pub mod learning;
pub mod recorder;
pub mod settings;
pub mod streaming;
pub mod transcribe;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
