# Desktop text rendering

Echo uses Slint's winit backend with Skia for normal desktop operation. FemtoVG
is deliberately excluded from Cargo features, so new windows and environment
overrides cannot select its glyph-atlas renderer. Arial is the application font;
set it explicitly on each top-level window.

Skia's OpenGL surface is preferred for desktop rendering. Slint 1.18.1's
Windows DX12/WGPU surface can render a transparent scene as an opaque black
rectangle on the desktop even when a snapshot has a correct alpha channel.
Enabling `renderer-skia-opengl` gives Skia's default surface factory an OpenGL
first choice while retaining its other surface fallbacks. Check transparency
using a native screen capture against a known background, not only a snapshot.

## Why small text looked uneven

The previous startup code explicitly selected FemtoVG. The font resolver selected
Arial, but FemtoVG rasterized its outlines into a glyph atlas and then rendered
text using textured triangles. Pixel alignment, filtering, and outline rendering
in that path produced visibly uneven letter strokes at small sizes. Changing a
window's font family did not remove that renderer.

Slint's upstream reports describe the same symptom, including one leg of a letter
rendering a pixel lower than the other:

- https://github.com/slint-ui/slint/issues/6215
- https://github.com/slint-ui/slint/issues/5177
- https://github.com/slint-ui/slint/issues/6298

Skia uses its platform font rasterization path instead of FemtoVG's outline atlas.
The software renderer remains available for diagnostics and machines where GPU
initialization fails; it does not use FemtoVG.

## Verification

Run the pipeline UI smoke test with the production renderer. It uses isolated
settings and covers the component pool, editor, all component dialogs, settings
pages, and the transcript overlay. Inspect its screenshots in the temporary
directory at each display scale:

```powershell
$env:SLINT_BACKEND = 'winit-skia'
foreach ($scale in '1', '1.25', '1.5') {
    $env:SLINT_SCALE_FACTOR = $scale
    cargo test pipeline_settings_and_preview_ui_smoke -- --ignored --nocapture --test-threads=1
}
Remove-Item Env:SLINT_BACKEND, Env:SLINT_SCALE_FACTOR
```

Also run `redesigned_ui_smoke` and
`overlay_animation_remains_frameless_ui_smoke` on Skia. Check the dependency tree
with `cargo tree -i i-slint-renderer-femtovg`; that package must be absent.
