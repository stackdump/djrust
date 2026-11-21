//! What the listener hears, as one small JSON document: the facts a remote reader (an AI author
//! asking the site's `get`, the phone's report, `listen`'s offline pass) needs to tell why a deck
//! is silent. The page's wasm [`Dj::mixer`](crate::wasm::Dj) and the phone's player both answer it,
//! so the two report the same fields from the same engine.
//!
//! ```json
//! {"crossfader":0.5,"master":1,
//!  "decks":{"A":{"playing":true,"volume":0.8,"gain":0.5,"mute":false,"crossfade_gain":0.707,
//!                "eq":{"low":0.5,"mid":0.5,"high":0.5},"kills":[],"filter":0.5,
//!                "insert_filter":null,"vocoder":false,"level":0.42}, "B":{...}}}
//! ```
//!
//! `kills` names the EQ bands turned fully down (`low`, `mid`, `high`). `insert_filter` is the
//! deck's insert filter when it is switched on (`{"sweep":0..1,"mix":0..1}`: 0.5 open, 0 a
//! low-pass down to 60 Hz, 1 a high-pass up to 8 kHz), else null. `vocoder` is true when an
//! active vocoder takes this deck as its carrier (the deck is heard through it). `level` is the
//! strip's peak at the last block, linear, after the EQ and before the fader. Numbers are rounded
//! to three places. Nothing here judges: the reasons (crossfader, mute, level 0, ...) are the
//! reader's, from these facts.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::fx::insert::{InsertKind, InsertParam};
use crate::fx::vocoder::CarrierSource;
use crate::json::{JsonObject, JsonValue};

use super::engine::{crossfade_gain, Engine};

/// An EQ knob at or below this is a kill.
pub const KILL_AT: f64 = 0.02;

fn r3(x: f64) -> JsonValue {
    JsonValue::Number(libm::round(x * 1000.0) / 1000.0)
}

/// The mixer facts of every deck of `engine`, as above.
pub fn audible_json(engine: &Engine) -> JsonValue {
    let mut decks = JsonObject::new();
    for d in &engine.decks {
        let mut o = JsonObject::new();
        o.set("playing", JsonValue::Bool(d.mirror.is_playing()));
        o.set("volume", r3(d.volume));
        o.set("gain", r3(d.gain));
        o.set("mute", JsonValue::Bool(d.mute));
        o.set("crossfade_gain", r3(crossfade_gain(&d.name, engine.crossfader)));
        let mut eq = JsonObject::new();
        eq.set("low", r3(d.strip.eq_low));
        eq.set("mid", r3(d.strip.eq_mid));
        eq.set("high", r3(d.strip.eq_high));
        o.set("eq", JsonValue::Object(eq));
        let kills: Vec<JsonValue> = [("low", d.strip.eq_low), ("mid", d.strip.eq_mid), ("high", d.strip.eq_high)]
            .iter()
            .filter(|(_, v)| *v <= KILL_AT)
            .map(|(k, _)| JsonValue::String((*k).to_string()))
            .collect();
        o.set("kills", JsonValue::Array(kills));
        o.set("filter", r3(d.strip.filter));
        let ins = engine.insert_settings(&d.name).filter(|s| s.is_on(InsertKind::Filter));
        o.set(
            "insert_filter",
            match ins {
                Some(s) => {
                    let mut f = JsonObject::new();
                    f.set("sweep", r3(s.get(InsertParam::FilterSweep)));
                    f.set("mix", r3(s.get(InsertParam::FilterMix)));
                    JsonValue::Object(f)
                }
                None => JsonValue::Null,
            },
        );
        o.set("vocoder", JsonValue::Bool(vocoded(engine, &d.name)));
        o.set("level", r3(d.mirror.level() as f64));
        decks.set(d.name.clone(), JsonValue::Object(o));
    }
    let mut o = JsonObject::new();
    o.set("crossfader", r3(engine.crossfader));
    o.set("master", r3(engine.master));
    o.set("decks", JsonValue::Object(decks));
    JsonValue::Object(o)
}

/// [`audible_json`] as compact text.
pub fn audible_text(engine: &Engine) -> String {
    audible_json(engine).canonical_js()
}

/// An active vocoder takes `deck` as its carrier (and is not bypassed).
pub fn vocoded(engine: &Engine, deck: &str) -> bool {
    engine.vocoder.instances.iter().any(|v| v.active && !v.settings.bypass && matches!(&v.carrier, CarrierSource::Deck(n) if n == deck))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::deck::engine::{engine as make, EngineConfig};

    fn engine() -> Engine {
        let (e, _r) = make(EngineConfig::new(48_000.0));
        e
    }

    #[test]
    fn reports_every_deck_and_the_crossfader() {
        let mut e = engine();
        e.set_crossfader(0.0);
        let v = audible_json(&e);
        assert_eq!(v.get("crossfader").and_then(|x| x.as_f64()), Some(0.0));
        let b = v.get("decks").and_then(|d| d.get("B")).expect("deck B");
        // all the way to A: B's share is 0
        assert_eq!(b.get("crossfade_gain").and_then(|x| x.as_f64()), Some(0.0));
        let a = v.get("decks").and_then(|d| d.get("A")).expect("deck A");
        assert_eq!(a.get("crossfade_gain").and_then(|x| x.as_f64()), Some(1.0));
        assert_eq!(a.get("insert_filter"), Some(&JsonValue::Null));
        assert_eq!(a.get("vocoder"), Some(&JsonValue::Bool(false)));
    }

    #[test]
    fn kills_and_the_insert_filter() {
        let mut e = engine();
        e.set_crossfader(0.5);
        e.decks[0].strip.eq_low = 0.0;
        e.decks[0].mute = true;
        assert!(e.set_insert_on("A", InsertKind::Filter, true));
        assert!(e.set_insert_param("A", InsertParam::FilterSweep, 0.0));
        let v = audible_json(&e);
        let a = v.get("decks").and_then(|d| d.get("A")).expect("deck A");
        assert_eq!(a.get("kills"), Some(&JsonValue::Array(alloc::vec![JsonValue::String("low".into())])));
        assert_eq!(a.get("mute"), Some(&JsonValue::Bool(true)));
        let f = a.get("insert_filter").expect("on");
        assert_eq!(f.get("sweep").and_then(|x| x.as_f64()), Some(0.0));
        // the text is one line of JSON
        assert!(!audible_text(&e).contains('\n'));
    }
}
