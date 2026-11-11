//! The runner's per-part moves behind the Pan and Shape macros (`WaveRunner::set_pan_move`,
//! `set_decay_move`): beats' `channelParamMove` (`pan-move`, `decay-move`) as measured renders on
//! a linear master. A pan move replaces a part's stereo placement; it needs no `.stereo`; it
//! leaves the drums alone when the macro targets non-drums; letting go puts everything back.

mod common;
use common::measure::*;
use djrust::musicality::fixes::Fixes;
use djrust::pflow::{NetBundle, Project};
use djrust::wave::*;

const SR: f64 = 48_000.0;

/// A kick on the beat and a held pad chord at 120 BPM.
fn kick_and_pad(kick: bool, pad: bool) -> Project {
    let mut k = NetBundle::ring(16);
    for step in [0, 4, 8, 12] {
        k.bind(step, 36, 10, 100, 50);
    }
    k.track.channel = 10;
    k.track.group = Some("drums".into());
    let mut c = NetBundle::ring(16);
    for (v, note) in [60, 64, 67].into_iter().enumerate() {
        c.bind(v as i64, note, 7, 90, 1950);
    }
    c.track.channel = 7;
    c.track.group = Some("harmony".into());
    let mut p = Project { tempo: 120.0, ..Default::default() };
    if kick {
        p.nets.insert("kick", k);
    }
    if pad {
        p.nets.insert("harmony", c);
    }
    p
}

fn linear(fixes: Fixes) -> WaveOptions {
    WaveOptions { master: Master::Linear, musicality: fixes, ..Default::default() }
}

/// Two seconds, stereo, 256-frame blocks; `moves` is called on the runner before each block with
/// the block's start frame.
fn render(
    p: &Project,
    fixes: Fixes,
    mut moves: impl FnMut(&mut WaveRunner, usize),
) -> (Vec<f32>, Vec<f32>) {
    let o = linear(fixes);
    let mut r = WaveRunner::new(SR, o.clone());
    r.load_prepared(WaveRunner::prepare(p, SR, &o, 0));
    r.play();
    let mut l = vec![0f32; (2.0 * SR) as usize];
    let mut rr = l.clone();
    let mut off = 0;
    while off < l.len() {
        moves(&mut r, off);
        let n = 256.min(l.len() - off);
        r.process(&mut l[off..off + n], Some(&mut rr[off..off + n]), n);
        off += n;
    }
    (l, rr)
}

fn energy(x: &[f32]) -> f64 {
    x.iter().map(|v| *v as f64 * *v as f64).sum()
}

#[test]
fn hard_left_puts_the_pad_on_the_left_and_leaves_the_kick_in_the_middle() {
    for fixes in [Fixes::EMPTY, Fixes::STEREO] {
        let kick = render(&kick_and_pad(true, false), fixes, |_, _| {});
        let full = render(&kick_and_pad(true, true), fixes, |r, _| {
            r.set_pan_move(MacroTargets::NonDrums, Some(-1.0));
        });
        // the right channel carries the kick alone: the pad is all on the left
        let right_diff: f64 = full.1.iter().zip(&kick.1).map(|(a, b)| (*a as f64 - *b as f64).abs()).sum();
        assert!(right_diff < 1e-3, "pad leaked into the right: {right_diff} ({fixes:?})");
        let pad = render(&kick_and_pad(false, true), fixes, |r, _| {
            r.set_pan_move(MacroTargets::NonDrums, Some(-1.0));
        });
        assert!(energy(&pad.1) < 1e-9 && energy(&pad.0) > 1e-3, "all of the pad is on the left");
        // the kick is still in the middle: with the pad out of the way, both sides match
        let kick_only = render(&kick_and_pad(true, false), fixes, |r, _| {
            r.set_pan_move(MacroTargets::NonDrums, Some(-1.0));
        });
        assert!(identical(&kick_only.0, &kick_only.1), "the kick moved");
        // the same on the right
        let right = render(&kick_and_pad(true, true), fixes, |r, _| {
            r.set_pan_move(MacroTargets::NonDrums, Some(1.0));
        });
        let left_diff: f64 = right.0.iter().zip(&kick.0).map(|(a, b)| (*a as f64 - *b as f64).abs()).sum();
        assert!(left_diff < 1e-3, "pad leaked into the left: {left_diff}");
        let pad_r = render(&kick_and_pad(false, true), fixes, |r, _| {
            r.set_pan_move(MacroTargets::NonDrums, Some(1.0));
        });
        assert!(energy(&pad_r.0) < 1e-9 && energy(&pad_r.1) > 1e-3, "all of the pad is on the right");
    }
}

#[test]
fn mono_takes_the_stereo_placement_away_and_letting_go_puts_it_back() {
    let wide = render(&kick_and_pad(false, true), Fixes::STEREO, |_, _| {});
    assert!(!identical(&wide.0, &wide.1), "the pad starts wide");
    let mono = render(&kick_and_pad(false, true), Fixes::STEREO, |r, _| {
        r.set_pan_move(MacroTargets::Everything, Some(0.0));
    });
    assert!(identical(&mono.0, &mono.1), "mono is mono");
    // a move that lasts for the first second only: the second is as untouched
    let half = render(&kick_and_pad(false, true), Fixes::STEREO, |r, at| {
        if at == 0 {
            r.set_pan_move(MacroTargets::Everything, Some(0.0));
        } else if at >= SR as usize {
            r.set_pan_move(MacroTargets::Everything, None);
        }
    });
    let n = SR as usize;
    assert!(identical(&half.0[..n], &half.1[..n]));
    assert_eq!(half.0[n + 256..], wide.0[n + 256..], "after the move, as untouched (left)");
    assert_eq!(half.1[n + 256..], wide.1[n + 256..], "after the move, as untouched (right)");
}

#[test]
fn a_move_that_is_never_made_changes_nothing() {
    let a = render(&kick_and_pad(true, true), Fixes::STEREO, |_, _| {});
    let b = render(&kick_and_pad(true, true), Fixes::STEREO, |r, _| {
        r.set_pan_move(MacroTargets::Everything, None);
        r.set_decay_move(MacroTargets::Everything, None);
    });
    assert_eq!(a, b);
}

#[test]
fn tighten_cuts_the_notes_short_and_loosen_lets_them_ring() {
    let go = |decay: Option<f64>| {
        let (l, _) = render(&kick_and_pad(false, true), Fixes::EMPTY, |r, _| {
            r.set_decay_move(MacroTargets::Everything, decay);
        });
        energy(&l)
    };
    let (tight, plain, loose) = (go(Some(0.3)), go(None), go(Some(2.5)));
    assert!(tight < plain, "tighten {tight} vs {plain}");
    assert!(loose > plain, "loosen {loose} vs {plain}");
}
