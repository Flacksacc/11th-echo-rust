//! Typed, authenticated client for Echo's thin native Hermes adapter.
//! No model harness, worker scheduler, or writes to Hermes storage live here.
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Duration;

// Shared wire-payload budget across every active conversation. A per-stream
// message count alone permits large frames in many tabs to multiply memory.
const PENDING_EVENT_BYTES: usize = 16 * 1024 * 1024;
static EVENT_BUDGET: std::sync::LazyLock<std::sync::Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| {
        std::sync::Arc::new(tokio::sync::Semaphore::new(PENDING_EVENT_BYTES))
    });

#[derive(Debug)]
pub struct RunStartError {
    pub message: String,
    pub definitely_rejected: bool,
}
impl From<&str> for RunStartError {
    fn from(message: &str) -> Self {
        Self {
            message: message.into(),
            definitely_rejected: false,
        }
    }
}

pub fn enable_current_assistant(settings: &mut crate::settings::AppSettings) -> Result<(), String> {
    inherit_assistant_model(settings);
    let (model, connection) = settings
        .model_registry
        .find(&settings.hermes.fast_model_id)?;
    if !model.verified {
        return Err("Verify the current assistant model before enabling Hermes.".into());
    }
    crate::models::validate_connection(connection)?;
    if settings.hermes.token.len() < 32 {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes)
            .map_err(|_| "Could not generate local Hermes authentication.")?;
        settings.hermes.token = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    }
    settings.hermes.enabled = true;
    Ok(())
}

/// Resolve the answer model on the selected assistant path, never a router or
/// an unrelated verified model. An explicit Hermes choice always takes priority.
pub fn inherit_assistant_model(settings: &mut crate::settings::AppSettings) {
    if !settings.hermes.fast_model_id.is_empty() {
        return;
    }
    let Some(pipelines) = &settings.pipelines else {
        return;
    };
    let Ok(pipeline) = pipelines.assistant_for_runtime() else {
        return;
    };
    let mut candidates = Vec::new();
    if let Some(graph) = &pipeline.graph {
        let mut pending = std::collections::VecDeque::new();
        let mut visited = std::collections::HashSet::new();
        for node in &graph.nodes {
            if matches!(node.kind, crate::graph::Kind::AssistantDisplay) {
                pending.push_back(node.id.as_str());
            }
        }
        while let Some(id) = pending.pop_front() {
            if !visited.insert(id) {
                continue;
            }
            if let Some(node) = graph.nodes.iter().find(|node| node.id == id) {
                if let Some(crate::pipelines::Step {
                    kind: crate::pipelines::StepKind::Model { model_id, .. },
                    ..
                }) = node.step()
                {
                    candidates.push(model_id.as_str());
                }
            }
            for edge in graph
                .edges
                .iter()
                .filter(|edge| edge.to == id && edge.input != "files" && edge.input != "image")
            {
                pending.push_back(edge.from.as_str());
            }
        }
    }
    if let Some(config) = &pipeline.assistant {
        candidates.extend([
            config.answer_model_id.as_str(),
            config.fast_answer_model_id.as_str(),
        ]);
    }
    if let Some(id) = candidates.into_iter().find(|id| {
        settings
            .model_registry
            .models
            .iter()
            .any(|model| model.id == *id && model.verified)
            && !settings.model_registry.is_decision(id)
    }) {
        settings.hermes.fast_model_id = id.to_owned();
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub enabled: bool,
    pub fast_model_id: String,
    pub worker_model: String,
    pub bridge_port: u16,
    pub gateway_port: u16,
    pub token: String,
    pub phrases_enabled: bool,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            fast_model_id: String::new(),
            worker_model: String::new(),
            bridge_port: 8643,
            gateway_port: 8642,
            token: String::new(),
            phrases_enabled: true,
        }
    }
}
impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HermesConfig")
            .field("enabled", &self.enabled)
            .field("fast_model_id", &self.fast_model_id)
            .field("credentials", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base: String,
    token: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RunEvent {
    #[serde(skip)]
    _pending: Option<std::sync::Arc<tokio::sync::OwnedSemaphorePermit>>,
    pub event: String,
    #[serde(default)]
    pub run_id: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub delta: String,
    #[serde(default)]
    pub output: String,
    #[serde(default)]
    pub already_streamed: bool,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

/// Close the file before cleanup on Windows, including cancellation of the
/// owning future. A partial export must never replace the chosen destination.
struct PendingExport {
    path: std::path::PathBuf,
    file: Option<tokio::fs::File>,
}
impl Drop for PendingExport {
    fn drop(&mut self) {
        drop(self.file.take());
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Client {
    pub async fn codex_import_request(&self, action: &str, body: Value) -> Result<Value, String> {
        if !matches!(
            action,
            "profiles" | "add-profile" | "catalog" | "preview" | "import" | "status" | "cancel"
        ) {
            return Err("Unknown Codex import operation.".into());
        }
        let result = self
            .request(
                reqwest::Method::POST,
                &format!("/echo/codex/{action}"),
                Some(body),
            )
            .await?;
        if let Some(error) = result["error"].as_str() {
            return Err(error.to_owned());
        }
        Ok(result)
    }
    pub fn new(config: &Config) -> Result<Self, String> {
        if config.bridge_port == 0 || config.gateway_port == 0 || config.token.len() < 32 {
            return Err(
                "Hermes needs a valid loopback port and local authentication token.".into(),
            );
        }
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .build()
            .map_err(|_| "Could not initialize the Hermes client.")?;
        Ok(Self {
            http,
            base: format!("http://127.0.0.1:{}", config.bridge_port),
            token: config.token.clone(),
        })
    }

    async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, String> {
        let mut request = self
            .http
            .request(method, format!("{}{}", self.base, path))
            .bearer_auth(&self.token)
            .timeout(Duration::from_secs(30));
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request
            .send()
            .await
            .map_err(|_| "Hermes is disconnected or still loading.")?;
        if !response.status().is_success() {
            // Error bodies may contain private tool/transcript content; never
            // expose them to diagnostics or put secrets into reqwest errors.
            return Err(format!(
                "Hermes returned HTTP {}.",
                response.status().as_u16()
            ));
        }
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| "Hermes response was interrupted.")?;
            if bytes.len() + chunk.len() > 2 * 1024 * 1024 {
                return Err(
                    "Hermes response exceeds the page budget. Request a smaller page.".into(),
                );
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes)
            .map_err(|_| "Hermes returned an incompatible response.".into())
    }

    pub async fn capabilities(&self) -> Result<Value, String> {
        let bridge = self
            .request(reqwest::Method::GET, "/echo/capabilities", None)
            .await?;
        if bridge.get("protocol").and_then(Value::as_u64) != Some(1) {
            return Err("The Hermes adapter requires an Echo compatibility update.".into());
        }
        if bridge.get("task_authority").and_then(Value::as_str) != Some("hermes-native")
            || bridge.get("history_authority").and_then(Value::as_str) != Some("hermes-native")
        {
            return Err("The Hermes adapter does not expose native tasks and history.".into());
        }
        self.request(reqwest::Method::GET, "/native/v1/capabilities", None)
            .await
    }

    pub async fn create_session(&self, title: &str) -> Result<String, String> {
        // Hermes enforces unique native titles, even for empty conversations.
        // Echo's friendly tab name is independent of this native identity.
        let id = format!("echo_{}", crate::pipelines::new_id());
        let title_budget = 100usize.saturating_sub(id.len() + 3);
        let title = format!(
            "{} · {id}",
            title.chars().take(title_budget).collect::<String>()
        );
        let result = self
            .request(
                reqwest::Method::POST,
                "/native/api/sessions",
                Some(json!({"id": id, "title": title, "source": "echo"})),
            )
            .await?;
        result
            .pointer("/session/id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| "Hermes did not return a native session ID.".into())
    }

    pub async fn task_conversation(&self, task: &str) -> Result<String, String> {
        self.request(
            reqwest::Method::POST,
            &format!("/echo/tasks/{}/conversation", path_id(task)?),
            None,
        )
        .await?["session_id"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| "Hermes did not return the task conversation.".into())
    }

    pub async fn start_run(
        &self,
        session: &str,
        input: &str,
        idempotency_key: &str,
        instructions: Option<&str>,
    ) -> Result<String, RunStartError> {
        let mut last = RunStartError::from("Hermes admission is unconfirmed. Check native history before sending this message again.");
        let mut uncertain = false;
        for attempt in 0..3 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(250 * attempt)).await;
            }
            match self
                .start_run_once(session, input, idempotency_key, instructions)
                .await
            {
                Ok(run) => return Ok(run),
                Err(error) if error.definitely_rejected && !uncertain => return Err(error),
                Err(mut error) if error.definitely_rejected => {
                    error.definitely_rejected = false;
                    last = error;
                    break;
                }
                Err(error) => {
                    uncertain = true;
                    last = error;
                }
            }
        }
        last.message = "Hermes admission is unconfirmed. The same native request ID was retried; check conversation history before sending this message again.".into();
        Err(last)
    }

    async fn start_run_once(
        &self,
        session: &str,
        input: &str,
        idempotency_key: &str,
        instructions: Option<&str>,
    ) -> Result<String, RunStartError> {
        let response = self
            .http
            .post(format!("{}/native/v1/runs", self.base))
            .bearer_auth(&self.token)
            .header("Idempotency-Key", idempotency_key)
            .timeout(Duration::from_secs(30))
            .json(&json!({"session_id": session, "input": input, "instructions": instructions}))
            .send()
            .await
            .map_err(|_| RunStartError::from("Hermes admission response was not received."))?;
        if !response.status().is_success() {
            return Err(RunStartError {
                message: format!("Hermes turn returned HTTP {}.", response.status().as_u16()),
                definitely_rejected: response.status().is_client_error(),
            });
        }
        let mut stream = response.bytes_stream();
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| "Hermes run response was interrupted.")?;
            if bytes.len() + chunk.len() > 256 * 1024 {
                return Err("Hermes run response exceeded its metadata budget.".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        let result: Value = serde_json::from_slice(&bytes)
            .map_err(|_| "Hermes returned an invalid run response.")?;
        result
            .get("run_id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| RunStartError::from("Hermes did not return a native run ID."))
    }

    pub async fn events(
        &self,
        run: &str,
        destination: tokio::sync::mpsc::Sender<RunEvent>,
    ) -> Result<(), String> {
        let run = path_id(run)?;
        let mut last_seq: Option<u64> = None;
        for attempt in 0..6 {
            if destination.is_closed() {
                return Err("Echo event consumer closed.".into());
            }
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(250 * attempt)).await;
            }
            let mut request = self
                .http
                .get(format!("{}/native/v1/runs/{run}/events", self.base))
                .bearer_auth(&self.token);
            if let Some(seq) = last_seq {
                request = request.header("Last-Event-ID", seq.to_string());
            }
            let Ok(response) = request.send().await else {
                continue;
            };
            if !response.status().is_success() {
                return Err(format!(
                    "Hermes events returned HTTP {}.",
                    response.status().as_u16()
                ));
            }
            let mut decoder = SseDecoder::default();
            let mut stream = response.bytes_stream();
            while let Ok(Some(Ok(chunk))) =
                tokio::time::timeout(Duration::from_secs(90), stream.next()).await
            {
                for data in decoder.push(&chunk)? {
                    let permit = tokio::select! {
                        permit = EVENT_BUDGET.clone().acquire_many_owned(data.len().max(1) as u32) =>
                            permit.map_err(|_| "Hermes event budget is unavailable.")?,
                        _ = destination.closed() => return Err("Echo event consumer closed.".into()),
                    };
                    let mut event: RunEvent = serde_json::from_str(&data)
                        .map_err(|_| "Hermes sent an incompatible event.")?;
                    event._pending = Some(std::sync::Arc::new(permit));
                    if event.event == "replay.truncated" {
                        return Err("Hermes no longer retains part of this live stream. Reload the conversation from native history.".into());
                    }
                    if let Some(seq) = event.extra.get("seq").and_then(Value::as_u64) {
                        if last_seq.is_some_and(|last| seq <= last) {
                            continue;
                        }
                        last_seq = Some(seq);
                    }
                    let terminal = matches!(
                        event.event.as_str(),
                        "run.completed" | "run.failed" | "run.stopped" | "run.cancelled"
                    );
                    destination
                        .send(event)
                        .await
                        .map_err(|_| "Echo event consumer closed.")?;
                    if terminal {
                        return Ok(());
                    }
                }
            }
        }
        Err("Hermes event stream disconnected after replay attempts. Background tasks may still be running.".into())
    }

    pub async fn tasks(&self) -> Result<Value, String> {
        self.request(reqwest::Method::GET, "/echo/tasks", None)
            .await
    }
    pub async fn runtime_idle(&self) -> Result<bool, String> {
        self.request(reqwest::Method::GET, "/echo/runtime/idle", None)
            .await
            .map(|reply| reply["idle"] == true)
    }
    pub async fn task_events(&self, after: u64) -> Result<Value, String> {
        self.request(
            reqwest::Method::GET,
            &format!("/echo/events?after={after}&limit=100"),
            None,
        )
        .await
    }
    pub async fn task_view(
        &self,
        task: &str,
        output: bool,
        cursor: Option<(u64, u64)>,
    ) -> Result<Value, String> {
        let (message, end) = cursor.unwrap_or((i64::MAX as u64, u64::MAX));
        self.request(
            reqwest::Method::GET,
            &format!(
                "/echo/tasks/{}/view?output={}&before={message}&end={end}",
                path_id(task)?,
                u8::from(output)
            ),
            None,
        )
        .await
    }
    pub async fn task_updates(
        &self,
        task: &str,
        session: &str,
        cursor: Option<Value>,
    ) -> Result<Value, String> {
        self.request(
            reqwest::Method::POST,
            &format!("/echo/tasks/{}/updates", path_id(task)?),
            Some(json!({"session": session, "cursor": cursor})),
        )
        .await
    }
    pub async fn stop_task(&self, task: &str) -> Result<Value, String> {
        self.request(
            reqwest::Method::POST,
            &format!("/echo/tasks/{}/stop", path_id(task)?),
            Some(json!({})),
        )
        .await
    }
    pub async fn stop_run(&self, run: &str) -> Result<Value, String> {
        let reply = self
            .request(
                reqwest::Method::POST,
                &format!("/native/v1/runs/{}/stop", path_id(run)?),
                Some(json!({})),
            )
            .await?;
        if matches!(
            reply["status"].as_str(),
            Some("stopping" | "cancelled" | "completed" | "failed" | "stopped" | "interrupted")
        ) {
            Ok(reply)
        } else {
            Err("Hermes did not confirm the run stop request.".into())
        }
    }
    pub async fn retry_task(&self, task: &str) -> Result<Value, String> {
        self.request(
            reqwest::Method::POST,
            &format!("/echo/tasks/{}/retry", path_id(task)?),
            Some(json!({})),
        )
        .await
    }
    pub async fn workspace_access(&self, task: &str, workspace: &str) -> Result<Value, String> {
        self.request(
            reqwest::Method::POST,
            &format!("/echo/tasks/{}/access", path_id(task)?),
            Some(json!({"workspace":workspace})),
        )
        .await
    }
    pub async fn worker_permissions(&self, task: &str) -> Result<Value, String> {
        self.request(
            reqwest::Method::GET,
            &format!("/echo/tasks/{}/permissions", path_id(task)?),
            None,
        )
        .await
    }
    pub async fn worker_model(&self, task: &str, model: &str) -> Result<Value, String> {
        self.request(
            reqwest::Method::POST,
            &format!("/echo/tasks/{}/model", path_id(task)?),
            Some(json!({"model":model})),
        )
        .await
    }
    pub async fn approval(&self, run: &str, response: Value) -> Result<Value, String> {
        self.request(
            reqwest::Method::POST,
            &format!("/native/v1/runs/{}/approval", path_id(run)?),
            Some(response),
        )
        .await
    }
    pub async fn history_blocks(
        &self,
        session: &str,
        cursor: Option<(u64, u64)>,
    ) -> Result<Value, String> {
        let (message, end) = cursor.unwrap_or_default();
        self.request(
            reqwest::Method::GET,
            &format!(
                "/echo/history/{}?message={message}&end={end}&limit=120",
                path_id(session)?
            ),
            None,
        )
        .await
    }
    pub async fn search_history(&self, session: &str, text: &str) -> Result<Value, String> {
        self.request(
            reqwest::Method::POST,
            &format!("/echo/history/{}/search", path_id(session)?),
            Some(json!({"text":text})),
        )
        .await
    }
    pub async fn history_stamps(&self, sessions: &[String]) -> Result<Value, String> {
        self.request(
            reqwest::Method::POST,
            "/echo/history-stamps",
            Some(json!({"sessions": sessions})),
        )
        .await
    }
    pub async fn completion_stamp(&self, session: &str, timestamp: f64) -> Result<Value, String> {
        self.request(
            reqwest::Method::GET,
            &format!(
                "/echo/history/{}/stamp?completed_before={timestamp}",
                path_id(session)?
            ),
            None,
        )
        .await
    }
    pub async fn export_history(
        &self,
        session: &str,
        path: &std::path::Path,
    ) -> Result<(), String> {
        use tokio::io::AsyncWriteExt;
        let response = self
            .http
            .get(format!(
                "{}/echo/history/{}/export",
                self.base,
                path_id(session)?
            ))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|_| "Hermes history export is disconnected.")?;
        if !response.status().is_success() {
            return Err("Hermes could not open this conversation for export.".into());
        }
        let pending = path.with_extension(format!("{}.pending", crate::pipelines::new_id()));
        let result = async {
            let file = tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&pending)
                .await
                .map_err(|_| "Could not create the history export file.")?;
            let mut pending_file = PendingExport {
                path: pending.clone(),
                file: Some(file),
            };
            let mut stream = response.bytes_stream();
            loop {
                let next = tokio::time::timeout(Duration::from_secs(60), stream.next())
                    .await
                    .map_err(|_| "History export timed out. The destination was not replaced.")?;
                let Some(chunk) = next else { break };
                let chunk = chunk.map_err(|_| {
                    "History export was interrupted. The destination was not replaced."
                })?;
                pending_file
                    .file
                    .as_mut()
                    .unwrap()
                    .write_all(&chunk)
                    .await
                    .map_err(|_| "Could not write the history export.")?;
            }
            pending_file
                .file
                .as_mut()
                .unwrap()
                .flush()
                .await
                .map_err(|_| "Could not finish the history export.")?;
            drop(pending_file.file.take());
            tokio::fs::rename(&pending, path)
                .await
                .map_err(|_| "Could not save the finished history export.")
        }
        .await;
        if result.is_err() {
            let _ = tokio::fs::remove_file(&pending).await;
        }
        result.map_err(str::to_owned)
    }
}

fn path_id(id: &str) -> Result<&str, String> {
    if !id.is_empty()
        && id.len() <= 256
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
    {
        Ok(id)
    } else {
        Err("Invalid native Hermes resource ID.".into())
    }
}

#[derive(Default)]
struct SseDecoder {
    pending: Vec<u8>,
    ending: u32,
    oversized: Option<String>,
}
impl SseDecoder {
    fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>, String> {
        let mut events = Vec::new();
        // Process each delimiter before accepting more: one large HTTP chunk
        // containing many small events must not count as one oversized event.
        for byte in bytes {
            self.ending = (self.ending << 8) | u32::from(*byte);
            if self.oversized.is_none() {
                self.pending.push(*byte);
                if self.pending.len() >= 256 * 1024 {
                    let prefix = String::from_utf8_lossy(&self.pending);
                    let data = prefix
                        .lines()
                        .find_map(|line| line.strip_prefix("data:"))
                        .ok_or("Oversized SSE event has no data.")?;
                    let (event, run_id) = event_identity(data)?;
                    if !matches!(
                        event.as_str(),
                        "run.completed" | "run.failed" | "run.stopped" | "run.cancelled"
                    ) {
                        return Err("Hermes event exceeded the bounded event budget. Its history remains available.".into());
                    }
                    self.oversized = Some(
                        json!({"event":event,"run_id":run_id,"history_required":true}).to_string(),
                    );
                    self.pending.clear();
                }
            }
            if self.ending & 0xffff == 0x0a0a || self.ending == 0x0d0a0d0a {
                if let Some(metadata) = self.oversized.take() {
                    events.push(metadata);
                    self.ending = 0;
                    continue;
                }
                let frame = std::str::from_utf8(&self.pending)
                    .map_err(|_| "Hermes event was not UTF-8.")?;
                let data = frame
                    .lines()
                    .filter_map(|line| {
                        line.strip_prefix("data:")
                            .map(|v| v.strip_prefix(' ').unwrap_or(v))
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                if !data.is_empty() && data != "[DONE]" {
                    events.push(data);
                }
                self.pending.clear();
                self.ending = 0;
            }
        }
        Ok(events)
    }
}

/// Native Hermes fixes envelope key order. Decode only the first two fields;
/// an oversized final output can stay in native history instead of UI memory.
fn event_identity(data: &str) -> Result<(String, String), String> {
    struct Identity<'a>(&'a mut Option<(String, String)>);
    impl<'de> serde::de::Visitor<'de> for Identity<'_> {
        type Value = ();
        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("native run event envelope")
        }
        fn visit_map<M: serde::de::MapAccess<'de>>(
            self,
            mut map: M,
        ) -> Result<Self::Value, M::Error> {
            let mut event = None;
            let mut run = None;
            while let Some(key) = map.next_key::<String>()? {
                match key.as_str() {
                    "event" => event = Some(map.next_value::<String>()?),
                    "run_id" => run = Some(map.next_value::<String>()?),
                    _ => {
                        let _ = map.next_value::<serde::de::IgnoredAny>()?;
                    }
                }
                if let (Some(event), Some(run)) = (&event, &run) {
                    *self.0 = Some((event.clone(), run.clone()));
                    // Stop before serde_json validates the enclosing object's
                    // closing brace; the bounded prefix deliberately lacks it.
                    return Err(serde::de::Error::custom("identity captured"));
                }
            }
            Err(serde::de::Error::custom("event identity missing"))
        }
    }
    let mut identity = None;
    let _ = serde::Deserializer::deserialize_map(
        &mut serde_json::Deserializer::from_str(data.trim_start()),
        Identity(&mut identity),
    );
    identity.ok_or_else(|| "Could not decode the native event identity.".into())
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn new_conversations_do_not_reuse_native_titles() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let mut titles = std::collections::HashSet::new();
            for number in 0..4 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0u8; 1024];
                let body = loop {
                    let count = socket.read(&mut buffer).await.unwrap();
                    assert!(count > 0);
                    bytes.extend_from_slice(&buffer[..count]);
                    if let Some(end) = bytes.windows(4).position(|value| value == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
                        assert!(headers.starts_with("post /native/api/sessions "));
                        let size: usize = headers
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length: "))
                            .unwrap()
                            .parse()
                            .unwrap();
                        if bytes.len() >= end + 4 + size {
                            break serde_json::from_slice::<Value>(&bytes[end + 4..end + 4 + size])
                                .unwrap();
                        }
                    }
                };
                assert_eq!(body["source"], "echo");
                let title = body["title"].as_str().unwrap();
                if number < 3 {
                    assert!(title.starts_with("Echo Assistant · echo_"));
                }
                assert!(
                    title.chars().count() <= 100,
                    "Hermes titles have a 100-character limit"
                );
                assert!(
                    titles.insert(title.to_owned()),
                    "Hermes rejects duplicate titles"
                );
                let response = json!({"session":{"id":body["id"]}}).to_string();
                socket.write_all(format!("HTTP/1.1 201 Created\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{response}",response.len()).as_bytes()).await.unwrap();
            }
        });
        let client = Client::new(&Config {
            bridge_port: port,
            token: "synthetic-test-token-not-a-secret-1234".into(),
            ..Default::default()
        })
        .unwrap();
        let mut tabs = crate::hermes_tabs::Tabs::default();
        for _ in 0..3 {
            tabs.open_conversation(client.create_session("Echo Assistant").await.unwrap());
        }
        assert_eq!(tabs.tabs.len(), 3);
        assert_eq!(tabs.selected().unwrap().name, "Chat 3");
        assert_eq!(
            tabs.tabs
                .iter()
                .map(|tab| &tab.session)
                .collect::<std::collections::HashSet<_>>()
                .len(),
            3
        );
        client.create_session(&"界".repeat(200)).await.unwrap();
        server.await.unwrap();
    }
    use super::*;
    #[tokio::test]
    async fn stopping_targets_native_run_and_requires_confirmation() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            for body in [r#"{"run_id":"run-1","status":"stopping"}"#, r#"{}"#] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = vec![0; 8192];
                let n = socket.read(&mut bytes).await.unwrap();
                let request = String::from_utf8_lossy(&bytes[..n]);
                assert!(request.starts_with("POST /native/v1/runs/run-1/stop "));
                assert!(request.to_lowercase().contains("authorization: bearer "));
                socket.write_all(format!("HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            }
        });
        let client = Client::new(&Config {
            bridge_port: port,
            token: "synthetic-test-token-not-a-secret-1234".into(),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            client.stop_run("run-1").await.unwrap()["status"],
            "stopping"
        );
        assert!(client.stop_run("run-1").await.is_err());
        assert!(client.stop_run("../different-run").await.is_err());
        server.await.unwrap();
    }
    #[tokio::test]
    async fn admission_retries_same_native_key_after_lost_response() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            for attempt in 0..2 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0u8; 1024];
                loop {
                    let count = socket.read(&mut buffer).await.unwrap();
                    assert!(count > 0);
                    bytes.extend_from_slice(&buffer[..count]);
                    if let Some(end) = bytes.windows(4).position(|value| value == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
                        let size: usize = headers
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length: "))
                            .unwrap()
                            .parse()
                            .unwrap();
                        if bytes.len() >= end + 4 + size {
                            assert!(headers.contains("idempotency-key: native-request-1"));
                            let body: Value = serde_json::from_slice(&bytes[end + 4..]).unwrap();
                            assert_eq!(body["input"], "Test input.");
                            break;
                        }
                    }
                }
                if attempt == 0 {
                    continue;
                } // Accepted remotely; response lost.
                let body = r#"{"run_id":"native-run-1"}"#;
                socket.write_all(format!("HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            }
        });
        let client = Client::new(&Config {
            bridge_port: port,
            token: "synthetic-test-token-not-a-secret-1234".into(),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            client
                .start_run("session-1", "Test input.", "native-request-1", None)
                .await
                .unwrap(),
            "native-run-1"
        );
        server.await.unwrap();
    }
    #[tokio::test]
    async fn event_reconnect_uses_native_cursor_without_duplicate_text() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            for attempt in 0..2 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = vec![0u8; 4096];
                let count = socket.read(&mut request).await.unwrap();
                let request = String::from_utf8_lossy(&request[..count]).to_lowercase();
                if attempt == 1 {
                    assert!(request.contains("last-event-id: 0"));
                }
                let delta = "data: {\"event\":\"message.delta\",\"run_id\":\"r1\",\"seq\":0,\"delta\":\"Hello.\"}\n\n";
                let body = if attempt == 0 {
                    delta.to_owned()
                } else {
                    format!("{delta}data: {{\"event\":\"run.completed\",\"run_id\":\"r1\",\"seq\":1}}\n\n")
                };
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            }
        });
        let client = Client::new(&Config {
            bridge_port: port,
            token: "synthetic-test-token-not-a-secret-1234".into(),
            ..Default::default()
        })
        .unwrap();
        let (sender, mut receiver) = tokio::sync::mpsc::channel(4);
        tokio::time::timeout(Duration::from_secs(10), client.events("r1", sender))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(receiver.recv().await.unwrap().delta, "Hello.");
        assert_eq!(receiver.recv().await.unwrap().event, "run.completed");
        assert!(receiver.recv().await.is_none());
        server.await.unwrap();
    }
    #[test]
    fn inherits_only_the_model_feeding_the_assistant_and_preserves_an_override() {
        let mut settings = crate::settings::AppSettings::default();
        let mut answer = crate::models::ConfiguredModel::new(
            "connection".into(),
            "Current assistant".into(),
            "answer".into(),
        );
        answer.id = "answer-id".into();
        answer.verified = true;
        let mut unrelated = answer.clone();
        unrelated.id = "unrelated-id".into();
        settings.model_registry.models = vec![unrelated, answer];
        let step = |id: &str| {
            crate::pipelines::Step::new(crate::pipelines::StepKind::Model {
                model_id: id.into(),
                name: id.into(),
                instructions: String::new(),
                max_tokens: 2048,
                temperature: None,
                timeout_seconds: 60,
            })
        };
        let answer = crate::graph::Node::new(
            crate::graph::Kind::Llm {
                step: step("answer-id"),
                history: true,
                tools: vec![],
                assistant_response: true,
            },
            0.,
            0.,
        );
        let unrelated =
            crate::graph::Node::new(crate::graph::Kind::Component(step("unrelated-id")), 0., 0.);
        let display = crate::graph::Node::new(crate::graph::Kind::AssistantDisplay, 0., 0.);
        let edge = crate::graph::Edge {
            from: answer.id.clone(),
            output: "text".into(),
            to: display.id.clone(),
            input: "text".into(),
        };
        settings.pipelines = Some(crate::pipelines::PipelineSet {
            version: 5,
            active_id: String::new(),
            active_assistant_id: "current".into(),
            pipelines: vec![crate::pipelines::Pipeline {
                id: "current".into(),
                name: "Current".into(),
                output: crate::pipelines::PipelineOutput::Assistant,
                steps: vec![],
                assistant: None,
                legacy_formatting: None,
                graph: Some(crate::graph::Graph {
                    nodes: vec![unrelated, answer, display],
                    edges: vec![edge],
                    hotkey: String::new(),
                    view: Default::default(),
                    legacy_formatting: None,
                }),
            }],
        });
        inherit_assistant_model(&mut settings);
        assert_eq!(settings.hermes.fast_model_id, "answer-id");
        // Older settings can point the assistant selector at the dictation
        // graph. Recover only when there is one unambiguous assistant graph.
        settings.hermes.fast_model_id.clear();
        let set = settings.pipelines.as_mut().unwrap();
        let mut dictation = set.pipelines[0].clone();
        dictation.id = "dictation".into();
        dictation.output = crate::pipelines::PipelineOutput::Transcription;
        dictation.graph.as_mut().unwrap().nodes.clear();
        set.active_id = "dictation".into();
        set.active_assistant_id = "dictation".into();
        set.pipelines.push(dictation);
        inherit_assistant_model(&mut settings);
        assert_eq!(settings.hermes.fast_model_id, "answer-id");
        settings.hermes.fast_model_id.clear();
        let set = settings.pipelines.as_mut().unwrap();
        let mut second = set.pipelines[0].clone();
        second.id = "another-assistant".into();
        set.pipelines.push(second);
        inherit_assistant_model(&mut settings);
        assert!(
            settings.hermes.fast_model_id.is_empty(),
            "An ambiguous model choice requires a selection"
        );
        settings.hermes.fast_model_id = "explicit".into();
        inherit_assistant_model(&mut settings);
        assert_eq!(settings.hermes.fast_model_id, "explicit");
    }
    #[test]
    fn sse_handles_utf8_split_crlf_comments_and_multiline_data() {
        let input = ": keepalive\r\n\r\ndata: 漢字\r\ndata: second\r\n\r\n";
        let mut decoder = SseDecoder::default();
        let mut events = Vec::new();
        for chunk in input.as_bytes().chunks(1) {
            events.extend(decoder.push(chunk).unwrap());
        }
        assert_eq!(events, ["漢字\nsecond"]);
        assert!(decoder.pending.is_empty());
    }
    #[test]
    fn resource_ids_cannot_escape_loopback_route() {
        for id in ["../config", "id?token=secret", "", "a/b"] {
            assert!(path_id(id).is_err());
        }
        assert!(path_id("run_123-a").is_ok());
    }
    #[test]
    fn config_debug_redacts_token() {
        let config = Config {
            token: "secret-auth-token".into(),
            ..Default::default()
        };
        assert!(!format!("{config:?}").contains("secret-auth-token"));
    }
    #[test]
    fn huge_final_output_keeps_completion_without_buffering_history() {
        let mut decoder = SseDecoder::default();
        assert!(decoder
            .push(b"data: {\"event\":\"run.completed\",\"run_id\":\"r1\",\"output\":\"")
            .unwrap()
            .is_empty());
        let chunk = vec![b'a'; 65536];
        for _ in 0..32 {
            assert!(decoder.push(&chunk).unwrap().is_empty());
            assert!(decoder.pending.len() <= 256 * 1024);
        }
        let events = decoder.push(b"\"}\n\n").unwrap();
        let event: RunEvent = serde_json::from_str(&events[0]).unwrap();
        assert_eq!(event.event, "run.completed");
        assert_eq!(event.run_id, "r1");
        assert!(event.output.is_empty());
        assert_eq!(event.extra["history_required"], true);
    }
}
