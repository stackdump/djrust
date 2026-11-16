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

Since 0.3.0 the module adds exports for net macros, control-net macro dispatch, seek, groove, a
frame clock and an event ring (`djw_features()` reports which; ABI_VERSION stays 1). `loader.js`
and `worklet.js` feature-detect every 0.3+ export, so the same glue still drives a 0.2.0 module.
The worklet's 0.3 messages (`fire-macro`, `panic`, `control-macros`, `seek`, `groove`, frame-
stamped `at` / `q`) and its test hooks (`script`, `tap`, `tapHash`, `assertQuantum`) are documented
in its header. With none of them used, 0.3.0 renders the 0.2.0 bytes
(`cabi/tests/golden.rs` against `cabi/tests/fixtures/golden-0.2.0.json`).

After 0.3.0: a live instrument change, `djw_set_instrument(net, name)` (feature `INSTRUMENT`,
2048; `dj.setInstrument(netId, name)`; the worklet's `{type:'instrument', netId, instrument}`). The
net's lanes are rebuilt as a load of the project with that instrument would build them and
swapped in while the song plays on (the tick, the marking and every other net untouched); the
notes the net was sounding stop. Setting a net to the instrument it already plays does nothing
(`cabi/tests/instrument.rs` runs that over the golden corpus).

The version is djrust's: `djwasm@X` is djrust X. The same genre, seed and structure renders the
same samples in every browser; a new engine version may change them, so pin it.

Build: `cabi/build.sh` (writes `cabi/pkg`, publishes nothing). Publish: `cabi/publish.sh`.
Tests: `cd cabi && cargo test`.

MIT licence.
