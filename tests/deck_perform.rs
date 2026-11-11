//! `DeckPerform.swift`'s pure rules: STEMS groups, disabled roles, the mix-out / mix-in tick
//! arithmetic, section stepping and scale-locked keys. The expectations are the numbers
//! `AutoMixTests.swift` asserts (the mix-out tail `MixOutTail(fromTick: 8·16, bars: 4)`,
//! mix-in rests) and values worked out from the Swift source.

use djrust::deck::perform::*;
use djrust::deck::timeline::SectionInfo;
use djrust::wave::rests::{MixOutTail, RestSchedule};
use djrust::wave::runner::RoleFilter;

fn roles() -> Vec<String> {
    ["kick", "snare", "hihat", "bass", "melody", "harmony", "arp"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

fn section(name: &str, start_bar: i64, bars: i64) -> SectionInfo {
    SectionInfo {
        name: name.into(),
        start_tick: start_bar * 16,
        steps: bars * 16,
        phrase_count: 1,
        phrase_pattern: vec![],
    }
}

#[test]
fn stems_are_the_music_and_the_lead() {
    assert_eq!(stem_names(), ["mic", "music"]);
    assert_eq!(
        stem_roles("music", &roles()),
        ["bass", "melody", "harmony", "arp"]
    );
    assert_eq!(stem_roles("mic", &roles()), ["melody"]);
    assert!(stem_roles("nothing", &roles()).is_empty());
    // a deck without a melody has no mic
    let drums: Vec<String> = vec!["kick".into(), "bass".into()];
    assert!(stem_roles("mic", &drums).is_empty());
    assert_eq!(stem_roles("music", &drums), ["bass"]);
}

#[test]
fn a_stem_button_brings_back_only_what_it_silenced() {
    let rs = roles();
    // melody is already muted by a section; bass, harmony and arp are playing
    let on = |r: &str| r != "melody";
    let mut st = StemsMuted::default();
    let ops = st.toggle("music", &rs, &on);
    assert_eq!(
        ops,
        [
            ("bass".into(), false),
            ("melody".into(), false),
            ("harmony".into(), false),
            ("arp".into(), false)
        ]
    );
    assert!(st.is_muted("music"));
    assert_eq!(
        st.groups["music"],
        ["bass", "harmony", "arp"],
        "the muted melody was not the button's"
    );
    // pressing again brings back those three, not the melody
    let back = st.toggle("music", &rs, &|_| false);
    assert_eq!(
        back,
        [
            ("bass".into(), true),
            ("harmony".into(), true),
            ("arp".into(), true)
        ]
    );
    assert!(!st.is_muted("music"));
    // two groups overlap on the melody: the first to come back leaves it to the other
    let mut st = StemsMuted::default();
    st.toggle("mic", &rs, &|_| true);
    st.toggle("music", &rs, &|r| r != "melody");
    assert_eq!(
        st.groups["music"],
        ["bass", "melody", "harmony", "arp"],
        "the mic group holds the melody"
    );
    let back = st.toggle("music", &rs, &|_| false);
    assert_eq!(
        back,
        [
            ("bass".into(), true),
            ("harmony".into(), true),
            ("arp".into(), true)
        ]
    );
}

#[test]
fn a_disabled_role_stays_out_and_stays_off() {
    let f = set_role_disabled(None, "bass", true).unwrap();
    assert_eq!(f.mute, ["bass"]);
    let f2 = set_role_disabled(Some(f), "arp", true).unwrap();
    assert_eq!(f2.mute, ["bass", "arp"]);
    let f3 = set_role_disabled(Some(f2), "bass", true).unwrap();
    assert_eq!(f3.mute.len(), 2, "no duplicates");
    let f4 = set_role_disabled(Some(f3), "bass", false).unwrap();
    assert_eq!(f4.mute, ["arp"]);
    assert_eq!(
        set_role_disabled(Some(f4), "arp", false),
        None::<RoleFilter>,
        "nothing disabled: no filter"
    );
    assert_eq!(toggle_role(&["bass".into()], "bass", false), None);
    assert_eq!(toggle_role(&["bass".into()], "arp", true), Some(false));
}

#[test]
fn a_change_from_the_next_bar_starts_where_the_swap_does() {
    // playing: the swap's tick is the last of the bar (16k − 1), the change begins one on
    assert_eq!(next_swap_tick(0), 15);
    assert_eq!(next_swap_tick(15), 15);
    assert_eq!(next_swap_tick(16), 31);
    assert_eq!(next_swap_tick(-4), 15);
    assert_eq!(next_bar_start(true, 20), (31, 32));
    // stopped: from where it stands, the bar line at or after
    assert_eq!(next_bar_start(false, 20), (20, 32));
    assert_eq!(next_bar_start(false, 32), (32, 32));
    assert_eq!(next_bar_start(false, 0), (0, 0));
}

#[test]
fn a_mix_out_plays_the_outro_or_the_bar_line() {
    // AutoMixTests: no outro ahead → the tail begins on the hand-off bar line 8·16, 4 bars
    let song = [section("intro", 0, 8), section("verse", 8, 16)];
    let (tick, tail) = mix_out_point(&song, 8, 4);
    assert_eq!(tick, 8 * 16 - 1);
    assert_eq!(tail, MixOutTail::new(8 * 16, 4));
    // an outro still ahead: jump to it
    let with = [
        section("intro", 0, 8),
        section("verse", 8, 16),
        section("outro", 24, 8),
    ];
    let (tick, tail) = mix_out_point(&with, 20, 8);
    assert_eq!((tick, tail), (24 * 16, MixOutTail::new(24 * 16, 8)));
    // an outro already behind the bar: play on
    let (tick, tail) = mix_out_point(&with, 28, 8);
    assert_eq!((tick, tail), (28 * 16 - 1, MixOutTail::new(28 * 16, 8)));
    // bars below 1 are 1
    assert_eq!(mix_out_point(&song, 8, 0).1.bars, 1);
    // by hand: at the next bar of a playing deck
    let (tick, tail) = mix_out(&with, true, 20 * 16 + 3, 4);
    assert_eq!((tick, tail), (24 * 16, MixOutTail::new(24 * 16, 4)));
    let (tick, tail) = mix_out(&song, true, 2 * 16 + 3, 4);
    assert_eq!((tick, tail), (3 * 16 - 1, MixOutTail::new(3 * 16, 4)));
    assert_eq!(MixOutTail::DEFAULT_BARS, 16);
}

#[test]
fn a_mix_in_rests_the_melodic_lanes_for_its_bars() {
    let (tick, rest) = mix_in(true, 5, 4).unwrap();
    assert_eq!(tick, 15);
    assert_eq!((rest.from, rest.to), (16, 16 + 4 * 16));
    assert_eq!(rest, RestSchedule::mix_in(16, 4));
    assert!(mix_in(true, 5, 0).is_none());
    assert!(is_mixing_in(Some(80), 79) && !is_mixing_in(Some(80), 80) && !is_mixing_in(None, 0));
}

#[test]
fn sections_launch_and_step_with_the_clock() {
    let s = [
        section("intro", 0, 8),
        section("verse", 8, 16),
        section("outro", 24, 8),
    ];
    assert_eq!(current_section_index(&s, 0), Some(0));
    assert_eq!(current_section_index(&s, 8 * 16), Some(1));
    assert_eq!(current_section_index(&s, 24 * 16 - 1), Some(1));
    assert_eq!(current_section_index(&s, 32 * 16), None, "past the end");
    assert_eq!(launch_section_tick(&s, 2), Some(24 * 16));
    assert_eq!(launch_section_tick(&s, 3), None);
    assert_eq!(step_section_tick(&s, 9 * 16, 1), Some(24 * 16));
    assert_eq!(step_section_tick(&s, 9 * 16, -1), Some(0));
    assert_eq!(
        step_section_tick(&s, 9 * 16, 5),
        Some(24 * 16),
        "clamped to the last"
    );
    assert_eq!(step_section_tick(&s, 9 * 16, -5), Some(0), "and the first");
    assert_eq!(step_section_tick(&[], 0, 1), None);
    assert_eq!(
        step_section_tick(&s, 99 * 16, 1),
        Some(8 * 16),
        "no current section counts as the first"
    );
}

#[test]
fn keys_snap_to_the_scale_with_ties_going_down() {
    // A minor (root 57): A B C D E F G
    assert_eq!(
        scale_locked(61, Some(57), "Minor"),
        60,
        "C♯ is a tie between C and D: down"
    );
    assert_eq!(
        scale_locked(58, Some(57), "Minor"),
        57,
        "B♭ is a tie between A and B: down"
    );
    assert_eq!(scale_locked(60, Some(57), "Minor"), 60);
    assert_eq!(
        scale_locked(63, Some(57), "Minor"),
        62,
        "E♭: D (1) vs E (1): down"
    );
    // a pentatonic scale leaves wider gaps
    assert_eq!(scale_locked(60, Some(57), "MinPentatonic"), 60);
    assert_eq!(scale_locked(62, Some(57), "MinPentatonic"), 62);
    assert_eq!(
        scale_locked(66, Some(57), "MinPentatonic"),
        67,
        "F♯ is a semitone below G"
    );
    assert_eq!(
        scale_locked(65, Some(57), "MinPentatonic"),
        64,
        "F: E is 1 away, G is 2"
    );
    // no key known: the note is its own
    assert_eq!(scale_locked(61, None, "Minor"), 61);
}
