#!/usr/bin/env python3
"""Verify release publication and retry behavior without contacting GitHub."""

import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


class PublishTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="bb-publish-test-")
        self.addCleanup(temporary.cleanup)
        self.directory = Path(temporary.name)
        self.root = Path(__file__).resolve().parents[1]
        shutil.copytree(self.root / "scripts", self.directory / "scripts")
        dist = self.directory / "dist"
        dist.mkdir()
        for archive in ("bb-0.1.0-x86_64-unknown-linux-gnu.tar.gz", "bb-0.1.0-x86_64-pc-windows-msvc.zip"):
            content = b"release asset fixture"
            (dist / archive).write_bytes(content)
            (dist / f"{archive}.sha256").write_text(f"{hashlib.sha256(content).hexdigest()}  {archive}\n")
        mocks = self.directory / "bin"
        mocks.mkdir()
        gh = mocks / "gh"
        gh.write_text('''#!/usr/bin/env python3
import json
import os
import sys

with open(os.environ["BB_TEST_CALLS"], "a") as log:
    log.write(json.dumps(sys.argv[1:]) + "\\n")
if sys.argv[1] == "api":
    if os.environ.get("BB_TEST_FAIL_LIST"):
        sys.exit(1)
    print(os.environ.get("BB_TEST_RELEASES", "[[]]"))
elif sys.argv[1:3] == ["release", "view"]:
    state = os.environ.get("BB_TEST_RELEASE", "missing")
    if state == "missing":
        sys.exit(1)
    print("true" if state == "draft" else "false")
elif sys.argv[1:3] == ["release", "upload"] and os.environ.get("BB_TEST_FAIL_UPLOAD"):
    sys.exit(1)
''')
        gh.chmod(0o755)
        self.environment = dict(os.environ, GH_REPO="test/bb-cli", RELEASE_TAG="v0.1.0",
                                BB_TEST_CALLS=str(self.directory / "calls.jsonl"))
        self.environment["PATH"] = str(mocks) + os.pathsep + self.environment["PATH"]

    def publish(self, succeeds=True):
        result = subprocess.run(["bash", "scripts/publish-release.sh"], cwd=self.directory,
                                env=self.environment, text=True, capture_output=True, check=False)
        self.assertEqual(result.returncode == 0, succeeds, result.stdout + result.stderr)
        calls = self.directory / "calls.jsonl"
        return [json.loads(line) for line in calls.read_text().splitlines()] if calls.exists() else []

    def test_stable_uploads_before_publishing_latest(self):
        calls = self.publish()
        self.assertEqual([call[:2] for call in calls], [
            ["release", "view"], ["release", "create"], ["release", "upload"],
            ["api", "repos/test/bb-cli/releases?per_page=100"], ["release", "edit"],
        ])
        self.assertIn("--draft", calls[1])
        self.assertIn("--verify-tag", calls[1])
        self.assertIn("--draft=false", calls[-1])
        self.assertIn("--latest=true", calls[-1])
        self.assertEqual((self.directory / "dist/VERSION").read_text(), "v0.1.0\n")
        assets = (self.directory / "dist/SHA256SUMS").read_text()
        for name in ("install.sh", "install.ps1", "VERSION", ".tar.gz", ".zip"):
            self.assertIn(name, assets)

    def test_linux_only_release_publishes_both_architectures(self):
        dist = self.directory / "dist"
        for asset in dist.glob("*.zip*"):
            asset.unlink()
        archive = "bb-0.1.0-aarch64-unknown-linux-gnu.tar.gz"
        content = b"ARM64 release asset fixture"
        (dist / archive).write_bytes(content)
        (dist / f"{archive}.sha256").write_text(f"{hashlib.sha256(content).hexdigest()}  {archive}\n")

        calls = self.publish()
        self.assertIn("--draft=false", calls[-1])
        self.assertIn("--latest=true", calls[-1])
        assets = (dist / "SHA256SUMS").read_text()
        for name in ("bb-0.1.0-x86_64-unknown-linux-gnu.tar.gz", archive):
            self.assertIn(f"dist/{name}", calls[2])
            self.assertIn(name, assets)
        self.assertNotIn(".zip", assets)
        subprocess.run(["sha256sum", "--check", "SHA256SUMS"], cwd=dist,
                       text=True, capture_output=True, check=True)

    def test_prerelease_does_not_replace_latest(self):
        self.environment["RELEASE_TAG"] = "v0.2.0-rc.1+build.2"
        calls = self.publish()
        self.assertIn("--prerelease=true", calls[1])
        self.assertIn("--prerelease", calls[-1])
        self.assertIn("--latest=false", calls[-1])

    def test_build_metadata_is_stable(self):
        self.environment["RELEASE_TAG"] = "v0.1.0+build-2"
        calls = self.publish()
        self.assertIn("--prerelease=false", calls[1])
        self.assertIn("--latest=true", calls[-1])

    def test_older_stable_release_does_not_replace_latest(self):
        self.environment["BB_TEST_RELEASES"] = json.dumps([
            [{"tag_name": "v0.2.0", "draft": False, "prerelease": False}],
        ])
        calls = self.publish()
        self.assertIn("--latest=false", calls[-1])

    def test_version_order_is_numeric_and_ignores_previews_and_drafts(self):
        self.environment["RELEASE_TAG"] = "v0.10.0"
        self.environment["BB_TEST_RELEASES"] = json.dumps([
            [{"tag_name": "v0.9.0", "draft": False, "prerelease": False}],
            [{"tag_name": "v1.0.0-rc.1", "draft": False, "prerelease": True},
             {"tag_name": "v2.0.0", "draft": True, "prerelease": False}],
        ])
        calls = self.publish()
        self.assertIn("--latest=true", calls[-1])

    def test_existing_draft_resumes_upload(self):
        self.environment["BB_TEST_RELEASE"] = "draft"
        calls = self.publish()
        self.assertEqual([call[0] if call[0] == "api" else call[1] for call in calls], ["view", "upload", "api", "edit"])

    def test_published_release_keeps_existing_assets(self):
        self.environment["BB_TEST_RELEASE"] = "published"
        calls = self.publish()
        self.assertEqual([call[1] for call in calls], ["view"])

    def test_failed_upload_never_publishes(self):
        self.environment["BB_TEST_FAIL_UPLOAD"] = "1"
        calls = self.publish(False)
        self.assertEqual([call[1] for call in calls], ["view", "create", "upload"])

    def test_corrupt_artifact_never_contacts_github(self):
        archive = next((self.directory / "dist").glob("*.tar.gz"))
        archive.write_bytes(b"corrupt")
        self.assertEqual(self.publish(False), [])

    def test_failed_version_query_keeps_release_as_draft(self):
        self.environment["BB_TEST_FAIL_LIST"] = "1"
        calls = self.publish(False)
        self.assertNotIn(["release", "edit"], [call[:2] for call in calls])


if __name__ == "__main__":
    unittest.main()
