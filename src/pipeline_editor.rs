//! Stage-aware editing of pipeline drafts. No settings are written here.
use crate::{
    component_pool::{groups, PoolItem},
    pipelines::*,
    settings::AppSettings,
    PipelineCard, PipelineEditorWindow, PipelineStage, StageEntry,
};
use slint::{ModelRc, VecModel};

fn entry(
    title: String,
    summary: String,
    index: i32,
    left: bool,
    right: bool,
    change: bool,
    remove: bool,
) -> StageEntry {
    StageEntry {
        card: PipelineCard {
            title: title.into(),
            summary: summary.into(),
            icon: slint::Image::default(),
            speech: false,
        },
        index,
        ordered: index >= 0,
        left,
        right,
        change,
        remove,
    }
}
fn stage(
    title: &str,
    description: &str,
    stage: i32,
    entries: Vec<StageEntry>,
    add: bool,
) -> PipelineStage {
    PipelineStage {
        title: title.into(),
        description: description.into(),
        stage,
        entries: ModelRc::new(VecModel::from(entries)),
        add,
    }
}
pub fn refresh(e: &PipelineEditorWindow, p: &Pipeline, settings: &AppSettings) {
    if p.graph.is_some() {
        crate::graph_ui::refresh_router_models(e, settings);
        crate::graph_ui::refresh(e, p);
        return;
    }
    e.set_cards(crate::pipeline_ui::cards(&p.steps));
    crate::pipeline_ui::refresh_assistant_cards(e, p, settings);
    let has_speech = p.steps.first().is_some_and(Step::is_transcription);
    let mut input = Vec::new();
    if !has_speech {
        input.push(entry(
            "Add transcription".into(),
            "Required first component".into(),
            -1,
            false,
            false,
            true,
            false,
        ));
    }
    for (i, s) in p.steps.iter().enumerate() {
        input.push(entry(
            s.label(),
            s.summary(),
            i as i32,
            !s.is_transcription() && i > usize::from(has_speech),
            !s.is_transcription() && i + 1 < p.steps.len(),
            true,
            true,
        ));
    }
    let mut stages = vec![stage(
        "Transcription & input processing",
        "Audio → text → ordered text processors",
        0,
        input,
        true,
    )];
    if let Some(c) = &p.assistant {
        let label = |id: &str| {
            settings
                .model_registry
                .models
                .iter()
                .find(|m| m.id == id)
                .map(|m| m.name.clone())
                .unwrap_or_else(|| "Add model".into())
        };
        let decision = settings.model_registry.is_decision(&c.router_model_id);
        stages.push(stage(
            "Router",
            "Input text → choose direct or delegated branch",
            1,
            vec![entry(
                label(&c.router_model_id),
                if decision {
                    "Selects an answer branch"
                } else {
                    "Answers directly or delegates"
                }
                .into(),
                -1,
                false,
                false,
                true,
                !c.router_model_id.is_empty(),
            )],
            c.router_model_id.is_empty(),
        ));
        for (sid, title, steps, id) in [
            (
                2,
                "Direct answer & processing",
                &c.direct_steps,
                &c.fast_answer_model_id,
            ),
            (
                3,
                "Delegated answer & processing",
                &c.delegated_steps,
                &c.answer_model_id,
            ),
        ] {
            let from_router = sid == 2 && !decision && !c.router_model_id.is_empty();
            let mut entries = vec![entry(
                if from_router {
                    "Answer from router".into()
                } else {
                    label(id)
                },
                if from_router {
                    "Uses the router’s direct answer"
                } else {
                    "Text-generating answer model"
                }
                .into(),
                if from_router { -3 } else { -1 },
                false,
                false,
                true,
                !from_router && !id.is_empty(),
            )];
            for (i, s) in steps.iter().enumerate() {
                entries.push(entry(
                    s.label(),
                    s.summary(),
                    i as i32,
                    i > 0,
                    i + 1 < steps.len(),
                    true,
                    true,
                ));
            }
            stages.push(stage(
                title,
                "Answer text → any number of text processors",
                sid,
                entries,
                true,
            ));
        }
        let output = c.effective_output_steps();
        let mut entries = Vec::new();
        for (i, s) in output.iter().enumerate() {
            let (title, summary) = match s {
                OutputStep::Processing(s) => (s.label(), s.summary()),
                OutputStep::Speech => (
                    "Speech generation".into(),
                    if c.voice_id.is_empty() {
                        "Configure a voice · text passes through"
                    } else {
                        "Generates audio · text passes through"
                    }
                    .into(),
                ),
                OutputStep::Chat => (
                    "Assistant chat".into(),
                    "Displays connected input text".into(),
                ),
            };
            let mut row = entry(
                title,
                summary,
                i as i32,
                i > 0 && !matches!(s, OutputStep::Chat),
                i + 1 < output.len()
                    && !matches!(s, OutputStep::Chat)
                    && !matches!(output[i + 1], OutputStep::Chat),
                true,
                true,
            );
            row.ordered = !matches!(s, OutputStep::Chat);
            entries.push(row);
        }
        if !output.iter().any(|s| matches!(s, OutputStep::Chat)) {
            entries.push(entry(
                "Add Assistant chat".into(),
                "Required final destination".into(),
                -1,
                false,
                false,
                true,
                false,
            ));
        }
        stages.push(stage(
            "Shared output",
            "Selected answer → ordered output components → Assistant chat",
            4,
            entries,
            true,
        ));
    } else {
        stages.push(stage(
            "Output",
            "Final text is pasted into the captured window",
            4,
            vec![entry(
                "Paste text".into(),
                "Captured target window".into(),
                -1,
                false,
                false,
                false,
                false,
            )],
            false,
        ));
    }
    if let Some(s) = stages.iter().find(|s| s.stage == 2) {
        e.set_direct_stage(s.clone());
    }
    if let Some(s) = stages.iter().find(|s| s.stage == 3) {
        e.set_delegated_stage(s.clone());
    }
    stages.retain(|s| s.stage != 2 && s.stage != 3);
    e.set_stages(ModelRc::new(VecModel::from(stages)));
}
pub fn step_at(p: &Pipeline, stage: i32, index: usize) -> Option<&Step> {
    match stage {
        5 => p.graph.as_ref()?.nodes.get(index)?.step(),
        0 => p.steps.get(index),
        2 => p.assistant.as_ref()?.direct_steps.get(index),
        3 => p.assistant.as_ref()?.delegated_steps.get(index),
        4 => match p.assistant.as_ref()?.output_steps.as_ref()?.get(index)? {
            OutputStep::Processing(s) => Some(s),
            _ => None,
        },
        _ => None,
    }
}
pub fn find_step_mut<'a>(p: &'a mut Pipeline, id: &str) -> Option<&'a mut Step> {
    if let Some(g) = &mut p.graph {
        return g
            .nodes
            .iter_mut()
            .filter_map(crate::graph::Node::step_mut)
            .find(|s| s.id == id);
    }
    if let Some(s) = p.steps.iter_mut().find(|s| s.id == id) {
        return Some(s);
    }
    let c = p.assistant.as_mut()?;
    c.direct_steps
        .iter_mut()
        .chain(&mut c.delegated_steps)
        .chain(c.output_steps.iter_mut().flatten().filter_map(|s| match s {
            OutputStep::Processing(s) => Some(s),
            _ => None,
        }))
        .find(|s| s.id == id)
}
fn is_role(p: &Pipeline, settings: &AppSettings, stage: i32, target: i32) -> bool {
    stage == 1
        || (stage == 2 || stage == 3)
            && (target == -1
                || target == -2
                    && p.assistant.as_ref().is_some_and(|c| {
                        (if stage == 2 {
                            &c.fast_answer_model_id
                        } else {
                            &c.answer_model_id
                        })
                        .is_empty()
                            && (stage == 3
                                || settings.model_registry.is_decision(&c.router_model_id))
                    }))
}
pub fn picker_items(
    catalog: &[Step],
    p: &Pipeline,
    settings: &AppSettings,
    stage: i32,
    target: i32,
) -> Vec<PoolItem> {
    let speech_only = stage == 0
        && (target == -1
            || target >= 0
                && p.steps
                    .get(target as usize)
                    .is_some_and(Step::is_transcription)
            || target == -2 && !p.steps.first().is_some_and(Step::is_transcription));
    let output = p
        .assistant
        .as_ref()
        .map(AssistantConfig::effective_output_steps)
        .unwrap_or_default();
    let special = stage == 4
        && target != -2
        && (target == -1
            || output
                .get(target as usize)
                .is_some_and(|s| !matches!(s, OutputStep::Processing(_))));
    let mut items = crate::pipeline_ui::pool_items(catalog, settings)
        .into_iter()
        .filter(|item| {
            let s = &catalog[item.index];
            if special {
                return false;
            }
            if speech_only {
                return s.is_transcription();
            }
            let decision = match &s.kind {
                StepKind::Model { model_id, .. } => settings.model_registry.is_decision(model_id),
                _ => false,
            };
            if is_role(p, settings, stage, target) {
                return matches!(s.kind, StepKind::Model { .. }) && (stage == 1 || !decision);
            }
            !s.is_transcription() && !decision
        })
        .collect::<Vec<_>>();
    if stage == 4 && p.assistant.is_some() {
        let item = |index, title: &str, summary: &str| PoolItem {
            index,
            group: 4,
            card: PipelineCard {
                title: title.into(),
                summary: summary.into(),
                icon: slint::Image::default(),
                speech: false,
            },
            search: format!("{title} {summary}").to_lowercase(),
        };
        if target == -2 && !output.iter().any(|s| matches!(s, OutputStep::Speech))
            || target >= 0 && matches!(output.get(target as usize), Some(OutputStep::Speech))
        {
            items.push(item(
                usize::MAX,
                "Speech generation",
                "Pocket TTS · generates audio and passes text through",
            ));
        }
        if target == -1
            || target == -2 && !output.iter().any(|s| matches!(s, OutputStep::Chat))
            || target >= 0 && matches!(output.get(target as usize), Some(OutputStep::Chat))
        {
            items.push(item(
                usize::MAX - 1,
                "Assistant chat",
                "Displays its connected input text",
            ));
        }
    }
    items
}
pub fn filter(e: &PipelineEditorWindow, catalog: &[Step], p: &Pipeline, settings: &AppSettings) {
    e.set_pool_groups(groups(
        &picker_items(
            catalog,
            p,
            settings,
            e.get_picker_stage(),
            e.get_picker_index(),
        ),
        &e.get_pool_search(),
        1,
    ));
}
pub fn select(
    p: &mut Pipeline,
    settings: &AppSettings,
    stage: i32,
    target: i32,
    choice: i32,
    catalog: &[Step],
) -> Result<usize, String> {
    if !picker_items(catalog, p, settings, stage, target)
        .iter()
        .any(|i| i.index as i32 == choice)
    {
        return Err("This component is not compatible with this position.".into());
    }
    let role = is_role(p, settings, stage, target);
    let mut selected = catalog.get(choice as usize).cloned();
    if let Some(s) = &mut selected {
        s.id = new_id();
    }
    if stage == 0 {
        let s = selected.ok_or("Choose an input component.")?;
        if s.is_transcription() {
            crate::pipeline_ui::add_step(p, s, 0)?;
            return Ok(0);
        }
        let index = if target >= 0 {
            target as usize
        } else {
            p.steps.len()
        };
        if target >= 0 {
            p.steps[index] = s;
        } else {
            p.steps.push(s);
        }
        p.legacy_formatting = None;
        return Ok(index);
    }
    let c = p
        .assistant
        .as_mut()
        .ok_or("Assistant stages are unavailable.")?;
    if role {
        let StepKind::Model { model_id, .. } = selected.ok_or("Choose a model.")?.kind else {
            return Err("Choose a model.".into());
        };
        if stage == 1 {
            if settings.model_registry.is_decision(&model_id) && c.fast_answer_model_id.is_empty() {
                c.fast_answer_model_id = if settings.model_registry.is_decision(&c.router_model_id)
                {
                    c.answer_model_id.clone()
                } else {
                    c.router_model_id.clone()
                };
            }
            c.router_model_id = model_id;
            c.preload_router_on_start = false;
        } else if stage == 2 {
            c.fast_answer_model_id = model_id;
        } else {
            c.answer_model_id = model_id;
            c.preload_answer_on_start = false;
        }
        return Ok(0);
    }
    if stage == 4 {
        let next = match choice {
            -1 => {
                c.voice_id.clear();
                c.preload_voice_on_start = false;
                OutputStep::Speech
            }
            -2 => OutputStep::Chat,
            _ => OutputStep::Processing(selected.ok_or("Choose a processor.")?),
        };
        let mut output = c.effective_output_steps();
        let index = if target >= 0 {
            target as usize
        } else if matches!(next, OutputStep::Chat) {
            output.len()
        } else {
            output
                .iter()
                .position(|s| matches!(s, OutputStep::Chat))
                .unwrap_or(output.len())
        };
        if target >= 0 {
            output[index] = next;
        } else {
            output.insert(index, next);
        }
        c.output_steps = Some(output);
        return Ok(index);
    }
    let steps = if stage == 2 {
        &mut c.direct_steps
    } else {
        &mut c.delegated_steps
    };
    let index = if target >= 0 {
        target as usize
    } else {
        steps.len()
    };
    let s = selected.ok_or("Choose a processor.")?;
    if target >= 0 {
        steps[index] = s;
    } else {
        steps.push(s);
    }
    Ok(index + 1)
}
pub fn edit(
    p: &mut Pipeline,
    stage: i32,
    index: i32,
    action: i32,
    settings: &AppSettings,
) -> Result<usize, String> {
    if ![1, 2, 3].contains(&action) {
        return Err("Unknown editing action.".into());
    }
    if stage == 1 || (stage == 2 || stage == 3) && index == -1 {
        if action != 3 {
            return Err("This model has a fixed role.".into());
        }
        let c = p
            .assistant
            .as_mut()
            .ok_or("Missing assistant configuration.")?;
        match stage {
            1 => {
                c.router_model_id.clear();
                c.preload_router_on_start = false;
            }
            2 if settings.model_registry.is_decision(&c.router_model_id) => {
                c.fast_answer_model_id.clear()
            }
            3 => {
                c.answer_model_id.clear();
                c.preload_answer_on_start = false;
            }
            _ => return Err("The direct answer is supplied by the router.".into()),
        }
        return Ok(0);
    }
    let i = usize::try_from(index).map_err(|_| "Choose a component first.")?;
    if stage == 4 {
        let c = p
            .assistant
            .as_mut()
            .ok_or("Missing assistant configuration.")?;
        let mut steps = c.effective_output_steps();
        if i >= steps.len() {
            return Err("Unknown output component.".into());
        }
        if action == 3 {
            steps.remove(i);
        } else {
            if matches!(steps[i], OutputStep::Chat) {
                return Err("Assistant chat stays last.".into());
            }
            let j = if action == 1 {
                i.checked_sub(1)
            } else {
                i.checked_add(1)
            }
            .filter(|j| *j < steps.len() && !matches!(steps[*j], OutputStep::Chat))
            .ok_or("Component is at the stage boundary.")?;
            steps.swap(i, j);
            c.output_steps = Some(steps);
            return Ok(j);
        }
        c.output_steps = Some(steps);
        return Ok(i.saturating_sub(1));
    }
    let steps = match stage {
        0 => &mut p.steps,
        2 => {
            &mut p
                .assistant
                .as_mut()
                .ok_or("Missing assistant configuration.")?
                .direct_steps
        }
        3 => {
            &mut p
                .assistant
                .as_mut()
                .ok_or("Missing assistant configuration.")?
                .delegated_steps
        }
        _ => return Err("Unknown stage.".into()),
    };
    if i >= steps.len() {
        return Err("Unknown component.".into());
    }
    if action == 3 {
        steps.remove(i);
    } else {
        let j = if action == 1 {
            i.checked_sub(1)
        } else {
            i.checked_add(1)
        }
        .filter(|j| *j < steps.len())
        .ok_or("Component is at the stage boundary.")?;
        if steps[i].is_transcription() || steps[j].is_transcription() {
            return Err("Transcription stays first.".into());
        }
        steps.swap(i, j);
        if stage == 0 {
            p.legacy_formatting = None;
        }
        return Ok(j + usize::from(stage != 0));
    }
    if stage == 0 {
        p.legacy_formatting = None;
    }
    Ok(i.saturating_sub(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (AppSettings, Pipeline, Vec<Step>) {
        let mut settings = AppSettings::default();
        for (id, provider) in [
            ("decision", "ollaya"),
            ("fast", "ollama"),
            ("strong", "ollama"),
        ] {
            let mut m = crate::models::ConfiguredModel::new(provider.into(), id.into(), id.into());
            m.id = id.into();
            m.verified = true;
            settings.model_registry.models.push(m);
            settings
                .model_registry
                .connections
                .push(crate::models::ModelConnection {
                    id: provider.into(),
                    provider: provider.into(),
                    endpoint: "http://127.0.0.1:11434".into(),
                    api_version: String::new(),
                    custom_header_name: String::new(),
                    custom_header_value: String::new(),
                    api_key: String::new(),
                    allow_lan_http: false,
                });
        }
        let mut p = migrate(&settings).active().unwrap().clone();
        p.steps.truncate(1);
        p.output = PipelineOutput::Assistant;
        p.assistant = Some(AssistantConfig {
            router_model_id: "decision".into(),
            fast_answer_model_id: "fast".into(),
            answer_model_id: "strong".into(),
            ..Default::default()
        });
        let catalog = crate::pipeline_ui::pool(&settings);
        (settings, p, catalog)
    }
    #[test]
    fn picker_respects_stage_and_position_types() {
        let (settings, p, catalog) = fixture();
        let input = picker_items(&catalog, &p, &settings, 0, -2);
        assert!(input.iter().all(|i| !catalog[i.index].is_transcription()));
        assert!(input.iter().all(|i|!matches!(&catalog[i.index].kind,StepKind::Model{model_id,..} if model_id=="decision")));
        let router = picker_items(&catalog, &p, &settings, 1, -1);
        assert!(router.iter().any(|i|matches!(&catalog[i.index].kind,StepKind::Model{model_id,..} if model_id=="decision")));
        let answer = picker_items(&catalog, &p, &settings, 2, -1);
        assert!(answer.iter().all(|i|matches!(&catalog[i.index].kind,StepKind::Model{model_id,..} if model_id!="decision")));
        assert!(picker_items(&catalog, &p, &settings, 0, 0)
            .iter()
            .all(|i| catalog[i.index].is_transcription()));
    }
    #[test]
    fn replacement_defaults_ordering_and_empty_roles() {
        let (settings, mut p, catalog) = fixture();
        let format = catalog
            .iter()
            .position(|s| matches!(s.kind, StepKind::Formatting(_)))
            .unwrap() as i32;
        let punctuation = catalog
            .iter()
            .position(|s| matches!(s.kind, StepKind::Punctuation(_)))
            .unwrap() as i32;
        select(&mut p, &settings, 0, -2, format, &catalog).unwrap();
        select(&mut p, &settings, 0, -2, punctuation, &catalog).unwrap();
        assert!(edit(&mut p, 0, 1, 1, &settings).is_err());
        edit(&mut p, 0, 2, 1, &settings).unwrap();
        assert!(matches!(p.steps[1].kind, StepKind::Punctuation(_)));
        let previous_id = p.steps[1].id.clone();
        select(&mut p, &settings, 0, 1, format, &catalog).unwrap();
        assert_ne!(p.steps[1].id, previous_id);
        assert_eq!(p.steps[1].kind, catalog[format as usize].kind);
        edit(&mut p, 0, 0, 3, &settings).unwrap();
        assert!(p.validate().is_err());
        let speech = catalog.iter().position(Step::is_transcription).unwrap() as i32;
        select(&mut p, &settings, 0, -1, speech, &catalog).unwrap();
        assert!(p.validate().is_ok());
        edit(&mut p, 1, -1, 3, &settings).unwrap();
        assert!(p.validate().is_err());
    }
    #[test]
    fn output_chain_keeps_chat_last_and_allows_processing_after_speech() {
        let (settings, mut p, catalog) = fixture();
        let format = catalog
            .iter()
            .position(|s| matches!(s.kind, StepKind::Formatting(_)))
            .unwrap() as i32;
        select(&mut p, &settings, 4, -2, -1, &catalog).unwrap();
        select(&mut p, &settings, 4, -2, format, &catalog).unwrap();
        assert!(matches!(
            p.assistant
                .as_ref()
                .unwrap()
                .effective_output_steps()
                .as_slice(),
            [
                OutputStep::Speech,
                OutputStep::Processing(_),
                OutputStep::Chat
            ]
        ));
        assert!(select(&mut p, &settings, 4, -2, -1, &catalog).is_err());
        assert!(edit(&mut p, 4, 1, 2, &settings).is_err());
        edit(&mut p, 4, 1, 1, &settings).unwrap();
        assert!(matches!(
            p.assistant.as_ref().unwrap().effective_output_steps()[0],
            OutputStep::Processing(_)
        ));
        edit(&mut p, 4, 2, 3, &settings).unwrap();
        assert!(p.validate().is_err());
        select(&mut p, &settings, 4, -1, -2, &catalog).unwrap();
        assert!(p.validate().is_ok());
    }
    #[test]
    fn saved_branch_and_output_steps_roundtrip_and_legacy_voice_is_preserved() {
        let (settings, mut p, catalog) = fixture();
        let format = catalog
            .iter()
            .position(|s| matches!(s.kind, StepKind::Formatting(_)))
            .unwrap() as i32;
        select(&mut p, &settings, 2, -2, format, &catalog).unwrap();
        select(&mut p, &settings, 3, -2, format, &catalog).unwrap();
        p.assistant.as_mut().unwrap().voice_id = "legacy.wav".into();
        assert!(matches!(
            p.assistant
                .as_ref()
                .unwrap()
                .effective_output_steps()
                .as_slice(),
            [OutputStep::Speech, OutputStep::Chat]
        ));
        select(&mut p, &settings, 4, -2, format, &catalog).unwrap();
        assert_eq!(
            p,
            serde_json::from_str::<Pipeline>(&serde_json::to_string(&p).unwrap()).unwrap()
        );
        let runtime = crate::assistant::AssistantPipeline::from_pipeline(&p).unwrap();
        assert_eq!(runtime.direct_steps.len(), 1);
        assert_eq!(runtime.delegated_steps.len(), 1);
        assert_eq!(runtime.output_steps.len(), 3);
        assert_eq!(runtime.voice_id, "legacy.wav");
    }
}
