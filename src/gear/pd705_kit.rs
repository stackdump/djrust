//! A PD705 kit as the hub edits it: samples picked onto the pad's inputs (docs/HUB.md, M8).
//!
//! A [`KitDraft`] is a name, up to 15 pad slots (Pad1-9, Trig1-4, Foot1-2) holding a sample
//! file each, and an optional step pattern ([`KitPattern`]). The hub keeps a library of up to
//! [`LIBRARY_MAX`] drafts. Building one is the `pd705` command's job (pd705-rs, run as a
//! separate process: `crate::host::pd705`); this module only decides *what to ask it*:
//! [`KitDraft::build_args`] is the argument list for `pd705 build`, the names it will accept,
//! and which pads hold loops. It reads nothing and writes nothing: a pad's length in seconds is
//! recorded by the host when the sample is picked.
//!
//! The pad ignores the loop's length on a note-on (a sample sounds in full), so a pad whose
//! sample is [`LOOP_SECONDS`] or longer counts as a **loop pad**: the hub never plays it from
//! the step pattern or a loop route ([`super::pd705::Kit::holds_back`]). A slot can override
//! that either way.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::pd705::{input_name, Kit, INPUTS};
use super::pd705_load::Tracker;
use super::pd705_pattern::KitPattern;
use crate::json::{JsonObject, JsonValue};

/// Kits the library keeps (the pad holds C1-C20).
pub const LIBRARY_MAX: usize = 20;
/// The longest kit name the pad takes (WaveManager's rule).
pub const KIT_NAME_MAX: usize = 13;
/// The longest sound name.
pub const SOUND_NAME_MAX: usize = 16;
/// A sample this long or longer is a loop pad by default.
pub const LOOP_SECONDS: f64 = 2.0;
/// The pad's own loudness for a sound when the kit says nothing (0-127).
pub const DEFAULT_VOLUME: u8 = 100;
/// The schema tag in the store's document.
pub const SCHEMA: &str = "djrust/pd705-kits/v1";
/// The Pi Zero that stands in for the pad's USB stick (pd705-rs `Pi::default`).
pub const DEFAULT_HOST: &str = env!("DJRUST_PD705_HOST");

/// WaveManager's name rule `^[a-zA-Z0-9]+[-~a-zA-Z0-9_\.' ]*[a-zA-Z0-9]$` (pd705-rs
/// `names.rs`): letters and digits at both ends, those and `- ~ _ . '` and space between.
pub fn check_name(name: &str, max: usize, what: &str) -> Result<(), String> {
    let b = name.as_bytes();
    if b.is_empty() || b.len() > max {
        return Err(format!("{what} name {name:?}: 1-{max} characters"));
    }
    let edge = |c: u8| c.is_ascii_alphanumeric();
    let inner = |c: u8| edge(c) || b"-~_.' ".contains(&c);
    if edge(b[0]) && edge(b[b.len() - 1]) && b.iter().all(|c| inner(*c)) {
        Ok(())
    } else {
        Err(format!(
            "{what} name {name:?}: start and end with a letter or digit; in between letters, \
             digits, space or - ~ _ . '"
        ))
    }
}

/// A sound name for `text` (a file stem or a loop's title) the pad will take: bad characters
/// become `_`, the edges are trimmed to letters and digits, it is cut to
/// [`SOUND_NAME_MAX`], and a number is added if `taken` already has it.
pub fn suggest_sound_name(text: &str, taken: &[String]) -> String {
    let mut s: String = text
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "-~_.' ".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    s = s
        .trim_matches(|c: char| !c.is_ascii_alphanumeric())
        .to_string();
    if s.len() > SOUND_NAME_MAX {
        s.truncate(SOUND_NAME_MAX);
        s = s
            .trim_end_matches(|c: char| !c.is_ascii_alphanumeric())
            .to_string();
    }
    if s.is_empty() {
        s = String::from("Sample");
    }
    if !taken.contains(&s) {
        return s;
    }
    for n in 2..1000 {
        let tail = format!("{n}");
        let keep = SOUND_NAME_MAX - tail.len();
        let mut head = s.clone();
        head.truncate(keep);
        let head = head.trim_end_matches(|c: char| !c.is_ascii_alphanumeric());
        let cand = format!("{head}{tail}");
        if !taken.contains(&cand) {
            return cand;
        }
    }
    s
}

/// A pad input from what a person or a socket call writes: `7`, `pad7`, `Pad7`, `trig1`,
/// `foot2` (pd705-rs `source_index`, but 1-15 as `gear::pd705`).
pub fn parse_input(s: &str) -> Result<u8, String> {
    let l = s.trim().to_ascii_lowercase();
    let (base, n, max) = if let Some(r) = l.strip_prefix("pad") {
        (0, r, 9)
    } else if let Some(r) = l.strip_prefix("trig") {
        (9, r, 4)
    } else if let Some(r) = l.strip_prefix("foot") {
        (13, r, 2)
    } else {
        (0, l.as_str(), 15)
    };
    match n.trim().parse::<u8>() {
        Ok(k) if (1..=max).contains(&k) => Ok(base + k),
        _ => Err(format!(
            "pad {s:?}: 1-9 (or pad1-9), trig1-4 (10-13), foot1-2 (14-15)"
        )),
    }
}

/// One pad's sample.
#[derive(Clone, Debug, PartialEq)]
pub struct PadSlot {
    /// 1-15.
    pub input: u8,
    /// The WAV on disk the build reads (the hub exports a loop-row clip to a file when it is
    /// picked).
    pub path: String,
    /// Where it came from, in words (`loop A loop 3`, `file kick.wav`).
    pub from: String,
    /// The sound's name on the pad.
    pub name: String,
    /// 0-127.
    pub volume: u8,
    /// Its length, seconds (0: not known).
    pub seconds: f64,
    /// `Some(true)` / `Some(false)` overrides the length rule.
    pub loop_mode: Option<bool>,
}

impl PadSlot {
    /// A slot for `path`, named from `title`, with nothing else decided.
    pub fn new(input: u8, path: &str, title: &str, seconds: f64) -> PadSlot {
        PadSlot {
            input,
            path: path.to_string(),
            from: String::new(),
            name: suggest_sound_name(title, &[]),
            volume: DEFAULT_VOLUME,
            seconds,
            loop_mode: None,
        }
    }

    /// Whether the pad holds a loop here.
    pub fn is_loop(&self) -> bool {
        self.loop_mode.unwrap_or(self.seconds >= LOOP_SECONDS)
    }
}

/// A kit being put together.
#[derive(Clone, Debug, PartialEq)]
pub struct KitDraft {
    /// 1-13 characters, WaveManager's rule.
    pub name: String,
    /// Sorted by input, one per input.
    pub pads: Vec<PadSlot>,
    /// What the clock plays for this kit (None: the default house groove on pads 7 / 8 / 9).
    pub pattern: Option<KitPattern>,
}

impl KitDraft {
    pub fn new(name: &str) -> KitDraft {
        KitDraft {
            name: name.to_string(),
            pads: Vec::new(),
            pattern: None,
        }
    }

    pub fn pad(&self, input: u8) -> Option<&PadSlot> {
        self.pads.iter().find(|p| p.input == input)
    }

    /// Put a sample on a pad (replacing what was there). The sound name is made unique among
    /// the other pads' that hold other files.
    pub fn set_pad(&mut self, mut slot: PadSlot) -> Result<(), String> {
        if !(1..=INPUTS).contains(&slot.input) {
            return Err(format!(
                "pad {}: 1-9 (pads), 10-13 (trig 1-4), 14-15 (foot 1-2)",
                slot.input
            ));
        }
        if slot.path.contains(',') {
            return Err(format!(
                "{}: a comma in the path cannot be given to `pd705 build`; copy the file to a name without one",
                slot.path
            ));
        }
        if slot.path.trim().is_empty() {
            return Err("a pad needs a sample file".into());
        }
        if slot.volume > 127 {
            return Err(format!("volume {}: 0-127", slot.volume));
        }
        // the same name for a different file is refused by the builder: keep names apart
        let taken: Vec<String> = self
            .pads
            .iter()
            .filter(|p| p.input != slot.input && p.path != slot.path)
            .map(|p| p.name.clone())
            .collect();
        if check_name(&slot.name, SOUND_NAME_MAX, "sound").is_err() || taken.contains(&slot.name) {
            slot.name = suggest_sound_name(&slot.name, &taken);
        }
        self.pads.retain(|p| p.input != slot.input);
        self.pads.push(slot);
        self.pads.sort_by_key(|p| p.input);
        Ok(())
    }

    /// Take a pad's sample off. Whether there was one.
    pub fn clear_pad(&mut self, input: u8) -> bool {
        let n = self.pads.len();
        self.pads.retain(|p| p.input != input);
        self.pads.len() != n
    }

    /// The inputs that hold a sample.
    pub fn inputs(&self) -> Vec<u8> {
        self.pads.iter().map(|p| p.input).collect()
    }

    /// The loop pads, in input order.
    pub fn loop_pads(&self) -> Vec<u8> {
        self.pads
            .iter()
            .filter(|p| p.is_loop())
            .map(|p| p.input)
            .collect()
    }

    /// The kit as the hub's hold-back needs it.
    pub fn to_kit(&self) -> Kit {
        Kit {
            name: self.name.clone(),
            loop_pads: self.loop_pads(),
        }
    }

    /// Everything wrong with the draft, in words; Ok when `pd705 build` should take it.
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut bad = Vec::new();
        if let Err(e) = check_name(&self.name, KIT_NAME_MAX, "kit") {
            bad.push(e);
        }
        if self.pads.is_empty() {
            bad.push("no samples on any pad".into());
        }
        for p in &self.pads {
            if let Err(e) = check_name(&p.name, SOUND_NAME_MAX, "sound") {
                bad.push(format!("{}: {e}", input_name(p.input)));
            }
            if p.path.contains(',') {
                bad.push(format!("{}: a comma in {}", input_name(p.input), p.path));
            }
            for q in &self.pads {
                if q.input > p.input && q.name == p.name && q.path != p.path {
                    bad.push(format!(
                        "{} and {} are both named {:?} but hold different files",
                        input_name(p.input),
                        input_name(q.input),
                        p.name
                    ));
                }
            }
        }
        if bad.is_empty() {
            Ok(())
        } else {
            Err(bad)
        }
    }

    /// The `pd705 build` arguments: this one kit, its pads, `-o OUT --force` when `out` is given.
    /// (Without `out`, `--to-pi` builds into a temp folder itself.)
    pub fn build_args(&self, out: Option<&str>) -> Vec<String> {
        let mut a = alloc::vec![
            String::from("build"),
            String::from("--kit"),
            self.name.clone()
        ];
        for p in &self.pads {
            a.push(String::from("--pad"));
            a.push(format!(
                "{}={},name={},vol={}",
                input_name(p.input).to_ascii_lowercase(),
                p.path,
                p.name,
                p.volume
            ));
        }
        if let Some(o) = out {
            a.push(String::from("-o"));
            a.push(o.to_string());
            a.push(String::from("--force"));
        }
        a
    }

    /// The arguments to install this kit on the Pi stick: the build (into `out`, a file named
    /// `pad-content.hppd`, so the caller owns the temp file and removes it), then `--to-pi`.
    /// `dry_run` only prints what would run (reads the sample files, opens no connection).
    pub fn install_args(&self, out: &str, host: &str, dry_run: bool) -> Vec<String> {
        let mut a = self.build_args(Some(out));
        a.push(String::from("--to-pi"));
        a.push(String::from("--pi-host"));
        a.push(host.to_string());
        if dry_run {
            a.push(String::from("--dry-run"));
        }
        a
    }

    // MARK: JSON

    pub fn to_json(&self) -> JsonValue {
        let mut o = JsonObject::new();
        o.set("name", JsonValue::str(self.name.clone()));
        o.set(
            "pads",
            JsonValue::Array(
                self.pads
                    .iter()
                    .map(|p| {
                        let mut s = JsonObject::new();
                        s.set("pad", JsonValue::int(p.input as i64));
                        s.set("path", JsonValue::str(p.path.clone()));
                        s.set("from", JsonValue::str(p.from.clone()));
                        s.set("name", JsonValue::str(p.name.clone()));
                        s.set("vol", JsonValue::int(p.volume as i64));
                        s.set("seconds", JsonValue::Number(p.seconds));
                        if let Some(l) = p.loop_mode {
                            s.set("loop", JsonValue::Bool(l));
                        }
                        JsonValue::Object(s)
                    })
                    .collect(),
            ),
        );
        if let Some(p) = &self.pattern {
            o.set("pattern", p.to_json());
        }
        JsonValue::Object(o)
    }

    pub fn from_json(v: &JsonValue) -> Result<KitDraft, String> {
        let name = v
            .get("name")
            .and_then(JsonValue::as_str)
            .ok_or("a kit needs \"name\"")?;
        let mut k = KitDraft::new(name);
        for p in v
            .get("pads")
            .and_then(JsonValue::as_array)
            .map(|a| a.as_slice())
            .unwrap_or(&[])
        {
            let input = p
                .get("pad")
                .and_then(JsonValue::as_i64)
                .filter(|i| (1..=INPUTS as i64).contains(i))
                .ok_or("a pad needs \"pad\" 1-15")? as u8;
            let path = p
                .get("path")
                .and_then(JsonValue::as_str)
                .ok_or_else(|| format!("{}: needs \"path\"", input_name(input)))?;
            let mut slot = PadSlot::new(
                input,
                path,
                p.get("name").and_then(JsonValue::as_str).unwrap_or(path),
                p.get("seconds").and_then(JsonValue::as_f64).unwrap_or(0.0),
            );
            slot.from = p
                .get("from")
                .and_then(JsonValue::as_str)
                .unwrap_or("")
                .to_string();
            slot.volume = p
                .get("vol")
                .and_then(JsonValue::as_i64)
                .map_or(DEFAULT_VOLUME, |v| v.clamp(0, 127) as u8);
            slot.loop_mode = p.get("loop").and_then(JsonValue::as_bool);
            k.set_pad(slot)?;
        }
        if let Some(p) = v.get("pattern").filter(|p| p.is_object_like()) {
            k.pattern = Some(KitPattern::from_json(p)?);
        }
        Ok(k)
    }
}

/// The hub's kits: the library of drafts, which one is selected, where the stick's Pi is, and
/// what is known of the kit on the pad. One JSON document on disk (`crate::host::pd705::BookStore`).
#[derive(Clone, Debug, PartialEq)]
pub struct KitBook {
    pub kits: Vec<KitDraft>,
    /// The name of the draft the window and the socket act on by default.
    pub selected: String,
    /// `user@host` of the Pi Zero.
    pub host: String,
    pub tracker: Tracker,
}

impl Default for KitBook {
    fn default() -> Self {
        KitBook {
            kits: Vec::new(),
            selected: String::new(),
            host: DEFAULT_HOST.to_string(),
            tracker: Tracker::default(),
        }
    }
}

impl KitBook {
    pub fn get(&self, name: &str) -> Option<&KitDraft> {
        self.kits.iter().find(|k| k.name == name)
    }

    pub fn get_mut(&mut self, name: &str) -> Option<&mut KitDraft> {
        self.kits.iter_mut().find(|k| k.name == name)
    }

    /// The selected draft.
    pub fn current(&self) -> Option<&KitDraft> {
        self.get(&self.selected)
    }

    pub fn current_mut(&mut self) -> Option<&mut KitDraft> {
        let n = self.selected.clone();
        self.get_mut(&n)
    }

    /// A draft by name, or the selected one when `name` is None.
    pub fn pick(&self, name: Option<&str>) -> Result<&KitDraft, String> {
        match name {
            Some(n) => self.get(n).ok_or_else(|| self.no_kit(n)),
            None => self
                .current()
                .ok_or_else(|| "no kit is selected: `kits add` one first".to_string()),
        }
    }

    pub fn no_kit(&self, name: &str) -> String {
        let names: Vec<&str> = self.kits.iter().map(|k| k.name.as_str()).collect();
        format!("no kit {name:?}; kits are {}", if names.is_empty() { "(none)".to_string() } else { names.join(", ") })
    }

    fn name_free(&self, name: &str) -> Result<(), String> {
        check_name(name, KIT_NAME_MAX, "kit")?;
        if self.kits.iter().any(|k| k.name.eq_ignore_ascii_case(name)) {
            return Err(format!("a kit named {name:?} exists"));
        }
        Ok(())
    }

    /// A new empty draft, selected.
    pub fn add(&mut self, name: &str) -> Result<&mut KitDraft, String> {
        if self.kits.len() >= LIBRARY_MAX {
            return Err(format!("the library holds {LIBRARY_MAX} kits (the pad's C1-C{LIBRARY_MAX}): remove one"));
        }
        self.name_free(name)?;
        self.kits.push(KitDraft::new(name));
        self.selected = name.to_string();
        Ok(self.kits.last_mut().expect("just pushed"))
    }

    /// A copy of `from` under `name`, selected.
    pub fn duplicate(&mut self, from: &str, name: &str) -> Result<(), String> {
        let mut k = self.get(from).cloned().ok_or_else(|| self.no_kit(from))?;
        if self.kits.len() >= LIBRARY_MAX {
            return Err(format!("the library holds {LIBRARY_MAX} kits: remove one"));
        }
        self.name_free(name)?;
        k.name = name.to_string();
        self.kits.push(k);
        self.selected = name.to_string();
        Ok(())
    }

    pub fn rename(&mut self, from: &str, to: &str) -> Result<(), String> {
        if !from.eq_ignore_ascii_case(to) {
            self.name_free(to)?;
        } else {
            check_name(to, KIT_NAME_MAX, "kit")?;
        }
        let was_selected = self.selected == from;
        self.get_mut(from).ok_or_else(|| format!("no kit {from:?}"))?.name = to.to_string();
        if was_selected {
            self.selected = to.to_string();
        }
        Ok(())
    }

    pub fn remove(&mut self, name: &str) -> Result<(), String> {
        let n = self.kits.len();
        self.kits.retain(|k| k.name != name);
        if self.kits.len() == n {
            return Err(self.no_kit(name));
        }
        if self.selected == name {
            self.selected = self.kits.first().map(|k| k.name.clone()).unwrap_or_default();
        }
        Ok(())
    }

    pub fn select(&mut self, name: &str) -> Result<(), String> {
        if self.get(name).is_none() {
            return Err(self.no_kit(name));
        }
        self.selected = name.to_string();
        Ok(())
    }

    /// The step pattern for the kit the clock should play: the tracker's kit's own pattern, else
    /// None (the default groove).
    pub fn clock_pattern(&self) -> Option<&KitPattern> {
        let n = self.tracker.pattern_kit()?;
        self.get(n)?.pattern.as_ref()
    }

    pub fn to_json(&self) -> JsonValue {
        JsonValue::Object(JsonObject::from_pairs([
            ("schema", JsonValue::str(SCHEMA)),
            ("selected", JsonValue::str(self.selected.clone())),
            ("host", JsonValue::str(self.host.clone())),
            ("kits", JsonValue::Array(self.kits.iter().map(KitDraft::to_json).collect())),
            ("tracker", self.tracker.to_json()),
        ]))
    }

    /// A kit that no longer decodes is skipped (and counted in the second value) rather than
    /// losing the rest of the library.
    pub fn from_json(v: &JsonValue) -> (KitBook, usize) {
        let mut b = KitBook::default();
        let mut bad = 0;
        if let Some(h) = v.get("host").and_then(JsonValue::as_str).filter(|h| !h.trim().is_empty()) {
            b.host = h.to_string();
        }
        for k in v.get("kits").and_then(JsonValue::as_array).map(|a| a.as_slice()).unwrap_or(&[]) {
            match KitDraft::from_json(k) {
                Ok(k) if b.kits.len() < LIBRARY_MAX && b.get(&k.name).is_none() => b.kits.push(k),
                _ => bad += 1,
            }
        }
        b.selected = v
            .get("selected")
            .and_then(JsonValue::as_str)
            .filter(|n| b.get(n).is_some())
            .map(str::to_string)
            .or_else(|| b.kits.first().map(|k| k.name.clone()))
            .unwrap_or_default();
        if let Some(t) = v.get("tracker") {
            b.tracker = Tracker::from_json(t);
        }
        (b, bad)
    }
}

/// What `pd705 build` printed, read back.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BuildReport {
    /// `(sound name, seconds)` per `sound NAME 7.700 s PATH` line.
    pub sounds: Vec<(String, f64)>,
    /// The file it wrote and its size.
    pub wrote: Option<(String, u64)>,
}

/// Read the lines of a `pd705 build` run.
pub fn parse_build_output(text: &str) -> BuildReport {
    let mut r = BuildReport::default();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("sound ") {
            // `sound NAME(padded)  SECONDS s  PATH  (how)`: the seconds are the token before " s"
            let toks: Vec<&str> = rest.split_whitespace().collect();
            if let Some(i) = toks.iter().position(|t| *t == "s") {
                if i >= 1 {
                    if let Ok(sec) = toks[i - 1].parse::<f64>() {
                        let name = toks[..i - 1].join(" ");
                        r.sounds.push((name, sec));
                    }
                }
            }
        } else if let Some(rest) = line.strip_prefix("wrote ") {
            if let Some((path, tail)) = rest.rsplit_once(" (") {
                let bytes = tail
                    .split_whitespace()
                    .next()
                    .and_then(|n| n.parse::<u64>().ok());
                if let Some(b) = bytes {
                    r.wrote = Some((path.to_string(), b));
                }
            }
        }
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn slot(input: u8, path: &str, title: &str, secs: f64) -> PadSlot {
        PadSlot::new(input, path, title, secs)
    }

    fn acid() -> KitDraft {
        let mut k = KitDraft::new("Acid House");
        k.set_pad(slot(1, "/s/AcidLoopA.wav", "AcidLoopA", 7.7)).unwrap();
        k.set_pad(slot(7, "/s/Kick.wav", "Kick", 0.4)).unwrap();
        k.set_pad(slot(8, "/s/Snare.wav", "Snare", 0.3)).unwrap();
        k.set_pad(slot(9, "/s/HatClosed.wav", "HatClosed", 0.1)).unwrap();
        k
    }

    #[test]
    fn names_follow_the_pads_rule() {
        for ok in ["Kick", "Acid House", "a", "Hat 1", "it's", "x-y_z.1"] {
            assert!(check_name(ok, 16, "sound").is_ok(), "{ok}");
        }
        for bad in ["", "_bad", "bad_", " x", "x ", "a,b", "seventeen_chars_xx", "ü"] {
            assert!(check_name(bad, 16, "sound").is_err(), "{bad:?}");
        }
        assert!(check_name("Fourteen_chars", KIT_NAME_MAX, "kit").is_err());
    }

    #[test]
    fn suggested_names_are_always_valid_and_unique() {
        for t in ["A loop 3", "kick (1)", "***", "", "a very long sample title indeed", "x"] {
            let n = suggest_sound_name(t, &[]);
            assert!(check_name(&n, SOUND_NAME_MAX, "sound").is_ok(), "{t:?} -> {n:?}");
        }
        assert_eq!(suggest_sound_name("kick (1)", &[]), "kick _1");
        assert_eq!(suggest_sound_name("***", &[]), "Sample");
        let taken = alloc::vec![String::from("Kick"), String::from("Kick2")];
        assert_eq!(suggest_sound_name("Kick", &taken), "Kick3");
        // a long name keeps room for the number
        let long = suggest_sound_name("abcdefghijklmnop", &[]);
        assert_eq!(long.len(), 16);
        let again = suggest_sound_name("abcdefghijklmnop", &[long]);
        assert!(again.len() <= 16 && again.ends_with('2'), "{again}");
    }

    #[test]
    fn inputs_parse_like_the_builders_sources() {
        assert_eq!(parse_input("7"), Ok(7));
        assert_eq!(parse_input("Pad9"), Ok(9));
        assert_eq!(parse_input("trig1"), Ok(10));
        assert_eq!(parse_input("trig4"), Ok(13));
        assert_eq!(parse_input("foot2"), Ok(15));
        assert_eq!(parse_input("15"), Ok(15));
        for bad in ["0", "16", "pad10", "trig5", "foot3", "kick", ""] {
            assert!(parse_input(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn long_samples_are_loop_pads_unless_overridden() {
        let mut k = acid();
        assert_eq!(k.loop_pads(), [1]);
        assert_eq!(k.to_kit().loop_pads, [1]);
        assert!(k.to_kit().holds_back(49));
        assert!(!k.to_kit().holds_back(36));
        // a long pad marked one-shot plays; a short one marked loop is held back
        let mut s = k.pad(1).unwrap().clone();
        s.loop_mode = Some(false);
        k.set_pad(s).unwrap();
        let mut s = k.pad(7).unwrap().clone();
        s.loop_mode = Some(true);
        k.set_pad(s).unwrap();
        assert_eq!(k.loop_pads(), [7]);
        // exactly the limit counts as a loop
        assert!(slot(2, "/s/x.wav", "x", LOOP_SECONDS).is_loop());
        assert!(!slot(2, "/s/x.wav", "x", LOOP_SECONDS - 0.01).is_loop());
    }

    #[test]
    fn setting_a_pad_replaces_keeps_order_and_separates_names() {
        let mut k = KitDraft::new("K");
        k.set_pad(slot(9, "/s/a.wav", "Hat", 0.1)).unwrap();
        k.set_pad(slot(2, "/s/b.wav", "Hat", 0.1)).unwrap();
        assert_eq!(k.inputs(), [2, 9]);
        // two different files may not share a sound name
        assert_ne!(k.pad(2).unwrap().name, k.pad(9).unwrap().name);
        assert!(k.validate().is_ok());
        // the same file on two pads shares one
        k.set_pad(slot(3, "/s/a.wav", "Hat", 0.1)).unwrap();
        assert_eq!(k.pad(3).unwrap().name, k.pad(9).unwrap().name);
        assert!(k.validate().is_ok());
        // replacing a pad
        k.set_pad(slot(9, "/s/c.wav", "Ride", 0.5)).unwrap();
        assert_eq!(k.pad(9).unwrap().path, "/s/c.wav");
        assert_eq!(k.pads.len(), 3);
        assert!(k.clear_pad(9));
        assert!(!k.clear_pad(9));
        // refusals
        assert!(k.set_pad(slot(0, "/s/a.wav", "x", 1.0)).is_err());
        assert!(k.set_pad(slot(16, "/s/a.wav", "x", 1.0)).is_err());
        assert!(k.set_pad(slot(4, "/s/a,b.wav", "x", 1.0)).unwrap_err().contains("comma"));
        assert!(k.set_pad(slot(4, "", "x", 1.0)).is_err());
    }

    #[test]
    fn validation_lists_every_problem() {
        let mut k = KitDraft::new("_bad");
        let e = k.validate().unwrap_err();
        assert!(e.iter().any(|m| m.contains("kit name")), "{e:?}");
        assert!(e.iter().any(|m| m.contains("no samples")), "{e:?}");
        k.name = "Fine".into();
        k.pads.push(PadSlot {
            name: "bad_".into(),
            ..slot(3, "/s/a.wav", "x", 1.0)
        });
        let e = k.validate().unwrap_err();
        assert!(e.iter().any(|m| m.starts_with("Pad3")), "{e:?}");
    }

    #[test]
    fn build_arguments_are_what_pd705_build_takes() {
        let k = acid();
        assert_eq!(
            k.build_args(Some("/tmp/out/pad-content.hppd")),
            [
                "build",
                "--kit",
                "Acid House",
                "--pad",
                "pad1=/s/AcidLoopA.wav,name=AcidLoopA,vol=100",
                "--pad",
                "pad7=/s/Kick.wav,name=Kick,vol=100",
                "--pad",
                "pad8=/s/Snare.wav,name=Snare,vol=100",
                "--pad",
                "pad9=/s/HatClosed.wav,name=HatClosed,vol=100",
                "-o",
                "/tmp/out/pad-content.hppd",
                "--force"
            ]
        );
        let mut t = KitDraft::new("T");
        t.set_pad(slot(10, "/s/k.wav", "k", 0.1)).unwrap();
        t.set_pad(slot(14, "/s/h.wav", "h", 0.1)).unwrap();
        let a = t.build_args(None);
        assert!(a.contains(&"trig1=/s/k.wav,name=k,vol=100".to_string()));
        assert!(a.contains(&"foot1=/s/h.wav,name=h,vol=100".to_string()));
        assert!(!a.contains(&"-o".to_string()));
    }

    #[test]
    fn a_dry_run_install_carries_dry_run_and_a_real_one_does_not() {
        let k = acid();
        let dry = k.install_args("/t/pad-content.hppd", "pi@raspberrypi.local", true);
        assert_eq!(dry[dry.len() - 4..], ["--to-pi", "--pi-host", "pi@raspberrypi.local", "--dry-run"]);
        let real = k.install_args("/t/pad-content.hppd", "pi@raspberrypi.local", false);
        assert!(!real.contains(&"--dry-run".to_string()));
        assert!(real.contains(&"--to-pi".to_string()));
        // the build goes to the caller's file, so nothing is left behind in a temp folder
        for a in [&dry, &real] {
            assert!(a.windows(2).any(|w| w == ["-o", "/t/pad-content.hppd"]), "{a:?}");
            assert!(a.contains(&"--force".to_string()));
        }
    }

    #[test]
    fn json_round_trips() {
        let mut k = acid();
        k.pattern = Some(KitPattern::house(&k.inputs()));
        let mut s = k.pad(7).unwrap().clone();
        s.loop_mode = Some(true);
        s.from = "loop A loop 3".into();
        s.volume = 90;
        k.set_pad(s).unwrap();
        let text = k.to_json().canonical_js();
        let back = KitDraft::from_json(&JsonValue::parse(&text).unwrap()).unwrap();
        assert_eq!(back, k);
        assert!(KitDraft::from_json(&JsonValue::parse("{}").unwrap()).is_err());
        assert!(KitDraft::from_json(&JsonValue::parse(r#"{"name":"K","pads":[{"pad":99,"path":"/x"}]}"#).unwrap()).is_err());
    }

    #[test]
    fn the_book_keeps_up_to_twenty_unique_names() {
        let mut b = KitBook::default();
        b.add("One").unwrap();
        assert_eq!(b.selected, "One");
        assert!(b.add("one").unwrap_err().contains("exists"));
        assert!(b.add("_bad").is_err());
        for i in 1..LIBRARY_MAX {
            b.add(&format!("Kit{i}")).unwrap();
        }
        assert_eq!(b.kits.len(), LIBRARY_MAX);
        assert!(b.add("Extra").unwrap_err().contains("holds"));
        b.rename("Kit3", "Renamed").unwrap();
        assert!(b.get("Renamed").is_some() && b.get("Kit3").is_none());
        assert!(b.rename("Renamed", "One").is_err());
        b.select("Renamed").unwrap();
        b.rename("Renamed", "Again").unwrap();
        assert_eq!(b.selected, "Again");
        b.remove("Again").unwrap();
        assert_eq!(b.selected, "One");
        assert!(b.remove("Again").unwrap_err().contains("no kit"));
        b.duplicate("One", "Copy").unwrap();
        assert_eq!(b.selected, "Copy");
        assert!(b.pick(None).is_ok() && b.pick(Some("nope")).is_err());
    }

    #[test]
    fn the_clock_plays_the_pattern_of_the_kit_the_tracker_names() {
        use super::super::pd705_load::Record;
        let mut b = KitBook::default();
        b.add("Acid House").unwrap().pattern = Some(KitPattern::breaks(&[7, 8, 9]));
        b.add("Other").unwrap();
        assert_eq!(b.clock_pattern(), None, "nothing installed: the default groove");
        b.tracker.set_active(Record { kit: "Acid House".into(), sha256: "a".repeat(64), loop_pads: vec![1], at: 0 });
        assert_eq!(b.clock_pattern(), b.get("Acid House").unwrap().pattern.as_ref());
        b.tracker.staged_ok(Record { kit: "Other".into(), sha256: "b".repeat(64), loop_pads: vec![], at: 0 });
        assert!(b.clock_pattern().is_some(), "still the kit on the pad");
        b.tracker.confirm_loaded(1).unwrap();
        assert_eq!(b.clock_pattern(), None, "Other has no pattern of its own: the default groove");
        // a deleted draft: the default groove too
        b.remove("Other").unwrap();
        assert_eq!(b.clock_pattern(), None);
    }

    #[test]
    fn the_book_round_trips_and_skips_what_does_not_decode() {
        let mut b = KitBook::default();
        b.add("Acid House").unwrap().set_pad(slot(7, "/s/Kick.wav", "Kick", 0.4)).unwrap();
        b.add("Two").unwrap();
        b.host = "me@pi.local".into();
        b.tracker.set_active(super::super::pd705_load::Record { kit: "Acid House".into(), sha256: "a".repeat(64), loop_pads: vec![], at: 5 });
        let text = b.to_json().canonical_js();
        let (back, bad) = KitBook::from_json(&JsonValue::parse(&text).unwrap());
        assert_eq!((bad, &back), (0, &b));
        let (broken, bad) = KitBook::from_json(
            &JsonValue::parse(r#"{"selected":"Gone","kits":[{"name":"A"},{"nope":1},{"name":"A"}]}"#).unwrap(),
        );
        assert_eq!(bad, 2, "the broken one and the repeated name");
        assert_eq!((broken.kits.len(), broken.selected.as_str(), broken.host.as_str()), (1, "A", DEFAULT_HOST));
        assert_eq!(KitBook::from_json(&JsonValue::parse("{}").unwrap()).0, KitBook::default());
    }

    #[test]
    fn the_builders_output_is_read_back() {
        let out = "sound Kick              0.400 s  /s/Kick.wav (already 48 kHz/16-bit/mono)\n\
                   sound Acid Loop A       7.700 s  /s/AcidLoopA.wav (converted from 44100 Hz 32-bit 2ch)\n\
                   kit 1 (imports as the next free C slot): Acid House Pad1=AcidLoopA\n\
                   wrote /tmp/x/pad-content.hppd (2936823 bytes)\n";
        let r = parse_build_output(out);
        assert_eq!(r.sounds, [("Kick".to_string(), 0.4), ("Acid Loop A".to_string(), 7.7)]);
        assert_eq!(r.wrote, Some(("/tmp/x/pad-content.hppd".to_string(), 2_936_823)));
        assert_eq!(parse_build_output("nothing"), BuildReport::default());
    }
}
