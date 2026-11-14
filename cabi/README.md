# djwasm

[djrust](https://github.com/stackdump/djrust), the bitwrap audio engine, as a **plain C-ABI
WebAssembly module** plus the JS glue that drives it. It is the engine
[beats.bitwrap.io](https://beats.bitwrap.io) loads into an AudioWorklet for `?engine=djrust`.

Why a C ABI and not wasm-bindgen (the [`djrust`](https://www.npmjs.com/package/djrust) package):
an `AudioWorkletGlobalScope` has no `TextEncoder` / `TextDecoder`, and the consuming site has no
bundler. `loader.js` is the whole glue and converts UTF-8 by hand where the built-ins are missing.

| file | what |
|---|---|
| `djwasm.wasm` | the module (`cabi/` in djrust, over the djrust crate with `default-features = false`) |
| `loader.js` | `instantiate(bytes)` / `instantiateSync(module)`: compose, model CID, load, play, render |
| `worklet.js` | the `djrust-engine` AudioWorkletProcessor (imports `./loader.js`) |
| `node.js` | `createDjrustEngine(ctx, connectTo, {wasmUrl})`: the page-side handle |
| `hashes.json` | sha256 and SRI (sha384) of each file, as built |

```js
// pinned: never a range. Check the bytes against hashes.json / your own lock.
const url = 'https://cdn.jsdelivr.net/npm/djwasm@0.2.0/djwasm.wasm';
const bytes = await (await fetch(url, { integrity: 'sha384-…' })).arrayBuffer();
```

The version is djrust's: `djwasm@X` is djrust X. The same genre, seed and structure renders the
same samples in every browser; a new engine version may change them, so pin it.

Build: `cabi/build.sh` (writes `cabi/pkg`, publishes nothing). Publish: `cabi/publish.sh`.
Tests: `cd cabi && cargo test`.

MIT licence.
