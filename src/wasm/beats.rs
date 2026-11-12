//! What the beats.bitwrap.io studio asks of the engine. The studio keeps its own UI and its own
//! sequencer Worker (which composes the project its tracks view draws); djrust plays that project
//! and tells the views what it played. So the calls here are:
//!
//! - a share envelope or a beats project loaded on a deck ([`play_payload`], [`source_of`]),
//!   always as a raw-`nets` share: the deck plays the beats project itself, with no dj
//!   musicality pass, so what sounds is what the tracks view shows;
//! - the fire / tick stream the views draw from ([`Events`]): every audible fire with its net
//!   and transition ids, the newest tick and tempo, the muted nets, the marking;
//! - an offline render with no AudioContext ([`render_offline`]): deterministic for one engine
//!   version, the same blocks and housekeeping cadence as the worklet;
//! - the composer's project and model CID for the share-CID parity gate ([`compose_project`],
//!   [`compose_model_cid`]), and the share CID of an envelope ([`share_cid`]).
//!
//! Everything here is plain Rust (strings in, strings out) so it is tested natively; `mod.rs`
//! wraps it for wasm-bindgen.

use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::cid;
use crate::gen::composer::compose;
use crate::host::deck::engine::{Engine, Renderer};
use crate::host::deck::perform::PerformCtx;
use crate::json::{JsonObject, JsonValue};
use crate::session::action::DeckAction;
use crate::session::model::DeckSource;
use crate::share::Share;
use crate::wave::{EventCursor, FireEvent, MarkingLayout, MarkingSnapshot, TickAnchor};

/// The worklet's render quantum.
pub const QUANTUM: usize = 128;
/// Housekeeping (`Dj::tick`) runs every this many quanta in the worklet (~10 ms at 48 kHz), and
/// [`render_offline`] keeps the same cadence so an offline render is the live one.
pub const TICK_EVERY: usize = 4;

fn parse(text: &str) -> Result<JsonValue, String> {
    JsonValue::parse(text).map_err(|e| format!("not JSON: {e:?}"))
}

/// A raw-`nets` share payload that plays `project` (beats' `projectToJSON` shape, as the studio's
/// worker hands it over) exactly: its nets, name, tempo, swing, humanize and initial mutes, and
/// the `genre` (so the deck takes that genre's sound settings) and `seed` (its voicing seed).
/// No `structure`: the project is already arranged, and a structure would arrange it again.
pub fn project_payload(project: &JsonValue, genre: Option<&str>, seed: Option<&JsonValue>) -> JsonValue {
    let mut o = JsonObject::new();
    o.set("@context", "https://beats.bitwrap.io/schema/beats-share".into());
    o.set("@type", "BeatsShare".into());
    o.set("v", JsonValue::int(1));
    if let Some(g) = genre.filter(|g| !g.is_empty()) {
        o.set("genre", g.into());
    }
    if let Some(s @ JsonValue::Number(_)) = seed {
        o.set("seed", s.clone());
    }
    for k in ["name", "tempo", "swing", "humanize"] {
        if let Some(v) = project.get(k).filter(|v| !matches!(v, JsonValue::Null)) {
            o.set(k, v.clone());
        }
    }
    o.set("nets", project.get("nets").cloned().unwrap_or(JsonValue::Object(JsonObject::new())));
    if let Some(m) = project.get("initialMutes").filter(|m| m.is_truthy()) {
        o.set("initialMutes", m.clone());
    }
    JsonValue::Object(o)
}

/// What a deck plays for `doc`: a raw-`nets` envelope as given; a composed envelope (genre +
/// seed + overrides) as raw nets of the project the browser builds from it
/// ([`Share::project`]: compose, the model overrides, the arrange overlay); a bare beats
/// project (`nets` and no `@type`) as [`project_payload`] of it.
pub fn play_payload(doc: &JsonValue) -> Result<JsonValue, String> {
    let has_nets = doc.get("nets").is_some_and(JsonValue::is_truthy);
    let is_share = doc.get("@type").and_then(JsonValue::as_str) == Some("BeatsShare");
    if has_nets && !is_share {
        let genre = doc.get("genre").and_then(JsonValue::as_str);
        return Ok(project_payload(doc, genre, doc.get("seed")));
    }
    if has_nets {
        return Ok(doc.clone());
    }
    let share = Share::from_payload(doc, None);
    let project = share.project().map_err(|e| format!("no project: {e:?}"))?;
    Ok(project_payload(&project.to_json(), share.genre.as_deref(), doc.get("seed")))
}

/// [`play_payload`] of a document's text, as a deck source.
pub fn source_of(text: &str) -> Result<DeckSource, String> {
    Ok(DeckSource::RawNets(play_payload(&parse(text)?)?))
}

/// Load `source` on `deck`: the load's answer.
pub fn load(engine: &mut Engine, deck: &str, source: DeckSource) -> String {
    let a = engine.perform(&DeckAction::Load { deck: deck.into(), source }, PerformCtx::default());
    format!("{a:?}")
}

// MARK: the fire / tick stream

/// A reader of one deck's fires, tick anchors and marking, for the studio's views. One per deck,
/// kept by the facade; [`read`](Events::read) returns what happened since the last read.
pub struct Events {
    cursor: EventCursor,
    fires: Vec<FireEvent>,
    anchors: Vec<TickAnchor>,
    snap: MarkingSnapshot,
    layout: Option<Arc<MarkingLayout>>,
    /// The muted nets last reported (None: not reported yet).
    muted: Option<Vec<String>>,
    /// The marking sequence last reported.
    sequence: i64,
    tick: i64,
    tempo: f64,
    frame: i64,
}

impl Default for Events {
    fn default() -> Self {
        Events {
            cursor: EventCursor::new(),
            fires: Vec::new(),
            anchors: Vec::new(),
            snap: MarkingSnapshot::new(),
            layout: None,
            muted: None,
            sequence: -1,
            tick: 0,
            tempo: 0.0,
            frame: 0,
        }
    }
}

fn q(s: &str) -> String {
    JsonValue::String(s.to_string()).canonical_js()
}

fn num(x: f64) -> String {
    if x.is_finite() {
        format!("{x}")
    } else {
        "0".into()
    }
}

impl Events {
    /// Read from now on (after a load or a seek: what came before is not this track's).
    pub fn reset(&mut self, engine: &Engine, deck: &str) {
        self.cursor = engine.event_cursor_now(deck);
        self.layout = None;
        self.muted = None;
        self.sequence = -1;
    }

    fn layout_of(&mut self, engine: &Engine, deck: &str, generation: i32) -> Option<Arc<MarkingLayout>> {
        if self.layout.as_ref().is_some_and(|l| l.generation == generation) {
            return self.layout.clone();
        }
        self.layout = engine.marking_layout_of(deck, generation);
        self.layout.clone()
    }

    /// What happened on `deck` since the last read, as JSON:
    ///
    /// `{"fires":[[netId, transitionId, note, velocity, channel, tick, frame], …],
    ///   "tick":…, "tempo":…, "frame":…, "lost":…, "muted":[netId…]?, "marking":{net:{place:n}}?}`
    ///
    /// `tick` is the graph's tick after the newest step (the first step is tick 1, as on a fire);
    /// `frame` is where that tick began in the deck's frame time base (frames rendered). `muted`
    /// is there when the set of muted nets changed; `marking` when `marking` is asked for and the
    /// marking changed since the last read.
    pub fn read(&mut self, engine: &Engine, deck: &str, marking: bool) -> String {
        self.fires.clear();
        self.anchors.clear();
        let (lost_f, lost_a) = engine.read_events(deck, &mut self.cursor, &mut self.fires, &mut self.anchors);
        if let Some(a) = self.anchors.last() {
            self.tick = a.tick as i64;
            self.tempo = a.tempo as f64;
            self.frame = a.frame;
        }
        let mut out = String::with_capacity(64 + self.fires.len() * 48);
        out.push_str("{\"fires\":[");
        let fires = core::mem::take(&mut self.fires);
        let mut first = true;
        for f in &fires {
            let Some(layout) = self.layout_of(engine, deck, f.generation) else { continue };
            let (Some(net), Some(info)) = (layout.net_ids.get(f.net as usize), layout.nets.get(f.net as usize)) else {
                continue;
            };
            let Some(t) = info.transition_ids.get(f.transition as usize) else { continue };
            if !first {
                out.push(',');
            }
            first = false;
            out.push_str(&format!(
                "[{},{},{},{},{},{},{}]",
                q(net),
                q(t),
                f.note,
                f.velocity,
                f.channel,
                f.tick,
                f.frame
            ));
        }
        self.fires = fires;
        out.push_str(&format!(
            "],\"tick\":{},\"tempo\":{},\"frame\":{},\"lost\":{}",
            self.tick,
            num(self.tempo),
            self.frame,
            lost_f + lost_a
        ));
        if engine.read_marking(deck, &mut self.snap) {
            if let Some(layout) = self.snap.layout.clone() {
                let muted: Vec<String> = layout
                    .net_ids
                    .iter()
                    .zip(self.snap.muted.iter())
                    .filter(|(_, m)| **m != 0)
                    .map(|(id, _)| id.clone())
                    .collect();
                if self.muted.as_ref() != Some(&muted) {
                    let ids: Vec<String> = muted.iter().map(|s| q(s)).collect();
                    out.push_str(&format!(",\"muted\":[{}]", ids.join(",")));
                    self.muted = Some(muted);
                }
                if marking && self.snap.header.sequence != self.sequence {
                    self.sequence = self.snap.header.sequence;
                    out.push_str(",\"marking\":{");
                    for (i, id) in layout.net_ids.iter().enumerate() {
                        if i > 0 {
                            out.push(',');
                        }
                        out.push_str(&q(id));
                        out.push_str(":{");
                        let range = layout.places_of_net(i);
                        for (k, slot) in range.enumerate() {
                            if k > 0 {
                                out.push(',');
                            }
                            let place = layout.place_ids[i].get(k).map(String::as_str).unwrap_or("");
                            out.push_str(&q(place));
                            out.push(':');
                            out.push_str(&num(self.snap.tokens.get(slot).copied().unwrap_or(0.0)));
                        }
                        out.push('}');
                    }
                    out.push('}');
                }
            }
        }
        out.push('}');
        out
    }
}

// MARK: offline render

/// Render `seconds` of `doc` (a share envelope or a beats project, as [`play_payload`] takes it)
/// at `sample_rate` on a fresh engine `boot` makes, with no AudioContext: interleaved stereo
/// f32. The same quanta and housekeeping cadence as the worklet ([`QUANTUM`], [`TICK_EVERY`]),
/// so for one engine version it is the live render, bit for bit, and it is deterministic.
pub fn render_offline(
    boot: impl FnOnce() -> (Engine, Renderer),
    text: &str,
    seconds: f64,
) -> Result<Vec<f32>, String> {
    let source = source_of(text)?;
    let (mut e, mut r) = boot();
    let sr = e.sample_rate;
    let answer = load(&mut e, "A", source);
    if e.deck("A").is_none_or(|d| d.project.is_none()) {
        return Err(format!("nothing loaded: {answer}"));
    }
    e.perform(&DeckAction::Play { deck: "A".into(), on: true }, PerformCtx::default());
    let frames = (seconds.max(0.0) * sr) as usize;
    let mut out = Vec::with_capacity(frames * 2);
    let (mut l, mut rr) = (alloc::vec![0.0f32; QUANTUM], alloc::vec![0.0f32; QUANTUM]);
    let mut n = 0usize;
    while out.len() < frames * 2 {
        r.render_block(&mut l, &mut rr);
        n += 1;
        let take = (frames - out.len() / 2).min(QUANTUM);
        for i in 0..take {
            out.push(l[i]);
            out.push(rr[i]);
        }
        if n % TICK_EVERY == 0 {
            e.service();
            e.service_loops();
        }
    }
    Ok(out)
}

// MARK: parity

/// `compose(genre, overrides)` as beats' JS composer runs it: the project, as `projectToJSON`
/// writes it (the wire shape the studio's worker hands over).
pub fn compose_project(genre: &str, overrides: &str) -> Result<String, String> {
    let ov = parse(if overrides.trim().is_empty() { "{}" } else { overrides })?;
    Ok(compose(genre, &ov).to_json().canonical_js())
}

/// The model CID (hex sha256 of `canonicalProjectJSON`) of `compose(genre, overrides)`: what
/// beats' `normalizeProjectForCID` path computes for the JS composer's project.
pub fn compose_model_cid(genre: &str, overrides: &str) -> Result<String, String> {
    let ov = parse(if overrides.trim().is_empty() { "{}" } else { overrides })?;
    Ok(compose(genre, &ov).model_cid())
}

/// The model CID of a project given as wire JSON (`parseProject` then `canonicalProjectJSON`).
pub fn project_model_cid(text: &str) -> Result<String, String> {
    Ok(crate::pflow::Project::from_wire_json(&parse(text)?).model_cid())
}

/// A share envelope's CID (`z…`), as beats' `codec.js` names it.
pub fn share_cid(text: &str) -> Result<String, String> {
    Ok(cid::share_cid(&parse(text)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::deck::engine::EngineConfig;
    use crate::host::deck::host::host_engine;

    fn boot() -> (Engine, Renderer) {
        let mut cfg = EngineConfig::new(48_000.0);
        cfg.history_beats = None;
        let (mut e, r) = host_engine(cfg, false, true);
        e.set_crossfader(0.0);
        (e, r)
    }

    fn envelope(genre: &str, seed: i64) -> String {
        format!("{{\"@context\":\"https://beats.bitwrap.io/schema/beats-share\",\"@type\":\"BeatsShare\",\"v\":1,\"genre\":\"{genre}\",\"seed\":{seed}}}")
    }

    fn peak(x: &[f32]) -> f32 {
        x.iter().fold(0.0f32, |m, v| m.max(v.abs()))
    }

    #[test]
    fn a_composed_share_plays_its_project_as_raw_nets() {
        let doc = parse(&envelope("techno", 42)).unwrap();
        let p = play_payload(&doc).unwrap();
        assert!(p.get("nets").is_some_and(JsonValue::is_truthy));
        // the project the deck plays is the one the browser builds from the share
        let want = Share::from_payload(&doc, None).project().unwrap().model_cid();
        let got = crate::pflow::Project::from_wire_json(&p).model_cid();
        assert_eq!(got, want);
    }

    #[test]
    fn a_project_round_trips_through_its_payload() {
        let p = compose("house", &parse("{\"seed\":7,\"structure\":\"standard\"}").unwrap());
        let payload = project_payload(&p.to_json(), Some("house"), Some(&JsonValue::int(7)));
        let back = Share::from_payload(&payload, None).raw_nets_project().unwrap();
        assert_eq!(back.model_cid(), p.model_cid());
    }

    #[test]
    fn compose_model_cid_matches_the_spec_project() {
        let spec = crate::TrackSpec::parse("dnb:3:standard").unwrap();
        let ov = spec.overrides().canonical_js();
        assert_eq!(compose_model_cid("dnb", &ov).unwrap(), spec.project().model_cid());
    }

    #[test]
    fn offline_render_sounds_and_is_deterministic() {
        let text = envelope("techno", 42);
        let a = render_offline(boot, &text, 4.0).unwrap();
        let b = render_offline(boot, &text, 4.0).unwrap();
        assert_eq!(a.len(), 4 * 48_000 * 2);
        assert!(peak(&a) > 0.01, "silent render (peak {})", peak(&a));
        assert!(a == b, "two renders of one share differ");
    }

    /// Golden render (plan contract 2): audio is a function of (share CID, engine version), so
    /// this hash may change only with the crate version. The same value comes out of the wasm
    /// build (`render_offline` in the npm package, node), so native and wasm agree bit for bit.
    #[test]
    fn offline_render_golden_hash() {
        let a = render_offline(boot, &envelope("techno", 42), 10.0).unwrap();
        let mut bytes = Vec::with_capacity(a.len() * 4);
        for x in &a {
            bytes.extend_from_slice(&x.to_le_bytes());
        }
        let got = cid::hex(&cid::sha256(&bytes));
        let want = match env!("CARGO_PKG_VERSION") {
            "0.1.0" => "ca88a195487d8406c42226bc27786ea971e7e1698dc25de016a305f4a4817c21",
            v => panic!("no golden render for djrust {v}: add it (render_offline techno:42, 10 s, 48 kHz)"),
        };
        assert_eq!(got, want, "techno:42 renders differently: a new engine version must bump the crate version");
    }

    #[test]
    fn events_name_the_nets_and_transitions_that_fired() {
        let (mut e, mut r) = boot();
        let src = source_of(&envelope("techno", 42)).unwrap();
        load(&mut e, "A", src);
        let mut ev = Events::default();
        ev.reset(&e, "A");
        e.perform(&DeckAction::Play { deck: "A".into(), on: true }, PerformCtx::default());
        let (mut l, mut rr) = (vec![0.0f32; QUANTUM], vec![0.0f32; QUANTUM]);
        let mut fires = 0usize;
        let mut saw_marking = false;
        for k in 0..(48_000 * 2 / QUANTUM) {
            r.render_block(&mut l, &mut rr);
            if k % TICK_EVERY == TICK_EVERY - 1 {
                e.service();
                let out = parse(&ev.read(&e, "A", true)).unwrap();
                let fs = out.get("fires").and_then(JsonValue::as_array).unwrap();
                for f in fs {
                    let f = f.as_array().unwrap();
                    assert!(f[0].as_str().is_some_and(|s| !s.is_empty()));
                    assert!(f[1].as_str().is_some_and(|s| !s.is_empty()));
                }
                fires += fs.len();
                saw_marking |= out.get("marking").is_some();
            }
        }
        assert!(fires > 8, "only {fires} fires in 2 s");
        assert!(saw_marking);
    }

    #[test]
    fn a_net_mute_silences_its_fires() {
        let (mut e, mut r) = boot();
        load(&mut e, "A", source_of(&envelope("techno", 42)).unwrap());
        let mut ev = Events::default();
        ev.reset(&e, "A");
        e.perform(&DeckAction::Play { deck: "A".into(), on: true }, PerformCtx::default());
        let (mut l, mut rr) = (vec![0.0f32; QUANTUM], vec![0.0f32; QUANTUM]);
        let mut pump = |e: &mut Engine, ev: &mut Events, blocks: usize| {
            let mut nets: Vec<String> = Vec::new();
            for k in 0..blocks {
                r.render_block(&mut l, &mut rr);
                if k % TICK_EVERY == TICK_EVERY - 1 {
                    e.service();
                    let out = parse(&ev.read(e, "A", false)).unwrap();
                    for f in out.get("fires").and_then(JsonValue::as_array).unwrap() {
                        nets.push(f.as_array().unwrap()[0].as_str().unwrap().to_string());
                    }
                }
            }
            nets
        };
        let before = pump(&mut e, &mut ev, 48_000 / QUANTUM);
        let kick = before.iter().find(|n| n.contains("kick")).cloned().expect("a kick fired");
        e.set_mute("A", &kick, true);
        pump(&mut e, &mut ev, 8); // the command lands on the next block
        let after = pump(&mut e, &mut ev, 48_000 / QUANTUM);
        assert!(!after.contains(&kick), "{kick} still fires when muted");
        assert!(!after.is_empty());
    }
}
