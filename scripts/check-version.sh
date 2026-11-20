#!/usr/bin/env bash
set -euo pipefail

want="${1:?usage: scripts/check-version.sh X.Y.Z}"
root="$(cd "$(dirname "$0")/.." && pwd)"
status=0

check() {
  local what="$1" got="$2"
  if [[ "$got" != "$want" ]]; then
    echo "version mismatch: $what is '$got', the release is '$want'" >&2
    status=1
  fi
}

cargo_version() {
  sed -n '/^\[package\]/,/^\[/{s/^version = "\(.*\)"/\1/p;}' "$root/crates/$1/Cargo.toml" | head -n 1
}

for crate in popsub popsub_protocol popsub_sdk; do
  check "crates/$crate" "$(cargo_version "$crate")"
done
check "popsub_sdk's popsub_protocol dependency" \
  "$(sed -n 's/^popsub_protocol = .*version = "\([^"]*\)".*/\1/p' "$root/crates/popsub_sdk/Cargo.toml")"
check "clients/typescript/package.json" \
  "$(sed -n 's/^  "version": "\(.*\)",$/\1/p' "$root/clients/typescript/package.json" | head -n 1)"
check "clients/python/pyproject.toml" \
  "$(sed -n 's/^version = "\(.*\)"$/\1/p' "$root/clients/python/pyproject.toml" | head -n 1)"
if ! grep -q "^## \[$want\]" "$root/CHANGELOG.md"; then
  echo "CHANGELOG.md has no '## [$want]' section" >&2
  status=1
fi
exit "$status"
