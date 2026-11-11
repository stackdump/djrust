//! The options a deck plays a track with: `WaveOptions::extended`'s fields (Swift's
//! `extended(samplers:seed:roleFilter:)` sets the instrument table, every fix, the stingers, the
//! turntable brake and the limiting master, and the deck's sampler bank, the track's seed and
//! the role filter: `extended_with`), `for_genre` and `for_palette`
//! (`DeckGenerator.swift:244-266`, `PaletteSound.swift:112`), and what a project prepared with
//! them carries. (PORTING.md's form (a); `tests/options_golden.rs` in `djhost` holds the options
//! a whole `PreparedTrack` ends up with to Swift's.)

use djrust::musicality::echo::EchoSpec;
use djrust::musicality::fixes::Fixes;
use djrust::musicality::space::SpaceSpec;
use djrust::sound::PaletteSet;
use djrust::wave::*;
use djrust::TrackSpec;

fn all() -> WaveOptions {
    WaveOptions {
        musicality: Fixes::ALL,
        ..WaveOptions::extended()
    }
}

/// `WaveOptions.extended`: what the app plays (form (a): the fields of `Instruments.swift:86-105`,
/// `.extended(samplers:seed:roleFilter:)`): the instrument table, every fix (`.all`), the
/// stingers curated from what is installed, the turntable brake and the limiting master. The
/// bank, the seed and the role filter are the deck's and the track's.
#[test]
fn extended_is_what_the_app_plays() {
    let o = WaveOptions::extended();
    assert!(o.instruments.is_some());
    assert_eq!(o.musicality, Fixes::ALL);
    assert_eq!(o.master, Master::Limit);
    assert_eq!(o.stingers, Some(StingerPolicy::Here));
    assert!(o.turntable_brake);
    assert!(o.bank.is_none() && o.role_filter.is_none() && o.voicing_seed == 0.0);
    // nothing that only a genre or a track sets is there yet
    assert!(o.genre.is_none() && o.space.is_none() && o.echo.is_none() && o.feel.is_none());
    assert!(o.track_gain.is_none() && o.track_balance.is_none() && o.rests.is_none());
    assert_eq!(o.duck_depth, 0.0);
    // the parity options have none of it
    let p = WaveOptions::default();
    assert!(p.instruments.is_none() && p.stingers.is_none() && !p.turntable_brake);
    assert_eq!(p.master, Master::Soft);
    assert!(p.musicality.is_empty());
}

/// `.extended(samplers:seed:roleFilter:)`: the seed and the role filter are the track's and the
/// deck's; everything else is `extended`'s. `PreparedTrack` then plays `.forGenre(genre)` of it
/// (`for_track`).
#[test]
fn extended_with_is_a_decks_options_for_one_track() {
    let filter = RoleFilter {
        mute: vec!["hats".into()],
        solo: None,
    };
    let o = WaveOptions::extended_with(None, 42.0, Some(filter.clone()));
    assert_eq!(o.voicing_seed, 42.0);
    assert_eq!(o.role_filter, Some(filter));
    assert_eq!(o.musicality, Fixes::ALL);
    assert_eq!(o.master, Master::Limit);
    let t = WaveOptions::for_track("jazz", 7.0);
    let by_hand = WaveOptions::extended_with(None, 7.0, None).for_genre("jazz");
    assert_eq!(t.genre, by_hand.genre);
    assert_eq!(t.space, by_hand.space);
    assert_eq!(t.echo, by_hand.echo);
    assert_eq!(t.duck_depth, by_hand.duck_depth);
    assert_eq!(t.brushes, by_hand.brushes);
    assert!(t.brushes, "jazz plays the brush kit");
    assert_eq!(t.voicing_seed, 7.0);
}

/// `grooveOn` and `.drift` are `opts.musicality.contains(...)` in Swift: the runner here reads
/// the fix bits too, so `.all` plays the swing and the 1/f humanize with nothing else set, and
/// the flags beside them are still how a test asks for one alone (form (a),
/// `WaveRunner.swift` `grooveOn = opts.musicality.contains(.groove)`).
#[test]
fn the_runner_reads_groove_and_drift_off_the_fixes() {
    // a project with swing and humanize, so both change what plays
    let mut p = TrackSpec::new("jazz", 42.0, Some("minimal")).project();
    p.swing = 55.0;
    p.humanize = 30.0;
    let render = |o: WaveOptions| -> Vec<f32> {
        let mut r = WaveRunner::new(22_050.0, o);
        r.load(&p);
        r.play();
        let mut l = vec![0f32; 128];
        let mut out = Vec::new();
        for _ in 0..600 {
            r.process(&mut l, None, 128);
            out.extend_from_slice(&l);
        }
        out
    };
    let plain = render(WaveOptions::default());
    let by_bit = render(WaveOptions {
        musicality: Fixes::GROOVE,
        ..Default::default()
    });
    let by_flag = render(WaveOptions {
        groove: true,
        ..Default::default()
    });
    assert!(plain != by_bit, "the swing plays");
    assert!(by_bit == by_flag, "the bit and the flag are one switch");
    let drift_bit = render(WaveOptions {
        musicality: Fixes::DRIFT,
        voicing_seed: 3.0,
        ..Default::default()
    });
    let drift_flag = render(WaveOptions {
        drift: true,
        voicing_seed: 3.0,
        ..Default::default()
    });
    assert!(drift_bit != plain, "the 1/f humanize plays");
    assert!(
        drift_bit == drift_flag,
        "the bit and the flag are one switch"
    );
}

/// A project prepared with the brake option carries it (`WaveProject.turntableBrake`, which a
/// section jump or a relocation keeps because it prepares with the deck's options).
#[test]
fn a_prepared_project_carries_the_brake() {
    let p = TrackSpec::new("techno", 42.0, Some("minimal")).project();
    for (o, want) in [
        (WaveOptions::extended(), true),
        (WaveOptions::default(), false),
        (WaveOptions::for_track("techno", 42.0), true),
    ] {
        assert_eq!(
            WaveRunner::prepare(&p, 22_050.0, &o, 0).turntable_brake,
            want
        );
    }
}

/// `forPalette`: the room and echo the palette plays in (another genre's), else the genre's, for
/// the fixes they hold; an echo of "none" is none; a palette that names neither, or none, is
/// `forGenre`'s.
#[test]
fn a_palette_plays_in_its_own_room_and_echo() {
    let set = PaletteSet::shared();
    let o = all();
    let shimmer = Fixes::ALL.contains(Fixes::SHIMMER);
    let mut rooms = 0;
    let mut dry = 0;
    let mut echoes = 0;
    for p in &set.palettes {
        let got = o.for_palette(Some(&p.id), "techno");
        let room = p.room.as_deref().unwrap_or("techno");
        assert_eq!(
            got.space,
            Some(SpaceSpec::for_genre(room, shimmer)),
            "{}",
            p.id
        );
        let echo = p.echo.as_deref().unwrap_or("techno");
        let want = if echo == "none" {
            None
        } else {
            EchoSpec::for_genre(echo)
        };
        assert_eq!(got.echo, want, "{}", p.id);
        rooms += p.room.is_some() as usize;
        dry += (p.echo.as_deref() == Some("none")) as usize;
        echoes += (p.echo.is_some() && p.echo.as_deref() != Some("none")) as usize;
    }
    assert!(
        rooms > 0 && dry > 0 && echoes > 0,
        "the shipped palettes name rooms and echoes"
    );
    // no palette (or one that is not shipped): the genre's own
    let genre = o.for_genre("techno");
    for name in [None, Some(""), Some("no-such-palette")] {
        let got = o.for_palette(name, "techno");
        assert_eq!(got.space, genre.space);
        assert_eq!(got.echo, genre.echo);
    }
    // only the fixes held: without .space / .echo nothing is set, and nothing else moves
    let off = WaveOptions {
        musicality: Fixes::ALL & !(Fixes::SPACE | Fixes::ECHO),
        ..WaveOptions::extended()
    };
    let moved = off.for_palette(Some(&set.palettes[0].id), "jazz");
    assert!(moved.space.is_none() && moved.echo.is_none());
    assert_eq!(moved.duck_depth, off.duck_depth);
}
