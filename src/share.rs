//! Share envelopes and share links, port of `Beats/Share.swift` and `Beats/ShareURL.swift`
//! (beats' share/url.js `shareFromPayload`, `parseShareFromUrl`, `buildShareUrlForms`,
//! and the model-relevant part of share/apply.js).
//!
//! Raw-`nets` envelopes load literally and, with a `structure`, are expanded by
//! [`arrange_with_opts`]; composed ones get the overlay directives (`fadeIn`,
//! `drumBreak`, `feelCurve`, `counterMelody`) after the overrides.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::cid;
use crate::codec;
use crate::gen::arrange::{arrange_size, arrange_with_opts, ArrangeOpts};
use crate::gen::composer::{compose, generate_track_name};
use crate::json::{js_to_int32, JsonObject, JsonValue};
use crate::pflow::Project;

#[derive(Clone, Debug)]
pub struct Share {
    pub genre: Option<String>,
    pub name: Option<String>,
    /// Passed straight to `compose` as its overrides: seed, structure, cohesion,
    /// macroCurve, then every trait key, in that insertion order.
    pub params: JsonObject,
    /// Post-generation UI/project layers. None when the payload had none.
    pub overrides: Option<JsonObject>,
    pub nets: Option<JsonValue>,
    pub structure: Option<JsonValue>,
    pub arrange_seed: Option<f64>,
    pub velocity_deltas: Option<JsonValue>,
    pub max_variants: Option<f64>,
    pub fade_in: Option<Vec<JsonValue>>,
    pub drum_break: Option<f64>,
    pub sections: Option<Vec<JsonValue>>,
    pub feel_curve: Option<Vec<JsonValue>>,
    pub macro_curve: Option<Vec<JsonValue>>,
    pub counter_melody: Option<Vec<JsonValue>>,
    pub cid: Option<String>,
    pub parents: Option<Vec<JsonValue>>,
    /// The envelope as given.
    pub payload: JsonValue,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShareError {
    CidMismatch,
    Json,
}

/// Why a share has no project.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unported {
    /// No genre (and no nets): the JS throws.
    NoGenre,
}

impl Share {
    /// `shareFromPayload(payload, cid)`
    pub fn from_payload(p: &JsonValue, cid: Option<&str>) -> Share {
        let get = |k: &str| p.get(k);
        let mut params = JsonObject::new();
        if let Some(seed @ JsonValue::Number(_)) = get("seed") {
            params.set("seed", seed.clone());
        }
        if let Some(s) = get("structure").filter(|s| s.is_truthy()) {
            params.set("structure", s.clone());
        }
        match get("cohesion").and_then(JsonValue::as_str) {
            Some(c @ ("v1" | "v2")) => params.set("cohesion", c.into()),
            // Sealed envelopes without the field predate cohesion v2: pin legacy explicitly.
            _ => params.set("cohesion", "v1".into()),
        }
        if let Some(mc) = get("macroCurve").filter(|m| m.as_array().is_some()) {
            params.set("macroCurve", mc.clone());
        }
        // Object.assign(params, traits): objects copy their entries; arrays by index.
        match get("traits") {
            Some(JsonValue::Object(t)) => {
                for (k, v) in t.iter() {
                    params.set(k, v.clone());
                }
            }
            Some(JsonValue::Array(a)) => {
                for (i, v) in a.iter().enumerate() {
                    params.set(i.to_string(), v.clone());
                }
            }
            _ => {}
        }

        let mut ov = JsonObject::new();
        for key in [
            "tracks",
            "fx",
            "feel",
            "autoDj",
            "macrosDisabled",
            "initialMutes",
        ] {
            if let Some(v) = get(key).filter(|v| v.is_truthy()) {
                ov.set(key, v.clone());
            }
        }
        for key in ["tempo", "swing", "humanize"] {
            if let Some(v @ JsonValue::Number(_)) = get(key) {
                ov.set(key, v.clone());
            }
        }
        if let Some(n @ JsonValue::String(_)) = get("note") {
            ov.set("note", n.clone());
        }

        let number = |k: &str| get(k).and_then(JsonValue::as_f64);
        let list = |k: &str| get(k).and_then(JsonValue::as_array).cloned();
        let truthy = |k: &str| get(k).filter(|v| v.is_truthy()).cloned();
        Share {
            genre: get("genre").and_then(JsonValue::as_str).map(String::from),
            name: get("name")
                .filter(|v| v.is_truthy())
                .and_then(JsonValue::as_str)
                .map(String::from),
            params,
            overrides: (!ov.is_empty()).then_some(ov),
            nets: truthy("nets"),
            structure: truthy("structure"),
            arrange_seed: number("arrangeSeed"),
            velocity_deltas: truthy("velocityDeltas"),
            max_variants: number("maxVariants"),
            fade_in: list("fadeIn"),
            drum_break: number("drumBreak"),
            sections: list("sections"),
            feel_curve: list("feelCurve"),
            macro_curve: list("macroCurve"),
            counter_melody: list("counterMelody"),
            cid: cid.filter(|c| !c.is_empty()).map(String::from),
            parents: list("parents"),
            payload: p.clone(),
        }
    }

    /// The object `shareFromPayload` returns, as JSON (`undefined` fields omitted).
    pub fn to_json(&self) -> JsonValue {
        fn v(x: &Option<JsonValue>) -> JsonValue {
            x.clone().unwrap_or(JsonValue::Null)
        }
        fn a(x: &Option<Vec<JsonValue>>) -> JsonValue {
            x.clone().map(JsonValue::Array).unwrap_or(JsonValue::Null)
        }
        fn n(x: Option<f64>) -> JsonValue {
            x.map(JsonValue::Number).unwrap_or(JsonValue::Null)
        }
        fn s(x: &Option<String>) -> JsonValue {
            x.clone().map(JsonValue::String).unwrap_or(JsonValue::Null)
        }
        let mut o = JsonObject::new();
        if let Some(g) = &self.genre {
            o.set("genre", g.as_str().into());
        }
        o.set("name", s(&self.name));
        o.set("params", self.params.clone().into());
        o.set(
            "overrides",
            self.overrides
                .clone()
                .map(JsonValue::Object)
                .unwrap_or(JsonValue::Null),
        );
        o.set("nets", v(&self.nets));
        o.set("structure", v(&self.structure));
        o.set("arrangeSeed", n(self.arrange_seed));
        o.set("velocityDeltas", v(&self.velocity_deltas));
        o.set("maxVariants", n(self.max_variants));
        o.set("fadeIn", a(&self.fade_in));
        o.set("drumBreak", n(self.drum_break));
        o.set("sections", a(&self.sections));
        o.set("feelCurve", a(&self.feel_curve));
        o.set("macroCurve", a(&self.macro_curve));
        o.set("counterMelody", a(&self.counter_melody));
        o.set("cid", s(&self.cid));
        o.set("parents", a(&self.parents));
        o.into()
    }

    /// Decode envelope JSON text. When `cid` is given the payload is re-hashed and must
    /// match it (the browser refuses mismatches).
    pub fn decode(text: &str, cid: Option<&str>) -> Result<Share, ShareError> {
        let payload = JsonValue::parse(text).map_err(|_| ShareError::Json)?;
        if let Some(c) = cid {
            if cid::share_cid(&payload) != c {
                return Err(ShareError::CidMismatch);
            }
        }
        Ok(Share::from_payload(&payload, cid))
    }

    /// The browser's `_pendingOverlay`.
    pub fn has_overlay(&self) -> bool {
        self.fade_in.is_some()
            || self.drum_break.is_some_and(|d| d != 0.0 && !d.is_nan())
            || self.feel_curve.is_some()
            || self.counter_melody.is_some()
    }

    /// The project the browser ends up with: for a raw-`nets` envelope
    /// [`raw_nets_project`](Self::raw_nets_project); otherwise the composer path,
    /// `compose(genre, params)`, then the model-level overrides, then the arrange overlay
    /// (`overlayProject`). `Err` for a payload with no genre (the JS throws there).
    pub fn project(&self) -> Result<Project, Unported> {
        if self.nets.is_some() {
            return self.raw_nets_project().ok_or(Unported::NoGenre);
        }
        let genre = self.genre.as_deref().ok_or(Unported::NoGenre)?;
        let mut proj = compose(genre, &JsonValue::Object(self.params.clone()));
        self.finish_composed(&mut proj);
        Ok(proj)
    }

    /// What the browser layers on a composed project, in its order: the model-level
    /// overrides (`_applyProjectSync`), then the arrange overlay (`overlayProject`).
    /// Shared with the decks (`TrackSpec::finish`).
    pub fn finish_composed(&self, proj: &mut Project) {
        if let Some(ov) = &self.overrides {
            proj.apply_share_overrides(ov);
        }
        self.overlay_project(proj);
    }

    /// The overlay `arrangeWithOpts(…, { overlayOnly: true, … })` the browser runs on a
    /// composed project once the worker has produced it, after the overrides. Runs on the
    /// parsed project, so `counterMelody` works (in the browser the overlay runs on plain
    /// JSON and a counter-melody throws: deliberate deviation, ROADMAP 2.8,
    /// GO-DIVERGENCE.md). A throw leaves the project as composed: the browser catches it
    /// and never sends the half-built project to the worker.
    pub fn overlay_project(&self, proj: &mut Project) {
        if !self.has_overlay() || self.nets.is_some() {
            return;
        }
        let mut opts = ArrangeOpts::new();
        opts.overlay_only = true;
        opts.fade_in = self.fade_in.clone().unwrap_or_default();
        opts.drum_break = self.drum_break.unwrap_or(0.0);
        opts.feel_curve = self.feel_curve.clone().unwrap_or_default();
        opts.counter_melody = self.counter_melody.clone().unwrap_or_default();
        let mut overlaid = proj.clone();
        let g = self
            .genre
            .as_deref()
            .filter(|g| !g.is_empty())
            .unwrap_or("wrapped");
        if arrange_with_opts(&mut overlaid, g, "", &opts).is_ok() {
            *proj = overlaid;
        }
    }

    /// The browser expands a raw-`nets` envelope with `arrangeWithOpts` when it carries a
    /// truthy `structure` other than "loop" (petri-note.js `_bootGenerate`).
    pub fn needs_arrange(&self) -> bool {
        match (&self.nets, &self.structure) {
            (Some(_), Some(s)) => *s != JsonValue::String("loop".into()),
            _ => false,
        }
    }

    /// The project the browser ends up with for a raw-`nets` envelope: the `project-load`
    /// it builds (name, tempo / swing / humanize from the overrides, the nets verbatim,
    /// `initialMutes`), expanded by `arrangeWithOpts` when [`needs_arrange`]
    /// (on the unparsed nets: [`ArrangeOpts::wire_form`]; a throw is caught and the
    /// project loads as far as the arrange got, as the JS `try/catch` does), then the
    /// model-level overrides that `_applyProjectSync` layers on once the worker answers.
    /// `counterMelody` is never passed to this arrange (the JS doesn't). `None` when the
    /// envelope has no nets.
    ///
    /// [`needs_arrange`]: Self::needs_arrange
    pub fn raw_nets_project(&self) -> Option<Project> {
        let nets = self.nets.as_ref()?;
        let ov = |k: &str| self.overrides.as_ref().and_then(|o| o.get(k));
        let mut proj = JsonObject::new();
        proj.set(
            "name",
            self.name
                .clone()
                .unwrap_or_else(|| self.derived_name())
                .into(),
        );
        proj.set("tempo", ov("tempo").cloned().unwrap_or(JsonValue::int(120)));
        proj.set("swing", ov("swing").cloned().unwrap_or(JsonValue::int(0)));
        proj.set(
            "humanize",
            ov("humanize").cloned().unwrap_or(JsonValue::int(0)),
        );
        proj.set("nets", nets.clone());
        if let Some(m) = ov("initialMutes").filter(|m| m.is_truthy()) {
            proj.set("initialMutes", m.clone());
        }
        let mut project = Project::from_wire_json(&JsonValue::Object(proj));
        if let (true, Some(structure)) = (self.needs_arrange(), &self.structure) {
            let mut opts = ArrangeOpts::new();
            opts.seed = self.arrange_seed.unwrap_or(0.0);
            opts.velocity_deltas = self
                .velocity_deltas
                .as_ref()
                .map(|d| d.as_object().cloned().unwrap_or_default());
            opts.max_variants = self.max_variants.unwrap_or(0.0);
            opts.fade_in = self.fade_in.clone().unwrap_or_default();
            opts.drum_break = self.drum_break.unwrap_or(0.0);
            opts.sections = self.sections.clone().unwrap_or_default();
            opts.feel_curve = self.feel_curve.clone().unwrap_or_default();
            opts.macro_curve = self.macro_curve.clone().unwrap_or_default();
            opts.wire_form = true;
            let g = self
                .genre
                .as_deref()
                .filter(|g| !g.is_empty())
                .unwrap_or("wrapped");
            let _ = arrange_with_opts(&mut project, g, &arrange_size(structure), &opts);
        }
        if let Some(ov) = &self.overrides {
            project.apply_share_overrides(ov);
        }
        Some(project)
    }

    /// `deriveShareName` (petri-note.js): the name of a hand-authored share with none.
    pub fn derived_name(&self) -> String {
        let mut seed: u32 = 0;
        if let Some(c) = &self.cid {
            let mut h: i32 = 5381;
            for u in c.encode_utf16() {
                h = h.wrapping_mul(33) ^ u as i32;
            }
            seed = h as u32;
        } else if let Some(s) = self.params.get("seed").and_then(JsonValue::as_f64) {
            seed = js_to_int32(s) as u32;
        }
        if seed == 0 {
            seed = 1;
        }
        let genre = self
            .genre
            .as_deref()
            .filter(|g| !g.is_empty())
            .unwrap_or("custom");
        generate_track_name(genre, seed as f64)
    }

    /// Legacy `{ genre, params }` links as a `Share` (the composer path).
    pub fn from_legacy(legacy: &JsonValue) -> Share {
        let mut p = JsonObject::new();
        if let Some(g) = legacy.get("genre") {
            p.set("genre", g.clone());
        }
        let mut s = Share::from_payload(&JsonValue::Object(p), None);
        s.params = legacy
            .get("params")
            .and_then(JsonValue::as_object)
            .cloned()
            .unwrap_or_default();
        s
    }
}

impl Project {
    /// The parts of `applyShareOverrides` that change the project model: per-channel
    /// `tracks` (mix merge, instrument, instrumentSet) and `initialMutes`.
    pub fn apply_share_overrides(&mut self, ov: &JsonObject) {
        if let Some(tracks) = ov.get("tracks").and_then(JsonValue::as_object) {
            for (ch, o) in tracks.iter() {
                let Some(ch_num) = parse_int_prefix(ch) else {
                    continue;
                };
                let ids: Vec<String> = self.nets.keys().to_vec();
                for id in ids {
                    let net = self.nets.get_mut(&id).unwrap();
                    if net.role == "control" || net.track.channel != ch_num {
                        continue;
                    }
                    if let Some(mix) = o.get("mix").filter(|m| m.is_truthy()) {
                        let mut merged = net
                            .track
                            .mix
                            .as_ref()
                            .and_then(JsonValue::as_object)
                            .cloned()
                            .unwrap_or_default();
                        if let Some(m) = mix.as_object() {
                            for (k, v) in m.iter() {
                                merged.set(k, v.clone());
                            }
                        }
                        net.track.mix = Some(merged.into());
                    }
                    if let Some(inst) = o
                        .get("instrument")
                        .and_then(JsonValue::as_str)
                        .filter(|i| !i.is_empty())
                    {
                        if net.track.sound.composed.is_none() {
                            net.track.sound.composed = Some(net.track.instrument.clone());
                        }
                        net.track.instrument = inst.into();
                        net.track.picked = true;
                    }
                    // dj's sound choices (never read by beats): a drum kit, a palette, a
                    // role's own palette option ("" = none)
                    if let Some(kit) = o.get("kit").and_then(JsonValue::as_str) {
                        net.track.kit = (!kit.is_empty()).then(|| kit.into());
                    }
                    if let Some(pal) = o.get("palette").and_then(JsonValue::as_str) {
                        net.track.palette = (!pal.is_empty()).then(|| pal.into());
                    }
                    if let Some(v) = o.get("voice").and_then(JsonValue::as_str) {
                        net.track.sound.override_ = (!v.is_empty()).then(|| v.into());
                    }
                    if let Some(arr) = o
                        .get("instrumentSet")
                        .filter(|s| s.is_truthy())
                        .and_then(JsonValue::as_array)
                    {
                        net.track.instrument_set = arr.clone();
                    }
                }
            }
        }
        if let Some(muted) = ov.get("initialMutes").and_then(JsonValue::as_array) {
            self.initial_mutes = muted
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
        }
    }
}

/// JS `parseInt(s)` for the integer-ish channel keys envelopes use.
fn parse_int_prefix(s: &str) -> Option<i64> {
    let mut digits = String::new();
    let mut seen_sign = false;
    for c in s.trim_start_matches(' ').chars() {
        if digits.is_empty() && !seen_sign && (c == '-' || c == '+') {
            seen_sign = true;
            digits.push(c);
        } else if c.is_ascii_digit() {
            digits.push(c);
        } else {
            break;
        }
    }
    digits.parse().ok()
}

// ---------------------------------------------------------------------------
// Share links

/// A parsed share link (`parseShareFromUrl`).
#[allow(clippy::large_enum_variant)] // parsed once per link; boxing buys nothing
#[derive(Clone, Debug)]
pub enum ShareLink {
    /// A verified `share-v1` envelope.
    Envelope(Share),
    /// A short link; `legacy` is what the JS falls back to when the fetch fails.
    NeedsFetch {
        cid: String,
        legacy: Option<JsonValue>,
    },
    /// A legacy `?p=` / `?g=` link.
    Legacy(JsonValue),
}

/// The short (`?cid=`) and self-contained (`?cid=&z=`) forms for a payload.
pub struct ShareForms {
    pub cid: String,
    pub short: String,
    pub full: String,
}

impl ShareLink {
    /// Parse a share URL: a full URL, `?query`, or a bare `a=b&c=d` query. As an
    /// extension, a URL with no query but a `#a=b` fragment reads the fragment.
    pub fn parse(link: &str) -> Option<ShareLink> {
        let q = Query::new(query_string(link));
        if let Some(c) = q.get("cid").filter(|c| !c.is_empty()) {
            match q.get("z").filter(|z| !z.is_empty()) {
                Some(z) => {
                    // JS: any decode/parse failure is swallowed and falls through.
                    if let Ok(text) = codec::b64url_to_gunzip(z) {
                        if let Ok(payload) = JsonValue::parse(&text) {
                            if cid::share_cid(&payload) == c {
                                return Some(ShareLink::Envelope(Share::from_payload(
                                    &payload,
                                    Some(c),
                                )));
                            }
                        }
                    }
                }
                None => {
                    return Some(ShareLink::NeedsFetch {
                        cid: c.into(),
                        legacy: legacy(&q),
                    })
                }
            }
        }
        legacy(&q).map(ShareLink::Legacy)
    }

    /// `buildShareUrlForms`
    pub fn forms(payload: &JsonValue, base: &str) -> ShareForms {
        let canonical = payload.canonical_js();
        let c = cid::share_cid_canonical(&canonical);
        let short = alloc::format!("{base}?cid={c}");
        let full = alloc::format!("{base}?cid={c}&z={}", codec::gzip_to_b64url(&canonical));
        ShareForms {
            cid: c,
            short,
            full,
        }
    }
}

fn legacy(q: &Query) -> Option<JsonValue> {
    if let Some(p) = q.get("p").filter(|p| !p.is_empty()) {
        if let Some(decoded) = codec::b64url_decode(p) {
            if decoded.get("genre").is_some_and(JsonValue::is_truthy) {
                return Some(decoded);
            }
        }
    }
    let g = q.get("g").filter(|g| !g.is_empty())?;
    let mut params = JsonObject::new();
    if let Some(n) = q.get("s").and_then(js_parse_int) {
        params.set("seed", n.into());
    }
    if let Some(st) = q.get("structure").filter(|s| !s.is_empty()) {
        params.set("structure", st.into());
    }
    if let Some(traits) = q
        .get("t")
        .filter(|t| !t.is_empty())
        .and_then(codec::b64url_decode)
    {
        match traits {
            JsonValue::Object(o) => {
                for (k, v) in o.iter() {
                    params.set(k, v.clone());
                }
            }
            JsonValue::Array(a) => {
                for (i, v) in a.into_iter().enumerate() {
                    params.set(i.to_string(), v);
                }
            }
            _ => {}
        }
    }
    Some(JsonObject::from_pairs([("genre", g.into()), ("params", params.into())]).into())
}

fn query_string(link: &str) -> &str {
    let (s, fragment) = match link.find('#') {
        Some(h) => (&link[..h], &link[h + 1..]),
        None => (link, ""),
    };
    if let Some(qm) = s.find('?') {
        return &s[qm + 1..];
    }
    if !fragment.is_empty() {
        return fragment;
    }
    if s.contains("://") {
        ""
    } else {
        s
    }
}

/// `new URLSearchParams(search)`: `+` is a space, percent escapes decode as UTF-8 (bad
/// sequences become U+FFFD), `get` returns the first match.
struct Query {
    pairs: Vec<(String, String)>,
}

impl Query {
    fn new(s: &str) -> Query {
        let pairs = s
            .split('&')
            .filter(|p| !p.is_empty())
            .map(|part| match part.split_once('=') {
                Some((k, v)) => (decode(k), decode(v)),
                None => (decode(part), String::new()),
            })
            .collect();
        Query { pairs }
    }
    fn get(&self, key: &str) -> Option<&str> {
        self.pairs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

fn decode(s: &str) -> String {
    fn hex(c: u8) -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'A'..=b'F' => Some(c - b'A' + 10),
            b'a'..=b'f' => Some(c - b'a' + 10),
            _ => None,
        }
    }
    let u = s.as_bytes();
    let mut out = Vec::with_capacity(u.len());
    let mut i = 0;
    while i < u.len() {
        if u[i] == b'+' {
            out.push(b' ');
        } else if u[i] == b'%' && i + 2 < u.len() {
            match (hex(u[i + 1]), hex(u[i + 2])) {
                (Some(a), Some(b)) => {
                    out.push(a << 4 | b);
                    i += 2;
                }
                _ => out.push(u[i]),
            }
        } else {
            out.push(u[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// JS `parseInt(s, 10)`: leading whitespace, optional sign, digits; None for NaN.
fn js_parse_int(s: &str) -> Option<f64> {
    let t = s.trim_start();
    let (sign, rest) = match t.as_bytes().first() {
        Some(b'-') => (-1.0, &t[1..]),
        Some(b'+') => (1.0, &t[1..]),
        _ => (1.0, t),
    };
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse::<f64>().ok().map(|d| sign * d)
}
