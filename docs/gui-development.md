# Slint GUI development

Echo uses Slint 1.18.1 and its Skia renderer. For UI changes, use the official
[Slint AI skill](https://docs.slint.dev/latest/docs/slint/guide/tooling/ai-coding-assistants/)
and inspect a rendered window and its interactions before considering the change
complete.

Start the application with its embedded MCP server from PowerShell:

```powershell
.\scripts\gui-dev.ps1 -Mode mcp
```

The server listens locally at `http://127.0.0.1:9315/mcp`. This trusted
repository's `.codex/config.toml` registers it with Codex; restart Codex after
adding the configuration. Use `-Port 9315` to choose a port, and update the
Codex URL if you change it. `-Headless` uses software-rasterized Skia when no
desktop is available. Don't run a second Echo instance against an active user's
settings or replace an instance with unsaved UI edits.

For safe layout inspection while Echo is already running, open an isolated
Slint window that does not load user settings, register hotkeys, or start audio:

```powershell
.\scripts\gui-dev.ps1 -Mode mcp -IsolatedWindow models
```

Other choices are `main`, `pipeline-editor`, `pipeline-step`, `hotkey`, and
`overlay`. This harness previews layout and built-in widget interactions;
use the full application for callbacks and end-to-end behavior.

For live layout iteration, run:

```powershell
.\scripts\gui-dev.ps1 -Mode preview
```

`-Mode both` enables the MCP server and live preview together. These modes set
`SLINT_EMIT_DEBUG_INFO`, `SLINT_MCP_PORT`, or `SLINT_LIVE_PREVIEW` as needed and
compile with `slint/mcp` or `slint/live-preview`. Normal `cargo build` and
release builds do not enable either feature.

Inspect the Slint element tree, click and drag affected controls, and capture
screenshots of the resulting states. Review clipping, text rendering, spacing,
focus, and controls at minimum and normal window sizes. For text and clipping
changes, also verify the production Skia renderer at 100%, 125%, and 150%
Windows scaling. See [text-rendering.md](text-rendering.md).
Set `$env:SLINT_SCALE_FACTOR` to `1.25` or `1.5` before launching the isolated
inspector to repeat the visual check at those scales.

The main window has a fixed 26px resource footer. Its version link still opens
updates; CPU, GPU, RAM and VRAM values refresh on a background thread about once
a second without elevation. The `main` inspector runs the real resource sampler.
Measurements include Echo's process and current descendants, not independently
running Ollama servers or remote providers. CPU is normalized to total logical
CPU capacity. GPU is the busiest engine, adding Echo/helper usage of that engine
without adding parallel engines together. RAM is private resident working-set
memory; VRAM is Windows-attributed dedicated GPU memory across adapters. These
are sampled estimates, and short-lived helpers can exit between samples.
Unavailable readings show a dash (including private RAM on Windows versions
without `PROCESS_MEMORY_COUNTERS_EX2` support, or GPU counters unsupported by
the driver). Accumulated CPU/GPU execution times are intentionally omitted.

For pipeline interactions using the production editor callbacks with synthetic
models and temporary settings, use `-IsolatedWindow pipeline-editor-live` or
`-IsolatedWindow assistant-pipeline-live`. These development-only inspectors do
not load user settings, register hotkeys, or start microphone recording. Their
model entries are synthetic; do not invoke model tests or voice generation during
layout inspection. Add, Change, movement, removal, Undo, and component dialogs
exercise the actual editor implementation.
