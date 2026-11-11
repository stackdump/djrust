//! The dj-only `tracks` keys of a share: `picked` (set by `instrument`), `kit`, `palette` and
//! `voice` (`Project.applyShareOverrides`, Share.swift 170-215), and the empty-string-clears
//! rule at Share.swift 195-201.
//!
//! Two forms of proof, said in each test:
//! * (a) the Swift suite's own literals (`InstrumentPickTests`), or what the Swift source
//!   states (these keys have no JS oracle: beats never reads them);
//! * (b) `swift/shareoverrides.json`, written by `tools/paritydump/shareoverrides.swift`:
//!   the per-net result of replaying override steps on composed projects, compared field by
//!   field with what Swift's `applyShareOverrides` left.
//!
//! The consumer side (`TrackSpec::finish` reading these back into the deck) is not tested here.

mod common;
use common::*;
use djrust::json::{JsonObject, JsonValue};
use djrust::pflow::{NetBundle, Project};
use djrust::share::Share;
use djrust::TrackSpec;

fn ov(text: &str) -> JsonObject {
    JsonValue::parse(text)
        .unwrap()
        .as_object()
        .unwrap_or_else(|| panic!("{text}"))
        .clone()
}

/// A composed project and the channel of its melody net.
fn techno() -> (Project, i64) {
    let p = TrackSpec::new("techno", 42.0, Some("minimal")).project();
    let melody = p
        .nets
        .values()
        .find(|n| n.track.group.as_deref() == Some("melody"))
        .expect("a melody net")
        .track
        .channel;
    (p, melody)
}

/// The non-control nets on `channel`.
fn on(p: &Project, channel: i64) -> Vec<&NetBundle> {
    p.nets
        .values()
        .filter(|n| n.role != "control" && n.track.channel == channel)
        .collect()
}

/// (a) `InstrumentPickTests.aUsersInstrumentIsMarkedPickedAndTheCIDStaysBeats`, step for step.
#[test]
fn a_users_instrument_is_marked_picked_and_the_cid_stays_beats() {
    let (mut p, melody) = techno();
    let before = p.model_cid();
    assert!(
        p.nets.values().all(|n| !n.track.picked),
        "the composer's choices"
    );
    p.apply_share_overrides(&ov(&format!(
        r#"{{"tracks":{{"{melody}":{{"instrument":"wobble-lead"}}}}}}"#
    )));
    let picked = on(&p, melody);
    assert!(
        !picked.is_empty()
            && picked
                .iter()
                .all(|n| n.track.picked && n.track.instrument == "wobble-lead")
    );
    assert_eq!(
        p.model_cid(),
        before,
        "the model CID is the song's structure, not its instruments"
    );
    let mut q = p.clone();
    let ids: Vec<String> = q.nets.keys().to_vec();
    for id in ids {
        q.nets.get_mut(&id).unwrap().track.picked = false;
    }
    assert_eq!(
        q.model_cid(),
        p.model_cid(),
        "the mark itself is not in the CID"
    );
}

/// (a) Share.swift 190-196: `instrument` remembers the composer's choice once in
/// `sound.composed`; an empty or non-string one picks nothing.
#[test]
fn an_instrument_keeps_the_composers_choice_once_and_empty_ones_pick_nothing() {
    let (mut p, melody) = techno();
    let composed: Vec<String> = on(&p, melody)
        .iter()
        .map(|n| n.track.instrument.clone())
        .collect();
    assert!(composed
        .iter()
        .all(|i| i != "wobble-lead" && i != "chiptune"));
    for empty in [r#""""#, "5", "null", "true", r#"["a"]"#, "{}"] {
        p.apply_share_overrides(&ov(&format!(
            r#"{{"tracks":{{"{melody}":{{"instrument":{empty}}}}}}}"#
        )));
        for n in on(&p, melody) {
            assert!(!n.track.picked, "instrument {empty} picks nothing");
            assert_eq!(n.track.sound.composed, None, "instrument {empty}");
        }
    }
    p.apply_share_overrides(&ov(&format!(
        r#"{{"tracks":{{"{melody}":{{"instrument":"wobble-lead"}}}}}}"#
    )));
    p.apply_share_overrides(&ov(&format!(
        r#"{{"tracks":{{"{melody}":{{"instrument":"chiptune"}}}}}}"#
    )));
    for (n, was) in on(&p, melody).iter().zip(&composed) {
        assert_eq!(n.track.instrument, "chiptune");
        assert!(n.track.picked);
        assert_eq!(
            n.track.sound.composed.as_deref(),
            Some(was.as_str()),
            "the second pick must not overwrite what the composer chose"
        );
    }
}

/// (a) Share.swift 197-201: `kit`, `palette` and `voice` land in `Track.kit`,
/// `Track.palette` and `Track.sound.override` of every music net on the channel, and
/// nowhere else.
#[test]
fn kit_palette_and_voice_land_on_the_channels_nets() {
    let (mut p, melody) = techno();
    let before = p.model_cid();
    // (kit, palette, voice) of every net on another channel
    type Sound = (Option<String>, Option<String>, Option<String>);
    let others: Vec<(String, Sound)> = p
        .nets
        .iter()
        .filter(|(_, n)| n.track.channel != melody)
        .map(|(id, n)| {
            (
                id.to_string(),
                (
                    n.track.kit.clone(),
                    n.track.palette.clone(),
                    n.track.sound.override_.clone(),
                ),
            )
        })
        .collect();
    assert!(!others.is_empty());
    p.apply_share_overrides(&ov(&format!(
        r#"{{"tracks":{{"{melody}":{{"kit":"808","palette":"retro","voice":"Strings"}}}}}}"#
    )));
    let hit = on(&p, melody);
    assert!(!hit.is_empty());
    for n in &hit {
        assert_eq!(n.track.kit.as_deref(), Some("808"));
        assert_eq!(n.track.palette.as_deref(), Some("retro"));
        assert_eq!(n.track.sound.override_.as_deref(), Some("Strings"));
        // these keys are not a pick, and they do not touch the instrument
        assert!(!n.track.picked);
        assert_eq!(n.track.sound.composed, None);
    }
    for (id, (kit, pal, voice)) in others {
        let n = p.nets.get(&id).unwrap();
        assert_eq!(n.track.kit, kit, "{id}");
        assert_eq!(n.track.palette, pal, "{id}");
        assert_eq!(n.track.sound.override_, voice, "{id}");
    }
    assert_eq!(p.model_cid(), before, "sound choices are not in the CID");
}

/// (a) Share.swift 198-200: an empty string clears the key (`""` = none), only the one named.
#[test]
fn an_empty_string_clears_a_kit_a_palette_and_a_voice() {
    let (mut p, melody) = techno();
    let set = format!(
        r#"{{"tracks":{{"{melody}":{{"kit":"808","palette":"retro","voice":"Strings"}}}}}}"#
    );
    // each key alone, in turn
    for (key, get) in [
        (
            "kit",
            (|n: &NetBundle| n.track.kit.clone()) as fn(&NetBundle) -> Option<String>,
        ),
        ("palette", |n| n.track.palette.clone()),
        ("voice", |n| n.track.sound.override_.clone()),
    ] {
        p.apply_share_overrides(&ov(&set));
        p.apply_share_overrides(&ov(&format!(
            r#"{{"tracks":{{"{melody}":{{"{key}":""}}}}}}"#
        )));
        for n in on(&p, melody) {
            assert_eq!(get(n), None, "{key} cleared");
            let kept = [
                n.track.kit.is_some(),
                n.track.palette.is_some(),
                n.track.sound.override_.is_some(),
            ];
            assert_eq!(kept.iter().filter(|k| **k).count(), 2, "only {key} cleared");
        }
    }
    // all three at once
    p.apply_share_overrides(&ov(&set));
    p.apply_share_overrides(&ov(&format!(
        r#"{{"tracks":{{"{melody}":{{"kit":"","palette":"","voice":""}}}}}}"#
    )));
    for n in on(&p, melody) {
        assert_eq!(
            (&n.track.kit, &n.track.palette, &n.track.sound.override_),
            (&None, &None, &None)
        );
    }
}

/// (a) `o["kit"]?.string`: a value that is not a string (number, null, bool, array) is no
/// override, and leaves what was there.
#[test]
fn a_kit_a_palette_or_a_voice_that_is_not_a_string_changes_nothing() {
    let (mut p, melody) = techno();
    p.apply_share_overrides(&ov(&format!(
        r#"{{"tracks":{{"{melody}":{{"kit":"808","palette":"retro","voice":"Strings"}}}}}}"#
    )));
    for junk in ["5", "null", "true", "[]", r#"["x"]"#, "{}"] {
        p.apply_share_overrides(&ov(&format!(
            r#"{{"tracks":{{"{melody}":{{"kit":{junk},"palette":{junk},"voice":{junk}}}}}}}"#
        )));
        for n in on(&p, melody) {
            assert_eq!(n.track.kit.as_deref(), Some("808"), "{junk}");
            assert_eq!(n.track.palette.as_deref(), Some("retro"), "{junk}");
            assert_eq!(
                n.track.sound.override_.as_deref(),
                Some("Strings"),
                "{junk}"
            );
        }
    }
    // and a channel's value that is not an object at all
    for junk in ["5", "null", r#""kit""#, "[]"] {
        p.apply_share_overrides(&ov(&format!(r#"{{"tracks":{{"{melody}":{junk}}}}}"#)));
        for n in on(&p, melody) {
            assert_eq!(n.track.kit.as_deref(), Some("808"), "{junk}");
        }
    }
}

/// (a) `net.role != "control"` and the channel match: a control net on the same channel is
/// left alone; so is any other channel.
#[test]
fn control_nets_are_skipped() {
    let (mut p, melody) = techno();
    let mut ctl = p
        .nets
        .values()
        .find(|n| n.track.channel == melody && n.role != "control")
        .unwrap()
        .clone();
    ctl.role = "control".into();
    p.nets.insert("ctl-test", ctl);
    p.apply_share_overrides(&ov(&format!(
        r#"{{"tracks":{{"{melody}":{{"kit":"808","palette":"retro","voice":"V","instrument":"x"}}}}}}"#
    )));
    let c = p.nets.get("ctl-test").unwrap();
    assert_eq!((&c.track.kit, &c.track.palette), (&None, &None));
    assert_eq!(c.track.sound.override_, None);
    assert!(!c.track.picked);
    assert_ne!(c.track.instrument, "x");
}

/// (a) `parseIntPrefix`: the channel key is JS `parseInt` of the key.
#[test]
fn channel_keys_are_read_like_parse_int() {
    let (plain, melody) = techno();
    for (key, hits) in [
        (format!("{melody}"), true),
        (format!(" {melody}"), true),
        (format!("{melody}abc"), true),
        (format!("+{melody}"), true),
        (format!("0{melody}"), true),
        (format!("x{melody}"), false),
        (format!("-{melody}"), false),
        (String::new(), false),
    ] {
        let mut p = plain.clone();
        p.apply_share_overrides(&ov(&format!(r#"{{"tracks":{{"{key}":{{"kit":"K"}}}}}}"#)));
        let got = on(&p, melody)
            .iter()
            .any(|n| n.track.kit.as_deref() == Some("K"));
        assert_eq!(got, hits, "key {key:?}");
    }
}

/// (a) A share link carries these keys: `shareFromPayload` keeps `tracks` whole in the
/// overrides, and the composed project gets them (`Share.project`).
#[test]
fn a_share_envelope_carries_them_to_the_project() {
    let (_, melody) = techno();
    let base = r#""genre":"techno","seed":42,"structure":"minimal","cohesion":"v1""#;
    let plain = Share::from_payload(&JsonValue::parse(&format!("{{{base}}}")).unwrap(), None)
        .project()
        .unwrap();
    let payload = JsonValue::parse(&format!(
        r#"{{{base},"tracks":{{"{melody}":{{"instrument":"wobble-lead","kit":"808","palette":"retro","voice":"Strings"}}}}}}"#
    ))
    .unwrap();
    let share = Share::from_payload(&payload, None);
    let tracks = share.overrides.as_ref().unwrap().get("tracks").unwrap();
    assert_eq!(
        tracks.get(&melody.to_string()).unwrap().js_stringified(),
        r#"{"instrument":"wobble-lead","kit":"808","palette":"retro","voice":"Strings"}"#
    );
    let p = share.project().unwrap();
    let hit = on(&p, melody);
    assert!(!hit.is_empty());
    for n in hit {
        assert_eq!(n.track.instrument, "wobble-lead");
        assert!(n.track.picked);
        assert_eq!(n.track.kit.as_deref(), Some("808"));
        assert_eq!(n.track.palette.as_deref(), Some("retro"));
        assert_eq!(n.track.sound.override_.as_deref(), Some("Strings"));
    }
    assert_eq!(p.model_cid(), plain.model_cid(), "none of it is in the CID");
}

/// (b) Swift's `applyShareOverrides` replayed step by step on composed projects
/// (`tools/paritydump/shareoverrides.swift`): every net's sound fields at the start, then after
/// every step the rows of the nets it changed.
#[test]
fn share_overrides_match_swift() {
    let doc = fixture("shareoverrides");
    let mut d = Diffs::default();
    let cases = rows(&doc, "cases");
    assert!(cases.len() >= 4);
    let mut changed_nets = 0;
    for case in cases {
        let structure = case.get("structure").and_then(JsonValue::as_str);
        let tag = format!("{}:{}:{structure:?}", s(case, "genre"), f(case, "seed"));
        let mut p = TrackSpec::new(s(case, "genre"), f(case, "seed"), structure).project();
        let mut before = net_rows(&p);
        d.eq(
            format!("{tag} start"),
            before
                .iter()
                .map(|r| r.1.as_str())
                .collect::<Vec<_>>()
                .join(","),
            rows(case, "start")
                .iter()
                .map(JsonValue::js_stringified)
                .collect::<Vec<_>>()
                .join(","),
        );
        d.eq(
            format!("{tag} start cid"),
            p.model_cid(),
            s(case, "modelCID").to_string(),
        );
        for step in rows(case, "steps") {
            let text = s(step, "ov");
            let o = JsonValue::parse(text).unwrap();
            p.apply_share_overrides(o.as_object().unwrap());
            let stag = format!("{tag} {}: {text}", s(step, "name"));
            let after = net_rows(&p);
            assert_eq!(after.len(), before.len());
            let changed: Vec<&str> = before
                .iter()
                .zip(&after)
                .filter(|(b, a)| b.1 != a.1)
                .map(|(_, a)| a.1.as_str())
                .collect();
            changed_nets += changed.len();
            d.eq(
                format!("{stag} changed"),
                changed.join(","),
                rows(step, "changed")
                    .iter()
                    .map(JsonValue::js_stringified)
                    .collect::<Vec<_>>()
                    .join(","),
            );
            before = after;
            d.eq(
                format!("{stag} cid"),
                p.model_cid(),
                s(case, "modelCID").to_string(),
            );
            d.eq(
                format!("{stag} mutes"),
                JsonValue::Array(p.initial_mutes.iter().map(|m| m.as_str().into()).collect())
                    .js_stringified(),
                step.get("initialMutes").unwrap().js_stringified(),
            );
        }
    }
    assert!(
        changed_nets > 40,
        "the steps must change something: {changed_nets}"
    );
    d.finish("share overrides");
}

/// Every net's fields the overrides can touch, in net order: (id, the dump's row as text).
fn net_rows(p: &Project) -> Vec<(String, String)> {
    let opt = |o: &Option<String>| o.as_deref().map_or(JsonValue::Null, JsonValue::from);
    p.nets
        .iter()
        .map(|(id, n)| {
            let t = &n.track;
            let row = JsonValue::Object(JsonObject::from_pairs([
                ("id", JsonValue::from(id)),
                ("role", JsonValue::from(n.role.as_str())),
                ("channel", JsonValue::int(t.channel)),
                ("instrument", JsonValue::from(t.instrument.as_str())),
                ("picked", JsonValue::Bool(t.picked)),
                ("kit", opt(&t.kit)),
                ("palette", opt(&t.palette)),
                ("voice", opt(&t.sound.override_)),
                ("composed", opt(&t.sound.composed)),
                ("mix", t.mix.clone().unwrap_or(JsonValue::Null)),
                ("instrumentSet", JsonValue::Array(t.instrument_set.clone())),
            ]));
            (id.to_string(), row.js_stringified())
        })
        .collect()
}
