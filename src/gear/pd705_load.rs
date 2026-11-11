//! Which kit is on the PD705, as far as the hub can know (docs/HUB.md, M8).
//!
//! The pad sends nothing about its kit, and an install only puts `pad-content.hppd` on the
//! Pi's stick: the kit reaches the pad when someone runs **LOAD > Kit > Append** on it. So the
//! tracker has two states, honestly:
//!
//! - **staged**: the file is on the stick and its sha256 was checked after the copy. The pad
//!   may or may not have loaded it yet;
//! - **active**: the kit was confirmed loaded on the pad (the Kits window's button, or the
//!   socket's `kits loaded`).
//!
//! The hold-back that keeps a kit's loop pads out of the groove and the loop routes
//! ([`super::pd705::Kit`]) uses [`Tracker::effective`]: the loop pads of **both**, so a staged
//! kit is already protected and an older kit still on the pad is too, until the new one is
//! confirmed. The step pattern follows [`Tracker::pattern_kit`]: the active kit's, else the
//! staged one's.
//!
//! Also here: the rules for when a real install is refused, and the readers of what an
//! install run printed (the dry-run plan carries the sha256 the installer will check, and a
//! real run reports `sha256 ok` per copy).

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::pd705::Kit;
use crate::json::{JsonObject, JsonValue};

/// What the tracker keeps of an installed kit: enough to hold its loop pads back even after
/// the draft is edited or deleted.
#[derive(Clone, Debug, PartialEq)]
pub struct Record {
    /// The kit's name on the pad.
    pub kit: String,
    /// The sha256 of the `pad-content.hppd` that was installed (hex).
    pub sha256: String,
    /// The kit's loop pads when it was built.
    pub loop_pads: Vec<u8>,
    /// Unix seconds when it was installed (staged) or confirmed (active).
    pub at: i64,
}

impl Record {
    fn json(&self) -> JsonValue {
        JsonValue::Object(JsonObject::from_pairs([
            ("kit", JsonValue::str(self.kit.clone())),
            ("sha256", JsonValue::str(self.sha256.clone())),
            (
                "loop_pads",
                JsonValue::Array(
                    self.loop_pads
                        .iter()
                        .map(|p| JsonValue::int(*p as i64))
                        .collect(),
                ),
            ),
            ("at", JsonValue::int(self.at)),
        ]))
    }

    fn from_json(v: &JsonValue) -> Option<Record> {
        let mut loop_pads: Vec<u8> = v
            .get("loop_pads")
            .and_then(JsonValue::ints)
            .unwrap_or_default()
            .into_iter()
            .filter(|p| (1..=super::pd705::INPUTS as i64).contains(p))
            .map(|p| p as u8)
            .collect();
        loop_pads.sort_unstable();
        loop_pads.dedup();
        Some(Record {
            kit: v.get("kit")?.as_str()?.to_string(),
            sha256: v.get("sha256")?.as_str()?.to_string(),
            loop_pads,
            at: v.get("at").and_then(JsonValue::as_i64).unwrap_or(0),
        })
    }
}

/// The two states a kit passes through. See the module docs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tracker {
    pub active: Option<Record>,
    pub staged: Option<Record>,
}

impl Tracker {
    /// An install finished and its copy checked: the kit is on the stick, not yet on the pad.
    pub fn staged_ok(&mut self, rec: Record) {
        self.staged = Some(rec);
    }

    /// The staged kit was loaded on the pad. Errs when nothing is staged.
    pub fn confirm_loaded(&mut self, at: i64) -> Result<Record, String> {
        let mut rec = self
            .staged
            .take()
            .ok_or("nothing is staged: install a kit first")?;
        rec.at = at;
        self.active = Some(rec.clone());
        Ok(rec)
    }

    /// Say a kit is on the pad without installing it from here (a kit loaded another way).
    pub fn set_active(&mut self, rec: Record) {
        self.active = Some(rec);
    }

    /// Forget the staged kit (it was never loaded).
    pub fn drop_staged(&mut self) -> bool {
        self.staged.take().is_some()
    }

    /// Nothing known.
    pub fn is_empty(&self) -> bool {
        self.active.is_none() && self.staged.is_none()
    }

    /// The kit the hold-back uses: the loop pads of the active and the staged kit together.
    /// None before any install, so the hub keeps its default.
    pub fn effective(&self) -> Option<Kit> {
        let name = self
            .staged
            .as_ref()
            .or(self.active.as_ref())
            .map(|r| r.kit.clone())?;
        let mut pads: Vec<u8> = self
            .active
            .iter()
            .chain(self.staged.iter())
            .flat_map(|r| r.loop_pads.iter().copied())
            .collect();
        pads.sort_unstable();
        pads.dedup();
        Some(Kit {
            name,
            loop_pads: pads,
        })
    }

    /// The kit whose step pattern the clock plays: the one on the pad, else the staged one.
    pub fn pattern_kit(&self) -> Option<&str> {
        self.active
            .as_ref()
            .or(self.staged.as_ref())
            .map(|r| r.kit.as_str())
    }

    /// `on the pad: Acid House · staged: Drums2 (load it on the pad, then confirm)`.
    pub fn line(&self) -> String {
        let short = |r: &Record| format!("{} ({})", r.kit, &r.sha256[..r.sha256.len().min(8)]);
        match (&self.active, &self.staged) {
            (None, None) => "no kit installed from the hub".into(),
            (Some(a), None) => format!("on the pad: {}", short(a)),
            (None, Some(s)) => format!(
                "staged on the stick: {} (load it on the pad, then confirm)",
                short(s)
            ),
            (Some(a), Some(s)) => format!(
                "on the pad: {} · staged: {} (load it on the pad, then confirm)",
                short(a),
                short(s)
            ),
        }
    }

    pub fn to_json(&self) -> JsonValue {
        let mut o = JsonObject::new();
        if let Some(a) = &self.active {
            o.set("active", a.json());
        }
        if let Some(s) = &self.staged {
            o.set("staged", s.json());
        }
        JsonValue::Object(o)
    }

    pub fn from_json(v: &JsonValue) -> Tracker {
        Tracker {
            active: v.get("active").and_then(Record::from_json),
            staged: v.get("staged").and_then(Record::from_json),
        }
    }
}

/// What the hub is doing with the pad right now, for [`install_blockers`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Live {
    /// The groove is on and the transport is running: notes are going to the pad.
    pub steps_playing: bool,
    /// A loop part is routed to the PD705.
    pub routed: bool,
}

/// Why a real install should wait. Installing detaches the Pi's USB stick for a few seconds (the
/// pad sees the stick pulled and put back), so it is refused while the hub is playing the pad or
/// routing loops to it, unless the caller says it knows (`kits install confirm_live`).
pub fn install_blockers(live: Live) -> Vec<String> {
    let mut v = Vec::new();
    if live.steps_playing {
        v.push("the PD705 steps are playing: stop the transport or turn the steps off".into());
    }
    if live.routed {
        v.push("a loop part is routed to the PD705: clear the route first".into());
    }
    v
}

/// What an install run printed, read back.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InstallReport {
    /// The sha256 the installer will check (dry run) or did check, from its script.
    pub plan_sha256: Option<String>,
    /// The copy on the stick was read back and matched (`pd705: wrote "…" (N bytes, sha256 ok)`).
    pub sha_ok: bool,
    /// Bytes copied.
    pub bytes: Option<u64>,
    /// The Pi's USB gadget state after it reattached (`configured` when the pad has the stick).
    pub udc: Option<String>,
    /// The gadget came back after a failure (`reattached after an error`).
    pub reattached_after_error: bool,
    /// A `pd705: …` line that reports a failure.
    pub error: Option<String>,
}

/// Read a `pd705 build --to-pi` run: the dry run's script (with the sha256 it compares) or a
/// real run's messages.
pub fn parse_install_output(text: &str) -> InstallReport {
    let mut r = InstallReport::default();
    for line in text.lines() {
        let l = line.trim();
        // the script's check, `[ "$got" = <sha256> ] || { … }`
        if let Some(i) = l.find("\"$got\" = ") {
            let tail = &l[i + "\"$got\" = ".len()..];
            let hex: String = tail.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
            if hex.len() == 64 {
                r.plan_sha256 = Some(hex.to_ascii_lowercase());
            }
        }
        if let Some(rest) = l.strip_prefix("pd705: wrote ") {
            if rest.contains("sha256 ok") {
                r.sha_ok = true;
            }
            if let Some((_, tail)) = rest.rsplit_once('(') {
                if let Some(n) = tail
                    .split_whitespace()
                    .next()
                    .and_then(|n| n.parse::<u64>().ok())
                {
                    r.bytes = Some(n);
                }
            }
        }
        if let Some(rest) = l.strip_prefix("pd705: UDC state:") {
            r.udc = Some(rest.trim().to_string());
        }
        if l.contains("gadget reattached after an error") {
            r.reattached_after_error = true;
        }
        if l.starts_with("pd705: ") && (l.contains("mismatch") || l.contains("failed") || l.contains("no image") || l.contains("already") || l.contains("not loaded")) && r.error.is_none() {
            r.error = Some(l.to_string());
        }
    }
    r
}

/// Whether a real run left the pad with its stick and the file checked.
pub fn install_succeeded(exit_ok: bool, r: &InstallReport) -> bool {
    exit_ok && r.sha_ok && r.error.is_none() && r.udc.as_deref() == Some("configured")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(kit: &str, sha: &str, pads: &[u8]) -> Record {
        Record {
            kit: kit.into(),
            sha256: sha.into(),
            loop_pads: pads.to_vec(),
            at: 100,
        }
    }

    #[test]
    fn nothing_known_leaves_the_hubs_default_alone() {
        let t = Tracker::default();
        assert!(t.is_empty());
        assert_eq!(t.effective(), None);
        assert_eq!(t.pattern_kit(), None);
        assert_eq!(t.line(), "no kit installed from the hub");
    }

    #[test]
    fn a_staged_kit_is_held_back_at_once_and_an_older_one_until_confirmed() {
        let mut t = Tracker::default();
        t.set_active(rec("Acid House", &"a".repeat(64), &[1, 2, 3]));
        t.staged_ok(rec("Drums2", &"b".repeat(64), &[5]));
        // both kits' loop pads are held back; the name is the one coming
        let k = t.effective().unwrap();
        assert_eq!(k.name, "Drums2");
        assert_eq!(k.loop_pads, [1, 2, 3, 5]);
        // the pattern is still the one for the kit on the pad
        assert_eq!(t.pattern_kit(), Some("Acid House"));
        let got = t.confirm_loaded(200).unwrap();
        assert_eq!((got.kit.as_str(), got.at), ("Drums2", 200));
        assert!(t.staged.is_none());
        let k = t.effective().unwrap();
        assert_eq!(k.loop_pads, [5], "the old kit's pads are free once the new one is on");
        assert_eq!(t.pattern_kit(), Some("Drums2"));
        assert!(t.confirm_loaded(300).is_err());
    }

    #[test]
    fn staged_only_and_dropping_it() {
        let mut t = Tracker::default();
        t.staged_ok(rec("K", &"c".repeat(64), &[2]));
        assert_eq!(t.pattern_kit(), Some("K"));
        assert!(t.line().starts_with("staged on the stick: K (cccccccc)"));
        assert!(t.drop_staged());
        assert!(!t.drop_staged());
        assert!(t.is_empty());
    }

    #[test]
    fn json_round_trips_and_drops_junk() {
        let mut t = Tracker::default();
        t.set_active(rec("A", &"1".repeat(64), &[3, 1, 3]));
        t.staged_ok(rec("B", &"2".repeat(64), &[]));
        let text = t.to_json().canonical_js();
        let back = Tracker::from_json(&JsonValue::parse(&text).unwrap());
        assert_eq!(back.staged, t.staged);
        assert_eq!(back.active.unwrap().loop_pads, [1, 3]);
        let junk = Tracker::from_json(&JsonValue::parse(r#"{"active":{"kit":"A"},"staged":5}"#).unwrap());
        assert!(junk.is_empty());
        // out-of-range pads in a file are dropped
        let r = Tracker::from_json(&JsonValue::parse(r#"{"active":{"kit":"A","sha256":"x","loop_pads":[0,3,99,3]}}"#).unwrap());
        assert_eq!(r.active.unwrap().loop_pads, [3]);
    }

    #[test]
    fn a_real_install_waits_for_the_pad_to_be_idle() {
        assert!(install_blockers(Live::default()).is_empty());
        let b = install_blockers(Live {
            steps_playing: true,
            routed: true,
        });
        assert_eq!(b.len(), 2);
        assert!(b[0].contains("steps are playing") && b[1].contains("routed"));
    }

    const SHA: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    const PLAN: &str = "kit 1 (imports as the next free C slot): K Pad7=Kick\n\
        wrote /tmp/pd705-build-1/pad-content.hppd (61234 bytes)\n\
        would install /tmp/pd705-build-1/pad-content.hppd on pi@raspberrypi.local as /pad-content.hppd (any existing one is backed up on the Pi first)\n\
        scp /tmp/pd705-build-1/pad-content.hppd pi@raspberrypi.local:/tmp/pd705-upload-1-0\n\
        ssh pi@raspberrypi.local sudo sh -s <<'EOF'\n\
        set -eu\n\
        cp /tmp/pd705-upload-1-0 \"$MNT\"/pad-content.hppd\n\
        got=$(sha256sum \"$MNT\"/pad-content.hppd | cut -d' ' -f1)\n\
        [ \"$got\" = 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef ] || { echo \"pd705: \"pad-content.hppd\" sha256 mismatch after copy\" >&2; exit 4; }\n\
        echo \"pd705: wrote \"pad-content.hppd\" ($(stat -c %s \"$MNT\"/pad-content.hppd) bytes, sha256 ok)\"\n\
        EOF\n";

    #[test]
    fn a_dry_run_plan_names_the_sha256_the_installer_will_check() {
        let r = parse_install_output(PLAN);
        assert_eq!(r.plan_sha256.as_deref(), Some(SHA));
        // the script's own echo line is not a result: nothing ran
        assert!(!r.sha_ok, "a dry run's script text is not a checked copy");
    }

    #[test]
    fn a_real_run_is_a_success_only_with_a_checked_copy_and_the_stick_back() {
        let good = "pd705: detaching the gadget (the pad sees the stick removed)\n\
            pd705: wrote pad-content.hppd (61234 bytes, sha256 ok)\n\
            pd705: reattaching the gadget (the pad sees the stick inserted)\n\
            pd705: UDC state: configured\n";
        let r = parse_install_output(good);
        assert!(r.sha_ok && r.bytes == Some(61_234) && r.udc.as_deref() == Some("configured"));
        assert!(install_succeeded(true, &r));
        assert!(!install_succeeded(false, &r), "a failing exit is a failure");
        // not configured: the pad may not have its stick back
        let r2 = parse_install_output(&good.replace("configured", "not attached"));
        assert!(!install_succeeded(true, &r2));
        // no check line
        let r3 = parse_install_output("pd705: UDC state: configured\n");
        assert!(!install_succeeded(true, &r3));
        // a mismatch
        let bad = "pd705: pad-content.hppd sha256 mismatch after copy\n\
            pd705: gadget reattached after an error\n";
        let r4 = parse_install_output(bad);
        assert!(r4.reattached_after_error && r4.error.is_some());
        assert!(!install_succeeded(true, &r4));
    }
}
