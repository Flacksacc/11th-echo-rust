# Echo 0.1.3 release validation

## Release status

**Signed and published.** The public stable feed now serves 0.1.3. Following disclosure of the remaining verification gaps, the user unlocked the signing key and instructed publication to continue.

Changes prepared for this release:

- Request foreground activation when opening the existing Echo window from the Start menu.
- Allow a local end-of-speech pause of up to 10,000 ms, preserving existing settings and the 600 ms default.
- Advance application and installer version metadata to 0.1.3 and update release notes.

The transcription-model research does not add alternative models to this binary.

## Findings and outstanding checks

1. **Resolved: missing manifest signature.** The user completed signing locally. Validation-only publication checks passed, and the manifest downloaded from the public HTTPS feed verifies with the configured public key.
2. **Medium: clean-Windows installation and interactive UI verification remain incomplete.** The release checklist in `AGENTS.md` requires these checks. This session has no Windows Sandbox or available VM and cannot create a clean account without elevation. The new binary's window foreground behavior, settings pages, tray, hotkey and overlay have not been exercised interactively. The user instructed publication to continue after these limitations were disclosed; no successful UI or clean-account test is implied.

## Completed checks

- `cargo fmt --check`: passed.
- `cargo check --locked`: passed.
- `cargo clippy --all-targets --all-features -- -D warnings`: passed.
- `cargo test`: 101 passed; three asset/network tests ignored by default.
- Explicit known-WAV local-model test: passed.
- Explicit Stop/drain/final-transcript local-model test: passed.
- Production `cargo build --release --locked --bin echo`: passed using the hash-verified sherpa native archive.
- Inno Setup installer compilation: passed.
- Existing production manifest verifies with the configured release public key.
- Built executable contains the configured HTTPS feed and verification key; product version is 0.1.3.
- Installer size and SHA-256 match the prepared manifest and checksum file.
- Website uses the stable manifest dynamically; SSH publication access is available, and the 0.1.3 installer URL has not been used.
- Publication completed through `build-and-publish.ps1 -SkipBuild`, with the manifest advanced last.
- Public manifest signature verified after publication; public version is 0.1.3, newer than the previous runtime's 0.1.2.
- Installer downloaded back over public HTTPS; exact size and SHA-256 verified against the signed manifest.
- Website returned HTTP 200 and uses the updated stable feed for its download link.

The production model-download test was not run. The two model tests above used the existing known test assets. No fresh download/UI test is implied.

## Prepared artifact

- Installer: `installer/output/update-bundle/Echo-0.1.3-Setup.exe`
- Size: 11,389,316 bytes
- SHA-256: `42180ce9341f4c164dc07d06a1bd74e514dd1addf485c86964525c890fc6b77d`
- Manifest: `installer/output/update-bundle/manifest.json`
- Checksum list: `installer/output/update-bundle/SHA256SUMS.txt`
- Verified signature: `installer/output/update-bundle/manifest.json.minisig`

Public download: https://stevewellauer.com/echo/updates/stable/Echo-0.1.3-Setup.exe

Public feed: https://stevewellauer.com/echo/updates/stable/manifest.json
