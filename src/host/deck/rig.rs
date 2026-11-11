//! The engine with its controllers: raw MIDI in, deck calls out (`ControllerHub.route` over a
//! real `DJEngine`, instead of `controllers::DryHost`'s recording stand-in). Hardware-free: a
//! recorded byte stream replays exactly like live input, and `djctl play` feeds it from
//! `crate::host::midi`.
//!
//! The rig also holds what an app keeps while it runs ([`Rig::journal`], [`Rig::takes`]), so
//! the one call every host already makes ([`Rig::tick`]) keeps them up: `AppModel.poll`'s
//! `pollLoops` (the row saved when it changed) and `pollJournal` (a checkpoint when one is due).

use std::io;
use std::path::{Path, PathBuf};

use crate::control::router::{ControllerRouter, RouteResult};

use super::engine::Engine;
use crate::host::controllers::normalize_midi;
use crate::host::journal::Journal;
use crate::host::take_library::TakeLibrary;

pub struct Rig {
    pub engine: Engine,
    pub router: ControllerRouter,
    /// `(port, bytes)` of every routed message, as the session recorder would see them.
    pub recorded: Vec<(String, Vec<u8>)>,
    /// The always-on journal of this run (`AppModel.journal`), once [`Rig::open_journal`] has
    /// opened it. The engine records into it through its own sink (what [`Rig::route`]'s
    /// controllers, `perform` and the grids say); the rig keeps the handle to checkpoint it from
    /// [`Rig::tick`] and to close it.
    pub journal: Option<Journal>,
    /// The loop row's autosave and restore (`AppModel.takeStore`), polled from [`Rig::tick`].
    pub takes: Option<TakeLibrary>,
    /// The inputs' audio written to files (opt-in; [`crate::host::input_record`]), finished by
    /// [`Rig::close_persistence`].
    pub input_recorder: Option<crate::host::input_record::InputRecorder>,
}

impl Rig {
    pub fn new(engine: Engine) -> Rig {
        let names = crate::control::controller::SurfaceHost::deck_names(&engine);
        let router = ControllerRouter::with_defaults(&names);
        Rig {
            engine,
            router,
            recorded: Vec::new(),
            journal: None,
            takes: None,
            input_recorder: None,
        }
    }

    /// `AppModel.openJournal`: start this run's journal in `dir` (its line 0 is the engine's
    /// checkpoint now, then an `app.start <version>` mark). Open it once the engine stands where
    /// the run starts: the decks loaded, the row restored. Returns the file. A journal already
    /// open is closed first (`app.stop`), so after an error the rig has none.
    pub fn open_journal(&mut self, dir: &Path, version: &str) -> io::Result<PathBuf> {
        self.close_journal();
        let j = Journal::open_run(&mut self.engine, dir, version)?;
        let path = j.path();
        self.journal = Some(j);
        Ok(path)
    }

    /// `AppModel.closeJournal`: an `app.stop` mark, and the file is synced and closed. A no-op
    /// without a journal.
    pub fn close_journal(&mut self) {
        if let Some(j) = self.journal.take() {
            j.close_run(&mut self.engine);
        }
    }

    /// The end of a run (`AppModel.stop`): the row's last changes are saved and written, then
    /// the journal is closed.
    pub fn close_persistence(&mut self) {
        if let Some(t) = self.takes.as_mut() {
            t.poll(&self.engine);
            t.flush();
        }
        if let Some(mut rec) = self.input_recorder.take() {
            rec.finish(&mut self.engine);
        }
        self.close_journal();
    }

    /// What went wrong writing the journal or the row (a full disk, a folder that is not
    /// writable), for the host to show.
    pub fn persistence_errors(&self) -> Vec<String> {
        let journal = self.journal.as_ref().and_then(|j| j.error());
        let takes = self.takes.as_ref().and_then(|t| t.error());
        journal
            .map(|e| format!("journal: {e}"))
            .into_iter()
            .chain(takes)
            .collect()
    }

    /// Attach `port` if a profile owns it.
    pub fn attach(&mut self, port: &str) -> bool {
        if self.router.profile_name(port).is_none() {
            return false;
        }
        self.router.attach(port);
        true
    }

    /// Forget `port` (unplugged): the next attach repaints its LEDs from scratch.
    pub fn detach(&mut self, port: &str) -> bool {
        self.router.detach(port)
    }

    /// A scan's result: forget every port in `gone` (unplugged: the next attach repaints its LEDs
    /// from scratch, as Swift's `ControllerHub.rescan` -> `router.keepOnly(present)` drops the
    /// `SurfaceController` and its LED cache), then attach every port in `added` a profile owns.
    /// Returns what was attached, in order. The one step the hosts take when the ports change
    /// (`djapp`'s registry poll, `djctl play`'s scan), so a replugged APC or Inpulse is painted
    /// again in both.
    pub fn hotplug(&mut self, gone: &[String], added: &[String]) -> Vec<String> {
        for g in gone {
            self.detach(g);
        }
        added
            .iter()
            .filter(|n| self.attach(n))
            .cloned()
            .collect()
    }

    /// Route one raw message (normalised first). None if it is not channel-voice MIDI.
    pub fn route(&mut self, port: &str, bytes: &[u8], now: f64) -> Option<RouteResult> {
        let b = normalize_midi(bytes)?;
        self.recorded.push((port.to_string(), b.to_vec()));
        if self.engine.trace.is_none() {
            return Some(self.router.route(port, &b, now, &mut self.engine));
        }
        // the trace names the control: the device now, the control once it is decoded
        let device = self
            .router
            .profile_name(port)
            .map_or_else(|| port.to_string(), |n| device_short(&n));
        self.engine.trace_via = device.clone();
        let before = self.engine.trace.as_ref().map_or(0, Vec::len);
        let r = self.router.route(port, &b, now, &mut self.engine);
        if let RouteResult::Surface(e, _) = &r {
            let name = self
                .router
                .surface(port)
                .and_then(|s| s.profile().group(&e.group).and_then(|g| g.name(e.index)).map(str::to_string));
            let label = control_label(&device, &e.group, e.index, name.as_deref());
            if let Some(t) = self.engine.trace.as_mut() {
                for x in t.iter_mut().skip(before) {
                    if x.entry.source == device {
                        x.entry.text = x.entry.text.replacen(&device, &label, 1);
                        x.entry.source = label.clone();
                    }
                }
            }
        }
        self.engine.trace_via.clear();
        Some(r)
    }

    /// Jog-ring decay and the nudges it asks for. Call about every 10 ms.
    pub fn tick(&mut self, now: f64) {
        self.router.tick(now, &mut self.engine);
        self.engine.service_jogs();
        // every deck's `service`: the beat clock, the composes that finished, the end of a song
        self.engine.service();
        self.engine.service_loops();
        // `AppModel.poll`: `pollLoops`, then `pollJournal` (a checkpoint when one is due)
        if let Some(t) = self.takes.as_mut() {
            t.poll(&self.engine);
        }
        if let Some(j) = &self.journal {
            j.checkpoint_if_due(&self.engine);
        }
    }

    /// Changed LED messages for every attached surface, by port.
    pub fn led_frames(&mut self, now: f64) -> Vec<(String, Vec<[u8; 3]>)> {
        self.router.led_frames(now, &self.engine)
    }
}

/// A device's short name for the log: "Inpulse", "APC", "MPK", else the profile's name.
pub fn device_short(profile: &str) -> String {
    let p = profile.to_lowercase();
    for (k, v) in [("inpulse", "Inpulse"), ("apc", "APC"), ("mpk", "MPK")] {
        if p.contains(k) {
            return v.to_string();
        }
    }
    profile.to_string()
}

/// A control in words: "Inpulse browser", "APC pad 3", "Inpulse A play".
pub fn control_label(device: &str, group: &str, index: usize, name: Option<&str>) -> String {
    let (deck, tail) = match group.split_once('.') {
        Some((d, t)) if d.starts_with("deck") && d.len() == 5 => (Some(&d[4..]), t),
        Some((_, t)) => (None, t),
        None => (None, group),
    };
    let what = match name {
        Some(n) if !n.is_empty() => n.to_string(),
        _ => match tail.strip_suffix('s') {
            // a group of several (pads, buttons, knobs): which one
            Some(t) => format!("{t} {}", index + 1),
            None => tail.to_string(),
        },
    };
    match deck {
        Some(d) => format!("{device} {d} {what}"),
        None => format!("{device} {what}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controls_read_as_words() {
        assert_eq!(device_short("DJControl Inpulse 200 Mk3"), "Inpulse");
        assert_eq!(control_label("APC", "pads", 2, None), "APC pad 3");
        assert_eq!(control_label("Inpulse", "mixer.browser", 0, None), "Inpulse browser");
        assert_eq!(control_label("Inpulse", "deckA.buttons", 4, Some("play")), "Inpulse A play");
    }
}
