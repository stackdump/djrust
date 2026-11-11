//! `WaveRunner.stop()` resets the runner's own 150 Hz side highpass on the sampled instruments'
//! mix (`samplerSide.reset()`, WaveRunner.swift:1615), as it does the tone filters and the duck:
//! what the highpass held of the last note must not leak into the next play. No Swift test
//! names it (the sampled mix is AUSampler); the Swift source is the held reference.

use djrust::musicality::fixes::Fixes;
use djrust::pflow::{NetBundle, Project};
use djrust::wave::{Master, SamplerHost, WaveOptions, WaveRunner};

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;

/// A host whose slots play a wide low tone (a 60 Hz square, opposite on the two sides) while
/// `on`, and nothing after.
struct Wide {
    on: bool,
    n: usize,
}

impl SamplerHost for Wide {
    fn begin_block(&mut self, _runner: &WaveRunner, _frames: usize) {}
    fn flush(
        &mut self,
        _runner: &mut WaveRunner,
        _start: i64,
        left: &mut [f32],
        right: &mut [f32],
        frames: usize,
    ) {
        for i in 0..frames {
            if self.on {
                let v = if (self.n / 400) % 2 == 0 { 0.3 } else { -0.3 };
                left[i] += v;
                right[i] -= v;
            }
            self.n += 1;
        }
    }
}

/// A runner on a silent one-bar net (no notes), the samplers in the curve (`.loudness`) and the
/// stereo field (`.stereo`: the sampled lows' side is taken out).
fn runner() -> WaveRunner {
    let mut p = Project {
        tempo: 120.0,
        ..Default::default()
    };
    let mut n = NetBundle::ring(16);
    n.track.channel = 4;
    n.track.group = Some("melody".into());
    p.nets.insert("melody", n);
    let opts = WaveOptions {
        master: Master::Linear,
        musicality: Fixes::LOUDNESS | Fixes::STEREO,
        ..Default::default()
    };
    let mut r = WaveRunner::new(SR, opts.clone());
    r.load_prepared(WaveRunner::prepare(&p, SR, &opts, 0));
    r.set_tempo(120.0);
    r.play();
    r
}

fn block(r: &mut WaveRunner, host: &mut Wide, frame: &mut i64) -> (Vec<f32>, Vec<f32>) {
    let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    r.process_with(&mut l, Some(&mut rr), BLOCK, *frame, Some(host));
    *frame += BLOCK as i64;
    (l, rr)
}

#[test]
fn a_stop_clears_what_the_sampler_side_highpass_holds() {
    for stop in [true, false] {
        let mut r = runner();
        let mut host = Wide { on: true, n: 0 };
        let mut frame = 0;
        let mut loud = 0f32;
        for _ in 0..20 {
            let (l, rr) = block(&mut r, &mut host, &mut frame);
            loud = loud.max(l.iter().chain(&rr).fold(0.0, |m, v| m.max(v.abs())));
        }
        assert!(loud > 0.05, "the wide tone is heard ({loud})");
        host.on = false;
        if stop {
            r.stop();
        }
        let (l, rr) = block(&mut r, &mut host, &mut frame);
        let tail = l.iter().chain(&rr).fold(0.0f32, |m, v| m.max(v.abs()));
        if stop {
            assert_eq!(tail, 0.0, "after a stop the highpass starts from nothing");
        } else {
            // (the control: without the stop the filter's state rings on into silence)
            assert!(tail > 1e-4, "the filter holds a tail ({tail})");
        }
    }
}
