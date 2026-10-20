//! Presentation-only tab state. Native Hermes sessions and tasks own execution.
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Tab {
    #[serde(default)]
    pub imported_from: Option<serde_json::Value>,
    pub id: String,
    pub session: String,
    pub task: String,
    pub name: String,
    pub title: String,
    pub activity: String,
    pub priority: bool,
    pub hidden: bool,
    pub unread: u32,
    pub voice: String,
    pub worker_model: String,
    pub worker_activity: String,
    pub last_heartbeat: Option<i64>,
    pub output: bool,
    pub draft: String,
    pub scroll: f32,
    #[serde(skip)]
    pub summary: String,
}
impl Default for Tab {
    fn default() -> Self {
        Self {
            imported_from: None,
            id: String::new(),
            session: String::new(),
            task: String::new(),
            name: "Assistant".into(),
            title: "Assistant".into(),
            activity: "Ready".into(),
            priority: false,
            hidden: false,
            unread: 0,
            voice: crate::voices::FOLLOW_DEFAULT.into(),
            worker_model: String::new(),
            worker_activity: String::new(),
            last_heartbeat: None,
            output: false,
            draft: String::new(),
            scroll: 0.0,
            summary: String::new(),
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
pub struct Tabs {
    pub selected: String,
    pub tabs: Vec<Tab>,
    #[serde(default)]
    pub source_cursor: u64,
    #[serde(skip)]
    recording: HashMap<u64, String>,
    #[serde(skip)]
    attention: VecDeque<String>,
    #[serde(skip)]
    last_interaction: Option<Instant>,
}
impl Tabs {
    pub fn open_conversation(&mut self, session: String) {
        let number = self.tabs.iter().filter(|tab| tab.task.is_empty()).count() + 1;
        self.tabs.push(Tab {
            id: session.clone(),
            session: session.clone(),
            name: format!("Chat {number}"),
            ..Default::default()
        });
        self.select(&session);
    }
    pub fn selected(&self) -> Option<&Tab> {
        self.tabs.iter().find(|t| t.id == self.selected)
    }
    pub fn selected_mut(&mut self) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|t| t.id == self.selected)
    }
    pub fn select(&mut self, id: &str) -> bool {
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == id && !t.hidden) {
            tab.unread = 0;
            self.selected = id.to_owned();
            self.attention.retain(|pending| pending != id);
            self.last_interaction = Some(Instant::now());
            true
        } else {
            false
        }
    }
    pub fn bind_recording(&mut self, epoch: u64) {
        // A later tab switch cannot redirect the transcription being captured.
        self.recording.clear();
        self.recording.insert(epoch, self.selected.clone());
        self.interaction();
    }
    pub fn recording_target(&mut self, epoch: u64) -> Option<String> {
        self.recording.remove(&epoch)
    }
    pub fn interaction(&mut self) {
        self.last_interaction = Some(Instant::now());
    }
    pub fn notify(&mut self, id: &str, summary: &str, attention_worthy: bool) {
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == id) {
            tab.summary =
                crate::assistant_text::fragment(summary, crate::assistant_text::BLOCK_BYTES)
                    .0
                    .to_owned();
            if self.selected != id {
                tab.unread = tab.unread.saturating_add(1);
            }
            if attention_worthy
                && tab.priority
                && !tab.hidden
                && self.selected != id
                && !self.attention.iter().any(|pending| pending == id)
            {
                self.attention.push_back(id.to_owned());
            }
        }
    }
    pub fn attention_target(&mut self, idle: bool) -> Option<String> {
        if !idle
            || self
                .last_interaction
                .is_some_and(|at| at.elapsed() < Duration::from_secs(2))
        {
            return None;
        }
        while let Some(id) = self.attention.pop_front() {
            if self
                .tabs
                .iter()
                .any(|t| t.id == id && t.priority && !t.hidden)
            {
                return Some(id);
            }
        }
        None
    }
    pub fn hide_selected(&mut self) {
        if let Some(tab) = self.selected_mut().filter(|tab| !tab.task.is_empty()) {
            tab.hidden = true;
        }
        if let Some(general) = self.tabs.iter().find(|t| t.task.is_empty() && !t.hidden) {
            self.selected = general.id.clone();
        }
        // This is a local UI operation; it does not archive, cancel, or delete.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tabs() -> Tabs {
        Tabs {
            selected: "general".into(),
            tabs: vec![
                Tab {
                    id: "general".into(),
                    ..Default::default()
                },
                Tab {
                    id: "worker".into(),
                    task: "native-task".into(),
                    priority: true,
                    ..Default::default()
                },
            ],
            ..Default::default()
        }
    }
    #[test]
    fn opening_a_native_conversation_preserves_worker_and_previous_draft() {
        let mut tabs = tabs();
        tabs.tabs[0].draft = "Unsent instructions".into();
        tabs.tabs[0].scroll = -400.0;
        tabs.bind_recording(42);
        tabs.open_conversation("native-session-2".into());
        assert_eq!(tabs.selected, "native-session-2");
        assert_eq!(tabs.selected().unwrap().session, "native-session-2");
        assert_eq!(tabs.selected().unwrap().name, "Chat 2");
        assert!(tabs.selected().unwrap().draft.is_empty());
        assert_eq!(tabs.tabs[0].draft, "Unsent instructions");
        assert_eq!(tabs.tabs[0].scroll, -400.0);
        assert_eq!(tabs.tabs[1].task, "native-task");
        assert_eq!(tabs.recording_target(42).as_deref(), Some("general"));
    }
    #[test]
    fn switching_tabs_during_capture_keeps_original_destination() {
        let mut tabs = tabs();
        tabs.bind_recording(42);
        assert!(tabs.select("worker"));
        assert_eq!(tabs.recording_target(42).as_deref(), Some("general"));
        assert_eq!(tabs.recording_target(42), None);
    }
    #[test]
    fn priority_attention_is_coalesced_and_user_interaction_wins() {
        let mut tabs = tabs();
        tabs.notify("worker", "First update", true);
        tabs.notify("worker", "Latest update", true);
        assert_eq!(tabs.attention.len(), 1);
        assert_eq!(tabs.attention_target(false), None);
        tabs.interaction();
        assert_eq!(tabs.attention_target(true), None);
        tabs.last_interaction = Some(Instant::now() - Duration::from_secs(3));
        assert_eq!(tabs.attention_target(true).as_deref(), Some("worker"));
        assert_eq!(tabs.attention_target(true), None);
        assert_eq!(tabs.tabs[1].summary, "Latest update");
    }
    #[test]
    fn hide_is_presentation_only_and_general_is_permanent() {
        let mut tabs = tabs();
        tabs.select("worker");
        tabs.hide_selected();
        assert_eq!(tabs.selected, "general");
        assert_eq!(tabs.tabs[1].task, "native-task");
        assert!(tabs.tabs[1].hidden);
        tabs.hide_selected();
        assert!(!tabs.tabs[0].hidden);
    }
}
