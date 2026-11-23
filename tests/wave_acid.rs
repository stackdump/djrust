//! The acid voice (`wave::acid`) as a net plays it: the app's `acid` (and `acid-square`) is the
//! 303-style voice, beats' (the parity path) its plain saw bass; an accent rides in the velocity
//! and makes a note louder and brighter, accents in a row build, a note's `glide` slides into it
//! from the note before without striking again, a note that does not slide lets the one before
//! go, a note a slide leaves holds its gate for the glide (round the ring too), `decay` is the
//! filter's sweep and the note's length gates its level. Deterministic, the same at any rate,
//! under full scale at its own level, and no harder on the deck's safety limiter than the
//! wavetable bass.

use djrust::json::JsonValue;
use djrust::wave::*;
use djrust::Project;

const SR: f64 = 48_000.0;
const TEMPO: f64 = 120.0; // a sixteenth is 125 ms
const STEP: f64 = 0.125;

fn envelope(inst: &str, notes: &str) -> JsonValue {
    let params = format!(r#"{{"tempo":{TEMPO},"steps":16,"parts":{{"bass":{{"instrument":"{inst}","notes":{notes}}}}}}}"#);
    djrust::roll::envelope(&JsonValue::parse(&params).unwrap()).unwrap()
}

/// beats' engine with the instrument table and nothing else: the voice as it is, summed plainly.
fn table_opts() -> WaveOptions {
    WaveOptions { instruments: Some(InstrumentTable::shared()), master: Master::Linear, ..Default::default() }
}

fn render_with(env: &JsonValue, seconds: f64, sr: f64, opts: WaveOptions) -> Vec<f32> {
    let p = Project::from_json(env);
    let mut r = WaveRunner::new(sr, opts);
    r.load(&p);
    r.play();
    let frames = (seconds * sr) as usize;
    let mut out = vec![0f32; frames];
    let mut off = 0;
    while off < frames {
        let n = 256.min(frames - off);
        r.process_at(&mut out[off..off + n], None, n, off as i64);
        off += n;
    }
    out
}

fn render(env: &JsonValue, seconds: f64) -> Vec<f32> {
    render_with(env, seconds, SR, table_opts())
}

fn at(s: f64) -> usize {
    (s * SR) as usize
}

fn rms(x: &[f32]) -> f64 {
    (x.iter().map(|v| f64::from(*v) * f64::from(*v)).sum::<f64>() / x.len().max(1) as f64).sqrt()
}

fn db(a: f64, b: f64) -> f64 {
    20.0 * (a / b).log10()
}

/// How bright: the first difference's RMS over the signal's (rises with the energy up top).
fn bright(x: &[f32]) -> f64 {
    let d: Vec<f32> = x.windows(2).map(|w| w[1] - w[0]).collect();
    rms(&d) / rms(x).max(1e-12)
}

/// The fundamental over `x`, between `fmin` and `fmax` Hz (the first autocorrelation peak near
/// the best in that range, so a resonant harmonic is not taken for it).
fn pitch(x: &[f32], fmin: f64, fmax: f64) -> f64 {
    let v: Vec<f64> = x.iter().map(|&a| f64::from(a)).collect();
    let (lo, hi) = ((SR / fmax) as usize, ((SR / fmin) as usize).min(v.len() / 2));
    let r = |lag: usize| -> f64 {
        let (mut s, mut a, mut b) = (0.0, 0.0, 0.0);
        for i in 0..v.len() - lag {
            s += v[i] * v[i + lag];
            a += v[i] * v[i];
            b += v[i + lag] * v[i + lag];
        }
        s / (a * b).sqrt().max(1e-30)
    };
    let rs: Vec<f64> = (lo..=hi).map(r).collect();
    let best = rs.iter().cloned().fold(f64::MIN, f64::max);
    let mut k = rs.iter().position(|&x| x > 0.92 * best).unwrap();
    while k + 1 < rs.len() && rs[k + 1] > rs[k] {
        k += 1;
    }
    SR / (lo + k) as f64
}

fn hz(note: i64) -> f64 {
    440.0 * 2f64.powf((note - 69) as f64 / 12.0)
}

#[test]
fn the_app_plays_acid_as_the_303_voice_and_beats_as_its_saw() {
    for inst in ["acid", "acid-square"] {
        let p = Project::from_json(&envelope(inst, "[[0,36,1,100]]"));
        let app = WaveRunner::prepare(&p, SR, &table_opts(), 0);
        let beats = WaveRunner::prepare(&p, SR, &WaveOptions::default(), 0);
        assert!(app.lanes().iter().any(Lane::is_acid), "{inst}: the app's lane is the acid voice");
        assert!(!beats.lanes().iter().any(Lane::is_acid), "{inst}: beats' is its saw");
        let ext = WaveRunner::prepare(&p, SR, &WaveOptions::extended(), 0);
        assert!(ext.lanes().iter().any(Lane::is_acid), "{inst}: and so is everything the app plays");
    }
    let lane = |inst: &str| {
        let p = Project::from_json(&envelope(inst, "[[0,36,1,100]]"));
        let w = WaveRunner::prepare(&p, SR, &table_opts(), 0);
        let l = w.lanes().iter().find(|l| l.is_acid()).unwrap();
        l.acid().unwrap().square
    };
    assert!(!lane("acid") && lane("acid-square"));
    // a saw bass that is not acid stays the wavetable voice
    let p = Project::from_json(&envelope("bass", "[[0,36,1,100]]"));
    assert!(!WaveRunner::prepare(&p, SR, &table_opts(), 0).lanes().iter().any(Lane::is_acid));
}

#[test]
fn a_glide_slides_into_the_note_without_striking_again() {
    // c3 held into the next step, then c4 gliding in (what BeatScript writes for `c3^ c4` on this voice)
    let slid = render(&envelope("acid", r#"[[0,48,2,100],[1,60,1,100,{"glide":60}]]"#), 0.5);
    let struck = render(&envelope("acid", "[[0,48,1,100],[1,60,1,100]]"), 0.5);
    let (lo, hi) = (hz(48), hz(60));
    let p = |x: &[f32]| pitch(x, 90.0, 330.0);
    assert!((p(&slid[at(0.02)..at(0.10)]) - lo).abs() < lo * 0.03, "the first note at its pitch");
    let early = p(&slid[at(STEP + 0.004)..at(STEP + 0.03)]);
    assert!(early > lo * 1.05 && early < hi * 0.95, "the slide's start is between the notes: {early} Hz");
    let late = p(&slid[at(STEP + 0.08)..at(STEP + 0.11)]);
    assert!((late - hi).abs() < hi * 0.03, "it arrives: {late} Hz");
    let jump = p(&struck[at(STEP + 0.004)..at(STEP + 0.03)]);
    assert!((jump - hi).abs() < hi * 0.03, "a note let go first: the next is struck at its pitch, {jump} Hz");
    // not struck again: the slid note carries on darker (its sweep is the first note's, falling),
    // where a struck one opens the filter again
    let b_slid = bright(&slid[at(STEP + 0.01)..at(STEP + 0.05)]);
    let b_struck = bright(&struck[at(STEP + 0.01)..at(STEP + 0.05)]);
    assert!(b_struck > b_slid * 1.1, "struck {b_struck} vs slid {b_slid}");
}

#[test]
fn one_note_at_a_time() {
    // two-step notes a step apart (a tonal part's notes as written): each lets the one before go,
    // so the second step sounds the second note alone
    let x = render(&envelope("acid", "[[0,48,2,100],[1,55,2,100]]"), 0.5);
    let f = pitch(&x[at(STEP + 0.03)..at(STEP + 0.1)], 90.0, 330.0);
    assert!((f - hz(55)).abs() < hz(55) * 0.03, "the second note alone: {f} Hz");
    // a chord's notes sound together (the roll spills the second onto a net of its own, `bass-b`)
    let p = Project::from_json(&envelope("acid", "[[0,48,2,100],[0,55,2,100]]"));
    let mut r = WaveRunner::new(SR, table_opts());
    r.load(&p);
    r.play();
    let mut out = vec![0f32; 2400];
    r.process_at(&mut out, None, 2400, 0);
    let lanes = r.project().unwrap().lanes();
    let sounding: usize = lanes.iter().filter_map(Lane::acid).map(|a| a.voices.iter().filter(|v| v.gated()).count()).sum();
    assert_eq!(sounding, 2, "both notes of the chord");
}

#[test]
fn glide_sets_the_slide_time() {
    let p = |x: &[f32]| pitch(x, 90.0, 330.0);
    let quick = render(&envelope("acid", r#"[[0,48,2,100],[1,60,1,100,{"glide":60}]]"#), 0.5);
    let slow = render(&envelope("acid", r#"[[0,48,2,100],[1,60,1,100,{"glide":300}]]"#), 0.5);
    let q = p(&quick[at(STEP + 0.06)..at(STEP + 0.09)]);
    let s = p(&slow[at(STEP + 0.06)..at(STEP + 0.09)]);
    assert!(q > s * 1.15, "60 ms is there ({q} Hz) while 300 ms is on its way ({s} Hz)");
    // a glide on a note whose voice still sounds slides even when it was let go (not held into it)
    let let_go = render(&envelope("acid", r#"[[0,48,1,100],[1,60,1,100,{"glide":120}]]"#), 0.5);
    let e = p(&let_go[at(STEP + 0.004)..at(STEP + 0.03)]);
    assert!(e < hz(60) * 0.95, "it starts below the note: {e} Hz");
}

#[test]
fn an_accent_is_louder_and_brighter_and_accents_in_a_row_build() {
    let plain = render(&envelope("acid", "[[0,36,1,100]]"), 0.3);
    let accented = render(&envelope("acid", "[[0,36,1,127]]"), 0.3);
    let (p, a) = (&plain[at(0.005)..at(0.08)], &accented[at(0.005)..at(0.08)]);
    assert!(db(rms(a), rms(p)) > 3.0, "louder: {} dB", db(rms(a), rms(p)));
    assert!(bright(a) > bright(p) * 1.1, "brighter: {} vs {}", bright(a), bright(p));
    // four accents in a row: the sweep builds, so the fourth opens further than the first
    let row = render(&envelope("acid", "[[0,36,1,127],[1,36,1,127],[2,36,1,127],[3,36,1,127]]"), 0.6);
    let first = bright(&row[at(0.01)..at(0.05)]);
    let fourth = bright(&row[at(3.0 * STEP + 0.01)..at(3.0 * STEP + 0.05)]);
    assert!(fourth > first * 1.05, "the fourth {fourth} vs the first {first}");
    // and without accents nothing builds
    let flat = render(&envelope("acid", "[[0,36,1,100],[1,36,1,100],[2,36,1,100],[3,36,1,100]]"), 0.6);
    let (f1, f4) = (bright(&flat[at(0.01)..at(0.05)]), bright(&flat[at(3.0 * STEP + 0.01)..at(3.0 * STEP + 0.05)]));
    assert!((f4 / f1 - 1.0).abs() < 0.05, "unaccented notes are alike: {f1} {f4}");
}

#[test]
fn decay_is_the_filter_sweep_and_the_length_gates_the_level() {
    let short = render(&envelope("acid", "[[0,36,4,100,{\"decay\":60}]]"), 1.0);
    let long = render(&envelope("acid", "[[0,36,4,100,{\"decay\":1500}]]"), 1.0);
    // late in the note the long sweep is still open
    let (bs, bl) = (bright(&short[at(0.3)..at(0.45)]), bright(&long[at(0.3)..at(0.45)]));
    assert!(bl > bs * 1.2, "long {bl} vs short {bs}");
    // and the note stops with its length (4 steps, 0.5 s) whatever its decay
    for x in [&short, &long] {
        assert!(rms(&x[at(0.1)..at(0.4)]) > 1e-3);
        assert!(rms(&x[at(0.56)..at(0.9)]) < 1e-5, "gated: {}", rms(&x[at(0.56)..at(0.9)]));
    }
}

#[test]
fn deterministic_and_the_same_at_any_rate() {
    let line = "[[0,36,2,127],[1,48,1,100],[2,39,1,100],[3,36,1,127,{\"glide\":90}],[4,43,2,100]]";
    let env = envelope("acid", line);
    assert!(render(&env, 0.8) == render(&env, 0.8));
    let base = render_with(&env, 0.8, 48_000.0, table_opts());
    for sr in [24_000.0, 44_100.0, 96_000.0] {
        let x = render_with(&env, 0.8, sr, table_opts());
        let d = db(rms(&x), rms(&base));
        assert!(d.abs() < 1.0, "{sr} Hz: {d} dB from 48 kHz");
    }
}

#[test]
fn under_full_scale_at_its_own_level() {
    // a busy line, every note accented, sliding and struck, at the top of its knobs' range of
    // notes: the voice's own level keeps it well under full scale, with nothing on the master
    let mut notes = Vec::new();
    for i in 0..16 {
        let n = 28 + (i * 7) % 24;
        let dur = if i % 3 == 0 { 2 } else { 1 };
        notes.push(format!("[{},{},{},127]", i, n, dur));
    }
    for inst in ["acid", "acid-square"] {
        let x = render(&envelope(inst, &format!("[{}]", notes.join(","))), 4.2);
        let peak = x.iter().fold(0f32, |m, v| m.max(v.abs()));
        assert!(x.iter().all(|v| v.is_finite()));
        assert!(peak < 0.9, "{inst}: peak {peak}");
        assert!(peak > 0.05, "{inst}: it plays ({peak})");
    }
}

fn envelope_at(tempo: f64, inst: &str, notes: &str) -> JsonValue {
    let params = format!(r#"{{"tempo":{tempo},"steps":16,"parts":{{"bass":{{"instrument":"{inst}","notes":{notes}}}}}}}"#);
    djrust::roll::envelope(&JsonValue::parse(&params).unwrap()).unwrap()
}

#[test]
fn a_slide_off_the_last_step_carries_into_the_downbeat() {
    // `bass a1 ... e1^` with the acid voice: the glide lands on step 0, and the note on step 15 says
    // `slide` (the ring cuts its length at the bar line: the gate must not)
    let bar = 16.0 * STEP;
    let dip = |notes: &str| {
        let x = render(&envelope("acid", notes), 2.0 * bar + 0.2);
        let tail = rms(&x[at(bar - 0.008)..at(bar)]);
        let body = rms(&x[at(bar - 0.06)..at(bar - 0.04)]);
        db(body, tail)
    };
    let slid = dip(r#"[[0,45,1,100,{"glide":60}],[15,40,1,100,{"slide":true}]]"#);
    let cut = dip(r#"[[0,45,1,100,{"glide":60}],[15,40,1,100]]"#);
    assert!(slid < 3.0, "legato into the bar line: {slid} dB down");
    assert!(cut > 12.0, "without the slide the note is let go first: {cut} dB down");
    // and the downbeat glides on from it
    let x = render(&envelope("acid", r#"[[0,57,1,100,{"glide":60}],[15,45,1,100,{"slide":true}]]"#), 2.0 * bar + 0.2);
    let early = pitch(&x[at(bar + 0.004)..at(bar + 0.03)], 90.0, 330.0);
    assert!(early > hz(45) * 1.05 && early < hz(57) * 0.95, "gliding up from e1: {early} Hz");
}

/// The app's render of a part (`WaveOptions::extended`: the loudness gain, the limiting master)
/// through the deck's safety limiter: the share of samples it works on, its most gain reduction
/// (dB), and the DC it leaves (the mean over the RMS).
fn limited(inst: &str, tempo: f64, notes: &str) -> (f64, f64, f64) {
    let env = envelope_at(tempo, inst, notes);
    let mut x = render_with(&env, 4.0 * 16.0 * 60.0 / tempo / 4.0, SR, WaveOptions::extended());
    let mut lim = djrust::fx::PeakLimiter::new(SR);
    for c in x.chunks_mut(256) {
        let k = c.len();
        lim.process(c, None, k);
    }
    let st = lim.statistics(&[0.5]);
    let mean = x.iter().map(|&v| f64::from(v)).sum::<f64>() / x.len() as f64;
    (st.limited_share, st.max_db, mean / rms(&x))
}

#[test]
fn no_harder_on_the_limiter_than_the_bass() {
    // the saw's reset rings the ladder into one-sided spikes; the output stage keeps them as tame
    // as the wavetable bass's peaks, so the limiter does not ride (and bias) an acid line
    let lines = [
        // plain sixteenths, no accent
        (126.0, r#"[[0,33,2,100],[1,33,2,100],[2,45,2,100],[3,33,2,100],[4,36,2,100],[5,33,2,100],[6,43,2,100],[7,33,2,100],[8,33,2,100],[9,45,2,100],[10,33,2,100],[11,40,2,100],[12,38,2,100],[13,33,2,100],[14,36,2,100],[15,33,2,100]]"#),
        // a busy line: accents in a row, slides, a high accented note, a long sweep
        (132.0, r#"[[0,33,2,127],[1,33,2,127],[2,33,2,127],[3,33,2,127],[4,45,2,100],[5,57,1,127,{"glide":60}],[6,45,2,100],[7,96,1,127],[8,33,8,100,{"decay":10000}],[12,40,2,100,{"glide":60}],[13,45,2,100],[15,57,2,127]]"#),
    ];
    for (tempo, notes) in lines {
        let (bass_share, bass_db, _) = limited("bass", tempo, notes);
        for inst in ["acid", "acid-square"] {
            let (share, gr, dc) = limited(inst, tempo, notes);
            assert!(share <= bass_share, "{inst}: limited on {share} of samples, the bass on {bass_share}");
            assert!(gr <= bass_db, "{inst}: {gr} dB of reduction at most, the bass {bass_db}");
            assert!(dc.abs() < 0.01, "{inst}: DC {dc} of the RMS");
        }
    }
}
