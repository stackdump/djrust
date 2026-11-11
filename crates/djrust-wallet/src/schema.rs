//! The schema documents (`schema/` at the djrust repo root). A credential names its schema by
//! `$id` (`credentialSchema.id`); each document's CID (JCS bytes, sha2-256) is pinned by the
//! vectors, so a document never changes under its `$id`.

use crate::{cid, jcs};
use serde_json::Value;
use std::sync::OnceLock;

pub struct SchemaDoc {
    pub name: &'static str,
    pub text: &'static str,
    json: OnceLock<Value>,
    cid: OnceLock<String>,
}

impl SchemaDoc {
    const fn new(name: &'static str, text: &'static str) -> Self {
        Self { name, text, json: OnceLock::new(), cid: OnceLock::new() }
    }

    pub fn json(&self) -> &Value {
        self.json.get_or_init(|| jcs::parse(self.text).expect("schema documents are valid JSON"))
    }

    /// The document's `$id`: what `credentialSchema.id` says.
    pub fn id(&self) -> &str {
        self.json()["$id"].as_str().expect("every schema document has an $id")
    }

    /// Whether a credential may be issued under this schema. A beats snapshot (a track's
    /// generator inputs, CC0) and a DjSnapshot (a whole session) are unsigned data by definition;
    /// they are referred to by CID from a signed statement (a chart claim, a snapshot share),
    /// never signed as a credential's subject.
    pub fn signable(&self) -> bool {
        !UNSIGNED.contains(&self.name)
    }

    pub fn cid(&self) -> &str {
        self.cid.get_or_init(|| cid::of_json(self.json()).expect("schema documents canonicalize"))
    }
}

/// Documents that are never a credential's subject ([SchemaDoc::signable]).
const UNSIGNED: [&str; 2] = ["beats-snapshot.v1", "dj-snapshot.v1"];

macro_rules! schema {
    ($name:literal) => {
        SchemaDoc::new($name, include_str!(concat!("../../../schema/", $name, ".schema.json")))
    };
}

/// A beats track share (BeatsShare) with provenance moved to the credential.
pub static BEATS_TRACK_V1: SchemaDoc = schema!("beats-track.v1");
/// Canonical (genre, seed, structure) of a deterministic track; CC0; unsigned.
pub static BEATS_SNAPSHOT_V1: SchemaDoc = schema!("beats-snapshot.v1");
/// "I discovered this snapshot": attribution only, signed.
pub static BEATS_DISCOVERY_CLAIM_V1: SchemaDoc = schema!("beats-discovery-claim.v1");
/// Short allowlisted-ASCII post on a snapshot, signed.
pub static BEATS_POST_V1: SchemaDoc = schema!("beats-post.v1");
/// A whole DJ session at one moment (decks, mixer, vocoders). Unsigned data: it is signed by
/// reference, under [BITWRAP_SNAPSHOT_SHARE_V1].
pub static DJ_SNAPSHOT_V1: SchemaDoc = schema!("dj-snapshot.v1");
/// A bearer invite code: a member vouches for whoever redeems it.
pub static BITWRAP_INVITE_V1: SchemaDoc = schema!("bitwrap-invite.v1");
/// The redeemer's half of a membership: accepts one invite, by its credential CID.
pub static BITWRAP_INVITE_ACCEPT_V1: SchemaDoc = schema!("bitwrap-invite-accept.v1");
/// A member's pre-signed revocation of their own identity.
pub static BITWRAP_REVOCATION_V1: SchemaDoc = schema!("bitwrap-revocation.v1");
/// "Charted by": the first member to find a deterministic track is credited with it.
pub static BITWRAP_CHART_CLAIM_V1: SchemaDoc = schema!("bitwrap-chart-claim.v1");
/// A djrust recorded session (`session.json` of a SessionBundle), published by a member.
pub static BITWRAP_SESSION_V1: SchemaDoc = schema!("bitwrap-session.v1");
/// A signed reference to a whole-session snapshot (dj-snapshot.v1, unsigned) by its CID.
pub static BITWRAP_SNAPSHOT_SHARE_V1: SchemaDoc = schema!("bitwrap-snapshot-share.v1");

pub fn all() -> [&'static SchemaDoc; 11] {
    [
        &BEATS_TRACK_V1, &BEATS_SNAPSHOT_V1, &BEATS_DISCOVERY_CLAIM_V1, &BEATS_POST_V1, &DJ_SNAPSHOT_V1,
        &BITWRAP_INVITE_V1, &BITWRAP_INVITE_ACCEPT_V1, &BITWRAP_REVOCATION_V1, &BITWRAP_CHART_CLAIM_V1, &BITWRAP_SESSION_V1,
        &BITWRAP_SNAPSHOT_SHARE_V1,
    ]
}

pub fn by_name(name: &str) -> Option<&'static SchemaDoc> {
    all().into_iter().find(|s| s.name == name)
}

pub fn by_cid(c: &str) -> Option<&'static SchemaDoc> {
    all().into_iter().find(|s| s.cid() == c)
}

pub fn by_id(id: &str) -> Option<&'static SchemaDoc> {
    all().into_iter().find(|s| s.id() == id)
}
