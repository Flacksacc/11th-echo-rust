//! Presentation state shared by the workspace and the optional desktop companion.
//! No window owns this state, and no transcript or audio is written to a log.
use serde::{Deserialize, Serialize};
use std::sync::{LazyLock, Mutex};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AvatarConfig {
    pub visible: bool,
    pub x: i32,
    pub y: i32,
    pub scale: f32,
    pub ledge_width: f32,
    pub sitting: bool,
    pub runtime: String,
    pub pack: String,
}

impl Default for AvatarConfig {
    fn default() -> Self {
        Self {
            visible: false,
            x: 80,
            y: 80,
            scale: 1.0,
            ledge_width: 420.0,
            sitting: false,
            runtime: String::new(),
            pack: String::new(),
        }
    }
}

impl AvatarConfig {
    pub fn normalize(&mut self) {
        self.scale = if self.scale.is_finite() {
            self.scale.clamp(0.5, 2.0)
        } else {
            1.0
        };
        self.ledge_width = if self.ledge_width.is_finite() {
            self.ledge_width.clamp(160.0, 1600.0)
        } else {
            420.0
        };
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Default)]
pub struct Presentation {
    pub session: String,
    pub state: String,
    pub spoken: String,
    pub listening: bool,
    pub show_spoken: bool,
    pub model_status: String,
    pub model_ready: bool,
    pub avatar: AvatarConfig,
    pub generation: u64,
    #[serde(skip)]
    pub last_sentence: String,
    #[serde(skip)]
    pub spoken_generation: u64,
    #[serde(skip)]
    pub from_audio: bool,
}

impl Presentation {
    fn append_spoken(&mut self, text: &str, generation: u64) {
        if text.is_empty() {
            self.spoken.clear();
            self.last_sentence.clear();
            return;
        }
        if generation != self.spoken_generation {
            self.spoken.clear();
            self.last_sentence.clear();
            self.spoken_generation = generation;
            self.from_audio = false;
        }
        if self.from_audio {
            return;
        }
        if text == self.last_sentence {
            return;
        }
        if text.starts_with(&self.spoken) {
            self.spoken = text.into();
        } else {
            if !self.spoken.is_empty() {
                self.spoken.push('\n');
            }
            self.spoken.push_str(text);
        }
        self.last_sentence = text.into();
    }
}

static STATE: LazyLock<Mutex<Presentation>> = LazyLock::new(|| {
    Mutex::new(Presentation {
        state: "Ready".into(),
        show_spoken: true,
        ..Default::default()
    })
});

pub fn snapshot() -> Presentation {
    STATE.lock().unwrap().clone()
}
pub fn runtime_snapshot() -> Presentation {
    let s = STATE.lock().unwrap();
    Presentation {
        session: s.session.clone(),
        state: s.state.clone(),
        listening: s.listening,
        model_status: s.model_status.clone(),
        model_ready: s.model_ready,
        avatar: s.avatar.clone(),
        generation: s.generation,
        ..Default::default()
    }
}
pub fn update(f: impl FnOnce(&mut Presentation)) {
    f(&mut STATE.lock().unwrap());
}
pub fn speech_started(text: &str, generation: u64) {
    if generation != crate::assistant_voice::playback_generation() {
        return;
    }
    update(|s| {
        if !s.from_audio || s.spoken_generation != generation {
            s.spoken.clear();
        }
        if !s.spoken.is_empty() {
            s.spoken.push('\n');
        }
        s.spoken.push_str(text);
        s.last_sentence = text.into();
        s.spoken_generation = generation;
        s.from_audio = true;
    });
}

// Keep the generated UI API compatible while moving presentation authority out
// of the workspace. Backend call sites use these publication methods.
impl crate::AssistantChatWindow {
    pub fn present_assistant_state(&self, value: slint::SharedString) {
        update(|s| s.state = value.to_string());
        self.set_assistant_state(value);
    }
    pub fn present_spoken_text(&self, value: slint::SharedString) {
        update(|s| s.append_spoken(&value, crate::assistant_voice::playback_generation()));
        self.set_spoken_text(value);
    }
    pub fn present_selected_tab_id(&self, value: slint::SharedString) {
        update(|s| {
            if s.session != value.as_str() {
                s.spoken.clear();
                s.last_sentence.clear();
            }
            s.session = value.to_string();
        });
        self.set_selected_tab_id(value);
    }
    pub fn present_listening(&self, value: bool) {
        update(|s| {
            s.listening = value;
            if value {
                s.spoken.clear();
                s.last_sentence.clear();
            }
        });
        self.set_listening(value);
    }
    pub fn present_show_spoken_text(&self, value: bool) {
        update(|s| s.show_spoken = value);
        self.set_show_spoken_text(value);
    }
    pub fn present_speech_model_status(&self, value: slint::SharedString) {
        update(|s| s.model_status = value.to_string());
        self.set_speech_model_status(value);
    }
    pub fn present_speech_model_ready(&self, value: bool) {
        update(|s| s.model_ready = value);
        self.set_speech_model_ready(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn whole_reply_accumulates_without_repeated_timer_updates() {
        let mut state = Presentation::default();
        state.append_spoken("First sentence.", 1);
        state.append_spoken("First sentence.", 1);
        state.append_spoken("Second sentence.", 1);
        assert_eq!(state.spoken, "First sentence.\nSecond sentence.");
        state.append_spoken("New reply.", 2);
        assert_eq!(state.spoken, "New reply.");
        state.append_spoken("", 2);
        assert!(state.spoken.is_empty());
    }
    #[test]
    fn placement_rejects_nonfinite_and_extreme_values() {
        let mut config = AvatarConfig {
            scale: f32::NAN,
            ledge_width: f32::INFINITY,
            ..Default::default()
        };
        config.normalize();
        assert_eq!(config.scale, 1.0);
        assert_eq!(config.ledge_width, 420.0);
        config.scale = 9.0;
        config.ledge_width = 5.0;
        config.normalize();
        assert_eq!(config.scale, 2.0);
        assert_eq!(config.ledge_width, 160.0);
    }
    #[test]
    fn old_settings_default_to_optional_runtime() {
        let config: AvatarConfig = serde_json::from_str("{}").unwrap();
        assert!(!config.visible);
        assert!(config.runtime.is_empty());
    }
}
