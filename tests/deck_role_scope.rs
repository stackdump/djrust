//! `WaveDeckSource.isRoleAudible` answers for every key of the latest project's `roleTargets`: a
//! riff group or a plain net id (`hit1`..`hit4`, a riff variant), not only the eight roles of
//! `Deck.roleOrder` the mixer shows (`Deck.isRoleOn` is `mode == .net && wave.isRoleAudible`).
//! `Deck.toggleRole` and `enforceDisabledRoles` take any string a session or a journal records
//! (`DeckAction.roleDisable`), so they depend on it. No Swift test names it: the literals are
//! the source's (WaveDeckSource.swift:334-343, DeckPerform.swift:280-309).

mod common_track;

use common_track::Stepper;
use djrust::session::action::DeckAction;
use djrust::session::model::DeckSource;
use djrust::TrackSpec;
use djrust::host::deck::PerformCtx;

const SR: f64 = 22_050.0;

fn playing() -> Stepper {
    let mut st = Stepper::new(SR, true);
    let a = DeckAction::Load {
        deck: "A".into(),
        source: DeckSource::Net(TrackSpec::new("techno", 42.0, None)),
    };
    st.e.perform(&a, PerformCtx::default());
    st.e.perform(
        &DeckAction::Play {
            deck: "A".into(),
            on: true,
        },
        PerformCtx::default(),
    );
    st.step(60);
    st
}

#[test]
fn a_plain_net_outside_the_mixer_rows_is_audible_unless_muted() {
    let mut st = playing();
    // the stinger nets (`hit1`) are plain nets of the role table and not one of `roleOrder`:
    // they sit muted until a Hit fires
    assert!(!st.e.is_role_audible("A", "hit1"), "muted at the start");
    assert!(st.e.set_mute("A", "hit1", false));
    st.step(2);
    assert!(st.e.is_role_audible("A", "hit1"), "unmuted: audible");
    assert!(st.e.set_mute("A", "hit1", true));
    st.step(2);
    assert!(!st.e.is_role_audible("A", "hit1"));
    // a name the song's table has no key for is not audible, and a deck with no net answers no
    assert!(!st.e.is_role_audible("A", "no-such-role"));
    let mut sample = Stepper::new(SR, true);
    sample.e.load_demo("A");
    assert!(!sample.e.is_role_audible("A", "kick"));
    assert!(!sample.e.is_role_audible("A", "hit1"));
}

#[test]
fn toggling_a_role_that_is_not_a_mixer_row_mutes_and_unmutes_it() {
    let mut st = playing();
    st.e.set_mute("A", "hit2", false);
    st.step(2);
    assert!(st.e.is_role_audible("A", "hit2"));
    // (`toggleRole`: `setRole(role, on: !isRoleOn(role))`: it was never muted by Rust's toggle
    // when the role was not one of the rows, which answered 'silent' and un-muted it)
    assert!(st.e.toggle_role("A", "hit2"));
    st.step(2);
    assert!(!st.e.is_role_audible("A", "hit2"), "toggled off");
    assert!(st.e.toggle_role("A", "hit2"));
    st.step(2);
    assert!(st.e.is_role_audible("A", "hit2"), "and on again");
}

#[test]
fn a_disabled_role_outside_the_mixer_rows_is_muted_again_when_a_section_brings_it_back() {
    let mut st = playing();
    st.e.set_mute("A", "hit3", false);
    st.step(2);
    assert!(st.e.is_role_audible("A", "hit3"));
    st.e.set_role_disabled("A", "hit3", true);
    st.step(2);
    assert!(!st.e.is_role_audible("A", "hit3"), "disabling muted it");
    // a section of the song unmutes it before the next compile ...
    st.e.set_mute("A", "hit3", false);
    st.step(2);
    assert!(st.e.is_role_audible("A", "hit3"));
    // ... and the housekeeping puts it back (`enforceDisabledRoles`)
    st.e.enforce_disabled_roles();
    st.step(2);
    assert!(!st.e.is_role_audible("A", "hit3"));
}
