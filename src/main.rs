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
mod post_processing;
mod settings;
mod startup;
mod state;
mod transcription;
mod updater;

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

fn begin_update_check(
    ui: slint::Weak<AppWindow>,
    config: updater::UpdateConfig,
    available: Arc<Mutex<Option<updater::UpdateInfo>>>,
    busy: Arc<AtomicBool>,
    manual: bool,
) {
    if busy.swap(true, Ordering::SeqCst) {
        return;
    }
    let update_ui = ui.clone();
    let _ = ui.upgrade_in_event_loop(move |ui| {
        ui.set_update_state(1);
        ui.set_update_status("Checking for updates…".into());
        if manual {
            ui.set_update_panel_visible(true);
        }
    });
    thread::spawn(move || {
        let result = Runtime::new()
            .map_err(|err| format!("Could not start the update check: {err}"))
            .and_then(|runtime| {
                let current = semver::Version::parse(env!("CARGO_PKG_VERSION"))
                    .map_err(|err| format!("Invalid application version: {err}"))?;
                runtime.block_on(updater::check_for_update(&config, &current))
            });
        busy.store(false, Ordering::SeqCst);
        let _ = update_ui.upgrade_in_event_loop(move |ui| match result {
            Ok(Some(info)) => {
                ui.set_update_available_version(info.manifest.version.to_string().into());
                ui.set_update_release_notes(
                    info.manifest
                        .release_notes
                        .clone()
                        .unwrap_or_default()
                        .into(),
                );
                ui.set_update_status(
                    format!(
                        "Version {} is ready to download and install.",
                        info.manifest.version
                    )
                    .into(),
                );
                ui.set_update_state(2);
                *available.lock().unwrap() = Some(info);
            }
            Ok(None) => {
                *available.lock().unwrap() = None;
                ui.set_update_available_version("".into());
                ui.set_update_release_notes("".into());
                ui.set_update_status("Echo is up to date.".into());
                ui.set_update_state(0);
            }
            Err(err) => {
                echo_warn!("updater", "Update check failed: {err}");
                ui.set_update_status(format!("Could not check for updates: {err}").into());
                ui.set_update_state(4);
            }
        });
    });
}

fn begin_update_install(
    ui: slint::Weak<AppWindow>,
    available: Arc<Mutex<Option<updater::UpdateInfo>>>,
    busy: Arc<AtomicBool>,
) {
    if busy.swap(true, Ordering::SeqCst) {
        return;
    }
    let Some(info) = available.lock().unwrap().clone() else {
        busy.store(false, Ordering::SeqCst);
        let _ = ui.upgrade_in_event_loop(|ui| {
            ui.set_update_status("Check for updates before installing.".into());
            ui.set_update_state(4);
        });
        return;
    };
    let progress_ui = ui.clone();
    let completion_ui = ui.clone();
    let _ = ui.upgrade_in_event_loop(|ui| {
        ui.set_update_state(3);
        ui.set_update_progress(0.0);
        ui.set_update_status("Downloading and verifying the installer…".into());
    });
    thread::spawn(move || {
        let last_percent = Arc::new(AtomicU64::new(u64::MAX));
        let progress_counter = last_percent.clone();
        let result = Runtime::new()
            .map_err(|err| format!("Could not start the update download: {err}"))
            .and_then(|runtime| {
                runtime.block_on(updater::download_update(&info, move |fraction| {
                    let percent = (fraction.clamp(0.0, 1.0) * 100.0).floor() as u64;
                    if progress_counter.swap(percent, Ordering::SeqCst) != percent {
                        let _ = progress_ui.upgrade_in_event_loop(move |ui| {
                            ui.set_update_progress(fraction.clamp(0.0, 1.0));
                            ui.set_update_status(format!("Downloading update… {percent}%").into());
                        });
                    }
                }))
            })
            .and_then(|path| updater::launch_installer(&path));
        match result {
            Ok(()) => {
                let _ = completion_ui.upgrade_in_event_loop(|ui| {
                    ui.set_update_progress(1.0);
                    ui.set_update_status(
                        "Installer started. Echo will restart after updating.".into(),
                    );
                    let _ = slint::quit_event_loop();
                });
            }
            Err(err) => {
                busy.store(false, Ordering::SeqCst);
                echo_error!("updater", "Update installation failed: {err}");
                let _ = completion_ui.upgrade_in_event_loop(move |ui| {
                    ui.set_update_status(format!("Update failed: {err}").into());
                    ui.set_update_state(4);
                });
            }
        }
    });
}

const FINALIZATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);
const CAPTURE_POST_ROLL: std::time::Duration = std::time::Duration::from_millis(300);
const OVERLAY_WIDTH: i32 = 560;
const OVERLAY_HEIGHT: i32 = 106;
const OVERLAY_MAX_LINES: usize = 6;
const OVERLAY_CHARACTERS_PER_LINE: usize = 62;
const OVERLAY_CAPTION_CHARACTERS: usize = OVERLAY_MAX_LINES * OVERLAY_CHARACTERS_PER_LINE;

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
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE, HWND, POINT, RECT, WAIT_OBJECT_0,
};
#[cfg(target_os = "windows")]
use windows::Win32::Graphics::Gdi::{
    CreateRoundRectRgn, DeleteObject, GetMonitorInfoW, MonitorFromPoint, SetWindowRgn, MONITORINFO,
    MONITOR_DEFAULTTONEAREST,
};
#[cfg(target_os = "windows")]
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, SetEvent, WaitForSingleObject,
};
#[cfg(target_os = "windows")]
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_CONTROL, VK_ESCAPE, VK_F1, VK_F10, VK_F11, VK_F12, VK_F2, VK_F3, VK_F4,
    VK_F5, VK_F6, VK_F7, VK_F8, VK_F9, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT, VK_SPACE,
};
#[cfg(target_os = "windows")]
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, FindWindowW, GetForegroundWindow, GetSystemMetrics,
    GetWindowLongPtrW, GetWindowRect, GetWindowThreadProcessId, IsIconic, SetForegroundWindow,
    SetWindowLongPtrW, ShowWindow, GWL_EXSTYLE, SM_CYSCREEN, SW_RESTORE, SW_SHOW, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW,
};

slint::include_modules!();

#[derive(Debug)]
enum AppCommand {
    ToggleRecording,
    StartRecording,
    StopRecording,
    ReconfigureWarmCapture,
    CapturePostRollElapsed(u64),
    CaptureFlushed(u64, Result<audio::CaptureFlushStats, String>),
    FinalizationTimedOut(u64),
}

fn app_command_name(command: &AppCommand) -> &'static str {
    match command {
        AppCommand::ToggleRecording => "toggle_recording",
        AppCommand::StartRecording => "start_recording",
        AppCommand::StopRecording => "stop_recording",
        AppCommand::ReconfigureWarmCapture => "reconfigure_warm_capture",
        AppCommand::CapturePostRollElapsed(_) => "capture_post_roll_elapsed",
        AppCommand::CaptureFlushed(_, _) => "capture_flushed",
        AppCommand::FinalizationTimedOut(_) => "finalization_timed_out",
    }
}

struct Session {
    epoch: u64,
    state: Arc<Mutex<RecordingState>>,
    audio_capture: Option<audio::AudioCapture>,
    warm_capture_attached: bool,
    audio_forward_stop_tx: Option<mpsc::UnboundedSender<()>>,
    network_stop_tx: Option<mpsc::UnboundedSender<transcription::TranscriptionCommand>>,
    transcript_pipeline: Arc<Mutex<TranscriptPipeline>>,
    task_abort_handles: Vec<tokio::task::AbortHandle>,
    finalization_watchdog: Option<tokio::task::AbortHandle>,
    _formatting_activity: post_processing::ActivityGuard,
}

impl Session {
    fn request_stop(&mut self) {
        // Capture shutdown and its resampler flush complete before this is
        // called. The provider drains its audio receiver before committing.
        if let Some(tx) = self.network_stop_tx.as_ref() {
            let _ = tx.send(transcription::TranscriptionCommand::Stop);
        }
    }

    fn abort_tasks(&mut self) {
        if let Some(tx) = self.audio_forward_stop_tx.take() {
            let _ = tx.send(());
        }
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

fn preferred_microphone(settings: &AppSettings) -> Option<String> {
    if settings.use_default_microphone {
        None
    } else {
        Some(settings.selected_microphone.clone())
    }
}

async fn reconfigure_warm_capture(
    warm_capture: &mut Option<audio::WarmAudioCapture>,
    settings: &AppSettings,
    level_sender: mpsc::Sender<f32>,
) -> Result<(), String> {
    if let Some(existing) = warm_capture.take() {
        let device_name = existing.device_name().to_string();
        match existing.shutdown().await {
            Ok(stats) => echo_info!(
                "audio",
                "Warm microphone stopped device={} pending_input_samples={} flushed_output_samples={}",
                device_name,
                stats.pending_input_samples,
                stats.flushed_output_samples
            ),
            Err(err) => echo_warn!(
                "audio",
                "Warm microphone shutdown failed device={}: {}",
                device_name,
                err
            ),
        }
    }

    if !settings.keep_microphone_ready {
        return Ok(());
    }

    let capture = audio::start_warm_audio_capture(level_sender, preferred_microphone(settings))
        .map_err(|err| err.to_string())?;
    echo_info!(
        "audio",
        "Warm microphone ready device={} pre_roll_ms=500",
        capture.device_name()
    );
    *warm_capture = Some(capture);
    Ok(())
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
struct InstanceActivationEvent(HANDLE);

#[cfg(target_os = "windows")]
impl InstanceActivationEvent {
    fn new() -> Result<Self, windows::core::Error> {
        // Auto-reset coalesces repeated launches. Create this before acquiring
        // the mutex so requests made during UI initialization remain pending.
        unsafe {
            CreateEventW(
                None,
                false,
                false,
                windows::core::w!("Local\\Echo-9D197E31-8816-4CB5-8753-3FD8664A9556-Activate"),
            )
            .map(Self)
        }
    }

    fn take_request(&self) -> bool {
        unsafe { WaitForSingleObject(self.0, 0) == WAIT_OBJECT_0 }
    }
}

#[cfg(target_os = "windows")]
impl Drop for InstanceActivationEvent {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

#[cfg(target_os = "windows")]
fn show_existing_instance(activation: &InstanceActivationEvent) {
    unsafe {
        let mut hwnd = FindWindowW(None, windows::core::w!("Echo"));
        if hwnd.0 == 0 {
            // A tray-only startup may not have created the main HWND yet.
            // The initialized overlay belongs to the same resident process.
            hwnd = FindWindowW(None, windows::core::w!("Echo Live"));
        }
        if hwnd.0 != 0 {
            let mut process_id = 0;
            GetWindowThreadProcessId(hwnd, Some(&mut process_id));
            if process_id != 0 {
                // The user-launched process can pass its foreground permission
                // to the resident process before asking its UI thread to show.
                if let Err(err) = AllowSetForegroundWindow(process_id) {
                    echo_warn!("instance", "Could not grant foreground permission: {err}");
                }
            }
        }
        if let Err(err) = SetEvent(activation.0) {
            echo_warn!("instance", "Could not request window activation: {err}");
        }
    }
}

#[cfg(target_os = "windows")]
fn show_main_window(ui: &AppWindow) {
    if let Err(err) = ui.show() {
        echo_warn!("instance", "Could not show main window: {err}");
        return;
    }
    unsafe {
        let hwnd = FindWindowW(None, windows::core::w!("Echo"));
        if hwnd.0 != 0 {
            // Preserve maximized windows; only minimized windows need restoring.
            let command = if IsIconic(hwnd).as_bool() {
                SW_RESTORE
            } else {
                SW_SHOW
            };
            let _ = ShowWindow(hwnd, command);
            let activated = SetForegroundWindow(hwnd).as_bool();
            echo_info!("instance", "Main window foreground activation={activated}");
        } else {
            echo_warn!(
                "instance",
                "Main window was unavailable for foreground activation"
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
    let characters = text.trim().chars().count().max(1);
    let lines = characters
        .div_ceil(OVERLAY_CHARACTERS_PER_LINE)
        .clamp(1, OVERLAY_MAX_LINES) as i32;
    (OVERLAY_WIDTH, 86 + lines * 20)
}

fn overlay_caption_text(text: &str) -> String {
    let trimmed = text.trim();
    let character_count = trimmed.chars().count();
    if character_count <= OVERLAY_CAPTION_CHARACTERS {
        return trimmed.to_string();
    }

    let mut tail = trimmed
        .chars()
        .skip(character_count - OVERLAY_CAPTION_CHARACTERS)
        .collect::<String>();
    if let Some(first_space) = tail.find(char::is_whitespace) {
        tail.drain(..=first_space);
    }
    format!("…{}", tail.trim_start())
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

fn set_overlay_height(overlay: &TranscriptOverlayWindow, height: i32) {
    let next_height = height.clamp(OVERLAY_HEIGHT, 86 + OVERLAY_MAX_LINES as i32 * 20);
    let previous_height = overlay.get_window_height();
    if previous_height == next_height {
        return;
    }

    #[cfg(target_os = "windows")]
    let previous_position = overlay.window().position();
    overlay.set_window_height(next_height);
    #[cfg(target_os = "windows")]
    {
        let scale = overlay.window().scale_factor();
        let previous_physical_height = (previous_height as f32 * scale).round() as i32;
        let next_physical_height = (next_height as f32 * scale).round() as i32;
        let proposed = slint::PhysicalPosition::new(
            previous_position.x,
            previous_position.y + previous_physical_height - next_physical_height,
        );
        overlay.window().set_position(clamp_overlay_position(
            proposed,
            (OVERLAY_WIDTH as f32 * scale).round() as i32,
            next_physical_height,
        ));

        let overlay_after_resize = overlay.as_weak();
        slint::Timer::single_shot(std::time::Duration::ZERO, move || {
            if let Some(overlay) = overlay_after_resize.upgrade() {
                if let Err(err) = apply_overlay_rounded_region(&overlay) {
                    echo_warn!("overlay", "Could not update rounded window region: {err}");
                }
            }
        });
    }
}

fn reset_overlay_to_listening(overlay: &TranscriptOverlayWindow, microphone_name: &str) {
    overlay.set_sentence_text("Listening...".into());
    overlay.set_microphone_name(microphone_name.into());
    overlay.set_window_width(OVERLAY_WIDTH);
    set_overlay_height(overlay, OVERLAY_HEIGHT);
    overlay.set_is_error(false);
    overlay.set_is_system_message(false);
    overlay.set_is_visible(true);
    show_overlay_without_activation(overlay);
}

/// Display the prepared live overlay through Slint's supported window API.
#[cfg(target_os = "windows")]
fn show_overlay_without_activation(overlay: &TranscriptOverlayWindow) {
    let foreground = unsafe { GetForegroundWindow() };
    let scale = overlay.window().scale_factor();
    overlay.window().set_position(clamp_overlay_position(
        overlay.window().position(),
        (OVERLAY_WIDTH as f32 * scale).round() as i32,
        (overlay.get_window_height() as f32 * scale).round() as i32,
    ));
    if let Err(err) = configure_overlay_as_non_activating(overlay) {
        echo_warn!("overlay", "Could not refresh non-activating style: {err}");
    }
    let _ = overlay.show();
    overlay.window().request_redraw();

    let overlay_after_show = overlay.as_weak();
    slint::Timer::single_shot(std::time::Duration::ZERO, move || {
        if let Some(overlay) = overlay_after_show.upgrade() {
            if let Err(err) = apply_overlay_rounded_region(&overlay) {
                echo_warn!("overlay", "Could not apply rounded window region: {err}");
            }
        }
        if foreground.0 != 0 {
            unsafe {
                let _ = SetForegroundWindow(foreground);
            }
        }
    });
}

#[cfg(not(target_os = "windows"))]
fn show_overlay_without_activation(overlay: &TranscriptOverlayWindow) {
    let _ = overlay.show();
    overlay.window().request_redraw();
}

fn hide_overlay_window(overlay: &TranscriptOverlayWindow) {
    let _ = overlay.hide();
}

#[cfg(target_os = "windows")]
fn clamp_overlay_position(
    position: slint::PhysicalPosition,
    width: i32,
    height: i32,
) -> slint::PhysicalPosition {
    unsafe {
        let monitor = MonitorFromPoint(
            POINT {
                x: position.x + width / 2,
                y: position.y + height / 2,
            },
            MONITOR_DEFAULTTONEAREST,
        );
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            let max_x = (info.rcWork.right - width).max(info.rcWork.left);
            let max_y = (info.rcWork.bottom - height).max(info.rcWork.top);
            return slint::PhysicalPosition::new(
                position.x.clamp(info.rcWork.left, max_x),
                position.y.clamp(info.rcWork.top, max_y),
            );
        }
    }
    position
}

#[cfg(target_os = "windows")]
fn overlay_hwnd() -> Result<HWND, Box<dyn std::error::Error>> {
    unsafe {
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
        Ok(hwnd)
    }
}

#[cfg(target_os = "windows")]
fn apply_overlay_rounded_region(
    _overlay: &TranscriptOverlayWindow,
) -> Result<(), Box<dyn std::error::Error>> {
    unsafe {
        let hwnd = overlay_hwnd()?;
        let mut rect = RECT::default();
        GetWindowRect(hwnd, &mut rect)?;
        let region = CreateRoundRectRgn(
            0,
            0,
            rect.right - rect.left + 1,
            rect.bottom - rect.top + 1,
            28,
            28,
        );
        if region.0 == 0 {
            return Err("Windows could not create the rounded overlay region".into());
        }
        if SetWindowRgn(hwnd, region, true) == 0 {
            let _ = DeleteObject(region);
            return Err("Windows could not apply the rounded overlay region".into());
        }
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn configure_overlay_as_non_activating(
    _overlay: &TranscriptOverlayWindow,
) -> Result<(), Box<dyn std::error::Error>> {
    unsafe {
        // Looking up the HWND directly avoids depending on optional raw-window-
        // handle support in the selected Slint renderer.
        let hwnd = overlay_hwnd()?;
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
    let overlay_h = OVERLAY_HEIGHT;
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
            // Transparent frameless windows need a complete composited frame.
            // Slint's winit backend falls back to the compiled software
            // renderer if FemtoVG cannot initialize on this machine.
            .renderer_name("femtovg".into())
            .select();
    }

    Ok(())
}

fn selected_microphone_for_snapshot(
    snapshot: &audio::InputDeviceSnapshot,
    current_selection: &str,
    follow_windows_default: bool,
) -> String {
    if !follow_windows_default {
        return current_selection.to_string();
    }

    snapshot
        .default_device
        .as_ref()
        .or_else(|| snapshot.devices.first())
        .cloned()
        .unwrap_or_default()
}

fn synchronize_microphone_editor(
    ui: &AppWindow,
    snapshot: &audio::InputDeviceSnapshot,
    replace_options: bool,
) {
    let current_selection = ui.get_selected_microphone().to_string();
    let next_selection = selected_microphone_for_snapshot(
        snapshot,
        &current_selection,
        ui.get_use_default_microphone(),
    );

    if replace_options {
        ui.set_microphone_options(ModelRc::new(VecModel::from(
            snapshot
                .devices
                .iter()
                .cloned()
                .map(SharedString::from)
                .collect::<Vec<_>>(),
        )));
    }

    if ui.get_selected_microphone().as_str() != next_selection.as_str() {
        ui.set_selected_microphone(next_selection.into());
    }
}

fn monitor_microphones(
    ui: slint::Weak<AppWindow>,
    initial_snapshot: audio::InputDeviceSnapshot,
    command_tx: mpsc::UnboundedSender<AppCommand>,
) {
    thread::spawn(move || {
        let mut previous_snapshot = initial_snapshot;
        loop {
            thread::sleep(std::time::Duration::from_millis(500));
            let snapshot = audio::input_device_snapshot();
            let replace_options = snapshot.devices != previous_snapshot.devices;
            let devices_changed =
                replace_options || snapshot.default_device != previous_snapshot.default_device;
            previous_snapshot = snapshot.clone();

            if ui
                .upgrade_in_event_loop(move |ui| {
                    synchronize_microphone_editor(&ui, &snapshot, replace_options);
                })
                .is_err()
            {
                break;
            }
            if devices_changed {
                let _ = command_tx.send(AppCommand::ReconfigureWarmCapture);
            }
        }
    });
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
    next.post_processing.enabled = ui.get_post_processing_enabled();
    next.post_processing.format_numbers = ui.get_post_format_numbers();
    next.post_processing.prefer_digits = ui.get_post_prefer_digits();
    next.post_processing.whole_numbers = ui.get_post_whole_numbers();
    next.post_processing.ordinals = ui.get_post_ordinals();
    next.post_processing.decimals_quantities = ui.get_post_decimals();
    next.post_processing.money = ui.get_post_money();
    next.post_processing.measurements = ui.get_post_measurements();
    next.post_processing.dates = ui.get_post_dates();
    next.post_processing.times = ui.get_post_times();
    next.post_processing.telephone_alphanumeric = ui.get_post_identifiers();
    next.post_processing.urls_emails = ui.get_post_addresses();
    next.post_processing.punctuation = ui.get_post_punctuation();
    next.post_processing.capitalization = ui.get_post_capitalization();
    next.post_processing.commas = ui.get_post_commas();
    next.post_processing.periods = ui.get_post_periods();
    next.post_processing.question_marks = ui.get_post_question_marks();
    next.post_processing.protected_phrases =
        post_processing_lines(ui.get_post_protected_phrases_text().as_ref());
    next.post_processing.custom_replacements =
        post_processing_lines(ui.get_post_custom_replacements_text().as_ref());
    next.selected_microphone = ui.get_selected_microphone().to_string();
    next.use_default_microphone = ui.get_use_default_microphone();
    next.keep_microphone_ready = ui.get_keep_microphone_ready();
    next.hotkey_text = ui.get_hotkey_text().to_string();
    next.start_with_windows = ui.get_start_with_windows();
    next.update_checks_enabled = ui.get_update_checks_enabled();
    next.local_sherpa = transcription::LocalSherpaConfig {
        model: transcription::LocalModel::from_id(&ui.get_local_model_text()),
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

fn post_processing_lines(value: &str) -> Vec<String> {
    value
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
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
    ui.set_local_model_text(settings.local_sherpa.model.label().into());
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
    ui.set_post_processing_enabled(settings.post_processing.enabled);
    ui.set_post_format_numbers(settings.post_processing.format_numbers);
    ui.set_post_prefer_digits(settings.post_processing.prefer_digits);
    ui.set_post_whole_numbers(settings.post_processing.whole_numbers);
    ui.set_post_ordinals(settings.post_processing.ordinals);
    ui.set_post_decimals(settings.post_processing.decimals_quantities);
    ui.set_post_money(settings.post_processing.money);
    ui.set_post_measurements(settings.post_processing.measurements);
    ui.set_post_dates(settings.post_processing.dates);
    ui.set_post_times(settings.post_processing.times);
    ui.set_post_identifiers(settings.post_processing.telephone_alphanumeric);
    ui.set_post_addresses(settings.post_processing.urls_emails);
    ui.set_post_punctuation(settings.post_processing.punctuation);
    ui.set_post_capitalization(settings.post_processing.capitalization);
    ui.set_post_commas(settings.post_processing.commas);
    ui.set_post_periods(settings.post_processing.periods);
    ui.set_post_question_marks(settings.post_processing.question_marks);
    ui.set_post_protected_phrases_text(
        settings.post_processing.protected_phrases.join("\n").into(),
    );
    ui.set_post_custom_replacements_text(
        settings
            .post_processing
            .custom_replacements
            .join("\n")
            .into(),
    );
    ui.set_selected_microphone(settings.selected_microphone.clone().into());
    ui.set_use_default_microphone(settings.use_default_microphone);
    ui.set_keep_microphone_ready(settings.keep_microphone_ready);
    ui.set_hotkey_text(settings.hotkey_text.clone().into());
    ui.set_start_with_windows(settings.start_with_windows);
    ui.set_update_checks_enabled(settings.update_checks_enabled);
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
    if draft.use_default_microphone && normalized_saved.use_default_microphone {
        normalized_saved.selected_microphone = draft.selected_microphone.clone();
    }
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
    let activation_event = InstanceActivationEvent::new()?;
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
            show_existing_instance(&activation_event);
            return Ok(());
        }
    };
    select_ui_backend()?;

    let microphone_snapshot = audio::input_device_snapshot();
    let mut initial_settings = load_settings();
    #[cfg(target_os = "windows")]
    match startup::is_enabled() {
        Ok(enabled) => initial_settings.start_with_windows = enabled,
        Err(err) => echo_warn!("startup", "Failed to read Windows startup setting: {err}"),
    }
    if initial_settings.use_default_microphone
        || initial_settings.selected_microphone.trim().is_empty()
    {
        initial_settings.selected_microphone = selected_microphone_for_snapshot(
            &microphone_snapshot,
            &initial_settings.selected_microphone,
            true,
        );
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
        transcription::local_models_available(&initial_settings.local_sherpa)
    );
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
    ) && transcription::local_models_available(&initial_settings.local_sherpa)
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
    ui.set_local_model_options(ModelRc::new(VecModel::from(vec![
        SharedString::from(transcription::LocalModel::ParakeetTdtV2.label()),
        SharedString::from(transcription::LocalModel::ParakeetTdtV3.label()),
    ])));
    ui.set_api_key_text(initial_settings.elevenlabs_api_key.clone().into());
    ui.set_elevenlabs_model_text(initial_settings.elevenlabs_model.clone().into());
    ui.set_elevenlabs_language_code_text(initial_settings.elevenlabs_language_code.clone().into());
    ui.set_elevenlabs_no_verbatim(initial_settings.elevenlabs_no_verbatim);
    ui.set_openai_api_key_text(initial_settings.openai_api_key.clone().into());
    ui.set_openai_model_text(initial_settings.openai_model.clone().into());
    ui.set_openai_language_code_text(initial_settings.openai_language_code.clone().into());
    ui.set_local_model_text(initial_settings.local_sherpa.model.label().into());
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
    ) && transcription::local_models_available(&initial_settings.local_sherpa)
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
    ui.set_microphone_options(ModelRc::new(VecModel::from(
        microphone_snapshot
            .devices
            .iter()
            .cloned()
            .map(SharedString::from)
            .collect::<Vec<SharedString>>(),
    )));
    ui.set_selected_microphone(selected_microphone.clone().into());
    ui.set_use_default_microphone(initial_settings.use_default_microphone);
    ui.set_keep_microphone_ready(initial_settings.keep_microphone_ready);
    ui.set_start_with_windows(initial_settings.start_with_windows);
    ui.set_update_checks_enabled(initial_settings.update_checks_enabled);
    ui.set_app_version(env!("CARGO_PKG_VERSION").into());
    let update_config = match updater::compiled_config() {
        Ok(config) => config,
        Err(err) => {
            echo_error!("updater", "Invalid compiled update configuration: {err}");
            ui.set_update_status(format!("Updates are unavailable: {err}").into());
            ui.set_update_state(4);
            None
        }
    };
    ui.set_updater_configured(update_config.is_some());
    if update_config.is_none() && ui.get_update_state() != 4 {
        ui.set_update_status("Updates are disabled in this development build.".into());
    }
    let available_update = Arc::new(Mutex::new(None::<updater::UpdateInfo>));
    let updater_busy = Arc::new(AtomicBool::new(false));

    let check_ui = ui.as_weak();
    let check_config = update_config.clone();
    let check_available = available_update.clone();
    let check_busy = updater_busy.clone();
    ui.on_check_for_updates(move || {
        if let Some(config) = check_config.clone() {
            begin_update_check(
                check_ui.clone(),
                config,
                check_available.clone(),
                check_busy.clone(),
                true,
            );
        } else if let Some(ui) = check_ui.upgrade() {
            ui.set_update_panel_visible(true);
            ui.set_update_state(4);
            ui.set_update_status("This build does not have an update feed configured.".into());
        }
    });

    let install_ui = ui.as_weak();
    let install_available = available_update.clone();
    let install_busy = updater_busy.clone();
    ui.on_install_update(move || {
        begin_update_install(
            install_ui.clone(),
            install_available.clone(),
            install_busy.clone(),
        );
    });

    if let Some(config) = update_config.clone() {
        let periodic_ui = ui.as_weak();
        let periodic_settings = settings.clone();
        let periodic_available = available_update.clone();
        let periodic_busy = updater_busy.clone();
        thread::spawn(move || loop {
            if periodic_settings.lock().unwrap().update_checks_enabled {
                begin_update_check(
                    periodic_ui.clone(),
                    config.clone(),
                    periodic_available.clone(),
                    periodic_busy.clone(),
                    false,
                );
            }
            thread::sleep(std::time::Duration::from_secs(6 * 60 * 60));
        });
    }
    monitor_microphones(ui.as_weak(), microphone_snapshot, cmd_tx.clone());

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
    ui.set_post_processing_enabled(initial_settings.post_processing.enabled);
    ui.set_post_format_numbers(initial_settings.post_processing.format_numbers);
    ui.set_post_prefer_digits(initial_settings.post_processing.prefer_digits);
    ui.set_post_whole_numbers(initial_settings.post_processing.whole_numbers);
    ui.set_post_ordinals(initial_settings.post_processing.ordinals);
    ui.set_post_decimals(initial_settings.post_processing.decimals_quantities);
    ui.set_post_money(initial_settings.post_processing.money);
    ui.set_post_measurements(initial_settings.post_processing.measurements);
    ui.set_post_dates(initial_settings.post_processing.dates);
    ui.set_post_times(initial_settings.post_processing.times);
    ui.set_post_identifiers(initial_settings.post_processing.telephone_alphanumeric);
    ui.set_post_addresses(initial_settings.post_processing.urls_emails);
    ui.set_post_punctuation(initial_settings.post_processing.punctuation);
    ui.set_post_capitalization(initial_settings.post_processing.capitalization);
    ui.set_post_commas(initial_settings.post_processing.commas);
    ui.set_post_periods(initial_settings.post_processing.periods);
    ui.set_post_question_marks(initial_settings.post_processing.question_marks);
    ui.set_post_protected_phrases_text(
        initial_settings
            .post_processing
            .protected_phrases
            .join("\n")
            .into(),
    );
    ui.set_post_custom_replacements_text(
        initial_settings
            .post_processing
            .custom_replacements
            .join("\n")
            .into(),
    );

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
    let initial_activity = diagnostics::session_snapshot();
    let initial_activity_revision = initial_activity.revision;
    ui.set_activity_log_text(initial_activity.text.into());

    // When the user closes the main window, hide it but keep the Slint
    // event loop alive so the app can continue running from the tray.
    let ui_weak_for_close = ui.as_weak();
    let settings_for_close = settings.clone();
    ui.window().on_close_requested(move || {
        if let Some(ui) = ui_weak_for_close.upgrade() {
            let modal_active = ui.get_local_download_prompt_visible()
                || ui.get_local_download_progress_visible()
                || ui.get_post_model_download_prompt_visible()
                || ui.get_post_model_installing()
                || ui.get_update_panel_visible();
            if modal_active {
                ui.set_status_text("Finish or dismiss the open dialog before closing".into());
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
    transcript_overlay.set_microphone_name(selected_microphone.clone().into());
    transcript_overlay.set_window_width(OVERLAY_WIDTH);
    transcript_overlay.set_window_height(OVERLAY_HEIGHT);
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
            let proposed_position = slint::PhysicalPosition::new(
                current.x + (dx as f32 * scale) as i32,
                current.y + (dy as f32 * scale) as i32,
            );
            #[cfg(target_os = "windows")]
            let new_position = clamp_overlay_position(
                proposed_position,
                (OVERLAY_WIDTH as f32 * scale).round() as i32,
                (overlay.get_window_height() as f32 * scale).round() as i32,
            );
            #[cfg(not(target_os = "windows"))]
            let new_position = proposed_position;
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

    let transcript_history_for_runtime = transcript_history_store.clone();
    let transcript_history_revision_for_runtime = transcript_history_revision.clone();
    let transcript_raw_for_runtime = transcript_raw_for_clipboard.clone();
    thread::spawn(move || {
        let rt = Runtime::new().unwrap();
        rt.block_on(async move {
            echo_info!("runtime", "Tokio runtime active");

            let mut active_session: Option<Session> = None;
            let mut warm_capture: Option<audio::WarmAudioCapture> = None;
            let mut warm_reconfigure_pending = false;
            let (finalize_tx, mut finalize_rx) = mpsc::unbounded_channel::<(u64, bool)>();
            let overlay_visible = Arc::new(AtomicBool::new(false));
            let session_epoch = Arc::new(AtomicU64::new(0));

            let startup_settings = settings_for_runtime.lock().unwrap().clone();
            if let Err(err) = reconfigure_warm_capture(
                &mut warm_capture,
                &startup_settings,
                level_tx.clone(),
            )
            .await
            {
                echo_warn!("audio", "Warm microphone startup failed: {}", err);
                let _ = ui_handle_for_tokio.upgrade_in_event_loop(move |ui| {
                    ui.set_status_text(
                        "Warm microphone unavailable; instant capture will retry".into(),
                    );
                });
            }

            loop {
                tokio::select! {
                    Some(level) = level_rx.recv() => {
                        let _ = ui_handle_for_tokio.upgrade_in_event_loop(move |ui| {
                            ui.set_audio_level(level);
                        });
                        let _ = overlay_handle_for_tokio.upgrade_in_event_loop(move |overlay| {
                            overlay.set_audio_level(level);
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
                        if let Some(session) = active_session.as_mut() {
                            if session.warm_capture_attached {
                                if let Some(capture) = warm_capture.as_ref() {
                                    if let Err(err) = capture.detach(session.epoch).await {
                                        echo_warn!("audio", "Warm microphone detach during finalization failed epoch={}: {}", session.epoch, err);
                                    }
                                }
                                session.warm_capture_attached = false;
                            }
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
                        if warm_reconfigure_pending {
                            warm_reconfigure_pending = false;
                            let current_settings = settings_for_runtime.lock().unwrap().clone();
                            if let Err(err) = reconfigure_warm_capture(
                                &mut warm_capture,
                                &current_settings,
                                level_tx.clone(),
                            )
                            .await
                            {
                                echo_warn!("audio", "Deferred warm microphone reconfiguration failed: {}", err);
                            }
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
                            overlay.set_audio_level(0.0);
                            overlay.set_window_width(OVERLAY_WIDTH);
                            set_overlay_height(&overlay, OVERLAY_HEIGHT);
                            overlay.set_is_error(false);
                            overlay.set_is_system_message(false);
                            overlay.set_is_visible(false);
                            hide_overlay_window(&overlay);
                        });
                    }
                    Some(cmd) = cmd_rx.recv() => {
                    echo_info!(
                        "command",
                        "Received command={} active_epoch={}",
                        app_command_name(&cmd),
                        active_session.as_ref().map_or(0, |session| session.epoch)
                    );
                    let cmd = match cmd {
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
                            let Some(formatting_activity) = post_processing::ActivityGuard::acquire() else {
                                let _ = ui_handle_for_tokio.upgrade_in_event_loop(|ui| {
                                    ui.set_status_text("Finish model installation before recording.".into());
                                });
                                continue;
                            };
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

                            if matches!(
                                provider,
                                transcription::TranscriptionProvider::LocalSherpaOnnx
                            ) && !transcription::local_models_available(&current_settings.local_sherpa)
                            {
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

                            let current_session_epoch =
                                session_epoch.fetch_add(1, Ordering::SeqCst) + 1;

                            let preferred_device = preferred_microphone(&current_settings);

                            let state = Arc::new(Mutex::new(RecordingState::BufferingPreConnect));
                            let transcript_pipeline = Arc::new(Mutex::new(TranscriptPipeline::new()));
                            let (audio_tx, audio_rx) = mpsc::channel::<Vec<i16>>(50);
                            let (network_stop_tx, network_stop_rx) =
                                mpsc::unbounded_channel::<transcription::TranscriptionCommand>();
                            let (text_tx, mut text_rx) =
                                mpsc::channel::<transcription::TranscriptionEvent>(100);
                            let (log_line_tx, mut log_line_rx) =
                                mpsc::unbounded_channel::<String>();
                            let audio_level_tx = level_tx.clone();

                            if current_settings.keep_microphone_ready && warm_capture.is_none() {
                                if let Err(err) = reconfigure_warm_capture(
                                    &mut warm_capture,
                                    &current_settings,
                                    level_tx.clone(),
                                )
                                .await
                                {
                                    echo_warn!(
                                        "audio",
                                        "Warm microphone retry failed before epoch={}: {}",
                                        current_session_epoch,
                                        err
                                    );
                                }
                            }

                            let capture_result: Result<
                                (Option<audio::AudioCapture>, bool, String),
                                String,
                            > = if current_settings.keep_microphone_ready {
                                if let Some(capture) = warm_capture.as_ref() {
                                    match capture.attach(current_session_epoch, audio_tx.clone()).await {
                                        Ok(pre_roll_samples) => {
                                            echo_info!(
                                                "audio",
                                                "Warm microphone attached epoch={} pre_roll_samples={}",
                                                current_session_epoch,
                                                pre_roll_samples
                                            );
                                            Ok((None, true, capture.device_name().to_string()))
                                        }
                                        Err(err) => {
                                            warm_reconfigure_pending = true;
                                            echo_warn!(
                                                "audio",
                                                "Warm microphone attach failed epoch={}; falling back to on-demand capture: {}",
                                                current_session_epoch,
                                                err
                                            );
                                            audio::start_audio_capture(
                                                audio_tx,
                                                audio_level_tx,
                                                preferred_device,
                                            )
                                            .map(|capture| {
                                                let name = capture.device_name().to_string();
                                                (Some(capture), false, name)
                                            })
                                            .map_err(|err| err.to_string())
                                        }
                                    }
                                } else {
                                    audio::start_audio_capture(
                                        audio_tx,
                                        audio_level_tx,
                                        preferred_device,
                                    )
                                    .map(|capture| {
                                        let name = capture.device_name().to_string();
                                        (Some(capture), false, name)
                                    })
                                    .map_err(|err| err.to_string())
                                }
                            } else {
                                audio::start_audio_capture(
                                    audio_tx,
                                    audio_level_tx,
                                    preferred_device,
                                )
                                .map(|capture| {
                                    let name = capture.device_name().to_string();
                                    (Some(capture), false, name)
                                })
                                .map_err(|err| err.to_string())
                            };

                            match capture_result {
                                Ok((capture, warm_capture_attached, microphone_name)) => {
                                    let injection_target = injector::capture_injection_target();
                                    echo_info!(
                                        "audio",
                                        "Capture path ready epoch={} warm={} before provider and overlay initialization",
                                        current_session_epoch,
                                        warm_capture_attached,
                                    );
                                    echo_info!(
                                        "session",
                                        "Starting epoch={} provider={} use_default_microphone={} gemini_enabled={}",
                                        current_session_epoch,
                                        provider.id(),
                                        current_settings.use_default_microphone,
                                        current_settings.gemini_enabled
                                    );
                                    if matches!(
                                        provider,
                                        transcription::TranscriptionProvider::LocalSherpaOnnx
                                    ) && !matches!(
                                        transcription::local_engine_status(),
                                        transcription::LocalEngineStatus::Ready
                                    ) {
                                        echo_info!(
                                            "local_model",
                                            "Local model preload requested after capture started"
                                        );
                                        transcription::preload_local_engine(
                                            &current_settings.local_sherpa,
                                        );
                                    }

                                    let _ = ui_handle_for_tokio.upgrade_in_event_loop(|ui| {
                                        ui.set_status_text("Connecting...".into());
                                        ui.set_has_error(false);
                                        ui.set_is_finalizing(false);
                                        ui.set_transcript("".into());
                                    });
                                    overlay_visible.store(true, Ordering::SeqCst);
                                    let _ = overlay_handle_for_tokio.upgrade_in_event_loop(
                                        move |overlay| {
                                            reset_overlay_to_listening(
                                                &overlay,
                                                &microphone_name,
                                            );
                                        },
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
                                    let log_line_tx_for_text = log_line_tx.clone();
                                    let settings_for_text = settings_for_runtime.clone();
                                    let finalize_tx_for_transcript = finalize_tx.clone();
                                    let ui_handle_for_network = ui_handle_for_tokio.clone();
                                    let ui_handle_for_transcript = ui_handle_for_tokio.clone();
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
                                        while let Some(line) = log_line_rx.recv().await {
                                            diagnostics::record(
                                                "INFO",
                                                "provider_event",
                                                format_args!(
                                                    "epoch={} {}",
                                                    current_session_epoch,
                                                    line
                                                ),
                                            );
                                        }
                                    });

                                    let transcript_task = tokio::spawn(async move {
                                        let mut latest_partial = String::new();
                                        let mut had_error = false;
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

                                                    let aggregated = {
                                                        let mut pipeline = transcript_pipeline_for_text.lock().unwrap();
                                                        stop_requested_for_msg = pipeline.stop_requested();
                                                        pipeline.push_fragment(&base_text)
                                                    };
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
                                                    had_error = true;
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

                                            let aggregated_for_overlay =
                                                overlay_caption_text(&display_text);
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
                                                        overlay.set_window_width(OVERLAY_WIDTH);
                                                        set_overlay_height(&overlay, OVERLAY_HEIGHT);
                                                        overlay.set_is_error(false);
                                                        overlay.set_is_system_message(false);
                                                        overlay.set_is_visible(false);
                                                        overlay_visible_setter.store(false, Ordering::SeqCst);
                                                        hide_overlay_window(&overlay);
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
                                                        set_overlay_height(&overlay, h);
                                                        overlay.set_is_visible(true);
                                                        overlay_visible_setter.store(true, Ordering::SeqCst);
                                                        show_overlay_without_activation(&overlay);
                                                    }
                                                });

                                        }

                                        let (base_text, stopped) = {
                                            let mut pipeline = transcript_pipeline_for_text.lock().unwrap();
                                            if !latest_partial.trim().is_empty() && pipeline.stop_requested() {
                                                pipeline.push_fragment(&latest_partial);
                                            }
                                            (pipeline.committed_text().to_string(), pipeline.stop_requested())
                                        };
                                        if stopped && !had_error && !base_text.trim().is_empty()
                                            && session_epoch_for_transcript.load(Ordering::SeqCst) == current_session_epoch
                                        {
                                            let snapshot = settings_for_text.lock().unwrap().clone();
                                            let rewritten = if snapshot.gemini_enabled {
                                                gemini::rewrite_text(&snapshot.gemini_api_key, &snapshot.gemini_model,
                                                    &snapshot.gemini_prompt_preset, &snapshot.gemini_custom_prompt, &base_text).await
                                            } else { base_text };
                                            let raw_fallback = rewritten.clone();
                                            let pp = settings_for_text.lock().unwrap().post_processing.clone();
                                            let worker_settings = pp.clone();
                                            let result = tokio::task::spawn_blocking(move || post_processing::process(&rewritten, &worker_settings)).await;
                                            let mut processed = result.unwrap_or_else(|_| post_processing::ProcessedText {
                                                text: raw_fallback.clone(), warning: Some("Formatting worker failed; original text retained.".into())
                                            });
                                            if session_epoch_for_transcript.load(Ordering::SeqCst) != current_session_epoch { return; }
                                            if settings_for_text.lock().unwrap().post_processing != pp {
                                                processed = post_processing::ProcessedText { text: raw_fallback,
                                                    warning: Some("Settings changed while formatting; original text retained.".into()) };
                                            }
                                            let final_text = processed.text.trim().to_string();
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

                                                    {
                                                        let final_payload = final_text.clone();
                                                        if !final_payload.is_empty() {
                                                            echo_info!(
                                                                "injection",
                                                                "Posting requested epoch={} characters={}",
                                                                current_session_epoch,
                                                                final_payload.chars().count()
                                                            );
                                                            let to_inject = format!("{} ", final_payload);
                                                            if session_epoch_for_transcript.load(Ordering::SeqCst) != current_session_epoch { return; }
                                                            match injector::inject_text(
                                                                &to_inject,
                                                                injection_target,
                                                            ) {
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

                                            if let Some(warning) = processed.warning {
                                                preserve_status_for_transcript.store(true, Ordering::SeqCst);
                                                let _ = ui_handle_for_transcript.upgrade_in_event_loop(move |ui| {
                                                    ui.set_post_model_status(warning.clone().into());
                                                    ui.set_status_text(format!("Post-processing: {warning}").into());
                                                    ui.set_has_error(true);
                                                });
                                            }
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
                                        audio_capture: capture,
                                        warm_capture_attached,
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
                                        _formatting_activity: formatting_activity,
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
                                        overlay.set_window_width(OVERLAY_WIDTH);
                                        set_overlay_height(&overlay, OVERLAY_HEIGHT);
                                        overlay.set_is_system_message(false);
                                        overlay.set_is_visible(false);
                                        hide_overlay_window(&overlay);
                                    });
                                    }
                                    }
                                    }
                                    AppCommand::ReconfigureWarmCapture => {
                                    if active_session.is_some() {
                                        warm_reconfigure_pending = true;
                                        echo_info!(
                                            "audio",
                                            "Warm microphone reconfiguration deferred until the active session finishes"
                                        );
                                        continue;
                                    }
                                    let current_settings = settings_for_runtime.lock().unwrap().clone();
                                    if let Err(err) = reconfigure_warm_capture(
                                        &mut warm_capture,
                                        &current_settings,
                                        level_tx.clone(),
                                    )
                                    .await
                                    {
                                        echo_warn!("audio", "Warm microphone reconfiguration failed: {}", err);
                                        let _ = ui_handle_for_tokio.upgrade_in_event_loop(move |ui| {
                                            ui.set_status_text(
                                                "Warm microphone unavailable; using normal startup".into(),
                                            );
                                        });
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

                                    if let Ok(mut pipeline) = session.transcript_pipeline.lock() {
                                    pipeline.request_stop();
                                    }
                                    let timed_out_epoch = session.epoch;
                                    let post_roll_tx = cmd_tx_for_runtime.clone();
                                    tokio::spawn(async move {
                                        tokio::time::sleep(CAPTURE_POST_ROLL).await;
                                        let _ = post_roll_tx.send(
                                            AppCommand::CapturePostRollElapsed(timed_out_epoch)
                                        );
                                    });
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
                                    AppCommand::CapturePostRollElapsed(capture_epoch) => {
                                    let Some(session) = active_session.as_mut() else {
                                        continue;
                                    };
                                    if session.epoch != capture_epoch {
                                        continue;
                                    }
                                    if session.warm_capture_attached {
                                        echo_info!(
                                            "audio",
                                            "Post-roll complete epoch={} post_roll_ms={}; detaching warm capture",
                                            capture_epoch,
                                            CAPTURE_POST_ROLL.as_millis()
                                        );
                                        let result = if let Some(capture) = warm_capture.as_ref() {
                                            capture.detach(capture_epoch).await
                                        } else {
                                            Err("Warm microphone became unavailable during recording".to_string())
                                        };
                                        session.warm_capture_attached = false;
                                        if let Err(err) = result {
                                            echo_warn!(
                                                "audio",
                                                "Warm microphone detach failed epoch={}: {}",
                                                capture_epoch,
                                                err
                                            );
                                        }
                                        session.request_stop();
                                        continue;
                                    }
                                    let Some(capture) = session.audio_capture.take() else {
                                        echo_error!(
                                            "audio",
                                            "Session epoch={} has no capture source during post-roll",
                                            capture_epoch
                                        );
                                        session.request_stop();
                                        continue;
                                    };
                                    echo_info!(
                                        "audio",
                                        "Post-roll complete epoch={} post_roll_ms={}; stopping capture",
                                        capture_epoch,
                                        CAPTURE_POST_ROLL.as_millis()
                                    );
                                    let worker = capture.begin_shutdown();
                                    let flushed_tx = cmd_tx_for_runtime.clone();
                                    tokio::spawn(async move {
                                        let result = tokio::task::spawn_blocking(move || {
                                            worker.join().map_err(|_| {
                                                "Audio conversion worker panicked during shutdown"
                                                    .to_string()
                                            })
                                        })
                                        .await
                                        .map_err(|err| err.to_string())
                                        .and_then(|result| result);
                                        let _ = flushed_tx.send(
                                            AppCommand::CaptureFlushed(capture_epoch, result)
                                        );
                                    });
                                    }
                                    AppCommand::CaptureFlushed(capture_epoch, result) => {
                                    let Some(session) = active_session.as_mut() else {
                                        continue;
                                    };
                                    if session.epoch != capture_epoch {
                                        continue;
                                    }
                                    match result {
                                        Ok(stats) => echo_info!(
                                            "audio",
                                            "Capture flush complete epoch={} pending_input_samples={} flushed_output_samples={}",
                                            capture_epoch,
                                            stats.pending_input_samples,
                                            stats.flushed_output_samples
                                        ),
                                        Err(err) => echo_error!(
                                            "audio",
                                            "Capture flush failed epoch={}: {}",
                                            capture_epoch,
                                            err
                                        ),
                                    }
                                    session.request_stop();
                                    }
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
                                    if let Some(session) = active_session.as_mut() {
                                        if session.warm_capture_attached {
                                            if let Some(capture) = warm_capture.as_ref() {
                                                let _ = capture.detach(session.epoch).await;
                                            }
                                            session.warm_capture_attached = false;
                                        }
                                    }
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
                                        hide_overlay_window(&overlay);
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
                || ui.get_update_panel_visible()
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
    let warm_capture_reconfigure_tx = cmd_tx.clone();
    ui.on_apply_settings(move || {
        let Some(editor) = ui_weak_for_apply.upgrade() else { return; };
        let draft = settings_snapshot_from_ui(&editor, &settings_for_ui.lock().unwrap());
        if let Err(error) = post_processing::validate(&draft.post_processing) {
            pending_navigation_for_apply.set(None);
            editor.set_status_text(format!("Post-processing settings: {error}").into());
            editor.set_post_model_status(format!("Settings not saved: {error}").into());
            editor.set_has_error(true);
            editor.set_settings_dirty(true);
            editor.set_active_tab(3);
            editor.set_settings_tab(3);
            return;
        }
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
            keep_microphone_ready,
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
            local_model,
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
                ui.get_keep_microphone_ready(),
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
                transcription::LocalModel::from_id(&ui.get_local_model_text()),
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
                transcription::LocalModel::default(),
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
            next.keep_microphone_ready = keep_microphone_ready;
            next.start_with_windows = start_with_windows;
            next.local_sherpa = transcription::LocalSherpaConfig {
                model: local_model,
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
            snapshot.post_processing = draft.post_processing;
            snapshot.hotkey_text = ui.get_hotkey_text().to_string();
            snapshot.update_checks_enabled = ui.get_update_checks_enabled();
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
            let _ = warm_capture_reconfigure_tx.send(AppCommand::ReconfigureWarmCapture);
            let local_selected = matches!(
                transcription::TranscriptionProvider::from_id(&snapshot.transcription_provider),
                transcription::TranscriptionProvider::LocalSherpaOnnx
            );
            if local_selected && transcription::local_models_available(&snapshot.local_sherpa) {
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
            ) && transcription::local_models_available(&saved.local_sherpa)
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
                local_config.model,
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

    let has_punctuation_model = post_processing::punctuation_model_available();
    ui.set_post_model_ready(has_punctuation_model);
    ui.set_post_model_status(
        if has_punctuation_model {
            "Checking offline punctuation model…"
        } else {
            "Offline punctuation model not installed"
        }
        .into(),
    );
    if initial_settings.post_processing.enabled
        && initial_settings.post_processing.punctuation
        && !has_punctuation_model
    {
        ui.set_post_model_download_prompt_visible(true);
    }
    if has_punctuation_model {
        let ready_ui = ui.as_weak();
        thread::spawn(move || {
            let result = post_processing::preload();
            let _ = ready_ui.upgrade_in_event_loop(move |ui| {
                ui.set_post_model_ready(result.is_ok());
                match result {
                    Ok(()) => ui.set_post_model_status("Offline punctuation model ready".into()),
                    Err(error) => {
                        ui.set_post_model_status(error.into());
                        if ui.get_post_processing_enabled() && ui.get_post_punctuation() {
                            ui.set_post_model_download_prompt_visible(true);
                            let _ = ui.show();
                        }
                    }
                }
            });
        });
    }
    let enable_ui = ui.as_weak();
    ui.on_post_processing_changed(move || {
        if let Some(ui) = enable_ui.upgrade() {
            if ui.get_post_processing_enabled()
                && ui.get_post_punctuation()
                && !ui.get_post_model_ready()
            {
                ui.set_post_model_download_prompt_visible(true);
            }
        }
    });
    let post_download_ui = ui.as_weak();
    ui.on_download_post_processing_model(move || {
        let Some(ui) = post_download_ui.upgrade() else {
            return;
        };
        let Some(activity) = post_processing::ActivityGuard::acquire() else {
            ui.set_post_model_status(
                "Stop transcription and wait for it to finish before installing.".into(),
            );
            return;
        };
        ui.set_post_model_download_prompt_visible(true);
        ui.set_post_model_installing(true);
        ui.set_post_model_ready(false);
        ui.set_post_model_progress(0.0);
        ui.set_post_model_status("Preparing punctuation model installation…".into());
        let progress_ui = post_download_ui.clone();
        let completion_ui = post_download_ui.clone();
        thread::spawn(move || {
            let _activity = activity;
            let result = Runtime::new()
                .map_err(|err| err.to_string())
                .and_then(|runtime| {
                    runtime.block_on(post_processing::download_punctuation_model(
                        move |fraction, status| {
                            let _ = progress_ui.upgrade_in_event_loop(move |ui| {
                                ui.set_post_model_progress(fraction);
                                ui.set_post_model_status(status.into());
                            });
                        },
                    ))
                });
            let _ = completion_ui.upgrade_in_event_loop(move |ui| {
                ui.set_post_model_installing(false);
                ui.set_post_model_download_prompt_visible(true);
                ui.set_post_model_ready(result.is_ok());
                match result {
                    Ok(()) => {
                        ui.set_post_model_progress(1.0);
                        ui.set_post_model_status("Installed, tested, and ready to use.".into());
                    }
                    Err(err) => ui.set_post_model_status(
                        format!("Installation failed: {err} You can retry.").into(),
                    ),
                }
            });
        });
    });
    let post_decline_ui = ui.as_weak();
    ui.on_decline_post_processing_model_download(move || {
        if let Some(ui) = post_decline_ui.upgrade() {
            if !ui.get_post_model_ready() {
                ui.set_post_model_status("Installation postponed. Written rules work; original punctuation will be retained.".into());
            }
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
        if !save_transcript_history(&[]) {
            echo_error!("history", "Failed to clear persisted transcript history");
            if let Some(ui) = ui_weak_for_clear.upgrade() {
                ui.set_has_error(true);
                ui.set_status_text(
                    "Could not clear transcript history; the existing history was kept".into(),
                );
            }
            return;
        }
        {
            let mut history = transcript_history_for_clear.lock().unwrap();
            history.clear();
            transcript_raw_for_clear.lock().unwrap().clear();
            transcript_history_revision_for_clear.fetch_add(1, Ordering::SeqCst);
        }
        if let Some(ui) = ui_weak_for_clear.upgrade() {
            ui.set_has_error(false);
            ui.set_status_text("Transcript history cleared".into());
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

    let activity_revision_seen = Rc::new(Cell::new(initial_activity_revision));

    let activity_revision_for_timer = activity_revision_seen.clone();
    let ui_weak_for_activity_timer = ui.as_weak();
    let activity_timer = slint::Timer::default();
    activity_timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(250),
        move || {
            let Some(ui) = ui_weak_for_activity_timer.upgrade() else {
                return;
            };
            if ui.get_active_tab() != 2 {
                return;
            }
            let revision = diagnostics::session_revision();
            if revision == activity_revision_for_timer.get() {
                return;
            }
            let snapshot = diagnostics::session_snapshot();
            activity_revision_for_timer.set(snapshot.revision);
            ui.set_activity_log_text(snapshot.text.into());
        },
    );

    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(50),
        move || {
            if let Some(ui) = ui_handle_for_timer.upgrade() {
                #[cfg(target_os = "windows")]
                if activation_event.take_request() {
                    show_main_window(&ui);
                }
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
                    && !transcription::local_models_available(
                        &settings.lock().unwrap().local_sherpa,
                    )
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
                                show_main_window(&ui);
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
                            show_main_window(&ui);
                        }
                    }
                }
            }
        },
    );

    let started_by_windows =
        cfg!(target_os = "windows") && std::env::args_os().skip(1).any(|arg| arg == "--startup");
    if !started_by_windows || ui.get_post_model_download_prompt_visible() {
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
                        hide_overlay_window(&overlay);
                    }
                    if !started_by_windows {
                        if let Some(ui) = ui_for_native_init.upgrade() {
                            show_main_window(&ui);
                        }
                    }
                });
            } else if !started_by_windows {
                if let Some(ui) = ui_for_native_init.upgrade() {
                    show_main_window(&ui);
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
    use super::{
        forward_audio_until_stopped, overlay_caption_text, overlay_size_for_text, parse_hotkey,
        selected_microphone_for_snapshot, OVERLAY_CAPTION_CHARACTERS, OVERLAY_HEIGHT,
        OVERLAY_MAX_LINES,
    };
    use crate::audio::InputDeviceSnapshot;
    use std::time::Duration;
    use tokio::sync::mpsc;

    #[test]
    #[ignore = "opens an isolated settings window; no recording, network, or user settings writes"]
    fn post_processing_settings_ui_smoke() {
        use slint::ComponentHandle;
        let ui = super::AppWindow::new().unwrap();
        let mut settings = super::AppSettings::default();
        let mut pp = serde_json::to_value(&settings.post_processing).unwrap();
        for field in pp.as_object_mut().unwrap().values_mut() {
            if field.is_boolean() {
                *field = serde_json::Value::Bool(false);
            }
        }
        settings.post_processing = serde_json::from_value(pp).unwrap();
        settings.post_processing.protected_phrases = vec!["GPT-4".into()];
        settings.post_processing.custom_replacements = vec!["echo app => Echo".into()];
        super::populate_settings_editor(&ui, &settings);
        assert_eq!(
            super::settings_snapshot_from_ui(&ui, &settings).post_processing,
            settings.post_processing
        );
        settings.post_processing = Default::default();
        super::populate_settings_editor(&ui, &settings);
        assert_eq!(
            super::settings_snapshot_from_ui(&ui, &settings).post_processing,
            settings.post_processing
        );
        ui.set_active_tab(3);
        ui.set_settings_tab(3);
        ui.set_post_model_status("Offline punctuation model ready (isolated UI test)".into());
        ui.window().set_size(slint::PhysicalSize::new(1000, 1050));
        ui.show().unwrap();
        let weak = ui.as_weak();
        slint::Timer::single_shot(Duration::from_millis(500), move || {
            let ui = weak.upgrade().unwrap();
            for tab in 0..=5 {
                ui.set_settings_tab(tab);
                let pixels = ui.window().take_snapshot().unwrap();
                assert!(pixels.width() >= 760 && pixels.height() >= 600);
                if tab == 3 {
                    save_ui_snapshot(&pixels, "echo-post-processing-ui.bmp");
                }
            }
            ui.set_settings_tab(3);
            let before_hover = ui.window().take_snapshot().unwrap();
            assert!(
                before_hover.as_slice().iter().any(|p| p.r > 100),
                "snapshot must contain rendered content; use SLINT_BACKEND=winit-software"
            );
            ui.window()
                .dispatch_event(slint::platform::WindowEvent::PointerMoved {
                    position: slint::LogicalPosition::new(908.0, 365.0),
                });
            save_ui_snapshot(
                &ui.window().take_snapshot().unwrap(),
                "echo-post-processing-help-ui.bmp",
            );
            ui.window()
                .dispatch_event(slint::platform::WindowEvent::PointerExited);
            ui.window()
                .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
                    position: slint::LogicalPosition::new(500.0, 550.0),
                    delta_x: 0.0,
                    delta_y: -700.0,
                });
            save_ui_snapshot(
                &ui.window().take_snapshot().unwrap(),
                "echo-post-processing-lower-ui.bmp",
            );
            ui.set_post_model_download_prompt_visible(true);
            ui.set_post_model_installing(true);
            ui.set_post_model_progress(0.93);
            ui.set_post_model_status("Loading model and testing punctuation…".into());
            save_ui_snapshot(
                &ui.window().take_snapshot().unwrap(),
                "echo-post-processing-install-ui.bmp",
            );
            ui.set_post_model_installing(false);
            ui.set_post_model_ready(true);
            ui.set_post_model_progress(1.0);
            ui.set_post_model_status("Installed, tested, and ready to use.".into());
            save_ui_snapshot(
                &ui.window().take_snapshot().unwrap(),
                "echo-post-processing-ready-ui.bmp",
            );
            ui.hide().unwrap();
            slint::quit_event_loop().unwrap();
        });
        slint::run_event_loop().unwrap();
    }

    fn save_ui_snapshot(pixels: &slint::SharedPixelBuffer<slint::Rgba8Pixel>, name: &str) {
        // Small dependency-free BMP encoder for visual smoke-test artifacts.
        let (width, height) = (pixels.width(), pixels.height());
        let stride = (width * 3 + 3) & !3;
        let mut bytes = vec![0u8; (54 + stride * height) as usize];
        bytes[..2].copy_from_slice(b"BM");
        let length = bytes.len() as u32;
        bytes[2..6].copy_from_slice(&length.to_le_bytes());
        bytes[10..14].copy_from_slice(&54u32.to_le_bytes());
        bytes[14..18].copy_from_slice(&40u32.to_le_bytes());
        bytes[18..22].copy_from_slice(&width.to_le_bytes());
        bytes[22..26].copy_from_slice(&height.to_le_bytes());
        bytes[26..28].copy_from_slice(&1u16.to_le_bytes());
        bytes[28..30].copy_from_slice(&24u16.to_le_bytes());
        for y in 0..height {
            for x in 0..width {
                let pixel = pixels.as_slice()[(y * width + x) as usize];
                let offset = (54 + (height - 1 - y) * stride + x * 3) as usize;
                bytes[offset..offset + 3].copy_from_slice(&[pixel.b, pixel.g, pixel.r]);
            }
        }
        let path = std::env::temp_dir().join(name);
        std::fs::write(&path, bytes).unwrap();
        eprintln!("UI snapshot: {}", path.display());
    }

    #[test]
    fn instance_activation_waits_for_ui_and_is_consumed_once() {
        // Use an unnamed event so this test cannot activate a running Echo.
        let event = super::InstanceActivationEvent(unsafe {
            super::CreateEventW(None, false, false, None).unwrap()
        });
        assert!(!event.take_request());
        unsafe { super::SetEvent(event.0).unwrap() };
        assert!(event.take_request());
        assert!(!event.take_request());
    }

    #[test]
    fn following_windows_uses_the_latest_default_microphone() {
        let initial = InputDeviceSnapshot {
            devices: vec!["Desk Mic".to_string(), "Headset".to_string()],
            default_device: Some("Desk Mic".to_string()),
        };
        let changed = InputDeviceSnapshot {
            devices: initial.devices.clone(),
            default_device: Some("Headset".to_string()),
        };

        assert_eq!(
            selected_microphone_for_snapshot(&initial, "Old Mic", true),
            "Desk Mic"
        );
        assert_eq!(
            selected_microphone_for_snapshot(&changed, "Desk Mic", true),
            "Headset"
        );
    }

    #[test]
    fn manual_microphone_selection_is_not_replaced_by_windows() {
        let snapshot = InputDeviceSnapshot {
            devices: vec!["Desk Mic".to_string(), "Headset".to_string()],
            default_device: Some("Headset".to_string()),
        };

        assert_eq!(
            selected_microphone_for_snapshot(&snapshot, "Desk Mic", false),
            "Desk Mic"
        );
    }

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

    #[test]
    fn overlay_keeps_short_captions_intact() {
        assert_eq!(overlay_caption_text("  hello world  "), "hello world");
    }

    #[test]
    fn overlay_shows_the_latest_words_within_the_six_line_limit() {
        let transcript = (0..80)
            .map(|index| format!("word{index}"))
            .collect::<Vec<_>>()
            .join(" ");
        let caption = overlay_caption_text(&transcript);

        assert!(caption.starts_with('…'));
        assert!(caption.ends_with("word79"));
        assert!(caption.chars().count() <= OVERLAY_CAPTION_CHARACTERS + 1);
    }

    #[test]
    fn overlay_height_grows_to_six_lines_and_stops() {
        assert_eq!(overlay_size_for_text("short").1, OVERLAY_HEIGHT);

        let long_caption = "x".repeat(OVERLAY_CAPTION_CHARACTERS * 2);
        assert_eq!(
            overlay_size_for_text(&long_caption).1,
            86 + OVERLAY_MAX_LINES as i32 * 20
        );
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
