#!/usr/bin/env bash
set -euo pipefail

want="${1:?usage: scripts/release-notes.sh X.Y.Z}"
root="$(cd "$(dirname "$0")/.." && pwd)"
awk -v heading="## [$want]" '
  index($0, heading) == 1 { printing = 1; next }
  printing && /^## \[/ { exit }
  printing { print }
' "$root/CHANGELOG.md"
