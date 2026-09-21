# Echo

Echo is a Windows speech-to-text assistant written in Rust.

It supports ElevenLabs Realtime, OpenAI Realtime Whisper, and fully offline local CPU transcription with Sherpa ONNX and Silero VAD. Local CPU defaults to NVIDIA Parakeet TDT 0.6B v2 INT8 (600M parameters, English); the Speech Engine settings also offer a downloadable NVIDIA Parakeet TDT 0.6B v3 INT8 option (600M parameters, 25 languages).

## Build and run

```powershell
cargo run
```

Use **Settings > Start Echo when I sign in to Windows** to enable or disable per-user startup. Startup launches are hidden in the notification area; opening Echo normally shows the main window.

## Build the single-file Windows installer

Install Rust, [Inno Setup 6](https://jrsoftware.org/isdl.php), and
[Minisign](https://jedisct1.github.io/minisign/). Create an update signing key once,
copy `installer\update-config.example.json` to the gitignored
`installer\update-config.local.json`, and replace its placeholders. Then run:

```powershell
.\installer\build-installer.ps1
```

The standalone installer is written to `installer\output\Echo-<version>-Setup.exe`.
An upload-ready, Minisign-signed update feed is written to
`installer\output\update-bundle`. Upload the installer and signature before
uploading `manifest.json` so clients never observe a partially published release.
After adding `publish_host` and `publish_path` to the local update configuration,
the following command builds, verifies, and publishes the release over SSH:

```powershell
.\installer\build-and-publish.ps1
```

For an interactive publish that opens a console for the Minisign password, run
`installer\publish-interactive.cmd`. It writes its non-secret completion state
to `target\echo-publish-status.json`; the password is never saved or passed in
a command argument.

See [Automatic updates](docs/automatic-updates.md) for key management and server
layout. The installer is per-user, needs no administrator access, and offers
shortcuts and startup-at-sign-in options. If you later obtain an Authenticode
certificate, code-sign both the application executable and installer and set
`authenticode_required` in the update configuration.

The installer deliberately never contains the local speech model. When a user selects **Local CPU** in Settings and the model is missing, the app asks permission to download the pinned model files, shows download/extraction/verification progress, and stores them under `%LOCALAPPDATA%\11th_echo\models`. The user can then save the provider setting. The application bundles the native Sherpa ONNX runtime, so users do not need Python or a separate ONNX installation.

For development with an existing model directory, set `ELEVENTH_ECHO_MODEL_DIR` to the directory containing the Parakeet and Silero model folders. The optional model-backed smoke test can then be run with:

```powershell
cargo test local_model_transcribes_known_wav -- --ignored --nocapture
```

## Diagnostic logs

Echo writes persistent diagnostics to `%LOCALAPPDATA%\11th_echo\logs\echo.log`. Logs rotate at 5 MB and retain five older files as `echo.1.log` through `echo.5.log`.

The log records startup, instance and hotkey ownership, settings metadata, recording-session epochs, audio/provider lifecycle, transcript event character counts, model download and load status, finalization, focused-control class and process identifiers, and exact Windows `SendInput` counts/errors. It deliberately excludes API keys, raw audio, transcript text, clipboard contents, and window titles.
