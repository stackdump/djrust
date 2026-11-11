//! The browser's audition (docs/BROWSER.md): a file on the headphone cue, never the master.

use djrust::deck::turntable::SampleBuffer;
use djrust::host::deck::{engine, EngineConfig};

const SR: f64 = 48_000.0;
const N: usize = 512;

fn tone(frames: usize) -> SampleBuffer {
    let l: Vec<f32> = (0..frames).map(|i| (i as f32 * 0.03).sin() * 0.5).collect();
    SampleBuffer::new(l, None, SR)
}

fn peak(x: &[f32]) -> f32 {
    x.iter().fold(0f32, |p, v| p.max(v.abs()))
}

#[test]
fn an_audition_plays_on_the_cue_only_and_loops_until_stopped() {
    let (mut e, mut r) = engine(EngineConfig::new(SR));
    e.set_headphone_mix(0.0);
    e.set_headphone_level(1.0);
    assert!(e.audition_play(tone(700), "tone", Some("/x/tone.wav")));
    let (mut l, mut rr, mut cl, mut cr) = (vec![0f32; N], vec![0f32; N], vec![0f32; N], vec![0f32; N]);
    for _ in 0..4 {
        r.render_with_cue(&mut l, &mut rr, Some((&mut cl, &mut cr)));
        assert_eq!(peak(&l) + peak(&rr), 0.0, "nothing on the master");
        assert!(peak(&cl) > 0.3 && peak(&cr) > 0.3, "the cue has it, past its 700 frames");
    }
    assert!(e.audition.playing());
    assert!(e.audition.cue_routed());
    assert_eq!(e.audition.title.as_deref(), Some("tone"));
    e.audition_stop();
    r.render_with_cue(&mut l, &mut rr, Some((&mut cl, &mut cr)));
    assert_eq!(peak(&cl), 0.0);
    assert!(!e.audition.playing());
}

#[test]
fn without_a_cue_output_an_audition_is_silent_and_says_so() {
    let (mut e, mut r) = engine(EngineConfig::new(SR));
    e.audition_play(tone(4000), "tone", None);
    let (mut l, mut rr) = (vec![0f32; N], vec![0f32; N]);
    r.render_block(&mut l, &mut rr);
    assert_eq!(peak(&l) + peak(&rr), 0.0);
    assert!(e.audition.playing());
    assert!(!e.audition.cue_routed());
}
