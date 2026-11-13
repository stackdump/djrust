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
//! Everything here is plain Rust (strings in, strings out): the wasm facade (`djrust::wasm`)
//! wraps it for wasm-bindgen, and native apps (the Android app) call it directly, so a beats
//! share plays the same way on every platform.

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

/// The share field naming how a share plays: absent is as written (beats' own sound: the
/// project, synth voices); [`PLAY_DJ`] is djrust's DJ sound (the musicality pass over the
/// composed track, the deck's palette), for a track a DJ app shares as it played it.
pub const PLAY_KEY: &str = "play";
pub const PLAY_DJ: &str = "dj";

/// The envelope fields that name the format, not the track.
const ENVELOPE_KEYS: [&str; 4] = ["@context", "@type", "v", PLAY_KEY];

/// What a deck plays for a document's text: a [`PLAY_DJ`] share (genre + seed, no `nets`) as
/// its track spec, so the deck composes it and runs the DJ pass; anything else as written,
/// [`play_payload`] of it as raw nets.
pub fn source_of(text: &str) -> Result<DeckSource, String> {
    let doc = parse(text)?;
    let dj = doc.get(PLAY_KEY).and_then(JsonValue::as_str) == Some(PLAY_DJ);
    if dj && !doc.get("nets").is_some_and(JsonValue::is_truthy) {
        return dj_spec(&doc).map(DeckSource::Net);
    }
    Ok(DeckSource::RawNets(play_payload(&doc)?))
}

/// The track spec a [`PLAY_DJ`] share names (its payload without the envelope's own fields).
pub fn dj_spec(doc: &JsonValue) -> Result<crate::TrackSpec, String> {
    let JsonValue::Object(o) = doc else { return Err("a share is a JSON object".into()) };
    let mut p = JsonObject::new();
    for (k, v) in o.iter() {
        if !ENVELOPE_KEYS.contains(&k) {
            p.set(k, v.clone());
        }
    }
    // a share without `cohesion` predates cohesion v2: beats composes it v1 (`shareFromPayload`),
    // so the DJ pass runs on the project the studio shows
    if p.get("cohesion").is_none() {
        p.set("cohesion", "v1".into());
    }
    crate::TrackSpec::from_payload(&JsonValue::Object(p)).ok_or_else(|| "a dj share needs a genre and a seed".into())
}

/// The beats share envelope of `spec` played with djrust's DJ sound ([`PLAY_DJ`]): what a DJ
/// app hands beats.bitwrap.io so a link plays there as it played on the deck. Its CID is not
/// the as-written track's (it sounds different).
pub fn dj_envelope(spec: &crate::TrackSpec) -> JsonValue {
    let mut o = JsonObject::new();
    o.set("@context", "https://beats.bitwrap.io/schema/beats-share".into());
    o.set("@type", "BeatsShare".into());
    o.set("v", JsonValue::int(1));
    if let JsonValue::Object(p) = spec.payload() {
        for (k, v) in p.iter() {
            o.set(k, v.clone());
        }
    }
    o.set(PLAY_KEY, PLAY_DJ.into());
    JsonValue::Object(o)
}

/// What a beats link (`https://beats.bitwrap.io/?cid=…[&z=…]`, a `?query`, or a bare query)
/// holds: the envelope itself (a self-contained `z` link, checked against its CID), or only the
/// CID, whose envelope is fetched from the share store (`/o/{cid}`).
#[derive(Clone, Debug, PartialEq)]
pub enum LinkSource {
    Envelope(JsonValue),
    NeedsFetch(String),
}

pub fn link_source(link: &str) -> Result<LinkSource, String> {
    let q = link.split_once('?').map_or(link, |(_, q)| q);
    let q = q.split_once('#').map_or(q, |(q, _)| q);
    let get = |k: &str| {
        q.split('&').find_map(|kv| kv.split_once('=').filter(|(a, _)| *a == k).map(|(_, v)| v))
    };
    let cid = get("cid").filter(|c| !c.is_empty()).ok_or("not a beats link: no cid")?;
    if !cid.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err("not a beats link: bad cid".into());
    }
    match get("z").filter(|z| !z.is_empty()) {
        None => Ok(LinkSource::NeedsFetch(cid.to_string())),
        Some(z) => {
            let text = crate::codec::b64url_to_gunzip(z).map_err(|e| alloc::format!("{e:?}"))?;
            let doc = parse(&text)?;
            if cid::share_cid(&doc) != cid {
                return Err("the link's payload does not hash to its cid".into());
            }
            Ok(LinkSource::Envelope(doc))
        }
    }
}

/// An envelope fetched for a [`LinkSource::NeedsFetch`] cid, checked: it must hash to it.
pub fn checked_envelope(cid: &str, text: &str) -> Result<JsonValue, String> {
    let doc = parse(text)?;
    if cid::share_cid(&doc) != cid {
        return Err("the fetched envelope does not hash to the link's cid".into());
    }
    Ok(doc)
}

/// The self-contained link (`?cid=…&z=…`) of an envelope on `base`
/// (e.g. `https://beats.bitwrap.io/`).
pub fn link_of(envelope: &JsonValue, base: &str) -> String {
    crate::share::ShareLink::forms(envelope, base).full
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

// MARK: the studio's controls (FX panel, macros, Beats pads, mixer strips)

/// Any [`DeckAction`] as JSON (`{"op": "masterFx", "key": "hp-freq", "value": 40}`,
/// `{"op": "stinger", "n": 1}`, `{"op": "fxReset"}`, `{"op": "panic"}`, …) on `deck`, unless
/// the action names its own. Returns `"done"`, or why it did not run (`"ignored: …"`,
/// `"deferred: …"`); an op the parser does not know is an error.
pub fn perform(engine: &mut Engine, deck: &str, text: &str) -> Result<String, String> {
    use crate::host::deck::perform::Applied;
    let mut v = parse(text)?;
    if let JsonValue::Object(o) = &mut v {
        if o.get("deck").is_none() {
            o.set("deck", deck.into());
        }
    }
    let a = DeckAction::from_json(&v).ok_or_else(|| alloc::format!("not a deck action: {text}"))?;
    Ok(match engine.perform(&a, PerformCtx::default()) {
        Applied::Done => "done".to_string(),
        Applied::Ignored(w) => alloc::format!("ignored: {w}"),
        Applied::Deferred(w) => alloc::format!("deferred: {w}"),
    })
}

/// How long an engaged effect stays on: `"momentary"` (while held), `"latch"` (until let go),
/// or a whole number of bars (`"1"` … `"4"`; `"bars:2"` reads the same).
pub fn engagement(mode: &str) -> Result<crate::host::deck::engage::Engagement, String> {
    use crate::host::deck::engage::Engagement;
    let m = mode.trim();
    match m {
        "momentary" | "hold" | "" => Ok(Engagement::Momentary),
        "latch" => Ok(Engagement::Latch),
        _ => match m.strip_prefix("bars:").unwrap_or(m).parse::<u8>() {
            Ok(b) if b >= 1 => Ok(Engagement::Bars(b)),
            _ => Err(alloc::format!("engagement {mode:?}: momentary, latch or a number of bars")),
        },
    }
}

/// Fire beats macro `id` (`public/lib/macros/catalog.js`: sweeps, riser, cut, breakdown,
/// pan and shape moves, `feel-*`, …) on `deck` for `bars` (0: the macro's own length), as the
/// studio's Macros panel and Auto-DJ do. `"done"`, or why it did not run.
pub fn fire_macro(engine: &mut Engine, deck: &str, id: &str, bars: i32) -> Result<String, String> {
    use crate::host::deck::perform::Applied;
    if !Engine::is_macro_id(id) {
        return Err(alloc::format!("no macro {id:?}"));
    }
    Ok(match engine.apply_macro(deck, id, (bars > 0).then_some(bars)) {
        Applied::Done => "done".to_string(),
        Applied::Ignored(w) => alloc::format!("ignored: {w}"),
        Applied::Deferred(w) => alloc::format!("deferred: {w}"),
    })
}

/// Engage catalogue effect `id` ([`catalogue_json`]: a trick's name or a macro id) on
/// `deck`'s next bar.
pub fn engage(engine: &mut Engine, deck: &str, id: &str, mode: &str) -> Result<(), String> {
    engine.engage(deck, id, engagement(mode)?)
}

/// Every effect the catalogue offers, for the studio's panels:
/// `[{"id","label","category","transition","latchable","needsNet"}]` in catalogue order.
pub fn catalogue_json() -> String {
    let rows = crate::fx::catalogue::ENTRIES
        .iter()
        .map(|e| {
            let mut o = JsonObject::new();
            o.set("id", e.id.into());
            o.set("label", e.label.into());
            o.set("category", e.category.id().into());
            o.set("transition", e.transition().into());
            o.set("latchable", e.latchable().into());
            o.set("needsNet", e.needs_net().into());
            o.into()
        })
        .collect();
    JsonValue::Array(rows).canonical_js()
}

/// The effects engaged on `deck` now: `[{"id","latched","pending"}]`, for the pads' lights.
pub fn fx_state_json(engine: &Engine, deck: &str) -> String {
    let rows = engine
        .fx_state(deck)
        .into_iter()
        .map(|s| {
            let mut o = JsonObject::new();
            o.set("id", s.id.into());
            o.set("latched", s.latched.into());
            o.set("pending", s.pending.into());
            o.into()
        })
        .collect();
    JsonValue::Array(rows).canonical_js()
}

/// A mixer strip's voice on `deck`'s MIDI `channel`: the keys given (`gainDB`, `decayScale`,
/// `lowpass`, `distortion`, … as [`VoiceTweak`](crate::wave::VoiceTweak) names them) change, a
/// key given as `null` goes back to the instrument's own, the rest stay. Live, and kept across
/// the next project's hand-over. False off a net deck or with no net on that channel.
///
/// `decayMul` (0.05 … 3, beats' mixer decay slider / 100) scales the instrument's own decay
/// (`decayScale`) rather than setting it: 1 is the instrument's own.
pub fn set_voice(engine: &mut Engine, deck: &str, channel: i64, text: &str) -> Result<bool, String> {
    use crate::wave::VoiceTweak;
    let v = parse(text)?;
    if !matches!(v, JsonValue::Object(_)) {
        return Err("a voice tweak is a JSON object".into());
    }
    let mut t = engine.voice_tweak(deck, channel);
    for k in VoiceTweak::KEYS {
        match v.get(k) {
            Some(JsonValue::Null) => t.set(k, None),
            Some(x) => {
                let n = x.as_f64().ok_or_else(|| alloc::format!("{k}: not a number"))?;
                t.set(k, Some(n));
            }
            None => {}
        }
    }
    match v.get("decayMul") {
        Some(JsonValue::Null) => t.set("decayScale", None),
        Some(x) => {
            let m = x.as_f64().ok_or("decayMul: not a number")?.clamp(0.05, 3.0);
            let Some(base) = engine.channel_voice(deck, channel).map(|c| c.voice.decay_scale) else {
                return Ok(false);
            };
            t.set("decayScale", if (m - 1.0).abs() < 1e-9 { None } else { Some(base * m) });
        }
        None => {}
    }
    Ok(engine.set_voice_tweak(deck, &t, channel))
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
            // 0.2.0 adds controls (perform, fire_macro, engage, set_voice); the render is unchanged
            "0.1.0" | "0.2.0" => "ca88a195487d8406c42226bc27786ea971e7e1698dc25de016a305f4a4817c21",
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

    /// A techno:42 deck, playing, and a way to run it: `run(e, r, blocks)` renders that many
    /// quanta with the worklet's housekeeping and returns the left channel.
    fn playing() -> (Engine, Renderer) {
        let (mut e, r) = boot();
        load(&mut e, "A", source_of(&envelope("techno", 42)).unwrap());
        e.perform(&DeckAction::Play { deck: "A".into(), on: true }, PerformCtx::default());
        (e, r)
    }

    fn run(e: &mut Engine, r: &mut Renderer, blocks: usize) -> Vec<f32> {
        let (mut l, mut rr) = (vec![0.0f32; QUANTUM], vec![0.0f32; QUANTUM]);
        let mut out = Vec::with_capacity(blocks * QUANTUM);
        for k in 0..blocks {
            r.render_block(&mut l, &mut rr);
            out.extend_from_slice(&l);
            if k % TICK_EVERY == TICK_EVERY - 1 {
                e.service();
            }
        }
        out
    }

    #[test]
    fn perform_takes_deck_actions_as_json() {
        let (mut e, mut r) = playing();
        run(&mut e, &mut r, 8);
        // the deck is filled in when the action does not name one
        assert_eq!(perform(&mut e, "A", r#"{"op":"masterFx","key":"lp-freq","value":10}"#).unwrap(), "done");
        assert_eq!(perform(&mut e, "A", r#"{"op":"fxReset"}"#).unwrap(), "done");
        assert!(perform(&mut e, "A", r#"{"op":"panic"}"#).unwrap().len() > 0);
        assert!(perform(&mut e, "A", r#"{"op":"no-such-op"}"#).is_err());
        assert!(perform(&mut e, "A", "not json").is_err());
    }

    #[test]
    fn a_master_fx_slider_changes_what_sounds() {
        let (mut a, mut ra) = playing();
        let (mut b, mut rb) = playing();
        // the low-pass slider all the way down on one of two identical decks
        assert_eq!(perform(&mut b, "A", r#"{"op":"masterFx","key":"lp-freq","value":0}"#).unwrap(), "done");
        let blocks = 48_000 / QUANTUM;
        let (x, y) = (run(&mut a, &mut ra, blocks), run(&mut b, &mut rb, blocks));
        assert!(peak(&x) > 0.01);
        assert!(x != y, "the lp-freq slider did nothing");
    }

    #[test]
    fn engagements_latch_show_and_let_go() {
        let (mut e, mut r) = playing();
        run(&mut e, &mut r, 8);
        let cat = parse(&catalogue_json()).unwrap();
        let id = cat.as_array().unwrap().iter()
            .find(|r| r.get("latchable").and_then(JsonValue::as_bool) == Some(true))
            .and_then(|r| r.get("id")?.as_str()).unwrap().to_string();
        let id = id.as_str();
        engage(&mut e, "A", id, "latch").unwrap();
        let st = parse(&fx_state_json(&e, "A")).unwrap();
        let rows = st.as_array().unwrap();
        assert!(rows.iter().any(|s| s.get("id").and_then(JsonValue::as_str) == Some(id)), "{st:?}");
        run(&mut e, &mut r, 48_000 * 3 / QUANTUM); // past its bar
        let st = parse(&fx_state_json(&e, "A")).unwrap();
        let s = st.as_array().unwrap().iter().find(|s| s.get("id").and_then(JsonValue::as_str) == Some(id)).cloned();
        assert_eq!(s.and_then(|s| s.get("latched").and_then(JsonValue::as_bool)), Some(true));
        e.release("A", id).unwrap();
        assert!(!fx_state_json(&e, "A").contains(&alloc::format!("\"{id}\"")));
        assert!(engage(&mut e, "A", "no-such-effect", "latch").is_err());
        assert!(engagement("0").is_err());
        assert_eq!(engagement("bars:2").unwrap(), crate::host::deck::engage::Engagement::Bars(2));
    }

    /// Every macro in beats' catalogue (`public/lib/macros/catalog.js`) but the Beats pads
    /// (`hit1`…`hit4`, which are stingers) fires on a playing deck.
    #[test]
    fn every_beats_macro_fires() {
        let (mut e, mut r) = playing();
        run(&mut e, &mut r, 8);
        assert!(fire_macro(&mut e, "A", "no-such-macro", 2).is_err());
        for id in [
            "auto-pan", "beat-repeat", "breakdown", "build-crush", "cathedral", "cut", "delay-throw",
            "double-drop", "drop", "dub-delay", "feel-ambient", "feel-build", "feel-chill", "feel-drive",
            "feel-euphoric", "feel-reset", "feel-wind-down", "filter-res", "half-time", "hard-left",
            "hard-right", "loosen", "mono", "octave-down", "octave-up", "phaser-drone", "ping-pong",
            "pitch-bend", "pulse", "reverb-wash", "riser", "solo-drums", "sweep-hp", "sweep-lp",
            "tape-stop", "tempo-anchor", "tighten", "vinyl-brake",
        ] {
            assert_eq!(fire_macro(&mut e, "A", id, 2).unwrap(), "done", "{id}");
            run(&mut e, &mut r, 4);
        }
    }

    #[test]
    fn a_strip_voice_tweak_is_set_kept_and_cleared() {
        let (mut e, mut r) = playing();
        run(&mut e, &mut r, 8);
        let ch = (1..=16)
            .find(|&c| e.channel_voice("A", c).is_some_and(|v| v.tonal))
            .expect("a tonal channel");
        assert!(set_voice(&mut e, "A", ch, r#"{"gainDB":-12,"decayScale":2}"#).unwrap());
        assert_eq!(e.voice_tweak("A", ch).get("gainDB"), Some(-12.0));
        // one key changed, the other kept
        assert!(set_voice(&mut e, "A", ch, r#"{"gainDB":-6}"#).unwrap());
        assert_eq!(e.voice_tweak("A", ch).get("gainDB"), Some(-6.0));
        assert_eq!(e.voice_tweak("A", ch).get("decayScale"), Some(2.0));
        // null is the instrument's own again
        assert!(set_voice(&mut e, "A", ch, r#"{"gainDB":null,"decayScale":null}"#).unwrap());
        assert!(e.voice_tweak("A", ch).is_empty());
        // decayMul scales the instrument's own decay; 1 is the instrument's own again
        let base = e.channel_voice("A", ch).unwrap().voice.decay_scale;
        assert!(set_voice(&mut e, "A", ch, r#"{"decayMul":2}"#).unwrap());
        let got = e.voice_tweak("A", ch).get("decayScale").unwrap();
        assert!((got - (base * 2.0).clamp(0.05, 20.0)).abs() < 1e-9, "{got} vs {base} x 2");
        assert!(set_voice(&mut e, "A", ch, r#"{"decayMul":1}"#).unwrap());
        assert!(e.voice_tweak("A", ch).is_empty());
        assert!(set_voice(&mut e, "A", ch, "[1]").is_err());
        assert!(set_voice(&mut e, "A", ch, r#"{"gainDB":"loud"}"#).is_err());
    }

    #[test]
    fn a_stinger_fires_by_number() {
        let (mut e, mut r) = playing();
        run(&mut e, &mut r, 8);
        let got = perform(&mut e, "A", r#"{"op":"stinger","n":1}"#).unwrap();
        assert!(got == "done" || got.starts_with("ignored"), "{got}");
    }

    #[test]
    fn a_dj_share_plays_its_spec_and_is_named_apart() {
        let spec = crate::TrackSpec::parse("techno:42:standard").unwrap();
        let env = dj_envelope(&spec);
        assert_eq!(env.get(PLAY_KEY).and_then(JsonValue::as_str), Some(PLAY_DJ));
        match source_of(&env.canonical_js()).unwrap() {
            DeckSource::Net(back) => assert_eq!(back.payload(), spec.payload()),
            other => panic!("a dj share played as {other:?}"),
        }
        // as written, the same track is another share (another sound, another CID)
        let written = envelope("techno", 42);
        assert!(matches!(source_of(&written).unwrap(), DeckSource::RawNets(_)));
        assert_ne!(cid::share_cid(&env), cid::share_cid(&parse(&written).unwrap()));
        assert!(dj_spec(&parse("{\"play\":\"dj\"}").unwrap()).is_err());
        // beats' share rule: no cohesion is v1
        let legacy = parse(r#"{"genre":"techno","seed":42,"play":"dj"}"#).unwrap();
        assert_eq!(dj_spec(&legacy).unwrap().cohesion, "v1");
    }

    #[test]
    fn links_give_their_envelope_or_the_cid_to_fetch() {
        let doc = parse(&envelope("house", 7)).unwrap();
        let c = cid::share_cid(&doc);
        let full = link_of(&doc, "https://beats.bitwrap.io/");
        match link_source(&full).unwrap() {
            LinkSource::Envelope(got) => assert_eq!(got.canonical_js(), doc.canonical_js()),
            other => panic!("{other:?}"),
        }
        assert_eq!(link_source(&alloc::format!("https://beats.bitwrap.io/?cid={c}")).unwrap(), LinkSource::NeedsFetch(c.clone()));
        assert_eq!(link_source(&alloc::format!("cid={c}&title=x")).unwrap(), LinkSource::NeedsFetch(c.clone()));
        // a z that is not the cid's envelope is refused
        let other = link_of(&parse(&envelope("house", 8)).unwrap(), "https://beats.bitwrap.io/");
        let z = other.split("&z=").nth(1).unwrap();
        assert!(link_source(&alloc::format!("?cid={c}&z={z}")).is_err());
        assert!(link_source("https://beats.bitwrap.io/feed").is_err());
        assert!(checked_envelope(&c, &doc.canonical_js()).is_ok());
        assert!(checked_envelope(&c, &envelope("house", 8)).is_err());
    }

    /// A beats share as written plays synth voices on every platform (a sampler bank, as the
    /// Android app installs, binds nothing for it); a composed track may sample.
    #[test]
    fn a_share_as_written_is_synth_only() {
        use crate::host::deck::track::{PreparedTrack, TrackSource};
        let (e, _r) = boot();
        let preparer = e.preparer("A").unwrap();
        let DeckSource::RawNets(p) = source_of(&envelope("techno", 42)).unwrap() else { panic!() };
        let raw = PreparedTrack::track_options(&TrackSource::RawNets(p), None, 42.0, &preparer);
        assert!(raw.synth_only);
        let spec = crate::TrackSpec::parse("techno:42").unwrap();
        let net = PreparedTrack::track_options(&TrackSource::Net(spec), None, 42.0, &preparer);
        assert!(!net.synth_only);
    }

    /// A compose in flight that the deck no longer wants (a share was opened on it) ends given
    /// back, not loaded.
    #[test]
    fn a_cancelled_compose_does_not_land() {
        use crate::host::deck::compose_job::{ComposeJobs, Landed};
        let (mut e, _r) = boot();
        let mut jobs = ComposeJobs::new();
        assert!(jobs.ask(&e, "A", crate::TrackSpec::parse("house:3").unwrap()));
        jobs.cancel("A");
        let mut landed = Vec::new();
        for _ in 0..600 {
            landed.extend(jobs.poll(&mut e));
            if !jobs.is_composing("A") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(landed.len(), 1);
        assert_eq!(landed[0].landed, Landed::Superseded);
        assert!(e.deck("A").and_then(|d| d.spec.as_ref()).is_none(), "the cancelled track loaded");
    }
}
