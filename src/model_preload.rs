//! Opt-in startup warming for models referenced by pipeline components.
use crate::{
    models::{validate_connection, ModelConnection},
    pipelines::StepKind,
    settings::AppSettings,
    transcription::{self, LocalSherpaConfig},
};
use std::{collections::HashSet, time::Duration};

#[derive(Default)]
pub struct PreloadPlan {
    pub speech: Vec<LocalSherpaConfig>,
    pub punctuation: bool,
    pub voice: bool,
    pub ollama: Vec<(ModelConnection, String)>,
    pub decisions: Vec<(ModelConnection, String)>,
}

impl PreloadPlan {
    pub fn from_settings(settings: &AppSettings) -> Self {
        let mut plan = Self {
            voice: settings.tts_preload && crate::assistant_voice::installed(),
            ..Self::default()
        };
        let mut speech_seen = HashSet::new();
        let mut ollama_seen = HashSet::new();
        let Some(set) = settings.pipelines.as_ref() else {
            return plan;
        };
        let mut add_model = |model_id: &str| {
            let Ok((model, connection)) = settings.model_registry.find(model_id) else {
                return;
            };
            if !model.verified
                || !["ollama", "ollaya"].contains(&connection.provider.as_str())
                || validate_connection(connection).is_err()
            {
                return;
            }
            if ollama_seen.insert((connection.id.clone(), model.model.clone())) {
                if connection.provider == crate::system_one::PROVIDER {
                    plan.decisions
                        .push((connection.clone(), model.model.clone()));
                } else {
                    plan.ollama.push((connection.clone(), model.model.clone()));
                }
            }
        };
        for pipeline in &set.pipelines {
            if let Some(g) = &pipeline.graph {
                for node in &g.nodes {
                    if let Some(step) = node.step().filter(|s| s.preload_on_start()) {
                        match &step.kind {
                            StepKind::Transcription {
                                provider, local, ..
                            } if provider == "local_sherpa_onnx" => {
                                let local = local.clone().normalized();
                                if transcription::local_models_available(&local)
                                    && speech_seen.insert((
                                        local.model,
                                        if local.model.uses_gpu() {
                                            0
                                        } else {
                                            local.num_threads
                                        },
                                    ))
                                {
                                    plan.speech.push(local);
                                }
                            }
                            StepKind::Model { model_id, .. } => add_model(model_id),
                            StepKind::Punctuation(config) if config.punctuation => {
                                plan.punctuation = true
                            }
                            _ => {}
                        }
                    }
                    match &node.kind {
                        crate::graph::Kind::Router {
                            model_id,
                            preload: true,
                            ..
                        } => add_model(model_id),
                        crate::graph::Kind::Speech { preload: true, .. }
                            if crate::assistant_voice::installed() =>
                        {
                            plan.voice = true
                        }
                        _ => {}
                    }
                }
                continue;
            }

            for step in pipeline.steps.iter().chain(
                pipeline
                    .assistant
                    .iter()
                    .flat_map(crate::pipelines::AssistantConfig::processing_steps),
            ) {
                if !step.preload_on_start() {
                    continue;
                }
                match &step.kind {
                    StepKind::Transcription {
                        provider, local, ..
                    } if provider == "local_sherpa_onnx" => {
                        let local = local.clone().normalized();
                        if transcription::local_models_available(&local)
                            && speech_seen.insert((
                                local.model,
                                if local.model.uses_gpu() {
                                    0
                                } else {
                                    local.num_threads
                                },
                            ))
                        {
                            plan.speech.push(local);
                        }
                    }
                    StepKind::Model { model_id, .. } => add_model(model_id),
                    StepKind::Punctuation(config) if config.punctuation => plan.punctuation = true,
                    _ => {}
                }
            }
            if let Some(config) = &pipeline.assistant {
                if config.preload_router_on_start {
                    add_model(&config.router_model_id);
                }
                if config.preload_answer_on_start {
                    add_model(&config.answer_model_id);
                }
                if config.preload_voice_on_start
                    && config
                        .effective_output_steps()
                        .iter()
                        .any(|s| matches!(s, crate::pipelines::OutputStep::Speech))
                    && crate::assistant_voice::installed()
                {
                    plan.voice = true;
                }
            }
        }
        plan
    }

    pub fn start(self) {
        for config in &self.speech {
            transcription::preload_local_engine(config);
        }
        if self.punctuation {
            std::thread::spawn(|| {
                if let Err(error) = crate::post_processing::preload() {
                    crate::echo_warn!("model_preload", "Punctuation model could not load: {error}");
                }
            });
        }
        if self.voice {
            std::thread::spawn(|| {
                if let Err(error) = crate::assistant_voice::preload() {
                    crate::echo_warn!("model_preload", "Voice model could not load: {error}");
                }
            });
        }
        if !self.ollama.is_empty() || !self.decisions.is_empty() {
            std::thread::spawn(move || {
                let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                else {
                    return;
                };
                runtime.block_on(async move {
                    for (connection, model) in self.decisions {
                        if let Err(error) = crate::system_one::preload(&connection, &model).await {
                            crate::echo_warn!(
                                "model_preload",
                                "Decision model could not load at startup: {error}"
                            );
                        }
                    }
                    for (connection, model) in self.ollama {
                        if let Err(error) = preload_ollama(&connection, &model).await {
                            crate::echo_warn!(
                                "model_preload",
                                "Ollama model could not load at startup: {error}"
                            );
                        }
                    }
                });
            });
        }
    }
}

fn ollama_generate_url(connection: &ModelConnection) -> Result<url::Url, String> {
    validate_connection(connection)?;
    let base = if connection.endpoint.trim().is_empty() {
        "http://127.0.0.1:11434"
    } else {
        connection.endpoint.trim()
    };
    let base = base.trim_end_matches('/');
    let path = if base.ends_with("/api") {
        "/generate"
    } else {
        "/api/generate"
    };
    url::Url::parse(&format!("{base}{path}")).map_err(|_| "Invalid Ollama endpoint".into())
}

async fn preload_ollama(connection: &ModelConnection, model: &str) -> Result<(), String> {
    let url = ollama_generate_url(connection)?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|_| "Could not initialize HTTP client")?;
    let mut request = client.post(url).json(&serde_json::json!({
        "model": model,
        "prompt": "",
        "stream": false,
        "keep_alive": -1,
    }));
    if !connection.api_key.is_empty() {
        request = request.bearer_auth(&connection.api_key);
    }
    if !connection.custom_header_name.is_empty() {
        request = request.header(
            &connection.custom_header_name,
            &connection.custom_header_value,
        );
    }
    let response = request
        .send()
        .await
        .map_err(|_| "Ollama server unavailable")?;
    if !response.status().is_success() {
        return Err(format!("Ollama returned HTTP {}", response.status()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipelines::{Pipeline, PipelineOutput, PipelineSet, Step};

    fn ollama_connection(endpoint: String) -> ModelConnection {
        ModelConnection {
            id: "ollama".into(),
            provider: "ollama".into(),
            endpoint,
            api_version: String::new(),
            custom_header_name: String::new(),
            custom_header_value: String::new(),
            api_key: String::new(),
            allow_lan_http: false,
        }
    }

    #[test]
    fn legacy_active_speech_keeps_preload_without_enabling_inactive_speech() {
        let mut local = Step::new(StepKind::Transcription {
            provider: "local_sherpa_onnx".into(),
            model: "parakeet".into(),
            language: "en".into(),
            no_verbatim: false,
            local: LocalSherpaConfig::default(),
        });
        assert!(!local.preload_on_start());
        local.preload_on_start = None;
        let mut set = PipelineSet {
            version: 3,
            active_id: "active".into(),
            active_assistant_id: String::new(),
            pipelines: ["active", "inactive"]
                .into_iter()
                .map(|id| Pipeline {
                    id: id.into(),
                    name: id.into(),
                    output: PipelineOutput::Transcription,
                    steps: vec![local.clone()],
                    assistant: None,
                    legacy_formatting: None,
                    graph: None,
                })
                .collect(),
        };
        crate::pipelines::upgrade_preload_defaults(&mut set);
        assert!(set.pipelines[0].steps[0].preload_on_start());
        assert!(!set.pipelines[1].steps[0].preload_on_start());
        let mut disabled = local.clone();
        disabled.preload_on_start = Some(false);
        assert!(!disabled.preload_on_start());
        let decoded: Step =
            serde_json::from_str(&serde_json::to_string(&disabled).unwrap()).unwrap();
        assert!(!decoded.preload_on_start());
    }

    #[test]
    fn ignores_unselected_and_cloud_components() {
        let mut settings = AppSettings::default();
        let mut speech = Step::new(StepKind::Transcription {
            provider: "elevenlabs_realtime".into(),
            model: "cloud".into(),
            language: "en".into(),
            no_verbatim: false,
            local: LocalSherpaConfig::default(),
        });
        speech.preload_on_start = Some(true);
        settings.pipelines = Some(PipelineSet {
            version: 3,
            active_id: "p".into(),
            active_assistant_id: String::new(),
            pipelines: vec![Pipeline {
                id: "p".into(),
                name: "Cloud".into(),
                output: PipelineOutput::Transcription,
                steps: vec![speech],
                assistant: None,
                legacy_formatting: None,
                graph: None,
            }],
        });
        let plan = PreloadPlan::from_settings(&settings);
        assert!(plan.speech.is_empty() && plan.ollama.is_empty() && !plan.punctuation);
    }

    #[tokio::test]
    async fn ollama_preload_sends_empty_request_with_persistent_keep_alive() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 1024];
            loop {
                let size = stream.read(&mut buffer).await.unwrap();
                assert!(size > 0);
                request.extend_from_slice(&buffer[..size]);
                if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&request[..end]);
                    let length = header
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .and_then(|value| value.trim().parse::<usize>().ok())
                        })
                        .unwrap();
                    if request.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}")
                .await
                .unwrap();
            request
        });
        preload_ollama(&ollama_connection(endpoint), "local-model")
            .await
            .unwrap();
        let request = server.await.unwrap();
        let text = String::from_utf8(request).unwrap();
        assert!(text.starts_with("POST /api/generate HTTP/1.1"));
        let body: serde_json::Value =
            serde_json::from_str(text.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(body["model"], "local-model");
        assert_eq!(body["prompt"], "");
        assert_eq!(body["keep_alive"], -1);
    }
}
