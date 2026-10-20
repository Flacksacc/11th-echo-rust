These MP3 files contain generated 440 Hz sine waves, not speech or recordings.
They exercise Echo's native MP3 decoder without requiring audio hardware,
Pocket TTS weights, or an external converter during tests.

- `voice-short.mp3`: 0.25 seconds, 24 kHz stereo, 32 kbps.
- `voice-too-long.mp3`: 30.25 seconds, 16 kHz mono, 8 kbps.

Generated with FFmpeg's `sine` source and `libmp3lame` encoder.
