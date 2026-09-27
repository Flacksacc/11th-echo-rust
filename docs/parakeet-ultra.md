# Parakeet Ultra

Echo offers Parakeet Ultra alongside V2 and V3 under **Local — Parakeet**. V2 remains the default. Ultra uses Photon/CUDA on Windows x64 with an NVIDIA Ampere or newer GPU; CPU models continue using Sherpa ONNX. Ultra detects its supported 25 languages automatically.

Choose Ultra, install its optional files, and save settings to activate it. The settings page displays the GPU probe and engine status. CPU/Silero tuning values remain saved but are hidden for Ultra. Echo tunes Photon's replacement previews to one-second updates with half a second of lookahead: the first preview becomes eligible after 1.5 seconds of audio, plus inference time. Earlier text can change as more context arrives. Stopping drains microphone audio before committing one full-context final transcript. The helper is embedded in Echo, so rebuilding the app applies these settings to an already-installed runtime.

No user-installed Python, ONNX Runtime, CUDA Toolkit, or developer tools are required. A compatible NVIDIA driver is required. GPU failures report an error and do not silently switch models or inject unfinished text. Switching away releases the helper; Windows job ownership also terminates it if Echo exits unexpectedly.

## Installation and releases

Both `cargo run` and packaged Echo install Ultra the same way: choose Ultra and click Install. No local runtime build, manifest environment variable, user-installed Python, pip, CUDA Toolkit, or developer tools are needed. Echo downloads embedded Python 3.12.10, pip 26.0.1, and the exact Windows wheels in `runtime/ultra/dependencies.json` directly from Python.org, PyPI, and PyTorch. The tested set includes Moondream 2.4.1, Kestrel 0.8.1, and PyTorch 2.11.0+cu130. Every input has a pinned URL, size, and SHA-256. Echo never resolves the newest dependencies during user installation.

The private Python interpreter runs the verified pip wheel with no indexes, no dependency resolution, binary wheels only, and hash-checked offline requirements. Installation does not alter system Python, PATH, or user site-packages. Package licenses and notices remain in the installed wheels. Direct upstream downloads avoid Echo publishing a repackaged runtime; they do not override upstream use/license terms. Model weights retain NVIDIA and Moondream attribution and CC-BY-4.0 notices.

Downloads and the installed runtime live beneath `%LOCALAPPDATA%/11th_echo/ultra`. Completed verified dependencies are cached in `download-cache/<dependency-fingerprint>` so interrupted installs and repairs can reuse them. Partial files and failed staging directories are discarded; the active runtime is replaced only after all dependencies and model files are verified. Allow approximately 10 GB free space for staging, the runtime, model, and retained repair cache. Downloads enforce HTTPS redirects and expected sizes/hashes. Embedded Python extraction rejects traversal, duplicate Windows paths, links, and excessive expanded sizes. A file receipt is checked before runtime loading.

Existing 0.1.9 runtime receipts remain supported without a reinstall. Retain already-published 0.1.9 runtime archives on the website for older Echo clients. New installer builds neither create nor upload a runtime ZIP. `installer/build-installer.ps1` needs normal Rust/Windows tools, Inno Setup, and the existing update-signing setup, not Python or uv. The publisher retains legacy archive validation support for old upload bundles.

For dependency maintenance only, review/update `requirements.in`, regenerate `requirements.lock`, and use Python 3.12 plus `packaging` to run `python runtime/ultra/refresh_dependencies.py`. This prints a proposed dependency inventory using locked hashes and Windows-compatible wheels; review it before updating `dependencies.json`. These are repository-owned pins, not a manifest developers must configure to run Echo.

Model revision: `73175eb7aeb0d82f1e2a6b53b3aabc10a90bcd0b`. Inference blocks outbound sockets and DNS, including dependency telemetry; internal Windows asyncio wakeup socket pairs remain permitted. Audio travels through bounded private process pipes and is not written to disk.

## Verification

```powershell
python runtime/ultra/test_helper.py
python runtime/ultra/test_dependencies.py
cargo test transcription::local_photon -- --ignored --skip gpu_runtime --skip upstream_runtime --test-threads=1
```

The subprocess fixture tests require build-machine Python (`ECHO_TEST_PYTHON` can select it). They cover tail draining, exactly one final commit, repeat sessions, crashes, malformed/stale responses, and cancellation. Default Rust tests cover model migration, protocol limits, archive validation, checksums, and install rollback.

For isolated upstream-install validation, set `ECHO_ULTRA_INSTALL_TEST_WORK` to a dedicated test directory and run the ignored integration test. It downloads the actual pinned wheels, installs private Python, verifies the receipt, and checks imports and versions without altering the active installation:

```powershell
$env:ECHO_ULTRA_INSTALL_TEST_WORK = "$PWD/target/ultra-upstream-validation"
cargo test upstream_runtime_installs_and_verifies -- --ignored --nocapture
```

For real GPU testing, point `ECHO_ULTRA_RUNTIME_DIR` at that test's reported private runtime (containing `python.exe` and `helper.py`) and `ECHO_ULTRA_MODEL_DIR` at the pinned weights. These are development overrides; the runtime override bypasses receipt verification, while model verification remains enforced. The install test verifies the receipt separately.

```powershell
cargo test gpu_runtime_transcribes_fixture -- --ignored --test-threads=1
python runtime/ultra/smoke_test.py --python PATH/TO/RUNTIME/python.exe --model-dir PATH/TO/WEIGHTS --wav installer/assets/models/parakeet-tdt-0.6b-v2-int8/test_wavs/0.wav
```

Real runtime tests must be complemented by microphone/hotkey/overlay/injection checks, multilingual/noisy/long speech, driver and low-memory failure checks, and download interruption/repair. Installer testing on a clean Windows account or VM is optional, not a release or publication requirement. Do not infer a universal minimum VRAM or consumer-GPU latency from upstream server benchmarks.
