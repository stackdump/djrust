//! A beats share (link or envelope payload) → what a deck holds. Port of
//! `Session/ShareSource.swift`: used by the control socket's `loadShare` (`dj_load_share`);
//! the deck then loads it through `DJEngine.perform`, so it is recorded like any load.

use alloc::string::String;
use core::fmt;

use super::model::DeckSource;
use crate::cid;
use crate::gen::BeatsTables;
use crate::json::JsonValue;
use crate::share::{Share, ShareLink};
use crate::spec::TrackSpec;

/// `ShareLoadError`: why a share can't go on a deck.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShareLoadError {
    Unreadable,
    /// A short `?cid=` link: its payload lives on a server, and fetching is the caller's job.
    NeedsFetch {
        cid: String,
    },
    /// An envelope with neither a genre to compose nor nets to play.
    NoTrack,
    UnknownGenre(String),
}

impl fmt::Display for ShareLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ShareLoadError::Unreadable => f.write_str(
                "not a beats share: give a share URL (?cid=…&z=…, or a legacy ?p= / ?g= link) or \
                 an envelope payload as JSON",
            ),
            ShareLoadError::NeedsFetch { cid } => write!(
                f,
                "a short link: the payload of {cid} lives on a server; give the full link (with \
                 &z=) or the payload JSON"
            ),
            ShareLoadError::NoTrack => f.write_str("the share has neither a genre nor nets"),
            ShareLoadError::UnknownGenre(g) => write!(f, "unknown genre {g} (see dj_catalog)"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for ShareLoadError {}

impl DeckSource {
    /// `DeckSourceDescriptor.share(_ text:)`. `text` is a share link (`ShareLink.parse`: a
    /// full `?cid=&z=` URL or its query — the CID is verified — or a legacy `?p=` / `?g=`
    /// link) or an envelope payload as JSON. Composed shares become `Net`, raw-`nets` ones
    /// `RawNets`.
    pub fn share(text: &str) -> Result<DeckSource, ShareLoadError> {
        let t = text.trim();
        if t.starts_with('{') {
            let v = JsonValue::parse(t).map_err(|_| ShareLoadError::Unreadable)?;
            return Self::share_payload(&v);
        }
        match ShareLink::parse(t) {
            Some(ShareLink::Envelope(s)) => Self::share_envelope(&s),
            Some(ShareLink::Legacy(v)) => Self::share_envelope(&Share::from_legacy(&v)),
            Some(ShareLink::NeedsFetch { cid, .. }) => Err(ShareLoadError::NeedsFetch { cid }),
            None => Err(ShareLoadError::Unreadable),
        }
    }

    /// `share(payload:)`: an envelope payload (`shareFromPayload` reads it; its CID is
    /// `shareCID` of it).
    pub fn share_payload(payload: &JsonValue) -> Result<DeckSource, ShareLoadError> {
        if payload.as_object().is_none() {
            return Err(ShareLoadError::Unreadable);
        }
        Self::share_envelope(&Share::from_payload(
            payload,
            Some(&cid::share_cid(payload)),
        ))
    }

    fn share_envelope(s: &Share) -> Result<DeckSource, ShareLoadError> {
        if let Some(nets) = &s.nets {
            return match nets.as_object() {
                Some(o) if !o.is_empty() => Ok(DeckSource::RawNets(s.payload.clone())),
                _ => Err(ShareLoadError::NoTrack),
            };
        }
        let spec = TrackSpec::from_share(s).ok_or(ShareLoadError::NoTrack)?;
        if !BeatsTables::shared().genres.contains_key(&spec.genre) {
            return Err(ShareLoadError::UnknownGenre(spec.genre));
        }
        Ok(DeckSource::Net(spec))
    }
}
