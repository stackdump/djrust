//! Port of `Musicality/MixTargets.swift`: the mix targets the musicality fixes aim at
//! (ROADMAP 6.1), as data that names its source: `data/targets.json`. Hand-edited, not
//! generated. Read through lookups over the parsed JSON.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use once_cell::race::OnceBox;

use crate::gen::tables::BeatsTables;
use crate::json::JsonValue;
use crate::musicality::space::SpaceSpec;
use crate::wave::groove::FAMILY_NAMES;

pub const TARGETS_JSON: &str = include_str!("../../data/targets.json");

/// The parts `MixBalance` measures.
pub const MIX_BALANCE_PARTS: [&str; 7] = ["kick", "snare", "hats", "bass", "pad", "lead", "arp"];

/// `LevelMatch.referenceLUFS`.
pub const REFERENCE_LUFS: f64 = -12.0;

pub struct MixTargets {
    root: JsonValue,
}

static SHARED: OnceBox<MixTargets> = OnceBox::new();

/// A `Target`'s `value`.
fn value(t: Option<&JsonValue>) -> Option<f64> {
    t?.get("value")?.as_f64()
}

/// A `Target`'s `band` as a range (`low <= high`), when it has one.
fn range(t: Option<&JsonValue>) -> Option<(f64, f64)> {
    let b = t?.get("band")?.as_array()?;
    if b.len() != 2 {
        return None;
    }
    let (lo, hi) = (b[0].as_f64()?, b[1].as_f64()?);
    if lo <= hi {
        Some((lo, hi))
    } else {
        None
    }
}

impl MixTargets {
    pub const METRICS: [&'static str; 5] = ["lufs", "lra", "truePeak", "slope", "side"];
    /// [RULE-OF-THUMB] M7: a falling slope.
    pub const SLOPE_RULE_OF_THUMB: (f64, f64) = (-6.0, -1.0);
    /// [RULE-OF-THUMB] M7: "no HF bump".
    pub const MAX_HF_BUMP_DB: f64 = 2.0;

    /// `shared`, validated as Swift's `parse` does.
    pub fn shared() -> &'static MixTargets {
        SHARED.get_or_init(|| {
            let root = JsonValue::parse(TARGETS_JSON).expect("targets.json parses");
            let t = MixTargets { root };
            t.validate().expect("targets.json");
            Box::new(t)
        })
    }

    fn validate(&self) -> Result<(), String> {
        let bad = |why: &str| -> String { alloc::format!("targets.json: {why}") };
        let l = self.root.get("loudness");
        if value(l.and_then(|l| l.get("probeOffsetDB"))).is_none() {
            return Err(bad("loudness.probeOffsetDB"));
        }
        let gr = l.and_then(|l| l.get("gainRange"));
        let (gmin, gmax) = (
            gr.and_then(|g| g.get("min")).and_then(|v| v.as_f64()),
            gr.and_then(|g| g.get("max")).and_then(|v| v.as_f64()),
        );
        match (gmin, gmax) {
            (Some(a), Some(b)) if a > 0.0 && a <= b => {}
            _ => return Err(bad("loudness.gainRange")),
        }
        let bal = self.root.get("balance");
        for part in MIX_BALANCE_PARTS {
            if value(bal.and_then(|b| b.get("default")).and_then(|d| d.get(part))).is_none() {
                return Err(bad(&alloc::format!("balance.default.{part}")));
            }
        }
        if value(bal.and_then(|b| b.get("maxTrimDB"))).is_none() {
            return Err(bad("balance.maxTrimDB"));
        }
        let hz = value(self.root.get("hfTilt").and_then(|h| h.get("default")));
        if !hz.map_or(false, |h| h > 0.0) {
            return Err(bad("hfTilt.default"));
        }
        Ok(())
    }

    /// The balance targets for a family (`GrooveTemplate.familyNames`' name): the default,
    /// with the family's overrides.
    pub fn balance_db(&self, family: Option<&str>) -> BTreeMap<String, f64> {
        let mut out = BTreeMap::new();
        let bal = self.root.get("balance");
        if let Some(d) = bal
            .and_then(|b| b.get("default"))
            .and_then(|d| d.as_object())
        {
            for (k, t) in d.iter() {
                if let Some(v) = value(Some(t)) {
                    out.insert(k.into(), v);
                }
            }
        }
        if let Some(f) = family {
            if let Some(o) = bal
                .and_then(|b| b.get("families"))
                .and_then(|x| x.get(f))
                .and_then(|o| o.as_object())
            {
                for (k, t) in o.iter() {
                    if let Some(v) = value(Some(t)) {
                        out.insert(k.into(), v);
                    }
                }
            }
        }
        out
    }

    /// Whether `balance.families` has a block of its own for `family`.
    pub fn has_balance_family(&self, family: &str) -> bool {
        self.root
            .get("balance")
            .and_then(|b| b.get("families"))
            .and_then(|f| f.get(family))
            .is_some()
    }

    /// `balance.maxTrimDB`.
    pub fn max_trim_db(&self) -> f64 {
        value(self.root.get("balance").and_then(|b| b.get("maxTrimDB"))).unwrap_or(0.0)
    }

    /// `loudness.probeOffsetDB`.
    pub fn probe_offset_db(&self) -> f64 {
        value(
            self.root
                .get("loudness")
                .and_then(|l| l.get("probeOffsetDB")),
        )
        .unwrap_or(0.0)
    }

    /// `loudness.gainRange` (min, max), linear.
    pub fn gain_range(&self) -> (f64, f64) {
        let g = self.root.get("loudness").and_then(|l| l.get("gainRange"));
        (
            g.and_then(|g| g.get("min"))
                .and_then(|v| v.as_f64())
                .unwrap_or(1.0),
            g.and_then(|g| g.get("max"))
                .and_then(|v| v.as_f64())
                .unwrap_or(1.0),
        )
    }

    fn room_spec(r: &JsonValue) -> SpaceSpec {
        let f = |k: &str| r.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0);
        SpaceSpec::new(f("seconds"), f("wet"), f("damping"))
    }

    /// `space.default`'s spec: a neutral medium room.
    pub fn default_room(&self) -> SpaceSpec {
        Self::room_spec(
            self.root
                .get("space")
                .and_then(|s| s.get("default"))
                .unwrap(),
        )
    }

    /// A genre's room, else the default one. When its family has a return band and the room
    /// a measured `returnDB`, its `wet` is scaled so the return lands in the band.
    pub fn room(&self, genre: &str) -> SpaceSpec {
        let space = self.root.get("space");
        let r = space
            .and_then(|s| s.get("genres"))
            .and_then(|g| g.get(genre))
            .or_else(|| space.and_then(|s| s.get("default")))
            .unwrap();
        let mut spec = Self::room_spec(r);
        let band = range(
            space
                .and_then(|s| s.get("families"))
                .and_then(|f| f.get(Self::family_name(genre)))
                .and_then(|f| f.get("returnDB")),
        );
        if let (Some((lo, hi)), Some(measured)) = (band, value(r.get("returnDB"))) {
            let to = measured.max(lo).min(hi);
            if to != measured {
                spec.wet *= libm::pow(10.0, (to - measured) / 20.0);
            }
        }
        spec
    }

    /// The pad shimmer's level for a genre's family (`.shimmer`); 0 = none.
    pub fn shimmer(&self, genre: &str) -> f64 {
        value(
            self.root
                .get("space")
                .and_then(|s| s.get("families"))
                .and_then(|f| f.get(Self::family_name(genre)))
                .and_then(|f| f.get("shimmer")),
        )
        .unwrap_or(0.0)
        .max(0.0)
    }

    /// How much of itself a lane sends to the room, from `space.sends`; None when the table
    /// has no entry.
    pub fn send(&self, kind: &str, group: &str) -> Option<f64> {
        let sends = self.root.get("space")?.get("sends")?;
        if kind == "tonal" {
            return value(sends.get(group).or_else(|| sends.get("tonal")));
        }
        value(sends.get(kind))
    }

    /// `space.sends.sampler`.
    pub fn sampler_send(&self) -> Option<f64> {
        value(self.root.get("space")?.get("sends")?.get("sampler"))
    }

    /// `BeatsTables.family(of:)`'s name for a genre.
    pub fn family_name(genre: &str) -> &'static str {
        let i = BeatsTables::shared().family_of(genre);
        if i >= 0 && (i as usize) < FAMILY_NAMES.len() {
            FAMILY_NAMES[i as usize]
        } else {
            FAMILY_NAMES[1]
        }
    }

    /// Where a lane sits: by its drum kind, or, tonal, by its group; centred otherwise.
    pub fn placement(&self, kind: &str, group: &str) -> (f64, f64) {
        let key = if kind == "tonal" { group } else { kind };
        match self.root.get("stereo").and_then(|s| s.get(key)) {
            Some(p) => (
                p.get("pan").and_then(|v| v.as_f64()).unwrap_or(0.0),
                p.get("spread").and_then(|v| v.as_f64()).unwrap_or(0.0),
            ),
            None => (0.0, 0.0),
        }
    }

    /// The band for `metric` in `family`, when one has been measured.
    pub fn band(&self, metric: &str, family: &str) -> Option<(f64, f64)> {
        range(self.root.get("bands")?.get(metric)?.get(family))
    }

    /// The integrated loudness (LUFS) a family's tracks are brought to.
    pub fn integrated_lufs(&self, family: Option<&str>) -> f64 {
        match family.and_then(|f| self.band("lufs", f)) {
            Some((lo, hi)) => (lo + hi) / 2.0,
            None => REFERENCE_LUFS,
        }
    }

    /// `.hfTilt`'s cutoff for `family`, else the default's.
    pub fn hf_tilt_hz(&self, family: &str) -> Option<f64> {
        let h = self.root.get("hfTilt")?;
        value(h.get(family)).or_else(|| value(h.get("default")))
    }

    /// What the mix's long-term slope should fall in for `family`.
    pub fn slope_band(&self, family: &str) -> (f64, f64) {
        self.band("slope", family)
            .unwrap_or(Self::SLOPE_RULE_OF_THUMB)
    }

    /// The reference bands' family names present in `bands[metric]`.
    pub fn band_families(&self, metric: &str) -> Vec<String> {
        self.root
            .get("bands")
            .and_then(|b| b.get(metric))
            .and_then(|m| m.as_object())
            .map(|o| o.iter().map(|(k, _)| String::from(k)).collect())
            .unwrap_or_default()
    }
}
