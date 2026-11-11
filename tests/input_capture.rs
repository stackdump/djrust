//! Capture to loop, offline (docs/INPUT-HISTORY.md): grab the last beats of an input's recording
//! or arm a live capture, both snapped to whole beats at the app tempo (and to the master clock's
//! bars when a deck runs), saved to the loop library, and back after a restart.
//!
//! The input is a click on every beat (an impulse of 1.0), so clicks in a clip say where the
//! beats fall.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::path::PathBuf;

use djrust::control::controller::{Action, SurfaceHost};
use djrust::looplib::{wav_encode, LoopMeta};
use djrust::TrackSpec;
use djrust::host::deck::{engine, Engine, EngineConfig, Renderer};
use djrust::host::stores::captures::CaptureLibrary;

struct Counting;
thread_local! {
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
    static ON: Cell<bool> = const { Cell::new(false) };
}
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        if ON.with(|o| o.get()) {
            ALLOCS.with(|c| c.set(c.get() + 1));
        }
        System.alloc(l)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        if ON.with(|o| o.get()) {
            ALLOCS.with(|c| c.set(c.get() + 1));
        }
        System.dealloc(p, l)
    }
}
#[global_allocator]
static A: Counting = Counting;

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;
/// A beat at 120 BPM.
const BEAT: usize = 24_000;

struct Rig {
    e: Engine,
    r: Renderer,
    feed: rtrb::Producer<f32>,
    fed: usize,
    /// Click spacing in frames (a beat at the tempo the test chose).
    beat: usize,
    out: Vec<f32>,
}

impl Rig {
    fn new(bpm: f64) -> Rig {
        let mut c = EngineConfig::new(SR);
        c.extended = false;
        c.inputs = vec!["TD-3".into()];
        let (mut e, r) = engine(c);
        let (feed, _) = e.take_input_feed(0).unwrap();
        e.set_input_trim(0, 0.0);
        e.set_input_volume(0, 1.0);
        e.set_crossfader(0.5);
        e.set_free_bpm(bpm);
        e.sync_history_tempo();
        Rig {
            e,
            r,
            feed,
            fed: 0,
            beat: (60.0 / bpm * SR).round() as usize,
            out: Vec::new(),
        }
    }

    /// Feed and render `frames` (rounded down to the block), servicing loops each block.
    fn run(&mut self, frames: usize) {
        let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
        for _ in 0..frames / BLOCK {
            for k in 0..BLOCK {
                let x = if (self.fed + k) % self.beat == 7 { 1.0 } else { 0.0 };
                let _ = self.feed.push(x);
            }
            self.fed += BLOCK;
            self.r.render_block(&mut l, &mut rr);
            self.e.sync_history_tempo();
            self.e.service_loops();
            self.out.extend_from_slice(&l);
        }
    }

    fn beats(&mut self, n: f64) {
        let f = (n * self.beat as f64) as usize;
        self.run(f);
    }

    /// A net deck playing, so there is a master clock.
    fn with_deck(&mut self) -> f64 {
        self.e.set_volume("A", 1.0);
        assert!(self.e.generate("A", &TrackSpec::new("techno", 42.0, None)));
        self.e.play("A");
        self.beat = 24_000;
        self.run(BLOCK * 8);
        let bpm = self.e.deck("A").unwrap().mirror.tempo();
        self.beat = (60.0 / bpm * SR).round() as usize;
        bpm
    }
}

fn clicks(x: &[f32]) -> Vec<usize> {
    let mut v = vec![];
    let mut k = 0;
    while k < x.len() {
        if x[k].abs() > 0.3 {
            v.push(k);
            k += 2000;
        } else {
            k += 1;
        }
    }
    v
}

fn temp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("djrust-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

#[test]
fn a_grab_is_whole_beats_long_at_the_app_tempo() {
    let mut g = Rig::new(120.0);
    g.beats(6.0);
    let id = g.e.capture_grab(0, Some(4.0)).expect("a grab");
    let c = g.e.clip(&id).unwrap();
    assert_eq!(
        (c.beats, c.source.as_str(), g.e.clip_extra(&id).from_input),
        (4, "TD-3", true)
    );
    assert_eq!(c.buffer.length(), 4 * BEAT);
    assert!((c.bpm - 120.0).abs() < 1e-9, "{}", c.bpm);
    let k = clicks(&c.buffer.left);
    assert_eq!(k.len(), 4, "{k:?}");
    for w in k.windows(2) {
        assert_eq!(w[1] - w[0], BEAT);
    }
    // it loops at once on the row
    assert!(g.e.loop_state(&id) != djrust::host::deck::loops::LoopState::Off);
}

#[test]
fn the_length_follows_the_tempo_and_snaps_to_whole_beats() {
    let mut g = Rig::new(100.0);
    g.beats(8.0);
    let beat = 28_800;
    let a = g.e.capture_grab(0, Some(4.0)).unwrap();
    assert_eq!(g.e.clip(&a).unwrap().buffer.length(), 4 * beat);
    let b = g.e.capture_grab(0, Some(2.6)).unwrap(); // 3 beats
    assert_eq!(g.e.clip(&b).unwrap().beats, 3);
    assert_eq!(g.e.clip(&b).unwrap().buffer.length(), 3 * beat);
    let c = g.e.capture_grab(0, Some(0.2)).unwrap(); // at least a beat
    assert_eq!(g.e.clip(&c).unwrap().beats, 1);
    assert!((g.e.clip(&c).unwrap().bpm - 100.0).abs() < 1e-9);
}

#[test]
fn a_grab_with_too_little_recorded_is_refused() {
    let mut g = Rig::new(120.0);
    g.beats(2.0);
    assert!(g.e.capture_grab(0, Some(8.0)).is_none());
    assert!(g.e.loops.row.clips.is_empty());
    assert!(g.e.capture_grab(5, None).is_none()); // no such input
}

#[test]
fn a_grab_ends_on_the_masters_bar_line_when_a_deck_runs() {
    let mut g = Rig::new(120.0);
    let bpm = g.with_deck();
    g.beats(11.3);
    let id = g.e.capture_grab(0, Some(4.0)).expect("a grab");
    let c = g.e.clip(&id).unwrap();
    assert_eq!(c.beats, 4);
    let fpb = 60.0 / bpm * SR;
    assert!(
        (c.buffer.length() as f64 - 4.0 * fpb).abs() <= 1.0,
        "{} vs {}",
        c.buffer.length(),
        4.0 * fpb
    );
    // snapped to the deck's clock: it started on a bar line
    assert!(c.start_beat.rem_euclid(4.0).abs() < 1e-6, "{}", c.start_beat);
}

#[test]
fn an_armed_capture_runs_to_the_bar_after_the_release() {
    let mut g = Rig::new(120.0);
    g.beats(1.0);
    assert!(g.e.capture_start(0));
    assert!(!g.e.capture_start(0), "one at a time");
    g.beats(9.0); // 2.25 bars
    assert_eq!(g.e.capture_progress(0), Some((3, false)));
    assert!(g.e.capture_end(0, false));
    g.beats(4.0);
    assert!(g.e.capture_progress(0).is_none());
    let c = g.e.loops.row.clips.last().expect("a clip");
    assert_eq!((c.beats, c.bars()), (12, 3));
    assert_eq!(c.buffer.length(), 12 * BEAT);
    let k = clicks(&c.buffer.left);
    assert_eq!(k.len(), 12, "{k:?}");
    for w in k.windows(2) {
        assert_eq!(w[1] - w[0], BEAT);
    }
}

#[test]
fn a_tap_of_the_arm_control_grabs_instead() {
    let mut g = Rig::new(120.0);
    g.beats(8.0);
    g.e.inputs[0].loop_beats = 2.0;
    assert!(g.e.capture_arm(0, true));
    g.run(BLOCK * 4); // ~20 ms
    assert!(g.e.capture_arm(0, false));
    assert!(g.e.capture_progress(0).is_none(), "the armed take was dropped");
    g.beats(1.0);
    let c = g.e.loops.row.clips.last().expect("a grab");
    assert_eq!(c.beats, 2);
}

#[test]
fn an_armed_capture_waits_for_the_next_bar_of_the_clock() {
    let mut g = Rig::new(120.0);
    g.with_deck();
    g.beats(5.5);
    assert!(g.e.capture_start(0));
    assert!(g.e.capture_waiting(0), "mid-bar: it begins on the next one");
    g.beats(10.0);
    assert!(g.e.capture_end(0, false));
    g.beats(8.0);
    let c = g.e.loops.row.clips.last().expect("a clip");
    assert!(g.e.clip_extra(&c.id).from_input);
    assert_eq!(c.beats % 4, 0);
    assert!(c.start_beat.rem_euclid(4.0).abs() < 1e-6, "{}", c.start_beat);
    assert!(c.beats >= 4);
}

#[test]
fn captures_persist_and_come_back_after_a_restart() {
    let dir = temp_dir("lib");
    let mut g = Rig::new(120.0);
    g.e.set_loop_library(Some(CaptureLibrary::new(&dir)));
    g.beats(6.0);
    let id = g.e.capture_grab(0, Some(4.0)).unwrap();
    let before = g.e.clip(&id).unwrap().buffer.left.clone();
    assert!(g.e.clip_extra(&id).saved);
    let lib = CaptureLibrary::new(&dir);
    let metas = lib.list();
    assert_eq!(metas.len(), 1);
    let m = &metas[0];
    assert_eq!((m.id.as_str(), m.beats, m.source.as_str(), m.kind.as_str()), (id.as_str(), 4, "TD-3", "input"));
    assert!((m.bpm - 120.0).abs() < 1e-9 && m.sample_rate == SR && m.row);
    assert!(dir.join(format!("{id}.wav")).is_file());

    // a new run
    let mut h = Rig::new(120.0);
    h.e.set_loop_library(Some(CaptureLibrary::new(&dir)));
    assert_eq!(h.e.library_load_row(), 1);
    let c = h.e.clip(&id).unwrap();
    assert_eq!((c.beats, c.source.as_str()), (4, "TD-3"));
    assert!((c.bpm - 120.0).abs() < 1e-9);
    assert_eq!(c.buffer.left, before, "bit-identical");
    // loaded, not playing; then played and mixed through the loop channel
    assert_eq!(h.e.loop_state(&id), djrust::host::deck::loops::LoopState::Off);
    h.e.set_looping(&id, true);
    h.run(BLOCK * 400);
    assert!(h.out.iter().any(|x| x.abs() > 0.1), "the loop sounds");

    // taken off the row: stays in the library, not back at the next start
    h.e.remove_clip(&id);
    let mut k = Rig::new(120.0);
    k.e.set_loop_library(Some(CaptureLibrary::new(&dir)));
    assert_eq!(k.e.library_load_row(), 0);
    assert_eq!(k.e.library_entries().len(), 1);
    assert!(k.e.library_load(&id));
    assert!(!k.e.library_load(&id), "already there");
    // deleted for good
    assert!(k.e.library_delete(&id));
    assert!(k.e.library_entries().is_empty() && k.e.clip(&id).is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_loop_saved_at_another_rate_loads_at_this_one_keeping_its_beats() {
    let dir = temp_dir("rate");
    let lib = CaptureLibrary::new(&dir);
    // 2 beats at 120 BPM, 44.1 kHz
    let n = 44_100;
    let x: Vec<f32> = (0..n).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
    let meta = LoopMeta {
        id: "feedbeef".into(),
        title: "old".into(),
        bpm: 120.0,
        beats: 2,
        sample_rate: 44_100.0,
        frames: n,
        channels: 1,
        source: "mic".into(),
        kind: "input".into(),
        created: 1,
        row: true,
        from: String::new(),
    };
    lib.save(&meta, &x, None).unwrap();
    assert!(wav_encode(44_100, &x, None).len() > 44);
    let mut g = Rig::new(120.0);
    g.e.set_loop_library(Some(CaptureLibrary::new(&dir)));
    assert_eq!(g.e.library_load_row(), 1);
    let c = g.e.clip("feedbeef").unwrap();
    assert_eq!(c.beats, 2);
    assert_eq!(c.buffer.length(), 48_000);
    assert!((c.bpm - 120.0).abs() < 1e-9);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_damaged_library_entry_is_skipped_not_fatal() {
    let dir = temp_dir("bad");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("junk.json"), "{ not json").unwrap();
    std::fs::write(dir.join("nowav.json"), r#"{"id":"nowav","bpm":120,"beats":4,"sampleRate":48000}"#).unwrap();
    std::fs::write(dir.join("badwav.json"), r#"{"id":"badwav","bpm":120,"beats":4,"sampleRate":48000}"#).unwrap();
    std::fs::write(dir.join("badwav.wav"), b"RIFFxxxx").unwrap();
    let lib = CaptureLibrary::new(&dir);
    assert_eq!(lib.list().len(), 1, "only the one with a wav is listed");
    assert!(lib.load("badwav", SR).is_none());
    assert!(lib.save(&LoopMeta { id: "../x".into(), ..lib.list()[0].clone() }, &[0.0], None).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_capture_actions_drive_the_engine_like_any_other_binding() {
    let mut g = Rig::new(120.0);
    g.beats(10.0);
    // named input, a length
    SurfaceHost::perform(&mut g.e, Action::CaptureGrab { input: Some("TD-3".into()), beats: Some(2.0) });
    assert_eq!(g.e.loops.row.clips.last().unwrap().beats, 2);
    // a name that is not an input does nothing
    let n = g.e.loops.row.clips.len();
    SurfaceHost::perform(&mut g.e, Action::CaptureGrab { input: Some("nope".into()), beats: None });
    assert_eq!(g.e.loops.row.clips.len(), n);
    // the length steps
    assert_eq!(g.e.inputs[0].loop_beats, 4.0);
    SurfaceHost::perform(&mut g.e, Action::CaptureBeats { input: None, delta: 1 });
    assert_eq!(g.e.inputs[0].loop_beats, 8.0);
    SurfaceHost::perform(&mut g.e, Action::CaptureBeats { input: None, delta: -1 });
    SurfaceHost::perform(&mut g.e, Action::CaptureBeats { input: None, delta: -1 });
    assert_eq!(g.e.inputs[0].loop_beats, 2.0);
    // the clip row: toggle the first clip out and in
    let id = g.e.loops.row.clips[0].id.clone();
    SurfaceHost::perform(&mut g.e, Action::ClipToggle { slot: 0, on: false });
    g.beats(8.0);
    assert_eq!(g.e.loop_state(&id), djrust::host::deck::loops::LoopState::Off);
    SurfaceHost::perform(&mut g.e, Action::ClipToggle { slot: 0, on: true });
    assert_ne!(g.e.loop_state(&id), djrust::host::deck::loops::LoopState::Off);
    assert_eq!(SurfaceHost::clip_count(&g.e), g.e.loops.row.clips.len());
}

#[test]
fn an_aimed_decks_loop_record_button_is_capture_to_loop() {
    let mut g = Rig::new(120.0);
    g.beats(10.0);
    g.e.set_history_jog(Some(("B", djrust::host::deck::strip_recall::RecallId::Input(0))));
    // tap: the last beats
    SurfaceHost::perform(&mut g.e, Action::LoopRecord { deck: "B".into(), on: true });
    g.run(BLOCK * 2);
    SurfaceHost::perform(&mut g.e, Action::LoopRecord { deck: "B".into(), on: false });
    g.beats(0.5);
    assert_eq!(g.e.loops.row.clips.len(), 1);
    assert_eq!(g.e.loops.row.clips[0].beats, 4);
    // hold: a live capture, to the bar after the release (with the first loop gone, so no loop
    // clock makes it wait for a bar)
    let first = g.e.loops.row.clips[0].id.clone();
    g.e.remove_clip(&first);
    g.beats(2.0);
    SurfaceHost::perform(&mut g.e, Action::LoopRecord { deck: "B".into(), on: true });
    g.beats(5.0);
    SurfaceHost::perform(&mut g.e, Action::LoopRecord { deck: "B".into(), on: false });
    g.beats(6.0);
    assert_eq!(g.e.loops.row.clips.len(), 1);
    assert_eq!(g.e.loops.row.clips[0].beats, 8);
}

#[test]
fn the_library_slots_load_and_save_from_a_binding() {
    let dir = temp_dir("slots");
    let mut g = Rig::new(120.0);
    g.beats(6.0);
    // no library: nothing to save to
    let id = g.e.capture_grab(0, Some(4.0)).unwrap();
    assert!(!g.e.clip_extra(&id).saved);
    g.e.set_loop_library(Some(CaptureLibrary::new(&dir)));
    assert_eq!(SurfaceHost::library_count(&g.e), 0);
    SurfaceHost::perform(&mut g.e, Action::LibrarySave { slot: 0 });
    assert_eq!(SurfaceHost::library_count(&g.e), 1);
    assert!(g.e.clip_extra(&id).saved);
    g.e.remove_clip(&id);
    assert!(g.e.clip(&id).is_none());
    SurfaceHost::perform(&mut g.e, Action::LibraryLoad { slot: 0 });
    assert!(g.e.clip(&id).is_some());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recording_the_tap_does_not_allocate_on_the_render_thread() {
    let mut g = Rig::new(120.0);
    g.beats(2.0);
    let (mut l, mut rr) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    ALLOCS.with(|c| c.set(0));
    ON.with(|o| o.set(true));
    for _ in 0..400 {
        g.r.render_block(&mut l, &mut rr);
    }
    ON.with(|o| o.set(false));
    assert_eq!(ALLOCS.with(|c| c.get()), 0);
}

#[test]
fn an_input_latency_offset_reads_the_recording_later_so_the_loop_lands_on_the_beat() {
    let mut g = Rig::new(120.0);
    g.with_deck();
    g.beats(11.3);
    let all_clicks = |g: &mut Rig, ms: f64| {
        assert!(g.e.set_input_latency(0, ms));
        let id = g.e.capture_grab(0, Some(4.0)).expect("a grab");
        clicks(&g.e.clip(&id).unwrap().buffer.left)
    };
    let a = all_clicks(&mut g, 0.0);
    let b = all_clicks(&mut g, 20.0);
    // 20 ms = 960 frames later in the recording: every click sits that much earlier in the clip
    for c in a.iter().filter(|c| **c >= 960) {
        assert!(b.contains(&(c - 960)), "{c} not shifted by 960 in {b:?}");
    }
    assert!(a.iter().any(|c| *c >= 960));
    // bounds
    assert!(g.e.set_input_latency(0, 9999.0));
    assert_eq!(g.e.inputs[0].latency_ms, 500.0);
    assert!(!g.e.set_input_latency(0, f64::NAN));
    assert!(!g.e.set_input_latency(7, 1.0));
}

#[test]
fn the_input_latency_action_is_a_knob_binding() {
    let mut g = Rig::new(120.0);
    let mut args = std::collections::BTreeMap::new();
    args.insert("input".to_string(), "TD-3".to_string());
    args.insert("range".to_string(), "0,100".to_string());
    g.e.apply_action(Action::Target {
        id: "inputLatency".into(),
        deck: None,
        slot: None,
        index: 0,
        value: 0.5,
        pressed: true,
        args,
    });
    assert!((g.e.inputs[0].latency_ms - 50.0).abs() < 1e-9);
}
