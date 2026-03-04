#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotkeyKey {
    Space,
    Letter(char),
    Digit(u8),
    Function(u8),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotkeySpec {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub meta: bool,
    pub key: HotkeyKey,
}

pub fn parse_hotkey_spec(input: &str) -> Result<HotkeySpec, String> {
    let mut ctrl = false;
    let mut shift = false;
    let mut alt = false;
    let mut meta = false;
    let mut key: Option<HotkeyKey> = None;

    for part in input.split('+').map(|p| p.trim()).filter(|p| !p.is_empty()) {
        let token = part.to_ascii_lowercase();
        match token.as_str() {
            "ctrl" | "control" => ctrl = true,
            "shift" => shift = true,
            "alt" => alt = true,
            "meta" | "win" | "super" => meta = true,
            "space" => key = Some(HotkeyKey::Space),
            "a" => key = Some(HotkeyKey::Letter('A')),
            "b" => key = Some(HotkeyKey::Letter('B')),
            "c" => key = Some(HotkeyKey::Letter('C')),
            "d" => key = Some(HotkeyKey::Letter('D')),
            "e" => key = Some(HotkeyKey::Letter('E')),
            "f" => key = Some(HotkeyKey::Letter('F')),
            "g" => key = Some(HotkeyKey::Letter('G')),
            "h" => key = Some(HotkeyKey::Letter('H')),
            "i" => key = Some(HotkeyKey::Letter('I')),
            "j" => key = Some(HotkeyKey::Letter('J')),
            "k" => key = Some(HotkeyKey::Letter('K')),
            "l" => key = Some(HotkeyKey::Letter('L')),
            "m" => key = Some(HotkeyKey::Letter('M')),
            "n" => key = Some(HotkeyKey::Letter('N')),
            "o" => key = Some(HotkeyKey::Letter('O')),
            "p" => key = Some(HotkeyKey::Letter('P')),
            "q" => key = Some(HotkeyKey::Letter('Q')),
            "r" => key = Some(HotkeyKey::Letter('R')),
            "s" => key = Some(HotkeyKey::Letter('S')),
            "t" => key = Some(HotkeyKey::Letter('T')),
            "u" => key = Some(HotkeyKey::Letter('U')),
            "v" => key = Some(HotkeyKey::Letter('V')),
            "w" => key = Some(HotkeyKey::Letter('W')),
            "x" => key = Some(HotkeyKey::Letter('X')),
            "y" => key = Some(HotkeyKey::Letter('Y')),
            "z" => key = Some(HotkeyKey::Letter('Z')),
            "0" => key = Some(HotkeyKey::Digit(0)),
            "1" => key = Some(HotkeyKey::Digit(1)),
            "2" => key = Some(HotkeyKey::Digit(2)),
            "3" => key = Some(HotkeyKey::Digit(3)),
            "4" => key = Some(HotkeyKey::Digit(4)),
            "5" => key = Some(HotkeyKey::Digit(5)),
            "6" => key = Some(HotkeyKey::Digit(6)),
            "7" => key = Some(HotkeyKey::Digit(7)),
            "8" => key = Some(HotkeyKey::Digit(8)),
            "9" => key = Some(HotkeyKey::Digit(9)),
            "f1" => key = Some(HotkeyKey::Function(1)),
            "f2" => key = Some(HotkeyKey::Function(2)),
            "f3" => key = Some(HotkeyKey::Function(3)),
            "f4" => key = Some(HotkeyKey::Function(4)),
            "f5" => key = Some(HotkeyKey::Function(5)),
            "f6" => key = Some(HotkeyKey::Function(6)),
            "f7" => key = Some(HotkeyKey::Function(7)),
            "f8" => key = Some(HotkeyKey::Function(8)),
            "f9" => key = Some(HotkeyKey::Function(9)),
            "f10" => key = Some(HotkeyKey::Function(10)),
            "f11" => key = Some(HotkeyKey::Function(11)),
            "f12" => key = Some(HotkeyKey::Function(12)),
            _ => return Err(format!("Unsupported key token: {}", part)),
        }
    }

    let key = key.ok_or_else(|| "No key found in hotkey string".to_string())?;
    Ok(HotkeySpec {
        ctrl,
        shift,
        alt,
        meta,
        key,
    })
}

#[cfg(test)]
mod tests {
    use super::{parse_hotkey_spec, HotkeyKey};

    #[test]
    fn parse_letters_digits_and_function_keys() {
        assert_eq!(
            parse_hotkey_spec("Ctrl+A").unwrap().key,
            HotkeyKey::Letter('A')
        );
        assert_eq!(
            parse_hotkey_spec("Alt+9").unwrap().key,
            HotkeyKey::Digit(9)
        );
        assert_eq!(
            parse_hotkey_spec("Shift+F12").unwrap().key,
            HotkeyKey::Function(12)
        );
        assert_eq!(
            parse_hotkey_spec("Ctrl+Space").unwrap().key,
            HotkeyKey::Space
        );
    }

    #[test]
    fn parse_modifiers() {
        let s = parse_hotkey_spec("Ctrl+Shift+Alt+Win+F8").unwrap();
        assert!(s.ctrl);
        assert!(s.shift);
        assert!(s.alt);
        assert!(s.meta);
        assert_eq!(s.key, HotkeyKey::Function(8));
    }

    #[test]
    fn parse_aliases_for_meta_and_ctrl() {
        assert!(parse_hotkey_spec("control+super+x").is_ok());
        assert!(parse_hotkey_spec("ctrl+meta+x").is_ok());
    }

    #[test]
    fn missing_key_is_error() {
        let e = parse_hotkey_spec("Ctrl+Shift").unwrap_err();
        assert!(e.contains("No key found"));
    }

    #[test]
    fn unknown_token_is_error() {
        let e = parse_hotkey_spec("Ctrl+Tab").unwrap_err();
        assert!(e.contains("Unsupported key token"));
    }
}
