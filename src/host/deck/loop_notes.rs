//! A loop's notes (beyond Swift; docs/LOOP-ROUTING.md): what the net played in a take, had
//! again by playing the take's bars once more, offline, and keeping the net's fires.
//!
//! The replay is retune's ([`super::retune`]): the same refusals, the same share (with any
//! retune's instruments), the song's own levels and rest schedule, the take's muted roles and
//! tempo, from a little before its first beat. So the notes are the ones heard in the clip:
//! `tests/loop_notes.rs` holds them against the fires the deck itself made while the take was
//! recorded. A clip with no net behind it (a sample deck's, an input's, a file or stock loop)
//! is refused, and is audio only: [`crate::deck::loop_route::no_notes`] says so in words.
//!
//! It plays the bars through a deck source, sound and all (the runner advances as it renders),
//! so it takes a moment: call it off the window's thread.

use std::collections::BTreeMap;

use crate::deck::clips::LoopClip;
use crate::deck::loop_route::{LoopNote, LoopNotes};
use crate::deck::loops::{retune_cut, retune_frame_limit, Refusal};
use crate::wave::FireFlags;

use super::retune::{drain_retired, replay_core, Replay, RetuneEnv};

const BLOCK: usize = 512;

/// The notes of `clip`'s bars as its net played them, at `sample_rate` (any rate: the notes
/// come out in sixteenths). `env` picks the sounds the replay plays (they don't change the
/// notes: [`RetuneEnv::default`], synth voices only, is the quickest).
pub fn extract(clip: &LoopClip, sample_rate: f64, env: &RetuneEnv) -> Result<LoopNotes, Refusal> {
    let Replay {
        mut core,
        start_tick,
        end_tick,
        bpm,
        nets,
        generation,
        ..
    } = replay_core(clip, &BTreeMap::new(), sample_rate, env, &[])?;
    // part of each net, per project generation (a song may swap its project on a bar)
    let part = |id: &str, group: &str| {
        if group.is_empty() {
            id.to_string()
        } else {
            group.to_string()
        }
    };
    let mut parts: BTreeMap<i32, Vec<String>> = BTreeMap::new();
    parts.insert(
        generation,
        nets.iter().map(|(id, g)| part(id, g)).collect(),
    );

    let limit = retune_frame_limit(end_tick, bpm, sample_rate);
    let (mut start, mut end): (Option<i64>, Option<i64>) = (None, None);
    let (mut l, mut r) = (vec![0f32; BLOCK], vec![0f32; BLOCK]);
    let mut done: i64 = 0;
    // the take's fires: (step in the take, part, drum, channel, note, velocity, length)
    let mut fires: Vec<(i64, String, bool, u8, u8, u8, f64)> = Vec::new();
    let per_step = sample_rate * 60.0 / (bpm.max(1.0) * 4.0);
    while end.is_none_or(|e| done < e) {
        if done >= limit || core.mirror.stopped.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(Refusal::SongEnded);
        }
        core.render(&mut l, &mut r, done);
        done += BLOCK as i64;
        if let Some(p) = core.runner.project() {
            parts.entry(p.generation).or_insert_with(|| {
                p.graph
                    .nets
                    .iter()
                    .map(|n| part(&n.id, &n.bundle.riff_group))
                    .collect()
            });
        }
        let anchors: Vec<(i64, i64, bool)> = core
            .runner
            .anchors
            .drain(..)
            .map(|a| {
                (
                    a.tick as i64,
                    a.frame,
                    a.flags.contains(crate::wave::TickAnchorFlags::SWAP),
                )
            })
            .collect();
        (start, end) = retune_cut(&anchors, start_tick, end_tick, start, end);
        for f in core.runner.fires.drain(..) {
            let step = f.tick as i64 - 1;
            if step < start_tick || step >= end_tick || f.note < 0 || f.note > 127 {
                continue;
            }
            let Some(name) = parts
                .get(&f.generation)
                .and_then(|p| p.get(f.net.max(0) as usize))
            else {
                continue;
            };
            let len = if f.dur_steps > 0.0 {
                f.dur_steps as f64
            } else {
                f.dur_ms as f64 / 1000.0 * sample_rate / per_step
            };
            fires.push((
                step - start_tick,
                name.clone(),
                f.flags.contains(FireFlags::DRUM),
                f.channel.clamp(1, 16),
                f.note as u8,
                f.velocity.min(127),
                len,
            ));
        }
        if done % (BLOCK as i64 * 64) == 0 {
            drain_retired(&mut core);
        }
    }
    drain_retired(&mut core);
    if start.is_none() {
        return Err(Refusal::SongEnded);
    }
    let steps = (clip.beats.max(0) * 4) as u32;
    let notes = fires
        .into_iter()
        .map(|(step, part, drum, channel, note, velocity, len)| LoopNote {
            // on its step: a hit's groove delay (swing, microtiming) is the voice's, inside
            // the tick, and is not carried
            at: step as f64,
            len,
            part,
            drum,
            channel,
            note,
            velocity,
        })
        .collect();
    Ok(LoopNotes::new(steps, notes))
}
