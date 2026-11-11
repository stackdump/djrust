//! Port of `Musicality/MelodyBigrams.swift`: a melody scale-degree bigram table counted from
//! the RS200 rock corpus's vocal transcriptions (de Clercq & Temperley, CC BY 4.0), in
//! `data/melody-bigrams.json`. `.shape` reads `moves`.

use alloc::string::String;
use alloc::vec::Vec;

use crate::json::JsonValue;

pub const MELODY_BIGRAMS_JSON: &str = include_str!("../../data/melody-bigrams.json");

#[derive(Clone, Debug, PartialEq)]
pub struct BigramStats {
    pub repeats: f64,
    pub steps: f64,
    pub steps_of_moves: f64,
    pub pitch_repeats: f64,
    pub pitch_steps: f64,
    pub pitch_leaps: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MelodyBigrams {
    pub name: String,
    pub songs: i64,
    pub bigrams: i64,
    /// How often each degree is sung.
    pub unigram: Vec<f64>,
    /// `[degree][next]`, P(next | degree); rows sum to 1.
    pub next: Vec<Vec<f64>>,
    /// `[degree][next]`, P(next | degree, next ≠ degree); the diagonal is 0.
    pub moves: Vec<Vec<f64>>,
    /// `[degree][next]`, the corpus's counts behind both.
    pub counts: Vec<Vec<i64>>,
    pub stats: BigramStats,
}

impl MelodyBigrams {
    pub const DEGREES: usize = 7;
    /// Rounding to 4 decimals leaves a row at most 7 × 0.00005 off 1.
    pub const ROW_TOLERANCE: f64 = 1e-3;

    fn wrap(d: i64) -> usize {
        (((d % 7) + 7) % 7) as usize
    }

    /// P(`to` | `from`), degrees taken mod 7.
    pub fn probability(&self, from: i64, to: i64) -> f64 {
        self.next[Self::wrap(from)][Self::wrap(to)]
    }

    /// The degree (0…6) most likely to follow `degree`; `moving` leaves the repeat out. Ties
    /// go to the lower degree.
    pub fn most_likely(&self, degree: i64, moving: bool) -> usize {
        let row = if moving {
            &self.moves[Self::wrap(degree)]
        } else {
            &self.next[Self::wrap(degree)]
        };
        let mut best = 0;
        for d in 1..Self::DEGREES {
            if row[d] > row[best] {
                best = d;
            }
        }
        best
    }

    fn share(&self, include: impl Fn(i64, i64) -> bool) -> f64 {
        let row_totals: Vec<f64> = self
            .counts
            .iter()
            .map(|r| r.iter().sum::<i64>() as f64)
            .collect();
        let all: f64 = row_totals.iter().sum();
        if all <= 0.0 {
            return 0.0;
        }
        let mut s = 0.0;
        for i in 0..Self::DEGREES {
            for j in 0..Self::DEGREES {
                if include(i as i64, j as i64) {
                    s += row_totals[i] * self.next[i][j];
                }
            }
        }
        s / all
    }

    /// The table's share of steps (to a neighbouring degree), weighted by how often each
    /// degree is the first of a bigram.
    pub fn step_share(&self) -> f64 {
        self.share(|a, b| a != b && (Self::wrap(a - b) == 1 || Self::wrap(b - a) == 1))
    }

    /// The table's share of repeated degrees.
    pub fn repeat_share(&self) -> f64 {
        self.share(|a, b| a == b)
    }

    /// Every table in the file, by name (`default` is the corpus-wide one).
    pub fn all() -> Result<Vec<MelodyBigrams>, String> {
        Self::parse(MELODY_BIGRAMS_JSON)
    }

    /// The corpus-wide table.
    pub fn default_table() -> MelodyBigrams {
        Self::all()
            .expect("melody-bigrams.json parses")
            .into_iter()
            .find(|t| t.name == "default")
            .expect("melody bigrams: no default table")
    }

    fn matrix_f(v: Option<&JsonValue>) -> Option<Vec<Vec<f64>>> {
        v?.as_array()?
            .iter()
            .map(|row| row.as_array()?.iter().map(|x| x.as_f64()).collect())
            .collect()
    }

    /// `parse`: validates the file the way Swift does (square, stochastic rows, zero move
    /// diagonal, counts summing to `bigrams`).
    pub fn parse(text: &str) -> Result<Vec<MelodyBigrams>, String> {
        let f = JsonValue::parse(text).map_err(|e| alloc::format!("melody-bigrams.json: {e:?}"))?;
        let bad = |why: &str| alloc::format!("melody-bigrams.json: {why}");
        let degrees: Vec<String> = f
            .get("degrees")
            .and_then(|d| d.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let want: Vec<String> = (1..=Self::DEGREES).map(|i| alloc::format!("{i}")).collect();
        if degrees != want {
            return Err(bad("degrees"));
        }
        let tables = f
            .get("tables")
            .and_then(|t| t.as_object())
            .ok_or_else(|| bad("tables"))?;
        let mut out = Vec::new();
        for (name, t) in tables.iter() {
            let next = Self::matrix_f(t.get("next")).ok_or_else(|| bad(name))?;
            let moves = Self::matrix_f(t.get("moves")).ok_or_else(|| bad(name))?;
            let counts: Vec<Vec<i64>> = t
                .get("counts")
                .and_then(|c| c.as_array())
                .and_then(|a| {
                    a.iter()
                        .map(|row| row.as_array()?.iter().map(|x| x.as_i64()).collect())
                        .collect()
                })
                .ok_or_else(|| bad(name))?;
            let unigram: Vec<f64> = t
                .get("unigram")
                .and_then(|u| u.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_f64()).collect())
                .ok_or_else(|| bad(name))?;
            let bigrams = t
                .get("bigrams")
                .and_then(|x| x.as_i64())
                .ok_or_else(|| bad(name))?;
            let square_f = |m: &[Vec<f64>]| m.len() == 7 && m.iter().all(|r| r.len() == 7);
            let stochastic = |m: &[Vec<f64>]| {
                m.iter().all(|row| {
                    row.iter().all(|x| *x >= 0.0)
                        && (row.iter().sum::<f64>() - 1.0).abs() <= Self::ROW_TOLERANCE
                })
            };
            let total: i64 = counts.iter().flatten().sum();
            if !(square_f(&next)
                && square_f(&moves)
                && counts.len() == 7
                && counts.iter().all(|r| r.len() == 7)
                && unigram.len() == 7
                && stochastic(&next)
                && stochastic(&moves)
                && (0..7).all(|i| moves[i][i] == 0.0)
                && total == bigrams)
            {
                return Err(bad(name));
            }
            let st = t.get("stats").ok_or_else(|| bad(name))?;
            let g = |o: Option<&JsonValue>, k: &str| -> f64 {
                o.and_then(|o| o.get(k))
                    .and_then(|x| x.as_f64())
                    .unwrap_or(0.0)
            };
            let pitch = st.get("pitch");
            out.push(MelodyBigrams {
                name: name.into(),
                songs: t.get("songs").and_then(|x| x.as_i64()).unwrap_or(0),
                bigrams,
                unigram,
                next,
                moves,
                counts,
                stats: BigramStats {
                    repeats: g(Some(st), "repeats"),
                    steps: g(Some(st), "steps"),
                    steps_of_moves: g(Some(st), "stepsOfMoves"),
                    pitch_repeats: g(pitch, "repeats"),
                    pitch_steps: g(pitch, "steps"),
                    pitch_leaps: g(pitch, "leaps"),
                },
            });
        }
        Ok(out)
    }
}
