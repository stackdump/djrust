//! A net deck's instrument choices per role: `RoleVoice` and `RoleVoice.list`
//! (`DeckVoices.swift`). The sound panel shows them for the deck playing a net
//! (`Deck.roleVoices`), and `ClipRetune.voices(for:)` for a loop taken from one.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::gen::tables::BeatsTables;
use crate::json::JsonValue;
use crate::pflow::Project;
use crate::session::model::DeckSource;
use crate::voices::VoiceCatalog;

/// One role of a net deck as an instrument choice: the beats instrument its nets play (which
/// picks the synth voice and, through its category, the sampled sound), and the instruments
/// beats' generator would pick from for that role: this genre's first (`RoleVoice`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleVoice {
    /// The role, or roles sharing the channel joined by " · " (`kick · snare · hihat`).
    pub role: String,
    /// The MIDI channel the role's nets share: the key of the share envelope's `tracks`
    /// override.
    pub channel: i64,
    pub instrument: String,
    pub options: Vec<String>,
}

impl RoleVoice {
    /// `RoleVoice.list`: `project`'s `roles` (in that order) with an instrument to choose, for
    /// a net of `genre`: what `Deck.roleVoices` shows for the deck playing it, and
    /// `ClipRetune.voices` for a loop. `catalog` is the bundled voice catalog
    /// ([`VoiceCatalog::bundled`]; `Deck.fallbackOptions` reads only its instrument →
    /// category map, which no installed sound changes).
    pub fn list(
        project: &Project,
        roles: &[String],
        genre: &str,
        catalog: &VoiceCatalog,
    ) -> Vec<RoleVoice> {
        let sets = &BeatsTables::shared().instrument_sets;
        let mut out: Vec<RoleVoice> = Vec::new();
        for role in roles {
            // A role is a riff group (`bass-0`, `bass-1`… share one channel) or a lone net's
            // id; the first net in the project's own order that fits.
            let Some(net) = project
                .nets
                .iter()
                .find(|(id, nb)| nb.role != "control" && (nb.riff_group == *role || *id == role))
                .map(|(_, nb)| nb)
            else {
                continue;
            };
            let mut options: Vec<String> = BeatsTables::shared()
                .instrument_set(genre)
                .and_then(|g| g.get(role))
                .cloned()
                .unwrap_or_default();
            // `sets.keys.sorted()`: the other genres' lists for the role, in genre order
            for (id, set) in sets {
                if id == genre {
                    continue;
                }
                for i in set.get(role).into_iter().flatten() {
                    if !options.contains(i) {
                        options.push(i.clone());
                    }
                }
            }
            // beats lists no instruments for some roles (harmony): offer what fits it instead
            // of leaving the role out of the Instruments panel
            if options.is_empty() {
                options = catalog.fallback_options(role);
            }
            if options.is_empty() {
                continue;
            }
            // Roles on one channel (the drum kit) are one choice: `tracks` is keyed by channel.
            if let Some(i) = out.iter().position(|v| v.channel == net.track.channel) {
                out[i].role.push_str(" · ");
                out[i].role.push_str(role);
                continue;
            }
            if !options.contains(&net.track.instrument) {
                options.insert(0, net.track.instrument.clone());
            }
            out.push(RoleVoice {
                role: role.clone(),
                channel: net.track.channel,
                instrument: net.track.instrument.clone(),
                options,
            });
        }
        out
    }
}

impl RoleVoice {
    /// `ClipRetune.voices(for:)` once the clip's source is a project: the instruments a clip
    /// can be retuned on, without a deck: its track's roles with an instrument to choose
    /// ([`list`](RoleVoice::list)), each on the instrument the clip plays now. The roles that
    /// were off in the take (`roles_off`: `LoopClip::origin_roles_off`) are left out (they are
    /// silent in it), and so are roles no net of `project` plays. The host finds the project
    /// (`spec.project()`, or a raw share's) and `genre` ("" for a raw share without one) from
    /// `LoopClip::origin_source`, and returns no voices at all for a clip `ClipRetune.render`
    /// refuses.
    pub fn list_for_retune(
        project: &Project,
        genre: &str,
        roles_off: &[String],
        catalog: &VoiceCatalog,
    ) -> Vec<RoleVoice> {
        let roles: Vec<String> = crate::deck::perform::ROLE_ORDER
            .iter()
            .filter(|&&role| {
                !roles_off.iter().any(|o| o == role)
                    && project.nets.iter().any(|(id, nb)| {
                        nb.role != "control" && (nb.riff_group == role || id == role)
                    })
            })
            .map(|r| r.to_string())
            .collect();
        RoleVoice::list(project, &roles, genre, catalog)
    }
}

/// `Deck.keepsVoiceSettings(loading:)`: does loading `loading` keep the voice settings of the
/// deck that plays `now`? Only when it is the song playing with another instrument picked (the
/// same share but for its `tracks` override): the settings belong to the song, and to the
/// instrument they were made on (a channel whose instrument changed starts afresh,
/// `WaveDeckSource.load`). A new song, a favorite or the same song loaded again start from the
/// instruments' own voices. `now` is `Deck.netSource`: None off a net.
pub fn keeps_voice_settings(now: Option<&DeckSource>, loading: &DeckSource) -> bool {
    /// The song without its `tracks` override: its payload, None for a source that is no song.
    fn song(d: &DeckSource) -> Option<JsonValue> {
        match d {
            DeckSource::Net(spec) => {
                let mut spec = spec.clone();
                spec.layers.remove("tracks");
                Some(spec.payload())
            }
            DeckSource::RawNets(payload) => {
                let mut o = payload.as_object()?.clone();
                o.remove("tracks");
                Some(JsonValue::Object(o))
            }
            DeckSource::None | DeckSource::Sample { .. } => None,
        }
    }
    let Some(now) = now else { return false };
    if now == loading {
        return false;
    }
    match (song(now), song(loading)) {
        (Some(a), Some(b)) => a.canonical_js() == b.canonical_js(),
        _ => false,
    }
}
