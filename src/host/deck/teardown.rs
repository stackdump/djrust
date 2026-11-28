//! An engine, or what an engine lets go of, freed a piece at a time.
//!
//! A host whose audio thread is the only thread its engine has (djweb: the engine lives in an
//! AudioWorklet) frees on that thread whatever its engine lets go of. Freed whole, an engine with two
//! decks playing is about 20 ms (native) of compiled projects (their nets, their lanes, the
//! projects they were compiled from): several render quanta, an underrun. A [`Teardown`] takes
//! the engine apart into those pieces, cheaply (moves, nothing freed), and the host frees a few
//! per quantum ([`step`](Teardown::step)), the engine itself (what is left) last.
//!
//! The same holds for a track a deck lets go of when another lands on it (the compiled project
//! the render thread retires, the project the deck played, the share it was loaded from): an
//! engine set to free in pieces ([`Engine::set_free_in_pieces`]) keeps those as [`Pieces`] for
//! the host to free a few per quantum ([`Engine::free_step`]), so the quantum a section lands in
//! frees nothing.
//!
//! A piece is small: the parts with many allocations of their own (a net's places and
//! transitions, its id lists, a share's JSON) free [`SHRINK`] entries a step, not all at once.

use std::any::Any;

use crate::json::JsonValue;
use crate::pflow::{NetBundle, Project};
use crate::wave::net::CompiledNet;
use crate::wave::WaveProject;

use super::engine::{Engine, Renderer};

/// Entries (a net's places, transitions or arcs, ids, JSON nodes) a piece frees at most per step,
/// about: each a few allocations, so a step stays around a thousand allocator calls.
pub const SHRINK: usize = 128;

/// Something freed a bit at a time.
trait Shrink {
    /// Free about [`SHRINK`] entries' worth. True once it is all freed.
    fn shrink(&mut self) -> bool;
}

/// Freed in one go.
struct Whole<T>(Option<T>);

impl<T> Shrink for Whole<T> {
    fn shrink(&mut self) -> bool {
        self.0 = None;
        true
    }
}

/// A list of ids, freed [`SHRINK`] at a time.
struct Strings(Vec<String>);

impl Shrink for Strings {
    fn shrink(&mut self) -> bool {
        let keep = self.0.len().saturating_sub(SHRINK);
        self.0.truncate(keep);
        if keep == 0 {
            self.0 = Vec::new();
        }
        keep == 0
    }
}

/// A net's places, transitions, arcs and bindings, [`SHRINK`] at a time, then the rest.
struct Bundle(Option<NetBundle>);

impl Shrink for Bundle {
    fn shrink(&mut self) -> bool {
        let Some(b) = self.0.as_mut() else { return true };
        if !b.transitions.free_some(SHRINK) || !b.places.free_some(SHRINK) || !b.bindings.free_some(SHRINK) {
            return false;
        }
        if !b.arcs.is_empty() {
            let keep = b.arcs.len().saturating_sub(SHRINK);
            b.arcs.truncate(keep);
            return false;
        }
        self.0 = None;
        true
    }
}

/// A JSON document freed a few nodes at a time: a container's children go on the stack (its keys
/// freed as they are taken off), a scalar is freed; about [`SHRINK`] nodes a step.
struct Json(Vec<JsonValue>);

impl Shrink for Json {
    fn shrink(&mut self) -> bool {
        let mut done = 0;
        while done < SHRINK {
            let Some(v) = self.0.pop() else { break };
            done += 1;
            match v {
                JsonValue::Array(a) => {
                    done += a.len() / 8;
                    self.0.extend(a);
                }
                JsonValue::Object(o) => {
                    let entries = o.into_entries();
                    done += entries.len();
                    self.0.extend(entries.into_iter().map(|(_, x)| x));
                }
                _ => {}
            }
        }
        if self.0.is_empty() {
            self.0 = Vec::new();
            return true;
        }
        false
    }
}

/// A project freed a net at a time, the net [`SHRINK`] entries a step ([`Bundle`]).
struct ProjectNets {
    project: Option<Project>,
    net: Bundle,
}

impl Shrink for ProjectNets {
    fn shrink(&mut self) -> bool {
        if self.net.0.is_some() {
            self.net.shrink();
            return false;
        }
        let Some(p) = self.project.as_mut() else { return true };
        match p.nets.pop_last() {
            Some((_, n)) => {
                self.net = Bundle(Some(n));
                false
            }
            None => {
                self.project = None;
                true
            }
        }
    }
}

struct Piece {
    name: &'static str,
    thing: Box<dyn Shrink>,
}

/// Things to free later, a few at a time, from the end: a compiled project or a project goes in
/// as its nets and lanes (each a piece), itself (what is left) freed after them.
#[derive(Default)]
pub struct Pieces {
    pieces: Vec<Piece>,
}

impl Pieces {
    /// Anything, as one piece.
    pub fn push<T: Any>(&mut self, x: T) {
        self.pieces.push(Piece { name: core::any::type_name::<T>(), thing: Box::new(Whole(Some(x))) });
    }

    fn push_shrink(&mut self, name: &'static str, s: impl Shrink + 'static) {
        self.pieces.push(Piece { name, thing: Box::new(s) });
    }

    fn push_strings(&mut self, v: Vec<String>) {
        if !v.is_empty() {
            self.push_shrink("ids", Strings(v));
        }
    }

    fn push_bundle(&mut self, b: NetBundle) {
        self.push_shrink("NetBundle", Bundle(Some(b)));
    }

    /// A compiled net: its bundle and its id lists, then the rest.
    fn push_net(&mut self, mut n: CompiledNet) {
        let bundle = std::mem::take(&mut n.bundle);
        let ids = [std::mem::take(&mut n.place_ids), std::mem::take(&mut n.trans_ids), std::mem::take(&mut n.action_target)];
        self.push(n);
        self.push_bundle(bundle);
        for v in ids {
            self.push_strings(v);
        }
    }

    /// A compiled project, as its lanes, its nets and the project they were compiled from.
    pub fn push_wave(&mut self, mut p: WaveProject) {
        // the sampler slots it holds are given back now, as dropping it would: freeing it later
        // must not keep a slot from the bank any longer
        p.release_slots();
        let lanes = p.take_lanes();
        let nets = std::mem::take(&mut p.graph.nets);
        let project = std::mem::take(&mut p.graph.project);
        // freed from the end: its parts, then what is left of it
        self.push(p);
        for l in lanes {
            self.push(l);
        }
        for n in nets {
            self.push_net(n);
        }
        self.push_project(project);
    }

    /// A project: taken whole, it frees its nets one at a time (each [`SHRINK`] entries a
    /// step), then itself.
    pub fn push_project(&mut self, p: Project) {
        self.push_shrink("Project", ProjectNets { project: Some(p), net: Bundle(None) });
    }

    /// A JSON document (a raw-`nets` share's payload: tens of thousands of nodes): taken whole
    /// (nothing walked or moved here), it frees about [`SHRINK`] nodes a step.
    pub fn push_json(&mut self, v: JsonValue) {
        self.push_shrink("json", Json(vec![v]));
    }

    /// Free up to `n` pieces' worth (a step of each). True once everything is freed.
    pub fn step(&mut self, n: usize) -> bool {
        for _ in 0..n {
            let Some(p) = self.pieces.last_mut() else { break };
            if p.thing.shrink() {
                self.pieces.pop();
            }
        }
        self.pieces.is_empty()
    }

    /// Pieces still to free.
    pub fn len(&self) -> usize {
        self.pieces.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pieces.is_empty()
    }

    /// What the piece freed next is (a type name, for a profile of what costs what).
    pub fn next_name(&self) -> Option<&'static str> {
        self.pieces.last().map(|p| p.name)
    }

    fn append(&mut self, other: Pieces) {
        self.pieces.extend(other.pieces);
    }
}


pub struct Teardown {
    /// Freed from the end: the pieces first, the engine and its renderer last.
    pieces: Pieces,
}

impl Teardown {
    pub fn new(mut e: Engine, mut r: Renderer) -> Teardown {
        let mut parts = Pieces::default();
        for p in r.take_projects() {
            parts.push_wave(p);
        }
        for d in e.decks.iter_mut() {
            if let Some(p) = d.project.take() {
                parts.push_project(p);
            }
        }
        // what the engine was itself still freeing in pieces stays pieces
        if let Some(f) = e.take_free_later() {
            parts.append(f);
        }
        let mut pieces = Pieces::default();
        pieces.push(e);
        pieces.push(r);
        pieces.append(parts);
        Teardown { pieces }
    }

    /// Free up to `n` pieces' worth. True once everything is freed.
    pub fn step(&mut self, n: usize) -> bool {
        self.pieces.step(n)
    }

    /// Pieces still to free.
    pub fn len(&self) -> usize {
        self.pieces.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pieces.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::deck::engine::EngineConfig;
    use crate::host::deck::host::host_engine;
    use crate::host::deck::perform::PerformCtx;
    use crate::session::action::DeckAction;
    use crate::session::model::DeckSource;
    use crate::TrackSpec;
    use std::time::Instant;

    /// Two decks loaded and played a little: the engine a restore swaps out.
    fn busy() -> (Engine, Renderer) {
        let mut cfg = EngineConfig::new(48_000.0);
        cfg.history_beats = None;
        let (mut e, mut r) = host_engine(cfg, false, true);
        for (d, s) in [("A", "techno:42:standard"), ("B", "house:7:extended")] {
            let spec = TrackSpec::parse(s).unwrap();
            e.perform(&DeckAction::Load { deck: d.into(), source: DeckSource::Net(spec) }, PerformCtx::default());
            e.perform(&DeckAction::Play { deck: d.into(), on: true }, PerformCtx::default());
        }
        let (mut l, mut rr) = (vec![0f32; 128], vec![0f32; 128]);
        for k in 0..200 {
            r.render_block(&mut l, &mut rr);
            if k % 4 == 3 {
                e.service();
            }
        }
        (e, r)
    }

    /// Freed in pieces, no piece is much of the whole (`cargo test --release teardown -- --nocapture`
    /// prints the times).
    #[test]
    fn an_engine_frees_in_small_pieces() {
        let (e, r) = busy();
        let t = Instant::now();
        drop((e, r));
        let whole = t.elapsed().as_secs_f64() * 1e3;
        let (e, r) = busy();
        let t = Instant::now();
        let mut td = Teardown::new(e, r);
        let split = t.elapsed().as_secs_f64() * 1e3;
        let n = td.len();
        assert!(n > 50, "{n} pieces");
        let (mut worst, mut all) = (0f64, 0f64);
        while !td.is_empty() {
            let t = Instant::now();
            td.step(1);
            let ms = t.elapsed().as_secs_f64() * 1e3;
            worst = worst.max(ms);
            all += ms;
        }
        eprintln!("whole {whole:.2} ms; split {split:.3} ms into {n} pieces, freed in {all:.2} ms, worst {worst:.3} ms");
    }
}
