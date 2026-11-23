//! The acid voice (`acid`, `acid-square` in the instrument table): a 303-style bass synth, all
//! synthesis, no samples. One oscillator (a saw, or a square) into a resonant four-pole lowpass
//! whose cutoff an envelope sweeps on each strike, a soft output stage (a 303's clips softly),
//! then the amplifier, which the note's length gates. An accent makes a note louder, opens the filter further and shortens its sweep, and
//! accents in a row build on each other (the accent sweep); a slide carries the voice into the
//! next note without striking it again.
//!
//! How a net plays it (the runner's acid lane, `Lane::is_acid`):
//! - **Accent** is the note's velocity, which is how the nets carry it (BeatScript's `c2!` is
//!   velocity +27, the acid genre's accent +25): none at [`ACCENT_FROM`] and below (above where
//!   a humanized 100 wanders), all of it at 127, in between in proportion.
//! - **Slide** is a note's `glide`, as on every voice (BeatScript writes it on the note a `^`
//!   slides into: `glide=` ms, and on this voice 60 ms when the part gives none): the voice still
//!   sounding the note before goes on, its envelopes are not struck again, and its pitch glides
//!   to the new note, most of the way in the glide's time (a 303's slide is a lag, fast then
//!   settling). A held note is not a slide by itself: a tonal part's notes are two steps long
//!   unless it says otherwise, so a line's notes overlap as written. The note a slide leaves says
//!   `slide` (BeatScript writes it with the glide): its gate stays open past its length until the
//!   glide comes, as a 303's gate stays high on a slide step, so a slide off a net's last step
//!   into its first (whose length the ring cuts at its end) is legato too.
//! - **One note at a time**, as a 303: a note that does not slide lets the one before go. Notes
//!   within [`CHORD_S`] of each other are a chord and each sounds on a voice of its own (a flam's
//!   grace, [`FLAM_MS`](crate::pflow::FLAM_MS) ahead, is let go when its note comes).
//! - **Decay** (the note's `decay`, BeatScript's `decay=`) is the 303's decay knob: how long the
//!   filter's sweep takes to fall, not the note's level, which the note's length gates.
//! - A **pitch drop** falls onto the note the way a slide does, in `PITCH_DROP_MS`.
//! - The runner's shaping of a synth voice (`.subtractive`'s filter envelope, `.unison`'s pair)
//!   leaves it alone: it has its own. `.subOctave` (a sine under a genre's bass) is added to it.
//!
//! Deterministic (arithmetic and `libm`, no state outside the voice), the same at every sample
//! rate (time constants in seconds, the filter prewarped), free of denormals (every state is
//! flushed to zero under 1e-18) and bounded (the ladder's input is saturated, so at full
//! resonance it rings and does not run away). The output stage's soft clip is symmetric and sits
//! before the coupling highpass: the saw's reset rings the ladder into one-sided spikes, and
//! clipped there they are no harder on a deck's safety limiter than the wavetable bass's peaks.

use alloc::vec;
use alloc::vec::Vec;

use crate::json::JsonValue;
use crate::pflow::{AcidKnobs, NoteExpr, ACID_ENV_OCT, PITCH_DROP_MS};

/// The velocity at and below which a note has no accent: above BeatScript's default 100 and the
/// ten either way a humanize moves it (an accent adds 27 to it, to 127).
pub const ACCENT_FROM: f64 = 110.0;
/// A 303's slide, ms: what a live key played while another is held glides in (a net's note says
/// its own `glide`).
pub const SLIDE_MS: f64 = 60.0;
/// Notes this close together are a chord, s: under a flam's [`FLAM_MS`](crate::pflow::FLAM_MS),
/// so a grace is let go when its note comes.
pub const CHORD_S: f64 = 0.012;
/// How much longer than its length a note that a slide leaves holds its gate (x its length): past
/// any swing of the note the glide comes on.
const SLIDE_HOLD: f64 = 1.5;
/// How much shorter than the note its gate is, ms (at most half the note): a 303's gate falls
/// before the next step, so two notes in a row are two notes.
pub const GATE_GAP_MS: f64 = 30.0;
/// The amplifier's attack and release, s (time constants).
const ATTACK_S: f64 = 0.002;
const RELEASE_S: f64 = 0.008;
/// How slowly a held note's level droops, s (the 303's volume envelope, which a gate rarely
/// holds long enough to hear).
const HOLD_S: f64 = 2.5;
/// An accented note's sweep falls at least this fast, s (the 303 takes its shortest decay on an
/// accent).
const ACCENT_DECAY_S: f64 = 0.09;
/// How much louder a full accent is (x the accent knob).
const ACCENT_GAIN: f64 = 0.35;
/// Octaves a full accent opens the filter by at its strike, and a full accent sweep on top.
const ACCENT_DIRECT: f64 = 1.2;
const ACCENT_OCT: f64 = 1.6;
/// The ladder's feedback at resonance 1 (4 is where a linear ladder oscillates; the saturation
/// keeps it bounded there too).
const K_MAX: f64 = 3.9;
/// The level the resonance takes from the passband, given back (per unit of feedback).
const COMP: f64 = 0.35;
/// The oscillator's level into the ladder: under the saturation's knee.
const DRIVE: f64 = 0.5;
/// How much further the drive knob turned all the way pushes the oscillator into the ladder's
/// saturation (x [`DRIVE`], on top of 1). The level is given back after the ladder, so the knob
/// adds grit, not loudness.
const DRIVE_MAX: f64 = 3.0;
/// The output stage's knee: a symmetric soft clip (`knee x tanh(y / knee)`) on the ladder's
/// output, in the voice's own units (the saw is +-1 into the ladder).
const KNEE: f64 = 0.4;
/// The output's coupling highpass, Hz.
const HP_HZ: f64 = 25.0;
const FC_MIN: f64 = 20.0;
const SILENT: f64 = 1e-6;
const FLUSH: f64 = 1e-18;

/// The voice's knobs, as a 303 has them. [`Default`] is a classic squelch.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct AcidParams {
    /// The filter's floor, Hz (the cutoff knob). No key tracking: a 303 has none.
    pub cutoff: f64,
    /// 0..1: how hard the filter rings at its cutoff (the resonance knob).
    pub resonance: f64,
    /// Octaves the envelope opens the filter by at a strike (the env mod knob).
    pub env_mod: f64,
    /// Seconds the filter's sweep takes to fall to about a third (the decay knob).
    pub decay: f64,
    /// 0..1: how much an accent does (the accent knob).
    pub accent: f64,
    /// 0..1: how hard the oscillator drives the ladder's saturation (0, the default, is the
    /// clean voice; a little is a driven 303's grit).
    #[cfg_attr(feature = "transfer", serde(default))]
    pub drive: f64,
}

impl Default for AcidParams {
    fn default() -> Self {
        AcidParams {
            cutoff: 300.0,
            resonance: 0.72,
            env_mod: 2.7,
            decay: 0.3,
            accent: 0.8,
            drive: 0.0,
        }
    }
}

impl AcidParams {
    /// `base` with the knobs `v` names (`cutoff` Hz, `resonance`, `envMod` octaves, `decay` s,
    /// `accent`, `drive`), each clamped to what the voice takes.
    pub fn from_json(v: &JsonValue, base: AcidParams) -> AcidParams {
        let n = |k: &str, d: f64, lo: f64, hi: f64| {
            v.get(k)
                .and_then(JsonValue::as_f64)
                .filter(|x| x.is_finite())
                .map_or(d, |x| x.clamp(lo, hi))
        };
        AcidParams {
            cutoff: n("cutoff", base.cutoff, FC_MIN, 12_000.0),
            resonance: n("resonance", base.resonance, 0.0, 1.0),
            env_mod: n("envMod", base.env_mod, 0.0, 6.0),
            decay: n("decay", base.decay, 0.01, 10.0),
            accent: n("accent", base.accent, 0.0, 1.0),
            drive: n("drive", base.drive, 0.0, 1.0),
        }
    }

    /// These knobs with a note's own settings over them (`cutoff=`, `res=`, `env=`, `accent=`,
    /// `drive=` on a BeatScript part).
    pub fn with(self, k: &AcidKnobs) -> AcidParams {
        let pct = |v: Option<u8>, d: f64| v.map_or(d, |v| f64::from(v) / 100.0);
        AcidParams {
            cutoff: if k.cutoff_hz > 0 { f64::from(k.cutoff_hz) } else { self.cutoff },
            resonance: pct(k.res, self.resonance),
            env_mod: k.env.map_or(self.env_mod, |v| f64::from(v) / 100.0 * ACID_ENV_OCT),
            decay: self.decay,
            accent: pct(k.accent, self.accent),
            drive: pct(k.drive, self.drive),
        }
    }
}

/// How accented a note of velocity `vel` (1..127) is, 0..1.
pub fn accent_of(vel: f64) -> f64 {
    ((vel - ACCENT_FROM) / (127.0 - ACCENT_FROM)).clamp(0.0, 1.0)
}

/// How long a note of `dur_ms` holds its gate, ms.
pub fn gate_ms(dur_ms: f64) -> f64 {
    dur_ms - (dur_ms * 0.5).min(GATE_GAP_MS)
}

/// One voice: an oscillator, the ladder, the amplifier and the two envelopes.
#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct AcidVoice {
    /// The note it plays (after a slide, the one it slid to).
    note: i64,
    phase: f64,
    /// log2 of the rate (cycles a sample), where a slide or a pitch drop takes it, and the
    /// per-sample share of the way it goes (0: there).
    lr: f64,
    lr_to: f64,
    slide_k: f64,
    rate: f64,
    /// The amplifier: its level, the level it goes to while the gate is open, the samples of
    /// gate left (`u32::MAX`: a held key).
    amp: f64,
    peak: f64,
    gate: u32,
    /// The filter's envelope and its per-sample fall.
    env: f64,
    env_mul: f64,
    /// The struck note's accent (x the knob).
    acc: f64,
    /// The knobs it plays at (the lane's, or the note's own): the cutoff's log2, the envelope's
    /// octaves, the ladder's feedback and the level it gives back, the oscillator's level in.
    log_cutoff: f64,
    env_mod: f64,
    k: f64,
    comp: f64,
    drive: f64,
    /// The ladder's four integrators and the coupling highpass.
    s: [f64; 4],
    hp_x: f64,
    hp_y: f64,
}

impl AcidVoice {
    /// Is it silent (its gate shut and its level gone)?
    pub fn silent(&self) -> bool {
        self.gate == 0 && self.amp < SILENT
    }

    /// The amplifier's level now.
    pub fn level(&self) -> f64 {
        self.amp
    }

    /// The note it plays.
    pub fn note(&self) -> i64 {
        self.note
    }

    /// The pitch it sounds now, Hz (`sr` the rate it runs at).
    pub fn hz(&self, sr: f64) -> f64 {
        self.rate * sr
    }

    /// The pitch it sounds now, in cycles a sample.
    pub fn rate(&self) -> f64 {
        self.rate
    }

    /// Is its gate open?
    pub fn gated(&self) -> bool {
        self.gate > 0
    }
}

/// An acid lane's voices and the coefficients they share (its knobs at its rate).
#[derive(Clone, Debug)]
#[cfg_attr(feature = "transfer", derive(serde::Serialize, serde::Deserialize))]
pub struct AcidLane {
    pub params: AcidParams,
    pub square: bool,
    pub voices: Vec<AcidVoice>,
    /// The voice the last note went to: where a slide goes on from.
    last: Option<usize>,
    /// Samples since that note.
    age: u32,
    /// The accent sweep: the last note's accented envelope, smoothed, so accents in a row build on
    /// each other (one for the lane, as a 303 has one).
    sweep: f64,
    sr: f64,
    inv_sr: f64,
    att_k: f64,
    rel_k: f64,
    droop: f64,
    sweep_k: f64,
    hp_r: f64,
    k: f64,
    comp: f64,
    log_cutoff: f64,
    /// The oscillator's level into the ladder at the drive knob.
    drive: f64,
    fc_max: f64,
    chord: u32,
}

#[inline]
fn one_pole(seconds: f64, sr: f64) -> f64 {
    1.0 - libm::exp(-1.0 / (seconds * sr))
}

#[inline]
fn log_rate(note: i64, inv_sr: f64) -> f64 {
    libm::log2(440.0 * inv_sr) + (note as f64 - 69.0) / 12.0
}

/// PolyBLEP: the step a band-limited edge takes at phase `t` (`dt` the phase a sample).
#[inline]
fn blep(t: f64, dt: f64) -> f64 {
    if t < dt {
        let x = t / dt;
        x + x - x * x - 1.0
    } else if t > 1.0 - dt {
        let x = (t - 1.0) / dt;
        x * x + x + x + 1.0
    } else {
        0.0
    }
}

#[inline]
fn flush(x: f64) -> f64 {
    if libm::fabs(x) < FLUSH {
        0.0
    } else {
        x
    }
}

impl AcidLane {
    /// `poly` voices of `params` at `sr`, a square wave if `square` (else a saw).
    pub fn new(params: AcidParams, square: bool, poly: usize, sr: f64) -> AcidLane {
        let k = K_MAX * params.resonance.clamp(0.0, 1.0);
        AcidLane {
            params,
            square,
            voices: vec![AcidVoice::default(); poly.max(1)],
            last: None,
            age: 0,
            sweep: 0.0,
            sr,
            inv_sr: 1.0 / sr,
            att_k: one_pole(ATTACK_S, sr),
            rel_k: one_pole(RELEASE_S, sr),
            droop: libm::exp(-1.0 / (HOLD_S * sr)),
            // the 303's accent sweep shares the resonance knob: more resonance, a slower, bigger wow
            sweep_k: one_pole(0.06 + 0.12 * params.resonance.clamp(0.0, 1.0), sr),
            hp_r: libm::exp(-2.0 * core::f64::consts::PI * HP_HZ / sr),
            k,
            comp: 1.0 + COMP * k,
            log_cutoff: libm::log2(params.cutoff.max(FC_MIN)),
            drive: DRIVE * (1.0 + DRIVE_MAX * params.drive.clamp(0.0, 1.0)),
            fc_max: 0.45 * sr,
            chord: (CHORD_S * sr) as u32,
        }
    }

    /// The voice a note with a `glide` slides on: the last note's, while it still sounds (its gate
    /// open, or let go and not yet silent), and never within a chord's reach of it.
    fn slides_from(&self, glide_ms: u16) -> Option<usize> {
        let lv = self.last.filter(|&v| v < self.voices.len())?;
        if glide_ms == 0 || self.age < self.chord {
            return None;
        }
        let v = &self.voices[lv];
        (v.gate > 0 || v.amp >= SILENT).then_some(lv)
    }

    /// A note from a net: `vel` its velocity (1..127, the accent rides in it), `dur_ms` its length
    /// (the gate), `e` how it is played (`glide`, `slide`, `decay`, `pitchDrop`), `decay_move` a shape
    /// macro's multiplier on the sweep's decay. Returns the voice it sounds on.
    pub fn strike(&mut self, note: i64, vel: f64, dur_ms: f64, e: &NoteExpr, decay_move: f64) -> usize {
        // a note a slide leaves holds its gate until the glide comes (a 303's slide step)
        let ms = if e.slide { dur_ms * SLIDE_HOLD } else { gate_ms(dur_ms) };
        let gate = ((ms * 0.001 * self.sr) as u32).max(1);
        self.play(note, vel, gate, e, decay_move)
    }

    /// A live key goes down: held until [`release`](Self::release). A key played while another is
    /// held slides from it in [`SLIDE_MS`] (a 303's legato).
    pub fn hold(&mut self, note: i64, vel: f64) -> usize {
        let held = self.last.is_some_and(|v| v < self.voices.len() && self.voices[v].gate == u32::MAX);
        let e = NoteExpr { glide_ms: if held { SLIDE_MS as u16 } else { 0 }, ..NoteExpr::default() };
        self.play(note, vel, u32::MAX, &e, 1.0)
    }

    /// The live key `note` comes up: its voice is let go.
    pub fn release(&mut self, note: i64) {
        for v in self.voices.iter_mut() {
            if v.gate == u32::MAX && v.note == note {
                v.gate = 0;
            }
        }
    }

    /// Every held key comes up.
    pub fn release_all(&mut self) {
        for v in self.voices.iter_mut() {
            if v.gate == u32::MAX {
                v.gate = 0;
            }
        }
    }

    /// A voice's knobs for a note that says `knobs`: the lane's own where it says none, computed
    /// as the lane computes them, so a plain note sounds exactly as it always did.
    fn knobs(&self, knobs: &AcidKnobs) -> (f64, f64, f64, f64, f64, f64) {
        if knobs.is_plain() {
            return (self.params.accent, self.log_cutoff, self.params.env_mod, self.k, self.comp, self.drive);
        }
        let p = self.params.with(knobs);
        let k = K_MAX * p.resonance.clamp(0.0, 1.0);
        let drive = DRIVE * (1.0 + DRIVE_MAX * p.drive.clamp(0.0, 1.0));
        (p.accent, libm::log2(p.cutoff.max(FC_MIN)), p.env_mod, k, 1.0 + COMP * k, drive)
    }

    fn play(&mut self, note: i64, vel: f64, gate: u32, e: &NoteExpr, decay_move: f64) -> usize {
        let (accent, log_cutoff, env_mod, kk, comp, drive) = self.knobs(&e.acid);
        let acc = accent_of(vel) * accent.clamp(0.0, 1.0);
        let w = (vel / 127.0).clamp(0.0, 1.0);
        let peak = w * (1.0 + ACCENT_GAIN * acc);
        let to = log_rate(note, self.inv_sr);
        let k = if let Some(lv) = self.slides_from(e.glide_ms) {
            // the envelopes go on; the pitch glides, most of the way in the slide's time
            let ms = f64::from(e.glide_ms);
            let v = &mut self.voices[lv];
            v.note = note;
            v.lr_to = to;
            v.slide_k = one_pole(ms * 0.001 / 3.0, self.sr);
            v.peak = peak;
            v.acc = v.acc.max(acc);
            v.gate = gate;
            (v.log_cutoff, v.env_mod, v.k, v.comp, v.drive) = (log_cutoff, env_mod, kk, comp, drive);
            lv
        } else {
            // one note at a time: the note before is let go, unless this one is a chord with it
            if let Some(lv) = self.last.filter(|&v| v < self.voices.len() && self.age >= self.chord) {
                self.voices[lv].gate = 0;
            }
            // the quietest voice whose gate is shut (the first of equals), struck
            let score = |v: &AcidVoice| v.amp + if v.gate > 0 { 10.0 } else { 0.0 };
            let mut k = 0;
            for (i, v) in self.voices.iter().enumerate() {
                if score(v) < score(&self.voices[k]) {
                    k = i;
                }
            }
            let decay = if e.decay_ms > 0 {
                f64::from(e.decay_ms) * 0.001
            } else {
                self.params.decay
            } * decay_move.max(0.05);
            // an accent takes the shortest sweep
            let tau = decay + (decay.min(ACCENT_DECAY_S) - decay) * acc;
            let v = &mut self.voices[k];
            if v.amp < SILENT {
                // from rest: a clean filter
                v.s = [0.0; 4];
                v.hp_x = 0.0;
                v.hp_y = 0.0;
            }
            v.note = note;
            v.lr_to = to;
            if e.pitch_drop > 0 {
                v.lr = to + f64::from(e.pitch_drop) / 12.0;
                v.slide_k = one_pole(PITCH_DROP_MS * 0.001 / 3.0, self.sr);
            } else {
                v.lr = to;
                v.slide_k = 0.0;
            }
            v.rate = libm::exp2(v.lr);
            v.env = 1.0;
            v.env_mul = libm::exp(-1.0 / (tau.max(0.005) * self.sr));
            v.acc = acc;
            v.peak = peak;
            v.gate = gate;
            (v.log_cutoff, v.env_mod, v.k, v.comp, v.drive) = (log_cutoff, env_mod, kk, comp, drive);
            k
        };
        self.last = Some(k);
        self.age = 0;
        k
    }

    /// The lane's clock and its accent sweep: once a sample, before its voices.
    #[inline]
    pub fn advance(&mut self) {
        self.age = self.age.saturating_add(1);
        let to = match self.last.and_then(|k| self.voices.get(k)) {
            Some(v) if !v.silent() => v.acc * v.env,
            _ => 0.0,
        };
        self.sweep = flush(self.sweep + (to - self.sweep) * self.sweep_k);
    }

    /// Is the accent sweep still falling (the lane is not done though its voices are)?
    pub fn sweeping(&self) -> bool {
        self.sweep >= SILENT
    }

    /// One sample of voice `k`; `bend` multiplies its pitch (vibrato, a nested ring's FM).
    pub fn sample(&mut self, k: usize, bend: f64) -> f64 {
        let (square, sweep) = (self.square, self.sweep);
        let v = &mut self.voices[k];
        // pitch: a slide or a pitch drop on its way
        if v.slide_k > 0.0 {
            let d = v.lr_to - v.lr;
            if libm::fabs(d) < 1e-7 {
                v.lr = v.lr_to;
                v.slide_k = 0.0;
            } else {
                v.lr += d * v.slide_k;
            }
            v.rate = libm::exp2(v.lr);
        }
        let dt = (v.rate * bend).clamp(0.0, 0.45);
        let mut ph = v.phase + dt;
        ph -= libm::floor(ph);
        v.phase = ph;
        let osc = if square {
            let mut t2 = ph + 0.5;
            t2 -= libm::floor(t2);
            (if ph < 0.5 { 1.0 } else { -1.0 }) + blep(ph, dt) - blep(t2, dt)
        } else {
            ph + ph - 1.0 - blep(ph, dt)
        };
        // the amplifier: up to its level while the gate is open, let go after
        if v.gate > 0 {
            if v.gate != u32::MAX {
                v.gate -= 1;
            }
            v.peak *= self.droop;
            v.amp += (v.peak - v.amp) * self.att_k;
        } else {
            v.amp -= v.amp * self.rel_k;
        }
        // the filter's envelope, and the lane's accent sweep on top
        v.env = flush(v.env * v.env_mul);
        let cv = v.log_cutoff + (v.env_mod + ACCENT_DIRECT * v.acc) * v.env + ACCENT_OCT * sweep;
        let fc = libm::exp2(cv).clamp(FC_MIN, self.fc_max);
        let g = libm::tan(core::f64::consts::PI * fc * self.inv_sr);
        let gg = g / (1.0 + g);
        // four one-pole stages (trapezoidal), the feedback solved for this sample, then the
        // stage input saturated
        let x = v.drive * osc;
        let b = 1.0 - gg;
        let g2 = gg * gg;
        let sum = g2 * gg * b * v.s[0] + g2 * b * v.s[1] + gg * b * v.s[2] + b * v.s[3];
        let y4 = (g2 * g2 * x + sum) / (1.0 + v.k * g2 * g2);
        let mut y = libm::tanh(x - v.k * y4);
        for s in v.s.iter_mut() {
            let d = (y - *s) * gg;
            let o = d + *s;
            *s = flush(o + d);
            y = o;
        }
        let y = y * v.comp / v.drive;
        // the output stage: a symmetric soft clip, before the highpass takes off what DC it makes
        let y = KNEE * libm::tanh(y / KNEE);
        let h = y - v.hp_x + self.hp_r * v.hp_y;
        v.hp_x = y;
        v.hp_y = flush(h);
        let out = h * v.amp;
        v.amp = flush(v.amp);
        out
    }

    /// Every voice silent, every state cleared.
    pub fn silence(&mut self) {
        for v in self.voices.iter_mut() {
            *v = AcidVoice::default();
        }
        self.last = None;
        self.age = 0;
        self.sweep = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f64 = 48_000.0;

    fn line(lane: &mut AcidLane, notes: &[(i64, f64, f64, i64, u16)], seconds: f64) -> Vec<f64> {
        // (note, start s, length s, velocity, glide ms)
        let n = (seconds * lane.sr) as usize;
        let mut out = vec![0.0; n];
        for (t, slot) in out.iter_mut().enumerate() {
            for &(note, at, len, vel, glide) in notes {
                if (at * lane.sr) as usize == t {
                    let e = NoteExpr { glide_ms: glide, ..NoteExpr::default() };
                    lane.strike(note, vel as f64, len * 1000.0, &e, 1.0);
                }
            }
            lane.advance();
            for k in 0..lane.voices.len() {
                if !lane.voices[k].silent() {
                    *slot += lane.sample(k, 1.0);
                }
            }
        }
        out
    }

    fn peak(x: &[f64]) -> f64 {
        x.iter().fold(0.0, |m, v| m.max(v.abs()))
    }

    #[test]
    fn accent_comes_from_velocity() {
        assert_eq!(accent_of(100.0), 0.0);
        assert_eq!(accent_of(110.0), 0.0, "a humanized 100 is not accented");
        assert_eq!(accent_of(80.0), 0.0);
        assert_eq!(accent_of(127.0), 1.0);
        assert!((accent_of(118.5) - 0.5).abs() < 1e-12);
    }

    /// Accented notes sliding up three octaves, then struck: everything the voice does at once.
    fn workout() -> Vec<(i64, f64, f64, i64, u16)> {
        (0..24).map(|i| (24 + i * 2, i as f64 * 0.12, if i % 4 == 3 { 0.08 } else { 0.25 }, 127, 0)).collect()
    }

    #[test]
    fn stable_at_full_resonance() {
        for square in [false, true] {
            let p = AcidParams { resonance: 1.0, accent: 1.0, ..AcidParams::default() };
            let x = line(&mut AcidLane::new(p, square, 2, SR), &workout(), 3.2);
            assert!(x.iter().all(|v| v.is_finite()));
            assert!(peak(&x) < 3.0, "square {square}: peak {}", peak(&x));
        }
    }

    #[test]
    fn bounded_with_every_knob_at_its_end() {
        for square in [false, true] {
            for (res, env, cut) in [(1.0, 6.0, 12_000.0), (1.0, 0.0, 20.0), (1.0, 6.0, 20.0), (0.0, 6.0, 300.0), (0.0, 0.0, 12_000.0)] {
                let p = AcidParams { cutoff: cut, resonance: res, env_mod: env, decay: 10.0, accent: 1.0, drive: 1.0 };
                let x = line(&mut AcidLane::new(p, square, 2, SR), &workout(), 3.2);
                assert!(x.iter().all(|v| v.is_finite()));
                assert!(peak(&x) < 8.0, "res {res} env {env} cutoff {cut}: peak {}", peak(&x));
            }
        }
    }

    #[test]
    fn it_dies_away_to_exact_zero() {
        let mut lane = AcidLane::new(AcidParams::default(), false, 1, SR);
        let x = line(&mut lane, &[(36, 0.0, 0.1, 100, 0)], 2.0);
        assert!(lane.voices[0].silent());
        assert!(peak(&x[(SR as usize)..]) == 0.0, "nothing left after the release");
    }

    #[test]
    fn the_same_line_renders_the_same() {
        let notes = [(36, 0.0, 0.25, 127, 0), (48, 0.125, 0.125, 100, 0), (39, 0.25, 0.25, 100, 80)];
        let a = line(&mut AcidLane::new(AcidParams::default(), false, 4, SR), &notes, 0.8);
        let b = line(&mut AcidLane::new(AcidParams::default(), false, 4, SR), &notes, 0.8);
        assert!(a == b);
    }

    #[test]
    fn a_glide_slides_and_a_plain_note_strikes_again_and_lets_the_one_before_go() {
        let glide = NoteExpr { glide_ms: 60, ..NoteExpr::default() };
        // two steps long, the next a step in with a glide: the same voice, not struck again
        let mut lane = AcidLane::new(AcidParams::default(), false, 4, SR);
        lane.strike(36, 100.0, 250.0, &NoteExpr::default(), 1.0);
        for _ in 0..6000 {
            lane.advance();
            lane.sample(0, 1.0);
        }
        let env_before = lane.voices[0].env;
        let v = lane.strike(48, 100.0, 125.0, &glide, 1.0);
        assert_eq!(v, 0, "the slide goes on in the voice that was sounding");
        assert_eq!(lane.voices[0].env, env_before, "its envelope was not struck again");
        assert!(lane.voices[0].gated());
        // the same without the glide: another voice, struck, and the first let go
        let mut lane = AcidLane::new(AcidParams::default(), false, 4, SR);
        lane.strike(36, 100.0, 250.0, &NoteExpr::default(), 1.0);
        for _ in 0..6000 {
            lane.advance();
            lane.sample(0, 1.0);
        }
        let v = lane.strike(48, 100.0, 125.0, &NoteExpr::default(), 1.0);
        assert_ne!(v, 0);
        assert_eq!(lane.voices[v].env, 1.0);
        assert!(!lane.voices[0].gated(), "one note at a time");
        // a live key played while another is held slides; let go of both, both are let go
        let mut lane = AcidLane::new(AcidParams::default(), false, 4, SR);
        let a = lane.hold(36, 100.0);
        for _ in 0..2400 {
            lane.advance();
            lane.sample(a, 1.0);
        }
        assert_eq!(lane.hold(43, 100.0), a);
        lane.release(43);
        assert!(!lane.voices[a].gated());
    }

    #[test]
    fn a_flams_grace_is_let_go_when_its_note_comes() {
        let mut lane = AcidLane::new(AcidParams::default(), false, 4, SR);
        let g = lane.strike(45, 50.0, 500.0, &NoteExpr::default(), 1.0);
        for _ in 0..(crate::pflow::FLAM_MS * 0.001 * SR) as usize {
            lane.advance();
            lane.sample(g, 1.0);
        }
        let n = lane.strike(45, 110.0, 500.0, &NoteExpr::default(), 1.0);
        assert_ne!(g, n);
        assert!(!lane.voices[g].gated(), "one note at a time: the grace is let go");
    }

    #[test]
    fn a_note_a_slide_leaves_holds_its_gate_for_the_glide() {
        let run = |e: NoteExpr| {
            let mut lane = AcidLane::new(AcidParams::default(), false, 4, SR);
            let v = lane.strike(40, 100.0, 125.0, &e, 1.0);
            for _ in 0..(0.125 * SR) as usize {
                lane.advance();
                lane.sample(v, 1.0);
            }
            (lane.voices[v].gated(), lane.voices[v].level())
        };
        let (open, level) = run(NoteExpr { slide: true, ..NoteExpr::default() });
        assert!(open && level > 0.5, "open at its length's end, at its level: {level}");
        let (open, level) = run(NoteExpr::default());
        assert!(!open && level < 0.05, "a plain note has let go by then: {level}");
    }

    #[test]
    fn notes_together_are_a_chord() {
        let mut lane = AcidLane::new(AcidParams::default(), false, 4, SR);
        let a = lane.strike(48, 100.0, 500.0, &NoteExpr::default(), 1.0);
        lane.advance();
        let b = lane.strike(52, 100.0, 500.0, &NoteExpr::default(), 1.0);
        assert_ne!(a, b);
    }

    /// A line's sound brightness: the RMS of its first difference over its RMS (higher, more top).
    fn brightness(x: &[f64]) -> f64 {
        let e: f64 = x.iter().map(|v| v * v).sum();
        let d: f64 = x.windows(2).map(|w| (w[1] - w[0]) * (w[1] - w[0])).sum();
        (d / e.max(1e-30)).sqrt()
    }

    fn one(knobs: AcidKnobs, vel: f64, seconds: f64) -> Vec<f64> {
        let mut lane = AcidLane::new(AcidParams::default(), false, 1, SR);
        let e = NoteExpr { acid: knobs, ..NoteExpr::default() };
        lane.strike(33, vel, seconds * 1000.0, &e, 1.0);
        (0..(seconds * SR) as usize)
            .map(|_| {
                lane.advance();
                lane.sample(0, 1.0)
            })
            .collect()
    }

    #[test]
    fn a_note_without_knobs_plays_the_lanes_and_a_notes_knobs_change_it() {
        // knobs the note does not set are the lane's, to the sample
        let plain = one(AcidKnobs::default(), 100.0, 0.3);
        let unset = one(AcidKnobs { res: None, ..AcidKnobs::default() }, 100.0, 0.3);
        assert!(plain == unset);
        // the cutoff knob opens the filter; the envelope knob at 0 leaves no sweep
        let open = one(AcidKnobs { cutoff_hz: 2000, ..AcidKnobs::default() }, 100.0, 0.3);
        assert!(brightness(&open) > brightness(&plain) * 1.3, "{} vs {}", brightness(&open), brightness(&plain));
        let flat = one(AcidKnobs { env: Some(0), ..AcidKnobs::default() }, 100.0, 0.3);
        assert!(brightness(&flat) < brightness(&plain));
        // knobs a note sets to the lane's own values sound as the lane does (within rounding)
        let same = one(AcidKnobs { cutoff_hz: 300, res: Some(72), env: Some(45), accent: Some(80), drive: Some(0) }, 100.0, 0.3);
        let err = plain.iter().zip(&same).fold(0.0_f64, |m, (a, b)| m.max((a - b).abs()));
        assert!(err < 1e-6, "{err}");
    }

    #[test]
    fn the_filter_moves_on_each_strike() {
        // the envelope sweeps the cutoff down: the note's start is brighter than its end
        let x = one(AcidKnobs::default(), 100.0, 0.4);
        let n = x.len();
        let (head, tail) = (&x[n / 40..n / 8], &x[n * 5 / 8..n * 7 / 8]);
        assert!(brightness(head) > brightness(tail) * 1.25, "{} then {}", brightness(head), brightness(tail));
    }

    #[test]
    fn an_accent_opens_the_filter_and_is_louder() {
        let plain = one(AcidKnobs::default(), 100.0, 0.2);
        let acc = one(AcidKnobs::default(), 127.0, 0.2);
        assert!(peak(&acc) > peak(&plain));
        assert!(brightness(&acc[..2400]) > brightness(&plain[..2400]));
        // and the accent knob at 0 takes the accent away (it is only the velocity's level then)
        let none = one(AcidKnobs { accent: Some(0), ..AcidKnobs::default() }, 127.0, 0.2);
        assert!(brightness(&none[..2400]) < brightness(&acc[..2400]));
    }

    #[test]
    fn drive_adds_grit_not_level_and_stays_bounded() {
        let clean = one(AcidKnobs::default(), 127.0, 0.3);
        let driven = one(AcidKnobs { drive: Some(100), res: Some(100), ..AcidKnobs::default() }, 127.0, 0.3);
        assert!(driven.iter().all(|v| v.is_finite()));
        assert!(peak(&driven) < 2.0 * peak(&clean), "{} vs {}", peak(&driven), peak(&clean));
        let light = one(AcidKnobs { drive: Some(30), ..AcidKnobs::default() }, 100.0, 0.3);
        let plain = one(AcidKnobs::default(), 100.0, 0.3);
        assert!(light != plain, "the drive knob changes the sound");
    }

    #[test]
    fn the_same_at_every_rate() {
        // the knobs mean the same at 24, 44.1 and 96 kHz: the strike's level a tenth of a second in
        let level = |sr: f64| {
            let mut lane = AcidLane::new(AcidParams::default(), false, 1, sr);
            lane.strike(36, 127.0, 300.0, &NoteExpr::default(), 1.0);
            let n = (0.1 * sr) as usize;
            let mut e = 0.0;
            for i in 0..n {
                lane.advance();
                let y = lane.sample(0, 1.0);
                if i > n / 2 {
                    e += y * y;
                }
            }
            (e / (n / 2) as f64).sqrt()
        };
        let base = level(48_000.0);
        for sr in [24_000.0, 44_100.0, 96_000.0] {
            let l = level(sr);
            assert!((20.0 * (l / base).log10()).abs() < 1.0, "{sr}: {l} vs {base}");
        }
    }
}
