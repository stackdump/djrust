//! The offline engine as the app runs it, for the loop row's tests (the `Rig` of
//! `LoopLaunchTests` and `LoopRecordTests`): 512-frame blocks, the takes serviced between
//! them, the master's left channel kept so an edge can be looked at.

#![allow(dead_code)]

use djrust::deck::turntable::SampleBuffer;
use djrust::deck::waveform::WaveformOverview;
use djrust::json::JsonValue;
use djrust::TrackSpec;
use djrust::host::deck::loops::LoopClip;
use djrust::host::deck::{engine, Engine, EngineConfig, Renderer};
use std::sync::Arc;

pub const SR: f64 = 48_000.0;
pub const BLOCK: usize = 512;
/// A bar at 120 bpm at 48 kHz.
pub const BAR: i64 = 96_000;
pub const BEAT: i64 = BAR / 4;
/// A frame, in beats: a boundary is rounded up to a whole frame.
pub const ONE_FRAME: f64 = 1.0 / BEAT as f64;

pub struct Rig {
    pub e: Engine,
    pub r: Renderer,
    /// Every master frame rendered (left channel), from frame 0.
    pub master: Vec<f32>,
}

impl Rig {
    /// Decks A and B, the crossfader on A, the strips at unity (so the master window is the
    /// deck after its EQ).
    pub fn new() -> Rig {
        let mut c = EngineConfig::new(SR);
        c.extended = false;
        let (mut e, r) = engine(c);
        e.set_crossfader(0.0);
        e.set_master(1.0);
        for d in ["A", "B"] {
            e.set_volume(d, 1.0);
        }
        Rig {
            e,
            r,
            master: Vec::new(),
        }
    }

    /// Deck A alone, as the app builds it (`DJEngine(decks: ["A"], offline: true)`: the app's
    /// options, every musicality fix and the limiter), the strip at unity: what
    /// `LoopRetuneTests`' rig runs.
    pub fn app_deck_a() -> Rig {
        let mut c = EngineConfig::new(SR);
        c.decks = vec!["A".into()];
        let (mut e, r) = engine(c);
        e.set_crossfader(0.0);
        e.set_master(1.0);
        e.set_volume("A", 1.0);
        Rig {
            e,
            r,
            master: Vec::new(),
        }
    }

    pub fn frame(&self) -> i64 {
        self.e.loop_frames()
    }

    /// Render until engine frame `frame`, the takes serviced after each block.
    pub fn run_to(&mut self, frame: i64) {
        let (mut l, mut r) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
        while self.frame() < frame {
            let n = BLOCK.min((frame - self.frame()) as usize);
            self.r.render_block(&mut l[..n], &mut r[..n]);
            self.master.extend_from_slice(&l[..n]);
            self.e.service_loops();
        }
    }

    pub fn run(&mut self, frames: i64) {
        self.run_to(self.frame() + frames);
    }

    /// Render until a clip lands (at most `limit` more frames).
    pub fn run_until_clip(&mut self, limit: i64) -> Option<String> {
        let count = self.e.loops.row.clips.len();
        let stop = self.frame() + limit;
        while self.e.loops.row.clips.len() == count && self.frame() < stop {
            self.run(BLOCK as i64);
        }
        (self.e.loops.row.clips.len() > count).then(|| self.e.loops.row.clips.last().unwrap().id.clone())
    }

    /// A net deck A at `bpm` (a techno track), playing.
    pub fn net_deck(&mut self, bpm: f64) {
        let mut spec = TrackSpec::new("techno", 1.0, None);
        spec.extra.set("bpm", JsonValue::Number(bpm));
        assert!(self.e.generate("A", &spec));
        self.e.play("A");
    }

    /// Two one-bar takes of a net deck at 120 bpm on deck A, both looping under the mixer
    /// (`twoTakes`).
    pub fn two_takes(&mut self) -> (String, String) {
        self.net_deck(120.0);
        self.run_to(BAR + 100);
        self.e.loop_record("A", true, None);
        self.run_to(2 * BAR - 100);
        self.e.loop_record("A", false, None);
        let one = self.run_until_clip(4 * BAR).expect("a first take");
        self.e.loop_record("A", true, None);
        self.run(BAR / 2);
        self.e.loop_record("A", false, None);
        let two = self.run_until_clip(4 * BAR).expect("a second take");
        (one, two)
    }

    /// Render until no clip is waiting to switch.
    pub fn settle(&mut self) {
        for _ in 0..2000 {
            let ids: Vec<String> = self.e.loops.row.clips.iter().map(|c| c.id.clone()).collect();
            if !ids.iter().any(|i| self.e.loop_switch_frame(i).is_some()) {
                break;
            }
            self.run(BLOCK as i64);
        }
        self.run(4096);
    }

    /// The loudest master sample in the 256 frames before `frame` and in the 2048 from it.
    pub fn edge(&self, frame: i64) -> (f32, f32) {
        let peak = |a: i64, b: i64| {
            self.master[a.max(0) as usize..(b as usize).min(self.master.len())]
                .iter()
                .fold(0f32, |p, v| p.max(v.abs()))
        };
        (peak(frame - 256, frame), peak(frame, frame + 2048))
    }

    /// Only the loop channel reaches the master from here on.
    pub fn decks_to_the_loops_only(&mut self) {
        for d in ["A", "B"] {
            self.e.set_volume(d, 0.0);
        }
    }
}

/// Is beat `b` on a line every `q` beats (to within the frame it was rounded to)?
pub fn on_line(b: f64, every: f64) -> bool {
    let r = (b / every - (b / every).round()) * every;
    r.abs() <= ONE_FRAME * 1.01
}

/// A one-bar clip at 120 bpm whose samples say which clip it is (`testClip`).
pub fn test_clip(n: usize) -> LoopClip {
    let len = 96_000;
    let l: Vec<f32> = (0..len)
        .map(|i| n as f32 * 0.01 + (i % 100) as f32 * 1e-4)
        .collect();
    let b = SampleBuffer::new(l.clone(), Some(l.iter().map(|v| -v).collect()), SR);
    let w = WaveformOverview::new(&b, 40);
    LoopClip::from_bars(
        &b.content_id(),
        &format!("A loop {n}"),
        "A",
        1,
        120.0,
        0.0,
        Arc::new(b),
        w,
        None,
    )
}
