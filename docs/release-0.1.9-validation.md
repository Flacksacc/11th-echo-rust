# Echo 0.1.9 release validation

This release adds the optional Parakeet Ultra CUDA runtime alongside the CPU
Parakeet V2/V3 models, source-build runtime discovery, and shorter live previews.
Preview eligibility is 1.5 seconds of audio, with one-second updates; inference
time is additional. Stopping still drains audio and produces a full-context final
transcript. The helper is embedded in Echo so rebuilding updates its behavior.

## Verified

- `cargo fmt --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test`: 131 passed, 12 intentionally ignored
- Both isolated Slint settings UI smoke tests, covering all settings pages and
  the CPU/GPU model selection and install prompt
- Python helper protocol/tail-drain tests and actual Kestrel preview-window tests
- Real RTX 5080 inference: two Rust-to-helper sessions produced nonempty final
  transcripts and terminated the helper cleanly
- The user verified microphone dictation and reported the shorter previews work
  well in a source build

## Release gates still pending

- Release build and Inno Setup installer compilation
- Signed update-bundle verification and public HTTPS checks
- Installation on a clean Windows account or VM; Hyper-V enumeration was denied
  in the current session
- Full manual tray/global-hotkey/overlay testing of the packaged release

The source review covered engine switching and generation ownership, cancellation
and child-process lifetime, final-only injection, HTTPS/hash checks, bounded ZIP
extraction, offline inference, preserved settings, and runtime publication order.
No release-blocking source defect was identified in that review. Independent
review subagents were unavailable in this session.
