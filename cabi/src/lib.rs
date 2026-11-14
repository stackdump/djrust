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
//!
//! The generator half ([`djw_compose`]) returns beats' project document byte for byte
//! (`JSON.stringify(projectToJSON(compose(genre, params)))`), its model CID and canonical size.

use std::cell::UnsafeCell;
use std::string::ToString;
use std::vec::Vec;

use djrust::json::{JsonObject, JsonValue};
use djrust::wave::runner::{WaveOptions, WaveRunner, MAX_BLOCK};
use djrust::{compose, Project};

/// Bump on any change to an export's signature or to the layout of a buffer it fills.
pub const ABI_VERSION: u32 = 1;

/// A negative return: the argument was not UTF-8, not JSON, or not what the call wants.
pub const ERR_ARG: i32 = -1;
/// A negative return: [`djw_init`] has not been called.
pub const ERR_NOT_READY: i32 = -2;

struct Engine {
    runner: WaveRunner,
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
/// `AudioContext`. Returns the largest block [`djw_process`] takes.
#[no_mangle]
pub extern "C" fn djw_init(sample_rate: f64) -> i32 {
    if !(sample_rate.is_finite() && (3000.0..=768_000.0).contains(&sample_rate)) {
        return fail(ERR_ARG, "sample rate out of range");
    }
    let engine = Engine {
        runner: WaveRunner::new(sample_rate, WaveOptions::default()),
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
    let doc = match JsonValue::parse(src) {
        Ok(v) => v,
        Err(err) => return fail(ERR_ARG, &std::format!("document is not JSON: {err:?}")),
    };
    if doc.get("nets").and_then(JsonValue::as_object).is_none() {
        return fail(ERR_ARG, "document has no nets");
    }
    e.runner.load_json(&doc);
    e.stop_reported = false;
    e.runner.graph().map(|g| g.nets.len() as i32).unwrap_or(0)
}

#[no_mangle]
pub extern "C" fn djw_play() {
    if let Some(e) = engine() {
        e.runner.play();
        e.stop_reported = false;
    }
}

#[no_mangle]
pub extern "C" fn djw_pause() {
    if let Some(e) = engine() {
        e.runner.pause();
    }
}

#[no_mangle]
pub extern "C" fn djw_stop() {
    if let Some(e) = engine() {
        e.runner.stop();
    }
}

#[no_mangle]
pub extern "C" fn djw_set_tempo(bpm: f64) {
    if let Some(e) = engine() {
        if bpm.is_finite() && bpm > 0.0 {
            e.runner.set_tempo(bpm);
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
    e.runner.set_mute(id, muted != 0);
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
    e.runner.process(l, Some(r), n);
    let ticks = e.runner.ticks_in_block();
    e.tick_pending = ticks > 0 && e.runner.graph().is_some();
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
        Some(e) if e.runner.stopped() && !e.stop_reported => {
            e.stop_reported = true;
            1
        }
        _ => 0,
    }
}

#[no_mangle]
pub extern "C" fn djw_tick() -> f64 {
    engine()
        .and_then(|e| e.runner.graph().map(|g| g.tick as f64))
        .unwrap_or(0.0)
}

#[no_mangle]
pub extern "C" fn djw_tempo() -> f64 {
    engine().map(|e| e.runner.tempo()).unwrap_or(0.0)
}

#[no_mangle]
pub extern "C" fn djw_playing() -> i32 {
    engine().map(|e| e.runner.playing() as i32).unwrap_or(0)
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
    let Some(g) = e.runner.graph() else {
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
    let Some(g) = e.runner.graph() else {
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
    let Some(g) = e.runner.graph() else {
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
