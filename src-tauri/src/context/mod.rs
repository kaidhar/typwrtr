//! Foreground application probe.
//!
//! Cross-platform via `active-win-pos-rs`. The `bundle_id` shape differs by OS:
//!
//! * **macOS**: a real CFBundleIdentifier, e.g. `com.microsoft.VSCode`,
//!   `com.tinyspeck.slackmacgap`.
//! * **Windows**: derived from the executable path — lowercased basename with
//!   the `.exe` stripped, e.g. `code`, `slack`, `chrome`. There is no native
//!   bundle identifier on Windows; this gives us a stable key to join on
//!   without dragging in registry-poking code.
//! * **Linux** (when supported): same shape as Windows.
//!
//! All callers should treat `bundle_id` as an opaque key — never parse it.

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct AppContext {
    pub bundle_id: String,
    pub display_name: String,
    pub window_title: Option<String>,
}

/// Probe the current foreground window. Returns `None` if no window is focused
/// or the OS denied us access (e.g. macOS Screen Recording permission absent —
/// `active-win-pos-rs` falls back to a less informative path in that case).
pub fn current() -> Option<AppContext> {
    let win = active_win_pos_rs::get_active_window().ok()?;

    let bundle_id = if !win.process_path.as_os_str().is_empty() {
        derive_bundle_id(&win.process_path, &win.app_name)
    } else if !win.app_name.is_empty() {
        win.app_name.to_lowercase()
    } else {
        return None;
    };

    let display_name = if !win.app_name.is_empty() {
        win.app_name.clone()
    } else {
        bundle_id.clone()
    };

    let window_title = if win.title.is_empty() {
        None
    } else {
        Some(win.title)
    };

    Some(AppContext {
        bundle_id,
        display_name,
        window_title,
    })
}

#[cfg(target_os = "macos")]
fn derive_bundle_id(_process_path: &std::path::Path, app_name: &str) -> String {
    // active-win-pos-rs surfaces the real bundle id in `app_name` on macOS via a
    // CGWindowListCopyWindowInfo lookup. If we ever upgrade to a version that
    // exposes a separate field we'll switch to it; for now this is the
    // authoritative source.
    if app_name.is_empty() {
        "unknown".to_string()
    } else {
        app_name.to_string()
    }
}

#[cfg(not(target_os = "macos"))]
fn derive_bundle_id(process_path: &std::path::Path, app_name: &str) -> String {
    let stem = process_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    if stem.is_empty() {
        if app_name.is_empty() {
            "unknown".to_string()
        } else {
            app_name.to_lowercase()
        }
    } else {
        stem.to_lowercase()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn windows_bundle_id_strips_extension_and_lowercases() {
        let path = PathBuf::from(r"C:\Program Files\Microsoft VS Code\Code.exe");
        assert_eq!(derive_bundle_id(&path, "Visual Studio Code"), "code");
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn windows_bundle_id_falls_back_to_app_name_on_empty_path() {
        let path = PathBuf::new();
        assert_eq!(derive_bundle_id(&path, "Slack"), "slack");
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn windows_bundle_id_emits_unknown_when_nothing_known() {
        let path = PathBuf::new();
        assert_eq!(derive_bundle_id(&path, ""), "unknown");
    }
}
