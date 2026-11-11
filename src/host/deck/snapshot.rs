//! Whole-session snapshots: `DjSnapshot` v1 (the schema `dj-snapshot.v1`, kept with the identity
//! identity wallet's `schema/`; designed in djweb `docs/SNAPSHOTS.md`). Moved here from djweb
//! `djwasm::snapshot` so djweb, djandroid and djapp capture and restore the same document.
//!
//! [`capture`] writes the engine as a `DjSnapshot` (djrust's own shapes: `DeckSource`,
//! `DeckParams`, the arrangement-start live ops); [`restore`] stands a **fresh** engine on one
//! (the live-engine twin of djhost `offline::stand_on_start`). Capture → restore → capture is
//! byte-identical, so the CID names the state.

use crate::cid;
use crate::json::{JsonObject, JsonValue};
use crate::session::action::DeckAction;
use crate::session::arrangement::ArrangeOp;
use crate::session::model::{DeckParams, DeckSource};
use crate::host::deck::engine::Engine;
use crate::host::deck::perform::PerformCtx;

pub const CONTEXT: &str = "https://beats.bitwrap.io/schema/beats-share.context.jsonld";
pub const TYPE: &str = "DjSnapshot";
pub const VERSION: i64 = 1;

/// The setup groups, in replay order.
const GROUPS: [&str; 4] = ["inputs", "aux", "post", "vocoders"];

#[derive(Debug, PartialEq)]
pub enum SnapshotError {
    Parse(String),
    NotASnapshot(&'static str),
    CidMismatch { expected: String, actual: String },
    /// A net deck's share does not hash to the cid it carries.
    TrackCidMismatch { deck: String },
    BadDeck { deck: String, why: String },
}

impl core::fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            SnapshotError::Parse(e) => write!(f, "not JSON: {e}"),
            SnapshotError::NotASnapshot(why) => write!(f, "not a DjSnapshot v1: {why}"),
            SnapshotError::CidMismatch { expected, actual } => {
                write!(f, "snapshot hashes to {actual}, not {expected}")
            }
            SnapshotError::TrackCidMismatch { deck } => {
                write!(f, "deck {deck}: the track share does not hash to its cid")
            }
            SnapshotError::BadDeck { deck, why } => write!(f, "deck {deck}: {why}"),
        }
    }
}

fn ops_json(ops: Vec<ArrangeOp>) -> JsonValue {
    JsonValue::Array(ops.iter().map(ArrangeOp::json).collect())
}

/// The engine now, as a `DjSnapshot` document.
pub fn capture(e: &Engine, title: Option<&str>) -> JsonValue {
    let (sources, start) = e.arrange_start();
    let mut decks = JsonObject::new();
    for (name, params) in &start.params {
        if name == "L" {
            continue;
        }
        let source = sources.get(name).cloned().unwrap_or(DeckSource::None);
        let playing = start
            .playing
            .as_ref()
            .is_some_and(|p| p.iter().any(|d| d == name));
        let mut d = JsonObject::from_pairs([
            ("source", source.json()),
            ("params", params.json()),
            ("playing", JsonValue::Bool(playing)),
        ]);
        if source != DeckSource::None {
            if let Some(p) = e.journal_position(name).filter(|p| p.is_finite() && *p > 0.0) {
                d.set("position", JsonValue::Number(p));
            }
        }
        decks.set(name.as_str(), JsonValue::Object(d));
    }
    let mut mixer = JsonObject::from_pairs([
        ("crossfader", JsonValue::Number(start.crossfader.unwrap_or(0.0))),
        ("master", JsonValue::Number(start.master.unwrap_or(0.8))),
    ]);
    if let Some(g) = start.loop_gain {
        mixer.set("loopGain", JsonValue::Number(g));
    }
    let setup = JsonObject::from_pairs([
        ("inputs", ops_json(e.input_setup_ops())),
        ("aux", ops_json(e.aux_setup_ops())),
        ("post", ops_json(e.post_setup_ops())),
        ("vocoders", ops_json(e.vocoder_setup_ops())),
    ]);
    let mut o = JsonObject::from_pairs([
        ("@context", JsonValue::str(CONTEXT)),
        ("@type", JsonValue::str(TYPE)),
        ("v", JsonValue::int(VERSION)),
        ("decks", JsonValue::Object(decks)),
        ("mixer", JsonValue::Object(mixer)),
        ("setup", JsonValue::Object(setup)),
    ]);
    if let Some(t) = title.filter(|t| !t.is_empty()) {
        o.set("title", JsonValue::str(t));
    }
    JsonValue::Object(o)
}

/// The canonical bytes (JCS) the CID hashes.
pub fn canonical(doc: &JsonValue) -> String {
    doc.canonical_js()
}

/// `z…` CIDv1 dag-json sha2-256, the beats share CID.
pub fn cid_of(doc: &JsonValue) -> String {
    cid::share_cid(doc)
}

/// A snapshot checked before anything touches the engine: the document's shape, its CID (when
/// `expect_cid`), and every net deck's share against its own cid.
pub struct Checked {
    pub doc: JsonValue,
    pub cid: String,
    pub decks: Vec<(String, DeckSource, DeckParams, bool, Option<f64>)>,
    pub setup: Vec<ArrangeOp>,
    pub crossfader: f64,
    pub master: f64,
    pub loop_gain: Option<f64>,
}

pub fn check(text: &str, expect_cid: Option<&str>) -> Result<Checked, SnapshotError> {
    let doc = JsonValue::parse(text).map_err(|e| SnapshotError::Parse(format!("{e:?}")))?;
    let cid = cid_of(&doc);
    if let Some(x) = expect_cid.filter(|x| !x.is_empty()) {
        if x != cid {
            return Err(SnapshotError::CidMismatch { expected: x.into(), actual: cid });
        }
    }
    let not = SnapshotError::NotASnapshot;
    if doc.get("@type").and_then(JsonValue::as_str) != Some(TYPE) {
        return Err(not("@type"));
    }
    if doc.get("v").and_then(JsonValue::as_i64) != Some(VERSION) {
        return Err(not("v"));
    }
    let decks_o = doc.get("decks").and_then(JsonValue::as_object).ok_or(not("decks"))?;
    let mut decks = Vec::new();
    for (name, d) in decks_o.iter() {
        let bad = |why: &str| SnapshotError::BadDeck { deck: name.into(), why: why.into() };
        let sj = d.get("source").ok_or_else(|| bad("no source"))?;
        let source = DeckSource::from_json(sj).ok_or_else(|| bad("unreadable source"))?;
        if let (Some(share), Some(c)) = (sj.get("share"), sj.get("cid").and_then(JsonValue::as_str)) {
            if cid::share_cid(share) != c {
                return Err(SnapshotError::TrackCidMismatch { deck: name.into() });
            }
        }
        let params = DeckParams::from_json(d.get("params").ok_or_else(|| bad("no params"))?);
        let playing = d.get("playing").and_then(JsonValue::as_bool).unwrap_or(false);
        let position = d.get("position").and_then(JsonValue::as_f64);
        decks.push((name.to_string(), source, params, playing, position));
    }
    let mixer = doc.get("mixer").ok_or(not("mixer"))?;
    let m = |k| mixer.get(k).and_then(JsonValue::as_f64);
    let setup_o = doc.get("setup").and_then(JsonValue::as_object).ok_or(not("setup"))?;
    let mut setup = Vec::new();
    for g in GROUPS {
        for op in setup_o.get(g).and_then(JsonValue::as_array).map(|a| a.as_slice()).unwrap_or(&[]) {
            setup.push(ArrangeOp::from_json(op).ok_or(not("setup op"))?);
        }
    }
    Ok(Checked {
        crossfader: m("crossfader").ok_or(not("mixer.crossfader"))?,
        master: m("master").ok_or(not("mixer.master"))?,
        loop_gain: m("loopGain"),
        doc,
        cid,
        decks,
        setup,
    })
}

/// Stand `e` (a fresh engine, deterministic: composes land inline) on a checked snapshot. Returns
/// the moves the engine refused, as text; an empty list is an exact restore.
pub fn stand(e: &mut Engine, s: &Checked) -> Vec<String> {
    let mut refused = Vec::new();
    for (name, source, ..) in &s.decks {
        if *source == DeckSource::None {
            continue;
        }
        let mut ctx = PerformCtx::default();
        if let DeckSource::Sample { id, .. } = source {
            ctx.buffer = e.resolve_sample(id);
            if ctx.buffer.is_none() {
                refused.push(format!("deck {name}: sample {id} is not here"));
                continue;
            }
        }
        e.apply_deck_action(&DeckAction::Load { deck: name.clone(), source: source.clone() }, ctx);
    }
    e.service();
    for (name, _, params, ..) in &s.decks {
        e.apply_params(name, params);
    }
    e.set_crossfader(s.crossfader);
    e.set_master(s.master);
    if let Some(g) = s.loop_gain {
        e.set_loop_gain(g);
    }
    for op in &s.setup {
        match op {
            ArrangeOp::Live { method, params } => {
                if let Err(why) = e.apply_live_op(method, params) {
                    refused.push(format!("{method}: {why}"));
                }
            }
            op => {
                if !e.apply_arrange_op(op) {
                    refused.push(format!("{} refused", op.op()));
                }
            }
        }
    }
    for (name, source, _, playing, position) in &s.decks {
        if *source == DeckSource::None {
            continue;
        }
        if let Some(p) = position {
            e.seek_journal_position(name, *p);
        }
        if *playing && !e.play(name) {
            refused.push(format!("deck {name}: would not play"));
        }
    }
    e.service();
    refused
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TrackSpec;
    use crate::host::deck::engine::{EngineConfig, Renderer};
    use crate::host::deck::host::host_engine;

    /// An engine as the facade boots one, with its renderer.
    struct Rig {
        e: Engine,
        r: Renderer,
    }

    impl Rig {
        /// Render `blocks` worklet quanta (128 frames), servicing every 4th as the worklet does.
        fn pump(&mut self, blocks: usize) {
            let (mut l, mut r) = (vec![0.0f32; 128], vec![0.0f32; 128]);
            for k in 0..blocks {
                self.r.render_block(&mut l, &mut r);
                if k % 4 == 3 {
                    self.e.service();
                }
            }
            self.e.service();
        }
    }

    fn fresh() -> Rig {
        let mut cfg = EngineConfig::new(48_000.0);
        cfg.history_beats = None;
        let (mut e, r) = host_engine(cfg, false, true);
        e.set_crossfader(0.0);
        Rig { e, r }
    }

    fn load(e: &mut Engine, deck: &str, spec: &str) {
        let spec = TrackSpec::parse(spec).unwrap();
        e.perform(
            &DeckAction::Load { deck: deck.into(), source: DeckSource::Net(spec) },
            PerformCtx::default(),
        );
        e.service();
    }

    /// Two decks, knobs, crossfader, two vocoder instances, a seeked deck.
    fn busy() -> Rig {
        let mut rig = fresh();
        let e = &mut rig.e;
        load(e, "A", "techno:42:standard");
        load(e, "B", "house:43:loop");
        e.perform(&DeckAction::Param { deck: "A".into(), name: "volume".into(), value: 0.6 }, PerformCtx::default());
        e.perform(&DeckAction::Param { deck: "B".into(), name: "pitch".into(), value: 0.1 }, PerformCtx::default());
        e.perform(&DeckAction::Param { deck: "B".into(), name: "eqLow".into(), value: 0.2 }, PerformCtx::default());
        e.set_crossfader(0.35);
        let v = |s: &str| JsonValue::parse(s).unwrap();
        e.apply_live_op("vocoder", &v(r#"{"carrier":"synth","modulator":"deck:B"}"#)).unwrap();
        e.apply_live_op("vocoder", &v(r#"{"on":true}"#)).unwrap();
        e.vocoder_add().unwrap();
        e.apply_live_op("vocoder", &v(r#"{"instance":1,"carrier":"deck:A"}"#)).unwrap();
        e.seek_journal_position("A", 64.0);
        e.play("A");
        rig.pump(200); // ~0.5 s: deck A runs on past tick 64
        rig
    }

    /// The snapshot with the decks' positions taken out, and the positions.
    fn split_positions(doc: &JsonValue) -> (String, Vec<(String, Option<f64>)>) {
        let mut d = doc.clone();
        let mut pos = Vec::new();
        if let JsonValue::Object(o) = &mut d {
            if let Some(JsonValue::Object(decks)) = o.get_mut("decks") {
                let names: Vec<String> = decks.iter().map(|(n, _)| n.to_string()).collect();
                for name in names {
                    if let Some(JsonValue::Object(x)) = decks.get_mut(&name) {
                        pos.push((name.clone(), x.remove("position").and_then(|p| p.as_f64())));
                    }
                }
            }
        }
        (d.canonical_js(), pos)
    }

    #[test]
    fn round_trip_is_byte_identical() {
        let a = busy();
        let s1 = capture(&a.e, Some("test set"));
        let text = canonical(&s1);
        let checked = check(&text, Some(&cid_of(&s1))).unwrap();
        let mut b = fresh();
        let refused = stand(&mut b.e, &checked);
        assert!(refused.is_empty(), "refused: {refused:?}");
        // The deck mirrors catch up on the first render quantum, which also plays the tick a
        // playing deck was about to play (position = the next tick to play). So: everything else
        // is byte-identical, and a playing deck has moved on by exactly that one tick.
        b.pump(1);
        let s2 = capture(&b.e, Some("test set"));
        let (rest1, pos1) = split_positions(&s1);
        let (rest2, pos2) = split_positions(&s2);
        assert_eq!(rest2, rest1);
        let playing = |s: &JsonValue, n: &str| s.get("decks").unwrap().get(n).unwrap().get("playing").unwrap().as_bool().unwrap();
        for ((n, p1), (_, p2)) in pos1.iter().zip(&pos2) {
            let step = if playing(&s1, n) { 1.0 } else { 0.0 };
            assert_eq!(*p2, p1.map(|p| p + step), "deck {n}");
        }
        assert!(pos1.iter().any(|(_, p)| p.is_some()), "a deck was mid-track");
        // a stopped engine round-trips with no allowance at all
        let mut c = fresh();
        let mut a2 = busy();
        a2.e.perform(&DeckAction::Play { deck: "A".into(), on: false }, PerformCtx::default());
        a2.pump(1);
        let s3 = capture(&a2.e, None);
        assert!(stand(&mut c.e, &check(&canonical(&s3), None).unwrap()).is_empty());
        c.pump(1);
        assert_eq!(canonical(&capture(&c.e, None)), canonical(&s3));
        assert_eq!(cid_of(&capture(&c.e, None)), cid_of(&s3));
    }

    #[test]
    fn captures_what_it_should() {
        let s = capture(&busy().e, None);
        let deck = |n: &str| s.get("decks").unwrap().get(n).unwrap();
        assert_eq!(deck("A").get("source").unwrap().get("kind").unwrap().as_str(), Some("net"));
        assert_eq!(deck("A").get("playing").unwrap().as_bool(), Some(true));
        assert_eq!(deck("B").get("playing").unwrap().as_bool(), Some(false));
        let pos = deck("A").get("position").expect("position").as_f64().unwrap();
        assert!(pos > 64.0, "{pos}");
        assert!(deck("B").get("position").is_none());
        let voc = s.get("setup").unwrap().get("vocoders").unwrap().as_array().unwrap();
        assert_eq!(voc.len(), 2, "{}", s.canonical_js());
        assert_eq!(voc[1].get("instance").unwrap().as_i64(), Some(1));
        assert_eq!(s.get("mixer").unwrap().get("crossfader").unwrap().as_f64(), Some(0.35));
        // the track's cid is the beats share cid of its payload
        let src = deck("A").get("source").unwrap();
        assert_eq!(src.get("cid").unwrap().as_str().unwrap(), cid::share_cid(src.get("share").unwrap()));
    }

    #[test]
    fn fresh_engine_snapshot_restores_to_itself() {
        let s = capture(&fresh().e, None);
        let mut b = fresh();
        let refused = stand(&mut b.e, &check(&canonical(&s), None).unwrap());
        assert!(refused.is_empty());
        assert_eq!(canonical(&capture(&b.e, None)), canonical(&s));
    }

    #[test]
    fn refuses_tampering() {
        let s = capture(&busy().e, None);
        let cid = cid_of(&s);
        let text = canonical(&s);
        let tampered = text.replacen("\"crossfader\":0.35", "\"crossfader\":0.36", 1);
        assert_ne!(tampered, text);
        assert!(matches!(check(&tampered, Some(&cid)), Err(SnapshotError::CidMismatch { .. })));
        // a track share edited without fixing its cid
        let bad_track = text.replacen("\"seed\":42", "\"seed\":41", 1);
        assert_ne!(bad_track, text);
        assert!(matches!(check(&bad_track, None), Err(SnapshotError::TrackCidMismatch { .. })));
        assert!(matches!(check("{}", None), Err(SnapshotError::NotASnapshot(_))));
    }
}
