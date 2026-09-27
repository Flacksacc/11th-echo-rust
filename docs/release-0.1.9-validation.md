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
- Production `cargo build --release --locked --bin echo` with the real update
  feed, public verification key, and optional runtime metadata embedded
- Inno Setup compiled `Echo-0.1.9-Setup.exe` (12,437,663 bytes)
- The new standalone runtime passed two actual GPU inference sessions and the
  preview-window timing/tail-drain tests
- The installer and optional runtime were assembled into a local upload bundle
  with SHA-256 checksums

- Minisign signature and signed update-bundle validation passed
- Published Echo 0.1.9 to the stable website feed; server-side SHA-256 checks
  passed, and public HTTPS checks returned 200 for the installer (12,437,663
  bytes) and optional runtime (2,251,994,607 bytes)

## Verification limitations

- Full manual tray/global-hotkey/overlay testing of the packaged release

The source review covered engine switching and generation ownership, cancellation
and child-process lifetime, final-only injection, HTTPS/hash checks, bounded ZIP
extraction, offline inference, preserved settings, and runtime publication order.
No release-blocking source defect was identified in that review. Independent
review subagents were unavailable in this session.

As directed by the user on 2026-09-27, installer testing on a clean Windows
account or VM is optional and does not gate release or publication. It was not
performed in this session.

## Reuse the prepared bundle

From the repository directory, sign the prepared manifest in a local terminal:

```powershell
minisign -S -s C:\EchoKeys\echo-update.key -m installer\output\update-bundle\manifest.json
```

After signing and performing applicable verification, publish the
prepared bundle:

```powershell
.\installer\build-and-publish.ps1 -SkipBuild
```
