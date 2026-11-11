//! The action trace (beyond Swift; docs/ACTION-LOG.md): what each hand on the engine did, as
//! words for the action log and as session ops for an arrangement.
//!
//! The engine records into [`Engine::trace`] while it is `Some` (the app turns it on): a
//! controller's [`Action`] after it is applied (so the words and the ops say what came of it:
//! the seed the wheel landed on, whether a part went on or off), and every [`DeckAction`] that
//! goes through `Engine::perform` (the window, the socket, a playback), as it was recorded.
//! The app stamps each with the bar, beat and tempo and keeps it
//! (`crate::control::action_log`, `crate::session::arrangement`).
//!
//! Some moves have no session op: the headphones, PFL, live keys, nudges, the input history and
//! captures, binding targets. They are logged in words with no ops, so an arrangement leaves
//! them out.

use crate::control::action_log::Entry;
use crate::journal::event::JournalEvent;
use crate::json::JsonValue;
use crate::control::controller::{Action, DeckParam};
use crate::session::action::DeckAction;
use crate::session::arrangement::{ArrangeOp, SwapStart};
use crate::session::model::DeckSource;

use crate::deck::beat::Quantize;
use super::engine::Engine;

/// One traced move.
#[derive(Clone, Debug, PartialEq)]
pub struct Traced {
    /// The log line's words and how it folds.
    pub entry: Entry,
    /// What an arrangement keeps of it (empty: words only).
    pub ops: Vec<ArrangeOp>,
}

fn param_name(p: DeckParam) -> (&'static str, &'static str) {
    match p {
        DeckParam::Volume => ("volume", "Volume"),
        DeckParam::Filter => ("filter", "Filter"),
        DeckParam::Gain => ("gain", "Gain"),
        DeckParam::EqLow => ("eqLow", "EQ low"),
        DeckParam::EqMid => ("eqMid", "EQ mid"),
        DeckParam::EqHigh => ("eqHigh", "EQ high"),
        DeckParam::Pitch => ("pitch", "Tempo fader"),
        DeckParam::ReverbWet => ("reverbWet", "Reverb wet"),
        DeckParam::ReverbSize => ("reverbSize", "Reverb size"),
    }
}

/// The `keys` live op for a key: what [`Engine::live_keys`] plays.
fn key_op(deck: &str, index: usize, velocity: i64, down: bool, quant: &str) -> ArrangeOp {
    use crate::json::{JsonObject, JsonValue};
    let mut o = JsonObject::from_pairs([
        ("deck", JsonValue::str(deck)),
        ("note", JsonValue::int(index as i64)),
        ("down", JsonValue::Bool(down)),
    ]);
    if down {
        o.set("velocity", JsonValue::int(velocity));
        o.set("quant", JsonValue::str(quant));
    }
    ArrangeOp::Live {
        method: "keys".into(),
        params: JsonValue::Object(o),
    }
}

fn signed(d: i64) -> String {
    if d > 0 {
        format!("+{d}")
    } else {
        d.to_string()
    }
}

fn on_off(on: bool) -> &'static str {
    if on {
        "on"
    } else {
        "off"
    }
}

impl Engine {
    /// The seed deck `deck` is composing to, else playing.
    fn seed_of(&self, deck: &str) -> Option<f64> {
        let d = self.deck(deck)?;
        d.track
            .seed_target
            .as_ref()
            .or(d.spec.as_ref())
            .map(|s| s.seed)
    }

    /// A swap op for what deck `deck` is now composing to (or playing).
    pub(crate) fn swap_op(&self, deck: &str) -> Option<ArrangeOp> {
        let d = self.deck(deck)?;
        let spec = d.track.seed_target.clone().or_else(|| d.spec.clone())?;
        Some(ArrangeOp::Swap {
            deck: deck.to_string(),
            source: DeckSource::Net(spec),
            start: if d.is_net_loop() {
                SwapStart::Top
            } else {
                SwapStart::Bar
            },
        })
    }

    /// Is anything listening to the moves: the action trace, or the journal (which keeps the
    /// live ops that neither a `DeckAction` nor MIDI carries)? The places that build a
    /// [`Traced`] only when someone wants it test this.
    pub(crate) fn tracing(&self) -> bool {
        self.trace.is_some() || self.journal.is_some()
    }

    /// A live op into the journal alone ([`JournalEvent::Live`]): by a hand, not a
    /// controller's action (the journal has the MIDI, and a replay of it makes the same move).
    pub(crate) fn journal_live(&mut self, method: &str, params: JsonValue) {
        if self.in_surface || self.journal.is_none() {
            return;
        }
        let frame = self.frame();
        if let Some(j) = self.journal.as_mut() {
            j.record(
                frame,
                JournalEvent::Live {
                    method: method.to_string(),
                    params,
                },
            );
        }
    }

    /// Record `t` if tracing: onto the trace, and its live ops into the journal
    /// ([`JournalEvent::Live`]) unless a controller's action is being applied (the journal has
    /// the MIDI, and a replay of it makes the same move again).
    pub(crate) fn trace_push(&mut self, t: Traced) {
        for op in &t.ops {
            if let ArrangeOp::Live { method, params } = op {
                self.journal_live(method, params.clone());
            }
        }
        if let Some(v) = self.trace.as_mut() {
            v.push(t);
        }
    }

    /// A move of the window, the socket or a playback that is a live op and has no `DeckAction`
    /// (the headphone cue, a nudge, a capture, a library save…): its words and the op that makes
    /// it again, `params` as resolved (what was set, not what was asked). Not when a
    /// controller's action is being applied: that one is described by
    /// [`describe_control`](Engine::trace_control), with the same op, once it is done.
    pub(crate) fn note_live(
        &mut self,
        key: String,
        text: String,
        fold: bool,
        method: &str,
        params: JsonValue,
    ) {
        if self.in_surface {
            return;
        }
        self.note_resolved(key, text, fold, method, params);
    }

    /// [`note_live`](Engine::note_live) for a move whose outcome only the engine knows (a
    /// capture that grabbed, a loaded library loop): noted whoever made it, a controller too
    /// (its action then describes nothing). The journal still takes it only from a hand
    /// ([`trace_push`](Engine::trace_push)).
    pub(crate) fn note_resolved(
        &mut self,
        key: String,
        text: String,
        fold: bool,
        method: &str,
        params: JsonValue,
    ) {
        if !self.tracing() || self.quiet {
            return;
        }
        let via = if self.trace_via.is_empty() {
            "app".to_string()
        } else {
            self.trace_via.clone()
        };
        self.trace_push(Traced {
            entry: Entry {
                source: via,
                key,
                text,
                step: None,
                fold,
            },
            ops: vec![ArrangeOp::Live {
                method: method.to_string(),
                params,
            }],
        });
    }

    /// Play one arrangement move (`crate::session::arrangement`): a `swap` re-composes the
    /// deck and hands over on its next bar (from the top for `start: top`), as the wheel does;
    /// any other op goes through [`perform`](Engine::perform). False for a swap this engine
    /// can't make (no such deck, or a source that is not a composed net).
    pub fn apply_arrange_op(&mut self, op: &ArrangeOp) -> bool {
        match op {
            ArrangeOp::Swap {
                deck,
                source: DeckSource::Net(spec),
                start,
            } => {
                let s = match start {
                    SwapStart::Top => crate::deck::slots::SwapStart::Top,
                    SwapStart::Bar => crate::deck::slots::SwapStart::NextBar,
                };
                let ok = self.swap_to(deck, spec, s);
                if ok {
                    self.trace_push_text(&format!("swap {deck} → seed {:.0}", spec.seed));
                }
                ok
            }
            ArrangeOp::Swap { .. } => false,
            ArrangeOp::Deck(a) => {
                self.perform(a, super::perform::PerformCtx::default());
                true
            }
            ArrangeOp::Live { method, params } => match self.apply_live_op(method, params) {
                Ok(()) => true,
                Err(e) => {
                    self.trace_push_text(&format!("{method}: {e}"));
                    false
                }
            },
        }
    }

    /// A line in words only, from whoever drives the engine now (`trace_via`): a playback's
    /// swap, say. No ops: it is not a move of its own.
    pub fn trace_push_text(&mut self, text: &str) {
        self.trace_push_text_ops(text, Vec::new());
    }

    /// [`trace_push_text`](Engine::trace_push_text) with the ops that make the move again.
    pub(crate) fn trace_push_text_ops(&mut self, text: &str, ops: Vec<ArrangeOp>) {
        if self.trace.is_none() && !ops.iter().any(|o| matches!(o, ArrangeOp::Live { .. })) {
            return;
        }
        let via = self.trace_via.clone();
        self.trace_push(Traced {
            entry: Entry {
                source: via.clone(),
                key: format!("{via}:{text}"),
                text: format!("{via}: {text}"),
                step: None,
                fold: false,
            },
            ops: Vec::new(),
        });
    }

    /// A `DeckAction` performed through `perform` (window, socket, playback), as recorded.
    pub(crate) fn trace_deck_action(&mut self, a: &DeckAction) {
        if self.trace.is_none() {
            return;
        }
        let via = if self.trace_via.is_empty() {
            "app".to_string()
        } else {
            self.trace_via.clone()
        };
        let deck = a.deck().unwrap_or_default();
        let fold = matches!(
            a,
            DeckAction::Param { .. }
                | DeckAction::Crossfader(_)
                | DeckAction::Master(_)
                | DeckAction::Fx { .. }
                | DeckAction::MasterFx { .. }
                | DeckAction::ClipGain { .. }
        );
        let key = match a {
            DeckAction::Param { deck, name, .. } => format!("param:{deck}:{name}"),
            DeckAction::Fx { deck, param, .. } => format!("fx:{deck}:{param}"),
            DeckAction::MasterFx { deck, key, .. } => format!("mfx:{deck}:{key}"),
            _ => format!("{}:{}:{deck}", via, a.op()),
        };
        let mut text = a.json().js_stringified();
        if text.len() > 120 {
            text.truncate(117);
            text.push('…');
        }
        self.trace_push(Traced {
            entry: Entry {
                source: via,
                key,
                text,
                step: None,
                fold,
            },
            ops: vec![ArrangeOp::Deck(a.clone())],
        });
    }

    /// A controller's action, after it was applied: its words and its session ops. `via`
    /// names the control ("Inpulse browser", "APC pad 3"); `focus_before` is the aimed deck
    /// before it (re-aiming at the same deck is not logged).
    pub(crate) fn trace_control(&mut self, a: &Action, via: &str, focus_before: &str) {
        if self.trace.is_none() {
            return;
        }
        // a press that aims at the deck already aimed at (before its macro, say) is no move
        if matches!(a, Action::Focus { deck } if deck == focus_before) {
            return;
        }
        let Some((key, text, step, fold, ops)) = self.describe_control(a, via) else {
            return;
        };
        self.trace_push(Traced {
            entry: Entry {
                source: via.to_string(),
                key,
                text,
                step,
                fold,
            },
            ops,
        });
    }

    #[allow(clippy::type_complexity)]
    fn describe_control(
        &self,
        a: &Action,
        via: &str,
    ) -> Option<(String, String, Option<i64>, bool, Vec<ArrangeOp>)> {
        use Action as X;
        let d = |a: DeckAction| vec![ArrangeOp::Deck(a)];
        let playing = |deck: &str| self.deck(deck).is_some_and(|x| x.playing);
        let said = |what: String| format!("{via}: {what}");
        let live = |method: &str, pairs: [(&'static str, JsonValue); 1]| ArrangeOp::Live {
            method: method.to_string(),
            params: JsonValue::Object(crate::json::JsonObject::from_pairs(pairs)),
        };
        let is_input = |name: Option<&String>| {
            self.capture_input(name.map(String::as_str)).is_some()
                && self.non_input_source(name.map(String::as_str)).is_none()
        };
        Some(match a {
            X::Deck { deck, param, value } => {
                let (name, label) = param_name(*param);
                (
                    format!("param:{deck}:{name}"),
                    said(format!("{label} {deck} {value:.2}")),
                    None,
                    true,
                    d(DeckAction::Param {
                        deck: deck.clone(),
                        name: name.into(),
                        value: *value,
                    }),
                )
            }
            X::MasterFx { deck, id, value } => (
                format!("mfx:{deck}:{id}"),
                said(format!("{id} {value} on {deck}")),
                None,
                true,
                d(DeckAction::MasterFx {
                    deck: deck.clone(),
                    key: id.clone(),
                    value: *value as f64,
                }),
            ),
            X::FxUnit { deck, param, value } => (
                format!("fx:{deck}:{}", param.id()),
                said(format!("FX {} {value:.2} on {deck}", param.id())),
                None,
                true,
                d(DeckAction::Fx {
                    deck: deck.clone(),
                    param: param.id().into(),
                    value: *value,
                }),
            ),
            X::FxKill { deck, on, on_beat } => (
                format!("fxkill:{deck}"),
                said(format!("FX kill {} on {deck}", on_off(*on))),
                None,
                false,
                d(DeckAction::FxKill {
                    deck: deck.clone(),
                    on: *on,
                    on_beat: *on_beat,
                }),
            ),
            X::FxReset { deck, on_beat } => (
                format!("fxreset:{deck}"),
                said(format!("FX reset on {deck}")),
                None,
                false,
                d(DeckAction::FxReset {
                    deck: deck.clone(),
                    on_beat: *on_beat,
                }),
            ),
            X::Focus { deck } => (
                format!("focus:{deck}"),
                said(format!("aim at {deck}")),
                None,
                false,
                d(DeckAction::Focus { deck: deck.clone() }),
            ),
            X::Regenerate { deck } => (
                format!("regen:{deck}"),
                said(format!(
                    "regenerate {deck} → seed {}",
                    self.seed_of(deck).map_or("?".into(), |s| format!("{s:.0}"))
                )),
                None,
                false,
                self.swap_op(deck).into_iter().collect(),
            ),
            X::AutoNextToggle { deck } => {
                let on = self.deck(deck).is_some_and(|x| x.auto_advance);
                (
                    format!("autonext:{deck}"),
                    said(format!("auto-next {} on {deck}", on_off(on))),
                    None,
                    false,
                    d(DeckAction::AutoAdvance {
                        deck: deck.clone(),
                        on,
                    }),
                )
            }
            X::Crossfader(v) => (
                "crossfader".into(),
                said(format!("Crossfader {v:.2}")),
                None,
                true,
                d(DeckAction::Crossfader(*v)),
            ),
            X::Master(v) => (
                "master".into(),
                said(format!("Master {v:.2}")),
                None,
                true,
                d(DeckAction::Master(*v)),
            ),
            X::HeadphoneMix(v) => (
                "hpmix".into(),
                said(format!("Cue mix {v:.2}")),
                None,
                true,
                vec![live("cue", [("mix", JsonValue::Number(v.clamp(0.0, 1.0)))])],
            ),
            X::HeadphoneLevel(v) => (
                "hplevel".into(),
                said(format!("Cue level {v:.2}")),
                None,
                true,
                vec![live("cue", [("level", JsonValue::Number(v.clamp(0.0, 1.0)))])],
            ),
            X::ClipToggle { slot, on } => {
                let id = self.loops.row.clips.get(*slot).map(|c| (c.id.clone(), c.title.clone()));
                let (id, title) = id.unwrap_or_default();
                (
                    format!("clip:{slot}"),
                    said(format!("loop {title} {}", if *on { "in" } else { "out" })),
                    None,
                    false,
                    if id.is_empty() {
                        vec![]
                    } else {
                        d(DeckAction::ClipLoop { id, on: *on })
                    },
                )
            }
            // a LOOP binding's `source`: a bus is traced by the call it makes (with what came
            // of it), an input in words here (its capture has no session op)
            X::LoopRecord { deck, .. } if self.index(deck).is_none() && self.names_bus(Some(deck)) => {
                return None
            }
            // an input's capture is noted by the engine call, with what came of it
            X::LoopRecord { deck, .. }
                if self.index(deck).is_none()
                    && matches!(self.loop_source(deck, false), Some(super::loop_source::LoopSource::Input(_))) =>
            {
                return None
            }
            X::LoopRecord { deck, .. } if self.history_aimed(deck).is_some() => return None,
            X::LoopRecord { deck, on } if self.index(deck).is_none() => (
                format!("looprec:{deck}"),
                said(format!("loop record {deck} {}", if *on { "down" } else { "up" })),
                None,
                false,
                vec![],
            ),
            X::CaptureGrab { input, .. } | X::CaptureArm { input, .. }
                if self.names_bus(input.as_deref()) =>
            {
                return None
            }
            X::LoopRecord { deck, on } => (
                format!("looprec:{deck}"),
                said(format!("loop record {deck} {}", if *on { "down" } else { "up" })),
                None,
                false,
                d(DeckAction::LoopRecord {
                    deck: deck.clone(),
                    on: *on,
                    bars: None,
                }),
            ),
            X::Nudge { deck, amount } => (
                format!("nudge:{deck}"),
                said(format!("nudge {deck} {amount:+.2}")),
                None,
                true,
                vec![ArrangeOp::Live {
                    method: "nudge".into(),
                    params: JsonValue::Object(crate::json::JsonObject::from_pairs([
                        ("deck", JsonValue::str(deck)),
                        ("ratio", JsonValue::Number(amount * 0.08)),
                    ])),
                }],
            ),
            X::SeedStep { delta, decks } => {
                // the decks it turned: the armed ones, else the one the host picked (the focus)
                let turned: Vec<String> = if decks.is_empty() {
                    vec![self.focus.clone()]
                } else {
                    decks.clone()
                };
                let turned: Vec<String> = turned
                    .into_iter()
                    .filter(|n| self.deck(n).is_some_and(|x| x.is_net() && x.spec.is_some()))
                    .collect();
                if turned.is_empty() {
                    return None;
                }
                let names = turned.join("+");
                let seeds: Vec<String> = turned
                    .iter()
                    .map(|n| self.seed_of(n).map_or("?".into(), |s| format!("{s:.0}")))
                    .collect();
                (
                    format!("browser:{names}"),
                    format!("Browser {names} {} → seed {}", signed(*delta), seeds.join(", ")),
                    Some(*delta),
                    false,
                    turned.iter().filter_map(|n| self.swap_op(n)).collect(),
                )
            }
            X::WanderToggle { deck, on } => (
                format!("wander:{deck}"),
                said(format!("wander {} on {deck}", on_off(*on))),
                None,
                false,
                d(DeckAction::Wander {
                    deck: deck.clone(),
                    on: *on,
                }),
            ),
            X::WanderPace { deck, delta } => {
                let bars = self.deck(deck).map_or(0, |x| x.walk.wander.pace);
                (
                    format!("wanderpace:{deck}"),
                    said(format!("wander pace {deck} {} → {bars} bars", signed(*delta))),
                    Some(*delta),
                    false,
                    d(DeckAction::WanderPace {
                        deck: deck.clone(),
                        bars,
                    }),
                )
            }
            X::AutoMixSet { on } => {
                let s = &self.auto_mix.settings;
                (
                    "automix".into(),
                    said(format!("auto-mix {}", on_off(*on))),
                    None,
                    false,
                    d(DeckAction::AutoMix {
                        on: *on,
                        bars: s.bars,
                        every: s.every,
                    }),
                )
            }
            X::AutoMixNow => (
                "automixnow".into(),
                said("auto-mix now".into()),
                None,
                false,
                d(DeckAction::AutoMixNow),
            ),
            // a live key is a `keys` live op, so an arrangement keeps it and replays it (with the
            // quantise it was played on); the release is an op too but no line of its own
            X::KeyDown {
                deck,
                index,
                velocity,
                quant,
            } => {
                let q = quant
                    .clone()
                    .unwrap_or_else(|| self.keys.quant.name().to_string());
                let on_grid = if q == "now" { String::new() } else { format!(" on the {q}") };
                (
                    format!("keys:{deck}"),
                    said(format!("key {index} on {deck}{on_grid}")),
                    None,
                    false,
                    vec![key_op(deck, *index, *velocity, true, &q)],
                )
            }
            X::KeyUp { deck, index } => (
                format!("keys:{deck}:up"),
                said(format!("key {index} up on {deck}")),
                None,
                true,
                vec![key_op(deck, *index, 0, false, "now")],
            ),
            X::AutoMixTouch { .. } => return None,
            X::Section { deck, slot } => (
                format!("section:{deck}"),
                said(format!("section {} on {deck}", slot + 1)),
                None,
                false,
                d(DeckAction::Section {
                    deck: deck.clone(),
                    index: *slot,
                }),
            ),
            X::SoundStep { deck, delta } => {
                // after it was applied: the palette the deck now plays from
                let now = self.palette(deck);
                let label = crate::sound::PaletteSet::shared()
                    .playing(now.as_deref())
                    .map(|p| p.label.clone());
                (
                    format!("sound:{deck}"),
                    said(format!(
                        "sound {} on {deck} → {}",
                        signed(*delta),
                        label.unwrap_or_else(|| "?".into())
                    )),
                    Some(*delta),
                    false,
                    d(DeckAction::SetPalette {
                        deck: deck.clone(),
                        palette: now,
                    }),
                )
            }
            X::SectionStep { deck, delta } => {
                let now = self
                    .deck(deck)
                    .and_then(|x| crate::control::controller::DeckView::current_section(x));
                (
                    format!("section:{deck}"),
                    said(format!(
                        "section {} on {deck}{}",
                        signed(*delta),
                        now.map_or(String::new(), |k| format!(" → {}", k + 1))
                    )),
                    Some(*delta),
                    false,
                    now.map(|k| {
                        d(DeckAction::Section {
                            deck: deck.clone(),
                            index: k as i64,
                        })
                    })
                    .unwrap_or_default(),
                )
            }
            X::ToggleRole { deck, role } => {
                let on = self
                    .deck(deck)
                    .is_some_and(|x| crate::control::controller::DeckView::is_role_on(x, role));
                (
                    format!("role:{deck}:{role}"),
                    said(format!("{role} {} on {deck}", on_off(on))),
                    None,
                    false,
                    d(DeckAction::Role {
                        deck: deck.clone(),
                        role: role.clone(),
                        on,
                    }),
                )
            }
            X::ToggleStem { deck, role } => {
                let muted = self.deck(deck).is_some_and(|x| {
                    crate::control::controller::DeckView::is_stem_muted(x, role)
                });
                (
                    format!("stem:{deck}:{role}"),
                    said(format!("stem {role} {} on {deck}", if muted { "muted" } else { "back" })),
                    None,
                    false,
                    d(DeckAction::StemMute {
                        deck: deck.clone(),
                        stem: role.clone(),
                        on: muted,
                    }),
                )
            }
            X::Macro { deck, id } => (
                format!("macro:{deck}:{id}"),
                said(format!("{id} macro on {deck}")),
                None,
                false,
                d(DeckAction::Macro {
                    deck: deck.clone(),
                    id: id.clone(),
                }),
            ),
            X::Panic { deck } => (
                format!("panic:{deck}"),
                said(format!("panic on {deck}")),
                None,
                false,
                d(DeckAction::Panic { deck: deck.clone() }),
            ),
            X::Stinger { deck, slot } => (
                format!("stinger:{deck}"),
                said(format!("hit {} on {deck}", slot + 1)),
                None,
                false,
                d(DeckAction::Stinger {
                    deck: deck.clone(),
                    n: *slot,
                }),
            ),
            X::NextTrack { deck } => (
                format!("next:{deck}"),
                said(format!("next track on {deck}")),
                None,
                false,
                d(DeckAction::NextTrack { deck: deck.clone() }),
            ),
            X::Sync { deck, bar } => {
                let align = if *bar { "bar" } else { "beat" };
                (
                    format!("sync:{deck}"),
                    said(format!("sync {deck} ({align})")),
                    None,
                    false,
                    d(DeckAction::Sync {
                        deck: deck.clone(),
                        align: align.into(),
                    }),
                )
            }
            X::QuantizeCycle { deck } => {
                let mode = match self.deck(deck).map(|x| x.quantize) {
                    Some(Quantize::Beat) => "beat",
                    Some(Quantize::Bar) => "bar",
                    _ => "off",
                };
                (
                    format!("quantize:{deck}"),
                    said(format!("quantise {deck} {mode}")),
                    None,
                    false,
                    d(DeckAction::Quantize {
                        deck: deck.clone(),
                        mode: mode.into(),
                    }),
                )
            }
            X::Rewind { deck } => (
                format!("rewind:{deck}"),
                said(format!("rewind {deck}")),
                None,
                false,
                d(DeckAction::Rewind { deck: deck.clone() }),
            ),
            X::PflToggle { deck } => (
                format!("pfl:{deck}"),
                said(format!("cue (PFL) {deck}")),
                None,
                false,
                self.deck(deck)
                    .map(|x| {
                        vec![ArrangeOp::Live {
                            method: "cue".into(),
                            params: JsonValue::Object(crate::json::JsonObject::from_pairs([
                                ("deck", JsonValue::str(deck)),
                                ("pfl", x.pfl.into()),
                            ])),
                        }]
                    })
                    .unwrap_or_default(),
            ),
            X::PlayToggle { deck } => {
                let on = playing(deck);
                (
                    format!("play:{deck}"),
                    said(format!("{} {deck}", if on { "play" } else { "pause" })),
                    None,
                    false,
                    d(DeckAction::Play {
                        deck: deck.clone(),
                        on,
                    }),
                )
            }
            X::Cue { deck } => (
                format!("cue:{deck}"),
                said(format!("cue {deck}")),
                None,
                false,
                d(DeckAction::Cue { deck: deck.clone() }),
            ),
            X::HotCue { deck, slot } => (
                format!("hotcue:{deck}:{slot}"),
                said(format!("hot cue {} on {deck}", slot + 1)),
                None,
                false,
                d(DeckAction::HotCue {
                    deck: deck.clone(),
                    slot: *slot,
                }),
            ),
            X::ClearCue { deck, slot } => (
                format!("clearcue:{deck}:{slot}"),
                said(format!("clear hot cue {} on {deck}", slot + 1)),
                None,
                false,
                d(DeckAction::ClearCue {
                    deck: deck.clone(),
                    slot: *slot,
                }),
            ),
            X::LoopIn { deck } | X::LoopOut { deck } | X::LoopHalve { deck } | X::LoopDouble { deck } => {
                let op = match a {
                    X::LoopIn { .. } => "in",
                    X::LoopOut { .. } => "out",
                    X::LoopHalve { .. } => "halve",
                    _ => "double",
                };
                (
                    format!("loop:{deck}:{op}"),
                    said(format!("loop {op} on {deck}")),
                    None,
                    false,
                    d(DeckAction::Loop {
                        deck: deck.clone(),
                        op: op.into(),
                    }),
                )
            }
            // an input's capture, history and library moves are noted by the engine calls they
            // make (the resolved strip, the beats it took, whether it grabbed)
            X::CaptureGrab { input, .. } | X::CaptureArm { input, .. } if is_input(input.as_ref()) => {
                return None
            }
            X::CaptureBeats { input, .. }
            | X::HistoryScrub { input, .. }
            | X::HistoryLive { input }
            | X::HistoryLoop { input, .. }
                if self.recall_target(input.as_deref()).is_some() =>
            {
                return None
            }
            X::LibraryLoad { .. } | X::LibrarySave { .. } => return None,
            X::CaptureGrab { input, beats } => (
                "capture".into(),
                said(format!(
                    "grab {} of {}",
                    beats.map_or("the capture".into(), |b| format!("{b} beats")),
                    input.as_deref().unwrap_or("the input")
                )),
                None,
                false,
                vec![],
            ),
            X::CaptureArm { input, on } => (
                "capturearm".into(),
                said(format!(
                    "capture {} {}",
                    input.as_deref().unwrap_or("input"),
                    if *on { "armed" } else { "released" }
                )),
                None,
                false,
                vec![],
            ),
            X::CaptureBeats { delta, .. } => (
                "capturebeats".into(),
                said(format!("capture length {}", signed(*delta))),
                Some(*delta),
                false,
                vec![],
            ),
            X::HistoryScrub { beats, .. } => (
                "scrub".into(),
                said(format!("history scrub {beats:+.2} beats")),
                None,
                true,
                vec![],
            ),
            X::HistoryLive { .. } => ("live".into(), said("history back to live".into()), None, false, vec![]),
            X::HistoryLoop { beats, .. } => (
                "histloop".into(),
                said(format!(
                    "history loop {}",
                    beats.map_or("in place".into(), |b| format!("{b} beats"))
                )),
                None,
                false,
                vec![],
            ),
            X::Target {
                id,
                deck,
                slot,
                index,
                value,
                args,
                pressed,
            } => {
                // the vocoder's and the freeze's moves are logged by the engine calls they make, in their words
                if !*pressed || id.starts_with("vocoder") || id.starts_with("freeze") || id.starts_with("trick") || id == "historySnap" || id == "historyJog" || id == "autoNextBars" {
                    return None;
                }
                let mut what = id.clone();
                if let Some(dk) = deck {
                    what.push_str(&format!(" on {dk}"));
                }
                if !args.is_empty() {
                    let a: Vec<String> = args.iter().map(|(k, v)| format!("{k}={v}")).collect();
                    what.push_str(&format!(" ({})", a.join(", ")));
                }
                (
                    format!("target:{id}"),
                    said(what),
                    None,
                    false,
                    if self.surface_traced > 0 {
                        // the call it made traced its own ops: a replay of those is the move
                        Vec::new()
                    } else {
                        vec![ArrangeOp::Live {
                        method: "target".into(),
                        params: JsonValue::Object(crate::json::JsonObject::from_pairs([
                            ("id", JsonValue::str(id)),
                            ("deck", deck.as_deref().map_or(JsonValue::Null, JsonValue::str)),
                            ("slot", slot.map_or(JsonValue::Null, JsonValue::int)),
                            ("index", JsonValue::int(*index as i64)),
                            ("value", JsonValue::Number(*value)),
                            ("pressed", (*pressed).into()),
                            (
                                "args",
                                JsonValue::Object(crate::json::JsonObject::from_pairs(
                                    args.iter().map(|(k, v)| (k.as_str(), JsonValue::str(v))),
                                )),
                            ),
                        ])),
                    }]
                    },
                )
            }
        })
    }
}
