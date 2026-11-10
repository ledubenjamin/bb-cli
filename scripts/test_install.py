#!/usr/bin/env python3
"""Exercise the native release installer offline against an actual packaged binary."""

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True)
    parser.add_argument("--dist", required=True, type=Path)
    parser.add_argument("--powershell", choices=("powershell", "pwsh"), default="powershell")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    windows = os.name == "nt"
    extension = ".zip" if windows else ".tar.gz"
    suffix = f"-{args.target}{extension}"
    archives = list(args.dist.glob(f"bb-*{suffix}"))
    if len(archives) != 1:
        parser.error("expected exactly one native archive")
    archive = archives[0]
    version = archive.name[3:-len(suffix)]
    tag = f"v{version}"

    with tempfile.TemporaryDirectory(prefix="bb-install-test-") as temporary:
        directory = Path(temporary)
        fixtures = directory / "downloads"
        fixtures.mkdir()
        shutil.copy2(archive, fixtures / archive.name)
        checksum = fixtures / f"{archive.name}.sha256"
        shutil.copy2(archive.with_suffix(archive.suffix + ".sha256"), checksum)
        (fixtures / "VERSION").write_text(tag + "\n")
        destination = directory / "install with spaces é"
        environment = dict(os.environ, BB_INSTALL_DIR=str(destination), BB_VERSION="",
                           BB_TEST_DOWNLOADS=str(fixtures), BB_TEST_TAG=tag)
        if windows:
            # A pwsh -> Python -> powershell chain otherwise inherits PS7 modules
            # that Windows PowerShell 5.1 cannot load (including Get-FileHash).
            # Let the selected shell construct its own default module paths.
            environment = {key: value for key, value in environment.items()
                           if key.upper() != "PSMODULEPATH"}
            mock = directory / "install-test.ps1"
            mock.write_text(r'''
$ErrorActionPreference = 'Stop'
function Invoke-RestMethod {
    param([string]$Uri)
    if ($Uri -ne 'https://github.com/ledubenjamin/bb-cli/releases/latest/download/VERSION') {
        throw "Unexpected version URL: $Uri"
    }
    Get-Content -LiteralPath (Join-Path $env:BB_TEST_DOWNLOADS 'VERSION') -Raw
}
function Invoke-WebRequest {
    param([string]$Uri, [switch]$UseBasicParsing, [string]$OutFile)
    $prefix = "https://github.com/ledubenjamin/bb-cli/releases/download/$env:BB_TEST_TAG/"
    if (-not $Uri.StartsWith($prefix)) { throw "Unexpected asset URL: $Uri" }
    Copy-Item -LiteralPath (Join-Path $env:BB_TEST_DOWNLOADS $Uri.Substring($prefix.Length)) -Destination $OutFile
}
& $env:BB_TEST_INSTALLER -NoPathUpdate
''')
            environment["BB_TEST_INSTALLER"] = str(root / "scripts/install.ps1")
            command = [args.powershell, "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", str(mock)]
        else:
            mocks = directory / "bin"
            mocks.mkdir()
            curl = mocks / "curl"
            curl.write_text('''#!/usr/bin/env python3
import os
from pathlib import Path
import sys

args = sys.argv[1:]
url = next(a for a in args if a.startswith("https://"))
base = "https://github.com/ledubenjamin/bb-cli/releases/"
name = url.rsplit("/", 1)[1]
expected = base + ("latest/download/VERSION" if name == "VERSION" else "download/" + os.environ["BB_TEST_TAG"] + "/" + name)
if url != expected:
    sys.exit("unexpected download URL: " + url)
data = (Path(os.environ["BB_TEST_DOWNLOADS"]) / name).read_bytes()
if "--output" in args:
    Path(args[args.index("--output") + 1]).write_bytes(data)
else:
    sys.stdout.buffer.write(data)
''')
            curl.chmod(0o755)
            environment["PATH"] = str(mocks) + os.pathsep + environment["PATH"]
            command = ["sh", str(root / "scripts/install.sh")]

        binary = destination / ("bb.exe" if windows else "bb")

        def install(succeeds):
            result = subprocess.run(command, env=environment, text=True, capture_output=True, check=False, timeout=90)
            if (result.returncode == 0) != succeeds:
                raise AssertionError(f"unexpected installer exit {result.returncode}:\n{result.stdout}\n{result.stderr}")
            if succeeds:
                actual = subprocess.check_output([str(binary), "--version"], text=True, timeout=10).strip()
                if actual != f"bb {version}":
                    raise AssertionError(f"wrong installed version: {actual}")

        install(True)  # Latest discovery and first installation.
        previous = directory / "previous-binary"
        previous.write_bytes(b"previous version")
        previous.replace(binary)
        environment["BB_VERSION"] = tag
        install(True)  # Explicit version and replacement of an existing binary.
        installed = binary.read_bytes()
        # Exercise the installed executable, including bundled assets, with no credentials.
        project = directory / "project with spaces é"
        project.mkdir()
        offline = {key: value for key, value in os.environ.items() if not key.startswith("BB_")}
        offline.update(BB_CONFIG_DIR=str(directory / "empty-config"), BB_API_URL="invalid URL")

        def bb(*arguments):
            return subprocess.check_output([str(binary), *arguments], cwd=project, env=offline, timeout=10)

        json.loads(bb("schema"))
        for agent, folder in (("codex", ".agents"), ("claude", ".claude")):
            args_install = ("skills", "install", "--agent", agent, "--json", "*")
            assert json.loads(bb(*args_install))["actions"][0]["changed"] is True
            assert (project / folder / "skills/bb-cli/SKILL.md").read_bytes() == bb("skills", "show")
            assert json.loads(bb(*args_install))["actions"][0]["changed"] is False
            assert json.loads(bb("skills", "status", "--agent", agent, "--json", "status"))["status"] == "current"
            bb("skills", "remove", "--agent", agent)
            assert not (project / folder / "skills/bb-cli/SKILL.md").exists()
        original_checksum = checksum.read_bytes()
        checksum.write_text(f"{'0' * 64}  {archive.name}\n")
        install(False)
        if binary.read_bytes() != installed:
            raise AssertionError("checksum failure replaced the installed binary")
        checksum.write_bytes(original_checksum)
        (fixtures / archive.name).unlink()
        install(False)
        if binary.read_bytes() != installed:
            raise AssertionError("download failure replaced the installed binary")
        print(f"Installer checks passed for {args.target}: install, update, skill lifecycle, bad checksum, failed download")


if __name__ == "__main__":
    main()
