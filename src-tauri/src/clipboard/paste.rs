pub fn paste_text(text: &str) -> Result<(), String> {
    // Snapshot the user's existing clipboard before we clobber it with the
    // dictated text. After the synthesised paste keystroke fires (and the OS
    // has consumed the clipboard), a background thread restores the prior
    // contents so dictation doesn't trample whatever the user had copied.
    let mut clipboard = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    let prior_clipboard = clipboard.get_text().ok();
    clipboard.set_text(text).map_err(|e| e.to_string())?;
    drop(clipboard);

    #[cfg(target_os = "macos")]
    {
        paste_macos()?;
    }

    #[cfg(target_os = "windows")]
    {
        use enigo::{Direction, Enigo, Key, Keyboard, Settings};
        let mut enigo = Enigo::new(&Settings::default()).map_err(|e| e.to_string())?;
        enigo
            .key(Key::Control, Direction::Press)
            .map_err(|e| e.to_string())?;
        enigo
            .key(Key::Unicode('v'), Direction::Click)
            .map_err(|e| e.to_string())?;
        enigo
            .key(Key::Control, Direction::Release)
            .map_err(|e| e.to_string())?;
    }

    // Restore the user's prior clipboard once the OS has had time to consume
    // the synthesised paste. 120 ms is conservative — works on slow apps
    // (Slack, Teams electrons) without the user noticing, and the recorder
    // doesn't wait on it. If `prior_clipboard` was empty, we clear instead of
    // leaving the dictated text lingering.
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(120));
        if let Ok(mut cb) = arboard::Clipboard::new() {
            match prior_clipboard {
                Some(p) => {
                    let _ = cb.set_text(p);
                }
                None => {
                    let _ = cb.clear();
                }
            }
        }
    });

    Ok(())
}

/// Synthesise Cmd+V via CGEvent. Avoids the ~80 ms `osascript` spawn cost.
/// CGEvent posting works from any thread, unlike enigo on macOS which requires
/// the main thread (it calls TSMGetInputSourceProperty internally).
#[cfg(target_os = "macos")]
fn paste_macos() -> Result<(), String> {
    use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation, CGKeyCode};
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

    // kVK_ANSI_V — virtual keycode for the V key, layout-independent.
    const KEY_V: CGKeyCode = 0x09;

    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .map_err(|_| "Failed to create CGEventSource".to_string())?;

    let key_down = CGEvent::new_keyboard_event(source.clone(), KEY_V, true)
        .map_err(|_| "Failed to create CGEvent (down)".to_string())?;
    key_down.set_flags(CGEventFlags::CGEventFlagCommand);
    key_down.post(CGEventTapLocation::HID);

    let key_up = CGEvent::new_keyboard_event(source, KEY_V, false)
        .map_err(|_| "Failed to create CGEvent (up)".to_string())?;
    key_up.set_flags(CGEventFlags::CGEventFlagCommand);
    key_up.post(CGEventTapLocation::HID);

    Ok(())
}
