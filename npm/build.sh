#!/usr/bin/env bash
# Build the `djrust` npm package into npm/pkg (nothing is published; `npm publish npm/pkg` is a
# separate, manual step). Version comes from the root Cargo.toml, so an npm version always names
# one engine build. Uses wasm-pack when installed, else cargo + the wasm-bindgen CLI (whose
# version must match npm/Cargo.lock). Runs wasm-opt -O3 when binaryen is installed (or WASM_OPT
# is set) and says so when it is not.
#   usage: npm/build.sh [--network]
set -euo pipefail
cd "$(dirname "$0")"
features=(); [ "${1:-}" = "--network" ] && features=(--features network)
version=$(sed -n 's/^version = "\(.*\)"/\1/p' ../Cargo.toml | head -1)
out=pkg; rm -rf "$out"; mkdir -p "$out"
# No build-machine paths in the binary's panic locations: remap the checkout, the cargo
# registry and the Rust sources to fixed names.
repo=$(cd .. && pwd -P); sysroot=$(rustc --print sysroot)
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$repo=djrust --remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=cargo --remap-path-prefix=$sysroot=rust"
if command -v wasm-pack >/dev/null; then
  wasm-pack build . --release --target web --out-dir "$out" --out-name djrust --no-pack -- "${features[@]}"
else
  cargo build --release --locked --target wasm32-unknown-unknown "${features[@]}"
  wasm-bindgen --target web --out-dir "$out" --out-name djrust \
    target/wasm32-unknown-unknown/release/djrust_npm.wasm
fi
wasm_opt=${WASM_OPT:-$(command -v wasm-opt || true)}
if [ -n "$wasm_opt" ]; then
  "$wasm_opt" -O3 --enable-bulk-memory --enable-nontrapping-float-to-int \
    "$out/djrust_bg.wasm" -o "$out/djrust_bg.wasm"
else
  echo "warning: wasm-opt not found (install binaryen or set WASM_OPT); the .wasm is unoptimised" >&2
fi
# The inline module: the same glue with the wasm embedded, so a CDN <script type=module> or an
# AudioWorklet (which cannot fetch) loads it with no second request.
python3 - "$out" <<'PY'
import base64, pathlib, sys
out = pathlib.Path(sys.argv[1])
b64 = base64.b64encode((out / "djrust_bg.wasm").read_bytes()).decode()
(out / "djrust.inline.js").write_text(
    "// djrust with its WebAssembly inlined: initialised on import, no fetch.\n"
    "import { initSync } from './djrust.js';\n"
    "export * from './djrust.js';\n"
    f"const b64 = '{b64}';\n"
    "const bytes = Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));\n"
    "initSync({ module: bytes });\n")
(out / "djrust.inline.d.ts").write_text("export * from './djrust.js';\n")
PY
sed "s/\"version\": \"0.0.0\"/\"version\": \"$version\"/" package.json > "$out/package.json"
cp ../LICENSE "$out/LICENSE"; cp README.md "$out/README.md"
ls -l "$out"
