//! The mixer as data: every strip, the loop channel, the master section and the transport, with a
//! JSON codec, plus the small rules the mixer panel needs (which deck is the master tempo, tap
//! tempo, the tempo gap a sync panel shows). Pure: hosts own the engine, this owns the numbers.
//!
//! Beyond Swift parity (docs/HUB.md, milestone M1): `DJEngine` keeps these values in its deck
//! objects and never writes them out as one document, so the field names here are new. The
//! knobs keep Swift's units (0...1, 0.5 flat for the EQ and filter).

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::deck::beat::tempo_match_in;
use crate::fx::aux_bus::{AuxFxKey, AuxSettings};
use crate::fx::StripSettings;
use crate::json::{JsonObject, JsonValue};

/// The schema string a [`MixerState`] carries.
pub const MIXER_SCHEMA: &str = "djrust/mixer/v1";

/// The tempo fader's travels a deck can choose (±8 % is Swift's `Deck.pitchRange`).
pub const PITCH_RANGES: [f64; 3] = [0.08, 0.16, 0.50];

/// The headphone mix a new engine starts at: 0, "cue only" (`cueSum` at full, the master out of
/// the phones), so nothing but a cued deck reaches them. `DJEngine._headphoneMix`; held by
/// `CueOutputTests.theMasterMixIsOnTheFirstPairOnly` and `CueControlTests`.
pub const HEADPHONE_MIX: f64 = 0.0;

/// The headphone level a new engine starts at (`DJEngine._headphoneLevel`).
pub const HEADPHONE_LEVEL: f64 = 0.7;

/// The loop channel's fader at the start (`DJEngine`'s loop channel; `LOOP_VOLUME` in djhost).
pub const LOOP_VOLUME: f64 = 0.8;

/// One deck's channel strip.
#[derive(Clone, Debug, PartialEq)]
pub struct StripState {
    pub deck: String,
    /// The channel fader, 0...1.
    pub volume: f64,
    /// The gain (trim) knob, 0...1 (0.5 = unity).
    pub gain: f64,
    pub eq: StripSettings,
    pub mute: bool,
    /// Cued in the headphones.
    pub pfl: bool,
    /// The tempo fader's travel, a fraction (0.08 = ±8 %).
    pub pitch_range: f64,
    /// The aux send, 0...1, post-fader (beyond Swift: `fx::aux_bus`). 0 = nothing sent.
    pub aux: f64,
}

impl StripState {
    pub fn new(deck: &str) -> StripState {
        StripState {
            deck: deck.to_string(),
            volume: 0.8,
            gain: 0.5,
            eq: StripSettings::default(),
            mute: false,
            pfl: false,
            pitch_range: PITCH_RANGES[0],
            aux: 0.0,
        }
    }
}

/// The loop channel "L": no crossfader, no gain knob, no cue.
#[derive(Clone, Debug, PartialEq)]
pub struct LoopStripState {
    pub volume: f64,
    pub eq: StripSettings,
    pub mute: bool,
    /// The aux send, 0...1.
    pub aux: f64,
}

impl Default for LoopStripState {
    fn default() -> Self {
        LoopStripState {
            volume: LOOP_VOLUME,
            eq: StripSettings::default(),
            mute: false,
            aux: 0.0,
        }
    }
}

/// Every mixer control's position.
#[derive(Clone, Debug, PartialEq)]
pub struct MixerState {
    pub strips: Vec<StripState>,
    pub loop_channel: LoopStripState,
    pub crossfader: f64,
    pub master: f64,
    pub headphone_mix: f64,
    pub headphone_level: f64,
    pub transport: Transport,
    /// The aux bus's return fader, mute and effect sliders (`fx::aux_bus`).
    pub aux: AuxSettings,
}

impl MixerState {
    /// The engine's starting positions for these decks.
    pub fn new(decks: &[&str]) -> MixerState {
        MixerState {
            strips: decks.iter().map(|d| StripState::new(d)).collect(),
            loop_channel: LoopStripState::default(),
            crossfader: 0.5,
            master: 1.0,
            headphone_mix: HEADPHONE_MIX,
            headphone_level: HEADPHONE_LEVEL,
            transport: Transport::default(),
            aux: AuxSettings::default(),
        }
    }

    pub fn strip(&self, deck: &str) -> Option<&StripState> {
        self.strips.iter().find(|s| s.deck == deck)
    }

    pub fn to_json(&self) -> JsonValue {
        let n = JsonValue::Number;
        let strips = self
            .strips
            .iter()
            .map(|s| {
                let mut o = JsonObject::from_pairs([
                    ("deck", JsonValue::str(s.deck.clone())),
                    ("volume", n(s.volume)),
                    ("gain", n(s.gain)),
                    ("eq", eq_json(&s.eq)),
                    ("mute", JsonValue::Bool(s.mute)),
                    ("pfl", JsonValue::Bool(s.pfl)),
                    ("pitchRange", n(s.pitch_range)),
                ]);
                // the aux send is written only when it is up: a mixer that never used the aux
                // reads and writes as it did before it existed
                if s.aux != 0.0 {
                    o.set("aux", n(s.aux));
                }
                JsonValue::Object(o)
            })
            .collect();
        let l = &self.loop_channel;
        let mut lo = JsonObject::from_pairs([
            ("volume", n(l.volume)),
            ("eq", eq_json(&l.eq)),
            ("mute", JsonValue::Bool(l.mute)),
        ]);
        if l.aux != 0.0 {
            lo.set("aux", n(l.aux));
        }
        let mut o = JsonObject::from_pairs([
            ("schema", JsonValue::str(MIXER_SCHEMA)),
            ("strips", JsonValue::Array(strips)),
            ("loop", JsonValue::Object(lo)),
            ("crossfader", n(self.crossfader)),
            ("master", n(self.master)),
            ("headphoneMix", n(self.headphone_mix)),
            ("headphoneLevel", n(self.headphone_level)),
            ("transport", self.transport.to_json()),
        ]);
        if self.aux != AuxSettings::default() {
            o.set("auxBus", aux_json(&self.aux));
        }
        JsonValue::Object(o)
    }

    /// Read a mixer back. Missing fields take the defaults and values are clamped to their
    /// ranges. Strips are kept in the document's order. A strip with no `deck` is skipped.
    pub fn from_json(v: &JsonValue) -> Result<MixerState, String> {
        let o = v.as_object().ok_or("mixer: not an object")?;
        if let Some(s) = o.get("schema").and_then(JsonValue::as_str) {
            if s != MIXER_SCHEMA {
                return Err(alloc::format!("mixer: unknown schema {s:?}"));
            }
        }
        let mut m = MixerState::new(&[]);
        if let Some(a) = o.get("strips").and_then(JsonValue::as_array) {
            for s in a {
                let Some(deck) = s.get("deck").and_then(JsonValue::as_str) else {
                    continue;
                };
                let d = StripState::new(deck);
                m.strips.push(StripState {
                    volume: unit(s.get("volume"), d.volume),
                    gain: unit(s.get("gain"), d.gain),
                    eq: eq_from(s.get("eq")),
                    mute: flag(s.get("mute")),
                    pfl: flag(s.get("pfl")),
                    pitch_range: pitch_range(s.get("pitchRange")),
                    aux: unit(s.get("aux"), 0.0),
                    deck: d.deck,
                });
            }
        }
        let l = o.get("loop");
        let dl = LoopStripState::default();
        m.loop_channel = LoopStripState {
            volume: unit(l.and_then(|l| l.get("volume")), dl.volume),
            eq: eq_from(l.and_then(|l| l.get("eq"))),
            mute: flag(l.and_then(|l| l.get("mute"))),
            aux: unit(l.and_then(|l| l.get("aux")), 0.0),
        };
        m.aux = aux_from(o.get("auxBus"));
        m.crossfader = unit(o.get("crossfader"), 0.5);
        m.master = unit(o.get("master"), 1.0);
        m.headphone_mix = unit(o.get("headphoneMix"), HEADPHONE_MIX);
        m.headphone_level = unit(o.get("headphoneLevel"), HEADPHONE_LEVEL);
        m.transport = Transport::from_json(o.get("transport"));
        Ok(m)
    }
}

/// The aux bus as JSON: `{"return", "mute", <beats' slider ids>: value}`.
pub fn aux_json(a: &AuxSettings) -> JsonValue {
    let mut o = JsonObject::from_pairs([
        ("return", JsonValue::Number(a.level)),
        ("mute", JsonValue::Bool(a.mute)),
    ]);
    for k in AuxFxKey::ALL {
        o.set(k.id(), JsonValue::Number(a.get(k)));
    }
    JsonValue::Object(o)
}

/// Read the aux bus back: missing fields take the defaults, values are clamped to their ranges.
pub fn aux_from(v: Option<&JsonValue>) -> AuxSettings {
    let mut a = AuxSettings::default();
    let Some(v) = v else {
        return a;
    };
    a.level = unit(v.get("return"), a.level);
    a.mute = flag(v.get("mute"));
    for k in AuxFxKey::ALL {
        if let Some(x) = v.get(k.id()).and_then(JsonValue::as_f64) {
            a.set(k, x);
        }
    }
    a
}

fn eq_json(e: &StripSettings) -> JsonValue {
    JsonValue::Object(JsonObject::from_pairs([
        ("high", JsonValue::Number(e.eq_high)),
        ("mid", JsonValue::Number(e.eq_mid)),
        ("low", JsonValue::Number(e.eq_low)),
        ("filter", JsonValue::Number(e.filter)),
    ]))
}

fn eq_from(v: Option<&JsonValue>) -> StripSettings {
    let k = |key: &str| unit(v.and_then(|v| v.get(key)), 0.5);
    StripSettings {
        filter: k("filter"),
        eq_low: k("low"),
        eq_mid: k("mid"),
        eq_high: k("high"),
    }
}

fn unit(v: Option<&JsonValue>, default: f64) -> f64 {
    match v.and_then(JsonValue::as_f64) {
        Some(x) if x.is_finite() => x.clamp(0.0, 1.0),
        _ => default,
    }
}

fn flag(v: Option<&JsonValue>) -> bool {
    v.and_then(JsonValue::as_bool).unwrap_or(false)
}

/// A pitch range read back: the nearest of [`PITCH_RANGES`].
fn pitch_range(v: Option<&JsonValue>) -> f64 {
    let Some(x) = v.and_then(JsonValue::as_f64).filter(|x| x.is_finite()) else {
        return PITCH_RANGES[0];
    };
    let mut best = PITCH_RANGES[0];
    for r in PITCH_RANGES {
        if (r - x).abs() < (best - x).abs() {
            best = r;
        }
    }
    best
}

// MARK: transport

/// The master transport: what the clock follows and whether it runs. M1 has START / STOP play
/// and pause the decks; M2 sends MIDI clock from the same state.
#[derive(Clone, Debug, PartialEq)]
pub struct Transport {
    pub running: bool,
    /// The deck the tempo is locked to (None: [`master_deck`] picks one).
    pub lock: Option<String>,
    /// The tempo when no deck plays.
    pub free_bpm: f64,
}

impl Default for Transport {
    fn default() -> Self {
        Transport {
            running: false,
            lock: None,
            free_bpm: 120.0,
        }
    }
}

impl Transport {
    pub fn to_json(&self) -> JsonValue {
        JsonValue::Object(JsonObject::from_pairs([
            ("running", JsonValue::Bool(self.running)),
            (
                "lock",
                self.lock.clone().map_or(JsonValue::Null, JsonValue::String),
            ),
            ("freeBpm", JsonValue::Number(self.free_bpm)),
        ]))
    }

    pub fn from_json(v: Option<&JsonValue>) -> Transport {
        let d = Transport::default();
        let Some(v) = v else { return d };
        Transport {
            running: flag(v.get("running")),
            lock: v.get("lock").and_then(JsonValue::as_str).map(String::from),
            free_bpm: v
                .get("freeBpm")
                .and_then(JsonValue::as_f64)
                .filter(|b| b.is_finite())
                .map_or(d.free_bpm, clamp_bpm),
        }
    }
}

/// Lowest and highest tempo the transport takes.
pub const BPM_RANGE: (f64, f64) = (40.0, 300.0);

pub fn clamp_bpm(b: f64) -> f64 {
    b.clamp(BPM_RANGE.0, BPM_RANGE.1)
}

/// A deck as the master-tempo rule sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct DeckTempo {
    pub deck: String,
    pub playing: bool,
    /// The tempo it runs at now; None: it has no clock (nothing loaded, or no beat grid).
    pub bpm: Option<f64>,
}

/// Where the master tempo comes from.
#[derive(Clone, Debug, PartialEq)]
pub enum TempoSource {
    Deck(String),
    Free,
}

/// The master tempo and where it comes from.
///
/// 1. A locked deck with a clock, playing or not.
/// 2. Else the playing deck the crossfader favours: A at or left of centre, B right of it.
///    At centre, A wins, as the first deck in mixer order.
/// 3. Else any playing deck with a clock, in mixer order.
/// 4. Else the free tempo.
///
/// Decks other than A and B count as centred.
pub fn master_deck(decks: &[DeckTempo], crossfader: f64, t: &Transport) -> (TempoSource, f64) {
    if let Some(lock) = &t.lock {
        if let Some(b) = decks.iter().find(|d| &d.deck == lock).and_then(|d| d.bpm) {
            return (TempoSource::Deck(lock.clone()), b);
        }
    }
    let favoured = if crossfader > 0.5 { "B" } else { "A" };
    let playing = |d: &&DeckTempo| d.playing && d.bpm.is_some();
    let pick = decks
        .iter()
        .filter(playing)
        .find(|d| d.deck == favoured)
        .or_else(|| decks.iter().find(playing));
    match pick {
        Some(d) => (
            TempoSource::Deck(d.deck.clone()),
            d.bpm.unwrap_or(t.free_bpm),
        ),
        None => (TempoSource::Free, t.free_bpm),
    }
}

/// How far `bpm` is from `target`, in BPM, after the half / double-time choice that sync makes
/// (so a 70 BPM deck against a 140 BPM master shows 0, not −70).
pub fn tempo_gap(bpm: f64, target: f64) -> f64 {
    if bpm <= 0.0 || target <= 0.0 {
        return 0.0;
    }
    let m = tempo_match_in(bpm, target, f64::INFINITY).mult;
    bpm - target * m
}

// MARK: tap tempo

/// Tap tempo: the mean of the last [`TapTempo::KEEP`] intervals. A pause longer than
/// [`TapTempo::RESET_S`] starts over. Times are seconds on any monotonic clock.
#[derive(Clone, Debug, Default)]
pub struct TapTempo {
    taps: Vec<f64>,
}

impl TapTempo {
    pub const KEEP: usize = 4;
    pub const RESET_S: f64 = 2.0;

    /// A tap at `now`. Returns the tempo once there are two taps.
    pub fn tap(&mut self, now: f64) -> Option<f64> {
        if let Some(&last) = self.taps.last() {
            if now - last > Self::RESET_S || now <= last {
                self.taps.clear();
            }
        }
        self.taps.push(now);
        if self.taps.len() > Self::KEEP + 1 {
            self.taps.remove(0);
        }
        self.bpm()
    }

    pub fn bpm(&self) -> Option<f64> {
        let n = self.taps.len();
        if n < 2 {
            return None;
        }
        let span = self.taps[n - 1] - self.taps[0];
        Some(clamp_bpm(60.0 * (n - 1) as f64 / span))
    }

    pub fn taps(&self) -> usize {
        self.taps.len()
    }
}

// MARK: auto transport

/// What [`AutoTransport::step`] asks the host to do. The host carries them out in order:
/// `Lock` before `Start`, so the clock starts on the locked deck's bar line.
#[derive(Clone, Debug, PartialEq)]
pub enum AutoStep {
    /// Lock the master tempo to this deck. While the transport runs this is a re-lock: the
    /// clock keeps going (no Stop / Start), it only follows the new deck.
    Lock(String),
    /// Set the transport running (only the flag: the decks already play). The clock's Start
    /// goes out on the locked deck's next bar line.
    Start,
    /// Clear the running flag (no deck is left to follow). The decks are not touched.
    Stop,
}

/// The transport following the decks (docs/HUB.md, "Auto transport"), so the clock the TD-3
/// and the PD705 run on never silently free-runs:
///
/// - a deck starts playing and the transport is stopped: lock to it and start;
/// - the locked deck stops: lock to another playing deck if there is one (the clock goes on),
///   else, once nothing has played for [`AutoTransport::STOP_AFTER_S`], stop;
/// - the crossfader hands the master to the other playing deck: re-lock, without Stop / Start.
///   The handover has [`AutoTransport::HYSTERESIS`] either side of centre, so a fader resting
///   at the middle does not flap the lock;
/// - a lock, START or STOP by hand ([`hold`](Self::hold)) holds it off until a deck next
///   starts playing.
///
/// Only decks with a clock (a beat grid) count. Pure: the host polls it once a frame with the
/// decks as [`DeckTempo`]s and the time in seconds on any monotonic clock.
#[derive(Clone, Debug)]
pub struct AutoTransport {
    pub on: bool,
    /// A hand on the transport since the last deck play: auto does nothing.
    pub held: bool,
    /// When `held` was set: deck plays in the next [`Self::HOLD_GRACE_S`] (START's own) do
    /// not release it.
    held_at: f64,
    /// Which decks were playing (with a clock) at the last step; None before the first, so
    /// a deck already playing at the first look counts as starting (a relaunch, auto on).
    was: Option<Vec<(String, bool)>>,
    /// Since when nothing has been playing while the transport runs.
    idle_since: Option<f64>,
}

impl Default for AutoTransport {
    fn default() -> Self {
        AutoTransport {
            on: true,
            held: false,
            held_at: f64::NEG_INFINITY,
            was: None,
            idle_since: None,
        }
    }
}

impl AutoTransport {
    /// The crossfader must pass centre by this much to hand the master over.
    pub const HYSTERESIS: f64 = 0.05;
    /// How long nothing plays before auto stops the transport (a cue press or a quick
    /// pause-and-play does not stop the gear).
    pub const STOP_AFTER_S: f64 = 0.5;
    /// After a hand on the transport, deck plays this soon are its own (START plays them).
    pub const HOLD_GRACE_S: f64 = 0.75;

    /// Turn auto on or off. Turning it on forgets which decks were playing, so one already
    /// playing starts the transport on the next step.
    pub fn set_on(&mut self, on: bool) {
        if on && !self.on {
            self.was = None;
            self.held = false;
        }
        self.on = on;
        self.idle_since = None;
    }

    /// A lock, START or STOP by hand: auto leaves the transport alone until a deck starts.
    pub fn hold(&mut self, now: f64) {
        self.held = true;
        self.held_at = now;
        self.idle_since = None;
    }

    /// One look at the decks: what to do now.
    pub fn step(
        &mut self,
        decks: &[DeckTempo],
        crossfader: f64,
        t: &Transport,
        now: f64,
    ) -> Vec<AutoStep> {
        let playing = |d: &DeckTempo| d.playing && d.bpm.is_some();
        let started: Vec<&DeckTempo> = decks
            .iter()
            .filter(|d| playing(d))
            .filter(|d| match &self.was {
                None => true,
                Some(w) => !w.iter().any(|(n, p)| *p && n == &d.deck),
            })
            .collect();
        let was = decks
            .iter()
            .map(|d| (d.deck.clone(), playing(d)))
            .collect();
        let started: Vec<String> = started.into_iter().map(|d| d.deck.clone()).collect();
        self.was = Some(was);
        if self.held && !started.is_empty() && now - self.held_at > Self::HOLD_GRACE_S {
            self.held = false;
        }
        if !self.on || self.held {
            self.idle_since = None;
            return Vec::new();
        }
        let unlocked = Transport {
            lock: None,
            ..t.clone()
        };
        if !t.running {
            self.idle_since = None;
            if started.is_empty() {
                return Vec::new();
            }
            // the deck that started; two at once: the one the crossfader favours
            let only: Vec<DeckTempo> = decks
                .iter()
                .filter(|d| started.contains(&d.deck))
                .cloned()
                .collect();
            return match master_deck(&only, crossfader, &unlocked).0 {
                TempoSource::Deck(d) => alloc::vec![AutoStep::Lock(d), AutoStep::Start],
                TempoSource::Free => Vec::new(),
            };
        }
        let locked = t
            .lock
            .as_ref()
            .and_then(|l| decks.iter().find(|d| &d.deck == l))
            .filter(|d| playing(d));
        let Some(locked) = locked else {
            // the locked deck stopped (or none is locked): follow another, else stop
            return match master_deck(decks, crossfader, &unlocked).0 {
                TempoSource::Deck(d) => {
                    self.idle_since = None;
                    alloc::vec![AutoStep::Lock(d)]
                }
                TempoSource::Free => {
                    let since = *self.idle_since.get_or_insert(now);
                    if now - since >= Self::STOP_AFTER_S {
                        self.idle_since = None;
                        alloc::vec![AutoStep::Stop]
                    } else {
                        Vec::new()
                    }
                }
            };
        };
        self.idle_since = None;
        // the master follows the crossfader, leaning toward the deck locked now
        let lean = match locked.deck.as_str() {
            "A" => -Self::HYSTERESIS,
            "B" => Self::HYSTERESIS,
            _ => 0.0,
        };
        match master_deck(decks, crossfader + lean, &unlocked).0 {
            TempoSource::Deck(d) if d != locked.deck => alloc::vec![AutoStep::Lock(d)],
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(deck: &str, playing: bool, bpm: Option<f64>) -> DeckTempo {
        DeckTempo {
            deck: deck.into(),
            playing,
            bpm,
        }
    }

    #[test]
    fn json_round_trip() {
        let mut m = MixerState::new(&["A", "B"]);
        m.strips[0].volume = 0.25;
        m.strips[0].eq.eq_low = 0.0;
        m.strips[1].mute = true;
        m.strips[1].pfl = true;
        m.strips[1].pitch_range = 0.16;
        m.loop_channel.mute = true;
        m.loop_channel.eq.filter = 0.2;
        m.crossfader = 0.75;
        m.transport.lock = Some("B".into());
        m.transport.free_bpm = 128.0;
        let text = m.to_json().canonical_js();
        let back = MixerState::from_json(&JsonValue::parse(&text).unwrap()).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn the_aux_sends_and_bus_round_trip_and_an_unused_aux_writes_nothing() {
        let fresh = MixerState::new(&["A", "B"]);
        let text = fresh.to_json().canonical_js();
        assert!(!text.contains("aux"), "a mixer that never used the aux is unchanged: {text}");
        let mut m = fresh.clone();
        m.strips[1].aux = 0.6;
        m.loop_channel.aux = 0.25;
        m.aux.level = 0.5;
        m.aux.mute = true;
        m.aux.set(AuxFxKey::DelayFeedback, 72.0);
        m.aux.set(AuxFxKey::ReverbSize, 95.0);
        let text = m.to_json().canonical_js();
        assert!(text.contains(r#""auxBus""#) && text.contains(r#""delay-feedback":72"#), "{text}");
        let back = MixerState::from_json(&JsonValue::parse(&text).unwrap()).unwrap();
        assert_eq!(back, m);
        // out-of-range values clamp, unknown ones default
        let v = JsonValue::parse(
            r#"{"strips":[{"deck":"A","aux":9}],"loop":{"aux":-1},
                "auxBus":{"return":2,"delay-feedback":500,"reverb-wet":"x"}}"#,
        )
        .unwrap();
        let m = MixerState::from_json(&v).unwrap();
        assert_eq!((m.strips[0].aux, m.loop_channel.aux), (1.0, 0.0));
        assert_eq!(m.aux.level, 1.0);
        assert_eq!(m.aux.get(AuxFxKey::DelayFeedback), 90.0);
        assert_eq!(m.aux.get(AuxFxKey::ReverbWet), 20.0);
    }

    #[test]
    fn missing_fields_default_and_values_clamp() {
        let v = JsonValue::parse(
            r#"{"strips":[{"deck":"A","volume":3,"pitchRange":0.2},{"volume":1}],
                "crossfader":-1,"transport":{"freeBpm":9999}}"#,
        )
        .unwrap();
        let m = MixerState::from_json(&v).unwrap();
        assert_eq!(m.strips.len(), 1, "a strip with no deck is skipped");
        assert_eq!(m.strips[0].volume, 1.0);
        assert_eq!(m.strips[0].gain, 0.5);
        assert_eq!(m.strips[0].pitch_range, 0.16);
        assert_eq!(m.crossfader, 0.0);
        assert_eq!(m.master, 1.0);
        assert_eq!(m.transport.free_bpm, BPM_RANGE.1);
        assert_eq!(m.loop_channel, LoopStripState::default());
    }

    #[test]
    fn unknown_schema_is_refused() {
        let v = JsonValue::parse(r#"{"schema":"other/v9"}"#).unwrap();
        assert!(MixerState::from_json(&v).is_err());
        assert!(MixerState::from_json(&JsonValue::Number(1.0)).is_err());
    }

    #[test]
    fn master_deck_rule() {
        let t = Transport::default();
        let both = [dt("A", true, Some(124.0)), dt("B", true, Some(128.0))];
        assert_eq!(
            master_deck(&both, 0.5, &t),
            (TempoSource::Deck("A".into()), 124.0)
        );
        assert_eq!(
            master_deck(&both, 0.9, &t),
            (TempoSource::Deck("B".into()), 128.0)
        );
        // the favoured deck is stopped: the other playing one
        let b_only = [dt("A", false, Some(124.0)), dt("B", true, Some(128.0))];
        assert_eq!(master_deck(&b_only, 0.0, &t).1, 128.0);
        // nothing plays: free tempo
        let none = [dt("A", false, Some(124.0)), dt("B", false, None)];
        assert_eq!(master_deck(&none, 0.5, &t), (TempoSource::Free, 120.0));
        // a lock wins, even stopped; a lock on a deck with no clock falls through
        let mut locked = Transport {
            lock: Some("A".into()),
            ..Default::default()
        };
        assert_eq!(master_deck(&none, 1.0, &locked).1, 124.0);
        locked.lock = Some("B".into());
        assert_eq!(master_deck(&none, 1.0, &locked).0, TempoSource::Free);
    }

    /// Run one step and carry out its steps on `t`, as the host does.
    fn run(a: &mut AutoTransport, decks: &[DeckTempo], cf: f64, t: &mut Transport, now: f64) -> Vec<AutoStep> {
        let out = a.step(decks, cf, t, now);
        for s in &out {
            match s {
                AutoStep::Lock(d) => t.lock = Some(d.clone()),
                AutoStep::Start => t.running = true,
                AutoStep::Stop => t.running = false,
            }
        }
        out
    }

    #[test]
    fn auto_transport_starts_on_a_play_and_follows_the_decks() {
        let mut a = AutoTransport::default();
        let mut t = Transport::default();
        let idle = [dt("A", false, Some(140.0)), dt("B", false, Some(128.0))];
        assert!(run(&mut a, &idle, 0.0, &mut t, 0.0).is_empty());
        // A plays: lock A, start
        let a_on = [dt("A", true, Some(140.0)), dt("B", false, Some(128.0))];
        assert_eq!(
            run(&mut a, &a_on, 0.0, &mut t, 1.0),
            [AutoStep::Lock("A".into()), AutoStep::Start]
        );
        assert!(t.running);
        assert!(run(&mut a, &a_on, 0.0, &mut t, 1.1).is_empty());
        // B joins with the crossfader left: A stays master
        let both = [dt("A", true, Some(140.0)), dt("B", true, Some(140.0))];
        assert!(run(&mut a, &both, 0.0, &mut t, 2.0).is_empty());
        // the crossfader resting just past centre does not flap the lock
        assert!(run(&mut a, &both, 0.52, &mut t, 2.1).is_empty());
        // past the band: re-lock to B, no Stop / Start
        assert_eq!(run(&mut a, &both, 0.6, &mut t, 2.2), [AutoStep::Lock("B".into())]);
        assert!(run(&mut a, &both, 0.48, &mut t, 2.3).is_empty());
        assert_eq!(run(&mut a, &both, 0.4, &mut t, 2.4), [AutoStep::Lock("A".into())]);
        // the locked deck stops: follow the other one, still running
        let b_on = [dt("A", false, Some(140.0)), dt("B", true, Some(140.0))];
        assert_eq!(run(&mut a, &b_on, 0.4, &mut t, 3.0), [AutoStep::Lock("B".into())]);
        assert!(t.running);
        // nothing plays: stop, after a moment (a quick pause does not stop the gear)
        assert!(run(&mut a, &idle, 0.4, &mut t, 4.0).is_empty());
        assert!(run(&mut a, &idle, 0.4, &mut t, 4.2).is_empty());
        assert_eq!(run(&mut a, &idle, 0.4, &mut t, 4.6), [AutoStep::Stop]);
        assert!(!t.running);
        assert_eq!(t.lock.as_deref(), Some("B"), "the last lock is kept");
        // a pause shorter than STOP_AFTER_S keeps it running
        assert_eq!(
            run(&mut a, &b_on, 0.4, &mut t, 5.0),
            [AutoStep::Lock("B".into()), AutoStep::Start]
        );
        assert!(run(&mut a, &idle, 0.4, &mut t, 5.1).is_empty());
        assert!(run(&mut a, &b_on, 0.4, &mut t, 5.3).is_empty());
        assert!(t.running);
    }

    #[test]
    fn auto_transport_takes_a_deck_already_playing_and_ignores_clockless_ones() {
        // a relaunch: A is playing at the first look
        let mut a = AutoTransport::default();
        let mut t = Transport {
            lock: Some("B".into()),
            ..Default::default()
        };
        let a_on = [dt("A", true, Some(140.0)), dt("B", false, Some(128.0))];
        assert_eq!(
            run(&mut a, &a_on, 1.0, &mut t, 0.0),
            [AutoStep::Lock("A".into()), AutoStep::Start]
        );
        // a deck with no beat grid is not a clock source
        let mut a = AutoTransport::default();
        let mut t = Transport::default();
        let clockless = [dt("A", true, None)];
        assert!(run(&mut a, &clockless, 0.0, &mut t, 0.0).is_empty());
        assert!(!t.running);
        // both start in the same look: the crossfader picks
        let both = [dt("A", true, Some(140.0)), dt("B", true, Some(128.0))];
        assert_eq!(
            run(&mut a, &both, 0.9, &mut t, 1.0),
            [AutoStep::Lock("B".into()), AutoStep::Start]
        );
    }

    #[test]
    fn auto_transport_off_or_held_leaves_it_alone() {
        let mut a = AutoTransport::default();
        a.set_on(false);
        let mut t = Transport::default();
        let a_on = [dt("A", true, Some(140.0)), dt("B", false, Some(128.0))];
        assert!(run(&mut a, &a_on, 0.0, &mut t, 0.0).is_empty());
        // turned on with A already playing: starts
        a.set_on(true);
        assert_eq!(
            run(&mut a, &a_on, 0.0, &mut t, 1.0),
            [AutoStep::Lock("A".into()), AutoStep::Start]
        );
        // a hand: STOP (the host pauses the decks) holds auto off
        a.hold(2.0);
        t.running = false;
        let idle = [dt("A", false, Some(140.0)), dt("B", false, Some(128.0))];
        assert!(run(&mut a, &idle, 0.0, &mut t, 2.1).is_empty());
        // a manual lock to B while both play is kept, whatever the crossfader does
        let both = [dt("A", true, Some(140.0)), dt("B", true, Some(140.0))];
        t.running = true;
        t.lock = Some("B".into());
        a.hold(2.2);
        // START's own plays (within the grace) do not release the hold
        assert!(run(&mut a, &both, 0.0, &mut t, 2.3).is_empty());
        assert!(run(&mut a, &both, 0.0, &mut t, 5.0).is_empty());
        assert_eq!(t.lock.as_deref(), Some("B"));
        // the next deck play releases it: A stops, then plays again
        let b_on = [dt("A", false, Some(140.0)), dt("B", true, Some(140.0))];
        assert!(run(&mut a, &b_on, 0.0, &mut t, 6.0).is_empty());
        assert!(a.held);
        assert_eq!(run(&mut a, &both, 0.0, &mut t, 7.0), [AutoStep::Lock("A".into())]);
        assert!(!a.held);
    }

    #[test]
    fn tempo_gap_takes_half_and_double_time() {
        assert!((tempo_gap(126.0, 128.0) + 2.0).abs() < 1e-12);
        assert!(tempo_gap(64.0, 128.0).abs() < 1e-12);
        assert!(tempo_gap(256.0, 128.0).abs() < 1e-12);
        assert_eq!(tempo_gap(0.0, 128.0), 0.0);
    }

    #[test]
    fn tap_tempo() {
        let mut t = TapTempo::default();
        assert_eq!(t.tap(10.0), None);
        let b = t.tap(10.5).unwrap();
        assert!((b - 120.0).abs() < 1e-9);
        for k in 2..10 {
            t.tap(10.0 + 0.5 * k as f64);
        }
        assert_eq!(t.taps(), TapTempo::KEEP + 1);
        assert!((t.bpm().unwrap() - 120.0).abs() < 1e-9);
        // a long pause starts over
        assert_eq!(t.tap(30.0), None);
        assert!((t.tap(30.4).unwrap() - 150.0).abs() < 1e-9);
    }
}
