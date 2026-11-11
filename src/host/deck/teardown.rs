//! An engine freed a piece at a time.
//!
//! A host whose audio thread is the only thread its engine has (djweb: the engine lives in an
//! AudioWorklet) frees an engine it swaps out on that thread. Freed whole, an engine with two
//! decks playing is about 20 ms (native) of compiled projects (their nets, their lanes, the
//! projects they were compiled from): several render quanta, an underrun. A [`Teardown`] takes
//! the engine apart into those pieces, cheaply (moves, nothing freed), and the host frees a few
//! per quantum ([`step`](Teardown::step)), the engine itself (what is left) last.

use std::any::Any;

use crate::pflow::Project;
use crate::wave::WaveProject;

use super::engine::{Engine, Renderer};

pub struct Teardown {
    /// Freed from the end: the pieces first, the engine and its renderer last.
    pieces: Vec<Box<dyn Any>>,
}

impl Teardown {
    pub fn new(mut e: Engine, mut r: Renderer) -> Teardown {
        let mut pieces: Vec<Box<dyn Any>> = Vec::new();
        for p in r.take_projects() {
            wave_pieces(p, &mut pieces);
        }
        for d in e.decks.iter_mut() {
            if let Some(p) = d.project.take() {
                project_pieces(p, &mut pieces);
            }
        }
        let mut all: Vec<Box<dyn Any>> = Vec::with_capacity(pieces.len() + 2);
        all.push(Box::new(e));
        all.push(Box::new(r));
        all.extend(pieces);
        Teardown { pieces: all }
    }

    /// Free up to `n` pieces. True once everything is freed.
    pub fn step(&mut self, n: usize) -> bool {
        for _ in 0..n {
            if self.pieces.pop().is_none() {
                break;
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
}

fn wave_pieces(mut p: WaveProject, out: &mut Vec<Box<dyn Any>>) {
    let rest_first = out.len();
    out.push(Box::new(())); // where the rest of `p` goes, freed after its parts
    for l in p.take_lanes() {
        out.push(Box::new(l));
    }
    for n in std::mem::take(&mut p.graph.nets) {
        out.push(Box::new(n));
    }
    project_pieces(std::mem::take(&mut p.graph.project), out);
    out[rest_first] = Box::new(p);
}

fn project_pieces(mut p: Project, out: &mut Vec<Box<dyn Any>>) {
    let keys: Vec<String> = p.nets.keys().to_vec();
    for k in keys.iter().rev() {
        if let Some(n) = p.nets.remove(k) {
            out.push(Box::new(n));
        }
    }
    out.push(Box::new(p));
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
