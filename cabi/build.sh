#!/usr/bin/env bash
# Build the `djwasm` npm package into cabi/pkg. Publishes nothing: `cabi/publish.sh` (or
# `npm publish cabi/pkg`) is the separate step.
#
#   cabi/build.sh            build cabi/pkg
#   cabi/build.sh --check    also fail unless cabi/Cargo.toml's version equals djrust's
#
# The npm version is djrust's (root Cargo.toml), so `djwasm@X` always names one engine build.
# No wasm-opt: the bytes stay a function of the sources and the toolchain (rust-toolchain.toml),
# and panic locations are remapped so no build-machine path ends up in them.
set -euo pipefail
cd "$(dirname "$0")"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' ../Cargo.toml | head -1)
mine=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
[ "$version" = "$mine" ] || { echo "cabi/Cargo.toml is $mine, djrust is $version: bump cabi/Cargo.toml" >&2; exit 1; }
repo=$(cd .. && pwd -P); sysroot=$(rustc --print sysroot)
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$repo=djrust --remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=cargo --remap-path-prefix=$sysroot=rust"
cargo build --release --locked --target wasm32-unknown-unknown >&2
out=pkg; rm -rf "$out"; mkdir -p "$out"
cp target/wasm32-unknown-unknown/release/djwasm.wasm "$out/djwasm.wasm"
cp js/loader.js js/worklet.js js/node.js "$out/"
sed "s/\"version\": \"[^\"]*\"/\"version\": \"$version\"/" package.json > "$out/package.json"
cp ../LICENSE "$out/LICENSE"; cp README.md "$out/README.md"
# What a consumer pins: sha256 and the SRI (sha384) of every file, as the lock in beats records.
python3 - "$out" "$version" <<'PY'
import base64, hashlib, json, pathlib, sys
out, version = pathlib.Path(sys.argv[1]), sys.argv[2]
files = {}
for name in ("djwasm.wasm", "loader.js", "worklet.js", "node.js"):
    b = (out / name).read_bytes()
    files[name] = {"bytes": len(b), "sha256": hashlib.sha256(b).hexdigest(),
                   "integrity": "sha384-" + base64.b64encode(hashlib.sha384(b).digest()).decode()}
(out / "hashes.json").write_text(json.dumps({"package": "djwasm", "version": version, "files": files}, indent=2) + "\n")
PY
echo "built $out (djwasm@$version)" >&2
cat "$out/hashes.json"
