# Workspace / desktop-avatar validation

The conversation workspace and desktop companion have independent lifetimes and positions. Closing the workspace hides it; Hermes sessions and the companion remain active. Production presentation state is shared without making the workspace responsible for rendering the character.

## Checks performed

- Production Skia workspace at 380×520 and 980×720 logical pixels with 100%, 125%, and 150% scale overrides: visible bottom tabs, new-tab reveal, composer, settings, loading warnings, and rich history.
- Synthetic workload: 50 busy tabs, 120 paged rich blocks, 465,120 bytes. Incremental update-to-frame median 18.13 ms, p95 19.23 ms, maximum 213.05 ms. MCP scrolling median 4.51 ms, p95 26.67 ms, maximum 27.35 ms; two rich rows instantiated. These measurements include IPC/frame scheduling and concurrent builds; they do not represent 50 real model workers or guarantee zero slowdown for unbounded data.
- Isolated production desktop-avatar controller: native workspace close leaves avatar visible; transparent empty regions exclude mouse input; settings, placement, sit request, and save/show controls work without writing user settings.
- Whole Unicode speech replies preserve every byte in bounded display rows. Display rows are independent of sentence streaming and PCM playback.
- Production Skia companion at 100%, 125%, and 150% scale overrides, with short and long Unicode replies: scrolling, native hit regions, and readable text checked; two speech-text rows instantiated in each case.
- UE 5.8.2 Game and Editor targets compile; BuildCookRun produces a standalone Win64 package. A packaged diagnostic GPU tile presents through DirectComposition, and synthetic mono PCM produces an `EchoSpeech` native facial-animation frame. Transport, GPU rendering, and facial data readiness are separate states.

The physical display remained at 125%. Scale overrides exercise layout and Skia text rendering; actual Windows monitor/DPI transitions remain a separate native check.

PCM is observed as playback advances, with an 80 ms lookahead, rather than filling the animation queue with a whole generated sentence. A ten-second synthetic utterance regression checks that initial delivery stays bounded and the complete tail reaches the observer.

## Pending supplied assets

No character or animation library is included. The supplied MetaHuman must be cooked into the companion or a compatible .pak, with the animation graph bound to the exported state and `EchoSpeech` Live Link subject. Character lighting, alpha/hair edges, foot grounding, sitting anchor, gesture blending, cloth/hair physics, and audible mouth alignment require that real asset. The diagnostic tile does not validate those properties.

The ordinary Echo installer excludes Unreal. Keep the entire optional `target/unreal-avatar/Windows` directory together; use [the companion setup instructions](../integration/unreal/README.md). Private validation artifacts are not published to the website.

Final checks passed: `cargo fmt --check`, `cargo check`, strict Clippy for all targets/features, 284 tests (49 environment/manual tests ignored), optimized Echo build, and Inno Setup compilation. Private artifacts are `target/installer-validation/Echo-Workspace-Avatar.exe` and `Echo-Workspace-Avatar-Setup.exe`; the executable copy's SHA-256 matches `target/release/echo.exe`. Test fixtures are closed, and the user's existing Echo process was left untouched. No clean-account installation or actual character acceptance is claimed.

The user subsequently requested normal-build delivery rather than separate private packages. These changes are part of the normal source and `target/release/echo.exe`. The standard `installer/output/Echo-0.1.10-Setup.exe` was rebuilt, and the standard per-user installed executable was updated with a hash-verified copy and a timestamped rollback backup. Existing settings, model files, and the running Echo session were preserved; the updated executable takes effect on the next restart. Continued assistant development should use the normal build and protect transcription functionality without forcing disruptive restarts. Website publication remains separate.

## Simplified workspace and inline recording

Removed the large task title, description, repeated task strip, inline conversation-name editor, Output/Updates switch, and Mic button. Hidden conversations are available through gear → Conversations…; task controls and bottom conversation tabs remain.

Live transcription now occupies a user-message row inside the same virtualized history in both Hermes and legacy modes. Starting recording selects the tail, and changes in live-message height keep the tail visible. The temporary row disappears when existing transcription finalization clears the live text. Background history updates retain normal reading-position behavior outside recording.

Production Skia MCP checks passed at minimum/normal sizes and 100%/125%/150% scale overrides: inline first sentence inside the history, removed controls absent, Conversations menu actionable, loading warnings and new tabs visible. A dedicated recording-follow regression passed with 100 existing messages and a growing Unicode paragraph in both modes. Real local-model sample decoding and queued-audio Stop/finalization checks also passed. Full tests: 284 passed, 50 ignored (the new isolated layout test was exercised separately).

Busy synthetic workload still instantiated two rich rows. Update-to-frame median 18.22 ms, p95 18.97 ms, maximum 19.58 ms; MCP scrolling median 14.06 ms, p95 25.44 ms, maximum 26.11 ms while builds were running. These measurements include IPC/frame scheduling. Physical display scaling remained 125%.

## Empty native history bubbles

The history bridge can return empty records (including NULL/tool-call-only content), and streaming starts with an empty tail. The UI previously projected these into cards labeled Assistant. The Rust history projection now suppresses empty and whitespace-only text rows while retaining native source records and cursor/stream state. Real replies appear on their first nonempty update.

The focused regression verifies empty tool records, whitespace, the empty-to-first-token transition, subsequent completion, and an empty-only page. Full tests: 285 passed, 51 ignored; strict all-target/all-feature Clippy, formatting, and cargo check passed. A synthetic production winit/Skia MCP fixture verified zero Assistant bubbles initially and one populated bubble after text arrived; before/after screenshots were reviewed at the physical 125% display scale. No private conversation contents were inspected. Earlier workspace size/scale matrix remains applicable to unchanged styling.

Optimized build and standard Inno Setup compilation passed. The standard installed executable was replaced with a hash-verified copy and a rollback backup; active Echo PID 112452 was preserved. Restart when convenient to load the fix. No website publication.

## Creating multiple native conversations

Echo previously submitted the same native title, Echo Assistant, for every new conversation. Hermes's title uniqueness rule rejected later creations with HTTP 400, preventing the + button from adding a tab. The client now supplies a unique native session ID and title while leaving short visible tab names independent. The title prefix is shortened by Unicode characters to fit Hermes's 100-character limit with the identity suffix intact.

A Rust HTTP regression creates three conversations with the same requested name, verifies distinct native IDs/titles and selects Chat 3, then checks a long Unicode title. The installed Hermes API was exercised in a disposable home: the old duplicate title returned 400, while three unique Echo session titles returned 201. No user history, models, or credentials were changed. Production Skia MCP layout fixtures exercise the + callback, tab appearance and selection at minimum/normal sizes and 100%/125%/150% scale overrides; the callback fixture uses synthetic tab data, with native creation verified separately.

Final checks passed: formatting, cargo check, strict Clippy for all targets/features, 286 tests (51 ignored), optimized build and standard Inno Setup compilation. One test-build attempt encountered a Windows link lock from the live GUI fixture; after closing owned fixtures, the full test rerun passed. Normal installed Echo was hash-verified against the release build, with rollback backup echo.before-update-20261011-170844.exe; running Echo PID 99600 was preserved. The fix loads next restart; no website publication.

## Codex conversation imports

The settings-gear picker uses a separate Slint window with production Skia text,
profile discovery/custom roots, title/project filtering, archived histories,
bounded catalog/preview pages and a cancellable background import. Completed
imports use new native Hermes sessions; the existing virtualized history renders
Markdown and code. No transcription/audio implementation was changed.

Synthetic checks exercise the actual gear callback, picker row selection,
preview paging and new native tab creation. Screenshots were reviewed at normal
and minimum sizes with 100%, 125% and 150% renderer scale overrides. Minimum-size
captures use physical dimensions matching the selected render scale; Windows
monitor scaling remains 125%. The managed bridge and native SessionDB run in a
disposable home; no private conversation was displayed or model called.

Import tests cover source preservation, duplicate event handling, legacy event
fallback, repeated imports, archived/custom profiles, malformed records,
path containment, cancellation before/after writes, interrupted-stage recovery,
local/embedded/missing images and safe errors. A synthetic rollout over 100 MiB
with 26,000 distinct response identities previews a late page with less than
4 MiB of traced Python allocations. This is a parser measurement, not an overall
process-memory or model-context guarantee. Identity deduplication uses an
anonymous disk-backed SQLite index with a bounded cache.

A native `/v1/runs` continuation receives the imported context with only the
provider boundary scripted. Native storage compaction preserves browsable
original history and permits subsequent turns. Live model response quality and
model-generated compaction summaries are not assessed by these synthetic tests.

Final verification for the import feature: formatting, cargo check, strict
all-target/all-feature Clippy, 286 Rust tests (52 ignored), and the optimized
build passed. Python integration checks passed in fresh disposable homes:
75 passed, two optional checks skipped, including 18 import regressions. The
native compatibility probe also passed. A broad first run lacked the isolated
home prerequisite; its fixture failures were resolved by running each test
module in a fresh initialized native home. No production code was changed for
those fixture failures.

The normal installed executable matches the final release SHA-256, with
rollback backup `echo.before-update-20261011-175844.exe`. Active Echo was not
restarted; the new importer loads on the user's next restart. No website
publication. Automatic approval review blocked recursive removal of the
synthetic GUI temporary directory; the owned helper was stopped and the
temporary files were retained.

## Dynamic activity bubbles

Consecutive native task events now update one assistant message, preserving its
identity and earliest history cursor. Real user/assistant messages separate
activity groups. Live runs also show thinking/tool/approval activity, and each
native interim-message boundary closes the current streamed reply before the
next reply begins. Updating an older streaming tail finds it by identity rather
than inserting duplicate rows when activity follows it.

Regression tests apply 998 status updates without growing the visible list,
then insert a reply and verify that subsequent activity occupies a new bubble.
History projection coalesces existing native events after reload. Tool-result
history uses compact completion text from bounded metadata; full native result
content and export remain intact. The production Skia fixture exercises changing
status, a stable real reply, then another changing status via the actual Slint
model and live element tree. Screenshots show normal assistant message cards.

Final checks: formatting, cargo check, strict Clippy, 288 Rust tests (53 ignored),
76 Python integration tests (two optional skipped), and release build passed.
The dynamic GUI fixture is exercised separately from the ignored test count.
No ASR implementation, user settings, native task execution or speech playback
implementation changed. Checks use synthetic data and a scripted/native boundary;
a live provider task is not submitted for this presentation change.

Production Skia renders and live element-tree checks passed at 100%, 125% and
150% scale overrides at normal sizes; physical monitor remained 125%. The
standard installer compiled successfully. Installed Echo matches the optimized
build by SHA-256; rollback copy is `echo.before-update-20261011-181228.exe`.
The active Echo process was preserved; restart when convenient to load this
change. No website publication.

## Activity cancellation and direct hotkey capture (2026-10-13)

The changing activity bubble exposes an X tied to its exact native run or worker
identity. Stopping one task leaves other tasks running; late stop feedback cannot
overwrite a newer run's message. Pending cancellation hides the X and shows
Stopping until native confirmation. Pipeline hotkeys are captured directly from
key events, with registered Echo shortcuts consumed by the editor while focused.

Final verification: formatting, cargo check, strict all-target/all-feature Clippy,
309 Rust tests (54 ignored), 80 Python integration tests (two optional skipped),
release build and standard Inno installer passed. Native API tests exercise two
concurrent runs and cancellation isolation. Production Skia MCP fixtures exercise
the X, stable activity/reply groups, hotkey capture, clear, Escape, Ctrl+C and Save.
Scale overrides at 100%, 125% and 150% passed on a physical 125% display. These
checks do not establish OS DPI transitions, physical global-shortcut capture or
a paid live assistant task. No transcription processing changes were made.

The normal installed executable matches release SHA-256
`D31AAB46AB8A3423E90D12B3625E1D57277951A8F3A11EB1D2D412FFD51B9962`;
rollback copy is `echo.before-update-20261013-211038.exe`. Active Echo was preserved;
the update loads on the user's next restart. No website publication.
