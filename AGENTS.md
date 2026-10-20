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
5. Build the Inno Setup installer.

Testing on a clean Windows account or VM is optional and is not a release or publication requirement.

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

## Text rendering

- Use the Skia renderer for normal desktop operation. Keep FemtoVG disabled in
  Cargo features: its glyph-atlas rendering produces uneven small text on
  Windows, including component names in newly added windows.
- Set `default-font-family: "Arial"` on new top-level windows.
- Verify text visually with the production Skia renderer at 100%, 125%, and
  150% display scaling. Software-renderer screenshots alone do not verify the
  renderer used by the application. See `docs/text-rendering.md`.

## GUI development

- Use the official Slint skill and version-matched Slint documentation for UI work.
- For changes to UI appearance or interaction, run a development build with
  `scripts/gui-dev.ps1 -Mode mcp`, inspect the live Slint element tree, exercise
  the affected controls, and review screenshots before declaring the work done.
- Use `scripts/gui-dev.ps1 -Mode preview` for rapid visual iteration. Check
  relevant windows at minimum and normal sizes, including 100%, 125%, and 150%
  display scaling when text or clipping is involved. Keep MCP and live preview
  features out of release builds.
