//! djwasm: the djrust engine in the browser.
//!
//! The whole engine (control side and render side) lives in ONE wasm instance, inside an
//! AudioWorklet. No threads, no SharedArrayBuffer, so no COOP/COEP is needed. The main thread
//! talks to it by `postMessage` (see web/dj-worklet.js). A second instance in a Worker
//! ([`DjComposer`], web/dj-composer.js) composes, measures and compiles each track and hands it
//! over as bytes, which the worklet installs a few frames per render quantum ([`install`]).
//! The engine's own composes (the next version, auto-next and Wander swaps, relocations: a seek,
//! a section, a palette; auto-mix; a deck copy) go the same way once the Worker is up
//! ([`Dj::set_offload`], `djrust::host::deck::offload`): the engine asks, the Worker answers
//! ([`DjComposer::answer_offload`]) over a port of their own, and the worklet takes the answer in
//! steps as it does a compose's. Without the Worker, composes run inline (`deterministic`) and
//! block the worklet for their length.
//!
//! The surface is deliberately small and JSON-shaped: strings in, strings out, so the same
//! messages can later be served by a Worker or a remote engine.

use crate::session::action::DeckAction;
use crate::TrackSpec;
use crate::host::deck::engine::{Engine, EngineConfig, Renderer};
use crate::host::deck::host::host_engine;
use wasm_bindgen::prelude::*;

pub mod beats;
pub mod install;
pub mod snapshot;

const TICKS_PER_BAR: i64 = 16;

#[wasm_bindgen]
pub struct Dj {
    engine: Engine,
    renderer: Renderer,
    sample_rate: f64,
    installer: install::Installer,
    /// The engine's own composes go to the Worker ([`set_offload`](Dj::set_offload)).
    offload: bool,
    /// Engines a restore swapped out, freed [`RETIRE_PIECES`] pieces per quantum
    /// ([`install_step`](Dj::install_step)): freed whole, one with two decks playing is
    /// ~20 ms (native) of compiled projects, an underrun.
    retiring: Vec<crate::host::deck::teardown::Teardown>,
    /// How much the instance's memory grew to boot the first engine: what a restore warms
    /// before it boots another (0 off wasm).
    engine_bytes: usize,
    /// The beats studio's readers of each deck's fires, ticks and marking ([`events`](Dj::events)).
    events: [beats::Events; 2],
}

fn deck_ix(deck: &str) -> usize {
    if deck == "B" { 1 } else { 0 }
}

/// Pieces of a swapped-out engine freed per render quantum: a piece is at most ~0.6 ms
/// (native; the engine's remainder, last, ~1.2 ms), so 4 stays under a quantum's 2.7 ms and frees
/// an engine of two playing decks (~600 pieces) in ~150 quanta (0.4 s).
const RETIRE_PIECES: usize = 4;

/// An offloaded compose's id, ahead of its bytes (u64 LE): the request and its answer carry it.
fn with_id(id: u64, bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + bytes.len());
    out.extend_from_slice(&id.to_le_bytes());
    out.extend_from_slice(bytes);
    out
}

fn split_id(bytes: &[u8]) -> Result<(u64, &[u8]), JsError> {
    if bytes.len() < 8 {
        return Err(JsError::new("not an offload message"));
    }
    Ok((u64::from_le_bytes(bytes[..8].try_into().unwrap()), &bytes[8..]))
}

/// The engine as the browser boots it. A restore builds another one of these.
fn boot(sample_rate: f64) -> (Engine, Renderer) {
    let mut cfg = EngineConfig::new(sample_rate);
    cfg.history_beats = None; // no history loop / freeze buffers in the browser
    // live = false: composes finish inline (no worker threads on wasm32)
    let (mut e, renderer) = host_engine(cfg, false, true);
    e.set_crossfader(0.0);
    (e, renderer)
}

/// `genre:seed[:structure]` as the shell asks for it ("" or "loop": no structure).
fn spec_of(genre: &str, seed: f64, structure: &str) -> Result<TrackSpec, JsError> {
    let text = if structure.is_empty() || structure == "loop" {
        format!("{genre}:{}", seed as i64)
    } else {
        format!("{genre}:{}:{structure}", seed as i64)
    };
    TrackSpec::parse(&text).map_err(|e| JsError::new(&format!("{e:?}")))
}

fn js_err(e: impl core::fmt::Display) -> JsError {
    JsError::new(&e.to_string())
}

fn deck_name(d: &str) -> Result<&'static str, JsError> {
    match d {
        "A" | "a" => Ok("A"),
        "B" | "b" => Ok("B"),
        _ => Err(JsError::new(&format!("no deck {d} (A or B)"))),
    }
}

fn q(s: &str) -> String {
    // minimal JSON string escaper
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn num(x: f64) -> String {
    if x.is_finite() {
        format!("{x}")
    } else {
        "0".into()
    }
}

#[wasm_bindgen]
impl Dj {
    #[wasm_bindgen(constructor)]
    pub fn new(sample_rate: f64) -> Dj {
        std::panic::set_hook(Box::new(|_| {}));
        let before = install::memory_bytes();
        let (engine, renderer) = boot(sample_rate);
        let engine_bytes = install::memory_bytes() - before;
        Dj { engine, renderer, sample_rate, installer: install::Installer::default(), offload: false, retiring: Vec::new(), engine_bytes, events: Default::default() }
    }

    /// The engine's own composes go to the composer Worker (on) or run inline as before (off,
    /// as when there is no Worker): the worklet hands out [`take_offload`](Self::take_offload)'s
    /// requests and brings back [`offload_answer`](Self::offload_answer)s.
    pub fn set_offload(&mut self, on: bool) {
        self.offload = on;
        self.engine.set_offload(on);
    }

    /// The next compose the engine asks of the Worker: its id (u64 LE) then the request, for
    /// `DjComposer::answer_offload`. None when there is none.
    pub fn take_offload(&mut self) -> Option<Vec<u8>> {
        let (id, bytes) = self.engine.take_offload()?;
        Some(with_id(id, &bytes))
    }

    /// The Worker's answer (`DjComposer::answer_offload`'s bytes): taken in by
    /// [`install_step`](Self::install_step) from here. An error: that compose failed. An answer
    /// no compose here waits for (asked by the engine a restore replaced) is let go.
    pub fn offload_answer(&mut self, bytes: &[u8]) -> Result<(), JsError> {
        let (id, answer) = split_id(bytes)?;
        if !self.engine.offload_waiting(id) {
            return Ok(());
        }
        self.engine.offload_answer(id, Ok(answer.to_vec())).map_err(|e| JsError::new(&e))
    }

    /// The Worker could not answer compose `id` (the low 32 bits of its id are enough: ids
    /// count up from 1): it fails, and the deck lands it as a failed compose.
    pub fn offload_failed(&mut self, id: f64, why: &str) {
        let _ = self.engine.offload_answer(id as u64, Err(why.to_string()));
    }

    /// Composes offloaded and not in yet.
    pub fn offload_pending(&self) -> u32 {
        self.engine.offload_pending() as u32
    }

    /// Render `frames` frames of the master mix into the two output channels.
    pub fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        let n = left.len().min(right.len());
        self.renderer.render_block(&mut left[..n], &mut right[..n]);
    }

    /// Housekeeping, about every 10 ms: the beat clock, landed composes, song ends.
    pub fn tick(&mut self) {
        self.engine.service();
        self.engine.service_loops();
    }

    /// Compose genre:seed[:structure] and load it on `deck` (inline: composes AND measures on
    /// this thread). Returns the load's answer or an error.
    pub fn compose(&mut self, deck: &str, genre: &str, seed: f64, structure: &str) -> Result<String, JsError> {
        let deck = deck_name(deck)?;
        let spec = spec_of(genre, seed, structure)?;
        Ok(self.load(deck, spec, None))
    }

    /// Start installing a track a `DjComposer::prepare` made for `deck` (any track pending for
    /// the deck is dropped). [`install_step`](Self::install_step) takes it in; nothing plays
    /// until it is all in. Errors starting `unfit:` mean it was prepared for a deck that
    /// compiles otherwise (a role switched off since); compose inline instead.
    pub fn begin_install(&mut self, deck: &str, bytes: Vec<u8>) -> Result<(), JsError> {
        let deck = deck_name(deck)?;
        self.installer.begin(&self.engine, deck, bytes).map_err(|e| JsError::new(&e))
    }

    /// Drop what `deck` has installing (a compose for it is about to run inline).
    pub fn cancel_install(&mut self, deck: &str) {
        self.installer.cancel(deck);
    }

    pub fn installing(&self) -> bool {
        self.installer.busy() || self.engine.offload_busy() || !self.retiring.is_empty()
    }

    /// Swap in a restored engine between two quanta (same thread as `render()`), the old one
    /// left to [`retiring`](Self::retiring).
    fn swap_in(&mut self, engine: Engine, renderer: Renderer) {
        let old = (std::mem::replace(&mut self.engine, engine), std::mem::replace(&mut self.renderer, renderer));
        self.retiring.push(crate::host::deck::teardown::Teardown::new(old.0, old.1));
        // (stood deterministic: the restored engine offloads from here as this one did)
        self.engine.set_offload(self.offload);
    }

    /// About `budget` bytes of the oldest install (a restore first): call once per render
    /// quantum while [`installing`](Self::installing). Returns JSON when one finished:
    /// `{"deck","answer","kb","steps"}` (loaded), `{"deck","error"}` (nothing loaded: compose the
    /// track inline), `{"restore":{"cid","refused"}}` (the restored session is in) or
    /// `{"offload":{"id","kb","steps","error"}}` (an offloaded compose is in: the deck lands it
    /// on its next tick; `error` null unless it failed). A host's own install goes first.
    pub fn install_step(&mut self, budget: usize) -> Option<String> {
        // a swapped-out engine is freed on quanta with nothing else to install
        if !self.installer.busy() && !self.engine.offload_busy() {
            if let Some(t) = self.retiring.first_mut() {
                if t.step(RETIRE_PIECES) {
                    self.retiring.remove(0);
                }
                return None;
            }
        }
        if !self.installer.busy() {
            let l = self.engine.offload_step(budget)?;
            let error = l.error.as_deref().map(q).unwrap_or_else(|| "null".into());
            return Some(format!(
                "{{\"offload\":{{\"id\":{},\"kb\":{},\"steps\":{},\"error\":{error}}}}}",
                l.id,
                l.bytes / 1024,
                l.steps
            ));
        }
        Some(match self.installer.step(&mut self.engine, budget)? {
            install::Landed::Track { deck, answer, bytes, steps } => {
                self.events[deck_ix(&deck)].reset(&self.engine, &deck);
                format!("{{\"deck\":{},\"answer\":{},\"kb\":{},\"steps\":{steps}}}", q(&deck), q(&answer), bytes / 1024)
            }
            install::Landed::Failed { deck, error } => format!("{{\"deck\":{},\"error\":{}}}", q(&deck), q(&error)),
            install::Landed::Restored { engine, renderer, cid, refused } => {
                self.swap_in(engine, renderer);
                let r: Vec<String> = refused.iter().map(|x| q(x)).collect();
                format!("{{\"restore\":{{\"cid\":{},\"refused\":[{}]}}}}", q(&cid), r.join(","))
            }
        })
    }

    fn load(&mut self, deck: &str, spec: TrackSpec, track: Option<crate::host::deck::track::PreparedTrack>) -> String {
        let a = self.engine.perform(
            &DeckAction::Load {
                deck: deck.into(),
                source: crate::session::model::DeckSource::Net(spec),
            },
            crate::host::deck::perform::PerformCtx { prepared: track, ..Default::default() },
        );
        format!("{a:?}")
    }

    pub fn set_playing(&mut self, deck: &str, on: bool) -> Result<(), JsError> {
        let deck = deck_name(deck)?;
        self.engine.perform(
            &DeckAction::Play { deck: deck.into(), on },
            crate::host::deck::perform::PerformCtx::default(),
        );
        Ok(())
    }

    /// pitch (-1..1), volume (0..1), ... any `DeckAction::Param` name.
    pub fn set_param(&mut self, deck: &str, name: &str, value: f64) -> Result<(), JsError> {
        let deck = deck_name(deck)?;
        self.engine.perform(
            &DeckAction::Param { deck: deck.into(), name: name.into(), value },
            crate::host::deck::perform::PerformCtx::default(),
        );
        Ok(())
    }

    pub fn set_crossfader(&mut self, v: f64) {
        self.engine.set_crossfader(v.clamp(0.0, 1.0));
    }

    pub fn set_role(&mut self, deck: &str, role: &str, on: bool) -> Result<(), JsError> {
        let deck = deck_name(deck)?;
        self.engine.perform(
            &DeckAction::Role { deck: deck.into(), role: role.into(), on },
            crate::host::deck::perform::PerformCtx::default(),
        );
        Ok(())
    }

    pub fn genres(&self) -> String {
        let g: Vec<String> = crate::gen::BeatsTables::shared()
            .genres
            .keys()
            .map(|k| q(k))
            .collect();
        format!("[{}]", g.join(","))
    }

    /// One deck, as the UIs draw it (a subset of djandroid's DeckState).
    pub fn state(&self, deck: &str) -> Result<String, JsError> {
        let deck = deck_name(deck)?;
        let e = &self.engine;
        let d = e.deck(deck).ok_or_else(|| JsError::new("closed"))?;
        let spec = d.spec.as_ref();
        let tick = d.mirror.tick();
        let phase = d.mirror.tick_phase().clamp(0.0, 1.0);
        let sections: Vec<String> = d
            .sections
            .iter()
            .map(|s| {
                format!(
                    "{{\"name\":{},\"startTick\":{},\"ticks\":{}}}",
                    q(&s.name),
                    s.start_tick,
                    s.steps
                )
            })
            .collect();
        let total_ticks = d.sections.iter().map(|s| s.start_tick + s.steps).max().unwrap_or(0);
        let roles: Vec<String> = e
            .roles(deck)
            .into_iter()
            .map(|r| {
                format!(
                    "{{\"name\":{},\"audible\":{},\"disabled\":{}}}",
                    q(&r),
                    e.is_role_audible(deck, &r),
                    e.is_role_disabled(deck, &r)
                )
            })
            .collect();
        let title = spec
            .map(|s| q(&crate::gen::generate_track_name(&s.genre, s.seed)))
            .unwrap_or_else(|| "null".into());
        let section = e
            .current_section(deck)
            .and_then(|i| d.sections.get(i))
            .map(|s| q(&s.name))
            .unwrap_or_else(|| "null".into());
        Ok(format!(
            "{{\"deck\":{},\"loaded\":{},\"playing\":{},\"genre\":{},\"seed\":{},\"structure\":{},\
\"title\":{},\"tempo\":{},\"baseTempo\":{},\"pitch\":{},\"volume\":{},\"tick\":{},\"position\":{},\
\"totalTicks\":{},\"bar\":{},\"totalBars\":{},\"section\":{},\"sections\":[{}],\"level\":{},\"roles\":[{}]}}",
            q(deck),
            d.project.is_some(),
            d.mirror.is_playing(),
            spec.map(|s| q(&s.genre)).unwrap_or_else(|| "null".into()),
            spec.map(|s| s.seed.to_string()).unwrap_or_else(|| "null".into()),
            spec.and_then(|s| s.structure.as_ref()).map(|s| q(s)).unwrap_or_else(|| "null".into()),
            title,
            num(d.mirror.tempo()),
            num(d.base_tempo),
            num(d.pitch),
            num(d.volume),
            tick,
            num(tick as f64 + if d.mirror.is_playing() { phase } else { 0.0 }),
            total_ticks,
            tick.max(0) / TICKS_PER_BAR + 1,
            total_ticks / TICKS_PER_BAR,
            section,
            sections.join(","),
            num(d.mirror.level() as f64),
            roles.join(","),
        ))
    }

    // MARK: the beats studio (`beats`)

    /// Load a beats share envelope or a beats project (the studio worker's `projectToJSON`) on
    /// `deck`, inline. It plays as a raw-`nets` share: exactly that project, no dj musicality
    /// pass. Returns the load's answer. The deck's event reader starts over.
    pub fn load_share(&mut self, deck: &str, text: &str) -> Result<String, JsError> {
        let deck = deck_name(deck)?;
        let source = beats::source_of(text).map_err(|e| JsError::new(&e))?;
        self.installer.cancel(deck);
        let a = beats::load(&mut self.engine, deck, source);
        self.events[deck_ix(deck)].reset(&self.engine, deck);
        Ok(a)
    }

    /// Mute or unmute one net of the project on `deck` (the studio's mixer, `initialMutes`).
    pub fn set_net_muted(&mut self, deck: &str, net_id: &str, muted: bool) -> Result<bool, JsError> {
        let deck = deck_name(deck)?;
        Ok(self.engine.set_mute(deck, net_id, muted))
    }

    /// The net clock's tempo on `deck`, in BPM (the studio's tempo control).
    pub fn set_tempo(&mut self, deck: &str, bpm: f64) -> Result<bool, JsError> {
        let deck = deck_name(deck)?;
        Ok(self.engine.set_tempo(deck, bpm))
    }

    /// Move `deck`'s net to `tick` (0: the top; a stop, a seek). The event reader starts over.
    pub fn relocate(&mut self, deck: &str, tick: f64) -> Result<bool, JsError> {
        let deck = deck_name(deck)?;
        let ok = self.engine.relocate(deck, tick.max(0.0) as i64, None, None);
        self.events[deck_ix(deck)].reset(&self.engine, deck);
        Ok(ok)
    }

    /// What `deck` played since the last call ([`beats::Events::read`]): fires with their net and
    /// transition ids, the newest tick, tempo and frame, the muted nets when they changed, and,
    /// with `marking`, the token count of every place when it changed.
    pub fn events(&mut self, deck: &str, marking: bool) -> Result<String, JsError> {
        let deck = deck_name(deck)?;
        let i = deck_ix(deck);
        Ok(self.events[i].read(&self.engine, deck, marking))
    }

    /// Any deck action as JSON (`{"op":"masterFx","key":"hp-freq","value":40}`,
    /// `{"op":"stinger","n":1}`, `{"op":"fxReset"}`, …) on `deck` ([`beats::perform`]).
    /// `"done"`, or why it did not run.
    pub fn perform(&mut self, deck: &str, action: &str) -> Result<String, JsError> {
        let deck = deck_name(deck)?;
        beats::perform(&mut self.engine, deck, action).map_err(|e| JsError::new(&e))
    }

    /// Fire beats macro `id` on `deck` for `bars` (0: its own length) ([`beats::fire_macro`]).
    pub fn fire_macro(&mut self, deck: &str, id: &str, bars: f64) -> Result<String, JsError> {
        let deck = deck_name(deck)?;
        beats::fire_macro(&mut self.engine, deck, id, bars as i32).map_err(|e| JsError::new(&e))
    }

    /// Engage catalogue effect `id` on `deck`'s next bar: `mode` is `momentary`, `latch` or a
    /// number of bars ([`beats::engage`]).
    pub fn engage(&mut self, deck: &str, id: &str, mode: &str) -> Result<(), JsError> {
        let deck = deck_name(deck)?;
        beats::engage(&mut self.engine, deck, id, mode).map_err(|e| JsError::new(&e))
    }

    /// Let go of an engaged effect on `deck` (a pad lifted, a latch undone).
    pub fn release(&mut self, deck: &str, id: &str) -> Result<(), JsError> {
        let deck = deck_name(deck)?;
        self.engine.release(deck, id).map_err(|e| JsError::new(&e))
    }

    /// What is engaged on `deck` now, as JSON ([`beats::fx_state_json`]).
    pub fn fx_state(&self, deck: &str) -> Result<String, JsError> {
        let deck = deck_name(deck)?;
        Ok(beats::fx_state_json(&self.engine, deck))
    }

    /// A mixer strip's voice tweak on `deck`'s MIDI `channel` ([`beats::set_voice`]).
    pub fn set_voice(&mut self, deck: &str, channel: f64, tweak: &str) -> Result<bool, JsError> {
        let deck = deck_name(deck)?;
        beats::set_voice(&mut self.engine, deck, channel as i64, tweak).map_err(|e| JsError::new(&e))
    }

    // MARK: whole-session snapshots (schema/dj-snapshot.v1.schema.json, docs/SNAPSHOTS.md)

    /// The session now, as canonical `DjSnapshot` JSON (the bytes its CID hashes).
    pub fn snapshot(&self, title: &str) -> String {
        snapshot::canonical(&snapshot::capture(&self.engine, Some(title)))
    }

    /// The CID of a snapshot's text (`z…`, the beats share CID). Errors if it is not JSON.
    pub fn snapshot_cid(&self, text: &str) -> Result<String, JsError> {
        let v = crate::json::JsonValue::parse(text).map_err(|e| JsError::new(&format!("{e:?}")))?;
        Ok(snapshot::cid_of(&v))
    }

    /// Put the session back as `text` has it, on a fresh engine swapped in for this one. With a
    /// non-empty `cid` the text must hash to it, and every track share must hash to its own cid,
    /// or nothing changes. Returns `{"cid", "refused": [...]}`; an empty `refused` is exact.
    pub fn restore(&mut self, text: &str, cid: &str) -> Result<String, JsError> {
        let checked = snapshot::check(text, Some(cid)).map_err(js_err)?;
        let (mut engine, renderer) = boot(self.sample_rate);
        let refused = snapshot::stand(&mut engine, &checked);
        self.swap_in(engine, renderer);
        let r: Vec<String> = refused.iter().map(|x| q(x)).collect();
        Ok(format!("{{\"cid\":{},\"refused\":[{}]}}", q(&checked.cid), r.join(",")))
    }

    /// [`restore`](Self::restore) with the decks' tracks prepared by a `DjComposer`
    /// (`prepare_snapshot`): checked now (errors as `restore`'s), then installed by
    /// [`install_step`](Self::install_step), which answers `{"restore": …}` when the session is
    /// in. Anything installing is dropped.
    pub fn begin_restore(&mut self, text: &str, cid: &str, prepared: Vec<u8>) -> Result<(), JsError> {
        let checked = snapshot::check(text, Some(cid)).map_err(js_err)?;
        let sr = self.sample_rate;
        self.installer.begin_restore(checked, Box::new(move || boot(sr)), self.engine_bytes, prepared).map_err(|e| JsError::new(&e))
    }

    /// The `dj-snapshot.v1` schema document, and its CID (an envelope's `schemaCID`).
    pub fn snapshot_schema(&self) -> String {
        snapshot::SCHEMA_JSON.to_string()
    }

    pub fn snapshot_schema_cid(&self) -> Result<String, JsError> {
        self.snapshot_cid(snapshot::SCHEMA_JSON)
    }

    /// Any djrust live op (`vocoder`, `aux`, `post`, `input`, `trick`, ...) with its JSON params,
    /// as djapp's socket takes them. This is how the shell reaches the vocoder instances.
    pub fn live_op(&mut self, method: &str, params: &str) -> Result<(), JsError> {
        let p = if params.trim().is_empty() {
            crate::json::JsonValue::Object(crate::json::JsonObject::new())
        } else {
            crate::json::JsonValue::parse(params).map_err(|e| JsError::new(&format!("{e:?}")))?
        };
        self.engine.apply_live_op(method, &p).map_err(|e| JsError::new(&e))
    }

    /// Every vocoder instance in use (djhost `vocoders_json`).
    pub fn vocoders(&self) -> String {
        self.engine.vocoders_json().canonical_js()
    }

    /// What this build was compiled with: `{"network": bool}`. The shell shows publish / fetch /
    /// sign only when `network` is on; local save and load are always there.
    pub fn features(&self) -> String {
        format!("{{\"network\":{}}}", cfg!(feature = "network"))
    }
}

/// The preparing half of a compose (`install`), in a wasm instance of its own: a Worker runs it
/// so the worklet's instance never composes, measures or compiles. It holds an engine only for its decks' preparers
/// (what a fresh deck compiles with, plus the role filter the shell says the deck has); it never
/// renders.
#[wasm_bindgen]
pub struct DjComposer {
    engine: Engine,
    sample_rate: f64,
    disabled: Vec<(String, String)>,
}

#[wasm_bindgen]
impl DjComposer {
    #[wasm_bindgen(constructor)]
    pub fn new(sample_rate: f64) -> DjComposer {
        std::panic::set_hook(Box::new(|_| {}));
        let (engine, _renderer) = boot(sample_rate);
        DjComposer { engine, sample_rate, disabled: Vec::new() }
    }

    /// Compose, measure and compile genre:seed[:structure] as `deck` with `disabled` roles (a
    /// JSON list of names, the deck's disabled roles) would: the bytes `Dj::begin_install` takes.
    pub fn prepare(&mut self, deck: &str, genre: &str, seed: f64, structure: &str, disabled: &str) -> Result<Vec<u8>, JsError> {
        let deck = deck_name(deck)?;
        let spec = spec_of(genre, seed, structure)?;
        self.filter_roles(deck, disabled)?;
        let p = self.engine.preparer(deck).ok_or_else(|| JsError::new("closed"))?;
        let track = crate::host::deck::track::PreparedTrack::compose(&spec, &p, 0);
        crate::host::deck::transfer::encode(track, &p).map_err(js_err)
    }

    /// The deck's role filter as the shell has it (`disabled`: a JSON list of role names).
    fn filter_roles(&mut self, deck: &str, disabled: &str) -> Result<(), JsError> {
        let want: Vec<String> = match crate::json::JsonValue::parse(if disabled.trim().is_empty() { "[]" } else { disabled }) {
            Ok(v) => v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default(),
            Err(e) => return Err(JsError::new(&format!("{e:?}"))),
        };
        // the deck's role filter as the shell has it: undo what is not disabled any more
        for (d, r) in self.disabled.clone() {
            if d == deck && !want.contains(&r) {
                self.engine.set_role_disabled(deck, &r, false);
                self.disabled.retain(|x| !(x.0 == d && x.1 == r));
            }
        }
        for r in want {
            if !self.disabled.iter().any(|x| x.0 == deck && x.1 == r) {
                self.engine.set_role_disabled(deck, &r, true);
                self.disabled.push((deck.into(), r));
            }
        }
        Ok(())
    }

    /// [`prepare`](Self::prepare) for a beats share envelope or project (`Dj::load_share`'s
    /// text): the bytes `Dj::begin_install` takes, for a raw-`nets` deck.
    pub fn prepare_share(&mut self, deck: &str, text: &str, disabled: &str) -> Result<Vec<u8>, JsError> {
        let deck = deck_name(deck)?;
        let source = match beats::source_of(text).map_err(|e| JsError::new(&e))? {
            crate::session::model::DeckSource::RawNets(p) => crate::host::deck::track::TrackSource::RawNets(p),
            _ => return Err(JsError::new("not a raw-nets source")),
        };
        self.filter_roles(deck, disabled)?;
        let p = self.engine.preparer(deck).ok_or_else(|| JsError::new("closed"))?;
        let track = crate::host::deck::track::PreparedTrack::from_source(&source, &p, 0)
            .ok_or_else(|| JsError::new("the share has no playable nets"))?;
        crate::host::deck::transfer::encode(track, &p).map_err(js_err)
    }

    /// Prepare every `Net` deck of a snapshot as `Dj::restore` will stand it (on a fresh engine):
    /// its track, and its relocation to the saved position (the session stood here up to the
    /// seeks): the packed entries `Dj::begin_restore` takes. Checks the text against `cid` as restore does.
    pub fn prepare_snapshot(&self, text: &str, cid: &str) -> Result<Vec<u8>, JsError> {
        let checked = snapshot::check(text, Some(cid)).map_err(js_err)?;
        let (mut fresh, _r) = boot(self.sample_rate);
        install::prepare_restore(&mut fresh, &checked).map_err(js_err)
    }

    /// Run a compose the worklet's engine offloaded (`Dj::take_offload`'s bytes: its id, then the
    /// request) as a worker thread of that engine would: the answer, under the same id, for
    /// `Dj::offload_answer`.
    pub fn answer_offload(&self, bytes: &[u8]) -> Result<Vec<u8>, JsError> {
        let (id, request) = split_id(bytes)?;
        let answer = crate::host::deck::transfer::answer_offload(request).map_err(js_err)?;
        Ok(with_id(id, &answer))
    }

    /// Pure snapshot helpers, so the worklet need not run them: a text's CID, the schema.
    pub fn snapshot_cid(&self, text: &str) -> Result<String, JsError> {
        let v = crate::json::JsonValue::parse(text).map_err(|e| JsError::new(&format!("{e:?}")))?;
        Ok(snapshot::cid_of(&v))
    }

    pub fn snapshot_schema(&self) -> String {
        snapshot::SCHEMA_JSON.to_string()
    }

    pub fn snapshot_schema_cid(&self) -> Result<String, JsError> {
        self.snapshot_cid(snapshot::SCHEMA_JSON)
    }
}

// MARK: free functions for the beats studio, its offline renderer and its parity gate

/// The engine version: the crate (and npm package) version. Audio rendered by djrust is a
/// function of the share CID and this.
#[wasm_bindgen]
pub fn engine_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// Every effect `Dj.engage` takes, as JSON ([`beats::catalogue_json`]).
#[wasm_bindgen]
pub fn effects_catalogue() -> String {
    beats::catalogue_json()
}

/// Render `seconds` of a share envelope or beats project at `sample_rate` with no AudioContext:
/// interleaved stereo f32, deterministic for one engine version ([`beats::render_offline`]).
#[wasm_bindgen]
pub fn render_offline(text: &str, seconds: f64, sample_rate: f64) -> Result<Vec<f32>, JsError> {
    beats::render_offline(|| boot(sample_rate), text, seconds).map_err(|e| JsError::new(&e))
}

/// `compose(genre, overrides)` as `projectToJSON` writes it (`overrides`: JSON, "" for none).
#[wasm_bindgen]
pub fn compose_project(genre: &str, overrides: &str) -> Result<String, JsError> {
    beats::compose_project(genre, overrides).map_err(|e| JsError::new(&e))
}

/// The model CID of `compose(genre, overrides)` (hex sha256 of `canonicalProjectJSON`).
#[wasm_bindgen]
pub fn compose_model_cid(genre: &str, overrides: &str) -> Result<String, JsError> {
    beats::compose_model_cid(genre, overrides).map_err(|e| JsError::new(&e))
}

/// The model CID of a project's wire JSON.
#[wasm_bindgen]
pub fn project_model_cid(text: &str) -> Result<String, JsError> {
    beats::project_model_cid(text).map_err(|e| JsError::new(&e))
}

/// A share envelope's CID (`z…`).
#[wasm_bindgen]
pub fn share_cid(text: &str) -> Result<String, JsError> {
    beats::share_cid(text).map_err(|e| JsError::new(&e))
}
