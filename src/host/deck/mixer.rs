//! The mixer panel's engine side (docs/HUB.md, milestone M1): SYNC (`Deck.sync(to:align:)`),
//! the tempo fader's range, the master transport (tempo source, tap, START / STOP) and the whole
//! mixer read and written as one [`MixerState`].

use crate::deck::beat::{SyncAlign, SyncResult};
use crate::fx::StripKnob;
use crate::mixer::{
    clamp_bpm, master_deck, DeckTempo, LoopStripState, MixerState, StripState, TempoSource,
    PITCH_RANGES,
};

use super::engine::Engine;

impl Engine {
    // MARK: tempo fader

    /// The tempo fader's travel for `deck` (one of [`PITCH_RANGES`]; others snap to the nearest).
    /// The tempo stays where it is: the fader's position is rescaled to the new travel (clamped
    /// to its ends when the new one is narrower). The fader itself is `Engine::set_pitch`, -1...1
    /// of this travel.
    pub fn set_pitch_range(&mut self, deck: &str, range: f64) -> bool {
        let mut best = PITCH_RANGES[0];
        for r in PITCH_RANGES {
            if (r - range).abs() < (best - range).abs() {
                best = r;
            }
        }
        let Some(d) = self.deck_mut(deck) else {
            return false;
        };
        let (old, pitch) = (d.pitch_range, d.pitch);
        d.pitch_range = best;
        let name = d.name.clone();
        let p = (pitch * old / best).clamp(-1.0, 1.0);
        self.set_pitch(deck, p);
        self.note_live(
            format!("pitch_range:{name}"),
            format!("{name} tempo range ±{:.0}%", best * 100.0),
            false,
            "pitch_range",
            super::vocoder::live_params([("deck", name.into()), ("range", crate::json::JsonValue::Number(best))]),
        );
        true
    }

    // MARK: sync

    /// The deck `deck` syncs to: the master-tempo deck when that is another deck, else the first
    /// other deck that has a clock.
    pub fn sync_target(&self, deck: &str) -> Option<String> {
        if let (TempoSource::Deck(m), _) = self.master_tempo() {
            if m != deck && self.beat_state(&m).is_some() {
                return Some(m);
            }
        }
        self.decks
            .iter()
            .map(|d| d.name.clone())
            .find(|n| n != deck && self.beat_state(n).is_some())
    }

    /// The mixer's SYNC: `Engine::sync` (`Deck.sync(to:align:)`) onto [`Engine::sync_target`],
    /// the master-tempo deck, rather than the controller SYNC's partner by name. None when
    /// either deck has no clock.
    pub fn sync_to_master(&mut self, deck: &str, align: SyncAlign) -> Option<SyncResult> {
        let target = self.sync_target(deck)?;
        self.sync(deck, &target, align)
    }

    // MARK: transport

    /// Every deck as the master-tempo rule sees it.
    pub fn deck_tempos(&self) -> Vec<DeckTempo> {
        self.decks
            .iter()
            .map(|d| DeckTempo {
                deck: d.name.clone(),
                playing: d.mirror.is_playing(),
                bpm: self.beat_state(&d.name).map(|b| b.bpm),
            })
            .collect()
    }

    /// The master tempo and where it comes from ([`master_deck`]).
    pub fn master_tempo(&self) -> (TempoSource, f64) {
        master_deck(&self.deck_tempos(), self.crossfader, &self.transport)
    }

    /// Lock the master tempo to a deck (None: follow the crossfader).
    pub fn set_tempo_lock(&mut self, deck: Option<&str>) {
        self.transport.lock = deck.filter(|d| self.index(d).is_some()).map(String::from);
        let lock = self.transport.lock.clone().unwrap_or_default();
        self.note_live(
            "transport:lock".into(),
            if lock.is_empty() {
                "tempo follows the crossfader".into()
            } else {
                format!("tempo locked to {lock}")
            },
            false,
            "transport",
            if lock.is_empty() {
                super::vocoder::live_params([("action", "unlock".into())])
            } else {
                super::vocoder::live_params([("action", "lock".into()), ("deck", lock.into())])
            },
        );
    }

    /// The tempo when no deck plays.
    pub fn set_free_bpm(&mut self, bpm: f64) {
        if bpm.is_finite() {
            self.transport.free_bpm = clamp_bpm(bpm);
            let b = self.transport.free_bpm;
            self.note_live(
                "transport:free".into(),
                format!("free tempo {b:.1}"),
                true,
                "transport",
                super::vocoder::live_params([
                    ("action", "free_bpm".into()),
                    ("bpm", crate::json::JsonValue::Number(b)),
                ]),
            );
        }
    }

    /// TAP at `now` (seconds, any monotonic clock). Once two taps give a tempo it becomes the
    /// free tempo. Returns that tempo.
    pub fn tap_tempo(&mut self, now: f64) -> Option<f64> {
        let bpm = self.tap.tap(now)?;
        self.set_free_bpm(bpm);
        Some(bpm)
    }

    /// START: every deck with something on it plays. The commands are queued back to back, so
    /// the render thread normally takes them in one block and the decks start together.
    /// Quantising the start to a bar comes with the MIDI clock (M2).
    pub fn transport_start(&mut self) {
        let names: Vec<String> = self
            .decks
            .iter()
            .filter(|d| d.project.is_some() || d.sample.is_some())
            .map(|d| d.name.clone())
            .collect();
        for n in &names {
            self.play(n);
        }
        self.transport.running = true;
        self.note_live("transport".into(), "START".into(), false, "transport",
            super::vocoder::live_params([("action", "start".into())]));
    }

    /// Set the running flag alone (the auto transport, `crate::mixer::AutoTransport`): the
    /// decks are left as they are, unlike START / STOP. The clock follows the flag.
    pub fn set_transport_running(&mut self, running: bool) {
        self.transport.running = running;
        self.note_live("transport".into(), format!("transport {}", if running { "running" } else { "halted" }), false, "transport",
            super::vocoder::live_params([("action", "running".into()), ("on", running.into())]));
    }

    /// STOP: every deck pauses where it is.
    pub fn transport_stop(&mut self) {
        let names: Vec<String> = self.decks.iter().map(|d| d.name.clone()).collect();
        for n in &names {
            self.pause(n);
        }
        self.transport.running = false;
        self.note_live("transport".into(), "STOP".into(), false, "transport",
            super::vocoder::live_params([("action", "stop".into())]));
    }

    // MARK: the mixer as one document

    /// Every mixer control's position now.
    pub fn mixer_state(&self) -> MixerState {
        MixerState {
            strips: self
                .decks
                .iter()
                .map(|d| StripState {
                    deck: d.name.clone(),
                    volume: d.volume,
                    gain: d.gain,
                    eq: d.strip,
                    mute: d.mute,
                    pfl: d.pfl,
                    pitch_range: d.pitch_range,
                    aux: d.aux,
                })
                .collect(),
            loop_channel: LoopStripState {
                volume: self.loops.volume,
                eq: self.loop_channel.strip,
                mute: self.loops.mute,
                aux: self.loop_channel.aux,
            },
            crossfader: self.crossfader,
            master: self.master,
            headphone_mix: self.headphone_mix,
            headphone_level: self.headphone_level,
            transport: self.transport.clone(),
            aux: self.aux,
        }
    }

    /// Put every control where `m` has it. Strips for decks this engine does not have are
    /// ignored. The transport's lock and free tempo are taken; START / STOP are not (restoring
    /// a mixer does not start the music).
    pub fn apply_mixer_state(&mut self, m: &MixerState) {
        // a restore, not a hand: none of it is noted as a live op
        let outer = std::mem::replace(&mut self.in_surface, true);
        self.apply_mixer_state_quiet(m);
        self.in_surface = outer;
    }

    fn apply_mixer_state_quiet(&mut self, m: &MixerState) {
        for s in &m.strips {
            if self.index(&s.deck).is_none() {
                continue;
            }
            self.set_volume(&s.deck, s.volume);
            self.set_gain(&s.deck, s.gain);
            self.set_strip(&s.deck, StripKnob::Filter, s.eq.filter);
            self.set_strip(&s.deck, StripKnob::EqLow, s.eq.eq_low);
            self.set_strip(&s.deck, StripKnob::EqMid, s.eq.eq_mid);
            self.set_strip(&s.deck, StripKnob::EqHigh, s.eq.eq_high);
            self.set_deck_mute(&s.deck, s.mute);
            self.set_pfl(&s.deck, s.pfl);
            self.set_pitch_range(&s.deck, s.pitch_range);
            self.set_aux_send(&s.deck, s.aux);
        }
        let l = &m.loop_channel;
        self.set_loop_volume(l.volume);
        self.set_loop_mute(l.mute);
        self.set_loop_strip(StripKnob::Filter, l.eq.filter);
        self.set_loop_strip(StripKnob::EqLow, l.eq.eq_low);
        self.set_loop_strip(StripKnob::EqMid, l.eq.eq_mid);
        self.set_loop_strip(StripKnob::EqHigh, l.eq.eq_high);
        self.set_aux_send("L", l.aux);
        self.set_aux_return(m.aux.level);
        self.set_aux_mute(m.aux.mute);
        for k in crate::fx::AuxFxKey::ALL {
            self.set_aux_fx(k, m.aux.get(k));
        }
        self.set_crossfader(m.crossfader);
        self.set_master(m.master);
        self.set_headphone_mix(m.headphone_mix);
        self.set_headphone_level(m.headphone_level);
        let lock = m.transport.lock.clone();
        self.set_tempo_lock(lock.as_deref());
        self.set_free_bpm(m.transport.free_bpm);
    }
}
