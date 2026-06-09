# Transcription Providers

The application runtime talks to transcription services through `src/transcription`.
Provider implementations should preserve the app-level contract below.

## Runtime Contract

- Input audio is mono signed 16-bit PCM at 16 kHz, passed as `Vec<i16>` chunks.
- Commands are `Start` and `Stop`.
- Events are `Partial`, `Committed`, and `Error`.
- `Partial` text is for live display only.
- `Committed` text is appended to the transcript pipeline and can trigger final text injection after stop.
- `Error` should be terminal for the active session.

## Adding a Provider

1. Add a provider module under `src/transcription`.
2. Implement a `run(audio_rx, command_rx, event_tx, log_tx)` method with the same signature used by `TranscriberClient`.
3. Add a variant to `TranscriptionProvider`.
4. Add a variant to `TranscriberClient` and wire it in `from_config`.
5. Add provider-specific settings while keeping existing defaults backward compatible.
6. Add parser/config tests for the provider.

## Current Provider

`ElevenLabsRealtimeTranscriber` uses ElevenLabs realtime speech-to-text:

- Endpoint: `wss://api.elevenlabs.io/v1/speech-to-text/realtime`
- Default model: `scribe_v2_realtime`
- Audio format: `pcm_16000`
- Commit strategy: `manual`
- Default language: `en`
- Default `no_verbatim`: `true`

The silence-before-commit behavior is intentionally part of the ElevenLabs adapter
because it is specific to the current realtime WebSocket behavior.

`OpenAiRealtimeWhisperTranscriber` uses OpenAI Realtime transcription:

- Endpoint: `wss://api.openai.com/v1/realtime?intent=transcription`
- Default model: `gpt-realtime-whisper`
- Session update event: `session.update` with `session.type` set to `transcription`
- Input format sent to OpenAI: `audio/pcm` at 24 kHz
- App input format: 16 kHz PCM, resampled to 24 kHz in the adapter
- Commit strategy: manual `input_audio_buffer.commit`
- Default language: `en`

OpenAI's file-oriented `gpt-4o-transcribe` and `gpt-4o-mini-transcribe`
models are intentionally not used here because this app needs live transcript
deltas from microphone audio.
