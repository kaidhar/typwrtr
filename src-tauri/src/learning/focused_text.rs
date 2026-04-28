#[derive(Debug, Clone)]
pub struct FocusedText {
    pub text: String,
}

#[cfg(target_os = "windows")]
pub fn capture_focused_text() -> Result<Option<FocusedText>, String> {
    use windows::core::{Error as WinError, Interface};
    use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
        COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Accessibility::{
        CUIAutomation, IUIAutomation, IUIAutomationLegacyIAccessiblePattern,
        IUIAutomationTextPattern, IUIAutomationValuePattern, UIA_LegacyIAccessiblePatternId,
        UIA_TextPatternId, UIA_ValuePatternId,
    };

    struct ComGuard {
        should_uninit: bool,
    }
    impl Drop for ComGuard {
        fn drop(&mut self) {
            if self.should_uninit {
                unsafe { CoUninitialize() };
            }
        }
    }

    let init = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    let _guard = if init.is_ok() {
        ComGuard {
            should_uninit: true,
        }
    } else if init == RPC_E_CHANGED_MODE {
        ComGuard {
            should_uninit: false,
        }
    } else {
        return Err(format!("UI Automation COM init failed: {:?}", init));
    };

    let automation: IUIAutomation =
        unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }
            .map_err(|e: WinError| format!("UI Automation create failed: {}", e))?;
    let element = unsafe { automation.GetFocusedElement() }
        .map_err(|e| format!("UI Automation focused element failed: {}", e))?;

    if let Ok(pattern) = unsafe { element.GetCurrentPattern(UIA_ValuePatternId) } {
        if let Ok(value) = pattern.cast::<IUIAutomationValuePattern>() {
            if let Ok(text) = unsafe { value.CurrentValue() } {
                let s = text.to_string();
                if !s.trim().is_empty() {
                    return Ok(Some(FocusedText { text: s }));
                }
            }
        }
    }

    if let Ok(pattern) = unsafe { element.GetCurrentPattern(UIA_TextPatternId) } {
        if let Ok(text_pattern) = pattern.cast::<IUIAutomationTextPattern>() {
            if let Ok(range) = unsafe { text_pattern.DocumentRange() } {
                if let Ok(text) = unsafe { range.GetText(10_000) } {
                    let s = text.to_string();
                    if !s.trim().is_empty() {
                        return Ok(Some(FocusedText { text: s }));
                    }
                }
            }
        }
    }

    if let Ok(pattern) = unsafe { element.GetCurrentPattern(UIA_LegacyIAccessiblePatternId) } {
        if let Ok(legacy) = pattern.cast::<IUIAutomationLegacyIAccessiblePattern>() {
            if let Ok(text) = unsafe { legacy.CurrentValue() } {
                let s = text.to_string();
                if !s.trim().is_empty() {
                    return Ok(Some(FocusedText { text: s }));
                }
            }
        }
    }

    Ok(None)
}

#[cfg(not(target_os = "windows"))]
pub fn capture_focused_text() -> Result<Option<FocusedText>, String> {
    Ok(None)
}
