use std::error::Error;
#[cfg(windows)]
use std::thread;
#[cfg(windows)]
use std::time::{Duration, Instant};
#[cfg(windows)]
use windows::Win32::Foundation::HWND;
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
    GetClassNameW, GetForegroundWindow, GetGUIThreadInfo, GetWindowTextW, GetWindowThreadProcessId,
    IsWindow, SetForegroundWindow, GUITHREADINFO,
};

#[cfg(windows)]
#[derive(Clone, Debug)]
pub struct ForegroundTarget {
    hwnd_raw: isize,
    pub process_id: u32,
    pub thread_id: u32,
    pub title: String,
    pub class_name: String,
}

#[cfg(windows)]
impl ForegroundTarget {
    fn hwnd(&self) -> HWND {
        HWND(self.hwnd_raw)
    }

    fn describe(&self) -> String {
        format!(
            "hwnd=0x{:X} pid={} tid={} class='{}' title='{}'",
            self.hwnd_raw as usize, self.process_id, self.thread_id, self.class_name, self.title
        )
    }
}

#[cfg(windows)]
fn is_key_down(vk: VIRTUAL_KEY) -> bool {
    unsafe { (GetAsyncKeyState(vk.0 as i32) as u16) & 0x8000 != 0 }
}

#[cfg(windows)]
fn utf16_buf_to_string(buf: &[u16], len: i32) -> String {
    if len <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..len as usize])
}

#[cfg(windows)]
fn window_text(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let len = unsafe { GetWindowTextW(hwnd, &mut buf) };
    utf16_buf_to_string(&buf, len)
}

#[cfg(windows)]
fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 128];
    let len = unsafe { GetClassNameW(hwnd, &mut buf) };
    utf16_buf_to_string(&buf, len)
}

#[cfg(windows)]
fn current_focus_description() -> Option<String> {
    unsafe {
        let foreground = GetForegroundWindow();
        if foreground.0 == 0 {
            return None;
        }

        let thread_id = GetWindowThreadProcessId(foreground, None);
        if thread_id == 0 {
            return None;
        }

        let mut info = GUITHREADINFO {
            cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };

        if GetGUIThreadInfo(thread_id, &mut info).is_err() {
            return None;
        }

        let focus = info.hwndFocus;
        if focus.0 == 0 {
            return Some("focus=<none>".to_string());
        }

        Some(format!(
            "focus_hwnd=0x{:X} class='{}' title='{}'",
            focus.0 as usize,
            class_name(focus),
            window_text(focus)
        ))
    }
}

#[cfg(windows)]
pub fn capture_foreground_target() -> Option<ForegroundTarget> {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0 == 0 {
            return None;
        }

        let mut process_id = 0u32;
        let thread_id = GetWindowThreadProcessId(hwnd, Some(&mut process_id));
        if thread_id == 0 {
            return None;
        }

        Some(ForegroundTarget {
            hwnd_raw: hwnd.0 as isize,
            process_id,
            thread_id,
            title: window_text(hwnd),
            class_name: class_name(hwnd),
        })
    }
}

#[cfg(windows)]
pub fn log_foreground_target(context: &str) {
    match capture_foreground_target() {
        Some(target) => {
            let focus =
                current_focus_description().unwrap_or_else(|| "focus=<unavailable>".to_string());
            eprintln!("⌨ [{context}] foreground {} {focus}", target.describe());
        }
        None => {
            eprintln!("⌨ [{context}] foreground=<none>");
        }
    }
}

#[cfg(windows)]
pub fn foreground_belongs_to_current_process() -> bool {
    capture_foreground_target()
        .map(|target| target.process_id == unsafe { GetCurrentProcessId() })
        .unwrap_or(false)
}

#[cfg(windows)]
pub fn restore_foreground_target(target: &ForegroundTarget) -> bool {
    unsafe {
        let hwnd = target.hwnd();
        if !IsWindow(hwnd).as_bool() {
            eprintln!(
                "⌨ [restore] captured target is no longer a valid window: {}",
                target.describe()
            );
            return false;
        }

        let restored = SetForegroundWindow(hwnd).as_bool();
        if restored {
            eprintln!("⌨ [restore] restored captured target {}", target.describe());
        } else {
            eprintln!(
                "⌨ [restore] failed to restore captured target {}",
                target.describe()
            );
        }
        restored
    }
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
fn wait_for_modifiers_to_clear() {
    let start = Instant::now();
    let mut last_log_at: Option<Instant> = None;

    while modifiers_are_pressed() {
        let now = Instant::now();
        if now.duration_since(start) >= Duration::from_secs(1)
            && last_log_at.is_none_or(|prev| now.duration_since(prev) >= Duration::from_secs(1))
        {
            let pressed = pressed_modifiers();
            eprintln!(
                "⌛ Waiting {:.1}s for modifiers to clear before injection: {}",
                now.duration_since(start).as_secs_f32(),
                pressed.join(", ")
            );
            last_log_at = Some(now);
        }
        thread::sleep(Duration::from_millis(1));
    }
}

/// Inject UTF-16 text into the system input stream using Win32 SendInput.
/// This will go to whichever window has focus.
///
/// Returns Ok(()) if successful, or an Error if SendInput fails.
#[cfg(windows)]
pub fn inject_text(text: &str) -> Result<(), Box<dyn Error + Send + Sync>> {
    let utf16: Vec<u16> = text.encode_utf16().collect();
    if utf16.is_empty() {
        return Ok(());
    }

    wait_for_modifiers_to_clear();
    log_foreground_target("before_sendinput");

    let mut inputs: Vec<INPUT> = Vec::with_capacity(utf16.len() * 2);

    // Inject UTF-16 characters only after all modifiers are released.
    for &code_unit in &utf16 {
        if code_unit == 0 {
            continue;
        }

        // Key down
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

        // Key up
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

    if inputs.is_empty() {
        return Ok(());
    }

    unsafe {
        let sent = SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
        if sent != inputs.len() as u32 {
            // SendInput returned less than expected.
            eprintln!(
                "⚠ SendInput only sent {} out of {} inputs",
                sent,
                inputs.len()
            );
            if sent == 0 {
                return Err("SendInput returned 0 - possible causes: no window focused, input blocked by system (UIPI), or insufficient privileges".into());
            }
        }
    }

    log_foreground_target("after_sendinput");

    Ok(())
}

#[cfg(not(windows))]
pub fn inject_text(text: &str) -> Result<(), Box<dyn Error + Send + Sync>> {
    println!("INJECT (No-op on Linux): {}", text);
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
