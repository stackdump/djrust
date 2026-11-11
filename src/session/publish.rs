//! A session laid out the way the CDN stores a CID: `blobs/<cid>/` holding
//! `index.md` (frontmatter is the metadata of record), `session.json` (the canonical bytes —
//! the directory name is the hash of exactly this) and optionally `audio.wav`. Port of the
//! pure parts of `Session/SessionPublish.swift`: the bundle, its refusal rules, the transport
//! trait and the staging order. The directory and ssh transports are `crate::host::publish`.
//!
//! This mirrors beats-builder's `publish.cdn.publish_to_cdn`: stage the files into the CDN
//! master's blob tree on the CDN host, ping `/admin/reload?cid=`, and (separately, and only on
//! request) run `mirror-push.sh`, which is what makes it public on pflow.dev.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

use super::action::DeckAction;
use super::model::{DeckSource, Session, SessionEventKind};
use crate::json::JsonValue;

/// One file of a bundle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleFile {
    pub name: String,
    pub data: Vec<u8>,
}

/// `PublishError`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PublishError {
    UnsafeCid(String),
    CommandFailed(String, i32, String),
    /// The session plays sounds that may only be played on this Mac (what, where).
    LocalOnlySounds(Vec<String>),
    /// The session puts outside audio (stock loops) on the loop row (what, where). No override.
    ImportedClips(Vec<String>),
}

impl fmt::Display for PublishError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PublishError::UnsafeCid(c) => write!(f, "refusing to publish unsafe CID {c}"),
            PublishError::LocalOnlySounds(what) => write!(
                f,
                "refusing to publish: the set plays sounds built from local game assets, for \
                 this Mac only ({}); pass the override to publish anyway",
                what.join("; ")
            ),
            PublishError::ImportedClips(what) => write!(
                f,
                "refusing to publish: the set puts stock loops on the loop row ({}); their \
                 licence covers playing them, not publishing them",
                what.join("; ")
            ),
            PublishError::CommandFailed(c, s, e) => write!(f, "{c} exited {s}: {e}"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for PublishError {}

/// `SessionBundle`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionBundle {
    pub cid: String,
    /// In write order. `index.md` is last: the indexer ignores a directory without one, so a
    /// half-copied bundle is never served.
    pub files: Vec<BundleFile>,
}

/// Where a published bundle is served, `<base><cid>/`: the build-time `DJRUST_PUBLISH_BASE`
/// (default `https://cdn.bitwrap.io/ipfs/`; README.md, "Build-time settings"). Use
/// [`SessionBundle::public_url_at`] to pick another CDN at run time.
pub const PUBLIC_BASE: &str = env!("DJRUST_PUBLISH_BASE");

impl SessionBundle {
    /// The bundle's URL under [`PUBLIC_BASE`].
    pub fn public_url(&self) -> String {
        self.public_url_at(PUBLIC_BASE)
    }

    /// The bundle's URL under `base` (an `http(s)://…/` prefix; a missing trailing `/` is added).
    pub fn public_url_at(&self, base: &str) -> String {
        let sep = if base.ends_with('/') { "" } else { "/" };
        format!("{base}{sep}{}/", self.cid)
    }

    /// A CID as the CDN's `validCID` accepts it (6–128 of A–Z a–z 0–9 - _), which is also
    /// safe to put in a shell line and can't escape the blobs directory.
    pub fn is_safe_cid(s: &str) -> bool {
        let n = s.len();
        (6..=128).contains(&n)
            && s.bytes().all(|b| {
                b.is_ascii_lowercase()
                    || b.is_ascii_uppercase()
                    || b.is_ascii_digit()
                    || b == b'-'
                    || b == b'_'
            })
    }

    /// `renderedAt` is passed in (not read from the clock) so bundles are reproducible. A
    /// session that plays local-only sounds is refused — draft or not, a CID on the CDN is
    /// public — unless `allow_local_only`. One that puts stock loops on the row is refused,
    /// with no override.
    pub fn new(
        session: &Session,
        title: Option<&str>,
        audio_wav: Option<&[u8]>,
        rendered_at: &str,
        draft: bool,
        allow_local_only: bool,
        local: &LocalOnly,
    ) -> Result<SessionBundle, PublishError> {
        let imported = session.imported_clips();
        if !imported.is_empty() {
            return Err(PublishError::ImportedClips(imported));
        }
        let local_sounds = session.local_only_sounds(local);
        if !local_sounds.is_empty() && !allow_local_only {
            return Err(PublishError::LocalOnlySounds(local_sounds));
        }
        let cid = session.cid();
        if !Self::is_safe_cid(&cid) {
            return Err(PublishError::UnsafeCid(cid));
        }
        let mut files = alloc::vec![BundleFile {
            name: "session.json".into(),
            data: (session.canonical() + "\n").into_bytes(),
        }];
        if let Some(a) = audio_wav {
            files.push(BundleFile {
                name: "audio.wav".into(),
                data: a.to_vec(),
            });
        }
        let md = Self::index_markdown(
            session,
            &cid,
            title,
            rendered_at,
            draft,
            audio_wav.is_some(),
        );
        files.push(BundleFile {
            name: "index.md".into(),
            data: md.into_bytes(),
        });
        Ok(SessionBundle { cid, files })
    }

    /// The `index.md` the CDN indexes.
    pub fn index_markdown(
        session: &Session,
        cid: &str,
        title: Option<&str>,
        rendered_at: &str,
        draft: bool,
        has_audio: bool,
    ) -> String {
        let mut genres: Vec<String> = Vec::new();
        for (_, d) in session.decks.iter() {
            if let DeckSource::Net(spec) = d {
                if !genres.contains(&spec.genre) {
                    genres.push(spec.genre.clone());
                }
            }
        }
        let seconds = libm::round(session.duration()) as i64;
        let name = title
            .map(String::from)
            .unwrap_or_else(|| format!("Live set {}", &cid[..cid.len().min(10)]));
        let mut l: Vec<String> = alloc::vec![
            "---".into(),
            format!("title: {}", Self::yaml_string(&name)),
            format!("cid: {cid}"),
            "source: beats-btw-dj".into(),
            format!("schema: {}", super::model::SCHEMA),
        ];
        if let Some(g) = genres.first() {
            l.push(format!("genre: {g}"));
        }
        l.push(format!("renderedAt: {rendered_at}"));
        l.push("provenance: performance".into());
        if draft {
            l.push("draft: true".into());
        }
        l.push("tags:".into());
        l.push("  - live-set".into());
        for g in &genres {
            l.push(format!("  - {g}"));
        }
        l.push("---".into());
        l.push(String::new());
        let nd = session.decks.len();
        l.push(format!(
            "A {seconds} s live performance on {nd} deck{} ({} recorded events). \
             `session.json` is the canonical session; this directory's name is its CID, so it \
             re-renders to the same set.",
            if nd == 1 { "" } else { "s" },
            session.events.len()
        ));
        if has_audio {
            l.push("\n`audio.wav` is the offline render of that session.".into());
        }
        l.push(String::new());
        l.join("\n")
    }

    /// `yamlString`.
    pub fn yaml_string(s: &str) -> String {
        if s.is_empty() {
            return "\"\"".into();
        }
        let needs = s.chars().any(|c| ":#[]{},&*!|>'\"%@`\\".contains(c))
            || s != s.trim_matches(|c: char| c == ' ' || c == '\t');
        if !needs {
            return s.to_string();
        }
        let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
        format!("\"{escaped}\"")
    }
}

/// Where a bundle goes (`CDNTransport`).
pub trait CdnTransport {
    /// Put the bundle's files under `blobs/<cid>/`, reporting each file as it starts.
    fn stage(
        &self,
        bundle: &SessionBundle,
        progress: &mut dyn FnMut(PublishStep),
    ) -> Result<(), PublishError>;
    /// Tell the CDN master to index just this CID. Returns whether it said yes.
    fn reload(&self, cid: &str) -> Result<bool, PublishError>;
    /// Push the master's blob tree to the public mirror (pflow.dev). This is what publishes.
    fn mirror(&self) -> Result<(), PublishError>;
}

/// `PublishResult`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishResult {
    pub cid: String,
    pub url: String,
    pub reloaded: bool,
    pub mirrored: bool,
}

/// `PublishStep`: where `publishSession` is, for a progress line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PublishStep {
    Staging {
        file: String,
        index: usize,
        of: usize,
    },
    Reloading,
    Mirroring,
    Done,
}

/// `publishSession`: stage, reload, and (only when asked) mirror. The mirror step is the
/// public one.
pub fn publish_session(
    bundle: &SessionBundle,
    via: &dyn CdnTransport,
    mirror: bool,
    progress: &mut dyn FnMut(PublishStep),
) -> Result<PublishResult, PublishError> {
    via.stage(bundle, progress)?;
    progress(PublishStep::Reloading);
    let reloaded = via.reload(&bundle.cid)?;
    if mirror {
        progress(PublishStep::Mirroring);
        via.mirror()?;
    }
    progress(PublishStep::Done);
    Ok(PublishResult {
        cid: bundle.cid.clone(),
        url: bundle.public_url(),
        reloaded,
        mirrored: mirror,
    })
}

/// `PublicProbe`: whether a published bundle is reachable at its public URL yet. The HEAD
/// request and the sleep are injected.
pub struct PublicProbe {
    pub attempts: u32,
}

impl PublicProbe {
    /// URLs that must answer 200 before the set counts as public (`session.json` is not
    /// polled).
    pub fn urls(bundle: &SessionBundle) -> Vec<String> {
        bundle
            .files
            .iter()
            .filter(|f| f.name != "session.json")
            .map(|f| format!("{}{}", bundle.public_url(), f.name))
            .collect()
    }

    /// True once every URL is 200, false after the last attempt. `on_attempt` gets the
    /// 1-based attempt number; `sleep` is not called after the last attempt.
    pub fn wait(
        &self,
        bundle: &SessionBundle,
        head: &mut dyn FnMut(&str) -> Option<u16>,
        sleep: &mut dyn FnMut(),
        on_attempt: &mut dyn FnMut(u32),
    ) -> bool {
        let urls = Self::urls(bundle);
        if urls.is_empty() {
            return false;
        }
        for n in 1..=core::cmp::max(self.attempts, 1) {
            on_attempt(n);
            let mut all = true;
            for u in &urls {
                if head(u) != Some(200) {
                    all = false;
                }
            }
            if all {
                return true;
            }
            if n < self.attempts {
                sleep();
            }
        }
        false
    }
}

// ---- what a set may not publish ------------------------------------------------------------------------------------

/// The bank files whose sounds a published set may not play (`SamplerSource.localOnlyFiles`):
/// the local banks, and Apple's GM bank when a palette or kit names it.
pub const LOCAL_ONLY_FILES: [&str; 2] = ["minecraft-local.sf2", "gs_instruments.dls"];

/// What `Session.localOnlySounds` needs from the sound palettes (`SoundPalette`): bank
/// categories (`voices/categories.json` `bankCategories`: category → bank file) and each
/// palette's role options (`voices/palettes.json`).
#[derive(Clone, Debug, Default)]
pub struct LocalOnly {
    bank_of: BTreeMap<String, String>,
    palettes: BTreeMap<String, Vec<String>>,
}

impl LocalOnly {
    /// From the bundled voice documents (`voices::CATEGORIES_JSON`, `sound::PALETTES_JSON`): what
    /// `SessionBundle.init` asks the shipped palettes.
    pub fn bundled() -> Self {
        let parse = |t: &str| JsonValue::parse(t).expect("the bundled voice documents are JSON");
        Self::from_json(
            &parse(crate::voices::CATEGORIES_JSON),
            &parse(crate::sound::PALETTES_JSON),
        )
    }

    /// From the two voice documents.
    pub fn from_json(categories: &JsonValue, palettes: &JsonValue) -> Self {
        let mut bank_of = BTreeMap::new();
        if let Some(o) = categories
            .get("bankCategories")
            .and_then(JsonValue::as_object)
        {
            for (k, v) in o.iter() {
                if let Some(bank) = v.get("bank").and_then(JsonValue::as_str) {
                    bank_of.insert(k.to_string(), bank.to_string());
                }
            }
        }
        let mut pals = BTreeMap::new();
        if let Some(o) = palettes.get("palettes").and_then(JsonValue::as_object) {
            for (k, v) in o.iter() {
                let Some(roles) = v.get("roles").and_then(JsonValue::as_object) else {
                    continue;
                };
                let mut opts = Vec::new();
                for (_, r) in roles.iter() {
                    if let Some(a) = r.as_array() {
                        opts.extend(a.iter().filter_map(JsonValue::as_str).map(String::from));
                    }
                }
                pals.insert(k.to_string(), opts);
            }
        }
        LocalOnly {
            bank_of,
            palettes: pals,
        }
    }

    /// `DrumKits.parse`: `file#bank:program` → its file.
    fn kit_file(id: &str) -> Option<&str> {
        let hash = id.rfind('#')?;
        let file = &id[..hash];
        let parts: Vec<&str> = id[hash + 1..]
            .split(':')
            .filter(|p| !p.is_empty())
            .collect();
        if file.is_empty() || file.contains('/') || parts.len() != 2 {
            return None;
        }
        let b = parts[0].parse::<i64>().ok()?;
        let p = parts[1].parse::<i64>().ok()?;
        if (0..=128).contains(&b) && (0..=127).contains(&p) {
            Some(file)
        } else {
            None
        }
    }

    /// `SoundPalette.isLocalOnly(option:)`: an option (or kit id) that plays local-only content.
    pub fn is_local_only_option(&self, option: &str) -> bool {
        if option.starts_with("dls:") {
            return true;
        }
        if let Some(cat) = option.strip_prefix("bank:") {
            if let Some(bank) = self.bank_of.get(cat) {
                return LOCAL_ONLY_FILES.contains(&bank.as_str());
            }
            return false;
        }
        if let Some(file) = Self::kit_file(option) {
            return LOCAL_ONLY_FILES.contains(&file);
        }
        false
    }

    /// `SoundPalette.named(id)?.isLocalOnly`: the palette a track names (its id, optionally
    /// with the asset version after `@`), and whether any of its options plays local-only
    /// content.
    pub fn palette_is_local_only(&self, id: &str) -> bool {
        if id.is_empty() {
            return false;
        }
        let bare = id.split('@').next().unwrap_or(id);
        self.palettes.get(bare).map_or(false, |opts| {
            opts.iter().any(|o| self.is_local_only_option(o))
        })
    }
}

impl Session {
    /// `Session.localOnlySounds`: what in the session plays local-only sounds: a palette,
    /// role option or kit from one, on a deck at the start, in a load, or as a poke. Empty
    /// for a set that may be published.
    pub fn local_only_sounds(&self, cat: &LocalOnly) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let check = |out: &mut Vec<String>, tracks: Option<&JsonValue>, at: &str| {
            if let Some(o) = tracks.and_then(JsonValue::as_object) {
                for (ch, t) in o.iter() {
                    if let Some(k) = t.get("kit").and_then(JsonValue::as_str) {
                        if cat.is_local_only_option(k) {
                            out.push(format!("{at} ch {ch} kit {k}"));
                        }
                    }
                    if let Some(p) = t.get("palette").and_then(JsonValue::as_str) {
                        if cat.palette_is_local_only(p) {
                            out.push(format!("{at} ch {ch} palette {p}"));
                        }
                    }
                    if let Some(v) = t.get("voice").and_then(JsonValue::as_str) {
                        if cat.is_local_only_option(v) {
                            out.push(format!("{at} ch {ch} voice {v}"));
                        }
                    }
                }
            }
        };
        let check_source = |out: &mut Vec<String>, d: &DeckSource, at: &str| match d {
            DeckSource::Net(spec) => check(out, spec.layers.get("tracks"), at),
            DeckSource::RawNets(p) => check(out, p.get("tracks"), at),
            DeckSource::None | DeckSource::Sample { .. } => {}
        };
        for (name, d) in &self.decks {
            check_source(&mut out, d, &format!("deck {name}"));
        }
        for e in &self.events {
            let SessionEventKind::Action(a) = &e.kind else {
                continue;
            };
            match a {
                DeckAction::Load { deck, source } => {
                    check_source(&mut out, source, &format!("load {deck} @{}", e.frame))
                }
                DeckAction::QueueNext { deck, spec } => check(
                    &mut out,
                    spec.layers.get("tracks"),
                    &format!("next {deck} @{}", e.frame),
                ),
                DeckAction::SetKit { deck, kit: Some(k) } if cat.is_local_only_option(k) => {
                    out.push(format!("setKit {deck} @{} {k}", e.frame))
                }
                DeckAction::SetPalette {
                    deck,
                    palette: Some(p),
                } if cat.palette_is_local_only(p) => {
                    out.push(format!("setPalette {deck} @{} {p}", e.frame))
                }
                DeckAction::SetRoleVoice {
                    deck,
                    role,
                    voice: Some(v),
                } if cat.is_local_only_option(v) => {
                    out.push(format!("setRoleVoice {deck} {role} @{} {v}", e.frame))
                }
                _ => {}
            }
        }
        // one line per place is plenty
        let mut seen: Vec<String> = Vec::new();
        out.into_iter()
            .filter(|s| {
                if seen.contains(s) {
                    false
                } else {
                    seen.push(s.clone());
                    true
                }
            })
            .collect()
    }

    /// `Session.importedClips`: the outside audio the set puts on the loop row, one line
    /// each: what and when. Every import counts, whatever its provenance says. Empty for a
    /// set that may be published.
    pub fn imported_clips(&self) -> Vec<String> {
        let mut out = Vec::new();
        for e in &self.events {
            if let SessionEventKind::Action(DeckAction::ClipImport {
                id,
                title,
                provenance,
                ..
            }) = &e.kind
            {
                let kind = provenance
                    .as_ref()
                    .and_then(|p| p.get("kind"))
                    .and_then(JsonValue::as_str)
                    .unwrap_or("outside");
                let name = if title.is_empty() {
                    id.chars().take(10).collect::<String>()
                } else {
                    title.clone()
                };
                out.push(format!("{kind} loop {name} @{}", e.frame));
            }
        }
        out
    }
}
