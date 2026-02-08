#[cfg(windows)]
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, VIRTUAL_KEY,
};
use std::error::Error;

/// Inject UTF-16 text into the system input stream using Win32 SendInput.
/// This will go to whichever window has focus.
/// 
/// Returns Ok(()) if successful, or an Error if SendInput fails.
#[cfg(windows)]
pub fn inject_text(text: &str) -> Result<(), Box<dyn Error + Send + Sync>> {
    // Convert Rust UTF-8 string to UTF-16 code units.
    let utf16: Vec<u16> = text.encode_utf16().collect();

    for &code_unit in &utf16 {
        // Skip nulls just in case
        if code_unit == 0 {
            continue;
        }

        // Key down
        let mut inputs: Vec<INPUT> = Vec::with_capacity(2);

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

        unsafe {
            let sent = SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
            if sent == 0 {
                // SendInput returned 0, which means it failed.
                // This can happen if:
                // - No window has focus
                // - The target window doesn't accept input
                // - Input is blocked by UIPI (User Interface Privilege Isolation)
                // - The desktop is locked
                eprintln!("⚠ SendInput failed for character U+{:04X}", code_unit);
                return Err("SendInput returned 0 - possible causes: no window focused, input blocked by system, or insufficient privileges".into());
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
