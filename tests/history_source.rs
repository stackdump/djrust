//! The history as a source (docs/INPUT-HISTORY.md, "Roll, slip and loops on the grid"): on a
//! deck or the master, a roll, slip mode and a loop ending at a frame are moves of the strip's
//! history read head, so the strip plays on underneath and going back to live rejoins it
//! exactly. Each test renders a twin engine that makes no move: after the rejoin the two are
//! the same, sample for sample.

use djrust::fx::insert::{InsertKind, InsertParam};
use djrust::history::Mode;
use djrust::host::deck::roll::HistoryRefused;
use djrust::host::deck::strip_recall::{RecallId, StripId};
use djrust::host::deck::{engine, Engine, EngineConfig, Renderer};
use djrust::TrackSpec;

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;
const A: RecallId = RecallId::Strip(StripId::Deck(0));
const M: RecallId = RecallId::Strip(StripId::Master);

struct Rig {
    e: Engine,
    r: Renderer,
    out: Vec<f32>,
}

impl Rig {
    fn new() -> Rig {
        let mut c = EngineConfig::new(SR);
        c.extended = false;
        c.history_beats = Some(8.0);
        c.strip_history = true;
        let (mut e, r) = engine(c);
        e.set_free_bpm(120.0);
        e.sync_history_tempo();
        Rig { e, r, out: Vec::new() }
    }

    /// Render `blocks` blocks, keeping the left channel in `out`.
    fn run(&mut self, blocks: usize) {
        let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
        self.out.clear();
        for _ in 0..blocks {
            self.r.render_block(&mut l, &mut rr);
            self.e.sync_history_tempo();
            self.e.service_loops();
            self.out.extend_from_slice(&l);
        }
    }

    /// Deck A playing a track, alone in the master, with beats in its history.
    fn deck_a(mut self) -> Rig {
        self.e.set_crossfader(0.0);
        self.e.set_volume("A", 1.0);
        assert!(self.e.generate("A", &TrackSpec::new("techno", 42.0, None)));
        self.e.play("A");
        self.run(800);
        self
    }

    /// Frames in `beats` at the master tempo (the track's own, not the free clock's).
    fn frames(&self, beats: f64) -> usize {
        let (_, bpm) = self.e.master_tempo();
        (SR * 60.0 / bpm * beats).round() as usize
    }

    fn level(&self, from: usize, to: usize) -> f32 {
        let x = &self.out[from..to.min(self.out.len())];
        (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }
}

/// Does `x` repeat with period `p` (frames) over `[from, to)`?
fn repeats(x: &[f32], p: usize, from: usize, to: usize) -> bool {
    (from..to).all(|k| (x[k] - x[k + p]).abs() < 1e-5)
}

#[test]
fn a_roll_on_a_deck_plays_on_underneath_and_rejoins_where_the_deck_would_have_been() {
    let mut g = Rig::new().deck_a();
    let mut twin = Rig::new().deck_a();
    assert_eq!(g.out, twin.out, "the same track renders the same");

    assert!(g.e.history_roll(A, true, Some(0.25)));
    assert!(g.e.history_rolling(A));
    g.run(100);
    twin.run(100);
    assert_eq!(g.e.recall_view(A).unwrap().history_mode(), Mode::Loop);
    // the roll repeats a quarter beat
    let q = g.frames(0.25);
    assert!(repeats(&g.out, q, 1000, 1000 + 2 * q), "the roll loops a 1/4 beat ({q} frames)");
    assert_ne!(g.out, twin.out);

    assert!(g.e.history_roll(A, false, None));
    assert!(!g.e.history_rolling(A));
    g.run(4);
    twin.run(4);
    assert_eq!(g.e.recall_view(A).unwrap().history_mode(), Mode::Live);
    // past the 64-frame fade (and the message's block), the deck is where it would have been
    assert_eq!(&g.out[2 * BLOCK..], &twin.out[2 * BLOCK..]);
    g.run(200);
    twin.run(200);
    assert_eq!(g.out, twin.out, "rejoined sample for sample");
}

#[test]
fn a_roll_on_the_master_loops_on_the_grid() {
    let mut g = Rig::new().deck_a();
    assert!(g.e.history_roll(M, true, Some(0.5)));
    g.run(150);
    let h = g.frames(0.5);
    assert!(repeats(&g.out, h, 2000, 2000 + 2 * h), "a 1/2-beat roll of the master ({h} frames)");
    assert!(g.e.history_roll(M, false, None));
    g.run(4);
    assert_eq!(g.e.recall_view(M).unwrap().history_mode(), Mode::Live);
}

#[test]
fn a_roll_lengths_step_and_a_held_roll_takes_a_new_one() {
    let mut g = Rig::new().deck_a();
    assert_eq!(g.e.history_roll_beats(A), 0.25);
    assert!(g.e.set_history_roll_beats(A, 0.125));
    assert!(g.e.history_roll(A, true, None));
    g.run(60);
    let e = g.frames(0.125);
    assert!(repeats(&g.out, e, 1000, 1000 + 2 * e), "a 1/8-beat roll ({e} frames)");
    assert!(g.e.set_history_roll_beats(A, 1.0));
    g.run(250);
    let b = g.frames(1.0);
    assert!(repeats(&g.out, b, 2000, 2000 + b), "re-rolled to a beat ({b} frames)");
    assert!(g.e.history_roll(A, false, None));
}

#[test]
fn slip_brings_a_released_jog_back_to_live() {
    // without slip, a released platter plays on as a delay of where it was left
    let mut g = Rig::new().deck_a();
    assert!(g.e.input_touch(A));
    g.e.input_scrub_beats(A, -1.0);
    g.run(40);
    g.e.input_release(A);
    g.run(20);
    assert_eq!(g.e.recall_view(A).unwrap().history_mode(), Mode::Play);

    // with it, the release rejoins the deck, which played on underneath
    let mut g = Rig::new().deck_a();
    let mut twin = Rig::new().deck_a();
    assert!(g.e.set_history_slip(A, true));
    assert!(g.e.history_slip(A));
    assert!(g.e.input_touch(A));
    g.e.input_scrub_beats(A, -1.0);
    g.run(40);
    twin.run(40);
    g.e.input_release(A);
    g.run(4);
    twin.run(4);
    assert_eq!(g.e.recall_view(A).unwrap().history_mode(), Mode::Live);
    g.run(100);
    twin.run(100);
    assert_eq!(g.out, twin.out, "slip rejoined sample for sample");
}

#[test]
fn a_loop_ending_at_a_frame_ends_only_by_its_own_id() {
    let mut g = Rig::new().deck_a();
    let fpb = g.frames(1.0) as i64;
    // two beats ending one beat ago: the failsafe's shape
    let end = g.e.loop_frames() - fpb;
    let id = g.e.history_loop_ending_at(M, 2.0, end, 2048).expect("held");
    g.run(10);
    assert_eq!(g.e.history_loop_id(M), Some(id));
    // a roll asked later takes the strip; the earlier asker's live leaves it alone
    assert!(g.e.history_roll(M, true, Some(0.25)));
    g.run(4);
    let roll = g.e.history_loop_id(M).expect("rolling");
    assert_ne!(roll, id);
    assert!(g.e.history_live_if(M, id, 2048));
    g.run(4);
    assert_eq!(g.e.recall_view(M).unwrap().history_mode(), Mode::Loop);
    assert_eq!(g.e.history_loop_id(M), Some(roll));
    assert!(g.e.history_roll(M, false, None));
    g.run(4);
    assert_eq!(g.e.recall_view(M).unwrap().history_mode(), Mode::Live);
    assert_eq!(g.e.history_loop_id(M), None);

    // more than the history holds, or no history at all, is refused
    let held = match g.e.history_loop_ending_at(M, 64.0, g.e.loop_frames(), 0) {
        Err(HistoryRefused::NotHeld { held_beats }) => held_beats,
        o => panic!("{o:?}"),
    };
    assert!(held > 4.0 && held <= 8.01, "{held}");
    let (mut plain, _) = engine(EngineConfig::new(SR));
    assert_eq!(plain.history_loop_ending_at(M, 1.0, 0, 0), Err(HistoryRefused::NoHistory));
}

#[test]
fn the_same_moves_render_the_same() {
    let moves = |g: &mut Rig| {
        let mut all = vec![];
        g.e.history_roll(A, true, Some(0.25));
        g.run(37);
        all.extend_from_slice(&g.out);
        g.e.history_roll(A, false, None);
        g.e.set_history_slip(A, true);
        g.e.input_touch(A);
        g.e.input_scrub_beats(A, -0.5);
        g.run(23);
        all.extend_from_slice(&g.out);
        g.e.input_release(A);
        g.e.history_roll(M, true, Some(1.0));
        g.run(61);
        all.extend_from_slice(&g.out);
        g.e.history_roll(M, false, None);
        g.run(30);
        all.extend_from_slice(&g.out);
        all
    };
    let a = moves(&mut Rig::new().deck_a());
    let b = moves(&mut Rig::new().deck_a());
    assert_eq!(a, b);
}

#[test]
fn echo_out_is_the_delay_insert_left_ringing_on_a_stopped_deck() {
    // the composition: the deck's delay insert on, then the deck stops; the echoes ring out
    let tail = |delay: bool| {
        let mut g = Rig::new().deck_a();
        if delay {
            assert!(g.e.set_insert_on("A", InsertKind::Delay, true));
            assert!(g.e.set_insert_param("A", InsertParam::DelayFeedback, 0.7));
        }
        g.run(200);
        g.e.pause("A");
        g.run(400); // ~2 s
        g.level(48_000, 96_000)
    };
    // the stopped deck's own room fades (its strip reverb); the echoes ring on well above it
    let dry = tail(false);
    let echo = tail(true);
    assert!(echo > 2.0 * dry, "the delay's tail rings on: {echo} vs {dry}");
}

#[test]
fn the_history_bindings_roll_and_slip_any_strip() {
    use djrust::control::controller::{Action, SurfaceHost};
    let mut g = Rig::new().deck_a();
    // `historyLoop` with op=roll: held, then let go
    SurfaceHost::perform(&mut g.e, Action::HistoryRoll { input: Some("M".into()), beats: Some(0.5), on: true });
    g.run(4);
    assert!(g.e.history_rolling(M));
    assert_eq!(g.e.recall_view(M).unwrap().history_mode(), Mode::Loop);
    SurfaceHost::perform(&mut g.e, Action::HistoryRoll { input: Some("M".into()), beats: None, on: false });
    g.run(4);
    assert_eq!(g.e.recall_view(M).unwrap().history_mode(), Mode::Live);
    // `historyLive` with op=slip toggles slip mode on the strip named
    SurfaceHost::perform(&mut g.e, Action::HistorySlip { input: Some("A".into()), on: None });
    assert!(g.e.history_slip(A));
    SurfaceHost::perform(&mut g.e, Action::HistorySlip { input: Some("A".into()), on: None });
    assert!(!g.e.history_slip(A));
    SurfaceHost::perform(&mut g.e, Action::HistorySlip { input: Some("A".into()), on: Some(true) });
    assert!(g.e.history_slip(A));
    // the moves are in the action trace, in words, with a `history` live op that replays them
    let ops = djrust::json::JsonValue::Object(djrust::json::JsonObject::from_pairs([
        ("strip", djrust::json::JsonValue::str("M")),
        ("action", djrust::json::JsonValue::str("roll")),
        ("beats", djrust::json::JsonValue::Number(0.25)),
    ]));
    assert_eq!(g.e.apply_live_op("history", &ops), Ok(()));
    g.run(4);
    assert!(g.e.history_rolling(M));
}
