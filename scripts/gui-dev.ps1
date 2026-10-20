param(
    [ValidateSet('mcp', 'preview', 'both')]
    [string]$Mode = 'mcp',
    [ValidateRange(1024, 65535)]
    [int]$Port = 9315,
    [switch]$Headless,
    [ValidateSet('main', 'models', 'model-creator', 'pipelines-list', 'pipeline-editor', 'pipeline-editor-live', 'assistant-pipeline-live', 'pipeline-step', 'pipeline-step-local', 'pipeline-step-model', 'hotkey', 'overlay', 'assistant-chat', 'assistant-pipeline', 'assistant-output', 'avatar-playground', 'desktop-avatar', 'voice-library', 'mcp-manager', 'agent-approval', 'component-setup', 'transcription-destination', 'assistant-interruption')]
    [string]$IsolatedWindow
)

$ErrorActionPreference = 'Stop'
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
Push-Location $repoRoot
try {
    if (-not $IsolatedWindow -and (Get-Process -Name 'echo' -ErrorAction SilentlyContinue)) {
        throw 'Echo is already running. Use -IsolatedWindow to inspect a window without touching the active session.'
    }
    if ($Mode -eq 'mcp' -or $Mode -eq 'both') {
        $env:SLINT_EMIT_DEBUG_INFO = '1'
        $env:SLINT_MCP_PORT = $Port.ToString()
    }
    if ($Mode -eq 'preview' -or $Mode -eq 'both') {
        $env:SLINT_LIVE_PREVIEW = '1'
    }
    $env:SLINT_BACKEND = if ($Headless) { 'headless-skia' } else { 'winit-skia' }

    $features = switch ($Mode) {
        'mcp' { 'slint/mcp' }
        'preview' { 'slint/live-preview' }
        'both' { 'slint/mcp,slint/live-preview' }
    }
    if ($env:ECHO_GUI_REMINDERS_INSPECT) {
        & cargo test --bin echo --features $features reminders_interactive_fixture -- --ignored --nocapture --test-threads=1
    } elseif ($env:ECHO_GUI_ACTIVITY_INSPECT) {
        & cargo test --bin echo --features $features dynamic_activity_interactive_fixture -- --ignored --nocapture --test-threads=1
    } elseif ($env:ECHO_GUI_CODEX_IMPORT_FIXTURE) {
        & cargo test --bin echo --features $features codex_import_interactive_fixture -- --ignored --nocapture --test-threads=1
    } elseif ($env:ECHO_GUI_EMPTY_HISTORY_INSPECT) {
        & cargo test --bin echo --features $features empty_history_interactive_fixture -- --ignored --nocapture --test-threads=1
    } elseif ($IsolatedWindow -eq 'desktop-avatar') {
        & cargo test --bin echo --features $features desktop_avatar_interactive_fixture -- --ignored --nocapture --test-threads=1
    } elseif ($IsolatedWindow -eq 'avatar-playground') {
        & cargo test --bin echo --features $features avatar_playground_interactive_fixture -- --ignored --nocapture --test-threads=1
    } elseif ($IsolatedWindow -eq 'assistant-interruption') {
        & cargo test --bin echo --features $features assistant_interruption_interactive_fixture -- --ignored --nocapture --test-threads=1
    } elseif ($IsolatedWindow -eq 'voice-library') {
        & cargo test --bin echo --features $features voice_library_interactive_fixture -- --ignored --nocapture --test-threads=1
    } elseif ($IsolatedWindow -eq 'transcription-destination') {
        if (-not $env:ECHO_TRANSCRIPTION_UI_INSPECT) { $env:ECHO_TRANSCRIPTION_UI_INSPECT = 'assistant' }
        & cargo test --bin echo --features $features pipeline_destination_selects_transcription_window_smoke -- --ignored --nocapture
    } elseif ($IsolatedWindow -in @('pipeline-editor-live', 'assistant-pipeline-live', 'pipeline-editor', 'assistant-pipeline')) {
        $env:ECHO_PIPELINE_INSPECT_ASSISTANT = if ($IsolatedWindow -in @('assistant-pipeline-live', 'assistant-pipeline')) { '1' } else { '0' }
        & cargo test --bin echo --features $features connected_pipeline_interactive_fixture -- --ignored --nocapture
    } elseif ($IsolatedWindow) {
        & cargo run --example gui_inspect --features $features -- $IsolatedWindow
    } else {
        & cargo run --features $features
    }
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}
finally {
    Pop-Location
}
