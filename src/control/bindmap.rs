//! The binding map: every control of a device and what it is bound to, as data for a picture
//! (docs/BINDINGS-MAP.md). Pure: the host draws it. One [`Cell`] per control of a group, with
//! its family (the colour), a short label, and whether the binding comes from the group as a
//! whole, from this control, or from the shift layer.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::control::registry;
use crate::control::surface::{Binding, DJActionKind, SurfaceProfile};

/// Controls per row before a group wraps in the picture.
pub const WRAP: usize = 16;

/// Where a cell's binding comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// Nothing is bound.
    None,
    /// A binding for exactly this control.
    Control,
    /// A binding for the whole group.
    Group,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cell {
    /// Index within the group.
    pub index: usize,
    /// The control's name from the profile ("play", "cue", ...), else "1", "2", ...
    pub name: String,
    pub origin: Origin,
    pub action: Option<DJActionKind>,
    /// The registry family ("Mixer", "Loops", ...): the cell's colour. "" when unbound.
    pub family: &'static str,
    /// A few characters for the cell itself.
    pub short: String,
    /// The full description, for hover.
    pub detail: String,
    /// The shifted layer differs: what the control does while shift is held.
    pub shifted: Option<String>,
    /// Parked on purpose (`unbound` with a reason).
    pub parked: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GroupMap {
    pub id: String,
    pub cells: Vec<Cell>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DeviceMap {
    pub device: String,
    pub groups: Vec<GroupMap>,
}

impl DeviceMap {
    /// The cell of control `index` of `group`.
    pub fn cell(&self, group: &str, index: usize) -> Option<&Cell> {
        self.groups
            .iter()
            .find(|g| g.id == group)
            .and_then(|g| g.cells.get(index))
    }

    pub fn bound(&self) -> usize {
        self.groups
            .iter()
            .flat_map(|g| &g.cells)
            .filter(|c| c.action.is_some_and(|a| a != DJActionKind::Unbound))
            .count()
    }
    pub fn total(&self) -> usize {
        self.groups.iter().map(|g| g.cells.len()).sum()
    }
    /// The families in use, in the registry's order, for a legend.
    pub fn families(&self) -> Vec<&'static str> {
        registry::families()
            .into_iter()
            .filter(|f| {
                self.groups
                    .iter()
                    .flat_map(|g| &g.cells)
                    .any(|c| c.family == *f)
            })
            .collect()
    }
}

/// The best binding for control `index` of `group` on the shift layer `shift`: an exact control
/// beats a group-wide one; a binding for the other layer is ignored, one for both layers
/// (shift None) answers either.
fn find<'a>(layout: &'a [Binding], group: &str, index: usize, shift: bool) -> Option<(&'a Binding, Origin)> {
    let layer = |b: &&Binding| b.shift.is_none() || b.shift == Some(shift);
    let exact = layout
        .iter()
        .filter(|b| b.group == group && b.index == Some(index))
        .filter(layer)
        // a layer-specific binding beats a both-layers one
        .max_by_key(|b| b.shift.is_some());
    if let Some(b) = exact {
        return Some((b, Origin::Control));
    }
    layout
        .iter()
        .filter(|b| b.group == group && b.index.is_none())
        .filter(layer)
        .max_by_key(|b| b.shift.is_some())
        .map(|b| (b, Origin::Group))
}

/// A short label for a cell: the first letters of the target's label, then its slot.
fn short_of(b: &Binding) -> String {
    let info = registry::info(b.action);
    let mut s: String = info
        .label
        .split_whitespace()
        .filter_map(|w| w.chars().next())
        .take(3)
        .collect();
    if let Some(d) = b.deck.as_deref().filter(|d| *d != "*") {
        s.push_str(d);
    }
    if let Some(n) = b.slot {
        s.push_str(&(n + 1).to_string());
    }
    s
}

/// The map of one device's profile (use the live profile, with the user's map laid over it).
pub fn build(profile: &SurfaceProfile) -> DeviceMap {
    let mut groups = Vec::new();
    for g in &profile.groups {
        let n = g.control_numbers().len().max(1);
        let mut cells = Vec::with_capacity(n);
        for index in 0..n {
            let name = g.names.as_ref().and_then(|v| v.get(index)).cloned().unwrap_or_else(|| (index + 1).to_string());
            let base = find(&profile.layout, &g.id, index, false);
            let held = find(&profile.layout, &g.id, index, true);
            let shifted = match (base, held) {
                (Some((a, _)), Some((b, _))) if a.action == b.action && a.deck == b.deck && a.slot == b.slot => None,
                (_, Some((b, _))) => Some(b.summary()),
                _ => None,
            };
            let cell = match base.or(held) {
                Some((b, origin)) => Cell {
                    index,
                    name,
                    origin,
                    action: Some(b.action),
                    family: if b.action == DJActionKind::Unbound { "" } else { registry::info(b.action).family },
                    short: if b.action == DJActionKind::Unbound { "·".into() } else { short_of(b) },
                    detail: b.summary(),
                    shifted,
                    parked: b.action == DJActionKind::Unbound,
                },
                None => Cell {
                    index,
                    name,
                    origin: Origin::None,
                    action: None,
                    family: "",
                    short: String::new(),
                    detail: "not bound".into(),
                    shifted: None,
                    parked: false,
                },
            };
            cells.push(cell);
        }
        groups.push(GroupMap { id: g.id.clone(), cells });
    }
    DeviceMap { device: profile.name.clone(), groups }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::profiles;

    #[test]
    fn every_shipped_device_maps_with_some_bound_controls() {
        let ps = profiles::all_surfaces();
        assert!(!ps.is_empty());
        for p in ps {
            let m = build(&p);
            assert_eq!(m.device, p.name);
            assert!(m.total() > 0, "{}", p.name);
            // a device that ships with everything parked (a free pad) has nothing bound yet
            let all_parked = m.groups.iter().flat_map(|g| &g.cells).all(|c| c.parked || c.action.is_none());
            assert!(m.bound() > 0 || all_parked, "{} has bound controls", p.name);
            for g in &m.groups {
                for (i, c) in g.cells.iter().enumerate() {
                    assert_eq!(c.index, i);
                    assert_eq!(c.action.is_none(), c.origin == Origin::None);
                }
            }
            assert!(!m.families().is_empty() || all_parked, "{}", p.name);
        }
    }

    #[test]
    fn a_control_binding_beats_its_groups_and_unbound_shows_parked() {
        let mut p = profiles::all_surfaces().into_iter().next().unwrap().as_ref().clone();
        let g = p.groups[0].id.clone();
        let mk = |index: Option<usize>, a: DJActionKind| {
            let mut b = crate::control::map::template(a);
            b.group = g.clone();
            b.index = index;
            b.shift = None;
            b
        };
        p.layout.retain(|b| b.group != g);
        p.layout.push(mk(None, DJActionKind::Volume));
        p.layout.push(mk(Some(1), DJActionKind::Filter));
        let m = build(&p);
        let cells = &m.groups[0].cells;
        assert_eq!((cells[0].origin, cells[0].action), (Origin::Group, Some(DJActionKind::Volume)));
        assert_eq!((cells[1].origin, cells[1].action), (Origin::Control, Some(DJActionKind::Filter)));
        assert_eq!(cells[1].family, "Mixer");
        p.layout.push(mk(Some(0), DJActionKind::Unbound));
        let m = build(&p);
        assert!(m.groups[0].cells[0].parked);
    }

    #[test]
    fn the_shift_layer_is_reported() {
        let mut p = profiles::all_surfaces().into_iter().next().unwrap().as_ref().clone();
        let g = p.groups[0].id.clone();
        p.layout.retain(|b| b.group != g);
        let mut a = crate::control::map::template(DJActionKind::Play);
        a.group = g.clone();
        a.index = Some(0);
        a.shift = Some(false);
        a.deck = Some("A".into());
        let mut b = crate::control::map::template(DJActionKind::Cue);
        b.group = g.clone();
        b.index = Some(0);
        b.shift = Some(true);
        b.deck = Some("A".into());
        p.layout.push(a);
        p.layout.push(b);
        let c = &build(&p).groups[0].cells[0];
        assert_eq!(c.action, Some(DJActionKind::Play));
        assert!(c.shifted.as_deref().is_some_and(|s| s.to_lowercase().contains("cue")), "{:?}", c.shifted);
    }
}
