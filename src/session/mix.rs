//! A DJ set written down rather than performed: tracks alternate decks A / B / A / B, each
//! plays its whole song, and each next one comes in on a bar of the one playing — load (from
//! the third track on), sync to the bar, play, a linear crossfader ramp, stop the outgoing
//! deck. The result is an ordinary [`Session`], so it is stored, exported and published like
//! a recorded set. Port of `Session/SessionMix.swift`.
//!
//! Section lengths, names and tempo come from composing the spec (`TrackSpec.project()`: compose
//! plus `finish`), so a spec's `layers` are applied as they are on a deck. `finish` applies the
//! `tracks` and `initialMutes` overrides and the arrange overlay, none of which moves a song's
//! name, tempo or sections; a `tempo`, `swing` or `humanize` layer is held but never applied
//! (Swift's `Project.applyShareOverrides` has no such key). So the plan below is the same with or
//! without `layers`; they matter to the project a host composes from [`MixTrack::spec`].

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

use super::action::DeckAction;
use super::model::{DeckParams, DeckSource, Session, SessionEvent};
use super::wav::RenderedAudio;
use crate::deck::tempo_match;
use crate::gen::BeatsTables;
use crate::loudness;
use crate::spec::TrackSpec;

/// `SessionMixError`.
#[derive(Clone, Debug, PartialEq)]
pub enum SessionMixError {
    TooFewTracks,
    UnknownGenre(String),
    /// A song without sections (a loop) has no end to mix out of.
    Endless(String),
    /// The track is too short for the crossfade.
    TooShort {
        title: String,
        seconds: f64,
    },
}

impl fmt::Display for SessionMixError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionMixError::TooFewTracks => f.write_str("a mix needs at least two tracks"),
            SessionMixError::UnknownGenre(g) => write!(f, "unknown genre {g}"),
            SessionMixError::Endless(t) => {
                write!(f, "{t} is an endless loop (give it a structure)")
            }
            SessionMixError::TooShort { title, seconds } => {
                write!(
                    f,
                    "{title} is too short ({seconds:.1} s) for this crossfade"
                )
            }
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for SessionMixError {}

/// `SessionMix.Track`: one track of the set, as it plays (synced to the track before it).
#[derive(Clone, Debug, PartialEq)]
pub struct MixTrack {
    pub spec: TrackSpec,
    pub title: String,
    pub deck: String,
    /// The song's own tempo, and what it plays at once synced.
    pub bpm: f64,
    pub played_bpm: f64,
    /// Sixteenth steps in the song.
    pub steps: i64,
    /// Frame its deck starts playing, and frame its deck is stopped.
    pub in_frame: i64,
    pub out_frame: i64,
}

impl MixTrack {
    /// Length of the song at `played_bpm`.
    pub fn seconds(&self) -> f64 {
        self.steps as f64 * 15.0 / self.played_bpm
    }
}

/// `SessionMix`.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionMix {
    pub session: Session,
    pub tracks: Vec<MixTrack>,
}

impl SessionMix {
    pub const DECKS: [&'static str; 2] = ["A", "B"];
    /// Inputs land on engine blocks; events are stamped on block boundaries.
    pub const GRAIN: i64 = 512;
    /// Crossfader resolution: a step every quarter second.
    pub const RAMP_STEP: f64 = 0.25;

    /// Plan the set. `xfade` is the crossfade length in seconds; the next track starts on the
    /// outgoing one's bar nearest `xfade` before its end.
    pub fn new(
        specs: &[TrackSpec],
        xfade: f64,
        sample_rate: f64,
    ) -> Result<SessionMix, SessionMixError> {
        if specs.len() < 2 {
            return Err(SessionMixError::TooFewTracks);
        }
        let sr = sample_rate;
        let grain = Self::GRAIN;
        let frame =
            |seconds: f64| -> i64 { libm::round(seconds * sr / grain as f64) as i64 * grain };
        let mut tracks: Vec<MixTrack> = Vec::new();
        let mut events: Vec<SessionEvent> = Vec::new();
        let act = |events: &mut Vec<SessionEvent>, f: i64, a: DeckAction| {
            events.push(SessionEvent::action(f, a));
        };

        for (i, spec) in specs.iter().enumerate() {
            if !BeatsTables::shared().genres.contains_key(&spec.genre) {
                return Err(SessionMixError::UnknownGenre(spec.genre.clone()));
            }
            // What a deck would show for it: the composed project's name, tempo and song length.
            let project = spec.project();
            let steps: i64 = project.structure.iter().map(|s| s.steps).sum();
            if steps <= 0 {
                return Err(SessionMixError::Endless(project.name));
            }
            let bpm = if project.tempo > 0.0 {
                project.tempo
            } else {
                120.0
            };
            let deck = Self::DECKS[i % 2].to_string();
            let Some(prev) = tracks.last().cloned() else {
                tracks.push(MixTrack {
                    spec: spec.clone(),
                    title: project.name.clone(),
                    deck: deck.clone(),
                    bpm,
                    played_bpm: bpm,
                    steps,
                    in_frame: 0,
                    out_frame: 0,
                });
                act(&mut events, 0, DeckAction::Play { deck, on: true });
                continue;
            };
            // `sync` to the deck playing: its tempo rule, so the song's length is what it plays.
            let played = bpm * tempo_match(bpm, prev.played_bpm).ratio;
            // Come in on the outgoing track's bar nearest `xfade` before its end.
            let bar = 240.0 / prev.played_bpm;
            let start = prev.in_frame as f64 / sr;
            let bars = libm::round((prev.seconds() - xfade) / bar);
            if bars < 1.0 {
                return Err(SessionMixError::TooShort {
                    title: prev.title.clone(),
                    seconds: prev.seconds(),
                });
            }
            let sync_frame = frame(start + bars * bar);
            let play_frame = sync_frame + grain;
            let ramp_end = play_frame + frame(xfade);
            if i >= 2 {
                // The deck's previous track must be stopped (and swapped out) before this one loads.
                let freed = tracks[i - 2].out_frame;
                let load_frame = sync_frame - frame(1.0);
                if load_frame <= freed {
                    return Err(SessionMixError::TooShort {
                        title: prev.title.clone(),
                        seconds: prev.seconds(),
                    });
                }
                act(
                    &mut events,
                    load_frame,
                    DeckAction::Load {
                        deck: deck.clone(),
                        source: DeckSource::Net(spec.clone()),
                    },
                );
            }
            act(
                &mut events,
                sync_frame,
                DeckAction::Sync {
                    deck: deck.clone(),
                    align: "bar".into(),
                },
            );
            act(
                &mut events,
                play_frame,
                DeckAction::Play {
                    deck: deck.clone(),
                    on: true,
                },
            );
            // Linear ramp toward the incoming deck: A is crossfader 0, B is 1.
            let n = core::cmp::max(1, libm::round(xfade / Self::RAMP_STEP) as i64);
            for k in 1..=n {
                let x = k as f64 / n as f64;
                let v = libm::round((if deck == "B" { x } else { 1.0 - x }) * 10_000.0) / 10_000.0;
                act(
                    &mut events,
                    play_frame + frame(xfade * x),
                    DeckAction::Crossfader(v),
                );
            }
            tracks[i - 1].out_frame = ramp_end + grain;
            act(
                &mut events,
                ramp_end + grain,
                DeckAction::Play {
                    deck: prev.deck.clone(),
                    on: false,
                },
            );
            tracks.push(MixTrack {
                spec: spec.clone(),
                title: project.name.clone(),
                deck,
                bpm,
                played_bpm: played,
                steps,
                in_frame: play_frame,
                out_frame: 0,
            });
        }
        // The last track plays to its end; a second of tail after it.
        let last = tracks.len() - 1;
        let end = tracks[last].in_frame + frame(tracks[last].seconds());
        tracks[last].out_frame = end;
        let last_deck = tracks[last].deck.clone();
        act(
            &mut events,
            end,
            DeckAction::Play {
                deck: last_deck,
                on: false,
            },
        );

        // The decks start as a recording would find them: first two tracks loaded, at rest,
        // with default knobs; the crossfader full on A; the engine's default master (a Float).
        let mut decks = BTreeMap::new();
        decks.insert("A".to_string(), DeckSource::Net(specs[0].clone()));
        decks.insert("B".to_string(), DeckSource::Net(specs[1].clone()));
        let mut params = BTreeMap::new();
        params.insert("A".to_string(), DeckParams::default());
        params.insert("B".to_string(), DeckParams::default());
        let session = Session::new(
            sr,
            end + frame(1.0),
            decks,
            params,
            0.0,
            0.8f32 as f64,
            events,
        );
        Ok(SessionMix { session, tracks })
    }

    /// "<first> into <last>" — the auto title for publishing.
    pub fn title(&self) -> String {
        format!(
            "{} into {}",
            self.tracks.first().unwrap().title,
            self.tracks.last().unwrap().title
        )
    }

    /// Measure `audio`, a render of this set.
    pub fn report(&self, audio: &RenderedAudio, window: f64) -> MixReport {
        report(&self.tracks, audio, window)
    }
}

// What a set measures like once rendered (`djctl mix --report`): each track's integrated
// loudness and true peak over the stretch where it plays alone, the level step at each
// crossfade, and the whole set's. The numbers say where to listen; they never say it sounds
// right.

/// `SessionMix.Report.TrackLevel`.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackLevel {
    pub title: String,
    pub deck: String,
    /// Integrated loudness, LUFS (−200 for silence), and true peak, dBTP.
    pub lufs: f64,
    pub true_peak: f64,
}

/// `SessionMix.Report.Transition`.
#[derive(Clone, Debug, PartialEq)]
pub struct Transition {
    /// Into track `index` (0-based; never 0).
    pub index: usize,
    pub before: f64,
    pub after: f64,
}

impl Transition {
    /// after − before, LU.
    pub fn step(&self) -> f64 {
        self.after - self.before
    }
}

/// `SessionMix.Report`.
#[derive(Clone, Debug, PartialEq)]
pub struct MixReport {
    pub tracks: Vec<TrackLevel>,
    pub transitions: Vec<Transition>,
    pub lufs: f64,
    pub true_peak: f64,
    pub seconds: f64,
}

impl MixReport {
    /// What keeps it from being a set to publish: shorter than `min_seconds`, a transition
    /// stepping more than `max_step` LU either way, a true peak over `ceiling` dBTP. Empty
    /// passes (ROADMAP 6.7: ≥ 600 s, ≤ 2 LU, ≤ −1 dBTP).
    pub fn problems(&self, min_seconds: f64, max_step: f64, ceiling: f64) -> Vec<String> {
        let mut out = Vec::new();
        if self.seconds < min_seconds {
            out.push(format!(
                "{:.1} s is under {:.0} s",
                self.seconds, min_seconds
            ));
        }
        for t in &self.transitions {
            if t.step().abs() > max_step {
                out.push(format!(
                    "transition into {} steps {:+.2} LU (over {:.1})",
                    t.index + 1,
                    t.step(),
                    max_step
                ));
            }
        }
        if self.true_peak > ceiling {
            out.push(format!(
                "true peak {:.2} dBTP is over {:.1}",
                self.true_peak, ceiling
            ));
        }
        out
    }

    /// `problems()` with the defaults (600 s, 2 LU, −1 dBTP).
    pub fn default_problems(&self) -> Vec<String> {
        self.problems(600.0, 2.0, -1.0)
    }

    /// The report as `djctl mix --report` prints it.
    pub fn lines(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .tracks
            .iter()
            .enumerate()
            .map(|(i, t)| {
                format!(
                    "  {}. {}  {:<28} {:7.2} LUFS  {:6.2} dBTP",
                    i + 1,
                    t.deck,
                    t.title,
                    t.lufs,
                    t.true_peak
                )
            })
            .collect();
        for t in &self.transitions {
            out.push(format!(
                "  {} → {}  {:7.2} → {:7.2} LUFS  step {:+.2} LU",
                t.index,
                t.index + 1,
                t.before,
                t.after,
                t.step()
            ));
        }
        out.push(format!(
            "  set  {:.1} s  {:.2} LUFS  {:.2} dBTP",
            self.seconds, self.lufs, self.true_peak
        ));
        let p = self.default_problems();
        out.push(if p.is_empty() {
            "  ok".to_string()
        } else {
            p.iter()
                .map(|x| format!("  ✗ {x}"))
                .collect::<Vec<_>>()
                .join("\n")
        });
        out
    }
}

/// `SessionMix.report(tracks:audio:window:)`: the same for any plan: `tracks` in order, each
/// `in_frame` after the one before's, each `out_frame` the end of the crossfade out.
pub fn report(tracks: &[MixTrack], audio: &RenderedAudio, window: f64) -> MixReport {
    let sr = audio.sample_rate;
    let n = audio.frames() as i64;
    let clamp = |f: i64| f.max(0).min(n);
    let slice = |a: i64, b: i64| -> (&[f32], &[f32]) {
        let lo = clamp(a);
        let hi = lo.max(clamp(b));
        (
            &audio.left[lo as usize..hi as usize],
            &audio.right[lo as usize..hi as usize],
        )
    };
    let lufs = |a: i64, b: i64| {
        let s = slice(a, b);
        loudness::lufs(s.0, s.1, sr)
    };
    let w = (window * sr) as i64;
    let mut levels = Vec::new();
    let mut steps = Vec::new();
    for (i, t) in tracks.iter().enumerate() {
        // Alone: from the previous track's stop to the next one's start.
        let from = if i > 0 { tracks[i - 1].out_frame } else { 0 };
        let to = if i + 1 < tracks.len() {
            tracks[i + 1].in_frame
        } else {
            t.out_frame
        };
        let s = slice(from, to);
        levels.push(TrackLevel {
            title: t.title.clone(),
            deck: t.deck.clone(),
            lufs: loudness::lufs(s.0, s.1, sr),
            true_peak: loudness::dbtp(s.0, Some(s.1)),
        });
        if i > 0 {
            let start = t.in_frame;
            let stop = tracks[i - 1].out_frame;
            steps.push(Transition {
                index: i,
                before: lufs(start - w, start),
                after: lufs(stop, stop + w),
            });
        }
    }
    MixReport {
        tracks: levels,
        transitions: steps,
        lufs: loudness::lufs(&audio.left, &audio.right, sr),
        true_peak: loudness::dbtp(&audio.left, Some(&audio.right)),
        seconds: n as f64 / sr,
    }
}

impl Session {
    /// `Session.playing` (`SpecRender.swift`): a one-deck set that plays `spec` from the first
    /// frame to the end of its song (plus `tail` seconds), or for `seconds` — a loop has no
    /// end, so it needs them. For rendering a composed track offline with nothing recorded
    /// by hand.
    pub fn playing(
        spec: &TrackSpec,
        seconds: Option<f64>,
        tail: f64,
        sample_rate: f64,
    ) -> Option<Session> {
        let length = match seconds {
            Some(s) => s,
            None => {
                let p = spec.project();
                let steps: i64 = p.structure.iter().map(|s| s.steps).sum();
                if steps <= 0 || p.tempo <= 0.0 {
                    return None;
                }
                steps as f64 * 60.0 / (p.tempo * 4.0) + tail
            }
        };
        let mut decks = BTreeMap::new();
        decks.insert("A".to_string(), DeckSource::Net(spec.clone()));
        let mut params = BTreeMap::new();
        params.insert("A".to_string(), DeckParams::default());
        Some(Session::new(
            sample_rate,
            libm::round(length * sample_rate) as i64,
            decks,
            params,
            0.0,
            0.8,
            alloc::vec![SessionEvent::action(
                0,
                DeckAction::Play {
                    deck: "A".into(),
                    on: true
                }
            )],
        ))
    }
}
