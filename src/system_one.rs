//! Ollaya's typed System One decision API. Laya selects branches; it never generates replies.
use crate::models::{validate_connection, ModelConnection};
use futures_util::StreamExt;
use serde::Deserialize;
use std::{collections::HashMap, time::Duration};

pub const PROVIDER: &str = "ollaya";
pub const INSTRUCTIONS: &str = "Choose direct for simple conversation or straightforward questions. Choose delegate for complex reasoning, coding, tool use, or uncertain requests.";

#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub choice: String,
    pub confidence: f64,
}

fn endpoint(connection: &ModelConnection, path: &str) -> Result<url::Url, String> {
    validate_connection(connection)?;
    let base = if connection.endpoint.trim().is_empty() {
        "http://127.0.0.1:11435"
    } else {
        connection
            .endpoint
            .trim()
            .trim_end_matches('/')
            .trim_end_matches("/v1")
    };
    url::Url::parse(&format!("{base}{path}")).map_err(|_| "Invalid Ollaya endpoint.".into())
}

async fn request(
    connection: &ModelConnection,
    path: &str,
    body: Option<serde_json::Value>,
    timeout: u64,
) -> Result<serde_json::Value, String> {
    if connection.id == "echo-laya" && connection.endpoint == "http://127.0.0.1:11436" {
        crate::components::ensure_laya().await?;
    }
    let url = endpoint(connection, path)?;
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(timeout.clamp(5, 600)));
    if matches!(
        url.host_str(),
        Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
    ) {
        builder = builder.no_proxy();
    }
    let client = builder
        .build()
        .map_err(|_| "Could not initialize Ollaya client.")?;
    let mut request = if let Some(body) = body {
        client.post(url).json(&body)
    } else {
        client.get(url)
    };
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
        .map_err(|_| "Ollaya decision server unavailable or timed out.")?;
    if !response.status().is_success() {
        return Err(format!(
            "Ollaya returned HTTP {}.",
            response.status().as_u16()
        ));
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| "Could not read Ollaya response.")?;
        if bytes.len().saturating_add(chunk.len()) > 1024 * 1024 {
            return Err("Ollaya response is too large.".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| "Invalid Ollaya JSON response.".into())
}

#[derive(Deserialize)]
struct Choice {
    #[serde(rename = "type")]
    kind: String,
    choice: String,
    confidence: f64,
    probabilities: HashMap<String, f64>,
}

fn parse_decision(value: serde_json::Value) -> Result<Decision, String> {
    let answer: Choice = serde_json::from_value(value["answers"]["route"].clone())
        .map_err(|_| "Missing System One route answer.")?;
    let valid_probability = |p: f64| p.is_finite() && (0.0..=1.0).contains(&p);
    let probabilities = &answer.probabilities;
    if answer.kind != "choice"
        || !["direct", "delegate"].contains(&answer.choice.as_str())
        || !valid_probability(answer.confidence)
        || probabilities.len() != 2
        || !["direct", "delegate"].iter().all(|key| {
            probabilities
                .get(*key)
                .is_some_and(|p| valid_probability(*p))
        })
        || (probabilities.values().sum::<f64>() - 1.0).abs() > 0.01
        || (2.0 * probabilities[&answer.choice] - 1.0 - answer.confidence).abs() > 0.01
    {
        return Err("Invalid System One route probabilities.".into());
    }
    Ok(Decision {
        choice: answer.choice,
        confidence: answer.confidence,
    })
}

pub async fn decide(
    connection: &ModelConnection,
    model: &str,
    instructions: &str,
    input: &str,
    timeout: u64,
) -> Result<Decision, String> {
    let body = serde_json::json!({
        "model": model, "state": input,
        "questions": { "route": {
            "type": "choice", "instructions": instructions,
            "criteria": {
                "direct": "Simple conversation and straightforward questions answerable quickly without tools.",
                "delegate": "Complex reasoning, coding, tool or computer actions, and uncertain answers."
            }
        }}
    });
    parse_decision(request(connection, "/v1/systemone", Some(body), timeout).await?)
}

/// Graph router choices use stable socket IDs, with descriptions supplied by wires.
pub async fn decide_options(
    connection: &ModelConnection,
    model: &str,
    instructions: &str,
    input: &str,
    timeout: u64,
    options: &[(String, String)],
) -> Result<Decision, String> {
    if options.len() < 2 {
        return Err("At least two router options are required.".into());
    }
    let criteria: serde_json::Map<String, serde_json::Value> = options
        .iter()
        .map(|(id, text)| (id.clone(), serde_json::Value::String(text.clone())))
        .collect();
    let body = serde_json::json!({"model": model, "state": input, "questions": {"route": {"type": "choice", "instructions": instructions, "criteria": criteria}}});
    let value = request(connection, "/v1/systemone", Some(body), timeout).await?;
    let answer: Choice = serde_json::from_value(value["answers"]["route"].clone())
        .map_err(|_| "Missing System One route answer.")?;
    if answer.kind != "choice"
        || !options.iter().any(|(id, _)| id == &answer.choice)
        || !answer.confidence.is_finite()
        || !(0.0..=1.0).contains(&answer.confidence)
        || answer.probabilities.len() != options.len()
        || !options.iter().all(|(id, _)| {
            answer
                .probabilities
                .get(id)
                .is_some_and(|p| p.is_finite() && (0.0..=1.0).contains(p))
        })
        || (answer.probabilities.values().sum::<f64>() - 1.0).abs() > 0.01
    {
        return Err("Invalid System One route probabilities.".into());
    }
    Ok(Decision {
        choice: answer.choice,
        confidence: answer.confidence,
    })
}

pub async fn list_models(connection: &ModelConnection) -> Result<Vec<String>, String> {
    let value = request(connection, "/v1/models", None, 30).await?;
    value["models"]
        .as_array()
        .ok_or("Missing Ollaya model list.")?
        .iter()
        .map(|m| {
            m["name"]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| "Invalid Ollaya model name.".into())
        })
        .collect()
}

pub async fn preload(connection: &ModelConnection, model: &str) -> Result<(), String> {
    request(
        connection,
        "/api/decide",
        Some(serde_json::json!({"model":model,"keep_alive":-1})),
        120,
    )
    .await?;
    Ok(())
}

/// Explicit setup command, run while holding Echo's single-instance guard.
/// Merge into the latest settings only after a successful local decision request.
pub async fn connect_local() -> Result<(), String> {
    use crate::{
        models::ConfiguredModel,
        pipelines::{AssistantConfig, PipelineOutput},
    };
    let mut settings = crate::settings::load_settings();
    let connection = ModelConnection {
        id: "ollaya-local".into(),
        provider: PROVIDER.into(),
        endpoint: "http://127.0.0.1:11435".into(),
        api_version: String::new(),
        custom_header_name: String::new(),
        custom_header_value: String::new(),
        api_key: String::new(),
        allow_lan_http: false,
    };
    decide(
        &connection,
        "laya:en",
        INSTRUCTIONS,
        "Hello, how are you?",
        120,
    )
    .await?;
    let mut model = ConfiguredModel::new(
        connection.id.clone(),
        "Laya · local decision".into(),
        "laya:en".into(),
    );
    model.id = "laya-local-router".into();
    model.instructions = INSTRUCTIONS.into();
    model.verified = true;
    // Do not overwrite a user's modified connection or component.
    if let Some(existing) = settings
        .model_registry
        .connections
        .iter()
        .find(|c| c.id == connection.id)
    {
        if existing != &connection {
            return Err("The existing local Ollaya connection was modified; configure it in Models & connections.".into());
        }
    } else {
        settings.model_registry.connections.push(connection);
    }
    if !settings
        .model_registry
        .models
        .iter()
        .any(|m| m.id == model.id)
    {
        settings.model_registry.models.push(model.clone());
    }
    let answer = settings
        .model_registry
        .models
        .iter()
        .find(|m| m.verified && !settings.model_registry.is_decision(&m.id))
        .map(|m| m.id.clone());
    if settings
        .pipelines
        .as_ref()
        .is_some_and(|set| set.version == 5)
    {
        let speech = settings
            .pipelines
            .as_ref()
            .and_then(|set| set.active().ok())
            .and_then(|p| p.graph.as_ref())
            .and_then(|g| g.transcription())
            .cloned()
            .ok_or("Select a graph with Transcription first.")?;
        if let Some(answer) = answer {
            let p = crate::pipelines::Pipeline {
                id: crate::pipelines::new_id(),
                name: "Laya assistant".into(),
                output: PipelineOutput::Assistant,
                steps: vec![speech],
                assistant: Some(AssistantConfig {
                    router_model_id: model.id,
                    answer_model_id: answer.clone(),
                    fast_answer_model_id: answer,
                    instructions: "You are Echo. Answer clearly and concisely.".into(),
                    preload_router_on_start: true,
                    ..Default::default()
                }),
                legacy_formatting: None,
                graph: None,
            };
            let g = crate::graph::migrate(&p, &settings);
            let set = settings.pipelines.as_mut().unwrap();
            set.active_assistant_id = p.id.clone();
            let mut p = p;
            p.graph = Some(g);
            p.steps.clear();
            p.assistant = None;
            p.output = PipelineOutput::Transcription;
            set.pipelines.push(p);
        }
        if !crate::settings::save_settings(&settings) {
            return Err("Could not save local decision connection.".into());
        }
        return Ok(());
    }
    let Some(set) = settings.pipelines.as_mut() else {
        return Err("No speech pipeline configured.".into());
    };
    if let Some(pipeline) = set
        .pipelines
        .iter_mut()
        .find(|p| p.id == set.active_assistant_id && p.output == PipelineOutput::Assistant)
    {
        let config = pipeline
            .assistant
            .as_mut()
            .ok_or("Assistant output is not configured.")?;
        if !settings.model_registry.is_decision(&config.router_model_id) {
            config.fast_answer_model_id = config.router_model_id.clone();
        }
        config.router_model_id = model.id;
        config.preload_router_on_start = true;
    } else if let Some(answer) = answer {
        let mut pipeline = set
            .pipelines
            .iter()
            .find(|p| p.id == set.active_id)
            .cloned()
            .ok_or("Select a transcription pipeline first.")?;
        pipeline.id = crate::pipelines::new_id();
        pipeline.name = "Laya assistant".into();
        pipeline.output = PipelineOutput::Assistant;
        pipeline.steps.truncate(1);
        pipeline.legacy_formatting = None;
        pipeline.assistant = Some(AssistantConfig {
            router_model_id: model.id,
            answer_model_id: answer.clone(),
            fast_answer_model_id: answer,
            instructions: "You are Echo. Answer clearly and concisely.".into(),
            preload_router_on_start: true,
            ..Default::default()
        });
        set.active_assistant_id = pipeline.id.clone();
        set.pipelines.push(pipeline);
    }
    if !crate::settings::save_settings(&settings) {
        return Err("Could not save local decision connection.".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_typed_routes_and_probabilities() {
        let value = serde_json::json!({"answers":{"route":{"type":"choice","choice":"direct","confidence":0.6,"probabilities":{"direct":0.8,"delegate":0.2}}}});
        assert_eq!(parse_decision(value.clone()).unwrap().choice, "direct");
        for bad in [serde_json::json!("tool"), serde_json::json!(null)] {
            let mut invalid = value.clone();
            invalid["answers"]["route"]["choice"] = bad;
            assert!(parse_decision(invalid).is_err());
        }
        let mut invalid = value;
        invalid["answers"]["route"]["confidence"] = serde_json::json!(0.95);
        assert!(parse_decision(invalid).is_err());
    }
    #[tokio::test]
    #[ignore = "Uses the configured Laya assistant and its real answer provider"]
    async fn configured_assistant_streams_a_reply() {
        let settings = crate::settings::load_settings();
        let pipeline = settings
            .pipelines
            .as_ref()
            .unwrap()
            .active_assistant()
            .unwrap();
        let assistant = crate::assistant::AssistantPipeline::from_pipeline(pipeline).unwrap();
        assert!(settings
            .model_registry
            .is_decision(&assistant.router_model_id));
        let mut displayed = String::new();
        let result = tokio::time::timeout(
            Duration::from_secs(90),
            crate::assistant::answer(
                &settings,
                &assistant,
                "",
                "Say hello in one short sentence.",
                |text| displayed = text.into(),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(!result.trim().is_empty());
        assert_eq!(displayed, result);
    }

    #[tokio::test]
    #[ignore = "Requires local Ollaya with laya:en installed"]
    async fn live_local_decision() {
        let connection = ModelConnection {
            id: "test".into(),
            provider: PROVIDER.into(),
            endpoint: "http://127.0.0.1:11435".into(),
            api_version: String::new(),
            custom_header_name: String::new(),
            custom_header_value: String::new(),
            api_key: String::new(),
            allow_lan_http: false,
        };
        assert!(list_models(&connection)
            .await
            .unwrap()
            .iter()
            .any(|name| name == "laya:en"));
        let result = decide(
            &connection,
            "laya:en",
            INSTRUCTIONS,
            "Hello, how are you?",
            120,
        )
        .await
        .unwrap();
        assert!(["direct", "delegate"].contains(&result.choice.as_str()));
    }
}
