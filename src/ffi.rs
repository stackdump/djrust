//! A small C ABI over the live decks, for Swift (through a bridging header) and any other
//! native caller that is not Rust. The surface mirrors the browser's ([`crate::wasm`]): make an
//! engine at a sample rate, compose `genre:seed[:structure]` onto deck A or B, play, render
//! stereo blocks, and call [`djrust_tick`] about every 10 ms for the beat clock and landed
//! composes. Strings are UTF-8 and NUL-terminated; every string this module returns is owned by
//! the caller and goes back through [`djrust_string_free`].
//!
//! ```c
//! typedef struct DjrustEngine DjrustEngine;
//! DjrustEngine *djrust_engine_new(double sample_rate);
//! void  djrust_engine_free(DjrustEngine *e);
//! char *djrust_compose(DjrustEngine *e, const char *deck, const char *spec); // NULL on error
//! int   djrust_set_playing(DjrustEngine *e, const char *deck, int on);       // 0 ok, -1 error
//! void  djrust_set_crossfader(DjrustEngine *e, double v);                     // 0 = A .. 1 = B
//! void  djrust_render(DjrustEngine *e, float *left, float *right, size_t frames);
//! void  djrust_tick(DjrustEngine *e);
//! char *djrust_version(void);
//! void  djrust_string_free(char *s);
//! ```
#![allow(unsafe_code)]

use crate::host::deck::engine::{Engine, EngineConfig, Renderer};
use crate::host::deck::host::host_engine;
use crate::host::deck::perform::PerformCtx;
use crate::session::action::DeckAction;
use crate::session::model::DeckSource;
use crate::TrackSpec;
use core::ffi::c_char;
use std::ffi::{CStr, CString};

/// An engine and its renderer. Opaque to C.
pub struct DjrustEngine {
    engine: Engine,
    renderer: Renderer,
}

fn deck_of(p: *const c_char) -> Option<&'static str> {
    if p.is_null() {
        return None;
    }
    // SAFETY: the caller passes a NUL-terminated string (module docs).
    match unsafe { CStr::from_ptr(p) }.to_str().ok()? {
        "A" | "a" => Some("A"),
        "B" | "b" => Some("B"),
        _ => None,
    }
}

fn owned(s: String) -> *mut c_char {
    CString::new(s).map(CString::into_raw).unwrap_or(core::ptr::null_mut())
}

/// A new engine at `sample_rate`, both decks empty and the crossfader at A. Composes run inline
/// on the calling thread, as in the browser.
#[no_mangle]
pub extern "C" fn djrust_engine_new(sample_rate: f64) -> *mut DjrustEngine {
    let mut cfg = EngineConfig::new(sample_rate);
    cfg.history_beats = None;
    let (mut engine, renderer) = host_engine(cfg, false, true);
    engine.set_crossfader(0.0);
    Box::into_raw(Box::new(DjrustEngine { engine, renderer }))
}

/// # Safety
/// `e` came from [`djrust_engine_new`] and is not used afterwards.
#[no_mangle]
pub unsafe extern "C" fn djrust_engine_free(e: *mut DjrustEngine) {
    if !e.is_null() {
        drop(Box::from_raw(e));
    }
}

/// Compose `spec` (`genre:seed[:structure]`) and load it on `deck`. Returns the load's answer
/// (free it with [`djrust_string_free`]), or NULL for a bad deck or spec.
///
/// # Safety
/// `e` is a live engine; `deck` and `spec` are NUL-terminated.
#[no_mangle]
pub unsafe extern "C" fn djrust_compose(e: *mut DjrustEngine, deck: *const c_char, spec: *const c_char) -> *mut c_char {
    let (Some(e), Some(deck)) = (e.as_mut(), deck_of(deck)) else { return core::ptr::null_mut() };
    if spec.is_null() {
        return core::ptr::null_mut();
    }
    let Ok(text) = CStr::from_ptr(spec).to_str() else { return core::ptr::null_mut() };
    let Ok(spec) = TrackSpec::parse(text) else { return core::ptr::null_mut() };
    let a = e.engine.perform(
        &DeckAction::Load { deck: deck.into(), source: DeckSource::Net(spec) },
        PerformCtx::default(),
    );
    owned(format!("{a:?}"))
}

/// Start (`on` != 0) or stop `deck`. 0, or -1 for a bad deck.
///
/// # Safety
/// `e` is a live engine; `deck` is NUL-terminated.
#[no_mangle]
pub unsafe extern "C" fn djrust_set_playing(e: *mut DjrustEngine, deck: *const c_char, on: i32) -> i32 {
    let (Some(e), Some(deck)) = (e.as_mut(), deck_of(deck)) else { return -1 };
    e.engine.perform(&DeckAction::Play { deck: deck.into(), on: on != 0 }, PerformCtx::default());
    0
}

/// # Safety
/// `e` is a live engine.
#[no_mangle]
pub unsafe extern "C" fn djrust_set_crossfader(e: *mut DjrustEngine, v: f64) {
    if let Some(e) = e.as_mut() {
        e.engine.set_crossfader(v.clamp(0.0, 1.0));
    }
}

/// Render `frames` stereo frames into `left` and `right`.
///
/// # Safety
/// `e` is a live engine; `left` and `right` each hold `frames` floats.
#[no_mangle]
pub unsafe extern "C" fn djrust_render(e: *mut DjrustEngine, left: *mut f32, right: *mut f32, frames: usize) {
    let Some(e) = e.as_mut() else { return };
    if left.is_null() || right.is_null() {
        return;
    }
    let l = core::slice::from_raw_parts_mut(left, frames);
    let r = core::slice::from_raw_parts_mut(right, frames);
    e.renderer.render_block(l, r);
}

/// Housekeeping, about every 10 ms: the beat clock, landed composes, song ends.
///
/// # Safety
/// `e` is a live engine.
#[no_mangle]
pub unsafe extern "C" fn djrust_tick(e: *mut DjrustEngine) {
    if let Some(e) = e.as_mut() {
        e.engine.service();
        e.engine.service_loops();
    }
}

/// The crate version, e.g. "0.1.0".
#[no_mangle]
pub extern "C" fn djrust_version() -> *mut c_char {
    owned(env!("CARGO_PKG_VERSION").into())
}

/// # Safety
/// `s` came from this module, or is NULL.
#[no_mangle]
pub unsafe extern "C" fn djrust_string_free(s: *mut c_char) {
    if !s.is_null() {
        drop(CString::from_raw(s));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compose_play_render_through_the_c_abi() {
        unsafe {
            let e = djrust_engine_new(48_000.0);
            let deck = CString::new("A").unwrap();
            let spec = CString::new("house:7").unwrap();
            let answer = djrust_compose(e, deck.as_ptr(), spec.as_ptr());
            assert!(!answer.is_null());
            djrust_string_free(answer);
            assert_eq!(djrust_set_playing(e, deck.as_ptr(), 1), 0);
            let (mut l, mut r) = (vec![0f32; 4800], vec![0f32; 4800]);
            for _ in 0..20 {
                djrust_tick(e);
                djrust_render(e, l.as_mut_ptr(), r.as_mut_ptr(), l.len());
            }
            assert!(l.iter().chain(&r).any(|s| *s != 0.0), "a playing deck is heard");
            let bad = CString::new("C").unwrap();
            assert_eq!(djrust_set_playing(e, bad.as_ptr(), 1), -1);
            djrust_engine_free(e);
        }
    }
}
