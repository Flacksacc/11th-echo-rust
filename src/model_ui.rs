use crate::{
    models::{ConfiguredModel, ModelConnection, DEFAULT_INSTRUCTIONS},
    settings::AppSettings,
    AppWindow, ModelManagerWindow, ModelRow,
};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{Arc, Mutex},
};

pub(crate) const PROVIDERS: &[&str] = &[
    "gemini",
    "openai",
    "anthropic",
    "openrouter",
    "deepseek",
    "groq",
    "mistral",
    "together",
    "xai",
    "cohere",
    "perplexity",
    "venice",
    "zai",
    "moonshot",
    "hyperbolic",
    "mira",
    "minimax",
    "xiaomimimo",
    "huggingface",
    "azure",
    "ollama",
    "ollaya",
    "openai_compatible",
];

fn refresh(window: &ModelManagerWindow, settings: &AppSettings) {
    window.set_rows(ModelRc::new(VecModel::from(
        settings
            .model_registry
            .models
            .iter()
            .map(|m| {
                let provider = settings
                    .model_registry
                    .connections
                    .iter()
                    .find(|c| c.id == m.connection_id)
                    .map(|c| c.provider.as_str())
                    .unwrap_or("Missing connection");
                ModelRow {
                    name: m.name.clone().into(),
                    detail: format!(
                        "{} · {} · {}",
                        provider,
                        m.model,
                        if m.imported {
                            "Imported"
                        } else if m.verified {
                            "Tested"
                        } else {
                            "Needs setup"
                        }
                    )
                    .into(),
                }
            })
            .collect::<Vec<_>>(),
    )))
}

fn clear(window: &ModelManagerWindow) {
    window.set_discovered_models(ModelRc::new(VecModel::from(
        Vec::<slint::SharedString>::new(),
    )));
    window.set_selected_index(-1);
    window.set_name("".into());
    window.set_provider("gemini".into());
    window.set_endpoint("".into());
    window.set_api_version("2024-10-21".into());
    window.set_api_key("".into());
    window.set_custom_header_name("".into());
    window.set_custom_header_value("".into());
    window.set_allow_lan_http(false);
    window.set_update_shared_connection(true);
    window.set_model_id("".into());
    window.set_instructions(DEFAULT_INSTRUCTIONS.into());
    window.set_max_tokens("2048".into());
    window.set_temperature("".into());
    window.set_additional_params("".into());
    window.set_timeout_seconds("60".into());
    window.set_status("".into());
}

pub fn install(ui: &AppWindow, shared: Arc<Mutex<AppSettings>>) {
    install_inner(ui, shared);
}

fn install_inner(
    ui: &AppWindow,
    shared: Arc<Mutex<AppSettings>>,
) -> Rc<RefCell<Option<ModelManagerWindow>>> {
    let shared_for_creator = shared.clone();
    let holder = Rc::new(RefCell::new(None::<ModelManagerWindow>));
    let installed_holder = holder.clone();
    let main = ui.as_weak();
    let open = Rc::new(move || {
        if let Some(window) = holder.borrow().as_ref() {
            refresh(window, &shared.lock().unwrap());
            let _ = window.show();
            return;
        }
        let Ok(window) = ModelManagerWindow::new() else {
            return;
        };
        window.set_providers(ModelRc::new(VecModel::from(
            PROVIDERS.iter().map(|s| (*s).into()).collect::<Vec<_>>(),
        )));
        clear(&window);
        refresh(&window, &shared.lock().unwrap());
        {
            let weak = window.as_weak();
            let shared = shared.clone();
            window.on_select_model(move |index| {
                let Some(w) = weak.upgrade() else {
                    return;
                };
                let settings = shared.lock().unwrap();
                let Some(m) = settings.model_registry.models.get(index as usize) else {
                    return;
                };
                let Some(c) = settings
                    .model_registry
                    .connections
                    .iter()
                    .find(|c| c.id == m.connection_id)
                else {
                    return;
                };
                w.set_selected_index(index);
                w.set_name(m.name.clone().into());
                w.set_provider(c.provider.clone().into());
                w.set_endpoint(c.endpoint.clone().into());
                w.set_api_version(c.api_version.clone().into());
                w.set_api_key(c.api_key.clone().into());
                w.set_custom_header_name(c.custom_header_name.clone().into());
                w.set_custom_header_value(c.custom_header_value.clone().into());
                w.set_allow_lan_http(c.allow_lan_http);
                w.set_update_shared_connection(true);
                w.set_model_id(m.model.clone().into());
                w.set_instructions(m.instructions.clone().into());
                w.set_max_tokens(m.max_tokens.to_string().into());
                w.set_temperature(
                    m.temperature
                        .map(|t| t.to_string())
                        .unwrap_or_default()
                        .into(),
                );
                w.set_timeout_seconds(m.timeout_seconds.to_string().into());
                w.set_additional_params(
                    crate::models::additional_params_text(&m.additional_params).into(),
                );
                w.set_status("".into());
                w.set_discovered_models(ModelRc::new(VecModel::from(
                    Vec::<slint::SharedString>::new(),
                )));
            });
        }
        {
            let weak = window.as_weak();
            window.on_refresh_models(move || {
                let Some(w) = weak.upgrade() else { return; };
                let connection = ModelConnection { id: String::new(), provider: w.get_provider().into(),
                    endpoint: w.get_endpoint().trim().into(), api_version: w.get_api_version().trim().into(), api_key: w.get_api_key().into(),
                    custom_header_name: w.get_custom_header_name().trim().into(), custom_header_value: w.get_custom_header_value().into(),
                    allow_lan_http: w.get_allow_lan_http() };
                w.set_busy(true);
                w.set_status("Retrieving model IDs…".into());
                let weak = weak.clone();
                std::thread::spawn(move || {
                    let result = tokio::runtime::Builder::new_current_thread().enable_all().build()
                        .map_err(|_| "Could not start model listing.".to_string())
                        .and_then(|rt| rt.block_on(crate::models::list_models(&connection)));
                    let _ = slint::invoke_from_event_loop(move || {
                        let Some(w) = weak.upgrade() else { return; };
                        w.set_busy(false);
                        match result {
                            Ok(ids) => {
                                let count = ids.len();
                                w.set_discovered_models(ModelRc::new(VecModel::from(ids.into_iter().map(Into::into).collect::<Vec<slint::SharedString>>())));
                                w.set_status(format!("Found {count} model IDs. Select one or enter another ID manually.").into());
                            }
                            Err(e) => w.set_status(e.into()),
                        }
                    });
                });
            });
        }
        {
            let weak = window.as_weak();
            window.on_new_model(move || {
                if let Some(w) = weak.upgrade() {
                    clear(&w);
                }
            });
        }
        {
            let weak = window.as_weak();
            let shared = shared.clone();
            let main = main.clone();
            window.on_test_and_save(move || {
                let Some(w) = weak.upgrade() else { return; };
                let name = w.get_name().trim().to_string();
                let provider = w.get_provider().to_string();
                let model_id = w.get_model_id().trim().to_string();
                let instructions = w.get_instructions().trim().to_string();
                if name.is_empty() || model_id.is_empty() || instructions.is_empty() {
                    w.set_status("Name, model ID, and instructions are required.".into()); return;
                }
                let Ok(max_tokens) = w.get_max_tokens().parse::<u64>() else {
                    w.set_status("Enter a valid output-token limit.".into()); return;
                };
                let Ok(timeout_seconds) = w.get_timeout_seconds().parse::<u64>() else {
                    w.set_status("Enter a timeout from 5 to 600 seconds.".into()); return;
                };
                if max_tokens == 0 || !(5..=600).contains(&timeout_seconds) {
                    w.set_status("Output tokens must be positive; timeout must be 5–600 seconds.".into()); return;
                }
                let temperature = if w.get_temperature().trim().is_empty() { None } else {
                    match w.get_temperature().parse::<f32>() {
                        Ok(t) if (0.0..=2.0).contains(&t) => Some(t),
                        _ => { w.set_status("Temperature must be from 0 to 2.".into()); return; }
                    }
                };
                let additional_params = match crate::models::parse_additional_params(&w.get_additional_params()) {
                    Ok(value) => value,
                    Err(error) => { w.set_status(error.into()); return; }
                };
                let mut candidate = shared.lock().unwrap().clone();
                let selected = w.get_selected_index();
                let old = if selected >= 0 { candidate.model_registry.models.get(selected as usize).cloned() } else { None };
                let mut connection = ModelConnection { id: crate::pipelines::new_id(), provider,
                    endpoint: w.get_endpoint().trim().into(), api_version: w.get_api_version().trim().into(), api_key: w.get_api_key().into(),
                    custom_header_name: w.get_custom_header_name().trim().into(), custom_header_value: w.get_custom_header_value().into(),
                    allow_lan_http: w.get_allow_lan_http() };
                if let Err(e) = crate::models::validate_connection(&connection) { w.set_status(e.into()); return; }
                if connection.api_key.is_empty() && !["ollama", "ollaya", "openai_compatible"].contains(&connection.provider.as_str()) {
                    w.set_status("This provider needs an API key.".into()); return;
                }
                let mut dependent_ids = Vec::new();
                if let Some(old_connection) = old.as_ref().and_then(|m| candidate.model_registry.connections.iter().find(|c| c.id == m.connection_id)).cloned() {
                    if w.get_update_shared_connection() {
                        connection.id = old_connection.id.clone();
                        if connection != old_connection {
                            dependent_ids = candidate.model_registry.models.iter()
                                .filter(|m| m.connection_id == connection.id && Some(&m.id) != old.as_ref().map(|o| &o.id))
                                .map(|m| m.id.clone()).collect();
                        }
                    }
                }
                if let Some(existing) = candidate.model_registry.connections.iter_mut().find(|c| c.id == connection.id) {
                    *existing = connection.clone();
                } else if let Some(existing) = candidate.model_registry.connections.iter().find(|c|
                    c.provider == connection.provider && c.endpoint == connection.endpoint && c.api_version == connection.api_version && c.api_key == connection.api_key
                    && c.custom_header_name == connection.custom_header_name && c.custom_header_value == connection.custom_header_value
                    && c.allow_lan_http == connection.allow_lan_http) {
                    connection.id = existing.id.clone();
                } else { candidate.model_registry.connections.push(connection.clone()); }
                let connection_id = connection.id.clone();
                let mut model = ConfiguredModel::new(connection_id, name, model_id);
                if let Some(old) = &old { model.id = old.id.clone(); }
                model.instructions = instructions;
                model.max_tokens = max_tokens;
                model.temperature = temperature;
                model.additional_params = additional_params;
                model.timeout_seconds = timeout_seconds;
                model.verified = true; // Set only on the isolated test snapshot; commit happens after success.
                if let Some(i) = (selected >= 0).then_some(selected as usize).filter(|i| *i < candidate.model_registry.models.len()) {
                    candidate.model_registry.models[i] = model.clone();
                } else { candidate.model_registry.models.push(model.clone()); }
                w.set_busy(true);
                w.set_status("Testing model with synthetic text…".into());
                let weak = weak.clone();
                let shared = shared.clone();
                let main = main.clone();
                std::thread::spawn(move || {
                    let result = tokio::runtime::Builder::new_current_thread().enable_all().build()
                        .map_err(|_| "Could not start model test.".to_string())
                        .and_then(|rt| rt.block_on(async {
                            for id in std::iter::once(&model.id).chain(dependent_ids.iter()) {
                                let tested = candidate.model_registry.models.iter().find(|m| &m.id == id).unwrap();
                                crate::models::test_model(&candidate, id, "Hello, how are you?").await
                                    .map_err(|e| format!("{}: {e}", tested.name))?;
                            }
                            Ok::<(), String>(())
                        }));
                    let _ = slint::invoke_from_event_loop(move || {
                        let Some(w) = weak.upgrade() else { return; };
                        w.set_busy(false);
                        match result {
                            Err(e) => w.set_status(format!("Test failed: {e}").into()),
                            Ok(_) => {
                                let mut lock = shared.lock().unwrap();
                                let mut next = lock.clone();
                                let new_connection = candidate.model_registry.connections.iter()
                                    .find(|c| c.id == model.connection_id).cloned();
                                if let Some(c) = new_connection {
                                    if let Some(existing) = next.model_registry.connections.iter_mut().find(|existing| existing.id == c.id) { *existing = c; }
                                    else { next.model_registry.connections.push(c); }
                                }
                                for id in &dependent_ids {
                                    if let Some(dependent) = next.model_registry.models.iter_mut().find(|m| &m.id == id) { dependent.verified = true; }
                                }
                                if let Some(index) = next.model_registry.models.iter().position(|m| m.id == model.id) {
                                    next.model_registry.models[index] = model.clone();
                                } else { next.model_registry.models.push(model.clone()); }
                                if let Some(set) = &mut next.pipelines {
                                    for step in set.pipelines.iter_mut().flat_map(|p| &mut p.steps) {
                                        if let crate::pipelines::StepKind::Model { model_id, name, .. } = &mut step.kind {
                                            if model_id == &model.id { *name = model.name.clone(); }
                                        }
                                    }
                                }
                                next.model_registry.connections.retain(|c| next.model_registry.models.iter().any(|m| m.connection_id == c.id));
                                if crate::settings::save_settings(&next) {
                                    *lock = next;
                                    refresh(&w, &lock);
                                    let index = lock.model_registry.models.iter().position(|m| m.id == model.id).unwrap_or(0);
                                    w.set_selected_index(index as i32);
                                    w.set_status("Model tested and saved. It is now available in pipeline editors.".into());
                                    drop(lock);
                                    if let Some(main) = main.upgrade() { main.invoke_model_components_changed(); }
                                } else { w.set_status("Test passed, but settings could not be saved.".into()); }
                            }
                        }
                    });
                });
            });
        }
        {
            let weak = window.as_weak();
            let shared = shared.clone();
            let main = main.clone();
            window.on_delete_model(move || {
                let Some(w) = weak.upgrade() else { return; };
                let index = w.get_selected_index();
                let mut lock = shared.lock().unwrap();
                let mut next = lock.clone();
                let Some(model) = (index >= 0).then(|| next.model_registry.models.get(index as usize)).flatten().cloned() else { return; };
                let users: Vec<String> = next.pipelines.iter().flat_map(|set| &set.pipelines)
                    .filter(|p| p.steps.iter().any(|s| matches!(&s.kind, crate::pipelines::StepKind::Model { model_id, .. } if model_id == &model.id)) || p.assistant.as_ref().is_some_and(|c| [&c.router_model_id, &c.answer_model_id, &c.fast_answer_model_id].contains(&&model.id)))
                    .map(|p| p.name.clone()).collect();
                if !users.is_empty() { w.set_status(format!("Remove this model from these pipelines first: {}", users.join(", ")).into()); return; }
                next.model_registry.models.retain(|m| m.id != model.id);
                next.model_registry.connections.retain(|c| next.model_registry.models.iter().any(|m| m.connection_id == c.id));
                if crate::settings::save_settings(&next) { *lock = next; refresh(&w, &lock); clear(&w); drop(lock); if let Some(main) = main.upgrade() { main.invoke_model_components_changed(); } }
                else { w.set_status("Could not save model deletion.".into()); }
            });
        }
        {
            let weak = window.as_weak();
            let holder = holder.clone();
            window.on_close_window(move || {
                if let Some(w) = weak.upgrade() {
                    let _ = w.hide();
                }
                holder.borrow_mut().take();
            });
        }
        let _ = window.show();
        *holder.borrow_mut() = Some(window);
    });
    let manage = open.clone();
    ui.on_open_model_manager(move || manage());
    crate::model_creator::install(ui, shared_for_creator);
    installed_holder
}

#[cfg(test)]
mod tests {
    use super::*;
    use slint::Model;

    #[test]
    #[ignore = "opens an isolated Slint window; no network or user settings writes"]
    fn model_manager_ui_smoke() {
        let window = ModelManagerWindow::new().unwrap();
        let mut settings = AppSettings {
            gemini_api_key: "test-key".into(),
            ..Default::default()
        };
        let source = settings.clone();
        settings.model_registry.import_gemini(&source);
        clear(&window);
        refresh(&window, &settings);
        assert_eq!(window.get_rows().row_count(), 1);
        window.window().set_size(slint::PhysicalSize::new(840, 670));
        window.show().unwrap();
        let pixels = window.window().take_snapshot().unwrap();
        assert!(pixels.as_slice().iter().any(|pixel| pixel.r > 100));
        let bytes: Vec<u8> = pixels
            .as_slice()
            .iter()
            .flat_map(|pixel| [pixel.r, pixel.g, pixel.b])
            .collect();
        let image = image::RgbImage::from_raw(pixels.width(), pixels.height(), bytes).unwrap();
        image
            .save(std::env::temp_dir().join("echo-model-manager-smoke.png"))
            .unwrap();
        window.hide().unwrap();
    }
}
