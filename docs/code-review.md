# Code review standard

## Review output

List actionable findings first, ordered by severity:

- **Blocker:** data loss, credential exposure, arbitrary code execution, unusable installer, or a primary workflow that cannot run.
- **High:** crash, deadlock, corrupted settings, unsafe download/install behavior, or a major feature regression.
- **Medium:** realistic edge-case failure, poor recovery, material performance issue, or important missing test.
- **Low:** contained robustness or maintainability issue with a concrete future cost.

Each finding must include:

1. File and line reference.
2. The specific execution path or input that triggers it.
3. User-visible or security impact.
4. A minimal recommended correction.
5. A verification or regression-test idea.

If no actionable findings exist, say so and state what was inspected and what could not be verified.

## Correctness and concurrency

- Trace start, stop, cancellation, reconnect, and shutdown state transitions.
- Check locks across callbacks and async boundaries for deadlocks or poisoned-lock panics.
- Check channel closure, duplicate commands, stale session results, and background-thread lifetime.
- Review all `unwrap`, indexing, arithmetic, and path assumptions reachable from user input or runtime failures.
- Verify provider switching cannot mix credentials, models, or active-session state.

## Security and privacy

- Check secret storage, logs, error messages, and configuration migration.
- Check HTTPS configuration, redirects, hashes, partial files, archive extraction, and replacement semantics.
- Check keyboard injection, clipboard access, window targeting, and global-hotkey behavior.
- Check transcript and audio retention for unintended disclosure.
- Treat downloaded model archives and server responses as untrusted input.

## Windows and packaging

- Compare installer payloads with runtime lookup paths.
- Verify required native DLLs and model assets on a clean machine.
- Verify install, upgrade, uninstall, startup registration, shortcuts, and process shutdown.
- Check paths containing spaces and non-ASCII characters, standard-user installs, locked files, and interrupted downloads.
- Verify version metadata, architecture, license notices, and code-signing readiness.

## Tests and release evidence

- Run format, Clippy, unit tests, release build, and installer compilation.
- Identify important ignored tests and explain how they will be exercised before release.
- Prefer behavioral regression tests over implementation-detail assertions.
- Record manual checks that cannot be automated, including microphone permissions, tray behavior, global hotkeys, overlay placement, and clean-machine installation.
