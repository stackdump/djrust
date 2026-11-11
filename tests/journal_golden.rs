//! Swift goldens for the journal's engine half (`tools/paritydump/journal.swift`, section
//! "journal", `tests/fixtures/swift/journal.json`).
//!
//! Three families, all on a real Swift `DJEngine(offline: true)` at 11.025 kHz, all *state*, not
//! samples (the AU strip, its EQ and reverb, and the mixer nodes make engine audio goldens
//! impossible, PORTING.md H):
//!
//! - `restore`: hand-built `JournalState`s (decks, params, voices, leaders, row settings, clips,
//!   scenes, removed, locks, the loop channel, auto-mix with a hand-off prepared and one under
//!   way: `restoreAutoMix`) restored into a fresh deterministic engine and
//!   stepped 4 blocks: the state read back with and without playheads, and each deck's
//!   {playing, tick, platter position}, held to the Swift engine.
//! - `reset`: `JournalRewindTests`' target and dirty engines: the dirty one reset to the target
//!   reads as the target, as a restore into a fresh engine does; a reset to a fresh state empties
//!   the engine; a clip missing from the library is a warning. The same scripts run here through
//!   `Engine::perform` and the same calls.
//! - `replay`: the text of a journal a real Swift `Journal` wrote (stamps fixed), replayed here
//!   from the start, from the nearest checkpoint, to a sequence number and to a frame, and
//!   `exportSession` of two slices: start, last and end sequence numbers, frames, warnings, the
//!   state and the stores as canonical text, the session's canonical text and CID.
//!
//! Clips are sawtooth audio built from integer arithmetic, so their content ids (which name
//! them) are the same on both sides. Floats are bit patterns where they are read off a deck.

mod common_fixture;

use std::sync::Arc;

use djrust::deck::clips::LoopClip;
use djrust::deck::turntable::SampleBuffer;
use djrust::deck::waveform::WaveformOverview;
use djrust::journal::file::parse;
use djrust::journal::state::JournalState;
use djrust::journal::stores::JournalStoreState;
use djrust::json::JsonValue;
use djrust::session::action::DeckAction;
use djrust::session::model::{DeckSource, DEMO_ID};
use djrust::TrackSpec;
use djrust::host::deck::{OfflineConductor, PerformCtx};
use djrust::host::replay::{export_session_from, offline_conductor, replay, JournalReplayOptions};

const R: f64 = 11_025.0;

fn golden() -> JsonValue {
    common_fixture::swift_fixture("journal")
}

/// `assert_eq!` on canonical text, saying where the two first part.
fn same_text(got: &str, want: &str, what: &str) {
    if got == want {
        return;
    }
    let at = got
        .bytes()
        .zip(want.bytes())
        .position(|(a, b)| a != b)
        .unwrap_or(got.len().min(want.len()));
    let around = |t: &str| {
        let from = at.saturating_sub(120);
        let to = (at + 160).min(t.len());
        t.get(from..to).unwrap_or("?").to_string()
    };
    panic!(
        "{what}: the texts part at byte {at}\n  here:  ...{}\n  swift: ...{}",
        around(got),
        around(want)
    );
}

fn bits(v: f64) -> String {
    format!("{:016x}", v.to_bits())
}

fn s<'a>(v: &'a JsonValue, k: &str) -> &'a str {
    v.get(k)
        .and_then(JsonValue::as_str)
        .unwrap_or_else(|| panic!("no string {k}"))
}

fn arr<'a>(v: &'a JsonValue, k: &str) -> &'a [JsonValue] {
    v.get(k)
        .and_then(JsonValue::as_array)
        .unwrap_or_else(|| panic!("no array {k}"))
}

fn n(v: &JsonValue, k: &str) -> i64 {
    v.get(k)
        .and_then(JsonValue::as_f64)
        .unwrap_or_else(|| panic!("no number {k}")) as i64
}

fn strings(v: &JsonValue, k: &str) -> Vec<String> {
    arr(v, k)
        .iter()
        .map(|x| x.as_str().unwrap().to_string())
        .collect()
}

/// `sawClip` of the dump.
fn saw_clip(n: usize, rate: f64) -> LoopClip {
    let len = (rate * 2.0) as usize;
    let period = 40 + 13 * n;
    let l: Vec<f32> = (0..len)
        .map(|k| (k % period) as f32 / period as f32 * 0.4 - 0.2)
        .collect();
    let b = SampleBuffer::new(l, None, rate);
    let w = WaveformOverview::new(&b, 40);
    LoopClip::from_bars(
        &b.content_id(),
        &format!("saw loop {n}"),
        "A",
        1,
        120.0,
        0.0,
        Arc::new(b),
        w,
        None,
    )
}

fn spec() -> TrackSpec {
    TrackSpec::new("techno", 7.0, Some("standard"))
}

fn state_text(c: &OfflineConductor, playheads: bool) -> String {
    c.engine()
        .journal_state(JournalStoreState::default(), playheads)
        .json()
        .canonical_js()
}

/// The decks' reads, as the dump writes them. The platter's position is read only off a deck that
/// holds audio: an ejected deck's is not compared. Swift's turntable stays in the render graph
/// after `eject` and coasts to a stop; here the core leaves sample mode (so that the next composed
/// track plays on the deck) and the platter stands where leaving stopped it. Nothing hears it,
/// and a checkpoint does not read it (`journalPosition` is nil on an empty deck).
fn deck_reads(c: &OfflineConductor) -> Vec<DeckRead> {
    let mut names: Vec<String> = c.engine().decks.iter().map(|d| d.name.clone()).collect();
    names.sort();
    names
        .into_iter()
        .map(|name| {
            let d = c.engine().deck(&name).unwrap();
            let held = d.sample.is_some();
            DeckRead {
                name,
                playing: d.playing,
                tick: d.mirror.tick(),
                net: d.is_net(),
                position: held.then(|| bits(d.mirror.turntable_position())),
                leader: d.leader.clone(),
                clip: d.clip_id.clone(),
            }
        })
        .collect()
}

/// One deck as the dump reads it.
struct DeckRead {
    name: String,
    playing: bool,
    tick: i64,
    net: bool,
    /// The platter's position as a bit pattern; None for a deck that holds no audio.
    position: Option<String>,
    leader: Option<String>,
    clip: Option<String>,
}

fn check_reads(want: &JsonValue, c: &OfflineConductor, what: &str) {
    let got = deck_reads(c);
    let want = arr(want, "decks");
    assert_eq!(got.len(), want.len(), "{what}: decks");
    for (g, w) in got.iter().zip(want) {
        let at = |m: &str| format!("{what}: deck {} {m}", g.name);
        assert_eq!(g.name, s(w, "name"), "{}", at("name"));
        assert_eq!(
            g.playing,
            w.get("playing").and_then(JsonValue::as_bool).unwrap(),
            "{}",
            at("playing")
        );
        assert_eq!(g.tick, n(w, "tick"), "{}", at("tick"));
        assert_eq!(g.net, s(w, "mode") == "net", "{}", at("mode"));
        if let Some(p) = &g.position {
            assert_eq!(p, s(w, "position"), "{}", at("platter position"));
        }
        assert_eq!(
            g.leader.as_deref(),
            w.get("leader").and_then(JsonValue::as_str),
            "{}",
            at("leader")
        );
        assert_eq!(
            g.clip.as_deref(),
            w.get("clip").and_then(JsonValue::as_str),
            "{}",
            at("clip")
        );
    }
}

fn step(c: &mut OfflineConductor, blocks: usize, inputs: impl FnOnce(&mut OfflineConductor)) {
    let mut inputs = Some(inputs);
    for _ in 0..blocks {
        c.step_with(|c| {
            if let Some(f) = inputs.take() {
                f(c)
            }
        });
    }
}

fn perform(c: &mut OfflineConductor, a: DeckAction) {
    c.engine_mut().perform(&a, PerformCtx::default());
}

fn ids(c: &OfflineConductor) -> (Vec<String>, Vec<String>, Vec<String>) {
    let e = c.engine();
    let mut looping = e.looping_clips();
    looping.sort();
    (
        e.loops.row.clips.iter().map(|k| k.id.clone()).collect(),
        e.loops
            .row
            .removed
            .iter()
            .map(|r| r.clip.id.clone())
            .collect(),
        looping,
    )
}

// MARK: restore

#[test]
fn restore_stands_a_fresh_engine_where_a_swift_checkpoint_says() {
    let g = golden();
    let library: Vec<LoopClip> = (1..=3).map(|k| saw_clip(k, R)).collect();
    for case in arr(&g, "restore") {
        let name = s(case, "name");
        let state = JournalState::from_json(&JsonValue::parse(s(case, "state")).unwrap())
            .unwrap_or_else(|| panic!("{name}: the state does not parse"));
        assert_eq!(
            state.json().canonical_js(),
            s(case, "state"),
            "{name}: the state re-encodes to Swift's text"
        );
        // the clips the Swift run had to hand: the same audio, named by the same content ids
        let have = strings(case, "clips");
        let clips: Vec<LoopClip> = have
            .iter()
            .map(|id| {
                library
                    .iter()
                    .find(|c| &c.id == id)
                    .unwrap_or_else(|| panic!("{name}: clip {id} is not the saw clip it names"))
                    .clone()
            })
            .collect();
        let mut c = offline_conductor(state.decks.keys().cloned().collect(), state.sample_rate);
        let warnings = c.restore_state(&state, &|id| clips.iter().find(|k| k.id == id).cloned());
        step(&mut c, 4, |_| {});
        assert_eq!(c.dropped(), 0, "{name}: the ring held it");
        assert_eq!(warnings, strings(case, "warnings"), "{name}: warnings");
        same_text(
            &state_text(&c, false),
            s(case, "after"),
            &format!("{name}: state"),
        );
        same_text(
            &state_text(&c, true),
            s(case, "afterHeads"),
            &format!("{name}: state with playheads"),
        );
        check_reads(case, &c, name);
        let (row, removed, looping) = ids(&c);
        assert_eq!(row, strings(case, "rowClips"), "{name}: row");
        assert_eq!(removed, strings(case, "removed"), "{name}: removed");
        assert_eq!(looping, strings(case, "looping"), "{name}: looping");
    }
}

// MARK: reset

fn rewind_engine() -> OfflineConductor {
    offline_conductor(vec!["A".into(), "B".into()], R)
}

fn demo_load(deck: &str, title: &str) -> DeckAction {
    DeckAction::Load {
        deck: deck.into(),
        source: DeckSource::Sample {
            id: DEMO_ID.into(),
            title: title.into(),
            grid: None,
        },
    }
}

fn net_load(deck: &str) -> DeckAction {
    DeckAction::Load {
        deck: deck.into(),
        source: DeckSource::Net(spec()),
    }
}

fn rewind_target(c: &mut OfflineConductor, clips: &[LoopClip]) -> JournalState {
    let cs = clips.to_vec();
    step(c, 4, |c| {
        perform(c, demo_load("A", "demo"));
        perform(c, net_load("B"));
        c.engine_mut().restore_clips(&cs);
        perform(
            c,
            DeckAction::ClipRemove {
                id: cs[2].id.clone(),
            },
        );
        perform(
            c,
            DeckAction::ClipGain {
                id: cs[0].id.clone(),
                value: 0.7,
            },
        );
        perform(
            c,
            DeckAction::ClipMute {
                id: cs[1].id.clone(),
                on: true,
            },
        );
        perform(
            c,
            DeckAction::Play {
                deck: "A".into(),
                on: true,
            },
        );
        perform(c, DeckAction::Crossfader(0.3));
        perform(c, DeckAction::LoopQuantize { beats: 8 });
        perform(
            c,
            DeckAction::Lock {
                key: "A.volume".into(),
                on: true,
            },
        );
        perform(
            c,
            DeckAction::Param {
                deck: "B".into(),
                name: "eqLow".into(),
                value: 0.2,
            },
        );
    });
    c.engine_mut().start_loop(&clips[0].id, true);
    c.engine_mut().save_scene("Scene 1");
    step(c, 20, |_| {});
    c.engine()
        .journal_state(JournalStoreState::default(), false)
}

fn rewind_dirty(c: &mut OfflineConductor, clips: &[LoopClip], new_clip: &LoopClip) {
    let (cs, nc) = (clips.to_vec(), new_clip.clone());
    step(c, 4, |c| {
        c.engine_mut().restore_clips(std::slice::from_ref(&nc));
        perform(
            c,
            DeckAction::ClipLoop {
                id: nc.id.clone(),
                on: true,
            },
        );
        perform(
            c,
            DeckAction::ClipRemove {
                id: cs[0].id.clone(),
            },
        );
        perform(
            c,
            DeckAction::ClipRestore {
                id: cs[2].id.clone(),
                at: Some(0),
            },
        );
        perform(
            c,
            DeckAction::ClipSolo {
                id: cs[1].id.clone(),
                on: true,
            },
        );
        perform(
            c,
            DeckAction::ClipKeyLock {
                id: cs[1].id.clone(),
                on: true,
            },
        );
        perform(c, net_load("A"));
        perform(c, demo_load("B", "d"));
        perform(
            c,
            DeckAction::Play {
                deck: "B".into(),
                on: true,
            },
        );
        perform(
            c,
            DeckAction::Follow {
                deck: "A".into(),
                leader: Some("B".into()),
            },
        );
        perform(
            c,
            DeckAction::RoleDisable {
                deck: "A".into(),
                role: "kick".into(),
                on: true,
            },
        );
        perform(
            c,
            DeckAction::Param {
                deck: "A".into(),
                name: "volume".into(),
                value: 0.2,
            },
        );
        perform(c, DeckAction::Crossfader(0.9));
        perform(c, DeckAction::Master(0.4));
        perform(
            c,
            DeckAction::Lock {
                key: "A.volume".into(),
                on: false,
            },
        );
        perform(
            c,
            DeckAction::Lock {
                key: "B.filter".into(),
                on: true,
            },
        );
        perform(c, DeckAction::LoopQuantize { beats: 16 });
        perform(c, DeckAction::LoopFxLink { on: true });
        perform(
            c,
            DeckAction::LoopFade {
                mode: djrust::journal::state::LOOP_FADES
                    .last()
                    .unwrap()
                    .to_string(),
            },
        );
        perform(c, DeckAction::TapBars { bars: Some(4) });
        perform(
            c,
            DeckAction::SceneSave {
                name: "Scene 2".into(),
            },
        );
        perform(
            c,
            DeckAction::SceneDelete {
                name: "Scene 1".into(),
            },
        );
    });
    step(c, 20, |_| {});
}

fn reset_case<'a>(g: &'a JsonValue, name: &str) -> &'a JsonValue {
    arr(g, "reset")
        .iter()
        .find(|c| s(c, "name") == name)
        .unwrap_or_else(|| panic!("no reset case {name}"))
}

#[test]
fn a_reset_reaches_the_state_a_swift_engine_does() {
    let g = golden();
    let clips: Vec<LoopClip> = (1..=3).map(|k| saw_clip(k, R)).collect();
    let late = saw_clip(9, R);
    let find = |id: &str| clips.iter().find(|c| c.id == id).cloned();

    // a dirty engine reset equals the target, and stays there as it renders
    let case = reset_case(&g, "a dirty engine reset equals the target");
    let mut c = rewind_engine();
    let want = rewind_target(&mut c, &clips);
    assert_eq!(want.json().canonical_js(), s(case, "want"), "the target");
    rewind_dirty(&mut c, &clips, &late);
    same_text(&state_text(&c, false), s(case, "dirty"), "the dirty engine");
    let warnings = c.reset_to(&want, &find);
    assert_eq!(warnings, strings(case, "warnings"));
    same_text(&state_text(&c, false), s(case, "after"), "after the reset");
    check_reads(case, &c, "after the reset");
    let (row, removed, _) = ids(&c);
    assert_eq!(row, strings(case, "rowClips"));
    assert_eq!(removed, strings(case, "removed"));
    assert_eq!(
        c.engine()
            .scenes()
            .iter()
            .map(|s| s.name.clone())
            .collect::<Vec<_>>(),
        strings(case, "scenes")
    );
    assert_eq!(
        c.engine().clip(&late.id).is_none(),
        case.get("lateGone").and_then(JsonValue::as_bool).unwrap()
    );
    assert_eq!(
        c.engine()
            .loops
            .row
            .removed
            .iter()
            .any(|r| r.clip.id == late.id),
        case.get("lateRemembered")
            .and_then(JsonValue::as_bool)
            .unwrap()
    );
    assert_eq!(
        c.engine().deck("A").unwrap().leader.as_deref(),
        case.get("leaderA").and_then(JsonValue::as_str)
    );
    let mut disabled = c.engine().deck("A").unwrap().disabled_roles.clone();
    disabled.sort();
    assert_eq!(disabled, strings(case, "disabledA"));
    assert_eq!(
        JsonValue::Array(
            c.engine()
                .loop_settings()
                .iter()
                .map(DeckAction::json)
                .collect()
        )
        .canonical_js(),
        s(case, "loopSettings")
    );
    step(&mut c, 20, |_| {});
    same_text(
        &state_text(&c, false),
        s(case, "stays"),
        "and it stays there",
    );
    // a restore of the same target into a fresh engine
    let mut fresh = rewind_engine();
    fresh.restore_state(&want, &find);
    step(&mut fresh, 1, |_| {});
    same_text(
        &state_text(&fresh, false),
        s(case, "restored"),
        "a fresh restore",
    );

    // a reset to a fresh state empties the engine
    let case = reset_case(&g, "a reset to a fresh state empties the engine");
    let mut c = rewind_engine();
    let empty = c
        .engine()
        .journal_state(JournalStoreState::default(), false);
    assert_eq!(
        empty.json().canonical_js(),
        s(case, "want"),
        "a fresh engine"
    );
    rewind_target(&mut c, &clips);
    let warnings = c.reset_to(&empty, &find);
    assert_eq!(warnings, strings(case, "warnings"));
    same_text(&state_text(&c, false), s(case, "after"), "emptied");
    check_reads(case, &c, "emptied");
    step(&mut c, 40, |_| {});
    let e = c.engine();
    assert_eq!(
        e.loops.row.clips.is_empty() && e.loops.row.removed.is_empty() && e.scenes().is_empty(),
        case.get("emptyRow").and_then(JsonValue::as_bool).unwrap()
    );
    assert_eq!(
        e.loop_settings() == djrust::journal::replay::default_loop_settings(),
        case.get("defaultSettings")
            .and_then(JsonValue::as_bool)
            .unwrap()
    );
    let mut looping = e.looping_clips();
    looping.sort();
    assert_eq!(looping, strings(case, "loopingAfterRender"));

    // a clip missing from the library is a warning
    let case = reset_case(&g, "a clip missing from the library is a warning");
    let mut c = rewind_engine();
    let want = rewind_target(&mut c, &clips);
    let missing = s(case, "missing").to_string();
    let warnings = c.reset_to(&want, &|id| if id == missing { None } else { find(id) });
    assert_eq!(warnings, strings(case, "warnings"));
    same_text(&state_text(&c, false), s(case, "after"), "a missing clip");
    assert_eq!(
        c.engine().clip(&missing).is_some(),
        case.get("hasClip").and_then(JsonValue::as_bool).unwrap()
    );
}

// MARK: replay

fn row_matches(want: &JsonValue, r: &djrust::host::replay::JournalReplayResult, what: &str) {
    assert_eq!(r.start_seq, n(want, "startSeq"), "{what}: startSeq");
    assert_eq!(r.last_seq, n(want, "lastSeq"), "{what}: lastSeq");
    assert_eq!(r.frames, n(want, "frames"), "{what}: frames");
    assert_eq!(r.warnings, strings(want, "warnings"), "{what}: warnings");
    assert_eq!(
        r.state.json().canonical_js(),
        s(want, "state"),
        "{what}: state"
    );
    assert_eq!(
        r.stores().json().canonical_js(),
        s(want, "stores"),
        "{what}: stores"
    );
}

#[test]
fn a_swift_journal_replays_here_to_the_same_state() {
    let g = golden();
    let rp = g.get("replay").unwrap();
    assert_eq!(rp.get("sampleRate").and_then(JsonValue::as_f64), Some(R));
    let c = parse(s(rp, "text"));
    assert!(c.skipped.is_empty() && !c.truncated);
    // the text is what Swift wrote: it re-encodes to itself, line by line
    let again: String = c.entries.iter().map(|e| e.line() + "\n").collect();
    assert_eq!(
        again,
        s(rp, "text"),
        "every line re-encodes to the same bytes"
    );
    let first_frame = c.entries[0].frame;
    assert_eq!(first_frame, n(rp, "firstFrame"));
    let mids: Vec<i64> = arr(rp, "mids")
        .iter()
        .map(|m| m.as_f64().unwrap() as i64)
        .collect();
    let (m1, m2) = (mids[0], mids[1]);
    let last = n(rp, "lastSeq");
    assert_eq!(c.entries.last().unwrap().seq, last);
    let fav = n(rp, "favoriteSeq");
    let mid_frame = n(rp, "midFrame");

    let go = |o: JournalReplayOptions| replay(&c, &o).unwrap();
    let opts = |to_seq: Option<i64>, end_frame: Option<i64>, from_start: bool, playheads: bool| {
        JournalReplayOptions {
            to_seq,
            end_frame,
            from_start,
            playheads,
            ..Default::default()
        }
    };
    let rows = rp.get("replays").unwrap();
    let want = |k: &str| rows.get(k).unwrap_or_else(|| panic!("no replay {k}"));
    let full = go(opts(None, None, true, false));
    row_matches(want("full"), &full, "from the start");
    // the live state a real Swift journal run ended in: the replay stands where it stood
    assert_eq!(
        full.state.json().canonical_js(),
        s(rp, "live"),
        "the replay is the live state"
    );
    for (key, o) in [
        ("near", opts(None, None, false, false)),
        ("toFavorite", opts(Some(fav), None, false, false)),
        ("second", opts(None, Some(R as i64), false, false)),
        ("betweenMids", opts(Some(m1 + 1), None, false, false)),
        (
            "betweenMidsFromStart",
            opts(Some(m1 + 1), None, true, false),
        ),
        ("onMid", opts(Some(m2), None, false, false)),
        ("beforeMid", opts(Some(m1 - 1), None, false, false)),
        ("byTime", opts(None, Some(mid_frame), false, false)),
        ("withHeads", opts(None, None, false, true)),
    ] {
        row_matches(want(key), &go(o), key);
    }

    // exportSession of two slices
    for (e, from) in arr(rp, "exports").iter().zip([n(rp, "sectionSeq"), m2 + 1]) {
        assert_eq!(n(e, "from"), from);
        let tail = if from == m2 + 1 { 1.0 } else { 2.0 };
        let session = export_session_from(&c, from, n(e, "to"), tail, None, None).unwrap();
        assert_eq!(
            session.canonical(),
            s(e, "canonical"),
            "slice from {from}: canonical text"
        );
        assert_eq!(session.cid(), s(e, "cid"), "slice from {from}: cid");
        assert_eq!(session.frames, n(e, "frames"), "slice from {from}: frames");
        assert_eq!(
            session.events.len() as i64,
            n(e, "events"),
            "slice from {from}: events"
        );
    }
}

// MARK: the starting state of SessionPlayer.render

#[test]
fn a_session_render_starts_the_engine_where_swifts_does() {
    let g = golden();
    let cases = arr(&g, "sessionStart");
    assert!(!cases.is_empty());
    for (i, case) in cases.iter().enumerate() {
        let session = djrust::session::model::Session::decode(s(case, "session"), None)
            .unwrap_or_else(|e| panic!("session {i}: {e}"));
        assert_eq!(
            session.canonical(),
            s(case, "session"),
            "session {i} re-encodes to Swift's text"
        );
        let mut c = djrust::host::replay::session_engine(&session, &Default::default()).unwrap();
        step(&mut c, 4, |_| {});
        assert_eq!(c.dropped(), 0);
        same_text(
            &state_text(&c, false),
            s(case, "state"),
            &format!("session {i}: the engine's state"),
        );
        check_reads(case, &c, &format!("session {i}"));
    }
}
