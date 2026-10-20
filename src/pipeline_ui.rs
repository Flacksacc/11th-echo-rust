use crate::component_pool::{groups as pool_groups, PoolItem, GROUPS as POOL_GROUPS};
use crate::{
    pipelines::*,
    settings::{AppSettings, PostProcessingSettings},
    transcription::{LocalModel, TranscriptionProvider},
    AppWindow, AssistantPipelineWindow, PipelineCard, PipelineEditorWindow, PipelineRow,
    PipelineStepWindow,
};
use slint::{CloseRequestResponse, ComponentHandle, ModelRc, VecModel};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{Arc, Mutex},
};

pub fn cards(steps: &[Step]) -> ModelRc<PipelineCard> {
    ModelRc::new(VecModel::from(
        steps
            .iter()
            .map(|s| PipelineCard {
                title: s.label().into(),
                summary: s.summary().into(),
                icon: icon(s),
                speech: s.is_transcription(),
            })
            .collect::<Vec<_>>(),
    ))
}
pub(crate) fn refresh_assistant_cards(
    editor: &PipelineEditorWindow,
    pipeline: &Pipeline,
    settings: &AppSettings,
) {
    let Some(config) = pipeline.assistant.as_ref() else {
        return;
    };
    let label = |id: &str| {
        settings
            .model_registry
            .models
            .iter()
            .find(|model| model.id == id)
            .map(|model| model.name.clone())
            .unwrap_or_else(|| "Choose a model".into())
    };
    editor.set_router_name(label(&config.router_model_id).into());
    editor.set_answer_name(label(&config.answer_model_id).into());
    editor.set_fast_answer_name(label(&config.fast_answer_model_id).into());
    editor.set_decision_router(settings.model_registry.is_decision(&config.router_model_id));
    editor.set_voice_name(if config.voice_id.is_empty() {
        "No voice selected".into()
    } else {
        "Voice selected".into()
    });
}
fn assistant_choice_is_ollama(settings: &AppSettings, choice: &str) -> bool {
    settings.model_registry.models.iter().any(|model| {
        crate::assistant_choice_label(&model.name, &model.id) == choice
            && settings
                .model_registry
                .find(&model.id)
                .is_ok_and(|(_, connection)| {
                    ["ollama", "ollaya"].contains(&connection.provider.as_str())
                })
    })
}
fn icon(s: &Step) -> slint::Image {
    // Embedded assets work in packaged builds without a source-tree dependency.
    let bytes: &[u8] = match s.kind {
        StepKind::Transcription { .. } => include_bytes!("../assets/icons/speech.svg"),
        StepKind::Gemini { .. } => include_bytes!("../assets/icons/rewrite.svg"),
        StepKind::Model { .. } => include_bytes!("../assets/icons/rewrite.svg"),
        _ => include_bytes!("../assets/icons/post-processing.svg"),
    };
    slint::Image::load_from_svg_data(bytes).unwrap_or_default()
}
fn pipeline_row(p: &Pipeline, set: &PipelineSet, settings: &AppSettings) -> PipelineRow {
    if let Some(g) = &p.graph {
        return PipelineRow {
            id: p.id.clone().into(),
            name: p.name.clone().into(),
            summary: format!(
                "{} nodes · {} wires · {}",
                g.nodes.len(),
                g.edges.len(),
                if g.hotkey.is_empty() {
                    "No hotkey"
                } else {
                    &g.hotkey
                }
            )
            .into(),
            details: g
                .nodes
                .iter()
                .map(|n| n.title())
                .collect::<Vec<_>>()
                .join(" → ")
                .into(),
            active: p.id == set.active_id,
            assistant: false,
        };
    }
    let assistant = p.output == PipelineOutput::Assistant;
    let active_id = if assistant {
        &set.active_assistant_id
    } else {
        &set.active_id
    };
    let active = &p.id == active_id;
    let kind = if assistant {
        "Assistant"
    } else {
        "Transcription"
    };
    let count = p.steps.len();
    let count_label = if count == 1 {
        "1 chain component".to_owned()
    } else {
        format!("{count} chain components")
    };
    let first = p
        .steps
        .iter()
        .take(2)
        .map(Step::label)
        .collect::<Vec<_>>()
        .join(" → ");
    let more = p.steps.len().saturating_sub(2);
    let compact = if more > 0 {
        format!("{first} → +{more}")
    } else {
        first
    };
    let summary = format!("{kind} · {count_label} · {compact}");
    let mut lines = vec![
        format!(
            "{kind} output · {}",
            if active { "Active" } else { "Available" }
        ),
        count_label,
    ];
    for (index, step) in p.steps.iter().take(6).enumerate() {
        let description = step.summary();
        lines.push(format!(
            "{}. {}{}",
            index + 1,
            step.label(),
            if description.is_empty() {
                String::new()
            } else {
                format!(" — {description}")
            }
        ));
    }
    if p.steps.len() > 6 {
        lines.push(format!("+{} more processing components", p.steps.len() - 6));
    }
    if let Some(config) = p.assistant.as_ref() {
        let model_name = |id: &str| {
            settings
                .model_registry
                .models
                .iter()
                .find(|model| model.id == id)
                .map(|model| model.name.as_str())
                .unwrap_or("Model unavailable")
        };
        lines.push(format!(
            "Fast router: {}",
            model_name(&config.router_model_id)
        ));
        lines.push(format!(
            "Delegated answer: {}",
            model_name(&config.answer_model_id)
        ));
        lines.push(if config.voice_id.is_empty() {
            "Reply: chat only".into()
        } else {
            "Reply: chat and voice".into()
        });
    }
    PipelineRow {
        id: p.id.clone().into(),
        name: p.name.clone().into(),
        summary: summary.into(),
        details: lines.join("\n").into(),
        active,
        assistant,
    }
}
pub fn refresh(ui: &AppWindow, shared: &AppSettings) {
    if let Some(set) = &shared.pipelines {
        ui.set_pipeline_rows(ModelRc::new(VecModel::from(
            set.pipelines
                .iter()
                .map(|p| pipeline_row(p, set, shared))
                .collect::<Vec<_>>(),
        )));
    }
}
pub(crate) fn pool(shared: &AppSettings) -> Vec<Step> {
    let mut entries = Vec::new();
    for provider in [
        TranscriptionProvider::ElevenLabsRealtime,
        TranscriptionProvider::OpenAiRealtimeWhisper,
    ] {
        let mut defaults = shared.clone();
        defaults.transcription_provider = provider.id().into();
        let c = defaults.transcription_config();
        entries.push(Step::new(StepKind::Transcription {
            provider: provider.id().into(),
            model: c.model_id,
            language: c.language_code,
            no_verbatim: c.no_verbatim,
            local: c.local,
        }));
    }
    for model in LocalModel::ALL {
        let mut local = shared.local_sherpa.clone();
        local.model = model;
        entries.push(Step::new(StepKind::Transcription {
            provider: "local_sherpa_onnx".into(),
            model: model.id().into(),
            language: "en".into(),
            no_verbatim: false,
            local,
        }));
    }
    entries.push(Step::new(StepKind::Punctuation(punctuation_only(
        shared.post_processing.clone(),
    ))));
    entries.push(Step::new(StepKind::Formatting(formatting_only(
        shared.post_processing.clone(),
    ))));
    for model in &shared.model_registry.models {
        if !model.verified {
            continue;
        }
        entries.push(Step::new(StepKind::Model {
            model_id: model.id.clone(),
            name: model.name.clone(),
            instructions: model.instructions.clone(),
            max_tokens: model.max_tokens,
            temperature: model.temperature,
            timeout_seconds: model.timeout_seconds,
        }));
    }
    entries
}

pub(crate) fn pool_items(catalog: &[Step], settings: &AppSettings) -> Vec<PoolItem> {
    catalog.iter().enumerate().map(|(index, step)| {
        let mut summary = step.summary();
        let (group, metadata) = match &step.kind {
            StepKind::Transcription { provider, model, language, .. } => (0, format!(
                "speech transcription speech-to-text audio {provider} {} {model} {language} {}",
                TranscriptionProvider::from_id(provider).label(),
                if provider == "local_sherpa_onnx" { "local offline" } else { "online cloud" }
            )),
            StepKind::Gemini { model, preset, prompt } => (1, format!("language model online cloud gemini google {model} {preset} {prompt}")),
            StepKind::Model { model_id, instructions, .. } => {
                let detail = settings.model_registry.find(model_id).ok().map(|(model, connection)| {
                    summary = format!("{} · {}", model.model, connection.provider);
                    format!("{} {} {}", model.model, connection.provider,
                        if ["ollama", "ollaya"].contains(&connection.provider.as_str()) { "local offline" } else { "online compatible" })
                }).unwrap_or_default();
                (if settings.model_registry.is_decision(model_id) { 3 } else { 1 }, format!("language model LLM assistant {} {detail} {instructions}", if settings.model_registry.is_decision(model_id) { "decision router" } else { "rewrite" }))
            }
            StepKind::Formatting(_) => (2, format!("formatting text processing tool numbers dates times money measurements replacements {}", step.summary())),
            StepKind::Punctuation(_) => (2, format!("punctuation capitalization text processing tool local offline {}", step.summary())),
        };
        let title = step.label();
        PoolItem {
            index, group,
            search: format!("{} {title} {summary} {metadata}", POOL_GROUPS[group]).to_lowercase(),
            card: PipelineCard { title: title.into(), summary: summary.into(), icon: icon(step), speech: step.is_transcription() },
        }
    }).collect()
}
pub fn move_step(draft: &mut Pipeline, source: usize, destination: usize) -> Result<(), String> {
    if source == 0 || destination == 0 {
        return Err("Speech must remain first.".into());
    }
    if source >= draft.steps.len() || destination > draft.steps.len() {
        return Err("Invalid insertion position.".into());
    }
    let destination = if destination > source {
        destination - 1
    } else {
        destination
    };
    if destination == source {
        return Ok(());
    }
    let s = draft.steps.remove(source);
    draft.steps.insert(destination, s);
    draft.legacy_formatting = None;
    Ok(())
}
pub fn add_step(draft: &mut Pipeline, mut step: Step, position: usize) -> Result<(), String> {
    step.id = new_id();
    if step.is_transcription() {
        if draft.steps.first().is_some_and(Step::is_transcription) {
            draft.steps[0] = step;
        } else {
            draft.steps.insert(0, step);
        }
    } else {
        if position == 0 || draft.steps.is_empty() {
            return Err("Add a speech component first; text steps go after it.".into());
        }
        draft.steps.insert(position.min(draft.steps.len()), step);
        draft.legacy_formatting = None;
    }
    Ok(())
}
pub(crate) fn commit(
    shared: &Arc<Mutex<AppSettings>>,
    path: &std::path::Path,
    change: impl FnOnce(&mut PipelineSet) -> Result<(), String>,
) -> Result<(), String> {
    let mut lock = shared.lock().unwrap();
    let mut next = lock.clone();
    let set = next
        .pipelines
        .as_mut()
        .ok_or("Pipeline configuration is missing.")?;
    change(set)?;
    if set.version < 5
        && set.pipelines.iter().any(|p| {
            p.assistant.as_ref().is_some_and(|c| {
                c.output_steps.is_some()
                    || !c.direct_steps.is_empty()
                    || !c.delegated_steps.is_empty()
            })
        })
    {
        set.version = 4;
    }
    set.validate()?;
    if !crate::settings::save_settings_to_path(path, &next) {
        return Err("Could not save pipelines. Your previous configuration is unchanged.".into());
    }
    next.pipeline_migration_save_failed = false;
    *lock = next;
    Ok(())
}

pub fn install(
    ui: &AppWindow,
    shared: Arc<Mutex<AppSettings>>,
    assistant_editor: slint::Weak<AssistantPipelineWindow>,
) {
    install_with_assistant(
        ui,
        shared,
        crate::settings::settings_path(),
        Some(assistant_editor),
    );
}
#[cfg(test)]
fn install_at(ui: &AppWindow, shared: Arc<Mutex<AppSettings>>, path: std::path::PathBuf) {
    install_with_assistant(ui, shared, path, None);
}
fn install_with_assistant(
    ui: &AppWindow,
    shared: Arc<Mutex<AppSettings>>,
    path: std::path::PathBuf,
    assistant_editor: Option<slint::Weak<AssistantPipelineWindow>>,
) {
    refresh(ui, &shared.lock().unwrap());
    if shared.lock().unwrap().pipeline_migration_save_failed {
        ui.set_pipeline_error("Current setup is active in memory but migration could not be saved. Check folder permissions.".into());
    }
    let editor = Rc::new(RefCell::new(None::<PipelineEditorWindow>));
    {
        let editor = editor.clone();
        ui.on_model_component_created(move |name| {
            if let Some(editor) = editor.borrow().as_ref() {
                editor.set_pool_search(name.clone());
                editor.invoke_filter_pool(name, editor.get_pool_columns());
            }
        });
    }
    {
        let holder = editor.clone();
        ui.on_graph_node_state(move |id, state| {
            if let Some(e) = holder.borrow().as_ref() {
                use slint::Model;
                let nodes = e.get_graph_nodes();
                if let Some(nodes) = nodes
                    .as_any()
                    .downcast_ref::<VecModel<crate::GraphNodeRow>>()
                {
                    if let Some(index) = nodes.iter().position(|n| n.id == id) {
                        if let Some(mut n) = nodes.row_data(index) {
                            n.state = state;
                            nodes.set_row_data(index, n);
                        }
                    }
                }
            }
        });
    }
    let main = ui.as_weak();
    {
        let editor = editor.clone();
        let shared = shared.clone();
        let main = main.clone();
        ui.on_model_components_changed(move || {
            if let Some(main) = main.upgrade() {
                refresh(&main, &shared.lock().unwrap());
            }
            if let Some(editor) = editor.borrow().as_ref() {
                editor.invoke_refresh_pool();
            }
        });
    }

    for action in 0..3 {
        let path = path.clone();
        let main = main.clone();
        let shared = shared.clone();
        let callback = move |index: i32| {
            let result = commit(&shared, &path, |set| {
                let p = set
                    .pipelines
                    .get(index as usize)
                    .ok_or("Pipeline no longer exists.")?
                    .clone();
                match action {
                    0 => {
                        set.activate(&p.id)?;
                    }
                    1 => {
                        let mut p = p;
                        p.id = new_id();
                        p.name = format!("{} copy", p.name);
                        if let Some(g) = &mut p.graph {
                            g.hotkey.clear();
                            let mut ids = std::collections::HashMap::new();
                            for node in &mut g.nodes {
                                let id = new_id();
                                ids.insert(node.id.clone(), id.clone());
                                node.id = id;
                            }
                            for wire in &mut g.edges {
                                wire.from = ids[&wire.from].clone();
                                wire.to = ids[&wire.to].clone();
                            }
                        }
                        // IDs are scoped to a pipeline; retaining step IDs preserves migrated compatibility.
                        set.pipelines.push(p);
                    }
                    _ => {
                        if p.id == set.active_id && p.graph.is_none() {
                            return Err(
                                "Activate another pipeline before deleting this one.".into()
                            );
                        }
                        set.pipelines.remove(index as usize);
                        if p.id == set.active_id {
                            set.active_id = set
                                .pipelines
                                .first()
                                .map(|p| p.id.clone())
                                .unwrap_or_default();
                        }
                        if p.id == set.active_assistant_id {
                            set.active_assistant_id = set
                                .pipelines
                                .iter()
                                .find(|candidate| {
                                    candidate.graph.is_some()
                                        || candidate.output == PipelineOutput::Assistant
                                })
                                .map(|candidate| candidate.id.clone())
                                .unwrap_or_default();
                        }
                    }
                }
                Ok(())
            });
            if let Some(ui) = main.upgrade() {
                ui.set_pipeline_error(result.err().unwrap_or_default().into());
                refresh(&ui, &shared.lock().unwrap());
                let _ = ui.invoke_sync_graph_hotkeys(
                    serde_json::to_string(&crate::graph_ui::hotkey_specs(&shared.lock().unwrap()))
                        .unwrap()
                        .into(),
                );
            }
            if action == 0
                && main.upgrade().is_some_and(|ui| {
                    !ui.get_is_recording()
                        && !ui.get_is_finalizing()
                        && !ui.get_local_download_progress_visible()
                        && !ui.get_post_model_installing()
                })
            {
                let lock = shared.lock().unwrap();
                if let Some(p) = lock.pipelines.as_ref().and_then(|s| s.active().ok()) {
                    if let Ok(c) = p.transcription_config(&lock) {
                        if c.provider == TranscriptionProvider::LocalSherpaOnnx
                            && crate::transcription::local_models_available(&c.local)
                        {
                            crate::transcription::preload_local_engine(&c.local);
                        }
                    }
                }
            }
        };
        match action {
            0 => ui.on_use_pipeline(callback),
            1 => ui.on_duplicate_pipeline(callback),
            _ => ui.on_delete_pipeline(callback),
        }
    }
    let path_edit = path.clone();
    let shared_edit = shared.clone();
    let editor_edit = editor.clone();
    let main_edit = main.clone();
    let assistant_edit = assistant_editor.clone();
    ui.on_edit_pipeline(move |index| {
        let p = shared_edit
            .lock()
            .unwrap()
            .pipelines
            .as_ref()
            .and_then(|s| s.pipelines.get(index as usize))
            .cloned();
        if let Some(p) = p {
            open_editor(
                p,
                shared_edit.clone(),
                main_edit.clone(),
                editor_edit.clone(),
                path_edit.clone(),
                assistant_edit.clone(),
            );
        }
    });
    let new_shared = shared.clone();
    let new_main = main.clone();
    let new_editor = editor.clone();
    let new_path = path.clone();
    let new_assistant_editor = assistant_editor.clone();
    ui.on_new_pipeline(move || {
        open_editor(
            Pipeline {
                id: new_id(),
                name: "New pipeline".into(),
                output: PipelineOutput::Transcription,
                steps: Vec::new(),
                assistant: None,
                legacy_formatting: None,
                graph: None,
            },
            shared.clone(),
            main.clone(),
            editor.clone(),
            path.clone(),
            assistant_editor.clone(),
        )
    });
    ui.on_new_assistant_pipeline(move || {
        open_editor(
            Pipeline {
                id: new_id(),
                name: "New assistant pipeline".into(),
                output: PipelineOutput::Assistant,
                steps: Vec::new(),
                assistant: Some(AssistantConfig {
                    instructions:
                        "You are Echo, a helpful voice assistant. Answer clearly and concisely."
                            .into(),
                    ..Default::default()
                }),
                legacy_formatting: None,
                graph: None,
            },
            new_shared.clone(),
            new_main.clone(),
            new_editor.clone(),
            new_path.clone(),
            new_assistant_editor.clone(),
        )
    });
}
fn open_editor(
    pipeline: Pipeline,
    shared: Arc<Mutex<AppSettings>>,
    main: slint::Weak<AppWindow>,
    holder: Rc<RefCell<Option<PipelineEditorWindow>>>,
    path: std::path::PathBuf,
    assistant_editor: Option<slint::Weak<AssistantPipelineWindow>>,
) {
    if let Some(existing) = holder.borrow().as_ref() {
        let _ = existing.show();
        return;
    }
    crate::graph_ui::open(pipeline, shared, main, holder, path, assistant_editor);
}
#[allow(dead_code)]
fn open_legacy_editor(
    pipeline: Pipeline,
    shared: Arc<Mutex<AppSettings>>,
    main: slint::Weak<AppWindow>,
    holder: Rc<RefCell<Option<PipelineEditorWindow>>>,
    path: std::path::PathBuf,
    assistant_editor: Option<slint::Weak<AssistantPipelineWindow>>,
) {
    if let Some(existing) = holder.borrow().as_ref() {
        let _ = existing.show();
        return;
    }
    let Ok(editor) = PipelineEditorWindow::new() else {
        return;
    };
    let mut pipeline = pipeline;
    if let Some(c) = &mut pipeline.assistant {
        c.output_steps = Some(c.effective_output_steps());
    }
    let draft = Rc::new(RefCell::new(pipeline));
    let history = Rc::new(RefCell::new(Vec::<Pipeline>::new()));
    let assistant_for_close = assistant_editor.clone();
    let catalog = Rc::new(RefCell::new(pool(&shared.lock().unwrap())));
    editor.set_pipeline_name(draft.borrow().name.clone().into());
    editor.set_assistant_mode(draft.borrow().output == PipelineOutput::Assistant);
    crate::pipeline_editor::refresh(&editor, &draft.borrow(), &shared.lock().unwrap());
    let pool_items = Rc::new(RefCell::new(pool_items(
        &catalog.borrow(),
        &shared.lock().unwrap(),
    )));
    editor.set_pool_groups(pool_groups(
        &pool_items.borrow(),
        "",
        editor.get_pool_columns().max(1) as usize,
    ));
    {
        let e = editor.as_weak();
        let d = draft.clone();
        let catalog = catalog.clone();
        let settings = shared.clone();
        editor.on_filter_pool(move |query, _| {
            if let Some(e) = e.upgrade() {
                e.set_pool_search(query);
                crate::pipeline_editor::filter(
                    &e,
                    &catalog.borrow(),
                    &d.borrow(),
                    &settings.lock().unwrap(),
                );
            }
        });
    }
    {
        let e = editor.as_weak();
        editor.on_open_picker(move |stage, index| {
            if let Some(e) = e.upgrade() {
                e.set_picker_stage(stage);
                e.set_picker_index(index);
                e.set_pool_search("".into());
                e.invoke_filter_pool("".into(), 1);
                e.set_picker_open(true);
            }
        });
    }
    {
        let e = editor.as_weak();
        let d = draft.clone();
        let settings = shared.clone();
        let catalog = catalog.clone();
        let history = history.clone();
        editor.on_pick_component(move |choice| {
            if let Some(e) = e.upgrade() {
                let before = d.borrow().clone();
                let mut next = before.clone();
                let result = crate::pipeline_editor::select(
                    &mut next,
                    &settings.lock().unwrap(),
                    e.get_picker_stage(),
                    e.get_picker_index(),
                    choice,
                    &catalog.borrow(),
                );
                match result {
                    Ok(index) => {
                        history.borrow_mut().push(before);
                        *d.borrow_mut() = next;
                        e.set_dirty(true);
                        e.set_undo_available(true);
                        e.set_error("".into());
                        crate::pipeline_editor::refresh(&e, &d.borrow(), &settings.lock().unwrap());
                        e.set_reveal_index(-1);
                        e.set_reveal_stage(e.get_picker_stage());
                        e.set_reveal_index(index as i32);
                        e.invoke_filter_pool(e.get_pool_search(), 1);
                    }
                    Err(error) => e.set_error(error.into()),
                }
            }
        });
    }
    {
        let e = editor.as_weak();
        let d = draft.clone();
        let settings = shared.clone();
        let history = history.clone();
        editor.on_undo(move || {
            if let Some(e) = e.upgrade() {
                let previous = history.borrow_mut().pop();
                if let Some(previous) = previous {
                    *d.borrow_mut() = previous;
                    crate::pipeline_editor::refresh(&e, &d.borrow(), &settings.lock().unwrap());
                    e.set_dirty(true);
                    e.set_undo_available(!history.borrow().is_empty());
                    e.set_error("".into());
                    e.invoke_filter_pool(e.get_pool_search(), 1);
                }
            }
        });
    }
    {
        let main = main.clone();
        editor.on_create_component(move || {
            if let Some(main) = main.upgrade() {
                main.invoke_create_model_component();
            }
        });
        let editor_weak = editor.as_weak();
        let catalog = catalog.clone();
        let settings = shared.clone();
        editor.on_refresh_pool(move || {
            if let Some(editor) = editor_weak.upgrade() {
                let settings = settings.lock().unwrap();
                let next = pool(&settings);
                *pool_items.borrow_mut() = crate::pipeline_ui::pool_items(&next, &settings);
                *catalog.borrow_mut() = next;
                drop(settings);
                editor.invoke_filter_pool(editor.get_pool_search(), 1);
            }
        });
    }
    refresh_assistant_cards(&editor, &draft.borrow(), &shared.lock().unwrap());
    let dialog = Rc::new(RefCell::new(None::<PipelineStepWindow>));
    {
        let e = editor.as_weak();
        let d = draft.clone();
        let settings = shared.clone();
        let history = history.clone();
        let dialog = dialog.clone();
        editor.on_stage_action(move |stage, index, action| {
            let (stage, index) = if stage == 2 && index == -3 {
                (1, -1)
            } else {
                (stage, index)
            };
            if let Some(e) = e.upgrade() {
                let empty_role = index == -1
                    && d.borrow().assistant.as_ref().is_some_and(|c| match stage {
                        1 => c.router_model_id.is_empty(),
                        2 => c.fast_answer_model_id.is_empty(),
                        3 => c.answer_model_id.is_empty(),
                        _ => false,
                    });
                if action == 4
                    || action == 0 && index == -1 && (stage == 0 || stage == 4 || empty_role)
                {
                    e.invoke_open_picker(stage, index);
                    return;
                }
                if action == 0 {
                    if index >= 0
                        && crate::pipeline_editor::step_at(&d.borrow(), stage, index as usize)
                            .is_some()
                    {
                        open_step_at(
                            stage,
                            index as usize,
                            d.clone(),
                            e.as_weak(),
                            dialog.clone(),
                            settings.clone(),
                            Some(history.clone()),
                        );
                    } else if stage != 0 {
                        let role = if stage == 4 {
                            if d.borrow()
                                .assistant
                                .as_ref()
                                .and_then(|c| c.output_steps.as_ref())
                                .and_then(|s| s.get(index as usize))
                                .is_some_and(|s| matches!(s, OutputStep::Speech))
                            {
                                4
                            } else {
                                5
                            }
                        } else {
                            stage
                        };
                        e.set_configuration_role(role);
                        e.invoke_configure_assistant();
                    }
                    return;
                }
                let before = d.borrow().clone();
                let mut next = before.clone();
                let result = crate::pipeline_editor::edit(
                    &mut next,
                    stage,
                    index,
                    action,
                    &settings.lock().unwrap(),
                );
                match result {
                    Ok(position) => {
                        history.borrow_mut().push(before);
                        *d.borrow_mut() = next;
                        e.set_dirty(true);
                        e.set_undo_available(true);
                        e.set_error("".into());
                        crate::pipeline_editor::refresh(&e, &d.borrow(), &settings.lock().unwrap());
                        e.set_reveal_index(-1);
                        e.set_reveal_stage(stage);
                        e.set_reveal_index(position as i32);
                    }
                    Err(error) => e.set_error(error.into()),
                }
            }
        });
    }
    if let Some(assistant_window) = assistant_editor {
        let draft_for_open = draft.clone();
        let shared_for_open = shared.clone();
        let assistant_for_open = assistant_window.clone();
        let editor_for_open = editor.as_weak();
        editor.on_configure_assistant(move || {
            let Some(window) = assistant_for_open.upgrade() else {
                return;
            };
            if window.get_editing_voice() {
                return;
            }
            window.set_default_voice_settings(false);
            window.set_configuration_role(
                editor_for_open
                    .upgrade()
                    .map(|e| e.get_configuration_role())
                    .unwrap_or(-1),
            );
            let settings = shared_for_open.lock().unwrap().clone();
            let draft = draft_for_open.borrow();
            let Some(config) = draft.assistant.as_ref() else {
                return;
            };
            let models = settings
                .model_registry
                .models
                .iter()
                .filter(|model| model.verified)
                .map(|model| crate::assistant_choice_label(&model.name, &model.id).into())
                .collect::<Vec<slint::SharedString>>();
            window.set_model_options(ModelRc::new(VecModel::from(models)));
            window.set_answer_model_options(ModelRc::new(VecModel::from(
                settings
                    .model_registry
                    .models
                    .iter()
                    .filter(|m| m.verified && !settings.model_registry.is_decision(&m.id))
                    .map(|m| crate::assistant_choice_label(&m.name, &m.id).into())
                    .collect::<Vec<slint::SharedString>>(),
            )));
            window
                .set_decision_router(settings.model_registry.is_decision(&config.router_model_id));
            window.set_fast_answer_name(
                settings
                    .model_registry
                    .models
                    .iter()
                    .find(|m| m.id == config.fast_answer_model_id)
                    .map(|m| crate::assistant_choice_label(&m.name, &m.id))
                    .unwrap_or_default()
                    .into(),
            );
            window.set_router_name(
                settings
                    .model_registry
                    .models
                    .iter()
                    .find(|m| m.id == config.router_model_id)
                    .map(|m| crate::assistant_choice_label(&m.name, &m.id))
                    .unwrap_or_default()
                    .into(),
            );
            window.set_answer_name(
                settings
                    .model_registry
                    .models
                    .iter()
                    .find(|m| m.id == config.answer_model_id)
                    .map(|m| crate::assistant_choice_label(&m.name, &m.id))
                    .unwrap_or_default()
                    .into(),
            );
            window.set_preload_router_on_start(config.preload_router_on_start);
            window.set_preload_answer_on_start(config.preload_answer_on_start);
            window.set_preload_voice_on_start(config.preload_voice_on_start);
            window.set_router_local_model(assistant_choice_is_ollama(
                &settings,
                &window.get_router_name(),
            ));
            window.set_answer_local_model(assistant_choice_is_ollama(
                &settings,
                &window.get_answer_name(),
            ));
            window.set_instructions(config.instructions.clone().into());
            window.set_voice_path(config.voice_id.clone().into());
            window.set_error("".into());
            window.set_voice_status("".into());
            let _ = window.show();
        });
        let draft_for_save = draft.clone();
        let history_for_save = history.clone();
        let shared_for_save = shared.clone();
        let editor_for_save = editor.as_weak();
        let assistant_for_save = assistant_window.clone();
        if let Some(window) = assistant_window.upgrade() {
            let selection_window = assistant_window.clone();
            let selection_settings = shared.clone();
            window.on_model_selection_changed(move || {
                if let Some(window) = selection_window.upgrade() {
                    let settings = selection_settings.lock().unwrap();
                    window.set_decision_router(settings.model_registry.models.iter().any(|m| {
                        crate::assistant_choice_label(&m.name, &m.id)
                            == window.get_router_name().as_str()
                            && settings.model_registry.is_decision(&m.id)
                    }));
                    window.set_router_local_model(assistant_choice_is_ollama(
                        &settings,
                        &window.get_router_name(),
                    ));
                    window.set_answer_local_model(assistant_choice_is_ollama(
                        &settings,
                        &window.get_answer_name(),
                    ));
                }
            });
            window.on_save(move || {
                let Some(window) = assistant_for_save.upgrade() else {
                    return;
                };
                if window.get_editing_voice() || window.get_recording_voice() {
                    window.set_error("Finish editing or recording the voice before saving.".into());
                    return;
                }
                let settings = shared_for_save.lock().unwrap().clone();
                let selected = [
                    window.get_router_name().to_string(),
                    window.get_answer_name().to_string(),
                    window.get_fast_answer_name().to_string(),
                ];
                let ids = selected
                    .iter()
                    .map(|label| {
                        settings
                            .model_registry
                            .models
                            .iter()
                            .find(|m| {
                                m.verified
                                    && crate::assistant_choice_label(&m.name, &m.id) == *label
                            })
                            .map(|m| m.id.clone())
                    })
                    .collect::<Vec<_>>();
                let role = window.get_configuration_role();
                let existing = draft_for_save
                    .borrow()
                    .assistant
                    .clone()
                    .unwrap_or_default();
                let router = if role == -1 || role == 1 {
                    ids[0].clone()
                } else {
                    Some(existing.router_model_id)
                };
                let answer = if role == -1 || role == 3 {
                    ids[1].clone()
                } else {
                    Some(existing.answer_model_id)
                };
                let fast = if role == -1 || role == 2 {
                    ids[2].clone()
                } else {
                    Some(existing.fast_answer_model_id)
                };
                let (Some(router), Some(answer)) = (router, answer) else {
                    window.set_error("Choose a tested model for this component.".into());
                    return;
                };
                let decision = settings.model_registry.is_decision(&router);
                if (role == -1 || role == 3) && settings.model_registry.is_decision(&answer)
                    || (role == -1 || role == 2)
                        && decision
                        && fast.as_ref().is_none_or(|id| {
                            settings.model_registry.is_decision(id) || id.is_empty()
                        })
                {
                    window.set_error("Select text models for answer components.".into());
                    return;
                }
                history_for_save
                    .borrow_mut()
                    .push(draft_for_save.borrow().clone());
                if let Some(config) = draft_for_save.borrow_mut().assistant.as_mut() {
                    if role == 1
                        && decision
                        && config.fast_answer_model_id.is_empty()
                        && !settings.model_registry.is_decision(&config.router_model_id)
                    {
                        config.fast_answer_model_id = config.router_model_id.clone();
                    }
                    config.router_model_id = router.clone();
                    config.answer_model_id = answer.clone();
                    if role != 1 {
                        config.fast_answer_model_id = fast.unwrap_or_default();
                    }
                    config.instructions = window.get_instructions().to_string();
                    config.voice_id = window.get_voice_path().to_string();
                    config.preload_router_on_start = settings
                        .model_registry
                        .find(&router)
                        .is_ok_and(|(_, connection)| {
                            ["ollama", "ollaya"].contains(&connection.provider.as_str())
                        })
                        && window.get_preload_router_on_start();
                    config.preload_answer_on_start = settings
                        .model_registry
                        .find(&answer)
                        .is_ok_and(|(_, connection)| {
                            ["ollama", "ollaya"].contains(&connection.provider.as_str())
                        })
                        && window.get_preload_answer_on_start();
                    config.preload_voice_on_start =
                        !config.voice_id.is_empty() && window.get_preload_voice_on_start();
                }
                if let Some(editor) = editor_for_save.upgrade() {
                    crate::pipeline_editor::refresh(&editor, &draft_for_save.borrow(), &settings);
                    editor.set_dirty(true);
                    editor.set_undo_available(true);
                }
                let _ = window.hide();
            });
        }
    }
    {
        let draft_for_drop = draft.clone();
        let catalog_for_drop = catalog.clone();
        let editor_for_drop = editor.as_weak();
        let settings_for_drop = shared.clone();
        editor.on_assign_assistant_model(move |source, slot| {
            let Some(editor) = editor_for_drop.upgrade() else {
                return;
            };
            let catalog = catalog_for_drop.borrow();
            let Some(step) = catalog.get(source.max(0) as usize) else {
                return;
            };
            let StepKind::Model { model_id, .. } = &step.kind else {
                editor.set_error("Drop a tested text model onto this assistant card.".into());
                return;
            };
            let settings = settings_for_drop.lock().unwrap();
            if slot != 0 && settings.model_registry.is_decision(model_id) {
                editor.set_error("Decision models belong on the router card.".into());
                return;
            }
            drop(settings);
            if let Some(config) = draft_for_drop.borrow_mut().assistant.as_mut() {
                if slot == 0 {
                    if settings_for_drop
                        .lock()
                        .unwrap()
                        .model_registry
                        .is_decision(model_id)
                        && config.fast_answer_model_id.is_empty()
                    {
                        config.fast_answer_model_id = if settings_for_drop
                            .lock()
                            .unwrap()
                            .model_registry
                            .is_decision(&config.router_model_id)
                        {
                            config.answer_model_id.clone()
                        } else {
                            config.router_model_id.clone()
                        };
                    }
                    config.router_model_id = model_id.clone();
                } else if slot == 2 {
                    config.fast_answer_model_id = model_id.clone();
                } else {
                    config.answer_model_id = model_id.clone();
                }
                editor.set_dirty(true);
                editor.set_error("".into());
            }
            crate::pipeline_editor::refresh(
                &editor,
                &draft_for_drop.borrow(),
                &settings_for_drop.lock().unwrap(),
            );
        });
    }
    {
        let d = draft.clone();
        let e = editor.as_weak();
        let catalog = catalog.clone();
        let settings_for_add = shared.clone();
        let history = history.clone();
        editor.on_add(move |index, position| {
            if let Some(e) = e.upgrade() {
                let before = d.borrow().clone();
                let result = catalog
                    .borrow()
                    .get(index as usize)
                    .ok_or("Unknown component.".into())
                    .and_then(|s| {
                        if let StepKind::Model { model_id, .. } = &s.kind {
                            if settings_for_add
                                .lock()
                                .unwrap()
                                .model_registry
                                .is_decision(model_id)
                            {
                                return Err(
                                    "Drop decision models onto the assistant router.".into()
                                );
                            }
                        }
                        add_step(&mut d.borrow_mut(), s.clone(), position.max(0) as usize)
                    });
                e.set_error(result.as_ref().err().cloned().unwrap_or_default().into());
                if result.is_ok() {
                    e.set_dirty(true);
                    history.borrow_mut().push(before);
                    e.set_undo_available(true);
                    crate::pipeline_editor::refresh(
                        &e,
                        &d.borrow(),
                        &settings_for_add.lock().unwrap(),
                    );
                }
            }
        });
    }
    {
        let d = draft.clone();
        let e = editor.as_weak();
        let settings = shared.clone();
        let history = history.clone();
        editor.on_move_step(move |source, destination| {
            if let Some(e) = e.upgrade() {
                let before = d.borrow().clone();
                let result = move_step(
                    &mut d.borrow_mut(),
                    source.max(0) as usize,
                    destination.max(0) as usize,
                );
                e.set_error(result.as_ref().err().cloned().unwrap_or_default().into());
                if result.is_ok() {
                    e.set_dirty(true);
                    history.borrow_mut().push(before);
                    e.set_undo_available(true);
                    crate::pipeline_editor::refresh(&e, &d.borrow(), &settings.lock().unwrap());
                }
            }
        });
    }
    {
        let e = editor.as_weak();
        editor.on_remove_step(move |index| {
            if let Some(e) = e.upgrade() {
                e.invoke_stage_action(0, index, 3);
            }
        });
    }
    {
        let d = draft.clone();
        let e = editor.as_weak();
        let dialog = dialog.clone();
        let shared = shared.clone();
        editor.on_configure(move |index| {
            open_step(
                index.max(0) as usize,
                d.clone(),
                e.clone(),
                dialog.clone(),
                shared.clone(),
            )
        });
    }
    {
        let d = draft.clone();
        let e = editor.as_weak();
        let holder = holder.clone();
        let dialog = dialog.clone();
        let assistant_for_close = assistant_for_close.clone();
        editor.on_save(move || {
            if let Some(e) = e.upgrade() {
                if assistant_for_close
                    .as_ref()
                    .and_then(|weak| weak.upgrade())
                    .is_some_and(|window| {
                        window.get_editing_voice() || window.get_recording_voice()
                    })
                {
                    e.set_error("Finish editing or recording the voice before saving.".into());
                    return;
                }
                d.borrow_mut().name = e.get_pipeline_name().trim().into();
                let p = d.borrow().clone();
                let result = p
                    .validate()
                    .and_then(|_| {
                        if let Some(c) = &p.assistant {
                            let settings = shared.lock().unwrap();
                            if settings.model_registry.is_decision(&c.router_model_id)
                                && c.fast_answer_model_id.is_empty()
                            {
                                return Err("Add a direct answer model before saving.".into());
                            }
                            for id in [&c.router_model_id, &c.answer_model_id].into_iter().chain(
                                settings
                                    .model_registry
                                    .is_decision(&c.router_model_id)
                                    .then_some(&c.fast_answer_model_id),
                            ) {
                                let (model, _) = settings.model_registry.find(id)?;
                                if !model.verified {
                                    return Err("Choose tested assistant models.".into());
                                }
                            }
                        }
                        Ok(())
                    })
                    .and_then(|_| {
                        commit(&shared, &path, |set| {
                            if let Some(old) = set.pipelines.iter_mut().find(|old| old.id == p.id) {
                                *old = p;
                            } else {
                                if p.output == PipelineOutput::Assistant
                                    && set.active_assistant_id.is_empty()
                                {
                                    set.active_assistant_id = p.id.clone();
                                }
                                set.pipelines.push(p);
                            }
                            Ok(())
                        })
                    });
                match result {
                    Err(err) => e.set_error(err.into()),
                    Ok(()) => {
                        if let Some(window) =
                            assistant_for_close.as_ref().and_then(|weak| weak.upgrade())
                        {
                            window.invoke_cancel();
                        }
                        if let Some(main) = main.upgrade() {
                            refresh(&main, &shared.lock().unwrap());
                            main.set_pipeline_error("".into());
                        }
                        if let Some(w) = dialog.borrow_mut().take() {
                            let _ = w.hide();
                        }
                        let _ = e.hide();
                        holder.borrow_mut().take();
                    }
                }
            }
        });
    }
    {
        let e = editor.as_weak();
        let holder = holder.clone();
        let dialog = dialog.clone();
        let assistant_for_close = assistant_for_close.clone();
        editor.on_cancel(move || {
            if let Some(window) = assistant_for_close.as_ref().and_then(|weak| weak.upgrade()) {
                window.invoke_cancel();
            }
            if let Some(e) = e.upgrade() {
                let _ = e.hide();
            }
            if let Some(w) = dialog.borrow_mut().take() {
                let _ = w.hide();
            }
            holder.borrow_mut().take();
        });
    }
    let e = editor.as_weak();
    editor.window().on_close_requested(move || {
        if let Some(e) = e.upgrade() {
            if e.get_dirty() {
                e.set_close_prompt(true);
            } else {
                e.invoke_cancel();
            }
        }
        CloseRequestResponse::KeepWindowShown
    });
    let _ = editor.show();
    *holder.borrow_mut() = Some(editor);
}
/// Isolated development harness: synthetic models, temporary settings, no audio or hotkeys.
#[cfg(test)]
fn inspect_fixture(assistant: bool) -> Result<(), Box<dyn std::error::Error>> {
    let mut settings = AppSettings::default();
    settings
        .model_registry
        .connections
        .push(crate::models::ModelConnection {
            id: "fixture".into(),
            provider: "ollama".into(),
            endpoint: "http://127.0.0.1:11434".into(),
            api_version: String::new(),
            custom_header_name: String::new(),
            custom_header_value: String::new(),
            api_key: String::new(),
            allow_lan_http: false,
        });
    for (id, name) in [
        ("fast", "Fast answer model"),
        ("strong", "Stronger answer model"),
    ] {
        let mut model =
            crate::models::ConfiguredModel::new("fixture".into(), name.into(), id.into());
        model.id = id.into();
        model.verified = true;
        settings.model_registry.models.push(model);
    }
    let mut p = migrate(&settings).active()?.clone();
    p.name = "Connected pipeline inspector".into();
    p.steps = vec![
        pool(&settings)
            .into_iter()
            .find(|s| s.label() == "Parakeet Ultra")
            .unwrap(),
        Step::new(StepKind::Formatting(formatting_only(
            settings.post_processing.clone(),
        ))),
    ];
    if assistant {
        p.output = PipelineOutput::Assistant;
        p.assistant = Some(AssistantConfig {
            router_model_id: "fast".into(),
            answer_model_id: "strong".into(),
            output_steps: Some(vec![OutputStep::Speech, OutputStep::Chat]),
            ..Default::default()
        });
    }
    settings.pipelines = Some(migrate(&settings));
    let shared = Arc::new(Mutex::new(settings));
    let main = AppWindow::new()?;
    let assistant_window = AssistantPipelineWindow::new()?;
    crate::install_tts_settings_entrypoint(&main, &assistant_window);
    let edit_voice = crate::AssistantPipelineWindow::new().unwrap();
    crate::voice_ui::install(&main, &assistant_window, &edit_voice, shared.clone())?;
    let assistant_weak = assistant_window.as_weak();
    assistant_window.on_cancel(move || {
        if let Some(w) = assistant_weak.upgrade() {
            let _ = w.hide();
        }
    });
    let holder = Rc::new(RefCell::new(None));
    let path = std::env::temp_dir().join(format!("echo-inspector-{}.json", new_id()));
    open_editor(
        p,
        shared,
        main.as_weak(),
        holder.clone(),
        path.clone(),
        Some(assistant_window.as_weak()),
    );
    let window = holder.borrow().as_ref().unwrap().clone_strong();
    if let Ok(value) = std::env::var("SLINT_SCALE_FACTOR") {
        if let Ok(scale) = value.parse::<f32>() {
            let weak = window.as_weak();
            slint::Timer::single_shot(std::time::Duration::from_millis(150), move || {
                if let Some(w) = weak.upgrade() {
                    w.window()
                        .dispatch_event(slint::platform::WindowEvent::ScaleFactorChanged {
                            scale_factor: scale,
                        });
                    // A headless scale event does not supply the logical resize
                    // that Windows normally sends with a DPI change.
                    if let (Ok(width), Ok(height)) = (
                        std::env::var("ECHO_GUI_WIDTH"),
                        std::env::var("ECHO_GUI_HEIGHT"),
                    ) {
                        if let (Ok(width), Ok(height)) =
                            (width.parse::<f32>(), height.parse::<f32>())
                        {
                            w.window()
                                .dispatch_event(slint::platform::WindowEvent::Resized {
                                    size: slint::LogicalSize::new(width / scale, height / scale),
                                });
                        }
                    }
                }
            });
        }
    }
    if let (Ok(width), Ok(height)) = (
        std::env::var("ECHO_GUI_WIDTH"),
        std::env::var("ECHO_GUI_HEIGHT"),
    ) {
        if let (Ok(width), Ok(height)) = (width.parse(), height.parse()) {
            window
                .window()
                .set_size(slint::PhysicalSize::new(width, height));
        }
    }
    if let Some(snapshot_path) = std::env::var_os("ECHO_GUI_SNAPSHOT_PATH") {
        let weak = window.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(500), move || {
            if let Some(w) = weak.upgrade() {
                let snapshot = w.window().take_snapshot().unwrap();
                image::save_buffer(
                    snapshot_path,
                    snapshot.as_bytes(),
                    snapshot.width(),
                    snapshot.height(),
                    image::ColorType::Rgba8,
                )
                .unwrap();
                w.invoke_cancel();
            }
        });
    }
    slint::run_event_loop()?;
    let _ = std::fs::remove_file(path);
    Ok(())
}
fn lines(s: slint::SharedString) -> Vec<String> {
    s.lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}
fn open_step(
    index: usize,
    draft: Rc<RefCell<Pipeline>>,
    editor: slint::Weak<PipelineEditorWindow>,
    holder: Rc<RefCell<Option<PipelineStepWindow>>>,
    shared: Arc<Mutex<AppSettings>>,
) {
    open_step_at(0, index, draft, editor, holder, shared, None);
}
pub(crate) fn open_step_at(
    stage: i32,
    index: usize,
    draft: Rc<RefCell<Pipeline>>,
    editor: slint::Weak<PipelineEditorWindow>,
    holder: Rc<RefCell<Option<PipelineStepWindow>>>,
    shared: Arc<Mutex<AppSettings>>,
    history: Option<Rc<RefCell<Vec<Pipeline>>>>,
) {
    if let Some(w) = holder.borrow().as_ref() {
        let _ = w.show();
        return;
    }
    let Some(step) = crate::pipeline_editor::step_at(&draft.borrow(), stage, index).cloned() else {
        return;
    };
    let Ok(w) = PipelineStepWindow::new() else {
        return;
    };
    w.set_component_title(step.label().into());
    let supports_preload = match &step.kind {
        StepKind::Transcription { provider, .. } => provider == "local_sherpa_onnx",
        StepKind::Model { model_id, .. } => shared
            .lock()
            .unwrap()
            .model_registry
            .find(model_id)
            .is_ok_and(|(_, connection)| {
                ["ollama", "ollaya"].contains(&connection.provider.as_str())
            }),
        StepKind::Punctuation(_) => true,
        _ => false,
    };
    w.set_supports_preload(supports_preload);
    w.set_preload_on_start(step.preload_on_start());
    w.set_presets(ModelRc::new(VecModel::from(vec![
        "Minimal corrections".into(),
        "Sound like a pirate".into(),
        "Sound like a medieval knight".into(),
        "Custom".into(),
    ])));
    match &step.kind {
        StepKind::Transcription {
            provider,
            model,
            language,
            no_verbatim,
            local,
        } => {
            w.set_kind(if provider == "local_sherpa_onnx" {
                1
            } else {
                0
            });
            w.set_model(model.clone().into());
            w.set_language(language.clone().into());
            w.set_no_verbatim(*no_verbatim);
            w.set_cpu_threads(local.num_threads as f32);
            w.set_vad_threshold(local.vad_threshold);
            w.set_silence_ms(local.silence_ms as f32);
            w.set_pre_roll_ms(local.pre_roll_ms as f32);
            w.set_post_roll_ms(local.post_roll_ms as f32);
            w.set_min_speech_ms(local.min_speech_ms as f32);
            w.set_max_segment_seconds(local.max_segment_seconds as f32);
            w.set_partial_interval_ms(local.partial_interval_ms as f32);
            w.set_redecode_full_session(local.redecode_full_session);
        }
        StepKind::Gemini {
            model,
            preset,
            prompt,
        } => {
            w.set_kind(2);
            w.set_model(model.clone().into());
            w.set_preset(preset.clone().into());
            w.set_prompt(prompt.clone().into());
        }
        StepKind::Model {
            instructions,
            max_tokens,
            temperature,
            timeout_seconds,
            ..
        } => {
            w.set_kind(5);
            w.set_prompt(if let Some(g) = &draft.borrow().graph {
                let nid = g
                    .nodes
                    .iter()
                    .find(|n| n.step().is_some_and(|s| s.id == step.id))
                    .map(|n| &n.id);
                nid.and_then(|id| {
                    g.edges
                        .iter()
                        .find(|e| &e.to == id && e.input == "instructions")
                })
                .and_then(|e| g.nodes.iter().find(|n| n.id == e.from))
                .and_then(|n| {
                    if let crate::graph::Kind::Text { text } = &n.kind {
                        Some(text.clone())
                    } else {
                        None
                    }
                })
                .unwrap_or_default()
                .into()
            } else {
                instructions.clone().into()
            });
            w.set_max_tokens(max_tokens.to_string().into());
            w.set_temperature(
                temperature
                    .map(|v| v.to_string())
                    .unwrap_or_default()
                    .into(),
            );
            w.set_timeout_seconds(timeout_seconds.to_string().into());
        }
        StepKind::Formatting(s) | StepKind::Punctuation(s) => {
            w.set_kind(if matches!(step.kind, StepKind::Formatting(_)) {
                3
            } else {
                4
            });
            populate_local(&w, s);
        }
    }
    {
        let weak = w.as_weak();
        let holder = holder.clone();
        w.on_apply(move || {
            let Some(w) = weak.upgrade() else {
                return;
            };
            let mut replacement = step.clone();
            if supports_preload {
                replacement.preload_on_start = Some(w.get_preload_on_start());
            }
            match &mut replacement.kind {
                StepKind::Transcription {
                    model,
                    language,
                    no_verbatim,
                    local,
                    ..
                } => {
                    *model = w.get_model().trim().into();
                    *language = w.get_language().trim().into();
                    *no_verbatim = w.get_no_verbatim();
                    if w.get_kind() == 1 {
                        local.num_threads = w.get_cpu_threads().round() as i32;
                        local.vad_threshold = w.get_vad_threshold();
                        local.silence_ms = w.get_silence_ms().round() as u32;
                        local.pre_roll_ms = w.get_pre_roll_ms().round() as u32;
                        local.post_roll_ms = w.get_post_roll_ms().round() as u32;
                        local.min_speech_ms = w.get_min_speech_ms().round() as u32;
                        local.max_segment_seconds = w.get_max_segment_seconds().round() as u32;
                        local.partial_interval_ms = w.get_partial_interval_ms().round() as u32;
                        local.redecode_full_session = w.get_redecode_full_session();
                        *local = local.clone().normalized();
                    }
                }
                StepKind::Gemini {
                    model,
                    preset,
                    prompt,
                } => {
                    *model = w.get_model().trim().into();
                    *preset = w.get_preset().into();
                    *prompt = w.get_prompt().into();
                }
                StepKind::Model {
                    instructions,
                    max_tokens,
                    temperature,
                    timeout_seconds,
                    ..
                } => {
                    let Ok(tokens) = w.get_max_tokens().parse::<u64>() else {
                        w.set_error("Enter a valid output-token limit.".into());
                        return;
                    };
                    let Ok(timeout) = w.get_timeout_seconds().parse::<u64>() else {
                        w.set_error("Enter a timeout from 5 to 600 seconds.".into());
                        return;
                    };
                    let temp = if w.get_temperature().trim().is_empty() {
                        None
                    } else {
                        match w.get_temperature().parse::<f32>() {
                            Ok(value) if (0.0..=2.0).contains(&value) => Some(value),
                            _ => {
                                w.set_error("Temperature must be 0–2.".into());
                                return;
                            }
                        }
                    };
                    *instructions = w.get_prompt().trim().into();
                    *max_tokens = tokens;
                    *temperature = temp;
                    *timeout_seconds = timeout;
                }
                StepKind::Formatting(s) | StepKind::Punctuation(s) => read_local(&w, s),
            }
            let mut next = draft.borrow().clone();
            let Some(target) = crate::pipeline_editor::find_step_mut(&mut next, &replacement.id)
            else {
                return;
            };
            let changed = *target != replacement;
            *target = replacement.clone();
            if changed {
                next.legacy_formatting = None;
            }
            let mut validation = next.clone();
            if !validation.steps.first().is_some_and(Step::is_transcription) {
                if let Some(speech) = pool(&shared.lock().unwrap())
                    .into_iter()
                    .find(Step::is_transcription)
                {
                    validation.steps.insert(0, speech);
                }
            }
            if let Some(c) = &mut validation.assistant {
                let output = c.output_steps.get_or_insert_with(Vec::new);
                if !output.iter().any(|s| matches!(s, OutputStep::Chat)) {
                    output.push(OutputStep::Chat);
                }
            }
            if let Err(err) = if next.graph.is_some() {
                Ok(())
            } else {
                validation.validate_components()
            } {
                w.set_error(err.into());
                return;
            }
            if changed {
                if next.graph.is_some() {
                    if let Some(e) = editor.upgrade() {
                        e.invoke_graph_checkpoint();
                    }
                } else if let Some(h) = &history {
                    h.borrow_mut().push(draft.borrow().clone());
                }
            }
            if let Some(g) = &mut next.graph {
                if changed {
                    g.legacy_formatting = None;
                }
                let nid = g
                    .nodes
                    .iter()
                    .find(|n| n.step().is_some_and(|s| s.id == replacement.id))
                    .map(|n| n.id.clone());
                if let Some(nid) = nid {
                    let instructions = g
                        .nodes
                        .iter_mut()
                        .find(|n| n.id == nid)
                        .and_then(crate::graph::Node::step_mut)
                        .and_then(|s| {
                            if let StepKind::Model { instructions, .. } = &mut s.kind {
                                Some(std::mem::take(instructions))
                            } else {
                                None
                            }
                        })
                        .unwrap_or_default();
                    {
                        let upstream = g
                            .edges
                            .iter()
                            .find(|e| e.to == nid && e.input == "instructions")
                            .map(|e| e.from.clone());
                        if let Some(id) = upstream {
                            if let Some(crate::graph::Node {
                                kind: crate::graph::Kind::Text { text },
                                ..
                            }) = g.nodes.iter_mut().find(|n| n.id == id)
                            {
                                *text = instructions;
                            }
                        } else if !instructions.is_empty() {
                            let node = crate::graph::Node::new(
                                crate::graph::Kind::Text { text: instructions },
                                40.,
                                400.,
                            );
                            let id = node.id.clone();
                            g.nodes.push(node);
                            g.edges.push(crate::graph::Edge {
                                from: id,
                                output: "text".into(),
                                to: nid,
                                input: "instructions".into(),
                            });
                        }
                    }
                }
            }
            *draft.borrow_mut() = next;
            if let Some(e) = editor.upgrade() {
                crate::pipeline_editor::refresh(&e, &draft.borrow(), &shared.lock().unwrap());
                if let Some(h) = &history {
                    e.set_undo_available(!h.borrow().is_empty());
                }
                if changed {
                    e.set_dirty(true);
                }
            }
            let _ = w.hide();
            holder.borrow_mut().take();
        });
    }
    {
        let weak = w.as_weak();
        let holder = holder.clone();
        w.on_cancel(move || {
            if let Some(w) = weak.upgrade() {
                let _ = w.hide();
            }
            holder.borrow_mut().take();
        });
    }
    let weak = w.as_weak();
    w.window().on_close_requested(move || {
        if let Some(w) = weak.upgrade() {
            w.invoke_cancel();
        }
        CloseRequestResponse::KeepWindowShown
    });
    let _ = w.show();
    *holder.borrow_mut() = Some(w);
}

fn populate_local(w: &PipelineStepWindow, s: &PostProcessingSettings) {
    w.set_numbers(s.format_numbers);
    w.set_digits(s.prefer_digits);
    w.set_whole(s.whole_numbers);
    w.set_ordinals(s.ordinals);
    w.set_decimals(s.decimals_quantities);
    w.set_money(s.money);
    w.set_measurements(s.measurements);
    w.set_dates(s.dates);
    w.set_times(s.times);
    w.set_identifiers(s.telephone_alphanumeric);
    w.set_addresses(s.urls_emails);
    w.set_punctuation(s.punctuation);
    w.set_capitalization(s.capitalization);
    w.set_commas(s.commas);
    w.set_periods(s.periods);
    w.set_questions(s.question_marks);
    w.set_protected_phrases(s.protected_phrases.join("\n").into());
    w.set_replacements(s.custom_replacements.join("\n").into());
}
fn read_local(w: &PipelineStepWindow, s: &mut PostProcessingSettings) {
    s.format_numbers = w.get_numbers();
    s.prefer_digits = w.get_digits();
    s.whole_numbers = w.get_whole();
    s.ordinals = w.get_ordinals();
    s.decimals_quantities = w.get_decimals();
    s.money = w.get_money();
    s.measurements = w.get_measurements();
    s.dates = w.get_dates();
    s.times = w.get_times();
    s.telephone_alphanumeric = w.get_identifiers();
    s.urls_emails = w.get_addresses();
    s.punctuation = w.get_punctuation();
    s.capitalization = w.get_capitalization();
    s.commas = w.get_commas();
    s.periods = w.get_periods();
    s.question_marks = w.get_questions();
    s.protected_phrases = lines(w.get_protected_phrases());
    s.custom_replacements = lines(w.get_replacements());
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "interactive development inspector; synthetic models and temporary settings"]
    fn connected_pipeline_interactive_fixture() {
        super::inspect_fixture(
            std::env::var("ECHO_PIPELINE_INSPECT_ASSISTANT").as_deref() == Ok("1"),
        )
        .unwrap();
    }
    use super::*;
    use slint::Model;

    fn visible_pool_entries(
        groups: ModelRc<crate::PipelinePoolGroup>,
    ) -> Vec<crate::PipelinePoolEntry> {
        groups
            .iter()
            .flat_map(|group| group.rows.iter().collect::<Vec<_>>())
            .flat_map(|row| row.entries.iter().collect::<Vec<_>>())
            .collect()
    }
    #[test]
    fn component_pool_searches_model_metadata_and_preserves_catalog_identity() {
        let mut settings = AppSettings::default();
        settings
            .model_registry
            .connections
            .push(crate::models::ModelConnection {
                id: "local".into(),
                provider: "ollama".into(),
                endpoint: "http://localhost:11434".into(),
                api_key: "private-key-never-searchable".into(),
                custom_header_value: "private-header-never-searchable".into(),
                api_version: String::new(),
                custom_header_name: String::new(),
                allow_lan_http: false,
            });
        let mut model = crate::models::ConfiguredModel::new(
            "local".into(),
            "Quick reply".into(),
            "qwen3:8b".into(),
        );
        model.instructions = "Summarize project notes".into();
        model.verified = true;
        settings.model_registry.models.push(model);
        let catalog = pool(&settings);
        let items = pool_items(&catalog, &settings);
        let original = catalog
            .iter()
            .position(|step| step.label() == "Quick reply")
            .unwrap();
        for query in [
            "QUICK",
            "QWEN3:8B",
            "ollama",
            "LLM",
            "summarize notes",
            "quick ollama",
        ] {
            let hits = visible_pool_entries(pool_groups(&items, query, 3));
            assert_eq!(hits.len(), 1, "query: {query}");
            assert_eq!(hits[0].catalog_index as usize, original);
            let mut draft = migrate(&settings).active().unwrap().clone();
            let end = draft.steps.len();
            add_step(
                &mut draft,
                catalog[hits[0].catalog_index as usize].clone(),
                end,
            )
            .unwrap();
            assert_eq!(draft.steps.last().unwrap().label(), "Quick reply");
        }
        for query in [
            "private-key-never-searchable",
            "private-header-never-searchable",
            "no-such-component",
        ] {
            assert_eq!(pool_groups(&items, query, 3).row_count(), 0);
        }
        assert!(visible_pool_entries(pool_groups(&items, "speech local", 2))
            .iter()
            .all(|entry| entry.card.speech));
        let all = visible_pool_entries(pool_groups(&items, "  ", 2));
        assert_eq!(all.len(), catalog.len());
        assert_eq!(
            all.iter()
                .map(|entry| entry.catalog_index)
                .collect::<std::collections::HashSet<_>>()
                .len(),
            catalog.len()
        );
    }
    #[test]
    fn component_pool_reflows_groups_without_reindexing_cards() {
        let settings = AppSettings::default();
        let catalog = pool(&settings);
        let items = pool_items(&catalog, &settings);
        let full = visible_pool_entries(pool_groups(&items, "", 5))
            .iter()
            .map(|entry| entry.catalog_index)
            .collect::<Vec<_>>();
        for columns in [1, 2, 3, 4, 5, 8, 12] {
            let groups = pool_groups(&items, "", columns);
            for group in groups.iter() {
                for row in group.rows.iter() {
                    assert!(row.entries.row_count() <= columns);
                }
            }
            assert_eq!(
                visible_pool_entries(groups)
                    .iter()
                    .map(|entry| entry.catalog_index)
                    .collect::<Vec<_>>(),
                full
            );
        }
    }

    #[test]
    #[ignore = "creates isolated Slint windows and a temporary settings file"]
    fn assistant_editor_saves_through_shared_pipeline_set() {
        let mut settings = AppSettings::default();
        let connection = crate::models::ModelConnection {
            id: "local".into(),
            provider: "ollama".into(),
            endpoint: "http://127.0.0.1:11434".into(),
            api_version: String::new(),
            custom_header_name: String::new(),
            custom_header_value: String::new(),
            api_key: String::new(),
            allow_lan_http: false,
        };
        let mut fast = crate::models::ConfiguredModel::new(
            connection.id.clone(),
            "Fast".into(),
            "fast".into(),
        );
        fast.verified = true;
        let mut strong = crate::models::ConfiguredModel::new(
            connection.id.clone(),
            "Strong".into(),
            "strong".into(),
        );
        strong.verified = true;
        settings.model_registry.connections.push(connection);
        settings
            .model_registry
            .models
            .extend([fast.clone(), strong.clone()]);
        settings.pipelines = Some(migrate(&settings));
        let dictation_id = settings.pipelines.as_ref().unwrap().active_id.clone();
        let speech = settings.pipelines.as_ref().unwrap().active().unwrap().steps[0].clone();
        let shared = Arc::new(Mutex::new(settings));
        let path = std::env::temp_dir().join(format!("echo-assistant-pipeline-{}.json", new_id()));
        let ui = AppWindow::new().unwrap();
        let assistant_window = AssistantPipelineWindow::new().unwrap();
        let holder = Rc::new(RefCell::new(None));
        open_editor(
            Pipeline {
                id: "assistant-test".into(),
                name: "Test assistant".into(),
                output: PipelineOutput::Assistant,
                steps: vec![speech],
                assistant: Some(AssistantConfig::default()),
                legacy_formatting: None,
                graph: None,
            },
            shared.clone(),
            ui.as_weak(),
            holder.clone(),
            path.clone(),
            Some(assistant_window.as_weak()),
        );
        let editor = holder.borrow().as_ref().unwrap().clone_strong();
        let catalog = pool(&shared.lock().unwrap());
        let fast_index = catalog.iter().position(|step| matches!(&step.kind, StepKind::Model { model_id, .. } if model_id == &fast.id)).unwrap();
        let strong_index = catalog.iter().position(|step| matches!(&step.kind, StepKind::Model { model_id, .. } if model_id == &strong.id)).unwrap();
        editor.invoke_assign_assistant_model(fast_index as i32, 0);
        editor.invoke_assign_assistant_model(strong_index as i32, 1);
        editor.invoke_configure_assistant();
        assistant_window.set_instructions("Answer briefly".into());
        assistant_window.invoke_save();
        editor.invoke_save();
        let loaded = crate::settings::load_settings_from_path(&path);
        let set = loaded.pipelines.unwrap();
        assert_eq!(set.active_id, dictation_id);
        let assistant = set.active_assistant().unwrap();
        assert_eq!(assistant.output, PipelineOutput::Assistant);
        assert_eq!(
            assistant.assistant.as_ref().unwrap().router_model_id,
            fast.id
        );
        assert_eq!(
            assistant.assistant.as_ref().unwrap().answer_model_id,
            strong.id
        );
        assert_eq!(
            assistant.assistant.as_ref().unwrap().instructions,
            "Answer briefly"
        );
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    #[ignore = "isolated Slint decision-router editor and temporary settings"]
    fn decision_router_editor_assigns_text_branches_and_rejects_linear_use() {
        let mut settings = AppSettings::default();
        for (id, provider) in [("laya", "ollaya"), ("fast", "ollama"), ("strong", "ollama")] {
            settings
                .model_registry
                .connections
                .push(crate::models::ModelConnection {
                    id: id.into(),
                    provider: provider.into(),
                    endpoint: String::new(),
                    api_version: String::new(),
                    custom_header_name: String::new(),
                    custom_header_value: String::new(),
                    api_key: String::new(),
                    allow_lan_http: false,
                });
            let mut model = crate::models::ConfiguredModel::new(id.into(), id.into(), id.into());
            model.id = id.into();
            model.verified = true;
            settings.model_registry.models.push(model);
        }
        settings.pipelines = Some(migrate(&settings));
        let mut pipeline = settings
            .pipelines
            .as_ref()
            .unwrap()
            .active()
            .unwrap()
            .clone();
        pipeline.id = "decision-assistant".into();
        pipeline.output = PipelineOutput::Assistant;
        pipeline.steps.truncate(1);
        pipeline.legacy_formatting = None;
        pipeline.assistant = Some(AssistantConfig::default());
        let shared = Arc::new(Mutex::new(settings));
        let path = std::env::temp_dir().join(format!("echo-laya-ui-{}.json", new_id()));
        let ui = AppWindow::new().unwrap();
        let config = AssistantPipelineWindow::new().unwrap();
        let holder = Rc::new(RefCell::new(None));
        open_editor(
            pipeline,
            shared.clone(),
            ui.as_weak(),
            holder.clone(),
            path.clone(),
            Some(config.as_weak()),
        );
        let editor = holder.borrow().as_ref().unwrap().clone_strong();
        let catalog = pool(&shared.lock().unwrap());
        let index = |id: &str| {
            catalog
                .iter()
                .position(|s| matches!(&s.kind,StepKind::Model{model_id,..} if model_id==id))
                .unwrap() as i32
        };
        editor.invoke_assign_assistant_model(index("fast"), 0);
        editor.invoke_assign_assistant_model(index("strong"), 1);
        editor.invoke_assign_assistant_model(index("laya"), 0);
        assert!(editor.get_decision_router());
        assert_eq!(editor.get_fast_answer_name(), "fast");
        editor.invoke_assign_assistant_model(index("laya"), 1);
        assert!(!editor.get_error().is_empty());
        assert_eq!(editor.get_answer_name(), "strong");
        let count = editor.get_cards().row_count();
        editor.invoke_add(index("laya"), 1);
        assert_eq!(editor.get_cards().row_count(), count);
        editor.invoke_assign_assistant_model(index("strong"), 2);
        editor.invoke_configure_assistant();
        assert!(config.get_decision_router());
        config.invoke_save();
        editor.invoke_save();
        let loaded = crate::settings::load_settings_from_path(&path);
        let saved = loaded
            .pipelines
            .as_ref()
            .unwrap()
            .active_assistant()
            .unwrap();
        assert_eq!(saved.assistant.as_ref().unwrap().router_model_id, "laya");
        assert_eq!(
            saved.assistant.as_ref().unwrap().fast_answer_model_id,
            "strong"
        );
        assert_eq!(saved.assistant.as_ref().unwrap().answer_model_id, "strong");
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn component_operations_enforce_speech_first_and_independent_instances() {
        let s = AppSettings::default();
        let mut p = migrate(&s).active().unwrap().clone();
        let original = p.clone();
        assert!(move_step(&mut p, 0, 2).is_err());
        assert!(move_step(&mut p, 1, 0).is_err());
        assert_eq!(p, original);
        let catalog = pool(&s);
        let gemini = catalog.last().unwrap().clone();
        assert!(add_step(&mut p, gemini.clone(), 0).is_err());
        let end = p.steps.len();
        add_step(&mut p, gemini.clone(), end).unwrap();
        add_step(&mut p, gemini, end + 1).unwrap();
        assert_ne!(p.steps[end].id, p.steps[end + 1].id);
        assert!(p.legacy_formatting.is_none());
        let speech = catalog[4].clone();
        let len = p.steps.len();
        add_step(&mut p, speech, len).unwrap();
        assert_eq!(p.steps.len(), len);
        assert!(p.steps[0].label().contains("Ultra"));
        move_step(&mut p, end + 1, 1).unwrap();
        p.validate().unwrap();
        p.steps.remove(0);
        let remaining = p.steps.len();
        add_step(&mut p, catalog[0].clone(), 0).unwrap();
        assert_eq!(p.steps.len(), remaining + 1);
        p.validate().unwrap();
    }
    #[test]
    fn persistence_failure_does_not_change_active_pipeline() {
        let s = AppSettings::default();
        let shared = Arc::new(Mutex::new(AppSettings {
            pipelines: Some(migrate(&s)),
            ..s
        }));
        let before = shared.lock().unwrap().pipelines.clone();
        let path = std::env::temp_dir().join(format!("echo-pipeline-block-{}", new_id()));
        std::fs::write(&path, b"block").unwrap();
        let result = commit(&shared, &path.join("settings.json"), |set| {
            set.pipelines[0].name = "Changed".into();
            Ok(())
        });
        assert!(result.is_err());
        assert_eq!(before, shared.lock().unwrap().pipelines);
        std::fs::remove_file(path).unwrap();
    }
    fn snapshot(window: &slint::Window, name: &str) {
        let pixels = window.take_snapshot().unwrap();
        assert!(pixels.as_slice().iter().any(|p| p.r > 100));
        let bytes = pixels
            .as_slice()
            .iter()
            .flat_map(|p| [p.r, p.g, p.b])
            .collect();
        let image = image::RgbImage::from_raw(pixels.width(), pixels.height(), bytes).unwrap();
        let path = std::env::temp_dir().join(format!("echo-pipelines-{name}.png"));
        image.save(path).unwrap();
    }
    #[test]
    #[ignore = "isolated windows and temporary settings; no audio/network/user settings writes"]
    fn pipeline_settings_and_preview_ui_smoke() {
        let mut s = AppSettings {
            gemini_api_key: "test-key".into(),
            ..Default::default()
        };
        let mut registry = crate::models::ModelRegistry::default();
        registry.import_gemini(&s);
        s.model_registry = registry;
        let shared = Arc::new(Mutex::new(AppSettings {
            pipelines: Some(migrate(&s)),
            ..s
        }));
        let path = std::env::temp_dir().join(format!("echo-pipeline-ui-{}.json", new_id()));
        let ui = AppWindow::new().unwrap();
        crate::populate_settings_editor(&ui, &shared.lock().unwrap());
        install_at(&ui, shared.clone(), path.clone());
        ui.set_active_tab(3);
        ui.set_settings_tab(2);
        ui.window().set_size(slint::PhysicalSize::new(1000, 800));
        ui.show().unwrap();
        let editor_holder = Rc::new(RefCell::new(None));
        let p = shared
            .lock()
            .unwrap()
            .pipelines
            .as_ref()
            .unwrap()
            .active()
            .unwrap()
            .clone();
        open_editor(
            p,
            shared.clone(),
            ui.as_weak(),
            editor_holder.clone(),
            path.clone(),
            None,
        );
        let editor = editor_holder.borrow().as_ref().unwrap().clone_strong();
        let overlay = crate::TranscriptOverlayWindow::new().unwrap();
        overlay.set_pipeline_name("Pipeline fixture".into());
        overlay.set_microphone_name("Fixture microphone".into());
        overlay.set_window_height(230);
        overlay.set_sentence_text("Your words appear here.".into());
        overlay.show().unwrap();
        let weak = ui.as_weak();
        let ed = editor.as_weak();
        let ov = overlay.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(600), move || {
            let ui = weak.upgrade().unwrap();
            let editor = ed.upgrade().unwrap();
            let overlay = ov.upgrade().unwrap();
            let create_requested = Rc::new(std::cell::Cell::new(false));
            let requested = create_requested.clone();
            ui.on_create_model_component(move || requested.set(true));
            editor.invoke_create_component();
            assert!(create_requested.get());
            let mut model = crate::models::ConfiguredModel::new(
                shared.lock().unwrap().model_registry.connections[0]
                    .id
                    .clone(),
                "New pool model".into(),
                "new-pool-model".into(),
            );
            model.verified = true;
            shared.lock().unwrap().model_registry.models.push(model);
            editor.set_pool_search("New pool model".into());
            editor.invoke_refresh_pool();
            let new_entry = visible_pool_entries(editor.get_pool_groups());
            assert_eq!(new_entry.len(), 1);
            assert_eq!(new_entry[0].card.title, "New pool model");
            let count = editor.get_cards().row_count();
            editor.invoke_add(new_entry[0].catalog_index, count as i32);
            assert_eq!(
                editor.get_cards().row_data(count).unwrap().title,
                "New pool model"
            );
            editor.invoke_remove_step(count as i32);
            editor.set_dirty(false);
            editor.set_pool_search("".into());
            editor.invoke_filter_pool("".into(), editor.get_pool_columns());
            snapshot(ui.window(), "list");
            snapshot(editor.window(), "editor");
            let original_cards = editor.get_cards();
            let original_size = editor.window().size();
            let scale = editor.window().scale_factor();
            editor.window().set_size(slint::PhysicalSize::new(
                (700.0 * scale).round() as u32,
                (600.0 * scale).round() as u32,
            ));
            editor
                .window()
                .dispatch_event(slint::platform::WindowEvent::Resized {
                    size: slint::LogicalSize::new(700.0, 600.0),
                });
            let format = visible_pool_entries(editor.get_pool_groups())
                .into_iter()
                .find(|entry| entry.card.title == "Local formatting")
                .unwrap()
                .catalog_index;
            let added = 30usize.saturating_sub(original_cards.row_count());
            for _ in 0..added {
                editor.invoke_add(format, editor.get_cards().row_count() as i32);
            }
            snapshot(editor.window(), "compact-editor");
            for _ in 0..added {
                editor.invoke_undo();
            }
            assert_eq!(editor.get_cards().row_count(), original_cards.row_count());
            editor.window().set_size(original_size);
            editor
                .window()
                .dispatch_event(slint::platform::WindowEvent::Resized {
                    size: slint::LogicalSize::new(
                        original_size.width as f32 / scale,
                        original_size.height as f32 / scale,
                    ),
                });
            // Button actions reorder, remove immediately, and restore via Undo.
            editor.invoke_stage_action(0, 1, 2);
            assert!(editor
                .get_cards()
                .row_data(1)
                .unwrap()
                .title
                .starts_with("Punctuation"));
            editor.invoke_stage_action(0, 2, 1);
            editor.invoke_stage_action(0, 1, 3);
            assert_eq!(editor.get_cards().row_count(), 2);
            assert!(editor.get_undo_available());
            editor.invoke_undo();
            assert_eq!(editor.get_cards().row_count(), 3);
            editor.invoke_stage_action(0, 1, 3);
            snapshot(editor.window(), "button-actions");
            let formatting = editor
                .get_pool_groups()
                .iter()
                .flat_map(|group| group.rows.iter().collect::<Vec<_>>())
                .flat_map(|row| row.entries.iter().collect::<Vec<_>>())
                .find(|entry| entry.card.title == "Local formatting")
                .map(|entry| entry.catalog_index)
                .unwrap();
            editor.invoke_add(formatting, 1);
            assert_eq!(editor.get_cards().row_count(), 3);
            for tab in [0, 1, 2, 4] {
                ui.set_settings_tab(tab);
                snapshot(ui.window(), &format!("settings-{tab}"));
            }
            ui.set_settings_tab(2);
            let before = shared.lock().unwrap().pipelines.clone();
            editor.invoke_filter_pool("gemini".into(), 2);
            let filtered = visible_pool_entries(editor.get_pool_groups());
            let gemini = filtered
                .iter()
                .find(|entry| entry.card.title.starts_with("Gemini"))
                .unwrap()
                .catalog_index;
            editor.invoke_add(gemini, 3);
            editor.invoke_add(gemini, 4);
            editor.invoke_filter_pool("".into(), editor.get_pool_columns());
            assert_eq!(editor.get_cards().row_count(), 5);
            editor.invoke_move_step(4, 1);
            assert!(editor
                .get_cards()
                .row_data(1)
                .unwrap()
                .title
                .starts_with("Gemini"));
            editor.invoke_move_step(0, 3);
            assert!(!editor.get_error().is_empty());
            editor.set_pipeline_name("Renamed fixture".into());
            editor.invoke_save();
            assert!(editor_holder.borrow().is_none());
            assert_ne!(before, shared.lock().unwrap().pipelines);
            assert_eq!(
                crate::settings::load_settings_from_path(&path).pipelines,
                shared.lock().unwrap().pipelines
            );
            ui.invoke_duplicate_pipeline(0);
            ui.invoke_use_pipeline(1);
            assert!(ui.get_pipeline_rows().row_data(1).unwrap().active);
            ui.invoke_delete_pipeline(0);
            assert_eq!(ui.get_pipeline_rows().row_count(), 1);
            let p = shared
                .lock()
                .unwrap()
                .pipelines
                .as_ref()
                .unwrap()
                .active()
                .unwrap()
                .clone();
            open_editor(
                p.clone(),
                shared.clone(),
                ui.as_weak(),
                editor_holder.clone(),
                path.clone(),
                None,
            );
            let editor = editor_holder.borrow().as_ref().unwrap().clone_strong();
            editor.invoke_remove_step(1);
            editor.invoke_cancel();
            assert_eq!(
                shared
                    .lock()
                    .unwrap()
                    .pipelines
                    .as_ref()
                    .unwrap()
                    .active()
                    .unwrap()
                    .steps
                    .len(),
                5
            );
            let previous_id = ui.get_pipeline_rows().row_data(0).unwrap().id;
            open_editor(
                Pipeline {
                    id: new_id(),
                    name: "Appended fixture".into(),
                    output: PipelineOutput::Transcription,
                    steps: vec![p.steps[0].clone()],
                    assistant: None,
                    legacy_formatting: None,
                    graph: None,
                },
                shared.clone(),
                ui.as_weak(),
                editor_holder.clone(),
                path.clone(),
                None,
            );
            assert_eq!(ui.get_pipeline_rows().row_count(), 1);
            let new_editor = editor_holder.borrow().as_ref().unwrap().clone_strong();
            new_editor.invoke_save();
            assert!(editor_holder.borrow().is_none());
            assert_eq!(ui.get_pipeline_rows().row_count(), 2);
            assert_eq!(ui.get_pipeline_rows().row_data(0).unwrap().id, previous_id);
            assert_eq!(
                ui.get_pipeline_rows().row_data(1).unwrap().name,
                "Appended fixture"
            );
            snapshot(ui.window(), "new-pipeline-position");
            let draft = Rc::new(RefCell::new(p.clone()));
            let dialog_holder = Rc::new(RefCell::new(None));
            // Cover every component dialog, including local CPU options.
            let mut fixtures = pool(&shared.lock().unwrap());
            fixtures.push(p.steps[0].clone());
            for step in fixtures {
                draft.borrow_mut().steps[0] = p.steps[0].clone();
                let index = if step.is_transcription() {
                    draft.borrow_mut().steps[0] = step;
                    0
                } else {
                    draft.borrow_mut().steps.push(step);
                    draft.borrow().steps.len() - 1
                };
                open_step(
                    index,
                    draft.clone(),
                    editor.as_weak(),
                    dialog_holder.clone(),
                    shared.clone(),
                );
                let dialog = dialog_holder.borrow().as_ref().unwrap().clone_strong();
                dialog.window().set_size(slint::PhysicalSize::new(620, 720));
                dialog
                    .window()
                    .dispatch_event(slint::platform::WindowEvent::Resized {
                        size: slint::LogicalSize::new(620.0, 720.0),
                    });
                snapshot(dialog.window(), &format!("component-{}", dialog.get_kind()));
                dialog.invoke_apply();
                assert!(dialog_holder.borrow().is_none());
            }
            for count in [3, 10, 30] {
                let steps = (0..count)
                    .map(|i| p.steps[i % p.steps.len()].clone())
                    .collect::<Vec<_>>();
                overlay.set_pipeline_cards(cards(&steps));
                overlay.set_pipeline_step((count / 2) as i32);
                snapshot(overlay.window(), &format!("preview-{count}"));
            }
            overlay.set_pipeline_failed(true);
            overlay.set_is_error(true);
            overlay.set_failure_message("Gemini unavailable · nothing was pasted".into());
            snapshot(overlay.window(), "failure");
            let _ = overlay.hide();
            let _ = ui.hide();
            let _ = std::fs::remove_file(path);
            slint::quit_event_loop().unwrap();
        });
        slint::run_event_loop().unwrap();
    }
}
