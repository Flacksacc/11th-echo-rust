//! User-configured processing chains. Credentials are deliberately not stored in steps.
use crate::{
    gemini, post_processing,
    settings::{AppSettings, PostProcessingSettings},
    transcription::{LocalSherpaConfig, TranscriptionConfig, TranscriptionProvider},
};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};

pub fn new_id() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "{:x}-{:x}-{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PipelineSet {
    pub version: u32,
    pub active_id: String,
    #[serde(default)]
    pub active_assistant_id: String,
    pub pipelines: Vec<Pipeline>,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PipelineOutput {
    #[default]
    Transcription,
    Assistant,
}
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Pipeline {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub output: PipelineOutput,
    #[serde(default)]
    pub steps: Vec<Step>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assistant: Option<AssistantConfig>,
    /// An unchanged migrated pair uses the legacy formatter's original token
    /// projection, preserving replacement casing and punctuation exactly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_formatting: Option<LegacyFormatting>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph: Option<crate::graph::Graph>,
}
impl Serialize for Pipeline {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        if let Some(graph) = &self.graph {
            let mut state = serializer.serialize_struct("Pipeline", 3)?;
            state.serialize_field("id", &self.id)?;
            state.serialize_field("name", &self.name)?;
            state.serialize_field("graph", graph)?;
            state.end()
        } else {
            let mut state = serializer.serialize_struct("Pipeline", 6)?;
            state.serialize_field("id", &self.id)?;
            state.serialize_field("name", &self.name)?;
            state.serialize_field("output", &self.output)?;
            state.serialize_field("steps", &self.steps)?;
            state.serialize_field("assistant", &self.assistant)?;
            state.serialize_field("legacy_formatting", &self.legacy_formatting)?;
            state.end()
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AssistantConfig {
    #[serde(default)]
    pub direct_steps: Vec<Step>,
    #[serde(default)]
    pub delegated_steps: Vec<Step>,
    /// None preserves legacy chat/voice delivery; Some is an explicit ordered chain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_steps: Option<Vec<OutputStep>>,
    #[serde(default)]
    pub router_model_id: String,
    #[serde(default)]
    pub answer_model_id: String,
    /// Text model for the direct branch when the router is a decision-only model.
    #[serde(default)]
    pub fast_answer_model_id: String,
    #[serde(default)]
    pub instructions: String,
    #[serde(default)]
    pub voice_id: String,
    #[serde(default)]
    pub enabled_tools: Vec<String>,
    #[serde(default)]
    pub preload_router_on_start: bool,
    #[serde(default)]
    pub preload_answer_on_start: bool,
    #[serde(default)]
    pub preload_voice_on_start: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "config", rename_all = "snake_case")]
pub enum OutputStep {
    Processing(Step),
    Speech,
    Chat,
}
impl AssistantConfig {
    pub fn effective_output_steps(&self) -> Vec<OutputStep> {
        self.output_steps.clone().unwrap_or_else(|| {
            let mut steps = Vec::new();
            if !self.voice_id.is_empty() {
                steps.push(OutputStep::Speech);
            }
            steps.push(OutputStep::Chat);
            steps
        })
    }
    pub fn processing_steps(&self) -> impl Iterator<Item = &Step> {
        self.direct_steps.iter().chain(&self.delegated_steps).chain(
            self.output_steps
                .iter()
                .flatten()
                .filter_map(|step| match step {
                    OutputStep::Processing(step) => Some(step),
                    _ => None,
                }),
        )
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LegacyFormatting {
    pub formatting_id: Option<String>,
    pub punctuation_id: String,
    pub settings: PostProcessingSettings,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Step {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preload_on_start: Option<bool>,
    #[serde(flatten)]
    pub kind: StepKind,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "config", rename_all = "snake_case")]
pub enum StepKind {
    Transcription {
        provider: String,
        model: String,
        language: String,
        no_verbatim: bool,
        local: LocalSherpaConfig,
    },
    Gemini {
        model: String,
        preset: String,
        prompt: String,
    },
    Model {
        model_id: String,
        name: String,
        instructions: String,
        max_tokens: u64,
        temperature: Option<f32>,
        timeout_seconds: u64,
    },
    Formatting(PostProcessingSettings),
    Punctuation(PostProcessingSettings),
}
impl Step {
    pub fn new(kind: StepKind) -> Self {
        let preload_on_start = matches!(
            &kind,
            StepKind::Transcription { provider, .. } if provider == "local_sherpa_onnx"
        )
        .then_some(false);
        Self {
            id: new_id(),
            preload_on_start,
            kind,
        }
    }
    pub fn preload_on_start(&self) -> bool {
        self.preload_on_start.unwrap_or(false)
    }
    pub fn label(&self) -> String {
        match &self.kind {
            StepKind::Transcription {
                provider, local, ..
            } => match TranscriptionProvider::from_id(provider) {
                TranscriptionProvider::LocalSherpaOnnx => match local.model {
                    crate::transcription::LocalModel::ParakeetUltra => "Parakeet Ultra",
                    crate::transcription::LocalModel::ParakeetTdtV2 => "Parakeet v2",
                    crate::transcription::LocalModel::ParakeetTdtV3 => "Parakeet v3",
                }
                .into(),
                p => p.label().into(),
            },
            StepKind::Gemini { preset, .. } => format!(
                "Gemini · {}",
                match preset.as_str() {
                    "Minimal corrections" => "Grammar",
                    "Sound like a pirate" => "Pirate",
                    "Sound like a medieval knight" => "Knight",
                    "Custom" => "Custom",
                    _ => preset,
                }
            ),
            StepKind::Model { name, .. } => name.clone(),
            StepKind::Formatting(_) => "Local formatting".into(),
            StepKind::Punctuation(_) => "Punctuation & casing".into(),
        }
    }
    pub fn summary(&self) -> String {
        match &self.kind {
            StepKind::Transcription {
                model, language, ..
            } => format!("{model} · {language}"),
            StepKind::Gemini { model, preset, .. } => format!("{preset} · {model}"),
            StepKind::Model { instructions, .. } => instructions.chars().take(80).collect(),
            StepKind::Punctuation(s) => [
                (s.punctuation && s.commas, "commas"),
                (s.punctuation && s.periods, "periods"),
                (s.punctuation && s.question_marks, "questions"),
                (s.capitalization, "capitalization"),
            ]
            .into_iter()
            .filter_map(|(on, label)| on.then_some(label))
            .collect::<Vec<_>>()
            .join(", "),
            StepKind::Formatting(s) => [
                (s.format_numbers, "numbers"),
                (s.money || s.measurements, "quantities"),
                (s.dates || s.times, "dates/times"),
                (s.urls_emails || s.telephone_alphanumeric, "addresses/IDs"),
                (!s.custom_replacements.is_empty(), "replacements"),
            ]
            .into_iter()
            .filter_map(|(on, label)| on.then_some(label))
            .collect::<Vec<_>>()
            .join(", "),
        }
    }
    pub fn is_transcription(&self) -> bool {
        matches!(self.kind, StepKind::Transcription { .. })
    }
    pub fn local_settings(&self) -> Option<&PostProcessingSettings> {
        match &self.kind {
            StepKind::Formatting(s) | StepKind::Punctuation(s) => Some(s),
            _ => None,
        }
    }
}

/// Preserve the old startup preload only for the active dictation speech model.
pub fn upgrade_preload_defaults(set: &mut PipelineSet) {
    for pipeline in &mut set.pipelines {
        for speech in &mut pipeline.steps {
            if speech.preload_on_start.is_none()
                && matches!(&speech.kind, StepKind::Transcription { provider, .. } if provider == "local_sherpa_onnx")
            {
                speech.preload_on_start = Some(
                    pipeline.id == set.active_id
                        && pipeline.output == PipelineOutput::Transcription,
                );
            }
        }
    }
}
impl PipelineSet {
    pub fn active(&self) -> Result<&Pipeline, String> {
        if !(1..=5).contains(&self.version) {
            return Err("Unsupported pipeline configuration version.".into());
        }
        self.pipelines
            .iter()
            .find(|p| {
                p.id == self.active_id
                    && (p.graph.is_some() || p.output == PipelineOutput::Transcription)
            })
            .ok_or_else(|| "Select an active pipeline in Settings → Pipelines.".into())
    }
    pub fn active_assistant(&self) -> Result<&Pipeline, String> {
        if !(1..=5).contains(&self.version) {
            return Err("Unsupported pipeline configuration version.".into());
        }
        self.pipelines
            .iter()
            .find(|p| {
                p.id == self.active_assistant_id
                    && (p.graph.is_some() || p.output == PipelineOutput::Assistant)
            })
            .ok_or_else(|| "Select an active assistant pipeline in Settings → Pipelines.".into())
    }
    /// Older unified selectors could accidentally select a dictation graph for
    /// assistant input. Recover only an unambiguous assistant, never dictation.
    pub fn assistant_for_runtime(&self) -> Result<&Pipeline, String> {
        if !(1..=5).contains(&self.version) {
            return self.active_assistant();
        }
        let is_assistant = |pipeline: &&Pipeline| pipeline.is_assistant();
        if let Some(pipeline) = self
            .active_assistant()
            .ok()
            .filter(is_assistant)
            .or_else(|| self.active().ok().filter(is_assistant))
        {
            return Ok(pipeline);
        }
        let mut choices = self.pipelines.iter().filter(is_assistant);
        match (choices.next(), choices.next()) {
            (Some(pipeline), None) => Ok(pipeline),
            _ => Err("Select an assistant pipeline in Settings → Pipelines.".into()),
        }
    }
    pub fn activate(&mut self, id: &str) -> Result<(), String> {
        let pipeline = self
            .pipelines
            .iter()
            .find(|pipeline| pipeline.id == id)
            .ok_or("Pipeline no longer exists.")?;
        if pipeline.graph.is_some() {
            self.active_id = id.into();
            if pipeline.is_assistant() {
                self.active_assistant_id = id.into();
            }
        } else if pipeline.is_assistant() {
            self.active_assistant_id = id.into();
        } else {
            self.active_id = id.into();
        }
        Ok(())
    }
    pub fn validate(&self) -> Result<(), String> {
        if !self.pipelines.is_empty() {
            self.active()?;
        }
        let mut hotkeys = std::collections::HashSet::new();
        for p in &self.pipelines {
            if let Some(g) = &p.graph {
                if !g.hotkey.trim().is_empty() {
                    #[cfg(windows)]
                    let key = crate::parse_hotkey(&g.hotkey)?.id().to_string();
                    #[cfg(not(windows))]
                    let key = g.hotkey.replace(" ", "").to_lowercase();
                    if !hotkeys.insert(key) {
                        return Err("Each graph hotkey must be unique.".into());
                    }
                }
            }
        }
        let mut ids = std::collections::HashSet::new();
        for p in &self.pipelines {
            if !ids.insert(&p.id) || p.id.is_empty() {
                return Err("Pipeline IDs must be unique.".into());
            }
            p.validate()?;
        }
        Ok(())
    }
}
impl Pipeline {
    pub fn is_assistant(&self) -> bool {
        self.graph
            .as_ref()
            .map_or(self.output == PipelineOutput::Assistant, |graph| {
                graph
                    .nodes
                    .iter()
                    .any(|node| matches!(node.kind, crate::graph::Kind::AssistantDisplay))
            })
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("Give the pipeline a name.".into());
        }
        if let Some(g) = &self.graph {
            return g.validate();
        }
        self.validate_components()?;
        if self.output == PipelineOutput::Assistant && self.assistant.is_none() {
            return Err("Configure the assistant output.".into());
        }
        if let Some(config) = self.assistant.as_ref() {
            if config.router_model_id.is_empty() || config.answer_model_id.is_empty() {
                return Err("Choose two tested assistant models.".into());
            }
        }
        if self.output == PipelineOutput::Transcription && self.assistant.is_some() {
            return Err("Transcription pipelines cannot contain assistant branches.".into());
        }
        Ok(())
    }
    pub fn validate_components(&self) -> Result<(), String> {
        if let Some(g) = &self.graph {
            return g.validate();
        }
        if !self.steps.first().is_some_and(Step::is_transcription)
            || self.steps.iter().skip(1).any(Step::is_transcription)
        {
            return Err("Exactly one speech component must be first.".into());
        }
        if let Some(config) = &self.assistant {
            let output = config.effective_output_steps();
            if !matches!(output.last(), Some(OutputStep::Chat))
                || output
                    .iter()
                    .filter(|s| matches!(s, OutputStep::Chat))
                    .count()
                    != 1
            {
                return Err("Assistant chat must be the final output component.".into());
            }
            if output
                .iter()
                .filter(|s| matches!(s, OutputStep::Speech))
                .count()
                > 1
            {
                return Err("Only one speech-generation component is allowed.".into());
            }
            if config.processing_steps().any(Step::is_transcription) {
                return Err(
                    "Answer and output stages accept text processors, not transcription.".into(),
                );
            }
        }
        let mut ids = std::collections::HashSet::new();
        for step in self.steps.iter().chain(
            self.assistant
                .iter()
                .flat_map(AssistantConfig::processing_steps),
        ) {
            if step.id.is_empty() || !ids.insert(&step.id) {
                return Err("Step IDs must be unique.".into());
            }
            if let Some(s) = step.local_settings() {
                post_processing::validate(s)?;
            }
            if let StepKind::Transcription {
                provider,
                model,
                language,
                ..
            } = &step.kind
            {
                if ![
                    "elevenlabs_realtime",
                    "openai_realtime_whisper",
                    "local_sherpa_onnx",
                ]
                .contains(&provider.as_str())
                {
                    return Err("Unknown speech provider.".into());
                }
                if model.trim().is_empty() || language.trim().is_empty() {
                    return Err("Speech model and language are required.".into());
                }
            }
            if let StepKind::Gemini { model, .. } = &step.kind {
                if model.trim().is_empty() {
                    return Err("Gemini needs a model.".into());
                }
            }
            if let StepKind::Model {
                model_id,
                max_tokens,
                timeout_seconds,
                ..
            } = &step.kind
            {
                if model_id.is_empty() || *max_tokens == 0 || !(5..=600).contains(timeout_seconds) {
                    return Err(
                        "Model component needs a model and valid generation settings.".into(),
                    );
                }
            }
        }
        Ok(())
    }
    pub fn transcription_config(
        &self,
        shared: &AppSettings,
    ) -> Result<TranscriptionConfig, String> {
        self.validate()?;
        let speech = self
            .graph
            .as_ref()
            .and_then(|g| g.transcription())
            .or_else(|| self.steps.first())
            .ok_or("Missing transcription node.")?;
        let StepKind::Transcription {
            provider,
            model,
            language,
            no_verbatim,
            local,
        } = &speech.kind
        else {
            unreachable!()
        };
        let provider = TranscriptionProvider::from_id(provider);
        Ok(TranscriptionConfig {
            provider,
            api_key: match provider {
                TranscriptionProvider::ElevenLabsRealtime => {
                    if shared.elevenlabs_api_key.is_empty() {
                        shared.api_key.clone()
                    } else {
                        shared.elevenlabs_api_key.clone()
                    }
                }
                TranscriptionProvider::OpenAiRealtimeWhisper => shared.openai_api_key.clone(),
                _ => String::new(),
            },
            model_id: model.clone(),
            language_code: language.clone(),
            no_verbatim: *no_verbatim,
            local: local.clone(),
        })
    }
    pub fn preflight(&self, shared: &AppSettings) -> Result<(), String> {
        self.validate()?;
        let config = self.transcription_config(shared)?;
        if let Some(g) = &self.graph {
            g.preflight(shared)?;
            if config.provider != TranscriptionProvider::LocalSherpaOnnx
                && config.api_key.trim().is_empty()
            {
                return Err("Configure the speech API key in Settings > Connections.".into());
            }
            return Ok(());
        }
        if let Some(assistant) = self.assistant.as_ref() {
            if assistant
                .effective_output_steps()
                .iter()
                .any(|s| matches!(s, OutputStep::Speech))
            {
                if !crate::assistant_voice::voice_selection_valid(std::path::Path::new(
                    &assistant.voice_id,
                )) {
                    return Err(
                        "The selected WAV sample is missing. Select another sample or use the standard voice.".into(),
                    );
                }
                if !crate::assistant_voice::installed() {
                    return Err("Install Pocket TTS in the speech-generation component.".into());
                }
            }
            let mut ids = vec![&assistant.router_model_id, &assistant.answer_model_id];
            if shared
                .model_registry
                .is_decision(&assistant.answer_model_id)
            {
                return Err("The answer branch requires a text model.".into());
            }
            if shared
                .model_registry
                .is_decision(&assistant.router_model_id)
            {
                if assistant.fast_answer_model_id.is_empty()
                    || shared
                        .model_registry
                        .is_decision(&assistant.fast_answer_model_id)
                {
                    return Err("Select a tested text model for the direct answer branch.".into());
                }
                ids.push(&assistant.fast_answer_model_id);
            }
            for id in ids {
                let (model, _) = shared.model_registry.find(id)?;
                if !model.verified {
                    return Err("Test both assistant models before use.".into());
                }
            }
        }
        if config.provider != TranscriptionProvider::LocalSherpaOnnx
            && config.api_key.trim().is_empty()
        {
            return Err("Configure the speech API key in Settings > Connections.".into());
        }
        for step in self.steps.iter().chain(
            self.assistant
                .iter()
                .flat_map(AssistantConfig::processing_steps),
        ) {
            match &step.kind {
                StepKind::Gemini { .. } if shared.gemini_api_key.trim().is_empty() => {
                    return Err("Configure the Gemini API key in Settings > Connections.".into())
                }
                StepKind::Model { model_id, .. } => {
                    let (model, connection) = shared.model_registry.find(model_id)?;
                    if connection.provider == crate::system_one::PROVIDER {
                        return Err("Decision models can only be used as assistant routers.".into());
                    }
                    if !model.verified
                        || !["ollama", "openai_compatible"].contains(&connection.provider.as_str())
                            && connection.api_key.is_empty()
                    {
                        return Err(format!(
                            "Configure and test {} in Settings > Connections.",
                            model.name
                        ));
                    }
                }
                StepKind::Punctuation(s)
                    if s.punctuation && !post_processing::punctuation_model_available() =>
                {
                    return Err(
                        "Install the punctuation model in Settings > Punctuation & casing.".into(),
                    )
                }
                _ => {}
            }
        }
        Ok(())
    }
}

pub fn formatting_only(mut s: PostProcessingSettings) -> PostProcessingSettings {
    s.enabled = true;
    s.punctuation = false;
    s.capitalization = false;
    s
}
pub fn punctuation_only(mut s: PostProcessingSettings) -> PostProcessingSettings {
    s.enabled = true;
    s.format_numbers = false;
    s.money = false;
    s.measurements = false;
    s.dates = false;
    s.times = false;
    s.telephone_alphanumeric = false;
    s.urls_emails = false;
    s.custom_replacements.clear();
    s
}
pub fn migrate(shared: &AppSettings) -> PipelineSet {
    let config = shared.transcription_config();
    let mut steps = vec![Step::new(StepKind::Transcription {
        provider: config.provider.id().into(),
        model: config.model_id,
        language: config.language_code,
        no_verbatim: config.no_verbatim,
        local: config.local,
    })];
    if let Some(speech) = steps.first_mut() {
        speech.preload_on_start = Some(
            matches!(&speech.kind, StepKind::Transcription { provider, .. } if provider == "local_sherpa_onnx"),
        );
    }
    if shared.gemini_enabled {
        steps.push(Step::new(StepKind::Gemini {
            model: if shared.gemini_model.trim().is_empty() {
                AppSettings::default().gemini_model
            } else {
                shared.gemini_model.clone()
            },
            preset: shared.gemini_prompt_preset.clone(),
            prompt: shared.gemini_custom_prompt.clone(),
        }));
    }
    let mut legacy_formatting = None;
    if shared.post_processing.enabled {
        let s = &shared.post_processing;
        let format = Step::new(StepKind::Formatting(formatting_only(s.clone())));
        let format_id = format.id.clone();
        steps.push(format);
        if s.punctuation || s.capitalization {
            let punct = Step::new(StepKind::Punctuation(punctuation_only(s.clone())));
            legacy_formatting = Some(LegacyFormatting {
                formatting_id: Some(format_id),
                punctuation_id: punct.id.clone(),
                settings: s.clone(),
            });
            steps.push(punct);
        }
    }
    let p = Pipeline {
        id: new_id(),
        name: "Current setup".into(),
        output: PipelineOutput::Transcription,
        steps,
        assistant: None,
        legacy_formatting,
        graph: None,
    };
    PipelineSet {
        version: 3,
        active_id: p.id.clone(),
        active_assistant_id: String::new(),
        pipelines: vec![p],
    }
}

/// Import the old assistant collection into the canonical pipeline set once.
/// Keep legacy settings untouched until the caller has persisted the result.
pub fn import_assistants(shared: &AppSettings) -> Result<Option<PipelineSet>, String> {
    if shared.assistant_pipelines.pipelines.is_empty() {
        return Ok(None);
    }
    let mut set = shared
        .pipelines
        .clone()
        .ok_or("Pipeline configuration is missing.")?;
    let mut active_id = shared.assistant_pipelines.active_id.clone();
    for old in &shared.assistant_pipelines.pipelines {
        if set
            .pipelines
            .iter()
            .any(|p| p.id == old.id && p.output == PipelineOutput::Assistant)
        {
            continue;
        }
        let id = if set.pipelines.iter().any(|p| p.id == old.id) {
            new_id()
        } else {
            old.id.clone()
        };
        if old.id == active_id {
            active_id = id.clone();
        }
        let mut speech = set
            .pipelines
            .iter()
            .find(|p| p.id == old.speech_pipeline_id)
            .and_then(|p| p.steps.first())
            .filter(|s| s.is_transcription())
            .cloned()
            .ok_or_else(|| {
                format!(
                    "Assistant pipeline '{}' has no speech source to import.",
                    old.name
                )
            })?;
        speech.preload_on_start = Some(false);
        set.pipelines.push(Pipeline {
            id,
            name: old.name.clone(),
            output: PipelineOutput::Assistant,
            steps: vec![speech],
            assistant: Some(AssistantConfig {
                router_model_id: old.router_model_id.clone(),
                answer_model_id: old.answer_model_id.clone(),
                fast_answer_model_id: String::new(),
                instructions: old.instructions.clone(),
                voice_id: old.voice_id.clone(),
                enabled_tools: old.enabled_tools.clone(),
                preload_router_on_start: false,
                preload_answer_on_start: false,
                preload_voice_on_start: false,
                ..Default::default()
            }),
            legacy_formatting: None,
            graph: None,
        });
    }
    set.active_assistant_id = active_id;
    set.version = set.version.max(3);
    Ok(Some(set))
}

pub fn upgrade_models(settings: &mut AppSettings) {
    if settings
        .pipelines
        .as_ref()
        .is_some_and(|set| set.version > 4)
    {
        return;
    }
    let mut registry = settings.model_registry.clone();
    let has_legacy_step = settings.pipelines.as_ref().is_some_and(|set| {
        set.pipelines
            .iter()
            .flat_map(|p| &p.steps)
            .any(|s| matches!(s.kind, StepKind::Gemini { .. }))
    });
    if registry.version == 0 || has_legacy_step {
        registry.import_gemini(settings);
    }
    if let Some(set) = &mut settings.pipelines {
        for step in set.pipelines.iter_mut().flat_map(|p| &mut p.steps) {
            if let StepKind::Gemini {
                model,
                preset,
                prompt,
            } = &step.kind
            {
                if let Some(found) = registry.models.iter().find(|m| {
                    m.model == *model
                        && registry
                            .connections
                            .iter()
                            .any(|c| c.id == m.connection_id && c.provider == "gemini")
                }) {
                    let instructions = gemini::instructions(preset, prompt);
                    step.kind = StepKind::Model {
                        model_id: found.id.clone(),
                        name: found.name.clone(),
                        instructions,
                        max_tokens: 512,
                        temperature: Some(0.4),
                        timeout_seconds: 30,
                    };
                }
            }
        }
        set.version = set.version.max(3);
    }
    registry.version = 1;
    settings.model_registry = registry;
}

/// Progress is emitted before/after each step. Timing belongs to the caller so
/// every rendered result can remain visible without delaying the UI thread.
pub async fn run_step(step: &Step, input: &str, shared: &AppSettings) -> Result<String, String> {
    match &step.kind {
        StepKind::Model {
            model_id,
            instructions,
            max_tokens,
            temperature,
            timeout_seconds,
            ..
        } => {
            crate::models::complete(
                shared,
                model_id,
                instructions,
                *max_tokens,
                *temperature,
                *timeout_seconds,
                input,
            )
            .await
        }
        StepKind::Gemini { .. } => {
            Err("Upgrade this Gemini component in Settings > Connections.".into())
        }
        StepKind::Formatting(s) | StepKind::Punctuation(s) => {
            let text = input.to_owned();
            let settings = s.clone();
            tokio::task::spawn_blocking(move || {
                let result = post_processing::process(&text, &settings);
                match result.warning {
                    Some(e) => Err(e),
                    None => Ok(result.text),
                }
            })
            .await
            .map_err(|_| "Formatting worker failed.".to_string())?
        }
        StepKind::Transcription { .. } => Err("Speech components can only run first.".into()),
    }
}

pub struct RunFailure {
    pub step: usize,
    pub message: String,
    pub text: String,
}

pub async fn execute(
    pipeline: &Pipeline,
    input: String,
    shared: &AppSettings,
    mut progress: impl FnMut(usize, &str, bool),
    cancelled: impl Fn() -> bool,
) -> Result<String, RunFailure> {
    let mut text = input;
    let mut legacy_input = None;
    let minimum = std::time::Duration::from_millis(100);
    progress(0, &text, true);
    tokio::time::sleep(minimum).await;
    for (index, step) in pipeline.steps.iter().enumerate().skip(1) {
        if cancelled() {
            return Err(RunFailure {
                step: index,
                message: "Recording cancelled.".into(),
                text,
            });
        }
        progress(index, &text, false);
        if pipeline
            .legacy_formatting
            .as_ref()
            .is_some_and(|l| l.formatting_id.as_ref() == Some(&step.id))
        {
            legacy_input = Some(text.clone());
        }
        let operation = async {
            if let Some(legacy) = pipeline
                .legacy_formatting
                .as_ref()
                .filter(|l| l.punctuation_id == step.id)
            {
                let settings = legacy.settings.clone();
                let source = legacy_input.clone().unwrap_or_else(|| text.clone());
                run_step(
                    &Step {
                        id: step.id.clone(),
                        preload_on_start: None,
                        kind: StepKind::Formatting(settings),
                    },
                    &source,
                    shared,
                )
                .await
            } else {
                run_step(step, &text, shared).await
            }
        };
        let seconds = match &step.kind {
            StepKind::Model {
                timeout_seconds, ..
            } => (*timeout_seconds).clamp(5, 600),
            StepKind::Gemini { .. } => 30,
            _ => 60,
        };
        let result = tokio::select! {
            result = tokio::time::timeout(std::time::Duration::from_secs(seconds), operation) =>
                result.unwrap_or_else(|_| Err("Component timed out.".into())),
            _ = async {
                while !cancelled() { tokio::time::sleep(std::time::Duration::from_millis(100)).await; }
            } => Err("Recording cancelled.".into()),
        };
        if cancelled() {
            return Err(RunFailure {
                step: index,
                message: "Recording cancelled.".into(),
                text,
            });
        }
        match result {
            Ok(output) if !output.trim().is_empty() => {
                text = output;
                progress(index, &text, true);
                tokio::time::sleep(minimum).await;
            }
            Ok(_) => {
                return Err(RunFailure {
                    step: index,
                    message: "Component returned empty text.".into(),
                    text,
                })
            }
            Err(message) => {
                return Err(RunFailure {
                    step: index,
                    message,
                    text,
                })
            }
        }
    }
    if cancelled() {
        return Err(RunFailure {
            step: pipeline.steps.len().saturating_sub(1),
            message: "Recording cancelled.".into(),
            text,
        });
    }
    Ok(text)
}

#[cfg(test)]
mod model_upgrade_tests {
    use super::*;

    #[test]
    fn assistant_import_uses_one_pipeline_set_and_copies_speech() {
        let mut settings = AppSettings::default();
        let original = migrate(&settings);
        let speech = original.pipelines[0].steps[0].clone();
        let dictation_id = original.active_id.clone();
        settings.pipelines = Some(original);
        settings
            .assistant_pipelines
            .pipelines
            .push(crate::assistant::AssistantPipeline {
                id: "old-assistant".into(),
                name: "My assistant".into(),
                speech_pipeline_id: dictation_id,
                router_model_id: "fast".into(),
                answer_model_id: "strong".into(),
                instructions: "Be concise".into(),
                voice_id: "sample.wav".into(),
                enabled_tools: vec![],
                fast_answer_model_id: String::new(),
                processing_steps: vec![],
                direct_steps: vec![],
                delegated_steps: vec![],
                output_steps: vec![OutputStep::Chat],
            });
        settings.assistant_pipelines.active_id = "old-assistant".into();
        let imported = import_assistants(&settings).unwrap().unwrap();
        assert_eq!(imported.pipelines.len(), 2);
        assert_eq!(imported.active_assistant().unwrap().steps, vec![speech]);
        assert_eq!(
            imported.active_assistant().unwrap().output,
            PipelineOutput::Assistant
        );
        assert_eq!(
            imported
                .active_assistant()
                .unwrap()
                .assistant
                .as_ref()
                .unwrap()
                .voice_id,
            "sample.wav"
        );
        assert_eq!(
            imported.active().unwrap().id,
            settings.pipelines.as_ref().unwrap().active_id
        );
        settings.pipelines = Some(imported);
        assert_eq!(
            import_assistants(&settings)
                .unwrap()
                .unwrap()
                .pipelines
                .len(),
            2
        );
    }

    #[test]
    fn assistant_speech_can_be_configured_before_router_models() {
        let settings = AppSettings::default();
        let mut pipeline = migrate(&settings).active().unwrap().clone();
        pipeline.output = PipelineOutput::Assistant;
        pipeline.assistant = Some(AssistantConfig::default());
        assert!(pipeline.validate_components().is_ok());
        assert!(pipeline.validate().is_err());
    }

    #[test]
    fn imports_distinct_gemini_models_and_preserves_steps() {
        let mut settings = AppSettings {
            gemini_api_key: "test-key".into(),
            gemini_enabled: true,
            ..Default::default()
        };
        let mut set = migrate(&settings);
        set.version = 1;
        let first_id = set.pipelines[0].steps[1].id.clone();
        set.pipelines[0].steps.push(Step::new(StepKind::Gemini {
            model: "another-gemini-model".into(),
            preset: "Custom".into(),
            prompt: "Keep it short".into(),
        }));
        settings.pipelines = Some(set);
        upgrade_models(&mut settings);
        let set = settings.pipelines.as_ref().unwrap();
        assert_eq!(set.version, 3);
        assert_eq!(set.pipelines[0].steps[1].id, first_id);
        assert!(matches!(
            set.pipelines[0].steps[1].kind,
            StepKind::Model {
                max_tokens: 512,
                temperature: Some(0.4),
                timeout_seconds: 30,
                ..
            }
        ));
        assert_eq!(settings.model_registry.models.len(), 2);
        let snapshot = settings.clone();
        upgrade_models(&mut settings);
        assert_eq!(settings, snapshot);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dictation_activation_preserves_assistant_and_old_selection_recovers_unambiguously() {
        let settings = AppSettings::default();
        let mut set = migrate(&settings);
        set.version = 5;
        let dictation = set.pipelines[0].id.clone();
        set.pipelines[0].graph = Some(crate::graph::migrate(&set.pipelines[0], &settings));
        let mut assistant = set.pipelines[0].clone();
        assistant.id = "assistant".into();
        for node in &mut assistant.graph.as_mut().unwrap().nodes {
            if matches!(node.kind, crate::graph::Kind::TextOutput) {
                node.kind = crate::graph::Kind::AssistantDisplay;
            }
        }
        set.pipelines.push(assistant.clone());
        set.activate("assistant").unwrap();
        set.activate(&dictation).unwrap();
        let mut saved: PipelineSet =
            serde_json::from_str(&serde_json::to_string(&set).unwrap()).unwrap();
        assert_eq!(saved.active().unwrap().id, dictation);
        assert_eq!(saved.active_assistant().unwrap().id, "assistant");
        saved.active_assistant_id = dictation.clone();
        assert_eq!(saved.assistant_for_runtime().unwrap().id, "assistant");
        assistant.id = "second-assistant".into();
        saved.pipelines.push(assistant);
        assert!(saved.assistant_for_runtime().is_err());
        saved.active_assistant_id = "second-assistant".into();
        assert_eq!(
            saved.assistant_for_runtime().unwrap().id,
            "second-assistant"
        );
        saved.version = 99;
        assert!(saved.assistant_for_runtime().is_err());
    }
    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Runtime::new().unwrap()
    }

    #[test]
    fn migration_copies_effective_settings_without_enabling_disabled_features() {
        for provider in [
            "elevenlabs_realtime",
            "openai_realtime_whisper",
            "local_sherpa_onnx",
        ] {
            for gemini in [false, true] {
                for formatting in [false, true] {
                    let mut s = AppSettings {
                        transcription_provider: provider.into(),
                        gemini_enabled: gemini,
                        ..Default::default()
                    };
                    s.local_sherpa.model = crate::transcription::LocalModel::ParakeetUltra;
                    s.local_sherpa.silence_ms = 8500;
                    s.gemini_prompt_preset = "Sound like a pirate".into();
                    s.gemini_custom_prompt = "Keep names".into();
                    s.post_processing.enabled = formatting;
                    s.post_processing.protected_phrases = vec!["one of a kind".into()];
                    s.post_processing.custom_replacements = vec!["echo app => Echo".into()];
                    let set = migrate(&s);
                    set.validate().unwrap();
                    assert_eq!(set.pipelines.len(), 1);
                    let p = set.active().unwrap();
                    assert_eq!(p.name, "Current setup");
                    let before = s.transcription_config();
                    let after = p.transcription_config(&s).unwrap();
                    assert_eq!(before.provider, after.provider);
                    assert_eq!(before.model_id, after.model_id);
                    assert_eq!(before.language_code, after.language_code);
                    assert_eq!(before.no_verbatim, after.no_verbatim);
                    assert_eq!(before.local, after.local);
                    assert_eq!(
                        p.steps
                            .iter()
                            .filter(|s| matches!(s.kind, StepKind::Gemini { .. }))
                            .count(),
                        usize::from(gemini)
                    );
                    assert_eq!(
                        p.steps
                            .iter()
                            .filter(|s| matches!(s.kind, StepKind::Formatting(_)))
                            .count(),
                        usize::from(formatting)
                    );
                    if formatting {
                        assert_eq!(
                            p.legacy_formatting.as_ref().unwrap().settings,
                            s.post_processing
                        );
                    }
                    let json = serde_json::to_string(&set).unwrap();
                    assert_eq!(serde_json::from_str::<PipelineSet>(&json).unwrap(), set);
                }
            }
        }
    }

    #[test]
    fn migration_preserves_formatting_and_capitalization_only_outputs() {
        let rt = runtime();
        for enabled in [false, true] {
            for capitalization in [false, true] {
                let mut s = AppSettings::default();
                s.post_processing.enabled = enabled;
                s.post_processing.punctuation = false;
                s.post_processing.capitalization = capitalization;
                s.post_processing.custom_replacements = vec!["acme corp => aCMe Corp".into()];
                s.post_processing.protected_phrases = vec!["one of a kind".into()];
                let p = migrate(&s).active().unwrap().clone();
                for text in [
                    "i paid twelve dollars and five cents.",
                    "hello,world? acme corp",
                    "one of a kind and twenty three",
                    "john dot smith at example dot com",
                    "we meet January fifth twenty twenty six at three thirty PM",
                    "code A zero zero seven",
                ] {
                    let old = post_processing::process(text, &s.post_processing);
                    assert!(old.warning.is_none());
                    let output = rt
                        .block_on(execute(&p, text.into(), &s, |_, _, _| {}, || false))
                        .unwrap_or_else(|f| panic!("{}", f.message));
                    assert_eq!(output, old.text, "{text}");
                }
            }
        }
    }

    #[test]
    #[ignore = "uses the installed, verified punctuation model; no network or settings writes"]
    fn migration_preserves_real_punctuation_outputs() {
        post_processing::preload().unwrap();
        let rt = runtime();
        let mut s = AppSettings::default();
        s.post_processing.custom_replacements = vec!["acme corp => aCMe Corp".into()];
        s.post_processing.protected_phrases = vec!["one of a kind".into()];
        for commas in [false, true] {
            for capitalization in [false, true] {
                s.post_processing.commas = commas;
                s.post_processing.capitalization = capitalization;
                let p = migrate(&s).active().unwrap().clone();
                for text in [
                    "hello world how are you i am fine",
                    "acme corp paid twelve dollars and five cents",
                    "one of a kind john dot smith at example dot com",
                    "meet January fifth twenty twenty six at three thirty PM",
                ] {
                    let old = post_processing::process(text, &s.post_processing);
                    assert!(old.warning.is_none());
                    let output = rt
                        .block_on(execute(&p, text.into(), &s, |_, _, _| {}, || false))
                        .unwrap_or_else(|f| panic!("{}", f.message));
                    assert_eq!(output, old.text);
                }
            }
        }
    }

    #[test]
    fn ordered_steps_report_output_and_stop_on_failure() {
        let mut s = AppSettings::default();
        s.post_processing.enabled = false;
        let mut p = migrate(&s).active().unwrap().clone();
        let mut first = formatting_only(PostProcessingSettings::default());
        first.format_numbers = false;
        first.custom_replacements = vec!["hello => twenty three".into()];
        p.steps.push(Step::new(StepKind::Formatting(first)));
        p.steps.push(Step::new(StepKind::Formatting(formatting_only(
            PostProcessingSettings::default(),
        ))));
        p.steps.push(Step::new(StepKind::Gemini {
            model: "test".into(),
            preset: "Custom".into(),
            prompt: "test".into(),
        }));
        p.steps.push(Step::new(StepKind::Formatting(formatting_only(
            PostProcessingSettings::default(),
        ))));
        let mut outputs = Vec::new();
        let start = std::time::Instant::now();
        let f = runtime()
            .block_on(execute(
                &p,
                "hello".into(),
                &s,
                |i, t, done| {
                    if done {
                        outputs.push((i, t.to_string()));
                    }
                },
                || false,
            ))
            .err()
            .unwrap();
        assert_eq!(
            outputs,
            vec![
                (0, "hello".into()),
                (1, "twenty three".into()),
                (2, "23".into())
            ]
        );
        assert_eq!(f.step, 3);
        assert_eq!(f.text, "23");
        assert!(start.elapsed() >= std::time::Duration::from_millis(300));
    }

    #[test]
    fn repeated_gemini_instances_are_independent_and_no_secrets_are_serialized() {
        let mut s = AppSettings {
            gemini_api_key: "never-serialize-this".into(),
            ..Default::default()
        };
        s.post_processing.enabled = false;
        let mut p = migrate(&s).active().unwrap().clone();
        p.steps.push(Step::new(StepKind::Gemini {
            model: "model-a".into(),
            preset: "Minimal corrections".into(),
            prompt: "".into(),
        }));
        p.steps.push(Step::new(StepKind::Gemini {
            model: "model-b".into(),
            preset: "Sound like a pirate".into(),
            prompt: "".into(),
        }));
        p.validate().unwrap();
        assert_ne!(p.steps[1].id, p.steps[2].id);
        assert_ne!(p.steps[1].kind, p.steps[2].kind);
        assert!(!serde_json::to_string(&p)
            .unwrap()
            .contains("never-serialize-this"));
        let snapshot = p.clone();
        p.steps.swap(1, 2);
        assert_ne!(snapshot, p);
    }

    #[test]
    fn validates_transcription_position_and_cancellation() {
        let s = AppSettings::default();
        let mut p = migrate(&s).active().unwrap().clone();
        p.steps.swap(0, 1);
        assert!(p.validate().is_err());
        p.steps.swap(0, 1);
        let failure = runtime()
            .block_on(execute(&p, "hello".into(), &s, |_, _, _| {}, || true))
            .err()
            .unwrap();
        assert_eq!(failure.text, "hello");
        assert_eq!(failure.step, 1);
    }
}
