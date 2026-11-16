//! The live instrument change (`WaveRunner::set_instrument`, `StudioShell::set_instrument`,
//! djwasm's `djw_set_instrument`, feature INSTRUMENT). Its contract, held here:
//!
//! - Stopped, it is a load: the project renders bit for bit what the project with the
//!   instrument written into its document renders.
//! - Playing, the song does not move: the tick, the marking and every other net play on as if
//!   nothing happened (bit for bit once the changed net is muted), and the changed net sounds
//!   different from the next block on.
//! - The same instrument again is idle (no rebuild, so no note is cut).
//! - A project queued for the bar takes it too, so the swap does not undo it.
//! - It refuses what has no lanes (a control net, an unknown id) and touches nothing then.

use djrust::pflow::Project;
use djrust::wave::synth::{is_drum_channel, tonal_voice};
use djrust::wave::*;
use djrust::{compose, JsonObject};

const SR: f64 = 48_000.0;
const BLOCK: usize = 128;

fn project(genre: &str, seed: f64) -> Project {
    let mut ov = JsonObject::new();
    ov.set("seed", seed.into());
    compose(genre, &ov.into())
}

fn opts() -> WaveOptions {
    WaveOptions::default()
}

/// The first music net that is tonal (not a drum channel, not a pad: a pad is the soft voice
/// whatever its instrument) and the first drum net.
fn pick(p: &Project) -> (String, String) {
    let mut tonal = None;
    let mut drum = None;
    for (id, nb) in p.nets.iter() {
        if nb.role == "control" {
            continue;
        }
        let group = nb.track.group.as_deref();
        if is_drum_channel(nb.track.channel) {
            drum.get_or_insert_with(|| id.to_string());
        } else if !matches!(group, Some("harmony") | Some("pad")) {
            tonal.get_or_insert_with(|| id.to_string());
        }
    }
    (tonal.expect("a tonal net"), drum.expect("a drum net"))
}

/// An instrument whose voice differs from `id`'s own.
fn other_instrument(p: &Project, id: &str) -> String {
    let nb = p.nets.get(id).unwrap();
    if is_drum_channel(nb.track.channel) {
        let now = nb.track.instrument.as_str();
        return if now == "drums-808" {
            "drums-cr78"
        } else {
            "drums-808"
        }
        .into();
    }
    let group = nb.track.group.as_deref();
    let own = tonal_voice(Some(&nb.track.instrument), group);
    for cand in ["vibes", "square-lead", "sub-bass", "pluck"] {
        if tonal_voice(Some(cand), group) != own {
            return cand.into();
        }
    }
    unreachable!("every candidate plays {id}'s voice")
}

fn with_instrument(p: &Project, id: &str, instrument: &str) -> Project {
    let mut q = p.clone();
    q.nets.get_mut(id).unwrap().track.instrument = instrument.into();
    q
}

fn runner(p: &Project) -> WaveRunner {
    let mut r = WaveRunner::new(SR, opts());
    r.load_prepared(WaveRunner::prepare(p, SR, &opts(), 0));
    r
}

fn blocks(r: &mut WaveRunner, n: usize) -> Vec<f32> {
    let mut out = vec![0f32; 2 * n * BLOCK];
    for b in 0..n {
        let (l, rest) = out[2 * b * BLOCK..].split_at_mut(BLOCK);
        r.process(l, Some(&mut rest[..BLOCK]), BLOCK);
    }
    out
}

fn same(a: &[f32], b: &[f32]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())
}

/// Blocks in `seconds`.
fn n(seconds: f64) -> usize {
    (seconds * SR) as usize / BLOCK
}

#[test]
fn stopped_it_is_a_load_of_the_changed_document() {
    for (genre, seed) in [
        ("techno", 1.0),
        ("lofi", 7.0),
        ("house", 42.0),
        ("dnb", 3.0),
    ] {
        let p = project(genre, seed);
        let (tonal, drum) = pick(&p);
        for id in [&tonal, &drum] {
            let inst = other_instrument(&p, id);
            let mut a = runner(&p);
            assert_eq!(a.set_instrument(id, &inst), 1, "{genre} {id}");
            a.play();
            let got = blocks(&mut a, n(6.0));
            let mut b = runner(&with_instrument(&p, id, &inst));
            b.play();
            let want = blocks(&mut b, n(6.0));
            assert!(
                same(&got, &want),
                "{genre}: {id} on {inst} is not the load's render"
            );
            // and it is a change: the unchanged project sounds different
            let mut c = runner(&p);
            c.play();
            assert!(
                !same(&got, &blocks(&mut c, n(6.0))),
                "{genre}: {id} on {inst} sounds the same"
            );
            assert_eq!(
                a.graph()
                    .unwrap()
                    .project
                    .nets
                    .get(id)
                    .unwrap()
                    .track
                    .instrument,
                inst
            );
        }
    }
}

#[test]
fn playing_the_song_does_not_move() {
    let p = project("techno", 5.0);
    let (tonal, drum) = pick(&p);
    for id in [&tonal, &drum] {
        let inst = other_instrument(&p, id);
        let (mut a, mut r) = (runner(&p), runner(&p));
        a.play();
        r.play();
        let before = (blocks(&mut a, n(3.3)), blocks(&mut r, n(3.3)));
        assert!(same(&before.0, &before.1));
        assert_eq!(a.set_instrument(id, &inst), 1);
        let after = blocks(&mut a, n(4.0));
        let unchanged = blocks(&mut r, n(4.0));
        assert!(
            !same(&after, &unchanged),
            "{id} on {inst}: no audible change"
        );
        let (ga, gr) = (a.graph().unwrap(), r.graph().unwrap());
        assert_eq!(ga.tick, gr.tick, "{id}: the tick moved");
        for (na, nr) in ga.nets.iter().zip(&gr.nets) {
            assert_eq!(na.state, nr.state, "{id}: net {}'s marking moved", na.id);
        }
        assert!(a.playing());
    }
}

#[test]
fn playing_every_other_net_plays_on_bit_for_bit() {
    let p = project("house", 11.0);
    let (tonal, drum) = pick(&p);
    for id in [&tonal, &drum] {
        let inst = other_instrument(&p, id);
        let (mut a, mut r) = (runner(&p), runner(&p));
        // the changed net silent in both: what is left is every other net
        a.set_mute(id, true);
        r.set_mute(id, true);
        a.play();
        r.play();
        let mut got = blocks(&mut a, n(2.1));
        let mut want = blocks(&mut r, n(2.1));
        assert_eq!(a.set_instrument(id, &inst), 1);
        got.extend(blocks(&mut a, n(5.0)));
        want.extend(blocks(&mut r, n(5.0)));
        assert!(same(&got, &want), "{id}: the change moved another net");
        assert!(got.iter().any(|x| *x != 0.0));
    }
}

#[test]
fn the_same_instrument_is_idle() {
    let p = project("lofi", 2.0);
    let (tonal, drum) = pick(&p);
    let (mut a, mut r) = (runner(&p), runner(&p));
    a.play();
    r.play();
    let mut got = blocks(&mut a, n(1.7));
    let mut want = blocks(&mut r, n(1.7));
    for id in [&tonal, &drum] {
        let own = p.nets.get(id).unwrap().track.instrument.clone();
        assert_eq!(a.set_instrument(id, &own), 1);
    }
    got.extend(blocks(&mut a, n(3.0)));
    want.extend(blocks(&mut r, n(3.0)));
    assert!(same(&got, &want));
    // nothing was rebuilt, so nothing waits to be freed
    assert!(a.pop_retired().is_none());
}

#[test]
fn a_project_queued_for_the_bar_takes_it_too() {
    let p = project("techno", 9.0);
    let q = project("techno", 10.0);
    let (tonal, _) = pick(&q);
    assert!(p.nets.get(&tonal).is_some(), "both songs have {tonal}");
    let inst = other_instrument(&q, &tonal);
    let mut a = runner(&p);
    a.play();
    blocks(&mut a, n(0.5));
    a.load(&q); // queued: it swaps in at the next bar
    assert_eq!(a.pending_generation() > 0, true);
    assert_eq!(a.set_instrument(&tonal, &inst), 2);
    blocks(&mut a, n(3.0)); // past the bar
    let g = a.graph().unwrap();
    assert_eq!(g.project.nets.get(&tonal).unwrap().track.instrument, inst);
}

#[test]
fn what_has_no_lanes_is_refused_and_untouched() {
    let p = project("techno", 4.0);
    let mut empty = WaveRunner::new(SR, opts());
    assert_eq!(empty.set_instrument("melody", "vibes"), 0);
    let (mut a, mut r) = (runner(&p), runner(&p));
    a.play();
    r.play();
    let control: Vec<String> = p
        .nets
        .iter()
        .filter(|(_, nb)| nb.role == "control")
        .map(|(id, _)| id.to_string())
        .collect();
    for id in control
        .iter()
        .map(String::as_str)
        .chain(["no-such-net", ""])
    {
        assert_eq!(a.set_instrument(id, "vibes"), 0, "{id:?}");
    }
    assert!(a.pop_retired().is_none());
    assert!(same(&blocks(&mut a, n(2.0)), &blocks(&mut r, n(2.0))));
}

#[test]
fn the_old_lanes_wait_in_the_retire_list() {
    let p = project("techno", 6.0);
    let (tonal, _) = pick(&p);
    let mut a = runner(&p);
    a.play();
    blocks(&mut a, 8);
    assert_eq!(a.set_instrument(&tonal, &other_instrument(&p, &tonal)), 1);
    let spent = a
        .pop_retired()
        .expect("the rebuilt project, holding the old lanes");
    assert_eq!(spent.graph.nets.len(), a.graph().unwrap().nets.len());
    assert!(a.pop_retired().is_none());
}

#[test]
fn the_studio_shell_answers_codes() {
    use djrust::wave::studio::{feature, ERR_ARG, ERR_NOT_READY, OK};
    assert_ne!(feature::THIS_BUILD & feature::INSTRUMENT, 0);
    let p = project("techno", 8.0);
    let (tonal, _) = pick(&p);
    let mut s = StudioShell::new(SR, 0).unwrap();
    assert_eq!(s.set_instrument(&tonal, "vibes"), ERR_NOT_READY);
    let doc = p.to_json();
    s.load_json(&doc);
    assert_eq!(s.set_instrument(&tonal, ""), ERR_ARG);
    assert_eq!(s.set_instrument("no-such-net", "vibes"), ERR_ARG);
    assert_eq!(s.set_instrument(&tonal, &other_instrument(&p, &tonal)), OK);
}
