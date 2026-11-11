//! Tone.js's automation timeline for the master chain's parameters (`ToneParam.swift`).
//!
//! What a beats setter does to a parameter (tone-engine.js `setLowpassFreq` & co.) is a Tone
//! call: `frequency.cancelScheduledValues(now)`, `frequency.rampTo(f, 0.1)`,
//! `wet.rampTo(w, 0.05)`, `bits.value = b`. Tone keeps its own copy of the AudioParam's event
//! list (core/util/Timeline.js) and mirrors every call onto the native param, so the value the
//! browser renders at time t is Tone's `getValueAtTime(t)`. This is a port of exactly that,
//! from Tone 15.1.22 (core/context/Param.js: `setValueAtTime`, `linearRampToValueAtTime`,
//! `exponentialRampToValueAtTime`, `cancelScheduledValues`, `cancelAndHoldAtTime`,
//! `setRampPoint`, `rampTo`, the `value` setter, `getValueAtTime`; Timeline.js: `add`, `get`,
//! `getAfter`, `cancel`, `_search`), quirks included:
//!
//! - `rampTo` starts from Tone's idea of the value *after* any cancel the caller made first.
//!   The HP/LP setters cancel before ramping, so a ramp cut short by the next one jumps back
//!   to where the cut ramp started, then ramps from there.
//! - a ramp point of exactly 0 becomes 1e-7 (`_minOutput`), linear ramps included.
//! - times within 1e-6 s are "equal" (core/util/Math.js `EQ`).
//!
//! `setTargetAtTime` is not ported (nothing in the master chain uses it).
//!
//! Real-time: fixed capacity, no allocation. Tone keeps 1000 events per param; this keeps
//! [`ToneParam::CAPACITY`], and [`ToneParam::prune`] drops events no future query can reach.

use libm::pow;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Set,
    Linear,
    Exponential,
}

#[derive(Clone, Copy, Debug)]
pub struct ToneParam {
    /// The value before any event (the native param's value when Tone built the node).
    initial_value: f64,
    /// `rampTo` is exponential for frequency units (Param.js `rampTo`), linear otherwise.
    exponential_ramps: bool,
    /// The context's `sampleTime` (one `cancel` in `cancelAndHoldAtTime` uses it).
    sample_time: f64,
    count: usize,
    times: [f64; Self::CAPACITY],
    kinds: [Kind; Self::CAPACITY],
    values: [f64; Self::CAPACITY],
}

impl ToneParam {
    /// core/util/Math.js EPSILON.
    pub const EPSILON: f64 = 1e-6;
    /// Param.js `_minOutput`.
    pub const MIN_OUTPUT: f64 = 1e-7;
    pub const CAPACITY: usize = 16;

    pub fn new(value: f64, exponential_ramps: bool, sample_rate: f64) -> ToneParam {
        ToneParam {
            initial_value: value,
            exponential_ramps,
            sample_time: 1.0 / sample_rate,
            count: 0,
            times: [0.0; Self::CAPACITY],
            kinds: [Kind::Set; Self::CAPACITY],
            values: [0.0; Self::CAPACITY],
        }
    }

    pub fn count(&self) -> usize {
        self.count
    }

    // MARK: Timeline.js

    fn eq(a: f64, b: f64) -> bool {
        (a - b).abs() < Self::EPSILON
    }
    fn lt(a: f64, b: f64) -> bool {
        a + Self::EPSILON < b
    }
    fn gt(a: f64, b: f64) -> bool {
        a > b + Self::EPSILON
    }

    /// `_search`: the index of the last event at or before `t` (equal within epsilon), or -1.
    fn search(&self, t: f64) -> i64 {
        if self.count == 0 {
            return -1;
        }
        let mut beginning = 0usize;
        let mut end = self.count;
        if self.times[self.count - 1] <= t {
            return self.count as i64 - 1;
        }
        while beginning < end {
            let mut mid = beginning + (end - beginning) / 2;
            let at = self.times[mid];
            if Self::eq(at, t) {
                let mut i = mid;
                while i < self.count && Self::eq(self.times[i], t) {
                    mid = i;
                    i += 1;
                }
                return mid as i64;
            }
            // JS reads `_timeline[mid + 1]` past the end as undefined (comparisons false).
            if Self::lt(at, t) && mid + 1 < self.count && Self::gt(self.times[mid + 1], t) {
                return mid as i64;
            } else if Self::gt(at, t) {
                end = mid;
            } else {
                beginning = mid + 1;
            }
        }
        -1
    }

    fn add(&mut self, kind: Kind, value: f64, t: f64) {
        // (Tone trims its oldest event after inserting; with pruning this never fills up.)
        if self.count == Self::CAPACITY {
            self.remove_first(1);
        }
        let at = (self.search(t) + 1) as usize;
        let mut i = self.count;
        while i > at {
            self.times[i] = self.times[i - 1];
            self.kinds[i] = self.kinds[i - 1];
            self.values[i] = self.values[i - 1];
            i -= 1;
        }
        self.times[at] = t;
        self.kinds[at] = kind;
        self.values[at] = value;
        self.count += 1;
    }

    fn remove_first(&mut self, n: usize) {
        if n == 0 {
            return;
        }
        for i in n..self.count {
            self.times[i - n] = self.times[i];
            self.kinds[i - n] = self.kinds[i];
            self.values[i - n] = self.values[i];
        }
        self.count -= n;
    }

    /// Timeline `cancel(after)`: drop every event at or after `after`.
    fn cancel(&mut self, after: f64) {
        if self.count > 1 {
            let mut index = self.search(after);
            if index >= 0 {
                if Self::eq(self.times[index as usize], after) {
                    let mut i = index;
                    while i >= 0 && Self::eq(self.times[i as usize], after) {
                        index = i;
                        i -= 1;
                    }
                    self.count = index as usize;
                } else {
                    self.count = index as usize + 1;
                }
            } else {
                self.count = 0;
            }
        } else if self.count == 1
            && (Self::gt(self.times[0], after) || Self::eq(self.times[0], after))
        {
            self.count = 0;
        }
    }

    // MARK: Param.js

    /// `getValueAtTime`.
    pub fn value_at(&self, time: f64) -> f64 {
        let t = time.max(0.0);
        let b = self.search(t);
        if b < 0 {
            return self.initial_value;
        }
        let b = b as usize;
        if b + 1 >= self.count {
            return self.values[b];
        }
        let a = b + 1;
        match self.kinds[a] {
            Kind::Linear => {
                self.values[b]
                    + (self.values[a] - self.values[b])
                        * ((t - self.times[b]) / (self.times[a] - self.times[b]))
            }
            Kind::Exponential => {
                self.values[b]
                    * pow(
                        self.values[a] / self.values[b],
                        (t - self.times[b]) / (self.times[a] - self.times[b]),
                    )
            }
            Kind::Set => self.values[b],
        }
    }

    /// `setValueAtTime`.
    pub fn set_value(&mut self, v: f64, t: f64) {
        self.add(Kind::Set, v, t);
    }

    /// `linearRampToValueAtTime`.
    pub fn linear_ramp(&mut self, v: f64, ending_at: f64) {
        self.add(Kind::Linear, v, ending_at);
    }

    /// `exponentialRampToValueAtTime`.
    pub fn exponential_ramp(&mut self, v: f64, ending_at: f64) {
        let v = if Self::eq(v, 0.0) {
            Self::MIN_OUTPUT
        } else {
            v
        };
        self.add(Kind::Exponential, v, ending_at);
    }

    /// `cancelScheduledValues`.
    pub fn cancel_scheduled_values(&mut self, t: f64) {
        self.cancel(t);
    }

    /// `cancelAndHoldAtTime`.
    pub fn cancel_and_hold(&mut self, t: f64) {
        let held = self.value_at(t);
        let b = self.search(t);
        let a: i64 = if b + 1 < self.count as i64 { b + 1 } else { -1 };
        if b >= 0 && Self::eq(self.times[b as usize], t) {
            if a >= 0 {
                let ta = self.times[a as usize];
                self.cancel(ta);
            } else {
                self.cancel(t + self.sample_time);
            }
        } else if a >= 0 {
            let after_time = self.times[a as usize];
            let after_kind = self.kinds[a as usize];
            self.cancel(after_time);
            match after_kind {
                Kind::Linear => self.linear_ramp(held, t),
                Kind::Exponential => self.exponential_ramp(held, t),
                Kind::Set => {}
            }
        }
        self.add(Kind::Set, held, t);
    }

    /// `setRampPoint`.
    pub fn set_ramp_point(&mut self, t: f64) {
        let mut current = self.value_at(t);
        self.cancel_and_hold(t);
        if current == 0.0 {
            current = Self::MIN_OUTPUT;
        }
        self.set_value(current, t);
    }

    /// `rampTo(value, rampTime)` called at `now` (no start time: Tone uses `now()`).
    pub fn ramp_to(&mut self, v: f64, duration: f64, now: f64) {
        self.set_ramp_point(now);
        if self.exponential_ramps {
            self.exponential_ramp(v, now + duration);
        } else {
            self.linear_ramp(v, now + duration);
        }
    }

    /// The `value` setter at `now`: cancel, then set.
    pub fn set_immediate(&mut self, v: f64, now: f64) {
        self.cancel_scheduled_values(now);
        self.set_value(v, now);
    }

    // MARK: rendering

    /// No event after `t`: the value stays what it is at `t` from here on.
    pub fn is_settled(&self, t: f64) -> bool {
        self.count == 0 || self.times[self.count - 1] <= t
    }

    /// Drop events that no query at `now` or later can reach: everything before the last
    /// event that is a millisecond or more in the past.
    pub fn prune(&mut self, now: f64) {
        let mut keep: i64 = -1;
        for i in 0..self.count {
            if self.times[i] < now - 1e-3 {
                keep = i as i64;
            }
        }
        if keep > 0 {
            self.remove_first(keep as usize);
        }
    }
}
