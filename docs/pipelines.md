# Node graphs

Settings → Pipelines manages one list of graphs. Each graph owns an optional
hotkey. There are no transcription/assistant graph categories: the wiring chooses
the destination. Use selects the graph for the main recording button and the
assistant board; individual graph hotkeys always activate their own graph.

## Editing

Click the **Hotkey** field and press the new shortcut, then Save. Backspace or
Delete clears the binding; Escape leaves the field. Echo captures its existing
registered shortcuts there instead of starting recording. Duplicate or unavailable
shortcuts still show an error when saving. Ctrl+C and Ctrl+V inside this field
are captured as shortcuts; elsewhere on the graph they copy/paste nodes.

Open Edit to work on a draft. Add node opens the searchable component catalog,
including every installed speech provider, tested language/decision model, and
existing text processor. Create model component retains the tested model workflow.
Double-click the canvas or use its context menu to add a node.

Drag a node header to move it; drag its lower-right corner to resize it. Drag an
output socket onto an input socket to connect them. An input accepts one wire;
an output can feed multiple nodes. Click the × on a wire to disconnect it.
Ports show their names and data types. Drag on empty canvas space to draw a selection rectangle; fully enclosed nodes are
selected. Ctrl- or Shift-click adds or removes a node; Ctrl- or Shift-drag adds
another enclosed group. Drag a selected node header to move the whole selection.
Click empty background to deselect all nodes.
Ctrl+C copies selected nodes; Ctrl+V pastes independent copies offset on the canvas,
including wires between copied nodes. Clipboard copies also work between graph
editors. Copy/Paste are available in the canvas context menu. Focused text editors
retain ordinary text copy/paste. Delete and Duplicate apply to the entire selection;
Replace is available for a single selected node. Each paste, group move, duplicate,
or deletion participates in Undo/Redo. Transcription can only be replaced with another transcription component.

Pan with the middle mouse button or Space-drag. The wheel zooms around the cursor
between 25% and 200%; Fit graph and 100% reset the view. Position, size, and viewport
are saved with the graph. Undo/Redo support draft edits. Save persists the graph;
Cancel discards it. Closing an unsaved draft offers Save, Discard, or Keep editing.
Text nodes have a multiline editor directly on the node. Resize the node to show
more text; edits update the draft immediately and participate in Undo/Redo. Text
nodes have no buttons. Settings use compact inline controls with small field titles.
Text settings such as Instructions, Separator, Template, replacements, and protected
phrases have optional text inputs. Wiring a field hides its editor and leaves its
title; disconnecting restores the saved inline value. Router nodes include a tested-model dropdown and
show the selected model. Their Option 1, Option 2, … outputs align with the
matching numbered inputs; editing option text does not rename the sockets.

## Nodes and data

| Node | Inputs | Outputs / effect |
| --- | --- | --- |
| Transcription | Audio supplied by recording | Text |
| Text | Editable constant | Text |
| Combine text | Ordered Text sockets | Text joined with an editable separator |
| Template | One Text socket per `{name}` | Expanded Text; values are substituted once |
| LLM | Prompt Text; optional Instructions Text | Generated Text |
| Local formatting / punctuation / Gemini | Text | Processed Text |
| Router | Prompt Text and two or more option Text sockets | Prompt Text on the selected option output |
| Speech · Pocket TTS | Text | Generates and plays speech; no output socket |
| Text output | Text | Injects text into the captured element; no output socket |
| Assistant display | Text | Displays and records text on the assistant board; no output socket |

Prompts belong on the graph. Connect Text to an LLM's Instructions input, or
combine static text with transcription using Combine text or Template. A user
request can also connect directly to the LLM's Prompt input. Generation limits,
timeouts, optional conversation history, and local preload choices are configured
per instance. Credentials remain shared in Settings → Connections. Installed models have separate Local speech, Punctuation & casing, and Text to speech settings sections.

A router starts with Prompt and Add option inputs. Connecting Text to Add option
creates a stable paired output and another empty Add option input. Each option's
connected text describes a choice. The selected output forwards the incoming
prompt, rather than generating an answer. Disconnecting an option also removes its
paired output and its wires; Undo restores the entire edit. Decision models and
text models can both route. New routers report invalid decisions as node errors.

## Execution

Every runnable graph has exactly one Transcription node. Static text supplies
supporting data. Graphs are acyclic. Multiple Text output or Assistant display
endings are allowed when routing guarantees exactly one activates per successful
run. Independent side paths, including speech, may terminate separately.

Graph validation checks connections, required inputs, and ending exclusivity and
coverage across router choices. Unconnected draft nodes are inert. A recording
snapshots the graph and settings at Start; edits apply to the next run. Typed
assistant requests use the selected graph, supplying text at Transcription's
output and bypassing microphone capture.

Ready independent nodes run concurrently off the UI thread. Unchosen router paths
are marked Skipped. A failed node blocks only its consumers; independent paths
continue and the error is reported. The overlay and open editor report node states.
Missing models, credentials, or speech assets fail the affected node when it runs;
they do not prevent independent paths or unused router branches from running.
LLMs stream into directly connected Assistant display and Speech nodes. A complete
sentence is sent to Pocket TTS as soon as it arrives; its audio frames play while
the rest of that sentence's audio and subsequent LLM text are still being generated.
An unfinished final text fragment is sent when the LLM finishes. Processors,
routers, and string operations wait for completed text. Streamed text and already
played speech cannot be rolled back.

Text output retains the captured foreground target and existing injection safety
checks. Intermediate values remain transient; successful pasted text enters
transcript history. Assistant conversation remains in memory and is reset by New
chat or closing the board. Model deadlines and recording finalization limits remain.

## Upgrade and verification

Node settings are edited directly in each node's scrollable body. There is no
Configure button or node configuration dialog. Model selectors, instructions,
generation limits, conversation history, transcription tuning, formatting,
punctuation, routing, templates, combine separators, and Speech voice overrides
participate in the graph draft and Undo/Redo. Save persists the graph; invalid
numeric edits are rejected. Instructions entered on an LLM node apply when its
Instructions input is unwired; a connected wire takes precedence. Wires remain
available while templates are being typed, and obsolete input wires are pruned
when saving. Resize a node for more visible controls, or scroll its settings.

Pipeline formats 1–4 are converted once to graph format 5 using the protected,
atomic settings writer. IDs, names, model settings, credentials references, local
preload preferences, and active selections are retained. Old active hotkeys become
graph hotkeys; other graphs start unassigned. Duplicate graphs have no hotkey.

Legacy combined router/answer behavior becomes separate Router and LLM nodes. The
direct path can require another model call, changing latency and exact wording.
Migrated routers retain fallback/confidence behavior; speech becomes a side wire
at the equivalent text source. Unedited migrated formatting retains its original
projection. Older versions of Echo reject version 5. Failed migration saves leave
the previous settings on disk and report the problem.

Run `cargo test --bin echo` and `cargo check`. The graph tests cover wiring, dynamic
options, exclusive destinations, strings, migration, cancellation, independent
failures, concurrency, and streaming with local mock providers. Inspect the actual
editor using `scripts/gui-dev.ps1 -Mode mcp -IsolatedWindow pipeline-editor-live`.
Use production Skia at minimum/normal sizes and 100%, 125%, and 150% scale. Live
microphone recording, graph hotkeys, speech playback, tray actions, downloads, and
external text injection require an interactive acceptance pass before release.

Implementation verification (2026-10-03): the automated suite passes, including
mock-provider concurrency/streaming and independent-node configuration failures.
The isolated graph draft smoke test exercises move/resize, undo/redo, wiring,
add/duplicate/delete, viewport persistence and a real temporary settings save/load.
Live Slint MCP interactions verified wire drag/disconnect, header dragging,
corner resizing, panning, cursor zoom, Text configuration, dynamic router sockets
and their Undo/Redo behavior. Headless production Skia renders were inspected at
700×600 and 1100×900 logical sizes at 100%, 125% and 150% scale. Native Windows
monitor DPI transitions and real audio/output acceptance remain unverified.

## Live feedback

The active graph chooses the live transcription window. Graphs with a connected
Assistant display open the assistant board from the start of recording and keep
the normal overlay hidden, including during transcription errors and processing.
Text-output graphs use the normal overlay. The selected window shows each
connected node’s execution state.
The strip shows up to four nodes: the previous completed node, the current
node, and the next two waiting nodes in dependency order. It advances throughout
the run, including graphs longer than four nodes, and omits skipped branches
from upcoming slots. At the start or end it shows only the available nodes. Short partial transcripts remain visible from the first word.

Spoken replies require a connected Speech node and installed Pocket TTS assets.
Use the standard Alba voice, a saved library voice, or the settings default.
The assistant board explains missing speech setup or playback
failures and offers Edit graph; successful text replies remain available when
a speech side path fails.
