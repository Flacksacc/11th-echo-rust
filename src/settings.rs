use dirs_next::config_dir;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(target_os = "windows")]
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
#[cfg(target_os = "windows")]
use std::os::windows::ffi::OsStrExt;
#[cfg(target_os = "windows")]
use windows::core::PCWSTR;
#[cfg(target_os = "windows")]
use windows::Win32::Foundation::{LocalFree, HLOCAL};
#[cfg(target_os = "windows")]
use windows::Win32::Security::Cryptography::{
    CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
};
#[cfg(target_os = "windows")]
use windows::Win32::Storage::FileSystem::{ReplaceFileW, REPLACE_FILE_FLAGS};

use crate::transcription::{
    LocalSherpaConfig, TranscriptionConfig, TranscriptionProvider,
    DEFAULT_ELEVENLABS_REALTIME_MODEL_ID, DEFAULT_LANGUAGE_CODE,
    DEFAULT_OPENAI_REALTIME_WHISPER_MODEL_ID, DEFAULT_PROVIDER_ID,
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

const PROTECTED_SECRET_PREFIX: &str = "dpapi:";
pub const MAX_TRANSCRIPT_HISTORY: usize = 500;
const MAX_TRANSCRIPT_HISTORY_FILE_BYTES: u64 = 8 * 1024 * 1024;
static SETTINGS_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscriptHistoryEntry {
    pub timestamp: String,
    pub text: String,
}

impl TranscriptHistoryEntry {
    pub fn display_text(&self) -> String {
        format!("[{}] {}", self.timestamp, self.text)
    }
}

#[cfg(target_os = "windows")]
fn protect_secret(secret: &str) -> Result<String, String> {
    if secret.is_empty() {
        return Ok(secret.to_string());
    }

    let bytes = secret.as_bytes();
    let input = CRYPT_INTEGER_BLOB {
        cbData: bytes.len() as u32,
        pbData: bytes.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptProtectData(
            &input,
            windows::core::w!("Echo API credential"),
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
        .map_err(|err| format!("Windows could not protect an API credential: {err}"))?;
        let protected = std::slice::from_raw_parts(output.pbData, output.cbData as usize);
        let encoded = BASE64.encode(protected);
        let _ = LocalFree(HLOCAL(output.pbData.cast()));
        Ok(format!("{PROTECTED_SECRET_PREFIX}{encoded}"))
    }
}

#[cfg(not(target_os = "windows"))]
fn protect_secret(_secret: &str) -> Result<String, String> {
    Err("Secure credential storage is only implemented on Windows".to_string())
}

#[cfg(target_os = "windows")]
fn unprotect_secret(stored: &str) -> Result<String, String> {
    let Some(encoded) = stored.strip_prefix(PROTECTED_SECRET_PREFIX) else {
        return Ok(stored.to_string());
    };
    let mut protected = BASE64
        .decode(encoded)
        .map_err(|err| format!("Protected API credential is not valid base64: {err}"))?;
    let input = CRYPT_INTEGER_BLOB {
        cbData: protected.len() as u32,
        pbData: protected.as_mut_ptr(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptUnprotectData(
            &input,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
        .map_err(|err| format!("Windows could not decrypt an API credential: {err}"))?;
        let plaintext = std::slice::from_raw_parts(output.pbData, output.cbData as usize);
        let result = String::from_utf8(plaintext.to_vec())
            .map_err(|err| format!("Decrypted API credential is not UTF-8: {err}"));
        let _ = LocalFree(HLOCAL(output.pbData.cast()));
        result
    }
}

#[cfg(not(target_os = "windows"))]
fn unprotect_secret(stored: &str) -> Result<String, String> {
    Ok(stored.to_string())
}

fn protect_settings_secrets(settings: &mut AppSettings) -> Result<(), String> {
    settings.api_key = protect_secret(&settings.api_key)?;
    settings.elevenlabs_api_key = protect_secret(&settings.elevenlabs_api_key)?;
    settings.openai_api_key = protect_secret(&settings.openai_api_key)?;
    settings.gemini_api_key = protect_secret(&settings.gemini_api_key)?;
    Ok(())
}

fn unprotect_settings_secrets(settings: &mut AppSettings) {
    for (label, value) in [
        ("legacy ElevenLabs", &mut settings.api_key),
        ("ElevenLabs", &mut settings.elevenlabs_api_key),
        ("OpenAI", &mut settings.openai_api_key),
        ("Gemini", &mut settings.gemini_api_key),
    ] {
        match unprotect_secret(value) {
            Ok(secret) => *value = secret,
            Err(err) => {
                crate::echo_error!(
                    "settings",
                    "Failed to decrypt {label} API credential: {err}"
                );
                value.clear();
            }
        }
    }
}

fn temporary_settings_path(path: &Path) -> PathBuf {
    let sequence = SETTINGS_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    path.with_extension(format!("json.tmp-{}-{sequence}", std::process::id()))
}

#[cfg(target_os = "windows")]
fn atomic_replace(temp_path: &Path, destination: &Path) -> Result<(), String> {
    if !destination.exists() {
        return fs::rename(temp_path, destination).map_err(|err| err.to_string());
    }
    let destination_wide: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let temp_wide: Vec<u16> = temp_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut last_error = None;
    for retry_delay_ms in [0, 10, 25, 50, 100] {
        if retry_delay_ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(retry_delay_ms));
        }
        let result = unsafe {
            ReplaceFileW(
                PCWSTR(destination_wide.as_ptr()),
                PCWSTR(temp_wide.as_ptr()),
                PCWSTR::null(),
                REPLACE_FILE_FLAGS(0),
                None,
                None,
            )
        };
        match result {
            Ok(()) => return Ok(()),
            Err(err) => last_error = Some(err),
        }
    }
    Err(last_error
        .map(|err| err.to_string())
        .unwrap_or_else(|| "Windows could not replace the destination file".to_string()))
}

#[cfg(not(target_os = "windows"))]
fn atomic_replace(temp_path: &Path, destination: &Path) -> Result<(), String> {
    fs::rename(temp_path, destination).map_err(|err| err.to_string())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    #[serde(default)]
    pub start_with_windows: bool,
    #[serde(default)]
    pub local_sherpa: LocalSherpaConfig,
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
            start_with_windows: false,
            local_sherpa: LocalSherpaConfig::default(),
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
        self.local_sherpa = self.local_sherpa.clone().normalized();
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
            TranscriptionProvider::LocalSherpaOnnx => (
                String::new(),
                provider.default_model_id().to_string(),
                "en".to_string(),
                false,
            ),
        };

        TranscriptionConfig {
            provider,
            api_key,
            model_id,
            language_code,
            no_verbatim,
            local: self.local_sherpa.clone(),
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
                TranscriptionProvider::LocalSherpaOnnx => {}
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

pub fn transcript_history_path() -> PathBuf {
    settings_path().with_file_name("transcripts.json")
}

pub fn load_transcript_history() -> Vec<TranscriptHistoryEntry> {
    load_transcript_history_from_path(&transcript_history_path())
}

fn load_transcript_history_from_path(path: &Path) -> Vec<TranscriptHistoryEntry> {
    if fs::metadata(path)
        .map(|metadata| metadata.len() > MAX_TRANSCRIPT_HISTORY_FILE_BYTES)
        .unwrap_or(false)
    {
        crate::echo_warn!(
            "history",
            "Transcript history file is too large; ignoring it"
        );
        return Vec::new();
    }

    let Ok(contents) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(mut entries) = serde_json::from_str::<Vec<TranscriptHistoryEntry>>(&contents) else {
        crate::echo_warn!("history", "Transcript history file is invalid; ignoring it");
        return Vec::new();
    };
    entries.retain(|entry| !entry.text.trim().is_empty());
    entries.truncate(MAX_TRANSCRIPT_HISTORY);
    entries
}

pub fn save_transcript_history(entries: &[TranscriptHistoryEntry]) -> bool {
    save_transcript_history_to_path(&transcript_history_path(), entries)
}

fn save_transcript_history_to_path(path: &Path, entries: &[TranscriptHistoryEntry]) -> bool {
    if let Some(parent) = path.parent() {
        if let Err(err) = fs::create_dir_all(parent) {
            crate::echo_error!(
                "history",
                "Failed to create transcript history directory: {err}"
            );
            return false;
        }
    }

    let bounded = &entries[..entries.len().min(MAX_TRANSCRIPT_HISTORY)];
    let Ok(json) = serde_json::to_vec_pretty(bounded) else {
        crate::echo_error!("history", "Failed to serialize transcript history");
        return false;
    };
    let temp_path = temporary_settings_path(path);
    let write_result = (|| -> Result<(), String> {
        let mut file = File::create(&temp_path).map_err(|err| err.to_string())?;
        file.write_all(&json).map_err(|err| err.to_string())?;
        file.sync_all().map_err(|err| err.to_string())?;
        drop(file);
        atomic_replace(&temp_path, path)
    })();
    if let Err(err) = write_result {
        let _ = fs::remove_file(&temp_path);
        crate::echo_error!(
            "history",
            "Failed to save transcript history atomically: {err}"
        );
        return false;
    }
    true
}

pub fn load_settings() -> AppSettings {
    load_settings_from_path(&settings_path())
}

pub fn load_settings_from_path(path: &Path) -> AppSettings {
    if let Ok(contents) = fs::read_to_string(path) {
        if let Ok(mut settings) = serde_json::from_str::<AppSettings>(&contents) {
            unprotect_settings_secrets(&mut settings);
            settings.normalize_transcription_settings();
            return settings;
        }
    }
    AppSettings::default()
}

pub fn save_settings(settings: &AppSettings) -> bool {
    save_settings_to_path(&settings_path(), settings)
}

pub fn save_settings_to_path(path: &Path, settings: &AppSettings) -> bool {
    let mut settings = settings.clone();
    settings.normalize_transcription_settings();
    if let Err(err) = protect_settings_secrets(&mut settings) {
        crate::echo_error!("settings", "Failed to protect API credentials: {err}");
        return false;
    }

    // Ensure the target directory exists (create the per-user folder if needed)
    if let Some(parent) = path.parent() {
        if let Err(err) = fs::create_dir_all(parent) {
            crate::echo_error!(
                "settings",
                "Failed to create settings directory {:?}: {}",
                parent,
                err
            );
            return false;
        }
    }

    match serde_json::to_string_pretty(&settings) {
        Ok(json) => {
            let temp_path = temporary_settings_path(path);
            let write_result = (|| -> Result<(), String> {
                let mut file = File::create(&temp_path).map_err(|err| err.to_string())?;
                file.write_all(json.as_bytes())
                    .map_err(|err| err.to_string())?;
                file.sync_all().map_err(|err| err.to_string())?;
                drop(file);
                atomic_replace(&temp_path, path)
            })();
            if let Err(err) = write_result {
                let _ = fs::remove_file(&temp_path);
                crate::echo_error!("settings", "Failed to save settings atomically: {err}");
                return false;
            }
            true
        }
        Err(err) => {
            crate::echo_error!("settings", "Failed to serialize settings: {}", err);
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        load_settings_from_path, load_transcript_history_from_path, save_settings_to_path,
        save_transcript_history_to_path, AppSettings, TranscriptHistoryEntry,
        MAX_TRANSCRIPT_HISTORY,
    };
    use crate::transcription::{LocalSherpaConfig, TranscriptionProvider};
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
            start_with_windows: false,
            local_sherpa: LocalSherpaConfig::default(),
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
        assert_eq!(loaded.start_with_windows, expected.start_with_windows);
    }

    #[test]
    fn transcript_history_roundtrip_is_capped_at_five_hundred() {
        let path = unique_path();
        let entries = (0..505)
            .map(|index| TranscriptHistoryEntry {
                timestamp: format!("2026-07-20 12:{:02}:00", index % 60),
                text: format!("Transcript {index}"),
            })
            .collect::<Vec<_>>();

        assert!(save_transcript_history_to_path(&path, &entries));
        let loaded = load_transcript_history_from_path(&path);
        let _ = fs::remove_file(&path);

        assert_eq!(loaded.len(), MAX_TRANSCRIPT_HISTORY);
        assert_eq!(loaded.first().unwrap().text, "Transcript 0");
        assert_eq!(loaded.last().unwrap().text, "Transcript 499");
    }

    #[test]
    fn invalid_transcript_history_is_ignored() {
        let path = unique_path();
        fs::write(&path, "{not-json").unwrap();
        let loaded = load_transcript_history_from_path(&path);
        let _ = fs::remove_file(&path);
        assert!(loaded.is_empty());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn saved_api_credentials_are_dpapi_protected() {
        let path = unique_path();
        let settings = AppSettings {
            api_key: "legacy-secret-value".to_string(),
            elevenlabs_api_key: "eleven-secret-value".to_string(),
            openai_api_key: "dpapi:literal-openai-secret-value".to_string(),
            gemini_api_key: "gemini-secret-value".to_string(),
            ..Default::default()
        };

        assert!(save_settings_to_path(&path, &settings));
        let stored = fs::read_to_string(&path).unwrap();
        assert!(stored.contains("dpapi:"));
        assert!(!stored.contains("legacy-secret-value"));
        assert!(!stored.contains("eleven-secret-value"));
        assert!(!stored.contains("literal-openai-secret-value"));
        assert!(!stored.contains("gemini-secret-value"));

        let loaded = load_settings_from_path(&path);
        let _ = fs::remove_file(&path);
        assert_eq!(loaded.api_key, settings.api_key);
        assert_eq!(loaded.elevenlabs_api_key, settings.elevenlabs_api_key);
        assert_eq!(loaded.openai_api_key, settings.openai_api_key);
        assert_eq!(loaded.gemini_api_key, settings.gemini_api_key);
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
        assert!(!loaded.start_with_windows);
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
