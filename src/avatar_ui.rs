use crate::{assistant_voice, settings, AppWindow, AssistantChatWindow, AvatarPlaygroundWindow};
use slint::{CloseRequestResponse, ComponentHandle, ModelRc, VecModel};
use std::{
    cell::RefCell,
    io::{Cursor, Read},
    path::Path,
    rc::Rc,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};

thread_local! {
    static PLAYGROUND: RefCell<Option<slint::Weak<AvatarPlaygroundWindow>>> = const { RefCell::new(None) };
}

pub struct Playground {
    _window: AvatarPlaygroundWindow,
    _timer: slint::Timer,
}

struct AvatarUpload {
    bytes: Vec<u8>,
    extension: &'static str,
    rgba: image::RgbaImage,
}

fn read_avatar(path: &Path) -> Result<AvatarUpload, String> {
    const MAX_BYTES: u64 = 10 * 1024 * 1024;
    let file = std::fs::File::open(path).map_err(|_| "Could not read avatar image.")?;
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Could not read avatar image.")?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("Avatar image must be under 10 MB.".into());
    }
    let mut reader = image::ImageReader::new(Cursor::new(&bytes))
        .with_guessed_format()
        .map_err(|_| "Choose a valid PNG or WebP image.")?;
    let extension = match reader.format() {
        Some(image::ImageFormat::Png) => "png",
        Some(image::ImageFormat::WebP) => "webp",
        _ => return Err("Choose a valid PNG or WebP image.".into()),
    };
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    reader.limits(limits);
    let rgba = reader
        .decode()
        .map_err(|_| "Choose a valid image no larger than 4096 × 4096.")?
        .to_rgba8();
    Ok(AvatarUpload {
        bytes,
        extension,
        rgba,
    })
}

pub fn refresh_voices() {
    PLAYGROUND.with(|slot| {
        if let Some(window) = slot.borrow().as_ref().and_then(slint::Weak::upgrade) {
            let mut current = window.get_voice_path().to_string();
            if !assistant_voice::voice_selection_valid(Path::new(&current)) {
                current.clear();
                window.set_voice_path("".into());
                window.set_status_text(
                    "Selected voice is unavailable. Using Alba (standard).".into(),
                );
            }
            let choices = crate::node_settings::voice_choices(&current, false);
            window.set_voice_options(ModelRc::new(VecModel::from(
                choices
                    .iter()
                    .map(|(label, _)| label.clone().into())
                    .collect::<Vec<_>>(),
            )));
            if let Some((index, (label, _))) = choices
                .iter()
                .enumerate()
                .find(|(_, (_, path))| path == &current)
            {
                // Use the index explicitly so shrinking or reordering the model
                // cannot leave the combo displaying a different voice.
                window.set_voice_index(index as i32);
                window.set_selected_voice(label.clone().into());
            }
        }
    });
}

fn stop_preview(window: &AvatarPlaygroundWindow, token: &AtomicU64) {
    let generation = token.swap(0, Ordering::SeqCst);
    if generation != 0 && generation == assistant_voice::playback_generation() {
        assistant_voice::stop_playback();
    }
    window.set_testing_voice(false);
}

pub fn install(
    main: &AppWindow,
    chat: &AssistantChatWindow,
    voice_editor: &crate::AssistantPipelineWindow,
    settings: Arc<Mutex<settings::AppSettings>>,
) -> Result<Playground, slint::PlatformError> {
    let window = AvatarPlaygroundWindow::new()?;
    PLAYGROUND.with(|slot| *slot.borrow_mut() = Some(window.as_weak()));
    let pending = Rc::new(RefCell::new(None::<AvatarUpload>));
    let token = Arc::new(AtomicU64::new(0));

    let weak = window.as_weak();
    let main_weak = main.as_weak();
    let chat_weak = chat.as_weak();
    let pending_open = pending.clone();
    main.on_open_avatar_playground(move || {
        if let (Some(window), Some(main), Some(chat)) =
            (weak.upgrade(), main_weak.upgrade(), chat_weak.upgrade())
        {
            if !window.window().is_visible() {
                pending_open.borrow_mut().take();
                window.set_avatar(chat.get_avatar());
                window.set_avatar_name("Current assistant avatar".into());
                window.set_voice_path(main.get_tts_voice_path());
                window.set_status_text("Upload a transparent PNG and try a voice.".into());
                refresh_voices();
            }
            let _ = window.show();
        }
    });
    let main_weak = main.as_weak();
    chat.on_choose_avatar(move || {
        if let Some(main) = main_weak.upgrade() {
            main.invoke_open_avatar_playground();
        }
    });

    let weak = window.as_weak();
    let pending_upload = pending.clone();
    window.on_upload_avatar(move || {
        let Some(window) = weak.upgrade() else {
            return;
        };
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Avatar image", &["png", "webp"])
            .pick_file()
        else {
            return;
        };
        match read_avatar(&path) {
            Ok(upload) => {
                let pixels = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                    upload.rgba.as_raw(),
                    upload.rgba.width(),
                    upload.rgba.height(),
                );
                window.set_avatar(slint::Image::from_rgba8(pixels));
                window.set_avatar_name(
                    path.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                        .into(),
                );
                *pending_upload.borrow_mut() = Some(upload);
                window.set_status_text(
                    "Preview updated. Use avatar in assistant to save this portrait.".into(),
                );
            }
            Err(error) => window.set_status_text(error.into()),
        }
    });
    let weak = window.as_weak();
    let chat_weak = chat.as_weak();
    window.on_apply_avatar(move || {
        let (Some(window), Some(chat)) = (weak.upgrade(), chat_weak.upgrade()) else {
            return;
        };
        let pending_avatar = pending.borrow();
        let Some(upload) = pending_avatar.as_ref() else {
            window.set_status_text("This portrait is already used by the assistant.".into());
            return;
        };
        let destination = settings::settings_path().with_file_name(format!(
            "assistant-avatar-{}.{}",
            crate::pipelines::new_id(),
            upload.extension
        ));
        let result = (|| -> Result<(), String> {
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|_| "Could not create Echo settings folder.")?;
            }
            std::fs::write(&destination, &upload.bytes)
                .map_err(|_| "Could not save avatar image.")?;
            let mut current = settings.lock().unwrap();
            let mut snapshot = current.clone();
            snapshot.assistant_avatar_path = destination.to_string_lossy().into_owned();
            if !crate::save_settings(&snapshot) {
                let _ = std::fs::remove_file(&destination);
                return Err("Could not save avatar choice.".into());
            }
            *current = snapshot;
            chat.set_avatar(window.get_avatar());
            Ok(())
        })();
        let applied = result.is_ok();
        drop(pending_avatar);
        if applied {
            pending.borrow_mut().take();
        }
        window.set_status_text(
            result
                .map_or_else(
                    |error| error,
                    |_| "Avatar saved and applied to the assistant.".into(),
                )
                .into(),
        );
    });
    let weak = window.as_weak();
    window.on_select_voice(move |label| {
        if let Some(window) = weak.upgrade() {
            if let Some((_, path)) =
                crate::node_settings::voice_choices(window.get_voice_path().as_str(), false)
                    .into_iter()
                    .find(|(name, _)| name == label.as_str())
            {
                window.set_voice_path(path.into());
                refresh_voices();
            }
        }
    });
    let weak = window.as_weak();
    let main_weak = main.as_weak();
    let manage_token = token.clone();
    window.on_manage_voices(move || {
        if let (Some(window), Some(main)) = (weak.upgrade(), main_weak.upgrade()) {
            stop_preview(&window, &manage_token);
            main.invoke_configure_tts();
        }
    });
    let weak = window.as_weak();
    let preview_token = token.clone();
    let editor_weak = voice_editor.as_weak();
    window.on_test_voice(move || {
        let Some(window) = weak.upgrade() else {
            return;
        };
        if window.get_testing_voice() {
            return;
        }
        if editor_weak.upgrade().is_some_and(|editor| {
            editor.get_editing_voice()
                || editor.get_testing_voice()
                || editor.get_recording_voice()
                || editor.get_importing_voice()
        }) {
            window.set_status_text(
                "Finish the voice-library preview, recording, or import before testing here."
                    .into(),
            );
            return;
        }
        let phrase = window.get_test_text().trim().to_string();
        if phrase.is_empty() {
            window.set_status_text("Enter a test phrase first.".into());
            return;
        }
        if !assistant_voice::installed() {
            window.set_status_text("Install Pocket TTS in Set up voices before testing.".into());
            return;
        }
        let path = std::path::PathBuf::from(window.get_voice_path().as_str());
        if !assistant_voice::voice_selection_valid(&path) {
            window.set_status_text(
                "This voice is missing. Choose Alba or set up another voice.".into(),
            );
            return;
        }
        assistant_voice::stop_playback();
        let generation = assistant_voice::playback_generation();
        preview_token.store(generation, Ordering::SeqCst);
        window.set_testing_voice(true);
        window.set_status_text("Generating voice preview…".into());
        let weak = window.as_weak();
        let token = preview_token.clone();
        std::thread::spawn(move || {
            let result = assistant_voice::speak_if_current(&phrase, &path, generation);
            let _ = weak.upgrade_in_event_loop(move |window| {
                if token.load(Ordering::SeqCst) == generation {
                    token.store(0, Ordering::SeqCst);
                    window.set_testing_voice(false);
                    window.set_status_text(
                        if assistant_voice::playback_generation() != generation {
                            "Voice preview interrupted.".into()
                        } else {
                            result
                                .map_or_else(|error| error, |_| "Voice preview finished.".into())
                                .into()
                        },
                    );
                }
            });
        });
    });
    let weak = window.as_weak();
    let stop_token = token.clone();
    window.on_stop_voice(move || {
        if let Some(window) = weak.upgrade() {
            stop_preview(&window, &stop_token);
            window.set_status_text("Voice preview stopped.".into());
        }
    });
    let weak = window.as_weak();
    let close_token = token.clone();
    window.on_close_window(move || {
        if let Some(window) = weak.upgrade() {
            stop_preview(&window, &close_token);
            let _ = window.hide();
        }
    });
    let weak = window.as_weak();
    let close_token = token.clone();
    window.window().on_close_requested(move || {
        if let Some(window) = weak.upgrade() {
            stop_preview(&window, &close_token);
            let _ = window.hide();
        }
        CloseRequestResponse::KeepWindowShown
    });
    let timer = slint::Timer::default();
    let weak = window.as_weak();
    timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(150),
        move || {
            let generation = token.load(Ordering::SeqCst);
            if generation != 0 && generation != assistant_voice::playback_generation() {
                token.store(0, Ordering::SeqCst);
                if let Some(window) = weak.upgrade() {
                    window.set_testing_voice(false);
                    window.set_status_text("Voice preview interrupted.".into());
                }
            }
        },
    );
    Ok(Playground {
        _window: window,
        _timer: timer,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn avatar_upload_preserves_alpha_and_rejects_other_formats() {
        let root = std::env::temp_dir().join(format!("echo-avatar-{}", crate::pipelines::new_id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("portrait.png");
        let mut pixels = image::RgbaImage::new(2, 2);
        pixels.put_pixel(1, 1, image::Rgba([1, 2, 3, 255]));
        pixels.save(&path).unwrap();
        let upload = read_avatar(&path).unwrap();
        assert_eq!(upload.rgba.get_pixel(0, 0).0[3], 0);
        assert_eq!(upload.rgba.get_pixel(1, 1).0[3], 255);
        assert_eq!(upload.bytes, std::fs::read(&path).unwrap());
        std::fs::write(&path, b"not an image").unwrap();
        assert!(read_avatar(&path).is_err());
        image::RgbImage::new(2, 2)
            .save(root.join("other.jpg"))
            .unwrap();
        assert!(read_avatar(&root.join("other.jpg")).is_err());
        image::RgbaImage::new(4097, 1).save(&path).unwrap();
        assert!(read_avatar(&path).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[ignore = "interactive playground using temporary voices; no user settings writes or audio"]
    fn avatar_playground_interactive_fixture() {
        let root = std::env::temp_dir().join(format!(
            "echo-avatar-fixture-{}",
            crate::pipelines::new_id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::env::set_var("ECHO_VOICE_LIBRARY_DIR", &root);
        for (id, name) in [
            ("first-voice", "Playground voice"),
            ("second-voice", "Second voice"),
        ] {
            let prompt = candle_core::Tensor::zeros(
                (1, 16, 1024),
                candle_core::DType::F32,
                &candle_core::Device::Cpu,
            )
            .unwrap();
            candle_core::safetensors::save(
                &std::collections::HashMap::from([("audio_prompt".to_string(), prompt)]),
                root.join(format!("{id}.safetensors")),
            )
            .unwrap();
            std::fs::write(
                root.join(format!("{id}.json")),
                serde_json::to_vec(&crate::voices::Voice {
                    id: id.into(),
                    name: name.into(),
                })
                .unwrap(),
            )
            .unwrap();
        }
        let main = AppWindow::new().unwrap();
        let chat = AssistantChatWindow::new().unwrap();
        if let Some(path) = std::env::var_os("ECHO_GUI_AVATAR_PATH") {
            chat.set_avatar(slint::Image::load_from_path(Path::new(&path)).unwrap());
        }
        let editor = crate::AssistantPipelineWindow::new().unwrap();
        crate::install_tts_settings_entrypoint(&main, &editor);
        let settings = Arc::new(Mutex::new(settings::AppSettings::default()));
        let edit_voice = crate::AssistantPipelineWindow::new().unwrap();
        crate::voice_ui::install(&main, &editor, &edit_voice, settings.clone()).unwrap();
        let playground = install(&main, &chat, &editor, settings).unwrap();
        let window = &playground._window;
        let weak = window.as_weak();
        window.on_apply_avatar(move || {
            if let Some(window) = weak.upgrade() {
                window.set_status_text(
                    "Avatar apply requested (isolated fixture; no settings written).".into(),
                );
            }
        });
        let weak = window.as_weak();
        window.on_test_voice(move || {
            if let Some(window) = weak.upgrade() {
                window.set_testing_voice(true);
                window.set_status_text(
                    format!(
                        "Preview requested for {} (isolated fixture; no audio).",
                        window.get_selected_voice()
                    )
                    .into(),
                );
            }
        });
        let weak = editor.as_weak();
        editor.on_cancel(move || {
            if let Some(editor) = weak.upgrade() {
                let _ = editor.hide();
            }
        });
        match std::env::var("ECHO_GUI_AVATAR_ENTRY").as_deref() {
            Ok("main") => {
                main.set_active_tab(3);
                main.set_settings_tab(6);
                main.show().unwrap();
            }
            Ok("assistant") => {
                chat.show().unwrap();
            }
            _ => main.invoke_open_avatar_playground(),
        }
        if let (Ok(width), Ok(height)) = (
            std::env::var("ECHO_GUI_WIDTH"),
            std::env::var("ECHO_GUI_HEIGHT"),
        ) {
            window.window().set_size(slint::PhysicalSize::new(
                width.parse().unwrap(),
                height.parse().unwrap(),
            ));
        }
        if let Ok(scale) = std::env::var("SLINT_SCALE_FACTOR") {
            let scale_factor = scale.parse::<f32>().unwrap();
            let weak = window.as_weak();
            slint::Timer::single_shot(std::time::Duration::from_millis(150), move || {
                if let Some(window) = weak.upgrade() {
                    let size = window.window().size();
                    window.window().dispatch_event(
                        slint::platform::WindowEvent::ScaleFactorChanged { scale_factor },
                    );
                    window
                        .window()
                        .dispatch_event(slint::platform::WindowEvent::Resized {
                            size: slint::LogicalSize::new(
                                size.width as f32 / scale_factor,
                                size.height as f32 / scale_factor,
                            ),
                        });
                }
            });
        }
        slint::run_event_loop().unwrap();
        std::env::remove_var("ECHO_VOICE_LIBRARY_DIR");
        std::fs::remove_dir_all(root).unwrap();
    }
}
