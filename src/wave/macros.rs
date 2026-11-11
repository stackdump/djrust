//! Port of `Beats/Wave/WaveMacros.swift`: live-performance controls on the net executor:
//! role mutes, slot launches, macros, stingers, fast-forward.
//!
//! These are the Rust side of what beats' worker does for `mute-group`, `fire-macro` and
//! friends (sequencer-worker.js, lib/macros/*): a mute now, its restore N ticks later,
//! locked to the net clock, as fixed-capacity timers instead of injected chain nets, so
//! nothing here allocates.

use super::net::WaveGraph;

/// `MacroTargets`: which music nets a mute macro hits (`MACRO_TARGETS` in macros/catalog.js).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum MacroTargets {
    NonDrums = 0,
    DrumsOnly = 1,
    Everything = 2,
    /// Everything but the kick: beat repeat keeps a pulse under the stutter.
    NonKick = 3,
}

impl MacroTargets {
    fn from_raw(r: u8) -> Option<MacroTargets> {
        match r {
            0 => Some(MacroTargets::NonDrums),
            1 => Some(MacroTargets::DrumsOnly),
            2 => Some(MacroTargets::Everything),
            3 => Some(MacroTargets::NonKick),
            _ => None,
        }
    }
}

/// `NetMacro`: the net-native macros of `lib/macros/catalog.js` (mute, beat-repeat,
/// compound and tempo kinds). FX, pitch and feel macros are `MasterMacro` (another module).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum NetMacro {
    Drop = 0,
    Breakdown,
    SoloDrums,
    Cut,
    DoubleDrop,
    BeatRepeat,
    HalfTime,
    TapeStop,
    TempoAnchor,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MacroUnit {
    Bar,
    Tick,
}

impl NetMacro {
    pub const ALL: [NetMacro; 9] = [
        NetMacro::Drop,
        NetMacro::Breakdown,
        NetMacro::SoloDrums,
        NetMacro::Cut,
        NetMacro::DoubleDrop,
        NetMacro::BeatRepeat,
        NetMacro::HalfTime,
        NetMacro::TapeStop,
        NetMacro::TempoAnchor,
    ];

    pub fn id(self) -> &'static str {
        match self {
            NetMacro::Drop => "drop",
            NetMacro::Breakdown => "breakdown",
            NetMacro::SoloDrums => "solo-drums",
            NetMacro::Cut => "cut",
            NetMacro::DoubleDrop => "double-drop",
            NetMacro::BeatRepeat => "beat-repeat",
            NetMacro::HalfTime => "half-time",
            NetMacro::TapeStop => "tape-stop",
            NetMacro::TempoAnchor => "tempo-anchor",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            NetMacro::Drop => "Drop",
            NetMacro::Breakdown => "Breakdown",
            NetMacro::SoloDrums => "Solo Drums",
            NetMacro::Cut => "Cut",
            NetMacro::DoubleDrop => "Double Drop",
            NetMacro::BeatRepeat => "Beat Repeat",
            NetMacro::HalfTime => "Half Time",
            NetMacro::TapeStop => "Tape Stop",
            NetMacro::TempoAnchor => "Anchor",
        }
    }

    pub fn default_duration(self) -> i64 {
        match self {
            NetMacro::Drop | NetMacro::DoubleDrop | NetMacro::BeatRepeat | NetMacro::TapeStop => 1,
            _ => 2,
        }
    }

    pub fn unit(self) -> MacroUnit {
        if self == NetMacro::Cut {
            MacroUnit::Tick
        } else {
            MacroUnit::Bar
        }
    }

    /// Ticks for a duration in this macro's own unit (a bar is 16 ticks).
    pub fn ticks(self, duration: Option<i64>) -> i64 {
        let d = duration.unwrap_or(self.default_duration());
        if self.unit() == MacroUnit::Bar {
            d * 16
        } else {
            d
        }
    }

    pub fn from_id(id: &str) -> Option<NetMacro> {
        NetMacro::ALL.iter().copied().find(|m| m.id() == id)
    }

    /// `tag` (1-based, rides on the mute timers).
    pub fn tag(self) -> u8 {
        self as u8 + 1
    }

    /// Its bit in `MacroSnapshot.mask`.
    pub fn bit(self) -> u32 {
        1 << (self as u32)
    }
}

impl WaveGraph {
    /// A mute macro that starts `delay` ticks from now. `macro_tag` is the `NetMacro::tag`
    /// it belongs to (0: none): the delayed mutes carry it. The timer's `arg` holds the
    /// mute length in its low 16 bits and the tag above them.
    pub fn schedule_mute(&mut self, which: MacroTargets, after: i64, ticks: i64, macro_tag: u8) {
        for i in 0..Self::TIMER_CAPACITY {
            if self.timer_kind[i] == 0 {
                let arg = (ticks.clamp(0, 0xFFFF) as i32) | ((macro_tag as i32) << 16);
                self.set_timer(i, 3, which as i32, after.max(1) as i32, arg);
                return;
            }
        }
        self.timer_overflow += 1;
    }

    // MARK: roles and slots

    /// Mute or unmute a role by `strHash(role)`: a riff group (all its slot nets) or a
    /// plain net. Unmuting a group brings back the slot it last activated.
    pub fn set_role_muted(&mut self, hash: i32, muted: bool) {
        let Some(&t) = self.role_targets.get(&hash) else {
            return;
        };
        if t >= 0 {
            self.set_group_muted(t as usize, muted);
        } else {
            self.muted[(-t - 2) as usize] = muted as u8;
        }
    }

    pub(crate) fn set_group_muted(&mut self, gi: usize, muted: bool) {
        if muted {
            self.muted_groups[gi] = 1;
            for k in 0..self.group_nets[gi].len() {
                let i = self.group_nets[gi][k];
                self.muted[i] = 1;
            }
        } else {
            self.muted_groups[gi] = 0;
            let pick = if self.active_slot[gi] >= 0 {
                self.active_slot[gi]
            } else {
                self.group_nets[gi].first().map_or(-1, |&i| i as i64)
            };
            if pick >= 0 {
                for k in 0..self.group_nets[gi].len() {
                    let i = self.group_nets[gi][k];
                    if i as i64 != pick {
                        self.muted[i] = 1;
                    }
                }
                self.muted[pick as usize] = 0;
            }
        }
    }

    /// Whether a role currently makes sound (any slot unmuted / the net unmuted).
    /// The nets (indices into `nets`) a role names: its riff group's nets, or the one net.
    /// `None` for a role the song does not have. Main-thread helper for hosts that answer
    /// "is this role audible" from the mute mirror without touching the graph.
    pub fn role_net_indices(&self, hash: i32) -> Option<alloc::vec::Vec<usize>> {
        let &t = self.role_targets.get(&hash)?;
        Some(if t >= 0 {
            self.group_nets[t as usize].clone()
        } else {
            alloc::vec![(-t - 2) as usize]
        })
    }

    /// A copy of the role tables (`roleTargets`, `groupNets`): what the main thread keeps of a
    /// project once it has handed it to the audio thread.
    pub fn role_tables(&self) -> super::net::RoleTables {
        super::net::RoleTables {
            role_targets: self.role_targets.clone(),
            group_nets: self.group_nets.clone(),
        }
    }

    /// The nets a role names, without allocating (`roleTargets` / `groupNets`).
    pub(crate) fn nets_of_role(&self, hash: i32) -> Option<super::net::RoleNets<'_>> {
        let &t = self.role_targets.get(&hash)?;
        Some(if t >= 0 {
            super::net::RoleNets::Group(&self.group_nets[t as usize])
        } else {
            super::net::RoleNets::Net((-t - 2) as usize)
        })
    }

    pub fn is_role_audible(&self, hash: i32) -> bool {
        let Some(&t) = self.role_targets.get(&hash) else {
            return false;
        };
        if t >= 0 {
            return self.group_nets[t as usize]
                .iter()
                .any(|&i| self.muted[i] == 0);
        }
        self.muted[(-t - 2) as usize] == 0
    }

    /// `activate-slot` aimed at a net by `strHash(netId)` (e.g. "bass-2").
    pub fn activate_slot(&mut self, hash: i32) {
        let Some(&tgt) = self.by_hash.get(&hash) else {
            return;
        };
        let gi = self.nets[tgt].group_idx;
        if gi < 0 {
            return;
        }
        for o in 0..self.nets.len() {
            if self.nets[o].group_idx == gi && o != tgt {
                self.muted[o] = 1;
            }
        }
        if self.muted_groups[gi as usize] == 0 {
            self.muted[tgt] = 0;
        }
        self.active_slot[gi as usize] = tgt as i64;
    }

    // MARK: macros

    /// `arg` of a kind-1 (unmute) timer is the `NetMacro::tag` of the macro that muted the
    /// net (0 when no macro did).
    fn add_timer(&mut self, kind: u8, net: usize, ticks: i64, arg: i32) {
        for i in 0..Self::TIMER_CAPACITY {
            if self.timer_kind[i] == 0 {
                self.set_timer(i, kind, net as i32, ticks.max(1) as i32, arg);
                return;
            }
        }
        self.timer_overflow += 1;
    }

    /// Does a macro selector pick net `i`? (Control nets never.)
    pub(crate) fn selects(&self, which: MacroTargets, i: usize) -> bool {
        if self.is_control_net[i] {
            return false;
        }
        match which {
            MacroTargets::NonDrums => !self.is_drum_net[i],
            MacroTargets::DrumsOnly => self.is_drum_net[i],
            MacroTargets::Everything => true,
            MacroTargets::NonKick => !self.is_kick_net[i],
        }
    }

    /// Mute every audible net the selector picks and unmute it again after `ticks` ticks.
    /// Skips nets already muted, so the restore never undoes a mute someone meant.
    /// Returns how many nets it muted.
    pub fn fire_mute(&mut self, which: MacroTargets, ticks: i64, macro_tag: u8) -> usize {
        let mut n = 0;
        for i in 0..self.nets.len() {
            if self.muted[i] != 0 || !self.selects(which, i) {
                continue;
            }
            self.set_muted(i, 1);
            self.add_timer(1, i, ticks, macro_tag as i32);
            n += 1;
        }
        n
    }

    /// Beat repeat: a mute burst of `burst` ticks every `step` ticks for `ticks` ticks.
    pub fn fire_beat_repeat(&mut self, ticks: i64, step: i64, burst: i64) {
        self.log_repeat();
        self.repeat_step = step.max(1);
        self.repeat_burst = burst.max(1);
        self.repeat_elapsed = 0;
        self.repeat_remaining = ticks.max(1);
        self.fire_mute(
            MacroTargets::NonKick,
            self.repeat_burst,
            NetMacro::BeatRepeat.tag(),
        );
        self.repeat_elapsed = 1;
        self.repeat_remaining -= 1;
    }

    /// Unmute a stinger net (`hit1`…) for `ticks` ticks, then mute it again.
    pub fn fire_stinger(&mut self, hash: i32, ticks: i64) {
        let Some(&i) = self.by_hash.get(&hash) else {
            return;
        };
        self.set_muted(i, 0);
        // Replace any pending restore for this net so a re-fire extends rather than stacks.
        for k in 0..Self::TIMER_CAPACITY {
            if self.timer_kind[k] == 2 && self.timer_net[k] as usize == i {
                self.set_timer(k, 0, 0, 0, 0);
            }
        }
        self.add_timer(2, i, ticks, 0);
    }

    /// Run all pending restores now and stop the repeater (the panic button).
    pub fn cancel_macros(&mut self) {
        for i in 0..Self::TIMER_CAPACITY {
            if self.timer_kind[i] != 0 {
                if self.timer_kind[i] != 3 {
                    let v = if self.timer_kind[i] == 1 { 0 } else { 1 };
                    self.set_muted(self.timer_net[i] as usize, v);
                }
                self.set_timer(i, 0, 0, 0, 0);
            }
        }
        self.log_repeat();
        self.repeat_remaining = 0;
    }

    /// Drop all macro state without restoring anything (reset / fast-forward).
    pub(crate) fn clear_macros(&mut self) {
        for k in self.timer_kind.iter_mut() {
            *k = 0;
        }
        self.repeat_remaining = 0;
    }

    /// Is any macro in effect: a pending restore or a running beat repeat?
    pub fn has_active_macros(&self) -> bool {
        self.repeat_remaining > 0 || self.timer_kind.iter().any(|&k| k != 0)
    }

    /// Number of pending tick-locked restores (UI / tests).
    pub fn pending_macro_timers(&self) -> usize {
        self.timer_kind.iter().filter(|&&k| k != 0).count()
    }

    /// Called at the end of every tick, after all nets (a macro's restore net sits last in
    /// the project, so its last transition fires on the Nth tick after injection).
    pub(crate) fn service_macros(&mut self) {
        for i in 0..Self::TIMER_CAPACITY {
            if self.timer_kind[i] == 0 {
                continue;
            }
            let remaining = self.timer_remaining[i] - 1;
            if remaining <= 0 {
                let kind = self.timer_kind[i];
                let net = self.timer_net[i];
                let arg = self.timer_arg[i];
                self.set_timer(i, 0, 0, 0, 0);
                if kind == 3 {
                    self.fire_mute(
                        MacroTargets::from_raw(net as u8).unwrap_or(MacroTargets::NonDrums),
                        (arg & 0xFFFF) as i64,
                        (arg >> 16) as u8,
                    );
                } else {
                    self.set_muted(net as usize, if kind == 1 { 0 } else { 1 });
                }
            } else {
                let (k, n, a) = (self.timer_kind[i], self.timer_net[i], self.timer_arg[i]);
                self.set_timer(i, k, n, remaining, a);
            }
        }
        if self.repeat_remaining > 0 {
            self.log_repeat();
            if self.repeat_elapsed % self.repeat_step == 0 {
                self.fire_mute(
                    MacroTargets::NonKick,
                    self.repeat_burst,
                    NetMacro::BeatRepeat.tag(),
                );
            }
            self.repeat_elapsed += 1;
            self.repeat_remaining -= 1;
        }
    }

    // MARK: seeking

    /// Rebuild the marking at `tick` by replaying from the start without sound (what the
    /// sequencer worker's `fastForwardTo` does). Not for the audio thread: it is O(tick).
    pub fn fast_forward(&mut self, target: i64) {
        self.reset();
        self.recording = false;
        let mut n = 0;
        while n < target && !self.stop_requested {
            self.advance();
            n += 1;
        }
        self.stop_requested = false;
        self.recording = true;
        self.clear_history();
    }
}
