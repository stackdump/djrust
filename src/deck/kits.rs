//! The `Project` half of a net deck's sound pokes (`DeckKits.swift`), as methods, by the names
//! Swift gives them: the drum channels and the kit the drums play, the channels that play, the
//! palette the track plays from, and the Hits (`curateStingers`). The pokes themselves
//! (`Deck.setKit`, `setPalette`, `setRoleVoice`) are the host's: they change these, then rebuild
//! the song from the next bar (`retarget`).
//!
//! Each body is the free function [`crate::wander::deck`] / [`crate::wander::carry`] keeps for
//! the walk (which edits the same fields from a plan): one implementation, two ways to call it.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::pflow::Project;
use crate::sound::is_drum_channel;
use crate::sound::stinger::{StingerAssignment, StingerContent};
use crate::sound::{PaletteSet, Sounds};
use crate::wander::carry::{project_channels, project_drum_channels};
use crate::wander::deck::{
    curate_project_stingers, project_kit, project_palette, project_role_channels,
    resolved_project_stingers, set_project_palette,
};

impl Project {
    /// `Project.drumChannels`: the channels of the project's drum nets (kick, snare, hats,
    /// clap), in order.
    pub fn drum_channels(&self) -> Vec<i64> {
        project_drum_channels(self)
    }

    /// `Project.drumKit`: the kit the drums play (`Track.kit` of the first drum net), None = the
    /// synth kit.
    pub fn drum_kit(&self) -> Option<String> {
        project_kit(self)
    }

    /// `Project.setDrumKit`: every drum net playing `kit` (what the `tracks` override does when
    /// the share loads). Unlike the readers above this one does not skip control nets, as
    /// Swift's does not.
    pub fn set_drum_kit(&mut self, kit: Option<&str>) {
        let ids: Vec<String> = self.nets.keys().to_vec();
        for id in ids {
            if let Some(nb) = self.nets.get_mut(&id) {
                if is_drum_channel(nb.track.channel) {
                    nb.track.kit = kit.map(ToString::to_string);
                }
            }
        }
    }

    /// `Project.channels`: the channels of every net that plays (not control nets), in order.
    pub fn channels(&self) -> Vec<i64> {
        project_channels(self)
    }

    /// `Project.palette`: the palette the track plays from (the first playing net's), None =
    /// none.
    pub fn palette(&self) -> Option<String> {
        project_palette(self)
    }

    /// `Project.setPalette`: every net on `palette` (control nets too, as Swift writes it).
    pub fn set_palette(&mut self, palette: Option<&str>) {
        set_project_palette(self, palette)
    }

    /// `Project.channels(soundRole:)`: the channels whose nets play as palette role `role`,
    /// sorted.
    pub fn channels_of_sound_role(&self, role: &str) -> Vec<i64> {
        project_role_channels(self, role)
    }

    /// `Project.curateStingers`: play `assignments` on this project's Hits: they are kept
    /// (`stingers`), each one-shot's option goes on its stinger net (`TrackSound.curated`; the
    /// user's role override still wins), and the sounds are resolved again. The composition is
    /// untouched.
    pub fn curate_stingers(
        &mut self,
        assignments: &[StingerAssignment],
        seed: f64,
        set: &PaletteSet,
        ctx: Sounds,
    ) {
        curate_project_stingers(self, assignments.to_vec(), seed, set, ctx)
    }

    /// `Project.resolvedStingers`: this project's Hits resolved for `genre` from its palette, key
    /// and tempo.
    pub fn resolved_stingers(
        &self,
        genre: &str,
        seed: f64,
        installed: &StingerContent,
        set: &PaletteSet,
    ) -> Vec<StingerAssignment> {
        resolved_project_stingers(self, set, genre, seed, installed)
    }
}
