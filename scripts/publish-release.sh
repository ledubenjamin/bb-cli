#!/usr/bin/env bash
# Keep incomplete assets in a draft; publish only after every upload succeeds.
set -euo pipefail

: "${RELEASE_TAG:?A version tag is required}"
: "${GH_REPO:?A GitHub repository is required}"

cp scripts/install.sh scripts/install.ps1 dist/
printf '%s\n' "$RELEASE_TAG" > dist/VERSION
(
  cd dist
  sha256sum --check -- *.sha256
  cat -- *.sha256 > SHA256SUMS
  sha256sum -- install.sh install.ps1 VERSION >> SHA256SUMS
)

prerelease=false
case "${RELEASE_TAG%%+*}" in
  *-*) prerelease=true ;;
esac

if draft=$(gh release view "$RELEASE_TAG" --json isDraft --jq .isDraft); then
  if [ "$draft" != true ]; then
    printf 'Release %s is already published; keeping its assets unchanged.\n' "$RELEASE_TAG"
    exit 0
  fi
else
  gh release create "$RELEASE_TAG" --draft --verify-tag --generate-notes \
    --title "bb $RELEASE_TAG" --prerelease="$prerelease"
fi

# A failed upload can be retried while the release remains a draft.
gh release upload "$RELEASE_TAG" dist/* --clobber
if [ "$prerelease" = true ]; then
  gh release edit "$RELEASE_TAG" --draft=false --prerelease --latest=false
else
  # An older build may finish later. Compare stable versions while the workflow
  # holds its publication concurrency slot so Latest never moves backwards.
  latest=$(gh api "repos/$GH_REPO/releases?per_page=100" --paginate --slurp | python3 -c '
import json
import os
import re
import sys

def version(tag):
    match = re.fullmatch(r"v([0-9]+)\.([0-9]+)\.([0-9]+)(?:\+[0-9A-Za-z.-]+)?", tag)
    return tuple(map(int, match.groups())) if match else None

current = version(os.environ["RELEASE_TAG"])
if current is None:
    raise SystemExit("Expected a stable semantic version tag")
published = [
    version(release["tag_name"])
    for page in json.load(sys.stdin) for release in page
    if not release["draft"] and not release["prerelease"]
]
print(str(all(current > other for other in published if other is not None)).lower())
')
  gh release edit "$RELEASE_TAG" --draft=false --prerelease=false --latest="$latest"
fi
