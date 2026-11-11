//! Swift goldens for `ClipRetune.render` (`tools/paritydump/retune.swift`, section "retune",
//! `crates/djengine/tests/fixtures/swift/retune.json`): a net take played again from its
//! provenance with other instruments on some channels.
//!
//! Every case is a clip given as data (the take's provenance with its share, tempo, roles off and
//! macros; its first beat, length and tempo) and the voices asked for. It is rebuilt here from
//! that data and rendered by [`djrust::host::deck::retune::render`]. What is compared:
//!
//! - **exact**: the outcome (the refusal Swift gave, or `ok`), the length of the buffer in frames
//!   and the share the render played (`Retuned.source`: its canonical text, by sha256);
//! - **audio**: the sha256 of each channel's float32 samples. Where it is not the same bits (what
//!   `prepare` measures by rendering probes, the loudness gain and the energy curve, is a sum of
//!   squares over audio, so Apple's `libm` and the `libm` crate move it by a part in 1e11 and a
//!   float32 sample now and then flips with it), every 4096-frame block's peak and energy within
//!   [`BLOCK_TOLERANCE`] of the case's own. Measured over the file (the test prints it): 4 of 7
//!   audio cases are not the same bits, worst block 1.3e-7, the tolerance is 1e-6.
//!
//! The file is written on a Mac and committed; the test panics, naming the `regen.sh` command,
//! when it is missing (`common_fixture`).

mod common_fixture;

use std::collections::BTreeMap;

use djrust::cid::{hex, sha256};
use djrust::deck::clips::LoopClip;
use djrust::deck::loops::Refusal;
use djrust::deck::turntable::SampleBuffer;
use djrust::deck::waveform::WaveformOverview;
use djrust::json::JsonValue;
use djrust::host::deck::retune::{render, RetuneEnv};

/// A render that is not bit-identical: every block's peak within this of the case's own peak,
/// its energy of the case's mean block energy (a flipped float32 sample is 6e-8 of itself).
const BLOCK_TOLERANCE: f64 = 1e-6;
const BLOCK: usize = 4096;

fn golden() -> JsonValue {
    common_fixture::swift_fixture("retune")
}

fn float_hash(x: &[f32]) -> String {
    let mut bytes = Vec::with_capacity(x.len() * 4);
    for f in x {
        bytes.extend_from_slice(&f.to_bits().to_le_bytes());
    }
    hex(&sha256(&bytes))
}

fn bits(s: &str) -> f64 {
    f64::from_bits(u64::from_str_radix(s, 16).unwrap())
}

fn text<'a>(v: &'a JsonValue, k: &str) -> &'a str {
    v.get(k)
        .and_then(JsonValue::as_str)
        .unwrap_or_else(|| panic!("no string {k}"))
}

fn refusal(e: &Refusal) -> String {
    match e {
        Refusal::NotANet => "notANet".into(),
        Refusal::Sequence => "sequence".into(),
        Refusal::Macros(m) => format!("macros:{}", m.join(",")),
        Refusal::NoProvenance => "noProvenance".into(),
        Refusal::SongEnded => "songEnded".into(),
        Refusal::TooFar => "tooFar".into(),
    }
}

/// The peak and the sum of squares (both channels) of each 4096-frame block.
fn blocks(l: &[f32], r: &[f32]) -> Vec<(f64, f64)> {
    l.chunks(BLOCK)
        .zip(r.chunks(BLOCK))
        .map(|(l, r)| {
            let (mut peak, mut sum) = (0f64, 0f64);
            for (a, b) in l.iter().zip(r) {
                let (a, b) = (*a as f64, *b as f64);
                peak = peak.max(a.abs()).max(b.abs());
                sum += a * a + b * b;
            }
            (peak, sum)
        })
        .collect()
}

#[test]
fn clip_retune_renders_what_swift_renders() {
    let g = golden();
    let rate = g.get("rate").and_then(JsonValue::as_f64).unwrap();
    let env = RetuneEnv::default();
    let cases = g.get("cases").and_then(JsonValue::as_array).unwrap();
    assert!(cases.len() >= 10, "{} cases", cases.len());
    let (mut exact, mut tolerant, mut refused) = (0, 0, 0);
    let mut worst = 0f64;
    for c in cases {
        let name = text(c, "name");
        let beats = c.get("beats").and_then(JsonValue::as_i64).unwrap();
        let bpm = bits(text(c, "bpm"));
        let start_beat = c.get("startBeat").and_then(JsonValue::as_f64).unwrap();
        let provenance = c.get("provenance").filter(|p| !matches!(p, JsonValue::Null));
        let voices: BTreeMap<i64, String> = c
            .get("voices")
            .and_then(JsonValue::as_object)
            .map(|o| {
                o.keys()
                    .map(|k| {
                        (
                            k.parse().unwrap(),
                            o.get(k).and_then(JsonValue::as_str).unwrap().to_string(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let roles_off: Vec<String> = c
            .get("rolesOff")
            .and_then(JsonValue::as_array)
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();
        let zero = vec![0f32; 480];
        let buffer = SampleBuffer::new(zero.clone(), Some(zero), rate);
        let waveform = WaveformOverview::new(&buffer, 4);
        let mut clip = LoopClip::new(
            "case",
            name,
            "A",
            beats,
            bpm,
            start_beat,
            std::sync::Arc::new(buffer),
            waveform,
        );
        clip.provenance = provenance.cloned();

        let outcome = match djrust::host::deck::retune::render_roles_off(&clip, &voices, rate, &env, &roles_off) {
            Ok(r) => {
                // `render` is the same call without the extra roles
                if roles_off.is_empty() {
                    assert!(render(&clip, &voices, rate, &env).is_ok(), "{name}");
                }
                Ok(r)
            }
            Err(e) => Err(e),
        };
        let want = text(c, "outcome");
        match outcome {
            Err(e) => {
                assert_eq!(refusal(&e), want, "{name}: the refusal");
                assert_eq!(text(c, "message"), e.message(), "{name}: the message");
                refused += 1;
            }
            Ok(r) => {
                assert_eq!(want, "ok", "{name}: Swift refused");
                let b = &r.buffer;
                assert_eq!(
                    b.length() as i64,
                    c.get("frames").and_then(JsonValue::as_i64).unwrap(),
                    "{name}: frames"
                );
                assert_eq!(
                    hex(&sha256(r.source.json().canonical_js().as_bytes())),
                    text(c, "sourceSha"),
                    "{name}: the share it played"
                );
                let (lh, rh) = (float_hash(&b.left), float_hash(b.right()));
                if lh == text(c, "left") && rh == text(c, "right") {
                    exact += 1;
                    continue;
                }
                // not the same bits: block by block
                let want_blocks: Vec<(f64, f64)> = c
                    .get("blocks")
                    .and_then(JsonValue::as_array)
                    .unwrap()
                    .iter()
                    .map(|p| {
                        let p = p.as_array().unwrap();
                        (bits(p[0].as_str().unwrap()), bits(p[1].as_str().unwrap()))
                    })
                    .collect();
                let got = blocks(&b.left, b.right());
                assert_eq!(got.len(), want_blocks.len(), "{name}: blocks");
                let peak = want_blocks.iter().map(|b| b.0).fold(0f64, f64::max);
                let mean = want_blocks.iter().map(|b| b.1).sum::<f64>() / want_blocks.len() as f64;
                for (k, (g, w)) in got.iter().zip(&want_blocks).enumerate() {
                    let dp = (g.0 - w.0).abs() / peak;
                    let de = (g.1 - w.1).abs() / mean;
                    worst = worst.max(dp).max(de);
                    assert!(
                        dp <= BLOCK_TOLERANCE && de <= BLOCK_TOLERANCE,
                        "{name}: block {k}: peak {} vs {} ({dp:e}), energy {} vs {} ({de:e})",
                        g.0,
                        w.0,
                        g.1,
                        w.1
                    );
                }
                tolerant += 1;
            }
        }
    }
    println!(
        "retune golden: {} cases: {exact} bit-identical by sha256, {tolerant} within the block tolerance (worst {worst:e}), {refused} refused as Swift refuses",
        cases.len()
    );
}
