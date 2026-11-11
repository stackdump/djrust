//! The per-deck Sound picker (Swift's `SoundChoices` palette chooser): stepping a deck's
//! palette (the picker's arrows, the `soundPrev` / `soundNext` binding targets) plays the same
//! song from another palette, is logged as `setPalette`, carries into the deck's new tracks,
//! and goes with the track when it is copied onto the other deck.

mod common_track;

use common_track::Stepper;
use djrust::control::controller::{Action, SurfaceHost};
use djrust::control::registry;
use djrust::control::surface::DJActionKind;
use djrust::session::action::DeckAction;
use djrust::session::arrangement::ArrangeOp;
use djrust::session::model::DeckSource;
use djrust::sound::PaletteCard;
use djrust::TrackSpec;
use djrust::host::deck::PerformCtx;

fn loaded(genre: &str) -> Stepper {
    let mut st = Stepper::new(24_000.0, true);
    let spec = TrackSpec::new(genre, 11.0, Some("standard"));
    st.e.perform(
        &DeckAction::Load {
            deck: "A".into(),
            source: DeckSource::Net(spec),
        },
        PerformCtx::default(),
    );
    st
}

/// What the song plays: every net's notes, without what voices them.
fn notes(st: &Stepper, deck: &str) -> Vec<(String, Vec<(i64, i64, i64)>)> {
    let p = st.e.deck(deck).unwrap().project.as_ref().unwrap();
    p.nets
        .iter()
        .map(|(id, n)| {
            (
                id.to_string(),
                n.bindings
                    .iter()
                    .map(|(_, b)| (b.note, b.channel, b.velocity))
                    .collect(),
            )
        })
        .collect()
}

fn playable(st: &Stepper) -> Vec<String> {
    st.e.palette_cards("A")
        .into_iter()
        .filter(|c| c.unavailable.is_none())
        .map(|c| c.id)
        .collect()
}

fn card(id: &str, ok: bool, selected: bool) -> PaletteCard {
    PaletteCard {
        id: id.into(),
        label: id.into(),
        icon: String::new(),
        color: String::new(),
        description: String::new(),
        unavailable: (!ok).then(|| "not here".to_string()),
        local_only: false,
        selected,
    }
}

#[test]
fn stepping_skips_what_cannot_play_and_wraps() {
    let cards = [
        card("a", true, true),
        card("b", false, false),
        card("c", true, false),
    ];
    assert_eq!(PaletteCard::step(&cards, 1).as_deref(), Some("c"));
    assert_eq!(PaletteCard::step(&cards, -1).as_deref(), Some("c"));
    assert_eq!(PaletteCard::step(&cards, 2), None, "back where it was");
    assert_eq!(PaletteCard::step(&cards, 0), None);
    let none_lit = [card("a", true, false), card("c", true, false)];
    assert_eq!(PaletteCard::step(&none_lit, 1).as_deref(), Some("a"));
    assert_eq!(PaletteCard::step(&none_lit, -1).as_deref(), Some("c"));
    assert_eq!(PaletteCard::step(&[card("a", true, true)], 1), None);
}

#[test]
fn the_picker_lists_every_palette_with_the_decks_lit() {
    let st = loaded("house");
    let cards = st.e.palette_cards("A");
    assert_eq!(cards.len(), djrust::sound::PaletteSet::shared().palettes.len());
    assert_eq!(cards.iter().filter(|c| c.selected).count(), 1);
    assert!(playable(&st).len() >= 2, "{:?}", playable(&st));
    assert!(st.e.palette_cards("B").is_empty(), "no net on B");
}

#[test]
fn a_step_is_the_same_song_from_another_palette_and_carries_on() {
    let mut st = loaded("house");
    let before = notes(&st, "A");
    let was = st.e.palette("A");
    assert!(st.e.step_palette("A", 1));
    let now = st.e.palette("A");
    assert_ne!(now, was);
    assert_eq!(notes(&st, "A"), before, "the pattern doesn't change");
    let spec = st.e.deck("A").unwrap().spec.clone().unwrap();
    assert_eq!((spec.genre.as_str(), spec.seed), ("house", 11.0));
    assert_eq!(spec.palette(), now, "the source records it (saved, shared, replayed)");
    // and back
    assert!(st.e.step_palette("A", -1));
    assert_eq!(st.e.palette("A"), was);
    assert!(st.e.step_palette("A", 1));

    // the deck's next track keeps it
    let next = spec.next_version();
    assert!(st.e.generate("A", &next));
    assert_eq!(st.e.palette("A"), now);
}

#[test]
fn bound_prev_next_step_the_palette_and_log_set_palette() {
    for (kind, raw) in [
        (DJActionKind::SoundPrev, "soundPrev"),
        (DJActionKind::SoundNext, "soundNext"),
    ] {
        assert_eq!(DJActionKind::from_raw(raw), Some(kind));
        let info = registry::info(kind);
        assert!(info.label.contains("sound"), "{}", info.label);
    }
    let mut st = loaded("house");
    st.e.trace = Some(Vec::new());
    SurfaceHost::perform(
        &mut st.e,
        Action::SoundStep {
            deck: "A".into(),
            delta: 1,
        },
    );
    let now = st.e.palette("A");
    let traced = st.e.trace.take().unwrap();
    assert!(
        traced.iter().any(|t| t.ops
            == [ArrangeOp::Deck(DeckAction::SetPalette {
                deck: "A".into(),
                palette: now.clone(),
            })]
            && t.entry.text.contains("sound +1 on A")),
        "{:?}",
        traced.iter().map(|t| &t.entry.text).collect::<Vec<_>>()
    );
    // a deck with nothing on it: nothing happens
    st.e.trace = Some(Vec::new());
    SurfaceHost::perform(
        &mut st.e,
        Action::SoundStep {
            deck: "B".into(),
            delta: -1,
        },
    );
    assert_eq!(st.e.palette("B"), None);
}

#[test]
fn the_picker_goes_through_perform_so_it_records() {
    let mut st = loaded("synthwave");
    let target = playable(&st)
        .into_iter()
        .find(|id| Some(id) != st.e.palette("A").as_ref())
        .unwrap();
    st.e.trace = Some(Vec::new());
    let a = DeckAction::SetPalette {
        deck: "A".into(),
        palette: Some(target.clone()),
    };
    st.e.perform(&a, PerformCtx::default());
    let traced = st.e.trace.take().unwrap();
    assert!(traced.iter().any(|t| t.ops == [ArrangeOp::Deck(a.clone())]));
    assert!(st
        .e
        .palette_cards("A")
        .iter()
        .any(|c| c.selected && c.id == target));
}

#[test]
fn copy_deck_from_start_carries_the_sound() {
    let mut st = loaded("house");
    assert!(st.e.step_palette("A", 1));
    let pal = st.e.palette("A");
    st.e.trace = Some(Vec::new());
    assert!(st.e.copy_deck_from_start("A", "B"));
    assert_eq!(st.e.palette("B"), pal);
    assert_eq!(st.e.net_source("B"), st.e.net_source("A"));
    assert_eq!(notes(&st, "B"), notes(&st, "A"));
    let traced = st.e.trace.take().unwrap();
    assert!(traced
        .iter()
        .any(|t| matches!(&t.ops[..], [ArrangeOp::Deck(DeckAction::Load { deck, .. })] if deck == "B")));
    assert!(!st.e.copy_deck_from_start("A", "A"));
    assert!(!st.e.copy_deck_from_start("A", "C"));
    let mut empty = Stepper::new(24_000.0, true);
    assert!(!empty.e.copy_deck_from_start("A", "B"));
}
