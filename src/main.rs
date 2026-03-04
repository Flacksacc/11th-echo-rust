mod injector;
mod audio;
mod hotkey;
mod network;
mod pipeline;
mod settings;
mod state;
mod gemini;

use slint::{CloseRequestResponse, Color, ComponentHandle, ModelRc, SharedString, VecModel};
use std::sync::{Arc, Mutex};
use std::thread;
use pipeline::TranscriptPipeline;
use settings::{load_settings, save_settings};
use state::RecordingState;
use tokio::runtime::Runtime;
use tokio::sync::mpsc;

#[cfg(target_os = "windows")]
use global_hotkey::{
    hotkey::{Code, HotKey, Modifiers},
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
};
#[cfg(target_os = "windows")]
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_CONTROL, VK_MENU, VK_RWIN, VK_LWIN, VK_SHIFT,
    VK_SPACE, VK_ESCAPE, VK_F1, VK_F2, VK_F3, VK_F4, VK_F5, VK_F6, VK_F7, VK_F8, VK_F9, VK_F10, VK_F11, VK_F12,
};
#[cfg(target_os = "windows")]
use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CYSCREEN};
#[cfg(target_os = "windows")]
use std::{cell::RefCell, rc::Rc};
#[cfg(target_os = "windows")]
use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem},
    MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent,
};

slint::include_modules!();

const ELEVEN_MODEL_ID: &str = "scribe_v2_realtime";

#[derive(Debug)]
enum AppCommand {
    StartRecording,
    StopRecording,
}

struct Session {
    state: Arc<Mutex<RecordingState>>,
    _audio_stream: Option<cpal::Stream>,
    network_stop_tx: Option<mpsc::UnboundedSender<network::ControlMessage>>,
    transcript_pipeline: Arc<Mutex<TranscriptPipeline>>,
}

impl Session {
    fn stop_network(&mut self) {
        if let Some(tx) = self.network_stop_tx.as_ref() {
            let _ = tx.send(network::ControlMessage::Stop);
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
    let previous = current_hotkey.clone();

    if let Some(existing) = previous.clone() {
        let _ = manager.unregister(existing);
    }

    match manager.register(new_hotkey.clone()) {
        Ok(_) => {
            let id = new_hotkey.id();
            *current_hotkey = Some(new_hotkey);
            Ok(id)
        }
        Err(err) => {
            if let Some(existing) = previous {
                let _ = manager.register(existing.clone());
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
        ("A", 0x41), ("B", 0x42), ("C", 0x43), ("D", 0x44), ("E", 0x45), ("F", 0x46),
        ("G", 0x47), ("H", 0x48), ("I", 0x49), ("J", 0x4A), ("K", 0x4B), ("L", 0x4C),
        ("M", 0x4D), ("N", 0x4E), ("O", 0x4F), ("P", 0x50), ("Q", 0x51), ("R", 0x52),
        ("S", 0x53), ("T", 0x54), ("U", 0x55), ("V", 0x56), ("W", 0x57), ("X", 0x58),
        ("Y", 0x59), ("Z", 0x5A),
        ("0", 0x30), ("1", 0x31), ("2", 0x32), ("3", 0x33), ("4", 0x34),
        ("5", 0x35), ("6", 0x36), ("7", 0x37), ("8", 0x38), ("9", 0x39),
        ("F1", VK_F1.0 as i32), ("F2", VK_F2.0 as i32), ("F3", VK_F3.0 as i32), ("F4", VK_F4.0 as i32),
        ("F5", VK_F5.0 as i32), ("F6", VK_F6.0 as i32), ("F7", VK_F7.0 as i32), ("F8", VK_F8.0 as i32),
        ("F9", VK_F9.0 as i32), ("F10", VK_F10.0 as i32), ("F11", VK_F11.0 as i32), ("F12", VK_F12.0 as i32),
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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    println!("🦋 11th Echo Rust (Iron Butterfly) Starting...");

    let microphones = audio::list_input_devices();
    let default_microphone =
        audio::default_input_device_name().unwrap_or_else(|| "Unavailable".to_string());
    let mut initial_settings = load_settings();
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
    save_settings(&initial_settings);
    let selected_microphone = initial_settings.selected_microphone.clone();
    let settings = Arc::new(Mutex::new(initial_settings.clone()));

    #[cfg(target_os = "windows")]
    let hotkey_text = Arc::new(Mutex::new(initial_settings.hotkey_text.clone()));

    #[cfg(target_os = "windows")]
    let hotkey_manager = GlobalHotKeyManager::new().unwrap();
    #[cfg(target_os = "windows")]
    let hotkey_state = Rc::new(RefCell::new(None::<HotKey>));
    #[cfg(target_os = "windows")]
    let hotkey_id_state = Rc::new(RefCell::new(None::<u32>));

    #[cfg(target_os = "windows")]
    {
        let startup_hotkey = hotkey_text.lock().unwrap().clone();
        match apply_hotkey(&hotkey_manager, &mut hotkey_state.borrow_mut(), &startup_hotkey) {
            Ok(id) => {
                *hotkey_id_state.borrow_mut() = Some(id);
            }
            Err(err) => {
                eprintln!(
                    "⚠️ Failed to register global hotkey {}: {}. Continuing without hotkey support.",
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

        let icon = tray_icon::Icon::from_path("eleventhecho.png", None).or_else(|png_err| {
            eprintln!(
                "⚠️ Tray icon PNG load failed ({}), trying ICO fallback.",
                png_err
            );
            tray_icon::Icon::from_path("eleventhecho.ico", None)
        })?;
        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(tray_menu))
            .with_tooltip("11th Echo")
            .with_icon(icon)
            .build()?;
        (quit_item.id().clone(), settings_item.id().clone(), Some(tray))
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
    ui.set_api_key_text(initial_settings.api_key.clone().into());
    ui.set_gemini_api_key_text(initial_settings.gemini_api_key.clone().into());
    ui.set_selected_microphone(selected_microphone.clone().into());
    ui.set_use_default_microphone(initial_settings.use_default_microphone);
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

    // When the user closes the main window, hide it but keep the Slint
    // event loop alive so the app can continue running from the tray.
    let ui_weak_for_close = ui.as_weak();
    ui.window().on_close_requested(move || {
        if let Some(ui) = ui_weak_for_close.upgrade() {
            let _ = ui.window().hide();
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
    #[cfg(target_os = "windows")]
    {
        let screen_h = unsafe { GetSystemMetrics(SM_CYSCREEN) };
        let overlay_h = 120;
        let margin = 24;
        transcript_overlay.window().set_position(slint::PhysicalPosition::new(margin, (screen_h - overlay_h - margin).max(0)));
    }
    #[cfg(not(target_os = "windows"))]
    {
        transcript_overlay.window().set_position(slint::LogicalPosition::new(24.0, 820.0));
    }

    let overlay_weak_for_drag = transcript_overlay.as_weak();
    transcript_overlay.on_move_window(move |dx, dy| {
        if let Some(overlay) = overlay_weak_for_drag.upgrade() {
            let current = overlay.window().position();
            let scale = overlay.window().scale_factor();
            overlay.window().set_position(slint::PhysicalPosition::new(
                current.x + (dx as f32 * scale) as i32,
                current.y + (dy as f32 * scale) as i32,
            ));
        }
    });

    let ui_handle_for_tokio = ui.as_weak();
    let overlay_handle_for_tokio = transcript_overlay.as_weak();
    let settings_for_runtime = settings.clone();

    thread::spawn(move || {
        let rt = Runtime::new().unwrap();
        rt.block_on(async move {
            println!("⚡ Tokio Runtime Active");

            let mut active_session: Option<Session> = None;
            let (finalize_tx, mut finalize_rx) = mpsc::unbounded_channel::<()>();

            loop {
                tokio::select! {
                    Some(level) = level_rx.recv() => {
                        let _ = ui_handle_for_tokio.upgrade_in_event_loop(move |ui| {
                            ui.set_audio_level(level);
                        });
                    }
                    Some(()) = finalize_rx.recv() => {
                        if let Some(session) = active_session.take() {
                            if let Ok(mut state) = session.state.lock() {
                                state.transition_to_idle();
                            }
                            println!("✅ Finalization complete, session closed");
                        }
                        let _ = ui_handle_for_tokio.upgrade_in_event_loop(|ui| {
                            ui.set_audio_level(0.0);
                            ui.set_is_recording(false);
                            ui.set_status_text("Idle".into());
                        });
                        let _ = overlay_handle_for_tokio.upgrade_in_event_loop(|overlay| {
                            overlay.set_sentence_text("".into());
                            overlay.set_window_width(520);
                            overlay.set_window_height(120);
                            let _ = overlay.hide();
                        });
                    }
                    Some(cmd) = cmd_rx.recv() => {
                        match cmd {
                            AppCommand::StartRecording => {
                                if let Some(session) = active_session.as_mut() {
                                    if session.state.lock().unwrap().can_start() {
                                        if let Ok(mut pipeline) = session.transcript_pipeline.lock() {
                                            *pipeline = TranscriptPipeline::new();
                                        }
                                        if let Ok(mut s) = session.state.lock() {
                                            s.transition_to_recording();
                                        }
                                        let _ = ui_handle_for_tokio.upgrade_in_event_loop(|ui| {
                                            ui.set_status_text("Listening...".into());
                                            ui.set_is_recording(true);
                                            ui.set_transcript("".into());
                                        });
                                        let _ = overlay_handle_for_tokio.upgrade_in_event_loop(|overlay| {
                                            overlay.set_sentence_text("".into());
                                            overlay.set_window_width(520);
                                            overlay.set_window_height(120);
                                            let _ = overlay.show();
                                        });
                                        if let Some(tx) = session.network_stop_tx.as_ref() {
                                            let _ = tx.send(network::ControlMessage::Start);
                                        }
                                        println!("⚡ Resumed existing transcription session");
                                        continue;
                                    } else {
                                        println!("❌ Cannot start recording: session already active");
                                        continue;
                                    }
                                }

                                let current_settings = settings_for_runtime.lock().unwrap().clone();
                                if current_settings.api_key.trim().is_empty() {
                                    let _ = ui_handle_for_tokio.upgrade_in_event_loop(|ui| {
                                        ui.set_status_text("Missing API key".into());
                                        ui.set_is_recording(false);
                                    });
                                    continue;
                                }

                                let preferred_device = if current_settings.use_default_microphone {
                                    None
                                } else {
                                    Some(current_settings.selected_microphone.clone())
                                };

                                println!("⚡ Starting Recording Session...");
                                let _ = ui_handle_for_tokio.upgrade_in_event_loop(|ui| {
                                    ui.set_status_text("Connecting...".into());
                                    ui.set_transcript("".into());
                                });
                                let _ = overlay_handle_for_tokio.upgrade_in_event_loop(|overlay| {
                                    overlay.set_sentence_text("Listening...".into());
                                    overlay.set_window_width(520);
                                    overlay.set_window_height(120);
                                    let _ = overlay.show();
                                });

                                let state = Arc::new(Mutex::new(RecordingState::BufferingPreConnect));
                                let transcript_pipeline = Arc::new(Mutex::new(TranscriptPipeline::new()));
                                let (audio_tx, audio_rx) = mpsc::channel::<Vec<i16>>(50);
                                let (network_stop_tx, network_stop_rx) =
                                    mpsc::unbounded_channel::<network::ControlMessage>();
                                let (text_tx, mut text_rx) =
                                    mpsc::channel::<network::TranscriptMessage>(100);
                                let audio_level_tx = level_tx.clone();

                                let stream_result =
                                    audio::start_audio_capture(audio_tx, audio_level_tx, preferred_device);

                                match stream_result {
                                    Ok(stream) => {
                                        let client = network::ElevenLabsClient::new(
                                            current_settings.api_key,
                                            ELEVEN_MODEL_ID.to_string(),
                                        );
                                        let client_state = state.clone();
                                        let injection_state = state.clone();
                                        let transcript_pipeline_for_text = transcript_pipeline.clone();
                                        let settings_for_text = settings_for_runtime.clone();
                                        let finalize_tx_for_network = finalize_tx.clone();
                                        let finalize_tx_for_injection = finalize_tx.clone();
                                        let ui_handle_for_network = ui_handle_for_tokio.clone();
                                        let ui_handle_for_transcript = ui_handle_for_tokio.clone();
                                        let overlay_handle_for_transcript = overlay_handle_for_tokio.clone();

                                        let _ = ui_handle_for_tokio.upgrade_in_event_loop(|ui| {
                                            ui.set_is_recording(true);
                                            ui.set_status_text("Listening...".into());
                                        });

                                        tokio::spawn(async move {
                                            {
                                                let mut s = client_state.lock().unwrap();
                                                s.transition_to_connecting();
                                            }

                                            let result = client.run(audio_rx, network_stop_rx, text_tx).await;
                                            if let Err(err) = result {
                                                eprintln!("❌ Network client failed: {}", err);
                                                if let Ok(mut s) = client_state.lock() {
                                                    *s = RecordingState::Error;
                                                }
                                                let _ = ui_handle_for_network.upgrade_in_event_loop(|ui| {
                                                    ui.set_status_text("Network error".into());
                                                    ui.set_is_recording(false);
                                                });
                                                let _ = finalize_tx_for_network.send(());
                                                return;
                                            }

                                            if let Ok(mut s) = client_state.lock() {
                                                if matches!(
                                                    *s,
                                                    RecordingState::BufferingPreConnect
                                                        | RecordingState::Connecting
                                                        | RecordingState::Recording
                                                ) {
                                                    s.transition_to_finalizing();
                                                }
                                            }

                                            println!("⚡ Network client task ended");
                                        });

                                        tokio::spawn(async move {
                                            let mut latest_partial = String::new();
                                            while let Some(msg) = text_rx.recv().await {
                                                {
                                                    let mut s = injection_state.lock().unwrap();
                                                    s.transition_to_recording();
                                                }

                                                let mut was_committed = false;
                                                let display_text = match msg {
                                                    network::TranscriptMessage::Partial(text) => {
                                                        latest_partial = text;
                                                        let committed = {
                                                            let pipeline = transcript_pipeline_for_text.lock().unwrap();
                                                            pipeline.committed_text().trim().to_string()
                                                        };
                                                        if committed.is_empty() {
                                                            latest_partial.clone()
                                                        } else if latest_partial.trim().is_empty() {
                                                            committed
                                                        } else {
                                                            format!("{} {}", committed, latest_partial.trim())
                                                        }
                                                    }
                                                    network::TranscriptMessage::Committed(text) => {
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
                                                            println!("🤖 [Gemini] Rewriting committed text...");
                                                            gemini::rewrite_text(&gkey, &gmodel, &gpreset, &gcustom, &text).await
                                                        } else {
                                                            text
                                                        };

                                                        let aggregated = {
                                                            let mut pipeline = transcript_pipeline_for_text.lock().unwrap();
                                                            pipeline.push_fragment(&final_text)
                                                        };
                                                        if let Err(e) = injector::inject_text(&final_text) {
                                                            eprintln!("❌ Injection Error: {}", e);
                                                        }
                                                        was_committed = true;
                                                        aggregated
                                                    }
                                                };

                                                let aggregated_for_ui = display_text.clone();
                                                let aggregated_for_overlay = display_text.clone();
                                                let hide_overlay = was_committed;
                                                let _ = ui_handle_for_transcript.upgrade_in_event_loop(move |ui| {
                                                    ui.set_transcript(aggregated_for_ui.into());
                                                });
                                                let _ = overlay_handle_for_transcript
                                                    .upgrade_in_event_loop(move |overlay| {
                                                        if hide_overlay {
                                                            overlay.set_sentence_text("".into());
                                                            overlay.set_window_width(520);
                                                            overlay.set_window_height(120);
                                                            let _ = overlay.hide();
                                                        } else {
                                                            let (w, h) =
                                                                overlay_size_for_text(
                                                                    &aggregated_for_overlay,
                                                                );
                                                            overlay.set_sentence_text(
                                                                aggregated_for_overlay.into(),
                                                            );
                                                            overlay.set_window_width(w);
                                                            overlay.set_window_height(h);
                                                            let _ = overlay.show();
                                                        }
                                                    });
                                            }

                                            // Committed transcripts are injected immediately as they arrive.
                                            let _ = finalize_tx_for_injection.send(());
                                        });

                                        active_session = Some(Session {
                                            state,
                                            _audio_stream: Some(stream),
                                            network_stop_tx: Some(network_stop_tx),
                                            transcript_pipeline,
                                        });
                                        if let Some(session) = active_session.as_ref() {
                                            if let Some(tx) = session.network_stop_tx.as_ref() {
                                                let _ = tx.send(network::ControlMessage::Start);
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        eprintln!("❌ Failed to start audio: {}", e);
                                        let _ = ui_handle_for_tokio.upgrade_in_event_loop(move |ui| {
                                            ui.set_is_recording(false);
                                            ui.set_status_text(format!("Audio error: {}", e).into());
                                            ui.set_active_tab(2);
                                        });
                                        let _ = overlay_handle_for_tokio.upgrade_in_event_loop(|overlay| {
                                            overlay.set_sentence_text("".into());
                                            overlay.set_window_width(520);
                                            overlay.set_window_height(120);
                                            let _ = overlay.hide();
                                        });
                                    }
                                }
                            }
                            AppCommand::StopRecording => {
                                println!("⚡ Stop requested");
                                let _ = ui_handle_for_tokio.upgrade_in_event_loop(|ui| {
                                    ui.set_status_text("Finalizing...".into());
                                    ui.set_is_recording(false);
                                });

                                // Immediately hide and reset the overlay, even if no
                                // transcript text was ever produced for this session.
                                let _ = overlay_handle_for_tokio.upgrade_in_event_loop(|overlay| {
                                    overlay.set_sentence_text("".into());
                                    overlay.set_window_width(520);
                                    overlay.set_window_height(120);
                                    let _ = overlay.hide();
                                });

                                if let Some(session) = active_session.as_mut() {
                                    {
                                        let mut s = session.state.lock().unwrap();
                                        if s.can_stop() {
                                            s.transition_to_finalizing();
                                        }
                                    }
                                    if let Ok(mut pipeline) = session.transcript_pipeline.lock() {
                                        pipeline.request_stop();
                                    }
                                    session.stop_network();
                                    if let Ok(mut s) = session.state.lock() {
                                        s.transition_to_idle();
                                    }
                                }
                            }
                        }
                    }
                }
            }
        });
    });

    let start_tx = cmd_tx.clone();
    ui.on_start_recording(move || {
        let _ = start_tx.send(AppCommand::StartRecording);
    });

    let stop_tx = cmd_tx.clone();
    ui.on_stop_recording(move || {
        let _ = stop_tx.send(AppCommand::StopRecording);
    });

    let settings_for_ui = settings.clone();
    let ui_weak_for_settings = ui.as_weak();

    let settings_for_save = settings.clone();
    let ui_weak_for_apply = ui.as_weak();
    ui.on_apply_settings(move || {
        let (api_key, gemini_api_key, gemini_enabled, gemini_model, gemini_preset, gemini_custom, selected_mic, use_default_mic) =
            if let Some(ui) = ui_weak_for_apply.upgrade() {
                (
                    ui.get_api_key_text().to_string(),
                    ui.get_gemini_api_key_text().to_string(),
                    ui.get_use_gemini_modifier(),
                    ui.get_gemini_model_text().to_string(),
                    ui.get_selected_gemini_preset().to_string(),
                    ui.get_gemini_custom_prompt().to_string(),
                    ui.get_selected_microphone().to_string(),
                    ui.get_use_default_microphone(),
                )
            } else {
                (String::new(), String::new(), false, "gemini-3.1-flash-lite-preview".to_string(), "Minimal corrections".to_string(), String::new(), String::new(), true)
            };

        let snapshot = {
            let mut current = settings_for_ui.lock().unwrap();
            current.api_key = api_key;
            current.gemini_api_key = gemini_api_key;
            current.gemini_enabled = gemini_enabled;
            current.gemini_model = gemini_model;
            current.gemini_prompt_preset = gemini_preset;
            current.gemini_custom_prompt = gemini_custom;
            current.selected_microphone = selected_mic;
            current.use_default_microphone = use_default_mic;
            current.clone()
        };
        save_settings(&snapshot);

        if let Some(ui) = ui_weak_for_settings.upgrade() {
            ui.set_status_text("Settings applied".into());
            ui.set_active_tab(0);
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
                ui.set_active_tab(2);
            }
        }
    });

    let ui_weak_for_clear = ui.as_weak();
    ui.on_clear_transcript(move || {
        if let Some(ui) = ui_weak_for_clear.upgrade() {
            ui.set_transcript("".into());
        }
    });

    let ui_handle_for_timer = ui.as_weak();
    let cmd_tx_for_timer = cmd_tx.clone();
    let settings_for_timer = settings.clone();
    let overlay_for_timer = transcript_overlay.as_weak();
    #[cfg(target_os = "windows")]
    let hotkey_capture_window_for_timer = hotkey_capture_window.as_weak();
    #[cfg(target_os = "windows")]
    let hotkey_capture_active_for_timer = hotkey_capture_active.clone();
    #[cfg(target_os = "windows")]
    let hotkey_capture_latched_for_timer = hotkey_capture_latched.clone();
    #[cfg(target_os = "windows")]
    let hotkey_text_for_timer = hotkey_text.clone();

    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(50),
        move || {
            if let Some(ui) = ui_handle_for_timer.upgrade() {
                {
                    let mut s = settings_for_timer.lock().unwrap();
                    s.api_key = ui.get_api_key_text().to_string();
                    s.gemini_api_key = ui.get_gemini_api_key_text().to_string();
                    s.gemini_enabled = ui.get_use_gemini_modifier();
                    s.gemini_model = ui.get_gemini_model_text().to_string();
                    s.gemini_prompt_preset = ui.get_selected_gemini_preset().to_string();
                    s.gemini_custom_prompt = ui.get_gemini_custom_prompt().to_string();
                    s.selected_microphone = ui.get_selected_microphone().to_string();
                    s.use_default_microphone = ui.get_use_default_microphone();
                    s.overlay_opacity = ui.get_overlay_opacity();
                    s.theme_background_top_color =
                        ui.get_theme_background_top_color().to_string();
                    s.theme_background_bottom_color =
                        ui.get_theme_background_bottom_color().to_string();
                    s.theme_window_color = ui.get_theme_window_color().to_string();
                    s.theme_button_accent_color =
                        ui.get_theme_button_accent_color().to_string();
                    s.theme_title_color = ui.get_theme_title_color().to_string();
                    s.theme_text_color = ui.get_theme_text_color().to_string();
                    s.overlay_background_color =
                        ui.get_overlay_background_color().to_string();
                    s.overlay_text_color = ui.get_overlay_text_color().to_string();
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
                                match apply_hotkey(&hotkey_manager, &mut hotkey_state.borrow_mut(), &combo) {
                                    Ok(new_id) => {
                                        *hotkey_id_state.borrow_mut() = Some(new_id);
                                        *hotkey_text_for_timer.lock().unwrap() = combo.clone();
                                        ui.set_hotkey_text(combo.clone().into());
                                        ui.set_status_text("Hotkey updated".into());
                                        if let Ok(mut saved) = settings_for_save.lock() {
                                            saved.hotkey_text = combo.clone();
                                            save_settings(&saved);
                                        }
                                        if let Some(capture) = hotkey_capture_window_for_timer.upgrade() {
                                            capture.set_state_text("Registered".into());
                                            capture.set_combo_text(combo.into());
                                            let _ = capture.hide();
                                        }
                                    }
                                    Err(err) => {
                                        ui.set_status_text(format!("Hotkey unchanged: {}", err).into());
                                        ui.set_active_tab(2);
                                        if let Some(capture) = hotkey_capture_window_for_timer.upgrade() {
                                            capture.set_state_text(format!("Failed: {}", err).into());
                                            capture.set_combo_text(combo.into());
                                        }
                                    }
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
                            if ui.get_is_recording() {
                                let _ = cmd_tx_for_timer.send(AppCommand::StopRecording);
                            } else {
                                let _ = cmd_tx_for_timer.send(AppCommand::StartRecording);
                            }
                        }
                    }

                    while let Ok(event) = TrayIconEvent::receiver().try_recv() {
                        if let TrayIconEvent::Click { button, button_state, .. } = event {
                            if button == MouseButton::Left && button_state == MouseButtonState::Up {
                                ui.set_active_tab(0);
                                let _ = ui.show();
                            }
                        }
                    }

                    while let Ok(event) = MenuEvent::receiver().try_recv() {
                        if event.id == quit_item_id {
                            slint::quit_event_loop().unwrap();
                        } else if event.id == settings_item_id {
                            ui.set_active_tab(2);
                            ui.show().unwrap();
                        }
                    }
                }
            }
        },
    );

    ui.show()?;
    slint::run_event_loop_until_quit()?;
    Ok(())
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::parse_hotkey;

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
}
