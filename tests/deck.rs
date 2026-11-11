//! The deck mixer against what Swift's `WaveDeckSourceTests`, `DeckNetTests` and
//! `SurfaceNetTests` hold the net deck to, offline (no device): the render order, the
//! crossfader law, role mutes, section jumps, macros, the headphone cue, the limiter, and
//! the controller path through `djrust::control`.

use djrust::fx::{MasterFxKey, MasterMacro};
use djrust::wave::{WaveOptions, WaveRunner};
use djrust::TrackSpec;
use djrust::host::deck::{crossfade_gain, engine, trim_for_gain, Engine, EngineConfig, Renderer};

const SR: f64 = 48_000.0;
const BLOCK: usize = 512;

fn config(extended: bool) -> EngineConfig {
    let mut c = EngineConfig::new(SR);
    c.extended = extended;
    c
}

fn spec(genre: &str, seed: f64, structure: &str) -> TrackSpec {
    TrackSpec::new(genre, seed, Some(structure))
}

/// Render `blocks` blocks of `BLOCK` frames, collecting the master.
fn render(r: &mut Renderer, blocks: usize) -> (Vec<f32>, Vec<f32>) {
    let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    let (mut ol, mut or) = (Vec::new(), Vec::new());
    for _ in 0..blocks {
        r.render_block(&mut l, &mut rr);
        ol.extend_from_slice(&l);
        or.extend_from_slice(&rr);
    }
    (ol, or)
}

fn rms(x: &[f32]) -> f64 {
    (x.iter().map(|v| (*v as f64) * (*v as f64)).sum::<f64>() / x.len().max(1) as f64).sqrt()
}

/// A mixer with A loaded, volume 1, crossfader hard left (A's gain is exactly 1) and the
/// first silent block rendered, so the strip's gain ramp has settled.
fn settled(extended: bool, s: &TrackSpec) -> (Engine, Renderer) {
    let (mut e, mut r) = engine(config(extended));
    e.set_crossfader(0.0);
    e.set_volume("A", 1.0);
    assert!(e.generate("A", s));
    render(&mut r, 1);
    (e, r)
}

#[test]
fn the_mixer_gain_laws_are_djengines() {
    assert_eq!(crossfade_gain("A", 0.0), 1.0);
    assert!(crossfade_gain("A", 1.0).abs() < 1e-15);
    assert!(crossfade_gain("B", 0.0).abs() < 1e-15);
    assert_eq!(crossfade_gain("B", 1.0), 1.0);
    // equal power at the centre
    let (a, b) = (crossfade_gain("A", 0.5), crossfade_gain("B", 0.5));
    assert!((a * a + b * b - 1.0).abs() < 1e-12);
    assert_eq!(crossfade_gain("L", 0.3), 1.0);
    // the gain knob's trim: 2^((gain - 0.5) * 2): unity at the middle, x2 / x0.5 at the ends
    assert_eq!(trim_for_gain(0.5), 1.0);
    assert!((trim_for_gain(1.0) - 2.0).abs() < 1e-12);
    assert!((trim_for_gain(0.0) - 0.5).abs() < 1e-12);
}

/// With beats' options and a neutral chain, the deck is the runner's exact bytes
/// (`WaveDeckSource`: "a neutral chain is bypassed and bit-transparent").
#[test]
fn a_neutral_deck_renders_the_runners_exact_bytes() {
    let s = spec("techno", 42.0, "minimal");
    let (mut e, mut r) = settled(false, &s);
    e.set_tempo("A", 240.0);
    e.play("A");
    let (l, rr) = render(&mut r, 120);

    let mut runner = WaveRunner::new(SR, WaveOptions::default());
    runner.load(&s.project());
    runner.set_tempo(240.0);
    runner.play();
    let (mut want_l, mut want_r) = (vec![0f32; 120 * BLOCK], vec![0f32; 120 * BLOCK]);
    // the deck rendered one silent block before play; the runner starts from the same frame
    for b in 0..120 {
        let (a, z) = (b * BLOCK, (b + 1) * BLOCK);
        runner.process_at(
            &mut want_l[a..z],
            Some(&mut want_r[a..z]),
            BLOCK,
            ((b + 1) * BLOCK) as i64,
        );
    }
    assert!(rms(&l) > 0.01, "the deck is playing");
    assert_eq!(l, want_l, "left");
    assert_eq!(rr, want_r, "right");
}

#[test]
fn the_same_inputs_render_the_same_bytes() {
    let go = || {
        let s = spec("synthwave", 1984.0, "standard");
        let (mut e, mut r) = settled(true, &s);
        e.play("A");
        let a = render(&mut r, 60);
        e.fire_master("A", MasterMacro::SweepLp, Some(1));
        e.set_crossfader(0.25);
        let b = render(&mut r, 60);
        (a, b)
    };
    assert!(go() == go());
}

#[test]
fn the_crossfader_places_the_decks() {
    let s = spec("techno", 7.0, "minimal");
    let (mut e, mut r) = engine(config(false));
    e.set_volume("A", 1.0);
    e.set_volume("B", 1.0);
    e.generate("A", &s);
    e.generate("B", &s);
    e.play("A");
    e.play("B");
    // hard left: A only
    e.set_crossfader(0.0);
    render(&mut r, 2);
    let (a_only, _) = render(&mut r, 20);
    // hard right: B only (the same song, the same clock: B plays what A played)
    e.set_crossfader(1.0);
    render(&mut r, 2);
    let (b_only, _) = render(&mut r, 20);
    assert!(rms(&a_only) > 0.01 && rms(&b_only) > 0.01);
    // centred: both at 1/sqrt(2), the two copies in step
    e.set_crossfader(0.5);
    render(&mut r, 2);
    let (both, _) = render(&mut r, 20);
    assert!(
        rms(&both) > rms(&a_only) * 0.9,
        "equal power keeps the level up"
    );
}

#[test]
fn roles_toggle_and_the_mirror_follows() {
    let s = spec("techno", 42.0, "standard");
    let (mut e, mut r) = settled(false, &s);
    let roles = e.roles("A");
    assert_eq!(roles[..2], ["kick".to_string(), "snare".to_string()]);
    // a song starts with some roles held out (`initialMutes`): pick one that sounds
    let on = roles
        .iter()
        .find(|x| e.is_role_audible("A", x))
        .cloned()
        .expect("a role plays");
    let off = roles
        .iter()
        .find(|x| *x != &on && e.is_role_audible("A", x))
        .cloned()
        .unwrap();
    e.set_role_muted("A", &on, true);
    render(&mut r, 1);
    assert!(!e.is_role_audible("A", &on));
    assert!(e.is_role_audible("A", &off));
    e.toggle_role("A", &on); // back on
    render(&mut r, 1);
    assert!(e.is_role_audible("A", &on));
    // a disabled role stays off, and the next project leaves it out
    e.set_role_disabled("A", &off, true);
    render(&mut r, 1);
    assert!(!e.is_role_audible("A", &off));
    assert!(!e.toggle_role("A", &off));
}

#[test]
fn a_section_jump_lands_at_the_next_bar() {
    let s = spec("techno", 42.0, "standard");
    let (mut e, mut r) = settled(false, &s);
    e.set_tempo("A", 480.0);
    e.play("A");
    render(&mut r, 20);
    let secs = e.deck("A").unwrap().sections.clone();
    assert!(secs.len() >= 3, "a standard song has sections: {secs:?}");
    assert_eq!(secs[0].start_tick, 0);
    assert!(e.jump_to_section("A", 2));
    // a bar is 16 ticks = half a second at 480 BPM; two seconds is plenty
    render(&mut r, 200);
    assert_eq!(
        e.current_section("A"),
        Some(2),
        "tick {}",
        e.deck("A").unwrap().mirror.tick()
    );
    assert!(e.jump_to_section("A", 1));
    render(&mut r, 200);
    assert_eq!(e.current_section("A"), Some(1));
}

#[test]
fn master_macros_show_in_the_mirror_and_the_chain() {
    let s = spec("techno", 42.0, "minimal");
    let (mut e, mut r) = settled(false, &s);
    e.play("A");
    render(&mut r, 10);
    let (hp, lp, idle) = e.deck("A").unwrap().mirror.master_filter_corners();
    assert_eq!(
        (hp, lp, idle),
        (20.0, 20_000.0, true),
        "a fresh chain rests"
    );
    e.fire_master("A", MasterMacro::SweepHp, Some(1));
    render(&mut r, 40);
    let m = &e.deck("A").unwrap().mirror;
    assert!(m.is_master_active(MasterMacro::SweepHp));
    let (hp, _, idle) = m.master_filter_corners();
    assert!(hp > 20.0 && !idle, "the high-pass sweeps up: {hp}");
    // a slider by hand
    e.panic("A");
    e.set_master_fx("A", MasterFxKey::LpFreq, 30);
    render(&mut r, 40);
    let (_, lp, _) = e.deck("A").unwrap().mirror.master_filter_corners();
    assert!(lp < 20_000.0, "{lp}");
}

#[test]
fn the_headphone_feed_is_the_cued_decks_pre_fader() {
    let s = spec("techno", 42.0, "minimal");
    let (mut e, mut r) = settled(false, &s);
    e.set_pfl("A", true);
    e.set_headphone_mix(0.0); // only the cue
    e.set_headphone_level(1.0);
    e.set_volume("A", 0.0); // the fader is down: the master is silent, the cue is not
    e.play("A");
    render(&mut r, 2);
    let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    let (mut cl, mut cr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    let mut master = 0.0;
    let mut cue = 0.0;
    for _ in 0..40 {
        r.render_with_cue(&mut l, &mut rr, Some((&mut cl, &mut cr)));
        master += rms(&l);
        cue += rms(&cl);
    }
    assert!(master < 1e-6, "master {master}");
    assert!(cue > 0.1, "cue {cue}");
    // and with the mix all the way to the master the cue hears the (silent) master
    e.set_headphone_mix(1.0);
    render(&mut r, 1);
    r.render_with_cue(&mut l, &mut rr, Some((&mut cl, &mut cr)));
    assert!(rms(&cl) < 1e-6);
}

#[test]
fn the_app_path_holds_the_true_peak_ceiling() {
    use djrust::fx::true_peak;
    let s = spec("dnb", 4242.0, "standard");
    let (mut e, mut r) = settled(true, &s);
    e.play("A");
    let (l, rr) = render(&mut r, 400);
    assert!(rms(&l) > 0.01);
    let tp = true_peak::dbtp(&l, Some(&rr));
    assert!(
        tp <= djrust::fx::PeakLimiter::CEILING_DB + 0.01,
        "{tp} dBTP"
    );
    assert!(e.deck("A").unwrap().mirror.limiter_gain_reduction_db() >= 0.0);
}

#[test]
fn a_stop_ends_the_transport_and_a_play_resumes_it() {
    let s = spec("techno", 42.0, "minimal");
    let (mut e, mut r) = settled(false, &s);
    e.set_tempo("A", 480.0);
    e.play("A");
    render(&mut r, 40);
    assert!(e.deck("A").unwrap().mirror.is_playing());
    let tick = e.deck("A").unwrap().mirror.tick();
    assert!(tick > 0);
    e.stop("A");
    render(&mut r, 2);
    assert!(!e.deck("A").unwrap().mirror.is_playing());
    assert_eq!(
        e.deck("A").unwrap().mirror.tick(),
        0,
        "stop rewinds the net"
    );
}

/// `DeckNetTests`: the swap keeps the deck's tempo from the new project, and a deck that is
/// not playing takes a load at once.
#[test]
fn a_stopped_deck_takes_a_load_at_once() {
    let (mut e, mut r) = engine(config(false));
    e.generate("A", &spec("house", 3.0, "minimal"));
    render(&mut r, 1);
    let m = &e.deck("A").unwrap().mirror;
    assert!(m.has_project.load(std::sync::atomic::Ordering::Relaxed));
    let house = m.tempo();
    e.generate("A", &spec("dnb", 3.0, "minimal"));
    render(&mut r, 1);
    let dnb = e.deck("A").unwrap().mirror.tempo();
    assert!(dnb > house, "dnb {dnb} is faster than house {house}");
}
