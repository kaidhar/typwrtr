//! Synthesised "copy current selection" — mirror of `paste`. Used by the
//! Phase-2 fix-up hotkey to grab whatever the user has highlighted in any app.
//!
//! Flow: save the prior clipboard, post Cmd/Ctrl+C, briefly poll the clipboard
//! until either it changes or a timeout, return the captured text, restore the
//! prior clipboard so the user's clipboard history is undisturbed.

use std::thread::sleep;
use std::time::{Duration, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(20);
const COPY_TIMEOUT: Duration = Duration::from_millis(400);

/// Capture the user's current selection. Returns `Ok(None)` if the clipboard
/// didn't change within `COPY_TIMEOUT` (probably nothing was selected). The
/// prior clipboard contents are best-effort restored on success or failure.
pub fn capture_selection() -> Result<Option<String>, String> {
    let mut clipboard = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    let prior = clipboard.get_text().ok();

    // A sentinel that we put on the clipboard *before* synthesising the copy
    // keystroke. If, after the timeout, the clipboard still equals the
    // sentinel, the user had nothing selected — we treat that as `None`
    // rather than returning the sentinel string.
    let sentinel = format!(
        "__typwrtr_fixup_sentinel_{}__",
        Instant::now().elapsed().as_nanos()
    );
    let _ = clipboard.set_text(&sentinel);
    drop(clipboard);

    synthesize_copy()?;

    // Poll until the clipboard differs from the sentinel or we time out.
    let started = Instant::now();
    let mut captured: Option<String> = None;
    loop {
        sleep(POLL_INTERVAL);
        let mut cb = match arboard::Clipboard::new() {
            Ok(c) => c,
            Err(_) => continue,
        };
        if let Ok(text) = cb.get_text() {
            if text != sentinel {
                captured = Some(text);
                break;
            }
        }
        if started.elapsed() >= COPY_TIMEOUT {
            break;
        }
    }

    // Restore prior clipboard regardless of outcome.
    if let Ok(mut cb) = arboard::Clipboard::new() {
        match prior {
            Some(p) => {
                let _ = cb.set_text(p);
            }
            None => {
                let _ = cb.clear();
            }
        }
    }

    Ok(captured)
}

#[cfg(target_os = "macos")]
fn synthesize_copy() -> Result<(), String> {
    use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation, CGKeyCode};
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

    const KEY_C: CGKeyCode = 0x08;

    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .map_err(|_| "Failed to create CGEventSource".to_string())?;

    let down = CGEvent::new_keyboard_event(source.clone(), KEY_C, true)
        .map_err(|_| "Failed to create CGEvent (down)".to_string())?;
    down.set_flags(CGEventFlags::CGEventFlagCommand);
    down.post(CGEventTapLocation::HID);

    let up = CGEvent::new_keyboard_event(source, KEY_C, false)
        .map_err(|_| "Failed to create CGEvent (up)".to_string())?;
    up.set_flags(CGEventFlags::CGEventFlagCommand);
    up.post(CGEventTapLocation::HID);

    Ok(())
}

#[cfg(target_os = "windows")]
fn synthesize_copy() -> Result<(), String> {
    use enigo::{Direction, Enigo, Key, Keyboard, Settings};
    let mut enigo = Enigo::new(&Settings::default()).map_err(|e| e.to_string())?;
    enigo
        .key(Key::Control, Direction::Press)
        .map_err(|e| e.to_string())?;
    enigo
        .key(Key::Unicode('c'), Direction::Click)
        .map_err(|e| e.to_string())?;
    enigo
        .key(Key::Control, Direction::Release)
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn synthesize_copy() -> Result<(), String> {
    Err("synthesize_copy not implemented on this platform".to_string())
}
