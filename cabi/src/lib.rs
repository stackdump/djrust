//! djwasm: djrust as a plain C-ABI WebAssembly module, the surface beats.bitwrap.io loads.
//!
//! A plain C ABI, not wasm-bindgen: the module is instantiated inside an `AudioWorklet`, whose
//! global scope has no `TextDecoder` / `TextEncoder` (wasm-bindgen's glue needs both), and the site
//! has no bundler. The JS side is a ~100-line loader (`public/djrust/loader.js` in
//! beats-bitwrap-io) that moves bytes through linear memory.
//!
//! Conventions (ABI version [`ABI_VERSION`]):
//!
//! - Text goes in through [`djw_alloc`]: the caller allocates, writes UTF-8, passes `(ptr, len)`
//!   and frees with [`djw_free`]. Text comes out through the *out buffer*: a call that produces
//!   text returns its byte length (or a negative code) and leaves the bytes at
//!   [`djw_out_ptr`], valid until the next call that writes it. After a negative return the out
//!   buffer holds a UTF-8 message.
//! - One runner per module instance (a worklet owns one). Nothing here is thread safe, and
//!   nothing needs to be: wasm32-unknown-unknown has no threads.
//! - [`djw_process`] allocates nothing: the block buffers and the event buffer are sized in
//!   [`djw_init`]. [`djw_load`] does allocate (it compiles the project), exactly as the JS wave
//!   engine's `load` does.
//! - Audio is [`WaveOptions::default`], the *parity* path: bit for bit the beats wave engine
//!   (`public/wave-engine/runner.js`), held to its output by `tests/wave.rs` in djrust and, from
//!   the JS side, by `scripts/test-djrust-wasm.mjs`. The dj-only extensions (`WaveOptions::
//!   extended`) are not switched on here; that is a later phase and a deliberate one.
//! - The runner sits in a [`StudioShell`] (djrust `wave::studio`), which adds the studio's live
//!   controls (0.3.0: net macros, control-net `fire-macro`, seek, groove, the frame clock and an
//!   event ring) without moving a sample of the default path: a control never used runs no
//!   code, and `tests/golden.rs` holds every build to 0.2.0's bytes. The new exports are
//!   additive, so the ABI stays 1: a loader feature-detects them (or reads [`djw_features`]).
//! - Every new message export (`djw_fire_macro` … `djw_set_groove`, `djw_events`) allocates
//!   nothing, as [`djw_process`] does not (`tests/alloc.rs` counts); [`djw_init`],
//!   [`djw_init_with`] and [`djw_load`] are the calls that allocate, and so does
//!   [`djw_set_instrument`] (the live instrument change rebuilds a net's lanes, as a load does).
//!
//! The generator half ([`djw_compose`]) returns beats' project document byte for byte
//! (`JSON.stringify(projectToJSON(compose(genre, params)))`), its model CID and canonical size.

use std::cell::UnsafeCell;
use std::string::ToString;
use std::vec::Vec;

use djrust::json::{JsonObject, JsonValue};
#[allow(unused_imports)] // named by the docs above
use djrust::wave::runner::WaveOptions;
use djrust::wave::runner::MAX_BLOCK;
use djrust::wave::studio::{self, feature, StudioShell};
use djrust::{compose, Project};

/// Bump on any change to an export's signature or to the layout of a buffer it fills.
pub const ABI_VERSION: u32 = 1;

/// A negative return: the argument was not UTF-8, not JSON, or not what the call wants.
pub const ERR_ARG: i32 = -1;
/// A negative return: [`djw_init`] has not been called (or there is no project to act on).
pub const ERR_NOT_READY: i32 = -2;
/// A negative return: a catalog id or flag this build knows but does not run (0.3.0: the master
/// and aux macros, `hit1`..`hit4`, permanent net macros, the stinger-voice flag). The page does
/// it itself.
pub const ERR_UNSUPPORTED: i32 = -3;

/// [`djw_init_with`] flag: swing and humanize move notes ([`djw_set_groove`]). Latched.
pub const DJW_GROOVE: u32 = studio::FLAG_GROOVE;
/// [`djw_init_with`] flag, reserved for 0.5's stinger voices: [`ERR_UNSUPPORTED`] until then.
pub const DJW_STINGER_VOICES: u32 = studio::FLAG_STINGER_VOICES;

/// Bytes the out buffer keeps reserved from [`djw_init`] on, so the message exports (an error
/// text, [`djw_events`] at its fullest) write it without allocating.
const OUT_RESERVE: usize = 16 * 1024;

struct Engine {
    shell: StudioShell,
    sample_rate: f64,
    left: Vec<f32>,
    right: Vec<f32>,
    /// The last block began a tick, whose events [`djw_report`] has not been asked for yet.
    tick_pending: bool,
    /// A self-stop (a `stop-transport` control) was already reported for this run.
    stop_reported: bool,
}

struct Global<T>(UnsafeCell<T>);
// wasm32-unknown-unknown is single threaded, and the native tests that use these exports run
// them one after another (they take `TEST_LOCK`).
unsafe impl<T> Sync for Global<T> {}

static ENGINE: Global<Option<Engine>> = Global(UnsafeCell::new(None));
static OUT: Global<Vec<u8>> = Global(UnsafeCell::new(Vec::new()));
static PANIC: Global<Vec<u8>> = Global(UnsafeCell::new(Vec::new()));

fn engine() -> Option<&'static mut Engine> {
    // SAFETY: single threaded; no reference outlives one export call.
    unsafe { (*ENGINE.0.get()).as_mut() }
}

fn out() -> &'static mut Vec<u8> {
    // SAFETY: as above.
    unsafe { &mut *OUT.0.get() }
}

fn put(text: &str) -> i32 {
    let o = out();
    o.clear();
    o.extend_from_slice(text.as_bytes());
    o.len() as i32
}

fn fail(code: i32, message: &str) -> i32 {
    put(message);
    code
}

/// # Safety
/// `ptr .. ptr + len` must be readable (a [`djw_alloc`] block the caller filled).
unsafe fn text<'a>(ptr: *const u8, len: usize) -> Option<&'a str> {
    if len == 0 {
        return Some("");
    }
    core::str::from_utf8(core::slice::from_raw_parts(ptr, len)).ok()
}

// ---------------------------------------------------------------------------------------------
// Module basics

/// The ABI version this module speaks.
#[no_mangle]
pub extern "C" fn djw_abi() -> u32 {
    ABI_VERSION
}

/// `{"abi":1,"crate":"<djrust version>","engine":"djrust","maxBlock":4096}` into the out buffer.
#[no_mangle]
pub extern "C" fn djw_info() -> i32 {
    put(&std::format!(
        "{{\"abi\":{},\"crate\":\"{}\",\"engine\":\"djrust\",\"maxBlock\":{}}}",
        ABI_VERSION,
        env!("CARGO_PKG_VERSION"),
        MAX_BLOCK
    ))
}

/// Reserve `len` bytes for the caller to write into (never null, even for 0).
#[no_mangle]
pub extern "C" fn djw_alloc(len: usize) -> *mut u8 {
    let mut v: Vec<u8> = Vec::with_capacity(len.max(1));
    let p = v.as_mut_ptr();
    core::mem::forget(v);
    p
}

/// Give back a block from [`djw_alloc`] (same `len`).
///
/// # Safety
/// `ptr` must come from [`djw_alloc`] with this `len`, and not be freed twice.
#[no_mangle]
pub unsafe extern "C" fn djw_free(ptr: *mut u8, len: usize) {
    if !ptr.is_null() {
        drop(Vec::from_raw_parts(ptr, 0, len.max(1)));
    }
}

/// Where the last text result is.
#[no_mangle]
pub extern "C" fn djw_out_ptr() -> *const u8 {
    out().as_ptr()
}

#[no_mangle]
pub extern "C" fn djw_out_len() -> usize {
    out().len()
}

/// Install a hook that keeps a panic's message (read it with [`djw_panic_ptr`] /
/// [`djw_panic_len`] after catching the trap), then lets the module abort. Call once, early.
#[no_mangle]
pub extern "C" fn djw_install_panic_hook() {
    std::panic::set_hook(std::boxed::Box::new(|info| {
        // SAFETY: single threaded; the hook runs on the panicking call's stack.
        let p = unsafe { &mut *PANIC.0.get() };
        p.clear();
        p.extend_from_slice(info.to_string().as_bytes());
    }));
}

#[no_mangle]
pub extern "C" fn djw_panic_ptr() -> *const u8 {
    // SAFETY: as above.
    unsafe { (*PANIC.0.get()).as_ptr() }
}

#[no_mangle]
pub extern "C" fn djw_panic_len() -> usize {
    // SAFETY: as above.
    unsafe { (*PANIC.0.get()).len() }
}

// ---------------------------------------------------------------------------------------------
// Generator

/// Compose a track. `spec` is JSON: `{"genre":"techno","seed":42,"structure":"standard",...}`;
/// every key but `genre` goes to `compose` as its overrides object (`seed`, `structure`,
/// `cohesion`, `bpm`, ...: whatever beats' `compose(genre, params)` reads). A missing `genre`
/// composes `techno`, as beats' fallback preset does.
///
/// On success the out buffer holds
/// `{"name":…,"modelCid":"<hex sha256>","canonicalBytes":N,"doc":<project document>}` where `doc`
/// is `JSON.stringify(projectToJSON(compose(genre, params)))` byte for byte, and the return is its
/// length. A stable text: a project loaded from it composes to the same CID on every target.
///
/// # Safety
/// `spec_ptr .. spec_ptr + spec_len` must be readable.
#[no_mangle]
pub unsafe extern "C" fn djw_compose(spec_ptr: *const u8, spec_len: usize) -> i32 {
    let Some(src) = text(spec_ptr, spec_len) else {
        return fail(ERR_ARG, "spec is not UTF-8");
    };
    let spec = match JsonValue::parse(src) {
        Ok(v) => v,
        Err(e) => return fail(ERR_ARG, &std::format!("spec is not JSON: {e:?}")),
    };
    let Some(obj) = spec.as_object() else {
        return fail(ERR_ARG, "spec must be a JSON object");
    };
    let genre = obj
        .get("genre")
        .and_then(JsonValue::as_str)
        .unwrap_or("techno")
        .to_string();
    let mut ov = JsonObject::new();
    for (k, v) in obj.iter() {
        if k != "genre" {
            ov.set(k, v.clone());
        }
    }
    let project: Project = compose(&genre, &JsonValue::Object(ov));
    let canonical = project.canonical_json();
    let doc = project.to_json().js_stringified();
    put(&std::format!(
        "{{\"name\":{},\"modelCid\":\"{}\",\"canonicalBytes\":{},\"doc\":{}}}",
        JsonValue::str(project.name.clone()).js_stringified(),
        djrust::cid::model_cid(&canonical),
        canonical.len(),
        doc
    ))
}

/// The model CID (lowercase hex sha256 of `canonicalProjectJSON`) of a project document the
/// caller already holds (the studio's `project-sync` payload): parse → normalize → canonical →
/// hash, the path `tools/parity/model-cid-parity.mjs` diffs against Go. The out buffer holds the
/// hex; the return is its length (64).
///
/// # Safety
/// `doc_ptr .. doc_ptr + doc_len` must be readable.
#[no_mangle]
pub unsafe extern "C" fn djw_model_cid(doc_ptr: *const u8, doc_len: usize) -> i32 {
    let Some(src) = text(doc_ptr, doc_len) else {
        return fail(ERR_ARG, "document is not UTF-8");
    };
    let doc = match JsonValue::parse(src) {
        Ok(v) => v,
        Err(e) => return fail(ERR_ARG, &std::format!("document is not JSON: {e:?}")),
    };
    put(&Project::from_json(&doc).model_cid())
}

// ---------------------------------------------------------------------------------------------
// Runner

/// Create (or replace) the runner at `sample_rate`. Allocates the block buffers; call once per
/// `AudioContext`. Returns the largest block [`djw_process`] takes. Exactly
/// `djw_init_with(sample_rate, 0)`.
#[no_mangle]
pub extern "C" fn djw_init(sample_rate: f64) -> i32 {
    djw_init_with(sample_rate, 0)
}

/// [`djw_init`] with construction flags: [`DJW_GROOVE`] (bit 0). [`DJW_STINGER_VOICES`] (bit 1)
/// is [`ERR_UNSUPPORTED`] in this build, any other bit [`ERR_ARG`]. Flags 0 is the 0.2.0 runner.
#[no_mangle]
pub extern "C" fn djw_init_with(sample_rate: f64, flags: u32) -> i32 {
    if !(sample_rate.is_finite() && (3000.0..=768_000.0).contains(&sample_rate)) {
        return fail(ERR_ARG, "sample rate out of range");
    }
    let shell = match StudioShell::new(sample_rate, flags) {
        Ok(s) => s,
        Err(code) if code == ERR_UNSUPPORTED => {
            return fail(code, "flag not supported by this build")
        }
        Err(code) => return fail(code, "unknown init flag"),
    };
    let o = out();
    o.reserve(OUT_RESERVE.saturating_sub(o.len()));
    let engine = Engine {
        shell,
        sample_rate,
        left: std::vec![0.0; MAX_BLOCK],
        right: std::vec![0.0; MAX_BLOCK],
        tick_pending: false,
        stop_reported: false,
    };
    // SAFETY: single threaded.
    unsafe { *ENGINE.0.get() = Some(engine) };
    MAX_BLOCK as i32
}

#[no_mangle]
pub extern "C" fn djw_sample_rate() -> f64 {
    engine().map(|e| e.sample_rate).unwrap_or(0.0)
}

fn free_retired(e: &mut Engine) {
    while let Some(p) = e.shell.runner_mut().pop_retired() {
        drop(p);
    }
}

/// Load a project document (the studio's `project-sync` JSON): at once when stopped, at the next
/// bar when playing. Returns the net count, or a negative code with a message in the out buffer.
///
/// # Safety
/// `doc_ptr .. doc_ptr + doc_len` must be readable.
#[no_mangle]
pub unsafe extern "C" fn djw_load(doc_ptr: *const u8, doc_len: usize) -> i32 {
    let Some(e) = engine() else {
        return fail(ERR_NOT_READY, "djw_init has not been called");
    };
    let Some(src) = text(doc_ptr, doc_len) else {
        return fail(ERR_ARG, "document is not UTF-8");
    };
    // The projects the runner has let go of (swapped out at a bar, faded out) wait in its
    // retire list for the host to free, so the audio thread never does. This module is that
    // host, and this is its allocating call: free them before the parse, so it reuses their
    // memory (0.2.0 never drained the list, and every load grew linear memory by a project).
    free_retired(e);
    let doc = match JsonValue::parse(src) {
        Ok(v) => v,
        Err(err) => return fail(ERR_ARG, &std::format!("document is not JSON: {err:?}")),
    };
    if doc.get("nets").and_then(JsonValue::as_object).is_none() {
        return fail(ERR_ARG, "document has no nets");
    }
    e.shell.load_json(&doc);
    // a load while stopped replaces the project at once (and a second load before the bar,
    // the pending one): free that too
    free_retired(e);
    e.stop_reported = false;
    e.shell
        .runner()
        .graph()
        .map(|g| g.nets.len() as i32)
        .unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn djw_play() {
    if let Some(e) = engine() {
        e.shell.play();
        e.stop_reported = false;
    }
}

#[no_mangle]
pub extern "C" fn djw_pause() {
    if let Some(e) = engine() {
        e.shell.pause();
    }
}

#[no_mangle]
pub extern "C" fn djw_stop() {
    if let Some(e) = engine() {
        e.shell.stop();
    }
}

#[no_mangle]
pub extern "C" fn djw_set_tempo(bpm: f64) {
    if let Some(e) = engine() {
        if bpm.is_finite() && bpm > 0.0 {
            e.shell.set_tempo(bpm);
        }
    }
}

/// Mute or unmute one net by id. Returns 0, or [`ERR_ARG`].
///
/// # Safety
/// `id_ptr .. id_ptr + id_len` must be readable.
#[no_mangle]
pub unsafe extern "C" fn djw_set_mute(id_ptr: *const u8, id_len: usize, muted: i32) -> i32 {
    let Some(e) = engine() else {
        return ERR_NOT_READY;
    };
    let Some(id) = text(id_ptr, id_len) else {
        return ERR_ARG;
    };
    e.shell.runner_mut().set_mute(id, muted != 0);
    0
}

/// Render `frames` (at most the block `djw_init` returned) into the left / right buffers
/// ([`djw_left_ptr`], [`djw_right_ptr`]). Returns the number of ticks that began in the block (0
/// almost always; at most a few); when it is above 0, [`djw_report`] describes the last tick.
/// Allocates nothing.
#[no_mangle]
pub extern "C" fn djw_process(frames: u32) -> i32 {
    let Some(e) = engine() else {
        return ERR_NOT_READY;
    };
    let n = (frames as usize).min(MAX_BLOCK);
    let (l, r) = (&mut e.left[..n], &mut e.right[..n]);
    e.shell.process(l, Some(r), n);
    let runner = e.shell.runner();
    let ticks = runner.ticks_in_block();
    e.tick_pending = ticks > 0 && runner.graph().is_some();
    ticks as i32
}

#[no_mangle]
pub extern "C" fn djw_left_ptr() -> *const f32 {
    engine()
        .map(|e| e.left.as_ptr())
        .unwrap_or(core::ptr::null())
}

#[no_mangle]
pub extern "C" fn djw_right_ptr() -> *const f32 {
    engine()
        .map(|e| e.right.as_ptr())
        .unwrap_or(core::ptr::null())
}

/// 1 once after the runner stopped itself (a `stop-transport` control), then 0 until the next
/// [`djw_play`]: the worklet's `playback-complete`.
#[no_mangle]
pub extern "C" fn djw_take_stopped() -> i32 {
    match engine() {
        Some(e) if e.shell.runner().stopped() && !e.stop_reported => {
            e.stop_reported = true;
            1
        }
        _ => 0,
    }
}

#[no_mangle]
pub extern "C" fn djw_tick() -> f64 {
    engine()
        .and_then(|e| e.shell.runner().graph().map(|g| g.tick as f64))
        .unwrap_or(0.0)
}

#[no_mangle]
pub extern "C" fn djw_tempo() -> f64 {
    engine().map(|e| e.shell.runner().tempo()).unwrap_or(0.0)
}

#[no_mangle]
pub extern "C" fn djw_playing() -> i32 {
    engine()
        .map(|e| e.shell.runner().playing() as i32)
        .unwrap_or(0)
}

/// What the last tick that [`djw_process`] reported did, for the page (the worklet's `wave-tick`
/// without its marking): `{"tick":N,"fired":[[netId,transId,{note,channel,velocity,duration,
/// durationSteps}],…],"controls":[[netId,transId,{action,targetNet,targetNote[,macro,macroBars,
/// macroParams]}],…]}`, the same objects the JS worklet takes from `parseProject`'s bindings.
/// Out buffer; returns its length, or 0 (out buffer untouched) when the last block had no tick or
/// the report was already taken. Allocates: call it on blocks that ticked, not on every block.
#[no_mangle]
pub extern "C" fn djw_report() -> i32 {
    let Some(e) = engine() else {
        return fail(ERR_NOT_READY, "djw_init has not been called");
    };
    if !e.tick_pending {
        return 0;
    }
    e.tick_pending = false;
    let Some(g) = e.shell.runner().graph() else {
        return 0;
    };
    let mut fired = Vec::new();
    for pair in g.fired[..g.fired_count.min(g.fired.len()) & !1].chunks_exact(2) {
        let (Some(n), t) = (g.nets.get(pair[0] as usize), pair[1] as usize) else {
            continue;
        };
        let Some(tid) = n.trans_ids.get(t) else {
            continue;
        };
        let midi = match n.bundle.bindings.get(tid) {
            Some(b) => {
                let mut o = JsonObject::new();
                o.set("note", JsonValue::int(b.note));
                o.set("channel", JsonValue::int(b.channel));
                o.set("velocity", JsonValue::int(b.velocity));
                o.set("duration", JsonValue::int(b.duration));
                o.set("durationSteps", JsonValue::int(b.duration_steps));
                JsonValue::Object(o)
            }
            None => JsonValue::Null,
        };
        fired.push(JsonValue::Array(std::vec![
            JsonValue::str(n.id.clone()),
            JsonValue::str(tid.clone()),
            midi
        ]));
    }
    let mut controls = Vec::new();
    for pair in g.controls[..g.control_count.min(g.controls.len()) & !1].chunks_exact(2) {
        let (Some(n), t) = (g.nets.get(pair[0] as usize), pair[1] as usize) else {
            continue;
        };
        let Some(tid) = n.trans_ids.get(t) else {
            continue;
        };
        let control = match n.bundle.control_bindings.get(tid) {
            Some(c) => {
                let mut o = JsonObject::new();
                o.set("action", JsonValue::str(c.action.clone()));
                o.set("targetNet", JsonValue::str(c.target_net.clone()));
                o.set("targetNote", JsonValue::int(c.target_note));
                if let Some(m) = c.macro_name.as_ref().filter(|m| !m.is_empty()) {
                    o.set("macro", JsonValue::str(m.clone()));
                }
                if let Some(b) = c.macro_bars.filter(|b| *b != 0.0) {
                    o.set("macroBars", JsonValue::Number(b));
                }
                if let Some(p) = c.macro_params.as_ref() {
                    o.set("macroParams", p.clone());
                }
                JsonValue::Object(o)
            }
            None => JsonValue::Null,
        };
        controls.push(JsonValue::Array(std::vec![
            JsonValue::str(n.id.clone()),
            JsonValue::str(tid.clone()),
            control
        ]));
    }
    let mut o = JsonObject::new();
    o.set("tick", JsonValue::int(g.tick));
    o.set("fired", JsonValue::Array(fired));
    o.set("controls", JsonValue::Array(controls));
    put(&JsonValue::Object(o).js_stringified())
}

/// The marking, `{netId: {placeId: tokens}}` in project order (the worklet's `markingOf`).
/// Allocates; call every few ticks, not every block. Out buffer; returns its length.
#[no_mangle]
pub extern "C" fn djw_marking() -> i32 {
    let Some(e) = engine() else {
        return fail(ERR_NOT_READY, "djw_init has not been called");
    };
    let Some(g) = e.shell.runner().graph() else {
        return put("{}");
    };
    let mut top = JsonObject::new();
    for n in &g.nets {
        let mut m = JsonObject::new();
        for p in 0..n.n_places {
            m.set(n.place_ids[p].clone(), JsonValue::Number(n.state[p]));
        }
        top.set(n.id.clone(), JsonValue::Object(m));
    }
    put(&JsonValue::Object(top).js_stringified())
}

/// Which nets are muted, `{netId: bool}` in project order. Out buffer; returns its length.
#[no_mangle]
pub extern "C" fn djw_muted() -> i32 {
    let Some(e) = engine() else {
        return fail(ERR_NOT_READY, "djw_init has not been called");
    };
    let Some(g) = e.shell.runner().graph() else {
        return put("{}");
    };
    let mut top = JsonObject::new();
    for n in &g.nets {
        top.set(
            n.id.clone(),
            JsonValue::Bool(g.muted.get(n.index).copied().unwrap_or(0) != 0),
        );
    }
    put(&JsonValue::Object(top).js_stringified())
}

// ---------------------------------------------------------------------------------------------
// 0.3.0 "net clock": additive exports (ABI 1). None of them allocates; none is needed for the
// 0.2.0 sound, which is what an instance plays until one of them is called.

/// What this build runs, one bit per studio feature (djrust `wave::studio::feature`): SEEK 1,
/// NET_MACROS 2, CONTROL_MACROS 4, GROOVE 8, FRAME_CLOCK 16, EVENTS 32 in 0.3.0; INSTRUMENT 2048
/// ([`djw_set_instrument`]) after it; FX 64, MASTER_MACROS 128, AUX_MACROS 256, VOICE 512,
/// STRIKE 1024, TRANSPOSE 4096,
/// LOOP 8192, STINGER_VOICES 16384, ROUTES 32768 later. A loader that finds no `djw_features`
/// export is talking to 0.2.0: every bit 0. (A wasm `i64`: JS receives a BigInt.)
#[no_mangle]
pub extern "C" fn djw_features() -> u64 {
    feature::THIS_BUILD
}

/// The key tables, once, as JSON in the out buffer (not for the audio path; it allocates):
/// `{"abi","features":{NAME:bit},"build":bits,"flags":{"GROOVE":1,"STINGER_VOICES":2},
/// "errors":{"ARG":-1,"NOT_READY":-2,"UNSUPPORTED":-3},"fx":[14 ids],"voice":[12 ids],
/// "macros":{"net":[…],"master":[…],"aux":[…],"hit":[…]},"maskBits":{"net":0,"master":16,
/// "aux":40,"hit":60},"events":{"kinds":[…],"sources":[…],"capacity":64},
/// "controlCapacity":16}`. `fx` index = the `djw_fx` key of 0.4 (MasterFxKey order, then
/// AuxFxKey order); `voice` index = the `djw_set_voice` key of 0.4 (VoiceTweak::KEYS, then
/// `decayMul`); a macro's bit in a mask is `maskBits[group] + its index in macros[group]`.
#[no_mangle]
pub extern "C" fn djw_keys() -> i32 {
    use djrust::fx::{AuxFxKey, AuxMacro, MasterFxKey, MasterMacro};
    use djrust::wave::{NetMacro, VoiceTweak};
    let strs = |v: &mut dyn Iterator<Item = &'static str>| {
        JsonValue::Array(v.map(JsonValue::str).collect())
    };
    let mut o = JsonObject::new();
    o.set("abi", JsonValue::int(ABI_VERSION as i64));
    let mut f = JsonObject::new();
    for (name, bit) in feature::NAMES {
        f.set(name, JsonValue::int(bit as i64));
    }
    o.set("features", JsonValue::Object(f));
    o.set("build", JsonValue::int(feature::THIS_BUILD as i64));
    let mut fl = JsonObject::new();
    fl.set("GROOVE", JsonValue::int(DJW_GROOVE as i64));
    fl.set("STINGER_VOICES", JsonValue::int(DJW_STINGER_VOICES as i64));
    o.set("flags", JsonValue::Object(fl));
    let mut er = JsonObject::new();
    er.set("ARG", JsonValue::int(ERR_ARG as i64));
    er.set("NOT_READY", JsonValue::int(ERR_NOT_READY as i64));
    er.set("UNSUPPORTED", JsonValue::int(ERR_UNSUPPORTED as i64));
    o.set("errors", JsonValue::Object(er));
    o.set(
        "fx",
        strs(
            &mut MasterFxKey::ALL
                .iter()
                .map(|k| k.id())
                .chain(AuxFxKey::ALL.iter().map(|k| k.id())),
        ),
    );
    o.set(
        "voice",
        strs(&mut VoiceTweak::KEYS.iter().copied().chain(["decayMul"])),
    );
    let mut m = JsonObject::new();
    m.set("net", strs(&mut NetMacro::ALL.iter().map(|x| x.id())));
    m.set("master", strs(&mut MasterMacro::ALL.iter().map(|x| x.id())));
    m.set("aux", strs(&mut AuxMacro::ALL.iter().map(|x| x.id())));
    m.set("hit", strs(&mut studio::HITS.iter().copied()));
    o.set("macros", JsonValue::Object(m));
    let mut b = JsonObject::new();
    b.set("net", JsonValue::int(0));
    b.set("master", JsonValue::int(studio::MASTER_BIT as i64));
    b.set("aux", JsonValue::int(studio::AUX_BIT as i64));
    b.set("hit", JsonValue::int(studio::HIT_BIT as i64));
    o.set("maskBits", JsonValue::Object(b));
    let mut ev = JsonObject::new();
    ev.set(
        "kinds",
        strs(&mut ["fired", "skipped", "restored", "tempo", "overflow"].into_iter()),
    );
    ev.set(
        "sources",
        strs(&mut ["page", "control", "engine"].into_iter()),
    );
    ev.set("capacity", JsonValue::int(studio::EVENT_CAPACITY as i64));
    o.set("events", JsonValue::Object(ev));
    o.set(
        "controlCapacity",
        JsonValue::int(studio::CONTROL_CAPACITY as i64),
    );
    put(&JsonValue::Object(o).js_stringified())
}

/// The engine frame clock: frames rendered while playing since the transport last started from
/// a stop (or a fresh load, or [`djw_init`]) or since [`djw_seek`]. Paused frames do not count;
/// a pause and play continue it. A worklet message stamped `at: F` lands on the first block
/// that starts at or after F.
#[no_mangle]
pub extern "C" fn djw_frame() -> f64 {
    engine().map(|e| e.shell.frame() as f64).unwrap_or(0.0)
}

/// Fire catalog macro `id` for `duration` in its own unit (bars; ticks for `cut`), -1 for its
/// default. 0: a net macro ran (its restore is the engine's); [`ERR_UNSUPPORTED`]: a master or
/// aux macro, `hit1`..`hit4`, or duration 0 (permanent) on a net macro (the page does those);
/// [`ERR_ARG`]: an id no catalog has, or duration below -1; [`ERR_NOT_READY`]: no project. An
/// accepted id (even unsupported) also lands in [`djw_events`] as `fired` from `page`.
///
/// # Safety
/// `id_ptr .. id_ptr + id_len` must be readable.
#[no_mangle]
pub unsafe extern "C" fn djw_fire_macro(id_ptr: *const u8, id_len: usize, duration: i32) -> i32 {
    let Some(e) = engine() else {
        return fail(ERR_NOT_READY, "djw_init has not been called");
    };
    let Some(id) = text(id_ptr, id_len) else {
        return fail(ERR_ARG, "id is not UTF-8");
    };
    match e.shell.fire_macro(id, duration) {
        ERR_ARG => fail(ERR_ARG, "no such macro, or a duration below -1"),
        ERR_NOT_READY => fail(ERR_NOT_READY, "no project loaded"),
        ERR_UNSUPPORTED => fail(ERR_UNSUPPORTED, "not run by this build: the page does it"),
        r => r,
    }
}

/// Not an export (no `extern "C"`, so not in the module): the native host oracle's switch to
/// the DJ app's exact tape-stop curve, so it can hold the port to the host bit for bit. The
/// module always sweeps in whole bpm, as the page does.
#[doc(hidden)]
pub fn oracle_tempo_sweep_rounding(on: bool) {
    if let Some(e) = engine() {
        e.shell.set_tempo_sweep_rounding(on);
    }
}

/// Let go of a latched macro: 0 for a net macro (they are timed: nothing to let go of),
/// [`ERR_UNSUPPORTED`] for a master or aux macro or a hit pad, [`ERR_ARG`] for an unknown id.
///
/// # Safety
/// `id_ptr .. id_ptr + id_len` must be readable.
#[no_mangle]
pub unsafe extern "C" fn djw_release_macro(id_ptr: *const u8, id_len: usize) -> i32 {
    let Some(e) = engine() else {
        return fail(ERR_NOT_READY, "djw_init has not been called");
    };
    let Some(id) = text(id_ptr, id_len) else {
        return fail(ERR_ARG, "id is not UTF-8");
    };
    match e.shell.release_macro(id) {
        ERR_ARG => fail(ERR_ARG, "no such macro"),
        ERR_UNSUPPORTED => fail(ERR_UNSUPPORTED, "not run by this build: the page does it"),
        r => r,
    }
}

/// Restore every macro effect now (mutes, beat repeat, tempo). With nothing running it does
/// nothing at all.
#[no_mangle]
pub extern "C" fn djw_panic() {
    if let Some(e) = engine() {
        e.shell.panic();
    }
}

/// The net macros in effect now, as bits (`maskBits`: a net macro's bit is its index in
/// `djw_keys().macros.net`). Master and aux bits join in 0.4. (A wasm `i64`: a JS BigInt.)
#[no_mangle]
pub extern "C" fn djw_macro_mask() -> u64 {
    engine().map(|e| e.shell.macro_mask()).unwrap_or(0)
}

/// Control nets' `fire-macro` transitions run in the engine (`on` != 0) or are only reported
/// in [`djw_report`]'s controls for the page to run (0, the default: 0.2.0's behaviour). With
/// it on, the id is `macro`, else the legacy `macroId`, else `targetNet`, for `macroBars` in
/// the macro's unit; an id on the disabled list ([`djw_set_macro_disabled`]) is skipped and
/// reported as `skipped`, a master or aux id is reported as `fired` with [`ERR_UNSUPPORTED`]
/// (the page runs it).
#[no_mangle]
pub extern "C" fn djw_set_control_macros(on: i32) {
    if let Some(e) = engine() {
        e.shell.set_control_macros(on != 0);
    }
}

/// Put `id` on (`disabled` != 0) or take it off the list a control net may not fire (beats'
/// `macrosDisabled`). Any catalog id; [`ERR_ARG`] for one no catalog has.
///
/// # Safety
/// `id_ptr .. id_ptr + id_len` must be readable.
#[no_mangle]
pub unsafe extern "C" fn djw_set_macro_disabled(
    id_ptr: *const u8,
    id_len: usize,
    disabled: i32,
) -> i32 {
    let Some(e) = engine() else {
        return fail(ERR_NOT_READY, "djw_init has not been called");
    };
    let Some(id) = text(id_ptr, id_len) else {
        return fail(ERR_ARG, "id is not UTF-8");
    };
    match e.shell.set_macro_disabled(id, disabled != 0) {
        ERR_ARG => fail(ERR_ARG, "no such macro"),
        r => r,
    }
}

/// Drain the engine's event ring into the out buffer as JSON; returns its length (`[]` is 2).
/// `[{"k":"fired"|"skipped"|"restored"|"tempo","id":"<catalog id or \"\">","src":"page"|
/// "control"|"engine","res":<code>,"frame":<djw_frame then>,"tick":<net tick>[,"bpm":<tempo
/// events>]},…]`, oldest first, then `{"k":"overflow",…,"res":<events dropped>}` if the
/// 64-entry ring filled. Call it when [`djw_event_count`] is above 0.
#[no_mangle]
pub extern "C" fn djw_events() -> i32 {
    let Some(e) = engine() else {
        return fail(ERR_NOT_READY, "djw_init has not been called");
    };
    struct W<'a>(&'a mut Vec<u8>);
    impl core::fmt::Write for W<'_> {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            self.0.extend_from_slice(s.as_bytes());
            Ok(())
        }
    }
    let o = out();
    o.clear();
    let _ = e.shell.write_events(&mut W(o));
    o.len() as i32
}

/// Entries waiting in [`djw_events`] (the overflow entry counts as one).
#[no_mangle]
pub extern "C" fn djw_event_count() -> u32 {
    engine().map(|e| e.shell.event_count()).unwrap_or(0)
}

/// Move the net to `tick` now (beats' `fastForwardTo`): stop, cut the tails, replay the
/// marking silently from 0, resume playing if it was. Restarts the frame clock; the next
/// [`djw_report`] is the next tick's. A seek to the tick the net is on while not playing only
/// restarts the frame clock (the neutral value: marking, mutes and tails are kept). Returns the
/// tick reached (short of `tick` only past a `stop-transport`), or [`ERR_ARG`] (`tick` negative
/// or not finite) / [`ERR_NOT_READY`] as an f64. Its cost grows with the song, but only through
/// the control nets (ticked `tick` times) and the other nets' cycle lengths (each steps alone
/// and skips whole cycles: djrust `WaveGraph::fast_forward`); the result is bit for bit the
/// tick-by-tick replay's (`fast_forward_replay`, the oracle in `tests/seek.rs`).
#[no_mangle]
pub extern "C" fn djw_seek(tick: f64) -> f64 {
    let Some(e) = engine() else {
        return fail(ERR_NOT_READY, "djw_init has not been called") as f64;
    };
    let r = e.shell.seek(tick);
    e.tick_pending = false;
    e.stop_reported = false;
    if r == ERR_ARG as f64 {
        fail(ERR_ARG, "tick must be finite and >= 0") as f64
    } else if r == ERR_NOT_READY as f64 {
        fail(ERR_NOT_READY, "no project loaded") as f64
    } else {
        r
    }
}

/// Swing and humanize (0..100, clamped) for the playing project. Notes move only on an
/// instance made with [`djw_init_with`]`(sr, DJW_GROOVE)`; elsewhere it is accepted and changes
/// nothing (0). The values stick: a project loaded later (at once, or swapped in at the bar)
/// plays them from its first tick instead of its own. [`ERR_ARG`]: not finite;
/// [`ERR_NOT_READY`]: no project (nothing is kept).
#[no_mangle]
pub extern "C" fn djw_set_groove(swing: f64, humanize: f64) -> i32 {
    let Some(e) = engine() else {
        return fail(ERR_NOT_READY, "djw_init has not been called");
    };
    match e.shell.set_groove(swing, humanize) {
        ERR_ARG => fail(ERR_ARG, "swing and humanize must be finite"),
        ERR_NOT_READY => fail(ERR_NOT_READY, "no project loaded"),
        r => r,
    }
}

/// Play net `id` on instrument `name` from the next block on, without moving the song
/// (feature INSTRUMENT, 2048): the net's lanes are rebuilt as a [`djw_load`] of the project with
/// `track.instrument` set to `name` would build them, and swapped in; the tick, the marking and
/// every other net play on. It applies to the project playing and to one queued for the next
/// bar. 0; [`ERR_NOT_READY`] with no project; [`ERR_ARG`] when either text is not UTF-8, the
/// name is empty, or `id` is not a net with lanes (a control net, an unknown id). Allocates, as
/// [`djw_load`] does (it frees the lanes it replaced in the same call).
///
/// # Safety
/// `id_ptr .. id_ptr + id_len` and `name_ptr .. name_ptr + name_len` must be readable.
#[no_mangle]
pub unsafe extern "C" fn djw_set_instrument(
    id_ptr: *const u8,
    id_len: usize,
    name_ptr: *const u8,
    name_len: usize,
) -> i32 {
    let Some(e) = engine() else {
        return fail(ERR_NOT_READY, "djw_init has not been called");
    };
    let (Some(id), Some(name)) = (text(id_ptr, id_len), text(name_ptr, name_len)) else {
        return fail(ERR_ARG, "id and name must be UTF-8");
    };
    let r = e.shell.set_instrument(id, name);
    free_retired(e);
    match r {
        ERR_ARG => fail(
            ERR_ARG,
            "no net with lanes of that id, or an empty instrument name",
        ),
        ERR_NOT_READY => fail(ERR_NOT_READY, "no project loaded"),
        r => r,
    }
}

const _: () = assert!(
    ERR_ARG == studio::ERR_ARG
        && ERR_NOT_READY == studio::ERR_NOT_READY
        && ERR_UNSUPPORTED == studio::ERR_UNSUPPORTED
);
