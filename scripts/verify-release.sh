#!/usr/bin/env bash
set -uo pipefail

version="${1:?usage: scripts/verify-release.sh X.Y.Z}"
repo="${GITHUB_REPOSITORY:-soumyadepp/popsub}"
deadline=$(( $(date +%s) + ${VERIFY_TIMEOUT_SECS:-900} ))
agent="popsub-release-check (github.com/$repo)"
missing=0

found() {
  local name="$1"
  shift
  while true; do
    if "$@" > /dev/null 2>&1; then
      echo "found:   $name"
      return 0
    fi
    if (( $(date +%s) >= deadline )); then
      echo "MISSING: $name" >&2
      missing=1
      return 1
    fi
    sleep 20
  done
}

http_ok() {
  [[ "$(curl -sL -o /dev/null -w '%{http_code}' -A "$agent" "$1")" == "200" ]]
}

found "crates.io popsub_protocol $version" http_ok "https://crates.io/api/v1/crates/popsub_protocol/$version"
found "crates.io popsub_sdk $version" http_ok "https://crates.io/api/v1/crates/popsub_sdk/$version"
found "npm popsub $version" http_ok "https://registry.npmjs.org/popsub/$version"
found "PyPI popsub $version" http_ok "https://pypi.org/pypi/popsub/$version/json"
found "Go module clients/go v$version" http_ok "https://proxy.golang.org/github.com/soumyadepp/popsub/clients/go/@v/v$version.info"
found "ghcr.io image $version" docker manifest inspect "ghcr.io/$repo:$version"
for target in x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu aarch64-apple-darwin; do
  asset="popsub-$version-$target.tar.gz"
  found "release asset $asset" http_ok "https://github.com/$repo/releases/download/v$version/$asset"
done

exit "$missing"
