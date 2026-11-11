//! LOOP record and the loop channel (`LoopRecorderTests`): a hold snaps back to the bar it
//! started in and records on to the next downbeat, a tap with a length is exactly that long,
//! a sample deck counts bars on its grid, the last bars can be grabbed after the fact, and a
//! take loops under the mixer whatever the crossfader does.

use djrust::TrackSpec;
use djrust::host::deck::loops::LoopState;
use djrust::host::deck::{engine, Engine, EngineConfig, Renderer};

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;

fn blocks(e: &mut Engine, r: &mut Renderer, n: usize) -> f32 {
    let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    let mut peak = 0f32;
    for _ in 0..n {
        r.render_block(&mut l, &mut rr);
        e.service_loops();
        peak = l.iter().fold(peak, |p, v| p.max(v.abs()));
    }
    peak
}

/// Blocks for `bars` bars at `bpm`.
fn bars(bars: f64, bpm: f64) -> usize {
    (bars * 4.0 * 60.0 / bpm * SR / BLOCK as f64).round() as usize
}

fn net_deck() -> (Engine, Renderer, f64) {
    let mut c = EngineConfig::new(SR);
    c.extended = false;
    let (mut e, mut r) = engine(c);
    e.set_crossfader(0.0);
    e.set_volume("A", 1.0);
    assert!(e.generate("A", &TrackSpec::new("techno", 42.0, None)));
    e.play("A");
    blocks(&mut e, &mut r, 2);
    let bpm = e.deck("A").unwrap().mirror.tempo();
    assert!(bpm > 60.0);
    (e, r, bpm)
}

fn finish(e: &mut Engine, r: &mut Renderer) {
    for _ in 0..4000 {
        if !e.is_recording_loop("A") {
            return;
        }
        blocks(e, r, 1);
    }
    panic!("the take never finished");
}

#[test]
fn a_held_loop_snaps_to_its_bar_and_records_to_the_next_downbeat() {
    let (mut e, mut r, bpm) = net_deck();
    blocks(&mut e, &mut r, bars(1.3, bpm));
    assert!(e.loop_record("A", true, None));
    assert!(e.is_recording_loop("A"));
    blocks(&mut e, &mut r, bars(1.5, bpm));
    assert!(e.loop_record("A", false, None));
    finish(&mut e, &mut r);
    assert_eq!(e.loops.row.clips.len(), 1);
    let c = &e.loops.row.clips[0];
    assert_eq!((c.bars(), c.start_beat), (2, 4.0), "bars 2 and 3 of the deck");
    assert!((c.bpm - bpm).abs() < 0.01, "{} vs {bpm}", c.bpm);
    let bar_frames = 4.0 * 60.0 / bpm * SR;
    assert!((c.buffer.length() as f64 - 2.0 * bar_frames).abs() <= 2.0);
    assert!(
        c.buffer.left.iter().any(|v| v.abs() > 0.01),
        "it holds sound"
    );
    // it loops on L at once, crossfader or not
    let id = c.id.clone();
    blocks(&mut e, &mut r, 20);
    assert_eq!(e.loop_state(&id), LoopState::On);
    e.set_crossfader(1.0); // A out of the mix
    blocks(&mut e, &mut r, 20);
    assert!(blocks(&mut e, &mut r, bars(0.5, bpm)) > 0.01, "L plays");
    assert!(e.loops.mirror.level() > 0.0);
    // out on the next bar, and silent then
    e.set_looping(&id, false);
    assert_eq!(e.loop_state(&id), LoopState::Stopping);
    blocks(&mut e, &mut r, bars(1.2, bpm));
    assert_eq!(e.loop_state(&id), LoopState::Off);
    assert!(blocks(&mut e, &mut r, 20) < 1e-6, "nothing left in the mix");
}

#[test]
fn a_tap_with_a_length_takes_exactly_that_many_bars() {
    let (mut e, mut r, bpm) = net_deck();
    blocks(&mut e, &mut r, bars(0.4, bpm));
    assert!(e.loop_record("A", true, Some(1)));
    blocks(&mut e, &mut r, 4);
    e.loop_record("A", false, None);
    finish(&mut e, &mut r);
    let c = &e.loops.row.clips[0];
    assert_eq!((c.bars(), c.start_beat), (1, 0.0));
}

#[test]
fn a_release_on_the_one_ends_the_take_there() {
    let (mut e, mut r, bpm) = net_deck();
    blocks(&mut e, &mut r, bars(0.5, bpm));
    e.loop_record("A", true, None);
    // 1.5 bars on: just past the bar line after the press's bar
    blocks(&mut e, &mut r, bars(1.5, bpm) + 2);
    e.loop_record("A", false, None);
    finish(&mut e, &mut r);
    assert_eq!(e.loops.row.clips[0].bars(), 2);
}

#[test]
fn a_stopped_deck_records_nothing() {
    let (mut e, mut r, _) = net_deck();
    e.pause("A");
    blocks(&mut e, &mut r, 4);
    assert!(!e.loop_record("A", true, None));
}

#[test]
fn the_last_bars_can_be_grabbed() {
    let (mut e, mut r, bpm) = net_deck();
    blocks(&mut e, &mut r, bars(3.5, bpm));
    let id = e.grab_last_bars("A", 2).expect("two bars were played");
    let c = e.clip(&id).unwrap();
    assert_eq!((c.bars(), c.start_beat), (2, 4.0));
    assert!((c.buffer.length() as f64 - 2.0 * 4.0 * 60.0 / bpm * SR).abs() <= 2.0);
    assert!(e.grab_last_bars("A", 9).is_none(), "not that far back");
}

#[test]
fn a_sample_deck_counts_bars_on_its_grid() {
    let mut c = EngineConfig::new(SR);
    c.extended = false;
    let (mut e, mut r) = engine(c);
    e.set_crossfader(0.0);
    e.set_volume("A", 1.0);
    assert!(e.load_demo("A"));
    e.play("A");
    let bpm = djrust::deck::demo_loop::BPM;
    blocks(&mut e, &mut r, bars(0.6, bpm));
    assert!(e.loop_record("A", true, None));
    blocks(&mut e, &mut r, bars(1.2, bpm));
    e.loop_record("A", false, None);
    finish(&mut e, &mut r);
    let clip = &e.loops.row.clips[0];
    assert_eq!(clip.bars(), 2);
    assert!((clip.bpm - bpm).abs() < 0.5, "{} vs {bpm}", clip.bpm);
}

// `loopClock(for:prefer:)` reads a net deck's newest tick anchor: an exact frame and beat, where
// the deck's `beatState` is the estimate of the last block (`ref.mode == .net`). A sample deck has
// no anchors and keeps the block's end.
#[test]
fn the_loop_clock_takes_its_origin_from_the_decks_newest_tick_anchor() {
    use djrust::wave::TickAnchor;
    let (mut e, mut r, bpm) = net_deck();
    for pass in 0..6 {
        blocks(&mut e, &mut r, 120 + pass * 13);
        let m = e.deck("A").unwrap().mirror.clone();
        let w = m.anchors.written();
        assert!(w > 4, "the deck has ticked");
        // what Swift reads: the last four anchors, the newest with a tick of 1 or more
        let mut buf = [TickAnchor {
            frame: 0,
            tick: 0,
            generation: 0,
            tempo: 0.0,
            flags: Default::default(),
        }; 4];
        let (n, lost) = m.anchors.read(w - 4, &mut buf);
        assert_eq!((n, lost), (4, 0));
        let a = buf.iter().rev().find(|a| a.tick >= 1).unwrap();
        // (a clip being lined up: with none, a deck that holds no clip does not qualify)
        let clock = e.loop_clock(Some("a clip"), Some("A"));
        assert_eq!(clock.ref_deck.as_deref(), Some("A"));
        assert_eq!(clock.origin_frame, a.frame);
        assert_eq!(clock.origin_beat, (a.tick - 1) as f64 / 4.0);
        // the tick began before the block ended, and within one tick of it (a sixteenth)
        let rendered = m.rendered_frames.load(std::sync::atomic::Ordering::Relaxed);
        let tick = 60.0 / bpm / 4.0 * SR;
        assert!(
            a.frame < rendered && (rendered - a.frame) as f64 <= tick + 1.0,
            "anchor {} against the block's end {rendered} (a tick is {tick:.0})",
            a.frame
        );
        assert_eq!(clock.frames_per_beat, 60.0 / clock.bpm.unwrap() * SR);
    }
}

#[test]
fn a_sample_decks_loop_clock_keeps_the_block_it_was_read_at() {
    let mut c = EngineConfig::new(SR);
    c.extended = false;
    let (mut e, mut r) = engine(c);
    e.set_crossfader(0.0);
    e.set_volume("A", 1.0);
    assert!(e.load_demo("A"));
    e.play("A");
    blocks(&mut e, &mut r, 40);
    let clock = e.loop_clock(Some("a clip"), Some("A"));
    assert_eq!(clock.ref_deck.as_deref(), Some("A"));
    let rendered = e
        .deck("A")
        .unwrap()
        .mirror
        .rendered_frames
        .load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(clock.origin_frame, rendered, "no anchor: the block's end");
}

// ---- the loop channel's strip: EQ and DJ filter in front of the fader --------------------------
//
// Swift's `loopChannel` is a `Deck`: an `AVAudioUnitEQ` (three bands and the DJ filter) sits
// between its source (the loop bank, after its master bus and limiter) and its fader. Here the
// bank runs the block through `djrust::fx::strip::StripEq` (RBJ biquads standing for the AU,
// PORTING.md H) before the fader; the reference below is that filter run by hand over the same
// render with the knobs at rest, so the comparison is exact.

use djrust::deck::turntable::SampleBuffer;
use djrust::deck::waveform::WaveformOverview;
use djrust::fx::{StripEq, StripKnob, StripSettings};
use djrust::session::action::DeckAction;
use djrust::host::deck::loops::LoopClip;
use djrust::host::deck::PerformCtx;
use std::sync::Arc;

/// A one-bar clip at 120 bpm: a 480 Hz sawtooth and a slow level on the right, so every band of
/// the strip has something to do.
fn saw_clip() -> LoopClip {
    let len = 96_000;
    let l: Vec<f32> = (0..len)
        .map(|i| 0.2 * ((i % 100) as f32 / 50.0 - 1.0))
        .collect();
    let r: Vec<f32> = (0..len)
        .map(|i| 0.1 * ((i % 100) as f32 / 50.0 - 1.0) + 0.05 * (i as f32 * 1e-3).sin())
        .collect();
    let b = SampleBuffer::new(l, Some(r), SR);
    let w = WaveformOverview::new(&b, 40);
    LoopClip::from_bars(
        &b.content_id(),
        "saw",
        "A",
        1,
        120.0,
        0.0,
        Arc::new(b),
        w,
        None,
    )
}

const L_BLOCK: usize = 512;

/// The master's two channels of the loop channel alone, 1.5 s: the strip's knobs as given
/// (`.param` actions to `L`, taken before the clip starts), the fader and trim at unity, the
/// master at unity, decks silent.
fn loop_channel(knobs: &[(&str, f64)]) -> (Vec<f32>, Vec<f32>) {
    let mut c = EngineConfig::new(SR);
    c.extended = false;
    let (mut e, mut r) = engine(c);
    e.set_master(1.0);
    for d in ["A", "B"] {
        e.set_volume(d, 0.0);
    }
    e.set_loop_volume(1.0);
    e.set_loop_gain(0.5); // trim 2^0
    for (name, value) in knobs {
        let a = e.perform(
            &DeckAction::Param {
                deck: "L".into(),
                name: (*name).into(),
                value: *value,
            },
            PerformCtx::default(),
        );
        assert_eq!(
            a,
            djrust::host::deck::Applied::Done,
            "{name}: the loop channel's strip is heard"
        );
    }
    let (mut l, mut rr) = (vec![0f32; L_BLOCK], vec![0f32; L_BLOCK]);
    let (mut left, mut right) = (Vec::new(), Vec::new());
    let mut go = |e: &mut Engine, r: &mut Renderer, n: usize| {
        for _ in 0..n {
            r.render_block(&mut l, &mut rr);
            e.service_loops();
            left.extend_from_slice(&l);
            right.extend_from_slice(&rr);
        }
    };
    go(&mut e, &mut r, 4); // the fader ramp settles on silence
    let clip = saw_clip();
    let id = clip.id.clone();
    e.restore_clips(&[clip]);
    e.set_looping(&id, true);
    go(&mut e, &mut r, 140);
    (left, right)
}

fn strip_by_hand(knobs: &[(StripKnob, f64)], l: &mut [f32], r: &mut [f32]) {
    let mut eq = StripEq::new(SR);
    for (k, v) in knobs {
        eq.set(*k, *v);
    }
    for (l, r) in l.chunks_mut(L_BLOCK).zip(r.chunks_mut(L_BLOCK)) {
        eq.process(l, r);
    }
}

#[test]
fn the_loop_channels_eq_and_filter_shape_what_it_plays() {
    let (flat_l, flat_r) = loop_channel(&[]);
    assert!(peak_of(&flat_l) > 0.05, "the clip sounds");
    for (names, knobs) in [
        // a cut at the bottom, a boost at the top: the shelves
        (
            vec![("eqLow", 0.1), ("eqHigh", 0.9)],
            vec![(StripKnob::EqLow, 0.1), (StripKnob::EqHigh, 0.9)],
        ),
        // the mid peak alone
        (vec![("eqMid", 0.0)], vec![(StripKnob::EqMid, 0.0)]),
        // the DJ filter either side of its dead zone: a low-pass, a high-pass
        (vec![("filter", 0.1)], vec![(StripKnob::Filter, 0.1)]),
        (vec![("filter", 0.9)], vec![(StripKnob::Filter, 0.9)]),
        // all four at once
        (
            vec![
                ("eqLow", 0.8),
                ("eqMid", 0.2),
                ("eqHigh", 0.3),
                ("filter", 0.2),
            ],
            vec![
                (StripKnob::EqLow, 0.8),
                (StripKnob::EqMid, 0.2),
                (StripKnob::EqHigh, 0.3),
                (StripKnob::Filter, 0.2),
            ],
        ),
    ] {
        let (got_l, got_r) = loop_channel(&names);
        let (mut want_l, mut want_r) = (flat_l.clone(), flat_r.clone());
        strip_by_hand(&knobs, &mut want_l, &mut want_r);
        assert!(
            got_l == want_l && got_r == want_r,
            "{names:?}: the render is the strip's"
        );
        let moved = got_l
            .iter()
            .zip(&flat_l)
            .fold(0f32, |m, (a, b)| m.max((a - b).abs()));
        assert!(
            moved > 0.01,
            "{names:?}: the strip changed the sound ({moved})"
        );
    }
}

fn peak_of(x: &[f32]) -> f32 {
    x.iter().fold(0f32, |m, v| m.max(v.abs()))
}

// The knobs at rest leave the channel as it was, bit for bit, also after they were moved and put
// back (a bypassed band is out of the signal path).
#[test]
fn the_loop_channels_strip_at_rest_is_bit_transparent() {
    let (flat_l, flat_r) = loop_channel(&[]);
    let (l, r) = loop_channel(&[
        ("eqLow", 0.5),
        ("eqMid", 0.5),
        ("eqHigh", 0.5),
        ("filter", 0.5),
    ]);
    assert!(l == flat_l && r == flat_r, "set to rest");
    let (l, r) = loop_channel(&[
        ("eqLow", 0.0),
        ("filter", 0.1),
        ("eqHigh", 1.0),
        ("eqLow", 0.5),
        ("filter", 0.5),
        ("eqHigh", 0.5),
    ]);
    assert!(l == flat_l && r == flat_r, "moved and put back");
    // inside the dead zones (0.01 of an EQ knob's rest, 0.45 to 0.55 of the filter's)
    let (l, r) = loop_channel(&[("eqMid", 0.505), ("filter", 0.54)]);
    assert!(l == flat_l && r == flat_r, "within the dead zones");
}

// What a journal and a session keep of the channel's strip: the knobs as they were set.
#[test]
fn the_loop_channels_strip_round_trips_in_its_params_and_the_journal_state() {
    let mut c = EngineConfig::new(SR);
    c.extended = false;
    let (mut e, _r) = engine(c);
    for (name, v) in [
        ("eqLow", 0.2),
        ("eqMid", 0.7),
        ("eqHigh", 0.9),
        ("filter", 0.3),
    ] {
        e.perform(
            &DeckAction::Param {
                deck: "L".into(),
                name: name.into(),
                value: v,
            },
            PerformCtx::default(),
        );
    }
    let p = e.current_params("L").unwrap();
    assert_eq!(
        (p.eq_low, p.eq_mid, p.eq_high, p.filter),
        (0.2, 0.7, 0.9, 0.3)
    );
    let s = e.journal_state(Default::default(), false);
    assert_eq!(
        (
            s.loop_channel.eq_low,
            s.loop_channel.eq_mid,
            s.loop_channel.eq_high,
            s.loop_channel.filter
        ),
        (0.2, 0.7, 0.9, 0.3)
    );
    assert_eq!(
        e.loop_channel.strip,
        StripSettings {
            filter: 0.3,
            eq_low: 0.2,
            eq_mid: 0.7,
            eq_high: 0.9
        }
    );
}
