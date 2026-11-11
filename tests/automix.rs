//! Auto-mix's rule and arithmetic. `AutoMixRuleTests.swift` ported with its literals (the
//! plan is a pure function of the outgoing configuration and the counter, the tempo window,
//! the locks, the compatible key); the engine arithmetic (`AutoMixEngine.swift`) is checked
//! against values worked out from the Swift source and the numbers `AutoMixTests.swift`
//! asserts through its rig (hand-off bar 8 for Mix now, 32 for `every: 4`, the crossfader's
//! 0.5 → 1 steps, B's first tick on A's bar line). No Swift on this box: no golden.

use std::collections::BTreeMap;

use djrust::automix::engine::*;
use djrust::automix::measure::AutoMixHandoffLevels;
use djrust::automix::*;
use djrust::cid::share_cid;
use djrust::deck::tempo_match;
use djrust::gen::tables::BeatsTables;
use djrust::json::JsonValue;
use djrust::wander::*;
use djrust::TrackSpec;

const ROLES: [&str; 7] = ["kick", "snare", "hihat", "bass", "melody", "harmony", "arp"];

fn context(spec: TrackSpec, locks: &[&str], mix_bpm: Option<f64>) -> WanderContext {
    let voices = vec![
        WanderVoice::new(
            &["kick", "snare", "hihat"],
            10,
            "drums",
            &["drums", "drums-909"],
        ),
        WanderVoice::new(&["bass"], 2, "sub-bass", &["sub-bass", "acid"]),
        WanderVoice::new(&["melody"], 1, "lead", &["lead", "pluck"]),
        WanderVoice::new(&["arp"], 4, "arp", &["arp", "bell"]),
    ];
    let is_loop = spec.structure.is_none();
    let mut c = WanderContext::new("A", spec.clone(), is_loop, &ROLES, &ROLES, locks, voices);
    if locks.contains(&"tempo") {
        c.anchor_bpm = Some(Wander::bpm(&spec));
    }
    if locks.contains(&"key") {
        if let Some(g) = BeatsTables::shared().genres.get(&spec.genre) {
            c.key = Some(WanderKey {
                scale: g.scale_name.clone(),
                root_pc: (g.root_note + 3) % 12,
                offset: 3,
            });
        }
    }
    c.mix_bpm = Some(mix_bpm.unwrap_or_else(|| Wander::bpm(&spec)));
    c
}

fn techno() -> TrackSpec {
    TrackSpec::new("techno", 42.0, Some("standard"))
}

/// The key the outgoing deck plays (techno's root, moved by `offset`).
fn key(offset: i64) -> WanderKey {
    let g = &BeatsTables::shared().genres["techno"];
    WanderKey {
        scale: g.scale_name.clone(),
        root_pc: (g.root_note + offset + 12) % 12,
        offset,
    }
}

#[test]
fn the_settings_clamp_to_the_offered_lengths_and_phrases() {
    assert_eq!(
        AutoMixSettings::new(true, 7, 5),
        AutoMixSettings::new(true, 8, 0)
    );
    assert_eq!(AutoMixSettings::new(false, 32, 16).bars, 32);
    assert!(AutoMixSettings::default().is_default());
    assert!(!AutoMixSettings::new(true, 8, 0).is_default());
    assert_eq!(
        AutoMixSettings::new(true, 8, 0).label(),
        "8 bars · to the outro"
    );
    assert_eq!(
        AutoMixSettings::new(true, 16, 8).label(),
        "16 bars · every 8 phrases"
    );
}

#[test]
fn the_settings_ride_in_a_session_only_when_set() {
    let plain = AutoMixState::default();
    assert!(
        plain.is_session_default(),
        "older sessions keep their text and CID"
    );
    let s = AutoMixState::with_start(AutoMixSettings::new(true, 16, 4), 2);
    assert!(!s.is_session_default());
    let back = AutoMixState::from_start_json(Some(&s.session_json()));
    assert_eq!(back.settings, AutoMixSettings::new(true, 16, 4));
    assert_eq!(back.handoffs, 2);
    assert_eq!(back, s);
    // missing: the default
    assert!(AutoMixState::from_start_json(None).is_session_default());
    // negative counters read as 0
    let v = JsonValue::parse(r#"{"on":true,"handoffs":-4}"#).unwrap();
    assert_eq!(AutoMixState::from_json(Some(&v)).handoffs, 0);
}

#[test]
fn a_checkpoint_carries_the_run_in_flight() {
    let spec = TrackSpec::new("house", 12.0, Some("standard"));
    let plan = AutoMixPlan {
        n: 3,
        spec: Some(spec.clone()),
        kind: "seed".into(),
        detail: "seed 41 → 12 · key A".into(),
        from: Some("zCid".into()),
    };
    let mut run = AutoMixRun::new(plan.clone(), "A", "B", 40, 8, 8, true, 1.0);
    run.incoming_ready = true;
    run.mix_out_loaded = true;
    run.mix_out_tick = Some(300);
    run.out_handoff_tick = Some(640);
    run.c0 = Some(0.5);
    run.last_fader = Some(0.5);
    run.prev_leader = Some("A".into());
    run.mult = 0.5;
    let mut st = AutoMixState::with_start(AutoMixSettings::new(true, 8, 4), 4);
    st.playing = Some("A".into());
    st.paused = true;
    st.quiet_bars = 3;
    st.start_bar = Some(2);
    st.now_requested = true;
    st.cued = Some("B".into());
    st.kept = Some(plan);
    st.run = Some(run);
    let back = AutoMixState::from_json(Some(&st.checkpoint_json()));
    assert_eq!(back, st);
    // the net descriptor round-trips the spec, part seeds included
    let mut sp = spec;
    sp.part_seeds.insert("bass".into(), 9.0);
    let j = net_descriptor_json(&sp);
    assert_eq!(net_descriptor_from_json(&j), Some(sp.clone()));
    assert_eq!(
        j.get("cid").and_then(JsonValue::as_str),
        Some(share_cid(&sp.payload()).as_str())
    );
}

#[test]
fn wanders_draws_are_unchanged() {
    // `plan(_:step:)` is the overload over every kind the locks leave, from its own stream
    let c = context(techno(), &[], None);
    for step in 0..200 {
        let mut rng = WanderRandom::new(&c.spec, step);
        assert_eq!(
            Wander::plan(&c, step),
            Wander::plan_with(&c, &mut rng, Wander::kinds(&c))
        );
    }
}

#[test]
fn the_incoming_is_a_pure_function_of_the_outgoing_and_the_counter() {
    let c = context(techno(), &[], None);
    let k = key(0);
    let (mut differs, mut not_wanders) = (0, 0);
    for n in 0..40 {
        let a = AutoMix::plan(&c, n, Some(&k));
        assert!(a.is_some());
        assert_eq!(
            a,
            AutoMix::plan(&c, n, Some(&k)),
            "same configuration and counter"
        );
        if a.as_ref().map(|p| &p.spec)
            != AutoMix::plan(&c, n + 1, Some(&k)).as_ref().map(|p| &p.spec)
        {
            differs += 1;
        }
        match Wander::plan(&c, n).map(|p| p.action) {
            Some(WanderAction::Swap(w)) if Some(&w) == a.as_ref().and_then(|p| p.spec.as_ref()) => {
            }
            _ => not_wanders += 1,
        }
        assert_eq!(a.unwrap().from, Some(share_cid(&c.spec.payload())));
    }
    assert!(differs >= 38, "the counter moves the draw ({differs}/40)");
    assert!(
        not_wanders >= 38,
        "salted apart from Wander's step n ({not_wanders}/40)"
    );
}

#[test]
fn a_hand_off_brings_a_new_track() {
    let c = context(techno(), &[], None);
    let mut kinds: BTreeMap<String, i64> = BTreeMap::new();
    for n in 0..300 {
        let p = AutoMix::plan(&c, n, Some(&key(0))).unwrap_or_else(|| panic!("no plan at {n}"));
        let spec = p.spec.clone().unwrap();
        *kinds.entry(p.kind.clone()).or_default() += 1;
        assert!(["seed", "instrument", "genre"].contains(&p.kind.as_str()));
        assert_ne!(spec, c.spec);
        assert!(spec.seed != c.spec.seed || spec.genre != c.spec.genre);
    }
    // Wander's weights renormalised: seed 20 : instrument 9 : genre 5 (59 / 26 / 15 %)
    let seed = *kinds.get("seed").unwrap_or(&0) as f64 / 300.0;
    let genre = *kinds.get("genre").unwrap_or(&0) as f64 / 300.0;
    assert!((seed - 20.0 / 34.0).abs() < 0.08, "seed {seed}");
    assert!((genre - 5.0 / 34.0).abs() < 0.08, "genre {genre}");
}

#[test]
fn genre_hops_stay_in_the_tempo_window() {
    let t = BeatsTables::shared();
    for (genre, bpm) in [
        ("techno", None),
        ("house", Some(129.0)),
        ("trance", None),
        ("dnb", None),
    ] {
        let spec = TrackSpec::new(genre, 7.0, Some("standard"));
        let c = context(spec.clone(), &[], bpm);
        for n in 0..200 {
            let Some(p) = AutoMix::plan(&c, n, None) else {
                continue;
            };
            if p.kind != "genre" {
                continue;
            }
            let s = p.spec.unwrap();
            let g = &t.genres[&s.genre];
            let m = tempo_match(g.bpm, c.mix_bpm.unwrap());
            assert!(
                !m.clamped && m.mult == 1.0,
                "{genre} → {} at {:?}",
                s.genre,
                c.mix_bpm
            );
        }
        // the tempo lock narrows it to ±6 % of its anchor
        let locked = context(spec, &["tempo"], bpm);
        for n in 0..200 {
            let Some(p) = AutoMix::plan(&locked, n, None) else {
                continue;
            };
            if p.kind != "genre" {
                continue;
            }
            let g = &t.genres[&p.spec.unwrap().genre];
            assert!(
                (g.bpm / locked.anchor_bpm.unwrap() - 1.0).abs() <= WanderLock::TEMPO_TOLERANCE
            );
        }
    }
}

#[test]
fn locks_hold() {
    // the genre lock: same genre always
    for n in 0..100 {
        let p = AutoMix::plan(&context(techno(), &["genre"], None), n, Some(&key(0))).unwrap();
        assert!(p.spec.unwrap().genre == "techno" && p.kind != "genre");
    }
    // the instruments lock (the union of both decks' locks): no instrument move, every voice
    // rides along
    for n in 0..60 {
        let p = AutoMix::plan(
            &context(techno(), &["instruments", "genre"], None),
            n,
            Some(&key(0)),
        )
        .unwrap();
        assert_eq!(p.kind, "seed");
        let tracks = p.spec.unwrap();
        let ins = tracks
            .layers
            .get("tracks")
            .and_then(|t| t.get("2"))
            .and_then(|t| t.get("instrument"))
            .and_then(JsonValue::as_str)
            .map(String::from);
        assert_eq!(ins.as_deref(), Some("sub-bass"));
    }
    // keep:bass on a same-genre hand-off carries the bass line's seed
    for n in 0..40 {
        let p = AutoMix::plan(
            &context(techno(), &["genre", &WanderLock::keep("bass")], None),
            n,
            Some(&key(0)),
        )
        .unwrap();
        assert_eq!(p.spec.unwrap().part_seeds.get("bass"), Some(&42.0));
    }
    // the key lock pins its key, not a compatible one
    let k = context(techno(), &["key"], None);
    for n in 0..40 {
        assert_eq!(
            AutoMix::plan(&k, n, Some(&key(0)))
                .unwrap()
                .spec
                .unwrap()
                .pinned_key_offset(),
            Some(3)
        );
    }
    // the palette carries
    let mut pal = context(techno(), &[], None);
    pal.palette = Some("synthwave".into());
    pal.channels = vec![1, 2, 4, 10];
    for n in 0..20 {
        let s = AutoMix::plan(&pal, n, Some(&key(0))).unwrap().spec.unwrap();
        assert_eq!(
            djrust::wander::carry::spec_palette(&s).as_deref(),
            Some("synthwave")
        );
    }
    // everything kept: nothing to draw
    let kept: Vec<String> = ROLES.iter().map(|r| WanderLock::keep(r)).collect();
    let refs: Vec<&str> = kept.iter().map(String::as_str).collect();
    assert!(!AutoMix::can_draw(&context(techno(), &refs, None)));
    assert!(AutoMix::can_draw(&context(techno(), &[], None)));
}

#[test]
fn the_key_is_compatible() {
    let out = key(2);
    let mut seen = std::collections::BTreeSet::new();
    let t = BeatsTables::shared();
    for n in 0..200 {
        let Some(p) = AutoMix::plan(&context(techno(), &[], None), n, Some(&out)) else {
            continue;
        };
        let Some(s) = p.spec else { continue };
        let Some(g) = t.genres.get(&s.genre) else {
            continue;
        };
        if g.scale_name == out.scale {
            let off = s.pinned_key_offset();
            assert!(off.is_some() && (-5..=6).contains(&off.unwrap()));
            let pc = (g.root_note + off.unwrap_or(0)).rem_euclid(12);
            let d = (pc - out.root_pc + 12) % 12;
            assert!([0, 7, 5].contains(&d), "root {pc} against {}", out.root_pc);
            seen.insert(d);
        } else {
            assert!(
                s.pinned_key_offset().is_none(),
                "another scale keeps the variety pass's own key"
            );
        }
    }
    assert_eq!(seen.into_iter().collect::<Vec<_>>(), [0, 5, 7]);
}

// MARK: the engine's arithmetic

fn sections(spec: &[(&str, i64, i64)]) -> Vec<AutoMixSection> {
    spec.iter()
        .map(|(n, s, l)| AutoMixSection {
            name: n.to_string(),
            start_tick: *s,
            steps: *l,
        })
        .collect()
}

#[test]
fn the_hand_off_bar_is_a_phrase_boundary() {
    let loop_ = [];
    // Mix now at bar 0 of the rig: the first phrase boundary ≥ 4 bars on is bar 8
    let s = AutoMixSettings::new(true, 4, 0);
    assert_eq!(
        handoff_bar(&s, &loop_, Some(0), true, 0, AutoMix::NOW_PREP_BARS, false),
        8
    );
    // every: 4 → four phrases after it came in
    let e = AutoMixSettings::new(true, 4, 4);
    assert_eq!(
        handoff_bar(&e, &loop_, Some(0), false, 0, AutoMix::PREP_BARS, false),
        32
    );
    // a loop with every 0 plays LOOP_PHRASES phrases
    assert_eq!(
        handoff_bar(&s, &loop_, Some(0), false, 3, AutoMix::PREP_BARS, false),
        64
    );
    // a cue under a sample deck: the first phrase from earliest
    assert_eq!(
        handoff_bar(&s, &loop_, Some(0), false, 3, AutoMix::PREP_BARS, true),
        16
    );
    // a song with an outro still ahead (every 0): the outro
    let song = sections(&[("intro", 0, 128), ("verse", 128, 256), ("outro", 640, 128)]);
    assert_eq!(handoff_bar(&s, &song, Some(0), false, 3, 8, false), 40);
    // an outro too close: the last phrase boundary that leaves `bars` before the end (end = 48)
    assert_eq!(handoff_bar(&s, &song, Some(0), false, 33, 8, false), 48);
    // without an outro
    let no_outro = sections(&[("intro", 0, 128), ("verse", 128, 640)]);
    // end = 48 bars, bars = 4 → latest = 44, the phrase boundary at or before it is 40
    assert_eq!(handoff_bar(&s, &no_outro, Some(0), false, 3, 8, false), 40);
    // a section start counts as a phrase boundary
    let odd = sections(&[("intro", 0, 5 * 16), ("drop", 5 * 16, 256)]);
    assert!(is_phrase(&odd, 5) && is_phrase(&odd, 8) && !is_phrase(&odd, 6));
    assert_eq!(phrase_from(&odd, 1), 5);
    assert_eq!(phrase_from(&odd, -3), 0);
    assert!(prep_reached(0, 8, 8) && !prep_reached(0, 9, 8));
}

#[test]
fn the_countdown_pauses_and_resumes_after_a_quiet_phrase() {
    let mut st = AutoMixState::default();
    st.settings = AutoMixSettings::new(true, 4, 4);
    assert_eq!(st.observe_countdown("A", 0), CountdownBar::Counting);
    assert_eq!(st.start_bar, Some(0));
    // steering pauses
    let ops = st.note_touch(Some("A"), AutoMixTouch::Steer, true);
    assert!(ops.is_empty() && st.paused && st.handoff_bar.is_none());
    // expression does not pause a running one
    let mut ex = AutoMixState::default();
    ex.settings = AutoMixSettings::new(true, 4, 4);
    ex.observe_countdown("A", 0);
    assert!(
        ex.note_touch(Some("A"), AutoMixTouch::Expression, true)
            .is_empty()
            && !ex.paused
    );
    // eight quiet bars resume it, counting from there
    for bar in 1..8 {
        assert_eq!(
            st.observe_countdown("A", bar),
            CountdownBar::Quiet,
            "bar {bar}"
        );
    }
    assert_eq!(st.observe_countdown("A", 8), CountdownBar::Counting);
    assert!(!st.paused && st.start_bar == Some(8));
    // another deck taking over resets the clock
    st.observe_countdown("B", 10);
    assert_eq!((st.playing.as_deref(), st.start_bar), (Some("B"), Some(10)));
}

#[test]
fn touches_on_an_idle_deck_cue_it_and_a_run_is_cancelled_or_ended() {
    let mut st = AutoMixState::default();
    st.settings = AutoMixSettings::new(true, 8, 0);
    st.playing = Some("A".into());
    // a load on the idle deck is a cue, not a pause
    assert!(st
        .note_touch(Some("B"), AutoMixTouch::Load, true)
        .is_empty());
    assert_eq!((st.cued.as_deref(), st.paused), (Some("B"), false));
    // a deck that is not A or B is ignored
    assert!(
        st.note_touch(Some("L"), AutoMixTouch::Transport, true)
            .is_empty()
            && !st.paused
    );
    // a run before its mix-out: a steer cancels and keeps the plan, a load on the incoming
    // deck replaces it (a cue, the plan dropped)
    let plan = AutoMixPlan {
        n: 0,
        spec: Some(techno()),
        kind: "seed".into(),
        detail: "x".into(),
        from: Some("z".into()),
    };
    let mut run = AutoMixRun::new(plan.clone(), "A", "B", 16, 8, 8, true, 1.0);
    run.incoming_ready = true;
    let mut s1 = AutoMixState::default();
    s1.settings = AutoMixSettings::new(true, 8, 0);
    s1.playing = Some("A".into());
    s1.run = Some(run.clone());
    assert_eq!(
        s1.note_touch(Some("A"), AutoMixTouch::Steer, true),
        [RunOp::CancelRun { keep_plan: true }]
    );
    assert!(s1.run.is_none() && s1.kept == Some(plan.clone()) && s1.paused);
    let mut s2 = s1.clone();
    s2.kept = None;
    s2.paused = false;
    s2.run = Some(run.clone());
    assert_eq!(
        s2.note_touch(Some("B"), AutoMixTouch::Load, true),
        [RunOp::CancelRun { keep_plan: false }]
    );
    assert!(s2.kept.is_none() && s2.cued.as_deref() == Some("B"));
    // after the mix-out: expression and steering leave it, the transport or a load ends it
    let mut late = run.clone();
    late.mix_out_loaded = true;
    let mut s3 = AutoMixState::default();
    s3.settings = AutoMixSettings::new(true, 8, 0);
    s3.playing = Some("A".into());
    s3.run = Some(late.clone());
    assert!(
        s3.note_touch(Some("A"), AutoMixTouch::Steer, true)
            .is_empty()
            && s3.run.is_some()
    );
    assert_eq!(
        s3.note_touch(Some("A"), AutoMixTouch::Transport, true),
        [RunOp::EndRun]
    );
    assert!(s3.run.is_none() && s3.paused);
    // off: nothing happens
    let mut off = AutoMixState::default();
    off.playing = Some("A".into());
    assert!(
        off.note_touch(Some("A"), AutoMixTouch::Steer, true)
            .is_empty()
            && !off.paused
    );
}

#[test]
fn the_pokes_map_to_touches() {
    assert_eq!(touch_of_op("play"), Some(AutoMixTouch::Transport));
    assert_eq!(touch_of_op("load"), Some(AutoMixTouch::Load));
    assert_eq!(touch_of_op("hotCue"), Some(AutoMixTouch::Steer));
    assert_eq!(touch_of_op("mixOut"), Some(AutoMixTouch::Steer));
    assert_eq!(touch_of_op("macro"), None);
    assert_eq!(touch_of_op("crossfader"), None);
}

#[test]
fn set_auto_mix_and_mix_now_move_the_state() {
    let mut st = AutoMixState::default();
    assert!(st.set_auto_mix(true, 8, 0).is_none());
    assert!(st.settings.on && st.handoff_bar.is_none());
    st.handoff_bar = Some(40);
    st.auto_mix_now();
    assert!(st.now_requested && st.handoff_bar.is_none());
    assert_eq!(st.prep(), AutoMix::NOW_PREP_BARS);
    st.begin(false);
    assert!(st.handoffs == 1 && !st.now_requested);
    st.begin(true);
    assert_eq!(st.handoffs, 1, "a reused kept plan does not count again");
    // off drops everything
    st.cued = Some("B".into());
    st.playing = Some("A".into());
    st.set_auto_mix(false, 8, 0);
    assert!(st.cued.is_none() && st.playing.is_none() && st.phase == AutoMixPhase::Off);
    st.auto_mix_now();
    assert!(!st.now_requested, "ignored while off");
    // a defer moves the hand-off a phrase on, its plan kept
    let mut run = AutoMixRun::new(
        cued_plan(5, None, "house #12"),
        "A",
        "B",
        24,
        8,
        8,
        true,
        1.0,
    );
    assert_eq!(run.plan.detail, "cued: house #12");
    st.settings.on = true;
    st.run = Some(run.clone());
    assert!(st.defer_handoff(4).is_none());
    assert_eq!(st.defer_handoff(5).unwrap().handoff_bar, 32);
    run.mix_out_loaded = true;
    st.run = Some(run);
    assert!(
        st.defer_handoff(5).is_none(),
        "not after the point of no return"
    );
}

#[test]
fn the_playing_deck_is_the_louder() {
    assert_eq!(
        choose_playing(false, false, 1.0, 1.0, None),
        PlayingDeck::Nobody
    );
    assert_eq!(
        choose_playing(true, false, 0.0, 0.0, None),
        PlayingDeck::One("A".into())
    );
    assert_eq!(
        choose_playing(false, true, 0.0, 0.0, None),
        PlayingDeck::One("B".into())
    );
    assert_eq!(
        choose_playing(true, true, 0.5, 0.9, None),
        PlayingDeck::Both("B".into())
    );
    assert_eq!(
        choose_playing(true, true, 0.9, 0.5, None),
        PlayingDeck::Both("A".into())
    );
    assert_eq!(
        choose_playing(true, true, 0.7, 0.7, Some("B")),
        PlayingDeck::Both("B".into())
    );
    assert_eq!(
        choose_playing(true, true, 0.7, 0.7, None),
        PlayingDeck::Both("A".into())
    );
}

fn run_h8() -> AutoMixRun {
    AutoMixRun::new(
        AutoMixPlan {
            n: 0,
            spec: None,
            kind: "seed".into(),
            detail: String::new(),
            from: None,
        },
        "A",
        "B",
        8,
        4,
        4,
        true,
        1.0,
    )
}

#[test]
fn the_start_holds_the_first_tick_back_to_the_bar_line() {
    let sr = 48_000.0;
    // 120 bpm: a tick (sixteenth) is 6000 samples
    let run = run_h8();
    let clock = |tick: i64, phase: f64, installed: bool| NetClock {
        tick,
        tick_phase: phase,
        tempo: 120.0,
        installed,
    };
    // not due until two ticks before the hand-off bar line (tick 128)
    assert_eq!(arm_frames_net(sr, &run, clock(120, 0.0, true)), None);
    // tick 126, a quarter in: (2 + 1) · 6000 − 1500 − 1
    assert_eq!(
        arm_frames_net(sr, &run, clock(126, 0.25, true)),
        Some(16_499.0)
    );
    // missed: start now, the correction puts it in place
    assert_eq!(arm_frames_net(sr, &run, clock(128, 0.0, true)), Some(0.0));
    // a swap still pending takes a tick of its own: the mix-out starts at 100
    let mut pending = run.clone();
    pending.mix_out_tick = Some(100);
    assert_eq!(
        arm_frames_net(sr, &pending, clock(126, 0.25, false)),
        Some(31.0 * 6000.0 - 1500.0 - 1.0)
    );
    // a mix-out that jumps to an outro: the target is its tail's first tick
    let mut jump = run.clone();
    jump.out_handoff_tick = Some(130);
    assert_eq!(
        arm_frames_net(sr, &jump, clock(127, 0.0, true)),
        Some(4.0 * 6000.0 - 1.0)
    );
    // a stopped tempo can't be timed
    assert_eq!(
        arm_frames_net(
            sr,
            &run,
            NetClock {
                tick: 126,
                tick_phase: 0.0,
                tempo: 0.0,
                installed: true
            }
        ),
        None
    );
    // the start's align, in the incoming deck's ticks
    let ticks = incoming_align_ticks(16_499.0, sr, 120.0).unwrap();
    assert!((ticks + (16_499.0 - 0.5) / 6000.0).abs() < 1e-12);
    assert_eq!(incoming_align_ticks(0.0, sr, 120.0), None);
    // a sample outgoing deck: within a beat and a half of the bar line
    assert_eq!(arm_frames_sample(sr, 8, 29.0, 120.0), None);
    assert_eq!(arm_frames_sample(sr, 8, 31.0, 120.0), Some(24_000.0));
    assert_eq!(arm_frames_sample(sr, 8, 32.5, 120.0), Some(0.0));
}

#[test]
fn a_late_start_is_corrected_within_a_sample() {
    let sr = 48_000.0;
    let run = run_h8();
    // out is 130 ticks in (2 past the line at 128), incoming exactly 2 ticks in: on the line
    let ok = correct_start(sr, &run, true, 131, 0.0, 120.0, true, 3, 0.0);
    assert_eq!(ok, Correction::Done);
    // incoming 0.1 tick late (600 samples): one align takes it out
    match correct_start(sr, &run, true, 131, 0.0, 120.0, true, 3, 0.1) {
        Correction::Align(t) => assert!((t + 0.1).abs() < 1e-9),
        other => panic!("{other:?}"),
    }
    // not readable yet
    assert_eq!(
        correct_start(sr, &run, false, 131, 0.0, 120.0, true, 3, 0.0),
        Correction::Wait
    );
    assert_eq!(
        correct_start(sr, &run, true, 128, 0.0, 120.0, true, 3, 0.0),
        Correction::Wait
    );
    assert_eq!(
        correct_start(sr, &run, true, 131, 0.0, 120.0, false, 3, 0.0),
        Correction::Wait
    );
    assert_eq!(
        correct_start(sr, &run, true, 131, 0.0, 120.0, true, 0, 0.0),
        Correction::Wait
    );
    // a sample outgoing deck, or a half-time incoming one, is left alone
    let mut s = run.clone();
    s.out_is_net = false;
    assert_eq!(
        correct_start(sr, &s, true, 131, 0.0, 120.0, true, 3, 0.5),
        Correction::Skip
    );
    let mut h = run;
    h.mult = 0.5;
    assert_eq!(
        correct_start(sr, &h, true, 131, 0.0, 120.0, true, 3, 0.5),
        Correction::Skip
    );
}

#[test]
fn the_crossfader_steps_once_a_bar_linear_in_position() {
    // AutoMixTests: c0 = 0.5 → 1 over four bars; four steps, each on a bar line
    let xs: Vec<f64> = (0..=4).map(|k| fader_position(0.5, 1.0, k, 4)).collect();
    assert_eq!(xs, [0.5, 0.625, 0.75, 0.875, 1.0]);
    assert_eq!(fader_position(0.5, 0.0, 2, 4), 0.25);
    assert_eq!(mix_bar_index(4, 0), 0);
    assert_eq!(mix_bar_index(4, 1), 0);
    assert_eq!(mix_bar_index(4, 17), 1);
    assert_eq!(mix_bar_index(4, 1000), 4, "capped at the hand-off's bars");
    assert!(handoff_done(4, 4, true) && handoff_done(1, 4, false) && !handoff_done(3, 4, true));
    assert_eq!((x_end_for("A"), x_end_for("B")), (1.0, 0.0));
}

#[test]
fn finishing_swaps_the_decks_and_logs_the_hand_off() {
    let mut st = AutoMixState::default();
    st.settings = AutoMixSettings::new(true, 4, 0);
    let mut run = run_h8();
    run.plan.spec = Some(techno());
    run.plan.kind = "genre".into();
    run.plan.detail = "techno → house · seed 512".into();
    st.run = Some(run.clone());
    st.update_phase(false);
    assert_eq!(st.phase, AutoMixPhase::Preparing);
    run.armed = true;
    st.run = Some(run.clone());
    st.update_phase(false);
    assert_eq!(st.phase, AutoMixPhase::Armed);
    st.update_phase(true);
    assert_eq!(st.phase, AutoMixPhase::Mixing);
    st.finish(&run, None, true);
    assert!(st.run.is_none());
    assert_eq!(st.playing.as_deref(), Some("B"));
    assert_eq!(st.start_bar, Some(4));
    let row = &st.log[0];
    assert!(row.from == "A" && row.to == "B" && row.bar == 8 && row.n == 0);
    assert_eq!(
        row.json().get("cid").and_then(JsonValue::as_str),
        Some(share_cid(&techno().payload()).as_str())
    );
    st.update_phase(false);
    assert_eq!(st.phase, AutoMixPhase::Playing);
    st.idle = Some("start a deck".into());
    st.update_phase(false);
    assert_eq!(st.phase, AutoMixPhase::Idle);
}

#[test]
fn a_handoffs_levels_are_a_dip_or_a_bump() {
    // the lofi worst case the constant names: a −23.64 LU dip inside the 24 LU limit
    let lv = AutoMixHandoffLevels {
        n: 1,
        from: "A".into(),
        to: "B".into(),
        move_: "m".into(),
        before: -15.0,
        after: -14.0,
        min_during: -38.64,
        max_during: -13.0,
        curve: vec![-15.0, -38.64],
    };
    assert!((lv.dip_lu() + 23.64).abs() < 1e-9 && lv.dip_lu().abs() <= AutoMix::MAX_DIP_LU);
    assert_eq!(lv.bump_lu(), 1.0);
    let j = lv.json();
    assert_eq!(j.get("dipLU").and_then(JsonValue::as_f64), Some(-23.64));
    // from a curve: only the frames inside (start, end] count
    let st: Vec<(i64, f64)> = vec![
        (10, -20.0),
        (20, -18.0),
        (30, -25.0),
        (40, -19.0),
        (50, -30.0),
    ];
    let l = AutoMixHandoffLevels::from_curve(0, "A", "B", "", -17.0, -16.0, &st, 10, 40, 20);
    assert_eq!((l.min_during, l.max_during), (-25.0, -18.0));
    assert_eq!(l.curve.len(), 5);
    let none = AutoMixHandoffLevels::from_curve(0, "A", "B", "", 0.0, 0.0, &[], 10, 40, 20);
    assert_eq!((none.min_during, none.max_during), (-200.0, -200.0));
}

// MARK: autoMixContext(out:inc:) and dj_state (parity form (a'): the Swift source,
// `AutoMixEngine.swift:298-310` and `:678-738`, read line by line; the engine-level behaviour
// they feed is held by `djhost/tests/deck_automix.rs` and the `automixdeck` golden)

#[test]
fn the_draws_context_merges_both_decks_locks_and_the_mix_tempo() {
    let spec = techno();
    let mut out_w = WanderState::default();
    out_w.locks.insert("genre".into());
    let mut inc_w = WanderState::default();
    inc_w.locks.insert("tempo".into());
    inc_w.anchor_bpm = Some(133.0);
    inc_w.key = Some(key(5));
    let c = context(spec.clone(), &[], None);
    let merged = auto_mix_context(c, None, &out_w, &inc_w, Some(key(2)), Some(131.5));
    // the locks are the union; the incoming's anchor and key stand in for an outgoing one that
    // has none; the mix tempo is the outgoing deck's
    assert!(merged.locked("genre") && merged.locked("tempo"));
    assert_eq!(merged.anchor_bpm, Some(133.0));
    assert_eq!(merged.key, Some(key(5)));
    assert_eq!(merged.mix_bpm, Some(131.5));
    assert_eq!(merged.spec, spec);

    // the outgoing deck's own win; a swap still composing is where the draw starts from
    let out_w = WanderState {
        anchor_bpm: Some(128.0),
        key: Some(key(0)),
        ..Default::default()
    };
    let heading = TrackSpec::new("house", 9.0, Some("standard"));
    let c = context(spec.clone(), &[], None);
    let merged = auto_mix_context(c, Some(&heading), &out_w, &inc_w, None, None);
    assert_eq!(merged.anchor_bpm, Some(128.0));
    assert_eq!(merged.key, Some(key(0)));
    assert_eq!(merged.spec, heading);
    assert_eq!(merged.mix_bpm, None);

    // a tempo lock nobody anchored holds the (heading) spec's tempo; a key lock nobody keyed
    // holds the key the outgoing deck plays in now
    let mut out_w = WanderState::default();
    out_w.locks.insert("tempo".into());
    out_w.locks.insert("key".into());
    let c = context(spec.clone(), &[], None);
    let merged = auto_mix_context(
        c,
        Some(&heading),
        &out_w,
        &WanderState::default(),
        Some(key(3)),
        Some(120.0),
    );
    assert_eq!(merged.anchor_bpm, Some(Wander::bpm(&heading)));
    assert_eq!(merged.key, Some(key(3)));
    // (and none to hold: it stays unset)
    let c = context(spec, &[], None);
    let merged = auto_mix_context(c, None, &out_w, &WanderState::default(), None, None);
    assert_eq!(merged.key, None);
}

struct Bars(BTreeMap<&'static str, (Option<i64>, Option<i64>)>);
impl AutoMixDecks for Bars {
    fn bar_of(&self, deck: &str) -> Option<i64> {
        self.0.get(deck).and_then(|d| d.0)
    }
    fn tick_of(&self, deck: &str) -> Option<i64> {
        self.0.get(deck).and_then(|d| d.1)
    }
}

fn run_state() -> AutoMixState {
    let mut st = AutoMixState {
        settings: AutoMixSettings::new(true, 8, 4),
        ..Default::default()
    };
    let mut plan = AutoMixPlan {
        n: 0,
        spec: Some(TrackSpec::new("house", 512.0, None)),
        kind: "genre".into(),
        detail: "techno → house · seed 512".into(),
        from: None,
    };
    plan.from = Some(share_cid(&techno().payload()));
    st.run = Some(AutoMixRun::new(plan, "A", "B", 32, 8, 8, true, 1.0));
    st
}

#[test]
fn dj_state_names_the_walk_and_the_status_line_says_it() {
    let decks = Bars(BTreeMap::from([
        ("A", (Some(27), Some(27 * 16 + 3))),
        ("B", (None, Some(0))),
    ]));
    // off
    let st = AutoMixState::default();
    assert_eq!(st.status(&decks), "auto-mix off");
    // idle: the reason
    let st = AutoMixState {
        settings: AutoMixSettings::new(true, 8, 0),
        idle: Some("start a deck".into()),
        ..Default::default()
    };
    assert_eq!(st.status(&decks), "waiting: start a deck");
    // counting down: "A playing · → B in 5 bars"
    let mut st = AutoMixState {
        settings: AutoMixSettings::new(true, 8, 4),
        phase: AutoMixPhase::Playing,
        ..Default::default()
    };
    assert_eq!(st.status(&decks), "auto-mix on");
    st.playing = Some("A".into());
    st.handoff_bar = Some(32);
    assert_eq!(st.status(&decks), "A playing · → B in 5 bars");
    let j = st.dj_state_json(&decks);
    assert_eq!(
        j.js_stringified(),
        r#"{"on":true,"bars":8,"every":4,"phase":"playing","handoffs":0,"paused":false,"fader":"auto","log":[],"playing":"A","incoming":null,"idle":null,"handoffBar":32,"handoffInBars":5}"#
    );
    // a bar already past the hand-off bar reads 0 bars to go
    st.handoff_bar = Some(20);
    assert_eq!(st.status(&decks), "A playing · → B in 0 bars");
    // paused: the quiet bars to go, never below 1
    st.paused = true;
    st.quiet_bars = 3;
    assert_eq!(
        st.status(&decks),
        "paused · resumes after 8 quiet bars (5 to go)"
    );
    st.quiet_bars = 8;
    assert!(st.status(&decks).ends_with("(1 to go)"));
    assert_eq!(
        st.dj_state_json(&decks)
            .get("resumesInBars")
            .and_then(JsonValue::as_i64),
        Some(1)
    );
    // a hand-off in flight: preparing, armed, mixing
    let mut st = run_state();
    st.playing = Some("A".into());
    st.phase = AutoMixPhase::Preparing;
    assert_eq!(
        st.status(&decks),
        "B: house · seed 512 (genre) · composing · in 5 bars"
    );
    st.run.as_mut().unwrap().incoming_ready = true;
    assert_eq!(
        st.status(&decks),
        "B: house · seed 512 (genre) · ready · in 5 bars"
    );
    st.phase = AutoMixPhase::Armed;
    assert_eq!(st.status(&decks), "B: house · seed 512 · starting");
    st.phase = AutoMixPhase::Mixing;
    let mixing = Bars(BTreeMap::from([
        ("A", (Some(33), None)),
        ("B", (None, Some(2 * 16 + 5))),
    ]));
    assert_eq!(st.status(&mixing), "mixing A → B · bar 3/8");
    let j = st.dj_state_json(&mixing);
    assert_eq!(j.get("mixBar").and_then(JsonValue::as_i64), Some(3));
    assert_eq!(j.get("handoffBar").and_then(JsonValue::as_i64), Some(32));
    assert!(
        j.get("handoffInBars").is_none(),
        "the hand-off bar has passed"
    );
    assert_eq!(j.get("incoming").and_then(JsonValue::as_str), Some("B"));
    assert!(j.get("next").is_some());
    // the bar is capped at the hand-off's length, and starts at 1
    let late = Bars(BTreeMap::from([
        ("A", (Some(40), None)),
        ("B", (None, Some(30 * 16))),
    ]));
    assert_eq!(st.status(&late), "mixing A → B · bar 8/8");
    let first = Bars(BTreeMap::from([
        ("A", (Some(32), None)),
        ("B", (None, Some(1))),
    ]));
    assert_eq!(st.status(&first), "mixing A → B · bar 1/8");
    // a cued plan has no genre or seed to name
    let mut st = run_state();
    st.run.as_mut().unwrap().plan = cued_plan(1, None, "dnb #777");
    st.phase = AutoMixPhase::Armed;
    assert_eq!(st.status(&decks), "B: cued · starting");
    // the log is newest first, twelve at most
    let mut st = AutoMixState {
        settings: AutoMixSettings::new(true, 8, 0),
        ..Default::default()
    };
    for n in 0..15 {
        st.log.push(AutoMixHandoff {
            n,
            from: "A".into(),
            to: "B".into(),
            bar: 8,
            kind: "seed".into(),
            move_: "m".into(),
            spec: None,
        });
    }
    let log = st.dj_state_json(&decks);
    let rows = log.get("log").and_then(JsonValue::as_array).unwrap();
    assert_eq!(rows.len(), 12);
    assert_eq!(rows[0].get("n").and_then(JsonValue::as_i64), Some(14));
}
