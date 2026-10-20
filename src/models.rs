//! User-managed text model connections. The pipeline stores references, never keys.
use crate::settings::AppSettings;
use futures_util::StreamExt;
use rig_core::{
    client::{CompletionClient, ModelListingClient, Nothing},
    completion::{AssistantContent, CompletionModel, FinishReason},
    streaming::StreamedAssistantContent,
};
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub const DEFAULT_INSTRUCTIONS: &str = "You are a text rewriter. Fix grammar, spelling, and punctuation while preserving meaning. Return only the revised text.";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelRegistry {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub connections: Vec<ModelConnection>,
    #[serde(default)]
    pub models: Vec<ConfiguredModel>,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelConnection {
    pub id: String,
    pub provider: String,
    #[serde(default)]
    pub endpoint: String,
    #[serde(default)]
    pub api_version: String,
    #[serde(default)]
    pub custom_header_name: String,
    #[serde(default)]
    pub custom_header_value: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub allow_lan_http: bool,
}

impl std::fmt::Debug for ModelConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelConnection")
            .field("id", &self.id)
            .field("provider", &self.provider)
            .field("endpoint", &self.endpoint)
            .field("api_version", &self.api_version)
            .field("custom_header_name", &self.custom_header_name)
            .field("custom_header_value", &"[REDACTED]")
            .field("api_key", &"[REDACTED]")
            .field("allow_lan_http", &self.allow_lan_http)
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConfiguredModel {
    pub id: String,
    pub connection_id: String,
    pub name: String,
    pub model: String,
    #[serde(default = "default_instructions")]
    pub instructions: String,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u64,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_params: Option<serde_json::Value>,
    #[serde(default)]
    pub verified: bool,
    #[serde(default)]
    pub imported: bool,
}

fn default_instructions() -> String {
    DEFAULT_INSTRUCTIONS.into()
}
fn default_max_tokens() -> u64 {
    2048
}
fn default_timeout() -> u64 {
    60
}

impl ConfiguredModel {
    pub fn new(connection_id: String, name: String, model: String) -> Self {
        Self {
            id: crate::pipelines::new_id(),
            connection_id,
            name,
            model,
            instructions: default_instructions(),
            max_tokens: default_max_tokens(),
            timeout_seconds: default_timeout(),
            temperature: None,
            additional_params: None,
            verified: false,
            imported: false,
        }
    }
}

pub fn parse_additional_params(text: &str) -> Result<Option<serde_json::Value>, String> {
    if text.trim().is_empty() {
        return Ok(None);
    }
    let value: serde_json::Value = serde_json::from_str(text).map_err(|error| {
        format!(
            "Additional request parameters must be valid JSON (line {}, column {}).",
            error.line(),
            error.column()
        )
    })?;
    if !value.is_object() {
        return Err("Additional request parameters must be a JSON object.".into());
    }
    Ok(Some(value))
}

pub fn additional_params_text(value: &Option<serde_json::Value>) -> String {
    value
        .as_ref()
        .map(|value| serde_json::to_string_pretty(value).unwrap())
        .unwrap_or_default()
}

impl ModelRegistry {
    pub fn is_decision(&self, id: &str) -> bool {
        self.find(id)
            .is_ok_and(|(_, c)| c.provider == crate::system_one::PROVIDER)
    }
    pub fn find(&self, id: &str) -> Result<(&ConfiguredModel, &ModelConnection), String> {
        let model = self
            .models
            .iter()
            .find(|m| m.id == id)
            .ok_or("Configured model was removed.")?;
        let connection = self
            .connections
            .iter()
            .find(|c| c.id == model.connection_id)
            .ok_or("Model connection was removed.")?;
        Ok((model, connection))
    }

    pub fn import_gemini(&mut self, settings: &AppSettings) {
        let has_gemini_steps = settings.pipelines.as_ref().is_some_and(|set| {
            set.pipelines
                .iter()
                .flat_map(|p| &p.steps)
                .any(|s| matches!(s.kind, crate::pipelines::StepKind::Gemini { .. }))
        });
        if settings.gemini_api_key.trim().is_empty() && !has_gemini_steps {
            return;
        }
        let mut names = vec![settings.gemini_model.clone()];
        if let Some(set) = &settings.pipelines {
            for step in set.pipelines.iter().flat_map(|p| &p.steps) {
                if let crate::pipelines::StepKind::Gemini { model, .. } = &step.kind {
                    names.push(model.clone());
                }
            }
        }
        let names: Vec<_> = names.into_iter().filter(|s| !s.trim().is_empty()).collect();
        if names.is_empty() {
            return;
        }
        let connection_id = if let Some(c) = self
            .connections
            .iter()
            .find(|c| c.provider == "gemini" && c.api_key == settings.gemini_api_key)
        {
            c.id.clone()
        } else {
            let id = crate::pipelines::new_id();
            self.connections.push(ModelConnection {
                id: id.clone(),
                provider: "gemini".into(),
                endpoint: String::new(),
                api_version: String::new(),
                custom_header_name: String::new(),
                custom_header_value: String::new(),
                api_key: settings.gemini_api_key.clone(),
                allow_lan_http: false,
            });
            id
        };
        for name in names {
            if self
                .models
                .iter()
                .any(|m| m.connection_id == connection_id && m.model == name)
            {
                continue;
            }
            let mut model =
                ConfiguredModel::new(connection_id.clone(), format!("Gemini · {name}"), name);
            model.max_tokens = 512;
            model.timeout_seconds = 30;
            model.imported = true;
            model.verified = !settings.gemini_api_key.trim().is_empty();
            self.models.push(model);
        }
    }
}

pub fn validate_connection(connection: &ModelConnection) -> Result<(), String> {
    if connection.provider.trim().is_empty() {
        return Err("Choose a model provider.".into());
    }
    if connection.custom_header_name.is_empty() != connection.custom_header_value.is_empty() {
        return Err("Provide both a custom header name and value.".into());
    }
    if !connection.custom_header_name.is_empty()
        && (reqwest_rig::header::HeaderName::from_bytes(connection.custom_header_name.as_bytes())
            .is_err()
            || reqwest_rig::header::HeaderValue::from_str(&connection.custom_header_value).is_err())
    {
        return Err("Enter a valid custom HTTP header.".into());
    }
    if connection.endpoint.trim().is_empty() {
        return if connection.provider == "openai_compatible" || connection.provider == "azure" {
            Err("Enter an endpoint for an OpenAI-compatible connection.".into())
        } else {
            Ok(())
        };
    }
    let url = url::Url::parse(&connection.endpoint).map_err(|_| "Enter a valid endpoint URL.")?;
    if url.username() != ""
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("Endpoint must not contain credentials, query, or fragment.".into());
    }
    if url.scheme() == "https" {
        return Ok(());
    }
    if url.scheme() != "http" {
        return Err("Use an HTTP or HTTPS endpoint.".into());
    }
    let host = url.host_str().unwrap_or_default();
    if host == "localhost" || host == "127.0.0.1" || host == "[::1]" || connection.allow_lan_http {
        Ok(())
    } else {
        Err("Use HTTPS, or explicitly allow unencrypted LAN HTTP.".into())
    }
}

fn headers(connection: &ModelConnection) -> Result<reqwest_rig::header::HeaderMap, String> {
    let mut headers = reqwest_rig::header::HeaderMap::new();
    if !connection.custom_header_name.is_empty() {
        let name =
            reqwest_rig::header::HeaderName::from_bytes(connection.custom_header_name.as_bytes())
                .map_err(|_| "Invalid custom header name.")?;
        let value = reqwest_rig::header::HeaderValue::from_str(&connection.custom_header_value)
            .map_err(|_| "Invalid custom header value.")?;
        headers.insert(name, value);
    }
    Ok(headers)
}

async fn request<M: CompletionModel + Clone>(
    model: M,
    instructions: &str,
    max_tokens: u64,
    temperature: Option<f32>,
    additional_params: Option<serde_json::Value>,
    input: &str,
) -> Result<String, String> {
    let mut builder = model
        .completion_request(input)
        .max_tokens(max_tokens)
        .additional_params_opt(additional_params)
        .temperature_opt(temperature.map(f64::from));
    if !instructions.trim().is_empty() {
        builder = builder.preamble(instructions.to_owned());
    }
    let request = builder.build();
    let response = model.completion(request).await.map_err(|_| {
        "Model request failed; check the connection, credentials, and model ID.".to_string()
    })?;
    match response.finish_reason() {
        Some(FinishReason::Length) => {
            return Err("Model output was truncated. Increase its output-token limit.".into())
        }
        Some(FinishReason::ContentFilter) => return Err("Model output was filtered.".into()),
        Some(FinishReason::ToolCalls) => return Err("Model requested an unsupported tool.".into()),
        Some(FinishReason::Other(_)) => return Err("Model did not finish successfully.".into()),
        _ => {}
    }
    let mut text = String::new();
    for part in response.choice {
        if let AssistantContent::Text(part) = part {
            text.push_str(&part.text);
        }
        if text.len() > 1024 * 1024 {
            return Err("Model response exceeded the size limit.".into());
        }
    }
    if text.trim().is_empty() {
        return Err("Model returned no text.".into());
    }
    Ok(text)
}

#[allow(clippy::too_many_arguments)]
async fn stream_request<M, F>(
    model: M,
    instructions: &str,
    max_tokens: u64,
    temperature: Option<f32>,
    additional_params: Option<serde_json::Value>,
    input: &str,
    agent: Option<&crate::agent_tools::Runtime>,
    mut on_text: F,
) -> Result<String, String>
where
    M: CompletionModel + Clone,
    F: FnMut(&str) + Send,
{
    use rig_core::message::{Message, UserContent};
    let mut history = vec![Message::User {
        content: vec![UserContent::text(input)],
    }];
    loop {
        if let Some(agent) = agent {
            agent.check()?;
        }
        let mut builder = model
            .completion_request(
                history
                    .last()
                    .cloned()
                    .ok_or("Missing conversation input.")?,
            )
            .max_tokens(max_tokens)
            .additional_params_opt(additional_params.clone())
            .temperature_opt(temperature.map(f64::from))
            .messages(history[..history.len() - 1].iter().cloned());
        if !instructions.trim().is_empty() {
            builder = builder.preamble(instructions.into());
        }
        if let Some(agent) = agent {
            builder = builder.tools(agent.definitions.clone());
        }
        let operation = model.stream(builder.build());
        let mut response = match agent {
            Some(agent) => agent.guarded(operation).await?,
            None => operation.await,
        }
        .map_err(|_| "Model stream failed to start.")?;
        let mut text = String::new();
        let mut calls = Vec::new();
        loop {
            let item = match agent {
                Some(agent) => agent.guarded(response.next()).await?,
                None => response.next().await,
            };
            let Some(item) = item else {
                break;
            };
            if let Some(agent) = agent {
                agent.check()?;
            }
            match item.map_err(|_| "Model stream failed.")? {
                StreamedAssistantContent::Text(delta) => {
                    if text.len() + delta.text.len() > 1024 * 1024 {
                        return Err("Model response exceeded the size limit.".into());
                    }
                    text.push_str(&delta.text);
                    if agent.is_none() || text.starts_with("<echo_assistant>") {
                        on_text(&text);
                    }
                }
                StreamedAssistantContent::ToolCall { tool_call, .. } => {
                    if agent.is_none() {
                        return Err("Model requested an unavailable tool.".into());
                    }
                    if text.contains("<echo_assistant>") {
                        return Err(
                            "Model requested tools after starting its final response.".into()
                        );
                    }
                    calls.push(tool_call);
                }
                StreamedAssistantContent::Final(final_record) => match final_record.finish_reason {
                    Some(FinishReason::Length) => {
                        return Err(
                            "Model output was truncated. Increase its output-token limit.".into(),
                        )
                    }
                    Some(FinishReason::ContentFilter) => {
                        return Err("Model output was filtered.".into())
                    }
                    Some(FinishReason::Other(_)) => {
                        return Err("Model did not finish successfully.".into())
                    }
                    _ => {}
                },
                _ => {}
            }
        }
        if calls.is_empty() {
            if text.trim().is_empty() {
                return Err("Model returned no text.".into());
            }
            if agent.is_some() && !text.starts_with("<echo_assistant>") {
                on_text(&text);
            }
            return Ok(text);
        }
        let agent = agent.ok_or("Unavailable agent tools.")?;
        let content = response.choice.clone();
        history.push(Message::Assistant { content, id: None });
        let mut results = Vec::new();
        for call in &calls {
            let result = agent
                .execute(&call.function.name, call.function.arguments.clone())
                .await?;
            results.push(UserContent::tool_result_for(
                call.id.clone(),
                call.provider.clone(),
                call.function.name.clone(),
                result,
            ));
        }
        history.push(Message::User { content: results });
    }
}

/// Stream a configured model through the same Rig provider and credential layer
/// used by text-rewriting components.
pub async fn stream<F>(
    settings: &AppSettings,
    model_id: &str,
    instructions: &str,
    input: &str,
    on_text: F,
) -> Result<String, String>
where
    F: FnMut(&str) + Send,
{
    stream_with_tools(settings, model_id, instructions, input, None, on_text).await
}
async fn stream_with_tools<F>(
    settings: &AppSettings,
    model_id: &str,
    instructions: &str,
    input: &str,
    agent: Option<&crate::agent_tools::Runtime>,
    on_text: F,
) -> Result<String, String>
where
    F: FnMut(&str) + Send,
{
    let (model, connection) = settings.model_registry.find(model_id)?;
    if connection.provider == crate::system_one::PROVIDER {
        return Err("Decision models belong in the assistant router, not a text component.".into());
    }
    if !model.verified {
        return Err("Test this model in Models & connections before use.".into());
    }
    validate_connection(connection)?;
    let http = reqwest_rig::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(model.timeout_seconds.clamp(5, 600)))
        .redirect(reqwest_rig::redirect::Policy::none())
        .build()
        .map_err(|_| "Could not create model HTTP client.")?;
    let key = connection.api_key.as_str();
    let extra_headers = headers(connection)?;
    let name = model.model.as_str();
    let endpoint = connection.endpoint.as_str();
    let max_tokens = model.max_tokens;
    let temperature = model.temperature;
    macro_rules! provider {
        ($p:ident) => {{
            let mut builder = rig_core::providers::$p::Client::builder()
                .api_key(key)
                .http_client(crate::models_transport::LimitedClient(http.clone()))
                .http_headers(extra_headers.clone());
            if !endpoint.is_empty() {
                builder = builder.base_url(endpoint);
            }
            let client = builder
                .build()
                .map_err(|_| "Could not configure the model provider.")?;
            stream_request(
                client.completion_model(name),
                instructions,
                max_tokens,
                temperature,
                model.additional_params.clone(),
                input,
                agent,
                on_text,
            )
            .await
        }};
    }
    match connection.provider.as_str() {
        "gemini" => provider!(gemini),
        "openai" => provider!(openai),
        "anthropic" => provider!(anthropic),
        "openrouter" => provider!(openrouter),
        "deepseek" => provider!(deepseek),
        "groq" => provider!(groq),
        "mistral" => provider!(mistral),
        "together" => provider!(together),
        "xai" => provider!(xai),
        "cohere" => provider!(cohere),
        "perplexity" => provider!(perplexity),
        "venice" => provider!(venice),
        "zai" => provider!(zai),
        "moonshot" => provider!(moonshot),
        "hyperbolic" => provider!(hyperbolic),
        "mira" => provider!(mira),
        "minimax" => provider!(minimax),
        "xiaomimimo" => provider!(xiaomimimo),
        "huggingface" => provider!(huggingface),
        "azure" => {
            let client = rig_core::providers::azure::Client::builder()
                .api_key(rig_core::providers::azure::AzureOpenAIAuth::ApiKey(
                    key.to_owned(),
                ))
                .http_client(crate::models_transport::LimitedClient(http))
                .http_headers(extra_headers)
                .azure_endpoint(endpoint.to_owned())
                .api_version(if connection.api_version.is_empty() {
                    "2024-10-21"
                } else {
                    &connection.api_version
                })
                .build()
                .map_err(|_| "Could not configure Azure OpenAI.")?;
            stream_request(
                client.completion_model(name),
                instructions,
                max_tokens,
                temperature,
                model.additional_params.clone(),
                input,
                agent,
                on_text,
            )
            .await
        }
        "openai_compatible" => {
            let client = rig_core::providers::openai::CompletionsClient::builder()
                .api_key(key)
                .http_client(crate::models_transport::LimitedClient(http))
                .http_headers(extra_headers)
                .base_url(endpoint)
                .build()
                .map_err(|_| "Could not configure compatible model server.")?;
            stream_request(
                client.completion_model(name),
                instructions,
                max_tokens,
                temperature,
                model.additional_params.clone(),
                input,
                agent,
                on_text,
            )
            .await
        }
        "ollama" => {
            if key.is_empty() {
                let mut builder = rig_core::providers::ollama::Client::builder()
                    .api_key(Nothing)
                    .http_client(crate::models_transport::LimitedClient(http))
                    .http_headers(extra_headers);
                if !endpoint.is_empty() {
                    builder = builder.base_url(endpoint);
                }
                let client = builder.build().map_err(|_| "Could not configure Ollama.")?;
                stream_request(
                    client.completion_model(name),
                    instructions,
                    max_tokens,
                    temperature,
                    model.additional_params.clone(),
                    input,
                    agent,
                    on_text,
                )
                .await
            } else {
                provider!(ollama)
            }
        }
        _ => Err("This provider does not yet support assistant streaming.".into()),
    }
}

// Match complete()'s instance settings, adding the streaming callback.
#[allow(clippy::too_many_arguments)]
pub async fn stream_component<F>(
    settings: &AppSettings,
    model_id: &str,
    instructions: &str,
    max_tokens: u64,
    temperature: Option<f32>,
    timeout_seconds: u64,
    input: &str,
    on_text: F,
) -> Result<String, String>
where
    F: FnMut(&str) + Send,
{
    let mut scoped = settings.clone();
    if let Some(model) = scoped
        .model_registry
        .models
        .iter_mut()
        .find(|m| m.id == model_id)
    {
        model.max_tokens = max_tokens;
        model.temperature = temperature;
        model.timeout_seconds = timeout_seconds;
    }
    stream(&scoped, model_id, instructions, input, on_text).await
}

#[allow(clippy::too_many_arguments)]
pub async fn stream_agent_component<F>(
    settings: &AppSettings,
    model_id: &str,
    instructions: &str,
    max_tokens: u64,
    temperature: Option<f32>,
    timeout_seconds: u64,
    input: &str,
    agent: Option<&crate::agent_tools::Runtime>,
    on_text: F,
) -> Result<String, String>
where
    F: FnMut(&str) + Send,
{
    if agent.is_none() {
        return stream_component(
            settings,
            model_id,
            instructions,
            max_tokens,
            temperature,
            timeout_seconds,
            input,
            on_text,
        )
        .await;
    }
    let mut scoped = settings.clone();
    if let Some(model) = scoped
        .model_registry
        .models
        .iter_mut()
        .find(|m| m.id == model_id)
    {
        model.max_tokens = max_tokens;
        model.temperature = temperature;
        model.timeout_seconds = timeout_seconds;
    }
    stream_with_tools(&scoped, model_id, instructions, input, agent, on_text).await
}

pub async fn complete(
    settings: &AppSettings,
    model_id: &str,
    instructions: &str,
    max_tokens: u64,
    temperature: Option<f32>,
    timeout_seconds: u64,
    input: &str,
) -> Result<String, String> {
    let (model, connection) = settings.model_registry.find(model_id)?;
    if connection.provider == crate::system_one::PROVIDER {
        return Err("Decision models belong in the assistant router, not a text component.".into());
    }
    if !model.verified {
        return Err("Test this model in Models & connections before use.".into());
    }
    validate_connection(connection)?;
    let http = reqwest_rig::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(timeout_seconds.clamp(5, 600)))
        .redirect(reqwest_rig::redirect::Policy::none())
        .build()
        .map_err(|_| "Could not create model HTTP client.")?;
    let key = connection.api_key.as_str();
    let extra_headers = headers(connection)?;
    let name = model.model.as_str();
    let endpoint = connection.endpoint.as_str();
    macro_rules! provider {
        ($p:ident) => {{
            let mut builder = rig_core::providers::$p::Client::builder()
                .api_key(key)
                .http_client(crate::models_transport::LimitedClient(http.clone()))
                .http_headers(extra_headers.clone());
            if !endpoint.is_empty() {
                builder = builder.base_url(endpoint);
            }
            let client = builder
                .build()
                .map_err(|_| "Could not configure the model provider.")?;
            request(
                client.completion_model(name),
                instructions,
                max_tokens,
                temperature,
                model.additional_params.clone(),
                input,
            )
            .await
        }};
    }
    match connection.provider.as_str() {
        "gemini" => provider!(gemini),
        "openai" => provider!(openai),
        "openai_compatible" => {
            let client = rig_core::providers::openai::CompletionsClient::builder()
                .api_key(key)
                .http_client(crate::models_transport::LimitedClient(http))
                .http_headers(extra_headers.clone())
                .base_url(endpoint)
                .build()
                .map_err(|_| "Could not configure the compatible model server.")?;
            request(
                client.completion_model(name),
                instructions,
                max_tokens,
                temperature,
                model.additional_params.clone(),
                input,
            )
            .await
        }
        "anthropic" => provider!(anthropic),
        "openrouter" => provider!(openrouter),
        "deepseek" => provider!(deepseek),
        "groq" => provider!(groq),
        "mistral" => provider!(mistral),
        "together" => provider!(together),
        "xai" => provider!(xai),
        "cohere" => provider!(cohere),
        "perplexity" => provider!(perplexity),
        "venice" => provider!(venice),
        "zai" => provider!(zai),
        "moonshot" => provider!(moonshot),
        "hyperbolic" => provider!(hyperbolic),
        "mira" => provider!(mira),
        "minimax" => provider!(minimax),
        "xiaomimimo" => provider!(xiaomimimo),
        "huggingface" => provider!(huggingface),
        "azure" => {
            let client = rig_core::providers::azure::Client::builder()
                .api_key(rig_core::providers::azure::AzureOpenAIAuth::ApiKey(
                    key.to_owned(),
                ))
                .http_client(crate::models_transport::LimitedClient(http))
                .http_headers(extra_headers.clone())
                .azure_endpoint(endpoint.to_owned())
                .api_version(if connection.api_version.is_empty() {
                    "2024-10-21"
                } else {
                    &connection.api_version
                })
                .build()
                .map_err(|_| "Could not configure Azure OpenAI.")?;
            request(
                client.completion_model(name),
                instructions,
                max_tokens,
                temperature,
                model.additional_params.clone(),
                input,
            )
            .await
        }
        "ollama" => {
            if key.is_empty() {
                let mut builder = rig_core::providers::ollama::Client::builder()
                    .api_key(Nothing)
                    .http_client(crate::models_transport::LimitedClient(http))
                    .http_headers(extra_headers.clone());
                if !endpoint.is_empty() {
                    builder = builder.base_url(endpoint);
                }
                let client = builder.build().map_err(|_| "Could not configure Ollama.")?;
                request(
                    client.completion_model(name),
                    instructions,
                    max_tokens,
                    temperature,
                    model.additional_params.clone(),
                    input,
                )
                .await
            } else {
                provider!(ollama)
            }
        }
        _ => Err("This provider is not supported by Echo's Rig adapter.".into()),
    }
}

pub async fn list_models(connection: &ModelConnection) -> Result<Vec<String>, String> {
    if connection.provider == crate::system_one::PROVIDER {
        return crate::system_one::list_models(connection).await;
    }
    validate_connection(connection)?;
    let http = reqwest_rig::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .redirect(reqwest_rig::redirect::Policy::none())
        .build()
        .map_err(|_| "Could not create model HTTP client.")?;
    let key = connection.api_key.as_str();
    let extra_headers = headers(connection)?;
    let endpoint = connection.endpoint.as_str();
    macro_rules! listed {
        ($p:ident) => {{
            let mut builder = rig_core::providers::$p::Client::builder()
                .api_key(key)
                .http_client(crate::models_transport::LimitedClient(http.clone()))
                .http_headers(extra_headers.clone());
            if !endpoint.is_empty() {
                builder = builder.base_url(endpoint);
            }
            let client = builder
                .build()
                .map_err(|_| "Could not configure model listing.")?;
            client
                .list_models()
                .await
                .map_err(|_| "Could not list models from this provider.".to_string())?
        }};
    }
    let models = match connection.provider.as_str() {
        "gemini" => listed!(gemini),
        "openai" => listed!(openai),
        "anthropic" => listed!(anthropic),
        "openrouter" => listed!(openrouter),
        "groq" => listed!(groq),
        "deepseek" => listed!(deepseek),
        "mistral" => listed!(mistral),
        "moonshot" => listed!(moonshot),
        "venice" => listed!(venice),
        "mira" => listed!(mira),
        "minimax" => listed!(minimax),
        "xiaomimimo" => listed!(xiaomimimo),
        "openai_compatible" => {
            let client = rig_core::providers::openai::CompletionsClient::builder()
                .api_key(key)
                .http_client(crate::models_transport::LimitedClient(http))
                .http_headers(extra_headers.clone())
                .base_url(endpoint)
                .build()
                .map_err(|_| "Could not configure model listing.")?;
            client
                .list_models()
                .await
                .map_err(|_| "Could not list models from this server.".to_string())?
        }
        "ollama" => {
            let mut builder = rig_core::providers::ollama::Client::builder()
                .api_key(key)
                .http_client(crate::models_transport::LimitedClient(http))
                .http_headers(extra_headers.clone());
            if !endpoint.is_empty() {
                builder = builder.base_url(endpoint);
            }
            let client = builder
                .build()
                .map_err(|_| "Could not configure Ollama listing.")?;
            client
                .list_models()
                .await
                .map_err(|_| "Could not list Ollama models.".to_string())?
        }
        _ => {
            return Err(
                "This provider does not offer model listing here. Enter its model ID manually."
                    .into(),
            )
        }
    };
    let mut ids: Vec<_> = models.iter().map(|m| m.id.clone()).collect();
    ids.sort();
    ids.dedup();
    Ok(ids)
}

/// Test a model with its own protocol; decision results are never used as generated text.
pub async fn test_model(settings: &AppSettings, id: &str, input: &str) -> Result<String, String> {
    let (model, connection) = settings.model_registry.find(id)?;
    if connection.provider == crate::system_one::PROVIDER {
        let result = crate::system_one::decide(
            connection,
            &model.model,
            &model.instructions,
            input,
            model.timeout_seconds,
        )
        .await?;
        Ok(format!(
            "Route: {} · confidence: {:.1}%",
            result.choice,
            result.confidence * 100.0
        ))
    } else {
        complete(
            settings,
            id,
            &model.instructions,
            model.max_tokens,
            model.temperature,
            model.timeout_seconds,
            input,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn additional_parameters_validate_and_roundtrip() {
        assert_eq!(parse_additional_params("  ").unwrap(), None);
        for invalid in ["[]", "null", "true", "42", "{broken}"] {
            assert!(parse_additional_params(invalid).is_err());
        }
        let mut model = ConfiguredModel::new("c".into(), "Test".into(), "test".into());
        let legacy = serde_json::to_value(&model).unwrap();
        assert!(legacy.get("additional_params").is_none());
        assert_eq!(
            serde_json::from_value::<ConfiguredModel>(legacy)
                .unwrap()
                .additional_params,
            None
        );
        model.additional_params =
            parse_additional_params(r#"{"reasoning_effort":"high","nested":{"enabled":true}}"#)
                .unwrap();
        let saved = serde_json::to_value(&model).unwrap();
        assert_eq!(
            serde_json::from_value::<ConfiguredModel>(saved).unwrap(),
            model
        );
        assert_eq!(
            parse_additional_params(&additional_params_text(&model.additional_params)).unwrap(),
            model.additional_params
        );
    }

    #[tokio::test]
    async fn additional_parameters_reach_tests_and_streamed_components() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            for streaming in [false, true] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let body = loop {
                    let mut chunk = [0u8; 4096];
                    let size = socket.read(&mut chunk).await.unwrap();
                    assert!(size > 0);
                    bytes.extend_from_slice(&chunk[..size]);
                    if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]);
                        let length: usize = headers
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse().unwrap())
                            })
                            .unwrap();
                        if bytes.len() >= end + 4 + length {
                            break serde_json::from_slice::<serde_json::Value>(
                                &bytes[end + 4..end + 4 + length],
                            )
                            .unwrap();
                        }
                    }
                };
                assert_eq!(body["reasoning_effort"], "high");
                assert_eq!(body["stream"].as_bool().unwrap_or(false), streaming);
                let (content_type, response) = if streaming {
                    ("text/event-stream", "data: {\"id\":\"mock\",\"object\":\"chat.completion.chunk\",\"created\":0,\"model\":\"test\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Verified\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n")
                } else {
                    (
                        "application/json",
                        r#"{"id":"mock","object":"chat.completion","created":0,"model":"test","choices":[{"index":0,"message":{"role":"assistant","content":"Verified"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#,
                    )
                };
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",response.len()).as_bytes()).await.unwrap();
            }
        });
        let mut settings = AppSettings::default();
        settings.model_registry.connections.push(ModelConnection {
            id: "c".into(),
            provider: "openai_compatible".into(),
            endpoint: format!("http://127.0.0.1:{port}/v1"),
            api_version: String::new(),
            custom_header_name: String::new(),
            custom_header_value: String::new(),
            api_key: String::new(),
            allow_lan_http: false,
        });
        let mut model = ConfiguredModel::new("c".into(), "Test".into(), "test".into());
        model.verified = true;
        model.additional_params = Some(serde_json::json!({"reasoning_effort":"high"}));
        let id = model.id.clone();
        settings.model_registry.models.push(model);
        assert_eq!(
            test_model(&settings, &id, "hello").await.unwrap(),
            "Verified"
        );
        let mut updates = Vec::new();
        assert_eq!(
            stream_component(&settings, &id, "", 128, None, 5, "hello", |text| updates
                .push(text.to_owned()))
            .await
            .unwrap(),
            "Verified"
        );
        assert_eq!(updates, vec!["Verified"]);
        server.await.unwrap();
    }
    #[test]
    fn import_gemini_once_and_redact_credentials() {
        let mut settings = AppSettings {
            gemini_api_key: "private-test-key".into(),
            ..Default::default()
        };
        let mut registry = ModelRegistry::default();
        registry.import_gemini(&settings);
        registry.import_gemini(&settings);
        assert_eq!(registry.models.len(), 1);
        assert_eq!(registry.connections.len(), 1);
        assert!(!format!("{:?}", registry.connections[0]).contains("private-test-key"));
        settings.gemini_api_key.clear();
        settings.gemini_enabled = true;
        settings.pipelines = Some(crate::pipelines::migrate(&settings));
        let mut unverified = ModelRegistry::default();
        unverified.import_gemini(&settings);
        assert!(!unverified.models[0].verified);
    }

    #[test]
    fn explicit_lan_http_and_custom_endpoint() {
        let mut c = ModelConnection {
            id: "c".into(),
            provider: "openai_compatible".into(),
            endpoint: "http://192.168.1.25:1234/v1".into(),
            api_version: String::new(),
            custom_header_name: String::new(),
            custom_header_value: String::new(),
            api_key: String::new(),
            allow_lan_http: false,
        };
        assert!(validate_connection(&c).is_err());
        c.allow_lan_http = true;
        assert!(validate_connection(&c).is_ok());
        c.endpoint.clear();
        assert!(validate_connection(&c).is_err());
    }

    #[tokio::test]
    async fn openai_compatible_local_completion() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let server = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = server.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            let (mut stream, _) = server.accept().await.unwrap();
            let mut data = vec![0; 8192];
            let used = stream.read(&mut data).await.unwrap();
            let request = String::from_utf8_lossy(&data[..used]);
            assert!(request.contains("/v1/chat/completions"), "{request}");
            let body = r#"{"id":"mock","object":"chat.completion","created":0,"model":"test-model","choices":[{"index":0,"message":{"role":"assistant","content":"Verified text"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":2,"total_tokens":3}}"#;
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).as_bytes()).await.unwrap();
        });
        let mut settings = AppSettings::default();
        let conn = ModelConnection {
            id: "c".into(),
            provider: "openai_compatible".into(),
            endpoint: format!("http://127.0.0.1:{port}/v1"),
            api_version: String::new(),
            custom_header_name: String::new(),
            custom_header_value: String::new(),
            api_key: String::new(),
            allow_lan_http: false,
        };
        settings.model_registry.connections.push(conn);
        let mut model = ConfiguredModel::new("c".into(), "Test".into(), "test-model".into());
        model.verified = true;
        let id = model.id.clone();
        settings.model_registry.models.push(model);
        settings.transcription_provider = "local_sherpa_onnx".into();
        let mut pipeline = crate::pipelines::migrate(&settings).pipelines.remove(0);
        pipeline.steps.push(crate::pipelines::Step::new(
            crate::pipelines::StepKind::Model {
                model_id: id.clone(),
                name: "Test".into(),
                instructions: DEFAULT_INSTRUCTIONS.into(),
                max_tokens: 128,
                temperature: None,
                timeout_seconds: 5,
            },
        ));
        assert!(pipeline.preflight(&settings).is_ok());
        let result = complete(&settings, &id, DEFAULT_INSTRUCTIONS, 128, None, 5, "hello").await;
        task.await.unwrap();
        assert_eq!(result.unwrap(), "Verified text");
    }

    #[tokio::test]
    async fn gemini_import_uses_rig_completion() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut data = vec![0; 8192];
            let used = stream.read(&mut data).await.unwrap();
            assert!(String::from_utf8_lossy(&data[..used]).contains("generateContent"));
            let body = r#"{"candidates":[{"content":{"parts":[{"text":"Gemini result"}],"role":"model"},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":1,"candidatesTokenCount":2,"totalTokenCount":3}}"#;
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).as_bytes()).await.unwrap();
        });
        let mut settings = AppSettings::default();
        let connection = ModelConnection {
            id: "gemini-connection".into(),
            provider: "gemini".into(),
            endpoint: format!("http://127.0.0.1:{port}"),
            api_version: String::new(),
            custom_header_name: String::new(),
            custom_header_value: String::new(),
            api_key: "test-key".into(),
            allow_lan_http: false,
        };
        settings.model_registry.connections.push(connection);
        let mut model = ConfiguredModel::new(
            "gemini-connection".into(),
            "Gemini".into(),
            "gemini-test".into(),
        );
        model.verified = true;
        let id = model.id.clone();
        settings.model_registry.models.push(model);
        let result = complete(
            &settings,
            &id,
            DEFAULT_INSTRUCTIONS,
            128,
            Some(0.4),
            5,
            "hello",
        )
        .await;
        server.await.unwrap();
        assert_eq!(result.unwrap(), "Gemini result");
    }

    #[tokio::test]
    async fn cancelling_a_waiting_model_preserves_previous_text() {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            if let Ok(Ok((_stream, _))) =
                tokio::time::timeout(Duration::from_secs(1), listener.accept()).await
            {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        });
        let mut settings = AppSettings {
            transcription_provider: "local_sherpa_onnx".into(),
            ..Default::default()
        };
        settings.post_processing.enabled = false;
        settings.model_registry.connections.push(ModelConnection {
            id: "local".into(),
            provider: "openai_compatible".into(),
            endpoint: format!("http://127.0.0.1:{port}/v1"),
            api_version: String::new(),
            custom_header_name: String::new(),
            custom_header_value: String::new(),
            api_key: String::new(),
            allow_lan_http: false,
        });
        let mut model = ConfiguredModel::new("local".into(), "Local".into(), "test-model".into());
        model.verified = true;
        let model_id = model.id.clone();
        settings.model_registry.models.push(model);
        let mut pipeline = crate::pipelines::migrate(&settings).pipelines.remove(0);
        pipeline.steps.push(crate::pipelines::Step::new(
            crate::pipelines::StepKind::Model {
                model_id,
                name: "Local".into(),
                instructions: DEFAULT_INSTRUCTIONS.into(),
                max_tokens: 128,
                temperature: None,
                timeout_seconds: 5,
            },
        ));
        let cancelled = Arc::new(AtomicBool::new(false));
        let signal = cancelled.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(250)).await;
            signal.store(true, Ordering::SeqCst);
        });
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            crate::pipelines::execute(
                &pipeline,
                "original text".into(),
                &settings,
                |_, _, _| {},
                || cancelled.load(Ordering::SeqCst),
            ),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(result.text, "original text");
        assert_eq!(result.message, "Recording cancelled.");
        server.await.unwrap();
    }
}
