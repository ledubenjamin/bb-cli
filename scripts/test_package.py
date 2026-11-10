#!/usr/bin/env python3
"""Exercise release packaging without compiling or executing a platform binary."""

import hashlib
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import zipfile

import package


class PackagingTests(unittest.TestCase):
    def test_zip_preserves_epoch_dated_dependency_notices_and_bundled_skill(self):
        with tempfile.TemporaryDirectory(prefix="bb-package-test-") as temporary:
            directory = Path(temporary)
            source = directory / "dependency"
            source.mkdir()
            manifest = source / "Cargo.toml"
            manifest.write_text('[package]\nname = "fixture"\nversion = "1.0.0"\n')
            notice = source / "LICENSE"
            notice.write_bytes(b"Fixture dependency notice\n")
            os.utime(notice, (0, 0))
            binary = directory / "bb.exe"
            binary.write_bytes(b"fixture binary contents")
            destination = directory / "dist"
            skill = b"---\nname: bb-cli\ndescription: Fixture instructions\n---\nUse bb schema.\n"
            metadata = {"packages": [
                {"name": "bb-cli", "version": "1.2.3", "source": None},
                {"name": "fixture", "version": "1.0.0", "source": "registry+fixture",
                 "manifest_path": str(manifest), "license": "MIT"},
            ]}

            def command_output(arguments, **kwargs):
                if arguments[0] == "cargo":
                    return json.dumps(metadata).encode()
                self.assertEqual(arguments[0], str(binary.resolve()))
                if arguments[1:] == ["--version"]:
                    self.assertTrue(kwargs.get("text"))
                    return "bb 1.2.3\n"
                if arguments[1:] == ["schema"]:
                    return b'{"commands": []}\n'
                if arguments[1:] == ["skills", "show"]:
                    return skill
                if arguments[1] == "completion":
                    return b"fixture completion\n"
                self.fail(f"unexpected packaged binary invocation: {arguments}")

            target = "x86_64-pc-windows-msvc"
            with patch("sys.argv", ["package.py", "--target", target, "--binary", str(binary),
                                    "--output", str(destination)]), \
                 patch.dict(os.environ, {"RELEASE_REF": "refs/tags/v1.2.3"}), \
                 patch.object(package.subprocess, "check_output", side_effect=command_output):
                package.main()

            name = f"bb-1.2.3-{target}"
            archive = destination / f"{name}.zip"
            with zipfile.ZipFile(archive) as contents:
                self.assertIsNone(contents.testzip())
                self.assertEqual(contents.read(f"{name}/bb.exe"), binary.read_bytes())
                self.assertEqual(contents.read(f"{name}/skills/bb-cli/SKILL.md"), skill)
                entry = f"{name}/third-party/fixture-1.0.0/LICENSE"
                self.assertEqual(contents.read(entry), notice.read_bytes())
                self.assertEqual(contents.getinfo(entry).date_time, (1980, 1, 1, 0, 0, 0))
            self.assertEqual(notice.stat().st_mtime, 0)
            self.assertEqual(archive.with_suffix(".zip.sha256").read_text(),
                             f"{hashlib.sha256(archive.read_bytes()).hexdigest()}  {archive.name}\n")


if __name__ == "__main__":
    unittest.main()
