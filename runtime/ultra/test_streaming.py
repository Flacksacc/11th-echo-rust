"""Exercise preview timing and exact tail draining using the installed runtime.

Run with the bundled python.exe -I runtime/ultra/test_streaming.py.
No GPU, model, microphone, or network is needed.
"""
import asyncio
import importlib.util
from pathlib import Path
import unittest

import numpy as np
from kestrel.models.parakeet_tdt import longform

spec = importlib.util.spec_from_file_location("echo_ultra_helper", Path(__file__).with_name("helper.py"))
helper = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helper)
helper.configure_streaming()


class StreamingTests(unittest.TestCase):
    def test_preview_threshold_cadence_and_full_context_final(self):
        async def run():
            source = longform.LiveAudioBuffer(
                16000, window_seconds=longform._STREAM_CHUNK_SECONDS,
                update_seconds=longform._LIVE_CHUNK_SECONDS,
            )
            received = 0

            async def chunks():
                nonlocal received
                # 3.75s, including a 0.25s tail. Keep PCM integer-representable
                # in float32 so exact final equality catches drops/duplicates.
                for offset in range(0, 60000, 4000):
                    received += 4000
                    yield np.arange(offset, offset + 4000, dtype=np.float32) / 65536

            windows = []
            async for window in longform._live_windows(chunks(), source, previews=True):
                windows.append((received, window))
            return windows

        windows = asyncio.run(run())
        previews = [(received, window) for received, window in windows if window.sample_count is not None]
        self.assertEqual([received / 16000 for received, _ in previews], [1.5, 2.5, 3.5])
        self.assertEqual([window.sample_count for _, window in previews], [16000] * 3)
        exact = [window for _, window in windows if window.sample_count is None]
        self.assertEqual(len(exact), 1)
        np.testing.assert_array_equal(exact[0].audio.waveform, np.arange(60000, dtype=np.float32) / 65536)

    def test_short_recording_still_finalizes_without_a_preview(self):
        async def run():
            source = longform.LiveAudioBuffer(16000, window_seconds=longform._STREAM_CHUNK_SECONDS, update_seconds=1)

            async def chunks():
                yield np.zeros(8000, dtype=np.float32)

            return [window async for window in longform._live_windows(chunks(), source, previews=True)]

        windows = asyncio.run(run())
        self.assertEqual(len(windows), 1)
        self.assertIsNone(windows[0].sample_count)
        self.assertEqual(windows[0].audio.waveform.size, 8000)


if __name__ == "__main__":
    unittest.main()
