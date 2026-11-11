//! The pure maths of `AutoMixMeasure.swift`: one hand-off's levels (dip, bump, the rounding of
//! the report). The offline set that produces the curve needs the whole deck engine and
//! BS.1770; the numbers it prints are computed here from a curve.

use alloc::string::String;
use alloc::vec::Vec;

use crate::json::{JsonObject, JsonValue};

/// One hand-off's levels (`AutoMixHandoffLevels`).
#[derive(Clone, Debug, PartialEq)]
pub struct AutoMixHandoffLevels {
    pub n: i64,
    pub from: String,
    pub to: String,
    pub move_: String,
    /// Integrated LUFS of the outgoing deck alone over the 8 bars before the hand-off, and of
    /// the incoming alone over the 8 bars after it.
    pub before: f64,
    pub after: f64,
    /// The lowest and highest short-term LUFS (per beat) during the hand-off's bars.
    pub min_during: f64,
    pub max_during: f64,
    /// Short-term LUFS per beat from 8 bars before to 8 bars after.
    pub curve: Vec<f64>,
}

/// `(x * 100).rounded() / 100`.
fn r(x: f64) -> f64 {
    libm::round(x * 100.0) / 100.0
}

impl AutoMixHandoffLevels {
    /// `minDuring − min(before, after)`: negative is a dip.
    pub fn dip_lu(&self) -> f64 {
        self.min_during - self.before.min(self.after)
    }

    /// `maxDuring − max(before, after)`: positive is a bump.
    pub fn bump_lu(&self) -> f64 {
        self.max_during - self.before.max(self.after)
    }

    /// `dictionary`, as JSON.
    pub fn json(&self) -> JsonValue {
        JsonObject::from_pairs([
            ("n", JsonValue::int(self.n)),
            ("from", JsonValue::str(self.from.as_str())),
            ("to", JsonValue::str(self.to.as_str())),
            ("move", JsonValue::str(self.move_.as_str())),
            ("before", JsonValue::Number(r(self.before))),
            ("after", JsonValue::Number(r(self.after))),
            ("minDuring", JsonValue::Number(r(self.min_during))),
            ("maxDuring", JsonValue::Number(r(self.max_during))),
            ("dipLU", JsonValue::Number(r(self.dip_lu()))),
            ("bumpLU", JsonValue::Number(r(self.bump_lu()))),
            (
                "curve",
                JsonValue::Array(
                    self.curve
                        .iter()
                        .map(|x| JsonValue::Number(r(*x)))
                        .collect(),
                ),
            ),
        ])
        .into()
    }

    /// The levels of a hand-off from a short-term curve (`(frame, lufs)` per beat) over the
    /// frames `start...end` (the incoming deck's first tick and the hand-off's end), with the
    /// alone levels `before` / `after` measured by the caller: the `st.filter` the Swift
    /// `run` does.
    pub fn from_curve(
        n: i64,
        from: &str,
        to: &str,
        move_: &str,
        before: f64,
        after: f64,
        st: &[(i64, f64)],
        start: i64,
        end: i64,
        window: i64,
    ) -> AutoMixHandoffLevels {
        let during: Vec<f64> = st
            .iter()
            .filter(|(f, _)| *f > start && *f <= end)
            .map(|(_, l)| *l)
            .collect();
        AutoMixHandoffLevels {
            n,
            from: from.into(),
            to: to.into(),
            move_: move_.into(),
            before,
            after,
            min_during: during
                .iter()
                .copied()
                .fold(None, |m: Option<f64>, x| Some(m.map_or(x, |m| m.min(x))))
                .unwrap_or(-200.0),
            max_during: during
                .iter()
                .copied()
                .fold(None, |m: Option<f64>, x| Some(m.map_or(x, |m| m.max(x))))
                .unwrap_or(-200.0),
            curve: st
                .iter()
                .filter(|(f, _)| *f > start - window && *f <= end + window)
                .map(|(_, l)| *l)
                .collect(),
        }
    }
}
