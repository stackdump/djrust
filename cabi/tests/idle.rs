//! Neutral is idle: the whole golden corpus, with every 0.3.0 control sent at its neutral
//! value, reproduces djwasm 0.2.0's streams exactly (`fixtures/golden-0.2.0.json`).
//!
//! A knob whose default is not bit-transparent would pass `golden.rs` (which never calls it)
//! and still move real sessions; this pass calls every one of them.

mod common;

use common::*;
use djwasm::*;

/// Every new export at its neutral value. Before play: a seek to tick 0 while stopped, a panic
/// with nothing running, an unknown id refused, a release of a timed net macro, groove 0/0 on
/// an instance without the groove flag, control macros on with every catalog id disabled.
/// Then per quantum: the frame clock, the macro mask, the tempo and the event ring read (and
/// drained), and the panic button pressed with nothing running.
#[test]
fn every_new_control_at_neutral_reproduces_golden() {
    let _g = lock();
    let ids = catalog_ids();
    assert_eq!(ids.len(), 42);
    let golden = load_golden();
    let mut bad = Vec::new();
    let mut skipped_seen = 0usize;
    for (case, want) in &golden.cases {
        let mut skipped = 0usize;
        let got = run_full(
            case,
            &|sr| djw_init_with(sr, 0),
            &mut || {
                assert_eq!(djw_seek(0.0), 0.0);
                djw_panic();
                assert_eq!(fire("bogus", -1), ERR_ARG);
                assert_eq!(fire("drop", -7), ERR_ARG);
                assert_eq!(djw_macro_mask(), 0);
                let id = "drop";
                assert_eq!(unsafe { djw_release_macro(id.as_ptr(), id.len()) }, 0);
                assert_eq!(djw_set_groove(0.0, 0.0), 0);
                djw_set_control_macros(1);
                for id in &ids {
                    assert_eq!(disable(id, true), 0);
                }
                assert_eq!(disable("bogus", true), ERR_ARG);
                assert!(drain_events().is_empty());
            },
            &mut |_| {
                assert_eq!(djw_macro_mask(), 0);
                assert!(djw_frame() >= 0.0);
                assert!(djw_tempo() > 0.0);
                djw_panic();
                for e in drain_events() {
                    // a project swapped in at the bar moves the tempo: an engine event
                    match ev_str(&e, "k") {
                        "skipped" => skipped += 1,
                        "tempo" => assert!(case.name.starts_with("live:swap"), "{}", case.name),
                        k => panic!("{}: {k}", case.name),
                    }
                }
            },
        )
        .hashes;
        skipped_seen += skipped;
        bad.extend(diff(case, want, &got));
    }
    eprintln!("control fire-macros skipped across the corpus: {skipped_seen}");
    assert!(
        bad.is_empty(),
        "{} streams differ from 0.2.0:\n{}",
        bad.len(),
        bad.join("\n")
    );
    // the disabled list was exercised, not just set: control nets asked and were refused
    assert!(skipped_seen > 0, "no control fire-macro was ever skipped");
}

/// The groove flag at 0/0 is idle: `djw_init_with(sr, DJW_GROOVE)` plus `djw_set_groove(0, 0)`
/// after the load. This includes the cases that load a second project while playing: the groove
/// values set through `djw_set_groove` stick for every later `djw_load` (`StudioShell::load_json`),
/// so a swapped-in project plays at 0/0 too instead of its own swing and humanize.
#[test]
fn groove_flag_at_zero_reproduces_golden() {
    let _g = lock();
    let golden = load_golden();
    let mut bad = Vec::new();
    let mut n = 0;
    for (case, want) in &golden.cases {
        n += 1;
        let got = run_full(
            case,
            &|sr| djw_init_with(sr, DJW_GROOVE),
            &mut || assert_eq!(djw_set_groove(0.0, 0.0), 0),
            &mut |_| {},
        )
        .hashes;
        bad.extend(diff(case, want, &got));
    }
    assert!(n > 150);
    assert!(
        bad.is_empty(),
        "{} streams differ from 0.2.0:\n{}",
        bad.len(),
        bad.join("\n")
    );
}

/// The flag is not idle at the projects' own values: composed genres carry humanize (techno
/// 10), which is why groove stays off by default.
#[test]
fn groove_flag_at_the_projects_values_moves_notes() {
    let _g = lock();
    let case = Case {
        name: "techno:42:loop".into(),
        source: Source::Compose(r#"{"genre":"techno","seed":42}"#.into()),
        sample_rate: 48000.0,
        quanta: 1500,
        script: vec![],
    };
    let plain = run(&case).hashes.pcm;
    let grooved = run_with(&case, &|sr| djw_init_with(sr, DJW_GROOVE), &mut |_| {})
        .hashes
        .pcm;
    assert_ne!(plain, grooved);
    let swung = run_full(
        &case,
        &|sr| djw_init_with(sr, DJW_GROOVE),
        &mut || assert_eq!(djw_set_groove(30.0, 20.0), 0),
        &mut |_| {},
    )
    .hashes
    .pcm;
    assert_ne!(swung, grooved);
    // deterministic
    let again = run_full(
        &case,
        &|sr| djw_init_with(sr, DJW_GROOVE),
        &mut || assert_eq!(djw_set_groove(30.0, 20.0), 0),
        &mut |_| {},
    )
    .hashes
    .pcm;
    assert_eq!(swung, again);
    // without the flag, groove values change nothing
    let flagless = run_full(
        &case,
        &|sr| djw_init(sr),
        &mut || assert_eq!(djw_set_groove(30.0, 20.0), 0),
        &mut |_| {},
    )
    .hashes
    .pcm;
    assert_eq!(flagless, plain);
}

#[test]
fn init_flags_are_checked() {
    let _g = lock();
    assert_eq!(djw_init_with(48000.0, DJW_STINGER_VOICES), ERR_UNSUPPORTED);
    assert_eq!(djw_init_with(48000.0, 1 << 7), ERR_ARG);
    assert_eq!(djw_init_with(0.0, 0), ERR_ARG);
    assert_eq!(djw_init_with(48000.0, DJW_GROOVE), 4096);
    assert_eq!(djw_set_groove(f64::NAN, 0.0), ERR_ARG);
    assert_eq!(djw_set_groove(0.0, 0.0), ERR_NOT_READY);
    // 0.3.0's six, plus INSTRUMENT (2048)
    assert_eq!(djw_features(), 0x3f | 0x800);
}
