# djrust (npm)

The bitwrap audio engine as WebAssembly, built from the [djrust](https://github.com/stackdump/djrust)
crate's `wasm` feature. It is the engine that [beats.bitwrap.io](https://beats.bitwrap.io) loads.

```js
// from a CDN, pinned (the inline build carries its own wasm: no second request)
import { Dj } from 'https://cdn.jsdelivr.net/npm/djrust@0.2.0/djrust.inline.js';

const dj = new Dj(sampleRate);           // inside an AudioWorkletProcessor
dj.compose('A', 'house', 7, 'standard'); // genre, seed, structure ('' or 'loop': none)
dj.set_playing('A', true);
dj.render(left, right);                  // each process() block
dj.tick();                               // about every 10 ms
```

`djrust/split` is the same module with the wasm as a separate `djrust_bg.wasm` (call the default
`init()` export first). The same genre, seed and structure renders the same samples in every
browser and on every platform djrust builds for; a new engine version may change them, so pin it.

MIT licence.
