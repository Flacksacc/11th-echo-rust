use std::error::Error;
#[cfg(windows)]
use std::thread;
#[cfg(windows)]
use std::time::{Duration, Instant};
#[cfg(windows)]
use windows::Win32::Foundation::GetLastError;
#[cfg(windows)]
use windows::Win32::System::Threading::GetCurrentProcessId;
#[cfg(windows)]
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, VIRTUAL_KEY, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN,
    VK_RCONTROL, VK_RMENU, VK_RSHIFT, VK_RWIN,
};
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetForegroundWindow, GetGUIThreadInfo, GetWindowThreadProcessId, GUITHREADINFO,
};

#[cfg(windows)]
fn is_key_down(vk: VIRTUAL_KEY) -> bool {
    unsafe { (GetAsyncKeyState(vk.0 as i32) as u16) & 0x8000 != 0 }
}

#[cfg(windows)]
fn modifiers_are_pressed() -> bool {
    [
        VK_LCONTROL,
        VK_RCONTROL,
        VK_LMENU,
        VK_RMENU,
        VK_LSHIFT,
        VK_RSHIFT,
        VK_LWIN,
        VK_RWIN,
    ]
    .into_iter()
    .any(is_key_down)
}

#[cfg(windows)]
fn pressed_modifiers() -> Vec<&'static str> {
    let mut pressed = Vec::new();
    if is_key_down(VK_LCONTROL) {
        pressed.push("LControl");
    }
    if is_key_down(VK_RCONTROL) {
        pressed.push("RControl");
    }
    if is_key_down(VK_LMENU) {
        pressed.push("LAlt");
    }
    if is_key_down(VK_RMENU) {
        pressed.push("RAlt");
    }
    if is_key_down(VK_LSHIFT) {
        pressed.push("LShift");
    }
    if is_key_down(VK_RSHIFT) {
        pressed.push("RShift");
    }
    if is_key_down(VK_LWIN) {
        pressed.push("LWin");
    }
    if is_key_down(VK_RWIN) {
        pressed.push("RWin");
    }
    pressed
}

#[cfg(windows)]
fn wait_for_modifiers_to_clear() -> Result<(), Box<dyn Error + Send + Sync>> {
    let start = Instant::now();
    let mut last_log_at: Option<Instant> = None;

    while modifiers_are_pressed() {
        let now = Instant::now();
        if now.duration_since(start) >= Duration::from_secs(5) {
            crate::echo_error!(
                "injector",
                "Direct input cancelled because modifiers remained pressed keys={}",
                pressed_modifiers().join(",")
            );
            return Err("Modifier keys remained pressed; text injection was cancelled".into());
        }
        if now.duration_since(start) >= Duration::from_secs(1)
            && last_log_at
                .is_none_or(|previous| now.duration_since(previous) >= Duration::from_secs(1))
        {
            crate::echo_warn!(
                "injector",
                "Waiting {:.1}s for modifiers to clear before injection: {}",
                now.duration_since(start).as_secs_f32(),
                pressed_modifiers().join(", ")
            );
            last_log_at = Some(now);
        }
        thread::sleep(Duration::from_millis(1));
    }
    Ok(())
}

/// Inject UTF-16 text directly into whichever Windows control currently has
/// focus. No start-time destination or clipboard is used.
#[cfg(windows)]
pub fn inject_text(text: &str) -> Result<(), Box<dyn Error + Send + Sync>> {
    let utf16 = text
        .encode_utf16()
        .filter(|code_unit| *code_unit != 0)
        .collect::<Vec<_>>();
    if utf16.is_empty() {
        return Ok(());
    }

    wait_for_modifiers_to_clear()?;
    let foreground = unsafe { GetForegroundWindow() };
    if foreground.0 == 0 {
        crate::echo_error!(
            "injector",
            "Direct input cancelled reason=no_foreground_window utf16_units={}",
            utf16.len()
        );
        return Err("No Windows application currently has focus".into());
    }

    let mut foreground_process_id = 0u32;
    let foreground_thread_id =
        unsafe { GetWindowThreadProcessId(foreground, Some(&mut foreground_process_id)) };
    let mut gui_info = GUITHREADINFO {
        cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    let gui_info_available = foreground_thread_id != 0
        && unsafe { GetGUIThreadInfo(foreground_thread_id, &mut gui_info) }.is_ok();
    let focused_control = if gui_info_available {
        gui_info.hwndFocus
    } else {
        Default::default()
    };
    let mut class_buffer = [0u16; 128];
    let class_length = if focused_control.0 != 0 {
        unsafe { GetClassNameW(focused_control, &mut class_buffer) }
    } else {
        0
    };
    let focused_class = if class_length > 0 {
        String::from_utf16_lossy(&class_buffer[..class_length as usize])
    } else {
        "unavailable".to_string()
    };
    crate::echo_info!(
        "injector",
        "Direct input starting utf16_units={} foreground_hwnd=0x{:X} foreground_pid={} foreground_tid={} focus_hwnd=0x{:X} focus_class={} foreground_is_echo={}",
        utf16.len(),
        foreground.0 as usize,
        foreground_process_id,
        foreground_thread_id,
        focused_control.0 as usize,
        focused_class,
        foreground_process_id == unsafe { GetCurrentProcessId() }
    );

    let mut inputs = Vec::with_capacity(utf16.len() * 2);
    for code_unit in utf16 {
        inputs.push(INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(0),
                    wScan: code_unit,
                    dwFlags: KEYBD_EVENT_FLAGS(KEYEVENTF_UNICODE.0),
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        });
        inputs.push(INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(0),
                    wScan: code_unit,
                    dwFlags: KEYBD_EVENT_FLAGS(KEYEVENTF_UNICODE.0 | KEYEVENTF_KEYUP.0),
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        });
    }

    unsafe {
        let sent = SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
        let last_error = GetLastError();
        if sent != inputs.len() as u32 {
            crate::echo_error!(
                "injector",
                "SendInput incomplete sent={} requested={} win32_error={}",
                sent,
                inputs.len(),
                last_error.0
            );
            return Err("Windows did not accept the complete transcript; the focused application may be running with higher privileges".into());
        }
        crate::echo_info!(
            "injector",
            "SendInput completed sent={} requested={}",
            sent,
            inputs.len()
        );
    }

    Ok(())
}

#[cfg(not(windows))]
pub fn inject_text(text: &str) -> Result<(), Box<dyn Error + Send + Sync>> {
    crate::echo_info!(
        "injector",
        "Direct input is a no-op on this platform characters={}",
        text.chars().count()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::inject_text;

    #[test]
    fn inject_empty_text_is_ok() {
        assert!(inject_text("").is_ok());
    }

    #[test]
    fn inject_null_only_text_is_ok() {
        assert!(inject_text("\0").is_ok());
    }
}
