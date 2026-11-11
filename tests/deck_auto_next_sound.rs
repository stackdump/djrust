//! dj-only: the deck's sound survives Auto-next. The palette, the kit, the role voices, an
//! instrument pick, the sound panel's voice tweaks and the auto-gain the performer chose stay
//! on the deck when auto-next hands over to the song's next version, even when they were
//! chosen after that next version was already composed. A queued next that pins a sound of its
//! own keeps it.

mod common_track;

use common_track::Stepper;
use djrust::json::JsonValue;
use djrust::session::action::DeckAction;
use djrust::session::model::DeckSource;
use djrust::wave::VoiceTweak;
use djrust::TrackSpec;
use djrust::host::deck::PerformCtx;

const RATE: f64 = 22_050.0;

fn shaped() -> VoiceTweak {
    VoiceTweak {
        gain_db: Some(6.0),
        lowpass: Some(1200.0),
        distortion: Some(0.3),
        ..VoiceTweak::default()
    }
}

/// Deck A playing `genre` 11 (minimal), auto-next on, its next version already composed.
fn auto_deck(genre: &str) -> Stepper {
    let mut st = Stepper::new(RATE, true);
    assert!(st.e.deterministic);
    let spec = TrackSpec::new(genre, 11.0, Some("minimal"));
    st.e.perform(
        &DeckAction::Load {
            deck: "A".into(),
            source: DeckSource::Net(spec.clone()),
        },
        PerformCtx::default(),
    );
    st.e.play("A");
    st.e.deck_mut("A").unwrap().auto_advance = true;
    st.e.service();
    assert_eq!(
        st.e.next_ready("A"),
        Some(&spec.next_version()),
        "the next version is composed before any sound is picked"
    );
    st
}

fn perform(st: &mut Stepper, a: DeckAction) {
    st.e.perform(&a, PerformCtx::default());
}

fn sound_option(st: &Stepper, role: &str) -> Option<String> {
    djrust::sound::project_sound_net(st.e.deck("A").unwrap().project.as_ref().unwrap(), role)
        .unwrap_or_else(|| panic!("no {role} net"))
        .track
        .sound
        .option
        .clone()
}

/// The deck's sound as the performer sees it.
#[derive(Debug, PartialEq)]
struct Sound {
    palette: Option<String>,
    kit: Option<String>,
    bass: Option<String>,
    voice_settings: Vec<i64>,
    auto_gain: bool,
}

fn sound(st: &Stepper) -> Sound {
    let d = st.e.deck("A").unwrap();
    Sound {
        palette: st.e.palette("A"),
        kit: st.e.kit("A"),
        bass: sound_option(st, "bass"),
        voice_settings: d.voice_settings.keys().copied().collect(),
        auto_gain: d.auto_gain,
    }
}

/// Run until auto-next has handed over `n` times.
fn advance(st: &mut Stepper, n: u64) {
    st.e.set_tempo("A", 3000.0);
    let ok = st.until(4000 * 16 * n as usize, |s| {
        s.e.deck("A").unwrap().track.advances >= n
    });
    assert!(ok, "auto-next handed over");
}

fn another_palette(st: &Stepper) -> String {
    let now = st.e.palette("A");
    st.e.palette_cards("A")
        .into_iter()
        .filter(|c| c.unavailable.is_none() && Some(&c.id) != now.as_ref() && c.id != "studio")
        .map(|c| c.id)
        .next()
        .expect("another playable palette")
}

#[test]
fn sound_picked_after_the_next_was_composed_survives_auto_next() {
    let mut st = auto_deck("house");
    let seed = st.e.deck("A").unwrap().seed;
    let palette = another_palette(&st);
    let kit = "MuseScore_General.sf2#128:32";
    perform(
        &mut st,
        DeckAction::SetPalette {
            deck: "A".into(),
            palette: Some(palette.clone()),
        },
    );
    perform(
        &mut st,
        DeckAction::SetRoleVoice {
            deck: "A".into(),
            role: "bass".into(),
            voice: Some("inst:acid".into()),
        },
    );
    perform(
        &mut st,
        DeckAction::SetKit {
            deck: "A".into(),
            kit: Some(kit.into()),
        },
    );
    st.e.set_auto_gain("A", true);
    // the bass channel: its voice tweaked live in the sound panel
    let bass_ch = djrust::sound::project_sound_net(
        st.e.deck("A").unwrap().project.as_ref().unwrap(),
        "bass",
    )
    .unwrap()
    .track
    .channel;
    let relocated = st.until(40_000, |s| !s.e.deck("A").unwrap().track.is_busy());
    assert!(relocated);
    assert!(st.e.set_voice_tweak("A", &shaped(), bass_ch));
    let before = sound(&st);
    assert_eq!(before.kit.as_deref(), Some(kit));
    assert_eq!(before.bass.as_deref(), Some("inst:acid"));
    assert_eq!(before.voice_settings, vec![bass_ch]);
    assert!(before.auto_gain);
    assert!(before.palette.is_some());

    // the next version waiting in the slot was composed with the old sound: it is carried again
    st.e.service();
    let ready = st.e.next_ready("A").cloned().expect("a next version");
    assert_eq!(
        ready.palette(),
        st.e.deck("A").unwrap().spec.as_ref().unwrap().palette()
    );

    advance(&mut st, 1);
    let d = st.e.deck("A").unwrap();
    assert_ne!(d.seed, seed, "a new song");
    assert!(d.playing);
    assert_eq!(sound(&st), before, "the sound is the one picked");
    assert_eq!(
        st.e.voice_setting("A", bass_ch).map(|v| v.tweak),
        Some(shaped()),
        "the tweak rides along"
    );

    // and through the version after
    advance(&mut st, 2);
    assert_eq!(sound(&st), before);
}

#[test]
fn a_sound_changed_between_composing_and_the_end_still_carries() {
    // the palette is changed and the song ends in the same tick: the stale next version is not
    // handed over, the carried one is
    let mut st = auto_deck("techno");
    let palette = another_palette(&st);
    perform(
        &mut st,
        DeckAction::SetPalette {
            deck: "A".into(),
            palette: Some(palette),
        },
    );
    let want = st.e.palette("A");
    assert!(want.is_some());
    advance(&mut st, 1);
    assert_eq!(st.e.palette("A"), want);
}

#[test]
fn an_instrument_pick_rides_into_the_next_version() {
    let mut st = auto_deck("house");
    let ch = djrust::sound::project_sound_net(
        st.e.deck("A").unwrap().project.as_ref().unwrap(),
        "bass",
    )
    .unwrap()
    .track
    .channel;
    let project = st.e.deck("A").unwrap().project.clone().unwrap();
    st.e.retarget("A", project, |src| src.with_instrument("synth-bass", ch));
    let picked = |st: &Stepper| {
        st.e.deck("A")
            .unwrap()
            .spec
            .as_ref()
            .and_then(|s| s.layers.get("tracks").cloned())
            .and_then(|t| t.as_object().and_then(|o| o.get(&ch.to_string())).cloned())
            .and_then(|e| e.as_object().and_then(|o| o.get("instrument")).cloned())
    };
    assert_eq!(picked(&st), Some(JsonValue::str("synth-bass")));
    advance(&mut st, 1);
    assert_eq!(picked(&st), Some(JsonValue::str("synth-bass")));
}

#[test]
fn a_queued_next_that_pins_its_own_palette_keeps_it() {
    let mut st = auto_deck("house");
    let deck_palette = another_palette(&st);
    perform(
        &mut st,
        DeckAction::SetPalette {
            deck: "A".into(),
            palette: Some(deck_palette.clone()),
        },
    );
    perform(
        &mut st,
        DeckAction::SetRoleVoice {
            deck: "A".into(),
            role: "bass".into(),
            voice: Some("gm:32".into()),
        },
    );
    let mine = st.e.palette("A");
    let pinned =
        st.e.palette_cards("A")
            .into_iter()
            .filter(|c| c.unavailable.is_none() && Some(&c.id) != mine.as_ref() && c.id != "studio")
            .map(|c| c.id)
            .next()
            .expect("a third palette");
    let queued = st.e.with_sounds(|ctx| {
        TrackSpec::new("house", 500.0, Some("minimal")).with_palette(Some(&pinned), ctx.installed)
    });
    let pinned_id = queued.palette();
    st.e.queue_next("A", &queued);
    assert!(st.e.advance_to_next("A"));
    assert_eq!(st.e.deck("A").unwrap().seed, 500.0);
    assert_eq!(st.e.palette("A"), pinned_id, "its own palette");
    assert_ne!(st.e.palette("A"), mine);
    assert_eq!(
        sound_option(&st, "bass").as_deref(),
        Some("gm:32"),
        "the deck's role voice, which it does not pin, rides along"
    );
}

#[test]
fn auto_next_with_no_sound_picked_is_the_plain_next_version() {
    let mut st = auto_deck("techno");
    let spec = st.e.deck("A").unwrap().spec.clone().unwrap();
    advance(&mut st, 1);
    assert_eq!(
        st.e.deck("A").unwrap().spec.as_ref(),
        Some(&spec.next_version())
    );
}
