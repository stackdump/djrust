//! Swift goldens for what a deck plays a track with (`tools/paritydump/options.swift`, section
//! "options", `crates/djengine/tests/fixtures/swift/options.json`): `PreparedTrack` built for a
//! matrix of (genre, seed, structure, palette, role filter, stinger content, start tick) and a
//! raw-`nets` share, and the audio a `WaveDeckSource` renders from it.
//!
//! Each case is rebuilt here through [`PreparedTrack::compose`] (or `from_raw_nets`) on a
//! [`Preparer`] of the app's own engine (`EngineConfig::new`: `WaveOptions::extended`), and
//! rendered through a [`DeckCore`] (the deck's audio side: runner, master chain, limiter; the
//! strip's EQ and filter are Audio Units in Swift and are not in either). What is compared:
//!
//! - **exact**: the options the track was prepared with (musicality bits, master, genre, seed,
//!   role filter, duck depth, room, echo, the feel's family, brushes, sub-octave, HF tilt, voice
//!   design: every `f64` by its bits), the fixes and whether the pass ran, the sha256 of the
//!   composed and of the played project, the curated Hits (`dj_state` json) and what each Hit's
//!   net plays, and what `prepare` measured: the loudness gain, the balance trims, the energy
//!   curve, both rest schedules, the voice design, and one row per lane (its sampler, level,
//!   pan, spread, voice fixes, sends, rest class, section-harmony group, choke);
//! - **measured**: what `prepare` measured by rendering probes of the parts (the loudness gain,
//!   the energy curve's gains, the balance trims and, with them, the lanes' levels) is a sum of
//!   squares over audio, so Apple's `libm` and the `libm` crate (a last-bit difference in a
//!   `sin` / `exp` / `pow`) move it by about a part in 1e11 at the worst (measured over the
//!   matrix: gain 1.1e-12, energy gain 1.5e-11, trims 7.5e-13, lane levels 4.4e-13, a cutoff by
//!   1 ulp): those are compared by [`MEASURED`] (relative), every other number by its bits;
//! - **audio**: the sha256 of every float32 rendered, and where it is not the same bits (the
//!   measured gain above differs in its last digits, so a float32 sample flips now and then),
//!   every block's peak and energy within [`BLOCK_TOLERANCE`] of the case's own, and the peak,
//!   the RMS, the integrated loudness and the true peak within [`TOLERANCE`];
//! - **jumps**: `Deck.sameSongOptions` over a deck holding the track (the options it prepares
//!   a section jump with, every field a measure rides in) and the project it prepares from
//!   each of the first three sections.
//!
//! The file is written on a Mac and committed; the test panics, naming the `regen.sh` command,
//! when it is missing (`common_fixture`).

mod common_fixture;

use std::sync::Arc;

use djrust::cid::{hex, sha256};
use djrust::json::{JsonObject, JsonValue};
use djrust::musicality::echo::EchoSpec;
use djrust::musicality::energy_curve::EnergyCurve;
use djrust::musicality::loudness::{IntegratedLoudness, TruePeak};
use djrust::musicality::space::SpaceSpec;
use djrust::musicality::synth_voice::VoiceDesign;
use djrust::sound::Installed;
use djrust::wave::rests::RestSchedule;
use djrust::wave::{Master, RoleFilter, StingerPolicy, WaveOptions, WaveProject};
use djrust::TrackSpec;
use djrust::host::deck::{engine, Command, DeckCore, DeckMirror, EngineConfig, PreparedTrack, Preparer};

/// What `prepare` measured, relative (see the header): the worst seen is 1.5e-11, this is 70x it.
const MEASURED: f64 = 1e-9;
/// A render that is not bit-identical: the peak and the RMS within this of themselves,
/// relative; the loudness and the true peak within `TOLERANCE_DB`.
const TOLERANCE: f64 = 1e-6;
const TOLERANCE_DB: f64 = 1e-6;
/// Every block of such a render: its peak within this of the case's peak, its energy of the
/// case's mean block energy (a flipped float32 sample is 6e-8 of itself).
const BLOCK_TOLERANCE: f64 = 1e-5;

fn golden() -> JsonValue {
    common_fixture::swift_fixture("options")
}

// ---- the rows, as the dump writes them -------------------------------------------------

fn hex64(d: f64) -> String {
    format!("{:016x}", d.to_bits())
}

fn s(x: impl Into<String>) -> JsonValue {
    JsonValue::str(x)
}

fn opt(x: Option<&str>) -> JsonValue {
    x.map_or(JsonValue::Null, JsonValue::str)
}

fn bits(xs: &[f64]) -> JsonValue {
    JsonValue::Array(xs.iter().map(|x| s(hex64(*x))).collect())
}

fn obj(pairs: Vec<(&str, JsonValue)>) -> JsonValue {
    JsonValue::Object(JsonObject::from_pairs(pairs))
}

fn int(x: i64) -> JsonValue {
    JsonValue::int(x)
}

fn space_row(x: Option<SpaceSpec>) -> JsonValue {
    x.map_or(JsonValue::Null, |x| {
        bits(&[x.seconds, x.wet, x.damping, x.shimmer])
    })
}

fn echo_row(x: Option<EchoSpec>) -> JsonValue {
    x.map_or(JsonValue::Null, |x| {
        bits(&[x.steps, x.feedback, x.wet, x.damping])
    })
}

fn design_row(x: &VoiceDesign) -> JsonValue {
    obj(vec![
        (
            "pluck",
            JsonValue::Array(x.pluck_groups.iter().map(|g| s(g.clone())).collect()),
        ),
        ("padWidth", s(hex64(x.pad_width))),
        ("padSideHz", s(hex64(x.pad_side_hz))),
    ])
}

fn rest_row(r: &RestSchedule) -> JsonValue {
    JsonValue::Array(
        r.entries
            .iter()
            .map(|e| JsonValue::Array(vec![int(e.from), int(e.to), int(e.mask.0 as i64)]))
            .collect(),
    )
}

fn energy_row(e: Option<&EnergyCurve>) -> JsonValue {
    let Some(e) = e else { return JsonValue::Null };
    obj(vec![
        (
            "start",
            JsonValue::Array(e.start.iter().map(|x| int(*x)).collect()),
        ),
        ("gain", bits(&e.gain)),
        ("cutoff", bits(&e.cutoff)),
        (
            "ramp",
            JsonValue::Array(e.ramp.iter().map(|x| JsonValue::Bool(*x)).collect()),
        ),
    ])
}

fn sorted(v: &[String]) -> JsonValue {
    let mut v = v.to_vec();
    v.sort();
    JsonValue::Array(v.into_iter().map(s).collect())
}

fn filter_row(f: Option<&RoleFilter>) -> JsonValue {
    let Some(f) = f else { return JsonValue::Null };
    obj(vec![
        ("mute", sorted(&f.mute)),
        (
            "solo",
            f.solo.as_ref().map_or(JsonValue::Null, |v| sorted(v)),
        ),
    ])
}

fn options_row(x: &WaveOptions) -> JsonValue {
    obj(vec![
        ("musicality", s(format!("{:x}", x.musicality.0))),
        (
            "master",
            s(match x.master {
                Master::Soft => "soft",
                Master::Linear => "linear",
                Master::Limit => "limit",
            }),
        ),
        ("masterGain", s(hex64(x.master_gain))),
        ("instruments", JsonValue::Bool(x.instruments.is_some())),
        ("genre", opt(x.genre.as_deref())),
        ("voicingSeed", s(hex64(x.voicing_seed))),
        ("roleFilter", filter_row(x.role_filter.as_ref())),
        ("turntableBrake", JsonValue::Bool(x.turntable_brake)),
        ("duckDepth", s(hex64(x.duck_depth))),
        ("space", space_row(x.space)),
        ("echo", echo_row(x.echo)),
        ("feel", opt(x.feel.as_ref().map(|f| f.family.as_str()))),
        ("brushes", JsonValue::Bool(x.brushes)),
        ("subOctave", JsonValue::Bool(x.sub_octave)),
        (
            "hfTilt",
            x.hf_tilt
                .map_or(JsonValue::Null, |t| bits(&[t.hat_hz, t.crash_hz, t.q])),
        ),
        ("voiceDesign", design_row(&x.voice_design)),
        (
            "trackGain",
            x.track_gain.map_or(JsonValue::Null, |g| s(hex64(g))),
        ),
        ("trackEnergy", energy_row(x.track_energy.as_ref())),
        ("trackBalance", balance_row(x.track_balance.as_ref())),
        (
            "trackRests",
            x.rests.as_ref().map_or(JsonValue::Null, rest_row),
        ),
        (
            "tail",
            x.tail.map_or(JsonValue::Null, |t| {
                JsonValue::Array(vec![int(t.from_tick), int(t.bars)])
            }),
        ),
    ])
}

fn balance_row(b: Option<&std::collections::BTreeMap<String, f64>>) -> JsonValue {
    b.map_or(JsonValue::Null, |b| {
        JsonValue::Object(JsonObject::from_pairs(
            b.iter().map(|(k, v)| (k.clone(), s(hex64(*v)))),
        ))
    })
}

fn lane_row(l: &djrust::wave::Lane) -> JsonValue {
    obj(vec![
        ("net", s(l.net_id.clone())),
        ("kind", s(l.kind.clone())),
        ("channel", int(l.channel)),
        ("instrument", s(l.instrument.clone())),
        ("sampler", int(l.sample_slot().map_or(-1, |x| x as i64))),
        ("level", s(hex64(l.level()))),
        ("pan", s(hex64(l.placement().0))),
        ("spread", bits(l.voice_spread())),
        ("kitVelocity", s(hex64(l.kit().2))),
        ("tilt", JsonValue::Bool(l.tilt_on())),
        ("subOctave", JsonValue::Bool(l.has_sub_octave())),
        ("pluck", JsonValue::Bool(l.is_pluck())),
        ("mip", JsonValue::Bool(l.mip_on())),
        (
            "unison",
            l.unison_pair()
                .map_or(JsonValue::Null, |(d, u)| bits(&[d, u])),
        ),
        ("filter", {
            let (a, b, m) = l.filter_envelope();
            bits(&[a, b, m])
        }),
        ("ducks", JsonValue::Bool(l.ducks())),
        ("isKick", JsonValue::Bool(l.keys_the_duck())),
        ("send", s(hex64(l.send()))),
        ("shimmer", s(hex64(l.shimmer_send()))),
        ("echo", s(hex64(l.echo_send()))),
        ("rest", int(l.rest_class().0 as i64)),
        ("harmonyGroup", int(l.harmony_group())),
        ("choke", s(hex64(l.choke_mul()))),
    ])
}

/// `like`: the song's own project, for a row of another start of it (a restart, a section jump):
/// its lanes are not written again, only whether they are the song's, row for row.
fn prepared_row(p: &WaveProject, like: Option<&WaveProject>) -> JsonValue {
    let balance = balance_row(p.balance.as_ref());
    let lanes_of =
        |p: &WaveProject| JsonValue::Array(p.lanes().iter().map(lane_row).collect::<Vec<_>>());
    let lanes = match like {
        Some(song) => (
            "lanesLikeTheSong",
            JsonValue::Bool(lanes_of(p).js_stringified() == lanes_of(song).js_stringified()),
        ),
        None => ("lanes", lanes_of(p)),
    };
    obj(vec![
        ("startTick", int(p.start_tick)),
        ("gain", s(hex64(p.gain))),
        ("duckDepth", s(hex64(p.duck_depth))),
        ("space", space_row(p.space)),
        ("echo", echo_row(p.echo)),
        ("balance", balance),
        ("energy", energy_row(p.energy.as_ref())),
        ("songRests", rest_row(&p.song_rests)),
        ("rests", rest_row(&p.rests)),
        ("tail", JsonValue::Bool(p.tail.is_some())),
        ("feel", opt(p.feel.as_ref().map(|f| f.family.as_str()))),
        // (`WaveProject.voiceDesign`, which this port's project does not keep: the one the
        // track was prepared with, `PreparedTrack::opts`)
        ("voiceDesign", JsonValue::Null),
        ("harmony", JsonValue::Bool(p.harmony.is_some())),
        (
            "drift",
            p.drift.map_or(JsonValue::Null, |d| s(format!("{d:x}"))),
        ),
        ("turntableBrake", JsonValue::Bool(p.turntable_brake)),
        ("riser", JsonValue::Bool(p.riser.is_some())),
        lanes,
    ])
}

struct Rendered {
    frames: usize,
    hash: String,
    peak: f32,
    rms: f64,
    lufs: Option<f64>,
    true_peak: f64,
    /// Each block's loudest sample and its energy.
    blocks: Vec<(f32, f64)>,
}

/// A render through a `DeckCore` on the app's own options, from the project's start.
fn render(project: WaveProject, rate: f64, seconds: f64) -> Rendered {
    let opts = WaveOptions::extended();
    let mirror = Arc::new(DeckMirror::new());
    let mut core = DeckCore::new("A", rate, opts.clone(), opts.turntable_brake, mirror);
    core.apply(Command::Load(project));
    core.apply(Command::Play);
    let block = 128;
    let blocks = (seconds * rate / block as f64).ceil() as usize;
    let (mut l, mut r) = (vec![0f32; block], vec![0f32; block]);
    let (mut left, mut right) = (Vec::new(), Vec::new());
    let mut per_block = Vec::with_capacity(blocks);
    for k in 0..blocks {
        core.render(&mut l, &mut r, (k * block) as i64);
        left.extend_from_slice(&l);
        right.extend_from_slice(&r);
        let mut pk = 0f32;
        let mut en = 0f64;
        for i in 0..block {
            pk = pk.max(l[i].abs()).max(r[i].abs());
            en += l[i] as f64 * l[i] as f64 + r[i] as f64 * r[i] as f64;
        }
        per_block.push((pk, en));
    }
    let mut peak = 0f32;
    let mut sum = 0f64;
    for k in 0..left.len() {
        peak = peak.max(left[k].abs()).max(right[k].abs());
        sum += left[k] as f64 * left[k] as f64 + right[k] as f64 * right[k] as f64;
    }
    let mut bytes = Vec::with_capacity(left.len() * 8);
    for x in left.iter().chain(&right) {
        bytes.extend_from_slice(&x.to_bits().to_le_bytes());
    }
    Rendered {
        frames: left.len(),
        hash: hex(&sha256(&bytes)),
        peak,
        rms: (sum / (2 * left.len()) as f64).sqrt(),
        lufs: IntegratedLoudness::integrated(&left, Some(&right), rate),
        true_peak: TruePeak::dbtp(&left, Some(&right)),
        blocks: per_block,
    }
}

// ---- comparing -------------------------------------------------------------------------

/// A number the dump wrote as the 16 hex digits of an `f64`.
fn as_bits(v: &JsonValue) -> Option<f64> {
    let t = v.as_str()?;
    (t.len() == 16 && t.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| f64::from_bits(u64::from_str_radix(t, 16).unwrap()))
}

/// Is this path something `prepare` measured by rendering (compared by [`MEASURED`])?
fn measured(path: &str) -> bool {
    let p = path.rsplit_once('.').map_or(path, |(_, last)| last);
    path.ends_with(".gain")
        || path.contains(".energy.gain[")
        || path.contains(".energy.cutoff[")
        || path.contains(".balance.")
        || path.contains("trackBalance.")
        || path.contains("trackEnergy.gain[")
        || path.contains("trackEnergy.cutoff[")
        || path.ends_with("trackGain")
        || p.starts_with("level")
        || p.starts_with("kitVelocity")
}

/// Every place `got` differs from `want`, by path (objects key by key, arrays element by
/// element: a lane's field is named). The measured numbers are within [`MEASURED`]; `worst`
/// keeps the largest relative difference seen among them.
fn diff(path: &str, got: &JsonValue, want: &JsonValue, out: &mut Vec<String>, worst: &mut f64) {
    match (got, want) {
        (JsonValue::Object(g), JsonValue::Object(w)) => {
            for (k, wv) in w.iter() {
                match g.get(k) {
                    Some(gv) => diff(&format!("{path}.{k}"), gv, wv, out, worst),
                    None => out.push(format!("{path}.{k}: missing")),
                }
            }
        }
        (JsonValue::Array(g), JsonValue::Array(w)) => {
            if g.len() != w.len() {
                out.push(format!("{path}: {} entries, Swift {}", g.len(), w.len()));
                return;
            }
            for (k, (gv, wv)) in g.iter().zip(w).enumerate() {
                diff(&format!("{path}[{k}]"), gv, wv, out, worst);
            }
        }
        _ => {
            if got.canonical_js() == want.canonical_js() {
                return;
            }
            if let (true, Some(a), Some(b)) = (measured(path), as_bits(got), as_bits(want)) {
                let rel = (a - b).abs() / a.abs().max(b.abs()).max(1e-300);
                *worst = worst.max(rel);
                if rel <= MEASURED {
                    return;
                }
            }
            out.push(format!(
                "{path}: {} Swift {}",
                got.canonical_js(),
                want.canonical_js()
            ));
        }
    }
}

fn num(v: &JsonValue, k: &str) -> f64 {
    v.get(k)
        .and_then(JsonValue::as_f64)
        .unwrap_or_else(|| panic!("no number {k}"))
}

fn text<'a>(v: &'a JsonValue, k: &str) -> &'a str {
    v.get(k)
        .and_then(JsonValue::as_str)
        .unwrap_or_else(|| panic!("no string {k}"))
}

fn from_bits(h: &str) -> f64 {
    f64::from_bits(u64::from_str_radix(h, 16).unwrap())
}

/// The render against Swift's: the same bits, else every block and the figures within
/// tolerance. Returns whether it was bit-identical; `worst` keeps the largest block difference
/// (relative to the case's own peak / mean block energy) and the loudness figures' deltas.
fn compare_render(
    label: &str,
    got: &Rendered,
    want: &JsonValue,
    problems: &mut Vec<String>,
    worst: &mut [f64; 5],
) -> bool {
    assert_eq!(got.frames as f64, num(want, "frames"), "{label}: frames");
    if got.hash == text(want, "hash") {
        return true;
    }
    let peak = f32::from_bits(u32::from_str_radix(text(want, "peak"), 16).unwrap()) as f64;
    let rms = from_bits(text(want, "rms"));
    let peak_rel = (got.peak as f64 - peak).abs() / peak.max(1e-9);
    let rms_rel = (got.rms - rms).abs() / rms.max(1e-9);
    worst[0] = worst[0].max(peak_rel);
    worst[1] = worst[1].max(rms_rel);
    if peak_rel > TOLERANCE {
        problems.push(format!("{label}: peak {} Swift {peak}", got.peak));
    }
    if rms_rel > TOLERANCE {
        problems.push(format!("{label}: rms {} Swift {rms}", got.rms));
    }
    match (got.lufs, want.get("lufs").and_then(JsonValue::as_str)) {
        (Some(g), Some(w)) => {
            worst[2] = worst[2].max((g - from_bits(w)).abs());
            if (g - from_bits(w)).abs() > TOLERANCE_DB {
                problems.push(format!("{label}: lufs {g} Swift {}", from_bits(w)));
            }
        }
        (None, None) => {}
        (g, w) => problems.push(format!("{label}: lufs {g:?} Swift {w:?}")),
    }
    let tp = from_bits(text(want, "truePeak"));
    worst[3] = worst[3].max((got.true_peak - tp).abs());
    if (got.true_peak - tp).abs() > TOLERANCE_DB {
        problems.push(format!("{label}: true peak {} Swift {tp}", got.true_peak));
    }
    // every block (the dump writes them for the song's own render)
    let Some(blocks) = want.get("blocks").and_then(JsonValue::as_array) else {
        return false;
    };
    assert_eq!(blocks.len(), got.blocks.len(), "{label}: blocks");
    let mean_energy: f64 = blocks
        .iter()
        .map(|b| from_bits(b.at(1).and_then(JsonValue::as_str).unwrap()))
        .sum::<f64>()
        / blocks.len() as f64;
    let mut bad = 0;
    for (k, (b, (gp, ge))) in blocks.iter().zip(&got.blocks).enumerate() {
        let wp = f32::from_bits(
            u32::from_str_radix(b.at(0).and_then(JsonValue::as_str).unwrap(), 16).unwrap(),
        ) as f64;
        let we = from_bits(b.at(1).and_then(JsonValue::as_str).unwrap());
        let dp = (*gp as f64 - wp).abs() / peak.max(1e-9);
        let de = (ge - we).abs() / mean_energy.max(1e-30);
        worst[4] = worst[4].max(dp).max(de);
        if dp > BLOCK_TOLERANCE || de > BLOCK_TOLERANCE {
            bad += 1;
            if bad <= 3 {
                problems.push(format!(
                    "{label}: block {k}: peak {gp} Swift {wp}, energy {ge} Swift {we}"
                ));
            }
        }
    }
    false
}

fn strings(v: &JsonValue, k: &str) -> Vec<String> {
    v.get(k).and_then(JsonValue::strings).unwrap_or_default()
}

/// One case: the track, as the dump built it.
fn track_of(case: &JsonValue, preparer: &Preparer) -> PreparedTrack {
    let start = num(case, "startTick") as i64;
    if let Some(payload) = case.get("rawNets") {
        return PreparedTrack::from_raw_nets(payload, preparer).expect("a raw-nets track");
    }
    let structure = case
        .get("structure")
        .and_then(JsonValue::as_str)
        .map(String::from);
    let mut spec = TrackSpec::new(text(case, "genre"), num(case, "seed"), structure.as_deref());
    if let Some(p) = case.get("palette").and_then(JsonValue::as_str) {
        spec = spec.with_palette(Some(p), &Installed::nothing(""));
    }
    PreparedTrack::compose(&spec, preparer, start)
}

/// Everything the dump says about `prepared` and its options, compared; the label names the
/// row (`prepared`, `restarted`, `jump 1`).
fn compare_prepared(
    label: &str,
    prepared: &JsonValue,
    got: &WaveProject,
    voice_design: &VoiceDesign,
    like: Option<&WaveProject>,
    found: &mut Vec<String>,
    worst: &mut f64,
) {
    let mut row = prepared_row(got, like);
    // (the voice design of a prepared project is the one it was prepared with: this port's
    // `WaveProject` does not keep it)
    if let JsonValue::Object(o) = &mut row {
        o.set("voiceDesign", design_row(voice_design));
    }
    diff(label, &row, prepared, found, worst);
}

#[test]
fn a_track_plays_with_the_options_swift_prepares_it_with() {
    let doc = golden();
    let mut identical = 0;
    let mut renders = 0;
    let mut cases = 0;
    let mut jumps = 0;
    let mut problems: Vec<String> = Vec::new();
    let mut worst_measured = 0f64;
    let mut worst_audio = [0f64; 5];
    for row in doc.get("cases").and_then(JsonValue::as_array).unwrap() {
        let case = row.get("case").unwrap();
        let name = text(case, "name").to_string();
        let rate = num(case, "rate");
        let seconds = num(case, "seconds");
        cases += 1;
        let (mut e, _r) = engine(EngineConfig::new(rate));
        let mut preparer = e.preparer("A").unwrap();
        preparer.opts.stingers = Some(match text(case, "stingers") {
            "nothing" => StingerPolicy::Nothing,
            _ => StingerPolicy::Everything,
        });
        let (mute, solo) = (strings(case, "mute"), case.get("solo"));
        let filter = if !mute.is_empty() || solo.is_some_and(|v| !matches!(v, JsonValue::Null)) {
            Some(RoleFilter {
                mute,
                solo: solo.and_then(JsonValue::strings),
            })
        } else {
            None
        };
        if filter.is_some() {
            preparer = preparer.with_role_filter(filter.clone());
        }
        let track = track_of(case, &preparer);
        let mut found: Vec<String> = Vec::new();
        let mut worst = 0f64;
        let (opts, none) = (&track.opts, &mut worst);
        diff(
            "opts",
            &options_row(opts),
            row.get("opts").unwrap(),
            &mut found,
            none,
        );
        diff(
            "fixes",
            &s(format!("{:x}", track.fixes.0)),
            row.get("fixes").unwrap(),
            &mut found,
            none,
        );
        diff(
            "passApplied",
            &JsonValue::Bool(track.pass_applied),
            row.get("passApplied").unwrap(),
            &mut found,
            none,
        );
        let sha = |t: String| hex(&sha256(t.as_bytes()));
        diff(
            "projectSha",
            &s(sha(track.project.canonical_json())),
            row.get("projectSha").unwrap(),
            &mut found,
            none,
        );
        diff(
            "playedSha",
            &s(sha(track.played.canonical_json())),
            row.get("playedSha").unwrap(),
            &mut found,
            none,
        );
        let hits: Vec<JsonValue> = track
            .played
            .stingers
            .iter()
            .map(|a| s(a.json().js_stringified()))
            .collect();
        diff(
            "stingers",
            &JsonValue::Array(hits),
            row.get("stingers").unwrap(),
            &mut found,
            none,
        );
        let curated: Vec<JsonValue> = ["hit1", "hit2", "hit3", "hit4"]
            .iter()
            .map(|h| {
                let nb = track.played.nets.get(h);
                JsonValue::Array(vec![
                    s(*h),
                    opt(nb.and_then(|n| n.track.sound.curated.as_deref())),
                    opt(nb.and_then(|n| n.track.sound.option.as_deref())),
                    opt(nb.map(|n| n.track.instrument.as_str())),
                ])
            })
            .collect();
        diff(
            "curated",
            &JsonValue::Array(curated),
            row.get("curated").unwrap(),
            &mut found,
            none,
        );
        compare_prepared(
            "prepared",
            row.get("prepared").unwrap(),
            track.prepared.wave(),
            &track.opts.voice_design,
            None,
            &mut found,
            &mut worst,
        );
        // restarted (the song measured once, played from another bar)
        let restarted = row.get("restarted").map(|r| {
            let t = track.restarted(&preparer, num(r, "tick") as i64);
            compare_prepared(
                "restarted",
                r.get("prepared").unwrap(),
                t.prepared.wave(),
                &t.opts.voice_design,
                Some(track.prepared.wave()),
                &mut found,
                &mut worst,
            );
            (t.prepared.project, r.get("render").unwrap())
        });
        // section jumps: `Deck.sameSongOptions` over a deck that holds this track
        if let Some(rows) = row.get("jumps").and_then(JsonValue::as_array) {
            if let Some(f) = &filter {
                // (the deck's role filter, as `Deck.roleFilter` holds it)
                assert!(e.generate_filtered("A", &TrackSpec::new("techno", 1.0, None), Some(f)));
            }
            let held = track_of(case, &preparer);
            assert!(e.load_net("A", held));
            for r in rows {
                jumps += 1;
                let tick = num(r, "tick") as i64;
                let req = e.move_request("A", tick, None, None).unwrap();
                let (mut f2, mut none2) = (Vec::new(), 0f64);
                diff(
                    "opts",
                    &options_row(&req.opts),
                    r.get("opts").unwrap(),
                    &mut f2,
                    &mut none2,
                );
                let jumped = req.prepare();
                compare_prepared(
                    "prepared",
                    r.get("prepared").unwrap(),
                    jumped.wave(),
                    &req.opts.voice_design,
                    Some(track.prepared.wave()),
                    &mut f2,
                    &mut worst,
                );
                let same_song = jumped.wave().track_serial == req.track_serial.unwrap();
                diff(
                    "sameSong",
                    &JsonValue::Bool(same_song),
                    r.get("sameSong").unwrap(),
                    &mut f2,
                    &mut none2,
                );
                for f in f2 {
                    found.push(format!("jump {}: {f}", num(r, "section")));
                }
            }
        }
        worst_measured = worst_measured.max(worst);
        for f in found {
            problems.push(format!("{name}: {f}"));
        }
        // the audio
        let want = row.get("render").unwrap();
        let got = render(track.prepared.project, rate, seconds);
        renders += 1;
        if compare_render(
            &format!("{name}: render"),
            &got,
            want,
            &mut problems,
            &mut worst_audio,
        ) {
            identical += 1;
        }
        if let Some((project, want)) = restarted {
            let got = render(project, rate, seconds);
            renders += 1;
            if compare_render(
                &format!("{name}: restarted render"),
                &got,
                want,
                &mut problems,
                &mut worst_audio,
            ) {
                identical += 1;
            }
        }
    }
    eprintln!(
        "{identical} of {renders} renders bit-identical over {cases} cases and {jumps} jumps; \
         worst measured {worst_measured:e} relative; the others: peak {:e}, rms {:e}, lufs {:e} dB, \
         true peak {:e} dB, a block {:e}",
        worst_audio[0], worst_audio[1], worst_audio[2], worst_audio[3], worst_audio[4]
    );
    assert!(
        problems.is_empty(),
        "{} problems:\n{}",
        problems.len(),
        problems
            .iter()
            .take(60)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}
