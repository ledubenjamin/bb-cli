#!/usr/bin/env python3
"""Package a native bb build using its offline discovery and skill commands."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tempfile
import zipfile


def dependency_notices(packages, directory):
    """Preserve bundled notices and a source inventory, including target/dev dependencies."""
    destination = directory / "third-party"
    destination.mkdir()
    inventory = []
    for package in packages:
        if not package.get("source"):
            continue
        source = Path(package["manifest_path"]).parent.resolve()
        notices = []
        for candidate in sorted(source.rglob("*")):
            name = candidate.name.lower()
            if not candidate.is_file() or not name.startswith(("license", "licence", "copying", "copyright", "notice")):
                continue
            try:
                relative = candidate.resolve().relative_to(source)
            except ValueError:
                continue
            target = destination / f"{package['name']}-{package['version']}" / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(candidate, target)
            notices.append(str(target.relative_to(directory)))
        inventory.append({
            "name": package["name"], "version": package["version"],
            "license": package.get("license"), "repository": package.get("repository"),
            "source": f"https://crates.io/api/v1/crates/{package['name']}/{package['version']}/download",
            "notices": notices,
        })
    (destination / "dependencies.json").write_text(json.dumps(inventory, indent=2) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--output", type=Path, default=Path("dist"))
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--format-version", "1", "--locked"], cwd=root
    ))
    version = next(package["version"] for package in metadata["packages"] if package["name"] == "bb-cli")
    if not re.fullmatch(r"[0-9A-Za-z.+-]+", version):
        parser.error("invalid package version")
    if not re.fullmatch(r"[a-z0-9_-]+", args.target):
        parser.error("invalid Rust target")
    release_ref = os.environ.get("RELEASE_REF", "")
    if release_ref.startswith("refs/tags/") and release_ref != f"refs/tags/v{version}":
        parser.error("release tag must match the Cargo workspace version")
    binary = args.binary.resolve(strict=True)
    actual_version = subprocess.check_output([str(binary), "--version"], text=True).strip()
    if actual_version != f"bb {version}":
        parser.error(f"binary version mismatch: {actual_version}")
    args.output.mkdir(parents=True, exist_ok=True)
    name = f"bb-{version}-{args.target}"
    with tempfile.TemporaryDirectory(prefix="bb-package-") as temporary:
        directory = Path(temporary) / name
        directory.mkdir()
        shutil.copy2(binary, directory / binary.name)
        for filename in ("README.md", "LICENSE", "NOTICE", "Cargo.lock", "CHANGELOG.md", "CONTRIBUTING.md", "SECURITY.md"):
            shutil.copy2(root / filename, directory / filename)
        shutil.copytree(root / "docs", directory / "docs")
        dependency_notices(metadata["packages"], directory)
        schema = subprocess.check_output([str(binary), "schema"])
        json.loads(schema)  # Refuse to package corrupt discovery output.
        (directory / "schema.json").write_bytes(schema)
        skill = directory / "skills" / "bb-cli"
        skill.mkdir(parents=True)
        (skill / "SKILL.md").write_bytes(subprocess.check_output([str(binary), "skills", "show"]))
        completions = directory / "completions"
        completions.mkdir()
        for shell, filename in (("bash", "bb.bash"), ("zsh", "_bb"), ("fish", "bb.fish"), ("powershell", "bb.ps1")):
            content = subprocess.check_output([str(binary), "completion", shell])
            (completions / filename).write_bytes(content)
        if "windows" in args.target:
            archive = args.output / f"{name}.zip"
            # Cargo sources can use the Unix epoch. ZIP dates start in 1980;
            # clamp unsupported dates without changing notice contents or sources.
            with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED,
                                 strict_timestamps=False) as output:
                for file in sorted(directory.rglob("*")):
                    if file.is_file():
                        output.write(file, file.relative_to(directory.parent))
        else:
            archive = args.output / f"{name}.tar.gz"
            with tarfile.open(archive, "w:gz") as output:
                output.add(directory, arcname=name)
    digest = hashlib.sha256()
    with archive.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    checksum = digest.hexdigest()
    archive.with_suffix(archive.suffix + ".sha256").write_bytes(f"{checksum}  {archive.name}\n".encode())
    print(archive)


if __name__ == "__main__":
    main()
