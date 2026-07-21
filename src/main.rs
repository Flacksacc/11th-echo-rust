#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

mod audio;
mod diagnostics;
mod gemini;
mod hotkey;
mod injector;
mod pipeline;
mod settings;
mod startup;
mod state;
mod transcription;

use arboard::Clipboard;
use chrono::Local;
use pipeline::TranscriptPipeline;
use settings::{
    load_settings, load_transcript_history, save_settings, save_transcript_history, AppSettings,
    TranscriptHistoryEntry, MAX_TRANSCRIPT_HISTORY,
};
use slint::{CloseRequestResponse, Color, ComponentHandle, ModelRc, SharedString, VecModel};
use state::RecordingState;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
};
use std::thread;
use tokio::runtime::Runtime;
use tokio::sync::mpsc;

const FINALIZATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

#[cfg(target_os = "windows")]
use global_hotkey::{
    hotkey::{Code, HotKey, Modifiers},
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
};
#[cfg(target_os = "windows")]
use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem},
    MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent,
};
#[cfg(target_os = "windows")]
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
#[cfg(target_os = "windows")]
use windows::Win32::System::Threading::CreateMutexW;
#[cfg(target_os = "windows")]
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_CONTROL, VK_ESCAPE, VK_F1, VK_F10, VK_F11, VK_F12, VK_F2, VK_F3, VK_F4,
    VK_F5, VK_F6, VK_F7, VK_F8, VK_F9, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT, VK_SPACE,
};
#[cfg(target_os = "windows")]
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GetSystemMetrics, GetWindowLongPtrW, GetWindowThreadProcessId,
    SetForegroundWindow, SetWindowLongPtrW, ShowWindow, GWL_EXSTYLE, SM_CYSCREEN, SW_RESTORE,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};

slint::include_modules!();

#[derive(Debug)]
enum AppCommand {
    ToggleRecording,
    StartRecording,
    StopRecording,
    LocalEngineLoaded(Result<(), String>),
    FinalizationTimedOut(u64),
}

fn app_command_name(command: &AppCommand) -> &'static str {
    match command {
        AppCommand::ToggleRecording => "toggle_recording",
        AppCommand::StartRecording => "start_recording",
        AppCommand::StopRecording => "stop_recording",
        AppCommand::LocalEngineLoaded(_) => "local_engine_loaded",
        AppCommand::FinalizationTimedOut(_) => "finalization_timed_out",
    }
}

struct Session {
    epoch: u64,
    state: Arc<Mutex<RecordingState>>,
    audio_stream: Option<cpal::Stream>,
    audio_forward_stop_tx: Option<mpsc::UnboundedSender<()>>,
    network_stop_tx: Option<mpsc::UnboundedSender<transcription::TranscriptionCommand>>,
    transcript_pipeline: Arc<Mutex<TranscriptPipeline>>,
    task_abort_handles: Vec<tokio::task::AbortHandle>,
    finalization_watchdog: Option<tokio::task::AbortHandle>,
}

impl Session {
    fn request_stop(&mut self) {
        // Queue the provider's finalization command before closing its audio
        // stream. Providers still finalize on audio closure as a fallback.
        if let Some(tx) = self.network_stop_tx.as_ref() {
            let _ = tx.send(transcription::TranscriptionCommand::Stop);
        }
        if let Some(tx) = self.audio_forward_stop_tx.take() {
            let _ = tx.send(());
        }
    }

    fn abort_tasks(&self) {
        for handle in &self.task_abort_handles {
            handle.abort();
        }
    }

    fn cancel_finalization_watchdog(&mut self) {
        if let Some(handle) = self.finalization_watchdog.take() {
            handle.abort();
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct AudioForwardStats {
    reason: &'static str,
    chunks: u64,
    samples: u64,
}

async fn forward_audio_until_stopped(
    mut captured_audio_rx: mpsc::Receiver<Vec<i16>>,
    network_audio_tx: mpsc::Sender<Vec<i16>>,
    mut stop_rx: mpsc::UnboundedReceiver<()>,
) -> AudioForwardStats {
    let mut chunks = 0u64;
    let mut samples = 0u64;
    loop {
        tokio::select! {
            biased;
            _ = stop_rx.recv() => {
                // Closing the receiver makes shutdown deterministic even if an
                // audio backend retains its callback sender briefly after the
                // CPAL stream is dropped. Drain only the chunks already queued.
                captured_audio_rx.close();
                while let Some(chunk) = captured_audio_rx.recv().await {
                    let chunk_samples = chunk.len() as u64;
                    if network_audio_tx.send(chunk).await.is_err() {
                        return AudioForwardStats { reason: "network_closed", chunks, samples };
                    }
                    chunks += 1;
                    samples += chunk_samples;
                }
                return AudioForwardStats { reason: "stop_requested", chunks, samples };
            }
            chunk = captured_audio_rx.recv() => {
                let Some(chunk) = chunk else {
                    return AudioForwardStats { reason: "capture_closed", chunks, samples };
                };
                let chunk_samples = chunk.len() as u64;
                if network_audio_tx.send(chunk).await.is_err() {
                    return AudioForwardStats { reason: "network_closed", chunks, samples };
                }
                chunks += 1;
                samples += chunk_samples;
            }
        }
    }
}

#[cfg(target_os = "windows")]
struct SingleInstanceGuard(HANDLE);

#[cfg(target_os = "windows")]
impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

#[cfg(target_os = "windows")]
fn acquire_single_instance() -> Result<Option<SingleInstanceGuard>, windows::core::Error> {
    unsafe {
        // A Local mutex limits enforcement to the current Windows session and
        // avoids requiring elevated global-object permissions.
        let handle = CreateMutexW(
            None,
            false,
            windows::core::w!("Local\\Echo-9D197E31-8816-4CB5-8753-3FD8664A9556"),
        )?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let _ = CloseHandle(handle);
            return Ok(None);
        }
        Ok(Some(SingleInstanceGuard(handle)))
    }
}

#[cfg(target_os = "windows")]
fn show_existing_instance() {
    unsafe {
        let hwnd = FindWindowW(None, windows::core::w!("Echo"));
        if hwnd.0 != 0 {
            let _ = ShowWindow(hwnd, SW_RESTORE);
            let activated = SetForegroundWindow(hwnd).as_bool();
            echo_info!(
                "instance",
                "Existing window activation attempted hwnd=0x{:X} activated={}",
                hwnd.0 as usize,
                activated
            );
        } else {
            echo_warn!(
                "instance",
                "Existing instance mutex was found but its main window was unavailable"
            );
        }
    }
}

#[cfg(target_os = "windows")]
fn parse_hotkey(input: &str) -> Result<HotKey, String> {
    use hotkey::HotkeyKey;

    let spec = hotkey::parse_hotkey_spec(input)?;
    let mut modifiers = Modifiers::empty();
    if spec.ctrl {
        modifiers |= Modifiers::CONTROL;
    }
    if spec.shift {
        modifiers |= Modifiers::SHIFT;
    }
    if spec.alt {
        modifiers |= Modifiers::ALT;
    }
    if spec.meta {
        modifiers |= Modifiers::META;
    }

    let key = match spec.key {
        HotkeyKey::Space => Code::Space,
        HotkeyKey::Letter('A') => Code::KeyA,
        HotkeyKey::Letter('B') => Code::KeyB,
        HotkeyKey::Letter('C') => Code::KeyC,
        HotkeyKey::Letter('D') => Code::KeyD,
        HotkeyKey::Letter('E') => Code::KeyE,
        HotkeyKey::Letter('F') => Code::KeyF,
        HotkeyKey::Letter('G') => Code::KeyG,
        HotkeyKey::Letter('H') => Code::KeyH,
        HotkeyKey::Letter('I') => Code::KeyI,
        HotkeyKey::Letter('J') => Code::KeyJ,
        HotkeyKey::Letter('K') => Code::KeyK,
        HotkeyKey::Letter('L') => Code::KeyL,
        HotkeyKey::Letter('M') => Code::KeyM,
        HotkeyKey::Letter('N') => Code::KeyN,
        HotkeyKey::Letter('O') => Code::KeyO,
        HotkeyKey::Letter('P') => Code::KeyP,
        HotkeyKey::Letter('Q') => Code::KeyQ,
        HotkeyKey::Letter('R') => Code::KeyR,
        HotkeyKey::Letter('S') => Code::KeyS,
        HotkeyKey::Letter('T') => Code::KeyT,
        HotkeyKey::Letter('U') => Code::KeyU,
        HotkeyKey::Letter('V') => Code::KeyV,
        HotkeyKey::Letter('W') => Code::KeyW,
        HotkeyKey::Letter('X') => Code::KeyX,
        HotkeyKey::Letter('Y') => Code::KeyY,
        HotkeyKey::Letter('Z') => Code::KeyZ,
        HotkeyKey::Letter(other) => return Err(format!("Unsupported letter key: {}", other)),
        HotkeyKey::Digit(0) => Code::Digit0,
        HotkeyKey::Digit(1) => Code::Digit1,
        HotkeyKey::Digit(2) => Code::Digit2,
        HotkeyKey::Digit(3) => Code::Digit3,
        HotkeyKey::Digit(4) => Code::Digit4,
        HotkeyKey::Digit(5) => Code::Digit5,
        HotkeyKey::Digit(6) => Code::Digit6,
        HotkeyKey::Digit(7) => Code::Digit7,
        HotkeyKey::Digit(8) => Code::Digit8,
        HotkeyKey::Digit(9) => Code::Digit9,
        HotkeyKey::Digit(other) => return Err(format!("Unsupported digit key: {}", other)),
        HotkeyKey::Function(1) => Code::F1,
        HotkeyKey::Function(2) => Code::F2,
        HotkeyKey::Function(3) => Code::F3,
        HotkeyKey::Function(4) => Code::F4,
        HotkeyKey::Function(5) => Code::F5,
        HotkeyKey::Function(6) => Code::F6,
        HotkeyKey::Function(7) => Code::F7,
        HotkeyKey::Function(8) => Code::F8,
        HotkeyKey::Function(9) => Code::F9,
        HotkeyKey::Function(10) => Code::F10,
        HotkeyKey::Function(11) => Code::F11,
        HotkeyKey::Function(12) => Code::F12,
        HotkeyKey::Function(other) => return Err(format!("Unsupported function key: F{}", other)),
    };
    Ok(HotKey::new(Some(modifiers), key))
}

#[cfg(target_os = "windows")]
fn apply_hotkey(
    manager: &GlobalHotKeyManager,
    current_hotkey: &mut Option<HotKey>,
    hotkey_text: &str,
) -> Result<u32, String> {
    let new_hotkey = parse_hotkey(hotkey_text)?;
    let previous = *current_hotkey;

    if let Some(existing) = previous {
        let _ = manager.unregister(existing);
    }

    match manager.register(new_hotkey) {
        Ok(_) => {
            let id = new_hotkey.id();
            *current_hotkey = Some(new_hotkey);
            Ok(id)
        }
        Err(err) => {
            if let Some(existing) = previous {
                let _ = manager.register(existing);
                *current_hotkey = Some(existing);
            }
            let message = err.to_string();
            if message.contains("AlreadyRegistered") {
                Err("Hotkey is already registered by another application".to_string())
            } else {
                Err(message)
            }
        }
    }
}

#[cfg(target_os = "windows")]
fn vk_down(vk: i32) -> bool {
    unsafe { (GetAsyncKeyState(vk) & 0x8000u16 as i16) != 0 }
}

#[cfg(target_os = "windows")]
fn detect_hotkey_combo() -> Option<String> {
    let mut mods: Vec<&str> = Vec::new();
    if vk_down(VK_CONTROL.0 as i32) {
        mods.push("Ctrl");
    }
    if vk_down(VK_SHIFT.0 as i32) {
        mods.push("Shift");
    }
    if vk_down(VK_MENU.0 as i32) {
        mods.push("Alt");
    }
    if vk_down(VK_LWIN.0 as i32) || vk_down(VK_RWIN.0 as i32) {
        mods.push("Win");
    }

    if mods.is_empty() {
        return None;
    }

    let keys: [(&str, i32); 49] = [
        ("Space", VK_SPACE.0 as i32),
        ("A", 0x41),
        ("B", 0x42),
        ("C", 0x43),
        ("D", 0x44),
        ("E", 0x45),
        ("F", 0x46),
        ("G", 0x47),
        ("H", 0x48),
        ("I", 0x49),
        ("J", 0x4A),
        ("K", 0x4B),
        ("L", 0x4C),
        ("M", 0x4D),
        ("N", 0x4E),
        ("O", 0x4F),
        ("P", 0x50),
        ("Q", 0x51),
        ("R", 0x52),
        ("S", 0x53),
        ("T", 0x54),
        ("U", 0x55),
        ("V", 0x56),
        ("W", 0x57),
        ("X", 0x58),
        ("Y", 0x59),
        ("Z", 0x5A),
        ("0", 0x30),
        ("1", 0x31),
        ("2", 0x32),
        ("3", 0x33),
        ("4", 0x34),
        ("5", 0x35),
        ("6", 0x36),
        ("7", 0x37),
        ("8", 0x38),
        ("9", 0x39),
        ("F1", VK_F1.0 as i32),
        ("F2", VK_F2.0 as i32),
        ("F3", VK_F3.0 as i32),
        ("F4", VK_F4.0 as i32),
        ("F5", VK_F5.0 as i32),
        ("F6", VK_F6.0 as i32),
        ("F7", VK_F7.0 as i32),
        ("F8", VK_F8.0 as i32),
        ("F9", VK_F9.0 as i32),
        ("F10", VK_F10.0 as i32),
        ("F11", VK_F11.0 as i32),
        ("F12", VK_F12.0 as i32),
    ];

    for (label, vk) in keys {
        if vk_down(vk) {
            return Some(format!("{}+{}", mods.join("+"), label));
        }
    }

    None
}

fn parse_theme_color(s: &str, default: Color) -> Color {
    let trimmed = s.trim().trim_start_matches('#');
    if trimmed.len() == 6 {
        if let (Ok(r), Ok(g), Ok(b)) = (
            u8::from_str_radix(&trimmed[0..2], 16),
            u8::from_str_radix(&trimmed[2..4], 16),
            u8::from_str_radix(&trimmed[4..6], 16),
        ) {
            return Color::from_rgb_u8(r, g, b);
        }
    }
    default
}

fn overlay_size_for_text(text: &str) -> (i32, i32) {
    let chars = text.chars().count().max(1);
    let width = 520;
    let chars_per_line = 58usize;
    let lines = chars.div_ceil(chars_per_line).clamp(1, 8) as i32;
    let height = (70 + (lines * 30)).clamp(110, 260);
    (width, height)
}

fn live_transcript_text(committed: &str, partial: &str) -> String {
    let committed = committed.trim();
    let partial = partial.trim();

    if partial.is_empty() {
        return committed.to_string();
    }

    if committed.is_empty() || partial.starts_with(committed) {
        return partial.to_string();
    }

    format!("{} {}", committed, partial)
}

fn reset_overlay_to_listening(overlay: &TranscriptOverlayWindow) {
    overlay.set_sentence_text("Listening...".into());
    overlay.set_window_width(520);
    overlay.set_window_height(120);
    overlay.set_is_error(false);
    overlay.set_is_system_message(false);
    overlay.set_is_visible(true);
    let _ = overlay.show();
    #[cfg(target_os = "windows")]
    if let Err(err) = configure_overlay_as_non_activating(overlay) {
        echo_warn!(
            "overlay",
            "Could not apply non-activating overlay style: {err}"
        );
    }
}

#[cfg(target_os = "windows")]
fn configure_overlay_as_non_activating(
    _overlay: &TranscriptOverlayWindow,
) -> Result<(), Box<dyn std::error::Error>> {
    unsafe {
        // Looking up the HWND directly avoids depending on optional raw-window-
        // handle support in the selected Slint renderer.
        let hwnd = FindWindowW(None, windows::core::w!("Echo Live"));
        if hwnd.0 == 0 {
            return Err("Windows could not find the Echo live overlay".into());
        }
        let mut owner_process_id = 0;
        if GetWindowThreadProcessId(hwnd, Some(&mut owner_process_id)) == 0
            || owner_process_id != std::process::id()
        {
            return Err("The Echo live overlay belongs to another process".into());
        }
        let existing_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let overlay_style =
            existing_style | WS_EX_NOACTIVATE.0 as isize | WS_EX_TOOLWINDOW.0 as isize;
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, overlay_style);
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn default_overlay_position() -> slint::PhysicalPosition {
    let screen_h = unsafe { GetSystemMetrics(SM_CYSCREEN) };
    let overlay_h = 120;
    let margin = 24;
    slint::PhysicalPosition::new(margin, (screen_h - overlay_h - margin).max(0))
}

#[cfg(not(target_os = "windows"))]
fn default_overlay_position() -> slint::LogicalPosition {
    slint::LogicalPosition::new(24.0, 820.0)
}

#[cfg(target_os = "windows")]
fn load_tray_icon() -> Result<tray_icon::Icon, Box<dyn std::error::Error>> {
    let mut candidates = Vec::new();
    if let Some(executable_dir) = std::env::current_exe()?.parent() {
        candidates.push(executable_dir.join("eleventhecho.png"));
        candidates.push(executable_dir.join("eleventhecho.ico"));
    }
    candidates.push(std::path::PathBuf::from("eleventhecho.png"));
    candidates.push(std::path::PathBuf::from("eleventhecho.ico"));

    let mut failures = Vec::new();
    for path in candidates {
        match tray_icon::Icon::from_path(&path, None) {
            Ok(icon) => return Ok(icon),
            Err(err) => failures.push(format!("{}: {err}", path.display())),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!("No usable tray icon was found ({})", failures.join("; ")),
    )
    .into())
}

fn select_ui_backend() -> Result<(), slint::PlatformError> {
    #[cfg(target_os = "windows")]
    if std::env::var_os("SLINT_BACKEND").is_none() {
        return slint::BackendSelector::new()
            .backend_name("winit".into())
            .renderer_name("software".into())
            .select();
    }

    Ok(())
}

fn settings_snapshot_from_ui(ui: &AppWindow, base: &AppSettings) -> AppSettings {
    let mut next = base.clone();
    next.api_key = ui.get_api_key_text().to_string();
    next.elevenlabs_api_key = next.api_key.clone();
    next.elevenlabs_model = ui.get_elevenlabs_model_text().to_string();
    next.elevenlabs_language_code = ui.get_elevenlabs_language_code_text().to_string();
    next.elevenlabs_no_verbatim = ui.get_elevenlabs_no_verbatim();
    next.openai_api_key = ui.get_openai_api_key_text().to_string();
    next.openai_model = ui.get_openai_model_text().to_string();
    next.openai_language_code = ui.get_openai_language_code_text().to_string();
    next.transcription_provider =
        transcription::TranscriptionProvider::from_id(&ui.get_transcription_provider_text())
            .id()
            .to_string();
    match transcription::TranscriptionProvider::from_id(&next.transcription_provider) {
        transcription::TranscriptionProvider::ElevenLabsRealtime => {
            next.transcription_model = next.elevenlabs_model.clone();
            next.transcription_language_code = next.elevenlabs_language_code.clone();
            next.transcription_no_verbatim = next.elevenlabs_no_verbatim;
        }
        transcription::TranscriptionProvider::OpenAiRealtimeWhisper => {
            next.transcription_model = next.openai_model.clone();
            next.transcription_language_code = next.openai_language_code.clone();
            next.transcription_no_verbatim = false;
        }
        transcription::TranscriptionProvider::LocalSherpaOnnx => {
            next.transcription_model = transcription::TranscriptionProvider::LocalSherpaOnnx
                .default_model_id()
                .to_string();
            next.transcription_language_code = "en".to_string();
            next.transcription_no_verbatim = false;
        }
    }
    next.gemini_api_key = ui.get_gemini_api_key_text().to_string();
    next.gemini_enabled = ui.get_use_gemini_modifier();
    next.gemini_model = ui.get_gemini_model_text().to_string();
    next.gemini_prompt_preset = ui.get_selected_gemini_preset().to_string();
    next.gemini_custom_prompt = ui.get_gemini_custom_prompt().to_string();
    next.selected_microphone = ui.get_selected_microphone().to_string();
    next.use_default_microphone = ui.get_use_default_microphone();
    next.hotkey_text = ui.get_hotkey_text().to_string();
    next.start_with_windows = ui.get_start_with_windows();
    next.local_sherpa = transcription::LocalSherpaConfig {
        num_threads: ui.get_local_cpu_threads().round() as i32,
        vad_threshold: ui.get_local_vad_threshold(),
        silence_ms: ui.get_local_silence_ms().round() as u32,
        pre_roll_ms: ui.get_local_pre_roll_ms().round() as u32,
        post_roll_ms: ui.get_local_post_roll_ms().round() as u32,
        min_speech_ms: ui.get_local_min_speech_ms().round() as u32,
        max_segment_seconds: ui.get_local_max_segment_seconds().round() as u32,
        partial_interval_ms: ui.get_local_partial_interval_ms().round() as u32,
        redecode_full_session: ui.get_local_redecode_full_session(),
    }
    .normalized();
    next.overlay_opacity = ui.get_overlay_opacity();
    next.theme_background_top_color = ui.get_theme_background_top_color().to_string();
    next.theme_background_bottom_color = ui.get_theme_background_bottom_color().to_string();
    next.theme_window_color = ui.get_theme_window_color().to_string();
    next.theme_button_accent_color = ui.get_theme_button_accent_color().to_string();
    next.theme_title_color = ui.get_theme_title_color().to_string();
    next.theme_text_color = ui.get_theme_text_color().to_string();
    next.overlay_background_color = ui.get_overlay_background_color().to_string();
    next.overlay_text_color = ui.get_overlay_text_color().to_string();
    next.normalize_transcription_settings();
    next
}

fn populate_settings_editor(ui: &AppWindow, settings: &AppSettings) {
    let provider = transcription::TranscriptionProvider::from_id(&settings.transcription_provider);
    ui.set_transcription_provider_text(provider.label().into());
    ui.set_api_key_text(settings.elevenlabs_api_key.clone().into());
    ui.set_elevenlabs_model_text(settings.elevenlabs_model.clone().into());
    ui.set_elevenlabs_language_code_text(settings.elevenlabs_language_code.clone().into());
    ui.set_elevenlabs_no_verbatim(settings.elevenlabs_no_verbatim);
    ui.set_openai_api_key_text(settings.openai_api_key.clone().into());
    ui.set_openai_model_text(settings.openai_model.clone().into());
    ui.set_openai_language_code_text(settings.openai_language_code.clone().into());
    ui.set_local_cpu_threads(settings.local_sherpa.num_threads as f32);
    ui.set_local_vad_threshold(settings.local_sherpa.vad_threshold);
    ui.set_local_silence_ms(settings.local_sherpa.silence_ms as f32);
    ui.set_local_pre_roll_ms(settings.local_sherpa.pre_roll_ms as f32);
    ui.set_local_post_roll_ms(settings.local_sherpa.post_roll_ms as f32);
    ui.set_local_min_speech_ms(settings.local_sherpa.min_speech_ms as f32);
    ui.set_local_max_segment_seconds(settings.local_sherpa.max_segment_seconds as f32);
    ui.set_local_partial_interval_ms(settings.local_sherpa.partial_interval_ms as f32);
    ui.set_local_redecode_full_session(settings.local_sherpa.redecode_full_session);
    ui.set_gemini_api_key_text(settings.gemini_api_key.clone().into());
    ui.set_use_gemini_modifier(settings.gemini_enabled);
    ui.set_gemini_model_text(settings.gemini_model.clone().into());
    ui.set_selected_gemini_preset(settings.gemini_prompt_preset.clone().into());
    ui.set_gemini_custom_prompt(settings.gemini_custom_prompt.clone().into());
    ui.set_selected_microphone(settings.selected_microphone.clone().into());
    ui.set_use_default_microphone(settings.use_default_microphone);
    ui.set_hotkey_text(settings.hotkey_text.clone().into());
    ui.set_start_with_windows(settings.start_with_windows);
    ui.set_overlay_opacity(settings.overlay_opacity);
    ui.set_theme_background_top_color(parse_theme_color(
        &settings.theme_background_top_color,
        Color::from_rgb_u8(2, 20, 11),
    ));
    ui.set_theme_background_bottom_color(parse_theme_color(
        &settings.theme_background_bottom_color,
        Color::from_rgb_u8(0, 8, 6),
    ));
    ui.set_theme_window_color(parse_theme_color(
        &settings.theme_window_color,
        Color::from_rgb_u8(4, 27, 17),
    ));
    ui.set_theme_button_accent_color(parse_theme_color(
        &settings.theme_button_accent_color,
        Color::from_rgb_u8(74, 222, 128),
    ));
    ui.set_theme_title_color(parse_theme_color(
        &settings.theme_title_color,
        Color::from_rgb_u8(228, 255, 233),
    ));
    ui.set_theme_text_color(parse_theme_color(
        &settings.theme_text_color,
        Color::from_rgb_u8(204, 239, 214),
    ));
    ui.set_overlay_background_color(parse_theme_color(
        &settings.overlay_background_color,
        Color::from_rgb_u8(3, 21, 12),
    ));
    ui.set_overlay_text_color(parse_theme_color(
        &settings.overlay_text_color,
        Color::from_rgb_u8(230, 255, 240),
    ));
}

fn settings_editor_is_dirty(ui: &AppWindow, saved: &AppSettings) -> bool {
    let draft = settings_snapshot_from_ui(ui, saved);
    let mut normalized_saved = saved.clone();
    normalized_saved.theme_background_top_color = parse_theme_color(
        &saved.theme_background_top_color,
        ui.get_theme_background_top_color(),
    )
    .to_string();
    normalized_saved.theme_background_bottom_color = parse_theme_color(
        &saved.theme_background_bottom_color,
        ui.get_theme_background_bottom_color(),
    )
    .to_string();
    normalized_saved.theme_window_color =
        parse_theme_color(&saved.theme_window_color, ui.get_theme_window_color()).to_string();
    normalized_saved.theme_button_accent_color = parse_theme_color(
        &saved.theme_button_accent_color,
        ui.get_theme_button_accent_color(),
    )
    .to_string();
    normalized_saved.theme_title_color =
        parse_theme_color(&saved.theme_title_color, ui.get_theme_title_color()).to_string();
    normalized_saved.theme_text_color =
        parse_theme_color(&saved.theme_text_color, ui.get_theme_text_color()).to_string();
    normalized_saved.overlay_background_color = parse_theme_color(
        &saved.overlay_background_color,
        ui.get_overlay_background_color(),
    )
    .to_string();
    normalized_saved.overlay_text_color =
        parse_theme_color(&saved.overlay_text_color, ui.get_overlay_text_color()).to_string();
    normalized_saved.normalize_transcription_settings();
    draft != normalized_saved
}

fn request_local_model_prompt(ui: &AppWindow) {
    if ui.get_unsaved_settings_prompt_visible() {
        ui.set_local_repair_pending(true);
    } else {
        ui.set_local_repair_pending(false);
        ui.set_local_download_prompt_visible(true);
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    match diagnostics::init() {
        Ok(path) => {
            diagnostics::install_panic_hook();
            echo_info!(
                "app",
                "Echo diagnostic log initialized path={}",
                path.display()
            );
        }
        Err(err) => eprintln!("Failed to initialize Echo diagnostic logging: {err}"),
    }
    echo_info!(
        "app",
        "Echo starting version={} os={} arch={} executable={}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        std::env::current_exe()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|err| format!("unavailable:{err}"))
    );
    #[cfg(target_os = "windows")]
    let _single_instance_guard = match acquire_single_instance()? {
        Some(guard) => {
            echo_info!("instance", "Single-instance mutex acquired");
            guard
        }
        None => {
            echo_info!(
                "instance",
                "Echo is already running; activating the existing window"
            );
            show_existing_instance();
            return Ok(());
        }
    };
    select_ui_backend()?;

    let microphones = audio::list_input_devices();
    let default_microphone =
        audio::default_input_device_name().unwrap_or_else(|| "Unavailable".to_string());
    let mut initial_settings = load_settings();
    #[cfg(target_os = "windows")]
    match startup::is_enabled() {
        Ok(enabled) => initial_settings.start_with_windows = enabled,
        Err(err) => echo_warn!("startup", "Failed to read Windows startup setting: {err}"),
    }
    if initial_settings.selected_microphone.trim().is_empty() {
        initial_settings.selected_microphone = if !default_microphone.is_empty() {
            default_microphone.clone()
        } else {
            microphones.first().cloned().unwrap_or_default()
        };
    }
    if initial_settings.hotkey_text.trim().is_empty() {
        initial_settings.hotkey_text = "Ctrl+Space".to_string();
    }
    echo_info!(
        "settings",
        "Settings loaded provider={} use_default_microphone={} start_with_windows={} local_threads={} gemini_enabled={} elevenlabs_key_present={} openai_key_present={} local_models_available={}",
        initial_settings.transcription_provider,
        initial_settings.use_default_microphone,
        initial_settings.start_with_windows,
        initial_settings.local_sherpa.num_threads,
        initial_settings.gemini_enabled,
        !initial_settings.elevenlabs_api_key.trim().is_empty(),
        !initial_settings.openai_api_key.trim().is_empty(),
        transcription::local_models_available()
    );
    save_settings(&initial_settings);
    let selected_microphone = initial_settings.selected_microphone.clone();
    let settings = Arc::new(Mutex::new(initial_settings.clone()));
    let initial_transcript_history = load_transcript_history();
    let transcript_history_store = Arc::new(Mutex::new(initial_transcript_history.clone()));
    let transcript_history_revision = Arc::new(AtomicU64::new(0));
    let transcript_raw_for_clipboard = Arc::new(Mutex::new(
        initial_transcript_history
            .iter()
            .map(|entry| entry.text.clone())
            .collect::<Vec<_>>(),
    ));
    if matches!(
        transcription::TranscriptionProvider::from_id(&initial_settings.transcription_provider),
        transcription::TranscriptionProvider::LocalSherpaOnnx
    ) && transcription::local_models_available()
    {
        transcription::preload_local_engine(&initial_settings.local_sherpa);
    }

    #[cfg(target_os = "windows")]
    let hotkey_text = Arc::new(Mutex::new(initial_settings.hotkey_text.clone()));

    #[cfg(target_os = "windows")]
    let hotkey_manager = Rc::new(GlobalHotKeyManager::new().unwrap());
    #[cfg(target_os = "windows")]
    let hotkey_state = Rc::new(RefCell::new(None::<HotKey>));
    #[cfg(target_os = "windows")]
    let hotkey_id_state = Rc::new(RefCell::new(None::<u32>));

    #[cfg(target_os = "windows")]
    {
        let startup_hotkey = hotkey_text.lock().unwrap().clone();
        match apply_hotkey(
            &hotkey_manager,
            &mut hotkey_state.borrow_mut(),
            &startup_hotkey,
        ) {
            Ok(id) => {
                echo_info!(
                    "hotkey",
                    "Global hotkey registered specification={} id={}",
                    startup_hotkey,
                    id
                );
                *hotkey_id_state.borrow_mut() = Some(id);
            }
            Err(err) => {
                echo_error!(
                    "hotkey",
                    "Failed to register global hotkey {}: {}. Continuing without hotkey support.",
                    startup_hotkey,
                    err
                );
            }
        }
    }

    #[cfg(target_os = "windows")]
    let (quit_item_id, settings_item_id, _tray_icon) = {
        let tray_menu = Menu::new();
        let settings_item = MenuItem::new("Settings Tab", true, None);
        let quit_item = MenuItem::new("Quit", true, None);
        tray_menu.append_items(&[&settings_item, &quit_item])?;

        let icon = load_tray_icon()?;
        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(tray_menu))
            .with_tooltip("Echo")
            .with_icon(icon)
            .build()?;
        (
            quit_item.id().clone(),
            settings_item.id().clone(),
            Some(tray),
        )
    };

    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<AppCommand>();
    let (level_tx, mut level_rx) = mpsc::channel::<f32>(10);

    let ui = AppWindow::new()?;
    ui.set_active_tab(0);
    ui.set_status_text("Idle".into());
    ui.set_is_recording(false);
    #[cfg(target_os = "windows")]
    ui.set_hotkey_text(hotkey_text.lock().unwrap().clone().into());
    #[cfg(not(target_os = "windows"))]
    ui.set_hotkey_text("Unavailable".into());
    let initial_provider =
        transcription::TranscriptionProvider::from_id(&initial_settings.transcription_provider);
    ui.set_transcription_provider_options(ModelRc::new(VecModel::from(vec![
        SharedString::from(transcription::DEFAULT_PROVIDER_LABEL),
        SharedString::from(transcription::OPENAI_REALTIME_WHISPER_PROVIDER_LABEL),
        SharedString::from(transcription::LOCAL_SHERPA_PROVIDER_LABEL),
    ])));
    ui.set_transcription_provider_text(initial_provider.label().into());
    ui.set_api_key_text(initial_settings.elevenlabs_api_key.clone().into());
    ui.set_elevenlabs_model_text(initial_settings.elevenlabs_model.clone().into());
    ui.set_elevenlabs_language_code_text(initial_settings.elevenlabs_language_code.clone().into());
    ui.set_elevenlabs_no_verbatim(initial_settings.elevenlabs_no_verbatim);
    ui.set_openai_api_key_text(initial_settings.openai_api_key.clone().into());
    ui.set_openai_model_text(initial_settings.openai_model.clone().into());
    ui.set_openai_language_code_text(initial_settings.openai_language_code.clone().into());
    ui.set_local_cpu_threads(initial_settings.local_sherpa.num_threads as f32);
    ui.set_local_max_cpu_threads(transcription::physical_core_count() as f32);
    ui.set_local_vad_threshold(initial_settings.local_sherpa.vad_threshold);
    ui.set_local_silence_ms(initial_settings.local_sherpa.silence_ms as f32);
    ui.set_local_pre_roll_ms(initial_settings.local_sherpa.pre_roll_ms as f32);
    ui.set_local_post_roll_ms(initial_settings.local_sherpa.post_roll_ms as f32);
    ui.set_local_min_speech_ms(initial_settings.local_sherpa.min_speech_ms as f32);
    ui.set_local_max_segment_seconds(initial_settings.local_sherpa.max_segment_seconds as f32);
    ui.set_local_partial_interval_ms(initial_settings.local_sherpa.partial_interval_ms as f32);
    ui.set_local_redecode_full_session(initial_settings.local_sherpa.redecode_full_session);
    // The settings workflow offers the model when Local CPU is selected.
    // Keep startup non-modal; existing Local users are prompted on opening the
    // Speech Engine settings page if their model is missing.
    ui.set_local_download_prompt_visible(false);
    ui.set_local_download_progress_visible(false);
    ui.set_local_download_progress(0.0);
    if matches!(
        initial_provider,
        transcription::TranscriptionProvider::LocalSherpaOnnx
    ) && transcription::local_models_available()
    {
        let preload_ui = ui.as_weak();
        thread::spawn(move || {
            if let Err(err) = transcription::wait_for_local_engine() {
                let _ = preload_ui.upgrade_in_event_loop(move |ui| {
                    ui.set_local_download_error(
                        format!("The local model could not load: {err}").into(),
                    );
                    request_local_model_prompt(&ui);
                    ui.set_status_text("Local model needs repair".into());
                    ui.set_has_error(true);
                    ui.set_active_tab(3);
                });
            }
        });
    }
    ui.set_gemini_api_key_text(initial_settings.gemini_api_key.clone().into());
    ui.set_selected_microphone(selected_microphone.clone().into());
    ui.set_use_default_microphone(initial_settings.use_default_microphone);
    ui.set_start_with_windows(initial_settings.start_with_windows);
    ui.set_default_microphone_text(default_microphone.clone().into());
    ui.set_microphone_options(ModelRc::new(VecModel::from(
        microphones
            .iter()
            .cloned()
            .map(SharedString::from)
            .collect::<Vec<SharedString>>(),
    )));

    let gemini_preset_options: Vec<SharedString> = vec![
        "Minimal corrections".into(),
        "Sound like a pirate".into(),
        "Sound like a medieval knight".into(),
        "Custom".into(),
    ];
    ui.set_gemini_preset_options(ModelRc::new(VecModel::from(gemini_preset_options)));
    ui.set_selected_gemini_preset(initial_settings.gemini_prompt_preset.clone().into());
    ui.set_gemini_custom_prompt(initial_settings.gemini_custom_prompt.clone().into());
    ui.set_gemini_model_text(initial_settings.gemini_model.clone().into());
    ui.set_use_gemini_modifier(initial_settings.gemini_enabled);

    ui.set_overlay_opacity(initial_settings.overlay_opacity);
    ui.set_theme_background_top_color(parse_theme_color(
        &initial_settings.theme_background_top_color,
        Color::from_rgb_u8(2, 20, 11),
    ));
    ui.set_theme_background_bottom_color(parse_theme_color(
        &initial_settings.theme_background_bottom_color,
        Color::from_rgb_u8(0, 8, 6),
    ));
    ui.set_theme_window_color(parse_theme_color(
        &initial_settings.theme_window_color,
        Color::from_rgb_u8(4, 27, 17),
    ));
    ui.set_theme_button_accent_color(parse_theme_color(
        &initial_settings.theme_button_accent_color,
        Color::from_rgb_u8(74, 222, 128),
    ));
    ui.set_theme_title_color(parse_theme_color(
        &initial_settings.theme_title_color,
        Color::from_rgb_u8(228, 255, 233),
    ));
    ui.set_theme_text_color(parse_theme_color(
        &initial_settings.theme_text_color,
        Color::from_rgb_u8(204, 239, 214),
    ));
    ui.set_transcript_history(ModelRc::new(VecModel::from(
        initial_transcript_history
            .iter()
            .map(|entry| SharedString::from(entry.display_text()))
            .collect::<Vec<_>>(),
    )));
    ui.set_log_items(ModelRc::new(VecModel::from(Vec::<SharedString>::new())));

    // When the user closes the main window, hide it but keep the Slint
    // event loop alive so the app can continue running from the tray.
    let ui_weak_for_close = ui.as_weak();
    let settings_for_close = settings.clone();
    ui.window().on_close_requested(move || {
        if let Some(ui) = ui_weak_for_close.upgrade() {
            let modal_active =
                ui.get_local_download_prompt_visible() || ui.get_local_download_progress_visible();
            if modal_active {
                ui.set_status_text(
                    "Finish or dismiss the local model dialog before closing".into(),
                );
                return CloseRequestResponse::KeepWindowShown;
            }
            let dirty = if ui.get_active_tab() == 3 {
                let saved = settings_for_close.lock().unwrap().clone();
                settings_editor_is_dirty(&ui, &saved)
            } else {
                false
            };
            if dirty {
                ui.set_pending_navigation_tab(-1);
                ui.set_unsaved_settings_prompt_visible(true);
            } else {
                let _ = ui.window().hide();
            }
        }
        CloseRequestResponse::KeepWindowShown
    });

    #[cfg(target_os = "windows")]
    let hotkey_capture_window = HotkeyCaptureWindow::new()?;
    #[cfg(target_os = "windows")]
    hotkey_capture_window.set_state_text("Waiting for key combo...".into());
    #[cfg(target_os = "windows")]
    hotkey_capture_window.set_combo_text("".into());
    #[cfg(target_os = "windows")]
    let hotkey_capture_active = Rc::new(RefCell::new(false));
    #[cfg(target_os = "windows")]
    let hotkey_capture_latched = Rc::new(RefCell::new(false));

    let transcript_overlay = TranscriptOverlayWindow::new()?;
    transcript_overlay.set_sentence_text("".into());
    transcript_overlay.set_window_width(520);
    transcript_overlay.set_window_height(120);
    transcript_overlay.set_overlay_opacity(initial_settings.overlay_opacity);
    transcript_overlay.set_overlay_background_color(parse_theme_color(
        &initial_settings.overlay_background_color,
        Color::from_rgb_u8(3, 21, 12),
    ));
    transcript_overlay.set_overlay_text_color(parse_theme_color(
        &initial_settings.overlay_text_color,
        Color::from_rgb_u8(230, 255, 240),
    ));
    transcript_overlay.set_is_error(false);
    transcript_overlay.set_is_system_message(false);
    #[cfg(target_os = "windows")]
    {
        let position = match (
            initial_settings.overlay_position_x,
            initial_settings.overlay_position_y,
        ) {
            (Some(x), Some(y)) => slint::PhysicalPosition::new(x, y),
            _ => default_overlay_position(),
        };
        transcript_overlay.window().set_position(position);
    }
    #[cfg(not(target_os = "windows"))]
    {
        let position = match (
            initial_settings.overlay_position_x,
            initial_settings.overlay_position_y,
        ) {
            (Some(x), Some(y)) => slint::LogicalPosition::new(x as f32, y as f32),
            _ => default_overlay_position(),
        };
        transcript_overlay.window().set_position(position);
    }

    let overlay_weak_for_drag = transcript_overlay.as_weak();
    let settings_for_overlay_drag = settings.clone();
    transcript_overlay.on_move_window(move |dx, dy| {
        if let Some(overlay) = overlay_weak_for_drag.upgrade() {
            let current = overlay.window().position();
            let scale = overlay.window().scale_factor();
            let new_position = slint::PhysicalPosition::new(
                current.x + (dx as f32 * scale) as i32,
                current.y + (dy as f32 * scale) as i32,
            );
            overlay.window().set_position(new_position);

            let snapshot = {
                let mut current_settings = settings_for_overlay_drag.lock().unwrap();
                current_settings.overlay_position_x = Some(new_position.x);
                current_settings.overlay_position_y = Some(new_position.y);
                current_settings.clone()
            };
            save_settings(&snapshot);
        }
    });

    let ui_handle_for_tokio = ui.as_weak();
    let overlay_handle_for_tokio = transcript_overlay.as_weak();
    let settings_for_runtime = settings.clone();
    let cmd_tx_for_runtime = cmd_tx.clone();

    let log_raw_for_clipboard: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    ui.on_copy_transcript({
        let raw = transcript_raw_for_clipboard.clone();
        move |index| {
            if let Ok(hist) = raw.lock() {
                if let Some(text) = hist.get(index as usize) {
                    if let Ok(mut cb) = Clipboard::new() {
                        let _ = cb.set_text(text.clone());
                    }
                }
            }
        }
    });

    ui.on_copy_log_item({
        let raw = log_raw_for_clipboard.clone();
        move |index| {
            if let Ok(hist) = raw.lock() {
                if let Some(text) = hist.get(index as usize) {
                    if let Ok(mut cb) = Clipboard::new() {
                        let _ = cb.set_text(text.clone());
                    }
                }
            }
        }
    });

    let transcript_history_for_runtime = transcript_history_store.clone();
    let transcript_history_revision_for_runtime = transcript_history_revision.clone();
    let transcript_raw_for_runtime = transcript_raw_for_clipboard.clone();
    thread::spawn(move || {
        let rt = Runtime::new().unwrap();
        rt.block_on(async move {
            echo_info!("runtime", "Tokio runtime active");

            let mut active_session: Option<Session> = None;
            let mut local_start_armed = false;
            let (finalize_tx, mut finalize_rx) = mpsc::unbounded_channel::<(u64, bool)>();
            let overlay_visible = Arc::new(AtomicBool::new(false));
            let session_epoch = Arc::new(AtomicU64::new(0));

            loop {
                tokio::select! {
                    Some(level) = level_rx.recv() => {
                        let _ = ui_handle_for_tokio.upgrade_in_event_loop(move |ui| {
                            ui.set_audio_level(level);
                        });
                    }
                    Some((finalized_epoch, preserve_status)) = finalize_rx.recv() => {
                        echo_info!(
                            "session",
                            "Finalization received epoch={} current_epoch={} preserve_status={}",
                            finalized_epoch,
                            session_epoch.load(Ordering::SeqCst),
                            preserve_status
                        );
                        if finalized_epoch != session_epoch.load(Ordering::SeqCst) {
                            echo_warn!(
                                "session",
                                "Ignoring stale finalization epoch={}",
                                finalized_epoch
                            );
                            continue;
                        }
                        if let Some(mut session) = active_session.take() {
                            session.cancel_finalization_watchdog();
                            if let Ok(mut state) = session.state.lock() {
                                state.transition_to_idle();
                            }
                            echo_info!(
                                "session",
                                "Finalization complete epoch={} session closed",
                                finalized_epoch
                            );
                        }
                        overlay_visible.store(false, Ordering::SeqCst);
                        let _ = ui_handle_for_tokio.upgrade_in_event_loop(move |ui| {
                            ui.set_audio_level(0.0);
                            ui.set_is_recording(false);
                            ui.set_is_finalizing(false);
                            if !preserve_status {
                                ui.set_has_error(false);
                                ui.set_status_text("Idle".into());
                            }
                        });
                        let _ = overlay_handle_for_tokio.upgrade_in_event_loop(|overlay| {
                            overlay.set_sentence_text("".into());
                            overlay.set_window_width(520);
                            overlay.set_window_height(120);
                            overlay.set_is_error(false);
                            overlay.set_is_system_message(false);
                            overlay.set_is_visible(false);
                            let _ = overlay.hide();
                        });
                    }
                    Some(cmd) = cmd_rx.recv() => {
                    echo_info!(
                        "command",
                        "Received command={} active_epoch={} local_start_armed={}",
                        app_command_name(&cmd),
                        active_session.as_ref().map_or(0, |session| session.epoch),
                        local_start_armed
                    );
                    let cmd = match cmd {
                        AppCommand::LocalEngineLoaded(result) => {
                            if !local_start_armed {
                                continue;
                            }
                            match result {
                                Ok(()) => {
                                    echo_info!("local_model", "Background model load completed");
                                    local_start_armed = false;
                                    AppCommand::StartRecording
                                }
                                Err(err) => {
                                    echo_error!(
                                        "local_model",
                                        "Background model load failed: {err}"
                                    );
                                    local_start_armed = false;
                                    let message = format!("Local model error:\n{err}");
                                    let prompt_error = format!("The local model could not load: {err}");
                                    let _ = ui_handle_for_tokio.upgrade_in_event_loop(move |ui| {
                                        ui.set_status_text("Local model failed to load".into());
                                        ui.set_has_error(true);
                                        ui.set_local_download_error(prompt_error.into());
                                        request_local_model_prompt(&ui);
                                        ui.set_active_tab(3);
                                        let _ = ui.show();
                                    });
                                    let _ = overlay_handle_for_tokio.upgrade_in_event_loop(move |overlay| {
                                        overlay.set_sentence_text(message.into());
                                        overlay.set_is_error(true);
                                        overlay.set_is_system_message(true);
                                        overlay.set_is_visible(true);
                                        let _ = overlay.show();
                                    });
                                    continue;
                                }
                            }
                        }
                        AppCommand::ToggleRecording => {
                            match active_session.as_ref() {
                                Some(session) => {
                                    let state = session.state.lock().unwrap();
                                    if state.can_stop() {
                                        AppCommand::StopRecording
                                    } else {
                                        echo_info!(
                                            "session",
                                            "Toggle ignored because recording is already stopping"
                                        );
                                        continue;
                                    }
                                }
                                None if local_start_armed => {
                                    local_start_armed = false;
                                    overlay_visible.store(false, Ordering::SeqCst);
                                    let _ = overlay_handle_for_tokio.upgrade_in_event_loop(|overlay| {
                                        overlay.set_is_visible(false);
                                        let _ = overlay.hide();
                                    });
                                    let _ = ui_handle_for_tokio.upgrade_in_event_loop(|ui| {
                                        ui.set_status_text("Local model loading in background".into());
                                    });
                                    continue;
                                }
                                None => AppCommand::StartRecording,
                            }
                        }
                        cmd => cmd,
                    };

                    match cmd {
                        AppCommand::StartRecording => {
                            if active_session.is_some() {
                                echo_warn!(
                                    "session",
                                    "Start rejected because a session is already active"
                                );
                                let _ = ui_handle_for_tokio.upgrade_in_event_loop(|ui| {
                                    ui.set_status_text("Finishing the previous transcription...".into());
                                    ui.set_is_recording(false);
                                    ui.set_is_finalizing(true);
                                });
                                continue;
                            }

                            let current_settings = settings_for_runtime.lock().unwrap().clone();
                            let provider = transcription::TranscriptionProvider::from_id(
                                &current_settings.transcription_provider,
                            );
                            if !matches!(provider, transcription::TranscriptionProvider::LocalSherpaOnnx)
                                && current_settings.transcription_config().api_key.trim().is_empty() {
                                echo_error!(
                                    "session",
                                    "Start rejected provider={} reason=missing_api_key",
                                    provider.id()
                                );
                                let _ = ui_handle_for_tokio.upgrade_in_event_loop(|ui| {
                                    ui.set_status_text("Missing API key".into());
                                    ui.set_is_recording(false);
                                });
                                continue;
                            }

                            if matches!(provider, transcription::TranscriptionProvider::LocalSherpaOnnx) {
                                if !transcription::local_models_available() {
                                    echo_warn!(
                                        "local_model",
                                        "Start rejected because required local model files are unavailable"
                                    );
                                    let _ = ui_handle_for_tokio.upgrade_in_event_loop(|ui| {
                                        ui.set_local_download_error("".into());
                                        request_local_model_prompt(&ui);
                                        ui.set_status_text("Local speech files are required".into());
                                        ui.set_has_error(false);
                                        ui.set_active_tab(3);
                                        let _ = ui.show();
                                    });
                                    continue;
                                }
                                match transcription::local_engine_status() {
                                    transcription::LocalEngineStatus::Ready => {}
                                    _ => {
                                        echo_info!(
                                            "local_model",
                                            "Local model preload requested before recording"
                                        );
                                        transcription::preload_local_engine(&current_settings.local_sherpa);
                                        local_start_armed = true;
                                        overlay_visible.store(true, Ordering::SeqCst);
                                        let _ = ui_handle_for_tokio.upgrade_in_event_loop(|ui| {
                                            ui.set_status_text("Loading local speech model...".into());
                                            ui.set_has_error(false);
                                        });
                                        let _ = overlay_handle_for_tokio.upgrade_in_event_loop(|overlay| {
                                            overlay.set_sentence_text("Local speech model is still loading...".into());
                                            overlay.set_window_width(520);
                                            overlay.set_window_height(120);
                                            overlay.set_is_error(true);
                                            overlay.set_is_system_message(true);
                                            overlay.set_is_visible(true);
                                            let _ = overlay.show();
                                        });
                                        let ready_tx = cmd_tx_for_runtime.clone();
                                        tokio::task::spawn_blocking(move || {
                                            let result = transcription::wait_for_local_engine();
                                            let _ = ready_tx.send(AppCommand::LocalEngineLoaded(result));
                                        });
                                        continue;
                                    }
                                }
                            }

                            let current_session_epoch =
                                session_epoch.fetch_add(1, Ordering::SeqCst) + 1;

                            let preferred_device = if current_settings.use_default_microphone {
                                None
                            } else {
                                Some(current_settings.selected_microphone.clone())
                            };

                            echo_info!(
                                "session",
                                "Starting epoch={} provider={} use_default_microphone={} gemini_enabled={}",
                                current_session_epoch,
                                provider.id(),
                                current_settings.use_default_microphone,
                                current_settings.gemini_enabled
                            );
                            let _ = ui_handle_for_tokio.upgrade_in_event_loop(|ui| {
                                ui.set_status_text("Connecting...".into());
                                ui.set_has_error(false);
                                ui.set_is_finalizing(false);
                                ui.set_transcript("".into());
                            });
                            overlay_visible.store(true, Ordering::SeqCst);
                            let _ = overlay_handle_for_tokio.upgrade_in_event_loop(|overlay| {
                                reset_overlay_to_listening(&overlay);
                            });

                            let state = Arc::new(Mutex::new(RecordingState::BufferingPreConnect));
                            let transcript_pipeline = Arc::new(Mutex::new(TranscriptPipeline::new()));
                            let log_display: Arc<Mutex<Vec<SharedString>>> = Arc::new(Mutex::new(Vec::new()));
                            let log_raw: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
                            let (audio_tx, audio_rx) = mpsc::channel::<Vec<i16>>(50);
                            let (network_stop_tx, network_stop_rx) =
                                mpsc::unbounded_channel::<transcription::TranscriptionCommand>();
                            let (text_tx, mut text_rx) =
                                mpsc::channel::<transcription::TranscriptionEvent>(100);
                            let (log_line_tx, mut log_line_rx) =
                                mpsc::unbounded_channel::<String>();
                            let audio_level_tx = level_tx.clone();

                            let stream_result =
                                audio::start_audio_capture(audio_tx, audio_level_tx, preferred_device);

                            match stream_result {
                                Ok(stream) => {
                                    echo_info!(
                                        "audio",
                                        "Capture stream started epoch={}",
                                        current_session_epoch
                                    );
                                    let client = transcription::TranscriberClient::from_config(
                                        current_settings.transcription_config(),
                                    );
                                    let client_state = state.clone();
                                    let injection_state = state.clone();
                                    let transcript_pipeline_for_text = transcript_pipeline.clone();
                                    let transcript_history_for_text = transcript_history_for_runtime.clone();
                                    let transcript_history_revision_for_text =
                                        transcript_history_revision_for_runtime.clone();
                                    let transcript_raw_for_cb = transcript_raw_for_runtime.clone();
                                    let log_display_for_text = log_display.clone();
                                    let log_raw_for_text = log_raw.clone();
                                    let log_raw_for_cb = log_raw_for_clipboard.clone();
                                    let log_line_tx_for_text = log_line_tx.clone();
                                    let settings_for_text = settings_for_runtime.clone();
                                    let finalize_tx_for_transcript = finalize_tx.clone();
                                    let ui_handle_for_network = ui_handle_for_tokio.clone();
                                    let ui_handle_for_transcript = ui_handle_for_tokio.clone();
                                    let ui_handle_for_log = ui_handle_for_tokio.clone();
                                    let overlay_handle_for_transcript = overlay_handle_for_tokio.clone();
                                    let overlay_visible_for_transcript = overlay_visible.clone();
                                    let session_epoch_for_transcript = session_epoch.clone();
                                    let preserve_session_status = Arc::new(AtomicBool::new(false));
                                    let preserve_status_for_network = preserve_session_status.clone();
                                    let preserve_status_for_transcript = preserve_session_status.clone();

                                    let _ = ui_handle_for_tokio.upgrade_in_event_loop(|ui| {
                                        ui.set_is_recording(true);
                                        ui.set_status_text("Listening...".into());
                                    });

                                    let (audio_to_net_tx, audio_to_net_rx) = mpsc::channel::<Vec<i16>>(50);
                                    let (audio_forward_stop_tx, audio_forward_stop_rx) =
                                        mpsc::unbounded_channel::<()>();
                                    let audio_forward_task = tokio::spawn(async move {
                                        echo_info!(
                                            "audio",
                                            "Forwarding task started epoch={}",
                                            current_session_epoch
                                        );
                                        let stats = forward_audio_until_stopped(
                                            audio_rx,
                                            audio_to_net_tx,
                                            audio_forward_stop_rx,
                                        )
                                        .await;
                                        echo_info!(
                                            "audio",
                                            "Forwarding task ended epoch={} reason={} chunks={} samples={}",
                                            current_session_epoch,
                                            stats.reason,
                                            stats.chunks,
                                            stats.samples
                                        );
                                    });

                                    let network_task = tokio::spawn(async move {
                                        echo_info!(
                                            "provider",
                                            "Transcription client task started epoch={}",
                                            current_session_epoch
                                        );
                                        {
                                            let mut s = client_state.lock().unwrap();
                                            s.transition_to_connecting();
                                        }

                                        let result = client.run(audio_to_net_rx, network_stop_rx, text_tx, log_line_tx).await;
                                        if let Err(err) = result {
                                            echo_error!(
                                                "provider",
                                                "Transcription client failed epoch={}: {}",
                                                current_session_epoch,
                                                err
                                            );
                                            if let Ok(mut s) = client_state.lock() {
                                                *s = RecordingState::Error;
                                            }
                                            preserve_status_for_network.store(true, Ordering::SeqCst);
                                            let _ = ui_handle_for_network.upgrade_in_event_loop(|ui| {
                                                ui.set_status_text("Transcription error".into());
                                                ui.set_is_recording(false);
                                                ui.set_has_error(true);
                                            });
                                            return;
                                        }

                                        echo_info!(
                                            "provider",
                                            "Transcription client task ended epoch={}",
                                            current_session_epoch
                                        );
                                    });

                                    let log_task = tokio::spawn(async move {
                                        let mut refresh =
                                            tokio::time::interval(std::time::Duration::from_millis(100));
                                        refresh.set_missed_tick_behavior(
                                            tokio::time::MissedTickBehavior::Delay,
                                        );
                                        let mut dirty = false;

                                        loop {
                                            tokio::select! {
                                                maybe_line = log_line_rx.recv() => {
                                                    let Some(line) = maybe_line else {
                                                        break;
                                                    };
                                                    diagnostics::record(
                                                        "INFO",
                                                        "provider_event",
                                                        format_args!(
                                                            "epoch={} {}",
                                                            current_session_epoch,
                                                            line
                                                        ),
                                                    );
                                                    let ts = Local::now().format("%H:%M:%S");
                                                    let display_line: SharedString =
                                                        format!("[{}] {}", ts, line).into();
                                                    let raw_line = line;
                                                    {
                                                        let mut disp = log_display_for_text.lock().unwrap();
                                                        let mut raw = log_raw_for_text.lock().unwrap();
                                                        disp.push(display_line);
                                                        raw.push(raw_line);
                                                        if disp.len() > 300 {
                                                            let excess = disp.len() - 300;
                                                            disp.drain(0..excess);
                                                            raw.drain(0..excess);
                                                        }
                                                        *log_raw_for_cb.lock().unwrap() = raw.clone();
                                                    }
                                                    dirty = true;
                                                }
                                                _ = refresh.tick(), if dirty => {
                                                    let items = log_display_for_text.lock().unwrap().clone();
                                                    let _ = ui_handle_for_log.upgrade_in_event_loop(move |ui| {
                                                        ui.set_log_items(ModelRc::new(VecModel::from(items)));
                                                    });
                                                    dirty = false;
                                                }
                                            }
                                        }

                                        if dirty {
                                            let items = log_display_for_text.lock().unwrap().clone();
                                            let _ = ui_handle_for_log.upgrade_in_event_loop(move |ui| {
                                                ui.set_log_items(ModelRc::new(VecModel::from(items)));
                                            });
                                        }
                                    });

                                    let transcript_task = tokio::spawn(async move {
                                        let mut latest_partial = String::new();
                                        while let Some(msg) = text_rx.recv().await {
                                            let (event_kind, event_characters) = match &msg {
                                                transcription::TranscriptionEvent::Partial(text) => {
                                                    ("partial", text.chars().count())
                                                }
                                                transcription::TranscriptionEvent::Committed(text) => {
                                                    ("committed", text.chars().count())
                                                }
                                                transcription::TranscriptionEvent::Error(error) => {
                                                    ("error", error.chars().count())
                                                }
                                            };
                                            echo_info!(
                                                "transcript",
                                                "Event received epoch={} kind={} characters={}",
                                                current_session_epoch,
                                                event_kind,
                                                event_characters
                                            );
                                            if session_epoch_for_transcript.load(Ordering::SeqCst)
                                                != current_session_epoch
                                            {
                                                echo_warn!(
                                                    "transcript",
                                                    "Ignoring stale transcript event epoch={}",
                                                    current_session_epoch
                                                );
                                                continue;
                                            }
                                            {
                                                let mut s = injection_state.lock().unwrap();
                                                s.transition_to_recording();
                                            }

                                            let mut was_committed = false;
                                            let mut is_error = false;
                                            let mut stop_requested_for_msg = false;
                                            let display_text = match msg {
                                                transcription::TranscriptionEvent::Partial(text) => {
                                                    latest_partial = text;
                                                    let committed = {
                                                        let pipeline = transcript_pipeline_for_text.lock().unwrap();
                                                        pipeline.committed_text().trim().to_string()
                                                    };
                                                    live_transcript_text(&committed, &latest_partial)
                                                }
                                                transcription::TranscriptionEvent::Committed(text) => {
                                                    // Decide what text to actually commit:
                                                    // - If ElevenLabs sends an empty committed transcript, only
                                                    //   commit the current partial if we have one. Falling back to
                                                    //   the existing committed transcript would duplicate content.
                                                    let empty_commit = text.trim().is_empty();
                                                    let base_text = if empty_commit {
                                                        if !latest_partial.trim().is_empty() {
                                                            latest_partial.trim().to_string()
                                                        } else {
                                                            String::new()
                                                        }
                                                    } else {
                                                        text.clone()
                                                    };
                                                    // Clear partial now that we've used it for empty-commit fallback.
                                                    latest_partial.clear();

                                                    // Snapshot Gemini settings while holding the lock briefly.
                                                    let (gemini_on, gkey, gmodel, gpreset, gcustom) = {
                                                        let s = settings_for_text.lock().unwrap();
                                                        (
                                                            s.gemini_enabled,
                                                            s.gemini_api_key.clone(),
                                                            s.gemini_model.clone(),
                                                            s.gemini_prompt_preset.clone(),
                                                            s.gemini_custom_prompt.clone(),
                                                        )
                                                    };
                                                    // Lock is dropped here before any await.

                                                    let final_text = if gemini_on {
                                                        echo_info!(
                                                            "gemini",
                                                            "Rewrite started epoch={} input_characters={}",
                                                            current_session_epoch,
                                                            base_text.chars().count()
                                                        );
                                                        let pending_text = "Gemini has the text and is modifying it.";
                                                        let (w, h) = overlay_size_for_text(pending_text);
                                                        let overlay_visible_setter =
                                                            overlay_visible_for_transcript.clone();
                                                        let session_epoch_for_overlay =
                                                            session_epoch_for_transcript.clone();
                                                        let _ = overlay_handle_for_transcript
                                                            .upgrade_in_event_loop(move |overlay| {
                                                                if session_epoch_for_overlay.load(Ordering::SeqCst)
                                                                    != current_session_epoch
                                                                {
                                                                    return;
                                                                }
                                                                overlay.set_is_error(false);
                                                                overlay.set_is_system_message(true);
                                                                overlay.set_sentence_text(pending_text.into());
                                                                overlay.set_window_width(w);
                                                                overlay.set_window_height(h);
                                                                overlay.set_is_visible(true);
                                                                overlay_visible_setter.store(true, Ordering::SeqCst);
                                                                let _ = overlay.show();
                                                            });
                                                        gemini::rewrite_text(&gkey, &gmodel, &gpreset, &gcustom, &base_text).await
                                                    } else {
                                                        base_text
                                                    };

                                                    if session_epoch_for_transcript.load(Ordering::SeqCst)
                                                        != current_session_epoch
                                                    {
                                                        echo_warn!(
                                                            "gemini",
                                                            "Discarding stale completed rewrite epoch={}",
                                                            current_session_epoch
                                                        );
                                                        continue;
                                                    }

                                                    let final_text = final_text.trim().trim_start_matches('-').trim().to_string();
                                                    stop_requested_for_msg = {
                                                        let pipeline = transcript_pipeline_for_text.lock().unwrap();
                                                        pipeline.stop_requested()
                                                    };
                                                    echo_info!(
                                                        "transcript",
                                                        "Commit processed epoch={} characters={} stop_requested={}",
                                                        current_session_epoch,
                                                        final_text.chars().count(),
                                                        stop_requested_for_msg
                                                    );

                                                    let aggregated = {
                                                        let mut pipeline = transcript_pipeline_for_text.lock().unwrap();
                                                        if final_text.is_empty() {
                                                            pipeline.committed_text().to_string()
                                                        } else {
                                                            pipeline.push_fragment(&final_text)
                                                        }
                                                    };
                                                    if !final_text.is_empty() {
                                                        let entry = TranscriptHistoryEntry {
                                                            timestamp: Local::now()
                                                                .format("%Y-%m-%d %H:%M:%S")
                                                                .to_string(),
                                                            text: final_text.clone(),
                                                        };
                                                        let (history_snapshot, revision) = {
                                                            let mut history = transcript_history_for_text.lock().unwrap();
                                                            history.insert(0, entry);
                                                            history.truncate(MAX_TRANSCRIPT_HISTORY);
                                                            if !save_transcript_history(&history) {
                                                                echo_error!(
                                                                    "history",
                                                                    "Failed to persist transcript history epoch={}",
                                                                    current_session_epoch
                                                                );
                                                            }
                                                            *transcript_raw_for_cb.lock().unwrap() = history
                                                                .iter()
                                                                .map(|entry| entry.text.clone())
                                                                .collect();
                                                            let revision = transcript_history_revision_for_text
                                                                .fetch_add(1, Ordering::SeqCst)
                                                                + 1;
                                                            (history.clone(), revision)
                                                        };
                                                        let items = history_snapshot
                                                            .iter()
                                                            .map(|entry| SharedString::from(entry.display_text()))
                                                            .collect::<Vec<_>>();
                                                        let revision_for_ui =
                                                            transcript_history_revision_for_text.clone();
                                                        let _ = ui_handle_for_transcript.upgrade_in_event_loop(move |ui| {
                                                            if revision_for_ui.load(Ordering::SeqCst) == revision {
                                                                ui.set_transcript_history(ModelRc::new(VecModel::from(items)));
                                                            }
                                                        });
                                                        let _ = log_line_tx_for_text.send(format!(
                                                            "Transcript committed ({} characters)",
                                                            final_text.chars().count()
                                                        ));
                                                    }
                                                    if stop_requested_for_msg {
                                                        let final_payload = aggregated.trim().to_string();
                                                        if !final_payload.is_empty() {
                                                            echo_info!(
                                                                "injection",
                                                                "Posting requested epoch={} characters={}",
                                                                current_session_epoch,
                                                                final_payload.chars().count()
                                                            );
                                                            let to_inject = format!("{} ", final_payload);
                                                            match injector::inject_text(&to_inject) {
                                                                Ok(()) => {
                                                                    echo_info!(
                                                                        "injection",
                                                                        "Posting completed epoch={}",
                                                                        current_session_epoch
                                                                    );
                                                                    preserve_status_for_transcript
                                                                        .store(true, Ordering::SeqCst);
                                                                    let _ = log_line_tx_for_text.send(
                                                                        "Windows accepted the direct transcript input"
                                                                            .into(),
                                                                    );
                                                                    let _ = ui_handle_for_transcript
                                                                        .upgrade_in_event_loop(|ui| {
                                                                            ui.set_status_text(
                                                                                "Transcript sent to focused window"
                                                                                    .into(),
                                                                            );
                                                                            ui.set_has_error(false);
                                                                        });
                                                                }
                                                                Err(e) => {
                                                                    echo_error!(
                                                                        "injection",
                                                                        "Posting failed epoch={}: {}",
                                                                        current_session_epoch,
                                                                        e
                                                                    );
                                                                    preserve_status_for_transcript
                                                                        .store(true, Ordering::SeqCst);
                                                                    let _ = log_line_tx_for_text.send(format!(
                                                                        "Direct transcript input failed: {e}"
                                                                    ));
                                                                    let _ = ui_handle_for_transcript.upgrade_in_event_loop(|ui| {
                                                                        ui.set_status_text("Injection error - check focused window and permissions".into());
                                                                        ui.set_has_error(true);
                                                                        ui.set_is_recording(false);
                                                                    });
                                                                }
                                                            }
                                                        }
                                                    }
                                                    was_committed = true;
                                                    aggregated
                                                }
                                                transcription::TranscriptionEvent::Error(err_json) => {
                                                    echo_error!(
                                                        "transcript",
                                                        "Speech service error epoch={} response_characters={}",
                                                        current_session_epoch,
                                                        err_json.chars().count()
                                                    );
                                                    latest_partial.clear();
                                                    is_error = true;
                                                    preserve_status_for_transcript.store(true, Ordering::SeqCst);
                                                    let friendly = format!("Error from speech service:\n{}", err_json);
                                                    let _ = ui_handle_for_transcript.upgrade_in_event_loop(|ui| {
                                                        ui.set_status_text("Speech service error".into());
                                                        ui.set_is_recording(false);
                                                        ui.set_has_error(true);
                                                    });
                                                    friendly
                                                }
                                            };

                                            let aggregated_for_overlay = display_text.clone();
                                            let hide_overlay = (was_committed && stop_requested_for_msg) || is_error;
                                            let text_for_ui = if was_committed || is_error {
                                                display_text.clone()
                                            } else {
                                                let pipeline = transcript_pipeline_for_text.lock().unwrap();
                                                pipeline.committed_text().to_string()
                                            };
                                            if session_epoch_for_transcript.load(Ordering::SeqCst)
                                                != current_session_epoch
                                            {
                                                echo_warn!(
                                                    "transcript",
                                                    "Ignoring stale transcript UI update epoch={}",
                                                    current_session_epoch
                                                );
                                                continue;
                                            }
                                            let session_epoch_for_ui = session_epoch_for_transcript.clone();
                                            let _ = ui_handle_for_transcript.upgrade_in_event_loop(move |ui| {
                                                if session_epoch_for_ui.load(Ordering::SeqCst)
                                                    == current_session_epoch
                                                {
                                                    ui.set_transcript(text_for_ui.into());
                                                }
                                            });
                                            let overlay_visible_setter = overlay_visible_for_transcript.clone();
                                            let session_epoch_for_overlay = session_epoch_for_transcript.clone();
                                            let _ = overlay_handle_for_transcript
                                                .upgrade_in_event_loop(move |overlay| {
                                                    if session_epoch_for_overlay.load(Ordering::SeqCst)
                                                        != current_session_epoch
                                                    {
                                                        return;
                                                    }
                                                    overlay.set_is_error(is_error);
                                                    if hide_overlay {
                                                        overlay.set_sentence_text("".into());
                                                        overlay.set_window_width(520);
                                                        overlay.set_window_height(120);
                                                        overlay.set_is_error(false);
                                                        overlay.set_is_system_message(false);
                                                        overlay.set_is_visible(false);
                                                        overlay_visible_setter.store(false, Ordering::SeqCst);
                                                        let _ = overlay.hide();
                                                    } else {
                                                        let (w, h) =
                                                            overlay_size_for_text(
                                                                &aggregated_for_overlay,
                                                            );
                                                        overlay.set_is_system_message(false);
                                                        overlay.set_sentence_text(
                                                            aggregated_for_overlay.into(),
                                                        );
                                                        overlay.set_window_width(w);
                                                        overlay.set_window_height(h);
                                                        overlay.set_is_visible(true);
                                                        overlay_visible_setter.store(true, Ordering::SeqCst);
                                                        let _ = overlay.show();
                                                    }
                                                });

                                        }
                                        echo_info!(
                                            "transcript",
                                            "Event channel closed epoch={} preserve_status={}",
                                            current_session_epoch,
                                            preserve_session_status.load(Ordering::SeqCst)
                                        );
                                        let _ = finalize_tx_for_transcript.send((
                                            current_session_epoch,
                                            preserve_session_status.load(Ordering::SeqCst),
                                        ));
                                    });

                                    active_session = Some(Session {
                                        epoch: current_session_epoch,
                                        state,
                                        audio_stream: Some(stream),
                                        audio_forward_stop_tx: Some(audio_forward_stop_tx),
                                        network_stop_tx: Some(network_stop_tx),
                                        transcript_pipeline,
                                        task_abort_handles: vec![
                                            audio_forward_task.abort_handle(),
                                            network_task.abort_handle(),
                                            log_task.abort_handle(),
                                            transcript_task.abort_handle(),
                                        ],
                                        finalization_watchdog: None,
                                    });
                                    if let Some(session) = active_session.as_ref() {
                                        if let Some(tx) = session.network_stop_tx.as_ref() {
                                            let _ =
                                                tx.send(transcription::TranscriptionCommand::Start);
                                        }
                                    }
                                    }
                                    Err(e) => {
                                    echo_error!(
                                        "audio",
                                        "Failed to start capture epoch={}: {}",
                                        current_session_epoch,
                                        e
                                    );
                                    let _ = ui_handle_for_tokio.upgrade_in_event_loop(move |ui| {
                                        ui.set_is_recording(false);
                                        ui.set_status_text(format!("Audio error: {}", e).into());
                                        ui.invoke_request_navigation(2);
                                    });
                                    let _ = overlay_handle_for_tokio.upgrade_in_event_loop(|overlay| {
                                        overlay.set_sentence_text("".into());
                                        overlay.set_window_width(520);
                                        overlay.set_window_height(120);
                                        overlay.set_is_system_message(false);
                                        overlay.set_is_visible(false);
                                        let _ = overlay.hide();
                                    });
                                    }
                                    }
                                    }
                                    AppCommand::StopRecording => {
                                    if let Some(session) = active_session.as_mut() {
                                    let should_stop = {
                                        let mut state = session.state.lock().unwrap();
                                        if state.can_stop() {
                                            state.transition_to_finalizing();
                                            true
                                        } else {
                                            false
                                        }
                                    };
                                    if !should_stop {
                                        echo_info!(
                                            "session",
                                            "Stop ignored epoch={} reason=already_stopping",
                                            session.epoch
                                        );
                                        continue;
                                    }

                                    echo_info!(
                                        "session",
                                        "Stop requested epoch={} finalization_timeout_seconds={}",
                                        session.epoch,
                                        FINALIZATION_TIMEOUT.as_secs()
                                    );
                                    let _ = ui_handle_for_tokio.upgrade_in_event_loop(|ui| {
                                        ui.set_status_text("Finalizing...".into());
                                        ui.set_is_recording(false);
                                        ui.set_is_finalizing(true);
                                    });

                                    // Close capture, then explicitly close and drain the
                                    // forwarding receiver before asking the provider to commit.
                                    session.audio_stream.take();
                                    if let Ok(mut pipeline) = session.transcript_pipeline.lock() {
                                    pipeline.request_stop();
                                    }
                                    session.request_stop();
                                    let timed_out_epoch = session.epoch;
                                    let timeout_tx = cmd_tx_for_runtime.clone();
                                    let watchdog = tokio::spawn(async move {
                                        tokio::time::sleep(FINALIZATION_TIMEOUT).await;
                                        let _ = timeout_tx.send(AppCommand::FinalizationTimedOut(timed_out_epoch));
                                    });
                                    session.finalization_watchdog = Some(watchdog.abort_handle());
                                    } else {
                                        echo_warn!(
                                            "session",
                                            "Stop ignored reason=no_active_session"
                                        );
                                    }
                                    }
                                    AppCommand::ToggleRecording => unreachable!(),
                                    AppCommand::LocalEngineLoaded(_) => unreachable!(),
                                    AppCommand::FinalizationTimedOut(timed_out_epoch) => {
                                    let timed_out = active_session
                                        .as_ref()
                                        .is_some_and(|session| session.epoch == timed_out_epoch);
                                    if !timed_out {
                                        continue;
                                    }

                                    echo_error!(
                                        "session",
                                        "Finalization timed out epoch={}; resetting it",
                                        timed_out_epoch
                                    );
                                    session_epoch.fetch_add(1, Ordering::SeqCst);
                                    if let Some(mut session) = active_session.take() {
                                        session.cancel_finalization_watchdog();
                                        session.abort_tasks();
                                    }
                                    overlay_visible.store(false, Ordering::SeqCst);
                                    let _ = ui_handle_for_tokio.upgrade_in_event_loop(|ui| {
                                        ui.set_audio_level(0.0);
                                        ui.set_is_recording(false);
                                        ui.set_is_finalizing(false);
                                        ui.set_has_error(true);
                                        ui.set_status_text("Finalization timed out; ready to try again".into());
                                    });
                                    let _ = overlay_handle_for_tokio.upgrade_in_event_loop(|overlay| {
                                        overlay.set_sentence_text("".into());
                                        overlay.set_is_visible(false);
                                        let _ = overlay.hide();
                                    });
                                    }
                                    }
                                    }
                                    }
                                    }
                                    });
    });
    let start_tx = cmd_tx.clone();
    ui.on_start_recording(move || {
        echo_info!("ui", "Start recording invoked from main window");
        let _ = start_tx.send(AppCommand::StartRecording);
    });

    let stop_tx = cmd_tx.clone();
    ui.on_stop_recording(move || {
        echo_info!("ui", "Stop recording invoked from main window");
        let _ = stop_tx.send(AppCommand::StopRecording);
    });

    let settings_for_ui = settings.clone();
    let ui_weak_for_settings = ui.as_weak();
    let pending_navigation_after_save = Rc::new(Cell::new(None::<i32>));
    let local_download_offer_suppressed = Rc::new(Cell::new(false));

    let ui_weak_for_navigation = ui.as_weak();
    let settings_for_navigation = settings.clone();
    ui.on_request_navigation(move |target| {
        if let Some(ui) = ui_weak_for_navigation.upgrade() {
            if ui.get_local_download_prompt_visible()
                || ui.get_local_download_progress_visible()
                || ui.get_unsaved_settings_prompt_visible()
            {
                return;
            }
            let dirty = if ui.get_active_tab() == 3 && target != 3 {
                let saved = settings_for_navigation.lock().unwrap().clone();
                settings_editor_is_dirty(&ui, &saved)
            } else {
                false
            };
            if dirty {
                ui.set_pending_navigation_tab(target);
                ui.set_unsaved_settings_prompt_visible(true);
            } else {
                ui.set_active_tab(target);
            }
        }
    });

    let ui_weak_for_apply = ui.as_weak();
    let pending_navigation_for_apply = pending_navigation_after_save.clone();
    #[cfg(target_os = "windows")]
    let hotkey_manager_for_apply = hotkey_manager.clone();
    #[cfg(target_os = "windows")]
    let hotkey_state_for_apply = hotkey_state.clone();
    #[cfg(target_os = "windows")]
    let hotkey_id_for_apply = hotkey_id_state.clone();
    #[cfg(target_os = "windows")]
    let hotkey_text_for_apply = hotkey_text.clone();
    ui.on_apply_settings(move || {
        let (
            elevenlabs_api_key,
            elevenlabs_model,
            elevenlabs_language_code,
            elevenlabs_no_verbatim,
            openai_api_key,
            openai_model,
            openai_language_code,
            transcription_provider,
            gemini_api_key,
            gemini_enabled,
            gemini_model,
            gemini_preset,
            gemini_custom,
            selected_mic,
            use_default_mic,
            start_with_windows,
            local_cpu_threads,
            local_vad_threshold,
            local_silence_ms,
            local_pre_roll_ms,
            local_post_roll_ms,
            local_min_speech_ms,
            local_max_segment_seconds,
            local_partial_interval_ms,
            local_redecode_full_session,
        ) = if let Some(ui) = ui_weak_for_apply.upgrade() {
            (
                ui.get_api_key_text().to_string(),
                ui.get_elevenlabs_model_text().to_string(),
                ui.get_elevenlabs_language_code_text().to_string(),
                ui.get_elevenlabs_no_verbatim(),
                ui.get_openai_api_key_text().to_string(),
                ui.get_openai_model_text().to_string(),
                ui.get_openai_language_code_text().to_string(),
                ui.get_transcription_provider_text().to_string(),
                ui.get_gemini_api_key_text().to_string(),
                ui.get_use_gemini_modifier(),
                ui.get_gemini_model_text().to_string(),
                ui.get_selected_gemini_preset().to_string(),
                ui.get_gemini_custom_prompt().to_string(),
                ui.get_selected_microphone().to_string(),
                ui.get_use_default_microphone(),
                ui.get_start_with_windows(),
                ui.get_local_cpu_threads().round() as i32,
                ui.get_local_vad_threshold(),
                ui.get_local_silence_ms().round() as u32,
                ui.get_local_pre_roll_ms().round() as u32,
                ui.get_local_post_roll_ms().round() as u32,
                ui.get_local_min_speech_ms().round() as u32,
                ui.get_local_max_segment_seconds().round() as u32,
                ui.get_local_partial_interval_ms().round() as u32,
                ui.get_local_redecode_full_session(),
            )
        } else {
            (
                String::new(),
                transcription::DEFAULT_ELEVENLABS_REALTIME_MODEL_ID.to_string(),
                transcription::DEFAULT_LANGUAGE_CODE.to_string(),
                true,
                String::new(),
                transcription::DEFAULT_OPENAI_REALTIME_WHISPER_MODEL_ID.to_string(),
                transcription::DEFAULT_LANGUAGE_CODE.to_string(),
                transcription::DEFAULT_PROVIDER_LABEL.to_string(),
                String::new(),
                false,
                "gemini-3.1-flash-lite-preview".to_string(),
                "Minimal corrections".to_string(),
                String::new(),
                String::new(),
                true,
                false,
                transcription::physical_core_count().clamp(1, 4),
                0.5,
                600,
                250,
                150,
                200,
                30,
                1000,
                false,
            )
        };

        #[cfg(target_os = "windows")]
        let startup_result = startup::set_enabled(start_with_windows);
        #[cfg(not(target_os = "windows"))]
        let startup_result: Result<(), String> = if start_with_windows {
            Err("Start with Windows is only available on Windows".to_string())
        } else {
            Ok(())
        };

        let mut snapshot = {
            let mut next = settings_for_ui.lock().unwrap().clone();
            next.api_key = elevenlabs_api_key.clone();
            next.transcription_provider =
                transcription::TranscriptionProvider::from_id(&transcription_provider)
                    .id()
                    .to_string();
            next.elevenlabs_api_key = elevenlabs_api_key;
            next.elevenlabs_model = elevenlabs_model.clone();
            next.elevenlabs_language_code = elevenlabs_language_code.clone();
            next.elevenlabs_no_verbatim = elevenlabs_no_verbatim;
            next.openai_api_key = openai_api_key;
            next.openai_model = openai_model.clone();
            next.openai_language_code = openai_language_code.clone();
            match transcription::TranscriptionProvider::from_id(&transcription_provider) {
                transcription::TranscriptionProvider::ElevenLabsRealtime => {
                    next.transcription_model = elevenlabs_model;
                    next.transcription_language_code = elevenlabs_language_code;
                    next.transcription_no_verbatim = elevenlabs_no_verbatim;
                }
                transcription::TranscriptionProvider::OpenAiRealtimeWhisper => {
                    next.transcription_model = openai_model;
                    next.transcription_language_code = openai_language_code;
                    next.transcription_no_verbatim = false;
                }
                transcription::TranscriptionProvider::LocalSherpaOnnx => {
                    next.transcription_model =
                        transcription::TranscriptionProvider::LocalSherpaOnnx
                            .default_model_id()
                            .to_string();
                    next.transcription_language_code = "en".to_string();
                    next.transcription_no_verbatim = false;
                }
            }
            next.gemini_api_key = gemini_api_key;
            next.gemini_enabled = gemini_enabled;
            next.gemini_model = gemini_model;
            next.gemini_prompt_preset = gemini_preset;
            next.gemini_custom_prompt = gemini_custom;
            next.selected_microphone = selected_mic;
            next.use_default_microphone = use_default_mic;
            next.start_with_windows = start_with_windows;
            next.local_sherpa = transcription::LocalSherpaConfig {
                num_threads: local_cpu_threads,
                vad_threshold: local_vad_threshold,
                silence_ms: local_silence_ms,
                pre_roll_ms: local_pre_roll_ms,
                post_roll_ms: local_post_roll_ms,
                min_speech_ms: local_min_speech_ms,
                max_segment_seconds: local_max_segment_seconds,
                partial_interval_ms: local_partial_interval_ms,
                redecode_full_session: local_redecode_full_session,
            }
            .normalized();
            next
        };
        if let Some(ui) = ui_weak_for_apply.upgrade() {
            snapshot.hotkey_text = ui.get_hotkey_text().to_string();
            snapshot.overlay_opacity = ui.get_overlay_opacity();
            snapshot.theme_background_top_color = ui.get_theme_background_top_color().to_string();
            snapshot.theme_background_bottom_color =
                ui.get_theme_background_bottom_color().to_string();
            snapshot.theme_window_color = ui.get_theme_window_color().to_string();
            snapshot.theme_button_accent_color = ui.get_theme_button_accent_color().to_string();
            snapshot.theme_title_color = ui.get_theme_title_color().to_string();
            snapshot.theme_text_color = ui.get_theme_text_color().to_string();
            snapshot.overlay_background_color = ui.get_overlay_background_color().to_string();
            snapshot.overlay_text_color = ui.get_overlay_text_color().to_string();
        }
        snapshot.normalize_transcription_settings();
        #[cfg(target_os = "windows")]
        let previous_hotkey = settings_for_ui.lock().unwrap().hotkey_text.clone();
        #[cfg(target_os = "windows")]
        let hotkey_changed = previous_hotkey != snapshot.hotkey_text;
        #[cfg(target_os = "windows")]
        let hotkey_result = if hotkey_changed {
            apply_hotkey(
                &hotkey_manager_for_apply,
                &mut hotkey_state_for_apply.borrow_mut(),
                &snapshot.hotkey_text,
            )
            .map(|new_id| {
                *hotkey_id_for_apply.borrow_mut() = Some(new_id);
                *hotkey_text_for_apply.lock().unwrap() = snapshot.hotkey_text.clone();
            })
        } else {
            Ok(())
        };
        #[cfg(not(target_os = "windows"))]
        let hotkey_result: Result<(), String> = Ok(());

        let saved = startup_result.is_ok() && hotkey_result.is_ok() && save_settings(&snapshot);
        if saved {
            echo_info!(
                "settings",
                "Settings saved provider={} use_default_microphone={} start_with_windows={} local_threads={} gemini_enabled={}",
                snapshot.transcription_provider,
                snapshot.use_default_microphone,
                snapshot.start_with_windows,
                snapshot.local_sherpa.num_threads,
                snapshot.gemini_enabled
            );
        } else {
            echo_error!(
                "settings",
                "Settings save failed startup_ok={} hotkey_ok={}",
                startup_result.is_ok(),
                hotkey_result.is_ok()
            );
        }
        #[cfg(target_os = "windows")]
        if !saved && hotkey_changed && hotkey_result.is_ok() {
            if let Ok(previous_id) = apply_hotkey(
                &hotkey_manager_for_apply,
                &mut hotkey_state_for_apply.borrow_mut(),
                &previous_hotkey,
            ) {
                *hotkey_id_for_apply.borrow_mut() = Some(previous_id);
                *hotkey_text_for_apply.lock().unwrap() = previous_hotkey;
            }
        }
        #[cfg(target_os = "windows")]
        if !saved && startup_result.is_ok() {
            let previous_startup = settings_for_ui.lock().unwrap().start_with_windows;
            if previous_startup != start_with_windows {
                let _ = startup::set_enabled(previous_startup);
            }
        }
        if saved {
            if let Ok(mut current) = settings_for_ui.lock() {
                *current = snapshot.clone();
            }
            let local_selected = matches!(
                transcription::TranscriptionProvider::from_id(&snapshot.transcription_provider),
                transcription::TranscriptionProvider::LocalSherpaOnnx
            );
            if local_selected && transcription::local_models_available() {
                transcription::preload_local_engine(&snapshot.local_sherpa);
                let repair_ui = ui_weak_for_settings.clone();
                thread::spawn(move || {
                    if let Err(err) = transcription::wait_for_local_engine() {
                        let _ = repair_ui.upgrade_in_event_loop(move |ui| {
                            ui.set_local_download_error(
                                format!("The local model could not load: {err}").into(),
                            );
                            request_local_model_prompt(&ui);
                            ui.set_status_text("Local model needs repair".into());
                            ui.set_has_error(true);
                            ui.set_active_tab(3);
                        });
                    }
                });
            }
        }

        if let Some(ui) = ui_weak_for_settings.upgrade() {
            if saved {
                ui.set_api_key_text(snapshot.elevenlabs_api_key.clone().into());
                ui.set_elevenlabs_model_text(snapshot.elevenlabs_model.clone().into());
                ui.set_elevenlabs_language_code_text(
                    snapshot.elevenlabs_language_code.clone().into(),
                );
                ui.set_elevenlabs_no_verbatim(snapshot.elevenlabs_no_verbatim);
                ui.set_openai_api_key_text(snapshot.openai_api_key.clone().into());
                ui.set_openai_model_text(snapshot.openai_model.clone().into());
                ui.set_openai_language_code_text(snapshot.openai_language_code.clone().into());
                ui.set_local_download_prompt_visible(false);
                ui.set_settings_dirty(false);
                ui.set_status_text("Settings saved".into());
                if let Some(target) = pending_navigation_for_apply.take() {
                    if target == -2 {
                        let _ = slint::quit_event_loop();
                    } else if target < 0 {
                        let _ = ui.window().hide();
                    } else {
                        ui.set_active_tab(target);
                    }
                }
            } else {
                pending_navigation_for_apply.set(None);
                let message = startup_result
                    .as_ref()
                    .err()
                    .map(|err| format!("Could not update Windows startup: {err}"))
                    .or_else(|| hotkey_result.as_ref().err().cloned())
                    .unwrap_or_else(|| "Settings save failed".to_string());
                ui.set_status_text(message.into());
                ui.set_has_error(true);
                ui.set_active_tab(3);
            }
        }
    });

    let ui_weak_for_unsaved_save = ui.as_weak();
    let pending_navigation_for_save = pending_navigation_after_save.clone();
    ui.on_save_unsaved_settings(move || {
        if let Some(ui) = ui_weak_for_unsaved_save.upgrade() {
            pending_navigation_for_save.set(Some(ui.get_pending_navigation_tab()));
            ui.set_unsaved_settings_prompt_visible(false);
            ui.invoke_apply_settings();
        }
    });

    let ui_weak_for_unsaved_discard = ui.as_weak();
    let settings_for_discard = settings.clone();
    ui.on_discard_unsaved_settings(move || {
        if let Some(ui) = ui_weak_for_unsaved_discard.upgrade() {
            let saved = settings_for_discard.lock().unwrap().clone();
            populate_settings_editor(&ui, &saved);
            ui.set_local_download_prompt_visible(false);
            ui.set_local_repair_pending(false);
            ui.set_local_download_error("".into());
            ui.set_settings_dirty(false);
            ui.set_unsaved_settings_prompt_visible(false);
            if matches!(
                transcription::TranscriptionProvider::from_id(&saved.transcription_provider),
                transcription::TranscriptionProvider::LocalSherpaOnnx
            ) && transcription::local_models_available()
            {
                transcription::preload_local_engine(&saved.local_sherpa);
            }
            let target = ui.get_pending_navigation_tab();
            if target == -2 {
                let _ = slint::quit_event_loop();
            } else if target < 0 {
                let _ = ui.window().hide();
            } else {
                ui.set_active_tab(target);
            }
        }
    });

    let ui_weak_for_unsaved_cancel = ui.as_weak();
    ui.on_cancel_unsaved_settings(move || {
        if let Some(ui) = ui_weak_for_unsaved_cancel.upgrade() {
            ui.set_unsaved_settings_prompt_visible(false);
        }
    });

    let ui_weak_for_download = ui.as_weak();
    let settings_for_download = settings.clone();
    ui.on_accept_local_model_download(move || {
        echo_info!("local_model", "User accepted local model download");
        let local_config = if let Some(ui) = ui_weak_for_download.upgrade() {
            ui.set_local_repair_pending(false);
            ui.set_local_download_prompt_visible(false);
            ui.set_local_download_progress_visible(true);
            ui.set_local_download_progress(0.0);
            ui.set_local_download_status("Preparing download...".into());
            ui.set_local_download_error("".into());
            settings_for_download.lock().unwrap().local_sherpa.clone()
        } else {
            transcription::LocalSherpaConfig::default()
        };
        let progress_ui = ui_weak_for_download.clone();
        let completion_ui = ui_weak_for_download.clone();
        thread::spawn(move || {
            let runtime = Runtime::new().unwrap();
            let result = runtime.block_on(transcription::download_local_models(
                move |fraction, status| {
                    let _ = progress_ui.upgrade_in_event_loop(move |ui| {
                        ui.set_local_download_progress(fraction);
                        ui.set_local_download_status(status.into());
                    });
                },
            ));

            let result = result.and_then(|_| {
                transcription::preload_local_engine(&local_config);
                transcription::wait_for_local_engine()
            });
            match &result {
                Ok(()) => echo_info!(
                    "local_model",
                    "Local model download, verification, and preload completed"
                ),
                Err(err) => echo_error!(
                    "local_model",
                    "Local model download or preload failed: {err}"
                ),
            }
            let _ = completion_ui.upgrade_in_event_loop(move |ui| match result {
                Ok(()) => {
                    ui.set_local_download_progress(1.0);
                    ui.set_local_download_progress_visible(false);
                    ui.set_local_download_status("Local speech model ready".into());
                    ui.set_status_text("Local speech model ready".into());
                    ui.set_has_error(false);
                    ui.set_active_tab(3);
                    ui.set_settings_tab(1);
                }
                Err(err) => {
                    ui.set_local_download_progress_visible(false);
                    request_local_model_prompt(&ui);
                    ui.set_local_download_status(format!("Download failed: {err}").into());
                    ui.set_local_download_error(format!("Download failed: {err}").into());
                    ui.set_status_text("Local model download failed".into());
                    ui.set_has_error(true);
                }
            });
        });
    });

    let ui_weak_for_download_decline = ui.as_weak();
    let local_offer_for_decline = local_download_offer_suppressed.clone();
    ui.on_decline_local_model_download(move || {
        echo_info!("local_model", "User postponed local model download");
        if let Some(ui) = ui_weak_for_download_decline.upgrade() {
            local_offer_for_decline.set(true);
            ui.set_local_repair_pending(false);
            ui.set_local_download_prompt_visible(false);
            ui.set_status_text("Local speech download postponed".into());
        }
    });

    let ui_weak_for_hotkey = ui.as_weak();
    #[cfg(target_os = "windows")]
    let hotkey_capture_window_for_start = hotkey_capture_window.as_weak();
    #[cfg(target_os = "windows")]
    let hotkey_capture_active_for_start = hotkey_capture_active.clone();
    #[cfg(target_os = "windows")]
    let hotkey_capture_latched_for_start = hotkey_capture_latched.clone();

    ui.on_start_hotkey_capture(move || {
        #[cfg(target_os = "windows")]
        {
            *hotkey_capture_active_for_start.borrow_mut() = true;
            *hotkey_capture_latched_for_start.borrow_mut() = false;
            if let Some(capture) = hotkey_capture_window_for_start.upgrade() {
                capture.set_state_text("Waiting for key combo...".into());
                capture.set_combo_text("".into());
                let _ = capture.show();
            }
            if let Some(ui) = ui_weak_for_hotkey.upgrade() {
                ui.set_status_text("Waiting for key combo...".into());
            }
        }

        #[cfg(not(target_os = "windows"))]
        {
            if let Some(ui) = ui_weak_for_hotkey.upgrade() {
                ui.set_status_text("Hotkeys are not supported on this platform".into());
                ui.invoke_request_navigation(2);
            }
        }
    });

    let ui_weak_for_clear = ui.as_weak();
    let transcript_history_for_clear = transcript_history_store.clone();
    let transcript_history_revision_for_clear = transcript_history_revision.clone();
    let transcript_raw_for_clear = transcript_raw_for_clipboard.clone();
    ui.on_clear_transcript(move || {
        echo_info!("history", "User requested transcript history clear");
        {
            let mut history = transcript_history_for_clear.lock().unwrap();
            history.clear();
            if !save_transcript_history(&history) {
                echo_error!("history", "Failed to clear persisted transcript history");
            }
            transcript_raw_for_clear.lock().unwrap().clear();
            transcript_history_revision_for_clear.fetch_add(1, Ordering::SeqCst);
        }
        if let Some(ui) = ui_weak_for_clear.upgrade() {
            ui.set_transcript("".into());
            ui.set_transcript_history(ModelRc::new(VecModel::from(Vec::<SharedString>::new())));
        }
    });

    let ui_handle_for_timer = ui.as_weak();
    let cmd_tx_for_timer = cmd_tx.clone();
    let settings_for_timer = settings.clone();
    let overlay_for_timer = transcript_overlay.as_weak();
    let last_provider_for_timer = Rc::new(RefCell::new(
        ui.get_transcription_provider_text().to_string(),
    ));
    let last_provider_for_timer_tick = last_provider_for_timer.clone();
    let local_offer_for_timer = local_download_offer_suppressed.clone();
    #[cfg(target_os = "windows")]
    let hotkey_capture_window_for_timer = hotkey_capture_window.as_weak();
    #[cfg(target_os = "windows")]
    let hotkey_capture_active_for_timer = hotkey_capture_active.clone();
    #[cfg(target_os = "windows")]
    let hotkey_capture_latched_for_timer = hotkey_capture_latched.clone();

    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(50),
        move || {
            if let Some(ui) = ui_handle_for_timer.upgrade() {
                let saved = settings_for_timer.lock().unwrap().clone();
                ui.set_settings_dirty(settings_editor_is_dirty(&ui, &saved));

                let selected_provider = ui.get_transcription_provider_text().to_string();
                let provider_changed = {
                    let mut previous = last_provider_for_timer_tick.borrow_mut();
                    if *previous == selected_provider {
                        false
                    } else {
                        *previous = selected_provider.clone();
                        true
                    }
                };
                if provider_changed {
                    local_offer_for_timer.set(false);
                }
                if ui.get_local_repair_pending()
                    && !ui.get_unsaved_settings_prompt_visible()
                    && !ui.get_local_download_prompt_visible()
                    && !ui.get_local_download_progress_visible()
                {
                    ui.set_local_repair_pending(false);
                    ui.set_local_download_prompt_visible(true);
                    ui.set_active_tab(3);
                    ui.set_settings_tab(1);
                    let _ = ui.show();
                }
                if ui.get_active_tab() == 3
                    && ui.get_settings_tab() == 1
                    && matches!(
                        transcription::TranscriptionProvider::from_id(&selected_provider),
                        transcription::TranscriptionProvider::LocalSherpaOnnx
                    )
                    && !transcription::local_models_available()
                    && !local_offer_for_timer.get()
                    && !ui.get_unsaved_settings_prompt_visible()
                    && !ui.get_local_download_prompt_visible()
                    && !ui.get_local_download_progress_visible()
                {
                    local_offer_for_timer.set(true);
                    ui.set_local_download_error("".into());
                    ui.set_local_download_prompt_visible(true);
                }

                if let Some(overlay) = overlay_for_timer.upgrade() {
                    overlay.set_overlay_opacity(ui.get_overlay_opacity());
                    overlay.set_overlay_background_color(ui.get_overlay_background_color());
                    overlay.set_overlay_text_color(ui.get_overlay_text_color());
                }

                #[cfg(target_os = "windows")]
                {
                    if *hotkey_capture_active_for_timer.borrow() {
                        if vk_down(VK_ESCAPE.0 as i32) {
                            *hotkey_capture_active_for_timer.borrow_mut() = false;
                            *hotkey_capture_latched_for_timer.borrow_mut() = false;
                            if let Some(capture) = hotkey_capture_window_for_timer.upgrade() {
                                let _ = capture.hide();
                            }
                            ui.set_status_text("Hotkey capture cancelled".into());
                            return;
                        }

                        if let Some(combo) = detect_hotkey_combo() {
                            if !*hotkey_capture_latched_for_timer.borrow() {
                                *hotkey_capture_latched_for_timer.borrow_mut() = true;
                                ui.set_hotkey_text(combo.clone().into());
                                ui.set_status_text("Hotkey change pending Save settings".into());
                                if let Some(capture) = hotkey_capture_window_for_timer.upgrade() {
                                    capture.set_state_text("Ready to save".into());
                                    capture.set_combo_text(combo.into());
                                    let _ = capture.hide();
                                }
                                *hotkey_capture_active_for_timer.borrow_mut() = false;
                            }
                        } else {
                            *hotkey_capture_latched_for_timer.borrow_mut() = false;
                        }
                    }

                    while let Ok(event) = GlobalHotKeyEvent::receiver().try_recv() {
                        let current_hotkey_id = *hotkey_id_state.borrow();
                        if current_hotkey_id.is_some_and(|id| event.id == id)
                            && event.state == HotKeyState::Pressed
                        {
                            echo_info!("hotkey", "Global recording hotkey pressed id={}", event.id);
                            let _ = cmd_tx_for_timer.send(AppCommand::ToggleRecording);
                        }
                    }

                    while let Ok(event) = TrayIconEvent::receiver().try_recv() {
                        if let TrayIconEvent::Click {
                            button,
                            button_state,
                            ..
                        } = event
                        {
                            if button == MouseButton::Left && button_state == MouseButtonState::Up {
                                echo_info!("tray", "Tray icon activated");
                                ui.invoke_request_navigation(0);
                                let _ = ui.show();
                            }
                        }
                    }

                    while let Ok(event) = MenuEvent::receiver().try_recv() {
                        if event.id == quit_item_id {
                            echo_info!("tray", "Quit menu item selected");
                            if ui.get_local_download_prompt_visible()
                                || ui.get_local_download_progress_visible()
                            {
                                ui.set_status_text(
                                    "Finish or dismiss the local model dialog before quitting"
                                        .into(),
                                );
                                let _ = ui.show();
                            } else if ui.get_active_tab() == 3
                                && settings_editor_is_dirty(&ui, &saved)
                            {
                                ui.set_pending_navigation_tab(-2);
                                ui.set_unsaved_settings_prompt_visible(true);
                                let _ = ui.show();
                            } else {
                                slint::quit_event_loop().unwrap();
                            }
                        } else if event.id == settings_item_id {
                            echo_info!("tray", "Settings menu item selected");
                            ui.set_active_tab(3);
                            ui.show().unwrap();
                        }
                    }
                }
            }
        },
    );

    let started_by_windows =
        cfg!(target_os = "windows") && std::env::args_os().skip(1).any(|arg| arg == "--startup");
    if !started_by_windows {
        ui.show()?;
    }

    #[cfg(target_os = "windows")]
    {
        // Native handles do not exist reliably until the event loop starts.
        // Create and style the overlay on the first event-loop turn, then hide
        // it until recording. Re-activate the main window after this one-time
        // setup when Echo was launched interactively.
        let overlay_for_native_init = transcript_overlay.as_weak();
        let ui_for_native_init = ui.as_weak();
        slint::Timer::single_shot(std::time::Duration::ZERO, move || {
            if let Some(overlay) = overlay_for_native_init.upgrade() {
                let _ = overlay.show();
                let overlay_after_creation = overlay.as_weak();
                slint::Timer::single_shot(std::time::Duration::from_millis(50), move || {
                    if let Some(overlay) = overlay_after_creation.upgrade() {
                        if let Err(err) = configure_overlay_as_non_activating(&overlay) {
                            echo_warn!(
                                "overlay",
                                "Could not initialize non-activating overlay style: {err}"
                            );
                        }
                        let _ = overlay.hide();
                    }
                    if !started_by_windows {
                        if let Some(ui) = ui_for_native_init.upgrade() {
                            let _ = ui.show();
                        }
                    }
                });
            } else if !started_by_windows {
                if let Some(ui) = ui_for_native_init.upgrade() {
                    let _ = ui.show();
                }
            }
        });
    }
    echo_info!("app", "Slint event loop starting");
    slint::run_event_loop_until_quit()?;
    echo_info!("app", "Slint event loop stopped; Echo exiting cleanly");
    Ok(())
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::{forward_audio_until_stopped, parse_hotkey};
    use std::time::Duration;
    use tokio::sync::mpsc;

    #[test]
    fn parse_hotkey_accepts_common_combos() {
        assert!(parse_hotkey("Ctrl+Space").is_ok());
        assert!(parse_hotkey("Ctrl+Shift+F8").is_ok());
        assert!(parse_hotkey("Alt+Win+9").is_ok());
        assert!(parse_hotkey("Control+Super+X").is_ok());
    }

    #[test]
    fn parse_hotkey_rejects_missing_key() {
        let err = parse_hotkey("Ctrl+Shift").unwrap_err();
        assert!(err.contains("No key found"));
    }

    #[test]
    fn parse_hotkey_rejects_unknown_key_token() {
        let err = parse_hotkey("Ctrl+Tab").unwrap_err();
        assert!(err.contains("Unsupported key token"));
    }

    #[tokio::test]
    async fn stopping_audio_forwarding_closes_the_network_stream() {
        let (capture_tx, capture_rx) = mpsc::channel(4);
        let (network_tx, mut network_rx) = mpsc::channel(4);
        let (stop_tx, stop_rx) = mpsc::unbounded_channel();

        let task = tokio::spawn(forward_audio_until_stopped(capture_rx, network_tx, stop_rx));

        capture_tx.send(vec![1, 2, 3]).await.unwrap();
        assert_eq!(network_rx.recv().await, Some(vec![1, 2, 3]));

        stop_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("forwarding task should stop promptly")
            .expect("forwarding task should not panic");

        assert_eq!(network_rx.recv().await, None);
        assert!(capture_tx.send(vec![4]).await.is_err());
    }
}
