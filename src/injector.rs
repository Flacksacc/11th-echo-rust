#[cfg(windows)]
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, VIRTUAL_KEY,
    GetAsyncKeyState, VK_CONTROL, VK_MENU, VK_SHIFT, VK_LWIN, VK_RWIN,
};
use std::error::Error;

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

    // Identify which modifiers are currently held down.
    let modifiers = [
        VK_CONTROL,
        VK_MENU, // Alt
        VK_SHIFT,
        VK_LWIN,
        VK_RWIN,
    ];

    let mut held_modifiers = Vec::new();
    for &mod_key in &modifiers {
        unsafe {
            // GetAsyncKeyState returns a short where the high bit (0x8000) is set if the key is down.
            if (GetAsyncKeyState(mod_key.0 as i32) as u16) & 0x8000 != 0 {
                held_modifiers.push(mod_key);
            }
        }
    }

    // Total inputs: (release mods) + (press chars) + (release chars) + (restore mods)
    let mut inputs: Vec<INPUT> = Vec::with_capacity(held_modifiers.len() * 2 + utf16.len() * 2);

    // 1. Temporarily release held modifiers
    for &mod_key in &held_modifiers {
        inputs.push(INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: mod_key,
                    wScan: 0,
                    dwFlags: KEYBD_EVENT_FLAGS(KEYEVENTF_KEYUP.0),
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        });
    }

    // 2. Inject UTF-16 characters
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
                    dwFlags: KEYBD_EVENT_FLAGS(
                        KEYEVENTF_UNICODE.0 | KEYEVENTF_KEYUP.0,
                    ),
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        });
    }

    // 3. Restore modifiers
    for &mod_key in held_modifiers.iter().rev() {
        inputs.push(INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: mod_key,
                    wScan: 0,
                    dwFlags: KEYBD_EVENT_FLAGS(0), // Key down
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
            eprintln!("⚠ SendInput only sent {} out of {} inputs", sent, inputs.len());
            if sent == 0 {
                return Err("SendInput returned 0 - possible causes: no window focused, input blocked by system (UIPI), or insufficient privileges".into());
            }
        }
    }

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
