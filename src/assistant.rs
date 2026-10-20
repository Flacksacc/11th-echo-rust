//! Assistant conversation and routing. Saved pipelines live in `pipelines`;
//! the legacy assistant records below are read only for settings migration.
use crate::{pipelines::Pipeline, settings::AppSettings};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default)]
pub struct Conversation {
    pub messages: Vec<ChatMessage>,
    next_id: u64,
    generation: u64,
}

#[derive(Clone, Debug)]
pub struct ChatMessage {
    pub id: u64,
    pub text: String,
    pub assistant_text: String,
    pub files: Vec<crate::artifacts::Artifact>,
    pub from_user: bool,
    pub complete: bool,
    pub interrupted: bool,
}

impl Conversation {
    pub fn interrupt(&mut self) {
        self.generation += 1;
        for message in &mut self.messages {
            if !message.from_user && !message.complete {
                message.interrupted = true;
            }
        }
    }

    pub fn reset(&mut self) {
        self.messages.clear();
        self.generation += 1;
    }

    pub fn start_turn(&mut self, user_text: String) -> (u64, u64, String) {
        let context = self
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
        self.next_id += 1;
        let user_id = self.next_id;
        self.messages.push(ChatMessage {
            id: user_id,
            text: user_text,
            assistant_text: String::new(),
            files: vec![],
            from_user: true,
            complete: true,
            interrupted: false,
        });
        self.next_id += 1;
        let reply_id = self.next_id;
        self.messages.push(ChatMessage {
            id: reply_id,
            text: String::new(),
            assistant_text: String::new(),
            files: vec![],
            from_user: false,
            complete: false,
            interrupted: false,
        });
        (self.generation, reply_id, context)
    }

    pub fn update(&mut self, generation: u64, id: u64, text: String, complete: bool) {
        if self.generation != generation {
            return;
        }
        if let Some(message) = self.messages.iter_mut().find(|m| m.id == id) {
            message.text = text;
            message.complete = complete;
        }
    }
}

pub fn ready_spoken_sentences(text: &str, emitted: &mut usize) -> Vec<String> {
    if *emitted > text.len() || !text.is_char_boundary(*emitted) {
        *emitted = 0;
    }
    let mut ready = Vec::new();
    let start = *emitted;
    for (relative, ch) in text[start..].char_indices() {
        if !matches!(ch, '.' | '?' | '!' | '\n') {
            continue;
        }
        let end = start + relative + ch.len_utf8();
        let next = text[end..].chars().next();
        if next.is_some_and(|next| !next.is_whitespace()) {
            continue;
        }
        let sentence = text[*emitted..end].trim();
        if !sentence.is_empty() {
            ready.push(sentence.to_string());
        }
        *emitted = end;
    }
    ready
}

#[derive(Deserialize)]
#[serde(tag = "route", rename_all = "snake_case")]
enum Route {
    Direct { answer: String },
    Delegate,
}

/// Deliver text only at the connected chat and speech components.
pub async fn answer_with_outputs<F, S, A, Fut>(
    settings: &AppSettings,
    pipeline: &AssistantPipeline,
    context: &str,
    input: &str,
    mut on_text: F,
    mut on_speech: S,
    mut speech_stage: A,
) -> Result<String, String>
where
    F: FnMut(&str) + Send,
    S: FnMut(&str) + Send,
    A: FnMut(String) -> Fut + Send,
    Fut: std::future::Future<Output = Result<(), String>> + Send,
{
    use crate::pipelines::OutputStep;
    let output_streams = !pipeline
        .output_steps
        .iter()
        .any(|s| matches!(s, OutputStep::Processing(_)));
    let speech = pipeline
        .output_steps
        .iter()
        .any(|s| matches!(s, OutputStep::Speech));
    let (text, direct) = generate_answer(settings, pipeline, context, input, |direct, text| {
        let branch = if direct {
            &pipeline.direct_steps
        } else {
            &pipeline.delegated_steps
        };
        if output_streams && branch.is_empty() {
            if speech {
                on_speech(text);
            }
            on_text(text);
        }
    })
    .await?;
    let branch = if direct {
        &pipeline.direct_steps
    } else {
        &pipeline.delegated_steps
    };
    if output_streams && branch.is_empty() {
        return Ok(text);
    }
    deliver_processed_answer(
        settings,
        branch,
        &pipeline.output_steps,
        text,
        &mut on_text,
        &mut speech_stage,
    )
    .await
}
async fn deliver_processed_answer<F, A, Fut>(
    settings: &AppSettings,
    branch: &[crate::pipelines::Step],
    output: &[crate::pipelines::OutputStep],
    mut text: String,
    on_text: &mut F,
    speech_stage: &mut A,
) -> Result<String, String>
where
    F: FnMut(&str) + Send,
    A: FnMut(String) -> Fut + Send,
    Fut: std::future::Future<Output = Result<(), String>> + Send,
{
    use crate::pipelines::OutputStep;
    for step in branch {
        text = process_text(settings, step, &text).await?;
    }
    for step in output {
        match step {
            OutputStep::Processing(step) => text = process_text(settings, step, &text).await?,
            OutputStep::Speech => speech_stage(text.clone())
                .await
                .map_err(|error| format!("Speech generation: {error}"))?,
            OutputStep::Chat => on_text(&text),
        }
    }
    Ok(text)
}
async fn process_text(
    settings: &AppSettings,
    step: &crate::pipelines::Step,
    input: &str,
) -> Result<String, String> {
    let text = crate::pipelines::run_step(step, input, settings)
        .await
        .map_err(|error| format!("{}: {error}", step.label()))?;
    if text.trim().is_empty() {
        return Err(format!("{} returned empty text.", step.label()));
    }
    Ok(text)
}
#[cfg(test)]
pub async fn answer<F>(
    settings: &AppSettings,
    pipeline: &AssistantPipeline,
    context: &str,
    input: &str,
    on_text: F,
) -> Result<String, String>
where
    F: FnMut(&str) + Send,
{
    answer_with_outputs(
        settings,
        pipeline,
        context,
        input,
        on_text,
        |_| {},
        |_| async { Ok(()) },
    )
    .await
}
async fn generate_answer<F>(
    settings: &AppSettings,
    pipeline: &AssistantPipeline,
    context: &str,
    input: &str,
    mut on_text: F,
) -> Result<(String, bool), String>
where
    F: FnMut(bool, &str) + Send,
{
    let mut prepared = input.to_owned();
    for step in &pipeline.processing_steps {
        prepared = crate::pipelines::run_step(step, &prepared, settings).await?;
        if prepared.trim().is_empty() {
            return Err("Assistant input component returned empty text.".into());
        }
    }
    let input = prepared.as_str();
    if settings
        .model_registry
        .is_decision(&pipeline.router_model_id)
    {
        let (model, connection) = settings.model_registry.find(&pipeline.router_model_id)?;
        if !model.verified {
            return Err("Test the decision model before use.".into());
        }
        // Keep the entire latest request. Oversized inputs or server failures take the stronger branch.
        // Laya's small context is for routing only; the answering model still receives all history.
        let direct = if input.chars().count() <= 1200 {
            let history: String = context
                .chars()
                .rev()
                .take(400)
                .collect::<String>()
                .chars()
                .rev()
                .collect();
            let state = format!("Conversation context: {history}\nLatest user request: {input}");
            crate::system_one::decide(
                connection,
                &model.model,
                &model.instructions,
                &state,
                model.timeout_seconds,
            )
            .await
            .is_ok_and(|decision| decision.choice == "direct" && decision.confidence >= 0.8)
        } else {
            false
        };
        let answer_id = if direct {
            &pipeline.fast_answer_model_id
        } else {
            &pipeline.answer_model_id
        };
        let prompt = format!("Conversation so far:\n{context}\n\nUser: {input}\nAssistant:");
        let text = crate::models::stream(
            settings,
            answer_id,
            &pipeline.instructions,
            &prompt,
            |text| on_text(direct, text),
        )
        .await?;
        return Ok((text, direct));
    }
    let router_prompt = format!("Assistant instructions:\n{}\n\nConversation so far:\n{context}\n\nLatest user message:\n{input}\n\nDecide if this can be answered quickly and accurately. Return only JSON: {{\"route\":\"direct\",\"answer\":\"your response\"}} or {{\"route\":\"delegate\"}}. Delegate when uncertain, when tools are needed, or when careful reasoning is required.", pipeline.instructions);
    let router_text = crate::models::complete(
        settings,
        &pipeline.router_model_id,
        "You route assistant messages. Return strict JSON only.",
        512,
        Some(0.0),
        30,
        &router_prompt,
    )
    .await?;
    let route = serde_json::from_str::<Route>(router_text.trim()).unwrap_or(Route::Delegate);
    match route {
        Route::Direct { answer } if !answer.trim().is_empty() => {
            on_text(true, &answer);
            Ok((answer, true))
        }
        _ => {
            let prompt = format!("Conversation so far:\n{context}\n\nUser: {input}\nAssistant:");
            let text = crate::models::stream(
                settings,
                &pipeline.answer_model_id,
                &pipeline.instructions,
                &prompt,
                |text| on_text(false, text),
            )
            .await?;
            Ok((text, false))
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AssistantPipelineSet {
    pub version: u32,
    #[serde(default)]
    pub active_id: String,
    #[serde(default)]
    pub pipelines: Vec<AssistantPipeline>,
}

impl Default for AssistantPipelineSet {
    fn default() -> Self {
        Self {
            version: 1,
            active_id: String::new(),
            pipelines: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AssistantPipeline {
    pub id: String,
    pub name: String,
    pub speech_pipeline_id: String,
    pub router_model_id: String,
    pub answer_model_id: String,
    #[serde(default)]
    pub fast_answer_model_id: String,
    #[serde(default)]
    pub instructions: String,
    #[serde(default)]
    pub voice_id: String,
    #[serde(default)]
    pub enabled_tools: Vec<String>,
    #[serde(skip)]
    pub processing_steps: Vec<crate::pipelines::Step>,
    #[serde(default)]
    pub direct_steps: Vec<crate::pipelines::Step>,
    #[serde(default)]
    pub delegated_steps: Vec<crate::pipelines::Step>,
    #[serde(default)]
    pub output_steps: Vec<crate::pipelines::OutputStep>,
}

impl AssistantPipelineSet {
    pub fn is_empty(&self) -> bool {
        self.pipelines.is_empty()
    }
}

impl AssistantPipeline {
    pub fn from_pipeline(pipeline: &Pipeline) -> Result<Self, String> {
        if pipeline.output != crate::pipelines::PipelineOutput::Assistant {
            return Err("Select an assistant pipeline.".into());
        }
        let config = pipeline
            .assistant
            .as_ref()
            .ok_or("Configure the assistant output.")?;
        Ok(Self {
            id: pipeline.id.clone(),
            name: pipeline.name.clone(),
            speech_pipeline_id: pipeline.id.clone(),
            router_model_id: config.router_model_id.clone(),
            answer_model_id: config.answer_model_id.clone(),
            fast_answer_model_id: config.fast_answer_model_id.clone(),
            instructions: config.instructions.clone(),
            voice_id: config.voice_id.clone(),
            enabled_tools: config.enabled_tools.clone(),
            processing_steps: pipeline.steps.iter().skip(1).cloned().collect(),
            direct_steps: config.direct_steps.clone(),
            delegated_steps: config.delegated_steps.clone(),
            output_steps: config.effective_output_steps(),
        })
    }
    pub fn speech_input(&self, settings: &AppSettings) -> Result<Pipeline, String> {
        let speech = settings
            .pipelines
            .as_ref()
            .and_then(|set| {
                set.pipelines
                    .iter()
                    .find(|p| p.id == self.speech_pipeline_id)
            })
            .ok_or("Select a speech pipeline for the assistant.")?;
        let step = speech
            .steps
            .first()
            .cloned()
            .ok_or("Speech pipeline has no transcription component.")?;
        let input = Pipeline {
            id: speech.id.clone(),
            name: speech.name.clone(),
            output: crate::pipelines::PipelineOutput::Transcription,
            steps: vec![step],
            assistant: None,
            legacy_formatting: None,
            graph: None,
        };
        input.preflight(settings)?;
        Ok(input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn replace(from: &str, to: &str) -> crate::pipelines::Step {
        let mut config =
            crate::pipelines::formatting_only(crate::settings::PostProcessingSettings::default());
        config.custom_replacements = vec![format!("{from} => {to}")];
        crate::pipelines::Step::new(crate::pipelines::StepKind::Formatting(config))
    }

    #[tokio::test]
    async fn output_connections_deliver_speech_input_and_downstream_chat_text() {
        use crate::pipelines::OutputStep;
        let output = vec![
            OutputStep::Processing(replace("branch", "spoken")),
            OutputStep::Speech,
            OutputStep::Processing(replace("spoken", "displayed")),
            OutputStep::Chat,
        ];
        let mut chat = Vec::new();
        let spoken = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = spoken.clone();
        let text = deliver_processed_answer(
            &AppSettings::default(),
            &[replace("answer", "branch")],
            &output,
            "answer".into(),
            &mut |text| chat.push(text.to_string()),
            &mut move |text| {
                captured.lock().unwrap().push(text);
                async { Ok(()) }
            },
        )
        .await
        .unwrap();
        assert_eq!(*spoken.lock().unwrap(), vec!["spoken"]);
        assert_eq!(chat, vec!["displayed"]);
        assert_eq!(text, "displayed");
    }

    #[tokio::test]
    async fn processor_or_speech_failure_stops_downstream_delivery() {
        use crate::pipelines::OutputStep;
        let settings = AppSettings::default();
        let missing = crate::pipelines::Step::new(crate::pipelines::StepKind::Model {
            model_id: "missing".into(),
            name: "Broken processor".into(),
            instructions: String::new(),
            max_tokens: 100,
            temperature: None,
            timeout_seconds: 5,
        });
        let mut displayed = false;
        let mut spoken = false;
        let error = deliver_processed_answer(
            &settings,
            &[],
            &[
                OutputStep::Processing(missing),
                OutputStep::Speech,
                OutputStep::Chat,
            ],
            "answer".into(),
            &mut |_| displayed = true,
            &mut |_| {
                spoken = true;
                async { Ok(()) }
            },
        )
        .await
        .unwrap_err();
        assert!(error.contains("Broken processor"));
        assert!(!spoken);
        assert!(!displayed);
        let error = deliver_processed_answer(
            &settings,
            &[],
            &[OutputStep::Speech, OutputStep::Chat],
            "answer".into(),
            &mut |_| displayed = true,
            &mut |_| async { Err("device unavailable".into()) },
        )
        .await
        .unwrap_err();
        assert!(error.contains("Speech generation"));
        assert!(!displayed);
    }

    #[tokio::test]
    async fn cancellation_at_speech_prevents_downstream_chat() {
        use crate::pipelines::OutputStep;
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };
        let started = Arc::new(tokio::sync::Notify::new());
        let displayed = Arc::new(AtomicBool::new(false));
        let signal = started.clone();
        let chat = displayed.clone();
        let task = tokio::spawn(async move {
            deliver_processed_answer(
                &AppSettings::default(),
                &[],
                &[OutputStep::Speech, OutputStep::Chat],
                "answer".into(),
                &mut |_| {
                    chat.store(true, Ordering::SeqCst);
                },
                &mut move |_| {
                    signal.notify_one();
                    std::future::pending::<Result<(), String>>()
                },
            )
            .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), started.notified())
            .await
            .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(!displayed.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn decision_router_streams_selected_branch_and_falls_back_on_failure() {
        use crate::models::{ConfiguredModel, ModelConnection};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for processed in [false, true] {
            for (decision_body, status, expected) in [
                (
                    r#"{"answers":{"route":{"type":"choice","choice":"direct","confidence":0.9,"probabilities":{"direct":0.95,"delegate":0.05}}}}"#,
                    200,
                    "fast",
                ),
                (
                    r#"{"answers":{"route":{"type":"choice","choice":"direct","confidence":0.2,"probabilities":{"direct":0.6,"delegate":0.4}}}}"#,
                    200,
                    "strong",
                ),
                (
                    r#"{"answers":{"route":{"type":"choice","choice":"delegate","confidence":0.9,"probabilities":{"direct":0.05,"delegate":0.95}}}}"#,
                    200,
                    "strong",
                ),
                ("{}", 503, "strong"),
            ] {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let address = listener.local_addr().unwrap();
                let server = tokio::spawn(async move {
                    for turn in 0..2 {
                        let (mut socket, _) = listener.accept().await.unwrap();
                        let mut data = Vec::new();
                        loop {
                            let mut buf = [0; 4096];
                            let count = socket.read(&mut buf).await.unwrap();
                            assert!(count > 0);
                            data.extend_from_slice(&buf[..count]);
                            if let Some(header_end) = data.windows(4).position(|w| w == b"\r\n\r\n")
                            {
                                let headers =
                                    String::from_utf8_lossy(&data[..header_end]).to_lowercase();
                                let length: usize = headers
                                    .lines()
                                    .find_map(|l| {
                                        l.strip_prefix("content-length:")
                                            .map(|n| n.trim().parse().unwrap())
                                    })
                                    .unwrap();
                                if data.len() >= header_end + 4 + length {
                                    break;
                                }
                            }
                        }
                        let request = String::from_utf8(data).unwrap();
                        let (code, content_type, body) = if turn == 0 {
                            assert!(request.starts_with("POST /v1/systemone "));
                            assert!(request.contains("latest request"));
                            (status, "application/json", decision_body.to_owned())
                        } else {
                            assert!(request.starts_with("POST /v1/chat/completions "));
                            let json: serde_json::Value =
                                serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1)
                                    .unwrap();
                            assert_eq!(json["model"], expected);
                            assert!(request.contains("entire older conversation"));
                            (200, "text/event-stream", format!("data: {{\"id\":\"test\",\"object\":\"chat.completion.chunk\",\"created\":0,\"model\":\"{expected}\",\"choices\":[{{\"index\":0,\"delta\":{{\"role\":\"assistant\",\"content\":\"Answer\"}},\"finish_reason\":null}}]}}\n\ndata: [DONE]\n\n"))
                        };
                        socket.write_all(format!("HTTP/1.1 {code} OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
                    }
                });
                let mut settings = AppSettings::default();
                for provider in ["ollaya", "openai_compatible"] {
                    settings.model_registry.connections.push(ModelConnection {
                        id: provider.into(),
                        provider: provider.into(),
                        endpoint: format!(
                            "http://{address}{}",
                            if provider == "ollaya" { "" } else { "/v1" }
                        ),
                        api_version: String::new(),
                        custom_header_name: String::new(),
                        custom_header_value: String::new(),
                        api_key: String::new(),
                        allow_lan_http: false,
                    });
                }
                for (id, provider) in [
                    ("router", "ollaya"),
                    ("fast", "openai_compatible"),
                    ("strong", "openai_compatible"),
                ] {
                    let mut model = ConfiguredModel::new(provider.into(), id.into(), id.into());
                    model.id = id.into();
                    model.verified = true;
                    settings.model_registry.models.push(model);
                }
                let mut pipeline = AssistantPipeline {
                    id: "test".into(),
                    name: "test".into(),
                    speech_pipeline_id: "speech".into(),
                    router_model_id: "router".into(),
                    answer_model_id: "strong".into(),
                    fast_answer_model_id: "fast".into(),
                    instructions: "Answer concisely".into(),
                    voice_id: String::new(),
                    enabled_tools: vec![],
                    processing_steps: vec![],
                    direct_steps: vec![],
                    delegated_steps: vec![],
                    output_steps: vec![crate::pipelines::OutputStep::Chat],
                };
                if processed {
                    pipeline.direct_steps = vec![replace("Answer", "direct")];
                    pipeline.delegated_steps = vec![replace("Answer", "delegated")];
                    let mut final_step = crate::pipelines::formatting_only(
                        crate::settings::PostProcessingSettings::default(),
                    );
                    final_step.custom_replacements = vec![
                        "direct => final direct".into(),
                        "delegated => final delegated".into(),
                    ];
                    pipeline.output_steps = vec![
                        crate::pipelines::OutputStep::Processing(crate::pipelines::Step::new(
                            crate::pipelines::StepKind::Formatting(final_step),
                        )),
                        crate::pipelines::OutputStep::Chat,
                    ];
                }
                let mut displayed = String::new();
                let text = tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    answer(
                        &settings,
                        &pipeline,
                        "entire older conversation",
                        "latest request",
                        |text| displayed = text.into(),
                    ),
                )
                .await
                .unwrap()
                .unwrap();
                server.await.unwrap();
                let expected = if processed {
                    if expected == "fast" {
                        "final direct"
                    } else {
                        "final delegated"
                    }
                } else {
                    "Answer"
                };
                assert_eq!(text, expected);
                assert_eq!(displayed, expected);
            }
        }
    }

    #[test]
    fn empty_set_is_safe_and_requires_configuration() {
        let set = AssistantPipelineSet::default();
        assert!(set.is_empty());
    }

    #[test]
    fn overlapping_replies_keep_their_turns_and_new_chat_ignores_old_updates() {
        let mut conversation = Conversation::default();
        let (generation, first, first_context) = conversation.start_turn("First question".into());
        let (_, second, second_context) = conversation.start_turn("Second question".into());
        assert!(first_context.is_empty());
        assert!(second_context.contains("First question"));
        assert!(!second_context.contains("Thinking"));
        conversation.update(generation, second, "Second answer".into(), true);
        conversation.update(generation, first, "First answer".into(), true);
        assert_eq!(conversation.messages[1].text, "First answer");
        assert_eq!(conversation.messages[3].text, "Second answer");
        conversation.reset();
        conversation.update(generation, first, "Late answer".into(), true);
        assert!(conversation.messages.is_empty());
    }

    #[test]
    fn interruption_preserves_partial_reply_and_rejects_late_updates() {
        let mut conversation = Conversation::default();
        let (generation, complete, _) = conversation.start_turn("Earlier question".into());
        conversation.update(generation, complete, "Earlier answer".into(), true);
        let (generation, partial, _) = conversation.start_turn("Old request".into());
        conversation.update(generation, partial, "Partial answer".into(), false);
        conversation.interrupt();
        conversation.update(generation, partial, "Late final answer".into(), true);
        assert_eq!(conversation.messages[3].text, "Partial answer");
        assert!(conversation.messages[3].interrupted);
        assert!(!conversation.messages[1].interrupted);
        let (new_generation, reply, context) = conversation.start_turn("New request".into());
        assert!(context.contains("Earlier answer"));
        assert!(!context.contains("Partial answer"));
        assert!(!context.contains("Late final answer"));
        conversation.update(new_generation, reply, "New answer".into(), true);
        assert_eq!(conversation.messages.last().unwrap().text, "New answer");
    }

    #[test]
    fn spoken_sentences_emit_once_and_leave_partial_tail() {
        let mut emitted = 0;
        assert!(ready_spoken_sentences("Hello, wor", &mut emitted).is_empty());
        assert_eq!(
            ready_spoken_sentences("Hello, world. Next", &mut emitted),
            ["Hello, world."]
        );
        assert_eq!(emitted, "Hello, world.".len());
        assert_eq!(
            ready_spoken_sentences("Hello, world. Next question?", &mut emitted),
            ["Next question?"]
        );
    }
}
