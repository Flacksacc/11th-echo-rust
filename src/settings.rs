use dirs_next::config_dir;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

use crate::transcription::{
    TranscriptionConfig, TranscriptionProvider, DEFAULT_ELEVENLABS_REALTIME_MODEL_ID,
    DEFAULT_LANGUAGE_CODE, DEFAULT_OPENAI_REALTIME_WHISPER_MODEL_ID, DEFAULT_PROVIDER_ID,
};

fn default_transcription_provider() -> String {
    DEFAULT_PROVIDER_ID.to_string()
}

fn default_transcription_model() -> String {
    DEFAULT_ELEVENLABS_REALTIME_MODEL_ID.to_string()
}

fn default_transcription_language_code() -> String {
    DEFAULT_LANGUAGE_CODE.to_string()
}

fn default_transcription_no_verbatim() -> bool {
    true
}

fn default_elevenlabs_model() -> String {
    DEFAULT_ELEVENLABS_REALTIME_MODEL_ID.to_string()
}

fn default_openai_model() -> String {
    DEFAULT_OPENAI_REALTIME_WHISPER_MODEL_ID.to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "default_transcription_provider")]
    pub transcription_provider: String,
    #[serde(default)]
    pub elevenlabs_api_key: String,
    #[serde(default)]
    pub elevenlabs_model: String,
    #[serde(default)]
    pub elevenlabs_language_code: String,
    #[serde(default = "default_transcription_no_verbatim")]
    pub elevenlabs_no_verbatim: bool,
    #[serde(default)]
    pub openai_api_key: String,
    #[serde(default)]
    pub openai_model: String,
    #[serde(default)]
    pub openai_language_code: String,
    #[serde(default = "default_transcription_model")]
    pub transcription_model: String,
    #[serde(default = "default_transcription_language_code")]
    pub transcription_language_code: String,
    #[serde(default = "default_transcription_no_verbatim")]
    pub transcription_no_verbatim: bool,
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
    #[serde(default)]
    pub overlay_position_x: Option<i32>,
    #[serde(default)]
    pub overlay_position_y: Option<i32>,
    pub gemini_api_key: String,
    pub gemini_enabled: bool,
    pub gemini_model: String,
    pub gemini_prompt_preset: String,
    pub gemini_custom_prompt: String,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            transcription_provider: default_transcription_provider(),
            elevenlabs_api_key: String::new(),
            elevenlabs_model: default_elevenlabs_model(),
            elevenlabs_language_code: default_transcription_language_code(),
            elevenlabs_no_verbatim: true,
            openai_api_key: String::new(),
            openai_model: default_openai_model(),
            openai_language_code: default_transcription_language_code(),
            transcription_model: default_transcription_model(),
            transcription_language_code: default_transcription_language_code(),
            transcription_no_verbatim: true,
            selected_microphone: String::new(),
            use_default_microphone: true,
            hotkey_text: "Ctrl+Space".to_string(),
            overlay_opacity: 0.85,
            theme_background_top_color: "#02140b".to_string(), // deep forest green
            theme_background_bottom_color: "#000806".to_string(), // near-black green
            theme_window_color: "#041b11".to_string(),         // card/window green
            theme_button_accent_color: "#4ade80".to_string(),  // bright leaf green
            theme_title_color: "#e4ffe9".to_string(),          // soft light green
            theme_text_color: "#ccefd6".to_string(),           // muted light green
            overlay_background_color: "#03150c".to_string(),   // darker overlay panel
            overlay_text_color: "#e6fff0".to_string(),         // overlay text
            overlay_position_x: None,
            overlay_position_y: None,
            gemini_api_key: String::new(),
            gemini_enabled: false,
            gemini_model: "gemini-3.1-flash-lite-preview".to_string(),
            gemini_prompt_preset: "Minimal corrections".to_string(),
            gemini_custom_prompt: String::new(),
        }
    }
}

impl AppSettings {
    pub fn normalize_transcription_settings(&mut self) {
        if self.transcription_provider.trim().is_empty() {
            self.transcription_provider = default_transcription_provider();
        }
        if self.elevenlabs_api_key.trim().is_empty() && !self.api_key.trim().is_empty() {
            self.elevenlabs_api_key = self.api_key.clone();
        }
        if self.api_key.trim().is_empty() && !self.elevenlabs_api_key.trim().is_empty() {
            self.api_key = self.elevenlabs_api_key.clone();
        }
        self.migrate_legacy_transcription_fields();
        if self.elevenlabs_model.trim().is_empty() {
            self.elevenlabs_model = default_elevenlabs_model();
        }
        if self.elevenlabs_language_code.trim().is_empty() {
            self.elevenlabs_language_code = default_transcription_language_code();
        }
        if self.openai_model.trim().is_empty() {
            self.openai_model = default_openai_model();
        }
        if self.openai_language_code.trim().is_empty() {
            self.openai_language_code = default_transcription_language_code();
        }
        if self.transcription_model.trim().is_empty() {
            self.transcription_model = default_transcription_model();
        }
        if self.transcription_language_code.trim().is_empty() {
            self.transcription_language_code = default_transcription_language_code();
        }
    }

    pub fn transcription_config(&self) -> TranscriptionConfig {
        let provider = TranscriptionProvider::from_id(&self.transcription_provider);
        let (api_key, model_id, language_code, no_verbatim) = match provider {
            TranscriptionProvider::ElevenLabsRealtime => {
                let api_key = if self.elevenlabs_api_key.trim().is_empty() {
                    self.api_key.clone()
                } else {
                    self.elevenlabs_api_key.clone()
                };
                (
                    api_key,
                    normalized_model_id(&provider, &self.elevenlabs_model),
                    if self.elevenlabs_language_code.trim().is_empty() {
                        default_transcription_language_code()
                    } else {
                        self.elevenlabs_language_code.clone()
                    },
                    self.elevenlabs_no_verbatim,
                )
            }
            TranscriptionProvider::OpenAiRealtimeWhisper => (
                self.openai_api_key.clone(),
                normalized_model_id(&provider, &self.openai_model),
                if self.openai_language_code.trim().is_empty() {
                    default_transcription_language_code()
                } else {
                    self.openai_language_code.clone()
                },
                false,
            ),
        };

        TranscriptionConfig {
            provider,
            api_key,
            model_id,
            language_code,
            no_verbatim,
        }
    }

    fn migrate_legacy_transcription_fields(&mut self) {
        let provider = TranscriptionProvider::from_id(&self.transcription_provider);
        let legacy_model = self.transcription_model.trim();
        if !legacy_model.is_empty() {
            match provider {
                TranscriptionProvider::ElevenLabsRealtime
                    if self.elevenlabs_model.trim().is_empty() =>
                {
                    self.elevenlabs_model = legacy_model.to_string();
                }
                TranscriptionProvider::OpenAiRealtimeWhisper
                    if self.openai_model.trim().is_empty() =>
                {
                    self.openai_model = legacy_model.to_string();
                }
                _ => {}
            }
        }

        let legacy_language = self.transcription_language_code.trim();
        if !legacy_language.is_empty() {
            match provider {
                TranscriptionProvider::ElevenLabsRealtime
                    if self.elevenlabs_language_code.trim().is_empty() =>
                {
                    self.elevenlabs_language_code = legacy_language.to_string();
                }
                TranscriptionProvider::OpenAiRealtimeWhisper
                    if self.openai_language_code.trim().is_empty() =>
                {
                    self.openai_language_code = legacy_language.to_string();
                }
                _ => {}
            }
        }

        if matches!(provider, TranscriptionProvider::ElevenLabsRealtime) {
            self.elevenlabs_no_verbatim = self.transcription_no_verbatim;
        }
    }
}

fn normalized_model_id(provider: &TranscriptionProvider, configured: &str) -> String {
    let configured = configured.trim();
    if configured.is_empty() {
        return provider.default_model_id().to_string();
    }

    match provider {
        TranscriptionProvider::ElevenLabsRealtime
            if configured == DEFAULT_OPENAI_REALTIME_WHISPER_MODEL_ID =>
        {
            provider.default_model_id().to_string()
        }
        TranscriptionProvider::OpenAiRealtimeWhisper
            if configured == DEFAULT_ELEVENLABS_REALTIME_MODEL_ID =>
        {
            provider.default_model_id().to_string()
        }
        _ => configured.to_string(),
    }
}

pub fn settings_path() -> PathBuf {
    // Prefer a per-user configuration directory; fall back to the current
    // directory if the OS-specific config dir is unavailable.
    let base = config_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join("11th_echo").join("settings.json")
}

pub fn load_settings() -> AppSettings {
    load_settings_from_path(&settings_path())
}

pub fn load_settings_from_path(path: &PathBuf) -> AppSettings {
    if let Ok(contents) = fs::read_to_string(path) {
        if let Ok(mut settings) = serde_json::from_str::<AppSettings>(&contents) {
            settings.normalize_transcription_settings();
            return settings;
        }
    }
    AppSettings::default()
}

pub fn save_settings(settings: &AppSettings) -> bool {
    save_settings_to_path(&settings_path(), settings)
}

pub fn save_settings_to_path(path: &PathBuf, settings: &AppSettings) -> bool {
    let mut settings = settings.clone();
    settings.normalize_transcription_settings();

    // Ensure the target directory exists (create the per-user folder if needed)
    if let Some(parent) = path.parent() {
        if let Err(err) = fs::create_dir_all(parent) {
            eprintln!(
                "❌ Failed to create settings directory {:?}: {}",
                parent, err
            );
            return false;
        }
    }

    match serde_json::to_string_pretty(&settings) {
        Ok(json) => {
            if let Err(err) = fs::write(path, json) {
                eprintln!("❌ Failed to save settings: {}", err);
                return false;
            }
            true
        }
        Err(err) => {
            eprintln!("❌ Failed to serialize settings: {}", err);
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{load_settings_from_path, save_settings_to_path, AppSettings};
    use crate::transcription::TranscriptionProvider;
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
            transcription_provider: "elevenlabs_realtime".to_string(),
            elevenlabs_api_key: "sk_test".to_string(),
            elevenlabs_model: "scribe_v2_realtime".to_string(),
            elevenlabs_language_code: "en".to_string(),
            elevenlabs_no_verbatim: true,
            openai_api_key: "op_test".to_string(),
            openai_model: "gpt-realtime-whisper".to_string(),
            openai_language_code: "en".to_string(),
            transcription_model: "scribe_v2_realtime".to_string(),
            transcription_language_code: "en".to_string(),
            transcription_no_verbatim: true,
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
            overlay_position_x: Some(128),
            overlay_position_y: Some(256),
            gemini_api_key: "gm_test".to_string(),
            gemini_enabled: true,
            gemini_model: "gemini-3.1-flash-lite-preview".to_string(),
            gemini_prompt_preset: "Minimal corrections".to_string(),
            gemini_custom_prompt: "Custom instructions".to_string(),
        };
        save_settings_to_path(&path, &expected);
        let loaded = load_settings_from_path(&path);
        let _ = fs::remove_file(&path);
        assert_eq!(loaded.api_key, expected.api_key);
        assert_eq!(loaded.elevenlabs_api_key, expected.elevenlabs_api_key);
        assert_eq!(loaded.elevenlabs_model, expected.elevenlabs_model);
        assert_eq!(
            loaded.elevenlabs_language_code,
            expected.elevenlabs_language_code
        );
        assert_eq!(
            loaded.elevenlabs_no_verbatim,
            expected.elevenlabs_no_verbatim
        );
        assert_eq!(loaded.openai_api_key, expected.openai_api_key);
        assert_eq!(loaded.openai_model, expected.openai_model);
        assert_eq!(loaded.openai_language_code, expected.openai_language_code);
        assert_eq!(
            loaded.transcription_provider,
            expected.transcription_provider
        );
        assert_eq!(loaded.transcription_model, expected.transcription_model);
        assert_eq!(
            loaded.transcription_language_code,
            expected.transcription_language_code
        );
        assert_eq!(
            loaded.transcription_no_verbatim,
            expected.transcription_no_verbatim
        );
        assert_eq!(loaded.selected_microphone, expected.selected_microphone);
        assert_eq!(
            loaded.use_default_microphone,
            expected.use_default_microphone
        );
        assert_eq!(loaded.hotkey_text, expected.hotkey_text);
    }

    #[test]
    fn invalid_file_falls_back_to_default() {
        let path = unique_path();
        fs::write(&path, "{not-json").unwrap();
        let loaded = load_settings_from_path(&path);
        let _ = fs::remove_file(&path);
        assert!(loaded.api_key.is_empty());
        assert!(loaded.elevenlabs_api_key.is_empty());
        assert_eq!(loaded.elevenlabs_model, "scribe_v2_realtime");
        assert_eq!(loaded.elevenlabs_language_code, "en");
        assert!(loaded.elevenlabs_no_verbatim);
        assert!(loaded.openai_api_key.is_empty());
        assert_eq!(loaded.openai_model, "gpt-realtime-whisper");
        assert_eq!(loaded.openai_language_code, "en");
        assert_eq!(loaded.transcription_provider, "elevenlabs_realtime");
        assert_eq!(loaded.transcription_model, "scribe_v2_realtime");
        assert_eq!(loaded.transcription_language_code, "en");
        assert!(loaded.transcription_no_verbatim);
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
        assert_eq!(loaded.overlay_position_x, None);
        assert_eq!(loaded.overlay_position_y, None);
    }

    #[test]
    fn old_api_key_migrates_to_elevenlabs_key() {
        let path = unique_path();
        fs::write(
            &path,
            r##"{
                "api_key": "sk_old",
                "selected_microphone": "",
                "use_default_microphone": true,
                "hotkey_text": "Ctrl+Space",
                "overlay_opacity": 0.85,
                "theme_background_top_color": "#02140b",
                "theme_background_bottom_color": "#000806",
                "theme_window_color": "#041b11",
                "theme_button_accent_color": "#4ade80",
                "theme_title_color": "#e4ffe9",
                "theme_text_color": "#ccefd6",
                "overlay_background_color": "#03150c",
                "overlay_text_color": "#e6fff0",
                "gemini_api_key": "",
                "gemini_enabled": false,
                "gemini_model": "gemini-3.1-flash-lite-preview",
                "gemini_prompt_preset": "Minimal corrections",
                "gemini_custom_prompt": ""
            }"##,
        )
        .unwrap();
        let loaded = load_settings_from_path(&path);
        let _ = fs::remove_file(&path);

        assert_eq!(loaded.api_key, "sk_old");
        assert_eq!(loaded.elevenlabs_api_key, "sk_old");
        let config = loaded.transcription_config();
        assert_eq!(config.api_key, "sk_old");
        assert_eq!(config.model_id, "scribe_v2_realtime");
    }

    #[test]
    fn legacy_model_migrates_to_active_provider_only() {
        let path = unique_path();
        fs::write(
            &path,
            r##"{
                "api_key": "",
                "transcription_provider": "openai_realtime_whisper",
                "openai_api_key": "sk_openai",
                "transcription_model": "gpt-realtime-whisper-preview",
                "transcription_language_code": "fr",
                "transcription_no_verbatim": true,
                "selected_microphone": "",
                "use_default_microphone": true,
                "hotkey_text": "Ctrl+Space",
                "overlay_opacity": 0.85,
                "theme_background_top_color": "#02140b",
                "theme_background_bottom_color": "#000806",
                "theme_window_color": "#041b11",
                "theme_button_accent_color": "#4ade80",
                "theme_title_color": "#e4ffe9",
                "theme_text_color": "#ccefd6",
                "overlay_background_color": "#03150c",
                "overlay_text_color": "#e6fff0",
                "gemini_api_key": "",
                "gemini_enabled": false,
                "gemini_model": "gemini-3.1-flash-lite-preview",
                "gemini_prompt_preset": "Minimal corrections",
                "gemini_custom_prompt": ""
            }"##,
        )
        .unwrap();

        let loaded = load_settings_from_path(&path);
        let _ = fs::remove_file(&path);

        assert_eq!(loaded.openai_model, "gpt-realtime-whisper-preview");
        assert_eq!(loaded.openai_language_code, "fr");
        assert_eq!(loaded.elevenlabs_model, "scribe_v2_realtime");
    }

    #[test]
    fn openai_provider_uses_openai_key_and_default_model() {
        let settings = AppSettings {
            transcription_provider: "openai_realtime_whisper".to_string(),
            elevenlabs_api_key: "sk_eleven".to_string(),
            openai_api_key: "sk_openai".to_string(),
            elevenlabs_model: "scribe_v2_realtime".to_string(),
            openai_model: "gpt-realtime-whisper".to_string(),
            ..Default::default()
        };

        let config = settings.transcription_config();
        assert_eq!(config.api_key, "sk_openai");
        assert_eq!(config.model_id, "gpt-realtime-whisper");
        assert_eq!(
            config.provider,
            TranscriptionProvider::OpenAiRealtimeWhisper
        );
    }

    #[test]
    fn each_provider_keeps_its_own_model() {
        let mut settings = AppSettings {
            transcription_provider: "elevenlabs_realtime".to_string(),
            elevenlabs_model: "scribe_custom".to_string(),
            openai_model: "gpt-realtime-whisper-custom".to_string(),
            ..Default::default()
        };

        assert_eq!(settings.transcription_config().model_id, "scribe_custom");
        settings.transcription_provider = "openai_realtime_whisper".to_string();
        assert_eq!(
            settings.transcription_config().model_id,
            "gpt-realtime-whisper-custom"
        );
    }
}
