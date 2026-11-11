//! Port of `Beats/Wave/WaveHistory.swift`: a reversible net clock.
//!
//! beats' jog lab ran its Petri clock backwards by un-firing the transitions whose
//! outputs held the token. The same holds here, but for the whole executor: the log
//! records what every recent tick fired and every state change it made (mutes, slot
//! choices, macro timers, the beat-repeat counter), so `unadvance` can undo a tick
//! exactly - including the control nets that drain tokens, which are not conservative.
//!
//! The log is fixed-size (512 ticks = 32 bars, fewer if the ticks are busy) and
//! preallocated; nothing here allocates.

use super::net::{CompiledNet, WaveGraph};

// change record: 6 i32 - [code, a, b, c, d, e]
//   0  muted[a] was b
//   1  mutedGroups[a] was b
//   2  activeSlot[a] was b
//   3  timer slot a was (kind b, net c, remaining d, arg e)
//   4  repeater was (remaining a, elapsed b, step c, burst d)
const CHANGE_SIZE: usize = 6;

impl WaveGraph {
    pub(crate) fn clear_history(&mut self) {
        self.h_ticks = 0;
        self.h_fire_count = 0;
        self.h_change_count = 0;
    }

    /// The beat repeater's (remaining, elapsed) counters.
    pub fn repeater_state(&self) -> (i64, i64) {
        (self.repeat_remaining, self.repeat_elapsed)
    }

    /// How many ticks `unadvance` can still undo.
    pub fn rewindable_ticks(&self) -> usize {
        self.h_ticks
    }

    pub(crate) fn log_fire(&mut self, net: usize, trans: usize) {
        if !self.recording || self.h_fire_count + 2 > self.h_fires.len() {
            return;
        }
        self.h_fires[self.h_fire_count] = net as i32;
        self.h_fires[self.h_fire_count + 1] = trans as i32;
        self.h_fire_count += 2;
    }

    pub(crate) fn log_change(&mut self, code: i32, a: i64, b: i64, c: i64, d: i64, e: i64) {
        if !self.recording || self.h_change_count + CHANGE_SIZE > self.h_changes.len() {
            return;
        }
        let i = self.h_change_count;
        self.h_changes[i] = code;
        self.h_changes[i + 1] = a as i32;
        self.h_changes[i + 2] = b as i32;
        self.h_changes[i + 3] = c as i32;
        self.h_changes[i + 4] = d as i32;
        self.h_changes[i + 5] = e as i32;
        self.h_change_count += CHANGE_SIZE;
    }

    /// `muted[i] = v`, logged so a rewind restores the old value.
    pub(crate) fn set_muted(&mut self, i: usize, v: u8) {
        if self.muted[i] == v {
            return;
        }
        self.log_change(0, i as i64, self.muted[i] as i64, 0, 0, 0);
        self.muted[i] = v;
    }

    pub(crate) fn set_timer(&mut self, i: usize, kind: u8, net: i32, remaining: i32, arg: i32) {
        self.log_change(
            3,
            i as i64,
            self.timer_kind[i] as i64,
            self.timer_net[i] as i64,
            self.timer_remaining[i] as i64,
            self.timer_arg[i] as i64,
        );
        self.timer_kind[i] = kind;
        self.timer_net[i] = net;
        self.timer_remaining[i] = remaining;
        self.timer_arg[i] = arg;
    }

    pub(crate) fn log_repeat(&mut self) {
        self.log_change(
            4,
            self.repeat_remaining,
            self.repeat_elapsed,
            self.repeat_step,
            self.repeat_burst,
            0,
        );
    }

    /// Make room for one more tick in the log, dropping the oldest ticks if it is full.
    pub(crate) fn ensure_history_room(&mut self) {
        // a tick fires at most one transition per enabled transition: bound it by the net sizes
        if self.fire_need_cache < 0 {
            let total: usize = self.nets.iter().map(|n| n.n_trans).sum();
            self.fire_need_cache = (self.h_fires.len() / 4).min(total * 2) as i64;
        }
        let fire_need = self.fire_need_cache as usize;
        let change_need = (self.h_changes.len() / 4).min(4096);
        while self.h_ticks > 0
            && (self.h_ticks >= Self::HISTORY_TICKS
                || self.h_fire_count + fire_need > self.h_fires.len()
                || self.h_change_count + change_need > self.h_changes.len())
        {
            self.drop_oldest_tick();
        }
    }

    fn drop_oldest_tick(&mut self) {
        let fire_cut = self.h_fire_end[0] as usize;
        let change_cut = self.h_change_end[0] as usize;
        self.h_fires.copy_within(fire_cut..self.h_fire_count, 0);
        self.h_changes
            .copy_within(change_cut..self.h_change_count, 0);
        for i in 1..self.h_ticks.max(1) {
            self.h_fire_end[i - 1] = self.h_fire_end[i] - fire_cut as i32;
            self.h_change_end[i - 1] = self.h_change_end[i] - change_cut as i32;
        }
        self.h_fire_count -= fire_cut;
        self.h_change_count -= change_cut;
        self.h_ticks -= 1;
    }

    pub(crate) fn end_tick_record(&mut self) {
        if !self.recording {
            return;
        }
        self.h_fire_end[self.h_ticks] = self.h_fire_count as i32;
        self.h_change_end[self.h_ticks] = self.h_change_count as i32;
        self.h_ticks += 1;
    }

    /// Undo the last tick: un-fire what it fired (newest first), restore what it changed,
    /// and step the tick counter back. `on_undo(net, transition)` is called for each undone
    /// fire that was audible - the notes a scratch plays. False when there is nothing left
    /// to undo.
    pub fn unadvance<F: FnMut(&CompiledNet, usize)>(&mut self, mut on_undo: F) -> bool {
        if self.h_ticks == 0 {
            return false;
        }
        let last = self.h_ticks - 1;
        let fire_start = if last == 0 {
            0
        } else {
            self.h_fire_end[last - 1] as usize
        };
        let change_start = if last == 0 {
            0
        } else {
            self.h_change_end[last - 1] as usize
        };
        let fire_end = self.h_fire_end[last] as usize;
        let change_end = self.h_change_end[last] as usize;

        let mut i = fire_end as i64 - 2;
        while i >= fire_start as i64 {
            let ni = self.h_fires[i as usize] as usize;
            let t = self.h_fires[i as usize + 1] as usize;
            let n = &self.nets[ni];
            if n.midi[t]
                && self.muted[ni] == 0
                && self.muted_notes[ni * 128 + (n.note[t] & 127) as usize] == 0
            {
                on_undo(n, t);
            }
            self.nets[ni].unfire(t);
            i -= 2;
        }
        let mut c = change_end;
        while c > change_start {
            c -= CHANGE_SIZE;
            let a = self.h_changes[c + 1] as i64;
            let b = self.h_changes[c + 2] as i64;
            match self.h_changes[c] {
                0 => self.muted[a as usize] = b as u8,
                1 => self.muted_groups[a as usize] = b as u8,
                2 => self.active_slot[a as usize] = b,
                3 => {
                    let a = a as usize;
                    self.timer_kind[a] = b as u8;
                    self.timer_net[a] = self.h_changes[c + 3];
                    self.timer_remaining[a] = self.h_changes[c + 4];
                    self.timer_arg[a] = self.h_changes[c + 5];
                }
                4 => {
                    self.repeat_remaining = a;
                    self.repeat_elapsed = b;
                    self.repeat_step = self.h_changes[c + 3] as i64;
                    self.repeat_burst = self.h_changes[c + 4] as i64;
                }
                _ => {}
            }
        }
        self.h_fire_count = fire_start;
        self.h_change_count = change_start;
        self.h_ticks -= 1;
        self.tick -= 1;
        self.fired_count = 0;
        self.control_count = 0;
        self.stop_requested = false;
        true
    }
}
