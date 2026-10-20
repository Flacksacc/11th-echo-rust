# Echo workspace and desktop avatar

Echo now separates the conversation workspace from the desktop companion. The workspace is a conventional opaque, resizable charcoal/blue-gray window, with native Hermes conversations, rich chat cards, a composer, and bottom tabs. The + creates a native session; Ctrl+1 through Ctrl+9 selects a visible tab. History remains virtualized and paged. Closing or minimizing the workspace leaves the companion and background tasks running.

Open the workspace gear menu > **Show desktop avatar**. **Desktop avatar settings…** configures the optional Unreal executable, prepared .pak, character scale, and walking range. Right-click the avatar to reopen conversations, configure it, place its invisible ledge, request sit/stand, dismiss the speech bubble, or hide it. Its position is independent of the workspace. The PNG portrait remains the fallback until a 3D character actually renders. PNGs do not acquire 3D pose or walking animations.

The avatar shows the selected conversation's activity and a scrollable bubble containing the spoken reply. Bubble display rows are virtualized; they do not change sentence-level streaming TTS. Voice input stops playback and mouth animation without cancelling background tasks. Model loading warnings remain visible; ordinary readiness stays in settings.

The Unreal foundation is optional and is not bundled into the ordinary Echo installer. See [Unreal companion setup](../integration/unreal/README.md). No 3D model or animations are included. Final character, facial-quality, physics, and grounding acceptance requires the user's supplied compatible pack.

---

# Echo Assistant

The workspace's gear menu opens speech volume, sentence speed, spoken-text visibility, model diagnostics, the Avatar playground, desktop-avatar settings, and **Conversations…** for reopening hidden conversations. Use the assistant hotkey to start speech input; type a message and press Enter or Send.

The workspace keeps the native title bar and settings gear, without repeated task headings, a description, an inline conversation-name editor, an Output/Updates switch, or a microphone button. Starting voice input jumps the conversation to the bottom and adds a live **You** message directly to the same scrolling history. The text grows there while transcription arrives; normal reading position remains unchanged by background updates when you are not recording.

Hermes conversations have a visible tab strip. Click **+** to create and select
a new native Hermes session while the other conversations and tasks continue.
Each native session gets a unique title because Hermes rejects duplicate titles;
Echo's visible tab names remain the short Chat names and can be customized.
The selected tab is brought into view, including in narrow windows. Existing
drafts and scroll positions are saved when leaving a tab. **Pocket TTS ready**
appears in the gear menu; loading and failure messages remain visible in the
main assistant view.

Both conversation views use virtualized lists with visible vertical scrollbars.
Streaming updates preserve existing rows, update the changed tail, and follow
the latest text only when the user is already at the bottom. Reading earlier
messages is not interrupted by arriving text. Native history remains paged at
120 rich-text blocks; offscreen history is not instantiated as UI elements.

Empty native history records, including tool-call-only records and empty
streaming tails, do not create chat bubbles. Their native history and cursors
are retained; a streamed reply becomes visible when its first text arrives.

Open **Avatar creation & playground** from **Settings → Text to speech**, or
**Avatar playground…** from the assistant's gear. Upload a PNG (or WebP) to
preview the portrait, edit the test phrase to see the assistant's speech bubble,
and switch between light and dark preview backgrounds. **Use avatar in assistant**
saves the portrait and updates the assistant immediately; closing without applying
leaves its portrait unchanged.

The playground's **Voice** dropdown lists Alba and your managed voices. **Test
voice** speaks the phrase through the configured speaker using Echo's current
volume and sentence speed; **Stop** cancels playback. Choosing a playground voice
does not change the settings default or Speech-node overrides. **Set up voices…**
opens the existing Pocket TTS voice library, where you can import, record, rename,
export, delete, and select the default voice. Library changes refresh the
playground dropdown automatically. Install Pocket TTS there if it is not installed.

Add an **Agent** from the node palette to use tools. Choose a tested text model,
then enable the individual tools directly on the node. Wire Transcription Text
to Agent Prompt, Agent Data to Assistant display Data, Agent Assistant to Speech
Text, and Agent Files to Assistant display Files. The optional Assistant input on
the display also receives the spoken response. Ordinary LLM nodes have an inline
Assistant response switch for the same three outputs without tools.

One streamed model response supplies speech first, the full written answer second,
and attachments last. Completed spoken sentences start Pocket TTS while the model
continues writing. Short plain answers are spoken in full; longer or structured
answers use a brief summary and a pointer to the written response. The board's
optional Spoken text bubble follows audio playback. Files appear beneath their
message, with Open, Save as, and Folder actions. Generated files use unique private
folders and remain available with the conversation; existing attached files are retained.

Settings → Connections now has **Tools, search & MCP servers** and **Install or
manage components**. Enter a Brave Search API key for current web information,
including weather searches. Windows inspection and file reading run automatically;
commands, file changes, browser navigation, and desktop actions require review.
Approval can cover one action or the same tool and target for the current turn.
Selected managed inspection tools run automatically; arbitrary MCP tools require
review even when their server labels them read-only. Interrupting the assistant
cancels model generation, tools, pending approvals, and speech.

The shared setup wizard also runs after a fresh interactive installation. Only CPU
Parakeet v2 and offline punctuation/casing are selected initially. Pocket TTS, custom
voice import, other transcription models, Laya, browser automation, and Windows
desktop control are optional. Selections are saved for later repair. Standard Pocket
TTS uses a public verified checkpoint and needs no Hugging Face token. Custom voice
import uses the gated checkpoint and requires a token. Browser automation downloads
private Node and Chromium runtimes; desktop control downloads Cua Driver. These
helpers need no separately installed Python or developer tools. Each download is
verified against its pinned SHA-256 or npm lockfile integrity before use.

Laya installed through Components uses Echo's own CPU Ollaya service on loopback
port 11436 with a private model store. It is registered as a decision model for Router
nodes. Existing external Ollaya connections on port 11435 continue to work. Managed
MCP helpers and their child processes stop when their run ends. For image-producing
tools, enable Image input on the Agent and select a model that supports images.

Activate the assistant again while it is thinking or speaking to interrupt the
current turn and start listening for a new request. This works with the assistant
hotkey and assistant graph hotkey. Activating while already
listening stops recording and submits the request. Partial interrupted replies
remain visible, labeled Interrupted, and are excluded from future model context.

Use the assistant gear's **Sentence speed** −/+ controls to choose
0.75× through 2× speed in 0.25× steps. Changes apply to the next spoken sentence.

Choose playback speakers in Settings under Audio and shortcuts. Follow the Windows
default speakers, or select a specific output and save. Assistant speech and voice
previews use the saved output on their next playback. Test speakers plays a short
tone on the current settings selection without needing Pocket TTS or a voice sample.
Unavailable selected devices report an error instead of playing through another
output. Spoken answers still require a connected Speech node, Pocket TTS assets,
and either the standard Alba voice or a saved custom voice.

Each Speech node has a compact inline Voice selector and startup-loading checkbox.
Choose **Use settings default**, **Alba (standard)**, or a named library voice.
Manage voices and test speech in Settings → Text to speech. Nodes have no action
buttons or speech testing sections. Installed Pocket TTS assets are required.

The assistant runs graphs from the same node graph list as recording. Every graph
owns its own optional hotkey; graph wiring determines whether a run pastes into
the captured element or displays a response on the board. Typed messages enter at
the selected graph's Transcription output. Assistant display receives text and has
no output; reasoning belongs in separate LLM and Router nodes.

Use Settings → Pipelines to connect transcription, prompt text, processors,
routers, models, and destinations. Router options are supplied through Text wires;
each option creates a paired output. Route alternative answers to separate ending
nodes so only one ending activates. Speech is an independent terminal node.
Direct LLM connections retain streamed display and sentence speech; whole-text
processors wait for completion. See [Node graphs](pipelines.md).

Ollaya decision models use the same editor. Select `ollaya` when creating a model component, use `laya:en`, and point it at `http://127.0.0.1:11435` (the default when the endpoint is empty). The connection test calls `/v1/systemone`; model discovery calls `/v1/models`. Drop Laya onto the router, then assign text models to the Direct and Delegate cards. Laya produces a typed choice rather than a reply. Direct requires normalized confidence of at least 0.8; low confidence, oversized requests, and decision-server errors use the delegated answer model. This starting threshold needs calibration against actual requests. Answer generation retains the full conversation; only routing uses a compact history. Decision models cannot be inserted in transcription chains or answer slots.

With Ollaya installed and serving, `echo.exe --connect-laya` verifies a real local decision and registers the component. Close the running Echo first, because the command holds its single-instance guard while saving. It updates the active assistant router, preserving its previous text router as the direct answer model, or creates a `Laya assistant` from the active speech component and an existing tested text model. Existing dictation settings are preserved. Local-model startup loading uses Ollaya's native `/api/decide` load request. Ollaya remains a separate per-user service; Echo does not silently download or launch it. See the [Ollaya API reference](https://ollaya.dev/docs/api).

For spoken replies, install Pocket TTS in Settings → Text to speech → Manage voices / test speech, then connect a Speech node and save the graph. Voice cloning may require access to the gated `kyutai/pocket-tts` checkpoint; any required Hugging Face token is protected with per-user Windows DPAPI. Echo downloads pinned assets over HTTPS and verifies SHA-256 checksums. The local Rust/Candle runtime needs no separate Python installation. Completed answer sentences are queued while text continues streaming. Activating the assistant again or hiding it stops playback. Tempo changes preserve pitch.

The MCP manager stores stdio or Streamable HTTP server definitions and connects to
discover their tools. Remote endpoints require HTTPS; local loopback HTTP is allowed.
Stdio commands launch directly with one argument per line. Environment variables,
authentication headers, and the Brave key are protected with per-user Windows DPAPI.
Select discovered tools on Agent nodes. Memory Palace is not connected automatically;
Echo never opens the Codex shared memory database.

Before release, test the global hotkey, microphone, chat, audio output, model download using an account with gated access, and MCP connection against real servers. The isolated UI preview is available with `scripts/gui-dev.ps1 -IsolatedWindow assistant-chat`, `assistant-pipeline`, `assistant-output`, or `mcp-manager`.

Model settings have dedicated sections: Local speech selects and installs Parakeet
v2, v3, or Ultra and configures defaults for new transcription nodes. Punctuation
& casing installs Edge Punct Casing and configures defaults for new punctuation
nodes. Connections manages online credentials and text models.

Settings → Text to speech has a **Default voice** selector and **Manage voices /
test speech** button. Select a voice in the library list; the adjacent panel contains the test phrase,
Test voice, and Use as default voice. Selection alone does not change the default.
Use **New voice** to open an empty Edit voice window for importing or recording.
Closing it without adding a voice preserves the library selection.
Open **Edit voice** to rename, import, record, export, or delete a voice.
The library imports WAV and MP3 recordings (up to 30 seconds and
20 MB) and compatible Pocket TTS `.safetensors` prompts. Recording audio is mixed to mono,
resampled, encoded locally, and stored as an `audio_prompt` safetensors file;
subsequent playback loads that prompt directly. Original uploaded recordings are not
copied into the library. Recorded temporary WAVs are removed after conversion.
Existing WAV selections remain usable and can be imported to obtain a reusable prompt.

Voices have unique, editable names and support selection, preview, export, and
deletion. Stored assets and metadata live next to settings under `voice_library`.
Invalid tensor shapes, unsupported types, nonfinite data, oversized files, and
invalid audio are rejected before a voice is committed. Renaming retains the file
identity, so references remain valid. Deletion is blocked for voices referenced by
settings, saved graphs, or the open editor (including Undo/Redo history).

**Use as default voice** returns the library selection to settings; **Save settings**
persists it. New Speech nodes follow that default at execution time and inherit
the startup-loading preference. Selecting Alba or a custom voice on a node pins
its override. Existing nodes keep their previous explicit voice selections.

Text model editors expose **Additional request parameters (JSON)**. Enter an optional
JSON object using the selected provider's request format, for example
`{"think":"high"}` for a supported Ollama model. Leave the field blank for provider
defaults. These parameters are saved per model and passed through Rig for model
tests, pipeline completions, and streamed assistant replies. Changing them in the
component creator requires another successful test before creation. Ollaya routing
uses its own protocol and does not expose this Rig-specific field.

Speech volume is available under the assistant gear and in Settings → General,
beside the speaker selection. The 0–100% slider adjusts Echo's speech and voice
previews, including audio already playing; 0% mutes them. The speaker test uses
the selected level. Assistant-board adjustments save automatically; General
settings adjustments are saved or discarded with the other settings. Existing
settings default to 100%. This control leaves Windows system volume unchanged.

The assistant gear's sentence speed control adjusts the next sentence from 0.75×
to 2×. Audio passes through a continuous Signalsmith tempo processor that keeps
pitch unchanged, compensates processing latency, and flushes the sentence ending.
At 2×, the generated sentence audio has half its original duration. Pocket TTS
still generates at its own pace; this slider does not accelerate model inference.
Audio streams during generation at every speed. Above 1×, playback uses a short
120 ms startup cushion rather than waiting for the complete sentence's audio.
The same tempo processor carries across frames and flushes the sentence ending.
Inference can continue ahead of playback. Sustained 2× speech still requires the
machine to generate audio fast enough to keep up; the speed control changes
playback tempo, not model inference speed.

The assistant shows Pocket TTS readiness beneath the avatar. While weights are
loading it displays **Loading Pocket TTS weights… Speech is not ready yet.**
After loading succeeds, it displays **Pocket TTS ready**. Failed loads show a
warning instead of remaining in the loading state. This reflects the shared
model cache, including startup preloading and voice previews; unloading clears
the indicator. Readiness refers to model weights, not speaker or voice setup.

## Hermes backend integration

Settings → Connections → **Hermes assistant settings**, or the assistant gear,
opens **Hermes assistant**. **Open assistant** is available in Connections too.
Enable Hermes, select a verified
fast conversation model, save, and connect. **Install / repair** provisions the
private runtime and Codex; **Codex login** uses the standard ChatGPT browser
login in Echo's private Codex profile. A blank worker model uses Codex's default.
**Update Hermes** runs Hermes's official source updater against the latest
`main` version. Finish conversations and background tasks first; Echo refuses
maintenance while they are active or another process owns the runtime. An idle
connected assistant reconnects after maintenance.

Before updating, Echo keeps a complete source/data snapshot under its private
component directory's `updates` folder, including committed SQLite WAL data and
dependency-selection receipts. Codex login, Echo presentation, voices and
workspace approvals remain outside the update transaction. The previous
dependency generation is leased while Hermes installs the new one. A disposable
home checks native HTTP sessions, authentication, routes, schemas, task creation,
subscriptions, queued instructions, events and cancellation without calling a
model. Failure restores source, data and dependency selection. An interrupted
transaction recovers before the next Connect, Update or Install / repair.
Backups are retained locally; they contain private conversation data.
Install / repair synchronizes dependencies on the current checkout and preserves
its version. The known bootstrap revision is used only for a fresh install.

Rig uses the compatible `0.42` dependency range instead of an exact `=0.42.0`
pin. Cargo.lock still records the resolved build for reproducibility. Rig 0.44
requires a separate migration to its redesigned provider API.

For an existing installation, `echo.exe --configure-hermes` enables the backend
with the model feeding the current assistant graph. Echo must be closed first.
The command preserves the graphs, microphone, speakers and voices.

Echo owns transcription, the avatar, sentence streaming, audio playback and
presentation. Hermes owns native conversations, task records, worker dispatch,
notification continuations and execution. Starting voice input stops playback;
background tasks continue. **Stop task** targets the selected native task and
**Stop all tasks** explicitly stops the private board's work. Closing the
assistant window hides it; exiting Echo shuts down its owned runtime processes.

The default conversation stays available while workers have separate tabs.
Each tab has a name, automatic task title, activity, priority and voice override.
The activated assistant branch's connected Speech node supplies the reply voice,
including progress phrases and spoken summaries. A custom voice or Alba pinned
on that node takes precedence over a conversation override. A Speech node set to
Use settings default can inherit the conversation override, then the settings
default. Without a connected Speech node, that native response stays silent.
Different voices on separate routed branches are supported; connecting different
Speech voices to the same native response reports an error instead of choosing
one arbitrarily. Choosing a dictation graph preserves the assistant selection.
Older selections pointing at dictation recover only when an assistant choice is
unambiguous.

**Updates** shows conversation/progress text; **Output** shows the output Hermes
exposes. Deep Codex child activity is outside the initial integration. Native
worker instructions remain queued until the worker acknowledges them.

Echo uses Hermes's supported secret-source extension to bind launcher-supplied
credentials to its private default profile. Worker and sibling profiles do not
inherit the conversation credentials. Google connections use an Echo provider
extension that reuses Hermes's native Gemini profile and streaming client.
The Codex worker uses Hermes's native app-server runtime and Codex's own login,
without a second Hermes OAuth login. Both extensions leave upstream source
unchanged.

Reconnect and repair check native pending tasks and conversation leases first.
Finish pending work, or use Stop all tasks, before applying connection changes.
Interrupted attempts are archived on startup and do not silently redispatch.
**Retry** creates an explicit native continuation in the same conversation;
it refuses an active or unconfirmed old worker and requires a fresh workspace
approval for modification tasks. Repeated retry clicks converge on the same attempt.
Workspace changes require an explicit grant; Git projects get native isolated
worktrees and shared folders permit one writer. Inspection workers use Codex's
read-only permission profile and a restricted native Hermes MCP tool set.

History is read from native Hermes storage in pages of at most 120 fragments,
each at most 4 KiB. Rendering uses Slint's Skia/Parley text support and virtualized
lists, including fragments within large messages. The text cache has a shared
64 MiB limit; queued inputs have a shared 4 MiB limit and queued SSE payloads a
shared 16 MiB wire-byte limit. These limits are separate from native history on
disk and do not claim a fixed total process-memory ceiling. Earlier history can
be paged without following new messages; **Latest** returns to the current page.
Search and complete export run outside the UI thread.

The October 7 implementation gate verified a live conversation with the current
assistant model and an authenticated Codex task through the native dispatcher
and Echo launcher. Native integration tests also exercise permission boundaries,
credential isolation, cancellation, replay, Unicode paging and history export.
Production Skia screenshots were inspected at 100%, 125% and 150% scaling.
The paging benchmark covered 100,000 messages, 50 sessions and about 1.08 GB of
source text; it measures paging latency, not complete UI frame or input latency.

Message admission retries use the same native idempotency key. Only confirmed
rejections restore input to the draft; an accepted or unconfirmed message is
not offered for accidental resubmission after a stream disconnect.

Assistant submissions now reuse the input side of the selected graph: local
formatting, punctuation/capitalization, templates, wired instructions and routing
run before the assistant boundary. The response model and its tools stay in
Hermes; graph response/speech/display nodes are not executed by this input pass.
Voice recording captures the graph and conversation destination at its start.

Task Updates combines bounded native conversation fragments and task events,
including continuations that share the same conversation. Independent cursors
preserve each source's native order even when timestamps regress. Older-history
views do not receive live event rows until Latest is selected. Reopened Updates
also resolves progress comments from the native task comment table.

Gemini additional request parameters support native `generationConfig` overlays
on streaming and nonstreaming requests, plus safety settings/cached content and
the native adapter's thinking/response-format fields. Unsupported top-level
options fail explicitly before startup instead of disappearing silently.

Completed stream speech is matched to native message identities using the
native completion timestamp as a fence against later notifier messages; equal
text is no longer treated as equal message identity. This still needs runtime
stress tests across reconnects and session compression. Partial history exports
close their Windows file handle and clean up when their future is cancelled.

Cached phrases prepare automatically while Echo is idle, covering the settings
voice and every configured tab override. Capture, conversation turns and audio
playback pause preparation; disabling automatic phrases or shutting down stops
it without claiming readiness. Existing WAVs are reused, voice-file replacement
schedules fresh preparation, and failed preparation retries at most once per
thirty seconds. The manual preparation action remains available.

Notifications follow confirmed native queued/claimed/completed/failed/review and
queued-instruction events, tool activity, approval requests, connection changes,
confirmed stops, and tasks that remain running for fifteen seconds. Selection,
capture, reply activity and playback-generation checks fence stale notifications;
a ten-second cooldown limits chatter. Archived state alone never claims worker
termination. Catalogue entries for states Hermes does not expose remain dormant
rather than inventing those states. Spoken results wait briefly for an
acknowledgement to finish instead of being discarded, and cancel if their
conversation, summary or playback generation changes.

Validation on October 8, 2026, used synthetic transcripts and private fixtures:

- Final format, Cargo check and Clippy across all targets/features passed. The
  Rust suite passed 274 tests with 46 intentionally ignored, the native Python
  suite passed 41, and seven additional isolated Windows Skia GUI smoke checks
  passed. A test-owned cached voice generation check and the authenticated
  native readiness gate also passed independently.

- All 104 default-voice phrases were rendered with installed Pocket TTS, checked
  as nonempty mono WAVs, and reused by a second preparation pass without synthesis.
- The current Gemini assistant and authenticated private Codex completed a real
  native conversation and worker task. The readiness test now uses the configured
  output-token and timeout limits; its former 32-token probe falsely failed.
- Native history and stable message identities survive closing and reopening
  the database. Retry, admission-response loss, stream reconnect and process-tree
  cancellation tests pass. Full durable admission reconciliation after an Echo
  crash is still outstanding.
- Native paging of 100,000 messages in 50 sessions (1,080,200,000 source bytes)
  measured 18.17 ms median and 19.83 ms p95 over 200 pages. Python traced peak was
  1,705,479 bytes and RSS growth 1,560,576 bytes. This excludes FTS indexing.
- A Windows Skia presentation fixture with 50 busy tabs and 120 rich-text blocks
  (465,120 source bytes) measured 16.57 ms median and 18.10 ms p95 from incremental
  update to rendered frame over 120 samples, with updates scheduled every 50 ms.
  Output/Updates toggles and name editing took 13–16 ms through Slint MCP.
  Thirty additional scroll events took 15 ms median (26 ms maximum) per MCP
  round trip; the scrolled view was inspected. This
  tests presentation, not fifty simultaneous paid model requests. Reproduce with
  `ECHO_GUI_HERMES=1`, `ECHO_GUI_STRESS=1`, and optionally
  `ECHO_GUI_STRESS_REPORT=<path>` through the GUI development script.
- The production Skia renderer was inspected at Slint scale overrides 1, 1.25,
  and 1.5, at 380×520 and 540×650 logical pixels. The physical monitor remained
  at 125%; OS-wide DPI transitions were not exercised. The inspection fixture
  now lets Slint's native scale override control the renderer and surface rather
  than injecting a second resize that caused misleading clipped screenshots.
- A local candidate installer was compiled; nothing was published. Installer
  uninstall-data removal now includes both local model storage and roaming
  settings/Hermes data, only after the existing explicit Yes confirmation.
  Silent uninstall continues preserving data. The candidate was not installed
  over the user's existing Echo registration.

The full migration remains in progress: additional worker-provider profiles;
complete computer-use capability integration; and durable admission/restart
reconciliation. In this pass the user explicitly skipped computer use, additional
worker providers and the full microphone-to-Hermes-to-speaker acceptance test.
Tray/global-hotkey interactions and real OS-wide DPI transitions remain manual
acceptance checks. Release builds and installer compilation do not establish
those runtime checks.

Release-review findings for this migration remain open:

- **Medium:** native admission keys are retried safely within a running request,
  but `src/hermes_ui.rs` generates the key in `turn` and `save_tab_bytes` stores
  presentation only. A crash after native acceptance but before local admission
  confirmation has no durable Echo request journal to reconcile on restart.
  Native history survives and uncertain requests are not automatically
  resubmitted. Complete the journal/reconciliation design and add a process-crash
  gate before claiming full restart recovery.

The earlier independent-updater finding is addressed by the built-in Update
Hermes transaction described above. Its filesystem tests cover update success,
native failure, incompatible releases, interruption/recovery, complete-backup
failure, WAL preservation, idle gating, origin validation, recovery path
validation and repair without downgrading. The disposable native compatibility
probe is also exercised against the provisioned runtime.

Spark/subagent tools were unavailable in this session; the primary agent carried
out the implementation, code inspection and validation. This is a candidate
packaging check, not a complete publication sign-off.

The earlier candidate artifacts are `target/installer-validation/Echo-Candidate.exe`
and `target/installer-validation/Echo-0.1.10-Candidate-Setup.exe`. Those artifacts
belong to the earlier packaging pass. The built-in updater build is staged as
`target/installer-validation/Echo-Hermes-Update.exe`; its installer has not been
rebuilt or published in this updater pass.
A concurrently running Echo locked `target/debug/echo.exe` during final
replacement, so that process and binary were left alone. Close the running
instance before using the candidate. No website or release-feed publication
occurred.

Built-in updater validation (2026-10-08):

- The real Echo updater advanced an isolated provisioned installation from
  `8620effe538c294fa3bd923d93931854a8c8a4c9` to fetched `main` revision
  `d94b70f675205c2c046138997819428772cd2678`. Native messages and a user-state
  marker survived; the previous dependency generation remained available.
- Full provisioned source/history rollback and fresh-process interruption
  recovery passed. A deliberately missing selected environment was repaired
  using the managed base Python without changing the updated source revision.
  Codex-only repair restored all six components without changing Hermes.
- Seventeen filesystem/preflight tests cover the transaction, private plugin
  dependencies, tool receipts, incomplete backups and interrupted restoration.
  Idle preflight reads native SQLite state without importing Hermes, so it
  cannot trigger dependency repairs before the backup.
- Production Skia settings renders and Update-button interactions were reviewed
  at minimum/normal sizes and scale overrides 100%, 125% and 150%. The physical
  monitor remained 125%; no OS-wide DPI transition was claimed.
- Final Rust tests: 274 passed, 47 ignored. Python integration tests against
  the updated provisioned source: 56 passed, two optional Codex checks skipped.
  Format, cargo check, strict all-target/all-feature Clippy and the optimized
  build passed. The staged updater executable matches target/release/echo.exe.
  The user's installed Hermes/data and running Echo were left untouched; tests
  used a separate provisioned runtime under target. No installer or website
  publication was performed for this updater pass.

## Importing Codex conversations

In the assistant's settings gear, choose **Import Codex conversation…**. Select a
profile, optionally search its conversation titles/project paths or switch to
archived conversations, and select a conversation to preview it. **Import into
new tab** creates a separate native Hermes session with the original title.
Use **Add profile…** for Codex home folders outside the automatically discovered
`.codex`, `.codex-*`, `CODEX_HOME`, and Echo-managed Codex profiles.

Echo reads the source profile and copies the rollout through its last complete
record into its own managed Hermes home. Original Markdown/code is preserved.
Historical tool calls and results provide supporting context; they are hidden
from ordinary chat and cannot execute as imported tool calls. Old developer,
system, reasoning, and bookkeeping records are not fed into the conversation.
The next turn uses the current Hermes conversation model and native compaction.
The original project path is context and does not grant workspace access.

Available local and embedded image attachments are copied. Image references in
chat appear as placeholders, with an explicit import note for unavailable
attachments. Remote image URLs are not fetched. Each import has an independent
identity, even when importing the same source twice. Source updates after the
snapshot do not change the imported tab.

Catalog pages contain at most 40 conversations; preview pages contain at most
20 blocks of 4,096 characters. Import writes run off the UI thread in bounded
batches, with cancellation and incomplete-stage cleanup after a bridge restart.
Importing while recording defers selection of the new tab until transcription
finishes. Existing paged, virtualized history renders the completed import.
Individual source records over 16 MiB are rejected with an explicit message;
this limits parser memory rather than silently truncating history.

## Changing activity messages

Consecutive native task status updates share one assistant bubble. Its text
changes in place instead of adding a separate message for each event. A real
user or assistant message ends the group; later activity creates a new changing
bubble after that message. The original native events remain in Hermes.

Live conversation runs show thinking, tool execution and approval activity in
the same conversation. Native interim replies close the current streamed
message, so subsequent response text starts a distinct bubble. Native tool
results are projected into compact completion messages without loading their
full output into the chat renderer. Their original content remains available
in Hermes and full conversation export. Heartbeats continue to update the
existing indicators rather than producing messages.

The current activity bubble has an **×** button to stop that specific Hermes
conversation run or background task. Speech for the selected activity stops
immediately, while the bubble shows **Stopping…** until Hermes confirms the
result. Finished messages have no stop button. The button retains the activity's
original identity across updates, tab changes, and task retries, so it cannot
stop unrelated work. A stopped run's expired approval prompt closes automatically.

## Calendar reminders

Open **Settings → Reminders** or **Calendar & reminders…** in the assistant gear.
Microsoft calendars are read-only. Default reminders occur one hour, 15 minutes,
and 5 minutes before a meeting, at its start, then every 5 minutes until dismissed.
Voice reminders wait for a sentence boundary and do not stop background tasks.
Rules, snoozing, and dismissal persist across restarts. The Microsoft application
ID can be entered later. See [Calendar & Reminders](calendar-reminders.md) for
account setup, all-day events, quiet hours, voice choices, and override scopes.

## Voice sayings

Select a voice in **Voice library** and choose **Sayings…**, or open it from
**Edit voice** after importing or recording a new voice. The window lists all
104 assistant sayings in 24 sections, each explaining its playback trigger and
grouping all alternative clips for that reason. Sections without an automatic
trigger are explicitly marked as reserved. Search by wording, situation, or trigger,
select a saying, edit it, and choose **Save** at the bottom of its information
panel. Switching to another option or closing with unsaved changes offers
**Save**, **Discard**, and **Cancel**. Saving continues the requested action;
Cancel keeps your current option and edits. **Restore default** loads the
original words into the editor; choose Save to keep them.

**Regenerate saying** replaces the selected clip using the selected voice;
**Regenerate all (104)** generates the entire set with that voice. Pocket TTS
must be installed. A progress bar advances after each completed clip and shows
the completed count and the saying currently being generated.
**Play saying** previews a completed clip. **Stop** keeps
completed clips and preserves any existing audio until its replacement is
complete. Edited wording requires new audio before that version can play.
Wording and generated clips are kept separately for each voice.
