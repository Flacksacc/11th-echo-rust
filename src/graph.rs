//! Typed, acyclic execution graphs. Layout never determines execution order.
use crate::{
    pipelines::{new_id, OutputStep, Pipeline, Step, StepKind},
    settings::AppSettings,
};
use futures_util::{stream::FuturesUnordered, StreamExt};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    #[serde(default)]
    pub hotkey: String,
    #[serde(default)]
    pub view: View,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_formatting: Option<crate::pipelines::LegacyFormatting>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct View {
    pub x: f32,
    pub y: f32,
    pub zoom: f32,
}
impl Default for View {
    fn default() -> Self {
        Self {
            x: 30.,
            y: 30.,
            zoom: 1.,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    #[serde(flatten)]
    pub kind: Kind,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "node_kind", content = "node_config", rename_all = "snake_case")]
pub enum Kind {
    Component(Step),
    Llm {
        step: Step,
        history: bool,
        #[serde(default)]
        tools: Vec<String>,
        #[serde(default)]
        assistant_response: bool,
    },
    Agent {
        #[serde(default)]
        image_input: bool,
        step: Step,
        history: bool,
        #[serde(default)]
        tools: Vec<String>,
        #[serde(default = "default_tool_limit")]
        max_tool_calls: u32,
        #[serde(default = "default_agent_timeout")]
        run_timeout_seconds: u64,
        #[serde(default = "default_tool_timeout")]
        tool_timeout_seconds: u64,
    },
    Text {
        text: String,
    },
    Combine {
        inputs: Vec<String>,
        separator: String,
    },
    Template {
        template: String,
    },
    Router {
        model_id: String,
        options: Vec<String>,
        #[serde(default)]
        fallback: Option<String>,
        #[serde(default)]
        threshold: f64,
        #[serde(default)]
        preload: bool,
    },
    Speech {
        voice: String,
        #[serde(default)]
        preload: bool,
    },
    TextOutput,
    AssistantDisplay,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Edge {
    pub from: String,
    pub output: String,
    pub to: String,
    pub input: String,
}
fn default_tool_limit() -> u32 {
    32
}
fn default_agent_timeout() -> u64 {
    600
}
fn default_tool_timeout() -> u64 {
    60
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataType {
    Text,
    Audio,
    Files,
}
impl DataType {
    pub fn label(self) -> &'static str {
        match self {
            Self::Text => "Text",
            Self::Audio => "Audio",
            Self::Files => "Files",
        }
    }
}
#[derive(Clone, Debug)]
pub struct Port {
    pub id: String,
    pub label: String,
    pub data_type: DataType,
    pub required: bool,
}
fn text_port(id: &str, label: &str, required: bool) -> Port {
    Port {
        id: id.into(),
        label: label.into(),
        data_type: DataType::Text,
        required,
    }
}
pub fn placeholders(template: &str) -> Vec<String> {
    let re = regex::Regex::new(r"\{([A-Za-z_][A-Za-z0-9_]*)\}").unwrap();
    let mut names = Vec::new();
    for c in re.captures_iter(template) {
        if !names.contains(&c[1].to_string()) {
            names.push(c[1].to_string());
        }
    }
    names
}
impl Node {
    pub fn new(kind: Kind, x: f32, y: f32) -> Self {
        let height = if matches!(kind, Kind::TextOutput | Kind::AssistantDisplay) {
            64.
        } else if matches!(kind, Kind::Text { .. }) {
            120.
        } else {
            180.
        };
        Self {
            id: new_id(),
            kind,
            x,
            y,
            width: 260.,
            height,
        }
    }
    pub fn transcription(&self) -> bool {
        matches!(&self.kind, Kind::Component(s) if s.is_transcription())
    }
    pub fn ending(&self) -> bool {
        matches!(self.kind, Kind::TextOutput | Kind::AssistantDisplay)
    }
    pub fn step(&self) -> Option<&Step> {
        match &self.kind {
            Kind::Component(s) | Kind::Llm { step: s, .. } | Kind::Agent { step: s, .. } => Some(s),
            _ => None,
        }
    }
    pub fn step_mut(&mut self) -> Option<&mut Step> {
        match &mut self.kind {
            Kind::Component(s) | Kind::Llm { step: s, .. } | Kind::Agent { step: s, .. } => Some(s),
            _ => None,
        }
    }
    pub fn title(&self) -> String {
        if matches!(self.kind, Kind::Agent { .. }) {
            return "Agent".into();
        }
        match &self.kind {
            Kind::Component(s) | Kind::Llm { step: s, .. } | Kind::Agent { step: s, .. } => {
                s.label()
            }
            Kind::Text { .. } => "Text".into(),
            Kind::Combine { .. } => "Combine text".into(),
            Kind::Template { .. } => "Template".into(),
            Kind::Router { .. } => "Router".into(),
            Kind::Speech { .. } => "Speech · Pocket TTS".into(),
            Kind::TextOutput => "Text output".into(),
            Kind::AssistantDisplay => "Assistant display".into(),
        }
    }
    pub fn inputs(&self) -> Vec<Port> {
        let mut ports = match &self.kind {
            Kind::Component(s) if s.is_transcription() => vec![],
            Kind::Text { .. } => vec![],
            Kind::Llm { .. } | Kind::Agent { .. } => vec![
                text_port("prompt", "Prompt", true),
                text_port("instructions", "Instructions", false),
            ],
            Kind::Combine { inputs, .. } => inputs
                .iter()
                .enumerate()
                .map(|(i, id)| text_port(id, &format!("Text {}", i + 1), true))
                .collect(),
            Kind::Template { template } => placeholders(template)
                .iter()
                .map(|id| text_port(id, id, true))
                .collect(),
            Kind::Router { options, .. } => std::iter::once(text_port("prompt", "Prompt", true))
                .chain(
                    options
                        .iter()
                        .enumerate()
                        .map(|(i, id)| text_port(id, &format!("Option {}", i + 1), true)),
                )
                .chain(std::iter::once(text_port(
                    "new_option",
                    "Add option",
                    false,
                )))
                .collect(),
            Kind::AssistantDisplay => vec![
                text_port("text", "Data", true),
                text_port("assistant", "Assistant", false),
                Port {
                    id: "files".into(),
                    label: "Files".into(),
                    data_type: DataType::Files,
                    required: false,
                },
            ],
            _ => vec![text_port("text", "Text", true)],
        };
        for (id, _, label) in crate::node_settings::text_inputs(self) {
            if !ports.iter().any(|port| port.id == id) {
                ports.push(text_port(&id, &label, false));
            }
        }
        ports
    }
    pub fn outputs(&self) -> Vec<Port> {
        match &self.kind {
            Kind::Agent { .. }
            | Kind::Llm {
                assistant_response: true,
                ..
            } => vec![
                text_port("assistant", "Assistant", false),
                text_port("text", "Data", false),
                Port {
                    id: "files".into(),
                    label: "Files".into(),
                    data_type: DataType::Files,
                    required: false,
                },
            ],
            Kind::Router { options, .. } => options
                .iter()
                .enumerate()
                .map(|(i, id)| text_port(id, &format!("Option {}", i + 1), false))
                .collect(),
            Kind::Speech { .. } | Kind::TextOutput | Kind::AssistantDisplay => vec![],
            _ => vec![text_port("text", "Text", false)],
        }
    }
}

impl Graph {
    pub fn transcription(&self) -> Option<&Step> {
        self.nodes
            .iter()
            .find(|n| n.transcription())
            .and_then(Node::step)
    }
    pub fn connect(
        &mut self,
        from: &str,
        output: &str,
        to: &str,
        input: &str,
    ) -> Result<(), String> {
        let source = self
            .nodes
            .iter()
            .find(|n| n.id == from)
            .ok_or("Unknown source node.")?;
        let out = source
            .outputs()
            .into_iter()
            .find(|p| p.id == output)
            .ok_or("Unknown output.")?;
        let target = self
            .nodes
            .iter()
            .find(|n| n.id == to)
            .ok_or("Unknown target node.")?;
        let port = target
            .inputs()
            .into_iter()
            .find(|p| p.id == input)
            .ok_or("Unknown input.")?;
        if from == to || out.data_type != port.data_type {
            return Err("Incompatible connection.".into());
        }
        if self.edges.iter().any(|e| e.to == to && e.input == input) {
            return Err("Disconnect the existing wire first; inputs accept one wire.".into());
        }
        if input == "new_option" && !matches!(target.kind, Kind::Router { .. }) {
            return Err("Unknown option input.".into());
        }
        let mut next = self.clone();
        let input = if input == "new_option" {
            let id = new_id();
            if let Kind::Router { options, .. } =
                &mut next.nodes.iter_mut().find(|n| n.id == to).unwrap().kind
            {
                options.push(id.clone());
            }
            id
        } else {
            input.to_string()
        };
        next.edges.push(Edge {
            from: from.into(),
            output: output.into(),
            to: to.into(),
            input,
        });
        next.order()?;
        next.legacy_formatting = None;
        *self = next;
        Ok(())
    }
    pub fn disconnect(&mut self, edge: usize) {
        if edge >= self.edges.len() {
            return;
        }
        self.legacy_formatting = None;
        let e = self.edges.remove(edge);
        if let Some(Node {
            kind: Kind::Router {
                options, fallback, ..
            },
            ..
        }) = self.nodes.iter_mut().find(|n| n.id == e.to)
        {
            if options.contains(&e.input) {
                options.retain(|id| id != &e.input);
                self.edges
                    .retain(|wire| !(wire.from == e.to && wire.output == e.input));
                if fallback.as_ref() == Some(&e.input) {
                    *fallback = None;
                }
            }
        }
    }
    pub fn remove(&mut self, id: &str) {
        let incoming_options: Vec<usize> = self
            .edges
            .iter()
            .enumerate()
            .filter(|(_, e)| e.from == id)
            .map(|(i, _)| i)
            .collect();
        for i in incoming_options.into_iter().rev() {
            self.disconnect(i);
        }
        self.edges.retain(|e| e.from != id && e.to != id);
        self.nodes.retain(|n| n.id != id);
    }
    pub fn order(&self) -> Result<Vec<usize>, String> {
        let mut ids = HashSet::new();
        for n in &self.nodes {
            if n.id.is_empty() || !ids.insert(&n.id) {
                return Err("Node IDs must be unique.".into());
            }
        }
        let mut order = Vec::new();
        while order.len() < self.nodes.len() {
            let ready = (0..self.nodes.len()).find(|i| {
                !order.contains(i)
                    && self
                        .edges
                        .iter()
                        .filter(|e| e.to == self.nodes[*i].id)
                        .all(|e| order.iter().any(|j| self.nodes[*j].id == e.from))
            });
            if let Some(i) = ready {
                order.push(i);
            } else {
                return Err("Graphs cannot contain cycles or references to missing nodes.".into());
            }
        }
        Ok(order)
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.nodes.iter().filter(|n| n.transcription()).count() != 1 {
            return Err("Add exactly one Transcription node.".into());
        }
        if !self.view.zoom.is_finite()
            || !(0.25..=2.).contains(&self.view.zoom)
            || !self.view.x.is_finite()
            || !self.view.y.is_finite()
        {
            return Err("Invalid canvas view.".into());
        }
        let order = self.order()?;
        let mut occupied = HashSet::new();
        for e in &self.edges {
            let a = self
                .nodes
                .iter()
                .find(|n| n.id == e.from)
                .ok_or("Missing source node.")?;
            let b = self
                .nodes
                .iter()
                .find(|n| n.id == e.to)
                .ok_or("Missing target node.")?;
            let out = a
                .outputs()
                .into_iter()
                .find(|p| p.id == e.output)
                .ok_or("Missing output socket.")?;
            let input = b
                .inputs()
                .into_iter()
                .find(|p| p.id == e.input && p.id != "new_option")
                .ok_or("Missing input socket.")?;
            if out.data_type != input.data_type || !occupied.insert((&e.to, &e.input)) {
                return Err("Inputs require one compatible wire.".into());
            }
        }
        // Disconnected draft nodes are inert. Every connected node is checked.
        let relevant = self.relevant();
        for n in self.nodes.iter().filter(|n| relevant.contains(&n.id)) {
            if ![n.x, n.y, n.width, n.height].iter().all(|v| v.is_finite())
                || n.width < 220.
                || n.height
                    < if n.ending() {
                        64.
                    } else if matches!(n.kind, Kind::Text { .. }) {
                        120.
                    } else {
                        90.
                    }
            {
                return Err(format!("{}: invalid size or position.", n.title()));
            }
            for p in n.inputs().iter().filter(|p| p.required) {
                if !occupied.contains(&(&n.id, &p.id)) {
                    return Err(format!("{}: connect {}.", n.title(), p.label));
                }
            }
            if let Kind::Router {
                model_id,
                options,
                fallback,
                ..
            } = &n.kind
            {
                if options.iter().collect::<HashSet<_>>().len() != options.len()
                    || options
                        .iter()
                        .any(|id| id == "prompt" || id == "new_option")
                    || model_id.is_empty()
                    || options.len() < 2
                {
                    return Err(
                        "Router requires a model and at least two connected options.".into(),
                    );
                }
                if fallback.as_ref().is_some_and(|id| !options.contains(id)) {
                    return Err("Router fallback option no longer exists.".into());
                }
            }
            if let Some(s) = n.step() {
                if let StepKind::Transcription {
                    provider,
                    model,
                    language,
                    ..
                } = &s.kind
                {
                    if ![
                        "elevenlabs_realtime",
                        "openai_realtime_whisper",
                        "local_sherpa_onnx",
                    ]
                    .contains(&provider.as_str())
                        || model.trim().is_empty()
                        || language.trim().is_empty()
                    {
                        return Err(
                            "Transcription needs a supported provider, model and language.".into(),
                        );
                    }
                }
                if let StepKind::Gemini { model, .. } = &s.kind {
                    if model.trim().is_empty() {
                        return Err("Gemini needs a model.".into());
                    }
                }

                if let Some(config) = s.local_settings() {
                    crate::post_processing::validate(config)?;
                }
                if let Kind::Agent {
                    max_tool_calls,
                    run_timeout_seconds,
                    tool_timeout_seconds,
                    ..
                } = &n.kind
                {
                    if !(1..=128).contains(max_tool_calls)
                        || !(30..=3600).contains(run_timeout_seconds)
                        || !(5..=600).contains(tool_timeout_seconds)
                    {
                        return Err("Agent limits must be 1–128 calls, 30–3600 seconds per run, and 5–600 seconds per tool.".into());
                    }
                }
                if let StepKind::Model {
                    model_id,
                    max_tokens,
                    timeout_seconds,
                    ..
                } = &s.kind
                {
                    if model_id.is_empty()
                        || *max_tokens == 0
                        || !(5..=600).contains(timeout_seconds)
                    {
                        return Err("Invalid LLM configuration.".into());
                    }
                }
            }
        }
        // Symbolic activation predicates prove endpoint exclusivity and coverage
        // without requiring users to insert merge nodes.
        let mut bdd = Bdd::default();
        let mut outputs: HashMap<(String, String), usize> = HashMap::new();
        let mut destinations = Vec::new();
        for i in order {
            let n = &self.nodes[i];
            if !relevant.contains(&n.id) {
                continue;
            }
            let mut active = 1;
            let mut from_recording = n.transcription();
            for e in self.edges.iter().filter(|e| e.to == n.id) {
                let predicate = *outputs
                    .get(&(e.from.clone(), e.output.clone()))
                    .unwrap_or(&0);
                active = bdd.and(active, predicate);
                from_recording |= self.has_transcription_ancestor(&e.from, &mut HashSet::new());
            }
            if n.ending() {
                if !from_recording {
                    return Err("Every ending must depend on Transcription.".into());
                }
                destinations.push(active);
            }
            if let Kind::Router { options, .. } = &n.kind {
                let mut selected = 0;
                for (j, id) in options.iter().enumerate() {
                    // Sequential binary encoding yields exactly one choice.
                    let route = if j + 1 == options.len() {
                        bdd.not(selected)
                    } else {
                        let variable = bdd.variable();
                        let not_selected = bdd.not(selected);
                        bdd.and(variable, not_selected)
                    };
                    selected = bdd.or(selected, route);
                    let out = bdd.and(active, route);
                    outputs.insert((n.id.clone(), id.clone()), out);
                }
            } else {
                for p in n.outputs() {
                    outputs.insert((n.id.clone(), p.id), active);
                }
            }
        }
        let mut any = 0;
        for d in destinations {
            let overlap = bdd.and(any, d);
            if overlap != 0 {
                return Err("Two ending nodes could activate together. Route them through mutually exclusive router outputs.".into());
            }
            any = bdd.or(any, d);
        }
        if any != 1 {
            return Err(
                "Every router choice must reach exactly one Text output or Assistant display."
                    .into(),
            );
        }
        Ok(())
    }
    fn has_transcription_ancestor(&self, id: &str, seen: &mut HashSet<String>) -> bool {
        if !seen.insert(id.into()) {
            return false;
        }
        self.nodes
            .iter()
            .find(|n| n.id == id)
            .is_some_and(Node::transcription)
            || self
                .edges
                .iter()
                .filter(|e| e.to == id)
                .any(|e| self.has_transcription_ancestor(&e.from, seen))
    }
    pub fn relevant(&self) -> HashSet<String> {
        let mut ids: HashSet<String> = self
            .nodes
            .iter()
            .filter(|n| n.transcription())
            .map(|n| n.id.clone())
            .collect();
        loop {
            let before = ids.len();
            for e in &self.edges {
                if ids.contains(&e.from) || ids.contains(&e.to) {
                    ids.insert(e.from.clone());
                    ids.insert(e.to.clone());
                }
            }
            if before == ids.len() {
                break;
            }
        }
        ids
    }
    pub fn preflight(&self, _settings: &AppSettings) -> Result<(), String> {
        // Resource/configuration failures belong to the affected node. They
        // must not prevent unrelated branches or unchosen routes from running.
        self.validate()
    }
}

fn preflight_node(n: &Node, settings: &AppSettings) -> Result<(), String> {
    if let Some(s) = n.step() {
        match &s.kind {
            StepKind::Model { model_id, .. } => {
                let (m, c) = settings.model_registry.find(model_id)?;
                if !m.verified {
                    return Err(format!("Test {} before use.", m.name));
                }
                crate::models::validate_connection(c)?;
                if settings.model_registry.is_decision(model_id) {
                    return Err("Use decision models in Router nodes.".into());
                }
            }
            StepKind::Gemini { .. } if settings.gemini_api_key.trim().is_empty() => {
                return Err("Configure the Gemini API key.".into())
            }
            StepKind::Punctuation(s)
                if s.punctuation && !crate::post_processing::punctuation_model_available() =>
            {
                return Err("Install the punctuation model.".into())
            }
            _ => {}
        }
    }
    match &n.kind {
        Kind::Router { model_id, .. } => {
            let (m, c) = settings.model_registry.find(model_id)?;
            if !m.verified {
                return Err("Test the routing model before use.".into());
            }
            crate::models::validate_connection(c)?;
        }
        Kind::Speech { voice, .. }
            if !crate::assistant_voice::voice_selection_valid(&crate::voices::resolve(
                voice, settings,
            )) || !crate::assistant_voice::installed() =>
        {
            return Err(
                "Install Pocket TTS and select an available voice in the Speech node.".into(),
            )
        }
        _ => {}
    }
    Ok(())
}

// Reduced ordered binary decision diagram; terminals 0=false, 1=true.
#[derive(Default)]
struct Bdd {
    nodes: Vec<(usize, usize, usize)>,
    unique: HashMap<(usize, usize, usize), usize>,
    cache: HashMap<(bool, usize, usize), usize>,
    variables: usize,
}
impl Bdd {
    fn make(&mut self, v: usize, lo: usize, hi: usize) -> usize {
        if lo == hi {
            return lo;
        }
        let key = (v, lo, hi);
        if let Some(id) = self.unique.get(&key) {
            return *id;
        }
        let id = self.nodes.len() + 2;
        self.nodes.push(key);
        self.unique.insert(key, id);
        id
    }
    fn variable(&mut self) -> usize {
        let v = self.variables;
        self.variables += 1;
        self.make(v, 0, 1)
    }
    fn not(&mut self, a: usize) -> usize {
        if a < 2 {
            return 1 - a;
        }
        let (v, l, h) = self.nodes[a - 2];
        let l = self.not(l);
        let h = self.not(h);
        self.make(v, l, h)
    }
    fn and(&mut self, a: usize, b: usize) -> usize {
        self.apply(false, a, b)
    }
    fn or(&mut self, a: usize, b: usize) -> usize {
        self.apply(true, a, b)
    }
    fn apply(&mut self, or: bool, a: usize, b: usize) -> usize {
        if a == b {
            return a;
        }
        if or {
            if a == 1 || b == 1 {
                return 1;
            }
            if a == 0 {
                return b;
            }
            if b == 0 {
                return a;
            }
        } else {
            if a == 0 || b == 0 {
                return 0;
            }
            if a == 1 {
                return b;
            }
            if b == 1 {
                return a;
            }
        }
        let key = (or, a.min(b), a.max(b));
        if let Some(id) = self.cache.get(&key) {
            return *id;
        }
        let (av, al, ah) = self.nodes[a - 2];
        let (bv, bl, bh) = self.nodes[b - 2];
        let v = av.min(bv);
        let lo = self.apply(
            or,
            if av == v { al } else { a },
            if bv == v { bl } else { b },
        );
        let hi = self.apply(
            or,
            if av == v { ah } else { a },
            if bv == v { bh } else { b },
        );
        let id = self.make(v, lo, hi);
        self.cache.insert(key, id);
        id
    }
}

pub fn migrate(p: &Pipeline, settings: &AppSettings) -> Graph {
    let mut g = Graph {
        nodes: vec![],
        edges: vec![],
        hotkey: String::new(),
        view: View::default(),
        legacy_formatting: p.legacy_formatting.clone(),
    };
    let mut previous = String::new();
    for (i, s) in p.steps.iter().enumerate() {
        let id = add_step(
            &mut g,
            s.clone(),
            60. + i as f32 * 320.,
            80.,
            settings,
            false,
        );
        if !previous.is_empty() {
            wire(
                &mut g,
                &previous,
                "text",
                &id,
                if matches!(s.kind, StepKind::Model { .. }) {
                    "prompt"
                } else {
                    "text"
                },
            );
        }
        previous = id;
    }
    if let Some(c) = &p.assistant {
        let x = 60. + p.steps.len() as f32 * 320.;
        let options = vec![new_id(), new_id()];
        let router = Node::new(
            Kind::Router {
                model_id: c.router_model_id.clone(),
                fallback: Some(options[1].clone()),
                threshold: if settings.model_registry.is_decision(&c.router_model_id) {
                    0.8
                } else {
                    0.
                },
                preload: c.preload_router_on_start,
                options: options.clone(),
            },
            x,
            80.,
        );
        let rid = router.id.clone();
        g.nodes.push(router);
        if !previous.is_empty() {
            wire(&mut g, &previous, "text", &rid, "prompt");
        }
        for (j, label) in [
            "Simple conversation and straightforward questions answerable quickly without tools.",
            "Complex reasoning, coding, tool or computer actions, and uncertain answers.",
        ]
        .iter()
        .enumerate()
        {
            let n = Node::new(
                Kind::Text {
                    text: label.to_string(),
                },
                x - 300.,
                530. + j as f32 * 200.,
            );
            let id = n.id.clone();
            g.nodes.push(n);
            wire(&mut g, &id, "text", &rid, &options[j]);
        }
        for (j, (model_id, steps)) in [
            (
                if settings.model_registry.is_decision(&c.router_model_id) {
                    &c.fast_answer_model_id
                } else {
                    &c.router_model_id
                },
                &c.direct_steps,
            ),
            (&c.answer_model_id, &c.delegated_steps),
        ]
        .into_iter()
        .enumerate()
        {
            let y = 80. + j as f32 * 650.;
            let mut s = settings
                .model_registry
                .models
                .iter()
                .find(|m| &m.id == model_id)
                .map(|m| {
                    Step::new(StepKind::Model {
                        model_id: m.id.clone(),
                        name: m.name.clone(),
                        instructions: String::new(),
                        max_tokens: m.max_tokens,
                        temperature: m.temperature,
                        timeout_seconds: m.timeout_seconds,
                    })
                })
                .unwrap_or_else(|| {
                    Step::new(StepKind::Model {
                        model_id: model_id.clone(),
                        name: "Answer model".into(),
                        instructions: String::new(),
                        max_tokens: 2048,
                        temperature: None,
                        timeout_seconds: 60,
                    })
                });
            s.preload_on_start = Some(j == 1 && c.preload_answer_on_start);
            let aid = add_step(&mut g, s, x + 320., y, settings, true);
            if let Some(Node {
                kind: Kind::Llm { tools, .. },
                ..
            }) = g.nodes.iter_mut().find(|n| n.id == aid)
            {
                *tools = c.enabled_tools.clone();
            }
            wire(&mut g, &rid, &options[j], &aid, "prompt");
            if !c.instructions.is_empty() {
                let n = Node::new(
                    Kind::Text {
                        text: c.instructions.clone(),
                    },
                    x + 320.,
                    y + 190.,
                );
                let id = n.id.clone();
                g.nodes.push(n);
                wire(&mut g, &id, "text", &aid, "instructions");
            }
            let mut prev = aid;
            let mut column = x + 640.;
            for s in steps {
                let id = add_step(&mut g, s.clone(), column, y, settings, false);
                wire(
                    &mut g,
                    &prev,
                    "text",
                    &id,
                    if matches!(s.kind, StepKind::Model { .. }) {
                        "prompt"
                    } else {
                        "text"
                    },
                );
                prev = id;
                column += 320.;
            }
            for out in c.effective_output_steps() {
                match out {
                    OutputStep::Processing(s) => {
                        let llm = matches!(s.kind, StepKind::Model { .. });
                        let id = add_step(&mut g, s, column, y, settings, false);
                        wire(
                            &mut g,
                            &prev,
                            "text",
                            &id,
                            if llm { "prompt" } else { "text" },
                        );
                        prev = id;
                        column += 320.;
                    }
                    OutputStep::Speech => {
                        let n = Node::new(
                            Kind::Speech {
                                voice: c.voice_id.clone(),
                                preload: c.preload_voice_on_start,
                            },
                            column,
                            y + 250.,
                        );
                        let id = n.id.clone();
                        g.nodes.push(n);
                        wire(&mut g, &prev, "text", &id, "text");
                    }
                    OutputStep::Chat => {
                        let n = Node::new(Kind::AssistantDisplay, column, y);
                        let id = n.id.clone();
                        g.nodes.push(n);
                        wire(&mut g, &prev, "text", &id, "text");
                    }
                }
            }
        }
    } else {
        let n = Node::new(Kind::TextOutput, 60. + p.steps.len() as f32 * 320., 80.);
        let id = n.id.clone();
        g.nodes.push(n);
        if !previous.is_empty() {
            wire(&mut g, &previous, "text", &id, "text");
        }
    }
    g
}
fn wire(g: &mut Graph, from: &str, output: &str, to: &str, input: &str) {
    g.edges.push(Edge {
        from: from.into(),
        output: output.into(),
        to: to.into(),
        input: input.into(),
    });
}
pub fn add_step(
    g: &mut Graph,
    mut s: Step,
    x: f32,
    y: f32,
    settings: &AppSettings,
    history: bool,
) -> String {
    let instructions = match &mut s.kind {
        StepKind::Model { instructions, .. } => std::mem::take(instructions),
        _ => String::new(),
    };
    let kind = match &s.kind {
        StepKind::Model { model_id, .. } if settings.model_registry.is_decision(model_id) => {
            Kind::Router {
                model_id: model_id.clone(),
                options: vec![],
                fallback: None,
                threshold: 0.,
                preload: s.preload_on_start(),
            }
        }
        StepKind::Model { .. } => Kind::Llm {
            step: s,
            history,
            tools: vec![],
            assistant_response: false,
        },
        _ => Kind::Component(s),
    };
    let n = Node::new(kind, x, y);
    let id = n.id.clone();
    g.nodes.push(n);
    if !instructions.is_empty() {
        let n = Node::new(Kind::Text { text: instructions }, x, y + 200.);
        let pid = n.id.clone();
        g.nodes.push(n);
        wire(g, &pid, "text", &id, "instructions");
    }
    id
}

#[derive(Clone, Debug)]
pub enum Event {
    State {
        node: String,
        state: &'static str,
    },
    Text {
        node: String,
        text: String,
    },
    Error {
        node: String,
        message: String,
    },
    Assistant {
        node: String,
        text: String,
    },
    Files {
        node: String,
        files: Vec<crate::artifacts::Artifact>,
    },
    Speaking {
        node: String,
        text: String,
    },
    Activity {
        node: String,
        text: String,
    },
    Approval {
        node: String,
        request: Arc<crate::agent_tools::Approval>,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Destination {
    Text,
    Assistant,
}
#[derive(Debug)]
pub struct RunResult {
    pub destination: Destination,
    pub text: String,
    pub errors: Vec<String>,
    /// Instructions resolved at a native assistant boundary, never legacy history.
    pub instructions: String,
    /// Speech selection downstream of the activated native assistant model.
    /// None means that its response is not connected to a Speech node.
    pub speech_voice: Option<String>,
}

fn native_speech_voice(graph: &Graph, response: &str) -> Result<Option<String>, String> {
    let mut pending = vec![response.to_owned()];
    let mut seen = HashSet::new();
    let mut voice = None;
    while let Some(id) = pending.pop() {
        if !seen.insert(id.clone()) {
            continue;
        }
        for edge in graph.edges.iter().filter(|edge| edge.from == id) {
            let Some(node) = graph.nodes.iter().find(|node| node.id == edge.to) else {
                continue;
            };
            if let Kind::Speech {
                voice: selection, ..
            } = &node.kind
            {
                if voice.as_ref().is_some_and(|voice| voice != selection) {
                    return Err("The native assistant response is connected to different Speech voices. Use one voice per response branch.".into());
                }
                voice = Some(selection.clone());
            } else if !matches!(
                node.kind,
                Kind::Llm { .. } | Kind::Agent { .. } | Kind::Router { .. }
            ) {
                pending.push(node.id.clone());
            }
        }
    }
    Ok(voice)
}
pub type Observer = Arc<dyn Fn(Event) + Send + Sync>;
#[derive(Clone)]
enum Value {
    Text(String),
    Files(Vec<crate::artifacts::Artifact>),
    Skipped,
    Failed,
}
struct SpeechRunGuard {
    cancel: Arc<std::sync::atomic::AtomicBool>,
    completed: bool,
}
impl Drop for SpeechRunGuard {
    fn drop(&mut self) {
        if !self.completed {
            self.cancel.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
}
type Outputs = Vec<(String, Value)>;
type SpeechStreams =
    Arc<std::sync::Mutex<HashMap<String, tokio::task::JoinHandle<Result<(), String>>>>>;

pub async fn execute(
    graph: &Graph,
    input: String,
    settings: &AppSettings,
    context: String,
    observer: Observer,
    cancel: Arc<dyn Fn() -> bool + Send + Sync>,
) -> Result<RunResult, String> {
    execute_until(graph, input, settings, context, observer, cancel, None).await
}

/// Reuse graph transformations up to the assistant model. Hermes owns the
/// response and tools; downstream speech/display nodes must not execute here.
pub async fn prepare_hermes(
    graph: &Graph,
    input: String,
    settings: &AppSettings,
    observer: Observer,
    cancel: Arc<dyn Fn() -> bool + Send + Sync>,
) -> Result<RunResult, String> {
    graph.validate()?;
    let mut ancestors: HashSet<String> = graph
        .nodes
        .iter()
        .filter(|node| matches!(node.kind, Kind::AssistantDisplay))
        .map(|node| node.id.clone())
        .collect();
    loop {
        let before = ancestors.len();
        for edge in &graph.edges {
            if ancestors.contains(&edge.to) {
                ancestors.insert(edge.from.clone());
            }
        }
        if before == ancestors.len() {
            break;
        }
    }
    let explicit: HashSet<String> = graph
        .nodes
        .iter()
        .filter(|node| {
            ancestors.contains(&node.id)
                && matches!(
                    node.kind,
                    Kind::Agent { .. }
                        | Kind::Llm { history: true, .. }
                        | Kind::Llm {
                            assistant_response: true,
                            ..
                        }
                )
        })
        .map(|node| node.id.clone())
        .collect();
    // Simple assistant graphs may have no history flag. Use their response
    // model while leaving earlier text-processing models in the input path.
    let reaches = |from: &str, to: &str| {
        let mut pending = vec![from.to_owned()];
        let mut seen = HashSet::new();
        while let Some(node) = pending.pop() {
            if !seen.insert(node.clone()) {
                continue;
            }
            for edge in graph.edges.iter().filter(|edge| edge.from == node) {
                if edge.to == to {
                    return true;
                }
                pending.push(edge.to.clone());
            }
        }
        false
    };
    let boundaries = if explicit.is_empty() {
        graph
            .nodes
            .iter()
            .filter(|node| {
                ancestors.contains(&node.id)
                    && matches!(node.kind, Kind::Llm { .. })
                    && !graph.nodes.iter().any(|next| {
                        next.id != node.id
                            && ancestors.contains(&next.id)
                            && matches!(next.kind, Kind::Llm { .. })
                            && reaches(&node.id, &next.id)
                    })
            })
            .map(|node| node.id.clone())
            .collect::<HashSet<_>>()
    } else {
        explicit
            .iter()
            .filter(|node| {
                !explicit
                    .iter()
                    .any(|other| other != *node && reaches(other, node))
            })
            .cloned()
            .collect()
    };
    if boundaries.is_empty() {
        return Err("The assistant graph needs an assistant model node before its display.".into());
    }
    let mut relevant = boundaries.clone();
    loop {
        let before = relevant.len();
        for edge in &graph.edges {
            if relevant.contains(&edge.to) {
                relevant.insert(edge.from.clone());
            }
        }
        if before == relevant.len() {
            break;
        }
    }
    execute_until(
        graph,
        input,
        settings,
        String::new(),
        observer,
        cancel,
        Some((relevant, boundaries)),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn execute_until(
    graph: &Graph,
    input: String,
    settings: &AppSettings,
    context: String,
    observer: Observer,
    cancel: Arc<dyn Fn() -> bool + Send + Sync>,
    preparation: Option<(HashSet<String>, HashSet<String>)>,
) -> Result<RunResult, String> {
    let speech_generation = crate::assistant_voice::playback_generation();
    graph.validate()?;
    let relevant = preparation
        .as_ref()
        .map(|(relevant, _)| relevant.clone())
        .unwrap_or_else(|| graph.relevant());
    let mut instructions = String::new();
    let mut speech_voice = None;
    let mut values: HashMap<(String, String), Value> = HashMap::new();
    let mut started = HashSet::new();
    let mut errors = Vec::new();
    let mut destination = None;
    let mut tasks = FuturesUnordered::new();
    let speech_streams: SpeechStreams = Arc::new(std::sync::Mutex::new(HashMap::new()));
    for n in graph.nodes.iter().filter(|n| relevant.contains(&n.id)) {
        observer(Event::State {
            node: n.id.clone(),
            state: "Waiting",
        });
    }
    loop {
        if cancel() {
            return Err("Graph cancelled.".into());
        }
        let before = started.len();
        for n in graph.nodes.iter().filter(|n| relevant.contains(&n.id)) {
            if started.contains(&n.id) {
                continue;
            }
            if n.transcription() {
                started.insert(n.id.clone());
                values.insert((n.id.clone(), "text".into()), Value::Text(input.clone()));
                observer(Event::State {
                    node: n.id.clone(),
                    state: "Complete",
                });
                continue;
            }
            let incoming: Vec<_> = graph.edges.iter().filter(|e| e.to == n.id).collect();
            if incoming.iter().any(|e| {
                matches!(
                    values.get(&(e.from.clone(), e.output.clone())),
                    Some(Value::Skipped | Value::Failed)
                )
            }) {
                let failed = incoming.iter().any(|e| {
                    matches!(
                        values.get(&(e.from.clone(), e.output.clone())),
                        Some(Value::Failed)
                    )
                });
                started.insert(n.id.clone());
                for p in n.outputs() {
                    values.insert(
                        (n.id.clone(), p.id),
                        if failed {
                            Value::Failed
                        } else {
                            Value::Skipped
                        },
                    );
                }
                observer(Event::State {
                    node: n.id.clone(),
                    state: if failed { "Blocked" } else { "Skipped" },
                });
                continue;
            }
            if incoming
                .iter()
                .any(|e| !values.contains_key(&(e.from.clone(), e.output.clone())))
            {
                continue;
            }
            let mut inputs = HashMap::new();
            for e in incoming {
                match values.get(&(e.from.clone(), e.output.clone())) {
                    Some(Value::Text(text)) => {
                        inputs.insert(e.input.clone(), text.clone());
                    }
                    Some(Value::Files(files)) => {
                        inputs.insert(e.input.clone(), serde_json::to_string(files).unwrap());
                    }
                    _ => {}
                }
            }
            started.insert(n.id.clone());
            observer(Event::State {
                node: n.id.clone(),
                state: "Running",
            });
            let observer = observer.clone();
            let context = context.clone();
            let cancel = cancel.clone();
            let original = graph
                .legacy_formatting
                .as_ref()
                .and_then(|legacy| legacy.formatting_id.as_ref())
                .and_then(|id| {
                    graph
                        .nodes
                        .iter()
                        .find(|node| node.step().is_some_and(|s| &s.id == id))
                })
                .and_then(|node| {
                    graph
                        .edges
                        .iter()
                        .find(|e| e.to == node.id && e.input == "text")
                })
                .and_then(|edge| values.get(&(edge.from.clone(), edge.output.clone())))
                .and_then(|v| {
                    if let Value::Text(text) = v {
                        Some(text.clone())
                    } else {
                        None
                    }
                })
                .unwrap_or_else(|| input.clone());
            let speech_streams = speech_streams.clone();
            let native_boundary = preparation
                .as_ref()
                .is_some_and(|(_, boundaries)| boundaries.contains(&n.id));
            tasks.push(async move {
                let operation=run_node(n,inputs,settings,&context,graph,observer.clone(),cancel.clone(),&original,speech_streams,speech_generation,native_boundary);
                let timeout=if matches!(n.kind,Kind::Agent{..}) {86400} else {match n.step().map(|s|&s.kind){Some(StepKind::Model{timeout_seconds,..})=>*timeout_seconds,Some(StepKind::Gemini{..})=>30,_=>600}};
                let result=tokio::select! {result=tokio::time::timeout(std::time::Duration::from_secs(timeout),operation)=>result.unwrap_or_else(|_|Err("Node timed out.".into())), _=async {while !cancel(){tokio::time::sleep(std::time::Duration::from_millis(30)).await;}}=>Err("Graph cancelled.".into())};
                (n,result)
            });
        }
        if tasks.is_empty() {
            if started.len() < relevant.len() {
                if started.len() > before {
                    continue;
                }
                return Err("Graph has unresolved inputs.".into());
            }
            break;
        }
        let Some((n, result)) = tasks.next().await else {
            break;
        };
        match result {
            Ok((outputs, end)) => {
                for (p, v) in outputs {
                    if p == "__hermes_instructions" {
                        if let Value::Text(text) = &v {
                            instructions = text.clone();
                        }
                    }
                    values.insert((n.id.clone(), p), v);
                }
                if let Some(end) = end {
                    if preparation
                        .as_ref()
                        .is_some_and(|(_, boundaries)| boundaries.contains(&n.id))
                    {
                        speech_voice = native_speech_voice(graph, &n.id)?;
                    }
                    if destination.replace(end).is_some() {
                        return Err("Multiple ending nodes activated.".into());
                    }
                }
                observer(Event::State {
                    node: n.id.clone(),
                    state: "Complete",
                });
            }
            Err(message) => {
                errors.push(format!("{}: {message}", n.title()));
                observer(Event::Error {
                    node: n.id.clone(),
                    message,
                });
                for p in n.outputs() {
                    values.insert((n.id.clone(), p.id), Value::Failed);
                }
                observer(Event::State {
                    node: n.id.clone(),
                    state: "Failed",
                });
            }
        }
    }
    if cancel() {
        return Err("Graph cancelled.".into());
    }
    let (destination, text) = destination.ok_or_else(|| {
        errors
            .first()
            .cloned()
            .unwrap_or_else(|| "No ending node activated.".into())
    })?;
    Ok(RunResult {
        destination,
        text,
        errors,
        instructions,
        speech_voice,
    })
}

// Keep immutable run inputs and the observer/cancellation handles explicit.
#[allow(clippy::too_many_arguments)]
async fn run_node(
    n: &Node,
    inputs: HashMap<String, String>,
    settings: &AppSettings,
    context: &str,
    graph: &Graph,
    observer: Observer,
    cancel: Arc<dyn Fn() -> bool + Send + Sync>,
    original: &str,
    speech_streams: SpeechStreams,
    speech_generation: u64,
    native_boundary: bool,
) -> Result<(Outputs, Option<(Destination, String)>), String> {
    let mut resolved = n.clone();
    for (port, key, _) in crate::node_settings::text_inputs(n) {
        if let Some(value) = inputs.get(&port) {
            crate::node_settings::edit(&mut resolved, &key, value, settings)?;
        }
    }
    let n = &resolved;
    if native_boundary {
        let configured = match n.step().map(|step| &step.kind) {
            Some(StepKind::Model { instructions, .. }) => instructions.clone(),
            _ => String::new(),
        };
        let instructions = inputs.get("instructions").cloned().unwrap_or(configured);
        let prompt = inputs.get("prompt").cloned().unwrap_or_default();
        return Ok((
            vec![("__hermes_instructions".into(), Value::Text(instructions))],
            Some((Destination::Assistant, prompt)),
        ));
    }
    preflight_node(n, settings)?;
    let get = |id: &str| inputs.get(id).cloned().unwrap_or_default();
    let text = match &n.kind {
        Kind::Text { text } => text.clone(),
        Kind::Combine {
            inputs: ports,
            separator,
        } => ports
            .iter()
            .map(|p| get(p))
            .collect::<Vec<_>>()
            .join(separator),
        Kind::Template { template } => {
            if placeholders(template)
                .iter()
                .any(|key| !inputs.contains_key(key))
            {
                return Err("Template requests an unconnected text input.".into());
            }
            let re = regex::Regex::new(r"\{([A-Za-z_][A-Za-z0-9_]*)\}").unwrap();
            re.replace_all(template, |c: &regex::Captures<'_>| get(&c[1]))
                .into_owned()
        }
        Kind::Component(s) => {
            // Legacy paired formatters retain their original projection until edited.
            if let Some(legacy) = graph
                .legacy_formatting
                .as_ref()
                .filter(|l| l.punctuation_id == s.id)
            {
                let combined = Step::new(StepKind::Formatting(legacy.settings.clone()));
                crate::pipelines::run_step(&combined, original, settings).await?
            } else {
                crate::pipelines::run_step(s, &get("text"), settings).await?
            }
        }
        Kind::Llm {
            step,
            history,
            tools,
            ..
        }
        | Kind::Agent {
            step,
            history,
            tools,
            ..
        } => {
            let StepKind::Model {
                model_id,
                instructions: configured_instructions,
                max_tokens,
                temperature,
                timeout_seconds,
                ..
            } = &step.kind
            else {
                return Err("LLM requires a text model.".into());
            };
            let prompt = if *history && !context.is_empty() {
                format!(
                    "Conversation so far:\n{context}\n\nUser: {}\nAssistant:",
                    get("prompt")
                )
            } else {
                get("prompt")
            };
            let instructions = if graph
                .edges
                .iter()
                .any(|edge| edge.to == n.id && edge.input == "instructions")
            {
                get("instructions")
            } else {
                configured_instructions.clone()
            };
            let structured = matches!(
                n.kind,
                Kind::Agent { .. }
                    | Kind::Llm {
                        assistant_response: true,
                        ..
                    }
            );
            let instructions = if structured {
                format!(
                    "{instructions}\n\n{}",
                    crate::response_channels::INSTRUCTIONS
                )
            } else {
                instructions
            };
            let speech_cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let mut speech_guard = SpeechRunGuard {
                cancel: speech_cancel.clone(),
                completed: false,
            };
            let mut parser = crate::response_channels::Parser::default();
            let mut parse_error = None;
            // Stream to connected sinks only; complete-value consumers remain buffered.
            let generation = speech_generation;
            let mut speech = Vec::new();
            for e in graph.edges.iter().filter(|e| {
                e.from == n.id && e.output == if structured { "assistant" } else { "text" }
            }) {
                if let Some(Node {
                    kind: Kind::Speech { voice, .. },
                    ..
                }) = graph.nodes.iter().find(|node| node.id == e.to)
                {
                    let (tx, rx) = std::sync::mpsc::channel();
                    let voice = crate::voices::resolve(voice, settings);
                    let speech_cancel = speech_cancel.clone();
                    let global_cancel = cancel.clone();
                    let notify = observer.clone();
                    let speech_node = e.to.clone();
                    speech_streams.lock().unwrap().insert(
                        e.to.clone(),
                        tokio::task::spawn_blocking(move || {
                            crate::assistant_voice::play_segments_observed(
                                rx,
                                &voice,
                                generation,
                                Arc::new(move || {
                                    global_cancel()
                                        || speech_cancel.load(std::sync::atomic::Ordering::SeqCst)
                                }),
                                Some(Arc::new(move |text| {
                                    notify(Event::Speaking {
                                        node: speech_node.clone(),
                                        text,
                                    })
                                })),
                            )
                        }),
                    );
                    speech.push((tx, 0usize));
                }
            }
            let last = Arc::new(std::sync::Mutex::new(String::new()));
            let last_update = last.clone();
            let callback =
                |raw: &str| {
                    let (data, spoken) = if structured {
                        match parser.update(raw) {
                            Ok(c) => (c.data.clone(), c.assistant.clone()),
                            Err(error) => {
                                parse_error = Some(error);
                                return;
                            }
                        }
                    } else {
                        (raw.to_owned(), raw.to_owned())
                    };
                    let text = data.as_str();
                    if cancel() {
                        return;
                    }
                    *last_update.lock().unwrap() = spoken.clone();
                    observer(Event::Text {
                        node: n.id.clone(),
                        text: text.into(),
                    });
                    for e in graph
                        .edges
                        .iter()
                        .filter(|e| e.from == n.id && e.output == "text")
                    {
                        if graph.nodes.iter().any(|node| {
                            node.id == e.to && matches!(node.kind, Kind::AssistantDisplay)
                        }) {
                            observer(Event::Text {
                                node: e.to.clone(),
                                text: text.into(),
                            });
                        }
                    }
                    if structured {
                        for e in graph
                            .edges
                            .iter()
                            .filter(|e| e.from == n.id && e.output == "assistant")
                        {
                            if graph.nodes.iter().any(|node| {
                                node.id == e.to && matches!(node.kind, Kind::AssistantDisplay)
                            }) {
                                observer(Event::Assistant {
                                    node: e.to.clone(),
                                    text: spoken.clone(),
                                });
                            }
                        }
                    }
                    for (tx, offset) in &mut speech {
                        for sentence in crate::assistant::ready_spoken_sentences(&spoken, offset) {
                            let _ = tx.send(sentence);
                        }
                    }
                };
            let agent = if let Kind::Agent {
                max_tool_calls,
                run_timeout_seconds,
                tool_timeout_seconds,
                ..
            } = &n.kind
            {
                Some(
                    crate::agent_tools::Runtime::connect(
                        settings,
                        tools,
                        matches!(
                            n.kind,
                            Kind::Agent {
                                image_input: true,
                                ..
                            }
                        ),
                        *max_tool_calls,
                        *run_timeout_seconds,
                        *tool_timeout_seconds,
                        cancel.clone(),
                        observer.clone(),
                        n.id.clone(),
                    )
                    .await?,
                )
            } else {
                None
            };
            let result = crate::models::stream_agent_component(
                settings,
                model_id,
                &instructions,
                *max_tokens,
                *temperature,
                *timeout_seconds,
                &prompt,
                agent.as_ref(),
                callback,
            )
            .await;
            let completed_channels = if structured && result.is_ok() && parse_error.is_none() {
                Some(parser.finish()?)
            } else {
                None
            };
            if result.is_ok() && parse_error.is_none() {
                for (tx, offset) in &speech {
                    if let Some(tail) = last.lock().unwrap().get(*offset..) {
                        if !tail.trim().is_empty() {
                            let _ = tx.send(tail.trim().into());
                        }
                    }
                }
            }
            drop(speech);
            result?;
            if let Some(error) = parse_error {
                return Err(error);
            }
            if structured {
                let channels = completed_channels.ok_or("Invalid assistant response.")?;
                let files = crate::artifacts::materialize(&channels.files)?;
                for e in graph
                    .edges
                    .iter()
                    .filter(|e| e.from == n.id && e.output == "files")
                {
                    observer(Event::Files {
                        node: e.to.clone(),
                        files: files.clone(),
                    });
                }
                speech_guard.completed = true;
                return Ok((
                    vec![
                        ("text".into(), Value::Text(channels.data)),
                        ("assistant".into(), Value::Text(channels.assistant)),
                        ("files".into(), Value::Files(files)),
                    ],
                    None,
                ));
            }
            speech_guard.completed = true;
            let final_text = last.lock().unwrap().clone();
            final_text
        }
        Kind::Router {
            model_id,
            options,
            fallback,
            threshold,
            ..
        } => {
            let (model, connection) = settings.model_registry.find(model_id)?;
            let choices: Vec<_> = options.iter().map(|id| (id.clone(), get(id))).collect();
            if choices.iter().any(|(_, v)| v.trim().is_empty()) {
                return Err("Router options must contain text.".into());
            }
            let choice = if settings.model_registry.is_decision(model_id) {
                crate::system_one::decide_options(
                    connection,
                    &model.model,
                    &model.instructions,
                    &get("prompt"),
                    model.timeout_seconds,
                    &choices,
                )
                .await
                .map(|d| {
                    if d.confidence >= *threshold {
                        d.choice
                    } else {
                        fallback.clone().unwrap_or(d.choice)
                    }
                })
            } else {
                let request = serde_json::json!({"prompt":get("prompt"),"options":choices.iter().map(|(id,text)|serde_json::json!({"id":id,"description":text})).collect::<Vec<_>>()});
                crate::models::complete(
                    settings,
                    model_id,
                    "Choose exactly one option. Return only JSON: {\"choice\":\"option id\"}.",
                    256,
                    Some(0.),
                    model.timeout_seconds,
                    &request.to_string(),
                )
                .await
                .and_then(|s| {
                    serde_json::from_str::<serde_json::Value>(s.trim())
                        .map_err(|_| "Invalid router response.".into())
                })
                .and_then(|v| {
                    v["choice"]
                        .as_str()
                        .map(str::to_owned)
                        .ok_or("Router response has no choice.".into())
                })
            };
            let choice = choice
                .and_then(|id| {
                    if options.contains(&id) {
                        Ok(id)
                    } else {
                        Err("Router selected an unknown option.".into())
                    }
                })
                .or_else(|error| fallback.clone().ok_or(error))?;
            let prompt = get("prompt");
            return Ok((
                options
                    .iter()
                    .map(|id| {
                        (
                            id.clone(),
                            if id == &choice {
                                Value::Text(prompt.clone())
                            } else {
                                Value::Skipped
                            },
                        )
                    })
                    .collect(),
                None,
            ));
        }
        Kind::Speech { voice, .. } => {
            let worker = speech_streams.lock().unwrap().remove(&n.id);
            if let Some(worker) = worker {
                worker
                    .await
                    .map_err(|_| "Speech task failed.".to_string())??;
            } else {
                let voice = crate::voices::resolve(voice, settings);
                let text = get("text");
                let generation = speech_generation;
                tokio::task::spawn_blocking(move || {
                    crate::assistant_voice::speak_if_current(&text, &voice, generation)
                })
                .await
                .map_err(|_| "Speech task failed.".to_string())??;
            }
            return Ok((vec![], None));
        }
        Kind::TextOutput | Kind::AssistantDisplay => {
            let text = get("text");
            if matches!(n.kind, Kind::AssistantDisplay) {
                let spoken = get("assistant");
                if !spoken.is_empty() {
                    observer(Event::Assistant {
                        node: n.id.clone(),
                        text: spoken,
                    });
                }
                let files = get("files");
                if !files.is_empty() {
                    observer(Event::Files {
                        node: n.id.clone(),
                        files: serde_json::from_str(&files)
                            .map_err(|_| "Invalid attachment input.")?,
                    });
                }
            }
            if text.trim().is_empty() {
                return Err("Ending node received empty text.".into());
            }
            observer(Event::Text {
                node: n.id.clone(),
                text: text.clone(),
            });
            return Ok((
                vec![],
                Some((
                    if matches!(n.kind, Kind::TextOutput) {
                        Destination::Text
                    } else {
                        Destination::Assistant
                    },
                    text,
                )),
            ));
        }
    };
    observer(Event::Text {
        node: n.id.clone(),
        text: text.clone(),
    });
    Ok((vec![("text".into(), Value::Text(text))], None))
}

#[cfg(test)]
#[path = "graph_tests.rs"]
mod tests;
