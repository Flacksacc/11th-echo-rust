//! Typed inline controls for all graph configuration fields.
use crate::{
    graph::{Kind, Node},
    settings::AppSettings,
    GraphSettingRow,
};
use serde_json::Value;
use slint::{ModelRc, VecModel};
thread_local! { static CONTEXT: std::cell::RefCell<AppSettings> = std::cell::RefCell::new(AppSettings::default()); }
pub fn set_context(settings: &AppSettings) {
    CONTEXT.with(|context| {
        let mut context = context.borrow_mut();
        context.model_registry.models = settings.model_registry.models.clone();
        context.assistant_mcp_servers = settings.assistant_mcp_servers.clone();
    });
}
pub fn rows_for(node: &Node, graph: &crate::graph::Graph) -> ModelRc<GraphSettingRow> {
    let mut fields = CONTEXT.with(|context| fields(node, &context.borrow()));
    for (port, key, _) in text_inputs(node) {
        if let Some(field) = fields.iter_mut().find(|field| field.key == key) {
            field.wired = graph
                .edges
                .iter()
                .any(|edge| edge.to == node.id && edge.input == port);
        }
    }
    ModelRc::new(VecModel::from(fields))
}

pub fn text_inputs(node: &Node) -> Vec<(String, String, String)> {
    fields(node, &AppSettings::default())
        .into_iter()
        .filter(|field| field.kind == 3)
        .map(|field| {
            let key = field.key.to_string();
            let port = if matches!(node.kind, Kind::Llm { .. } | Kind::Agent { .. })
                && key.ends_with("/instructions")
            {
                "instructions".into()
            } else {
                format!("setting:{key}")
            };
            (port, key, field.label.to_string())
        })
        .collect()
}

fn strings(values: Vec<String>) -> ModelRc<slint::SharedString> {
    ModelRc::new(VecModel::from(
        values.into_iter().map(Into::into).collect::<Vec<_>>(),
    ))
}
pub fn voice_choices(current: &str, follow: bool) -> Vec<(String, String)> {
    let mut choices = vec![];
    if follow {
        choices.push((
            "Use settings default".into(),
            crate::voices::FOLLOW_DEFAULT.into(),
        ));
    }
    choices.push(("Alba (standard)".into(), String::new()));
    if let Ok(voices) = crate::voices::list() {
        choices.extend(
            voices
                .into_iter()
                .map(|v| (v.label(), v.path().to_string_lossy().into_owned())),
        );
    }
    if !choices.iter().any(|(_, path)| path == current) {
        choices.push((format!("Existing voice: {current}"), current.into()));
    }
    choices
}
fn label(key: &str) -> String {
    match key {
        "preload" | "preload_on_start" => "Preload at startup",
        "history" => "Conversation history",
        "assistant_response" => "Assistant response",
        "max_tool_calls" => "Tool-call limit",
        "run_timeout_seconds" => "Run timeout (s)",
        "tool_timeout_seconds" => "Tool timeout (s)",
        "num_threads" => "CPU threads",
        "no_verbatim" => "Clean filler words",
        "max_tokens" => "Output token limit",
        "temperature" => "Temperature",
        "timeout_seconds" => "Timeout (s)",
        "custom_replacements" => "Replacements",
        "protected_phrases" => "Protected phrases",
        "model_id" => "Model",
        "voice" => "Voice",
        "input_count" => "Number of inputs (1–64)",
        "vad_threshold" => "Speech detection threshold (0.1–0.9)",
        "silence_ms" => "Silence before finalizing (ms)",
        "partial_interval_ms" => "Preview interval (ms)",
        "min_speech_ms" => "Minimum speech (ms)",
        "max_segment_seconds" => "Maximum segment (seconds)",
        "pre_roll_ms" => "Pre-roll (ms)",
        "post_roll_ms" => "Post-roll (ms)",
        "redecode_full_session" => "Re-decode whole session after Stop",
        "provider" => "Transcription provider",
        "threshold" => "Minimum routing confidence (0–1)",
        "fallback" => "Fallback route",
        "separator" => "Separator",
        "instructions" => "Instructions",
        _ => {
            let text = key.replace('_', " ");
            return format!("{}{}", text[..1].to_uppercase(), &text[1..]);
        }
    }
    .into()
}
pub fn fields(node: &Node, settings: &AppSettings) -> Vec<GraphSettingRow> {
    if matches!(
        node.kind,
        Kind::TextOutput | Kind::AssistantDisplay | Kind::Text { .. }
    ) {
        return vec![];
    }
    let value = serde_json::to_value(&node.kind).unwrap();
    let mut fields = Vec::new();
    fn visit(
        value: &Value,
        path: &str,
        node: &Node,
        settings: &AppSettings,
        fields: &mut Vec<GraphSettingRow>,
    ) {
        if let Value::Object(object) = value {
            for (key, value) in object {
                if key == "local" && node.step().is_some_and(|step|matches!(&step.kind,crate::pipelines::StepKind::Transcription{provider,..} if provider != crate::transcription::LOCAL_SHERPA_PROVIDER_ID)) {continue;}
                // Identifiers and wiring are managed by the graph, not text fields.
                if matches!(
                    key.as_str(),
                    "node_kind" | "kind" | "id" | "name" | "options" | "inputs" | "tools"
                ) {
                    continue;
                }
                if path.ends_with("/local") && key != "model" && node.step().is_some_and(|s| matches!(&s.kind, crate::pipelines::StepKind::Transcription { local, .. } if local.model.uses_gpu())) { continue; }
                if let Some(s) = node.step() {
                    if matches!(&s.kind, crate::pipelines::StepKind::Transcription {provider, ..} if provider == "local_sherpa_onnx")
                        && !path.ends_with("/local")
                        && matches!(key.as_str(), "model" | "language" | "no_verbatim")
                    {
                        continue;
                    }
                    if matches!(&s.kind, crate::pipelines::StepKind::Formatting(_))
                        && matches!(
                            key.as_str(),
                            "punctuation"
                                | "capitalization"
                                | "commas"
                                | "periods"
                                | "question_marks"
                        )
                    {
                        continue;
                    }
                    if matches!(&s.kind, crate::pipelines::StepKind::Punctuation(_))
                        && !matches!(
                            key.as_str(),
                            "config"
                                | "preload_on_start"
                                | "punctuation"
                                | "capitalization"
                                | "commas"
                                | "periods"
                                | "question_marks"
                                | "protected_phrases"
                                | "enabled"
                        )
                    {
                        continue;
                    }
                }
                visit(value, &format!("{path}/{key}"), node, settings, fields);
            }
            return;
        }
        let key = path.rsplit('/').next().unwrap_or("");
        let mut choices: Vec<(String, String)> = match key {
            "voice" => voice_choices(value.as_str().unwrap_or(""), true),
            "model_id" => settings
                .model_registry
                .models
                .iter()
                .filter(|m| m.verified)
                .map(|m| (crate::assistant_choice_label(&m.name, &m.id), m.id.clone()))
                .collect(),
            "model" if path.contains("/local/") => crate::transcription::LocalModel::ALL
                .into_iter()
                .map(|model| {
                    (
                        model.label().into(),
                        serde_json::to_value(model)
                            .unwrap()
                            .as_str()
                            .unwrap()
                            .into(),
                    )
                })
                .collect(),
            "provider" => [
                crate::transcription::TranscriptionProvider::LocalSherpaOnnx,
                crate::transcription::TranscriptionProvider::ElevenLabsRealtime,
                crate::transcription::TranscriptionProvider::OpenAiRealtimeWhisper,
            ]
            .into_iter()
            .map(|provider| (provider.label().into(), provider.id().into()))
            .collect(),
            "preset" => [
                "Minimal corrections",
                "Sound like a pirate",
                "Sound like a medieval knight",
                "Custom",
            ]
            .into_iter()
            .map(|s| (s.into(), s.into()))
            .collect(),
            "fallback" => {
                let mut choices = vec![("No fallback".into(), String::new())];
                if let Kind::Router { options, .. } = &node.kind {
                    choices.extend(
                        options
                            .iter()
                            .enumerate()
                            .map(|(i, id)| (format!("Option {}", i + 1), id.clone())),
                    );
                }
                choices
            }
            _ => vec![],
        };
        let raw = if let Value::String(s) = value {
            s.clone()
        } else if value.is_null() {
            String::new()
        } else if let Value::Array(values) = value {
            values
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            value.to_string()
        };
        if !choices.is_empty() && !choices.iter().any(|(_, id)| id == &raw) {
            choices.push((format!("Unavailable: {raw}"), raw.clone()));
        }
        let display = choices
            .iter()
            .find(|(_, id)| id == &raw)
            .map_or_else(|| raw.clone(), |(name, _)| name.clone());
        fields.push(GraphSettingRow {
            key: path.into(),
            label: label(key).into(),
            value: display.into(),
            checked: value.as_bool().unwrap_or(false),
            wired: false,
            kind: if key == "voice" {
                4
            } else if value.is_boolean() || key == "preload_on_start" {
                1
            } else if !choices.is_empty() {
                2
            } else if value.is_array()
                || matches!(key, "instructions" | "prompt" | "template" | "separator")
            {
                3
            } else {
                0
            },
            choices: strings(choices.iter().map(|(name, _)| name.clone()).collect()),
            choice_values: strings(choices.into_iter().map(|(_, id)| id).collect()),
        });
    }
    visit(
        value.get("node_config").unwrap_or(&Value::Null),
        "/node_config",
        node,
        settings,
        &mut fields,
    );
    if let Kind::Combine { inputs, .. } = &node.kind {
        fields.insert(
            0,
            GraphSettingRow {
                key: "input_count".into(),
                label: label("input_count").into(),
                value: inputs.len().to_string().into(),
                ..Default::default()
            },
        );
    }
    if node.step().is_some_and(|s| s.preload_on_start.is_none()) {
        let key = if matches!(node.kind, Kind::Llm { .. } | Kind::Agent { .. }) {
            "/node_config/step/preload_on_start"
        } else {
            "/node_config/preload_on_start"
        };
        fields.push(GraphSettingRow {
            key: key.into(),
            label: label("preload").into(),
            kind: 1,
            ..Default::default()
        });
    }
    if let Kind::Agent { tools, .. } = &node.kind {
        let mut choices: Vec<_> = crate::agent_tools::builtin_tools()
            .into_iter()
            .map(|t| (format!("builtin::{}", t.name), t.name.replace('_', " ")))
            .collect();
        for server in &settings.assistant_mcp_servers {
            for tool in &server.discovered_tools {
                choices.push((
                    crate::agent_tools::identity(&server.id, &tool.name),
                    format!("{} / {}", server.name, tool.name),
                ));
            }
        }
        for id in tools {
            if !choices.iter().any(|(key, _)| key == id) {
                choices.push((id.clone(), format!("Unavailable: {id}")));
            }
        }
        for (id, name) in choices {
            fields.push(GraphSettingRow {
                key: format!("tool:{id}").into(),
                label: name.into(),
                kind: 1,
                checked: tools.contains(&id),
                ..Default::default()
            });
        }
    }
    fields.sort_by_cached_key(|field| {
        let key = field.key.rsplit('/').next().unwrap_or("");
        let rank = if matches!(
            key,
            "voice" | "provider" | "model" | "model_id" | "input_count"
        ) {
            0
        } else if matches!(key, "preload" | "preload_on_start" | "history") {
            1
        } else if matches!(key, "instructions" | "template" | "separator") {
            2
        } else if field.kind == 1 {
            3
        } else if field.kind == 3 {
            5
        } else {
            4
        };
        (rank, field.key.to_string())
    });
    fields
}
pub fn edit(node: &mut Node, key: &str, text: &str, settings: &AppSettings) -> Result<(), String> {
    use slint::Model;
    let field = fields(node, settings)
        .into_iter()
        .find(|f| f.key == key)
        .ok_or("Unknown node setting.")?;
    if key == "input_count" {
        let count = text
            .parse::<usize>()
            .map_err(|_| "Enter an input count from 1 to 64.")?;
        if !(1..=64).contains(&count) {
            return Err("Enter an input count from 1 to 64.".into());
        }
        if let Kind::Combine { inputs, .. } = &mut node.kind {
            inputs.resize_with(count, crate::pipelines::new_id);
        }
        return Ok(());
    }
    if let Some(id) = key.strip_prefix("tool:") {
        if let Kind::Agent { tools, .. } = &mut node.kind {
            if text == "true" {
                if !tools.iter().any(|t| t == id) {
                    tools.push(id.into());
                }
            } else {
                tools.retain(|t| t != id);
            }
            return Ok(());
        }
    }
    let mut value = serde_json::to_value(&node.kind).unwrap();
    let replacement = match field.kind {
        1 => Value::Bool(text == "true"),
        2 | 4 => {
            let index = field
                .choices
                .iter()
                .position(|choice| choice == text)
                .ok_or("Choose an available option.")?;
            let id = field.choice_values.row_data(index).unwrap().to_string();
            if key.ends_with("/fallback") && id.is_empty() {
                Value::Null
            } else {
                Value::String(id)
            }
        }
        3 if value.pointer(key).is_some_and(Value::is_array) => Value::Array(
            text.lines()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| Value::String(s.into()))
                .collect(),
        ),
        _ if value.pointer(key).is_some_and(Value::is_number) || key.ends_with("/temperature") => {
            if text.trim().is_empty() && key.ends_with("/temperature") {
                Value::Null
            } else {
                serde_json::from_str::<Value>(text)
                    .ok()
                    .filter(Value::is_number)
                    .ok_or("Enter a valid number.")?
            }
        }
        _ => Value::String(text.into()),
    };
    if key.ends_with("/preload_on_start") && value.pointer(key).is_none() {
        let (parent, _) = key.rsplit_once('/').unwrap();
        value
            .pointer_mut(parent)
            .and_then(Value::as_object_mut)
            .ok_or("Invalid node setting.")?
            .insert("preload_on_start".into(), replacement);
    } else {
        *value.pointer_mut(key).ok_or("Invalid node setting.")? = replacement;
    }
    let mut kind: Kind =
        serde_json::from_value(value).map_err(|_| "That value is invalid for this setting.")?;
    if matches!(&kind,Kind::Router{threshold,..} if !threshold.is_finite() || !(0.0..=1.0).contains(threshold))
    {
        return Err("Routing confidence must be between 0 and 1.".into());
    }
    if let Kind::Component(step) | Kind::Llm { step, .. } | Kind::Agent { step, .. } = &mut kind {
        match &mut step.kind {
            crate::pipelines::StepKind::Transcription {
                provider,
                model,
                local,
                ..
            } => {
                *local = local.clone().normalized();
                if provider == crate::transcription::LOCAL_SHERPA_PROVIDER_ID {
                    *model = local.model.id().into();
                } else if key.ends_with("/provider") {
                    *model = crate::transcription::TranscriptionProvider::from_id(provider)
                        .default_model_id()
                        .into();
                }
            }
            crate::pipelines::StepKind::Model {
                model_id,
                name,
                max_tokens,
                temperature,
                timeout_seconds,
                ..
            } => {
                if *max_tokens == 0
                    || !(5..=600).contains(timeout_seconds)
                    || temperature.is_some_and(|t| !t.is_finite() || !(0.0..=2.0).contains(&t))
                {
                    return Err("Use a positive token limit, a 5–600 second timeout, and temperature between 0 and 2.".into());
                }
                if key.ends_with("/model_id") {
                    if let Some(model) = settings
                        .model_registry
                        .models
                        .iter()
                        .find(|m| &m.id == model_id)
                    {
                        *name = model.name.clone();
                    }
                }
            }
            _ => {}
        }
    }
    node.kind = kind;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipelines::{Step, StepKind};
    #[test]
    fn every_node_family_has_typed_inline_fields() {
        let settings = AppSettings::default();
        assert!(fields(&Node::new(Kind::TextOutput, 0., 0.), &settings).is_empty());
        assert!(fields(&Node::new(Kind::AssistantDisplay, 0., 0.), &settings).is_empty());
        let nodes = [
            Kind::Component(Step::new(StepKind::Transcription {
                provider: crate::transcription::LOCAL_SHERPA_PROVIDER_ID.into(),
                model: String::new(),
                language: "en".into(),
                no_verbatim: true,
                local: Default::default(),
            })),
            Kind::Component(Step::new(StepKind::Formatting(Default::default()))),
            Kind::Component(Step::new(StepKind::Punctuation(Default::default()))),
            Kind::Component(Step::new(StepKind::Gemini {
                model: "gemini".into(),
                preset: "Custom".into(),
                prompt: "Rewrite".into(),
            })),
            Kind::Llm {
                step: Step::new(StepKind::Model {
                    model_id: "model".into(),
                    name: "Model".into(),
                    instructions: "Answer".into(),
                    max_tokens: 2000,
                    temperature: None,
                    timeout_seconds: 30,
                }),
                history: true,
                tools: vec![],
                assistant_response: false,
            },
            Kind::Combine {
                inputs: vec!["a".into(), "b".into()],
                separator: "\n".into(),
            },
            Kind::Template {
                template: "{text}".into(),
            },
            Kind::Router {
                model_id: "model".into(),
                options: vec!["a".into(), "b".into()],
                fallback: None,
                threshold: 0.5,
                preload: false,
            },
            Kind::Speech {
                voice: crate::voices::FOLLOW_DEFAULT.into(),
                preload: false,
            },
        ];
        for kind in nodes {
            let node = Node::new(kind, 0., 0.);
            let rows = fields(&node, &settings);
            assert!(!rows.is_empty(), "{}", node.title());
            for row in rows {
                assert!(!row.key.ends_with("/id"));
                let text = if row.kind == 1 {
                    row.checked.to_string()
                } else {
                    row.value.to_string()
                };
                edit(&mut node.clone(), &row.key, &text, &settings).unwrap();
            }
        }
    }
    #[test]
    fn voice_overrides_and_invalid_generation_edits_preserve_settings() {
        let settings = AppSettings::default();
        let mut speech = Node::new(
            Kind::Speech {
                voice: crate::voices::FOLLOW_DEFAULT.into(),
                preload: false,
            },
            0.,
            0.,
        );
        edit(
            &mut speech,
            "/node_config/voice",
            "Alba (standard)",
            &settings,
        )
        .unwrap();
        assert!(matches!(&speech.kind,Kind::Speech{voice,..} if voice.is_empty()));
        edit(
            &mut speech,
            "/node_config/voice",
            "Use settings default",
            &settings,
        )
        .unwrap();
        assert!(
            matches!(&speech.kind,Kind::Speech{voice,..} if voice == crate::voices::FOLLOW_DEFAULT)
        );
        let mut llm = Node::new(
            Kind::Llm {
                step: Step::new(StepKind::Model {
                    model_id: "model".into(),
                    name: "Model".into(),
                    instructions: String::new(),
                    max_tokens: 2000,
                    temperature: None,
                    timeout_seconds: 30,
                }),
                history: false,
                tools: vec![],
                assistant_response: false,
            },
            0.,
            0.,
        );
        let old = llm.clone();
        assert!(edit(
            &mut llm,
            "/node_config/step/config/timeout_seconds",
            "0",
            &settings
        )
        .is_err());
        assert_eq!(old, llm);
        edit(
            &mut llm,
            "/node_config/step/config/temperature",
            "0.5",
            &settings,
        )
        .unwrap();
        edit(
            &mut llm,
            "/node_config/step/config/temperature",
            "",
            &settings,
        )
        .unwrap();
    }
    #[test]
    fn ultra_keeps_model_choice_and_hides_cpu_segmentation() {
        let settings = AppSettings::default();
        let mut node = Node::new(
            Kind::Component(Step::new(StepKind::Transcription {
                provider: crate::transcription::LOCAL_SHERPA_PROVIDER_ID.into(),
                model: String::new(),
                language: "en".into(),
                no_verbatim: true,
                local: Default::default(),
            })),
            0.,
            0.,
        );
        edit(
            &mut node,
            "/node_config/config/local/model",
            crate::transcription::LocalModel::ParakeetUltra.label(),
            &settings,
        )
        .unwrap();
        let rows = fields(&node, &settings);
        assert!(rows.iter().any(|row| row.key.ends_with("/local/model")));
        assert!(!rows.iter().any(|row| row.key.ends_with("/num_threads")));
    }
}
