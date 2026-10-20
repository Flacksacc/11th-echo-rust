use crate::{
    sayings::{Saying, Store},
    AssistantPipelineWindow, SayingRow, VoiceSayingsWindow,
};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

enum Navigation {
    Select(slint::SharedString),
    Close,
}

struct State {
    pending: Option<Navigation>,
    voice: PathBuf,
    store: Store,
    directory: PathBuf,
    parent: Option<slint::Weak<AssistantPipelineWindow>>,
    library: slint::Weak<AssistantPipelineWindow>,
    cancel: Arc<AtomicBool>,
    closing: bool,
    playback: Option<u64>,
}
fn label(category: &str) -> String {
    category
        .split('_')
        .map(|word| {
            let mut chars = word.chars();
            chars
                .next()
                .map(|first| format!("{}{}", first.to_uppercase(), chars.as_str()))
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}
// Keep these explanations aligned with turn/task playback triggers in hermes_ui.
fn reason(category: &str) -> &'static str {
    match category {
        "thinking" => "While a spoken reply is still thinking after 0.8 seconds.",
        "checking" => "When the assistant starts a tool whose name includes read or inspect.",
        "researching" => "When the assistant starts a search, browse, or fetch tool.",
        "working" => "When a worker claims a task, or the assistant starts another kind of tool.",
        "long_task" => {
            "When the selected background task has been running for at least 15 seconds."
        }
        "queued" => "When the backend confirms a task has been scheduled.",
        "instructions_queued" => {
            "When the backend confirms your instructions were added to a task."
        }
        "question" => "Reserved for clarification questions. No automatic playback trigger yet.",
        "approval" => "When the assistant requests permission for a step.",
        "completed" => "When the backend confirms a task has completed.",
        "failed" => "When the backend reports that a task failed.",
        "disconnected" => "When the assistant loses a previously working backend connection.",
        "reconnecting" => "When installing or repairing the assistant backend begins.",
        "model_loading" => {
            "Reserved for language-model loading. No automatic playback trigger yet."
        }
        "speech_loading" => "Reserved for voice-model loading. No automatic playback trigger yet.",
        "listening" => "Reserved for microphone listening. No automatic playback trigger yet.",
        "transcribing" => "Reserved for transcription progress. No automatic playback trigger yet.",
        "paused" => "Reserved for paused tasks. No automatic playback trigger yet.",
        "stopped" => "After the backend confirms your request to stop a task or conversation.",
        "switching" => "Reserved for switching conversations. No automatic playback trigger yet.",
        "resuming" => "Reserved for resuming tasks. No automatic playback trigger yet.",
        "saving" => "Reserved for saving results. No automatic playback trigger yet.",
        "files_ready" => {
            "Reserved for generated files becoming available. No automatic playback trigger yet."
        }
        "review_ready" => "When the backend reports that a task is ready for review.",
        _ => "No automatic playback trigger is configured for this section.",
    }
}
fn section_rows(sayings: &[&Saying], directory: &std::path::Path) -> Vec<SayingRow> {
    let mut rows = Vec::new();
    for (category, defaults) in crate::hermes_phrases::GROUPS {
        let options: Vec<_> = sayings.iter().filter(|s| s.category == *category).collect();
        if options.is_empty() {
            continue;
        }
        let count = if options.len() == defaults.len() {
            format!("{} options", options.len())
        } else {
            format!("{} of {} options", options.len(), defaults.len())
        };
        rows.push(SayingRow {
            heading: true,
            category: format!("{} · {count}", label(category)).into(),
            text: reason(category).into(),
            ..Default::default()
        });
        for saying in options {
            let number = saying
                .id
                .rsplit('-')
                .next()
                .and_then(|n| n.parse::<usize>().ok())
                .unwrap_or(0)
                + 1;
            let readiness = if crate::hermes_phrases::audio_path(directory, saying).is_file() {
                "Ready"
            } else {
                "Not recorded"
            };
            rows.push(SayingRow {
                heading: false,
                id: saying.id.clone().into(),
                category: label(category).into(),
                text: saying.text.clone().into(),
                state: format!("Option {number} · {readiness}").into(),
            });
        }
    }
    rows
}
// Keep the virtualized list and its measured row heights alive when only row
// contents change. Replacing the model makes a scrolled list estimate them again.
fn update_rows(window: &VoiceSayingsWindow, rows: Vec<SayingRow>) {
    let current = window.get_sayings();
    if let Some(model) = current.as_any().downcast_ref::<VecModel<SayingRow>>() {
        if model.row_count() == rows.len()
            && model
                .iter()
                .zip(&rows)
                .all(|(old, new)| old.id == new.id && old.heading == new.heading)
        {
            for (index, row) in rows.into_iter().enumerate() {
                if model.row_data(index).is_some_and(|old| {
                    old.category != row.category || old.text != row.text || old.state != row.state
                }) {
                    model.set_row_data(index, row);
                }
            }
            return;
        }
    }
    window.set_sayings(ModelRc::new(VecModel::from(rows)));
}
fn show_saying(window: &VoiceSayingsWindow, saying: &Saying, directory: &std::path::Path) {
    window.set_selected_id(saying.id.clone().into());
    window.set_category(label(&saying.category).into());
    window.set_wording(saying.text.clone().into());
    window.set_audio_state(
        if crate::hermes_phrases::audio_path(directory, saying).is_file() {
            "Generated audio ready"
        } else {
            "Not recorded"
        }
        .into(),
    );
    window.set_dirty(false);
}
fn refresh(
    window: &VoiceSayingsWindow,
    state: &Arc<Mutex<State>>,
    refilter: bool,
) -> Result<(), String> {
    let (store, directory) = {
        let state = state.lock().unwrap();
        (state.store.clone(), state.directory.clone())
    };
    let all = store.list()?;
    let query = window.get_search().trim().to_lowercase();
    // Saving or generating audio must not remove a visible option or reset its
    // position. Apply the text filter again only when the user changes search.
    let visible: Vec<_> = window
        .get_sayings()
        .iter()
        .filter(|row| !row.heading)
        .map(|row| row.id)
        .collect();
    let filtered: Vec<_> = all
        .iter()
        .filter(|s| {
            if !refilter {
                return visible.iter().any(|id| id.as_str() == s.id);
            }
            format!("{} {} {}", label(&s.category), reason(&s.category), s.text)
                .to_lowercase()
                .contains(&query)
        })
        .collect();
    let selected = window.get_selected_id().to_string();
    let selected = filtered
        .iter()
        .find(|s| s.id == selected)
        .copied()
        .or_else(|| filtered.first().copied());
    window.set_total(all.len() as i32);
    update_rows(window, section_rows(&filtered, &directory));
    if let Some(saying) = selected {
        show_saying(window, saying, &directory);
    } else {
        window.set_selected_id("".into());
        window.set_category("No matching sayings".into());
        window.set_wording("".into());
        window.set_audio_state("Not recorded".into());
    }
    window.set_dirty(false);
    Ok(())
}
fn close(window: &VoiceSayingsWindow, state: &Arc<Mutex<State>>) {
    let parent = {
        let mut state = state.lock().unwrap();
        state.closing = false;
        state.parent.take()
    };
    if let Some(parent) = parent.and_then(|parent| parent.upgrade()) {
        parent.set_sayings_open(false);
        if parent.get_configuration_role() != 6 {
            if let Some(library) = state.lock().unwrap().library.upgrade() {
                library.set_editing_voice(false);
            }
        }
    }
    let _ = window.hide();
}
fn stop(window: &VoiceSayingsWindow, state: &Arc<Mutex<State>>) {
    let mut state = state.lock().unwrap();
    state.cancel.store(true, Ordering::Release);
    if state
        .playback
        .take()
        .is_some_and(|generation| generation == crate::assistant_voice::playback_generation())
    {
        crate::assistant_voice::stop_playback();
    }
    window.set_status("Stopping… Completed sayings are kept.".into());
}
fn navigate(window: &VoiceSayingsWindow, state: &Arc<Mutex<State>>, action: Navigation) {
    match action {
        Navigation::Select(id) => {
            let (store, directory) = {
                let state = state.lock().unwrap();
                (state.store.clone(), state.directory.clone())
            };
            // Selecting a clip changes the editor, never the list model.
            match store.get(id.as_str()) {
                Ok(saying) => show_saying(window, &saying, &directory),
                Err(error) => window.set_status(error.into()),
            }
        }
        Navigation::Close => {
            if window.get_busy() {
                state.lock().unwrap().closing = true;
                stop(window, state);
            } else {
                close(window, state);
            }
        }
    }
}
fn request_navigation(window: &VoiceSayingsWindow, state: &Arc<Mutex<State>>, action: Navigation) {
    if window.get_confirm_changes() {
        return;
    }
    if window.get_dirty() {
        state.lock().unwrap().pending = Some(action);
        window.set_confirm_error("".into());
        window.set_confirm_changes(true);
    } else {
        navigate(window, state, action);
    }
}
fn save(window: &VoiceSayingsWindow, state: &Arc<Mutex<State>>) -> Result<(), String> {
    let store = state.lock().unwrap().store.clone();
    store.edit(
        window.get_selected_id().as_str(),
        window.get_wording().as_str(),
    )?;
    refresh(window, state, false)?;
    window.set_status(
        "Changes saved for this voice. Regenerate the saying to use its new words.".into(),
    );
    Ok(())
}
fn completed(window: &VoiceSayingsWindow, state: &Arc<Mutex<State>>, message: String) {
    window.set_busy(false);
    window.set_playing(false);
    window.set_current_saying("".into());
    state.lock().unwrap().playback = None;
    window.set_status(message.into());
    if let Err(error) = refresh(window, state, false) {
        window.set_status(error.into());
    }
    let closing = state.lock().unwrap().closing;
    if closing {
        close(window, state);
    }
}

pub fn install(
    library: &AssistantPipelineWindow,
    editor: &AssistantPipelineWindow,
) -> Result<(), slint::PlatformError> {
    let window = VoiceSayingsWindow::new()?;
    let state = Arc::new(Mutex::new(State {
        pending: None,
        voice: PathBuf::new(),
        store: Store::for_voice(std::path::Path::new("")),
        directory: PathBuf::new(),
        parent: None,
        library: library.as_weak(),
        cancel: Arc::new(AtomicBool::new(false)),
        closing: false,
        playback: None,
    }));
    for parent in [library, editor] {
        let owner = parent.as_weak();
        let window = window.clone_strong();
        let state = state.clone();
        parent.on_manage_sayings(move || {
            let Some(parent)=owner.upgrade() else {return;};
            if (parent.get_configuration_role() != 6 && parent.get_editing_voice()) || parent.get_installing_voice() || parent.get_testing_voice() || parent.get_importing_voice() || parent.get_recording_voice() || parent.get_creating_voice() || parent.get_sayings_open() {return;}
            let voice=PathBuf::from(parent.get_voice_path().as_str());
            let directory=match crate::hermes_phrases::directory(&voice) {Ok(dir)=>dir,Err(error)=>{parent.set_voice_status(error.into());return;}};
            {
                let mut state=state.lock().unwrap();state.store=Store::for_voice(&voice);state.voice=voice;state.directory=directory;
                state.parent=Some(parent.as_weak());state.closing=false;state.pending=None;
            }
            window.set_confirm_changes(false);window.set_confirm_error("".into());
            window.set_voice_label(parent.get_selected_voice());window.set_search("".into());window.set_selected_id("thinking-0".into());
            window.set_generation_total(0);window.set_generated_count(0);window.set_current_saying("".into());
            window.set_status("Choose a saying to edit, play, or regenerate. Regenerate all rebuilds every saying in this voice.".into());
            if let Err(error)=refresh(&window,&state,true) {parent.set_voice_status(error.into());return;}
            if let Err(error)=window.show() {parent.set_voice_status(error.to_string().into());return;}
            #[cfg(test)]
            inspect_window_size(&window);
            parent.set_sayings_open(true);
            if let Some(library)=state.lock().unwrap().library.upgrade() {library.set_editing_voice(true);}
        });
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        window.on_select_saying(move |id| {
            if let Some(window) = weak.upgrade() {
                if window.get_busy() || id == window.get_selected_id() {
                    return;
                }
                request_navigation(&window, &state, Navigation::Select(id));
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        window.on_search_changed(move || {
            if let Some(window) = weak.upgrade() {
                if let Err(error) = refresh(&window, &state, true) {
                    window.set_status(error.into());
                }
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        window.on_wording_changed(move || {
            if let Some(window) = weak.upgrade() {
                let store = state.lock().unwrap().store.clone();
                window.set_dirty(
                    store
                        .get(window.get_selected_id().as_str())
                        .is_ok_and(|s| s.text != window.get_wording().as_str()),
                );
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        window.on_save_words(move || {
            if let Some(window) = weak.upgrade() {
                if window.get_busy() || window.get_confirm_changes() {
                    return;
                }
                if let Err(error) = save(&window, &state) {
                    window.set_status(error.into());
                }
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        window.on_restore_words(move || {
            if let Some(window) = weak.upgrade() {
                if window.get_busy() || window.get_confirm_changes() {
                    return;
                }
                let store = state.lock().unwrap().store.clone();
                match store.original_text(window.get_selected_id().as_str()) {
                    Ok(text) => {
                        window.set_wording(text.into());
                        window.invoke_wording_changed();
                        window.set_status(
                            "Default wording restored. Choose Save to keep this change.".into(),
                        );
                    }
                    Err(error) => window.set_status(error.into()),
                }
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        window.on_resolve_changes(move |choice| {
            let Some(window) = weak.upgrade() else {
                return;
            };
            if !window.get_confirm_changes() {
                return;
            }
            // 0 = save and continue, 1 = discard and continue, 2 = keep editing.
            if choice == 0 {
                if let Err(error) = save(&window, &state) {
                    window.set_confirm_error(error.into());
                    return;
                }
            }
            let pending = state.lock().unwrap().pending.take();
            window.set_confirm_changes(false);
            window.set_confirm_error("".into());
            if choice == 0 || choice == 1 {
                if let Some(action) = pending {
                    navigate(&window, &state, action);
                }
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        window.on_generate(move |all| {
            let Some(window) = weak.upgrade() else {
                return;
            };
            if window.get_busy() || window.get_dirty() {
                return;
            }
            if !crate::assistant_voice::installed() {
                window.set_status(
                    "Install Pocket TTS in the voice library before generating sayings.".into(),
                );
                return;
            }
            let (voice, directory, store, cancel) = {
                let mut state = state.lock().unwrap();
                state.cancel = Arc::new(AtomicBool::new(false));
                (
                    state.voice.clone(),
                    state.directory.clone(),
                    state.store.clone(),
                    state.cancel.clone(),
                )
            };
            let sayings: Result<Vec<Saying>, String> = if all {
                store.list()
            } else {
                store
                    .get(window.get_selected_id().as_str())
                    .map(|s| vec![s])
            };
            let sayings = match sayings {
                Ok(sayings) => sayings,
                Err(error) => {
                    window.set_status(error.into());
                    return;
                }
            };
            window.set_busy(true);
            window.set_generated_count(0);
            window.set_generation_total(sayings.len() as i32);
            window.set_current_saying("".into());
            window.set_status(format!("Generating 0/{}…", sayings.len()).into());
            let weak = window.as_weak();
            let state = state.clone();
            std::thread::spawn(move || {
                let result = (|| -> Result<usize, String> {
                    let total = sayings.len();
                    let mut done = 0;
                    for saying in sayings {
                        if cancel.load(Ordering::Acquire) {
                            return Ok(done);
                        }
                        let current = format!(
                            "Generating {} of {total}: {} — {}",
                            done + 1,
                            label(&saying.category),
                            saying.text
                        );
                        let _ = weak.upgrade_in_event_loop(move |window| {
                            window.set_current_saying(current.into());
                        });
                        if !crate::hermes_phrases::generate_one(
                            &voice,
                            &directory,
                            &store,
                            &saying,
                            true,
                            &|| cancel.load(Ordering::Acquire),
                            &|| !crate::assistant_voice::playback_busy(),
                        )? {
                            return Ok(done);
                        }
                        done += 1;
                        let _ = weak.upgrade_in_event_loop(move |window| {
                            window.set_generated_count(done as i32);
                            window.set_status(format!("Generated {done}/{total} sayings.").into())
                        });
                    }
                    Ok(done)
                })();
                let message = match result {
                    Ok(done) => {
                        if cancel.load(Ordering::Acquire) {
                            format!("Stopped. {done} completed sayings kept.")
                        } else {
                            format!("Generated {done} sayings with this voice.")
                        }
                    }
                    Err(error) => error,
                };
                let _ =
                    weak.upgrade_in_event_loop(move |window| completed(&window, &state, message));
            });
        });
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        window.on_play(move || {let Some(window)=weak.upgrade() else {return;};
            if window.get_busy() || window.get_dirty() {return;}
            let path={let state=state.lock().unwrap();match state.store.get(window.get_selected_id().as_str()) {Ok(saying)=>crate::hermes_phrases::audio_path(&state.directory,&saying),Err(error)=>{window.set_status(error.into());return;}}};
            if !path.is_file() {window.set_status("Generate this saying first.".into());return;}
            crate::assistant_voice::stop_playback();let generation=crate::assistant_voice::playback_generation();
            state.lock().unwrap().playback=Some(generation);window.set_busy(true);window.set_playing(true);
            let weak=window.as_weak();let state=state.clone();
            std::thread::spawn(move || {
                let message=match crate::assistant_voice::play_cached_phrase(&path,generation) {Ok(true)=>"Saying playback finished.".into(),Ok(false)=>"Audio is busy or playback was stopped. Try again when the assistant is quiet.".into(),Err(error)=>error};
                let _=weak.upgrade_in_event_loop(move |window|completed(&window,&state,message));
            });
        });
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        window.on_stop(move || {
            if let Some(window) = weak.upgrade() {
                stop(&window, &state);
            }
        });
    }
    {
        let weak = window.as_weak();
        let state = state.clone();
        window.on_dismiss(move || {
            if let Some(window) = weak.upgrade() {
                request_navigation(&window, &state, Navigation::Close);
            }
        });
    }
    let weak = window.as_weak();
    window.window().on_close_requested(move || {
        if let Some(window) = weak.upgrade() {
            request_navigation(&window, &state, Navigation::Close);
        }
        slint::CloseRequestResponse::KeepWindowShown
    });
    Ok(())
}

// Display-scale inspection stays isolated from production window behavior.
#[cfg(test)]
fn inspect_window_size(window: &VoiceSayingsWindow) {
    let Ok(mode) = std::env::var("ECHO_GUI_SAYINGS_SIZE") else {
        return;
    };
    let scale = std::env::var("SLINT_SCALE_FACTOR")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .unwrap_or(1.0);
    let (width, height) = if mode == "minimum" {
        (820.0, 600.0)
    } else {
        (1000.0, 700.0)
    };
    let weak = window.as_weak();
    slint::Timer::single_shot(std::time::Duration::from_millis(150), move || {
        if let Some(window) = weak.upgrade() {
            window
                .window()
                .dispatch_event(slint::platform::WindowEvent::ScaleFactorChanged {
                    scale_factor: scale,
                });
            window.window().set_size(slint::PhysicalSize::new(
                (width * scale) as u32,
                (height * scale) as u32,
            ));
            window
                .window()
                .dispatch_event(slint::platform::WindowEvent::Resized {
                    size: slint::LogicalSize::new(width, height),
                });
        }
    });
}
