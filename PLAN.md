# Implementation Plan - 11th Echo Rust

Based on analysis from 2026-02-15.

## Phase 1: State Machine & Safety (Critical)
- [ ] **Fix Audio Callback**: Replace `blocking_send` with `try_send` in `src/audio.rs` to prevent freezing the audio thread.
- [ ] **State Machine Refactor**: Ensure `RecordingState` in `src/state.rs` is the single source of truth.
- [ ] **Fix Stop Race Condition**: Modify `src/main.rs` to transition `Recording -> Finalizing` on stop, wait for 11Labs final transcript, then `-> Idle`.

## Phase 2: Circular Audio Buffer
- [ ] **Implement Ring Buffer**: Create a buffer (e.g., 2-5 seconds) to hold audio samples.
- [ ] **Pre-Connect Buffering**: Start capturing to buffer immediately on hotkey press.
- [ ] **Flush on Connect**: When WebSocket connects, send buffered audio first.

## Phase 3: UI Improvements
- [ ] **Transcript Display**: Add a text area to `ui/appwindow.slint` to show real-time transcripts.
- [ ] **Popup Overlay**: (Optional) Add a small overlay for "Recording..." state.
- [ ] **Transcript Accumulation**: Accumulate text in `src/main.rs` and update UI.

## Phase 4: Finalization Flow
- [ ] **Wait for Final**: Ensure the app waits for the "final" transcript event from 11Labs before injecting text.
- [ ] **Inject Logic**: Only trigger `injector.rs` after the session is fully complete.
