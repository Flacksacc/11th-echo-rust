# Repository guidance for Codex

## Project shape

- `src/` contains the Rust application, audio pipeline, provider implementations, settings, startup integration, and text injection.
- `ui/appwindow.slint` defines the main window, settings, hotkey capture window, and live transcript overlay.
- `installer/` contains the Inno Setup release packaging.
- `docs/` contains implementation and release notes.

## Required verification

For ordinary code changes, run the narrowest relevant tests plus `cargo check`.
Before release, run:

1. `cargo fmt --check`
2. `cargo clippy --all-targets --all-features -- -D warnings`
3. `cargo test`
4. `cargo build --release`
5. Build the Inno Setup installer and test it on a clean Windows account or VM.

Do not treat compilation alone as UI verification. Exercise the main window, every settings page, the tray menu, the global hotkey, the overlay, and local-model download behavior.

## Review expectations

Use `docs/code-review.md` for release and PR reviews. Report findings before proposing broad refactors. Findings must include severity, a concrete failure scenario, and file/line evidence. Do not report preferences as defects.

For release reviews, delegate independent read-only passes in parallel when subagents are available:

- correctness and concurrency
- security and privacy
- Windows runtime and installer packaging
- tests and release readiness

The primary agent must deduplicate findings, verify high-severity claims, and produce one prioritized release report. Parallel agents should not edit the same files during review.

## Safety and compatibility

- Preserve user settings and unrelated working-tree changes.
- Never log API keys, raw audio, or full private transcripts without an explicit diagnostic opt-in.
- Downloads must use HTTPS, verify expected hashes, and resist archive path traversal.
- Keep the program usable without Python, a separately installed ONNX runtime, or developer tools.
- Startup behavior must remain per-user and reversible.
- Installer contents must match the runtime's asset and DLL discovery rules.
