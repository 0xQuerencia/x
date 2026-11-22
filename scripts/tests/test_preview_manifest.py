import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "write-preview-manifest.py"
SHA = "a" * 40


class PreviewManifestTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        (self.root / "x.exe").write_bytes(b"test X preview binary")

    def call(self, command, *extra):
        args = [
            sys.executable, str(SCRIPT), command,
            "--directory", str(self.root), "--source-sha", SHA,
            "--platform", "windows",
        ]
        if command == "create":
            args += [
                "--target", "x86_64-pc-windows-msvc",
                "--compiler", "rustc 1.100.0",
                "--artifact", "x.exe",
            ]
        args.extend(extra)
        return subprocess.run(args, text=True, capture_output=True, check=False)

    def make(self):
        result = self.call("create")
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_manifest_round_trip_records_source_and_digest(self):
        self.make()
        payload = json.loads((self.root / "BUILD_INFO.json").read_text())
        self.assertEqual(payload["source_sha"], SHA)
        self.assertIs(payload["signed"], False)
        self.assertEqual(payload["artifacts"][0]["bytes"], 21)
        self.assertEqual(self.call("verify").returncode, 0)

    def test_tampered_artifact_and_mismatched_commit_fail(self):
        self.make()
        (self.root / "x.exe").write_bytes(b"tampered")
        self.assertNotEqual(self.call("verify").returncode, 0)
        (self.root / "x.exe").write_bytes(b"test X preview binary")
        bad_source = subprocess.run(
            [
                sys.executable, str(SCRIPT), "verify",
                "--directory", str(self.root),
                "--source-sha", "b" * 40,
                "--platform", "windows",
            ],
            text=True, capture_output=True, check=False,
        )
        self.assertNotEqual(bad_source.returncode, 0)

    def test_macos_preview_round_trip_and_platform_mismatch(self):
        source = self.root / "x-macos-arm64"
        source.write_bytes(b"mock Mach-O bytes")
        create = subprocess.run(
            [
                sys.executable, str(SCRIPT), "create",
                "--directory", str(self.root),
                "--source-sha", SHA, "--platform", "macos",
                "--target", "aarch64-apple-darwin",
                "--compiler", "rustc 1.100.0",
                "--artifact", "x-macos-arm64",
            ],
            text=True, capture_output=True, check=False,
        )
        self.assertEqual(create.returncode, 0, create.stderr)
        report = json.loads((self.root / "BUILD_INFO.json").read_text())
        self.assertEqual(report["platform"], "macos")
        self.assertFalse(report["signed"])
        verify = subprocess.run(
            [
                sys.executable, str(SCRIPT), "verify",
                "--directory", str(self.root), "--source-sha", SHA,
                "--platform", "macos",
            ],
            text=True, capture_output=True, check=False,
        )
        self.assertEqual(verify.returncode, 0, verify.stderr)
        self.assertNotEqual(self.call("verify").returncode, 0)

    def test_manifest_does_not_follow_path_traversal(self):
        self.make()
        manifest = self.root / "BUILD_INFO.json"
        payload = json.loads(manifest.read_text())
        payload["artifacts"][0]["name"] = "../outside.exe"
        manifest.write_text(json.dumps(payload))
        self.assertNotEqual(self.call("verify").returncode, 0)

    def test_unsigned_claim_and_duplicate_files_are_refused(self):
        self.make()
        manifest = self.root / "BUILD_INFO.json"
        payload = json.loads(manifest.read_text())
        payload["signed"] = True
        manifest.write_text(json.dumps(payload))
        self.assertNotEqual(self.call("verify").returncode, 0)
        duplicate = self.call("create", "--artifact", "x.exe")
        self.assertNotEqual(duplicate.returncode, 0)


if __name__ == "__main__":
    unittest.main()
