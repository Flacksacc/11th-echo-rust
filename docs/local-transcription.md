# Local CPU transcription

The `local_sherpa_onnx` provider runs NVIDIA Parakeet TDT 0.6B v2 INT8 and Silero VAD through the statically linked `sherpa-onnx` Rust crate. It does not use Python, a separately installed ONNX Runtime, an API key, or a network service.

The hotkey remains the recording boundary. Silero may close and stabilize multiple speech phrases during one recording, but pauses never inject text or end the session. Active speech is re-decoded at the configured provisional interval. Pressing the hotkey again finalizes the trailing phrase, optionally runs the three-minute-capped full-session accuracy pass, applies the configured Gemini rewrite once, and injects the complete text.

If verified model files already exist, the model preloads in the background when this provider is selected. Pressing the hotkey while it loads arms recording and displays a loading message; pressing it again cancels the armed recording without cancelling the load. Local failures never fall back to a cloud provider.

The model is never included in the installer. Selecting Local CPU in Settings immediately offers to download approximately 461 MiB into `%LOCALAPPDATA%\11th_echo\models`; the provider setting is saved only when the user selects **Save settings**. The app displays download/extraction/verification progress, requires HTTPS, enforces size and timeout limits, and verifies the pinned archive and every installed model hash before activation. A failed operation cleans up partial work, leaves any previous valid package intact, and offers retry instead of switching to a cloud provider.

Model sources, exact sizes, hashes, and runtime download safeguards are defined in `src/transcription/local_sherpa.rs`. `installer/build-installer.ps1` produces the single-file application installer without downloading or packaging model assets.
