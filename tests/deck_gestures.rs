//! The fixes under performance, part two (`FixesUnderGesturesTests`): what the runner fixes do
//! under a DJ's gestures when the deck's master chain is in the way: a master sweep stacked on
//! `.energy`'s lowpass, and a stop under the turntable brake. (The runner-level halves of the
//! Swift suite are `wave_duck.rs`, `wave_balance.rs` and `wave_sends.rs`'.) Form (a): each test
//! names the Swift one; the deck is the one the app plays (`WaveOptions::extended` and the
//! genre's settings, synth voices, `DeckCore`).

use std::sync::Arc;

use djrust::fx::MasterMacro;
use djrust::musicality::Fixes;
use djrust::wave::WaveOptions;
use djrust::TrackSpec;
use djrust::host::deck::{Command, DeckCore, DeckMirror};

const RATE: f64 = 22_050.0;
const BLOCK: usize = 128;

/// `deck(_:structure:drop:)`: a deck playing `genre` (seed 42) with the app's options and the
/// genre's settings, synth voices only; `drop` takes fixes out.
fn deck(genre: &str, structure: &str, drop: Fixes) -> (DeckCore, Arc<DeckMirror>) {
    let mut o = WaveOptions::extended().for_genre(genre);
    o.musicality = Fixes(o.musicality.0 & !drop.0);
    if drop.contains(Fixes::ECHO) {
        o.echo = None;
    }
    let mirror = Arc::new(DeckMirror::new());
    let mut core = DeckCore::new("A", RATE, o.clone(), o.turntable_brake, mirror.clone());
    let p = TrackSpec::new(genre, 42.0, Some(structure)).project();
    core.apply(Command::Load(djrust::wave::WaveRunner::prepare(
        &p, RATE, &o, 0,
    )));
    core.apply(Command::Play);
    (core, mirror)
}

fn blocks(seconds: f64) -> usize {
    (seconds * RATE) as usize / BLOCK
}

fn rms_db(x: &[f32]) -> f64 {
    let sum: f64 = x.iter().map(|v| *v as f64 * *v as f64).sum();
    10.0 * (sum / x.len() as f64).max(1e-20).log10()
}

/// Render `cores` side by side for `n` blocks; `at(b, cores)` runs before block `b`. Each core's
/// mid ((L + R) / 2).
fn render(
    cores: &mut [&mut DeckCore],
    n: usize,
    mut at: impl FnMut(usize, &mut [&mut DeckCore]),
) -> Vec<Vec<f32>> {
    let mut outs = vec![Vec::new(); cores.len()];
    let (mut l, mut r) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    for b in 0..n {
        at(b, cores);
        for (i, c) in cores.iter_mut().enumerate() {
            c.render(&mut l, &mut r, (b * BLOCK) as i64);
            outs[i].extend(l.iter().zip(&r).map(|(a, b)| (a + b) / 2.0));
        }
    }
    outs
}

// FixesUnderGesturesTests.theMasterSweepComesBackToTheEnergyCurve: the master lowpass (a mixer
// insert, after the runner) and `.energy`'s tonal lowpass (in the runner) are in series; a sweep
// stacked on the curve ends where the curve has the deck, as loud as a deck that never swept
// (+- 0.5 dB).
#[test]
fn the_master_sweep_comes_back_to_the_energy_curve() {
    let (mut swept, swept_m) = deck("techno", "standard", Fixes::EMPTY);
    let (mut reference, _) = deck("techno", "standard", Fixes::EMPTY);
    let fire = 40;
    let out = render(
        &mut [&mut swept, &mut reference],
        fire + blocks(9.5),
        |b, cores| {
            if b == fire {
                cores[0].apply(Command::MasterMacro {
                    m: MasterMacro::SweepLp,
                    bars: 1,
                });
            }
        },
    );
    assert!(
        !swept_m.is_master_active(MasterMacro::SweepLp),
        "the sweep has run out"
    );
    let a = (fire + blocks(8.5)) * BLOCK;
    let b = (fire + blocks(9.5)) * BLOCK;
    let x = rms_db(&out[0][a..b]);
    let y = rms_db(&out[1][a..b]);
    assert!(y > -60.0, "the deck plays");
    assert!((x - y).abs() < 0.5, "{x} vs {y} dB");
}

// FixesUnderGesturesTests.aStopUnderTheBrakeLeavesNothingToComeBack: a stop under the turntable
// brake (in its stopped quarter) ends the record there: nothing it held (voices, the room, the
// sends in flight) fades back in after the cancel.
#[test]
fn a_stop_under_the_brake_leaves_nothing_to_come_back() {
    let (mut src, mirror) = deck("techno", "minimal", Fixes::ECHO);
    let fire = blocks(3.0);
    let stop = fire + blocks(1.8); // a bar at 120 BPM is 2 s; stopped from 1.5 s
    let mut braking = false;
    let out = render(&mut [&mut src], stop + blocks(1.5), |b, cores| {
        if b == fire {
            cores[0].apply(Command::MasterMacro {
                m: MasterMacro::VinylBrake,
                bars: -1,
            });
        }
        if b == stop {
            braking = mirror.is_master_active(MasterMacro::VinylBrake);
            cores[0].apply(Command::Stop);
        }
    })
    .remove(0);
    assert!(braking, "the brake was on");
    assert!(rms_db(&out[..fire * BLOCK]) > -50.0, "the deck played");
    let after = &out[(stop + 1) * BLOCK..];
    let peak = after.iter().fold(0f32, |m, v| m.max(v.abs()));
    assert!(peak < 1e-5, "stale audio after the stop: peak {peak}");
}
