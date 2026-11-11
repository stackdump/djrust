//! The FX unit from the main thread (`Deck.setFx` / `fxKill` / `fxReset`): what the window's
//! FX bar turns. The lowpass darkens the deck, kill bypasses it, reset brings it back.

use djrust::fx::unit::FxParam;
use djrust::TrackSpec;
use djrust::host::deck::{engine, EngineConfig, Renderer};

/// RMS of the first difference: the top end.
fn brightness(r: &mut Renderer, blocks: usize) -> f64 {
    let (mut l, mut rr) = (vec![0f32; 512], vec![0f32; 512]);
    let (mut e, mut prev, mut n) = (0.0, 0f32, 0usize);
    for _ in 0..blocks {
        r.render_block(&mut l, &mut rr);
        for &v in &l {
            e += ((v - prev) as f64).powi(2);
            prev = v;
            n += 1;
        }
    }
    (e / n as f64).sqrt()
}

#[test]
fn the_fx_unit_lowpass_darkens_the_deck_and_kill_bypasses_it() {
    let mut c = EngineConfig::new(48_000.0);
    c.extended = false;
    let (mut e, mut r) = engine(c);
    e.set_crossfader(0.0);
    e.set_volume("A", 1.0);
    assert!(e.generate("A", &TrackSpec::new("techno", 42.0, None)));
    e.play("A");
    brightness(&mut r, 40);
    let open = brightness(&mut r, 200);
    assert!(open > 0.0);
    e.set_fx("A", FxParam::Lp, 0.1);
    brightness(&mut r, 20); // the glide
    let dark = brightness(&mut r, 200);
    assert!(dark < open * 0.5, "lowpass: {dark} vs {open}");
    e.fx_kill("A", true);
    brightness(&mut r, 20);
    let killed = brightness(&mut r, 200);
    assert!(killed > dark * 1.5, "kill: {killed} vs {dark}");
    e.fx_kill("A", false);
    e.fx_reset("A");
    assert_eq!(e.deck("A").unwrap().fx_unit.get(FxParam::Lp), 1.0);
    brightness(&mut r, 20);
    let back = brightness(&mut r, 200);
    assert!(back > dark * 1.5, "reset: {back} vs {dark}");
}

// ---- the sends: the room and the echo of a net deck ----------------------------------------

use djrust::fx::unit::FxUnitCommand;
use djrust::wave::{WaveOptions, WaveRunner};
use djrust::{compose, JsonObject};
use djrust::host::deck::{Command, DeckCore, DeckMirror};
use std::sync::Arc;

/// A deck playing `song` prepared with `opts`, and `blocks` blocks of `frames` it renders with
/// the FX unit's commands applied first: the left channel, then the right, block by block
/// (`FxUnitSendTests.render`).
fn render_deck(
    opts: &WaveOptions,
    song: &djrust::pflow::Project,
    rate: f64,
    pokes: &[(FxParam, f64)],
    blocks: usize,
) -> (Vec<f32>, bool, bool) {
    let mut core = DeckCore::new("A", rate, opts.clone(), false, Arc::new(DeckMirror::new()));
    let p = WaveRunner::prepare(song, rate, opts, 0);
    let (room, echo) = (p.space.is_some(), p.echo.is_some());
    core.apply(Command::Load(p));
    core.apply(Command::Play);
    for &(param, value) in pokes {
        core.apply(Command::FxUnit(FxUnitCommand::Set {
            param: param.index() as u8,
            value,
            snap: false,
        }));
    }
    let (mut l, mut r) = (vec![0f32; 256], vec![0f32; 256]);
    let mut out = Vec::new();
    for b in 0..blocks {
        core.render(&mut l, &mut r, (b * 256) as i64);
        out.extend_from_slice(&l);
        out.extend_from_slice(&r);
    }
    (out, room, echo)
}

fn rms_of(x: &[f32]) -> f64 {
    (x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / x.len() as f64).sqrt()
}

// FxUnitTests.FxUnitSendTests.aNetDecksRoomAndEchoAnswer: trance seed 7 at 22050 Hz, the app's
// options for the genre, 600 blocks of 256; neutral sends are the genre's own room and echo to
// the byte, and the room and echo at 1 with 0.8 feedback are heard (rms of the difference over
// a tenth of the dry rms)
#[test]
fn a_net_decks_room_and_echo_answer() {
    let rate = 22_050.0;
    let opts = WaveOptions::for_track("trance", 0.0);
    let mut ov = JsonObject::new();
    ov.set("seed", 7.0.into());
    let song = compose("trance", &ov.into());

    let (dry, room, echo) = render_deck(&opts, &song, rate, &[], 600);
    assert!(
        room && echo,
        "a trance song has a room and an echo (hasRoom, hasEcho)"
    );
    let neutral_pokes: Vec<(FxParam, f64)> = FxParam::SENDS.iter().map(|&p| (p, 0.0)).collect();
    let (neutral, _, _) = render_deck(&opts, &song, rate, &neutral_pokes, 600);
    assert!(
        neutral == dry,
        "neutral sends: the genre's room and echo exactly"
    );
    let (wet, _, _) = render_deck(
        &opts,
        &song,
        rate,
        &[
            (FxParam::Room, 1.0),
            (FxParam::Echo, 1.0),
            (FxParam::Feedback, 0.8),
        ],
        600,
    );
    let diff: Vec<f32> = wet.iter().zip(&dry).map(|(w, d)| w - d).collect();
    assert!(
        rms_of(&diff) > 0.1 * rms_of(&dry),
        "4x the sends are heard: {} vs {}",
        rms_of(&diff),
        rms_of(&dry)
    );
}

/// Each send is heard on its own: the room alone, the echo alone, the feedback alone (it only
/// reaches the repeats, so it needs the echo up to be heard).
#[test]
fn each_send_reaches_its_own_effect() {
    let rate = 22_050.0;
    let opts = WaveOptions::for_track("trance", 0.0);
    let mut ov = JsonObject::new();
    ov.set("seed", 7.0.into());
    let song = compose("trance", &ov.into());
    let (dry, _, _) = render_deck(&opts, &song, rate, &[], 400);
    let heard = |pokes: &[(FxParam, f64)]| {
        let (w, _, _) = render_deck(&opts, &song, rate, pokes, 400);
        let diff: Vec<f32> = w.iter().zip(&dry).map(|(w, d)| w - d).collect();
        rms_of(&diff) / rms_of(&dry)
    };
    let room = heard(&[(FxParam::Room, 1.0)]);
    let echo = heard(&[(FxParam::Echo, 1.0)]);
    let feedback_only = heard(&[(FxParam::Feedback, 1.0)]);
    let feedback_with_echo = heard(&[(FxParam::Echo, 0.5), (FxParam::Feedback, 1.0)]);
    let echo_half = heard(&[(FxParam::Echo, 0.5)]);
    assert!(room > 0.01, "the room send is heard ({room})");
    assert!(echo > 0.01, "the echo send is heard ({echo})");
    // the feedback boost lengthens the repeats: alone it raises the genre's own, and with the
    // echo send up it changes that render too
    assert!(
        feedback_only > 0.0,
        "feedback alone is heard ({feedback_only})"
    );
    assert!(feedback_with_echo != echo_half);
}

/// The parity path has neither room nor echo (`set_unit_sends` has nothing to send to): the FX
/// unit's send knobs leave the bytes alone.
#[test]
fn a_deck_without_a_room_or_echo_ignores_the_sends() {
    let rate = 22_050.0;
    let mut ov = JsonObject::new();
    ov.set("seed", 7.0.into());
    let song = compose("trance", &ov.into());
    let opts = WaveOptions::default();
    let (dry, room, echo) = render_deck(&opts, &song, rate, &[], 200);
    assert!(!room && !echo);
    let (wet, _, _) = render_deck(
        &opts,
        &song,
        rate,
        &[
            (FxParam::Room, 1.0),
            (FxParam::Echo, 1.0),
            (FxParam::Feedback, 1.0),
        ],
        200,
    );
    assert!(wet == dry);
}
