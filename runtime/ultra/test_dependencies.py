"""Check checked-in upstream artifacts against the reviewed dependency lock."""
import json
from pathlib import Path
import re
import unittest


class DependencyPinsTests(unittest.TestCase):
    def test_every_locked_dependency_has_exactly_one_hash_verified_artifact(self):
        base = Path(__file__).resolve().parent
        pins = json.loads((base / "dependencies.json").read_text(encoding="utf-8"))
        lock = (base / "requirements.lock").read_text(encoding="utf-8")
        locked = {}
        for block in re.split(r"\n(?=[a-zA-Z0-9])", lock):
            match = re.match(r"([\w-]+)==([^\s\\]+)", block)
            if match:
                name, version = match.groups()
                locked[name] = (version, set(re.findall(r"--hash=sha256:([a-f0-9]{64})", block)))
        packages = {p["name"]: p for p in pins["packages"]}
        self.assertEqual(len(packages), len(pins["packages"]))
        self.assertEqual(set(packages), set(locked) | {"torch"})
        for name, (version, hashes) in locked.items():
            self.assertEqual(packages[name]["version"], version)
            self.assertIn(packages[name]["sha256"], hashes)
        torch = re.search(r"torch @ (https://\S+)#sha256=([a-f0-9]{64})", lock)
        self.assertEqual(packages["torch"]["url"], torch[1])
        self.assertEqual(packages["torch"]["sha256"], torch[2])


if __name__ == "__main__":
    unittest.main()
