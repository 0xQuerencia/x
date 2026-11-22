import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "x-qa-gate.py"
spec = importlib.util.spec_from_file_location("x_qa_gate", SCRIPT)
qa = importlib.util.module_from_spec(spec)
spec.loader.exec_module(qa)


class QaAcceptanceTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.path = Path(self.tmp.name) / "x-qa.json"
        self.source = {
            "version": 1,
            "platform": "windows11",
            "source_sha": "a" * 40,
            "cases": {},
        }

    def test_macos_screen_permission_and_capture_are_required(self):
        value = {
            "version": 1,
            "platform": "macos",
            "source_sha": "a" * 40,
            "cases": {name: "pass" for name in qa.SCENARIOS["macos"]},
        }
        self.assertTrue(qa.gate(qa.validate(value))["ready"])
        value["cases"].pop("screen_permission")
        self.assertFalse(qa.gate(qa.validate(value))["ready"])
        value["cases"]["screen_permission"] = "unsupported"
        self.assertFalse(qa.gate(qa.validate(value))["ready"])
        value["cases"]["screen_permission"] = "pass"
        value["cases"]["screenshot"] = "fail"
        self.assertFalse(qa.gate(qa.validate(value))["ready"])

    def test_missing_and_unsupported_never_mark_release_ready(self):
        qa.save(self.path, self.source, creating=True)
        result = qa.gate(qa.read(self.path))
        self.assertFalse(result["ready"])
        self.assertEqual(result["passed"], 0)
        data = qa.read(self.path)
        data["cases"] = {name: "pass" for name in qa.SCENARIOS["windows11"]}
        data["cases"]["microphone"] = "unsupported"
        qa.save(self.path, data)
        self.assertFalse(qa.gate(qa.read(self.path))["ready"])
        data["cases"]["microphone"] = "pass"
        qa.save(self.path, data)
        self.assertTrue(qa.gate(qa.read(self.path))["ready"])

    def test_only_fixed_cases_status_and_sha_are_accepted(self):
        for invalid in [
            {**self.source, "source_sha": "a" * 39},
            {**self.source, "platform": "invalid"},
            {**self.source, "cases": {"custom_secret": "pass"}},
            {**self.source, "cases": {"microphone": "great, username=alice"}},
            {**self.source, "secret_key": "should-not-be-stored"},
        ]:
            with self.assertRaises(ValueError):
                qa.validate(invalid)

    def test_record_never_overwrites_existing_or_linked_files(self):
        qa.save(self.path, self.source, creating=True)
        with self.assertRaises(ValueError):
            qa.save(self.path, self.source, creating=True)
        other = Path(self.tmp.name) / "other.json"
        other.symlink_to(self.path)
        with self.assertRaises(ValueError):
            qa.save(other, self.source)
        with self.assertRaises(ValueError):
            qa.read(other)
        self.assertEqual(qa.read(self.path)["cases"], {})

    def test_no_private_content_or_unverified_completion_is_serialized(self):
        qa.save(self.path, self.source, creating=True)
        text = self.path.read_text()
        self.assertNotIn("password", text)
        self.assertNotIn("window_title", text)
        self.assertNotIn("transcript", text)
        result = qa.gate(qa.read(self.path))
        self.assertFalse(result["ready"])
        self.assertFalse(result["release_published"])
        self.assertTrue(result["operator_report_only"])
        self.assertEqual(len(result["missing"]), len(qa.SCENARIOS["windows11"]))


if __name__ == "__main__":
    unittest.main()
