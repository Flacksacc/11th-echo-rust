#[derive(Debug, Clone)]
pub enum RecordingState {
    Idle,
    BufferingPreConnect,
    Connecting,
    Recording,
    Finalizing,
    Error,
}

impl RecordingState {
    pub fn can_start(&self) -> bool {
        matches!(self, RecordingState::Idle | RecordingState::Error)
    }

    pub fn can_stop(&self) -> bool {
        matches!(
            self,
            RecordingState::BufferingPreConnect
                | RecordingState::Connecting
                | RecordingState::Recording
        )
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
        if !matches!(self, RecordingState::Finalizing) {
            *self = RecordingState::Finalizing;
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

#[cfg(test)]
mod tests {
    use super::RecordingState;

    #[test]
    fn start_stop_guards_work() {
        let mut state = RecordingState::Idle;
        assert!(state.can_start());
        assert!(!state.can_stop());

        state = RecordingState::BufferingPreConnect;
        assert!(!state.can_start());
        assert!(state.can_stop());
    }

    #[test]
    fn transitions_follow_expected_path() {
        let mut state = RecordingState::BufferingPreConnect;
        state.transition_to_connecting();
        assert!(matches!(state, RecordingState::Connecting));
        state.transition_to_recording();
        assert!(matches!(state, RecordingState::Recording));
        state.transition_to_finalizing();
        assert!(matches!(state, RecordingState::Finalizing));
        state.transition_to_idle();
        assert!(matches!(state, RecordingState::Idle));
    }

    #[test]
    fn connecting_transition_is_noop_from_idle() {
        let mut state = RecordingState::Idle;
        state.transition_to_connecting();
        assert!(matches!(state, RecordingState::Idle));
    }

    #[test]
    fn recording_transition_allows_from_connecting() {
        let mut state = RecordingState::Connecting;
        state.transition_to_recording();
        assert!(matches!(state, RecordingState::Recording));
    }
}
