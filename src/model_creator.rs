use crate::{
    models::{ConfiguredModel, ModelConnection, DEFAULT_INSTRUCTIONS},
    settings::AppSettings,
    AppWindow, ModelCreatorWindow,
};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{Arc, Mutex},
};

#[derive(Clone, PartialEq)]
struct Draft {
    connection: ModelConnection,
    model: ConfiguredModel,
}
impl Draft {
    fn matches(&self, other: &Self) -> bool {
        // Display name is allowed to change after testing.
        let mut model = other.model.clone();
        model.name = self.model.name.clone();
        self.connection == other.connection && self.model == model
    }
    fn snapshot(&self, settings: &AppSettings) -> AppSettings {
        let mut next = settings.clone();
        next.model_registry
            .connections
            .push(self.connection.clone());
        next.model_registry.models.push(self.model.clone());
        next
    }
}
fn connection(w: &ModelCreatorWindow) -> ModelConnection {
    ModelConnection {
        id: "creator-connection".into(),
        provider: w.get_provider().into(),
        endpoint: w.get_endpoint().trim().into(),
        api_version: w.get_api_version().trim().into(),
        api_key: w.get_api_key().into(),
        custom_header_name: w.get_custom_header_name().trim().into(),
        custom_header_value: w.get_custom_header_value().into(),
        allow_lan_http: w.get_allow_lan_http(),
    }
}
fn draft(w: &ModelCreatorWindow) -> Result<Draft, String> {
    let connection = connection(w);
    crate::models::validate_connection(&connection)?;
    if connection.api_key.is_empty()
        && !["ollama", "ollaya", "openai_compatible"].contains(&connection.provider.as_str())
    {
        return Err("This provider needs an API key.".into());
    }
    let model_id = w.get_model_id().trim().to_string();
    let instructions = w.get_instructions().trim().to_string();
    if model_id.is_empty() || instructions.is_empty() {
        return Err("Model ID and default prompt are required.".into());
    }
    let max_tokens = w
        .get_max_tokens()
        .trim()
        .parse::<u64>()
        .map_err(|_| "Enter a valid output-token limit.")?;
    let timeout_seconds = w
        .get_timeout_seconds()
        .trim()
        .parse::<u64>()
        .map_err(|_| "Enter a timeout from 5 to 600 seconds.")?;
    if max_tokens == 0 || !(5..=600).contains(&timeout_seconds) {
        return Err("Output tokens must be positive; timeout must be 5–600 seconds.".into());
    }
    let temperature = if w.get_temperature().trim().is_empty() {
        None
    } else {
        let value = w
            .get_temperature()
            .trim()
            .parse::<f32>()
            .map_err(|_| "Temperature must be from 0 to 2.")?;
        if !(0.0..=2.0).contains(&value) {
            return Err("Temperature must be from 0 to 2.".into());
        }
        Some(value)
    };
    let mut model =
        ConfiguredModel::new(connection.id.clone(), w.get_name().trim().into(), model_id);
    model.id = "creator-model".into();
    model.instructions = instructions;
    model.max_tokens = max_tokens;
    model.timeout_seconds = timeout_seconds;
    model.temperature = temperature;
    model.additional_params = crate::models::parse_additional_params(&w.get_additional_params())?;
    model.verified = true; // Isolated test only; persistence requires a successful result.
    Ok(Draft { connection, model })
}

pub fn install(ui: &AppWindow, shared: Arc<Mutex<AppSettings>>) {
    install_inner(ui, shared);
}
fn install_inner(
    ui: &AppWindow,
    shared: Arc<Mutex<AppSettings>>,
) -> Rc<RefCell<Option<ModelCreatorWindow>>> {
    install_at(ui, shared, crate::settings::settings_path())
}
fn install_at(
    ui: &AppWindow,
    shared: Arc<Mutex<AppSettings>>,
    path: std::path::PathBuf,
) -> Rc<RefCell<Option<ModelCreatorWindow>>> {
    let holder = Rc::new(RefCell::new(None::<ModelCreatorWindow>));
    let result = holder.clone();
    let main = ui.as_weak();
    ui.on_create_model_component(move || {
        if let Some(w) = holder.borrow().as_ref() {
            let _ = w.show();
            return;
        }
        let Ok(w) = ModelCreatorWindow::new() else {
            return;
        };
        w.set_providers(ModelRc::new(VecModel::from(
            crate::model_ui::PROVIDERS
                .iter()
                .map(|s| (*s).into())
                .collect::<Vec<slint::SharedString>>(),
        )));
        w.set_instructions(DEFAULT_INSTRUCTIONS.into());
        let tested = Rc::new(RefCell::new(None::<Draft>));
        {
            let weak = w.as_weak();
            let tested = tested.clone();
            w.on_configuration_changed(move || {
                tested.borrow_mut().take();
                if let Some(w) = weak.upgrade() {
                    w.set_tested(false);
                    w.set_response("".into());
                    w.set_status("Test this configuration before creating it.".into());
                }
            });
        }
        {
            let weak = w.as_weak();
            w.on_connection_changed(move || {
                if let Some(w) = weak.upgrade() {
                    w.set_discovered_models(ModelRc::default());
                    if w.get_provider() == crate::system_one::PROVIDER
                        && w.get_instructions() == DEFAULT_INSTRUCTIONS
                    {
                        w.set_instructions(crate::system_one::INSTRUCTIONS.into());
                        w.set_sample_input("Hello, how are you?".into());
                    }
                }
            });
        }
        {
            let weak = w.as_weak();
            w.on_refresh_models(move || {
                let Some(w) = weak.upgrade() else {
                    return;
                };
                if w.get_busy() {
                    return;
                }
                let connection = connection(&w);
                w.set_busy(true);
                w.set_status("Retrieving model IDs…".into());
                let weak = weak.clone();
                std::thread::spawn(move || {
                    let response = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|_| "Could not start model listing.".into())
                        .and_then(|rt| rt.block_on(crate::models::list_models(&connection)));
                    let _ = slint::invoke_from_event_loop(move || {
                        let Some(w) = weak.upgrade() else {
                            return;
                        };
                        w.set_busy(false);
                        match response {
                            Ok(ids) => {
                                w.set_discovered_models(ModelRc::new(VecModel::from(
                                    ids.into_iter()
                                        .map(Into::into)
                                        .collect::<Vec<slint::SharedString>>(),
                                )));
                                w.set_status(
                                    "Select a discovered model or enter its ID manually.".into(),
                                );
                            }
                            Err(e) => w.set_status(e.into()),
                        }
                    });
                });
            });
        }
        let pending = Rc::new(RefCell::new(None::<Draft>));
        {
            let weak = w.as_weak();
            let pending = pending.clone();
            let tested = tested.clone();
            w.on_test_completed(move |text, success| {
                let Some(w) = weak.upgrade() else {
                    return;
                };
                w.set_busy(false);
                let Some(previous) = pending.borrow_mut().take() else {
                    return;
                };
                if success
                    && !text.trim().is_empty()
                    && draft(&w).is_ok_and(|current| previous.matches(&current))
                {
                    *tested.borrow_mut() = Some(previous);
                    w.set_response(text);
                    w.set_tested(true);
                    w.set_status(
                        "Test passed. Review the response, then create the component.".into(),
                    );
                } else {
                    w.set_status(if success {
                        "No valid response for the current configuration. Test again.".into()
                    } else {
                        format!("Test failed: {text}").into()
                    });
                }
            });
        }
        {
            let weak = w.as_weak();
            let shared = shared.clone();
            let tested = tested.clone();
            let pending = pending.clone();
            w.on_test_model(move || {
                let Some(w) = weak.upgrade() else {
                    return;
                };
                if w.get_busy() {
                    return;
                }
                tested.borrow_mut().take();
                w.set_tested(false);
                w.set_response("".into());
                let draft = match draft(&w) {
                    Ok(d) => d,
                    Err(e) => {
                        w.set_status(e.into());
                        return;
                    }
                };
                let input = w.get_sample_input().to_string();
                if input.trim().is_empty() {
                    w.set_status("Enter some test input.".into());
                    return;
                }
                let candidate = draft.snapshot(&shared.lock().unwrap());
                w.set_busy(true);
                w.set_status("Testing the default prompt…".into());
                *pending.borrow_mut() = Some(draft);
                let weak = weak.clone();
                let model = candidate.model_registry.models.last().unwrap().clone();
                std::thread::spawn(move || {
                    let response = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|_| "Could not start model test.".into())
                        .and_then(|rt| {
                            rt.block_on(crate::models::test_model(&candidate, &model.id, &input))
                        });
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(w) = weak.upgrade() {
                            match response {
                                Ok(text) => w.invoke_test_completed(text.into(), true),
                                Err(e) => w.invoke_test_completed(e.into(), false),
                            }
                        }
                    });
                });
            });
        }
        {
            let weak = w.as_weak();
            let shared = shared.clone();
            let tested = tested.clone();
            let main = main.clone();
            let holder = holder.clone();
            let path = path.clone();
            w.on_create_model(move || {
                let Some(w) = weak.upgrade() else {
                    return;
                };
                if w.get_busy() {
                    return;
                }
                let current = match draft(&w) {
                    Ok(d) => d,
                    Err(e) => {
                        w.set_status(e.into());
                        return;
                    }
                };
                if current.model.name.is_empty() {
                    w.set_status("Enter a component name.".into());
                    return;
                }
                if !tested
                    .borrow()
                    .as_ref()
                    .is_some_and(|d| d.matches(&current))
                {
                    w.set_tested(false);
                    w.set_status("Test this configuration before creating it.".into());
                    return;
                }
                let mut current = current;
                current.connection.id = crate::pipelines::new_id();
                current.model.connection_id = current.connection.id.clone();
                current.model.id = crate::pipelines::new_id();
                let mut lock = shared.lock().unwrap();
                let next = current.snapshot(&lock);
                if !crate::settings::save_settings_to_path(&path, &next) {
                    w.set_status(
                        "Could not save settings. Your tested draft is still available.".into(),
                    );
                    return;
                }
                *lock = next;
                drop(lock);
                tested.borrow_mut().take();
                w.set_tested(false);
                let _ = w.hide();
                holder.borrow_mut().take();
                if let Some(main) = main.upgrade() {
                    main.invoke_model_components_changed();
                    main.invoke_model_component_created(current.model.name.into());
                }
            });
        }
        {
            let weak = w.as_weak();
            let holder = holder.clone();
            let tested = tested.clone();
            let pending = pending.clone();
            w.on_close_window(move || {
                tested.borrow_mut().take();
                pending.borrow_mut().take();
                if let Some(w) = weak.upgrade() {
                    w.set_tested(false);
                    let _ = w.hide();
                }
                holder.borrow_mut().take();
            });
            let weak = w.as_weak();
            w.window().on_close_requested(move || {
                if let Some(w) = weak.upgrade() {
                    w.invoke_close_window();
                }
                slint::CloseRequestResponse::HideWindow
            });
        }
        let _ = w.show();
        *holder.borrow_mut() = Some(w);
    });
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Draft {
        let connection = ModelConnection {
            id: "creator-connection".into(),
            provider: "openai_compatible".into(),
            endpoint: "http://localhost:1234/v1".into(),
            api_version: String::new(),
            api_key: "private-test-key".into(),
            custom_header_name: String::new(),
            custom_header_value: String::new(),
            allow_lan_http: false,
        };
        let mut model =
            ConfiguredModel::new(connection.id.clone(), "Test".into(), "test-model".into());
        model.id = "creator-model".into();
        model.verified = true;
        Draft { connection, model }
    }
    #[test]
    fn tested_configuration_allows_rename_but_rejects_request_changes() {
        let tested = fixture();
        let mut renamed = tested.clone();
        renamed.model.name = "Renamed".into();
        assert!(tested.matches(&renamed));
        for edit in 0..13 {
            let mut changed = tested.clone();
            match edit {
                0 => changed.connection.provider = "ollama".into(),
                1 => changed.connection.endpoint.push_str("/new"),
                2 => changed.connection.api_key.push('x'),
                3 => changed.connection.custom_header_name = "X-Key".into(),
                4 => changed.connection.custom_header_value = "secret".into(),
                5 => changed.connection.allow_lan_http = true,
                6 => changed.connection.api_version = "new".into(),
                7 => changed.model.model = "another".into(),
                8 => changed.model.instructions = "new prompt".into(),
                9 => changed.model.max_tokens += 1,
                10 => changed.model.temperature = Some(0.5),
                11 => {
                    changed.model.additional_params =
                        Some(serde_json::json!({"reasoning_effort":"high"}))
                }
                _ => changed.model.timeout_seconds += 1,
            }
            assert!(
                !tested.matches(&changed),
                "edit {edit} must require testing"
            );
        }
    }
    fn settle() {
        slint::Timer::single_shot(std::time::Duration::from_millis(30), || {
            slint::quit_event_loop().unwrap()
        });
        slint::run_event_loop().unwrap();
    }
    fn wait(w: &ModelCreatorWindow) {
        let timer = slint::Timer::default();
        let weak = w.as_weak();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        timer.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(10),
            move || {
                if weak.upgrade().is_none_or(|w| !w.get_busy())
                    || std::time::Instant::now() > deadline
                {
                    slint::quit_event_loop().unwrap();
                }
            },
        );
        slint::run_event_loop().unwrap();
        assert!(!w.get_busy(), "request timed out");
    }
    #[test]
    #[ignore = "isolated UI and local mock server; temporary settings only"]
    fn creator_tests_prompt_discovers_models_and_saves_only_on_create() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            for request_index in 0..4 {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    let n = stream.read(&mut chunk).unwrap();
                    request.extend_from_slice(&chunk[..n]);
                    if let Some(split) = request.windows(4).position(|v| v == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&request[..split]);
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                line.to_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|n| n.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if request.len() >= split + 4 + length {
                            break;
                        }
                    }
                }
                let request = String::from_utf8(request).unwrap();
                let body = if request_index == 0 {
                    assert!(request.contains("GET /v1/models"));
                    r#"{"object":"list","data":[{"id":"test-model","object":"model","created":0,"owned_by":"local"}]}"#
                } else {
                    assert!(request.contains("POST /v1/chat/completions"));
                    assert!(request.contains("Rewrite in pirate language."));
                    assert!(request.contains("hello there"));
                    if request_index == 1 || request_index == 3 {
                        r#"{"id":"mock","object":"chat.completion","created":0,"model":"test-model","choices":[{"index":0,"message":{"role":"assistant","content":"Ahoy there"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":2,"total_tokens":3}}"#
                    } else {
                        r#"{"id":"mock","object":"chat.completion","created":0,"model":"test-model","choices":[{"index":0,"message":{"role":"assistant","content":""},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":0,"total_tokens":1}}"#
                    }
                };
                stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).unwrap();
            }
        });
        let ui = AppWindow::new().unwrap();
        ui.show().unwrap();
        let shared = Arc::new(Mutex::new(AppSettings::default()));
        let path =
            std::env::temp_dir().join(format!("echo-creator-{}.json", crate::pipelines::new_id()));
        let holder = install_at(&ui, shared.clone(), path.clone());
        let created = Rc::new(RefCell::new(String::new()));
        let result = created.clone();
        ui.on_model_component_created(move |name| *result.borrow_mut() = name.to_string());
        ui.invoke_create_model_component();
        let w = holder.borrow().as_ref().unwrap().clone_strong();
        w.set_name("My draft".into());
        ui.invoke_create_model_component();
        assert_eq!(w.get_name(), "My draft");
        w.set_provider("openai_compatible".into());
        w.set_endpoint(endpoint.into());
        w.set_model_id("test-model".into());
        w.set_api_key("creator-private-key".into());
        w.set_instructions("Rewrite in pirate language.".into());
        w.set_sample_input("hello there".into());
        w.set_additional_params("{broken}".into());
        w.invoke_test_model();
        assert!(!w.get_busy());
        assert!(w.get_status().contains("valid JSON"));
        w.set_additional_params(r#"{"reasoning_effort":"high"}"#.into());
        settle();
        w.invoke_create_model();
        assert!(shared.lock().unwrap().model_registry.models.is_empty());
        assert!(!path.exists());
        w.invoke_refresh_models();
        wait(&w);
        use slint::Model;
        assert_eq!(w.get_discovered_models().row_count(), 1);
        w.invoke_test_model();
        wait(&w);
        assert!(w.get_tested(), "{}", w.get_status());
        assert_eq!(w.get_response(), "Ahoy there");
        assert!(!path.exists());
        w.set_name("Pirate".into());
        w.set_sample_input("hello there".into());
        settle();
        assert!(w.get_tested());
        // Failed persistence retains the tested draft; retry succeeds without a network call.
        std::fs::create_dir(&path).unwrap();
        w.invoke_create_model();
        assert!(holder.borrow().is_some());
        assert!(w.get_tested());
        assert!(shared.lock().unwrap().model_registry.models.is_empty());
        std::fs::remove_dir(&path).unwrap();
        w.invoke_create_model();
        assert!(holder.borrow().is_none());
        assert_eq!(*created.borrow(), "Pirate");
        assert!(!std::fs::read_to_string(&path)
            .unwrap()
            .contains("creator-private-key"));
        let loaded = crate::settings::load_settings_from_path(&path);
        assert_eq!(
            loaded.model_registry.connections[0].api_key,
            "creator-private-key"
        );
        assert_eq!(loaded.model_registry.models.len(), 1);
        assert_eq!(
            loaded.model_registry.models[0].additional_params,
            Some(serde_json::json!({"reasoning_effort":"high"}))
        );
        assert_eq!(
            loaded.model_registry.models[0].instructions,
            "Rewrite in pirate language."
        );
        assert!(loaded.model_registry.models[0].verified);
        std::fs::remove_file(&path).unwrap();
        ui.invoke_create_model_component();
        let w = holder.borrow().as_ref().unwrap().clone_strong();
        let saved = shared.lock().unwrap().model_registry.connections[0].clone();
        w.set_name("Cancelled draft".into());
        w.set_provider(saved.provider.into());
        w.set_endpoint(saved.endpoint.into());
        w.set_model_id("test-model".into());
        w.set_instructions("Rewrite in pirate language.".into());
        w.set_sample_input("hello there".into());
        settle();
        w.invoke_test_model();
        wait(&w);
        assert!(!w.get_tested());
        assert!(w.get_response().is_empty());
        w.set_tested(true);
        w.set_instructions("Changed".into());
        settle();
        assert!(!w.get_tested());
        w.set_instructions("Rewrite in pirate language.".into());
        settle();
        w.invoke_test_model();
        assert!(w.get_busy());
        w.invoke_close_window();
        wait(&w);
        w.invoke_create_model();
        assert!(!w.get_tested());
        assert!(holder.borrow().is_none());
        assert_eq!(shared.lock().unwrap().model_registry.models.len(), 1);
        server.join().unwrap();
        ui.hide().unwrap();
    }
}
