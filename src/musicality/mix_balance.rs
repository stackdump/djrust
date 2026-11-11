//! Port of `Musicality/MixBalance.swift` (`.balance`): how loud each part of a track is
//! against the whole mix. The pure maths (targets, entry, family, error, the sampled-slot
//! gain) and `trims` (each part soloed through the loudness probe).

use alloc::collections::BTreeMap;
use alloc::string::String;

use crate::gen::tables::BeatsTables;
use crate::musicality::energy_curve::EnergyCurve;
use crate::musicality::fixes::Fixes;
use crate::musicality::loudness::Loudness;
use crate::musicality::mix_targets::{MixTargets, MIX_BALANCE_PARTS, REFERENCE_LUFS};
use crate::musicality::synth_voice::HfTilt;
use crate::pflow::{OrderedMap, Project};
use crate::wave::groove::FAMILY_NAMES;
use crate::wave::runner::{Master, WaveOptions};
use crate::wave::synth::{drum_kind, is_drum_channel};

pub struct MixBalance;

impl MixBalance {
    /// The parts measured: drum kinds by note, tonal parts by group.
    pub const PARTS: [&'static str; 7] = MIX_BALANCE_PARTS;
    /// The rate the parts are measured at for `trims`.
    pub const PROBE_RATE: f64 = 22_050.0;

    /// `part(drumNote:group:)`: the part a drum hit or a tonal net's `group` belongs to.
    pub fn part_of(drum_note: Option<i64>, group: &str) -> Option<&'static str> {
        if let Some(note) = drum_note {
            return match drum_kind(note) {
                "kick" => {
                    if note == 36 || note == 35 {
                        Some("kick")
                    } else {
                        None
                    }
                }
                "snare" | "clap" => Some("snare"),
                "hat" | "openhat" => {
                    if note == 49 || note == 57 {
                        None
                    } else {
                        Some("hats")
                    }
                }
                _ => None,
            };
        }
        match group {
            "bass" => Some("bass"),
            "harmony" | "pad" => Some("pad"),
            "melody" => Some("lead"),
            "arp" => Some("arp"),
            _ => None,
        }
    }

    /// `part(kind:group:)`: the part a lane plays.
    pub fn part_of_kind(kind: &str, group: &str) -> Option<&'static str> {
        match kind {
            "kick" => Some("kick"),
            "snare" | "clap" => Some("snare"),
            "hat" | "openhat" => Some("hats"),
            "tonal" => Self::part_of(None, group),
            _ => None,
        }
    }

    /// `solo`: `project` with only `keep`'s notes left (control nets untouched; fills and
    /// crashes, which are no part's, silent).
    pub fn solo(project: &Project, keep: &str) -> Project {
        let mut p = project.clone();
        for id in p.nets.keys().to_vec() {
            let nb = p.nets.get_mut(&id).unwrap();
            if id.starts_with("fill-") {
                nb.bindings = OrderedMap::new();
                continue;
            }
            if nb.role == "control" {
                continue;
            }
            let group: String = crate::musicality::util::part(nb).into();
            let mut bound = OrderedMap::new();
            for (t, b) in nb.bindings.iter() {
                let ch = if b.channel != 0 {
                    b.channel
                } else {
                    nb.track.channel
                };
                let which = Self::part_of(
                    if is_drum_channel(ch) {
                        Some(b.note)
                    } else {
                        None
                    },
                    &group,
                );
                if which == Some(keep) {
                    bound.insert(t, *b);
                }
            }
            nb.bindings = bound;
        }
        p
    }

    /// `targetDB`: where each part should sit against the others, K-weighted dB
    /// (`targets.json`'s `balance.default`).
    pub fn target_db() -> BTreeMap<String, f64> {
        MixTargets::shared().balance_db(None)
    }

    /// `entry(family:)`: the targets.json balance entry a family plays to.
    pub fn entry(family: Option<&str>) -> String {
        let Some(family) = family else {
            return "default".into();
        };
        if Self::has_block(family) {
            return family.into();
        }
        if family == "jazz" && Self::has_block("chill") {
            return "chill".into();
        }
        "default".into()
    }

    fn has_block(family: &str) -> bool {
        MixTargets::shared().has_balance_family(family)
    }

    /// `targets(family:)`: `target_db` with its entry's parts.
    pub fn targets(family: Option<&str>) -> BTreeMap<String, f64> {
        let e = Self::entry(family);
        if e == "default" {
            Self::target_db()
        } else {
            MixTargets::shared().balance_db(Some(&e))
        }
    }

    /// `family(genre:)`: the family a genre's balance is looked up under; None for no genre.
    pub fn family(genre: Option<&str>) -> Option<&'static str> {
        let genre = genre?;
        let i = BeatsTables::shared().family_of(genre);
        if i >= 0 && (i as usize) < FAMILY_NAMES.len() {
            Some(FAMILY_NAMES[i as usize])
        } else {
            None
        }
    }

    /// `LevelMatch.targetLUFS(genre:)`.
    pub fn target_lufs(genre: Option<&str>) -> f64 {
        let f = Self::family(genre);
        let _ = REFERENCE_LUFS;
        MixTargets::shared().integrated_lufs(f)
    }

    /// A part moves at most this far (`balance.maxTrimDB`).
    pub fn max_trim_db() -> f64 {
        MixTargets::shared().max_trim_db()
    }

    /// `trims(for:fixes:family:hfTilt:)`: the trim (dB) for each part of `project` that puts the
    /// parts in `family`'s targets' relation (`targets`), as near as `maxTrimDB` allows: each part
    /// soloed through the loudness probe (two bars of the busiest section, the voices `fixes`
    /// shape, at `PROBE_RATE`), then moved toward its target around the median of where they all
    /// are (so the mix as a whole moves little; `.loudness` sets its level after). Empty when
    /// fewer than two parts play.
    ///
    /// `hf_tilt`: measure the hats through a tilt (diagnostics). The deck's own trims leave it
    /// None: `.hfTilt` takes the hats' top off and means to leave the rest of them where it was
    /// (trims that heard the tilt lifted the whole hat part back by up to that much and brought
    /// the 12.5 / 16 kHz bump back). The loudness probe does hear the tilt (`Loudness::gain`):
    /// that sets the level, not the hats' share of it.
    pub fn trims(
        project: &Project,
        fixes: Fixes,
        family: Option<&str>,
        hf_tilt: Option<HfTilt>,
    ) -> BTreeMap<String, f64> {
        let target = Self::targets(family);
        let start = Loudness::busiest_start(project);
        let mut level: BTreeMap<&str, f64> = BTreeMap::new();
        let o = WaveOptions {
            musicality: fixes & Loudness::VOICE_FIXES,
            hf_tilt,
            ..Default::default()
        };
        for name in Self::PARTS {
            if let Some(l) = Loudness::measure_with(
                &Self::solo(project, name),
                Some(start),
                Self::PROBE_RATE,
                o.clone(),
            ) {
                level.insert(name, l);
            }
        }
        if level.len() < 2 {
            return BTreeMap::new();
        }
        let mut offsets: alloc::vec::Vec<f64> = level.iter().map(|(k, v)| v - target[*k]).collect();
        offsets.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let centre = offsets[offsets.len() / 2];
        let max = Self::max_trim_db();
        level
            .iter()
            .map(|(k, l)| ((*k).into(), (centre + target[*k] - l).max(-max).min(max)))
            .collect()
    }

    /// `error`: how far a mix is from `family`'s balance, dB: the standard deviation of each
    /// part's level less its target.
    pub fn error(levels: &BTreeMap<String, f64>, family: Option<&str>) -> Option<f64> {
        let target = Self::targets(family);
        let d: alloc::vec::Vec<f64> = levels
            .iter()
            .filter_map(|(k, v)| target.get(k).map(|t| v - t))
            .collect();
        if d.len() < 2 {
            return None;
        }
        let mean = d.iter().sum::<f64>() / d.len() as f64;
        Some(libm::sqrt(
            d.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / d.len() as f64,
        ))
    }
}

/// What `djctl render --balance` reports: the parts' levels against the mix, the genre's
/// family, the targets.json entry it was held to and how far off it is.
#[derive(Clone, Debug, PartialEq)]
pub struct BalanceReport {
    pub levels: BTreeMap<String, f64>,
    pub family: Option<String>,
    pub entry: String,
    pub targets: BTreeMap<String, f64>,
    pub error_db: Option<f64>,
}

impl BalanceReport {
    pub fn new(levels: BTreeMap<String, f64>, genre: Option<&str>) -> BalanceReport {
        let family = MixBalance::family(genre);
        BalanceReport {
            entry: MixBalance::entry(family),
            targets: MixBalance::targets(family),
            error_db: MixBalance::error(&levels, family),
            family: family.map(String::from),
            levels,
        }
    }
}

/// `.balance` on sampled parts (ROADMAP 6.6): a sampler slot plays at a gain of its own.
pub struct SampledBalance;

impl SampledBalance {
    /// A slot moves at most this far (dB) either way.
    pub const RANGE_DB: f64 = 12.0;

    /// The gain (dB) for a slot playing a part trimmed `trim_db` whose instrument sits
    /// `part_level_db` from its synth voice (None: not measured), within ± `RANGE_DB`.
    pub fn gain_db(trim_db: f64, part_level_db: Option<f64>) -> f64 {
        (trim_db - part_level_db.unwrap_or(0.0))
            .max(-Self::RANGE_DB)
            .min(Self::RANGE_DB)
    }

    /// Where that sampler then sits against its trimmed synth voice, linear.
    pub fn probe_level(trim_db: f64, part_level_db: Option<f64>, group: &str, gain_db: f64) -> f64 {
        let level = part_level_db.unwrap_or_else(|| Loudness::sampled_part_db(group));
        libm::pow(10.0, (level + gain_db - trim_db) / 20.0)
    }
}

impl MixBalance {
    /// `soloOptions`: the options every solo of `played` renders with (`measure`,
    /// `stereo_solos`): `opts` through a linear master on synth voices (no bank: this port has no
    /// sampled path here), at one gain (`prepare` would measure the solo's own under
    /// `.loudness`), with the whole track's energy curve and balance trims (measured from the
    /// track, not from a solo, whose would differ), so each part sits at its place in the mix.
    pub fn solo_options(played: &Project, opts: &WaveOptions) -> WaveOptions {
        let mut o = opts.clone();
        o.bank = None;
        o.master = Master::Linear;
        o.track_gain = Some(1.0);
        if o.musicality.contains(Fixes::ENERGY) && o.track_energy.is_none() {
            o.track_energy = EnergyCurve::new(played);
        }
        if o.musicality.contains(Fixes::BALANCE) && o.track_balance.is_none() {
            o.track_balance = Some(Self::trims(
                played,
                o.musicality,
                Self::family(o.genre.as_deref()),
                None,
            ));
        }
        o
    }

    /// `stereoSolos`: stereo renders of the whole mix ("mix") and of each part of `PARTS`
    /// soloed, over `bars` bars of the busiest section of `played`, with `measure`'s options
    /// ([`solo_options`](Self::solo_options)). Parts silent there are left out.
    pub fn stereo_solos(
        played: &Project,
        opts: &crate::wave::runner::WaveOptions,
        bars: i64,
        sample_rate: f64,
    ) -> (
        f64,
        BTreeMap<String, (alloc::vec::Vec<f32>, alloc::vec::Vec<f32>)>,
    ) {
        use crate::wave::runner::WaveRunner;
        let start = Loudness::busiest_start(played);
        let o = Self::solo_options(played, opts);
        let tempo = if played.tempo > 0.0 {
            played.tempo
        } else {
            120.0
        };
        let frames = (bars as f64 * 4.0 * 60.0 / tempo * sample_rate) as usize;
        let render = |p: &Project| -> Option<(alloc::vec::Vec<f32>, alloc::vec::Vec<f32>)> {
            let mut runner = WaveRunner::new(sample_rate, o.clone());
            runner.load_prepared(WaveRunner::prepare(p, sample_rate, &o, start));
            runner.play();
            let mut left = alloc::vec![0.0f32; frames];
            let mut right = alloc::vec![0.0f32; frames];
            let mut n = 0;
            while n < frames {
                let m = 512.min(frames - n);
                runner.process(&mut left[n..n + m], Some(&mut right[n..n + m]), m);
                n += m;
                if runner.stopped() {
                    break;
                }
            }
            let energy: f64 = (0..n)
                .map(|i| (left[i] * left[i] + right[i] * right[i]) as f64)
                .sum();
            if n == 0 || energy <= 1e-12 * n as f64 {
                return None;
            }
            left.truncate(n);
            right.truncate(n);
            Some((left, right))
        };
        let mut out = BTreeMap::new();
        if let Some(r) = render(played) {
            out.insert(String::from("mix"), r);
        }
        for name in Self::PARTS {
            if let Some(r) = render(&Self::solo(played, name)) {
                out.insert(String::from(name), r);
            }
        }
        (sample_rate, out)
    }

    /// `measure`: each part's K-weighted level against the whole mix (dB; 0 = as loud as
    /// everything together), over `bars` bars of the busiest section of `played`, mono,
    /// through a linear master. Parts that do not play there are left out.
    pub fn measure(
        played: &Project,
        opts: &crate::wave::runner::WaveOptions,
        bars: i64,
        sample_rate: f64,
    ) -> BTreeMap<String, f64> {
        use crate::musicality::loudness::KWeighting;
        use crate::wave::runner::WaveRunner;
        let start = Loudness::busiest_start(played);
        let o = Self::solo_options(played, opts);
        let tempo = if played.tempo > 0.0 {
            played.tempo
        } else {
            120.0
        };
        let frames = (bars as f64 * 4.0 * 60.0 / tempo * sample_rate) as usize;
        let level = |p: &Project| -> Option<f64> {
            let mut runner = WaveRunner::new(sample_rate, o.clone());
            runner.load_prepared(WaveRunner::prepare(p, sample_rate, &o, start));
            runner.play();
            let mut buf = alloc::vec![0.0f32; 512];
            let mut k = KWeighting::new(sample_rate);
            let (mut sum, mut n) = (0.0, 0usize);
            while n < frames {
                let m = 512.min(frames - n);
                runner.process(&mut buf[..m], None, m);
                for v in &buf[..m] {
                    let y = k.process(*v as f64);
                    sum += y * y;
                }
                n += m;
                if runner.stopped() {
                    break;
                }
            }
            if n == 0 || sum <= 1e-12 * n as f64 {
                None
            } else {
                Some(10.0 * libm::log10(sum / n as f64))
            }
        };
        let mut out = BTreeMap::new();
        let Some(mix) = level(played) else {
            return out;
        };
        for name in Self::PARTS {
            if let Some(l) = level(&Self::solo(played, name)) {
                out.insert(String::from(name), l - mix);
            }
        }
        out
    }
}
