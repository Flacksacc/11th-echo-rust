# Automatic updates

Echo uses a static HTTPS feed and keeps the update server independent from the
source repository. The production feed URL and Minisign public key are compiled
into `echo.exe`; the secret signing key is never compiled, copied into the
installer, or committed.

## One-time setup

1. Install Minisign and run `minisign -G` to create a password-protected keypair.
2. Keep the secret key and its password in offline or protected release storage.
3. Copy `installer/update-config.example.json` to
   `installer/update-config.local.json` and set:
   - `feed_url`: the final HTTPS URL for `manifest.json`.
   - `public_key_path`: the Minisign public key file.
   - `secret_key_path`: the Minisign secret key file.
   - `release_notes_path`: optional UTF-8 text, limited to 20 KiB.
   - `authenticode_required`: leave `false` until both the app and installer are
     Authenticode-signed by a trusted certificate.
   - `publish_host`: an SSH host alias from your local SSH configuration.
   - `publish_path`: the server directory corresponding to the feed URL.

The local configuration is gitignored. CI may point at another protected JSON
file with `ECHO_UPDATE_CONFIG`. A feed URL is not a secret: clients must know it
to check for updates, but keeping it out of Git makes the deployment endpoint
replaceable per build.

## Build and publish

Run:

```powershell
.\installer\build-installer.ps1
```

The script validates HTTPS configuration and key files, injects only the feed
URL and public key into the release binary, builds the Inno Setup installer, and
creates `installer/output/update-bundle` containing:

- `Echo-<version>-Setup.exe`
- `manifest.json.minisig`
- `manifest.json`
- `SHA256SUMS.txt`

Upload the installer and `manifest.json.minisig` first. Upload `manifest.json`
last as the release commit point. All files should be served from the same
directory over HTTPS. Do not reuse an existing version number or overwrite a
published installer.

To build, verify, and publish in one operation, run:

```powershell
.\installer\build-and-publish.ps1
```

The publishing script verifies the manifest signature, installer size, and
SHA-256 locally, stages the bundle through SSH, verifies the hash again on the
server, and atomically moves `manifest.json` into place last. To publish an
already-built bundle, add `-SkipBuild`. To test all local validations without
changing the server, add `-SkipBuild -ValidateOnly`.

## Client behavior and recovery

Echo checks on startup and every six hours when automatic checks are enabled.
Users can opt out in Settings or check manually from the version panel. A newer
stable SemVer release is offered only after the exact manifest bytes pass the
compiled Minisign public-key check. The installer is streamed to
`%LOCALAPPDATA%\11th_echo\updates`, bounded to 512 MiB, checked against its exact
declared size and SHA-256, and atomically finalized before launch.

The update installer runs per-user and silently preserves the existing install
directory and settings, closes Echo, then restarts it. Failed and interrupted
downloads are never executed; the UI exposes Retry. To roll back, publish a new,
higher version containing the desired older application code.

Rotating the Minisign key requires a bootstrap Echo release signed by the old
key whose binary contains the new public key. Never publish a manifest signed
only by the new key until that bootstrap version has reached users.
