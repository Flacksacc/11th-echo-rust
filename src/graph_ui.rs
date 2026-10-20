//! Graph editor drafts and one-time conversion of legacy pipeline settings.
use crate::{
    graph::{self, Kind, Node, View},
    pipelines::{new_id, Pipeline, Step},
    settings::AppSettings,
    AppWindow, AssistantPipelineWindow, GraphNodeRow, GraphPortRow, GraphWireRow,
    PipelineEditorWindow, PipelineStepWindow,
};
use slint::{CloseRequestResponse, ComponentHandle, Model, ModelRc, VecModel};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    path::PathBuf,
    rc::Rc,
    sync::{Arc, Mutex},
};

thread_local! { static ACTIVE_EDITOR: RefCell<Option<slint::Weak<PipelineEditorWindow>>> = const { RefCell::new(None) }; }
#[cfg(windows)]
pub fn hotkey_capture_editor() -> Option<PipelineEditorWindow> {
    use raw_window_handle::HasWindowHandle;
    let editor = ACTIVE_EDITOR.with(|active| active.borrow().as_ref().and_then(|e| e.upgrade()))?;
    if !editor.window().is_visible() || !editor.get_graph_hotkey_focused() {
        return None;
    }
    let handle = editor.window().window_handle();
    let handle = handle.window_handle().ok()?;
    let raw_window_handle::RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return None;
    };
    (unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() }.0
        == handle.hwnd.get())
    .then_some(editor)
}
pub fn uses_voice(path: &str) -> bool {
    ACTIVE_EDITOR.with(|active| {
        active
            .borrow()
            .as_ref()
            .and_then(|e| e.upgrade())
            .is_some_and(|e| e.invoke_graph_voice_in_use(path.into()))
    })
}
pub fn refresh_voice_choices() {
    ACTIVE_EDITOR.with(|active| {
        let Some(e) = active.borrow().as_ref().and_then(|e| e.upgrade()) else {
            return;
        };
        for node in e.get_graph_nodes().iter() {
            if let Some(fields) = node
                .settings
                .as_any()
                .downcast_ref::<VecModel<crate::GraphSettingRow>>()
            {
                for i in 0..fields.row_count() {
                    let mut field = fields.row_data(i).unwrap();
                    if field.kind != 4 {
                        continue;
                    }
                    let path = field
                        .choices
                        .iter()
                        .position(|c| c == field.value)
                        .and_then(|i| field.choice_values.row_data(i))
                        .unwrap_or_default()
                        .to_string();
                    let choices = crate::node_settings::voice_choices(&path, true);
                    field.value = choices
                        .iter()
                        .find(|(_, v)| v == &path)
                        .unwrap()
                        .0
                        .clone()
                        .into();
                    let labels = choices.iter().map(|(n, _)| n.clone().into()).collect();
                    let paths = choices.into_iter().map(|(_, p)| p.into()).collect();
                    if let Some(model) = field
                        .choices
                        .as_any()
                        .downcast_ref::<VecModel<slint::SharedString>>()
                    {
                        model.set_vec(labels);
                    } else {
                        field.choices = rows(labels);
                    }
                    if let Some(model) = field
                        .choice_values
                        .as_any()
                        .downcast_ref::<VecModel<slint::SharedString>>()
                    {
                        model.set_vec(paths);
                    } else {
                        field.choice_values = rows(paths);
                    }
                    fields.set_row_data(i, field);
                }
            }
        }
    });
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct NodeClipboard {
    format: String,
    version: u32,
    nodes: Vec<Node>,
    edges: Vec<graph::Edge>,
}
impl NodeClipboard {
    fn copy(graph: &graph::Graph, selection: &HashSet<String>) -> Self {
        Self {
            format: "echo-node-selection".into(),
            version: 1,
            nodes: graph
                .nodes
                .iter()
                .filter(|n| selection.contains(&n.id))
                .cloned()
                .collect(),
            edges: graph
                .edges
                .iter()
                .filter(|e| selection.contains(&e.from) && selection.contains(&e.to))
                .cloned()
                .collect(),
        }
    }
    fn paste(&self, graph: &mut graph::Graph, offset: f32) -> Result<Vec<String>, String> {
        if self.format != "echo-node-selection" || self.version != 1 || self.nodes.is_empty() {
            return Err("The clipboard does not contain Echo nodes.".into());
        }
        let ids: HashMap<_, _> = self
            .nodes
            .iter()
            .map(|n| (n.id.clone(), new_id()))
            .collect();
        if ids.len() != self.nodes.len() {
            return Err("Clipboard node IDs must be unique.".into());
        }
        let mut ports = HashMap::new();
        let mut nodes = self.nodes.clone();
        for node in &mut nodes {
            if ![node.x, node.y, node.width, node.height]
                .iter()
                .all(|v| v.is_finite())
            {
                return Err("Clipboard contains invalid node geometry.".into());
            }
            let old = node.id.clone();
            node.id = ids[&old].clone();
            node.x += offset;
            node.y += offset;
            if let Some(step) = node.step_mut() {
                step.id = new_id();
            }
            if let Kind::Router {
                options, fallback, ..
            } = &mut node.kind
            {
                for option in options.iter_mut() {
                    let fresh = new_id();
                    ports.insert((old.clone(), option.clone()), fresh.clone());
                    *option = fresh;
                }
                if let Some(value) = fallback {
                    *value = ports
                        .get(&(old.clone(), value.clone()))
                        .ok_or("Clipboard router fallback is invalid.")?
                        .clone();
                }
            }
        }
        let mut edges = Vec::new();
        for edge in &self.edges {
            edges.push(graph::Edge {
                from: ids
                    .get(&edge.from)
                    .ok_or("Clipboard contains an external wire.")?
                    .clone(),
                to: ids
                    .get(&edge.to)
                    .ok_or("Clipboard contains an external wire.")?
                    .clone(),
                output: ports
                    .get(&(edge.from.clone(), edge.output.clone()))
                    .unwrap_or(&edge.output)
                    .clone(),
                input: ports
                    .get(&(edge.to.clone(), edge.input.clone()))
                    .unwrap_or(&edge.input)
                    .clone(),
            });
        }
        let mut next = graph.clone();
        let selected = nodes.iter().map(|n| n.id.clone()).collect();
        next.nodes.extend(nodes);
        next.edges.extend(edges);
        next.order()?;
        // Drafts may temporarily contain additional transcription or ending nodes;
        // ordinary Save validation still enforces runnable-graph requirements.
        *graph = next;
        Ok(selected)
    }
}

fn node_height(node: &Node) -> f32 {
    let ports = node.inputs().len().max(node.outputs().len()) as f32;
    let minimum = if matches!(node.kind, Kind::Text { .. }) {
        120.
    } else if node.ending() {
        64.
    } else {
        140. + ports * 20.
    };
    node.height.max(minimum)
}

fn enclosed_nodes(graph: &graph::Graph, x1: f32, y1: f32, x2: f32, y2: f32) -> HashSet<String> {
    let (left, right, top, bottom) = (x1.min(x2), x1.max(x2), y1.min(y2), y1.max(y2));
    graph
        .nodes
        .iter()
        .filter(|n| {
            n.x >= left && n.y >= top && n.x + n.width <= right && n.y + node_height(n) <= bottom
        })
        .map(|n| n.id.clone())
        .collect()
}
fn selection(e: &PipelineEditorWindow) -> HashSet<String> {
    let mut ids: HashSet<_> = e
        .get_graph_selection()
        .iter()
        .map(|id| id.to_string())
        .collect();
    if ids.is_empty() && !e.get_graph_selected().is_empty() {
        ids.insert(e.get_graph_selected().to_string());
    }
    ids
}
fn select(e: &PipelineEditorWindow, ids: &HashSet<String>) {
    let model = e.get_graph_nodes();
    let selected: Vec<_> = model
        .iter()
        .filter(|n| ids.contains(n.id.as_str()))
        .map(|n| n.id)
        .collect();
    e.set_graph_selection(rows(selected.clone()));
    e.set_graph_selected(selected.first().cloned().unwrap_or_default());
    if let Some(model) = model.as_any().downcast_ref::<VecModel<GraphNodeRow>>() {
        for i in 0..model.row_count() {
            if let Some(mut node) = model.row_data(i) {
                node.selected = ids.contains(node.id.as_str());
                model.set_row_data(i, node);
            }
        }
    }
}

pub fn upgrade(settings: &mut AppSettings) {
    let Some(mut set) = settings.pipelines.take() else {
        return;
    };
    if !(1..=5).contains(&set.version) {
        settings.pipelines = Some(set);
        return;
    }
    for p in &mut set.pipelines {
        if p.graph.is_none() {
            let mut g = graph::migrate(p, settings);
            if p.id == set.active_id {
                g.hotkey = settings.hotkey_text.clone();
            } else if p.id == set.active_assistant_id
                && settings.assistant_hotkey_text != settings.hotkey_text
            {
                g.hotkey = settings.assistant_hotkey_text.clone();
            }
            p.graph = Some(g);
            p.steps.clear();
            p.assistant = None;
            p.legacy_formatting = None;
            p.output = crate::pipelines::PipelineOutput::Transcription;
        }
    }
    set.version = 5;
    settings.pipelines = Some(set);
}
fn rows<T: 'static + Clone>(v: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(v))
}
pub fn refresh_router_models(e: &PipelineEditorWindow, settings: &AppSettings) {
    crate::node_settings::set_context(settings);
    let models: Vec<_> = settings
        .model_registry
        .models
        .iter()
        .filter(|m| m.verified)
        .collect();
    e.set_graph_router_models(rows(
        models
            .iter()
            .map(|m| crate::assistant_choice_label(&m.name, &m.id).into())
            .collect(),
    ));
    e.set_graph_router_model_ids(rows(models.iter().map(|m| m.id.clone().into()).collect()));
}
pub fn refresh(e: &PipelineEditorWindow, p: &Pipeline) {
    let Some(g) = &p.graph else {
        return;
    };
    e.set_graph_mode(true);
    e.set_assistant_mode(false);
    let selection = selection(e);
    let nodes: Vec<GraphNodeRow> = g
        .nodes
        .iter()
        .map(|n| {
            let detail = match &n.kind {
                Kind::Text { text } => text.clone(),
                Kind::Template { template } => template.clone(),
                Kind::Speech { voice, .. } => {
                    crate::node_settings::voice_choices(voice, true)
                        .into_iter()
                        .find(|(_, path)| path == voice)
                        .unwrap()
                        .0
                }
                Kind::Component(s) if s.is_transcription() => {
                    format!("{} → Text", graph::DataType::Audio.label())
                }
                _ => n.step().map(Step::summary).unwrap_or_default(),
            };
            let router_model = if let Kind::Router { model_id, .. } = &n.kind {
                e.get_graph_router_model_ids()
                    .iter()
                    .position(|id| id.as_str() == model_id)
                    .and_then(|i| e.get_graph_router_models().row_data(i))
                    .map(|label| label.to_string())
                    .unwrap_or_else(|| {
                        if model_id.is_empty() {
                            "Choose a router model".into()
                        } else {
                            format!("Unavailable model: {model_id}")
                        }
                    })
            } else {
                String::new()
            };
            GraphNodeRow {
                selected: selection.contains(&n.id),
                id: n.id.clone().into(),
                title: n.title().into(),
                detail: if matches!(n.kind, Kind::Router { .. }) {
                    format!("Router: {router_model}").into()
                } else {
                    detail.into()
                },
                router: matches!(n.kind, Kind::Router { .. }),
                router_model: router_model.into(),
                settings: if matches!(n.kind, Kind::Text { .. }) {
                    ModelRc::default()
                } else {
                    crate::node_settings::rows_for(n, g)
                },
                editable_text: matches!(n.kind, Kind::Text { .. }),
                state: "".into(),
                x: n.x,
                y: n.y,
                w: n.width,
                h: node_height(n),
                inputs: rows(
                    n.inputs()
                        .iter()
                        .map(|port| GraphPortRow {
                            id: port.id.clone().into(),
                            label: format!("{} · {}", port.label, port.data_type.label()).into(),
                        })
                        .collect(),
                ),
                outputs: rows(
                    n.outputs()
                        .iter()
                        .map(|port| GraphPortRow {
                            id: port.id.clone().into(),
                            label: format!("{} · {}", port.label, port.data_type.label()).into(),
                        })
                        .collect(),
                ),
            }
        })
        .collect();
    let existing = e.get_graph_nodes();
    if existing.row_count() == nodes.len() && existing.iter().zip(&nodes).all(|(a, b)| a.id == b.id)
    {
        if let Some(model) = existing.as_any().downcast_ref::<VecModel<GraphNodeRow>>() {
            for (i, mut node) in nodes.into_iter().enumerate() {
                if let Some(old) = model.row_data(i) {
                    if old.settings.row_count() == node.settings.row_count()
                        && old
                            .settings
                            .iter()
                            .zip(node.settings.iter())
                            .all(|(a, b)| a.key == b.key)
                    {
                        if let Some(fields) = old
                            .settings
                            .as_any()
                            .downcast_ref::<VecModel<crate::GraphSettingRow>>()
                        {
                            for (index, mut field) in node.settings.iter().enumerate() {
                                if let Some(previous) = fields.row_data(index) {
                                    // Replacing an unchanged ComboBox model resets its selection
                                    // asynchronously and can overwrite a restored Undo value.
                                    if previous.choices.iter().eq(field.choices.iter()) {
                                        field.choices = previous.choices;
                                        field.choice_values = previous.choice_values;
                                    }
                                }
                                fields.set_row_data(index, field);
                            }
                            node.settings = old.settings;
                        }
                    }
                }
                node.state = model.row_data(i).map(|n| n.state).unwrap_or_default();
                model.set_row_data(i, node);
            }
        } else {
            e.set_graph_nodes(rows(nodes));
        }
    } else {
        e.set_graph_nodes(rows(nodes));
    }
    let wires = g
        .edges
        .iter()
        .enumerate()
        .filter_map(|(i, wire)| {
            let a = g.nodes.iter().find(|n| n.id == wire.from)?;
            let b = g.nodes.iter().find(|n| n.id == wire.to)?;
            let ai = a.outputs().iter().position(|p| p.id == wire.output)?;
            let bi = b.inputs().iter().position(|p| p.id == wire.input)?;
            Some(GraphWireRow {
                index: i as i32,
                x1: a.x + a.width,
                y1: a.y
                    + 40.
                    + (ai + usize::from(matches!(a.kind, Kind::Router { .. }))) as f32 * 20.,
                x2: b.x,
                y2: b.y + 40. + bi as f32 * 20.,
            })
        })
        .collect();
    e.set_graph_wires(rows(wires));
    e.set_graph_pan_x(g.view.x);
    e.set_graph_pan_y(g.view.y);
    e.set_graph_zoom(g.view.zoom);
    select(e, &selection);
}
type Draft = Rc<RefCell<Pipeline>>;
type History = Rc<RefCell<Vec<Pipeline>>>;
fn checkpoint(d: &Draft, h: &History, r: &History, e: &PipelineEditorWindow) {
    d.borrow_mut().name = e.get_pipeline_name().to_string();
    d.borrow_mut().graph.as_mut().unwrap().hotkey = e.get_graph_hotkey().to_string();
    h.borrow_mut().push(d.borrow().clone());
    r.borrow_mut().clear();
    e.set_undo_available(true);
    e.set_redo_available(false);
}
fn changed(d: &Draft, e: &PipelineEditorWindow) {
    e.set_dirty(true);
    e.set_error("".into());
    refresh(e, &d.borrow());
}

pub fn open(
    mut p: Pipeline,
    shared: Arc<Mutex<AppSettings>>,
    main: slint::Weak<AppWindow>,
    holder: Rc<RefCell<Option<PipelineEditorWindow>>>,
    path: PathBuf,
    _assistant: Option<slint::Weak<AssistantPipelineWindow>>,
) {
    if p.graph.is_none() {
        p.graph = Some(graph::migrate(&p, &shared.lock().unwrap()));
    }
    let Ok(e) = PipelineEditorWindow::new() else {
        return;
    };
    e.set_pipeline_name(p.name.clone().into());
    ACTIVE_EDITOR.with(|active| *active.borrow_mut() = Some(e.as_weak()));
    e.on_capture_graph_hotkey(|event| crate::hotkey::captured_combo(&event).into());
    e.set_graph_hotkey(p.graph.as_ref().unwrap().hotkey.clone().into());
    let d = Rc::new(RefCell::new(p));
    let h = Rc::new(RefCell::new(Vec::new()));
    let redo = Rc::new(RefCell::new(Vec::new()));
    let source = Rc::new(RefCell::new(None::<(String, String)>));
    let dialog = Rc::new(RefCell::new(None::<PipelineStepWindow>));
    refresh_router_models(&e, &shared.lock().unwrap());
    refresh(&e, &d.borrow());
    macro_rules! capture {
        () => {
            (d.clone(), h.clone(), redo.clone(), e.as_weak())
        };
    }
    {
        let (d, h, r, _) = capture!();
        e.on_graph_voice_in_use(move |path| {
            let has_voice = |pipeline: &Pipeline| {
                pipeline.graph.as_ref().is_some_and(|g| {
                    g.nodes
                        .iter()
                        .any(|n| matches!(&n.kind,Kind::Speech{voice,..} if voice == path.as_str()))
                })
            };
            has_voice(&d.borrow())
                || h.borrow().iter().any(has_voice)
                || r.borrow().iter().any(has_voice)
        });
    }
    {
        let (d, h, r, weak) = capture!();
        let shared = shared.clone();
        e.on_graph_setting_edited(move |id, key, value, begin| {
            let Some(e) = weak.upgrade() else {
                return false;
            };
            let mut next = d.borrow().clone();
            let graph = next.graph.as_mut().unwrap();
            let Some(node) = graph.nodes.iter_mut().find(|n| n.id == id.as_str()) else {
                return false;
            };
            if let Err(error) = crate::node_settings::edit(
                node,
                key.as_str(),
                value.as_str(),
                &shared.lock().unwrap(),
            ) {
                e.set_error(error.into());
                return false;
            }
            if next == *d.borrow() {
                return false;
            }
            if begin {
                checkpoint(&d, &h, &r, &e);
            } else {
                r.borrow_mut().clear();
                e.set_redo_available(false);
            }
            *d.borrow_mut() = next;
            changed(&d, &e);
            true
        });
    }
    {
        let weak = e.as_weak();
        e.on_graph_select_node(move |id, additive| {
            if let Some(e) = weak.upgrade() {
                let mut ids = selection(&e);
                if id.is_empty() {
                    select(&e, &HashSet::new());
                    return;
                }
                if additive {
                    if !ids.insert(id.to_string()) {
                        ids.remove(id.as_str());
                    }
                } else if !ids.contains(id.as_str()) {
                    ids = HashSet::from([id.to_string()]);
                }
                select(&e, &ids);
            }
        });
    }
    {
        let (d, _, _, weak) = capture!();
        let base = Rc::new(RefCell::new(HashSet::new()));
        e.on_graph_select_rectangle(move |x1, y1, x2, y2, additive, begin| {
            if let Some(e) = weak.upgrade() {
                if begin {
                    *base.borrow_mut() = if additive {
                        selection(&e)
                    } else {
                        HashSet::new()
                    };
                }
                let mut ids = base.borrow().clone();
                ids.extend(enclosed_nodes(
                    d.borrow().graph.as_ref().unwrap(),
                    x1,
                    y1,
                    x2,
                    y2,
                ));
                select(&e, &ids);
            }
        });
    }
    {
        let (d, _, _, weak) = capture!();
        e.on_graph_move_selection(move |x, y| {
            if let Some(e) = weak.upgrade() {
                let ids = selection(&e);
                for node in &mut d.borrow_mut().graph.as_mut().unwrap().nodes {
                    if ids.contains(&node.id) {
                        node.x += x;
                        node.y += y;
                    }
                }
                changed(&d, &e);
            }
        });
    }
    {
        let (d, _, _, weak) = capture!();
        e.on_graph_copy(move || {
            let Some(e) = weak.upgrade() else {
                return "".into();
            };
            let fragment = NodeClipboard::copy(d.borrow().graph.as_ref().unwrap(), &selection(&e));
            if fragment.nodes.is_empty() {
                return "".into();
            }
            serde_json::to_string(&fragment).unwrap().into()
        });
    }
    {
        let (d, h, r, weak) = capture!();
        let previous = RefCell::new((String::new(), 0u32));
        e.on_graph_paste(move |text| {
            let Some(e) = weak.upgrade() else {
                return;
            };
            let text = text.to_string();
            let Ok(fragment) = serde_json::from_str::<NodeClipboard>(&text) else {
                e.set_error("The clipboard does not contain Echo nodes.".into());
                return;
            };
            let offset = {
                let previous = previous.borrow();
                if previous.0 == text {
                    previous.1 + 1
                } else {
                    1
                }
            };
            let mut next = d.borrow().clone();
            match fragment.paste(next.graph.as_mut().unwrap(), 40. * offset as f32) {
                Ok(ids) => {
                    checkpoint(&d, &h, &r, &e);
                    *d.borrow_mut() = next;
                    e.set_graph_selection(rows(ids.iter().map(|id| id.clone().into()).collect()));
                    e.set_graph_selected(ids.first().cloned().unwrap_or_default().into());
                    changed(&d, &e);
                    *previous.borrow_mut() = (text, offset);
                }
                Err(error) => e.set_error(error.into()),
            }
        });
    }
    {
        let (d, h, r, weak) = capture!();
        e.on_graph_checkpoint(move || {
            if let Some(e) = weak.upgrade() {
                checkpoint(&d, &h, &r, &e);
            }
        });
    }
    {
        let (d, h, r, weak) = capture!();
        e.on_graph_edit_text(move |id, value, begin| {
            let Some(e) = weak.upgrade() else {
                return false;
            };
            let mut next = d.borrow().clone();
            next.name = e.get_pipeline_name().to_string();
            next.graph.as_mut().unwrap().hotkey = e.get_graph_hotkey().to_string();
            let Some(Node {
                kind: Kind::Text { text },
                ..
            }) = next
                .graph
                .as_mut()
                .unwrap()
                .nodes
                .iter_mut()
                .find(|n| n.id == id.as_str())
            else {
                return false;
            };
            if text == value.as_str() {
                return false;
            }
            *text = value.to_string();
            if begin {
                checkpoint(&d, &h, &r, &e);
            } else {
                r.borrow_mut().clear();
                e.set_redo_available(false);
            }
            *d.borrow_mut() = next;
            changed(&d, &e);
            true
        });
    }
    {
        let (d, h, r, weak) = capture!();
        let shared = shared.clone();
        e.on_graph_select_router(move |id, value| {
            let Some(e) = weak.upgrade() else {
                return;
            };
            let model_id = shared
                .lock()
                .unwrap()
                .model_registry
                .models
                .iter()
                .find(|m| {
                    m.verified && crate::assistant_choice_label(&m.name, &m.id) == value.as_str()
                })
                .map(|m| m.id.clone());
            let Some(model_id) = model_id else {
                return;
            };
            let mut next = d.borrow().clone();
            next.name = e.get_pipeline_name().to_string();
            next.graph.as_mut().unwrap().hotkey = e.get_graph_hotkey().to_string();
            let Some(Node {
                kind: Kind::Router {
                    model_id: current, ..
                },
                ..
            }) = next
                .graph
                .as_mut()
                .unwrap()
                .nodes
                .iter_mut()
                .find(|n| n.id == id.as_str())
            else {
                return;
            };
            if *current == model_id {
                return;
            }
            *current = model_id;
            checkpoint(&d, &h, &r, &e);
            *d.borrow_mut() = next;
            changed(&d, &e);
        });
    }
    {
        let (d, _, _, weak) = capture!();
        e.on_graph_geometry(move |id, x, y, w, height| {
            if let Some(e) = weak.upgrade() {
                if let Some(n) = d
                    .borrow_mut()
                    .graph
                    .as_mut()
                    .unwrap()
                    .nodes
                    .iter_mut()
                    .find(|n| n.id == id.as_str())
                {
                    n.x = x;
                    n.y = y;
                    n.width = w;
                    n.height = if matches!(n.kind, Kind::Text { .. }) {
                        height.max(120.)
                    } else {
                        height
                    };
                }
                changed(&d, &e);
            }
        });
    }
    {
        let (d, _, _, weak) = capture!();
        e.on_graph_view(move |x, y, zoom| {
            if let Some(e) = weak.upgrade() {
                d.borrow_mut().graph.as_mut().unwrap().view = View {
                    x,
                    y,
                    zoom: zoom.clamp(0.25, 2.),
                };
                changed(&d, &e);
            }
        });
    }
    {
        let source = source.clone();
        e.on_graph_start_wire(move |id, port| {
            *source.borrow_mut() = Some((id.to_string(), port.to_string()));
        });
    }
    {
        let (d, h, r, weak) = capture!();
        e.on_graph_finish_wire(move |x, y| {
            let Some((from, output)) = source.borrow_mut().take() else {
                return;
            };
            let Some(e) = weak.upgrade() else {
                return;
            };
            let target = d
                .borrow()
                .graph
                .as_ref()
                .unwrap()
                .nodes
                .iter()
                .rev()
                .find_map(|n| {
                    n.inputs()
                        .into_iter()
                        .enumerate()
                        .filter(|(i, _)| {
                            (n.x - x).abs() < 18. / e.get_graph_zoom()
                                && (n.y + 40. + *i as f32 * 20. - y).abs()
                                    < 14. / e.get_graph_zoom()
                        })
                        .min_by(|(a, _), (b, _)| {
                            (n.y + 40. + *a as f32 * 20. - y)
                                .abs()
                                .total_cmp(&(n.y + 40. + *b as f32 * 20. - y).abs())
                        })
                        .map(|(_, p)| (n.id.clone(), p.id.clone()))
                });
            if let Some((to, input)) = target {
                let mut next = d.borrow().clone();
                let result = next
                    .graph
                    .as_mut()
                    .unwrap()
                    .connect(&from, &output, &to, &input);
                match result {
                    Ok(()) => {
                        checkpoint(&d, &h, &r, &e);
                        *d.borrow_mut() = next;
                        changed(&d, &e);
                    }
                    Err(error) => e.set_error(error.into()),
                }
            }
        });
    }
    {
        let (d, h, r, weak) = capture!();
        e.on_graph_disconnect(move |index| {
            if let Some(e) = weak.upgrade() {
                checkpoint(&d, &h, &r, &e);
                d.borrow_mut()
                    .graph
                    .as_mut()
                    .unwrap()
                    .disconnect(index as usize);
                changed(&d, &e);
            }
        });
    }
    {
        let (d, h, r, weak) = capture!();
        e.on_graph_delete(move |id| {
            if let Some(e) = weak.upgrade() {
                let mut ids = selection(&e);
                if !ids.contains(id.as_str()) {
                    ids = HashSet::from([id.to_string()]);
                }
                if ids.is_empty() {
                    return;
                }
                checkpoint(&d, &h, &r, &e);
                for id in ids {
                    d.borrow_mut().graph.as_mut().unwrap().remove(&id);
                }
                e.set_graph_selection(rows(Vec::new()));
                e.set_graph_selected("".into());
                changed(&d, &e);
            }
        });
    }
    {
        let (d, h, r, weak) = capture!();
        e.on_graph_duplicate(move |id| {
            if let Some(e) = weak.upgrade() {
                let mut ids = selection(&e);
                if !ids.contains(id.as_str()) {
                    ids = HashSet::from([id.to_string()]);
                }
                let fragment = NodeClipboard::copy(d.borrow().graph.as_ref().unwrap(), &ids);
                if fragment.nodes.is_empty() {
                    return;
                }
                let mut next = d.borrow().clone();
                if let Ok(ids) = fragment.paste(next.graph.as_mut().unwrap(), 40.) {
                    checkpoint(&d, &h, &r, &e);
                    *d.borrow_mut() = next;
                    e.set_graph_selection(rows(ids.iter().map(|id| id.clone().into()).collect()));
                    e.set_graph_selected(ids.first().cloned().unwrap_or_default().into());
                    changed(&d, &e);
                }
            }
        });
    }
    for backwards in [true, false] {
        let (d, h, r, weak) = capture!();
        let cb = move || {
            if let Some(e) = weak.upgrade() {
                let (from, to) = if backwards { (&h, &r) } else { (&r, &h) };
                let previous = from.borrow_mut().pop();
                if let Some(previous) = previous {
                    to.borrow_mut().push(d.borrow().clone());
                    *d.borrow_mut() = previous;
                    e.set_pipeline_name(d.borrow().name.clone().into());
                    e.set_graph_hotkey(d.borrow().graph.as_ref().unwrap().hotkey.clone().into());
                    changed(&d, &e);
                    e.set_undo_available(!h.borrow().is_empty());
                    e.set_redo_available(!r.borrow().is_empty());
                }
            }
        };
        if backwards {
            e.on_undo(cb);
        } else {
            e.on_redo(cb);
        }
    }
    {
        let (d, _, _, weak) = capture!();
        e.on_graph_fit(move || {
            if let Some(e) = weak.upgrade() {
                let bounds = d.borrow();
                let g = bounds.graph.as_ref().unwrap();
                if g.nodes.is_empty() {
                    return;
                }
                let left = g.nodes.iter().map(|n| n.x).fold(f32::INFINITY, f32::min);
                let top = g.nodes.iter().map(|n| n.y).fold(f32::INFINITY, f32::min);
                let right = g
                    .nodes
                    .iter()
                    .map(|n| n.x + n.width)
                    .fold(f32::NEG_INFINITY, f32::max);
                let bottom = g
                    .nodes
                    .iter()
                    .map(|n| n.y + node_height(n))
                    .fold(f32::NEG_INFINITY, f32::max);
                let z = ((e.get_graph_canvas_width() - 60.) / (right - left))
                    .min((e.get_graph_canvas_height() - 60.) / (bottom - top))
                    .clamp(0.25, 2.);
                drop(bounds);
                e.invoke_graph_checkpoint();
                e.invoke_graph_view(30. - left * z, 30. - top * z, z);
            }
        });
    }
    let catalog = Rc::new(RefCell::new(crate::pipeline_ui::pool(
        &shared.lock().unwrap(),
    )));
    {
        let weak = e.as_weak();
        let shared = shared.clone();
        let catalog = catalog.clone();
        let d = d.clone();
        e.on_refresh_pool(move || {
            *catalog.borrow_mut() = crate::pipeline_ui::pool(&shared.lock().unwrap());
            if let Some(e) = weak.upgrade() {
                refresh_router_models(&e, &shared.lock().unwrap());
                refresh(&e, &d.borrow());
                e.invoke_filter_pool(e.get_pool_search(), 1);
            }
        });
    }
    {
        let weak = e.as_weak();
        let catalog = catalog.clone();
        let shared = shared.clone();
        e.on_filter_pool(move |query, _| {
            if let Some(e) = weak.upgrade() {
                let mut items =
                    crate::pipeline_ui::pool_items(&catalog.borrow(), &shared.lock().unwrap());
                for (index, title, group) in [
                    (-10i32, "Text", 2),
                    (-11, "Combine text", 2),
                    (-12, "Template", 2),
                    (-13, "Text output", 4),
                    (-14, "Assistant display", 4),
                    (-15, "Speech · Pocket TTS", 4),
                    (-16, "Router", 3),
                    (-17, "Agent", 3),
                ] {
                    items.push(crate::component_pool::PoolItem {
                        index: index as usize,
                        group,
                        card: crate::PipelineCard {
                            speech: false,
                            title: title.into(),
                            summary: "".into(),
                            icon: slint::Image::default(),
                        },
                        search: title.to_lowercase(),
                    });
                }
                e.set_pool_groups(crate::component_pool::groups(&items, &query, 1));
            }
        });
    }
    {
        let weak = e.as_weak();
        e.on_open_picker(move |_, target| {
            if let Some(e) = weak.upgrade() {
                e.set_picker_stage(5);
                e.set_picker_index(target);
                e.set_picker_open(true);
                e.invoke_refresh_pool();
            }
        });
    }
    {
        let (d, h, r, weak) = capture!();
        let catalog = catalog.clone();
        let shared = shared.clone();
        e.on_pick_component(move |index| {
            let Some(e)=weak.upgrade() else {return;};
            let mut next=d.borrow().clone();let g=next.graph.as_mut().unwrap();
            let replacing=if e.get_picker_index()==1 {g.nodes.iter().find(|n|n.id==e.get_graph_selected().as_str()).cloned()} else {None};
            let offset=(g.nodes.len()%5) as f32*24.;
            let x=(80.+offset-e.get_graph_pan_x())/e.get_graph_zoom();let y=(100.+offset-e.get_graph_pan_y())/e.get_graph_zoom();
            let kind=match index {
                -17=>{let step=crate::pipelines::Step::new(crate::pipelines::StepKind::Model{model_id:String::new(),name:"Agent".into(),instructions:String::new(),max_tokens:2048,temperature:None,timeout_seconds:60});Some(Kind::Agent{image_input:false,step,history:true,tools:vec![],max_tool_calls:32,run_timeout_seconds:600,tool_timeout_seconds:60})},
                -10=>Some(Kind::Text{text:String::new()}),
                -11=>Some(Kind::Combine{inputs:vec![new_id(),new_id()],separator:"\n".into()}),
                -12=>Some(Kind::Template{template:"{instructions}\n\n{message}".into()}),
                -13=>Some(Kind::TextOutput),-14=>Some(Kind::AssistantDisplay),
                -15=>{let defaults=shared.lock().unwrap();Some(Kind::Speech{voice:crate::voices::FOLLOW_DEFAULT.into(),preload:defaults.tts_preload})},-16=>Some(Kind::Router{model_id:String::new(),options:vec![],fallback:None,threshold:0.,preload:false}),_=>None
            };
            let id=if let Some(kind)=kind {
                if replacing.as_ref().is_some_and(Node::transcription){e.set_error("Choose a transcription provider to replace Transcription.".into());return;}
                let n=Node::new(kind,x,y);let id=n.id.clone();g.nodes.push(n);id
            }else if let Some(mut step)=catalog.borrow().get(index as usize).cloned(){
                if let Some(old)=&replacing{if old.transcription()!=step.is_transcription(){e.set_error("Replace Transcription with a transcription provider.".into());return;}}
                else if step.is_transcription() && g.transcription().is_some(){e.set_error("A graph has only one Transcription node. Configure or replace the existing one.".into());return;}
                step.id=new_id();graph::add_step(g,step,x,y,&shared.lock().unwrap(),false)
            }else{return;};
            let selected=if let Some(old)=replacing {
                let index=g.nodes.iter().position(|n|n.id==id).unwrap();let mut replacement=g.nodes.remove(index);
                replacement.id=old.id.clone();replacement.x=old.x;replacement.y=old.y;replacement.width=old.width;replacement.height=old.height;
                let inputs:HashSet<_>=replacement.inputs().iter().map(|p|p.id.clone()).collect();let outputs:HashSet<_>=replacement.outputs().iter().map(|p|p.id.clone()).collect();
                let new_instructions=g.edges.iter().any(|wire|wire.to==id && wire.input=="instructions");
                g.edges.retain(|wire|(wire.to!=old.id || (inputs.contains(&wire.input) && !(new_instructions && wire.input=="instructions"))) && (wire.from!=old.id || outputs.contains(&wire.output)));
                for wire in &mut g.edges {
                    if wire.to == id { wire.to = old.id.clone(); }
                    if wire.from == id { wire.from = old.id.clone(); }
                }
                let index=g.nodes.iter().position(|n|n.id==old.id).unwrap();g.nodes[index]=replacement;g.legacy_formatting=None;old.id
            }else{id};
            checkpoint(&d,&h,&r,&e);*d.borrow_mut()=next;e.set_graph_selection(rows(vec![selected.clone().into()]));e.set_graph_selected(selected.into());e.set_picker_open(false);changed(&d,&e);
        });
    }
    {
        let weak = main.clone();
        e.on_create_component(move || {
            if let Some(main) = weak.upgrade() {
                main.invoke_create_model_component();
            }
        });
    }
    {
        let (d, _, _, weak) = capture!();
        let shared = shared.clone();
        let main = main.clone();
        let holder = holder.clone();
        let dialog = dialog.clone();
        e.on_save(move || {
            let Some(e) = weak.upgrade() else {
                return;
            };
            let mut p = d.borrow().clone();
            // Incomplete numeric edits must not silently save the last valid value.
            let settings = shared.lock().unwrap();
            for row in e.get_graph_nodes().iter() {
                if let Some(node) = p
                    .graph
                    .as_mut()
                    .unwrap()
                    .nodes
                    .iter_mut()
                    .find(|n| n.id == row.id.as_str())
                {
                    let canonical = crate::node_settings::fields(node, &settings);
                    for field in row.settings.iter() {
                        let unchanged =
                            canonical
                                .iter()
                                .find(|f| f.key == field.key)
                                .is_some_and(|f| {
                                    if field.kind == 1 {
                                        f.checked == field.checked
                                    } else {
                                        f.value == field.value
                                    }
                                });
                        if unchanged {
                            continue;
                        }
                        let value = if field.kind == 1 {
                            field.checked.to_string()
                        } else {
                            field.value.to_string()
                        };
                        if let Err(error) =
                            crate::node_settings::edit(node, &field.key, &value, &settings)
                        {
                            e.set_error(format!("{}: {} — {error}", row.title, field.label).into());
                            return;
                        }
                    }
                }
            }
            drop(settings);
            let graph = p.graph.as_mut().unwrap();
            graph.edges.retain(|edge| {
                graph
                    .nodes
                    .iter()
                    .find(|n| n.id == edge.to)
                    .is_some_and(|n| n.inputs().iter().any(|port| port.id == edge.input))
            });
            p.steps.clear();
            p.assistant = None;
            p.legacy_formatting = None;
            p.output = crate::pipelines::PipelineOutput::Transcription;
            p.name = e.get_pipeline_name().trim().into();
            p.graph.as_mut().unwrap().hotkey = e.get_graph_hotkey().trim().into();
            let old_specs = hotkey_specs(&shared.lock().unwrap());
            let mut specs = old_specs.clone();
            if let Some(old) = specs.iter_mut().find(|(id, _)| id == &p.id) {
                old.1 = p.graph.as_ref().unwrap().hotkey.clone();
            } else {
                specs.push((p.id.clone(), p.graph.as_ref().unwrap().hotkey.clone()));
            }
            let result = p
                .validate()
                .and_then(|_| {
                    if let Some(main) = main.upgrade() {
                        let error = main.invoke_sync_graph_hotkeys(
                            serde_json::to_string(&specs).unwrap().into(),
                        );
                        if !error.is_empty() {
                            return Err(error.into());
                        }
                    }
                    Ok(())
                })
                .and_then(|_| {
                    crate::pipeline_ui::commit(&shared, &path, |set| {
                        set.version = 5;
                        if let Some(old) = set.pipelines.iter_mut().find(|old| old.id == p.id) {
                            *old = p.clone();
                        } else {
                            if set.active_id.is_empty() {
                                set.active_id = p.id.clone();
                            }
                            set.pipelines.push(p.clone());
                        }
                        Ok(())
                    })
                });
            match result {
                Err(error) => {
                    if let Some(main) = main.upgrade() {
                        let _ = main.invoke_sync_graph_hotkeys(
                            serde_json::to_string(&old_specs).unwrap().into(),
                        );
                    }
                    e.set_error(error.into());
                }
                Ok(()) => {
                    if let Some(main) = main.upgrade() {
                        crate::pipeline_ui::refresh(&main, &shared.lock().unwrap());
                        main.invoke_model_components_changed();
                    }
                    if let Some(w) = dialog.borrow_mut().take() {
                        let _ = w.hide();
                    }
                    let _ = e.hide();
                    holder.borrow_mut().take();
                }
            }
        });
    }
    {
        let weak = e.as_weak();
        let holder = holder.clone();
        let dialog = dialog.clone();
        e.on_cancel(move || {
            if let Some(e) = weak.upgrade() {
                let _ = e.hide();
            }
            if let Some(w) = dialog.borrow_mut().take() {
                let _ = w.hide();
            }
            holder.borrow_mut().take();
        });
    }
    let weak = e.as_weak();
    e.window().on_close_requested(move || {
        if let Some(e) = weak.upgrade() {
            if e.get_dirty() {
                e.set_close_prompt(true);
            } else {
                e.invoke_cancel();
            }
        }
        CloseRequestResponse::KeepWindowShown
    });
    e.invoke_filter_pool("".into(), 1);
    let _ = e.show();
    *holder.borrow_mut() = Some(e);
}

pub fn hotkey_specs(settings: &AppSettings) -> Vec<(String, String)> {
    settings
        .pipelines
        .iter()
        .flat_map(|set| &set.pipelines)
        .filter_map(|p| p.graph.as_ref().map(|g| (p.id.clone(), g.hotkey.clone())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn copy_fixture() -> graph::Graph {
        let settings = AppSettings::default();
        let p = crate::pipelines::migrate(&settings)
            .active()
            .unwrap()
            .clone();
        let mut graph = graph::migrate(&p, &settings);
        graph.nodes.push(Node::new(
            Kind::Text {
                text: "Option description".into(),
            },
            100.,
            350.,
        ));
        graph.nodes.push(Node::new(
            Kind::Router {
                model_id: "model-reference".into(),
                options: vec!["option-one".into(), "option-two".into()],
                fallback: Some("option-one".into()),
                threshold: 0.8,
                preload: false,
            },
            450.,
            350.,
        ));
        let text = graph.nodes[graph.nodes.len() - 2].id.clone();
        let router = graph.nodes.last().unwrap().id.clone();
        graph.connect(&text, "text", &router, "option-one").unwrap();
        graph
    }

    #[test]
    fn copied_groups_preserve_internal_wires_and_remap_node_and_router_ids() {
        let mut graph = copy_fixture();
        let text = graph.nodes[graph.nodes.len() - 2].clone();
        let router = graph.nodes.last().unwrap().clone();
        let ids = HashSet::from([text.id.clone(), router.id.clone()]);
        let fragment = NodeClipboard::copy(&graph, &ids);
        assert_eq!(fragment.edges.len(), 1);
        let serialized = serde_json::to_string(&fragment).unwrap();
        let fragment: NodeClipboard = serde_json::from_str(&serialized).unwrap();
        let pasted = fragment.paste(&mut graph, 40.).unwrap();
        assert_eq!(pasted.len(), 2);
        assert!(pasted.iter().all(|id| !ids.contains(id)));
        let copied_text = &graph.nodes[graph.nodes.len() - 2];
        assert_eq!(copied_text.kind, text.kind);
        assert_eq!((copied_text.x, copied_text.y), (text.x + 40., text.y + 40.));
        let copied_router = graph.nodes.last().unwrap();
        let Kind::Router {
            options,
            fallback,
            model_id,
            ..
        } = &copied_router.kind
        else {
            panic!()
        };
        assert_eq!(model_id, "model-reference");
        assert_eq!(fallback.as_ref(), Some(&options[0]));
        assert_ne!(options[0], "option-one");
        let wire = graph.edges.last().unwrap();
        assert_eq!(wire.from, copied_text.id);
        assert_eq!(wire.to, copied_router.id);
        assert_eq!(wire.input, options[0]);
        let single = NodeClipboard::copy(&graph, &HashSet::from([router.id]));
        assert!(single.edges.is_empty());
        let original = graph.nodes[0].clone();
        let source = NodeClipboard::copy(&graph, &HashSet::from([original.id.clone()]));
        source.paste(&mut graph, 80.).unwrap();
        let clone = graph.nodes.last().unwrap();
        assert_ne!(clone.step().unwrap().id, original.step().unwrap().id);
        assert!(graph.validate().is_err()); // Copying is a draft edit; two sources cannot be saved.
    }

    #[test]
    fn rectangle_selection_encloses_actual_node_bounds_in_both_directions() {
        let graph = copy_fixture();
        let text = &graph.nodes[graph.nodes.len() - 2];
        let router = graph.nodes.last().unwrap();
        assert!(node_height(router) > router.height);
        let x = router.x + router.width;
        let y = router.y + node_height(router);
        let forward = enclosed_nodes(&graph, text.x, text.y, x, y);
        assert_eq!(forward, HashSet::from([text.id.clone(), router.id.clone()]));
        assert_eq!(enclosed_nodes(&graph, x, y, text.x, text.y), forward);
        assert!(!enclosed_nodes(&graph, router.x, router.y, x, y - 1.).contains(&router.id));
    }

    #[test]
    fn invalid_clipboard_is_rejected_without_mutating_graph() {
        let mut graph = copy_fixture();
        let before = graph.clone();
        let ids = HashSet::from([graph.nodes.last().unwrap().id.clone()]);
        let mut fragment = NodeClipboard::copy(&graph, &ids);
        fragment.edges.push(graph::Edge {
            from: "external".into(),
            to: fragment.nodes[0].id.clone(),
            output: "text".into(),
            input: "prompt".into(),
        });
        assert!(fragment.paste(&mut graph, 40.).is_err());
        assert_eq!(graph, before);
    }

    #[test]
    #[ignore = "isolated live Slint graph callbacks and temporary settings; no audio or global shortcuts"]
    fn graph_editor_draft_smoke() {
        let mut settings = AppSettings::default();
        settings.post_processing.enabled = false;
        settings.tts_voice_path = "C:/Echo/default-voice.wav".into();
        settings.tts_preload = true;
        for id in ["fast", "strong"] {
            let mut model =
                crate::models::ConfiguredModel::new("ollama".into(), id.into(), id.into());
            model.id = id.into();
            model.verified = true;
            settings.model_registry.models.push(model);
        }
        settings.pipelines = Some(crate::pipelines::migrate(&settings));
        upgrade(&mut settings);
        let p = settings
            .pipelines
            .as_ref()
            .unwrap()
            .active()
            .unwrap()
            .clone();
        let shared = Arc::new(Mutex::new(settings));
        let main = AppWindow::new().unwrap();
        let holder = Rc::new(RefCell::new(None));
        let path = std::env::temp_dir().join(format!("echo-graph-smoke-{}.json", new_id()));
        open(
            p,
            shared.clone(),
            main.as_weak(),
            holder.clone(),
            path.clone(),
            None,
        );
        let e = holder.borrow().as_ref().unwrap().clone_strong();
        let original_nodes = e.get_graph_nodes().row_count();
        let original_wires = e.get_graph_wires().row_count();
        let first = e.get_graph_nodes().row_data(0).unwrap();
        let last = e.get_graph_nodes().row_data(original_nodes - 1).unwrap();
        e.invoke_graph_select_rectangle(
            first.x - 1.,
            first.y - 1.,
            last.x + last.w + 1.,
            e.get_graph_nodes()
                .iter()
                .map(|n| n.y + n.h)
                .fold(0., f32::max)
                + 1.,
            false,
            true,
        );
        assert_eq!(selection(&e).len(), original_nodes);
        assert!(e.get_graph_nodes().iter().all(|n| n.selected));
        let copied = e.invoke_graph_copy();
        assert_eq!(
            serde_json::from_str::<NodeClipboard>(&copied)
                .unwrap()
                .nodes
                .len(),
            original_nodes
        );
        e.invoke_graph_paste(copied.clone());
        assert_eq!(e.get_graph_nodes().row_count(), original_nodes * 2);
        assert_eq!(e.get_graph_wires().row_count(), original_wires * 2);
        e.invoke_undo();
        assert_eq!(e.get_graph_nodes().row_count(), original_nodes);
        e.invoke_graph_select_rectangle(
            first.x - 1.,
            first.y - 1.,
            last.x + last.w + 1.,
            e.get_graph_nodes()
                .iter()
                .map(|n| n.y + n.h)
                .fold(0., f32::max)
                + 1.,
            false,
            true,
        );
        e.invoke_graph_checkpoint();
        e.invoke_graph_move_selection(25., 35.);
        assert_eq!(e.get_graph_nodes().row_data(0).unwrap().x, first.x + 25.);
        assert_eq!(
            e.get_graph_nodes().row_data(original_nodes - 1).unwrap().y,
            last.y + 35.
        );
        e.invoke_undo();
        assert_eq!(e.get_graph_nodes().row_data(0).unwrap().x, first.x);
        e.invoke_graph_duplicate(e.get_graph_selected());
        assert_eq!(e.get_graph_nodes().row_count(), original_nodes * 2);
        assert_eq!(e.get_graph_wires().row_count(), original_wires * 2);
        assert_eq!(selection(&e).len(), original_nodes);
        e.invoke_undo();
        assert_eq!(e.get_graph_nodes().row_count(), original_nodes);
        e.invoke_redo();
        assert_eq!(e.get_graph_nodes().row_count(), original_nodes * 2);
        e.invoke_undo();
        e.invoke_graph_select_node(first.id.clone(), false);
        e.invoke_graph_select_node(last.id.clone(), true);
        assert_eq!(selection(&e).len(), 2);
        e.invoke_graph_select_node(first.id.clone(), true);
        assert_eq!(selection(&e), HashSet::from([last.id.to_string()]));
        let source = e.get_graph_nodes().row_data(0).unwrap();
        e.invoke_graph_checkpoint();
        e.invoke_graph_geometry(source.id.clone(), 90., 190., 320., 230.);
        assert_eq!(e.get_graph_nodes().row_data(0).unwrap().x, 90.);
        e.invoke_undo();
        assert_eq!(e.get_graph_nodes().row_data(0).unwrap().x, source.x);
        e.invoke_redo();
        assert_eq!(e.get_graph_nodes().row_data(0).unwrap().w, 320.);
        e.invoke_graph_disconnect(0);
        assert_eq!(e.get_graph_wires().row_count(), 0);
        e.invoke_undo();
        assert_eq!(e.get_graph_wires().row_count(), 1);
        e.invoke_redo();
        assert_eq!(e.get_graph_wires().row_count(), 0);
        let dest = e.get_graph_nodes().row_data(1).unwrap();
        e.invoke_graph_start_wire(source.id, "text".into());
        e.invoke_graph_finish_wire(dest.x, dest.y + 40.);
        assert_eq!(e.get_graph_wires().row_count(), 1);
        e.invoke_graph_checkpoint();
        e.invoke_graph_view(40., 50., 1.25);
        assert_eq!(e.get_graph_zoom(), 1.25);
        e.invoke_pick_component(-10);
        assert_eq!(e.get_graph_nodes().row_count(), 3);
        let text_id = e.get_graph_selected();
        e.invoke_graph_edit_text(text_id.clone(), "First edit".into(), true);
        e.invoke_graph_edit_text(text_id.clone(), "First line\nSecond line".into(), false);
        assert!(e.get_graph_nodes().row_data(2).unwrap().editable_text);
        e.invoke_undo();
        assert!(e.get_graph_nodes().row_data(2).unwrap().detail.is_empty());
        e.invoke_redo();
        assert_eq!(
            e.get_graph_nodes().row_data(2).unwrap().detail,
            "First line\nSecond line"
        );
        e.invoke_graph_duplicate(e.get_graph_selected());
        assert_eq!(e.get_graph_nodes().row_count(), 4);
        e.invoke_graph_delete(e.get_graph_selected());
        assert_eq!(e.get_graph_nodes().row_count(), 3);
        e.invoke_pick_component(-15);
        assert_eq!(
            e.get_graph_nodes().row_data(3).unwrap().detail,
            "Use settings default"
        );
        let speech_id = e.get_graph_selected();
        assert!(e.invoke_graph_setting_edited(
            speech_id.clone(),
            "/node_config/voice".into(),
            "Alba (standard)".into(),
            true
        ));
        assert_eq!(
            e.get_graph_nodes().row_data(3).unwrap().detail,
            "Alba (standard)"
        );
        e.invoke_undo();
        assert_eq!(
            e.get_graph_nodes().row_data(3).unwrap().detail,
            "Use settings default"
        );
        e.invoke_redo();
        assert_eq!(
            e.get_graph_nodes().row_data(3).unwrap().detail,
            "Alba (standard)"
        );
        e.invoke_undo();
        e.invoke_graph_delete(e.get_graph_selected());
        assert_eq!(e.get_graph_nodes().row_count(), 3);
        e.invoke_pick_component(-16);
        let router_id = e.get_graph_selected();
        let fast = crate::assistant_choice_label("fast", "fast");
        let strong = crate::assistant_choice_label("strong", "strong");
        e.invoke_graph_select_router(router_id.clone(), fast.clone().into());
        assert_eq!(e.get_graph_nodes().row_data(3).unwrap().router_model, fast);
        e.set_pipeline_name("Saved graph fixture".into());
        e.invoke_graph_select_router(router_id.clone(), strong.clone().into());
        assert_eq!(
            e.get_graph_nodes().row_data(3).unwrap().detail,
            format!("Router: {strong}")
        );
        e.invoke_undo();
        assert_eq!(e.get_graph_nodes().row_data(3).unwrap().router_model, fast);
        e.invoke_redo();
        assert_eq!(
            e.get_graph_nodes().row_data(3).unwrap().router_model,
            strong
        );
        let router = e.get_graph_nodes().row_data(3).unwrap();
        e.invoke_graph_start_wire(text_id.clone(), "text".into());
        e.invoke_graph_finish_wire(router.x, router.y + 60.);
        let router = e.get_graph_nodes().row_data(3).unwrap();
        assert_eq!(router.inputs.row_data(1).unwrap().label, "Option 1 · Text");
        assert_eq!(router.outputs.row_data(0).unwrap().label, "Option 1 · Text");
        e.invoke_graph_start_wire(text_id.clone(), "text".into());
        e.invoke_graph_finish_wire(router.x, router.y + 80.);
        assert_eq!(
            e.get_graph_nodes()
                .row_data(3)
                .unwrap()
                .outputs
                .row_data(1)
                .unwrap()
                .label,
            "Option 2 · Text"
        );
        e.invoke_undo();
        e.invoke_undo();
        assert_eq!(
            e.get_graph_nodes().row_data(3).unwrap().outputs.row_count(),
            0
        );
        e.invoke_save();
        assert!(e.get_error().is_empty(), "{}", e.get_error());
        assert!(holder.borrow().is_none());
        let loaded = crate::settings::load_settings_from_path(&path);
        assert_eq!(
            loaded.pipelines.as_ref().unwrap().active().unwrap().name,
            "Saved graph fixture"
        );
        assert_eq!(loaded.pipelines, shared.lock().unwrap().pipelines);
        let text_node = loaded
            .pipelines
            .as_ref()
            .unwrap()
            .active()
            .unwrap()
            .graph
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .find(|n| n.id == text_id.as_str())
            .unwrap();
        assert!(
            matches!(&text_node.kind, Kind::Text { text } if text == "First line\nSecond line")
        );
        let router_node = loaded
            .pipelines
            .as_ref()
            .unwrap()
            .active()
            .unwrap()
            .graph
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .find(|n| n.id == router_id.as_str())
            .unwrap();
        assert!(matches!(&router_node.kind, Kind::Router { model_id, .. } if model_id == "strong"));
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(json["pipelines"]["pipelines"][0].get("output").is_none());
        std::fs::remove_file(path).unwrap();
    }
}
