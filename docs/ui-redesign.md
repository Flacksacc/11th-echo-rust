# Content-first UI redesign

Echo remains a Rust/Slint application: `cargo run` and the existing installer
workflow are unchanged. The Avalonia MCP was used for layout and accessibility
guidance; its available tools provide documentation, not Slint visual automation.

The design draws on the compact navigation/content separation in
[FluentAvalonia's gallery](https://github.com/amwx/FluentAvalonia), the
keyboard-first approach of [Fluent Search](https://avaloniaui.net/success/fluent-search),
and the content-first workspace of [Lunacy](https://avaloniaui.net/blog/case-study-lunacy).
No third-party visual assets were copied.

- A 44-pixel icon rail replaces two stacked navigation bars. Hover labels and
  keyboard-accessible names explain each icon; duplicate branding is removed.
- Transcripts use one selectable, read-only multiline text control, without a
  recording/listening toolbar. The global hotkey controls dictation.
  Messages are separated by blank lines, newest
  first. Select text and use Ctrl+C; there are no per-entry controls, copy
  buttons, history counters, or history heading. Existing persisted history is
  preserved. Formatting happens once per committed message outside the UI loop.
- Activity uses the same single selectable document control. Transcripts and
  Activity have no subtitle; settings show "Settings" above the current section.
- Settings keep all existing fields and bindings, but use compact controls,
  readable supporting text and page-level scrolling, including the save action.
- Model installation, updates, and unsaved-change dialogs size to their content.
- Buttons, navigation, help, and palette choices support keyboard focus.
- Existing palettes, overlay behavior, recording gates, downloads, rewriting,
  post-processing, startup, hotkeys, and settings storage remain intact.

## Compact-layout follow-up checklist

1. Replace sidebar text buttons with icons and hover labels.
2. Remove sidebar Echo branding, the Preferences heading, and the shortcut readout.
3. Move version text to the upper-right header, with click, hover-color, and keyboard behavior.
4. Reduce page margins to four pixels and tighten toolbar/transcript padding.
   Reserve a separate 14-pixel gutter for the window-edge scrollbar so it never
   overlays a rounded content card.
5. Use one page-edge scroll container below the fixed title. Transcript cards,
   activity text, and settings forms do not own their own page scrollbars;
   multiline settings editors still scroll their editable text when necessary.
6. Reset scroll position when switching pages and verify edge scrolling with long content.

## Visual smoke checks

Run each test separately with `SLINT_BACKEND=winit-software` and
`SLINT_SCALE_FACTOR=1`. The isolated windows never record audio, download files,
or write user settings. BMP screenshots are written to the system temporary directory.

`cargo test redesigned_ui_smoke -- --ignored --nocapture` renders all pages and
major dialogs and checks navigation, native transcript text selection,
read-only behavior, and page-edge scrolling with a full 500-message fixture.
Set `ECHO_UI_SMOKE_WIDTH` / `ECHO_UI_SMOKE_HEIGHT` before launching to verify
760×600, 960×720, and 1280×900 independently. Resizing and immediately capturing
without processing window events does not reliably test the requested size.

Also run `parakeet_model_settings_ui_smoke` and
`post_processing_settings_ui_smoke` separately for provider-specific settings,
formatting options, scrolling, and installation states.

These isolated checks do not replace a release check of actual audio capture,
the global hotkey, tray actions, overlay dragging, and installation on a clean account.

## Recording icon indicator

The supplied three-frame WebP is embedded and decoded once into fixed-size
frames. While recording or finalizing, the title-bar icons, running taskbar icon,
and system-tray icon animate; idle restores the embedded Echo logo. Warm idle
microphone capture does not activate the indicator. Frames retain their original
timing with a 50 ms minimum, and unchanged frames do not trigger icon updates.
The executable and actual Start-menu shortcut remain static: Windows shortcuts
store an icon-file reference and do not support live WebP animation.

The frameless live overlay animates its in-content waveform instead of its native
window icon, avoiding non-client title-bar redraws. Its static native icon and
`no-frame` setting remain unchanged. `overlay_animation_remains_frameless_ui_smoke`
checks caption styles and stable native-icon handles over multiple content frames.

## Console responsiveness

Activity uses Segoe UI at 14 pixels, matching the transcript document, rather than
small Consolas text. Log refreshes pause while text is selected and catch up when
selection clears; diagnostic collection and persisted logs continue normally.
The 50 ms housekeeping timer no longer clones transcript history or compares all
settings while viewing Activity/Transcripts. Settings dirty-state checks still run
on Settings and explicitly on navigation/quit paths.

For source builds, dependencies (including Slint and its font, Unicode, geometry,
and raster stack) are optimized at level 2 while application code retains its
normal debug profile. The existing SHA-256-specific level-3 override remains.
This avoids the particularly expensive unoptimized full-document text shaping
during selection and scrolling without discarding history or adding per-line UI
controls. `activity_console_performance_smoke` exercises selection and scrolling
with 1,000 full-length lines and reports measured per-frame times; run separately
with `SLINT_BACKEND=winit-software` and `SLINT_SCALE_FACTOR=1`.

### Standard textbox and Ultra-loading follow-up

Transcripts and Activity use one regular read-only Slint TextInput per page,
with Arial matching the page titles and normal selection/Ctrl+C. The transcript
display is limited to the latest 100 newline-delimited lines (including blank
separators), newest message first, without deleting older saved history. Main
window, settings, hotkey dialog and overlay use the same Arial family.
The experimental Windows RichEdit
child window and its preview workaround were removed after display problems;
text now stays in the same rendering tree as dialogs, tooltips and scrolling.

Ultra recording is gated before session creation and microphone attachment.
Pressing the shortcut before its keyed engine is Ready opens a non-activating,
frameless "Model is loading" notice and creates no recording session. When ready,
the notice asks the user to press the shortcut again; recording is never started
automatically. Load failures stay visible. The user's optional warm-idle
microphone setting is unchanged; no session audio is forwarded while gated.
