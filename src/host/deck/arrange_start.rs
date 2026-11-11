//! The engine as an arrangement's `start` (`crate::session::arrange_start`, docs/HEADLESS.md):
//! what djapp's action log snapshots when it starts or is cleared, and what an offline render
//! stands its engine on before beat 0.

use std::collections::BTreeMap;

use crate::session::arrange_start::{ArrangeStart, StartInput};
use crate::session::model::DeckSource;

use super::engine::Engine;

impl Engine {
    /// What each deck holds (a composed deck by its spec, a sample deck by its audio's content
    /// id) and the rest of the engine as an arrangement's `start`: the crossfader and master,
    /// every deck's knobs and the loop channel's, the decks playing, the input strips, the
    /// capture library, the vocoder's setup. `start.samples` is the host's to fill (it knows
    /// the files).
    pub fn arrange_start(&self) -> (BTreeMap<String, DeckSource>, ArrangeStart) {
        let mut decks = BTreeMap::new();
        let mut start = ArrangeStart {
            crossfader: Some(self.crossfader),
            master: Some(self.master),
            playing: Some(Vec::new()),
            ..Default::default()
        };
        for d in &self.decks {
            let src = self
                .net_source(&d.name)
                .or_else(|| self.source_descriptor(&d.name).ok())
                .filter(|s| *s != DeckSource::None);
            if let Some(s) = src {
                decks.insert(d.name.clone(), s);
            }
            if let Some(p) = self.current_params(&d.name) {
                start.params.insert(d.name.clone(), p);
            }
            if self.deck(&d.name).is_some_and(|x| x.playing) {
                if let Some(pl) = start.playing.as_mut() {
                    pl.push(d.name.clone());
                }
            }
        }
        if let Some(p) = self.current_params("L") {
            start.loop_gain = Some(p.gain);
            start.params.insert("L".into(), p);
        }
        let now = self.frame();
        start.inputs = self
            .inputs
            .iter()
            .enumerate()
            .map(|(k, i)| StartInput {
                label: i.settings.label.clone(),
                history_beats: i.history_beats,
                // the audio it has heard since the recorder began, and where beat 0 falls in it
                file: i.record_file.clone(),
                file_start: self
                    .input_record_frame(k)
                    .filter(|_| i.record_file.is_some())
                    .map_or(0, |f| (now - f).max(0) as u64),
            })
            .collect();
        if let Some(lib) = self.capture_library.as_ref() {
            start.loops_dir = Some(lib.directory.display().to_string());
            start.loop_row = true;
        }
        start.setup = self.input_setup_ops();
        start.setup.extend(self.aux_setup_ops());
        start.setup.extend(self.post_setup_ops());
        start.setup.extend(self.vocoder_setup_ops());
        (decks, start)
    }
}
