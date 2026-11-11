//! `WaveOptions.forPalette` (`Audio/PaletteSound.swift`): the room and the echo a sound
//! palette plays in, on top of the genre's (`WaveOptions::for_genre`).

use super::runner::WaveOptions;
use crate::musicality::echo::EchoSpec;
use crate::musicality::fixes::Fixes;
use crate::musicality::space::SpaceSpec;
use crate::sound::PaletteSet;

impl WaveOptions {
    /// `WaveOptions.forPalette`: these options with the room and echo the palette plays in
    /// (`SoundPalette.room` / `.echo`, another genre's), else `genre`'s, for the fixes they
    /// hold. The room keeps its pad shimmer under `.shimmer`, as [`for_genre`](Self::for_genre)
    /// sets it (the room's genre's family level). `palette` is as a track records it (with its
    /// asset version, `minecraft@32`, or none).
    pub fn for_palette(&self, palette: Option<&str>, genre: &str) -> WaveOptions {
        let mut o = self.clone();
        let p = PaletteSet::shared().named(palette);
        let fixes = self.musicality;
        if fixes.contains(Fixes::SPACE) {
            let room = p.and_then(|p| p.room.as_deref()).unwrap_or(genre);
            o.space = Some(SpaceSpec::for_genre(room, fixes.contains(Fixes::SHIMMER)));
        }
        if fixes.contains(Fixes::ECHO) {
            let e = p.and_then(|p| p.echo.as_deref()).unwrap_or(genre);
            o.echo = if e == "none" {
                None
            } else {
                EchoSpec::for_genre(e)
            };
        }
        o
    }
}
