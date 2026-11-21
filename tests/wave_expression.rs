//! How a note is played beyond pitch, length and velocity (`NoteExpr`): a roll note's expression
//! object becomes its binding's keys (only those set), and the runner plays them: a ratchet's
//! strikes inside the step, a flam's grace hit, a glide's bend from the note before, an 808's
//! pitch drop and its decay.

use djrust::json::JsonValue;
use djrust::pflow::{NoteExpr, FLAM_MS};
use djrust::wave::*;
use djrust::Project;

const SR: f64 = 48_000.0;
const TEMPO: f64 = 120.0; // a sixteenth is 125 ms

fn envelope(part: &str, notes: &str) -> JsonValue {
    let params = format!(r#"{{"tempo":{TEMPO},"steps":16,"parts":{{"{part}":{{"notes":{notes}}}}}}}"#);
    djrust::roll::envelope(&JsonValue::parse(&params).unwrap()).unwrap()
}

fn midi(env: &JsonValue, net: &str, t: usize) -> JsonValue {
    env.get("nets").and_then(|n| n.get(net)).and_then(|n| n.get("transitions")).and_then(|ts| ts.get(&format!("t{t}"))).and_then(|t| t.get("midi")).cloned().expect("a binding")
}

fn render(env: &JsonValue, seconds: f64) -> (Vec<f32>, Vec<HitRecord>) {
    let p = Project::from_json(env);
    let mut r = WaveRunner::new(SR, WaveOptions { record_hits: true, ..Default::default() });
    r.load(&p);
    r.play();
    let frames = (seconds * SR) as usize;
    let mut out = vec![0f32; frames];
    let mut off = 0;
    while off < frames {
        let n = 256.min(frames - off);
        r.process_at(&mut out[off..off + n], None, n, off as i64);
        off += n;
    }
    (out, std::mem::take(&mut r.hits))
}

/// Upward zero crossings a second, over `x[a..b]`.
fn hz(x: &[f32], a: usize, b: usize) -> f64 {
    let ups = (a + 1..b).filter(|&i| x[i - 1] <= 0.0 && x[i] > 0.0).count();
    ups as f64 / ((b - a) as f64 / SR)
}

fn rms(x: &[f32], a: usize, b: usize) -> f64 {
    (x[a..b].iter().map(|v| f64::from(*v) * f64::from(*v)).sum::<f64>() / (b - a) as f64).sqrt()
}

#[test]
fn a_plain_note_keeps_its_binding_and_an_expressive_one_says_only_what_is_set() {
    let plain = envelope("hihat", "[[0,42,1,100]]");
    let m = midi(&plain, "hihat", 0);
    assert_eq!(m.as_object().unwrap().keys().collect::<Vec<_>>(), ["note", "channel", "velocity", "durationSteps"]);
    // an empty expression object is the plain note, and so is the same note in object form
    assert_eq!(envelope("hihat", "[[0,42,1,100,{}]]"), plain);
    assert_eq!(envelope("hihat", r#"[{"step":0,"note":42,"dur":1,"vel":100}]"#), plain);
    let ex = envelope("hihat", r#"[[0,42,1,100,{"ratchet":3,"flam":true}]]"#);
    let m = midi(&ex, "hihat", 0);
    assert_eq!(m.get("ratchet").and_then(JsonValue::as_i64), Some(3));
    assert_eq!(m.get("flam").and_then(JsonValue::as_bool), Some(true));
    assert!(m.get("glide").is_none() && m.get("decay").is_none() && m.get("pitchDrop").is_none());
    let bass = envelope("bass", r#"[[0,36,2,100,{"glide":80,"decay":900,"pitchDrop":12}]]"#);
    let m = midi(&bass, "bass", 0);
    assert_eq!((m.get("glide").and_then(JsonValue::as_i64), m.get("decay").and_then(JsonValue::as_i64), m.get("pitchDrop").and_then(JsonValue::as_i64)), (Some(80), Some(900), Some(12)));
    // the binding reads back as written, and a ratchet of 1 is none
    let e = NoteExpr::from_json(&m);
    assert_eq!((e.glide_ms, e.decay_ms, e.pitch_drop, e.ratchet, e.flam), (80, 900, 12, 0, false));
    assert_eq!(NoteExpr::from_json(&JsonValue::parse(r#"{"ratchet":1}"#).unwrap()), NoteExpr::default());
    // the syllable still rides in the fifth item, alone or in the object
    let sung = envelope("melody", r#"[[0,60,2,100,{"syl":"da","glide":50}]]"#);
    let m = midi(&sung, "melody", 0);
    assert_eq!((m.get("vowel").and_then(JsonValue::as_str), m.get("glide").and_then(JsonValue::as_i64)), (Some("da"), Some(50)));
}

#[test]
fn a_ratchet_strikes_its_step_evenly() {
    let (_, hits) = render(&envelope("hihat", r#"[[0,42,1,100,{"ratchet":3}]]"#), 0.4);
    let first: Vec<&HitRecord> = hits.iter().filter(|h| h.tick == hits[0].tick).collect();
    assert_eq!(first.len(), 3, "three strikes in the step: {hits:?}");
    let step = 60.0 / TEMPO / 4.0 * SR;
    for (k, h) in first.iter().enumerate() {
        let off = (h.frame - first[0].frame) as f64;
        assert!((off - k as f64 * step / 3.0).abs() <= 256.0 + 1.0, "strike {k} at {off} frames, want {}", k as f64 * step / 3.0);
        assert_eq!(h.velocity, 100);
    }
    let (_, plain) = render(&envelope("hihat", "[[0,42,1,100]]"), 0.4);
    assert_eq!(plain.iter().filter(|h| h.tick == plain[0].tick).count(), 1);
}

#[test]
fn a_flam_is_a_soft_grace_hit_then_the_note() {
    let (_, hits) = render(&envelope("snare", r#"[[0,38,1,100,{"flam":true}]]"#), 0.3);
    let first: Vec<&HitRecord> = hits.iter().filter(|h| h.tick == hits[0].tick).collect();
    assert_eq!(first.len(), 2, "{hits:?}");
    assert_eq!((first[0].velocity, first[1].velocity), (45, 100));
    let gap = (first[1].frame - first[0].frame) as f64 / SR * 1000.0;
    assert!((gap - FLAM_MS).abs() < 256.0 / SR * 1000.0 + 0.1, "the note {gap} ms after its grace hit");
}

#[test]
fn a_glide_bends_from_the_note_before_without_striking_again() {
    // c3 held two steps, then c4 gliding in over 100 ms
    let env = envelope("bass", r#"[[0,48,3,100],[2,60,4,100,{"glide":100}]]"#);
    let (x, _) = render(&env, 0.8);
    let at = |ms: f64| (ms / 1000.0 * SR) as usize;
    let step = 125.0;
    let (lo, hi) = (130.81, 261.63);
    let before = hz(&x, at(step), at(2.0 * step));
    let early = hz(&x, at(2.0 * step) + 10, at(2.0 * step + 25.0));
    let late = hz(&x, at(2.0 * step + 150.0), at(2.0 * step + 300.0));
    assert!((before - lo).abs() < 15.0, "the first note at {before} Hz");
    assert!(early > lo * 0.9 && early < hi * 0.85, "the glide's start {early} Hz is between the notes");
    assert!((late - hi).abs() < 20.0, "the glide arrives: {late} Hz");
    // without the glide the second note is at its pitch at once
    let plain = envelope("bass", "[[0,48,3,100],[2,60,4,100]]");
    let (y, _) = render(&plain, 0.8);
    let early_plain = hz(&y, at(2.0 * step) + 10, at(2.0 * step + 25.0));
    assert!(early_plain > early, "plain {early_plain} Hz vs gliding {early} Hz");
}

#[test]
fn a_pitch_drop_starts_high_and_falls_onto_the_note() {
    let env = envelope("bass", r#"[[0,48,8,100,{"pitchDrop":12}]]"#);
    let (x, _) = render(&env, 0.6);
    let at = |ms: f64| (ms / 1000.0 * SR) as usize;
    let first = hz(&x, at(1.0), at(12.0));
    let settled = hz(&x, at(120.0), at(300.0));
    assert!(first > settled * 1.25, "starts high ({first} Hz) and falls to {settled} Hz");
    assert!((settled - 130.81).abs() < 15.0, "{settled}");
}

#[test]
fn a_decay_rings_on_past_the_notes_length() {
    let short = envelope("bass", "[[0,36,1,100]]");
    let long = envelope("bass", r#"[[0,36,1,100,{"decay":1500}]]"#);
    let (a, _) = render(&short, 1.2);
    let (b, _) = render(&long, 1.2);
    let at = |ms: f64| (ms / 1000.0 * SR) as usize;
    let (ra, rb) = (rms(&a, at(600.0), at(900.0)), rms(&b, at(600.0), at(900.0)));
    assert!(rb > ra * 4.0 && rb > 1e-3, "the decay rings on: {rb} vs {ra}");
}
