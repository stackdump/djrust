#!/usr/bin/env bash
# Build and publish `djwasm` to npm (needs `npm login` as an owner), then confirm jsDelivr serves
# the same bytes. The jsDelivr URLs and SRI hashes it prints are what a consumer pins.
#
#   cabi/publish.sh [--dry-run]
set -euo pipefail
cd "$(dirname "$0")"
./build.sh >/dev/null
version=$(python3 -c 'import json;print(json.load(open("pkg/package.json"))["version"])')
if [ "${1:-}" = "--dry-run" ]; then (cd pkg && npm publish --dry-run); exit 0; fi
if npm view "djwasm@$version" version >/dev/null 2>&1; then
  echo "djwasm@$version is already on npm: versions are immutable; bump djrust's version" >&2; exit 1
fi
(cd pkg && npm publish --access public)
url="https://cdn.jsdelivr.net/npm/djwasm@$version/djwasm.wasm"
want=$(python3 -c 'import json;print(json.load(open("pkg/hashes.json"))["files"]["djwasm.wasm"]["sha256"])')
for i in $(seq 1 30); do
  have=$(curl -fsSL "$url" 2>/dev/null | sha256sum | cut -d' ' -f1) || true
  [ "$have" = "$want" ] && { echo "jsDelivr serves $url ($want)"; exit 0; }
  sleep 10
done
echo "published, but jsDelivr did not serve matching bytes within 5 minutes: check $url" >&2; exit 1
