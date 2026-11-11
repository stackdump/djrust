//! Port of `NetSnapshot.swift`: a ring net as a display sees it (where its hits are and where
//! the token is), derived from the net clock so nothing reads the executor's state across
//! threads.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::pflow::NetBundle;

/// `RingView`.
#[derive(Clone, Debug, PartialEq)]
pub struct RingView {
    pub id: String,
    pub role: String,
    pub size: usize,
    /// Per step: is there a note on the transition that fires from this step's place?
    pub hits: Vec<bool>,
    pub velocities: Vec<i64>,
    /// Step the token sits on (the one that fires next).
    pub token: usize,
    /// Step that fired most recently (-1 before the first tick).
    pub last_fired: i64,
    /// How far into the current tick we are, 0…1: for a fading flash on `last_fired`.
    pub phase: f64,
    pub muted: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NetSnapshot {
    pub tick: i64,
    pub rings: Vec<RingView>,
}

/// The shape of a composer ring: `p0 → t0 → p1 → … → p(n-1) → t(n-1) → p0`, one token on p0
/// (`RingShape`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RingShape {
    pub size: usize,
    pub hits: Vec<bool>,
    pub velocities: Vec<i64>,
}

impl RingShape {
    /// `RingShape(_ nb:)`: None unless `nb` is exactly such a ring.
    pub fn of(nb: &NetBundle) -> Option<RingShape> {
        let n = nb.places.len();
        if n == 0 || nb.transitions.len() != n || nb.arcs.len() != 2 * n {
            return None;
        }
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for a in &nb.arcs {
            if a.inhibit || a.weight != [1.0] {
                return None;
            }
            seen.insert(format!("{}>{}", a.source, a.target));
        }
        for i in 0..n {
            if !seen.contains(&format!("p{i}>t{i}"))
                || !seen.contains(&format!("t{i}>p{}", (i + 1) % n))
            {
                return None;
            }
            let p = nb.places.get(&format!("p{i}"))?;
            let tokens: f64 = p.initial.iter().sum();
            if tokens != if i == 0 { 1.0 } else { 0.0 } {
                return None;
            }
        }
        Some(RingShape {
            size: n,
            hits: (0..n)
                .map(|i| nb.bindings.get(&format!("t{i}")).is_some())
                .collect(),
            velocities: (0..n)
                .map(|i| {
                    nb.bindings
                        .get(&format!("t{i}"))
                        .map(|b| b.velocity)
                        .unwrap_or(0)
                })
                .collect(),
        })
    }

    /// The ring as a display shows it at the net clock: a single-token ring has its token on
    /// `tick mod n` (`Deck.netSnapshot`).
    pub fn view(&self, id: &str, role: &str, tick: i64, tick_phase: f64, muted: bool) -> RingView {
        let n = self.size as i64;
        RingView {
            id: id.into(),
            role: role.into(),
            size: self.size,
            hits: self.hits.clone(),
            velocities: self.velocities.clone(),
            token: (tick % n) as usize,
            last_fired: if tick == 0 { -1 } else { (tick - 1) % n },
            phase: tick_phase,
            muted,
        }
    }
}

/// `Deck.netSnapshot`: the audible ring of each role the song has, in the order `roles` lists
/// them (`Deck.roleOrder` then `hit1`...`hit4`), with token positions derived from the net
/// clock (`tick`, `tick_phase`) so nothing reads the executor's state across threads.
///
/// `roles` gives each role with the nets it may be: `(index in the compiled graph, net id)`,
/// a riff group's nets in group order or the one net. A role shows the first of them that is
/// not muted (`is_muted(index)`), else the first; `bundle(id)` finds that net in the deck's
/// project. A net that is not a ring (see [`RingShape::of`]) is left out. `shapes` caches the
/// shapes found, by net id (`Deck.ringShapes`).
pub fn net_snapshot<'a>(
    tick: i64,
    tick_phase: f64,
    roles: impl IntoIterator<Item = (&'a str, &'a [(usize, String)])>,
    is_muted: &dyn Fn(usize) -> bool,
    bundle: &dyn Fn(&str) -> Option<&'a NetBundle>,
    shapes: &mut BTreeMap<String, RingShape>,
) -> NetSnapshot {
    let mut rings = Vec::new();
    for (role, candidates) in roles {
        let pick = candidates
            .iter()
            .find(|(i, _)| !is_muted(*i))
            .or_else(|| candidates.first());
        let Some((idx, id)) = pick else { continue };
        let Some(nb) = bundle(id) else { continue };
        let shape = match shapes.get(id) {
            Some(s) => s.clone(),
            None => match RingShape::of(nb) {
                Some(s) => {
                    shapes.insert(id.clone(), s.clone());
                    s
                }
                None => continue,
            },
        };
        rings.push(shape.view(id, role, tick, tick_phase, is_muted(*idx)));
    }
    NetSnapshot { tick, rings }
}
