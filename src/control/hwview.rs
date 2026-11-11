//! The drawn view of a device: a [`ControllerLayout`] (where each control sits) joined to a
//! [`DeviceMap`] (what each is bound to), plus the live part, which control just moved
//! (docs/BINDINGS-MAP.md). Pure: `djapp` paints what these say.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::control::bindmap::{Cell, DeviceMap};
use crate::control::layout::{ControlSpot, ControllerLayout};

/// One thing a spot can be: a profile control, in one mode of the unit.
#[derive(Clone, Debug, PartialEq)]
pub struct Label {
    pub group: String,
    pub index: usize,
    /// The mode the unit is in for this to be the control ("HOT CUE", "SHIFT"); None: always.
    pub mode: Option<String>,
    pub cell: Cell,
}

/// A spot with what it is bound to.
#[derive(Clone, Debug, PartialEq)]
pub struct SpotView {
    /// Index into the layout's `spots`.
    pub spot: usize,
    /// One per binding of the spot, the first being its base (unshifted) control.
    pub labels: Vec<Label>,
}

/// Is `mode` a shift layer (it is held, not chosen)?
pub fn is_shift_mode(mode: &str) -> bool {
    mode.starts_with("SHIFT")
}

impl SpotView {
    /// The label for the unit in `mode` (None: the base): the one made for that mode, else the
    /// base. Shift layers are never picked here.
    pub fn pick(&self, mode: Option<&str>) -> Option<&Label> {
        let base = self.labels.iter().find(|l| l.mode.as_deref().is_none_or(|m| !is_shift_mode(m)));
        match mode.filter(|m| !is_shift_mode(m)) {
            Some(m) => self
                .labels
                .iter()
                .find(|l| l.mode.as_deref() == Some(m))
                .or(base),
            None => base,
        }
    }

    /// The label for the shift layer, if the spot has one.
    pub fn shift_label(&self) -> Option<&Label> {
        self.labels
            .iter()
            .find(|l| l.mode.as_deref().is_some_and(is_shift_mode))
    }
}

/// Every spot of `layout` with its labels, looked up in `map`. A spot whose control the map does
/// not know (the layout is ahead of the profile) has no labels for it.
pub fn view(layout: &ControllerLayout, map: &DeviceMap) -> Vec<SpotView> {
    layout
        .spots
        .iter()
        .enumerate()
        .map(|(spot, s)| SpotView {
            spot,
            labels: s
                .bindings
                .iter()
                .filter_map(|b| {
                    map.cell(&b.group, b.index).map(|cell| Label {
                        group: b.group.clone(),
                        index: b.index,
                        mode: b.mode.clone(),
                        cell: cell.clone(),
                    })
                })
                .collect(),
        })
        .collect()
}

/// The modes a unit can be put in, in the order the layout names them: the pad modes of the
/// Inpulse ("HOT CUE", "LOOP", ...), the MPK's ("PROG", "NOTE A", ...). Shift layers are left
/// out: shift is held, not chosen.
pub fn modes(layout: &ControllerLayout) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in &layout.spots {
        for b in &s.bindings {
            if let Some(m) = &b.mode {
                if !is_shift_mode(m) && !out.contains(m) {
                    out.push(m.clone());
                }
            }
        }
    }
    out
}

/// The spot that a control `(group, index)` is on, with the key's number for a keyboard.
pub fn spot_of(layout: &ControllerLayout, group: &str, index: usize) -> Option<usize> {
    layout.spots.iter().position(|s| {
        s.bindings.iter().any(|b| b.group == group && b.index == index)
            || s.keys.is_some_and(|(first, n)| {
                s.bindings.first().is_some_and(|b| b.group == group) && (first..first + n).contains(&index)
            })
    })
}

/// The control a pointer at `frac` (0...1) along a keyboard spot hits: its index in the group.
/// The keys are laid out as a piano's, white keys even, black keys over the gaps; `y_frac` is
/// how far down the key (0...1), because a black key covers only the top of the gap.
pub fn key_at(first: usize, count: usize, frac: f64, y_frac: f64) -> Option<usize> {
    let keys = piano(first, count);
    let whites = keys.iter().filter(|k| !k.black).count();
    if whites == 0 {
        return None;
    }
    let x = frac.clamp(0.0, 1.0) * whites as f64;
    if y_frac < BLACK_DEPTH {
        for k in keys.iter().filter(|k| k.black) {
            if x >= k.x && x < k.x + k.w {
                return Some(k.index);
            }
        }
    }
    keys.iter()
        .filter(|k| !k.black)
        .find(|k| x >= k.x && x < k.x + k.w)
        .map(|k| k.index)
}

/// How far down a black key reaches.
pub const BLACK_DEPTH: f64 = 0.6;

/// One key of a drawn piano, in white-key units (x from 0 to the number of white keys).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PianoKey {
    pub index: usize,
    pub black: bool,
    pub x: f64,
    pub w: f64,
}

/// The keys from note `first`, `count` of them, the way a keyboard is cut.
pub fn piano(first: usize, count: usize) -> Vec<PianoKey> {
    let mut out = Vec::with_capacity(count);
    let mut white = 0.0;
    for i in 0..count {
        let n = first + i;
        let black = matches!(n % 12, 1 | 3 | 6 | 8 | 10);
        if black {
            out.push(PianoKey { index: n, black: true, x: white - 0.3, w: 0.6 });
        } else {
            out.push(PianoKey { index: n, black: false, x: white, w: 1.0 });
            white += 1.0;
        }
    }
    out
}

/// How long a moved control stays lit, in seconds.
pub const GLOW: f64 = 0.6;

/// Which controls moved lately and how far, for the live highlight: fed with every decoded
/// event (hardware or on-screen), read by the drawing each frame.
#[derive(Clone, Debug, Default)]
pub struct Activity {
    /// `(device, group, index)` → (when it last moved, its value 0...1, held down).
    last: BTreeMap<(String, String, usize), (f64, f64, bool)>,
    /// Jog wheels: `(device, deck)` → when.
    jogs: BTreeMap<(String, String), f64>,
    /// Controls ever seen, in the order first seen, for a device with no drawing.
    seen: BTreeMap<String, Vec<(String, usize)>>,
}

impl Activity {
    pub fn new() -> Activity {
        Activity::default()
    }

    /// `device`'s control moved.
    pub fn moved(&mut self, device: &str, group: &str, index: usize, value: f64, pressed: bool, now: f64) {
        self.last
            .insert((device.to_string(), group.to_string(), index), (now, value, pressed));
        let seen = self.seen.entry(device.to_string()).or_default();
        if !seen.iter().any(|(g, i)| g == group && *i == index) {
            seen.push((group.to_string(), index));
        }
    }

    /// `device`'s jog wheel for `deck` moved.
    pub fn jog(&mut self, device: &str, deck: &str, now: f64) {
        self.jogs.insert((device.to_string(), deck.to_string()), now);
    }

    /// How lit the control is at `now`: 1 while it is held down, fading to 0 over [`GLOW`]
    /// seconds after it moves.
    pub fn glow(&self, device: &str, group: &str, index: usize, now: f64) -> f64 {
        match self.last.get(&(device.to_string(), group.to_string(), index)) {
            Some(&(_, _, true)) => 1.0,
            Some(&(t, _, false)) => fade(now - t),
            None => 0.0,
        }
    }

    /// The last value a control had (0...1), if it has moved.
    pub fn value(&self, device: &str, group: &str, index: usize) -> Option<f64> {
        self.last
            .get(&(device.to_string(), group.to_string(), index))
            .map(|v| v.1)
    }

    pub fn jog_glow(&self, device: &str, deck: &str, now: f64) -> f64 {
        self.jogs
            .get(&(device.to_string(), deck.to_string()))
            .map_or(0.0, |t| fade(now - t))
    }

    /// Is anything still lit (the window needs another frame)?
    pub fn any_lit(&self, now: f64) -> bool {
        self.last.values().any(|&(t, _, held)| held || now - t < GLOW)
            || self.jogs.values().any(|t| now - t < GLOW)
    }

    /// The controls of `device` seen to move, first seen first.
    pub fn seen(&self, device: &str) -> &[(String, usize)] {
        self.seen.get(device).map_or(&[], Vec::as_slice)
    }

    /// Has `device` been heard from at all?
    pub fn heard(&self, device: &str) -> bool {
        self.last.keys().any(|(d, _, _)| d == device) || self.jogs.keys().any(|(d, _)| d == device)
    }
}

fn fade(age: f64) -> f64 {
    if age < 0.0 {
        1.0
    } else {
        (1.0 - age / GLOW).clamp(0.0, 1.0)
    }
}

/// Does any spot of the layout cover `(group, index)`? Controls with no spot are listed beside
/// the drawing so none goes missing from it.
pub fn covered(layout: &ControllerLayout, group: &str, index: usize) -> bool {
    layout.spots.iter().any(|s: &ControlSpot| {
        s.bindings.iter().any(|b| b.group == group && b.index == index)
            || s.keys.is_some_and(|(first, n)| {
                s.bindings.first().is_some_and(|b| b.group == group) && (first..first + n).contains(&index)
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::bindmap;
    use crate::control::profiles;

    #[test]
    fn every_drawn_device_labels_every_spot_it_binds() {
        for l in ControllerLayout::all() {
            let p = profiles::all_surfaces()
                .into_iter()
                .find(|p| p.name == l.profile_name)
                .unwrap_or_else(|| panic!("no profile {}", l.profile_name));
            let m = bindmap::build(&p);
            let v = view(&l, &m);
            assert_eq!(v.len(), l.spots.len());
            for (s, sv) in l.spots.iter().zip(&v) {
                assert_eq!(sv.labels.len(), s.bindings.len(), "{} {:?}: a spot names a control the profile lacks", l.title, s.caption);
            }
        }
    }

    /// Every control of a profile is on its drawing, bar the ones listed (and said why).
    #[test]
    fn every_control_has_a_place_on_its_drawing() {
        // controls a profile has that have no spot of their own, and why
        let elsewhere = |dev: &str, group: &str| match (dev, group) {
            // the APC's drum mode reuses the 16 lower pads under other notes
            ("APC mini mk2", "drumPads") => true,
            // the MPK's pad banks A / B are the same eight pads as PROG, under the pad mode
            ("MPK Mini Mk II", "padNotes") => true,
            // shift-layer copies sit on their base spot as the "SHIFT" mode
            (d, g) if d.contains("Inpulse") && (g.contains("shift") || g.contains("padBend") || g.contains("ClearPads")) => true,
            _ => false,
        };
        let mut covered_n = 0;
        for l in ControllerLayout::all() {
            let l_has_keys = |g: &str| {
                l.spots.iter().any(|s| s.keys.is_some() && s.bindings.first().is_some_and(|b| b.group == g))
            };
            let p = profiles::all_surfaces().into_iter().find(|p| p.name == l.profile_name).unwrap();
            for g in &p.groups {
                let n = if g.kind == crate::control::surface::ControlType::Pitchbend { 1 } else { g.control_numbers().len() };
                for i in 0..n {
                    if covered(&l, &g.id, i) {
                        covered_n += 1;
                        continue;
                    }
                    // a keyboard is drawn for the keys it has; the octave switch moves the others
                    assert!(l_has_keys(&g.id) || elsewhere(&p.name, &g.id), "{}: {}[{i}] is not drawn", p.name, g.id);
                }
            }
        }
        assert!(covered_n > 250, "{covered_n}");
    }

    #[test]
    fn spots_stay_inside_their_drawing() {
        for l in ControllerLayout::all() {
            for s in &l.spots {
                assert!(s.x >= 0.0 && s.y >= 0.0, "{} {:?}", l.title, s.caption);
                assert!(s.x + s.w <= l.width + 0.01, "{} {:?} x {}", l.title, s.caption, s.x + s.w);
                assert!(s.y + s.h <= l.height + 0.01, "{} {:?} y {}", l.title, s.caption, s.y + s.h);
            }
        }
    }

    #[test]
    fn no_two_spots_overlap() {
        for l in ControllerLayout::all() {
            for (i, a) in l.spots.iter().enumerate() {
                for b in l.spots.iter().skip(i + 1) {
                    // a jog wheel's well and the knobs sit apart; faders carry a thumb
                    let apart = a.x + a.w <= b.x + 0.001
                        || b.x + b.w <= a.x + 0.001
                        || a.y + a.h <= b.y + 0.001
                        || b.y + b.h <= a.y + 0.001;
                    assert!(apart, "{}: {:?} overlaps {:?}", l.title, a.caption, b.caption);
                }
            }
        }
    }

    #[test]
    fn the_new_devices_are_drawn_with_their_controls() {
        let td3 = ControllerLayout::td3();
        assert!(td3.spots.iter().any(|s| s.keys == Some((36, 13))));
        assert!(td3.spots.iter().filter(|s| s.is_panel_only()).count() >= 10, "knobs and switches are drawn, not bound");
        let pd = ControllerLayout::pd705();
        assert_eq!(pd.spots.iter().filter(|s| s.bindings.first().is_some_and(|b| b.group == "pads")).count(), 9);
        assert_eq!(pd.spots.iter().filter(|s| s.bindings.first().is_some_and(|b| b.group == "triggers")).count(), 4);
        assert_eq!(pd.spots.iter().filter(|s| s.bindings.first().is_some_and(|b| b.group == "feet")).count(), 2);
        let kb = ControllerLayout::computer_keyboard();
        assert_eq!(kb.spots.len(), 38, "every key the profile names is drawn");
    }

    #[test]
    fn pad_modes_come_from_the_layout() {
        let m = modes(&ControllerLayout::inpulse_200_mk3());
        assert_eq!(m[0], "HOT CUE");
        assert!(m.contains(&"LOOP".to_string()) && m.contains(&"SAMPLER".to_string()));
        assert!(!m.iter().any(|x| is_shift_mode(x)), "{m:?}");
        let mpk = modes(&ControllerLayout::mpk_mini_mk2());
        assert_eq!(mpk, ["PROG", "NOTE A", "NOTE B"]);
    }

    #[test]
    fn a_pad_shows_the_label_of_the_mode_the_unit_is_in() {
        let l = ControllerLayout::inpulse_200_mk3();
        let p = profiles::inpulse_200_mk3();
        let v = view(&l, &bindmap::build(&p));
        let pad = l
            .spots
            .iter()
            .position(|s| s.bindings.iter().any(|b| b.group == "deckA.loopPads"))
            .unwrap();
        let hot = v[pad].pick(Some("HOT CUE")).unwrap();
        let lp = v[pad].pick(Some("LOOP")).unwrap();
        assert_eq!(hot.group, "deckA.hotcuePads");
        assert_eq!(lp.group, "deckA.loopPads");
        assert_eq!(v[pad].pick(None).unwrap().group, "deckA.hotcuePads");
        // an unknown mode falls back to the base
        assert_eq!(v[pad].pick(Some("nope")).unwrap().group, "deckA.hotcuePads");
        // a shift layer is not a mode to pick
        let play = l.spots.iter().position(|s| s.caption.as_deref() == Some("SYNC")).unwrap();
        assert_eq!(v[play].pick(Some("SHIFT")).unwrap().mode, None);
        assert!(v[play].shift_label().is_some());
    }

    #[test]
    fn a_click_along_the_keys_hits_the_key_under_it() {
        // the TD-3's 13 keys from C: 8 white keys, so each is 1/8 wide
        assert_eq!(key_at(36, 13, 0.01, 0.9), Some(36));
        assert_eq!(key_at(36, 13, 0.99, 0.9), Some(48));
        assert_eq!(key_at(36, 13, 0.0 + 1.0 / 8.0 * 1.5, 0.9), Some(38), "D, low on the key");
        // high on the key, the black key C# sits between C and D
        assert_eq!(key_at(36, 13, 1.0 / 8.0, 0.1), Some(37));
        assert_eq!(key_at(36, 13, 1.0 / 8.0, 0.9), Some(38), "low on the key, past the C");
        // the MPK's 25 keys: 15 white
        assert_eq!(key_at(48, 25, 0.001, 0.9), Some(48));
        assert_eq!(key_at(48, 25, 0.999, 0.9), Some(72));
        let p = piano(48, 25);
        assert_eq!(p.iter().filter(|k| k.black).count(), 10);
        assert_eq!(p.iter().filter(|k| !k.black).count(), 15);
    }

    #[test]
    fn a_moved_control_glows_then_fades() {
        let mut a = Activity::new();
        assert_eq!(a.glow("d", "pads", 3, 0.0), 0.0);
        a.moved("d", "pads", 3, 0.5, false, 10.0);
        assert!((a.glow("d", "pads", 3, 10.0) - 1.0).abs() < 1e-9);
        assert!(a.glow("d", "pads", 3, 10.0 + GLOW / 2.0) > 0.4);
        assert_eq!(a.glow("d", "pads", 3, 10.0 + GLOW + 0.01), 0.0);
        assert!(a.any_lit(10.1) && !a.any_lit(12.0));
        // held down stays lit until it is let go
        a.moved("d", "pads", 4, 1.0, true, 10.0);
        assert_eq!(a.glow("d", "pads", 4, 99.0), 1.0);
        a.moved("d", "pads", 4, 0.0, false, 100.0);
        assert_eq!(a.glow("d", "pads", 4, 101.0), 0.0);
        assert_eq!(a.value("d", "pads", 3), Some(0.5));
        assert_eq!(a.seen("d"), [("pads".to_string(), 3), ("pads".to_string(), 4)]);
        assert!(a.heard("d") && !a.heard("e"));
        a.jog("d", "A", 5.0);
        assert!(a.jog_glow("d", "A", 5.1) > 0.5 && a.jog_glow("d", "B", 5.1) == 0.0);
    }

    #[test]
    fn a_device_with_no_drawing_gets_one_from_what_it_was_seen_to_do() {
        let p = profiles::generic("Some Synth");
        assert!(ControllerLayout::generic(&p, &[]).spots.is_empty());
        let seen = [("cc".to_string(), 74), ("notes".to_string(), 60), ("cc".to_string(), 71), ("notes".to_string(), 61)];
        let l = ControllerLayout::generic(&p, &seen);
        assert_eq!(l.spots.len(), 4);
        // notes come before ccs (group order), each group in index order, in rows
        let caps: Vec<_> = l.spots.iter().map(|s| s.caption.clone().unwrap()).collect();
        assert_eq!(caps, ["note 60", "note 61", "CC 71", "CC 74"]);
        assert_eq!(l.spots[0].kind, crate::control::layout::SpotKind::Pad);
        assert_eq!(l.spots[2].kind, crate::control::layout::SpotKind::Knob);
        assert!(l.spots[2].y > l.spots[0].y, "a new group starts a new row");
        // a bound control is drawn although it was never seen
        let mut bound = p.clone();
        let mut b = crate::control::map::template(crate::control::surface::DJActionKind::Play);
        b.group = "notes".into();
        b.index = Some(36);
        bound.layout.push(b);
        assert_eq!(ControllerLayout::generic(&bound, &[]).spots.len(), 1);
    }
}
