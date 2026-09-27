import asyncio
import importlib.util
import io
import json
from pathlib import Path
import struct
import unittest

spec = importlib.util.spec_from_file_location("echo_ultra_helper", Path(__file__).with_name("helper.py"))
helper = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helper)


def frame(session, kind, **fields):
    data = json.dumps({"protocol": 1, "session": session, "kind": kind, **fields}).encode()
    return struct.pack("<I", len(data)) + data


class FakeUpdates:
    def __init__(self, audio):
        self.audio = audio
        self.captured = []

    async def __aiter__(self):
        async for chunk in self.audio:
            self.captured.extend(chunk)
            yield {"text": "replacing preview"}

    async def aresult(self):
        return {"text": "Final tail included."}


class FakeSpeech:
    async def atranscribe(self, **prompt):
        self.updates = FakeUpdates(prompt["audio"])
        return self.updates


class FakeArray(list):
    def __truediv__(self, scale):
        return FakeArray(value / scale for value in self)


class FakeNumpy:
    float32 = object()

    @staticmethod
    def asarray(samples, dtype):
        return FakeArray(samples)


class HelperTests(unittest.TestCase):
    def test_rejects_oversize_truncation_and_wrong_version(self):
        for data in [struct.pack("<I", helper.MAX_FRAME + 1), frame(1, "audio")[:-1]]:
            with self.assertRaises((ValueError, EOFError)):
                helper.read_frame(io.BytesIO(data))
        data = b'{"protocol":2,"session":1}'
        with self.assertRaises(ValueError):
            helper.read_frame(io.BytesIO(struct.pack("<I", len(data)) + data))

    def test_final_follows_all_audio_and_replacement_previews(self):
        source = io.BytesIO(frame(7, "audio", audio=[-32768, 0]) + frame(7, "audio", audio=[32767]) + frame(7, "stop"))
        output = io.BytesIO()
        speech = FakeSpeech()
        asyncio.run(helper.transcribe_session(speech, source, output, 7, FakeNumpy))
        self.assertEqual(speech.updates.captured, [-1.0, 0.0, 32767 / 32768])
        output.seek(0)
        messages = [helper.read_frame(output) for _ in range(3)]
        self.assertEqual([message["kind"] for message in messages], ["partial", "partial", "final"])
        self.assertEqual(messages[-1]["text"], "Final tail included.")
        self.assertEqual(output.read(), b"")

    def test_invalid_audio_cancels_without_final(self):
        for samples in [[32768], [True], [], [1] * 8193]:
            output = io.BytesIO()
            with self.assertRaises(ValueError):
                asyncio.run(helper.transcribe_session(FakeSpeech(), io.BytesIO(frame(1, "audio", audio=samples)), output, 1, FakeNumpy))
            self.assertNotIn(b'"final"', output.getvalue())

    def test_errors_never_echo_private_dependency_messages(self):
        self.assertNotIn("private transcript", helper.public_error(RuntimeError("private transcript")))


if __name__ == "__main__":
    unittest.main()
