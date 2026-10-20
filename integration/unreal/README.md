# Echo Unreal companion

This optional UE 5.8.2 Win64 application renders separately from Echo's Slint/Skia workspace. It uses an offscreen Unreal world, a shared D3D11 scene-capture texture, and a DirectComposition desktop surface. It does not patch the installed engine. Echo owns speech playback; the companion never plays a second copy. MetaHuman's runtime audio Live Link pipeline is the first facial-animation provider.

## Prepare your character

Open `EchoAvatar/EchoAvatar.uproject` in UE 5.8.2. Supply your own assembled MetaHuman and animations. Create `/Game/EchoAvatar/BP_EchoAvatar` as an Actor Blueprint containing your character. Add `EchoAvatarComponent` and set the standing and sitting anchor offsets on your supplied rig. Face +X, keep the character at the origin, and use centimeters. The character component positions the selected anchor at world Z=0; the capture maps that line to the desktop ledge.

In the supplied animation Blueprint, blend the provided idle/walk/sit/listen/think/speak clips using the component's `Walking`, `WalkPosition`, `Sitting`, `Listening`, `AssistantState`, and `Gesture` values. Configure foot placement in that graph and retain the supplied hair/cloth simulation. Drive the face from the native Live Link subject `EchoSpeech`, using the MetaHuman facial control mapping. No falling, ragdoll, desktop inspection, or collisions with other windows are implemented. Walking is constrained to the configured ledge; idle movement is deterministic and limited.

The component exports requests to the animation graph; it cannot invent missing animation clips, facial controls, or physics assets. Final pose blending and lip-sync quality must be tested with the real character. The PNG fallback stays available when a compatible character cannot load.

## Build and install locally

From the Echo checkout:

```powershell
& integration/unreal/build-companion.ps1
```

The engine is a development requirement only. The script builds, cooks, stages, and archives a standalone Windows runtime under `target/unreal-avatar/Windows`. The entire Windows directory, including Engine files and notices, must stay together. Do not copy only the executable. End users do not need Unreal Editor.

Select the packaged `Windows/EchoAvatar.exe` in Desktop avatar settings. A character cooked into that runtime needs no separate pack selection. For a separate character pack, use UE 5.8.2, the same EchoAvatar module and project layout, `.pak` packaging with IoStore disabled, and the fixed `/Game/EchoAvatar/BP_EchoAvatar` entry point. Cook all referenced meshes, rigs, materials, animation, and facial assets. Select the trusted resulting .pak. Changing a pack restarts the companion; it never changes Hermes sessions.

## Transport contract

Echo creates a random `\\.\pipe\EchoAvatar-<process>-<id>` endpoint with a protected DACL granting only the current user, remote connections rejected. The child receives its pipe via `-EchoPipe=`. Frames have a little-endian u32 byte length followed by UTF-8 JSON. Version is 1; maximum frame size is 256 KiB. The client first sends `{ "version":1, "type":"hello" }`. Echo sends `state`, `audio`, and `clock` frames; the companion reports `ready` with `rendered` true only after a GPU presentation succeeds. Connection readiness is distinct from character rendering and verified facial output.

Audio is mono float PCM after tempo processing, with generation ID, sample rate, and starting sample position. Playback clock is cumulative microseconds in the same generation. Queues are bounded and nonblocking on the speech side. Stale generations and late audio are rejected, and inference is limited to an 80 ms lookahead. Cancellation resets the native audio subject. Avatar errors never cancel Hermes work or block the workspace.

No transcripts or PCM are intentionally logged or saved by the bridge. Unreal's own runtime logs contain engine diagnostics. The ordinary installer excludes Unreal, and no avatar runtime or website release is published automatically.

## Validation gates

An asset-free `-EchoSurfaceProbe` renders a colored tile to test desktop GPU composition without creating a character. It is a diagnostic switch, not an avatar pack. Native audio frame evaluation and desktop capture must be checked in a packaged build. Compilation alone does not establish transparency, character lighting, hair edges, mouth alignment, or physics quality. Actual character render and facial-quality gates remain pending until the supplied character pack is present.
