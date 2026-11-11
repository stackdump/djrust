# JS-oracle fixtures

Copied unchanged from beats-btw-dj `Tests/DJCoreTests/Fixtures/beats` (commit `c4946e4`), except
`captured-messages.json` and `mc-recent-events.json`, which come from `Tests/DJCoreTests/Fixtures`
itself (byte-identical copies). They are written by beats' own JavaScript (and, for the Chrome one, a
real Chrome rendering beats' Tone nodes; `captured-messages.json` is a capture from real controller
hardware and `mc-recent-events.json` a real Minecraft event window), not by the Swift port, so they
need no Mac to regenerate and are a stronger oracle than `../swift`. Regenerate
in beats-btw-dj with `make fixtures-beats` / `make fixtures-chrome` (scripts run against
beats-bitwrap-io `668978e`).

| File | Writer | Used by |
|---|---|---|
| `arrange.json` | `beats-arrange.mjs` | `tests/arrange.rs` |
| `share-urls.json` | `beats-share-url.mjs` | `tests/share_raw.rs` |
| `pflow.json` | `beats-parity.mjs` | `tests/share_raw.rs` |
| `macro-fx.json` | `beats-macro-fx.mjs`: beats' macro code + Tone 15.1.22's Param timeline (exact) | `tests/fx_macros.rs` |
| `master-chain-chrome.json` | `master-chain-chrome.mjs`: Chrome (headless) rendering Tone's nodes, 22.05 kHz (tolerances) | `tests/fx_chain.rs` |
| `captured-messages.json` | `djctl monitor` captures from the Inpulse, APC and MPK | `tests/control_decode.rs` |
| `wave.json` | `beats-wave.mjs`: beats' net.js / runner.js executor traces (exact), incidence matrices, invariants, voices, biquads, wavetables, 12 float32 renders | `tests/wave.rs` |
| `mc-recent-events.json` | a real `mc_recent_events` window from a Minecraft server | `tests/deck_minecraft.rs` |
| `projects.json` | `beats-parity.mjs`: 304 composed projects (19 genres x 4 seeds x loop/standard x cohesion v1/v2) with canonical bytes, model CID, document hash and name | `tests/beats_composer.rs`, `tests/beats_foundation.rs` |
| `compose.json` | `beats-parity.mjs`: `compose()` with every trait override beats reads | `tests/beats_composer.rs` |
| `modules.json` | `beats-parity.mjs`: bjorklund patterns, song templates, cohesion-v2 themes, rendered motifs | `tests/beats_composer.rs` |
| `foundation.json` | `beats-parity.mjs`: JS number printing, ToInt32, JSON serializations, mulberry32 streams, base58, CIDs | `tests/beats_foundation.rs` |
| `envelopes.json` | `beats-parity.mjs`: share payloads, their canonical text, CID and `shareFromPayload` | `tests/beats_foundation.rs` |
| `go.json` | `beats-go-oracle.mjs`: what beats' Go generator produced for the same cases (a second oracle: it agrees with JS on the kick, stingers, many v2 nets, some whole projects) | `tests/beats_go_oracle.rs` |

`projects`, `compose`, `modules`, `foundation` and `envelopes` (with `share-urls`) are also the
inputs that `tools/paritydump/regen.sh` feeds Swift (it reads them from the Swift repo at the
commit it dumps), so `fixtures/swift/*.json` and these answer the same questions from two
sides: Swift's output, and beats' own. `go.json` is a different generator's output, not an
input of anything.
