# Release audit — 2026-07-20

## Decision

**Release candidate built; public publication is still gated on signing and a clean-machine acceptance test.** The code, tests, release build, and single-file installer now pass their local gates. The local speech model is never part of the installer and is offered when Local CPU is selected in Settings.

## Gate results

| Gate | Result | Evidence |
|---|---|---|
| `cargo fmt --check` | Pass | Workspace is formatted. |
| Strict Clippy | Pass | `cargo clippy --all-targets --all-features -- -D warnings`. |
| Unit tests | Pass | 81 passed; three model-backed/download tests intentionally ignored in the default suite. The local audio-closure finalization regression test was also run explicitly and passed. |
| Release build | Pass | `cargo build --release --locked`. |
| Runtime dependency check | Pass | PE imports contain Windows system DLLs only; no `VCRUNTIME140`, `MSVCP`, or separately installed ONNX runtime. |
| Inno Setup compile | Pass | Inno Setup 6.7.3 produced one per-user installer executable. |
| Installer model exclusion | Pass | The Inno compiler payload contains the executable, icons, notices, and license texts only. No model/archive path is referenced by the build script. |
| Code signing | **Pending** | Application and installer are unsigned. |
| Pristine Windows acceptance | **Pending** | Must be exercised on a clean supported Windows VM before publication. |

Candidate artifact:

- File: `installer/output/Echo-0.1.0-Setup.exe`
- Size: 11,187,708 bytes
- SHA-256: `D1698E57FABA96662E7ED1792060178A83BD625F96839ED8C95E99221E93252F`
- Signature: `NotSigned`

## Resolved findings

- Tray icons are installed and resolved relative to the executable, so startup is independent of the working directory.
- The MSVC CRT is statically linked.
- API credentials are protected with user-scoped Windows DPAPI and settings are atomically replaced. Legacy plaintext values remain readable for migration and are protected on the next save.
- Transcript/provider payloads and foreground-window titles are no longer written to production logs.
- Transcript posting uses direct Unicode `SendInput` against whichever Windows control is focused when finalization completes. Echo does not save a start-time destination or use the clipboard.
- Transcript processing owns session finalization, with epoch checks after asynchronous rewriting and before external side effects.
- Both cloud providers preserve and drain captured audio when Stop arrives during connection or before readiness; provider connection/final-response waits are time-bounded and pre-ready queues are capped.
- Session shutdown now closes the audio-forwarding receiver explicitly, exposes a distinct finalizing UI state, and force-recovers after a bounded timeout so a provider cannot strand subsequent start/stop cycles.
- Echo enforces one instance per Windows session. A duplicate launch activates the existing main window instead of competing for the global hotkey, microphone, and settings file.
- Local CPU finalization commits on either an explicit Stop command or audio-channel closure, eliminating the race that previously ended Sherpa without emitting a final transcript.
- Privacy-safe rotating diagnostic logs cover startup, session/audio/provider state, event sizes, model lifecycle, focused-control metadata, and exact `SendInput` results without storing API keys, raw audio, transcript text, clipboard contents, or window titles.
- Gemini requests have connect and request timeouts.
- Runtime model downloads are HTTPS-only, redirect-limited, size-bounded, timeout-bounded, staged through partial files, SHA-256 verified before activation, and stale abandoned work is cleaned safely.
- Windows startup command quoting is covered by tests.
- Uninstall offers an explicit choice to remove runtime-downloaded models.

## Remaining release work

1. Choose and document the project’s top-level distribution license and review the third-party notice set with counsel or the release owner.
2. Sign and timestamp both the application executable and installer with the publisher certificate, then regenerate and record the artifact hash.
3. Run the clean-machine matrix below. The current candidate should not be publicly posted until this succeeds.
4. Consider a future graceful upgrade-shutdown protocol for the hidden tray process. Inno Setup can request closure, but an explicit application-level shutdown handshake would make upgrades more deterministic.
5. Local CPU decode is performed off the async runtime, but an in-flight decode cannot be preempted immediately; Stop completes after that decode returns.

## Required clean-machine checks

1. Install as a standard user on a pristine supported Windows VM.
2. Launch from Start Menu, desktop shortcut, direct executable, and Windows startup from a path containing spaces.
3. Exercise tray, Quit, hotkey conflicts, every settings page, overlay movement, microphone denial/change, and settings persistence.
4. Exercise ElevenLabs and OpenAI start/stop/error paths with valid and invalid credentials.
5. Select Local CPU in Settings and confirm the consent prompt, progress, successful verified installation, explicit settings save, transcription, and offline reuse after restarting with the network disconnected.
6. Interrupt and retry the model download; exercise corrupt, oversized, stalled, and redirected response cases where practical.
7. Change foreground windows while a transcription/rewrite is pending and confirm text is not injected into the wrong application.
8. Upgrade while visible, hidden in the tray, and recording.
9. Uninstall and verify shortcuts, startup registration, binaries, and both model-retention choices.
