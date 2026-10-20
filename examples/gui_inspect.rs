//! Isolated Slint windows for visual/MCP inspection without Echo settings or services.

use slint::{ComponentHandle, ModelRc, VecModel};

#[path = "../src/component_pool.rs"]
mod component_pool;
#[path = "../src/resources.rs"]
mod resources;

slint::include_modules!();

fn avatar_fixture() -> Result<(), Box<dyn std::error::Error>> {
    let avatar = DesktopAvatarWindow::new()?;
    avatar.set_avatar(slint::Image::load_from_path(std::path::Path::new(
        "eleventhecho.png",
    ))?);
    avatar.set_activity("Thinking · Project planning".into());
    let spoken = "Your workspace is ready. What shall we work on?";
    avatar.set_spoken(spoken.into());
    avatar.set_speech_blocks(ModelRc::new(VecModel::from(vec![AvatarSpeechBlock {
        text: spoken.into(),
    }])));
    avatar.set_bubble_height(90.0);
    avatar.set_show_spoken(true);
    let settings = DesktopAvatarSettingsWindow::new()?;
    let settings_weak = settings.as_weak();
    avatar.on_open_settings(move || {
        if let Some(settings) = settings_weak.upgrade() {
            let _ = settings.show();
        }
    });
    let weak = avatar.as_weak();
    avatar.on_toggle_placement(move || {
        if let Some(avatar) = weak.upgrade() {
            avatar.set_placement_mode(!avatar.get_placement_mode());
        }
    });
    let weak = avatar.as_weak();
    avatar.on_dismiss_speech(move || {
        if let Some(avatar) = weak.upgrade() {
            avatar.set_spoken("".into());
        }
    });
    avatar.on_hide_avatar(|| {
        let _ = slint::quit_event_loop();
    });
    avatar.run()?;
    Ok(())
}

// Synthetic presentation benchmark: 50 busy tabs, the production 120-block
// page limit, and incremental updates. It does not run 50 paid model workers.
fn stress_assistant(window: &AssistantChatWindow) -> Result<(), Box<dyn std::error::Error>> {
    use slint::Model;
    let tabs = std::rc::Rc::new(VecModel::from(
        (0..50)
            .map(|i| AgentTab {
                id: format!("stress-{i}").into(),
                name: format!("Task {i}").into(),
                title: format!("Background task {i}").into(),
                activity: "Working".into(),
                task_id: format!("stress-{i}").into(),
                ..Default::default()
            })
            .collect::<Vec<_>>(),
    ));
    let paragraph = "**Status:** A synthetic task is continuing. Text stays selectable and the history page stays bounded. ".repeat(38);
    let rows = std::rc::Rc::new(VecModel::from(
        (0..120)
            .map(|i| RichMessageBlock {
                id: i.to_string().into(),
                message_id: i.to_string().into(),
                text: slint::StyledText::from_markdown(&paragraph).unwrap(),
                plain_text: paragraph.clone().into(),
                first: true,
                last: true,
                ..Default::default()
            })
            .collect::<Vec<_>>(),
    ));
    window.set_agent_tabs(ModelRc::from(tabs.clone()));
    window.set_rich_blocks(ModelRc::from(rows.clone()));
    let requested = std::rc::Rc::new(std::cell::Cell::new(None::<std::time::Instant>));
    let samples = std::rc::Rc::new(std::cell::RefCell::new(Vec::<f64>::new()));
    let frame_request = requested.clone();
    let frame_samples = samples.clone();
    window.window().set_rendering_notifier(move |phase, _| {
        if matches!(phase, slint::RenderingState::AfterRendering) {
            if let Some(started) = frame_request.take() {
                frame_samples
                    .borrow_mut()
                    .push(started.elapsed().as_secs_f64() * 1000.);
            }
        }
    })?;
    let weak = window.as_weak();
    let timer = std::rc::Rc::new(slint::Timer::default());
    let held_timer = timer.clone();
    let mut tick = 0usize;
    timer.start(slint::TimerMode::Repeated, std::time::Duration::from_millis(50), move || {
        let Some(window) = weak.upgrade() else { held_timer.stop(); return; };
        if tick == 120 {
            held_timer.stop();
            let mut values = samples.borrow().clone(); values.sort_by(f64::total_cmp);
            if !values.is_empty() {
                let report = serde_json::json!({"tabs":50,"visible_blocks":120,"source_bytes_per_page":paragraph.len()*120,
                    "samples":values.len(),"median_update_to_frame_ms":values[values.len()/2],"p95_update_to_frame_ms":values[values.len()*95/100],
                    "max_update_to_frame_ms":values[values.len()-1],"renderer":"winit-skia","scale_factor":window.window().scale_factor()});
                eprintln!("{report}");
                if let Some(path) = std::env::var_os("ECHO_GUI_STRESS_REPORT") { std::fs::write(path, report.to_string()).unwrap(); }
            }
            if std::env::var_os("ECHO_GUI_STRESS_EXIT").is_some() { let _ = window.hide(); let _ = slint::quit_event_loop(); }
            return;
        }
        if requested.get().is_none() { requested.set(Some(std::time::Instant::now())); }
        let tab = tick % 50;
        let mut data = tabs.row_data(tab).unwrap(); data.unread += 1; tabs.set_row_data(tab,data);
        window.set_selected_tab_id(format!("stress-{tab}").into());
        window.set_selected_tab_index(tab as i32);
        if std::env::var_os("ECHO_GUI_SCROLL_STRESS").is_some() {
            window.set_hermes_scroll_position(-((tick % 60) as f32) * 120.0);
        }
        window.set_task_title(format!("Background task {tab}").into());
        let mut row = rows.row_data(119).unwrap();
        row.text = slint::StyledText::from_plain_text(&format!("Streaming update {tick}: the task is still working."));
        rows.set_row_data(119,row);
        window.window().request_redraw(); tick += 1;
    });
    Ok(())
}

fn inspect<T: ComponentHandle + 'static>(window: T) -> Result<(), Box<dyn std::error::Error>> {
    window.show()?;
    // Slint's winit backend applies SLINT_SCALE_FACTOR to both its renderer
    // and native window. A second synthetic resize desynchronizes the surface.
    if let (Some(width), Some(height)) = (
        std::env::var("ECHO_GUI_WIDTH")
            .ok()
            .and_then(|s| s.parse().ok()),
        std::env::var("ECHO_GUI_HEIGHT")
            .ok()
            .and_then(|s| s.parse().ok()),
    ) {
        window
            .window()
            .set_size(slint::PhysicalSize::new(width, height));
    }
    if let Some(path) = std::env::var_os("ECHO_GUI_SNAPSHOT_PATH") {
        // Capture only after the native renderer, layout and scaling events
        // have settled; an immediate snapshot misses the 150 ms scale update.
        let result = std::rc::Rc::new(std::cell::RefCell::new(None));
        let saved = result.clone();
        let weak = window.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(450), move || {
            let outcome = (|| -> Result<(), Box<dyn std::error::Error>> {
                let window = weak
                    .upgrade()
                    .ok_or("Inspection window closed before capture")?;
                let snapshot = window.window().take_snapshot()?;
                image::save_buffer(
                    path,
                    snapshot.as_bytes(),
                    snapshot.width(),
                    snapshot.height(),
                    image::ColorType::Rgba8,
                )?;
                window.hide()?;
                Ok(())
            })();
            *saved.borrow_mut() = Some(outcome);
            let _ = slint::quit_event_loop();
        });
        window.run()?;
        result
            .borrow_mut()
            .take()
            .ok_or("Snapshot timer did not run")??;
        Ok(())
    } else {
        Ok(window.run()?)
    }
}

fn graph_progress_fixture(current: usize) -> ModelRc<GraphRunRow> {
    let indices = current
        .checked_sub(1)
        .into_iter()
        .chain(std::iter::once(current))
        .chain(current + 1..=(current + 2).min(14));
    ModelRc::new(VecModel::from(
        indices
            .map(|i| GraphRunRow {
                id: i.to_string().into(),
                title: format!("Node {}", i + 1).into(),
                state: if i < current {
                    "Complete"
                } else if i == current {
                    "Running"
                } else {
                    "Waiting"
                }
                .into(),
            })
            .collect::<Vec<_>>(),
    ))
}

fn transcript_fixture() -> slint::SharedString {
    std::env::var("ECHO_GUI_TRANSCRIPT")
        .unwrap_or_else(|_| {
            "This is the first sentence, with several wide words WWWWWW WWWWWW WWWWWW WWWWWW."
                .into()
        })
        .into()
}

fn resize_overlay_fixture(window: &TranscriptOverlayWindow) {
    let height = window
        .get_caption_required_height()
        .ceil()
        .clamp(106., 360.);
    window.set_window_height(height as i32);
    let scale = window.window().scale_factor();
    window.window().set_size(slint::PhysicalSize::new(
        (560. * scale).round() as u32,
        (height * scale).round() as u32,
    ));
}

fn install_pool_fixture(window: &PipelineEditorWindow) {
    use component_pool::{groups, PoolItem, GROUPS};
    let created = std::rc::Rc::new(std::cell::RefCell::new(None::<ModelCreatorWindow>));
    window.on_create_component(move || {
        if let Ok(model) = ModelCreatorWindow::new() {
            model.set_providers(ModelRc::new(VecModel::from(vec![
                "gemini".into(),
                "openai".into(),
                "ollama".into(),
                "ollaya".into(),
                "openai_compatible".into(),
            ])));
            model.set_instructions(
                "Fix grammar, spelling, and punctuation. Return only the revised text.".into(),
            );
            let weak = model.as_weak();
            model.on_close_window(move || {
                if let Some(model) = weak.upgrade() {
                    let _ = model.hide();
                }
            });
            let _ = model.show();
            *created.borrow_mut() = Some(model);
        }
    });

    use slint::Model;
    use std::rc::Rc;
    let data = [
        (
            0,
            "Parakeet Ultra",
            "parakeet-ultra · English",
            "local offline speech transcription",
        ),
        (
            0,
            "Parakeet v2",
            "parakeet-tdt-v2 · English",
            "local offline speech transcription",
        ),
        (
            0,
            "Parakeet v3",
            "parakeet-tdt-v3 · English",
            "local offline speech transcription",
        ),
        (
            0,
            "ElevenLabs",
            "scribe-v2-realtime · English",
            "online cloud speech transcription",
        ),
        (
            0,
            "OpenAI Whisper",
            "whisper-1 · English",
            "online cloud speech transcription",
        ),
        (
            1,
            "Fast model",
            "qwen3:8b · ollama",
            "local offline LLM assistant rewrite",
        ),
        (
            1,
            "Strong model",
            "gemini-2.5-pro · gemini",
            "online cloud google LLM assistant rewrite",
        ),
        (
            1,
            "A very long model title to check tile clipping",
            "llama-3.3-70b · openai_compatible",
            "language LLM local network rewrite",
        ),
        (
            2,
            "Punctuation & casing",
            "commas, periods, capitalization",
            "text processing tool offline",
        ),
        (
            3,
            "Laya · local decision",
            "laya:en · ollaya",
            "decision router local offline",
        ),
        (
            2,
            "Local formatting",
            "numbers, quantities, dates/times",
            "text processing tool offline measurements replacements",
        ),
    ];
    let items = Rc::new(
        data.iter()
            .enumerate()
            .map(|(index, &(group, title, summary, metadata))| PoolItem {
                index,
                group,
                card: PipelineCard {
                    title: title.into(),
                    summary: summary.into(),
                    speech: group == 0,
                    icon: slint::Image::load_from_svg_data(if group == 0 {
                        include_bytes!("../assets/icons/speech.svg")
                    } else {
                        include_bytes!("../assets/icons/rewrite.svg")
                    })
                    .unwrap_or_default(),
                },
                search: format!("{} {title} {summary} {metadata}", GROUPS[group]).to_lowercase(),
            })
            .collect::<Vec<_>>(),
    );
    window.set_cards(ModelRc::new(VecModel::from(vec![
        items[0].card.clone(),
        items[8].card.clone(),
    ])));
    window.set_pool_groups(groups(
        &items,
        "",
        window.get_pool_columns().max(1) as usize,
    ));
    let weak = window.as_weak();
    let filter_items = items.clone();
    window.on_filter_pool(move |query, columns| {
        if let Some(window) = weak.upgrade() {
            window.set_pool_groups(groups(&filter_items, &query, columns.max(1) as usize));
        }
    });
    let weak = window.as_weak();
    let add_items = items.clone();
    window.on_add(move |index, position| {
        if let (Some(window), Some(item)) = (weak.upgrade(), add_items.get(index as usize)) {
            let mut cards = window.get_cards().iter().collect::<Vec<_>>();
            if item.card.speech {
                cards[0] = item.card.clone();
            } else {
                cards.insert(
                    (position.max(1) as usize).min(cards.len()),
                    item.card.clone(),
                );
            }
            window.set_cards(ModelRc::new(VecModel::from(cards)));
            window.set_dirty(true);
        }
    });
    let weak = window.as_weak();
    window.on_assign_assistant_model(move |index, target| {
        if let (Some(window), Some(item)) = (weak.upgrade(), items.get(index as usize)) {
            if !item.card.speech && (item.group == 1 || (item.group == 3 && target == 0)) {
                if target == 0 {
                    window.set_router_name(item.card.title.clone());
                } else if target == 2 {
                    window.set_fast_answer_name(item.card.title.clone());
                } else {
                    window.set_answer_name(item.card.title.clone());
                }
                window.set_dirty(true);
            }
        }
    });
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().nth(1).as_deref() == Some("desktop-avatar") {
        return avatar_fixture();
    }

    match std::env::args().nth(1).as_deref() {
        Some("main") => {
            let window = AppWindow::new()?;
            let weak = window.as_weak();
            window.on_request_navigation(move |tab| {
                if let Some(window) = weak.upgrade() {
                    window.set_active_tab(tab);
                }
            });
            window.set_speaker_options(ModelRc::new(VecModel::from(vec![
                "Speakers (Desktop audio)".into(),
                "Headphones (CORSAIR VIRTUOSO MAX WIRELESS Gaming Headset)".into(),
            ])));
            window.set_selected_speaker("Speakers (Desktop audio)".into());
            let weak = window.as_weak();
            window.on_test_speakers(move || {
                if let Some(window) = weak.upgrade() {
                    window.set_speaker_test_status(
                        "Speaker test callback received (isolated preview; no audio played)."
                            .into(),
                    );
                }
            });
            window.set_local_model_options(ModelRc::new(VecModel::from(vec![
                "Parakeet TDT 0.6B v2 — 600M parameters, English, INT8".into(),
                "Parakeet TDT 0.6B v3 — 600M parameters, 25 languages, INT8".into(),
                "Parakeet Ultra — NVIDIA GPU, 25 languages".into(),
            ])));
            window.set_local_model_text(
                "Parakeet TDT 0.6B v2 — 600M parameters, English, INT8".into(),
            );
            window.set_tts_model_status("Pocket TTS is not installed (isolated fixture).".into());
            let tts_editor = AssistantPipelineWindow::new()?;
            let weak = window.as_weak();
            let editor = tts_editor.as_weak();
            window.on_configure_tts(move || {
                if let (Some(window), Some(editor)) = (weak.upgrade(), editor.upgrade()) {
                    editor.set_configuration_role(4);
                    editor.set_default_voice_settings(true);
                    editor.set_voice_path(window.get_tts_voice_path());
                    let main = window.as_weak();
                    let voice = editor.as_weak();
                    editor.on_save(move || {
                        if let (Some(main), Some(voice)) = (main.upgrade(), voice.upgrade()) {
                            main.set_tts_voice_path(voice.get_voice_path());
                            let _ = voice.hide();
                        }
                    });
                    let _ = editor.show();
                }
            });
            let weak = tts_editor.as_weak();
            tts_editor.on_cancel(move || {
                if let Some(editor) = weak.upgrade() {
                    let _ = editor.hide();
                }
            });
            window.set_app_version(env!("CARGO_PKG_VERSION").into());
            let _monitor = resources::install(&window);
            inspect(window)
        }
        Some("pipelines-list") => {
            let window = AppWindow::new()?;
            window.set_active_tab(3);
            window.set_settings_tab(2);
            window.set_pipeline_rows(ModelRc::new(VecModel::from(vec![
                PipelineRow {
                    id: "dictation-example".into(),
                    name: "Everyday dictation".into(),
                    summary: "Transcription · 3 chain components · Parakeet Ultra → Punctuation → +1".into(),
                    details: "Transcription output · Active\n3 chain components\n1. Parakeet Ultra — English\n2. Punctuation & casing — commas, periods\n3. Local formatting — numbers, dates".into(),
                    active: true,
                    assistant: false,
                },
                PipelineRow {
                    id: "assistant-example".into(),
                    name: "Everyday assistant".into(),
                    summary: "Assistant · 1 chain component · Parakeet Ultra".into(),
                    details: "Assistant output · Active\n1 chain component\n1. Parakeet Ultra — English\nFast router: Fast model\nDelegated answer: Strong model\nReply: chat and voice".into(),
                    active: true,
                    assistant: true,
                },
                PipelineRow {
                    id: "alternate-example".into(),
                    name: "Long name for a compact transcription pipeline card".into(),
                    summary: "Transcription · 2 chain components · Local speech → Model".into(),
                    details: "Transcription output · Available\n2 chain components\n1. Local speech — English\n2. Model — cleanup".into(),
                    active: false,
                    assistant: false,
                },
            ])));
            inspect(window)
        }
        Some("model-creator") => {
            let window = ModelCreatorWindow::new()?;
            window.set_providers(ModelRc::new(VecModel::from(vec![
                "gemini".into(),
                "openai".into(),
                "ollama".into(),
                "ollaya".into(),
                "openai_compatible".into(),
            ])));
            window.set_instructions(
                "Fix grammar, spelling, and punctuation. Return only the revised text.".into(),
            );
            window.set_provider("ollaya".into());
            window.set_name("Laya · local decision".into());
            window.set_model_id("laya:en".into());
            window.set_endpoint("http://127.0.0.1:11435".into());
            window.set_instructions("Choose direct for simple conversation; delegate complex reasoning, coding, or tool use.".into());
            window.set_sample_input("Hello, how are you?".into());
            inspect(window)
        }
        Some("models") => {
            let window = ModelManagerWindow::new()?;
            window.set_providers(ModelRc::new(VecModel::from(vec![
                "gemini".into(),
                "openai".into(),
                "ollama".into(),
                "ollaya".into(),
                "openai_compatible".into(),
            ])));
            inspect(window)
        }
        Some(kind @ ("pipeline-editor" | "assistant-pipeline")) => {
            let window = PipelineEditorWindow::new()?;
            window.set_assistant_mode(kind == "assistant-pipeline");
            window.set_pipeline_name(
                if kind == "assistant-pipeline" {
                    "Everyday assistant"
                } else {
                    "Everyday dictation"
                }
                .into(),
            );
            install_pool_fixture(&window);
            if kind == "assistant-pipeline" {
                window.set_decision_router(true);
                window.set_router_name("Laya · local decision".into());
                window.set_fast_answer_name("Fast model".into());
                window.set_answer_name("Strong model".into());
            }
            inspect(window)
        }
        Some("pipeline-step") => inspect(PipelineStepWindow::new()?),
        Some("pipeline-step-local") => {
            let window = PipelineStepWindow::new()?;
            window.set_kind(1);
            window.set_component_title("Parakeet Ultra".into());
            window.set_model("parakeet-ultra".into());
            window.set_language("en".into());
            window.set_supports_preload(true);
            window.set_preload_on_start(true);
            window.set_cpu_threads(4.0);
            inspect(window)
        }
        Some("pipeline-step-model") => {
            let window = PipelineStepWindow::new()?;
            window.set_kind(5);
            window.set_component_title("Local Ollama model".into());
            window.set_supports_preload(true);
            window.set_max_tokens("2048".into());
            window.set_timeout_seconds("60".into());
            inspect(window)
        }
        Some("hotkey") => inspect(HotkeyCaptureWindow::new()?),
        Some("overlay") => {
            let window = TranscriptOverlayWindow::new()?;
            window.set_is_visible(true);
            window.set_sentence_text(transcript_fixture());
            let weak = window.as_weak();
            window.on_caption_layout_changed(move || {
                if let Some(window) = weak.upgrade() {
                    resize_overlay_fixture(&window);
                }
            });
            let weak = window.as_weak();
            slint::Timer::single_shot(std::time::Duration::from_millis(300), move || {
                if let Some(window) = weak.upgrade() {
                    resize_overlay_fixture(&window);
                }
            });
            window.set_window_height(190);
            window.set_graph_run_nodes(graph_progress_fixture(5));
            window.set_graph_progress_nodes(graph_progress_fixture(5));
            window.set_pipeline_step(5);
            window.set_graph_status("Node 6 · Running".into());
            inspect(window)
        }
        Some("assistant-chat") => {
            let window = AssistantChatWindow::new()?;
            let scale = std::env::var("SLINT_SCALE_FACTOR")
                .ok()
                .and_then(|s| s.parse::<f32>().ok())
                .unwrap_or(1.0);
            if let Ok(width) = std::env::var("ECHO_GUI_WIDTH") {
                window.set_window_width((width.parse::<f32>()? / scale).round() as i32);
            }
            if let Ok(height) = std::env::var("ECHO_GUI_HEIGHT") {
                window.set_window_height((height.parse::<f32>()? / scale).round() as i32);
            }
            if std::env::var_os("ECHO_GUI_HERMES").is_some() {
                window.set_hermes_mode(true);
                window.set_hermes_ready(true);
                let fixture_tabs = std::rc::Rc::new(VecModel::from(vec![
                    AgentTab {
                        id: "general".into(),
                        name: "Assistant".into(),
                        title: "Assistant".into(),
                        activity: "Ready".into(),
                        ..Default::default()
                    },
                    AgentTab {
                        id: "task".into(),
                        name: "Project review".into(),
                        title: "Inspect project dependencies".into(),
                        activity: "Working".into(),
                        task_id: "task".into(),
                        unread: 2,
                        priority: true,
                    },
                ]));
                window.set_agent_tabs(ModelRc::from(fixture_tabs.clone()));
                let weak_new = window.as_weak();
                window.on_new_conversation(move || {
                    use slint::Model;
                    if let Some(w) = weak_new.upgrade() {
                        let count = fixture_tabs.row_count();
                        let id = format!("chat-{count}");
                        fixture_tabs.push(AgentTab {
                            id: id.clone().into(),
                            name: format!("Chat {count}").into(),
                            title: "Assistant".into(),
                            activity: "Ready".into(),
                            ..Default::default()
                        });
                        w.set_selected_tab_id(id.into());
                        w.set_selected_tab_index(count as i32);
                        w.set_tab_name(format!("Chat {count}").into());
                        w.set_task_title("Assistant".into());
                        w.set_has_task(false);
                        w.set_draft("".into());
                        w.set_spoken_text("".into());
                        w.set_rich_blocks(ModelRc::default());
                        w.set_assistant_state("Ready".into());
                    }
                });
                window.set_selected_tab_id("general".into());
                window.set_task_title("Assistant".into());
                window.set_tab_name("Assistant".into());
                window.set_rich_blocks(ModelRc::new(VecModel::from(vec![
                    RichMessageBlock { id: "1".into(), message_id: "1".into(), text: slint::StyledText::from_plain_text("Check the dependencies while I ask another question."), plain_text: "Check the dependencies while I ask another question.".into(), from_user: true, first: true, last: true, kind: 0, ..Default::default() },
                    RichMessageBlock { id: "2".into(), message_id: "2".into(), text: slint::StyledText::from_markdown("The review is running in its own tab. **This conversation remains available.**").unwrap(), plain_text: "The review is running in its own tab.".into(), from_user: false, first: true, last: true, kind: 0, ..Default::default() },
                    RichMessageBlock { id: "3".into(), message_id: "3".into(), text: slint::StyledText::from_plain_text("Progress and results"), plain_text: "Progress and results".into(), kind: 1, first: true, ..Default::default() },
                    RichMessageBlock { id: "4".into(), message_id: "3".into(), text: slint::StyledText::from_plain_text("cargo check\ncargo test --bin echo hermes"), plain_text: "cargo check\ncargo test --bin echo hermes".into(), kind: 2, ..Default::default() },
                    RichMessageBlock { id: "5".into(), message_id: "3".into(), kind: 3, last: true,
                        table_rows: ModelRc::new(VecModel::from(vec![
                            RichTableRow { header: true, cells: ModelRc::new(VecModel::from(vec![slint::StyledText::from_plain_text("Check"),slint::StyledText::from_plain_text("Result")])) },
                            RichTableRow { header: false, cells: ModelRc::new(VecModel::from(vec![slint::StyledText::from_plain_text("Native sessions and cancellation"),slint::StyledText::from_plain_text("Verified without canceling other conversations")])) },
                        ])), ..Default::default() },
                ])));
                let weak = window.as_weak();
                window.on_select_tab(move |id| {
                    if let Some(w) = weak.upgrade() {
                        let task = id == "task";
                        w.set_selected_tab_index(if task {
                            1
                        } else {
                            id.strip_prefix("chat-")
                                .and_then(|v| v.parse::<i32>().ok())
                                .unwrap_or_default()
                        });
                        w.set_selected_tab_id(id);
                        w.set_has_task(task);
                        w.set_can_retry(false);
                        w.set_tab_name(if task { "Project review" } else { "Assistant" }.into());
                        w.set_task_title(
                            if task {
                                "Inspect project dependencies"
                            } else {
                                "Assistant"
                            }
                            .into(),
                        );
                        w.set_assistant_state(
                            if task {
                                "Working · native worker connected"
                            } else {
                                "Ready"
                            }
                            .into(),
                        );
                    }
                });
                let weak = window.as_weak();
                window.on_stop_task(move || {
                    if let Some(w) = weak.upgrade() {
                        w.set_can_retry(true);
                        w.set_assistant_state("Stopped · isolated fixture".into());
                    }
                });
                let weak = window.as_weak();
                window.on_retry_task(move || {
                    if let Some(w) = weak.upgrade() {
                        w.set_can_retry(false);
                        w.set_assistant_state("Retry queued · isolated fixture".into());
                    }
                });
                if std::env::var_os("ECHO_GUI_HERMES_RETRY").is_some() {
                    window.invoke_select_tab("task".into());
                    window.invoke_stop_task();
                }
                let weak = window.as_weak();
                window.on_view_changed(move |output| {
                    if let Some(w) = weak.upgrade() {
                        w.set_show_output(output);
                    }
                });
                window.set_listening(true);
                window.set_live_transcript("This first sentence is visible immediately.".into());
                let setup = HermesSettingsWindow::new()?;
                setup.set_models(ModelRc::new(VecModel::from(vec![
                    "Current assistant model".into(),
                ])));
                setup.set_selected_model("Current assistant model".into());
                let weak_update = setup.as_weak();
                setup.on_update(move || {
                    if let Some(w) = weak_update.upgrade() {
                        w.set_status(
                            "Hermes update requested. Conversations and settings are preserved."
                                .into(),
                        );
                    }
                });
                if std::env::var_os("ECHO_GUI_HERMES_SETTINGS").is_some() {
                    return inspect(setup);
                }
                let weak_setup = setup.as_weak();
                window.on_hermes_settings(move || {
                    if let Some(w) = weak_setup.upgrade() {
                        let _ = w.show();
                    }
                });
                let tab_settings = HermesTabSettingsWindow::new()?;
                tab_settings.set_voices(ModelRc::new(VecModel::from(vec![
                    "Settings default".into(),
                    "Alba".into(),
                ])));
                tab_settings.set_selected_voice("Settings default".into());
                tab_settings.set_has_task(true);
                let weak_tab = tab_settings.as_weak();
                window.on_tab_settings(move || {
                    if let Some(w) = weak_tab.upgrade() {
                        let _ = w.show();
                    }
                });
                let hidden = HermesHiddenTabsWindow::new()?;
                let weak_hidden = hidden.as_weak();
                window.on_list_hidden(move || {
                    if let Some(w) = weak_hidden.upgrade() {
                        let _ = w.show();
                    }
                });
                let history = HermesHistoryWindow::new()?;
                history.set_results(ModelRc::new(VecModel::from(vec![HistorySearchResult {
                    message: "1".into(),
                    offset: "0".into(),
                    preview: "Assistant · Native task progress remains in this conversation."
                        .into(),
                }])));
                let weak_history = history.as_weak();
                window.on_history_tools(move || {
                    if let Some(w) = weak_history.upgrade() {
                        let _ = w.show();
                    }
                });
                if std::env::var_os("ECHO_GUI_STRESS").is_some() {
                    stress_assistant(&window)?;
                }
                window.set_listening(false);
                window.set_live_transcript("".into());
                if let Ok(transcript) = std::env::var("ECHO_GUI_LIVE_TRANSCRIPT") {
                    window.set_listening(true);
                    window.set_live_transcript(transcript.into());
                }
                window.set_spoken_text(std::env::var("ECHO_GUI_SPOKEN_TEXT").unwrap_or_else(|_| "I'll check the project while you continue. You can follow its progress in the other tab.".into()).into());
                window.set_speech_model_status(
                    std::env::var("ECHO_GUI_SPEECH_MODEL_STATUS")
                        .unwrap_or_else(|_| "Pocket TTS ready".into())
                        .into(),
                );
                window
                    .set_speech_model_ready(window.get_speech_model_status() == "Pocket TTS ready");
                return inspect(window);
            }
            if let Ok(status) = std::env::var("ECHO_GUI_SPEECH_MODEL_STATUS") {
                window.set_speech_model_ready(status == "Pocket TTS ready");
                window.set_speech_model_status(status.into());
                let weak = window.as_weak();
                window.on_submit(move |_| {
                    if let Some(window) = weak.upgrade() {
                        window.set_speech_model_status("Pocket TTS ready".into());
                        window.set_speech_model_ready(true);
                    }
                });
            }
            if let Some(path) = std::env::var_os("ECHO_GUI_AVATAR_PATH") {
                window.set_avatar(slint::Image::load_from_path(std::path::Path::new(&path))?);
            }
            window.set_spoken_text("Here is a summary of your notes. You can read the details and open the report below.".into());
            window.set_tool_activity("Agent: Completed file inspection".into());
            window.set_graph_run_nodes(graph_progress_fixture(5));
            window.set_graph_progress_nodes(graph_progress_fixture(5));
            window.set_graph_step(5);
            let weak = window.as_weak();
            window.on_start_listening(move || {
                if let Some(window) = weak.upgrade() {
                    let current = (window.get_graph_step() + 1) % 15;
                    window.set_graph_step(current);
                    window.set_graph_progress_nodes(graph_progress_fixture(current as usize));
                }
            });
            window.set_assistant_state("Ready".into());
            if let Ok(transcript) = std::env::var("ECHO_GUI_LIVE_TRANSCRIPT") {
                window.set_listening(true);
                window.set_assistant_state("Listening…".into());
                window.set_live_transcript(transcript.into());
            }
            window.set_messages(ModelRc::new(VecModel::from(vec![
                AssistantMessage {
                    files: Default::default(),
                    text: "Can you summarize my notes?".into(),
                    from_user: true,
                    state: "".into(),
                },
                AssistantMessage {
                    files: ModelRc::new(VecModel::from(vec![AssistantFile {
                        id: "report".into(),
                        name: "Meeting notes.md".into(),
                    }])),
                    text: "I can help with that. Here is the summary so far…".into(),
                    from_user: false,
                    state: "".into(),
                },
            ])));
            let weak = window.as_weak();
            slint::Timer::single_shot(std::time::Duration::from_millis(300), move || {
                if let Some(window) = weak.upgrade() {
                    window.invoke_scroll_bottom();
                }
            });
            if std::env::var_os("ECHO_GUI_STRESS").is_some() {
                stress_assistant(&window)?;
            }
            inspect(window)
        }
        Some("assistant-output") => {
            let window = AssistantPipelineWindow::new()?;
            if std::env::var_os("ECHO_GUI_TTS_CONFIG").is_some() {
                window.set_configuration_role(4);
            }
            let weak = window.as_weak();
            window.on_test_voice(move || {
                if let Some(window) = weak.upgrade() {
                    let phrase = window.get_voice_test_text();
                    window.set_voice_status(
                        format!("Preview requested for: {phrase} (isolated UI; no audio played)")
                            .into(),
                    );
                }
            });
            window.set_model_options(ModelRc::new(VecModel::from(vec![
                "Fast model · example".into(),
                "Strong model · example".into(),
                "Laya · local decision".into(),
            ])));
            window.set_answer_model_options(window.get_model_options());
            window.set_decision_router(true);
            window.set_fast_answer_name("Fast model · example".into());
            window.set_router_name("Laya · local decision".into());
            window.set_answer_name("Strong model · example".into());
            window.set_router_local_model(true);
            window.set_answer_local_model(true);
            window.set_instructions("You are Echo. Answer clearly and concisely.".into());
            window.set_voice_path("Example voice.wav".into());
            inspect(window)
        }
        Some("mcp-manager") => {
            let window = McpManagerWindow::new()?;
            window.set_server_options(ModelRc::new(VecModel::from(vec![
                "Local memory · example".into()
            ])));
            window.set_selected_server("Local memory · example".into());
            window.set_server_name("Local memory".into());
            window.set_command_text("C:\\Echo\\memory-palace.exe".into());
            inspect(window)
        }
        Some("agent-approval") => {
            let window = AgentApprovalWindow::new()?;
            window.set_action_text("Write a file\n\nPath: C:\\Echo\\report.md\n\nContent: A summary of the meeting notes. Review this exact action before allowing it.".into());
            window.set_scope_label("write_file: C:\\Echo\\report.md".into());
            let weak = window.as_weak();
            window.on_decide(move |allow, scope| {
                if let Some(window) = weak.upgrade() {
                    window
                        .set_action_text(format!("Decision: allow={allow}, scope={scope}").into());
                }
            });
            inspect(window)
        }
        Some("component-setup") => {
            let window = ComponentSetupWindow::new()?;
            fn rows(page: i32) -> ModelRc<ComponentChoice> {
                let (id, title, description, tradeoff, download) = match page {
                    0 => (
                        "core",
                        "CPU transcription — Parakeet v2",
                        "Private English transcription without a GPU.",
                        "CPU processing is slower than GPU transcription; English only.",
                        "482 MB",
                    ),
                    1 => (
                        "ultra",
                        "Parakeet Ultra",
                        "GPU transcription with 25-language support.",
                        "Requires a compatible NVIDIA GPU and a large private GPU runtime.",
                        "1.26 GB plus runtime",
                    ),
                    2 => (
                        "pocket",
                        "Pocket TTS — standard voice",
                        "Local streaming speech with Alba; no API key.",
                        "Extra model storage and CPU usage.",
                        "236 MB",
                    ),
                    3 => (
                        "browser",
                        "Browser automation",
                        "Websites through a separate managed browser.",
                        "Downloads private Node and Chromium; changes require approval.",
                        "Approximately 350 MB",
                    ),
                    _ => return ModelRc::default(),
                };
                let mut choices = vec![ComponentChoice {
                    id: id.into(),
                    title: title.into(),
                    description: description.into(),
                    tradeoff: tradeoff.into(),
                    download: download.into(),
                    selected: page == 0,
                    installed: false,
                }];
                if page == 0 {
                    choices.push(ComponentChoice {
                        id: "punctuation".into(),
                        title: "Offline punctuation and capitalization".into(),
                        description: "Adds sentence marks and casing locally.".into(),
                        tradeoff: "English only; another processing step.".into(),
                        download: "31 MB".into(),
                        selected: true,
                        installed: false,
                    });
                }
                if page == 2 {
                    for title in ["Custom voice import", "Laya decision model"] {
                        choices.push(ComponentChoice {
                            id: title.into(),
                            title: title.into(),
                            description: "Optional local assistant component.".into(),
                            tradeoff: "Additional download and model storage.".into(),
                            download: "Approximately 900 MB".into(),
                            selected: false,
                            installed: false,
                        });
                    }
                }
                if page == 3 {
                    choices.push(ComponentChoice {
                        id: "desktop".into(),
                        title: "Windows desktop control".into(),
                        description: "Inspect and operate applications on this desktop.".into(),
                        tradeoff: "Application support varies; changes require approval.".into(),
                        download: "Cua Driver package".into(),
                        selected: false,
                        installed: false,
                    });
                }
                ModelRc::new(VecModel::from(choices))
            }
            window.set_choices(rows(0));
            let weak = window.as_weak();
            window.on_change_page(move |page| {
                if let Some(window) = weak.upgrade() {
                    window.set_page(page);
                    window.set_choices(rows(page));
                }
            });
            let weak = window.as_weak();
            window.on_install_selected(move || {
                if let Some(window) = weak.upgrade() {
                    window.set_status(
                        "Fixture: install selection received; no downloads performed.".into(),
                    );
                }
            });
            let weak = window.as_weak();
            window.on_close_setup(move || {
                if let Some(window) = weak.upgrade() {
                    let _ = window.hide();
                }
            });
            inspect(window)
        }
        _ => {
            eprintln!("Choose main, models, pipeline-editor, pipeline-step, pipeline-step-local, pipeline-step-model, hotkey, overlay, assistant-chat, assistant-pipeline, assistant-output, or mcp-manager.");
            Ok(())
        }
    }
}
