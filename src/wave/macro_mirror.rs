//! Port of `Beats/Wave/MacroMirror.swift`: what the audio thread tells the display about
//! running macros (SPEC mode 26): which of the [`NetMacro`]s are in effect and which nets each
//! of them is holding muted. The numbers are built once per render block from timers the
//! executor already keeps (`WaveGraph`'s tick-locked restores, the beat repeater, the runner's
//! tempo effect), published as one consistent record through [`MacroMirror`] and read on the
//! main thread. Nothing here allocates on the audio thread.
//!
//! Ownership. A net is muted at most once: `fire_mute` skips a net that is already muted, so
//! the timer that will restore it belongs to exactly one macro and carries that macro's tag.
//! The held sets are therefore kept per macro (a macro fired over another one's mutes is shown
//! holding only what it muted itself).
//!
//! Which project. The record carries the generation of the graph the audio thread was running
//! when it was built. Net indices mean something only in that graph, and a project handed over
//! while a macro runs is installed only at the next bar, so the display resolves roles through
//! the latest project's tables only when the generations agree ([`roles_held`]).
//!
//! The pieces, with the Swift they mirror:
//!
//! - [`MacroSnapshot`] is `MacroSnapshot` (the 16-macro, 256-net record, bit packing included);
//! - [`WaveGraph::macro_snapshot`] is `WaveGraph.macroSnapshot` (`WaveMacros.swift`), and the
//!   runner's own `WaveRunner::macro_snapshot` adds the tempo macros;
//! - [`MacroMirror`] is the seqlock-published record;
//! - [`roles_held`] is `WaveDeckSource.heldRoles`, the main thread's half.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::sync::atomic::{fence, AtomicI32, AtomicU32, AtomicU64, Ordering};

use super::macros::{MacroTargets, NetMacro};
use super::net::{str_hash, RoleNets, RoleTables, WaveGraph};

/// One block's worth of macro state: `mask` bit `k` is `NetMacro` number `k`; `held` is a
/// bitset over the first 256 nets for each macro, four words each (macro `k`, net `i`: bit
/// `i % 64` of word `4k + i / 64`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MacroSnapshot {
    pub mask: u32,
    pub held: [u64; 64],
}

impl Default for MacroSnapshot {
    fn default() -> Self {
        MacroSnapshot {
            mask: 0,
            held: [0; 64],
        }
    }
}

impl MacroSnapshot {
    /// Words per macro: 256 nets, like the mute mirror.
    pub const WORDS_PER_MACRO: usize = 4;
    /// How many macros the record has room for.
    pub const CAPACITY: usize = 16;

    /// Mark net `i` as held muted by macro `slot` (`NetMacro` number). Nets past 255 are not
    /// tracked, like the mute mirror.
    pub fn hold(&mut self, net: i64, slot: i64) {
        if !(0..256).contains(&net) || !(0..Self::CAPACITY as i64).contains(&slot) {
            return;
        }
        let (i, slot) = (net as usize, slot as usize);
        self.held[slot * Self::WORDS_PER_MACRO + i / 64] |= 1u64 << (i % 64);
    }

    /// Is net `i` held by `m`?
    pub fn is_held_by(&self, net: i64, m: NetMacro) -> bool {
        if !(0..256).contains(&net) {
            return false;
        }
        let i = net as usize;
        self.held[m as usize * Self::WORDS_PER_MACRO + i / 64] & (1u64 << (i % 64)) != 0
    }

    /// Is net `i` held by any macro?
    pub fn is_held(&self, net: i64) -> bool {
        if !(0..256).contains(&net) {
            return false;
        }
        let i = net as usize;
        (0..Self::CAPACITY)
            .any(|k| self.held[k * Self::WORDS_PER_MACRO + i / 64] & (1u64 << (i % 64)) != 0)
    }

    /// `WaveDeckSource.isActive(_ macro:)` over a record: is `m`'s bit in the mask?
    pub fn is_active(&self, m: NetMacro) -> bool {
        self.mask & m.bit() != 0
    }

    /// `WaveDeckSource.activeMacros` over a record: the macros in the mask, in catalogue order.
    pub fn active_macros(&self) -> Vec<NetMacro> {
        NetMacro::ALL
            .iter()
            .copied()
            .filter(|m| self.mask & m.bit() != 0)
            .collect()
    }
}

impl WaveGraph {
    /// Which macros are in effect, and which nets each is holding muted, for the display:
    /// bit `k` of `mask` is `NetMacro` number `k`; nets below 256 whose pending restore a macro
    /// owns (a mute timer carrying its tag) are in that macro's part of `held`. A beat repeat
    /// counts while it runs, even between its bursts. Tempo macros live on the runner
    /// (`WaveRunner::macro_snapshot`). Allocation-free: this runs on the audio thread once per
    /// render block. Port of `WaveGraph.macroSnapshot`.
    pub fn macro_snapshot(&self) -> MacroSnapshot {
        let mut s = MacroSnapshot::default();
        if self.repeat_remaining > 0 {
            s.mask |= NetMacro::BeatRepeat.bit();
        }
        for i in 0..Self::TIMER_CAPACITY {
            if self.timer_kind[i] == 0 {
                continue;
            }
            let tag: i32 = match self.timer_kind[i] {
                1 => self.timer_arg[i],
                3 => self.timer_arg[i] >> 16,
                _ => 0,
            };
            if tag <= 0 || tag > 32 {
                continue;
            }
            s.mask |= 1u32 << (tag - 1) as u32;
            if self.timer_kind[i] == 1 {
                s.hold(self.timer_net[i] as i64, (tag - 1) as i64);
            }
        }
        s
    }

    /// Does a macro selector pick net `i`? (Control nets never.) The one rule `fire_mute`
    /// mutes by, and the display asks to know which roles a running macro touches. Public
    /// face of `selects`.
    pub fn macro_selects(&self, which: MacroTargets, net: usize) -> bool {
        self.selects(which, net)
    }

    /// Is net `i` a drum net (a drum channel)? Read-only face of the table `selects` uses.
    pub fn is_drum_net_at(&self, net: usize) -> bool {
        self.is_drum_net[net]
    }

    /// Is net `i` a control net (it steers other nets and never sounds)?
    pub fn is_control_net_at(&self, net: usize) -> bool {
        self.is_control_net[net]
    }

    /// Is net `i` a stinger (`hit1`...)?
    pub fn is_stinger_net_at(&self, net: usize) -> bool {
        self.is_stinger_net[net]
    }
}

/// The latest whole record a reader got, with the generation of the graph it was built from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MacroRead {
    pub snapshot: MacroSnapshot,
    /// `WaveProject::generation` of the graph the record was built from (0: none yet).
    pub generation: i32,
}

/// The published record: the audio thread (the only writer) fills it once per render block,
/// any thread reads it, and a reader never sees half of one block's record and half of
/// another's. A seqlock: the writer makes the sequence odd, writes, makes it even; a reader
/// that finds it odd, or changed after its copy, tries again. All the words are atomics, so no
/// read races a write; no allocation, no lock.
///
/// Port of `MacroMirror`. Build it once, off the audio thread; share it by `Arc`.
pub struct MacroMirror {
    sequence: AtomicU32,
    mask_bits: AtomicU32,
    generation_bits: AtomicI32,
    words: Box<[AtomicU64]>,
}

impl Default for MacroMirror {
    fn default() -> Self {
        MacroMirror::new()
    }
}

impl MacroMirror {
    /// Words in use: every macro of the catalogue has its four.
    const COUNT: usize = NetMacro::ALL.len() * MacroSnapshot::WORDS_PER_MACRO;

    pub fn new() -> MacroMirror {
        const _: () =
            assert!(MacroMirror::COUNT <= MacroSnapshot::CAPACITY * MacroSnapshot::WORDS_PER_MACRO);
        MacroMirror {
            sequence: AtomicU32::new(0),
            mask_bits: AtomicU32::new(0),
            generation_bits: AtomicI32::new(0),
            words: (0..Self::COUNT).map(|_| AtomicU64::new(0)).collect(),
        }
    }

    /// Audio thread, once per render block.
    pub fn publish(&self, s: &MacroSnapshot, generation: i32) {
        let seq = self.sequence.load(Ordering::Relaxed);
        self.sequence.store(seq.wrapping_add(1), Ordering::Relaxed); // odd: a write is in progress
        fence(Ordering::Release); // ...and that is visible before any word changes
        self.mask_bits.store(s.mask, Ordering::Relaxed);
        self.generation_bits.store(generation, Ordering::Relaxed);
        for (w, v) in self.words.iter().zip(s.held.iter()) {
            w.store(*v, Ordering::Relaxed);
        }
        self.sequence.store(seq.wrapping_add(2), Ordering::Release); // even: the record is whole
    }

    /// The latest whole record; None if a writer kept getting in the way (the caller keeps
    /// its last).
    pub fn read(&self) -> Option<MacroRead> {
        for _ in 0..16 {
            let before = self.sequence.load(Ordering::Acquire);
            if before & 1 != 0 {
                continue;
            }
            let mut r = MacroRead::default();
            r.snapshot.mask = self.mask_bits.load(Ordering::Relaxed);
            r.generation = self.generation_bits.load(Ordering::Relaxed);
            for (i, w) in self.words.iter().enumerate() {
                r.snapshot.held[i] = w.load(Ordering::Relaxed);
            }
            fence(Ordering::Acquire);
            if self.sequence.load(Ordering::Relaxed) == before {
                return Some(r);
            }
        }
        None
    }
}

/// Of `roles` (riff-group or plain-net role names), those with a net that the record holds
/// muted (`held(snapshot, net)`: one macro's own mutes, or any macro's) and that is still muted
/// (`is_net_muted`: a role the performer switched back on during the macro is no longer held),
/// in the order given. Reads the latest project's role table like `isRoleAudible`; allocates
/// its result. Empty while the record comes from another project than the latest one (a
/// hand-over waiting for its bar): the nets it names are the running graph's, the tables are
/// the new one's. Main thread.
///
/// Port of `WaveDeckSource.heldRoles` (`rolesHeld(by:among:)` is `held = is_held_by(macro)`,
/// `rolesHeldByMacros` is `held = is_held`). `latest` is the latest project loaded:
/// its generation and a graph whose tables (not its state) are read.
pub fn roles_held<'a>(
    read: &MacroRead,
    latest: Option<(i32, &WaveGraph)>,
    roles: &[&'a str],
    held: impl Fn(&MacroSnapshot, i64) -> bool,
    is_net_muted: impl Fn(usize) -> bool,
) -> Vec<&'a str> {
    let Some((generation, g)) = latest else {
        return Vec::new();
    };
    held_roles(
        read,
        generation,
        |hash| g.nets_of_role(hash),
        roles,
        held,
        is_net_muted,
    )
}

/// [`roles_held`] against the role tables the deck kept of the latest project
/// ([`RoleTables`], `WaveDeckSource.latest`'s graph after it has gone to the audio thread).
pub fn roles_held_in<'a>(
    read: &MacroRead,
    latest: Option<(i32, &RoleTables)>,
    roles: &[&'a str],
    held: impl Fn(&MacroSnapshot, i64) -> bool,
    is_net_muted: impl Fn(usize) -> bool,
) -> Vec<&'a str> {
    let Some((generation, t)) = latest else {
        return Vec::new();
    };
    held_roles(
        read,
        generation,
        |hash| t.nets_of(hash),
        roles,
        held,
        is_net_muted,
    )
}

fn held_roles<'t, 'a>(
    read: &MacroRead,
    generation: i32,
    nets_of: impl Fn(i32) -> Option<RoleNets<'t>>,
    roles: &[&'a str],
    held: impl Fn(&MacroSnapshot, i64) -> bool,
    is_net_muted: impl Fn(usize) -> bool,
) -> Vec<&'a str> {
    if generation != read.generation {
        return Vec::new();
    }
    roles
        .iter()
        .copied()
        .filter(|role| {
            let Some(nets) = nets_of(str_hash(role)) else {
                return false;
            };
            let one;
            let nets: &[usize] = match nets {
                RoleNets::Group(g) => g,
                RoleNets::Net(n) => {
                    one = [n];
                    &one
                }
            };
            nets.iter()
                .any(|&n| held(&read.snapshot, n as i64) && is_net_muted(n))
        })
        .collect()
}
