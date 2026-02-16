#[derive(Debug, Clone)]
pub enum RecordingState {
    Idle,
    BufferingPreConnect,
    Connecting,
    Recording,
    Finalizing { pending_injections: usize },
    Error(String),
}

impl RecordingState {
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            RecordingState::BufferingPreConnect
                | RecordingState::Connecting
                | RecordingState::Recording
                | RecordingState::Finalizing { .. }
        )
    }

    pub fn can_start(&self) -> bool {
        matches!(self, RecordingState::Idle | RecordingState::Error(_))
    }

    pub fn can_stop(&self) -> bool {
        matches!(
            self,
            RecordingState::BufferingPreConnect
                | RecordingState::Connecting
                | RecordingState::Recording
        )
    }

    pub fn is_finalizing(&self) -> bool {
        matches!(self, RecordingState::Finalizing { .. })
    }

    pub fn transition_to_connecting(&mut self) {
        if matches!(self, RecordingState::BufferingPreConnect) {
            *self = RecordingState::Connecting;
        }
    }

    pub fn transition_to_recording(&mut self) {
        if matches!(
            self,
            RecordingState::BufferingPreConnect | RecordingState::Connecting
        ) {
            *self = RecordingState::Recording;
        }
    }

    pub fn transition_to_finalizing(&mut self) {
        if !matches!(self, RecordingState::Finalizing { .. }) {
            *self = RecordingState::Finalizing {
                pending_injections: 0,
            };
        }
    }

    pub fn begin_injection(&mut self) {
        if let RecordingState::Finalizing { pending_injections } = self {
            *pending_injections += 1;
        }
    }

    pub fn finish_injection(&mut self) {
        if let RecordingState::Finalizing { pending_injections } = self {
            *pending_injections = pending_injections.saturating_sub(1);
        }
    }

    pub fn transition_to_idle(&mut self) {
        *self = RecordingState::Idle;
    }
}

impl Default for RecordingState {
    fn default() -> Self {
        RecordingState::Idle
    }
}
