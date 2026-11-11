//! The pure rules of `DeckPerform.swift`: the roles and STEMS groups, disabled roles, the
//! mix-out / mix-in tick arithmetic (`mixOutPoint`, `nextBarStart`, `mixOut`, `mixIn`),
//! section stepping and the scale-locked keys. Relocation, preparing a project and the audio
//! calls are the host's; these decide *where* and *what*.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::timeline::SectionInfo;
use crate::gen::core::scale_intervals;
use crate::wave::rests::{Entry, MixOutTail};
use crate::wave::runner::RoleFilter;

/// `Deck.roleOrder`: the roles in mixer order.
pub const ROLE_ORDER: [&str; 8] = [
    "kick", "snare", "hihat", "clap", "bass", "melody", "harmony", "arp",
];

/// A controller's STEMS buttons on a net deck: each mutes a group of roles. "music" is
/// everything but the drums; "mic" is the lead, the nearest a net has to a vocal.
pub const STEMS: [(&str, &[&str]); 2] = [
    ("music", &["bass", "melody", "harmony", "arp"]),
    ("mic", &["melody"]),
];

/// `Deck.stemNames`: mic, music.
pub fn stem_names() -> Vec<&'static str> {
    let mut v: Vec<&str> = STEMS.iter().map(|(n, _)| *n).collect();
    v.sort_unstable();
    v
}

/// `Deck.stemRoles`: the roles of `stem` this deck's net has.
pub fn stem_roles(stem: &str, deck_roles: &[String]) -> Vec<String> {
    let group: &[&str] = STEMS
        .iter()
        .find(|(n, _)| *n == stem)
        .map(|(_, g)| *g)
        .unwrap_or(&[]);
    deck_roles
        .iter()
        .filter(|r| group.contains(&r.as_str()))
        .cloned()
        .collect()
}

/// Which STEMS groups the button has muted, and the roles each held (`Deck.stemsMuted`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StemsMuted {
    pub groups: BTreeMap<String, Vec<String>>,
}

impl StemsMuted {
    pub fn is_muted(&self, stem: &str) -> bool {
        self.groups.contains_key(stem)
    }

    /// `toggleStem`: mute the group, or bring back what it silenced. Song sections mute and
    /// unmute roles too, so "silent now" is not "muted by the button": the group remembers
    /// which roles were playing when it muted them and brings back only those (and none
    /// another muted group still holds). Returns the `(role, on)` calls to make, in order.
    pub fn toggle(
        &mut self,
        stem: &str,
        deck_roles: &[String],
        is_role_on: &dyn Fn(&str) -> bool,
    ) -> Vec<(String, bool)> {
        if let Some(playing) = self.groups.remove(stem) {
            let held: Vec<&String> = self.groups.values().flatten().collect();
            return playing
                .iter()
                .filter(|r| !held.contains(r))
                .map(|r| (r.clone(), true))
                .collect();
        }
        let rs = stem_roles(stem, deck_roles);
        let held_by_others = |r: &String| self.groups.values().any(|g| g.contains(r));
        let remembered: Vec<String> = rs
            .iter()
            .filter(|r| is_role_on(r) || held_by_others(r))
            .cloned()
            .collect();
        self.groups.insert(stem.to_string(), remembered);
        rs.into_iter().map(|r| (r, false)).collect()
    }
}

/// `Deck.setRoleDisabled`, the filter half: a disabled role is left out of every net this deck
/// compiles from now on (`roleFilter`), nil once nothing is disabled.
pub fn set_role_disabled(
    filter: Option<RoleFilter>,
    role: &str,
    disabled: bool,
) -> Option<RoleFilter> {
    let mut f = filter.unwrap_or_default();
    if disabled {
        if !f.mute.iter().any(|m| m == role) {
            f.mute.push(role.to_string());
        }
    } else {
        f.mute.retain(|m| m != role);
    }
    (!f.is_empty()).then_some(f)
}

/// The roles a toggle may flip: a disabled role stays off (`toggleRole`).
pub fn toggle_role(disabled: &[String], role: &str, is_on: bool) -> Option<bool> {
    if disabled.iter().any(|d| d == role) {
        None
    } else {
        Some(!is_on)
    }
}

/// `nextSwapTick`: the first tick of the next bar's hand-over, less one: a project prepared
/// from here carries on where the playing one would have been.
pub fn next_swap_tick(tick: i64) -> i64 {
    let t = tick.max(0);
    t + (15 - t % 16)
}

/// `nextBarStart`: where a change made "from the next bar" starts: the tick to prepare from
/// and the bar line the change begins on.
pub fn next_bar_start(is_playing: bool, tick: i64) -> (i64, i64) {
    if is_playing {
        let t = next_swap_tick(tick);
        (t, t + 1)
    } else {
        let t = tick.max(0);
        (t, (t + 15) / 16 * 16)
    }
}

/// The last `outro` section, if its start is at or after `bar_tick`.
fn outro_from(sections: &[SectionInfo], bar_tick: i64) -> Option<i64> {
    sections
        .iter()
        .rev()
        .find(|s| s.name == "outro")
        .map(|s| s.start_tick)
        .filter(|t| *t >= bar_tick)
}

/// `mixOutPoint(atBar:bars:)`: where `mixOut(bars:)` performed in bar `h - 1` of a playing
/// deck starts (the tick the relocation prepares from) and the tail it plays: the outro if it
/// is still ahead, else the bar line `h`.
pub fn mix_out_point(sections: &[SectionInfo], h: i64, bars: i64) -> (i64, MixOutTail) {
    let mut tick = h * 16 - 1; // `nextSwapTick` anywhere in bar h - 1
    let mut bar = h * 16;
    if let Some(o) = outro_from(sections, bar) {
        tick = o;
        bar = o;
    }
    (tick, MixOutTail::new(bar, bars.max(1)))
}

/// `mixOut(bars:)`: at the next bar the deck jumps to the song's outro (or, already in it or
/// with none, plays on from where it is) and from that bar line plays only its drums-forward
/// lanes for `bars` bars. Returns `(tick to relocate to, the tail)`.
pub fn mix_out(
    sections: &[SectionInfo],
    is_playing: bool,
    tick: i64,
    bars: i64,
) -> (i64, MixOutTail) {
    let (mut t, mut bar) = next_bar_start(is_playing, tick);
    if let Some(o) = outro_from(sections, bar) {
        t = o;
        bar = o;
    }
    (t, MixOutTail::new(bar, bars.max(1)))
}

/// `mixIn(bars:)`: bring the deck in drums-forward: from the next bar its melodic lanes rest
/// for `bars` bars and come back on the bar line after. Returns `(tick to relocate to, the
/// rest)`, None for `bars <= 0`.
pub fn mix_in(is_playing: bool, tick: i64, bars: i64) -> Option<(i64, Entry)> {
    if bars <= 0 {
        return None;
    }
    let (t, bar) = next_bar_start(is_playing, tick);
    Some((t, crate::wave::rests::RestSchedule::mix_in(bar, bars)))
}

/// `isMixingIn`: a mix-in's rest is still to come or playing.
pub fn is_mixing_in(mix_in_end_tick: Option<i64>, tick: i64) -> bool {
    mix_in_end_tick.map(|e| tick < e).unwrap_or(false)
}

/// `currentSectionIndex`: the section the net clock is in.
pub fn current_section_index(sections: &[SectionInfo], tick: i64) -> Option<usize> {
    sections
        .iter()
        .rposition(|s| s.start_tick <= tick && tick < s.start_tick + s.steps)
}

/// `launchSection`: the tick of section `index`.
pub fn launch_section_tick(sections: &[SectionInfo], index: usize) -> Option<i64> {
    sections.get(index).map(|s| s.start_tick)
}

/// `stepSection`: the tick of the section `delta` from the current one (clamped).
pub fn step_section_tick(sections: &[SectionInfo], tick: i64, delta: i64) -> Option<i64> {
    if sections.is_empty() {
        return None;
    }
    let cur = current_section_index(sections, tick).unwrap_or(0) as i64;
    let i = (cur + delta).max(0).min(sections.len() as i64 - 1);
    Some(sections[i as usize].start_tick)
}

/// `scaleLocked`: snap a MIDI note to the project's scale (nearest scale tone, ties go down).
/// `scale_name` is the project's (`"Minor"`, without the `Scale` suffix).
pub fn scale_locked(note: i64, root_note: Option<i64>, scale_name: &str) -> i64 {
    let Some(root) = root_note else { return note };
    let intervals = scale_intervals(&alloc::format!("{scale_name}Scale"));
    if intervals.is_empty() {
        return note;
    }
    let mut best = note;
    let mut best_dist = i64::MAX;
    for octave in -1..=11 {
        for i in intervals {
            let candidate = (root % 12) + octave * 12 + i;
            let d = (candidate - note).abs();
            if d < best_dist || (d == best_dist && candidate < best) {
                best = candidate;
                best_dist = d;
            }
        }
    }
    best
}
