# Parakeet Ultra

Echo offers Parakeet Ultra alongside V2 and V3 under **Local — Parakeet**. V2 remains the default. Ultra uses Photon/CUDA on Windows x64 with an NVIDIA Ampere or newer GPU; CPU models continue using Sherpa ONNX. Ultra detects its supported 25 languages automatically.

Choose Ultra, install its optional files, and save settings to activate it. The settings page displays the GPU probe and engine status. CPU/Silero tuning values remain saved but are hidden for Ultra. Echo tunes Photon's replacement previews to one-second updates with half a second of lookahead: the first preview becomes eligible after 1.5 seconds of audio, plus inference time. Earlier text can change as more context arrives. Stopping drains microphone audio before committing one full-context final transcript. The helper is embedded in Echo, so rebuilding the app applies these settings to an already-installed runtime.

No user-installed Python, ONNX Runtime, CUDA Toolkit, or developer tools are required. A compatible NVIDIA driver is required. GPU failures report an error and do not silently switch models or inject unfinished text. Switching away releases the helper; Windows job ownership also terminates it if Echo exits unexpectedly.

## Build and release

For source development, run `cargo run` with no manifest environment variable. Debug builds discover the latest successful package at `target/ultra-runtime/runtime-manifest.json` when you click Install. They verify and extract its local archive, ignoring its publication URL, and download the model weights normally. An installed verification receipt keeps that runtime usable even after rebuilding or removing the source package. Rebuild the local package and click Install again to use a newer runtime. Release builds retain their embedded package configuration.

If the local runtime has not been built yet, use Python 3.12 and `uv` to run `python runtime/ultra/build_runtime.py --url-base https://localhost/`. The URL is unused by the source installation path; no hosting or manifest configuration is needed.

The publisher must have an M87 Labs agreement permitting redistribution of Photon/Kestrel and kernel packages. That permission was confirmed for this project. Runtime notices preserve supplied package licenses; Ultra's model is CC-BY-4.0 with NVIDIA and Moondream attribution.

Build-machine prerequisites: Python 3.12, `uv`, normal Rust/Windows build tools, and Inno Setup. The optional runtime uses embedded Python 3.12.10, Moondream 2.4.1, Kestrel 0.8.1, and CUDA-enabled PyTorch 2.11.0. All Python dependencies are locked with hashes; the CUDA wheel uses a pinned Windows URL. GPU assets are not added to the main installer.

`installer/build-installer.ps1` builds the optional runtime by default using the configured HTTPS update-feed directory, embeds its manifest into Echo, and adds the immutable runtime archive to the upload bundle. `build-and-publish.ps1` validates and stages that archive before advancing the update manifest. Set `ultra_enabled` to false in release configuration to intentionally make a CPU-only release. To reuse an already-built package, set `ECHO_ULTRA_RUNTIME_MANIFEST` to its manifest before building.

For a standalone package build:

```powershell
python runtime/ultra/build_runtime.py --url-base https://your-update-host/stable
$env:ECHO_ULTRA_RUNTIME_MANIFEST = (Resolve-Path target/ultra-runtime/runtime-manifest.json).Path
cargo build --release
```

The builder prints the archive, manifest, and extracted runtime directory. Publish the archive at the URL embedded in that manifest. A build without a runtime manifest explains that Ultra is unavailable instead of downloading unpinned executable code. Published runtime archives have content-derived immutable names; retain old archives for existing Echo installations.

Runtime and weights install separately from CPU models beneath `%LOCALAPPDATA%/11th_echo/ultra`. Downloads enforce HTTPS redirects, pinned sizes and SHA-256, bounded extraction, no links or traversal, and staged replacement with rollback. Model revision: `73175eb7aeb0d82f1e2a6b53b3aabc10a90bcd0b`. The runtime verifies files again before loading. Inference blocks outbound sockets and DNS, including dependency telemetry; internal Windows asyncio wakeup socket pairs remain permitted. Audio travels through bounded private process pipes and is not written to disk.

## Verification

```powershell
python runtime/ultra/test_helper.py
cargo test transcription::local_photon -- --ignored --skip gpu_runtime --test-threads=1
```

The subprocess fixture tests require build-machine Python (`ECHO_TEST_PYTHON` can select it). They cover tail draining, exactly one final commit, repeat sessions, crashes, malformed/stale responses, and cancellation. Default Rust tests cover model migration, protocol limits, archive validation, checksums, and install rollback.

For real GPU testing, point `ECHO_ULTRA_RUNTIME_DIR` at the extracted standalone runtime (containing `python.exe` and `helper.py`) and `ECHO_ULTRA_MODEL_DIR` at the pinned weights. These are development overrides; the runtime override bypasses runtime-package verification, while model verification remains enforced.

```powershell
cargo test gpu_runtime_transcribes_fixture -- --ignored --test-threads=1
python runtime/ultra/smoke_test.py --python PATH/TO/RUNTIME/python.exe --model-dir PATH/TO/WEIGHTS --wav installer/assets/models/parakeet-tdt-0.6b-v2-int8/test_wavs/0.wav
```

Real runtime tests must be complemented by microphone/hotkey/overlay/injection checks, multilingual/noisy/long speech, driver and low-memory failure checks, and download interruption/repair. Installer testing on a clean Windows account or VM is optional, not a release or publication requirement. Do not infer a universal minimum VRAM or consumer-GPU latency from upstream server benchmarks.
