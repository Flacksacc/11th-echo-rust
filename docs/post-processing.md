# Native Windows post-processing

Echo uses Rust English written-form rules (`text2num` 2.8.0) and the sherpa-onnx 1.13.4 Edge-Punct-Casing INT8 model, on CPU with one inference thread. The model jointly restores sentence punctuation and predicts word casing. No Python, WSL, external ONNX installation, or new cloud service is required. Optional Gemini rewriting remains a separate existing feature.

## Workflow

The overlay remains the raw speech transcript. Committed provider fragments accumulate without post-processing. After manual stop and provider drain, Echo processes the entire session once: optional Gemini, source sentence-punctuation stripping, written rules, then punctuation inference. Only the final result is sent to the original injection target and added to history. Formatting runs off the UI/async executor thread. Stale sessions cannot paste; if formatting settings change during processing, source text is retained with a warning.

If the model is missing, invalid, or changes words, written rules still run against the original source punctuation and an actionable warning is shown. No transcript text is logged by the formatter. Long transcripts use 96-atom windows with 16 atoms of context on each side and deterministic boundary ownership.

## Settings

Settings → Post processing has a default-on master switch and hover/keyboard-focus help icons. Save settings persists every rule; Discard restores all of them.

- Ordinary-number master: whole numbers, ordinals, decimals/signed values. The small-count preference is independent of compound formats.
- Independent compound categories: currency, measurements/percentages, named-month dates, explicit AM/PM/o'clock times, cued telephone/identifier digit sequences, email/web addresses.
- Punctuation rebuild, model-predicted sentence/word capitalization, and separate comma/period/question-mark filters.
- Protected phrases and `spoken => written` custom replacements, one per line. Invalid, duplicate, or directly conflicting replacements prevent saving. Precedence: protected phrase, longest custom replacement, built-in rules.

Examples: `twelve dollars and five cents` → `$12.05`; `john dot smith at example dot com` → `john.smith@example.com`; `john smith at north wind dot co dot uk` → `johnsmith@northwind.co.uk`; `three kilograms` → `3 kg`; `code a zero zero seven` → `code A007`.

Protected spans are text the formatter must not rewrite internally: addresses, existing written values, acronyms, custom replacement results, and user-protected phrases. The neural inference view substitutes a natural word for each protected span and validates every returned word. Only predicted boundary punctuation and uppercase letters are copied back; the original protected text is never reconstructed from model output.

This is a conservative English rules engine, not a grammar or spelling rewriter. Numeric dates are not guessed. Phone/identifier sequences require a cue. Spoken email local-parts and domain labels can contain multiple alphanumeric words; these are joined unless you dictate `dot`, `dash`, `underscore`, or `plus`. Email conversion requires `at` plus a recognized domain ending, so unsupported or incomplete addresses stay as spoken text. URL parsing supports common spoken separators and explicit paths, not arbitrary natural-language descriptions of query strings. Ambiguous pounds mean weight; say "pounds sterling" for currency. Punctuation and capitalization can still be inaccurate. Existing case and protected names are preserved.

## Installation

Enabling the feature (or starting with it enabled) offers installation if punctuation is needed and the model is unavailable. There is no settings-page Download button or automatic network download during dictation. Explicit confirmation opens progress through download, SHA-256 verification/unpacking, native model loading/inference testing, and an acknowledged Ready to use screen. Failures remain visible and retryable. Recording and installation are mutually exclusive, including global-hotkey starts. Closing the main window cannot hide an active installation.

The previously downloaded archive is reused only if its exact size and SHA-256 match. Archive extraction rejects unsafe paths and non-regular model entries. Installation stages and tests the new model before replacing the old file, with rollback if renaming fails. Installed model bytes are also verified before native loading.

Pinned package: `sherpa-onnx-online-punct-en-2024-08-06.tar.bz2` from the sherpa-onnx `punctuation-models` release. It contains the English Edge-Punct-Casing model and BPE vocabulary.

- Archive size: 30,667,839 bytes
- Archive SHA-256: `9f5e5a72c7d2829635bd074fce92b6bbd5b78da8a52e7ad8ed1be933f366b99d`
- Model SHA-256: `9d611f445fe4a46186080fe161be6059d87d72eb88d3a8cb00c1a06e83a6067e`
- Vocabulary SHA-256: `e118b7ad88c54db562517df49e1cffd4836d166c34fb190fd311d7f34eb238f5`
- User cache: `%LOCALAPPDATA%\11th_echo\models\punctuation`

## Verification

Run `cargo test`, `cargo check`, `cargo fmt --check`, and `cargo clippy --all-targets --all-features -- -D warnings`.

Opt-in native CPU and cached-installation tests do not modify the user's installed model or initiate downloads:

```powershell
$env:ECHO_PUNCTUATION_TEST_ARCHIVE = 'C:\path\to\verified-package.tar.bz2'
cargo test post_processing::tests::native_model_smoke -- --ignored --nocapture
cargo test cached_model_installation_smoke -- --ignored --nocapture
```

An isolated settings-window test checks both directions of every settings binding and renders all settings pages plus installation/completion states. It does not register hotkeys, capture audio, use network services, or save user settings. Screenshots go to the temporary directory. Use software rendering for reliable snapshot capture:

```powershell
$env:SLINT_BACKEND = 'winit-software'
cargo test post_processing_settings_ui_smoke -- --ignored --nocapture
```

Before release, manually exercise real dictation → stop → paste, tooltip hover/focus and scrolling at supported display scaling, tray/hotkey behavior, consent/retry/startup installation, and a clean-account installer. Automated and isolated model/UI tests do not replace those checks.
