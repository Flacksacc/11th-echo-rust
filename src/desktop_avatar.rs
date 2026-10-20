use crate::{
    assistant_presentation as presentation, settings, AssistantChatWindow,
    DesktopAvatarSettingsWindow, DesktopAvatarWindow,
};
use slint::{CloseRequestResponse, ComponentHandle};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::{Arc, Mutex},
};

#[cfg(test)]
thread_local! { static ISOLATED: Cell<bool> = const { Cell::new(false) }; }

pub struct DesktopAvatar {
    _window: DesktopAvatarWindow,
    _settings: DesktopAvatarSettingsWindow,
    _timer: slint::Timer,
    _companion: Rc<RefCell<Option<crate::avatar_bridge::Companion>>>,
}

fn speech_blocks(text: &str) -> Vec<crate::AvatarSpeechBlock> {
    // Preserve all text while bounding each rich-layout operation. Prefer a
    // whitespace boundary; splitting a display row does not split TTS input.
    let mut blocks = Vec::new();
    let mut remaining = text;
    while !remaining.is_empty() {
        let mut end = remaining.len().min(2048);
        while !remaining.is_char_boundary(end) {
            end -= 1;
        }
        if end < remaining.len() {
            if let Some(boundary) = remaining[..end].rfind(char::is_whitespace) {
                if boundary > 1024 {
                    end = boundary + remaining[boundary..].chars().next().unwrap().len_utf8();
                }
            }
        }
        blocks.push(crate::AvatarSpeechBlock {
            text: remaining[..end].into(),
        });
        remaining = &remaining[end..];
    }
    blocks
}

fn save(config: &presentation::AvatarConfig, settings: &Arc<Mutex<settings::AppSettings>>) {
    let snapshot = {
        let mut settings = settings.lock().unwrap();
        settings.desktop_avatar = config.clone();
        settings.clone()
    };
    #[cfg(test)]
    let persist = !ISOLATED.with(Cell::get);
    #[cfg(not(test))]
    let persist = true;
    if persist {
        let _ = settings::save_settings(&snapshot);
    }
    presentation::update(|s| s.avatar = config.clone());
}

pub fn install(
    chat: &AssistantChatWindow,
    settings: Arc<Mutex<settings::AppSettings>>,
) -> Result<DesktopAvatar, slint::PlatformError> {
    let workspace = chat.as_weak();
    chat.window().on_close_requested(move || {
        if let Some(workspace) = workspace.upgrade() {
            let _ = workspace.hide();
        }
        CloseRequestResponse::KeepWindowShown
    });
    let window = DesktopAvatarWindow::new()?;
    let editor = DesktopAvatarSettingsWindow::new()?;
    let mut config = settings.lock().unwrap().desktop_avatar.clone();
    config.normalize();
    presentation::update(|s| s.avatar = config.clone());
    window.set_avatar(chat.get_avatar());
    window
        .window()
        .set_position(slint::PhysicalPosition::new(config.x, config.y));
    clamp_to_monitor(&window);
    let companion = Rc::new(RefCell::new(None::<crate::avatar_bridge::Companion>));
    let placement = Rc::new(Cell::new(false));
    let dismissed = Rc::new(RefCell::new(String::new()));

    let weak = window.as_weak();
    let settings_show = settings.clone();
    chat.on_show_avatar(move || {
        if let Some(window) = weak.upgrade() {
            let mut config = presentation::snapshot().avatar;
            config.visible = true;
            save(&config, &settings_show);
            let _ = window.show();
            apply_hit_region(&window);
        }
    });
    let chat_weak = chat.as_weak();
    window.on_open_workspace(move || {
        if let Some(chat) = chat_weak.upgrade() {
            let _ = chat.show();
        }
    });
    let settings_hide = settings.clone();
    let weak = window.as_weak();
    window.on_hide_avatar(move || {
        let mut config = presentation::snapshot().avatar;
        config.visible = false;
        save(&config, &settings_hide);
        if let Some(window) = weak.upgrade() {
            let _ = window.hide();
        }
    });
    let weak = window.as_weak();
    let placing = placement.clone();
    window.on_toggle_placement(move || {
        placing.set(!placing.get());
        if let Some(window) = weak.upgrade() {
            window.set_placement_mode(placing.get());
            apply_hit_region(&window);
        }
    });
    let settings_pose = settings.clone();
    window.on_toggle_sitting(move || {
        let mut config = presentation::snapshot().avatar;
        config.sitting = !config.sitting;
        save(&config, &settings_pose);
    });
    let dismiss = dismissed.clone();
    let weak = window.as_weak();
    window.on_dismiss_speech(move || {
        *dismiss.borrow_mut() = presentation::snapshot().spoken;
        if let Some(window) = weak.upgrade() {
            window.set_spoken("".into());
            apply_hit_region(&window);
        }
    });
    let editor_weak = editor.as_weak();
    window.on_open_settings(move || {
        if let Some(editor) = editor_weak.upgrade() {
            let config = presentation::snapshot().avatar;
            editor.set_runtime(config.runtime.into());
            editor.set_pack(config.pack.into());
            editor.set_avatar_scale(config.scale);
            editor.set_ledge_width(config.ledge_width);
            editor.set_sitting(config.sitting);
            editor.set_status(crate::avatar_bridge::status().into());
            let _ = editor.show();
        }
    });
    let avatar_weak = window.as_weak();
    chat.on_desktop_avatar_settings(move || {
        if let Some(avatar) = avatar_weak.upgrade() {
            avatar.invoke_open_settings();
        }
    });
    let editor_weak = editor.as_weak();
    editor.on_browse_runtime(move || {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Packaged Unreal companion", &["exe"])
            .pick_file()
        {
            if let Some(editor) = editor_weak.upgrade() {
                editor.set_runtime(path.to_string_lossy().into_owned().into());
            }
        }
    });
    let editor_weak = editor.as_weak();
    editor.on_browse_pack(move || {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Prepared avatar pack", &["pak"])
            .pick_file()
        {
            if let Some(editor) = editor_weak.upgrade() {
                editor.set_pack(path.to_string_lossy().into_owned().into());
            }
        }
    });
    let editor_weak = editor.as_weak();
    let settings_save = settings.clone();
    let owned = companion.clone();
    editor.on_save(move || {
        if let Some(editor) = editor_weak.upgrade() {
            let mut config = presentation::snapshot().avatar;
            config.runtime = editor.get_runtime().to_string();
            config.pack = editor.get_pack().to_string();
            config.scale = editor.get_avatar_scale();
            config.ledge_width = editor.get_ledge_width();
            config.sitting = editor.get_sitting();
            config.visible = true;
            config.normalize();
            // Launch a replacement only after validating its executable and pack.
            if !config.runtime.is_empty() {
                if let Err(error) = crate::avatar_bridge::validate_config(&config) {
                    editor.set_status(error.into());
                    return;
                }
                // Stop and join the old transport before starting its replacement.
                owned.borrow_mut().take();
                match crate::avatar_bridge::launch(&config) {
                    Ok(next) => *owned.borrow_mut() = Some(next),
                    Err(error) => {
                        editor.set_status(error.into());
                        return;
                    }
                }
            } else {
                owned.borrow_mut().take();
            }
            save(&config, &settings_save);
            editor.set_status("Saved. The workspace and avatar remain independent.".into());
        }
    });
    let weak = window.as_weak();
    let placing = placement.clone();
    editor.on_place(move || {
        placing.set(true);
        if let Some(window) = weak.upgrade() {
            window.set_placement_mode(true);
            let _ = window.show();
            apply_hit_region(&window);
        }
    });
    let weak = window.as_weak();
    let close_settings = settings.clone();
    window.window().on_close_requested(move || {
        let mut config = presentation::snapshot().avatar;
        config.visible = false;
        save(&config, &close_settings);
        if let Some(window) = weak.upgrade() {
            let _ = window.hide();
        }
        CloseRequestResponse::KeepWindowShown
    });
    if config.visible && !config.runtime.is_empty() {
        if let Ok(child) = crate::avatar_bridge::launch(&config) {
            *companion.borrow_mut() = Some(child);
        }
    }
    let timer = slint::Timer::default();
    let editor_status = editor.as_weak();
    let weak = window.as_weak();
    let chat_weak = chat.as_weak();
    let owned = companion.clone();
    let mut last = presentation::Presentation::default();
    let mut last_notification = String::new();
    let mut last_position = (config.x, config.y);
    let mut last_scale = 0.0;
    let mut position_changed = std::time::Instant::now();
    let mut position_dirty = false;
    let mut monitor_check = std::time::Instant::now();
    let mut last_runtime_status = crate::avatar_bridge::status();
    timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(100),
        move || {
            let Some(window) = weak.upgrade() else {
                return;
            };
            let state = presentation::snapshot();
            let current_scale = window.window().scale_factor();
            if current_scale != last_scale {
                apply_hit_region(&window);
                last_scale = current_scale;
            }
            if monitor_check.elapsed() > std::time::Duration::from_secs(2) {
                clamp_to_monitor(&window);
                monitor_check = std::time::Instant::now();
            }
            let portrait_visible = !crate::avatar_bridge::renderer_ready();
            if window.get_portrait_visible() != portrait_visible {
                window.set_portrait_visible(portrait_visible);
                apply_hit_region(&window);
            }
            let exited = owned
                .borrow_mut()
                .as_mut()
                .is_some_and(|child| !child.running());
            if exited {
                owned.borrow_mut().take();
                crate::avatar_bridge::companion_exited();
            }
            let runtime_status = crate::avatar_bridge::status();
            if runtime_status != last_runtime_status {
                if let Some(editor) = editor_status
                    .upgrade()
                    .filter(|editor| editor.window().is_visible())
                {
                    editor.set_status(runtime_status.clone().into());
                }
                last_runtime_status = runtime_status;
            }
            if state.avatar.visible && !window.window().is_visible() {
                let _ = window.show();
                clamp_to_monitor(&window);
            }
            if !state.avatar.visible && window.window().is_visible() {
                let _ = window.hide();
            }
            if let Some(chat) = chat_weak.upgrade() {
                if window.get_avatar() != chat.get_avatar() {
                    window.set_avatar(chat.get_avatar());
                    apply_hit_region(&window);
                }
            }
            let notification = crate::reminders::notification().unwrap_or_default();
            if state != last || notification != last_notification {
                let spoken = if !notification.is_empty() {
                    notification.as_str()
                } else if *dismissed.borrow() == state.spoken {
                    ""
                } else {
                    &state.spoken
                };
                if window.get_spoken().as_str() != spoken {
                    window.set_spoken(spoken.into());
                    window.set_speech_blocks(slint::ModelRc::new(slint::VecModel::from(
                        speech_blocks(spoken),
                    )));
                    window.set_bubble_height(
                        (50.0 + spoken.chars().count() as f32 / 50.0 * 18.0).clamp(70.0, 240.0),
                    );
                }
                window.set_show_spoken(state.show_spoken || !notification.is_empty());
                last_notification = notification;
                window.set_activity(
                    if state.listening {
                        "Listening…"
                    } else if !state.model_ready && !state.model_status.is_empty() {
                        &state.model_status
                    } else {
                        &state.state
                    }
                    .into(),
                );
                window.set_sitting(state.avatar.sitting);
                window.set_avatar_scale(state.avatar.scale);
                let scale = window.window().scale_factor();
                window.window().set_size(slint::PhysicalSize::new(
                    (640.0 * scale * state.avatar.scale) as u32,
                    (360.0 * scale * state.avatar.scale) as u32,
                ));
                apply_hit_region(&window);
                last = state;
            }
            {
                let position = window.window().position();
                if (position.x, position.y) != last_position {
                    last_position = (position.x, position.y);
                    let mut config = presentation::snapshot().avatar;
                    config.x = position.x;
                    config.y = position.y;
                    presentation::update(|s| s.avatar = config);
                    position_changed = std::time::Instant::now();
                    position_dirty = true;
                }
                if position_dirty
                    && position_changed.elapsed() > std::time::Duration::from_millis(400)
                {
                    save(&presentation::snapshot().avatar, &settings);
                    position_dirty = false;
                }
            }
        },
    );
    Ok(DesktopAvatar {
        _window: window,
        _settings: editor,
        _timer: timer,
        _companion: companion,
    })
}

#[cfg(windows)]
fn hwnd(window: &DesktopAvatarWindow) -> Option<windows::Win32::Foundation::HWND> {
    use raw_window_handle::HasWindowHandle;
    let handle = window.window().window_handle();
    match handle.window_handle().ok()?.as_raw() {
        raw_window_handle::RawWindowHandle::Win32(handle) => {
            Some(windows::Win32::Foundation::HWND(handle.hwnd.get()))
        }
        _ => None,
    }
}

#[cfg(windows)]
fn clamp_to_monitor(window: &DesktopAvatarWindow) {
    use windows::Win32::{
        Foundation::RECT,
        Graphics::Gdi::{GetMonitorInfoW, MonitorFromRect, MONITORINFO, MONITOR_DEFAULTTONEAREST},
    };
    let position = window.window().position();
    let size = window.window().size();
    let rect = RECT {
        left: position.x,
        top: position.y,
        right: position.x + size.width as i32,
        bottom: position.y + size.height as i32,
    };
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    unsafe {
        if GetMonitorInfoW(MonitorFromRect(&rect, MONITOR_DEFAULTTONEAREST), &mut info).as_bool() {
            let work = info.rcWork;
            window.window().set_position(slint::PhysicalPosition::new(
                position
                    .x
                    .clamp(work.left, (work.right - size.width as i32).max(work.left)),
                position
                    .y
                    .clamp(work.top, (work.bottom - size.height as i32).max(work.top)),
            ));
        }
    }
}

#[cfg(windows)]
fn apply_hit_region(window: &DesktopAvatarWindow) {
    use windows::Win32::Graphics::Gdi::{
        CombineRgn, CreateRectRgn, CreateRoundRectRgn, DeleteObject, SetWindowRgn, RGN_OR,
    };
    let Some(hwnd) = hwnd(window) else {
        return;
    };
    let scale = window.window().scale_factor() * window.get_avatar_scale();
    // A native region removes transparent pixels from both rendering and hit
    // testing, rather than making the whole avatar unclickable.
    unsafe {
        use windows::Win32::UI::WindowsAndMessaging::{
            GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
        };
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        SetWindowLongPtrW(
            hwnd,
            GWL_EXSTYLE,
            style | WS_EX_NOACTIVATE.0 as isize | WS_EX_TOOLWINDOW.0 as isize,
        );
        let region = CreateRectRgn(0, 0, 0, 0);
        if let Some(pixels) = window
            .get_avatar()
            .to_rgba8()
            .filter(|_| window.get_portrait_visible())
        {
            let width = (180.0 * scale) as u32;
            let height = (292.0 * scale) as u32;
            let factor =
                (width as f32 / pixels.width() as f32).min(height as f32 / pixels.height() as f32);
            let image_width = pixels.width() as f32 * factor;
            let image_height = pixels.height() as f32 * factor;
            let offset_x = (width as f32 - image_width) / 2.;
            let offset_y = (height as f32 - image_height) / 2.;
            for y in 0..image_height as i32 {
                let source_y = (y as f32 / factor) as u32;
                let mut start = None;
                for x in 0..=image_width as i32 {
                    let alpha = x < image_width as i32
                        && pixels.as_slice()
                            [(source_y * pixels.width() + (x as f32 / factor) as u32) as usize]
                            .a
                            > 24;
                    if alpha && start.is_none() {
                        start = Some(x);
                    }
                    if !alpha {
                        if let Some(left) = start.take() {
                            let run = CreateRectRgn(
                                left + offset_x as i32,
                                y + offset_y as i32,
                                x + offset_x as i32,
                                y + offset_y as i32 + 1,
                            );
                            CombineRgn(region, region, run, RGN_OR);
                            let _ = DeleteObject(run);
                        }
                    }
                }
            }
        }
        if !window.get_portrait_visible() {
            let rect = CreateRectRgn(0, 0, (180.0 * scale) as i32, (292.0 * scale) as i32);
            CombineRgn(region, region, rect, RGN_OR);
            let _ = DeleteObject(rect);
        }
        let add = |x: i32, y: i32, right: i32, bottom: i32| {
            let rect = CreateRoundRectRgn(
                (x as f32 * scale) as i32,
                (y as f32 * scale) as i32,
                (right as f32 * scale) as i32,
                (bottom as f32 * scale) as i32,
                12,
                12,
            );
            CombineRgn(region, region, rect, RGN_OR);
            let _ = DeleteObject(rect);
        };
        if window.get_show_spoken() && !window.get_spoken().is_empty() {
            add(176, 12, 628, (12.0 + window.get_bubble_height()) as i32);
        }
        add(190, 264, 628, 292);
        if window.get_placement_mode() {
            add(0, 296, 640, 340);
        }
        if SetWindowRgn(hwnd, region, true) == 0 {
            let _ = DeleteObject(region);
        }
    }
}

#[cfg(not(windows))]
fn clamp_to_monitor(_: &DesktopAvatarWindow) {}
#[cfg(not(windows))]
fn apply_hit_region(_: &DesktopAvatarWindow) {}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn speech_rows_preserve_unicode_and_whole_long_reply() {
        let text = "A long reply with Unicode 🐈 and multiple sentences.\n".repeat(2000);
        let rows = speech_blocks(&text);
        assert!(rows.len() > 40);
        assert!(rows.iter().all(|row| row.text.len() <= 2048));
        assert_eq!(
            rows.iter().map(|r| r.text.as_str()).collect::<String>(),
            text
        );
    }

    #[test]
    #[ignore = "isolated native workspace/avatar controls; no user settings or audio"]
    fn desktop_avatar_interactive_fixture() {
        ISOLATED.with(|flag| flag.set(true));
        let chat = AssistantChatWindow::new().unwrap();
        chat.set_hermes_mode(true);
        chat.set_hermes_ready(true);
        chat.set_task_title("Project planning".into());
        chat.set_agent_tabs(slint::ModelRc::new(slint::VecModel::from(vec![
            crate::AgentTab {
                id: "general".into(),
                name: "General".into(),
                activity: "Ready".into(),
                ..Default::default()
            },
        ])));
        chat.present_selected_tab_id("general".into());
        chat.present_spoken_text("Your workspace is ready. What shall we work on?".into());
        let settings = Arc::new(Mutex::new(settings::AppSettings::default()));
        let desktop = install(&chat, settings).unwrap();
        chat.invoke_show_avatar();
        chat.show().unwrap();
        if let Ok(scale) = std::env::var("SLINT_SCALE_FACTOR") {
            let scale_factor = scale.parse::<f32>().unwrap();
            let avatar = desktop._window.as_weak();
            let workspace = chat.as_weak();
            slint::Timer::single_shot(std::time::Duration::from_millis(150), move || {
                if let Some(avatar) = avatar.upgrade() {
                    avatar.window().dispatch_event(
                        slint::platform::WindowEvent::ScaleFactorChanged { scale_factor },
                    );
                }
                if let Some(workspace) = workspace.upgrade() {
                    workspace.window().dispatch_event(
                        slint::platform::WindowEvent::ScaleFactorChanged { scale_factor },
                    );
                }
            });
        }
        if std::env::var_os("ECHO_GUI_LONG_SPEECH").is_some() {
            chat.present_spoken_text(
                "A long Unicode reply 🐈 with preserved words and responsive scrolling. "
                    .repeat(1500)
                    .into(),
            );
        }
        if std::env::var_os("ECHO_GUI_AUTO_CLOSE").is_some() {
            slint::Timer::single_shot(std::time::Duration::from_secs(3), || {
                slint::quit_event_loop().unwrap();
            });
        }
        slint::run_event_loop().unwrap();
        drop(desktop);
        ISOLATED.with(|flag| flag.set(false));
    }
}
