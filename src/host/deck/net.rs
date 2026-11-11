//! Playing a net deck (`DeckPerform.swift`, `DeckVoices.swift`, `DeckKits.swift`): the live
//! keys, the sound panel's voice settings, and the kit poke (which, like the palette and a
//! role's voice, rebuilds the song from the next bar through [`Engine::retarget`]). The roles,
//! STEMS, Hits, palette and role voice are [`moves`](super::moves)'s: the pokes Wander's moves
//! are made of are the ones a `DeckAction` reaches. The tick arithmetic and the decisions are
//! `crate::deck::{perform, stinger, kits}`; this is what they do to a deck.

use std::collections::BTreeMap;

use crate::deck::perform::scale_locked;
use crate::session::model::DeckSource;
use crate::sound::{playing_palette, role_sounds, PaletteSet, RoleSoundRow, SoundPalette};
use crate::wave::{VoiceLaneParams, VoiceSetting, VoiceTweak};

use super::core::Command;
use super::engine::{Engine, MainDeck};
use super::track::TrackSource;

impl MainDeck {
    /// `Deck.netSource`: the net the deck is playing, as a descriptor a favorite (or a session)
    /// can reload.
    pub fn net_source(&self) -> Option<DeckSource> {
        if !self.is_net() {
            return None;
        }
        if let Some(spec) = &self.spec {
            return Some(DeckSource::Net(spec.clone()));
        }
        self.track
            .raw_nets
            .as_ref()
            .map(|p| DeckSource::RawNets(p.clone()))
    }
}

impl Engine {
    // MARK: keys

    /// `Deck.scaleLocked`: snap a MIDI note to the project's scale (nearest scale tone, ties go
    /// down).
    pub fn scale_locked(&self, deck: &str, note: i64) -> i64 {
        match self.deck(deck).and_then(|d| d.project.as_ref()) {
            Some(p) => scale_locked(note, p.root_note, &p.scale_name),
            None => note,
        }
    }

    /// `Deck.keyDown`: a key down on the live voice. Returns the note that actually sounds.
    pub fn key_down(&mut self, deck: &str, note: i64, velocity: i64) -> i64 {
        if !self.is_net_deck(deck) {
            return note;
        }
        let q = if self.keys.scale_lock {
            self.scale_locked(deck, note)
        } else {
            note
        };
        if let Some(d) = self.deck_mut(deck) {
            d.held_keys.insert(note, q);
        }
        self.command(deck, Command::note_on(q, velocity));
        q
    }

    /// `Deck.keyUp`.
    pub fn key_up(&mut self, deck: &str, note: i64) {
        if !self.is_net_deck(deck) {
            return;
        }
        let held = self.deck_mut(deck).and_then(|d| d.held_keys.remove(&note));
        let q = held.unwrap_or_else(|| {
            if self.keys.scale_lock {
                self.scale_locked(deck, note)
            } else {
                note
            }
        });
        self.command(deck, Command::note_off(q));
    }

    // MARK: the sound panel's voice settings

    /// `Deck.voiceSetting`.
    pub fn voice_setting(&self, deck: &str, channel: i64) -> Option<&VoiceSetting> {
        self.deck(deck)?.voice_settings.get(&channel)
    }

    /// `Deck.voiceTweak`: the channel's tweak this session (default: the instrument's own).
    pub fn voice_tweak(&self, deck: &str, channel: i64) -> VoiceTweak {
        self.voice_setting(deck, channel)
            .map(|s| s.tweak)
            .unwrap_or_default()
    }

    /// `Deck.usesSynthVoice`.
    pub fn uses_synth_voice(&self, deck: &str, channel: i64) -> bool {
        self.voice_setting(deck, channel)
            .is_some_and(|s| s.use_synth)
    }

    /// `Deck.roleVoices`: the deck's roles with an instrument to choose, in the deck's role
    /// order. Empty for a sample deck, and for roles beats has no instrument pool for.
    pub fn role_voices(&self, deck: &str) -> Vec<crate::deck::voices::RoleVoice> {
        let Some(d) = self.deck(deck).filter(|d| d.is_net()) else {
            return Vec::new();
        };
        let Some(project) = d.project.as_ref() else {
            return Vec::new();
        };
        let genre = d.spec.as_ref().map_or("", |s| s.genre.as_str());
        self.with_sounds(|ctx| {
            crate::deck::voices::RoleVoice::list(project, &self.roles(deck), genre, ctx.catalog)
        })
    }

    /// `Deck.channelVoice`: what channel `channel` plays now (the latest project loaded).
    pub fn channel_voice(&self, deck: &str, channel: i64) -> Option<&crate::wave::ChannelVoice> {
        let d = self.deck(deck).filter(|d| d.is_net())?;
        d.track.latest.as_ref()?.channel_voices.get(&channel)
    }

    /// `Deck.setVoiceTweak` (`DeckAction.voiceTweak`): shape channel `channel`'s voice live, on
    /// the song playing, from the next block: no reload. An empty tweak is the instrument's own
    /// voice (Reset).
    ///
    /// Swift's `Deck` discards `WaveDeckSource.setVoice`'s result; it is kept here: false off a
    /// net, and when the latest project has no net on that channel.
    pub fn set_voice_tweak(&mut self, deck: &str, tweak: &VoiceTweak, channel: i64) -> bool {
        if !self.is_net_deck(deck) {
            return false;
        }
        let synth = self.uses_synth_voice(deck, channel);
        self.set_voice_setting(deck, channel, tweak, synth)
    }

    /// `Deck.setSynthVoice` (`DeckAction.voiceSynth`): a sampled channel plays its synth voice
    /// instead of the sample (`on`), or the sample again. Live, like a tweak.
    pub fn set_synth_voice(&mut self, deck: &str, on: bool, channel: i64) -> bool {
        if !self.is_net_deck(deck) {
            return false;
        }
        let tweak = self.voice_tweak(deck, channel);
        self.set_voice_setting(deck, channel, &tweak, on)
    }

    /// `WaveDeckSource.setVoice(channel:tweak:useSynth:)`: takes effect at the next block on the
    /// project playing and any queued, and on every project loaded after (the hand-over). A
    /// default setting (empty tweak, sample) is the instrument exactly. False when the latest
    /// project has no net on that channel.
    pub fn set_voice_setting(
        &mut self,
        deck: &str,
        channel: i64,
        tweak: &VoiceTweak,
        use_synth: bool,
    ) -> bool {
        let sr = self.sample_rate;
        let Some(d) = self.deck_mut(deck) else {
            return false;
        };
        let Some(v) = d
            .track
            .latest
            .as_ref()
            .and_then(|l| l.channel_voices.get(&channel))
            .cloned()
        else {
            return false;
        };
        let setting = VoiceSetting {
            tweak: *tweak,
            use_synth,
            instrument: v.instrument.clone(),
        };
        if setting.is_default() {
            d.voice_settings.remove(&channel);
        } else {
            d.voice_settings.insert(channel, setting);
        }
        let params = VoiceLaneParams::new(channel, tweak, use_synth, Some(&v.voice), sr);
        self.command(deck, Command::Voice(params))
    }

    /// `Deck.keepsVoiceSettings(loading:)`: does loading `source` keep this deck's voice
    /// settings? Only when it is the song playing with another instrument picked (the same
    /// share but for its `tracks` override): the settings belong to the song, and to the
    /// instrument they were made on (a channel whose instrument changed starts afresh, at the
    /// hand-over). A new song, a favorite or the same song loaded again start from the
    /// instruments' own voices.
    pub(crate) fn keeps_voice_settings(&self, deck: &str, source: &TrackSource) -> bool {
        let loading = match source {
            TrackSource::Net(s) => DeckSource::Net(s.clone()),
            TrackSource::RawNets(p) => DeckSource::RawNets(p.clone()),
        };
        let now = self.deck(deck).and_then(MainDeck::net_source);
        crate::deck::voices::keeps_voice_settings(now.as_ref(), &loading)
    }

    // MARK: kit, palette, role voice

    /// `Deck.kit`: the kit the deck's drums play, None for beats' synth kit or a sample deck.
    pub fn kit(&self, deck: &str) -> Option<String> {
        self.deck(deck)
            .filter(|d| d.is_net())?
            .project
            .as_ref()?
            .drum_kit()
    }

    /// `Deck.palette`: the palette the deck's track plays from, as recorded, or None.
    pub fn palette(&self, deck: &str) -> Option<String> {
        self.deck(deck)
            .filter(|d| d.is_net())?
            .project
            .as_ref()?
            .palette()
    }

    /// `Deck.playingPalette`: the palette the deck's track plays (its own, else the default), None
    /// for a sample deck.
    pub fn playing_palette(&self, deck: &str) -> Option<&'static SoundPalette> {
        let project = self.deck(deck).filter(|d| d.is_net())?.project.as_ref()?;
        playing_palette(project, PaletteSet::shared())
    }

    /// `Deck.roleSounds`: every palette role the deck's track plays, for the fine-tune area: what
    /// each plays, whether the user overrode it, and its options with why one can't play here
    /// (against what is installed: [`Engine::with_sounds`]). Empty for a sample deck. The labels
    /// of the `cat:` options and the voicing of each pick use the deck's voicing seed.
    pub fn role_sounds(&self, deck: &str) -> Vec<RoleSoundRow> {
        let Some(d) = self.deck(deck).filter(|d| d.is_net()) else {
            return Vec::new();
        };
        let Some(project) = d.project.as_ref() else {
            return Vec::new();
        };
        self.with_sounds(|ctx| role_sounds(project, d.seed, PaletteSet::shared(), ctx))
    }

    /// `Deck.setKit` (`DeckAction.setKit`): play the drums on `kit` (None = the synth kit): the
    /// song keeps its place and swaps at the next bar, its source gains the `tracks` override
    /// so a favorite or share keeps the kit. A kit not installed here plays the synth kit.
    pub fn set_kit(&mut self, deck: &str, kit: Option<&str>) {
        let Some(mut project) = self
            .deck(deck)
            .filter(|d| d.is_net())
            .and_then(|d| d.project.clone())
        else {
            return;
        };
        let kit = kit.filter(|k| !k.is_empty());
        if project.drum_kit().as_deref() == kit {
            return;
        }
        let channels = project.drum_channels();
        if channels.is_empty() {
            return;
        }
        project.set_drum_kit(kit);
        self.retarget(deck, project, |s| s.with_kit(kit, &channels));
    }
}

/// The voice settings a hand-over re-applies to the project it is about to send
/// (`WaveDeckSource.load`): a channel whose instrument changed starts from that instrument's
/// own voice (its setting is dropped), one the project has no lane on keeps its setting.
pub(crate) fn reapply_voice_settings(
    settings: &mut BTreeMap<i64, VoiceSetting>,
    prepared: &mut super::engine::Prepared,
    sample_rate: f64,
) {
    let chans: Vec<i64> = settings.keys().copied().collect();
    for ch in chans {
        let Some(v) = prepared.latest.channel_voices.get(&ch) else {
            continue;
        };
        let setting = &settings[&ch];
        if v.instrument != setting.instrument {
            settings.remove(&ch);
            continue;
        }
        let p = VoiceLaneParams::new(
            ch,
            &setting.tweak,
            setting.use_synth,
            Some(&v.voice),
            sample_rate,
        );
        prepared.project.apply_voice(&p);
    }
}
