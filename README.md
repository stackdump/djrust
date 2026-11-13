# djrust

**djrust is the bitwrap audio engine.** It composes, plays and mixes the deterministic generative
tracks behind [beats.bitwrap.io](https://beats.bitwrap.io): give it a genre, a seed and a structure,
and it writes the track (a Petri-net project), runs it on its wave engine, and renders the audio.
The same input renders the same samples, bit for bit, in a browser, on Android, on iOS and on the
desktop. One crate holds the engine and a thin adapter for each of those platforms, behind cargo
features, so each app pulls in only the adapter it needs.

## What it does

- **Generator.** `genre:seed[:structure]` track specs, the beats composer (`compose(genre,
  overrides)`), arrangement, musicality passes and their measurements. This is the same generator
  as beats.bitwrap.io's JavaScript, checked against it by golden fixtures.
- **Share codec and CIDs.** Share envelopes and links, the share gzip framing, base64url, and
  CIDv1 (dag-json, sha2-256, base58btc) over RFC 8785 JCS bytes. A track's share CID is the same
  one beats.bitwrap.io names it by.
- **Wave engine.** The Petri-net runner and its DJ extensions, the voice catalogue and sampler
  rules, sound palettes and stingers, the master effects chain, macros, the FX unit, the vinyl
  brake and offline loudness.
- **Decks** (`djrust::host`). Two net decks and sample decks, a mixer with crossfader and cue, a
  real-time render thread fed through lock-free rings, loops and clips, Wander and auto-mix, the
  vocoder, input strips, a session journal and whole-session snapshots (`dj-snapshot.v1`, see
  `schema/`).
- **Controllers and gear.** MIDI controller decode and routing, surface and jog profiles, and MIDI
  gear models. The live MIDI port backends are not part of this crate.

The engine at the crate root has no I/O, no clock and no threads. With `--no-default-features`
it builds as `no_std` + `alloc`, and it uses `libm`, so its floating point is the same on every
target.

## Features

| feature   | adds | for |
|-----------|------|-----|
| `std`     | std error impls (default) | everything but no_std builds |
| `host`    | the live decks, `djrust::host` (default) | every app |
| `wasm`    | `djrust::wasm`: the wasm-bindgen facade (`Dj` for an AudioWorklet, `DjComposer` for a Worker) | browsers, the npm package |
| `network` | the code that publishes or fetches snapshots by CID | apps that share online |
| `transfer` | a prepared track as bytes (`djrust::transfer`, `djrust::host::deck::transfer`): one engine instance composes, another installs it in steps; the engine's own composes can be offloaded (`deck::offload`) | browsers (a Worker composes, the AudioWorklet installs) |
| `audio`   | `djrust::host::audio`: output and input streams on cpal (CoreAudio, ALSA, WASAPI, AAudio) | native and Android |
| `sf2`     | SoundFont 2 voices (rustysynth) | native and Android |
| `decode`  | audio files onto a sample deck (symphonia: WAV, AIFF, FLAC, MP3, AAC/M4A, ALAC, Ogg Vorbis) | desktop |
| `native`  | `audio` + `sf2` + `decode` | desktop apps |
| `android` | `audio` plus `djrust::android::init`, the JNI handshake cpal's AAudio backend needs | Android apps |
| `ffi`     | `djrust::ffi`: a C ABI (engine, compose, play, crossfade, render, tick) | Swift / iOS / iPadOS, any C caller |

## Building

The toolchain is stable Rust (1.82 or later), and `rust-toolchain.toml` adds the wasm32 target.

```sh
cargo test                                   # engine + decks, ~1600 tests incl. JS and Swift goldens
cargo build --no-default-features            # the pure engine, no_std + alloc
cargo build --no-default-features --target wasm32-unknown-unknown
```

**Browser / npm.** `npm/` is the cdylib for the `djrust` npm package. Build it with
`npm/build.sh`, which writes the package to `npm/pkg` and publishes nothing. The script uses
wasm-pack if it is installed, otherwise cargo and the wasm-bindgen CLI at the version pinned in
`npm/Cargo.lock`. It runs wasm-opt when binaryen is installed. The package ships a split ES
module (`djrust/split`: glue plus `djrust_bg.wasm`) and an inline one (the default: the wasm is
embedded, so an AudioWorklet or a CDN `<script type=module>` needs no second request):

```js
import { Dj } from 'https://cdn.jsdelivr.net/npm/djrust@0.2.0/djrust.inline.js';
```

To link the facade into your own wasm crate, depend on `djrust` with
`default-features = false, features = ["wasm"]` and `pub use djrust::wasm::*;` from a `cdylib`.

**Android.** Depend on `djrust` with `features = ["android", "sf2"]` from your JNI or UniFFI
library and build it with [cargo-ndk](https://github.com/bbqsrc/cargo-ndk). For example, run
`cargo ndk -t arm64-v8a -t x86_64 build --release` from that crate. Call
`djrust::android::init(env, context)` once from your own `Java_…_init` export before any audio
starts.

**Swift / iOS / iPadOS.** Build a static library with the C ABI. The function list and a C header
sketch are in `src/ffi.rs`:

```sh
rustup target add aarch64-apple-ios aarch64-apple-ios-sim
cargo rustc --release --features ffi --target aarch64-apple-ios --crate-type staticlib
```

Link the `.a` into your Xcode target and declare the functions in a bridging header. Add
`audio` to the features to have djrust open the output itself, or render with `djrust_render`
from your own AVAudioEngine source node.

**Native desktop.** Depend on `djrust` with `features = ["native"]`, and use
`djrust::host::deck::host::host_engine` with `djrust::host::audio`. Linux needs the ALSA headers
(`libasound2-dev`) for `audio`.

### Build-time settings

| env var | default | what |
|---------|---------|------|
| `DJRUST_FORMAT_NS` | `djrust` | the namespace of on-disk format IDs (`<ns>/session/v1`, `<ns>/loops/v1`, …) and of the app-support folder; an app with existing files sets its own in `.cargo/config.toml` `[env]` |
| `DJRUST_PD705_HOST` | `pi@raspberrypi.local` | the default ssh target for installing a PD705 kit |
| `DJRUST_PUBLISH_BASE` | `https://cdn.bitwrap.io/ipfs/` | where a published session is served (`<base><cid>/`); `SessionBundle::public_url_at` overrides it at run time |

## Determinism and versions

Rendering is deterministic for a given djrust version. A new version may change the samples,
for example when a voice or a musicality pass improves. Pin the version, and key anything you
cache (rendered audio, golden hashes) by share CID plus engine version.

## Data

Two of the tables in `data/` are derived from public datasets under CC BY 4.0. Each file names its
source:

- `grooves.json`: drum timing from the Groove MIDI Dataset (Gillick et al., Magenta,
  https://magenta.tensorflow.org/datasets/groove)
- `melody-bigrams.json`: melodic interval statistics from the RS200 corpus (Trevor de Clercq and
  David Temperley, "A Corpus Study of Rock Music", Popular Music 30/1, 2011)

The golden fixtures under `tests/fixtures` come from beats.bitwrap.io's JavaScript (`js/`) and
from the original Swift app (`swift/`). Their format IDs are written in djrust's namespace.

## The wallet

`crates/djrust-wallet` is a second package in this workspace: the bitwrap identity wallet core.
It handles did:pkh identities, and it signs shares, snapshots, invites and charted-by claims as
W3C Verifiable Credentials. It does no I/O and keeps no keys. The engine does not depend on it.
See its [README](crates/djrust-wallet/README.md) and the
[standards mapping](crates/djrust-wallet/docs/STANDARDS.md).

## Licence

MIT, see [LICENSE](LICENSE). Dependencies are MIT, Apache-2.0, Zlib, BSD or Unicode licensed (the wallet's `ryu-js` is
Apache-2.0 or BSL-1.0).
The `decode` feature adds symphonia, which is MPL-2.0: a file-level copyleft that applies to
symphonia's own source files only and is compatible with shipping djrust under MIT.
