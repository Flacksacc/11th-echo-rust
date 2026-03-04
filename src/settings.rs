use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    pub api_key: String,
    pub selected_microphone: String,
    pub use_default_microphone: bool,
    pub hotkey_text: String,
    pub overlay_opacity: f32,
    pub theme_background_top_color: String,
    pub theme_background_bottom_color: String,
    pub theme_window_color: String,
    pub theme_button_accent_color: String,
    pub theme_title_color: String,
    pub theme_text_color: String,
    pub overlay_background_color: String,
    pub overlay_text_color: String,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            selected_microphone: String::new(),
            use_default_microphone: true,
            hotkey_text: "Ctrl+Space".to_string(),
            overlay_opacity: 0.85,
            theme_background_top_color: "#02140b".to_string(),   // deep forest green
            theme_background_bottom_color: "#000806".to_string(), // near-black green
            theme_window_color: "#041b11".to_string(),           // card/window green
            theme_button_accent_color: "#4ade80".to_string(),    // bright leaf green
            theme_title_color: "#e4ffe9".to_string(),            // soft light green
            theme_text_color: "#ccefd6".to_string(),             // muted light green
            overlay_background_color: "#03150c".to_string(),     // darker overlay panel
            overlay_text_color: "#e6fff0".to_string(),           // overlay text
        }
    }
}

pub fn settings_path() -> PathBuf {
    PathBuf::from("settings.json")
}

pub fn load_settings() -> AppSettings {
    load_settings_from_path(&settings_path())
}

pub fn load_settings_from_path(path: &PathBuf) -> AppSettings {
    if let Ok(contents) = fs::read_to_string(path) {
        if let Ok(settings) = serde_json::from_str::<AppSettings>(&contents) {
            return settings;
        }
    }
    AppSettings::default()
}

pub fn save_settings(settings: &AppSettings) {
    save_settings_to_path(&settings_path(), settings);
}

pub fn save_settings_to_path(path: &PathBuf, settings: &AppSettings) {
    match serde_json::to_string_pretty(settings) {
        Ok(json) => {
            if let Err(err) = fs::write(path, json) {
                eprintln!("❌ Failed to save settings: {}", err);
            }
        }
        Err(err) => {
            eprintln!("❌ Failed to serialize settings: {}", err);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{load_settings_from_path, save_settings_to_path, AppSettings};
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_path() -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("eleventh_echo_settings_mod_{}.json", stamp))
    }

    #[test]
    fn roundtrip_persists_values() {
        let path = unique_path();
        let expected = AppSettings {
            api_key: "sk_test".to_string(),
            selected_microphone: "Mic A".to_string(),
            use_default_microphone: false,
            hotkey_text: "Ctrl+Shift+F8".to_string(),
            overlay_opacity: 0.9,
            theme_background_top_color: "#222222".to_string(),
            theme_background_bottom_color: "#000000".to_string(),
            theme_window_color: "#111111".to_string(),
            theme_button_accent_color: "#ff0000".to_string(),
            theme_title_color: "#00ff00".to_string(),
            theme_text_color: "#0000ff".to_string(),
            overlay_background_color: "#123456".to_string(),
            overlay_text_color: "#654321".to_string(),
        };
        save_settings_to_path(&path, &expected);
        let loaded = load_settings_from_path(&path);
        let _ = fs::remove_file(&path);
        assert_eq!(loaded.api_key, expected.api_key);
        assert_eq!(loaded.selected_microphone, expected.selected_microphone);
        assert_eq!(loaded.use_default_microphone, expected.use_default_microphone);
        assert_eq!(loaded.hotkey_text, expected.hotkey_text);
    }

    #[test]
    fn invalid_file_falls_back_to_default() {
        let path = unique_path();
        fs::write(&path, "{not-json").unwrap();
        let loaded = load_settings_from_path(&path);
        let _ = fs::remove_file(&path);
        assert!(loaded.api_key.is_empty());
        assert!(loaded.selected_microphone.is_empty());
        assert!(loaded.use_default_microphone);
        assert_eq!(loaded.hotkey_text, "Ctrl+Space");
        assert!((loaded.overlay_opacity - 0.85).abs() < f32::EPSILON);
        assert_eq!(loaded.theme_background_top_color, "#02140b");
        assert_eq!(loaded.theme_background_bottom_color, "#000806");
        assert_eq!(loaded.theme_window_color, "#041b11");
        assert_eq!(loaded.theme_button_accent_color, "#4ade80");
        assert_eq!(loaded.theme_title_color, "#e4ffe9");
        assert_eq!(loaded.theme_text_color, "#ccefd6");
        assert_eq!(loaded.overlay_background_color, "#03150c");
        assert_eq!(loaded.overlay_text_color, "#e6fff0");
    }
}
