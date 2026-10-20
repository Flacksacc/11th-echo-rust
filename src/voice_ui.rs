use crate::{settings::AppSettings, AppWindow, AssistantPipelineWindow};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
fn rows(values: Vec<String>) -> ModelRc<slint::SharedString> {
    ModelRc::new(VecModel::from(
        values.into_iter().map(Into::into).collect::<Vec<_>>(),
    ))
}

pub fn refresh_main(main: &AppWindow) {
    crate::avatar_ui::refresh_voices();
    let current = main.get_tts_voice_path().to_string();
    let choices = crate::node_settings::voice_choices(&current, false);
    main.set_tts_voice_options(rows(choices.iter().map(|(name, _)| name.clone()).collect()));
    main.set_tts_voice_option_paths(rows(choices.iter().map(|(_, path)| path.clone()).collect()));
    main.set_tts_selected_voice(
        choices
            .iter()
            .find(|(_, path)| path == &current)
            .unwrap()
            .0
            .clone()
            .into(),
    );
}
pub fn refresh_editor(editor: &AssistantPipelineWindow) {
    let current = editor.get_voice_path().to_string();
    if !current.is_empty() {
        editor.set_creating_voice(false);
    }
    let choices = crate::node_settings::voice_choices(&current, false);
    editor.set_voice_options(rows(choices.iter().map(|(name, _)| name.clone()).collect()));
    editor.set_voice_option_paths(rows(choices.iter().map(|(_, path)| path.clone()).collect()));
    editor.set_selected_voice(
        choices
            .iter()
            .find(|(_, path)| path == &current)
            .unwrap()
            .0
            .clone()
            .into(),
    );
    editor.set_voice_name(
        crate::voices::list()
            .ok()
            .and_then(|v| v.into_iter().find(|v| v.path() == Path::new(&current)))
            .map_or(String::new(), |v| v.name)
            .into(),
    );
}
pub fn start_import(
    editor: &AssistantPipelineWindow,
    main: slint::Weak<AppWindow>,
    source: PathBuf,
    remove_source: bool,
) {
    if editor.get_importing_voice() {
        return;
    }
    let mut base = if remove_source {
        "Recorded voice".to_string()
    } else {
        source
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    };
    if ["Alba", "Alba (standard)", "Use settings default"]
        .iter()
        .any(|reserved| base.eq_ignore_ascii_case(reserved))
    {
        base = format!("Imported {base}");
    }
    let existing = crate::voices::list().unwrap_or_default();
    let mut name = base.clone();
    let mut index = 2;
    while existing
        .iter()
        .any(|voice| voice.name.eq_ignore_ascii_case(&name))
    {
        name = format!("{base} ({index})");
        index += 1;
    }
    editor.set_importing_voice(true);
    editor.set_voice_status("Converting and validating voice locally…".into());
    let weak = editor.as_weak();
    std::thread::spawn(move || {
        let result = crate::voices::import(&source, &name);
        if remove_source {
            let _ = std::fs::remove_file(source);
        }
        let _ = weak.upgrade_in_event_loop(move |editor| {
            editor.set_importing_voice(false);
            match result {
                Ok(voice) => {
                    editor.set_voice_path(voice.path().to_string_lossy().into_owned().into());
                    editor.set_voice_status(
                        "Voice added to library. Open Sayings to edit or regenerate all 104 sayings."
                            .into(),
                    );
                }
                Err(error) => editor.set_voice_status(error.into()),
            }
            refresh_editor(&editor);
            if let Some(main) = main.upgrade() {
                refresh_main(&main);
            }
            crate::graph_ui::refresh_voice_choices();
        });
    });
}
pub fn close_editor(library: &AssistantPipelineWindow, editor: &AssistantPipelineWindow) {
    if editor.get_importing_voice() || editor.get_sayings_open() {
        return;
    }
    if !editor.get_creating_voice() || !editor.get_voice_path().is_empty() {
        library.set_voice_path(editor.get_voice_path());
    }
    library.set_voice_status(editor.get_voice_status());
    refresh_editor(library);
    library.set_editing_voice(false);
    let _ = editor.hide();
}

fn open_editor(
    library: &AssistantPipelineWindow,
    editor: &AssistantPipelineWindow,
    creating: bool,
) {
    if library.get_testing_voice() || library.get_installing_voice() || library.get_editing_voice()
    {
        return;
    }
    editor.set_creating_voice(creating);
    editor.set_voice_path(if creating {
        "".into()
    } else {
        library.get_voice_path()
    });
    editor.set_voice_status("".into());
    refresh_editor(editor);
    if editor.show().is_ok() {
        library.set_editing_voice(true);
    }
}

pub fn install(
    main: &AppWindow,
    library: &AssistantPipelineWindow,
    editor: &AssistantPipelineWindow,
    settings: Arc<Mutex<AppSettings>>,
) -> Result<(), slint::PlatformError> {
    crate::sayings_ui::install(library, editor)?;
    refresh_main(main);
    refresh_editor(library);
    editor.set_configuration_role(6);
    let weak = editor.as_weak();
    let library_weak = library.as_weak();
    editor.on_cancel(move || {
        if let (Some(library), Some(editor)) = (library_weak.upgrade(), weak.upgrade()) {
            close_editor(&library, &editor);
        }
    });
    let weak = editor.as_weak();
    editor.window().on_close_requested(move || {
        if let Some(editor) = weak.upgrade() {
            editor.invoke_cancel();
        }
        slint::CloseRequestResponse::KeepWindowShown
    });
    let weak = editor.as_weak();
    let library_weak = library.as_weak();
    library.on_edit_voice(move || {
        if let (Some(editor), Some(library)) = (weak.upgrade(), library_weak.upgrade()) {
            open_editor(&library, &editor, false);
        }
    });
    let weak = editor.as_weak();
    let library_weak = library.as_weak();
    library.on_new_voice(move || {
        if let (Some(editor), Some(library)) = (weak.upgrade(), library_weak.upgrade()) {
            open_editor(&library, &editor, true);
        }
    });
    let weak = library.as_weak();
    editor.on_install_voice_model(move || {
        if let Some(library) = weak.upgrade() {
            library.invoke_install_voice_model();
        }
    });
    let weak = editor.as_weak();
    let main_weak = main.as_weak();
    editor.on_import_voice(move || {
        let Some(editor) = weak.upgrade() else {
            return;
        };
        if editor.get_importing_voice() || editor.get_recording_voice() {
            return;
        }
        editor.set_importing_voice(true);
        editor.set_voice_status("Choose a WAV, MP3, or safetensors voice file.".into());
        let weak = editor.as_weak();
        let main = main_weak.clone();
        // Native Windows dialogs run their own event loop. Keep it off Slint's
        // event thread so window callbacks cannot reenter a UI interaction.
        std::thread::spawn(move || {
            let source = rfd::FileDialog::new()
                .add_filter("Voice recording / prompt", &["wav", "mp3", "safetensors"])
                .pick_file();
            let _ = weak.upgrade_in_event_loop(move |editor| {
                editor.set_importing_voice(false);
                if let Some(source) = source {
                    start_import(&editor, main, source, false);
                } else {
                    editor.set_voice_status("Import cancelled.".into());
                }
            });
        });
    });
    let weak = main.as_weak();
    main.on_select_default_voice(move |label| {
        if let Some(main) = weak.upgrade() {
            if let Some(i) = main.get_tts_voice_options().iter().position(|v| v == label) {
                main.set_tts_voice_path(main.get_tts_voice_option_paths().row_data(i).unwrap());
                main.set_settings_dirty(true);
            }
        }
    });
    let weak = library.as_weak();
    library.on_select_voice(move |label| {
        if let Some(editor) = weak.upgrade() {
            if let Some(i) = editor.get_voice_options().iter().position(|v| v == label) {
                editor.set_voice_path(editor.get_voice_option_paths().row_data(i).unwrap());
                refresh_editor(&editor);
            }
        }
    });
    let weak = editor.as_weak();
    let main_weak = main.as_weak();
    editor.on_rename_voice(move || {
        if let Some(editor) = weak.upgrade() {
            let result = crate::voices::rename(
                Path::new(editor.get_voice_path().as_str()),
                editor.get_voice_name().as_str(),
            );
            editor.set_voice_status(
                result
                    .map_or_else(|e| e, |_| "Voice renamed.".into())
                    .into(),
            );
            refresh_editor(&editor);
            if let Some(main) = main_weak.upgrade() {
                refresh_main(&main);
            }
            crate::graph_ui::refresh_voice_choices();
        }
    });
    let weak = editor.as_weak();
    editor.on_export_voice(move || {
        if let Some(editor) = weak.upgrade() {
            let source = PathBuf::from(editor.get_voice_path().as_str());
            if !source
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("safetensors"))
            {
                editor.set_voice_status(
                    "Import this existing WAV into the library before exporting.".into(),
                );
                return;
            }
            let weak = editor.as_weak();
            editor.set_importing_voice(true);
            std::thread::spawn(move || {
                let result = rfd::FileDialog::new()
                    .add_filter("Pocket TTS voice", &["safetensors"])
                    .set_file_name("voice.safetensors")
                    .save_file()
                    .map(|target| crate::voices::export(&source, &target));
                let _ = weak.upgrade_in_event_loop(move |editor| {
                    editor.set_importing_voice(false);
                    editor.set_voice_status(match result {
                        Some(Ok(())) => "Voice exported.".into(),
                        Some(Err(error)) => error.into(),
                        None => "Export cancelled.".into(),
                    });
                });
            });
        }
    });
    let weak = editor.as_weak();
    let main_weak = main.as_weak();
    editor.on_delete_voice(move || {
        if let Some(editor) = weak.upgrade() {
            let path = editor.get_voice_path().to_string();
            let settings = settings.lock().unwrap();
            let used = settings.tts_voice_path == path || settings.pipelines.as_ref().is_some_and(|set|set.pipelines.iter().any(|p|p.graph.as_ref().is_some_and(|g|g.nodes.iter().any(|n|matches!(&n.kind,crate::graph::Kind::Speech{voice,..} if voice == &path))))) || crate::graph_ui::uses_voice(&path) || main_weak.upgrade().is_some_and(|main|main.get_tts_voice_path() == path);
            drop(settings);
            if used {editor.set_voice_status("This voice is selected in settings or a graph. Choose another voice there before deleting it.".into());return;}
            match crate::voices::delete(Path::new(&path)) {
                Ok(()) => {editor.set_voice_path("".into());editor.set_voice_status("Voice deleted.".into());}
                Err(e) => editor.set_voice_status(e.into()),
            }
            refresh_editor(&editor);if let Some(main) = main_weak.upgrade(){refresh_main(&main);}crate::graph_ui::refresh_voice_choices();
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "isolated interactive voice library with temporary prompts; no audio, recording, or user settings writes"]
    fn voice_library_interactive_fixture() {
        let root =
            std::env::temp_dir().join(format!("echo-voice-ui-{}", crate::pipelines::new_id()));
        std::fs::create_dir_all(&root).unwrap();
        std::env::set_var("ECHO_VOICE_LIBRARY_DIR", &root);
        std::env::set_var("ECHO_SAYINGS_DIR", root.join("sayings"));
        std::env::set_var("ECHO_HERMES_COMPONENT_DIR", root.join("hermes"));
        crate::assistant_voice::set_output_volume(0.0);
        for (id, name) in [
            ("inspector-voice", "Inspector voice"),
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
        let editor = AssistantPipelineWindow::new().unwrap();
        let settings = Arc::new(Mutex::new(AppSettings::default()));
        crate::install_tts_settings_entrypoint(&main, &editor);
        let edit = AssistantPipelineWindow::new().unwrap();
        install(&main, &editor, &edit, settings).unwrap();
        main.invoke_configure_tts();
        let weak = editor.as_weak();
        editor.on_test_voice(move || {
            if let Some(editor) = weak.upgrade() {
                editor.set_voice_status(
                    format!(
                        "Preview requested: {} (isolated UI, no playback)",
                        editor.get_voice_test_text()
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
        if let (Ok(w), Ok(h)) = (
            std::env::var("ECHO_GUI_WIDTH"),
            std::env::var("ECHO_GUI_HEIGHT"),
        ) {
            editor.window().set_size(slint::PhysicalSize::new(
                w.parse().unwrap(),
                h.parse().unwrap(),
            ));
        }
        if let Ok(scale) = std::env::var("SLINT_SCALE_FACTOR") {
            let scale: f32 = scale.parse().unwrap();
            let weak = editor.as_weak();
            slint::Timer::single_shot(std::time::Duration::from_millis(150), move || {
                if let Some(editor) = weak.upgrade() {
                    let size = editor.window().size();
                    editor.window().dispatch_event(
                        slint::platform::WindowEvent::ScaleFactorChanged {
                            scale_factor: scale,
                        },
                    );
                    editor
                        .window()
                        .dispatch_event(slint::platform::WindowEvent::Resized {
                            size: slint::LogicalSize::new(
                                size.width as f32 / scale,
                                size.height as f32 / scale,
                            ),
                        });
                }
            });
        }
        if let Some(path) = std::env::var_os("ECHO_GUI_SNAPSHOT_PATH") {
            let weak = editor.as_weak();
            slint::Timer::single_shot(std::time::Duration::from_millis(500), move || {
                if let Some(editor) = weak.upgrade() {
                    let pixels = editor.window().take_snapshot().unwrap();
                    image::save_buffer(
                        path,
                        pixels.as_bytes(),
                        pixels.width(),
                        pixels.height(),
                        image::ColorType::Rgba8,
                    )
                    .unwrap();
                    editor.invoke_cancel();
                }
            });
        }
        slint::run_event_loop().unwrap();
        std::env::remove_var("ECHO_VOICE_LIBRARY_DIR");
        std::env::remove_var("ECHO_SAYINGS_DIR");
        std::env::remove_var("ECHO_HERMES_COMPONENT_DIR");
        std::fs::remove_dir_all(root).unwrap();
    }
}
