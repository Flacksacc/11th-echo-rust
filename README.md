# Echo

Echo is a Windows speech-to-text assistant written in Rust.

It supports ElevenLabs Realtime, OpenAI Realtime Whisper, and fully offline local CPU transcription with Sherpa ONNX, Silero VAD, and NVIDIA Parakeet TDT 0.6B v2 INT8.

## Build and run

```powershell
cargo run
```

Use **Settings > Start Echo when I sign in to Windows** to enable or disable per-user startup. Startup launches are hidden in the notification area; opening Echo normally shows the main window.

## Build the single-file Windows installer

Install Rust and [Inno Setup 6](https://jrsoftware.org/isdl.php), then run:

```powershell
.\installer\build-installer.ps1
```

The signed-or-unsigned standalone installer is written to `installer\output\Echo-0.1.0-Setup.exe`. The installer is per-user, needs no administrator access, and offers shortcuts and startup-at-sign-in options. For public distribution, code-sign both the application executable and the installer with your organization’s certificate.

The installer deliberately never contains the local speech model. When a user selects **Local CPU** in Settings and the model is missing, the app asks permission to download the pinned model files, shows download/extraction/verification progress, and stores them under `%LOCALAPPDATA%\11th_echo\models`. The user can then save the provider setting. The application bundles the native Sherpa ONNX runtime, so users do not need Python or a separate ONNX installation.

For development with an existing model directory, set `ELEVENTH_ECHO_MODEL_DIR` to the directory containing the Parakeet and Silero model folders. The optional model-backed smoke test can then be run with:

```powershell
cargo test local_model_transcribes_known_wav -- --ignored --nocapture
```

## Diagnostic logs

Echo writes persistent diagnostics to `%LOCALAPPDATA%\11th_echo\logs\echo.log`. Logs rotate at 5 MB and retain five older files as `echo.1.log` through `echo.5.log`.

The log records startup, instance and hotkey ownership, settings metadata, recording-session epochs, audio/provider lifecycle, transcript event character counts, model download and load status, finalization, focused-control class and process identifiers, and exact Windows `SendInput` counts/errors. It deliberately excludes API keys, raw audio, transcript text, clipboard contents, and window titles.
