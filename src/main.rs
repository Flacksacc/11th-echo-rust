#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

#[cfg(test)]
mod agent_tests;
mod agent_tools;
mod artifacts;
mod assistant;
mod assistant_mcp;
mod assistant_text;
mod assistant_voice;
mod audio;
mod component_pool;
mod component_setup;
mod components;
mod diagnostics;
mod gemini;
mod graph;
#[cfg(windows)]
mod graph_hotkeys;
mod graph_ui;
mod hermes;
mod hermes_phrases;
mod hermes_runtime;
mod hermes_tabs;
mod hermes_ui;
mod hotkey;
mod icon_animation;
mod injector;
mod model_creator;
mod model_preload;
mod model_ui;
mod models;
mod models_transport;
mod pipeline;
mod pipeline_editor;
mod pipeline_ui;
mod pipelines;
mod post_processing;
mod resources;
mod response_channels;
mod settings;
mod startup;
mod state;
mod system_one;
mod transcription;
mod updater;

use chrono::Local;
use pipeline::TranscriptPipeline;
use settings::{
    load_settings, load_transcript_history, save_settings, save_transcript_history, AppSettings,
    TranscriptHistoryEntry, MAX_TRANSCRIPT_HISTORY,
};
use slint::{CloseRequestResponse, Color, ComponentHandle, Model, ModelRc, SharedString, VecModel};
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

const MAX_TRANSCRIPT_DISPLAY_LINES: usize = 100;

pub(crate) fn assistant_choice_label(name: &str, id: &str) -> String {
    use sha2::Digest;
    let hash = sha2::Sha256::digest(id.as_bytes());
    format!(
        "{name} · {:02x}{:02x}{:02x}{:02x}",
        hash[0], hash[1], hash[2], hash[3]
    )
}

fn populate_mcp_manager(window: &McpManagerWindow, settings: &AppSettings, selected_id: &str) {
    let choices = settings
        .assistant_mcp_servers
        .iter()
        .map(|server| assistant_choice_label(&server.name, &server.id).into())
        .collect::<Vec<SharedString>>();
    window.set_server_options(ModelRc::new(VecModel::from(choices)));
    if let Some(server) = settings
        .assistant_mcp_servers
        .iter()
        .find(|server| server.id == selected_id)
    {
        window.set_selected_server(assistant_choice_label(&server.name, &server.id).into());
        window.set_server_name(server.name.clone().into());
        window.set_server_enabled(server.enabled);
        window.set_environment_text(
            serde_json::to_string_pretty(&server.environment)
                .unwrap()
                .into(),
        );
        window.set_headers_text(
            serde_json::to_string_pretty(&server.headers)
                .unwrap()
                .into(),
        );
        match &server.transport {
            assistant_mcp::McpTransport::Stdio { command, args } => {
                window.set_transport_name("Stdio".into());
                window.set_command_text(command.clone().into());
                window.set_args_text(args.join("\n").into());
                window.set_url_text("".into());
            }
            assistant_mcp::McpTransport::Http { url } => {
                window.set_transport_name("Streamable HTTP".into());
                window.set_command_text("".into());
                window.set_args_text("".into());
                window.set_url_text(url.clone().into());
            }
        }
    } else {
        window.set_selected_server("".into());
        window.set_server_name("".into());
        window.set_transport_name("Stdio".into());
        window.set_command_text("".into());
        window.set_args_text("".into());
        window.set_url_text("".into());
        window.set_server_enabled(true);
        window.set_environment_text("{}".into());
        window.set_headers_text("{}".into());
    }
}

fn mcp_server_from_window(
    window: &McpManagerWindow,
    settings: &AppSettings,
) -> assistant_mcp::McpServer {
    let selected = window.get_selected_server().to_string();
    let id = settings
        .assistant_mcp_servers
        .iter()
        .find(|server| assistant_choice_label(&server.name, &server.id) == selected)
        .map(|server| server.id.clone())
        .unwrap_or_else(pipelines::new_id);
    let transport = if window.get_transport_name().as_str() == "Stdio" {
        assistant_mcp::McpTransport::Stdio {
            command: window.get_command_text().trim().to_string(),
            args: window
                .get_args_text()
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect(),
        }
    } else {
        assistant_mcp::McpTransport::Http {
            url: window.get_url_text().trim().to_string(),
        }
    };
    assistant_mcp::McpServer {
        id: id.clone(),
        name: window.get_server_name().trim().to_string(),
        transport,
        enabled: window.get_server_enabled(),
        discovered_tools: settings
            .assistant_mcp_servers
            .iter()
            .find(|s| s.id == id)
            .map(|s| s.discovered_tools.clone())
            .unwrap_or_default(),
        environment: serde_json::from_str(window.get_environment_text().as_str())
            .unwrap_or_default(),
        headers: serde_json::from_str(window.get_headers_text().as_str()).unwrap_or_default(),
    }
}

struct VoiceRecording {
    capture: audio::AudioCapture,
    samples: Arc<Mutex<Vec<i16>>>,
    collector: thread::JoinHandle<()>,
}

fn finish_voice_recording(recording: VoiceRecording) -> Result<std::path::PathBuf, String> {
    recording
        .capture
        .begin_shutdown()
        .join()
        .map_err(|_| "Microphone capture stopped unexpectedly.".to_string())?;
    recording
        .collector
        .join()
        .map_err(|_| "Voice recording stopped unexpectedly.".to_string())?;
    let samples = recording.samples.lock().unwrap();
    if samples.len() < 16_000 {
        return Err("Record at least one second of speech.".into());
    }
    let dir = settings::settings_path().with_file_name("assistant_voices");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let destination = dir.join(format!("{}.wav", pipelines::new_id()));
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(&destination, spec).map_err(|e| e.to_string())?;
    for &sample in samples.iter() {
        writer.write_sample(sample).map_err(|e| e.to_string())?;
    }
    writer.finalize().map_err(|e| e.to_string())?;
    Ok(destination)
}

fn discard_voice_recording(recording: VoiceRecording) {
    let _ = recording.capture.begin_shutdown().join();
    let _ = recording.collector.join();
}

fn refresh_assistant_chat(
    window: &slint::Weak<AssistantChatWindow>,
    conversation: &Arc<Mutex<assistant::Conversation>>,
) {
    let conversation = conversation.clone();
    let _ = window.upgrade_in_event_loop(move |chat| {
        let follow = chat.get_chat_at_bottom();
        let scroll = chat.get_chat_scroll_position();
        // Read on the UI thread: an older queued refresh must not restore stale rows.
        let messages = conversation
            .lock()
            .unwrap()
            .messages
            .iter()
            .map(|m| AssistantMessage {
                text: m.text.clone().into(),
                from_user: m.from_user,
                files: ModelRc::new(VecModel::from(
                    m.files
                        .iter()
                        .map(|f| AssistantFile {
                            id: f.id.clone().into(),
                            name: f.name.clone().into(),
                        })
                        .collect::<Vec<_>>(),
                )),
                state: if m.interrupted {
                    "Interrupted"
                } else if m.complete {
                    ""
                } else {
                    "Thinking…"
                }
                .into(),
            })
            .collect::<Vec<_>>();
        let current = chat.get_messages();
        if let Some(model) = current
            .as_any()
            .downcast_ref::<VecModel<AssistantMessage>>()
        {
            while model.row_count() > messages.len() {
                model.remove(model.row_count() - 1);
            }
            for (i, message) in messages.into_iter().enumerate() {
                if i == model.row_count() {
                    model.push(message);
                } else if !model.row_data(i).is_some_and(|old| {
                    old.text == message.text
                        && old.state == message.state
                        && old.from_user == message.from_user
                        && old.files.row_count() == message.files.row_count()
                        && old
                            .files
                            .iter()
                            .zip(message.files.iter())
                            .all(|(a, b)| a.id == b.id && a.name == b.name)
                }) {
                    model.set_row_data(i, message);
                }
            }
        } else {
            chat.set_messages(ModelRc::new(VecModel::from(messages)));
        }
        let weak = chat.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(20), move || {
            if let Some(chat) = weak.upgrade() {
                if follow && chat.get_chat_scroll_position() == scroll {
                    chat.invoke_scroll_bottom();
                }
            }
        });
    });
}

fn graph_run_rows(graph: &graph::Graph, listening: bool) -> ModelRc<GraphRunRow> {
    let relevant = graph.relevant();
    ModelRc::new(VecModel::from(
        graph
            .order()
            .unwrap_or_else(|_| (0..graph.nodes.len()).collect())
            .into_iter()
            .map(|i| &graph.nodes[i])
            .filter(|n| relevant.contains(&n.id))
            .map(|n| GraphRunRow {
                id: n.id.clone().into(),
                title: n.title().into(),
                state: if listening && n.transcription() {
                    "Running"
                } else {
                    "Waiting"
                }
                .into(),
            })
            .collect::<Vec<_>>(),
    ))
}
fn graph_progress_rows(nodes: &ModelRc<GraphRunRow>, current: i32) -> ModelRc<GraphRunRow> {
    let rows = nodes.iter().collect::<Vec<_>>();
    if rows.is_empty() {
        return ModelRc::default();
    }
    let current = (current.max(0) as usize).min(rows.len() - 1);
    let mut visible = Vec::with_capacity(4);
    if let Some(previous) = rows[..current]
        .iter()
        .rev()
        .find(|row| row.state == "Complete")
    {
        visible.push(previous.clone());
    }
    visible.push(rows[current].clone());
    visible.extend(
        rows[current + 1..]
            .iter()
            .filter(|row| row.state == "Waiting" || row.state == "Running")
            .take(2)
            .cloned(),
    );
    ModelRc::new(VecModel::from(visible))
}
fn update_graph_run_state(model: &ModelRc<GraphRunRow>, id: &str, state: &str) {
    if let Some(model) = model.as_any().downcast_ref::<VecModel<GraphRunRow>>() {
        if let Some(i) = model.iter().position(|row| row.id.as_str() == id) {
            if let Some(mut row) = model.row_data(i) {
                row.state = state.into();
                model.set_row_data(i, row);
            }
        }
    }
}
fn graph_uses_assistant(graph: &graph::Graph) -> bool {
    let relevant = graph.relevant();
    graph
        .nodes
        .iter()
        .any(|n| relevant.contains(&n.id) && matches!(n.kind, graph::Kind::AssistantDisplay))
}
fn begin_assistant_graph_listening(
    chat: &AssistantChatWindow,
    graph: &graph::Graph,
    settings: &AppSettings,
) {
    let rows = graph_run_rows(graph, true);
    let current = rows
        .iter()
        .position(|row| row.state == "Running")
        .unwrap_or(0) as i32;
    chat.set_graph_run_nodes(rows);
    chat.set_graph_step(current);
    chat.set_graph_progress_nodes(graph_progress_rows(&chat.get_graph_run_nodes(), current));
    chat.set_live_transcript("".into());
    chat.present_listening(true);
    chat.present_spoken_text("".into());
    chat.set_tool_activity("".into());
    chat.present_assistant_state("Listening…".into());
    chat.set_speech_status(graph_speech_status(graph, settings).into());
    let _ = chat.show();
    chat.invoke_scroll_bottom();
}
fn prepare_transcription_overlay(
    overlay: &TranscriptOverlayWindow,
    pipeline: &pipelines::Pipeline,
    microphone: &str,
    assistant: bool,
) {
    if assistant {
        overlay.set_is_visible(false);
        hide_overlay_window(overlay);
        return;
    }
    overlay.set_graph_run_nodes(
        pipeline
            .graph
            .as_ref()
            .map(|g| graph_run_rows(g, true))
            .unwrap_or_default(),
    );
    overlay.set_graph_progress_nodes(graph_progress_rows(&overlay.get_graph_run_nodes(), 0));
    overlay.set_graph_status("".into());
    overlay.set_pipeline_cards(if pipeline.graph.is_some() {
        ModelRc::default()
    } else {
        pipeline_ui::cards(&pipeline.steps)
    });
    overlay.set_pipeline_name(pipeline.name.clone().into());
    overlay.set_pipeline_step(0);
    overlay.set_pipeline_failed(false);
    overlay.set_step_complete(false);
    reset_overlay_to_listening(overlay, microphone);
}
fn graph_speech_status(graph: &graph::Graph, settings: &AppSettings) -> String {
    let relevant = graph.relevant();
    let speech: Vec<_> = graph
        .nodes
        .iter()
        .filter(|n| relevant.contains(&n.id))
        .filter_map(|n| {
            if let graph::Kind::Speech { voice, .. } = &n.kind {
                Some(voice)
            } else {
                None
            }
        })
        .collect();
    if speech.is_empty() {
        return "Speech off: add a Speech node and connect it to the answer in the graph editor."
            .into();
    }
    if !assistant_voice::installed() {
        return "Speech setup needed: install Pocket TTS from the Speech node's voice settings."
            .into();
    }
    if speech
        .iter()
        .any(|voice| !assistant_voice::voice_selection_valid(&voices::resolve(voice, settings)))
    {
        return "Speech setup needed: a selected voice is missing. Select another voice or use Alba.".into();
    }
    String::new()
}

// This boundary combines run inputs with the independent UI/conversation handles.
#[allow(clippy::too_many_arguments)]
async fn run_graph_chat(
    graph: &graph::Graph,
    input: String,
    settings: &AppSettings,
    chat: slint::Weak<AssistantChatWindow>,
    conversation: Arc<Mutex<assistant::Conversation>>,
    ui: slint::Weak<AppWindow>,
    cancel: Arc<dyn Fn() -> bool + Send + Sync>,
    progress: graph::Observer,
) -> Result<graph::RunResult, String> {
    let initial_rows = graph_run_rows(graph, false).iter().collect::<Vec<_>>();
    let speech_status = graph_speech_status(graph, settings);
    let initial_cancel = cancel.clone();
    let _ = chat.upgrade_in_event_loop(move |chat| {
        if initial_cancel() {
            return;
        }
        chat.set_graph_run_nodes(ModelRc::new(VecModel::from(initial_rows)));
        chat.set_speech_status(speech_status.into());
        chat.present_listening(false);
        chat.set_graph_step(0);
        chat.set_graph_progress_nodes(graph_progress_rows(&chat.get_graph_run_nodes(), 0));
        chat.present_assistant_state("Processing graph…".into());
    });
    let context = conversation
        .lock()
        .unwrap()
        .messages
        .iter()
        .filter(|m| m.complete)
        .rev()
        .take(16)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|m| {
            format!(
                "{}: {}",
                if m.from_user { "User" } else { "Assistant" },
                m.text
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let displays = graph
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, graph::Kind::AssistantDisplay))
        .map(|n| n.id.clone())
        .collect::<std::collections::HashSet<_>>();
    let turn = Arc::new(Mutex::new(None::<(u64, u64)>));
    let update_turn = turn.clone();
    let update_conversation = conversation.clone();
    let update_chat = chat.clone();
    let user_input = input.clone();
    let speech_nodes = graph
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, graph::Kind::Speech { .. }))
        .map(|n| n.id.clone())
        .collect::<std::collections::HashSet<_>>();
    let has_speech = !speech_nodes.is_empty();
    let node_titles = graph
        .nodes
        .iter()
        .map(|node| (node.id.clone(), node.title()))
        .collect::<std::collections::HashMap<_, _>>();
    let display_cancel = cancel.clone();
    let observer: graph::Observer = Arc::new(move |event| {
        if display_cancel() {
            return;
        }
        match &event {
            graph::Event::Text { node, text } if displays.contains(node) => {
                let mut turn = update_turn.lock().unwrap();
                let mut conv = update_conversation.lock().unwrap();
                if display_cancel() {
                    return;
                }
                let (generation, id) = *turn.get_or_insert_with(|| {
                    let (g, id, _) = conv.start_turn(user_input.clone());
                    (g, id)
                });
                conv.update(generation, id, text.clone(), false);
                drop(conv);
                drop(turn);
                refresh_assistant_chat(&update_chat, &update_conversation);
                let cancelled = display_cancel.clone();
                let _ = update_chat.upgrade_in_event_loop(move |chat| {
                    if cancelled() {
                        return;
                    }
                    chat.present_listening(false);
                    chat.set_live_transcript("".into());
                    chat.present_assistant_state("Receiving…".into());
                    let _ = chat.show();
                });
            }
            graph::Event::Assistant { node, text } if displays.contains(node) => {
                let spoken = text.clone();
                let cancelled = display_cancel.clone();
                let pending_only = !has_speech;
                let mut turn = update_turn.lock().unwrap();
                let mut conv = update_conversation.lock().unwrap();
                let (_, id) = *turn.get_or_insert_with(|| {
                    let (g, id, _) = conv.start_turn(user_input.clone());
                    (g, id)
                });
                if let Some(message) = conv.messages.iter_mut().find(|m| m.id == id) {
                    message.assistant_text = spoken.clone();
                }
                drop(conv);
                drop(turn);
                refresh_assistant_chat(&update_chat, &update_conversation);
                let _ = update_chat.upgrade_in_event_loop(move |chat| {
                    if !cancelled() && pending_only {
                        chat.present_spoken_text(spoken.into());
                    }
                });
            }
            graph::Event::Files { node, files } if displays.contains(node) => {
                let mut turn = update_turn.lock().unwrap();
                let mut conv = update_conversation.lock().unwrap();
                let (g, id) = *turn.get_or_insert_with(|| {
                    let (g, id, _) = conv.start_turn(user_input.clone());
                    (g, id)
                });
                let _ = g;
                if let Some(message) = conv.messages.iter_mut().find(|m| m.id == id) {
                    message.files = files.clone();
                }
                drop(conv);
                drop(turn);
                refresh_assistant_chat(&update_chat, &update_conversation);
            }
            graph::Event::Speaking { node, text } if speech_nodes.contains(node) => {
                let text = text.clone();
                let cancelled = display_cancel.clone();
                let _ = update_chat.upgrade_in_event_loop(move |chat| {
                    if !cancelled() {
                        chat.present_spoken_text(text.into());
                    }
                });
            }
            graph::Event::Activity { node, text } => {
                let text = format!(
                    "{}: {text}",
                    node_titles.get(node).map(String::as_str).unwrap_or("Agent")
                );
                let cancelled = display_cancel.clone();
                let _ = update_chat.upgrade_in_event_loop(move |chat| {
                    if !cancelled() {
                        chat.set_tool_activity(text.into());
                    }
                });
            }
            graph::Event::Approval { node, request } => {
                let _ = node;
                show_agent_approval(request.clone());
            }
            graph::Event::Error { node, message } => {
                let id = node.clone();
                let speech_error = speech_nodes
                    .contains(node)
                    .then(|| format!("Speech failed: {message}"));
                if let Some(message) = speech_error {
                    let cancelled = display_cancel.clone();
                    let _ = update_chat.upgrade_in_event_loop(move |chat| {
                        if !cancelled() {
                            chat.set_speech_status(message.into());
                        }
                    });
                }
                let state = format!("Failed: {message}");
                let cancelled = display_cancel.clone();
                let _ = ui.upgrade_in_event_loop(move |ui| {
                    if !cancelled() {
                        ui.invoke_graph_node_state(id.into(), state.into());
                    }
                });
            }
            graph::Event::State { node, state } => {
                let id = node.clone();
                let state = state.to_string();
                let chat_id = id.clone();
                let chat_state = state.clone();
                let cancelled = display_cancel.clone();
                let _ = update_chat.upgrade_in_event_loop(move |chat| {
                    if !cancelled() {
                        update_graph_run_state(&chat.get_graph_run_nodes(), &chat_id, &chat_state);
                        if chat_state == "Running" {
                            if let Some(i) = chat
                                .get_graph_run_nodes()
                                .iter()
                                .position(|row| row.id.as_str() == chat_id)
                            {
                                chat.set_graph_step(i as i32);
                            }
                        }
                        chat.set_graph_progress_nodes(graph_progress_rows(
                            &chat.get_graph_run_nodes(),
                            chat.get_graph_step(),
                        ));
                    }
                });
                let cancelled = display_cancel.clone();
                let _ = ui.upgrade_in_event_loop(move |ui| {
                    if !cancelled() {
                        ui.invoke_graph_node_state(id.into(), state.into());
                    }
                });
            }
            _ => {}
        }
        progress(event);
    });
    let result = graph::execute(graph, input, settings, context, observer, cancel.clone()).await;
    if cancel() {
        return result;
    }
    let status = match &result {
        Ok(result) if result.errors.is_empty() => "Ready".to_string(),
        Ok(result) => format!("Completed with node errors: {}", result.errors.join("; ")),
        Err(error) => format!("Graph failed: {error}"),
    };
    let final_cancel = cancel.clone();
    let _ = chat.upgrade_in_event_loop(move |chat| {
        if !final_cancel() {
            chat.present_assistant_state(status.into());
        }
    });
    if let Some((generation, id)) = *turn.lock().unwrap() {
        let mut conv = conversation.lock().unwrap();
        match &result {
            Ok(result) => conv.update(generation, id, result.text.clone(), true),
            Err(_) => {
                if let Some(message) = conv.messages.iter_mut().find(|m| m.id == id) {
                    message.interrupted = true;
                    message.complete = false;
                }
            }
        }
        drop(conv);
        refresh_assistant_chat(&chat, &conversation);
    }
    result
}

fn transcript_history_text(history: &[TranscriptHistoryEntry]) -> String {
    // Persisted history is already newest-first. Format once per commit,
    // away from the UI event loop, without creating controls per entry.
    let mut lines = Vec::with_capacity(MAX_TRANSCRIPT_DISPLAY_LINES);
    for entry in history {
        if !lines.is_empty() {
            lines.push(String::new());
        }
        let text = entry.display_text();
        lines.extend(
            text.lines()
                .take(MAX_TRANSCRIPT_DISPLAY_LINES - lines.len())
                .map(str::to_owned),
        );
        if lines.len() == MAX_TRANSCRIPT_DISPLAY_LINES {
            break;
        }
    }
    lines.join("\n")
}

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
    ToggleGraph(String),
    ToggleRecording,
    ToggleAssistant,
    StartRecording,
    StartAssistantRecording,
    AssistantSubmit(String),
    AssistantTranscriptSubmit(u64, String),
    StopRecording,
    ReconfigureWarmCapture,
    CapturePostRollElapsed(u64),
    CaptureFlushed(u64, Result<audio::CaptureFlushStats, String>),
    FinalizationTimedOut(u64),
    ProcessingStarted(u64),
}

fn app_command_name(command: &AppCommand) -> &'static str {
    match command {
        AppCommand::ToggleGraph(_) => "toggle_graph",
        AppCommand::ToggleRecording => "toggle_recording",
        AppCommand::ToggleAssistant => "toggle_assistant",
        AppCommand::StartRecording => "start_recording",
        AppCommand::StartAssistantRecording => "start_assistant_recording",
        AppCommand::AssistantSubmit(_) => "assistant_submit",
        AppCommand::AssistantTranscriptSubmit(_, _) => "assistant_transcript_submit",
        AppCommand::StopRecording => "stop_recording",
        AppCommand::ReconfigureWarmCapture => "reconfigure_warm_capture",
        AppCommand::CapturePostRollElapsed(_) => "capture_post_roll_elapsed",
        AppCommand::CaptureFlushed(_, _) => "capture_flushed",
        AppCommand::FinalizationTimedOut(_) => "finalization_timed_out",
        AppCommand::ProcessingStarted(_) => "processing_started",
    }
}

struct Session {
    assistant: bool,
    discard_assistant_output: Arc<AtomicBool>,
    epoch: u64,
    state: Arc<Mutex<RecordingState>>,
    audio_capture: Option<audio::AudioCapture>,
    warm_capture_attached: bool,
    audio_forward_stop_tx: Option<mpsc::UnboundedSender<()>>,
    network_stop_tx: Option<mpsc::UnboundedSender<transcription::TranscriptionCommand>>,
    transcript_pipeline: Arc<Mutex<TranscriptPipeline>>,
    task_abort_handles: Vec<tokio::task::AbortHandle>,
    finalization_watchdog: Option<tokio::task::AbortHandle>,
    text_processing_started: bool,
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
enum RecordingActivation {
    Start,
    Stop,
    Ignore,
}

fn recording_activation(active: Option<(bool, bool)>, assistant: bool) -> RecordingActivation {
    match active {
        None => RecordingActivation::Start,
        Some((true, false)) if assistant => RecordingActivation::Start,
        Some((is_assistant, true)) if !assistant || is_assistant => RecordingActivation::Stop,
        Some(_) => RecordingActivation::Ignore,
    }
}

async fn interrupt_assistant(
    active_session: &mut Option<Session>,
    session_epoch: &AtomicU64,
    warm_capture: Option<&audio::WarmAudioCapture>,
    tasks: &Mutex<Vec<tokio::task::AbortHandle>>,
    conversation: &Mutex<assistant::Conversation>,
) {
    // Invalidate callbacks before stopping tasks; aborting alone cannot retract
    // work that has already been queued on the UI thread.
    session_epoch.fetch_add(1, Ordering::SeqCst);
    assistant_voice::stop_playback();
    conversation.lock().unwrap().interrupt();
    for task in tasks.lock().unwrap().drain(..) {
        task.abort();
    }
    if let Some(mut session) = active_session.take() {
        session
            .discard_assistant_output
            .store(true, Ordering::SeqCst);
        session.cancel_finalization_watchdog();
        session.abort_tasks();
        if session.warm_capture_attached {
            if let Some(capture) = warm_capture {
                if let Err(error) = capture.detach(session.epoch).await {
                    echo_warn!("audio", "Assistant interruption detach failed: {}", error);
                }
            }
        }
        if let Some(capture) = session.audio_capture.take() {
            let worker = capture.begin_shutdown();
            let _ = tokio::task::spawn_blocking(move || worker.join()).await;
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
    text.trim().to_string()
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
    let next_height = (height
        + if overlay.get_pipeline_cards().row_count() > 0
            || overlay.get_graph_run_nodes().row_count() > 0
        {
            46
        } else {
            0
        })
    .max(overlay.get_caption_required_height().ceil() as i32)
    .clamp(OVERLAY_HEIGHT, 360);
    let previous_height = overlay.get_window_height();
    #[cfg(target_os = "windows")]
    let previous_position = overlay.window().position();
    let scale = overlay.window().scale_factor();
    let physical = slint::PhysicalSize::new(
        (OVERLAY_WIDTH as f32 * scale).round() as u32,
        (next_height as f32 * scale).round() as u32,
    );
    if overlay.window().size() != physical {
        overlay.window().set_size(physical);
    }
    if previous_height == next_height {
        return;
    }

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

fn show_model_loading_notice(overlay: &TranscriptOverlayWindow) {
    overlay.set_pipeline_failed(false);
    overlay.set_step_complete(false);
    overlay.set_sentence_text("Model is loading. No audio is being recorded. Please wait, then press your recording shortcut again.".into());
    overlay.set_microphone_name("Parakeet Ultra · Loading".into());
    overlay.set_is_system_message(true);
    overlay.set_is_error(false);
    overlay.set_audio_level(0.0);
    overlay.set_is_visible(true);
    set_overlay_height(overlay, OVERLAY_HEIGHT);
    show_overlay_without_activation(overlay);
}

fn ultra_recording_blocked(
    provider: transcription::TranscriptionProvider,
    model: transcription::LocalModel,
    status: &transcription::LocalEngineStatus,
) -> bool {
    matches!(
        provider,
        transcription::TranscriptionProvider::LocalSherpaOnnx
    ) && model.uses_gpu()
        && !matches!(status, transcription::LocalEngineStatus::Ready)
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
            if let Err(err) = configure_overlay_as_non_activating(&overlay) {
                echo_warn!("overlay", "Could not refresh frameless style: {err}");
            }
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
        use windows::Win32::UI::WindowsAndMessaging::FindWindowExW;
        let mut previous = HWND::default();
        loop {
            let hwnd = FindWindowExW(None, previous, None, windows::core::w!("Echo Live"));
            if hwnd.0 == 0 {
                return Err("Windows could not find this process's Echo live overlay".into());
            }
            let mut owner_process_id = 0;
            if GetWindowThreadProcessId(hwnd, Some(&mut owner_process_id)) != 0
                && owner_process_id == std::process::id()
            {
                return Ok(hwnd);
            }
            previous = hwnd;
        }
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
        use windows::Win32::UI::WindowsAndMessaging::{
            SetWindowPos, GWL_STYLE, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
            SWP_NOZORDER, WS_CAPTION, WS_THICKFRAME,
        };
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
        let frameless = style & !(WS_CAPTION.0 as isize | WS_THICKFRAME.0 as isize);
        if style != frameless {
            SetWindowLongPtrW(hwnd, GWL_STYLE, frameless);
            SetWindowPos(
                hwnd,
                None,
                0,
                0,
                0,
                0,
                SWP_FRAMECHANGED | SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER,
            )?;
        }
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

fn activity_log_refresh_needed(tab: i32, selected_bytes: i32, revision: u64, seen: u64) -> bool {
    tab == 2 && selected_bytes == 0 && revision != seen
}

fn select_ui_backend() -> Result<(), slint::PlatformError> {
    #[cfg(target_os = "windows")]
    if std::env::var_os("SLINT_BACKEND").is_none() {
        return slint::BackendSelector::new()
            .backend_name("winit".into())
            // Skia uses native font rasterization instead of FemtoVG's uneven
            // glyph atlas. Use it for every window, including the overlay.
            .renderer_name("skia".into())
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
    initial_speakers: assistant_voice::OutputDeviceSnapshot,
    command_tx: mpsc::UnboundedSender<AppCommand>,
) {
    thread::spawn(move || {
        let mut previous_snapshot = initial_snapshot;
        let mut previous_speakers = initial_speakers;
        loop {
            thread::sleep(std::time::Duration::from_millis(500));
            let snapshot = audio::input_device_snapshot();
            let replace_options = snapshot.devices != previous_snapshot.devices;
            let devices_changed =
                replace_options || snapshot.default_device != previous_snapshot.default_device;
            previous_snapshot = snapshot.clone();
            let speakers = assistant_voice::output_device_snapshot();
            let replace_speakers = speakers.devices != previous_speakers.devices;
            previous_speakers = speakers.clone();

            if ui
                .upgrade_in_event_loop(move |ui| {
                    synchronize_microphone_editor(&ui, &snapshot, replace_options);
                    synchronize_speaker_editor(&ui, &speakers, replace_speakers);
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

fn synchronize_speaker_editor(
    ui: &AppWindow,
    snapshot: &assistant_voice::OutputDeviceSnapshot,
    replace_options: bool,
) {
    if replace_options {
        ui.set_speaker_options(ModelRc::new(VecModel::from(
            snapshot
                .devices
                .iter()
                .cloned()
                .map(SharedString::from)
                .collect::<Vec<_>>(),
        )));
    }
    if ui.get_use_default_speaker() {
        let selected = snapshot
            .default_device
            .as_ref()
            .or_else(|| snapshot.devices.first())
            .cloned()
            .unwrap_or_default();
        if ui.get_selected_speaker().as_str() != selected {
            ui.set_selected_speaker(selected.into());
        }
    }
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
    next.selected_speaker = ui.get_selected_speaker().to_string();
    next.use_default_speaker = ui.get_use_default_speaker();
    next.output_volume = assistant_voice::normalize_output_volume(ui.get_output_volume());
    next.keep_microphone_ready = ui.get_keep_microphone_ready();
    next.hotkey_text = ui.get_hotkey_text().to_string();
    next.assistant_hotkey_text = ui.get_assistant_hotkey_text().to_string();
    next.hugging_face_token = ui.get_hugging_face_token_text().to_string();
    next.tts_voice_path = ui.get_tts_voice_path().to_string();
    next.tts_preload = ui.get_tts_preload();
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
    ui.set_selected_speaker(settings.selected_speaker.clone().into());
    ui.set_use_default_speaker(settings.use_default_speaker);
    ui.set_output_volume(assistant_voice::normalize_output_volume(
        settings.output_volume,
    ));
    ui.set_keep_microphone_ready(settings.keep_microphone_ready);
    ui.set_hotkey_text(settings.hotkey_text.clone().into());
    ui.set_assistant_hotkey_text(settings.assistant_hotkey_text.clone().into());
    ui.set_hugging_face_token_text(settings.hugging_face_token.clone().into());
    ui.set_tts_voice_path(settings.tts_voice_path.clone().into());
    voice_ui::refresh_main(ui);
    ui.set_tts_preload(settings.tts_preload);
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
    if draft.use_default_speaker && normalized_saved.use_default_speaker {
        normalized_saved.selected_speaker = draft.selected_speaker.clone();
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

fn install_tts_settings_entrypoint(ui: &AppWindow, assistant_editor: &AssistantPipelineWindow) {
    let ui_for_tts = ui.as_weak();
    let voice_editor = assistant_editor.as_weak();
    ui.on_configure_tts(move || {
        let (Some(ui), Some(editor)) = (ui_for_tts.upgrade(), voice_editor.upgrade()) else {
            return;
        };
        if editor.get_editing_voice()
            || editor.get_recording_voice()
            || editor.get_testing_voice()
            || editor.get_importing_voice()
        {
            return;
        }
        editor.set_configuration_role(4);
        editor.set_default_voice_settings(true);
        editor.set_voice_path(ui.get_tts_voice_path());
        voice_ui::refresh_editor(&editor);
        editor.set_preload_voice_on_start(ui.get_tts_preload());
        editor.set_error("".into());
        editor.set_voice_status(
            if assistant_voice::installed() {
                "Pocket TTS is installed."
            } else {
                "Install Pocket TTS to generate speech."
            }
            .into(),
        );
        let main = ui.as_weak();
        let weak = editor.as_weak();
        editor.on_save(move || {
            let (Some(main), Some(editor)) = (main.upgrade(), weak.upgrade()) else {
                return;
            };
            if editor.get_editing_voice()
                || editor.get_importing_voice()
                || editor.get_recording_voice()
            {
                return;
            }
            main.set_tts_voice_path(editor.get_voice_path());
            voice_ui::refresh_main(&main);
            main.set_tts_preload(editor.get_preload_voice_on_start());
            main.set_tts_model_status(
                if assistant_voice::installed() {
                    "Pocket TTS is installed."
                } else {
                    "Pocket TTS is not installed."
                }
                .into(),
            );
            main.set_settings_dirty(true);
            if editor.get_testing_voice() {
                assistant_voice::stop_playback();
                editor.set_testing_voice(false);
            }
            let _ = editor.hide();
        });
        let _ = editor.show();
    });
}

mod assistant_presentation;
mod avatar_bridge;
mod avatar_ui;
mod desktop_avatar;
mod node_settings;
mod reminders;
mod sayings;
mod sayings_ui;
mod speech_lane;
mod voice_ui;
mod voices;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let worker_args = std::env::args().skip(1).collect::<Vec<_>>();
    if worker_args
        .first()
        .is_some_and(|arg| arg == "--reminder-command")
    {
        if let Err(error) = reminders::cli() {
            println!("{}", serde_json::json!({"error":error}));
            std::process::exit(1);
        }
        return Ok(());
    }
    if worker_args.first().is_some_and(|arg| arg == "app-server")
        || (std::env::var_os("ECHO_CODEX_PROXY").is_some() && worker_args == ["--version"])
    {
        let code = hermes_runtime::codex_proxy(&worker_args).unwrap_or_else(|error| {
            eprintln!("Echo worker launcher failed: {error}");
            1
        });
        std::process::exit(code);
    }
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
            if std::env::args().any(|arg| arg == "--component-setup") {
                rfd::MessageDialog::new().set_title("Echo components").set_description("Echo is already running. Open Settings → Connections → Manage components in the running application.").show();
                show_existing_instance(&activation_event);
                return Ok(());
            }
            if std::env::args().any(|arg| arg == "--connect-laya") {
                return Err("Close Echo before connecting Laya.".into());
            }
            if std::env::args().any(|arg| arg == "--configure-hermes") {
                return Err("Close Echo before configuring its Hermes backend.".into());
            }
            echo_info!(
                "instance",
                "Echo is already running; activating the existing window"
            );
            show_existing_instance(&activation_event);
            return Ok(());
        }
    };
    if std::env::args().any(|arg| arg == "--connect-laya") {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        runtime.block_on(system_one::connect_local())?;
        return Ok(());
    }
    if std::env::args().any(|arg| arg == "--configure-hermes") {
        let mut configured = load_settings();
        hermes::enable_current_assistant(&mut configured)?;
        if !save_settings(&configured) {
            return Err("Could not save the Hermes backend configuration.".into());
        }
        println!("Hermes is configured with the current assistant model. Start Echo to connect.");
        return Ok(());
    }
    select_ui_backend()?;
    if std::env::args().any(|arg| arg == "--component-setup") {
        let fresh = !settings::settings_path().exists();
        let settings = if fresh {
            components::fresh_settings()
        } else {
            load_settings()
        };
        let shared = Arc::new(Mutex::new(settings));
        let window = component_setup::show(shared.clone())?;
        window.run()?;
        if fresh && !settings::settings_path().exists() {
            save_settings(&shared.lock().unwrap());
        }
        return Ok(());
    }

    let microphone_snapshot = audio::input_device_snapshot();
    let speaker_snapshot = assistant_voice::output_device_snapshot();
    let mut initial_settings = load_settings();
    if !settings::settings_path().exists() {
        initial_settings = components::fresh_settings();
    }
    assistant_voice::set_output_device(
        &initial_settings.selected_speaker,
        initial_settings.use_default_speaker,
    );
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
    if let Some(p) = initial_settings
        .pipelines
        .as_ref()
        .and_then(|set| set.active().ok())
    {
        if let Ok(c) = p.transcription_config(&initial_settings) {
            initial_settings.transcription_provider = c.provider.id().into();
            initial_settings.local_sherpa = c.local;
        }
    }
    let selected_microphone = initial_settings.selected_microphone.clone();
    let settings = Arc::new(Mutex::new(initial_settings.clone()));
    let initial_transcript_history = load_transcript_history();
    let transcript_history_store = Arc::new(Mutex::new(initial_transcript_history.clone()));
    let transcript_history_revision = Arc::new(AtomicU64::new(0));
    model_preload::PreloadPlan::from_settings(&initial_settings).start();

    #[cfg(target_os = "windows")]
    let hotkey_text = Arc::new(Mutex::new(initial_settings.hotkey_text.clone()));

    #[cfg(target_os = "windows")]
    let hotkey_manager = Rc::new(GlobalHotKeyManager::new().unwrap());
    #[cfg(target_os = "windows")]
    let hotkey_state = Rc::new(RefCell::new(None::<HotKey>));
    #[cfg(target_os = "windows")]
    let hotkey_id_state = Rc::new(RefCell::new(None::<u32>));
    #[cfg(target_os = "windows")]
    let assistant_hotkey_state = Rc::new(RefCell::new(None::<HotKey>));
    #[cfg(target_os = "windows")]
    let assistant_hotkey_id_state = Rc::new(RefCell::new(None::<u32>));

    #[cfg(target_os = "windows")]
    {
        if initial_settings
            .pipelines
            .as_ref()
            .is_some_and(|set| set.version != 5)
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
    }
    #[cfg(target_os = "windows")]
    if initial_settings
        .pipelines
        .as_ref()
        .is_some_and(|set| set.version != 5)
        && initial_settings.assistant_hotkey_text != initial_settings.hotkey_text
    {
        match apply_hotkey(
            &hotkey_manager,
            &mut assistant_hotkey_state.borrow_mut(),
            &initial_settings.assistant_hotkey_text,
        ) {
            Ok(id) => *assistant_hotkey_id_state.borrow_mut() = Some(id),
            Err(err) => echo_error!("hotkey", "Assistant hotkey registration failed: {}", err),
        }
    }

    #[cfg(windows)]
    let graph_hotkeys = Rc::new(RefCell::new(graph_hotkeys::GraphHotkeys::default()));
    #[cfg(windows)]
    {
        let specifications = crate::graph_ui::hotkey_specs(&initial_settings);
        if let Err(error) = graph_hotkeys
            .borrow_mut()
            .sync(&hotkey_manager, &specifications)
        {
            echo_warn!("hotkey", "{error}");
        }
    }
    let icons = icon_animation::Icons::load()?;
    #[cfg(target_os = "windows")]
    let (quit_item_id, settings_item_id, tray_icon) = {
        let tray_menu = Menu::new();
        let settings_item = MenuItem::new("Settings Tab", true, None);
        let quit_item = MenuItem::new("Quit", true, None);
        tray_menu.append_items(&[&settings_item, &quit_item])?;

        let icon = icons.idle.tray.clone();
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
    #[cfg(windows)]
    {
        let registry = graph_hotkeys.clone();
        let manager = hotkey_manager.clone();
        ui.on_sync_graph_hotkeys(move |json| {
            let result = serde_json::from_str::<Vec<(String, String)>>(&json)
                .map_err(|_| "Invalid graph shortcut configuration.".to_string())
                .and_then(|specs| registry.borrow_mut().sync(&manager, &specs));
            result.err().unwrap_or_default().into()
        });
    }

    model_ui::install(&ui, settings.clone());
    ui.set_active_tab(0);
    ui.set_status_text(if initial_settings.pipeline_migration_save_failed {
        "Your previous setup is active, but its pipeline could not be saved. Check settings folder permissions.".into()
    } else { "Idle".into() });
    ui.set_is_recording(false);
    #[cfg(target_os = "windows")]
    ui.set_hotkey_text(hotkey_text.lock().unwrap().clone().into());
    ui.set_assistant_hotkey_text(initial_settings.assistant_hotkey_text.clone().into());
    ui.set_hugging_face_token_text(initial_settings.hugging_face_token.clone().into());
    ui.set_tts_voice_path(initial_settings.tts_voice_path.clone().into());
    ui.set_tts_preload(initial_settings.tts_preload);
    ui.set_tts_model_status(
        if assistant_voice::installed() {
            "Pocket TTS is installed."
        } else {
            "Pocket TTS is not installed."
        }
        .into(),
    );
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
    ui.set_local_model_options(ModelRc::new(VecModel::from(
        transcription::LocalModel::ALL
            .into_iter()
            .map(|model| SharedString::from(model.label()))
            .collect::<Vec<_>>(),
    )));
    ui.set_ultra_download_description(transcription::ultra_download_description().into());
    let gpu_status_ui = ui.as_weak();
    thread::spawn(move || {
        let status = transcription::ultra_gpu_status();
        let _ =
            gpu_status_ui.upgrade_in_event_loop(move |ui| ui.set_ultra_gpu_status(status.into()));
    });
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
    // The settings workflow offers the model when the local provider is selected.
    // Keep startup non-modal; existing Local users are prompted on opening the
    // Speech Engine settings page if their model is missing.
    ui.set_local_download_prompt_visible(false);
    ui.set_local_download_progress_visible(false);
    ui.set_local_download_progress(0.0);
    if matches!(
        initial_provider,
        transcription::TranscriptionProvider::LocalSherpaOnnx
    ) && transcription::local_models_available(&initial_settings.local_sherpa)
        && initial_settings
            .pipelines
            .as_ref()
            .and_then(|set| set.active().ok())
            .and_then(|pipeline| pipeline.steps.first())
            .is_some_and(pipelines::Step::preload_on_start)
    {
        let preload_ui = ui.as_weak();
        let preload_config = initial_settings.local_sherpa.clone();
        thread::spawn(move || {
            if let Err(err) = transcription::wait_for_local_engine(&preload_config) {
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
    ui.set_selected_speaker(initial_settings.selected_speaker.clone().into());
    ui.set_use_default_speaker(initial_settings.use_default_speaker);
    ui.set_output_volume(assistant_voice::normalize_output_volume(
        initial_settings.output_volume,
    ));
    assistant_voice::set_output_volume(initial_settings.output_volume);
    synchronize_speaker_editor(&ui, &speaker_snapshot, true);
    let speaker_test_ui = ui.as_weak();
    ui.on_test_speakers(move || {
        if let Some(ui) = speaker_test_ui.upgrade() {
            if ui.get_speaker_test_running() { return; }
            let preferred = (!ui.get_use_default_speaker()).then(|| ui.get_selected_speaker().to_string());
            let volume = ui.get_output_volume();
            ui.set_speaker_test_running(true);
            ui.set_speaker_test_status("Playing a short test tone…".into());
            let weak = ui.as_weak();
            thread::spawn(move || {
                let result = assistant_voice::test_output_device(preferred.as_deref(), volume);
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    ui.set_speaker_test_running(false);
                    ui.set_speaker_test_status(match result {
                        Ok(()) => "Test tone finished. If you did not hear it, check the selected output and Windows volume.".into(),
                        Err(message) => message.into(),
                    });
                });
            });
        }
    });
    ui.set_keep_microphone_ready(initial_settings.keep_microphone_ready);
    ui.set_start_with_windows(initial_settings.start_with_windows);
    ui.set_update_checks_enabled(initial_settings.update_checks_enabled);
    ui.set_app_version(env!("CARGO_PKG_VERSION").into());
    let _resource_monitor = resources::install(&ui);
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
    monitor_microphones(
        ui.as_weak(),
        microphone_snapshot,
        speaker_snapshot,
        cmd_tx.clone(),
    );

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
    ui.set_transcript_history_text(transcript_history_text(&initial_transcript_history).into());
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
    #[cfg(target_os = "windows")]
    let hotkey_capture_assistant = Rc::new(Cell::new(false));

    let transcript_overlay = TranscriptOverlayWindow::new()?;
    let caption_overlay = transcript_overlay.as_weak();
    transcript_overlay.on_caption_layout_changed(move || {
        if let Some(overlay) = caption_overlay.upgrade() {
            set_overlay_height(&overlay, OVERLAY_HEIGHT);
        }
    });
    let copy_overlay = transcript_overlay.as_weak();
    transcript_overlay.on_copy_result(move || {
        if let Some(w) = copy_overlay.upgrade() {
            if let Ok(mut clipboard) = arboard::Clipboard::new() {
                match clipboard.set_text(w.get_sentence_text().to_string()) {
                    Ok(()) => w.set_failure_message("Last successful result copied.".into()),
                    Err(_) => w.set_failure_message("Could not copy the result. Try again.".into()),
                }
            }
        }
    });
    let dismiss_overlay = transcript_overlay.as_weak();
    transcript_overlay.on_dismiss_result(move || {
        if let Some(w) = dismiss_overlay.upgrade() {
            w.set_pipeline_failed(false);
            w.set_sentence_text("".into());
            w.set_failure_message("".into());
            w.set_is_visible(false);
            hide_overlay_window(&w);
        }
    });
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

    let assistant_chat = AssistantChatWindow::new()?;
    let speech_model_timer = slint::Timer::default();
    let speech_model_chat = assistant_chat.as_weak();
    speech_model_timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(100),
        move || {
            if let Some(chat) = speech_model_chat.upgrade() {
                let readiness = assistant_voice::model_readiness();
                let label = readiness.label();
                if chat.get_speech_model_status().as_str() != label {
                    chat.present_speech_model_status(label.into());
                    chat.present_speech_model_ready(
                        readiness == assistant_voice::ModelReadiness::Ready,
                    );
                }
            }
        },
    );
    assistant_chat.present_show_spoken_text(initial_settings.assistant_show_spoken_text);
    let visibility_settings = settings.clone();
    let visibility_chat = assistant_chat.as_weak();
    assistant_chat.on_spoken_visibility_changed(move |show| {
        let mut next = visibility_settings.lock().unwrap().clone();
        next.assistant_show_spoken_text = show;
        if save_settings(&next) {
            *visibility_settings.lock().unwrap() = next;
        } else if let Some(chat) = visibility_chat.upgrade() {
            chat.present_assistant_state("Could not save spoken text preference.".into());
        }
    });
    assistant_chat.on_open_file(|id| {
        if let Err(error) = artifacts::open(&id) {
            rfd::MessageDialog::new().set_description(error).show();
        }
    });
    assistant_chat.on_save_file(|id| {
        if let Err(error) = artifacts::save_as(&id) {
            rfd::MessageDialog::new().set_description(error).show();
        }
    });
    assistant_chat.on_show_file(|id| {
        if let Err(error) = artifacts::show_in_folder(&id) {
            rfd::MessageDialog::new().set_description(error).show();
        }
    });
    let assistant_conversation = Arc::new(Mutex::new(assistant::Conversation::default()));
    let clear_conversation = assistant_conversation.clone();
    let clear_chat = assistant_chat.as_weak();
    assistant_chat.on_clear_files(move || {
        if rfd::MessageDialog::new()
            .set_title("Clear generated files?")
            .set_description(
                "Generated attachments will no longer open. Original files are retained.",
            )
            .set_buttons(rfd::MessageButtons::YesNo)
            .show()
            == rfd::MessageDialogResult::Yes
        {
            if let Err(error) = artifacts::clear() {
                rfd::MessageDialog::new().set_description(error).show();
            } else {
                for message in &mut clear_conversation.lock().unwrap().messages {
                    message.files.retain(|file| !file.generated);
                }
                refresh_assistant_chat(&clear_chat, &clear_conversation);
            }
        }
    });
    if !initial_settings.assistant_avatar_path.is_empty() {
        if let Ok(avatar) = slint::Image::load_from_path(std::path::Path::new(
            &initial_settings.assistant_avatar_path,
        )) {
            assistant_chat.set_avatar(avatar);
        }
    }
    let assistant_reply_tasks = Arc::new(Mutex::new(Vec::<tokio::task::AbortHandle>::new()));
    let selection_timer = slint::Timer::default();
    {
        let shared = settings.clone();
        let settings = settings.clone();
        let chat = assistant_chat.as_weak();
        assistant_chat.on_graph_selected(move |label| {
            let mut snapshot = settings.lock().unwrap().clone();
            if let Some(set) = snapshot.pipelines.as_mut() {
                if let Some(p) = set
                    .pipelines
                    .iter()
                    .find(|p| assistant_choice_label(&p.name, &p.id) == label.as_str())
                {
                    set.active_assistant_id = p.id.clone();
                    if save_settings(&snapshot) {
                        *settings.lock().unwrap() = snapshot;
                    } else if let Some(chat) = chat.upgrade() {
                        chat.present_assistant_state("Could not save graph selection.".into());
                    }
                }
            }
        });
        let chat = assistant_chat.as_weak();
        selection_timer.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(500),
            move || {
                if let Some(chat) = chat.upgrade() {
                    let snapshot = shared.lock().unwrap();
                    if let Some(set) = &snapshot.pipelines {
                        chat.set_graph_options(ModelRc::new(VecModel::from(
                            set.pipelines
                                .iter()
                                .map(|p| assistant_choice_label(&p.name, &p.id).into())
                                .collect::<Vec<SharedString>>(),
                        )));
                        if let Some(p) = set
                            .pipelines
                            .iter()
                            .find(|p| p.id == set.active_assistant_id)
                            .or_else(|| set.pipelines.iter().find(|p| p.id == set.active_id))
                        {
                            chat.set_selected_graph(assistant_choice_label(&p.name, &p.id).into());
                        }
                    }
                }
            },
        );
    }
    {
        let main = ui.as_weak();
        let shared = settings.clone();
        assistant_chat.on_edit_graph(move || {
            let index = shared.lock().unwrap().pipelines.as_ref().and_then(|set| {
                let selected = set.active_assistant().or_else(|_| set.active()).ok()?;
                set.pipelines.iter().position(|p| p.id == selected.id)
            });
            if let (Some(main), Some(index)) = (main.upgrade(), index) {
                main.invoke_edit_pipeline(index as i32);
            }
        });
    }
    let assistant_submit_tx = cmd_tx.clone();
    assistant_chat.on_submit(move |text| {
        let _ = assistant_submit_tx.send(AppCommand::AssistantSubmit(text.to_string()));
    });
    let assistant_listen_tx = cmd_tx.clone();
    assistant_chat.on_start_listening(move || {
        assistant_voice::stop_playback();
        let _ = assistant_listen_tx.send(AppCommand::ToggleAssistant);
    });
    let assistant_conversation_for_new = assistant_conversation.clone();
    let assistant_tasks_for_new = assistant_reply_tasks.clone();
    let assistant_chat_for_new = assistant_chat.as_weak();
    assistant_chat.on_new_chat(move || {
        if let Some(chat) = assistant_chat_for_new.upgrade() {
            chat.present_listening(false);
            chat.set_live_transcript("".into());
            chat.set_graph_run_nodes(ModelRc::default());
            chat.set_graph_progress_nodes(ModelRc::default());
            chat.set_speech_status("".into());
        }
        assistant_voice::stop_playback();
        for task in assistant_tasks_for_new.lock().unwrap().drain(..) {
            task.abort();
        }
        assistant_conversation_for_new.lock().unwrap().reset();
        refresh_assistant_chat(&assistant_chat_for_new, &assistant_conversation_for_new);
    });
    assistant_chat.on_speed_changed(assistant_voice::set_playback_speed);
    assistant_chat.set_output_volume(assistant_voice::normalize_output_volume(
        initial_settings.output_volume,
    ));
    {
        let chat = assistant_chat.as_weak();
        ui.on_output_volume_changed(move |volume| {
            let volume = assistant_voice::normalize_output_volume(volume);
            assistant_voice::set_output_volume(volume);
            if let Some(chat) = chat.upgrade() {
                chat.set_output_volume(volume);
            }
        });
        let main = ui.as_weak();
        let chat = assistant_chat.as_weak();
        let shared = settings.clone();
        let save_timer = slint::Timer::default();
        assistant_chat.on_volume_changed(move |volume| {
            let volume = assistant_voice::normalize_output_volume(volume);
            assistant_voice::set_output_volume(volume);
            if let Some(main) = main.upgrade() {
                main.set_output_volume(volume);
            }
            let shared = shared.clone();
            let chat = chat.clone();
            save_timer.start(
                slint::TimerMode::SingleShot,
                std::time::Duration::from_millis(300),
                move || {
                    let mut snapshot = shared.lock().unwrap().clone();
                    snapshot.output_volume = volume;
                    if save_settings(&snapshot) {
                        *shared.lock().unwrap() = snapshot;
                    } else if let Some(chat) = chat.upgrade() {
                        chat.present_assistant_state("Could not save speech volume.".into());
                    }
                },
            );
        });
    }

    let assistant_editor = AssistantPipelineWindow::new()?;
    let edit_voice = AssistantPipelineWindow::new()?;
    let mcp_manager = McpManagerWindow::new()?;
    let components_ui = Rc::new(RefCell::new(None::<ComponentSetupWindow>));
    let components_settings = settings.clone();
    ui.on_manage_components(
        move || match component_setup::show(components_settings.clone()) {
            Ok(window) => *components_ui.borrow_mut() = Some(window),
            Err(error) => {
                rfd::MessageDialog::new()
                    .set_description(error.to_string())
                    .show();
            }
        },
    );
    let tools_ui = mcp_manager.as_weak();
    let tools_settings = settings.clone();
    ui.on_manage_agent_tools(move || {
        if let Some(window) = tools_ui.upgrade() {
            let current = tools_settings.lock().unwrap().clone();
            populate_mcp_manager(&window, &current, "");
            window.set_brave_key(current.brave_api_key.into());
            let _ = window.show();
        }
    });
    mcp_manager.set_brave_key(settings.lock().unwrap().brave_api_key.clone().into());
    let component_window = Rc::new(RefCell::new(None::<ComponentSetupWindow>));
    let component_settings = settings.clone();
    mcp_manager.on_manage_components(move || {
        match component_setup::show(component_settings.clone()) {
            Ok(window) => *component_window.borrow_mut() = Some(window),
            Err(error) => {
                rfd::MessageDialog::new()
                    .set_description(error.to_string())
                    .show();
            }
        }
    });
    let search_settings = settings.clone();
    let weak = mcp_manager.as_weak();
    mcp_manager.on_save_brave(move || {
        if let Some(window) = weak.upgrade() {
            let mut snapshot = search_settings.lock().unwrap().clone();
            snapshot.brave_api_key = window.get_brave_key().to_string();
            if save_settings(&snapshot) {
                *search_settings.lock().unwrap() = snapshot;
                window.set_status_text("Search key saved.".into());
            } else {
                window.set_status_text("Could not save search key.".into());
            }
        }
    });
    let mcp_manager_for_open = mcp_manager.as_weak();
    let settings_for_mcp_open = settings.clone();
    assistant_editor.on_open_mcp_manager(move || {
        if let Some(window) = mcp_manager_for_open.upgrade() {
            let current = settings_for_mcp_open.lock().unwrap().clone();
            let active = window.get_selected_server().to_string();
            let selected = current
                .assistant_mcp_servers
                .iter()
                .find(|server| assistant_choice_label(&server.name, &server.id) == active)
                .map(|server| server.id.as_str())
                .or_else(|| {
                    current
                        .assistant_mcp_servers
                        .first()
                        .map(|server| server.id.as_str())
                })
                .unwrap_or("");
            populate_mcp_manager(&window, &current, selected);
            window.set_status_text("".into());
            let _ = window.show();
        }
    });
    let mcp_manager_for_select = mcp_manager.as_weak();
    let settings_for_mcp_select = settings.clone();
    mcp_manager.on_select_server(move |label| {
        if let Some(window) = mcp_manager_for_select.upgrade() {
            let current = settings_for_mcp_select.lock().unwrap().clone();
            let id = current
                .assistant_mcp_servers
                .iter()
                .find(|server| assistant_choice_label(&server.name, &server.id) == label.as_str())
                .map(|server| server.id.as_str())
                .unwrap_or("");
            populate_mcp_manager(&window, &current, id);
            window.set_status_text("".into());
        }
    });
    let mcp_manager_for_new = mcp_manager.as_weak();
    let settings_for_mcp_new = settings.clone();
    mcp_manager.on_new_server(move || {
        if let Some(window) = mcp_manager_for_new.upgrade() {
            populate_mcp_manager(&window, &settings_for_mcp_new.lock().unwrap(), "");
            window.set_status_text("Enter a server, then save it.".into());
        }
    });
    let mcp_manager_for_save = mcp_manager.as_weak();
    let settings_for_mcp_save = settings.clone();
    mcp_manager.on_save_server(move || {
        let Some(window) = mcp_manager_for_save.upgrade() else {
            return;
        };
        let mut snapshot = settings_for_mcp_save.lock().unwrap().clone();
        for raw in [window.get_environment_text(), window.get_headers_text()] {
            if serde_json::from_str::<std::collections::BTreeMap<String, String>>(&raw).is_err() {
                window.set_status_text(
                    "Environment and headers must be JSON objects with string values.".into(),
                );
                return;
            }
        }
        let server = mcp_server_from_window(&window, &snapshot);
        if let Err(error) = server.validate() {
            window.set_status_text(error.into());
            return;
        }
        if let Some(existing) = snapshot
            .assistant_mcp_servers
            .iter_mut()
            .find(|existing| existing.id == server.id)
        {
            *existing = server.clone();
        } else {
            snapshot.assistant_mcp_servers.push(server.clone());
        }
        if save_settings(&snapshot) {
            *settings_for_mcp_save.lock().unwrap() = snapshot.clone();
            populate_mcp_manager(&window, &snapshot, &server.id);
            window.set_status_text("MCP server saved.".into());
        } else {
            window.set_status_text("Could not save MCP server.".into());
        }
    });
    let mcp_manager_for_remove = mcp_manager.as_weak();
    let settings_for_mcp_remove = settings.clone();
    mcp_manager.on_remove_server(move || {
        let Some(window) = mcp_manager_for_remove.upgrade() else {
            return;
        };
        let mut snapshot = settings_for_mcp_remove.lock().unwrap().clone();
        let selected = window.get_selected_server().to_string();
        snapshot
            .assistant_mcp_servers
            .retain(|server| assistant_choice_label(&server.name, &server.id) != selected);
        if save_settings(&snapshot) {
            *settings_for_mcp_remove.lock().unwrap() = snapshot.clone();
            populate_mcp_manager(&window, &snapshot, "");
            window.set_status_text("MCP server removed.".into());
        } else {
            window.set_status_text("Could not remove MCP server.".into());
        }
    });
    let mcp_manager_for_test = mcp_manager.as_weak();
    let settings_for_mcp_test = settings.clone();
    mcp_manager.on_test_server(move || {
        let Some(window) = mcp_manager_for_test.upgrade() else {
            return;
        };
        for raw in [window.get_environment_text(), window.get_headers_text()] {
            if serde_json::from_str::<std::collections::BTreeMap<String, String>>(&raw).is_err() {
                window.set_status_text(
                    "Environment and headers must be JSON objects with string values.".into(),
                );
                return;
            }
        }
        let server = mcp_server_from_window(&window, &settings_for_mcp_test.lock().unwrap());
        window.set_status_text("Connecting to MCP server…".into());
        let weak = mcp_manager_for_test.clone();
        let shared = settings_for_mcp_test.clone();
        std::thread::spawn(move || {
            let result = tokio::runtime::Runtime::new()
                .map_err(|e| e.to_string())
                .and_then(|runtime| {
                    runtime.block_on(async {
                        let client = assistant_mcp::connect(&server).await?;
                        let tools = client
                            .peer()
                            .list_all_tools()
                            .await
                            .map_err(|_| "Could not discover tools.".to_string())?;
                        let _ = client.cancel().await;
                        Ok(tools)
                    })
                });
            let status = match result {
                Ok(tools) => {
                    let status = if tools.is_empty() {
                        "Connected. This server has no tools.".to_string()
                    } else {
                        format!(
                            "Connected. Tools:\n{}",
                            tools
                                .iter()
                                .map(|t| format!(
                                    "{}: {}",
                                    t.name,
                                    t.description.as_deref().unwrap_or("")
                                ))
                                .collect::<Vec<_>>()
                                .join("\n")
                        )
                    };
                    let mut snapshot = shared.lock().unwrap();
                    if let Some(saved) = snapshot
                        .assistant_mcp_servers
                        .iter_mut()
                        .find(|s| s.id == server.id && s.transport == server.transport)
                    {
                        saved.discovered_tools = tools;
                        if !save_settings(&snapshot) {
                            let _ = weak.upgrade_in_event_loop(|window| {
                                window.set_status_text(
                                    "Connected, but could not save discovered tools.".into(),
                                )
                            });
                            return;
                        }
                    }
                    status
                }
                Err(error) => error,
            };
            let _ = weak.upgrade_in_event_loop(move |window| window.set_status_text(status.into()));
        });
    });
    voice_ui::install(&ui, &assistant_editor, &edit_voice, settings.clone())?;
    let voice_recording = Rc::new(RefCell::new(None::<VoiceRecording>));
    let voice_recording_for_button = voice_recording.clone();
    let assistant_editor_for_record = edit_voice.as_weak();
    let main_for_voice_record = ui.as_weak();
    let settings_for_record = settings.clone();
    edit_voice.on_record_voice(move || {
        let Some(editor) = assistant_editor_for_record.upgrade() else {
            return;
        };
        if let Some(recording) = voice_recording_for_button.borrow_mut().take() {
            editor.set_recording_voice(false);
            match finish_voice_recording(recording) {
                Ok(path) => {
                    voice_ui::start_import(&editor, main_for_voice_record.clone(), path, true);
                }
                Err(error) => editor.set_voice_status(error.into()),
            }
            return;
        }
        let current = settings_for_record.lock().unwrap().clone();
        let microphone = if current.use_default_microphone {
            None
        } else {
            Some(current.selected_microphone)
        };
        let (audio_tx, mut audio_rx) = tokio::sync::mpsc::channel::<Vec<i16>>(128);
        let (level_tx, _level_rx) = tokio::sync::mpsc::channel(2);
        match audio::start_audio_capture(audio_tx, level_tx, microphone) {
            Ok(capture) => {
                let samples = Arc::new(Mutex::new(Vec::<i16>::new()));
                let collected = samples.clone();
                let collector = thread::spawn(move || {
                    while let Some(chunk) = audio_rx.blocking_recv() {
                        let mut buffer = collected.lock().unwrap();
                        let remaining = (16_000usize * 30).saturating_sub(buffer.len());
                        buffer.extend(chunk.into_iter().take(remaining));
                    }
                });
                *voice_recording_for_button.borrow_mut() = Some(VoiceRecording {
                    capture,
                    samples,
                    collector,
                });
                editor.set_recording_voice(true);
                editor.set_voice_status(
                    "Recording voice. Speak clearly, then press Stop recording.".into(),
                );
            }
            Err(_) => {
                editor.set_voice_status("Could not start microphone for voice recording.".into())
            }
        }
    });
    let assistant_editor_for_install = assistant_editor.as_weak();
    let main_for_voice_install = ui.as_weak();
    assistant_editor.on_install_voice_model(move || {
        let Some(editor) = assistant_editor_for_install.upgrade() else {
            return;
        };
        if editor.get_installing_voice() {
            return;
        }
        editor.set_installing_voice(true);
        editor.set_voice_status("Downloading and verifying Pocket TTS…".into());
        let window = assistant_editor_for_install.clone();
        let main = main_for_voice_install.clone();
        std::thread::spawn(move || {
            let result = tokio::runtime::Runtime::new()
                .map_err(|e| e.to_string())
                .and_then(|runtime| runtime.block_on(assistant_voice::install_standard()));
            let message = match result {
                Ok(()) => "Pocket TTS installed and verified.".to_string(),
                Err(error) => error,
            };
            let _ = window.upgrade_in_event_loop(move |editor| {
                editor.set_installing_voice(false);
                editor.set_voice_status(message.into());
                if let Some(main) = main.upgrade() {
                    main.set_tts_model_status(
                        if assistant_voice::installed() {
                            "Pocket TTS is installed."
                        } else {
                            "Pocket TTS is not installed."
                        }
                        .into(),
                    );
                }
            });
        });
    });
    let assistant_editor_for_test_voice = assistant_editor.as_weak();
    assistant_editor.on_test_voice(move || {
        let Some(editor) = assistant_editor_for_test_voice.upgrade() else {
            return;
        };
        if editor.get_testing_voice() || editor.get_editing_voice() {
            return;
        }
        let phrase = editor.get_voice_test_text().trim().to_string();
        if phrase.is_empty() {
            editor.set_voice_status("Enter a test phrase first.".into());
            return;
        }
        if !assistant_voice::installed() {
            editor.set_voice_status("Install Pocket TTS before testing the voice.".into());
            return;
        }
        let path = std::path::PathBuf::from(editor.get_voice_path().to_string());
        if !assistant_voice::voice_selection_valid(&path) {
            editor.set_voice_status(
                "The selected voice is missing. Import another voice or use Alba.".into(),
            );
            return;
        }
        assistant_voice::stop_playback();
        let preview_generation = assistant_voice::playback_generation();
        editor.set_testing_voice(true);
        editor.set_voice_status("Generating voice preview…".into());
        let window = assistant_editor_for_test_voice.clone();
        std::thread::spawn(move || {
            let result = assistant_voice::speak_if_current(&phrase, &path, preview_generation);
            let message = match result {
                Ok(()) => "Voice preview finished.".to_string(),
                Err(error) => error,
            };
            if assistant_voice::playback_generation() == preview_generation {
                let _ = window.upgrade_in_event_loop(move |editor| {
                    editor.set_testing_voice(false);
                    editor.set_voice_status(message.into());
                });
            }
        });
    });
    let edit_weak = edit_voice.as_weak();
    let library_weak = assistant_editor.as_weak();
    let recording_for_done = voice_recording.clone();
    edit_voice.on_cancel(move || {
        let (Some(editor), Some(library)) = (edit_weak.upgrade(), library_weak.upgrade()) else {
            return;
        };
        if editor.get_importing_voice() {
            return;
        }
        if let Some(recording) = recording_for_done.borrow_mut().take() {
            discard_voice_recording(recording);
        }
        editor.set_recording_voice(false);
        voice_ui::close_editor(&library, &editor);
    });
    let edit_weak = edit_voice.as_weak();
    edit_voice.window().on_close_requested(move || {
        if let Some(editor) = edit_weak.upgrade() {
            editor.invoke_cancel();
        }
        CloseRequestResponse::KeepWindowShown
    });
    let weak = assistant_editor.as_weak();
    assistant_editor.on_cancel(move || {
        if let Some(editor) = weak.upgrade() {
            if editor.get_editing_voice() {
                return;
            }
            if editor.get_testing_voice() {
                assistant_voice::stop_playback();
            }
            editor.set_testing_voice(false);
            let _ = editor.hide();
        }
    });
    let weak = assistant_editor.as_weak();
    assistant_editor.window().on_close_requested(move || {
        if let Some(editor) = weak.upgrade() {
            editor.invoke_cancel();
        }
        CloseRequestResponse::KeepWindowShown
    });
    install_tts_settings_entrypoint(&ui, &assistant_editor);
    let _avatar_playground =
        avatar_ui::install(&ui, &assistant_chat, &assistant_editor, settings.clone())?;
    pipeline_ui::install(&ui, settings.clone(), assistant_editor.as_weak());
    let _desktop_avatar = desktop_avatar::install(&assistant_chat, settings.clone())?;
    let hermes_integration = hermes_ui::install(&assistant_chat, settings.clone())?;
    let _reminders = match reminders::ui::install(&ui, &assistant_chat, settings.clone()) {
        Ok(integration) => Some(integration),
        Err(error) => {
            log::warn!("Calendar reminders could not start: {error}");
            None
        }
    };
    {
        let chat = assistant_chat.as_weak();
        ui.on_hermes_settings(move || {
            if let Some(chat) = chat.upgrade() {
                chat.invoke_hermes_settings();
            }
        });
        let chat = assistant_chat.as_weak();
        ui.on_open_assistant(move || {
            if let Some(chat) = chat.upgrade() {
                let _ = chat.show();
            }
        });
    }
    let hermes_for_runtime = hermes_integration.controller.clone();
    let assistant_chat_for_tokio = assistant_chat.as_weak();
    let assistant_conversation_for_runtime = assistant_conversation.clone();
    let assistant_tasks_for_runtime = assistant_reply_tasks.clone();

    let ui_handle_for_tokio = ui.as_weak();
    let overlay_handle_for_tokio = transcript_overlay.as_weak();
    let settings_for_runtime = settings.clone();
    let cmd_tx_for_runtime = cmd_tx.clone();

    let transcript_history_for_runtime = transcript_history_store.clone();
    let transcript_history_revision_for_runtime = transcript_history_revision.clone();
    thread::spawn(move || {
        let rt = Runtime::new().unwrap();
        rt.block_on(async move {
            echo_info!("runtime", "Tokio runtime active");

            let mut active_session: Option<Session> = None;
            let mut requested_graph: Option<String> = None;
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
                        hermes_for_runtime.finish_capture(finalized_epoch);
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
                        let final_ui_epoch = session_epoch.clone();
                        let _ = ui_handle_for_tokio.upgrade_in_event_loop(move |ui| {
                            if final_ui_epoch.load(Ordering::SeqCst) != finalized_epoch { return; }
                            ui.set_audio_level(0.0);
                            ui.set_is_recording(false);
                            ui.set_is_finalizing(false);
                            if !preserve_status {
                                ui.set_has_error(false);
                                ui.set_status_text("Idle".into());
                            }
                        });
                        let final_overlay_epoch = session_epoch.clone();
                        let _ = overlay_handle_for_tokio.upgrade_in_event_loop(move |overlay| {
                            if final_overlay_epoch.load(Ordering::SeqCst) != finalized_epoch { return; }
                            if overlay.get_pipeline_failed() { overlay.set_audio_level(0.0); return; }
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
                        AppCommand::ToggleGraph(id) => {
                            let assistant = settings_for_runtime.lock().unwrap().pipelines.as_ref()
                                .and_then(|set| set.pipelines.iter().find(|p| p.id == id))
                                .and_then(|p| p.graph.as_ref()).is_some_and(graph_uses_assistant);
                            match recording_activation(active_session.as_ref().map(|s| (s.assistant, s.state.lock().unwrap().can_stop())), assistant) {
                                RecordingActivation::Start => { requested_graph=Some(id); AppCommand::StartRecording }
                                RecordingActivation::Stop => AppCommand::StopRecording,
                                RecordingActivation::Ignore => continue,
                            }
                        }
                        AppCommand::ToggleRecording => {
                            match active_session.as_ref() {
                                Some(session) => {
                                    let state = session.state.lock().unwrap();
                                    if state.can_stop() {
                                        AppCommand::StopRecording
                                    } else if session.assistant && settings_for_runtime.lock().unwrap().pipelines.as_ref()
                                        .and_then(|set| set.active().ok()).and_then(|p| p.graph.as_ref()).is_some_and(graph_uses_assistant) {
                                        AppCommand::StartRecording
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
                        AppCommand::ToggleAssistant => {
                            match recording_activation(active_session.as_ref().map(|s| (s.assistant, s.state.lock().unwrap().can_stop())), true) {
                                RecordingActivation::Start => AppCommand::StartAssistantRecording,
                                RecordingActivation::Stop => AppCommand::StopRecording,
                                RecordingActivation::Ignore => continue,
                            }
                        }
                        AppCommand::AssistantTranscriptSubmit(epoch, input) => {
                            if epoch != session_epoch.load(Ordering::SeqCst) { continue; }
                            if settings_for_runtime.lock().unwrap().hermes.enabled {
                                assistant_voice::stop_playback();
                                hermes_for_runtime.submit_recording(epoch, input);
                                continue;
                            }
                            AppCommand::AssistantSubmit(input)
                        }
                        cmd => cmd,
                    };

                    match cmd {
                        AppCommand::AssistantSubmit(input) => {
                            let input = input.trim().to_string();
                            if input.is_empty() { continue; }
                            assistant_voice::stop_playback();
                            let playback_generation = assistant_voice::playback_generation();
                            let current_settings = settings_for_runtime.lock().unwrap().clone();
                            if current_settings.hermes.enabled {
                                hermes_for_runtime.submit(input);
                                continue;
                            }
                            if let Some(p)=current_settings.pipelines.as_ref().and_then(|set|set.active_assistant().or_else(|_|set.active()).ok()).filter(|p|p.graph.is_some()).cloned() {
                                let graph=p.graph.unwrap();
                                if let Err(error)=graph.preflight(&current_settings) { let _=assistant_chat_for_tokio.upgrade_in_event_loop(move|chat|chat.present_assistant_state(error.into())); continue; }
                                let chat=assistant_chat_for_tokio.clone(); let conversation=assistant_conversation_for_runtime.clone();let ui=ui_handle_for_tokio.clone();
                                let target=injector::capture_injection_target();
                                let task=tokio::spawn(async move {
                                    let generation=playback_generation;
                                    let cancel=Arc::new(move||assistant_voice::playback_generation()!=generation);
                                    let result=run_graph_chat(&graph,input,&current_settings,chat.clone(),conversation,ui,cancel,Arc::new(|_|{})).await;
                                    if assistant_voice::playback_generation() != generation { return; }
                                    let status = match result {
                                        Ok(result) => {
                                            let delivered = if result.destination == graph::Destination::Text {
                                                injector::inject_text(&result.text, target).map(|_| ()).map_err(|e| e.to_string())
                                            } else { Ok(()) };
                                            delivered.map(|()| if result.errors.is_empty() { "Ready".into() } else { format!("Completed with node errors: {}", result.errors.join("; ")) })
                                        },
                                        Err(error) => Err(error),
                                    };
                                    let _=chat.upgrade_in_event_loop(move|chat|{if assistant_voice::playback_generation()==generation {chat.present_assistant_state(status.unwrap_or_else(|error| error).into());}});
                                });
                                let mut tasks=assistant_tasks_for_runtime.lock().unwrap();tasks.retain(|task|!task.is_finished());tasks.push(task.abort_handle());continue;
                            }
                            let pipeline = match current_settings.pipelines.as_ref()
                                .ok_or_else(|| "Pipeline configuration is missing.".to_string())
                                .and_then(|set| set.active_assistant())
                                .and_then(|p| { p.preflight(&current_settings)?; assistant::AssistantPipeline::from_pipeline(p) }) {
                                Ok(pipeline) => pipeline,
                                Err(error) => {
                                    let _ = assistant_chat_for_tokio.upgrade_in_event_loop(move |chat| {
                                        chat.present_assistant_state(error.into()); let _ = chat.show();
                                    });
                                    continue;
                                }
                            };
                            let (generation, reply_id, context) = assistant_conversation_for_runtime.lock().unwrap().start_turn(input.clone());
                            refresh_assistant_chat(&assistant_chat_for_tokio, &assistant_conversation_for_runtime);
                            let chat = assistant_chat_for_tokio.clone();
                            let conversation = assistant_conversation_for_runtime.clone();
                            let task = tokio::spawn(async move {
                                let (speech_tx, speech_rx) = std::sync::mpsc::channel::<String>();
                                let spoken_offset = Arc::new(Mutex::new(0usize));
                                let speech_task = if pipeline.output_steps.iter().any(|s| matches!(s, pipelines::OutputStep::Speech)) {
                                    let voice = std::path::PathBuf::from(&pipeline.voice_id);
                                    Some(tokio::task::spawn_blocking(move || assistant_voice::play_segments(speech_rx, &voice, playback_generation)))
                                } else { None };
                                let update_chat = chat.clone();
                                let update_conversation = conversation.clone();
                                let update_speech_tx = speech_tx.clone();
                                let update_spoken_offset = spoken_offset.clone();
                                let spoken_text = Arc::new(Mutex::new(String::new()));
                                let update_spoken_text = spoken_text.clone();
                                let result = assistant::answer_with_outputs(&current_settings, &pipeline, &context, &input,
                                    move |text| {
                                        if assistant_voice::playback_generation() != playback_generation { return; }
                                        update_conversation.lock().unwrap().update(generation, reply_id, text.to_string(), false);
                                        refresh_assistant_chat(&update_chat, &update_conversation);
                                    }, move |text| {
                                        if assistant_voice::playback_generation() != playback_generation { return; }
                                        *update_spoken_text.lock().unwrap() = text.to_string();
                                        let mut offset = update_spoken_offset.lock().unwrap();
                                        for sentence in assistant::ready_spoken_sentences(text, &mut offset) {
                                            let _ = update_speech_tx.send(sentence);
                                        }
                                    }, {
                                        let voice=pipeline.voice_id.clone();
                                        move |text| { let voice=voice.clone(); async move {
                                            tokio::task::spawn_blocking(move || assistant_voice::speak_if_current(&text,std::path::Path::new(&voice),playback_generation))
                                                .await.map_err(|_| "Speech generation task failed.".to_string())?
                                        }}
                                    }).await;
                                if assistant_voice::playback_generation() != playback_generation { return; }
                                match result {
                                    Ok(text) => {
                                        conversation.lock().unwrap().update(generation, reply_id, text.clone(), true);
                                        let offset = *spoken_offset.lock().unwrap();
                                        if let Some(tail) = spoken_text.lock().unwrap().get(offset..) {
                                            if !tail.trim().is_empty() { let _ = speech_tx.send(tail.trim().to_string()); }
                                        }
                                    },
                                    Err(error) => conversation.lock().unwrap().update(generation, reply_id, format!("Assistant error: {error}"), true),
                                }
                                drop(speech_tx);
                                refresh_assistant_chat(&chat, &conversation);
                                let _ = chat.upgrade_in_event_loop(move |chat| { if assistant_voice::playback_generation() == playback_generation { chat.present_assistant_state("Ready".into()); } });
                                if let Some(task) = speech_task {
                                    if let Ok(Err(error)) = task.await {
                                        if assistant_voice::playback_generation() == playback_generation {
                                            let _ = chat.upgrade_in_event_loop(move |chat| { if assistant_voice::playback_generation() == playback_generation { chat.present_assistant_state(error.into()); } });
                                        }
                                    }
                                }
                            });
                            let mut tasks = assistant_tasks_for_runtime.lock().unwrap();
                            tasks.retain(|task| !task.is_finished());
                            tasks.push(task.abort_handle());
                        }
                        command @ (AppCommand::StartRecording | AppCommand::StartAssistantRecording) => {
                            let requested_assistant = matches!(command, AppCommand::StartAssistantRecording);
                            let graph_mode = settings_for_runtime.lock().unwrap().pipelines.as_ref().is_some_and(|s|s.version==5);
                            let mut current_settings = settings_for_runtime.lock().unwrap().clone();
                            if graph_mode { if let Some(set)=current_settings.pipelines.as_mut() {
                                if let Some(id)=requested_graph.take() { set.active_id=id; }
                                else if requested_assistant {
                                    match set.assistant_for_runtime() {
                                        Ok(pipeline) => set.active_id=pipeline.id.clone(),
                                        Err(error) => {
                                            let _ = ui_handle_for_tokio.upgrade_in_event_loop(move |ui| {
                                                ui.set_status_text(error.into());
                                                ui.set_is_recording(false);
                                            });
                                            continue;
                                        }
                                    }
                                }
                            }}
                            let starts_assistant = requested_assistant || current_settings.pipelines.as_ref()
                                .and_then(|set| set.active().ok()).and_then(|p| p.graph.as_ref()).is_some_and(graph_uses_assistant);
                            let native_assistant = starts_assistant && current_settings.hermes.enabled;
                            let assistant_mode = (requested_assistant && !graph_mode) || native_assistant;
                            if starts_assistant && active_session.as_ref().is_none_or(|s| s.assistant) {
                                interrupt_assistant(&mut active_session, &session_epoch, warm_capture.as_ref(),
                                    &assistant_tasks_for_runtime, &assistant_conversation_for_runtime).await;
                                refresh_assistant_chat(&assistant_chat_for_tokio, &assistant_conversation_for_runtime);
                                let _ = assistant_chat_for_tokio.upgrade_in_event_loop(|chat| {
                                    chat.present_listening(true);
                                    chat.set_live_transcript("".into());
                                    chat.present_assistant_state("Listening…".into()); let _ = chat.show();
                                });
                            }
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
                            let assistant_config = if assistant_mode && !native_assistant {
                                Some(current_settings.pipelines.as_ref()
                                    .ok_or_else(|| "Pipeline configuration is missing.".to_string())
                                    .and_then(|set| set.active_assistant())
                                    .and_then(|p| { p.preflight(&current_settings)?; assistant::AssistantPipeline::from_pipeline(p) }))
                            } else { None };
                            let assistant_config = match assistant_config.transpose() {
                                Ok(config) => config,
                                Err(message) => {
                                    let chat_message = message.clone();
                                    let _ = assistant_chat_for_tokio.upgrade_in_event_loop(move |chat| { chat.present_assistant_state(chat_message.into()); });
                                    let _ = ui_handle_for_tokio.upgrade_in_event_loop(move |ui| {
                                        ui.set_status_text(message.into()); ui.set_has_error(true);
                                    });
                                    continue;
                                }
                            };
                            let session_pipeline = if let Some(config) = &assistant_config {
                                config.speech_input(&current_settings)
                            } else if native_assistant {
                                current_settings.pipelines.as_ref().ok_or_else(|| "Pipeline configuration is missing.".to_string())
                                    .and_then(|set| set.active().cloned())
                            } else {
                                current_settings.pipelines.as_ref().ok_or_else(|| "Pipeline configuration is missing.".to_string())
                                    .and_then(|set| set.active().cloned())
                                    .and_then(|p| { p.preflight(&current_settings)?; Ok(p) })
                            };
                            let session_pipeline = match session_pipeline {
                                Ok(p) => p,
                                Err(message) => {
                                    let _ = ui_handle_for_tokio.upgrade_in_event_loop(move |ui| {
                                        ui.set_status_text(message.into()); ui.set_has_error(true); ui.set_is_recording(false);
                                    });
                                    continue;
                                }
                            };
                            let assistant_graph=assistant_mode || session_pipeline.graph.as_ref().is_some_and(graph_uses_assistant);
                            if assistant_graph {
                                overlay_visible.store(false, Ordering::SeqCst);
                                let _ = overlay_handle_for_tokio.upgrade_in_event_loop(|overlay| {
                                    overlay.set_is_visible(false);
                                    hide_overlay_window(&overlay);
                                });
                            }
                            if assistant_graph && !assistant_mode {
                                assistant_voice::stop_playback();
                                if let Some(graph) = session_pipeline.graph.clone() {
                                    let selected = assistant_choice_label(&session_pipeline.name, &session_pipeline.id);
                                    let speech_settings = current_settings.clone();
                                    let _ = assistant_chat_for_tokio.upgrade_in_event_loop(move |chat| {
                                        chat.set_selected_graph(selected.into());
                                        begin_assistant_graph_listening(&chat, &graph, &speech_settings);
                                    });
                                }
                            }
                            let speech_config = match session_pipeline.transcription_config(&current_settings) {
                                Ok(config) => config,
                                Err(error) => {
                                    let _ = assistant_chat_for_tokio.upgrade_in_event_loop(move |chat| chat.present_assistant_state(error.into()));
                                    continue;
                                }
                            };
                            current_settings.transcription_provider = speech_config.provider.id().into();
                            current_settings.local_sherpa = speech_config.local.clone();
                            let provider = transcription::TranscriptionProvider::from_id(
                                &current_settings.transcription_provider,
                            );
                            if !matches!(provider, transcription::TranscriptionProvider::LocalSherpaOnnx)
                                && speech_config.api_key.trim().is_empty() {
                                echo_error!(
                                    "session",
                                    "Start rejected provider={} reason=missing_api_key",
                                    provider.id()
                                );
                                let _ = ui_handle_for_tokio.upgrade_in_event_loop(|ui| {
                                    ui.set_status_text("Missing API key".into());
                                    ui.set_is_recording(false);
                                });
                                if assistant_graph {
                                    let _ = assistant_chat_for_tokio.upgrade_in_event_loop(|chat| {
                                        chat.present_listening(false);
                                        chat.present_assistant_state("Missing API key".into());
                                    });
                                }
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
                                    let required_model = current_settings.local_sherpa.model;
                                    let _ = ui_handle_for_tokio.upgrade_in_event_loop(move |ui| {
                                        ui.set_local_model_text(required_model.label().into());
                                        ui.set_local_download_error("".into());
                                        request_local_model_prompt(&ui);
                                        ui.set_status_text("Local speech files are required".into());
                                        ui.set_has_error(false);
                                        ui.set_active_tab(3);
                                        let _ = ui.show();
                                    });
                                    if assistant_graph {
                                        let _ = assistant_chat_for_tokio.upgrade_in_event_loop(|chat| {
                                            chat.present_listening(false);
                                            chat.present_assistant_state("Local speech files are required".into());
                                        });
                                    }
                                    continue;
                            }

                            if ultra_recording_blocked(provider, current_settings.local_sherpa.model,
                                &transcription::local_engine_status_for(&current_settings.local_sherpa))
                            {
                                // Never attach/forward microphone audio while Ultra is loading.
                                transcription::preload_local_engine(&current_settings.local_sherpa);
                                echo_info!("session", "Start rejected: Ultra model is loading; no audio session created");
                                let _ = ui_handle_for_tokio.upgrade_in_event_loop(|ui| {
                                    ui.set_model_loading_notice_visible(true);
                                    ui.set_status_text("Parakeet Ultra model is loading…".into());
                                    ui.set_is_recording(false);
                                    ui.set_is_finalizing(false);
                                    ui.set_has_error(false);
                                });
                                if assistant_graph {
                                    let _ = assistant_chat_for_tokio.upgrade_in_event_loop(|chat| {
                                        chat.present_listening(false);
                                        chat.present_assistant_state("Loading local speech model…".into());
                                    });
                                } else {
                                    let loading_pipeline = session_pipeline.clone();
                                    let _ = overlay_handle_for_tokio.upgrade_in_event_loop(move |overlay| {
                                        overlay.set_graph_run_nodes(loading_pipeline.graph.as_ref().map(|g| graph_run_rows(g, true)).unwrap_or_default());
                                        overlay.set_graph_progress_nodes(graph_progress_rows(&overlay.get_graph_run_nodes(), 0));
                                        overlay.set_graph_status("".into());overlay.set_pipeline_cards(if loading_pipeline.graph.is_some(){ModelRc::default()}else{pipeline_ui::cards(&loading_pipeline.steps)}); overlay.set_pipeline_name(loading_pipeline.name.into()); overlay.set_pipeline_step(0);
                                        show_model_loading_notice(&overlay);
                                    });
                                }
                                continue;
                            }

                            let _ = ui_handle_for_tokio.upgrade_in_event_loop(|ui| {
                                ui.set_model_loading_notice_visible(false);
                            });
                            let current_session_epoch =
                                session_epoch.fetch_add(1, Ordering::SeqCst) + 1;
                            if native_assistant { hermes_for_runtime.bind_recording(current_session_epoch, session_pipeline.graph.clone()); }
                            let discard_assistant_output = Arc::new(AtomicBool::new(false));

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
                                    let loading_local_model = matches!(
                                        provider,
                                        transcription::TranscriptionProvider::LocalSherpaOnnx
                                    ) && !matches!(
                                        transcription::local_engine_status_for(&current_settings.local_sherpa),
                                        transcription::LocalEngineStatus::Ready
                                    );
                                    if loading_local_model {
                                        echo_info!(
                                            "local_model",
                                            "Local model preload requested after capture started"
                                        );
                                        transcription::preload_local_engine(
                                            &current_settings.local_sherpa,
                                        );
                                    }

                                    let _ = ui_handle_for_tokio.upgrade_in_event_loop(move |ui| {
                                        ui.set_status_text(if loading_local_model {
                                            "Loading local speech model…".into()
                                        } else {
                                            "Connecting...".into()
                                        });
                                        ui.set_has_error(false);
                                        ui.set_is_finalizing(false);
                                        ui.set_transcript("".into());
                                    });
                                    overlay_visible.store(!assistant_graph, Ordering::SeqCst);
                                    let overlay_pipeline = session_pipeline.clone();
                                    let _ = overlay_handle_for_tokio.upgrade_in_event_loop(move |overlay| {
                                        prepare_transcription_overlay(&overlay, &overlay_pipeline, &microphone_name, assistant_graph);
                                    });

                                    let client = transcription::TranscriberClient::from_config(
                                        speech_config,
                                    );
                                    let requires_committed_final = matches!(provider, transcription::TranscriptionProvider::LocalSherpaOnnx) && current_settings.local_sherpa.model.uses_gpu();
                                    let client_state = state.clone();
                                    let injection_state = state.clone();
                                    let transcript_pipeline_for_text = transcript_pipeline.clone();
                                    let transcript_history_for_text = transcript_history_for_runtime.clone();
                                    let transcript_history_revision_for_text =
                                        transcript_history_revision_for_runtime.clone();
                                    let log_line_tx_for_text = log_line_tx.clone();
                                    let session_settings_for_text = current_settings.clone();
                                    let processing_command_tx = cmd_tx_for_runtime.clone();
                                    let finalize_tx_for_transcript = finalize_tx.clone();
                                    let ui_handle_for_network = ui_handle_for_tokio.clone();
                                    let ui_handle_for_transcript = ui_handle_for_tokio.clone();
                                    let overlay_handle_for_transcript = overlay_handle_for_tokio.clone();
                                    let assistant_chat_for_transcript = assistant_chat_for_tokio.clone();
                                    let graph_conversation = assistant_conversation_for_runtime.clone();
                                    let discard_for_transcript = discard_assistant_output.clone();
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

                                        let error_tx = text_tx.clone();
                                        let result = client.run(audio_to_net_rx, network_stop_rx, text_tx, log_line_tx).await;
                                        if let Err(err) = result {
                                            if requires_committed_final {
                                                let _ = error_tx.send(transcription::TranscriptionEvent::Error(err.to_string())).await;
                                            }
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
                                                    let base_text = if empty_commit && !requires_committed_final {
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
                                            if assistant_graph {
                                                let live = display_text.clone();
                                                let epoch = session_epoch_for_transcript.clone();
                                                let _ = assistant_chat_for_transcript.upgrade_in_event_loop(move |chat| {
                                                    if epoch.load(Ordering::SeqCst) != current_session_epoch { return; }
                                                    if is_error {
                                                        chat.present_assistant_state(live.into());
                                                        chat.set_live_transcript("".into());
                                                        chat.present_listening(false);
                                                    } else {
                                                        chat.set_live_transcript(live.into());
                                                    }
                                                });
                                                continue;
                                            }
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
                                                    if is_error {
                                                        overlay.set_pipeline_failed(true); overlay.set_pipeline_step(0);
                                                        overlay.set_failure_message(aggregated_for_overlay.into());
                                                        overlay.set_sentence_text("".into());
                                                        set_overlay_height(&overlay, 220); overlay.set_is_visible(true);
                                                        overlay_visible_setter.store(true, Ordering::SeqCst);
                                                        show_overlay_without_activation(&overlay);
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
                                            (pipeline.finalization_text(&latest_partial, !requires_committed_final && !had_error), pipeline.stop_requested())
                                        };
                                        if stopped && !had_error && !base_text.trim().is_empty()
                                            && session_epoch_for_transcript.load(Ordering::SeqCst) == current_session_epoch
                                        {
                                            if assistant_mode {
                                                if discard_for_transcript.load(Ordering::SeqCst) {
                                                    let _ = finalize_tx_for_transcript.send((current_session_epoch, true));
                                                    return;
                                                }
                                                let final_text = base_text.trim().to_string();
                                                let thinking_epoch = session_epoch_for_transcript.clone();
                                                let _ = assistant_chat_for_transcript.upgrade_in_event_loop(move |chat| {
                                                    if thinking_epoch.load(Ordering::SeqCst) != current_session_epoch { return; }
                                                    chat.set_live_transcript("".into());
                                                    chat.present_assistant_state("Thinking…".into());
                                                });
                                                let _ = processing_command_tx.send(AppCommand::AssistantTranscriptSubmit(current_session_epoch, final_text));
                                                let _ = finalize_tx_for_transcript.send((current_session_epoch, true));
                                                return;
                                            }
                                            let _ = processing_command_tx.send(AppCommand::ProcessingStarted(current_session_epoch));
                                            let progress_overlay = overlay_handle_for_transcript.clone();
                                            let progress_epoch = session_epoch_for_transcript.clone();
                                            let result = if let Some(graph)=&session_pipeline.graph {
                                                assistant_voice::stop_playback();
                                                let cancelled_epoch=session_epoch_for_transcript.clone();
                                                let discarded=discard_for_transcript.clone();let cancel=Arc::new(move||cancelled_epoch.load(Ordering::SeqCst)!=current_session_epoch || discarded.load(Ordering::SeqCst));
                                                let overlay=progress_overlay.clone();let epoch=progress_epoch.clone();
                                                let progress_titles = graph.nodes.iter().map(|n| (n.id.clone(), n.title())).collect::<std::collections::HashMap<_, _>>();
                                                let progress:graph::Observer=Arc::new(move|event|{
                                                    if assistant_graph { return; }
                                                    let epoch=epoch.clone();
                                                    let title = if let graph::Event::State { node, .. } = &event { progress_titles.get(node).cloned() } else { None };
                                                    let _=overlay.upgrade_in_event_loop(move|overlay|{
                                                        if epoch.load(Ordering::SeqCst)!=current_session_epoch{return;}
                                                        match event {
                                                            graph::Event::State{node,state}=>{ update_graph_run_state(&overlay.get_graph_run_nodes(), &node, state); if state == "Running" { if let Some(i) = overlay.get_graph_run_nodes().iter().position(|row| row.id.as_str() == node) { overlay.set_pipeline_step(i as i32); } } overlay.set_graph_progress_nodes(graph_progress_rows(&overlay.get_graph_run_nodes(), overlay.get_pipeline_step())); overlay.set_graph_status(format!("{} · {state}",title.unwrap_or(node)).into()); },
                                                            graph::Event::Text{text,..}=>{ let caption = overlay_caption_text(&text); set_overlay_height(&overlay, overlay_size_for_text(&caption).1); overlay.set_sentence_text(caption.into()); },
                                                            graph::Event::Error{message,..}=>overlay.set_failure_message(message.into()),
                                                            _=>{},
                                                        }
                                                    });
                                                });
                                                let outcome=run_graph_chat(graph,base_text,&session_settings_for_text,assistant_chat_for_transcript.clone(),graph_conversation.clone(),ui_handle_for_transcript.clone(),cancel,progress).await;
                                                match outcome {
                                                    Ok(outcome)=>{if !outcome.errors.is_empty(){let message=outcome.errors.join("; ");let _=ui_handle_for_transcript.upgrade_in_event_loop(move|ui|{ui.set_status_text(message.into());ui.set_has_error(true);});}
                                                        if outcome.destination==graph::Destination::Assistant { let _=overlay_handle_for_transcript.upgrade_in_event_loop(|overlay|{overlay.set_is_visible(false);let _=overlay.hide();});let _=finalize_tx_for_transcript.send((current_session_epoch,true));return; }
                                                        Ok(outcome.text)
                                                    },
                                                    Err(message)=>Err(pipelines::RunFailure{step:0,message,text:String::new()})
                                                }
                                            }else{pipelines::execute(&session_pipeline, base_text, &session_settings_for_text,
                                                move |index, text, complete| {
                                                    let text = overlay_caption_text(text);
                                                    let epoch = progress_epoch.clone();
                                                    let _ = progress_overlay.upgrade_in_event_loop(move |overlay| {
                                                        if epoch.load(Ordering::SeqCst) != current_session_epoch { return; }
                                                        overlay.set_pipeline_step(index as i32); overlay.set_step_complete(complete);
                                                        overlay.set_sentence_text(text.clone().into());
                                                        set_overlay_height(&overlay, overlay_size_for_text(&text).1);
                                                        overlay.set_is_visible(true); show_overlay_without_activation(&overlay);
                                                    });
                                                }, || session_epoch_for_transcript.load(Ordering::SeqCst) != current_session_epoch).await};
                                            if session_epoch_for_transcript.load(Ordering::SeqCst) != current_session_epoch { return; }
                                            let processed = match result {
                                                Ok(text) => post_processing::ProcessedText { text, warning: None },
                                                Err(failure) => {
                                                    preserve_status_for_transcript.store(true, Ordering::SeqCst);
                                                    let message = failure.message.clone();
                                                    let _ = ui_handle_for_transcript.upgrade_in_event_loop(move |ui| { ui.set_status_text(format!("Pipeline stopped: {message}").into()); ui.set_has_error(true); });
                                                    if assistant_graph {
                                                        let message = format!("Pipeline stopped: {}", failure.message);
                                                        let _ = assistant_chat_for_transcript.upgrade_in_event_loop(move |chat| {
                                                            chat.present_assistant_state(message.into());
                                                            chat.present_listening(false);
                                                        });
                                                    } else {
                                                    let _ = overlay_handle_for_transcript.upgrade_in_event_loop(move |overlay| {
                                                        overlay.set_pipeline_step(failure.step as i32); overlay.set_pipeline_failed(true); overlay.set_is_error(true);
                                                        overlay.set_sentence_text(failure.text.into());
                                                        overlay.set_failure_message(failure.message.into());
                                                        set_overlay_height(&overlay, 250); show_overlay_without_activation(&overlay);
                                                    });
                                                    }
                                                    let _ = finalize_tx_for_transcript.send((current_session_epoch, true));
                                                    return;
                                                }
                                            };
                                            // Close before injection; retain the original foreground target.
                                            let (closed_tx, closed_rx) = tokio::sync::oneshot::channel();
                                            let _ = overlay_handle_for_transcript.upgrade_in_event_loop(move |overlay| {
                                                overlay.set_is_visible(false); hide_overlay_window(&overlay); let _ = closed_tx.send(());
                                            });
                                            if closed_rx.await.is_err() || session_epoch_for_transcript.load(Ordering::SeqCst) != current_session_epoch { return; }
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
                                                            let revision = transcript_history_revision_for_text
                                                                .fetch_add(1, Ordering::SeqCst)
                                                                + 1;
                                                            (history.clone(), revision)
                                                        };
                                                        let history_text = transcript_history_text(&history_snapshot);
                                                        let revision_for_ui =
                                                            transcript_history_revision_for_text.clone();
                                                        let _ = ui_handle_for_transcript.upgrade_in_event_loop(move |ui| {
                                                            if revision_for_ui.load(Ordering::SeqCst) == revision {
                                                                ui.set_transcript_history_text(history_text.into());
                                                                if ui.get_active_tab() == 0 {
                                                                    ui.set_content_scroll_y(0.0);
                                                                }
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
                                        if assistant_graph {
                                            let state = if had_error {
                                                "Transcription failed. Press Listen to try again."
                                            } else {
                                                "No speech detected. Press Listen to try again."
                                            };
                                            let _ = assistant_chat_for_transcript.upgrade_in_event_loop(move |chat| {
                                                chat.present_assistant_state(state.into());
                                                chat.set_live_transcript("".into());
                                                chat.present_listening(false);
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
                                        assistant: assistant_graph,
                                        discard_assistant_output,
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
                                        text_processing_started: false,
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
                                    if assistant_graph {
                                        let state = format!("Microphone error: {e}");
                                        let _ = assistant_chat_for_tokio.upgrade_in_event_loop(move |chat| { chat.present_assistant_state(state.into()); chat.present_listening(false); });
                                    }
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
                                    AppCommand::ToggleGraph(_) | AppCommand::ToggleRecording | AppCommand::ToggleAssistant | AppCommand::AssistantTranscriptSubmit(_, _) => unreachable!(),
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
                                    AppCommand::ProcessingStarted(epoch) => {
                                        if let Some(session) = active_session.as_mut().filter(|s| s.epoch == epoch) { session.text_processing_started = true; session.cancel_finalization_watchdog(); }
                                    }
                                    AppCommand::FinalizationTimedOut(timed_out_epoch) => {
                                    let timed_out = active_session
                                        .as_ref()
                                        .is_some_and(|session| session.epoch == timed_out_epoch && !session.text_processing_started);
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
    #[cfg(target_os = "windows")]
    let assistant_hotkey_state_for_apply = assistant_hotkey_state.clone();
    #[cfg(target_os = "windows")]
    let assistant_hotkey_id_for_apply = assistant_hotkey_id_state.clone();
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
            snapshot.hugging_face_token = draft.hugging_face_token;
            snapshot.tts_voice_path = draft.tts_voice_path;
            snapshot.tts_preload = draft.tts_preload;
            snapshot.selected_speaker = ui.get_selected_speaker().to_string();
            snapshot.use_default_speaker = ui.get_use_default_speaker();
            snapshot.output_volume = draft.output_volume;
            snapshot.hotkey_text = ui.get_hotkey_text().to_string();
            snapshot.assistant_hotkey_text = ui.get_assistant_hotkey_text().to_string();
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
        let previous_assistant_hotkey = settings_for_ui.lock().unwrap().assistant_hotkey_text.clone();
        #[cfg(target_os = "windows")]
        let assistant_hotkey_changed = previous_assistant_hotkey != snapshot.assistant_hotkey_text;
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

        #[cfg(target_os = "windows")]
        let assistant_hotkey_result = if snapshot.assistant_hotkey_text == snapshot.hotkey_text {
            Err("Assistant and dictation hotkeys must be different.".to_string())
        } else if assistant_hotkey_changed {
            apply_hotkey(&hotkey_manager_for_apply, &mut assistant_hotkey_state_for_apply.borrow_mut(), &snapshot.assistant_hotkey_text)
                .map(|id| { *assistant_hotkey_id_for_apply.borrow_mut() = Some(id); })
        } else { Ok(()) };
        #[cfg(not(target_os = "windows"))]
        let assistant_hotkey_result: Result<(), String> = Ok(());

        let saved = startup_result.is_ok() && hotkey_result.is_ok() && assistant_hotkey_result.is_ok() && save_settings(&snapshot);
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
        if !saved && assistant_hotkey_changed && assistant_hotkey_result.is_ok() {
            if let Ok(id) = apply_hotkey(&hotkey_manager_for_apply, &mut assistant_hotkey_state_for_apply.borrow_mut(), &previous_assistant_hotkey) {
                *assistant_hotkey_id_for_apply.borrow_mut() = Some(id);
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
            assistant_voice::set_output_device(&snapshot.selected_speaker, snapshot.use_default_speaker);
            assistant_voice::set_output_volume(snapshot.output_volume);
            let _ = warm_capture_reconfigure_tx.send(AppCommand::ReconfigureWarmCapture);
            model_preload::PreloadPlan::from_settings(&snapshot).start();
            let local_selected = matches!(
                transcription::TranscriptionProvider::from_id(&snapshot.transcription_provider),
                transcription::TranscriptionProvider::LocalSherpaOnnx
            ) && snapshot.pipelines.as_ref().and_then(|set| set.active().ok())
                .and_then(|pipeline| pipeline.steps.first()).is_some_and(pipelines::Step::preload_on_start);
            if local_selected && transcription::local_models_available(&snapshot.local_sherpa) {
                let repair_ui = ui_weak_for_settings.clone();
                let preload_config = snapshot.local_sherpa.clone();
                thread::spawn(move || {
                    if let Err(err) = transcription::wait_for_local_engine(&preload_config) {
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
                    .or_else(|| assistant_hotkey_result.as_ref().err().cloned())
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
        let Some(activity) = post_processing::ActivityGuard::acquire() else {
            if let Some(ui) = ui_weak_for_download.upgrade() {
                ui.set_local_download_error("Finish recording or the current model installation before installing speech files.".into());
            }
            return;
        };
        let local_config = if let Some(ui) = ui_weak_for_download.upgrade() {
            ui.set_local_repair_pending(false);
            ui.set_local_download_prompt_visible(false);
            ui.set_local_download_progress_visible(true);
            ui.set_local_download_progress(0.0);
            ui.set_local_download_status("Preparing download...".into());
            ui.set_local_download_error("".into());
            let mut config = settings_for_download.lock().unwrap().local_sherpa.clone();
            config.model = transcription::LocalModel::from_id(&ui.get_local_model_text());
            config
        } else {
            transcription::LocalSherpaConfig::default()
        };
        let progress_ui = ui_weak_for_download.clone();
        let completion_ui = ui_weak_for_download.clone();
        let settings_after_download = settings_for_download.clone();
        thread::spawn(move || {
            let _activity = activity;
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
                let saved = settings_after_download.lock().unwrap().clone();
                let speech = saved.pipelines.as_ref().and_then(|set| set.active().ok()).and_then(|p| p.transcription_config(&saved).ok());
                if let Some(config) = speech.filter(|c| c.provider == transcription::TranscriptionProvider::LocalSherpaOnnx && c.local.model == local_config.model) {
                    transcription::preload_local_engine(&config.local);
                    transcription::wait_for_local_engine(&config.local)
                } else { Ok(()) }
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
                    ui.set_settings_tab(5);
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
            "Checking offline punctuation and capitalization model…"
        } else {
            "Offline punctuation and capitalization model not installed"
        }
        .into(),
    );
    let pipeline_needs_punctuation = initial_settings
        .pipelines
        .as_ref()
        .and_then(|s| s.active().ok())
        .is_some_and(|p| {
            p.steps
                .iter()
                .any(|s| matches!(&s.kind, pipelines::StepKind::Punctuation(pp) if pp.punctuation))
        });
    if pipeline_needs_punctuation && !has_punctuation_model {
        ui.set_post_model_download_prompt_visible(true);
    }
    if has_punctuation_model {
        let ready_ui = ui.as_weak();
        thread::spawn(move || {
            let result = post_processing::preload();
            let _ = ready_ui.upgrade_in_event_loop(move |ui| {
                ui.set_post_model_ready(result.is_ok());
                match result {
                    Ok(()) => ui.set_post_model_status(
                        "Offline punctuation and capitalization model ready".into(),
                    ),
                    Err(error) => {
                        ui.set_post_model_status(error.into());
                        if pipeline_needs_punctuation {
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
        ui.set_post_model_status(
            "Preparing punctuation and capitalization model installation…".into(),
        );
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
                ui.set_post_model_status("Installation postponed. Written rules work; original punctuation and casing will be retained.".into());
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
    #[cfg(target_os = "windows")]
    let hotkey_capture_assistant_for_start = hotkey_capture_assistant.clone();

    ui.on_start_hotkey_capture(move || {
        #[cfg(target_os = "windows")]
        {
            *hotkey_capture_active_for_start.borrow_mut() = true;
            *hotkey_capture_latched_for_start.borrow_mut() = false;
            hotkey_capture_assistant_for_start.set(false);
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

    #[cfg(target_os = "windows")]
    let assistant_capture_active = hotkey_capture_active.clone();
    #[cfg(target_os = "windows")]
    let assistant_capture_latched = hotkey_capture_latched.clone();
    #[cfg(target_os = "windows")]
    let assistant_capture_target = hotkey_capture_assistant.clone();
    #[cfg(target_os = "windows")]
    let assistant_capture_window = hotkey_capture_window.as_weak();
    ui.on_start_assistant_hotkey_capture(move || {
        #[cfg(target_os = "windows")]
        {
            *assistant_capture_active.borrow_mut() = true;
            *assistant_capture_latched.borrow_mut() = false;
            assistant_capture_target.set(true);
            if let Some(capture) = assistant_capture_window.upgrade() {
                capture.set_state_text("Waiting for assistant key combo...".into());
                capture.set_combo_text("".into());
                let _ = capture.show();
            }
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
    let last_model_for_timer = Rc::new(RefCell::new(ui.get_local_model_text().to_string()));
    let local_offer_for_timer = local_download_offer_suppressed.clone();
    #[cfg(target_os = "windows")]
    let hotkey_capture_window_for_timer = hotkey_capture_window.as_weak();
    #[cfg(target_os = "windows")]
    let hotkey_capture_active_for_timer = hotkey_capture_active.clone();
    #[cfg(target_os = "windows")]
    let hotkey_capture_latched_for_timer = hotkey_capture_latched.clone();
    #[cfg(target_os = "windows")]
    let hotkey_capture_assistant_for_timer = hotkey_capture_assistant.clone();
    #[cfg(target_os = "windows")]
    let assistant_hotkey_id_for_timer = assistant_hotkey_id_state.clone();

    let activity_revision_seen = Rc::new(Cell::new(initial_activity_revision));

    let activity_revision_for_timer = activity_revision_seen.clone();
    let ui_weak_for_activity_timer = ui.as_weak();
    let icon_timer = slint::Timer::default();
    let icon_ui = ui.as_weak();
    let icon_overlay = transcript_overlay.as_weak();
    #[cfg(windows)]
    let icon_hotkey = hotkey_capture_window.as_weak();
    let mut animation_start = None::<std::time::Instant>;
    let mut previous_frame = None;
    icon_timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(50),
        move || {
            let Some(ui) = icon_ui.upgrade() else {
                return;
            };
            let index = if ui.get_is_recording() || ui.get_is_finalizing() {
                let start = animation_start.get_or_insert_with(std::time::Instant::now);
                Some(icons.index(start.elapsed()))
            } else {
                animation_start = None;
                None
            };
            if previous_frame == Some(index) {
                return;
            }
            previous_frame = Some(index);
            let frame = index.map_or(&icons.idle, |index| &icons.frames[index]);
            ui.set_application_icon(frame.image.clone());
            if let Some(overlay) = icon_overlay.upgrade() {
                overlay.set_recording_indicator(frame.image.clone());
            }
            #[cfg(windows)]
            {
                if let Some(hotkey) = icon_hotkey.upgrade() {
                    hotkey.set_application_icon(frame.image.clone());
                }
                if let Some(tray) = &tray_icon {
                    let _ = tray.set_icon(Some(frame.tray.clone()));
                }
                frame.update_taskbar();
            }
        },
    );

    let activity_timer = slint::Timer::default();
    activity_timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(250),
        move || {
            let Some(ui) = ui_weak_for_activity_timer.upgrade() else {
                return;
            };
            let revision = diagnostics::session_revision();
            if !activity_log_refresh_needed(
                ui.get_active_tab(),
                ui.get_selected_activity_bytes(),
                revision,
                activity_revision_for_timer.get(),
            ) {
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
                let (saved_local, settings_dirty, local_gpu_selected) = {
                    let saved = settings_for_timer.lock().unwrap();
                    // The console needs no settings diff and no history clone.
                    let dirty =
                        (ui.get_active_tab() == 3).then(|| settings_editor_is_dirty(&ui, &saved));
                    let local_gpu = matches!(
                        transcription::TranscriptionProvider::from_id(&saved.transcription_provider),
                        transcription::TranscriptionProvider::LocalSherpaOnnx
                    ) && saved.local_sherpa.model.uses_gpu();
                    (saved.local_sherpa.clone(), dirty, local_gpu)
                };
                if let Some(dirty) = settings_dirty {
                    ui.set_settings_dirty(dirty);
                }

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
                let model_changed = {
                    let mut previous = last_model_for_timer.borrow_mut();
                    let selected = ui.get_local_model_text().to_string();
                    let changed = *previous != selected;
                    *previous = selected;
                    changed
                };
                if provider_changed || model_changed {
                    local_offer_for_timer.set(false);
                }
                let mut selected_local = saved_local.clone();
                selected_local.model =
                    transcription::LocalModel::from_id(&ui.get_local_model_text());
                ui.set_local_engine_status_text(
                    if selected_local.model != saved_local.model {
                        "Save settings to activate this model".to_string()
                    } else {
                        match transcription::local_engine_status_for(&selected_local) {
                            transcription::LocalEngineStatus::Unloaded => "Not loaded".into(),
                            transcription::LocalEngineStatus::Loading => "Loading…".into(),
                            transcription::LocalEngineStatus::Ready => "Ready".into(),
                            transcription::LocalEngineStatus::Failed(err) => {
                                format!("Failed: {err}")
                            }
                        }
                    }
                    .into(),
                );
                if ui.get_local_repair_pending()
                    && !ui.get_unsaved_settings_prompt_visible()
                    && !ui.get_local_download_prompt_visible()
                    && !ui.get_local_download_progress_visible()
                {
                    ui.set_local_repair_pending(false);
                    ui.set_local_download_prompt_visible(true);
                    ui.set_active_tab(3);
                    ui.set_settings_tab(5);
                    let _ = ui.show();
                }
                if ui.get_active_tab() == 3
                    && ui.get_settings_tab() == 5
                    && matches!(
                        transcription::TranscriptionProvider::from_id(&selected_provider),
                        transcription::TranscriptionProvider::LocalSherpaOnnx
                    )
                    && !transcription::local_models_available(&selected_local)
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
                    if ui.get_model_loading_notice_visible() && !local_gpu_selected {
                        ui.set_model_loading_notice_visible(false);
                        hide_overlay_window(&overlay);
                    }
                    if ui.get_model_loading_notice_visible() {
                        match transcription::local_engine_status_for(&saved_local) {
                            transcription::LocalEngineStatus::Ready => {
                                ui.set_model_loading_notice_visible(false);
                                ui.set_status_text("Parakeet Ultra ready".into());
                                overlay.set_sentence_text("Model is ready. Press your recording shortcut to start dictation.".into());
                                overlay.set_microphone_name("Parakeet Ultra · Ready".into());
                                let weak_ui = ui.as_weak();
                                let weak_overlay = overlay.as_weak();
                                slint::Timer::single_shot(std::time::Duration::from_secs(3), move || {
                                    if let (Some(ui), Some(overlay)) = (weak_ui.upgrade(), weak_overlay.upgrade()) {
                                        if !ui.get_is_recording() && !ui.get_is_finalizing()
                                            && !ui.get_model_loading_notice_visible()
                                            && overlay.get_is_system_message() && !overlay.get_is_error()
                                            && overlay.get_microphone_name() == "Parakeet Ultra · Ready" {
                                            hide_overlay_window(&overlay);
                                        }
                                    }
                                });
                            }
                            transcription::LocalEngineStatus::Failed(error) => {
                                ui.set_model_loading_notice_visible(false);
                                ui.set_has_error(true);
                                ui.set_status_text(format!("Ultra could not load: {error}").into());
                                overlay.set_is_error(true);
                                overlay.set_sentence_text(format!("Model could not load: {error}").into());
                            }
                            _ => {}
                        }
                    }
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
                                if hotkey_capture_assistant_for_timer.get() {
                                    ui.set_assistant_hotkey_text(combo.clone().into());
                                } else {
                                    ui.set_hotkey_text(combo.clone().into());
                                }
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
                        if let Some(editor)=graph_ui::hotkey_capture_editor(){
                            if event.state==HotKeyState::Pressed {
                                let saved=settings_for_timer.lock().unwrap();
                                let combo=graph_hotkeys::capture_registered_combo(event.id,*hotkey_id_state.borrow(),*assistant_hotkey_id_for_timer.borrow(),&saved);
                                if let Some(combo)=combo{editor.set_graph_hotkey(combo.into());editor.set_dirty(true);}
                            }
                            continue;
                        }
                        if event.state==HotKeyState::Pressed {
                            if let Some(id)=graph_hotkeys.borrow().registered.iter().find(|(_,key)|key.id()==event.id).map(|(id,_)|id.clone()) {let _=cmd_tx_for_timer.send(AppCommand::ToggleGraph(id));continue;}
                        }
                        let current_hotkey_id = *hotkey_id_state.borrow();
                        let assistant_hotkey_id = *assistant_hotkey_id_for_timer.borrow();
                        if current_hotkey_id.is_some_and(|id| event.id == id)
                            && event.state == HotKeyState::Pressed
                        {
                            echo_info!("hotkey", "Global recording hotkey pressed id={}", event.id);
                            let _ = cmd_tx_for_timer.send(AppCommand::ToggleRecording);
                        } else if assistant_hotkey_id.is_some_and(|id| event.id == id)
                            && event.state == HotKeyState::Pressed
                        {
                            let _ = cmd_tx_for_timer.send(AppCommand::ToggleAssistant);
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
                                && settings_editor_is_dirty(
                                    &ui,
                                    &settings_for_timer.lock().unwrap(),
                                )
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
    let event_loop_result = slint::run_event_loop_until_quit();
    hermes_integration.controller.shutdown();
    transcription::shutdown_local_engine();
    event_loop_result?;
    echo_info!("app", "Slint event loop stopped; Echo exiting cleanly");
    Ok(())
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::{
        forward_audio_until_stopped, overlay_caption_text, overlay_size_for_text, parse_hotkey,
        selected_microphone_for_snapshot, OVERLAY_HEIGHT, OVERLAY_MAX_LINES,
    };
    use crate::audio::InputDeviceSnapshot;
    use std::time::Duration;
    use tokio::sync::mpsc;

    #[test]
    fn assistant_activation_stops_listening_and_restarts_processing() {
        use super::{recording_activation, RecordingActivation as Action};
        assert_eq!(recording_activation(None, true), Action::Start);
        assert_eq!(recording_activation(Some((true, true)), true), Action::Stop);
        assert_eq!(
            recording_activation(Some((true, false)), true),
            Action::Start
        );
        assert_eq!(
            recording_activation(Some((false, false)), true),
            Action::Ignore
        );
        assert_eq!(
            recording_activation(Some((false, true)), true),
            Action::Ignore
        );
        assert_eq!(
            recording_activation(Some((false, true)), false),
            Action::Stop
        );
        assert_eq!(
            recording_activation(Some((false, false)), false),
            Action::Ignore
        );
    }

    #[tokio::test]
    async fn assistant_interrupt_drops_model_request_and_invalidates_queued_speech() {
        use std::sync::{atomic::AtomicU64, Mutex};
        use tokio::io::AsyncReadExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let mut settings = super::AppSettings::default();
        settings
            .model_registry
            .connections
            .push(crate::models::ModelConnection {
                id: "interruption-test".into(),
                provider: "openai_compatible".into(),
                endpoint: format!("http://{address}/v1"),
                api_key: String::new(),
                api_version: String::new(),
                custom_header_name: String::new(),
                custom_header_value: String::new(),
                allow_lan_http: false,
            });
        let mut model = crate::models::ConfiguredModel::new(
            "interruption-test".into(),
            "Test".into(),
            "test".into(),
        );
        model.verified = true;
        let model_id = model.id.clone();
        settings.model_registry.models.push(model);
        let task = tokio::spawn(async move {
            crate::models::stream(
                &settings,
                &model_id,
                "Answer the request",
                "request",
                |_| {},
            )
            .await
        });
        let (mut socket, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let mut request = vec![0; 16384];
        assert!(socket.read(&mut request).await.unwrap() > 0);
        let tasks = Mutex::new(vec![task.abort_handle()]);
        let epoch = AtomicU64::new(1);
        let conversation = Mutex::new(crate::assistant::Conversation::default());
        let generation = crate::assistant_voice::playback_generation();
        super::interrupt_assistant(&mut None, &epoch, None, &tasks, &conversation).await;
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(tasks.lock().unwrap().is_empty());
        assert_ne!(crate::assistant_voice::playback_generation(), generation);
        // An old queued sentence must return before loading a voice or audio device.
        crate::assistant_voice::speak_if_current(
            "stale sentence",
            std::path::Path::new("missing-voice"),
            generation,
        )
        .unwrap();
        // Dropping the model future closes its outstanding HTTP request.
        let closed = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if socket.read(&mut request).await.unwrap() == 0 {
                    break;
                }
            }
        })
        .await;
        assert!(closed.is_ok());
    }

    #[test]
    #[ignore = "native assistant interruption fixture; synthetic microphone, no user settings"]
    #[allow(clippy::arc_with_non_send_sync)] // Matches the UI-thread session API; this fixture never sends Session across threads.
    fn assistant_interruption_interactive_fixture() {
        use slint::{ComponentHandle, Model};
        use std::sync::{
            atomic::{AtomicBool, AtomicU64, Ordering},
            Arc, Mutex,
        };
        let settings = super::AppSettings::default();
        let p = crate::pipelines::migrate(&settings)
            .active()
            .unwrap()
            .clone();
        let mut graph = crate::graph::migrate(&p, &settings);
        for node in &mut graph.nodes {
            if matches!(node.kind, crate::graph::Kind::TextOutput) {
                node.kind = crate::graph::Kind::AssistantDisplay;
            }
        }
        let runtime = Arc::new(tokio::runtime::Runtime::new().unwrap());
        let task = runtime.spawn(std::future::pending::<()>());
        let watchdog = runtime.spawn(std::future::pending::<()>());
        let watchdog_handle = watchdog.abort_handle();
        let tasks = Arc::new(Mutex::new(vec![task.abort_handle()]));
        let conversation = Arc::new(Mutex::new(crate::assistant::Conversation::default()));
        let (generation, reply, _) = conversation
            .lock()
            .unwrap()
            .start_turn("Old request".into());
        conversation.lock().unwrap().update(
            generation,
            reply,
            "This unfinished response stays visible.".into(),
            false,
        );
        let epoch = Arc::new(AtomicU64::new(7));
        let discarded = Arc::new(AtomicBool::new(false));
        let session = Arc::new(Mutex::new(Some(super::Session {
            assistant: true,
            discard_assistant_output: discarded.clone(),
            epoch: 7,
            state: Arc::new(Mutex::new(crate::state::RecordingState::Finalizing)),
            audio_capture: None,
            warm_capture_attached: false,
            audio_forward_stop_tx: None,
            network_stop_tx: None,
            transcript_pipeline: Arc::new(Mutex::new(crate::pipeline::TranscriptPipeline::new())),
            task_abort_handles: vec![watchdog_handle],
            finalization_watchdog: Some(watchdog.abort_handle()),
            text_processing_started: true,
            _formatting_activity: crate::post_processing::ActivityGuard::acquire().unwrap(),
        })));
        let chat = super::AssistantChatWindow::new().unwrap();
        if let Ok(width) = std::env::var("ECHO_GUI_WIDTH") {
            chat.set_window_width(width.parse().unwrap());
        }
        if let Ok(height) = std::env::var("ECHO_GUI_HEIGHT") {
            chat.set_window_height(height.parse().unwrap());
        }
        if let Ok(scale) = std::env::var("SLINT_SCALE_FACTOR") {
            let scale: f32 = scale.parse().unwrap();
            let weak = chat.as_weak();
            slint::Timer::single_shot(Duration::from_millis(30), move || {
                let chat = weak.upgrade().unwrap();
                chat.window()
                    .dispatch_event(slint::platform::WindowEvent::ScaleFactorChanged {
                        scale_factor: scale,
                    });
                chat.window().set_size(slint::PhysicalSize::new(
                    (chat.get_window_width() as f32 * scale).round() as u32,
                    (chat.get_window_height() as f32 * scale).round() as u32,
                ));
            });
        }
        chat.present_assistant_state("Receiving…".into());
        super::refresh_assistant_chat(&chat.as_weak(), &conversation);
        let weak = chat.as_weak();
        let callback_conversation = conversation.clone();
        let callback_runtime = runtime.clone();
        let callback_session = session.clone();
        let callback_discarded = discarded.clone();
        chat.on_start_listening(move || {
            callback_runtime.block_on(super::interrupt_assistant(
                &mut callback_session.lock().unwrap(),
                &epoch,
                None,
                &tasks,
                &callback_conversation,
            ));
            assert!(callback_session.lock().unwrap().is_none());
            assert!(callback_discarded.load(Ordering::SeqCst));
            callback_conversation.lock().unwrap().update(
                generation,
                reply,
                "STALE RESULT".into(),
                true,
            );
            if let Some(chat) = weak.upgrade() {
                super::begin_assistant_graph_listening(&chat, &graph, &settings);
                super::refresh_assistant_chat(&weak, &callback_conversation);
                chat.set_live_transcript("This is my new spoken request.".into());
            }
        });
        let weak = chat.as_weak();
        let submit_conversation = conversation.clone();
        chat.on_submit(move |text| {
            let mut conv = submit_conversation.lock().unwrap();
            let (generation, reply, context) = conv.start_turn(text.to_string());
            assert!(!context.contains("unfinished response"));
            conv.update(
                generation,
                reply,
                "The new request completed successfully.".into(),
                true,
            );
            drop(conv);
            super::refresh_assistant_chat(&weak, &submit_conversation);
            if let Some(chat) = weak.upgrade() {
                chat.present_listening(false);
                chat.set_live_transcript("".into());
                chat.present_assistant_state("Ready".into());
            }
        });
        if std::env::var_os("ECHO_ASSISTANT_INTERRUPT_INSPECT").is_none() {
            let weak = chat.as_weak();
            slint::Timer::single_shot(Duration::from_millis(100), move || {
                let chat = weak.upgrade().unwrap();
                chat.invoke_start_listening();
                let weak = chat.as_weak();
                slint::Timer::single_shot(Duration::from_millis(100), move || {
                    let chat = weak.upgrade().unwrap();
                    assert!(chat.get_listening());
                    assert_eq!(chat.get_assistant_state(), "Listening…");
                    assert!(chat.get_messages().iter().any(|m| m.state == "Interrupted"));
                    assert!(!chat.get_messages().iter().any(|m| m.text == "STALE RESULT"));
                    if let Some(path) = std::env::var_os("ECHO_GUI_SNAPSHOT_PATH") {
                        let pixels = chat.window().take_snapshot().unwrap();
                        image::save_buffer(
                            path,
                            pixels.as_bytes(),
                            pixels.width(),
                            pixels.height(),
                            image::ColorType::Rgba8,
                        )
                        .unwrap();
                    }
                    chat.invoke_submit("New request".into());
                    slint::Timer::single_shot(Duration::from_millis(100), || {
                        let _ = slint::quit_event_loop();
                    });
                });
            });
        }
        chat.run().unwrap();
        assert!(runtime.block_on(task).unwrap_err().is_cancelled());
        assert!(runtime.block_on(watchdog).unwrap_err().is_cancelled());
        assert!(conversation
            .lock()
            .unwrap()
            .messages
            .iter()
            .any(|m| m.interrupted));
        assert!(chat
            .get_messages()
            .iter()
            .any(|m| m.text == "The new request completed successfully."));
        chat.hide().unwrap();
    }

    #[test]
    #[ignore = "isolated Slint graph run; verifies speech failure remains visible beside delivered text"]
    fn graph_chat_reports_speech_failure() {
        use slint::{ComponentHandle, Model};
        use std::sync::{Arc, Mutex};
        let settings = super::AppSettings::default();
        let p = crate::pipelines::migrate(&settings)
            .active()
            .unwrap()
            .clone();
        let mut graph = crate::graph::migrate(&p, &settings);
        graph.nodes.retain(|n| n.transcription());
        graph.edges.clear();
        let source = graph.nodes[0].id.clone();
        let display = crate::graph::Node::new(crate::graph::Kind::AssistantDisplay, 0., 0.);
        let display_id = display.id.clone();
        graph.nodes.push(display);
        graph.connect(&source, "text", &display_id, "text").unwrap();
        let speech = crate::graph::Node::new(
            crate::graph::Kind::Speech {
                voice: "missing-test-voice.wav".into(),
                preload: false,
            },
            0.,
            0.,
        );
        let speech_id = speech.id.clone();
        graph.nodes.push(speech);
        graph.connect(&source, "text", &speech_id, "text").unwrap();
        let main = super::AppWindow::new().unwrap();
        let chat = super::AssistantChatWindow::new().unwrap();
        let conversation = Arc::new(Mutex::new(crate::assistant::Conversation::default()));
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let outcome = runtime
            .block_on(super::run_graph_chat(
                &graph,
                "Hi".into(),
                &settings,
                chat.as_weak(),
                conversation,
                main.as_weak(),
                Arc::new(|| false),
                Arc::new(|_| {}),
            ))
            .unwrap();
        assert_eq!(outcome.text, "Hi");
        assert_eq!(outcome.errors.len(), 1);
        slint::Timer::single_shot(std::time::Duration::from_millis(100), || {
            let _ = slint::quit_event_loop();
        });
        slint::run_event_loop().unwrap();
        assert!(chat.get_speech_status().starts_with("Speech failed:"));
        assert!(chat
            .get_assistant_state()
            .starts_with("Completed with node errors:"));
        assert!(chat
            .get_graph_run_nodes()
            .iter()
            .any(|row| row.id.as_str() == speech_id && row.state == "Failed"));
        assert!(chat
            .get_messages()
            .iter()
            .any(|message| !message.from_user && message.text == "Hi"));
        chat.hide().unwrap();
    }
    #[test]
    fn assistant_output_detection_ignores_disconnected_nodes() {
        let settings = super::AppSettings::default();
        let p = crate::pipelines::migrate(&settings)
            .active()
            .unwrap()
            .clone();
        let mut graph = crate::graph::migrate(&p, &settings);
        let display = crate::graph::Node::new(crate::graph::Kind::AssistantDisplay, 0., 0.);
        let id = display.id.clone();
        graph.nodes.push(display);
        assert!(!super::graph_uses_assistant(&graph));
        let source = graph
            .nodes
            .iter()
            .find(|n| n.transcription())
            .unwrap()
            .id
            .clone();
        graph.connect(&source, "text", &id, "text").unwrap();
        assert!(super::graph_uses_assistant(&graph));
    }
    #[test]
    #[ignore = "isolated destination window routing; synthetic transcription, no microphone"]
    fn pipeline_destination_selects_transcription_window_smoke() {
        use slint::ComponentHandle;
        let settings = super::AppSettings::default();
        let mut pipeline = crate::pipelines::migrate(&settings)
            .active()
            .unwrap()
            .clone();
        pipeline.graph = Some(crate::graph::migrate(&pipeline, &settings));
        let overlay = super::TranscriptOverlayWindow::new().unwrap();
        let chat = super::AssistantChatWindow::new().unwrap();
        assert!(!super::graph_uses_assistant(
            pipeline.graph.as_ref().unwrap()
        ));
        super::prepare_transcription_overlay(&overlay, &pipeline, "Synthetic microphone", false);
        assert!(overlay.window().is_visible());
        assert!(overlay.get_is_visible());
        assert!(!chat.window().is_visible());

        let graph = pipeline.graph.as_mut().unwrap();
        let ending = graph
            .nodes
            .iter_mut()
            .find(|n| matches!(n.kind, crate::graph::Kind::TextOutput))
            .unwrap();
        ending.kind = crate::graph::Kind::AssistantDisplay;
        let assistant = super::graph_uses_assistant(graph);
        assert!(assistant);
        super::begin_assistant_graph_listening(&chat, graph, &settings);
        super::prepare_transcription_overlay(
            &overlay,
            &pipeline,
            "Synthetic microphone",
            assistant,
        );
        assert!(chat.window().is_visible());
        assert!(chat.get_listening());
        assert!(!overlay.window().is_visible());
        assert!(!overlay.get_is_visible());
        chat.set_live_transcript("The first sentence appears in the assistant board.".into());

        // Optional MCP inspection exercises the same startup policy as recording.
        if let Ok(destination) = std::env::var("ECHO_TRANSCRIPTION_UI_INSPECT") {
            if destination == "text" {
                for node in &mut pipeline.graph.as_mut().unwrap().nodes {
                    if matches!(node.kind, crate::graph::Kind::AssistantDisplay) {
                        node.kind = crate::graph::Kind::TextOutput;
                    }
                }
                super::prepare_transcription_overlay(
                    &overlay,
                    &pipeline,
                    "Synthetic microphone",
                    false,
                );
                chat.hide().unwrap();
                overlay.set_sentence_text(
                    "The first sentence appears in the transcription overlay.".into(),
                );
            }
            slint::run_event_loop().unwrap();
        }
        overlay.hide().unwrap();
        chat.hide().unwrap();
    }
    #[test]
    #[ignore = "isolated first-sentence geometry and assistant recording startup; no microphone"]
    fn first_sentence_and_assistant_start_smoke() {
        use slint::{ComponentHandle, Model};
        let settings = super::AppSettings::default();
        let p = crate::pipelines::migrate(&settings)
            .active()
            .unwrap()
            .clone();
        let graph = crate::graph::migrate(&p, &settings);
        let chat = super::AssistantChatWindow::new().unwrap();
        super::begin_assistant_graph_listening(&chat, &graph, &settings);
        assert!(chat.get_listening());
        assert!(chat.window().is_visible());
        assert_eq!(chat.get_assistant_state(), "Listening…");
        assert!(chat
            .get_graph_progress_nodes()
            .iter()
            .any(|n| n.state == "Running"));
        let first =
            "This is the first sentence, with several wide words WWWWWW WWWWWW WWWWWW WWWWWW.";
        chat.set_live_transcript(first.into());
        chat.invoke_scroll_bottom();
        assert_eq!(chat.get_live_transcript(), first);
        let overlay = super::TranscriptOverlayWindow::new().unwrap();
        overlay.set_graph_run_nodes(super::graph_run_rows(&graph, true));
        overlay.set_graph_progress_nodes(super::graph_progress_rows(
            &overlay.get_graph_run_nodes(),
            0,
        ));
        overlay.set_sentence_text(first.into());
        super::set_overlay_height(&overlay, super::OVERLAY_HEIGHT);
        assert!(overlay.get_window_height() >= overlay.get_caption_required_height().ceil() as i32);
        assert!(overlay.window().size().height >= overlay.get_window_height() as u32);
        overlay.show().unwrap();
        let image = overlay.window().take_snapshot().unwrap();
        assert!(image.height() >= overlay.get_caption_required_height().ceil() as u32);
        overlay.hide().unwrap();
        chat.hide().unwrap();
    }
    #[test]
    fn progress_window_advances_through_fifteen_nodes() {
        use slint::{Model, ModelRc, VecModel};
        for current in 0..15 {
            let rows = ModelRc::new(VecModel::from(
                (0..15)
                    .map(|i| super::GraphRunRow {
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
            ));
            let visible = super::graph_progress_rows(&rows, current);
            let expected: Vec<_> = ((current - 1).max(0)..=(current + 2).min(14))
                .map(|i| i.to_string())
                .collect();
            assert_eq!(
                visible
                    .iter()
                    .map(|row| row.id.to_string())
                    .collect::<Vec<_>>(),
                expected
            );
            assert!(visible.row_count() <= 4);
            assert_eq!(
                visible.iter().filter(|row| row.state == "Running").count(),
                1
            );
        }
    }
    #[test]
    fn progress_window_omits_skipped_branches_and_duplicate_current() {
        use slint::{Model, ModelRc, VecModel};
        let rows = ModelRc::new(VecModel::from(
            [
                "Complete", "Skipped", "Running", "Skipped", "Waiting", "Blocked", "Waiting",
            ]
            .iter()
            .enumerate()
            .map(|(i, state)| super::GraphRunRow {
                id: i.to_string().into(),
                title: "Node".into(),
                state: (*state).into(),
            })
            .collect::<Vec<_>>(),
        ));
        assert_eq!(
            super::graph_progress_rows(&rows, 2)
                .iter()
                .map(|row| row.id.to_string())
                .collect::<Vec<_>>(),
            vec!["0", "2", "4", "6"]
        );
        assert_eq!(
            super::graph_progress_rows(&ModelRc::default(), 99).row_count(),
            0
        );
        let last = super::graph_progress_rows(&rows, 99);
        assert_eq!(
            last.iter()
                .map(|row| row.id.to_string())
                .collect::<Vec<_>>(),
            vec!["0", "6"]
        );
    }
    #[test]
    fn speech_notice_requires_connected_output() {
        let mut settings = super::AppSettings::default();
        let p = crate::pipelines::migrate(&settings)
            .active()
            .unwrap()
            .clone();
        let mut graph = crate::graph::migrate(&p, &settings);
        assert!(super::graph_speech_status(&graph, &settings).starts_with("Speech off:"));
        let speech = crate::graph::Node::new(
            crate::graph::Kind::Speech {
                voice: crate::voices::FOLLOW_DEFAULT.into(),
                preload: false,
            },
            0.,
            0.,
        );
        let id = speech.id.clone();
        graph.nodes.push(speech);
        assert!(super::graph_speech_status(&graph, &settings).starts_with("Speech off:"));
        let source = graph
            .nodes
            .iter()
            .find(|n| n.transcription())
            .unwrap()
            .id
            .clone();
        graph.connect(&source, "text", &id, "text").unwrap();
        if crate::assistant_voice::installed() {
            assert!(super::graph_speech_status(&graph, &settings).is_empty());
            settings.tts_voice_path = "missing-settings-default-voice.safetensors".into();
            assert!(
                super::graph_speech_status(&graph, &settings).starts_with("Speech setup needed:")
            );
            settings.tts_voice_path.clear();
        } else {
            assert!(
                super::graph_speech_status(&graph, &settings).starts_with("Speech setup needed:")
            );
        }
        if let crate::graph::Kind::Speech { voice, .. } = &mut graph
            .nodes
            .iter_mut()
            .find(|node| node.id == id)
            .unwrap()
            .kind
        {
            *voice = "missing-custom-voice.wav".into();
        }
        assert!(super::graph_speech_status(&graph, &settings).starts_with("Speech setup needed:"));
    }
    #[test]
    #[ignore = "isolated Slint graph progress models and adaptive overlay geometry"]
    fn graph_feedback_smoke() {
        use slint::{ComponentHandle, Model, ModelRc, VecModel};
        let overlay = super::TranscriptOverlayWindow::new().unwrap();
        overlay.set_sentence_text("Hi".into());
        super::set_overlay_height(&overlay, super::overlay_size_for_text("Hi").1);
        assert!(overlay.get_window_height() >= overlay.get_caption_required_height().ceil() as i32);
        let rows = ModelRc::new(VecModel::from(vec![
            super::GraphRunRow {
                id: "first".into(),
                title: "Text".into(),
                state: "Waiting".into(),
            },
            super::GraphRunRow {
                id: "second".into(),
                title: "Text".into(),
                state: "Waiting".into(),
            },
        ]));
        overlay.set_graph_run_nodes(rows.clone());
        overlay.set_graph_progress_nodes(super::graph_progress_rows(&rows, 0));
        super::set_overlay_height(&overlay, super::overlay_size_for_text("Hi").1);
        assert!(overlay.get_window_height() >= overlay.get_caption_required_height().ceil() as i32);
        super::update_graph_run_state(&rows, "second", "Running");
        assert_eq!(rows.row_data(0).unwrap().state, "Waiting");
        assert_eq!(rows.row_data(1).unwrap().state, "Running");
        overlay.show().unwrap();
        let snapshot = overlay.window().take_snapshot().unwrap();
        assert!(snapshot.height() >= 152);
        overlay.hide().unwrap();
    }
    #[test]
    #[ignore = "isolated speaker settings persistence and device refresh; no audio"]
    fn speaker_settings_ui_smoke() {
        use slint::ComponentHandle;
        let ui = super::AppWindow::new().unwrap();
        let mut saved = super::AppSettings::default();
        saved.normalize_transcription_settings();
        super::populate_settings_editor(&ui, &saved);
        let snapshot = super::assistant_voice::OutputDeviceSnapshot {
            devices: vec!["Speakers A".into(), "Headphones B".into()],
            default_device: Some("Speakers A".into()),
        };
        super::synchronize_speaker_editor(&ui, &snapshot, true);
        assert_eq!(ui.get_selected_speaker(), "Speakers A");
        assert!(!super::settings_editor_is_dirty(&ui, &saved));
        ui.set_use_default_speaker(false);
        ui.set_selected_speaker("Headphones B".into());
        ui.set_output_volume(0.35);
        super::synchronize_speaker_editor(&ui, &snapshot, false);
        assert_eq!(ui.get_selected_speaker(), "Headphones B");
        assert!(super::settings_editor_is_dirty(&ui, &saved));
        let draft = super::settings_snapshot_from_ui(&ui, &saved);
        assert_eq!(draft.selected_speaker, "Headphones B");
        assert!(!draft.use_default_speaker);
        assert_eq!(draft.output_volume, 0.35);
        let restored: super::AppSettings =
            serde_json::from_str(&serde_json::to_string(&draft).unwrap()).unwrap();
        super::populate_settings_editor(&ui, &restored);
        assert_eq!(ui.get_selected_speaker(), "Headphones B");
        assert!(!ui.get_use_default_speaker());
        assert_eq!(ui.get_output_volume(), 0.35);
        ui.hide().unwrap();
    }

    #[test]
    #[ignore = "isolated TTS defaults persistence; no audio or network"]
    fn tts_settings_ui_smoke() {
        use slint::ComponentHandle;
        let ui = super::AppWindow::new().unwrap();
        let mut saved = super::AppSettings::default();
        saved.normalize_transcription_settings();
        super::populate_settings_editor(&ui, &saved);
        assert!(!super::settings_editor_is_dirty(&ui, &saved));
        let voice = super::AssistantPipelineWindow::new().unwrap();
        super::install_tts_settings_entrypoint(&ui, &voice);
        ui.invoke_configure_tts();
        assert_eq!(voice.get_configuration_role(), 4);
        assert!(voice.get_default_voice_settings());
        voice.set_voice_path("C:/Echo/default-voice.wav".into());
        voice.set_preload_voice_on_start(true);
        voice.invoke_save();
        assert_eq!(ui.get_tts_voice_path(), "C:/Echo/default-voice.wav");
        assert!(ui.get_tts_preload());
        ui.set_tts_preload(true);
        ui.set_hugging_face_token_text("test-token".into());
        assert!(super::settings_editor_is_dirty(&ui, &saved));
        let draft = super::settings_snapshot_from_ui(&ui, &saved);
        assert_eq!(draft.tts_voice_path, "C:/Echo/default-voice.wav");
        assert!(draft.tts_preload);
        assert_eq!(draft.hugging_face_token, "test-token");
        super::populate_settings_editor(&ui, &saved);
        assert_eq!(ui.get_tts_voice_path(), "");
        assert!(!ui.get_tts_preload());
        assert!(!super::settings_editor_is_dirty(&ui, &saved));
        ui.hide().unwrap();
    }

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
        ui.set_post_model_status(
            "Offline punctuation and capitalization model ready (isolated UI test)".into(),
        );
        ui.window().set_size(slint::PhysicalSize::new(1000, 1050));
        ui.show().unwrap();
        let weak = ui.as_weak();
        slint::Timer::single_shot(Duration::from_millis(500), move || {
            let ui = weak.upgrade().unwrap();
            for tab in 0..=6 {
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
                    position: slint::LogicalPosition::new(968.0, 72.0),
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

    #[test]
    #[ignore = "renders isolated local-model settings; no microphone, network, or user settings writes"]
    fn parakeet_model_settings_ui_smoke() {
        use slint::ComponentHandle;
        let ui = super::AppWindow::new().unwrap();
        let settings = super::AppSettings {
            transcription_provider: super::transcription::LOCAL_SHERPA_PROVIDER_ID.into(),
            local_sherpa: super::transcription::LocalSherpaConfig {
                num_threads: 2,
                ..Default::default()
            },
            ..Default::default()
        };
        super::populate_settings_editor(&ui, &settings);
        ui.set_local_model_options(slint::ModelRc::new(slint::VecModel::from(
            super::transcription::LocalModel::ALL
                .into_iter()
                .map(|model| slint::SharedString::from(model.label()))
                .collect::<Vec<_>>(),
        )));
        ui.set_ultra_gpu_status("Detected NVIDIA GPU: RTX 5080 (isolated UI fixture)".into());
        ui.set_ultra_download_description(
            super::transcription::ultra_download_description().into(),
        );
        ui.set_local_engine_status_text("Ready".into());
        ui.set_active_tab(3);
        ui.set_settings_tab(5);
        ui.window().set_size(slint::PhysicalSize::new(1000, 1050));
        ui.show().unwrap();
        let weak = ui.as_weak();
        slint::Timer::single_shot(Duration::from_millis(500), move || {
            let ui = weak.upgrade().unwrap();
            for model in super::transcription::LocalModel::ALL {
                ui.set_local_model_text(model.label().into());
                let snapshot = super::settings_snapshot_from_ui(&ui, &settings);
                assert_eq!(snapshot.local_sherpa.model, model);
                assert_eq!(snapshot.local_sherpa.num_threads, 2);
                save_ui_snapshot(
                    &ui.window().take_snapshot().unwrap(),
                    &format!("echo-{}.bmp", model.id()),
                );
            }
            ui.set_local_download_prompt_visible(true);
            save_ui_snapshot(
                &ui.window().take_snapshot().unwrap(),
                "echo-ultra-install.bmp",
            );
            ui.set_local_download_prompt_visible(false);
            for tab in 0..=6 {
                ui.set_settings_tab(tab);
                let pixels = ui.window().take_snapshot().unwrap();
                assert!(pixels.width() >= 760 && pixels.height() >= 600);
            }
            ui.hide().unwrap();
            slint::quit_event_loop().unwrap();
        });
        slint::run_event_loop().unwrap();
    }

    #[test]
    fn activity_refresh_preserves_selection_and_catches_up_afterward() {
        assert!(!super::activity_log_refresh_needed(2, 10, 2, 1));
        assert!(super::activity_log_refresh_needed(2, 0, 2, 1));
        assert!(!super::activity_log_refresh_needed(2, 0, 2, 2));
        assert!(!super::activity_log_refresh_needed(0, 0, 2, 1));
    }

    #[test]
    fn ultra_requires_ready_engine_before_recording() {
        use super::transcription::{
            LocalEngineStatus as Status, LocalModel, TranscriptionProvider as Provider,
        };
        for status in [
            Status::Unloaded,
            Status::Loading,
            Status::Failed("failed".into()),
        ] {
            assert!(super::ultra_recording_blocked(
                Provider::LocalSherpaOnnx,
                LocalModel::ParakeetUltra,
                &status
            ));
        }
        assert!(!super::ultra_recording_blocked(
            Provider::LocalSherpaOnnx,
            LocalModel::ParakeetUltra,
            &Status::Ready
        ));
        assert!(!super::ultra_recording_blocked(
            Provider::LocalSherpaOnnx,
            LocalModel::ParakeetTdtV2,
            &Status::Unloaded
        ));
        assert!(!super::ultra_recording_blocked(
            Provider::ElevenLabsRealtime,
            LocalModel::ParakeetUltra,
            &Status::Loading
        ));
    }

    #[test]
    fn transcript_document_limits_display_to_latest_100_lines() {
        let history: Vec<_> = (0..500)
            .map(|index| super::TranscriptHistoryEntry {
                timestamp: index.to_string(),
                text: format!("Message {index}"),
            })
            .collect();
        let text = super::transcript_history_text(&history);
        assert!(text.starts_with("[0] Message 0"));
        assert!(text.lines().count() <= super::MAX_TRANSCRIPT_DISPLAY_LINES);
        assert!(!text.contains("[50] Message 50"));
        assert_eq!(history.len(), 500, "Display limits must not delete history");

        let multiline = vec![super::TranscriptHistoryEntry {
            timestamp: "new".into(),
            text: (0..150)
                .map(|i| format!("Line {i}"))
                .collect::<Vec<_>>()
                .join("\n"),
        }];
        let text = super::transcript_history_text(&multiline);
        assert_eq!(text.lines().count(), 100);
        assert!(text.ends_with("Line 99"));
        assert!(!text.contains("Line 100"));
    }

    #[test]
    fn transcript_document_preserves_newest_first_history() {
        let history = vec![
            super::TranscriptHistoryEntry {
                timestamp: "new".into(),
                text: "Latest message".into(),
            },
            super::TranscriptHistoryEntry {
                timestamp: "old".into(),
                text: "Earlier message".into(),
            },
        ];
        assert_eq!(
            super::transcript_history_text(&history),
            "[new] Latest message\n\n[old] Earlier message"
        );
        assert_eq!(super::transcript_history_text(&[]), "");
        let source = include_str!("../ui/appwindow.slint");
        assert_eq!(
            source
                .matches("transcript-text := SelectableDocument")
                .count(),
            1
        );
        assert!(!source.contains("for line[i]"));
        assert!(source.contains("text: root.transcript-history-text;"));
        assert!(source.contains("text: root.activity-log-text;"));
        assert!(!source.contains("native-document"));
        assert_eq!(source.matches("default-font-family: \"Arial\";").count(), 3);
        assert!(!source.contains("Segoe UI"));
        assert!(!source.contains("RECENT TRANSCRIPTS"));
        assert!(!source.contains("text: \"Copy\""));
    }

    #[test]
    #[ignore = "native Skia assistant scrolling with synthetic streaming history; no services"]
    fn assistant_history_scroll_survives_stream_updates_ui_smoke() {
        use slint::ComponentHandle;
        super::select_ui_backend().unwrap();
        let chat = super::AssistantChatWindow::new().unwrap();
        let conversation = std::sync::Arc::new(std::sync::Mutex::new(
            super::assistant::Conversation::default(),
        ));
        let mut latest = (0, 0);
        for i in 0..100 {
            let mut conversation = conversation.lock().unwrap();
            let (generation, reply, _) = conversation.start_turn(format!("Synthetic question {i}"));
            conversation.update(
                generation,
                reply,
                "A bounded synthetic answer for the scrolling test. ".repeat(12),
                true,
            );
            latest = (generation, reply);
        }
        chat.show().unwrap();
        super::refresh_assistant_chat(&chat.as_weak(), &conversation);
        let result = std::rc::Rc::new(std::cell::RefCell::new(None));
        let output = result.clone();
        let weak = chat.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(500), move || {
            let chat = weak.upgrade().unwrap();
            let phase = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                chat.invoke_scroll_bottom();
                let _ = chat.window().take_snapshot().unwrap();
                chat.invoke_scroll_bottom();
                let start = std::time::Instant::now();
                for _ in 0..30 {
                    chat.window()
                        .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
                            position: slint::LogicalPosition::new(150.0, 350.0),
                            delta_x: 0.0,
                            delta_y: 120.0,
                        });
                    let _ = chat.window().take_snapshot().unwrap();
                }
                println!(
                    "Assistant history wheel-to-snapshot mean: {:.2} ms",
                    start.elapsed().as_secs_f64() * 1000.0 / 30.0
                );
                assert!(
                    !chat.get_chat_at_bottom(),
                    "The user must be able to scroll away from the streaming tail"
                );
                chat.get_chat_scroll_position()
            }));
            let scroll = match phase {
                Ok(scroll) => scroll,
                Err(error) => {
                    *output.borrow_mut() = Some(Err(error));
                    let _ = chat.hide();
                    let _ = slint::quit_event_loop();
                    return;
                }
            };
            conversation.lock().unwrap().update(
                latest.0,
                latest.1,
                "More streamed text. ".repeat(60),
                true,
            );
            super::refresh_assistant_chat(&chat.as_weak(), &conversation);
            let weak = chat.as_weak();
            slint::Timer::single_shot(std::time::Duration::from_millis(150), move || {
                let chat = weak.upgrade().unwrap();
                *output.borrow_mut() = Some(std::panic::catch_unwind(
                    std::panic::AssertUnwindSafe(|| {
                        assert!(
                            (chat.get_chat_scroll_position() - scroll).abs() < 2.0,
                            "A streamed update must not move the user's reading position"
                        );
                        assert!(!chat.get_chat_at_bottom());
                    }),
                ));
                let _ = chat.hide();
                let _ = slint::quit_event_loop();
            });
        });
        slint::run_event_loop_until_quit().unwrap();
        if let Some(Err(error)) = result.borrow_mut().take() {
            std::panic::resume_unwind(error);
        };
    }

    #[test]
    #[ignore = "isolated live-message layout and automatic following; no microphone or services"]
    fn assistant_live_message_follows_recording_in_history() {
        use slint::ComponentHandle;
        let chat = super::AssistantChatWindow::new().unwrap();
        let hermes = std::env::var_os("ECHO_GUI_HERMES").is_some();
        chat.set_hermes_mode(hermes);
        chat.set_rich_blocks(slint::ModelRc::new(slint::VecModel::from(
            (0..100)
                .map(|i| crate::RichMessageBlock {
                    id: i.to_string().into(),
                    text: slint::StyledText::from_plain_text(
                        format!("Earlier message {i}. ").repeat(20).as_str(),
                    ),
                    first: true,
                    last: true,
                    ..Default::default()
                })
                .collect::<Vec<_>>(),
        )));
        chat.set_messages(slint::ModelRc::new(slint::VecModel::from(
            (0..100)
                .map(|i| crate::AssistantMessage {
                    text: format!("Earlier message {i}. ").repeat(20).into(),
                    ..Default::default()
                })
                .collect::<Vec<_>>(),
        )));
        chat.show().unwrap();
        let _ = chat.window().take_snapshot().unwrap();
        chat.set_hermes_scroll_position(0.0);
        chat.set_listening(true);
        chat.set_live_transcript("This is the first sentence of my new message.".into());
        let _ = chat.window().take_snapshot().unwrap();
        let _ = chat.window().take_snapshot().unwrap();
        assert!(if hermes {
            chat.get_hermes_at_bottom()
        } else {
            chat.get_chat_at_bottom()
        });
        chat.set_live_transcript("A longer live message with Unicode 🐈. ".repeat(150).into());
        let _ = chat.window().take_snapshot().unwrap();
        let _ = chat.window().take_snapshot().unwrap();
        assert!(if hermes {
            chat.get_hermes_at_bottom()
        } else {
            chat.get_chat_at_bottom()
        });
        chat.set_listening(false);
        chat.set_live_transcript("".into());
        let _ = chat.window().take_snapshot().unwrap();
        chat.hide().unwrap();
    }

    #[test]
    #[ignore = "isolated 1000-line Activity selection/scroll performance check"]
    fn activity_console_performance_smoke() {
        use slint::ComponentHandle;
        let ui = super::AppWindow::new().unwrap();
        ui.set_active_tab(2);
        ui.set_activity_log_text((0..1000).map(|i| format!("12:00:00 [{i:04}] Speech engine ready. This is a full-length diagnostic line for scrolling and selection.")).collect::<Vec<_>>().join("\n").into());
        ui.window().set_size(slint::PhysicalSize::new(960, 720));
        ui.show().unwrap();
        let outcome = std::rc::Rc::new(std::cell::RefCell::new(None));
        let result_for_timer = outcome.clone();
        let weak = ui.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(300), move || {
            let ui = weak.upgrade().unwrap();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = ui.window().take_snapshot().unwrap();
                let position = slint::LogicalPosition::new(60.0, 53.0);
                let button = slint::platform::PointerEventButton::Left;
                ui.window()
                    .dispatch_event(slint::platform::WindowEvent::PointerPressed {
                        position,
                        button,
                    });
                let start = std::time::Instant::now();
                for step in 0..12 {
                    ui.window()
                        .dispatch_event(slint::platform::WindowEvent::PointerMoved {
                            position: slint::LogicalPosition::new(90.0 + step as f32 * 10.0, 53.0),
                        });
                    let _ = ui.window().take_snapshot().unwrap();
                }
                println!(
                    "Activity selection: {:.1} ms/frame",
                    start.elapsed().as_secs_f64() * 1000.0 / 12.0
                );
                ui.window()
                    .dispatch_event(slint::platform::WindowEvent::PointerReleased {
                        position: slint::LogicalPosition::new(210.0, 53.0),
                        button,
                    });
                assert!(ui.get_selected_activity_bytes() > 0);
                let start = std::time::Instant::now();
                for _ in 0..12 {
                    ui.window()
                        .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
                            position: slint::LogicalPosition::new(950.0, 350.0),
                            delta_x: 0.0,
                            delta_y: -100.0,
                        });
                    let _ = ui.window().take_snapshot().unwrap();
                }
                println!(
                    "Activity scrolling: {:.1} ms/frame",
                    start.elapsed().as_secs_f64() * 1000.0 / 12.0
                );
                assert!(ui.get_content_scroll_y() < 0.0);
                save_ui_snapshot(
                    &ui.window().take_snapshot().unwrap(),
                    "echo-activity-performance.bmp",
                );
            }));
            if result.is_err() {
                *result_for_timer.borrow_mut() = Some(result);
                ui.hide().unwrap();
                slint::quit_event_loop().unwrap();
            } else {
                ui.set_active_tab(0);
                let weak = ui.as_weak();
                slint::Timer::single_shot(std::time::Duration::from_millis(100), move || {
                    let ui = weak.upgrade().unwrap();
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        assert_eq!(
                            ui.get_selected_activity_bytes(),
                            0,
                            "Leaving Activity must clear the refresh pause"
                        );
                        ui.set_active_tab(2);
                        let _ = ui.window().take_snapshot().unwrap();
                        assert!(super::activity_log_refresh_needed(
                            2,
                            ui.get_selected_activity_bytes(),
                            2,
                            1
                        ));
                    }));
                    *result_for_timer.borrow_mut() = Some(result);
                    ui.hide().unwrap();
                    slint::quit_event_loop().unwrap();
                });
            }
        });
        slint::run_event_loop().unwrap();
        if let Err(panic) = outcome
            .borrow_mut()
            .take()
            .expect("Activity check must finish")
        {
            std::panic::resume_unwind(panic);
        };
    }

    #[test]
    #[ignore = "renders isolated UI pages and dialogs; no recording, network, or settings writes"]
    fn redesigned_ui_smoke() {
        use slint::ComponentHandle;
        let ui = super::AppWindow::new().unwrap();
        ui.set_local_model_options(slint::ModelRc::new(slint::VecModel::from(
            super::transcription::LocalModel::ALL
                .into_iter()
                .map(|model| slint::SharedString::from(model.label()))
                .collect::<Vec<_>>(),
        )));

        let navigated = std::rc::Rc::new(std::cell::Cell::new(-1));
        let outcome = std::rc::Rc::new(std::cell::RefCell::new(None));
        let timer_outcome = outcome.clone();
        let flag = navigated.clone();
        ui.on_request_navigation(move |tab| flag.set(tab));
        let settings = super::AppSettings::default();
        super::populate_settings_editor(&ui, &settings);
        ui.set_hotkey_text("Ctrl + Space".into());
        ui.set_app_version("0.1.9".into());
        ui.set_ultra_download_description(
            super::transcription::ultra_download_description().into(),
        );
        ui.set_activity_log_text(
            "12:00  Ready\n12:01  Speech engine initialized\n12:02  Transcription completed".into(),
        );
        ui.set_transcript_history_text("A compact workspace leaves more room for the actual transcript.\n\nLonger transcripts wrap naturally. Select any text and copy it with Ctrl+C.".into());
        let width = std::env::var("ECHO_UI_SMOKE_WIDTH")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(960);
        let height = std::env::var("ECHO_UI_SMOKE_HEIGHT")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(720);
        ui.window()
            .set_size(slint::PhysicalSize::new(width, height));
        ui.show().unwrap();
        let weak = ui.as_weak();
        slint::Timer::single_shot(Duration::from_millis(500), move || {
            let ui = weak.upgrade().unwrap();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                {
                    for tab in [0, 2, 3] {
                        ui.set_active_tab(tab);
                        let pages = if tab == 3 { 7 } else { 1 };
                        for page in 0..pages {
                            ui.set_settings_tab(page);
                            let pixels = ui.window().take_snapshot().unwrap();
                            assert_eq!(
                                (pixels.width(), pixels.height()),
                                (width, height),
                                "Run with SLINT_SCALE_FACTOR=1"
                            );
                            assert!(pixels.as_slice().iter().any(|p| p.r > 100));
                            save_ui_snapshot(
                                &pixels,
                                &format!("echo-redesign-{width}-{tab}-{page}.bmp"),
                            );
                        }
                    }
                }
                ui.set_unsaved_settings_prompt_visible(true);
                #[cfg(windows)]
                {
                    use windows::Win32::{
                        Foundation::{LPARAM, WPARAM},
                        UI::WindowsAndMessaging::*,
                    };
                    let icons = super::icon_animation::Icons::load().unwrap();
                    let hwnd = super::icon_animation::main_window();
                    assert_ne!(hwnd.0, 0);
                    let mut previous = 0;
                    for frame in icons.frames.iter().chain(std::iter::once(&icons.idle)) {
                        ui.set_application_icon(frame.image.clone());
                        let _ = ui.window().take_snapshot().unwrap();
                        frame.update_taskbar();
                        let big = unsafe { SendMessageW(hwnd, WM_GETICON, WPARAM(1), LPARAM(0)) }.0;
                        let small =
                            unsafe { SendMessageW(hwnd, WM_GETICON, WPARAM(0), LPARAM(0)) }.0;
                        assert_ne!(big, 0, "Running taskbar icon must be set");
                        assert_ne!(small, 0, "Title-bar icon must be set");
                        assert_ne!(
                            big, previous,
                            "Each animation frame must replace the native icon"
                        );
                        previous = big;
                    }
                    // Clear the borrowed BIG handle before the cached frames are dropped.
                    unsafe {
                        SendMessageW(hwnd, WM_SETICON, WPARAM(1), LPARAM(0));
                    }
                }
                save_ui_snapshot(
                    &ui.window().take_snapshot().unwrap(),
                    "echo-redesign-unsaved.bmp",
                );
                ui.set_unsaved_settings_prompt_visible(false);
                ui.set_update_panel_visible(true);
                ui.set_update_available_version("0.2.0".into());
                ui.set_update_status("An update is available.".into());
                save_ui_snapshot(
                    &ui.window().take_snapshot().unwrap(),
                    "echo-redesign-update.bmp",
                );
                ui.set_update_panel_visible(false);
                ui.set_local_download_prompt_visible(true);
                save_ui_snapshot(
                    &ui.window().take_snapshot().unwrap(),
                    "echo-redesign-download.bmp",
                );
                ui.set_local_download_prompt_visible(false);
                ui.set_active_tab(0);
                let _ = ui.window().take_snapshot().unwrap();
                for (x, y) in [(22.0, 61.0)] {
                    ui.window()
                        .dispatch_event(slint::platform::WindowEvent::PointerMoved {
                            position: slint::LogicalPosition::new(x, y),
                        });
                    if x == 22.0 {
                        assert_eq!(
                            ui.get_navigation_hint(),
                            "Activity",
                            "Hover must explain the navigation icon"
                        );
                        save_ui_snapshot(
                            &ui.window().take_snapshot().unwrap(),
                            &format!("echo-redesign-{width}-hover.bmp"),
                        );
                    }
                    for pressed in [true, false] {
                        let position = slint::LogicalPosition::new(x, y);
                        let button = slint::platform::PointerEventButton::Left;
                        ui.window().dispatch_event(if pressed {
                            slint::platform::WindowEvent::PointerPressed { position, button }
                        } else {
                            slint::platform::WindowEvent::PointerReleased { position, button }
                        });
                    }
                }
                assert_eq!(navigated.get(), 2);
                let position = slint::LogicalPosition::new(60.0, 53.0);
                let button = slint::platform::PointerEventButton::Left;
                ui.window()
                    .dispatch_event(slint::platform::WindowEvent::PointerMoved { position });
                ui.window()
                    .dispatch_event(slint::platform::WindowEvent::PointerPressed {
                        position,
                        button,
                    });
                let position = slint::LogicalPosition::new(320.0, 53.0);
                ui.window()
                    .dispatch_event(slint::platform::WindowEvent::PointerMoved { position });
                ui.window()
                    .dispatch_event(slint::platform::WindowEvent::PointerReleased {
                        position,
                        button,
                    });
                save_ui_snapshot(
                    &ui.window().take_snapshot().unwrap(),
                    &format!("echo-redesign-{width}-selection.bmp"),
                );
                assert!(
                    ui.get_selected_transcript_bytes() > 0,
                    "Transcript text must support native selection for copying"
                );
                let before = ui.get_transcript_history_text();
                ui.window()
                    .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: "x".into() });
                ui.window()
                    .dispatch_event(slint::platform::WindowEvent::KeyReleased { text: "x".into() });
                assert_eq!(
                    ui.get_transcript_history_text(),
                    before,
                    "History must remain read-only"
                );
                save_ui_snapshot(
                    &ui.window().take_snapshot().unwrap(),
                    &format!("echo-redesign-{width}-selection.bmp"),
                );
                ui.window()
                    .dispatch_event(slint::platform::WindowEvent::PointerMoved {
                        position: slint::LogicalPosition::new(25.0, height as f32 - 13.0),
                    });
                for pressed in [true, false] {
                    let position = slint::LogicalPosition::new(25.0, height as f32 - 13.0);
                    let button = slint::platform::PointerEventButton::Left;
                    ui.window().dispatch_event(if pressed {
                        slint::platform::WindowEvent::PointerPressed { position, button }
                    } else {
                        slint::platform::WindowEvent::PointerReleased { position, button }
                    });
                }
                assert!(
                    ui.get_update_panel_visible(),
                    "Version text must open updates"
                );
                ui.set_update_panel_visible(false);
                ui.set_transcript_history_text(
                    (0..50)
                        .map(|i| format!("Transcript row {i}: scroll the page, not an inset card."))
                        .collect::<Vec<String>>()
                        .join("\n\n")
                        .into(),
                );
                let _ = ui.window().take_snapshot().unwrap();
                ui.window()
                    .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
                        position: slint::LogicalPosition::new(
                            width as f32 - 5.0,
                            height as f32 - 40.0,
                        ),
                        delta_x: 0.0,
                        delta_y: -400.0,
                    });
                assert!(
                    ui.get_content_scroll_y() < 0.0,
                    "The outer page must scroll at the window edge"
                );
                save_ui_snapshot(
                    &ui.window().take_snapshot().unwrap(),
                    &format!("echo-redesign-{width}-scrolled.bmp"),
                );
                ui.set_active_tab(3);
                assert_eq!(
                    super::settings_snapshot_from_ui(&ui, &settings).post_processing,
                    settings.post_processing
                );
            }));
            *timer_outcome.borrow_mut() = Some(result);
            let weak = ui.as_weak();
            slint::Timer::single_shot(Duration::from_millis(100), move || {
                let ui = weak.upgrade().unwrap();
                let reset_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    assert_eq!(
                        ui.get_content_scroll_y(),
                        0.0,
                        "Switching pages must reset scrolling after change notifications"
                    );
                }));
                if reset_result.is_err() {
                    *timer_outcome.borrow_mut() = Some(reset_result);
                }
                ui.hide().unwrap();
                slint::quit_event_loop().unwrap();
            });
        });
        slint::run_event_loop().unwrap();
        let result = outcome.borrow_mut().take().expect("UI timer must finish");
        if let Err(payload) = result {
            std::panic::resume_unwind(payload);
        }
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
    #[ignore = "opens an isolated overlay window; run with SLINT_BACKEND=winit-software"]
    #[cfg(windows)]
    fn overlay_animation_remains_frameless_ui_smoke() {
        use slint::ComponentHandle;
        use windows::Win32::{
            Foundation::{LPARAM, WPARAM},
            UI::WindowsAndMessaging::*,
        };
        let overlay = super::TranscriptOverlayWindow::new().unwrap();
        overlay.set_sentence_text("Animated recording indicator".into());
        overlay.show().unwrap();
        let icons = super::icon_animation::Icons::load().unwrap();
        let weak = overlay.as_weak();
        let outcome = std::rc::Rc::new(std::cell::RefCell::new(None));
        let result_for_timer = outcome.clone();
        let timer = slint::Timer::default();
        let mut tick = 0;
        let mut native_icon = None;
        timer.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(100),
            move || {
                let overlay = weak.upgrade().unwrap();
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let frame = if tick < 6 {
                        &icons.frames[tick % 3]
                    } else {
                        &icons.idle
                    };
                    overlay.set_recording_indicator(frame.image.clone());
                    let pixels = overlay.window().take_snapshot().unwrap();
                    if tick == 0 {
                        super::configure_overlay_as_non_activating(&overlay).unwrap();
                    }
                    let hwnd = super::overlay_hwnd().unwrap();
                    let style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) };
                    assert_eq!(
                        style & WS_CAPTION.0 as isize,
                        0,
                        "Overlay must have no native caption"
                    );
                    let icon = unsafe { SendMessageW(hwnd, WM_GETICON, WPARAM(0), LPARAM(0)) }.0;
                    if let Some(previous) = native_icon {
                        assert_eq!(
                            icon, previous,
                            "Content animation must not change the native window icon"
                        );
                    }
                    native_icon = Some(icon);
                    if tick == 1 {
                        save_ui_snapshot(&pixels, "echo-overlay-waveform.bmp");
                    }
                }));
                tick += 1;
                if result.is_err() || tick == 7 {
                    *result_for_timer.borrow_mut() = Some(result);
                    overlay.hide().unwrap();
                    slint::quit_event_loop().unwrap();
                }
            },
        );
        slint::run_event_loop().unwrap();
        if let Err(panic) = outcome
            .borrow_mut()
            .take()
            .expect("Overlay smoke must finish")
        {
            std::panic::resume_unwind(panic);
        };
    }

    #[test]
    fn overlay_keeps_short_captions_intact() {
        assert_eq!(overlay_caption_text("  hello world  "), "hello world");
    }

    #[test]
    fn overlay_retains_the_first_sentence_and_full_transcript() {
        let transcript = (0..80)
            .map(|index| format!("word{index}"))
            .collect::<Vec<_>>()
            .join(" ");
        let caption = overlay_caption_text(&transcript);

        assert_eq!(caption, transcript);
        assert!(caption.starts_with("word0"));
        assert!(caption.ends_with("word79"));
    }

    #[test]
    fn overlay_height_grows_to_six_lines_and_stops() {
        assert_eq!(overlay_size_for_text("short").1, OVERLAY_HEIGHT);

        let long_caption = "x".repeat(OVERLAY_MAX_LINES * 62 * 2);
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

thread_local! {static AGENT_APPROVAL_WINDOWS:RefCell<Vec<AgentApprovalWindow>>=const{RefCell::new(Vec::new())};}
fn show_agent_approval(request: Arc<agent_tools::Approval>) {
    let _ = slint::invoke_from_event_loop(move || {
        if (request.cancelled)() {
            return;
        }
        let Ok(window) = AgentApprovalWindow::new() else {
            return;
        };
        window.set_action_text(request.description.clone().into());
        window.set_scope_label(request.scope_label.clone().into());
        let weak = window.as_weak();
        let approval = request.clone();
        window.on_decide(move |accepted, scope| {
            if !(approval.cancelled)() {
                if let Some(tx) = approval.reply.lock().unwrap().take() {
                    let _ = tx.send(agent_tools::Decision {
                        allow: accepted,
                        scope,
                    });
                }
            }
            if let Some(window) = weak.upgrade() {
                let _ = window.hide();
            }
        });
        let approval = request.clone();
        window.window().on_close_requested(move || {
            if let Some(tx) = approval.reply.lock().unwrap().take() {
                let _ = tx.send(agent_tools::Decision::default());
            }
            CloseRequestResponse::HideWindow
        });
        let _ = window.show();
        AGENT_APPROVAL_WINDOWS.with(|windows| {
            windows
                .borrow_mut()
                .retain(|window| window.window().is_visible());
            windows.borrow_mut().push(window.clone_strong());
        });
        fn poll(window: slint::Weak<AgentApprovalWindow>, request: Arc<agent_tools::Approval>) {
            slint::Timer::single_shot(std::time::Duration::from_millis(30), move || {
                if (request.cancelled)() {
                    request.reply.lock().unwrap().take();
                    if let Some(window) = window.upgrade() {
                        let _ = window.hide();
                    }
                } else if request.reply.lock().unwrap().is_some() {
                    poll(window, request);
                }
            });
        }
        poll(window.as_weak(), request);
    });
}
mod codex_import;
