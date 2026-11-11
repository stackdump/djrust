//! What a host does to a fresh engine before it plays: the settings `DJCore` leaves to its app.
//!
//! Swift's `DJEngine` is built for the app (`deterministic = false`, `autoGain` off per deck)
//! and `AppModel.start` finishes the job (`applyAutoGainPreference`, before the first load).
//! This engine's constructor is built for replay and tests (`deterministic` on, inline composes),
//! so the two live hosts, `djapp` and `djctl play`, start from [`host_engine`] and the same
//! setup is made in one place and tested once.

use crate::session::action::DeckAction;

use super::engine::{engine, Engine, EngineConfig, Renderer};
use super::perform::PerformCtx;

/// An engine as a host runs it.
///
/// `live`: real audio is playing and the control thread must not wait for a compose. The engine
/// runs as Swift's app runs it, with `deterministic` off: a seed-wheel click, the APC's
/// regenerate, the next track, Wander's swaps and look-ahead, auto-mix's loads and a relocation
/// compose on a worker, and [`Engine::service`] (`Rig::tick`) lands them, so the host must call
/// it about every 10 ms. Not live (`--screenshot`, `--device null` in `djctl play`, a headless
/// run) keeps the engine's own default: every compose finishes inline, the same inputs give the
/// same state every time.
///
/// `auto_gain`: sample decks level what they load to the app's reference
/// ([`Engine::apply_auto_gain_preference`]; Swift's default is on).
pub fn host_engine(config: EngineConfig, live: bool, auto_gain: bool) -> (Engine, Renderer) {
    let (mut e, r) = engine(config);
    e.deterministic = !live;
    e.apply_auto_gain_preference(auto_gain);
    (e, r)
}

impl Engine {
    /// `AppModel.applyAutoGainPreference`: every deck takes `on` as its auto-gain, through
    /// `perform(.param(autoGain))` so a recording session carries it (a replay sets it before
    /// anything loads), for the decks that are not there already. Call it before the first
    /// load: a sample loaded with it off plays at its own level until the setting changes.
    /// Swift's app does it at the start of `AppModel.start`, ahead of `loadDemo`, with the
    /// remembered "Auto level" chip (default on).
    pub fn apply_auto_gain_preference(&mut self, on: bool) {
        let mut names: Vec<String> = self.decks.iter().map(|d| d.name.clone()).collect();
        names.sort();
        for name in names {
            if self.auto_gain(&name) != on {
                self.perform(
                    &DeckAction::Param {
                        deck: name,
                        name: "autoGain".into(),
                        value: if on { 1.0 } else { 0.0 },
                    },
                    PerformCtx::default(),
                );
            }
        }
    }
}
