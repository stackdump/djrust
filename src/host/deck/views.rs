//! What a display draws from a deck, read from its mirrors and the tables the main thread
//! keeps: `Deck.netSnapshot` (`NetSnapshot.swift`), `Deck.waveformOverlay` (`Waveform.swift`)
//! and `Deck.sectionTimeline` (`SectionTimeline.swift`). The assembly is `crate::deck`'s;
//! this file hands it the deck's state.
//!
//! And the feeds a display polls: `Deck.macroActivity` (`MacroChase.swift`: which macros are in
//! effect and which roles each holds muted, from the macro mirror), the marking mirror's readers
//! (`readMarking`, `markingLayout`) and `Deck.advances` for the ring's spin.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::control::chase::MacroActivity;
use crate::deck::snapshot::{net_snapshot, NetSnapshot};
use crate::deck::timeline::SectionTimeline;
use crate::deck::waveform::WaveformOverlay;
use crate::wave::{EventCursor, FireEvent, MarkingLayout, MarkingSnapshot, TickAnchor};

use super::engine::Engine;

impl Engine {
    /// `Deck.netSnapshot`: the audible ring of each role the song has (kick, snare, ..., and
    /// the four hits), with token positions derived from the net clock. None unless the deck
    /// plays a net.
    pub fn net_snapshot(&self, deck: &str) -> Option<NetSnapshot> {
        let d = self.deck(deck)?;
        if !d.is_net() {
            return None;
        }
        let project = d.project.as_ref()?;
        let roles = d
            .ring_roles
            .iter()
            .map(|(r, nets)| (r.as_str(), nets.as_slice()));
        let snapshot = net_snapshot(
            d.mirror.tick(),
            d.mirror.tick_phase(),
            roles,
            &|i| d.mirror.is_net_muted(i),
            &|id| project.nets.get(id),
            &mut d.ring_shapes.borrow_mut(),
        );
        Some(snapshot)
    }

    /// `Deck.waveformOverlay`: the playhead, the loop, the cues and the bar lines over a sample
    /// deck's waveform, as fractions of its length. None on a net deck.
    pub fn waveform_overlay(&self, deck: &str) -> Option<WaveformOverlay> {
        let d = self.deck(deck)?;
        let s = d.sample.as_ref()?;
        WaveformOverlay::of_sample_deck(
            s.buffer.length(),
            d.mirror.turntable_position(),
            s.loop_start,
            s.loop_end,
            s.looping,
            &d.hot_cues,
            s.grid.as_ref(),
            self.sample_rate,
        )
    }

    /// `Deck.sectionTimeline`: the section bar of a song deck. None for a loop, a sample deck
    /// or an empty one.
    pub fn section_timeline(&self, deck: &str) -> Option<SectionTimeline> {
        let d = self.deck(deck)?;
        if !d.is_net() {
            return None;
        }
        SectionTimeline::of_sections(
            &d.sections,
            d.mirror.tick.load(Ordering::Relaxed),
            d.mirror.tick_phase(),
        )
    }

    /// `Deck.macroActivity`: the macros the engine reports in effect on this deck and, for the
    /// ones the JS chases, the roles each holds muted: a role is a target of a macro while a net
    /// of it waits for that macro's own restore (so a macro fired over another one's mutes chases
    /// only what it muted itself). Idle on a sample deck or while nothing runs. Main thread.
    pub fn macro_activity(&self, deck: &str) -> MacroActivity {
        let Some(d) = self.deck(deck).filter(|d| d.is_net()) else {
            return MacroActivity::idle();
        };
        let running = d.mirror.active_macros();
        if running.is_empty() {
            return MacroActivity::idle();
        }
        let roles: Vec<String> = d.roles.iter().map(|(r, _)| r.clone()).collect();
        let among: Vec<&str> = roles.iter().map(String::as_str).collect();
        let latest = d
            .track
            .latest
            .as_ref()
            .map(|l| (l.generation, &l.role_tables));
        let mut activity = MacroActivity::idle();
        for m in running {
            if m.chase_targets().is_some() {
                let held = d.mirror.roles_held_by_in(latest, m, &among);
                activity
                    .targets
                    .insert(m, held.into_iter().map(String::from).collect());
            }
            activity.running.push(m);
        }
        activity
    }

    /// `Deck.advances`: how many times the deck has handed over to its pre-composed next track
    /// (`advanceToNext`), which the ring's Auto-DJ swing watches
    /// ([`RingSpin::observe`](crate::control::ring::RingSpin::observe)).
    pub fn advances(&self, deck: &str) -> i64 {
        self.deck(deck).map_or(0, |d| d.track.advances as i64)
    }

    /// `readMarking(into:)` on the deck's source: the marking of the project that is sounding
    /// (every net's tokens, the mutes, the tick and which project they are from), consistent and
    /// allocation-free once `snap` is sized. On a deck that is not playing, a project loaded
    /// since the last render is the one reported; on a playing deck the old project is reported
    /// until it hands over at the bar. False if nothing is loaded. Gate on the deck's mode: the
    /// mirror stops where it was when the deck goes back to a sample.
    pub fn read_marking(&self, deck: &str, snap: &mut MarkingSnapshot) -> bool {
        self.deck(deck).is_some_and(|d| d.mirror.read_marking(snap))
    }

    /// `readMarking(generation:into:)`: a recent project's marking, by its generation.
    pub fn read_marking_of(&self, deck: &str, generation: i32, snap: &mut MarkingSnapshot) -> bool {
        self.deck(deck)
            .is_some_and(|d| d.mirror.read_marking_of(generation, snap))
    }

    /// `markingLayout`: the layout of the project loaded last (net ids, place ids, what each net
    /// is).
    pub fn marking_layout(&self, deck: &str) -> Option<Arc<MarkingLayout>> {
        self.deck(deck)?.mirror.marking_layout()
    }

    /// `markingLayout(generation:)`: the layout of a recent project, e.g. the generation stamped
    /// on a fire or an anchor.
    pub fn marking_layout_of(&self, deck: &str, generation: i32) -> Option<Arc<MarkingLayout>> {
        self.deck(deck)?.mirror.marking_layout_of(generation)
    }

    /// `eventCursorNow`: a cursor for a reader that subscribes to the deck's fires and tick
    /// anchors now.
    pub fn event_cursor_now(&self, deck: &str) -> EventCursor {
        self.deck(deck)
            .map_or_else(EventCursor::new, |d| d.mirror.event_cursor_now())
    }

    /// `readEvents`: append the deck's fires and anchors written since `cursor` (oldest first) and
    /// move it past them. How many records of each stream the reader lost: overwritten before it
    /// got to them, because it was a whole ring behind (`(0, 0)` for an unknown deck). Net mode
    /// only writes them: a sample deck makes no events but keeps counting frames.
    pub fn read_events(
        &self,
        deck: &str,
        cursor: &mut EventCursor,
        fires: &mut Vec<FireEvent>,
        anchors: &mut Vec<TickAnchor>,
    ) -> (usize, usize) {
        self.deck(deck)
            .map_or((0, 0), |d| d.mirror.read_events(cursor, fires, anchors))
    }
}
