//! Discovery priority by Merkle-root anchoring (`Anchor.kt`, `AnchoredClaims.kt`, and
//! `ClaimRegistry` in `DiscoveryClaims.kt`).
//!
//! Claims are queued in receipt order and sealed into batches: a tagged Merkle tree whose root
//! is published through an [Anchor]. A claim's priority is what the anchor itself says about its
//! batch (time, position) plus the index its inclusion proof pins down, never arrival order and
//! never the self-asserted `iat`. Anchor transports (EVM JSON-RPC, log publication) are not here:
//! [EvmChain] is a trait the platform implements, and nothing in this module touches the network.

use crate::encoding::{hex, keccak_parts};
use crate::credential::Credential;
use crate::key::{self, EthSignature};
use crate::merkle::{Proof, Scheme};
use crate::{claims, snapshot, Error, Failure, Reason, SKEW_SEC};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

fn get_str<'a>(o: &'a Value, k: &str) -> Result<&'a str, Error> {
    o.get(k).and_then(Value::as_str).ok_or_else(|| Error::invalid(format!("missing string {k}")))
}

fn get_u64(o: &Value, k: &str) -> Result<u64, Error> {
    o.get(k).and_then(Value::as_u64).ok_or_else(|| Error::invalid(format!("missing unsigned {k}")))
}

/// Where a batch root was published. Only a pointer: verifiers ask the resolver what it means.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AnchorRef {
    /// Which anchor, e.g. `evm:1` or `log:0xAbc…`. Selects the resolver.
    pub anchor_id: String,
    /// Anchor-specific locator: an EVM tx hash, a log sequence number.
    pub reference: String,
}

impl AnchorRef {
    pub fn new(anchor_id: impl Into<String>, reference: impl Into<String>) -> Self {
        Self { anchor_id: anchor_id.into(), reference: reference.into() }
    }

    pub fn to_json(&self) -> Value {
        json!({"anchor": self.anchor_id, "ref": self.reference})
    }

    pub fn from_json(o: &Value) -> Result<Self, Error> {
        Ok(Self::new(get_str(o, "anchor")?, get_str(o, "ref")?))
    }
}

/// What an anchor attests about a published root. `position` strictly increases within one
/// anchor; `time` is the anchor's own clock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnchorRecord {
    pub reference: AnchorRef,
    pub root: [u8; 32],
    pub time: u64,
    pub position: u64,
}

/// Verifier side: the anchor's own record for a reference, or None if unknown / not final / forged.
pub trait AnchorResolver {
    fn resolve(&self, r: &AnchorRef) -> Option<AnchorRecord>;
}

impl<F: Fn(&AnchorRef) -> Option<AnchorRecord>> AnchorResolver for F {
    fn resolve(&self, r: &AnchorRef) -> Option<AnchorRecord> {
        self(r)
    }
}

/// The anchor could not publish; callers keep their batch queued.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnchorUnavailable(pub String);

impl std::fmt::Display for AnchorUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "anchor unavailable: {}", self.0)
    }
}

impl std::error::Error for AnchorUnavailable {}

/// Publisher side. A reference may resolve only after confirmation.
pub trait Anchor: AnchorResolver {
    fn id(&self) -> String;
    fn publish(&self, root: &[u8; 32]) -> Result<AnchorRef, AnchorUnavailable>;
}

enum SetEntry<'a> {
    Anchor(&'a dyn Anchor),
    Resolver(&'a dyn AnchorResolver),
}

/// Routes a reference to the resolver registered for its anchor id; unknown ids resolve to None.
#[derive(Default)]
pub struct AnchorSet<'a> {
    by_id: HashMap<String, SetEntry<'a>>,
}

impl<'a> AnchorSet<'a> {
    pub fn of(anchors: &[&'a dyn Anchor]) -> Self {
        Self { by_id: anchors.iter().map(|a| (a.id(), SetEntry::Anchor(*a))).collect() }
    }

    pub fn with_resolver(mut self, id: impl Into<String>, r: &'a dyn AnchorResolver) -> Self {
        self.by_id.insert(id.into(), SetEntry::Resolver(r));
        self
    }
}

impl AnchorResolver for AnchorSet<'_> {
    fn resolve(&self, r: &AnchorRef) -> Option<AnchorRecord> {
        match self.by_id.get(&r.anchor_id)? {
            SetEntry::Anchor(a) => a.resolve(r),
            SetEntry::Resolver(x) => x.resolve(r),
        }
    }
}

/// Where a claim sits in the global order; smaller wins. Compares time, then anchor id (a
/// deterministic tiebreak only), then position, then index in batch. The field order *is* the
/// comparison order (derived `Ord`).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClaimPriority {
    pub time: u64,
    pub anchor_id: String,
    pub position: u64,
    pub index: usize,
}

/// The batch tree: a tagged Merkle tree over one batch's leaves, in receipt order, whose anchored
/// value commits to the leaf count (so the proof binds the index).
pub mod batch_tree {
    use super::*;

    pub const TAG: &str = "bitwrap.beats.anchor.batch.v1";
    pub const KIND_CLAIM: u8 = 1;
    pub const KIND_COMMIT: u8 = 2;

    pub fn scheme() -> Scheme {
        Scheme::new(TAG).expect("the batch tag is valid")
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct InclusionProof {
        pub count: usize,
        pub index: usize,
        pub path: Proof,
    }

    impl InclusionProof {
        pub fn to_json(&self) -> Value {
            json!({"count": self.count, "index": self.index, "path": self.path.to_json()})
        }

        pub fn from_json(o: &Value) -> Result<Self, Error> {
            Ok(Self {
                count: get_u64(o, "count")? as usize,
                index: get_u64(o, "index")? as usize,
                path: Proof::from_json(o.get("path").ok_or_else(|| Error::invalid("missing path"))?)?,
            })
        }
    }

    pub fn claim_leaf(credential_cid: &str) -> Vec<u8> {
        let mut l = vec![KIND_CLAIM];
        l.extend_from_slice(credential_cid.as_bytes());
        l
    }

    pub fn commit_leaf(commitment: &[u8; 32]) -> Vec<u8> {
        let mut l = vec![KIND_COMMIT];
        l.extend_from_slice(commitment);
        l
    }

    fn seal(count: usize, tree_root: &[u8; 32]) -> [u8; 32] {
        keccak_parts(&[TAG.as_bytes(), &[0x03], &(count as u32).to_be_bytes(), tree_root])
    }

    /// The value to anchor: keccak256(TAG | 0x03 | leafCount(be32) | treeRoot). Never empty.
    pub fn root(leaves: &[Vec<u8>]) -> Result<[u8; 32], Error> {
        if leaves.is_empty() {
            return Err(Error::invalid("empty batch"));
        }
        if leaves.len() > u32::MAX as usize {
            return Err(Error::invalid("batch too large"));
        }
        Ok(seal(leaves.len(), &scheme().root(leaves)))
    }

    pub fn prove(leaves: &[Vec<u8>], index: usize) -> Result<InclusionProof, Error> {
        Ok(InclusionProof { count: leaves.len(), index, path: scheme().prove(leaves, index)? })
    }

    pub fn verify(anchored_root: &[u8; 32], leaf: &[u8], p: &InclusionProof) -> bool {
        if p.count < 1 || p.index >= p.count || p.count > u32::MAX as usize {
            return false;
        }
        let s = scheme();
        if s.index_of(p.count, &p.path) != Some(p.index) {
            return false;
        }
        &seal(p.count, &s.root_from_proof(leaf, &p.path)) == anchored_root
    }
}

use batch_tree::InclusionProof;

// ---------------------------------------------------------------------------------------------
// Anchor 1: EVM chain (transport supplied by the platform; none here).
// ---------------------------------------------------------------------------------------------

/// The anchored root and inclusion data of a transaction, as read back from the chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvmRootReceipt {
    pub root: [u8; 32],
    pub block_number: u64,
    pub tx_index: u32,
    pub block_time: u64,
    pub confirmations: u32,
}

/// What [EvmAnchor] needs from a chain. A JSON-RPC implementation is platform code, compiled
/// only into `network` builds.
pub trait EvmChain: Send + Sync {
    fn chain_id(&self) -> u64;
    /// Sends a transaction carrying `root`; returns its hash.
    fn submit_root(&self, root: &[u8; 32]) -> Result<String, AnchorUnavailable>;
    /// The tx's root and inclusion data, or None if not (yet) mined / not found.
    fn lookup(&self, tx_hash: &str) -> Option<EvmRootReceipt>;
}

pub struct EvmAnchor {
    chain: Arc<dyn EvmChain>,
    min_confirmations: u32,
    id: String,
}

impl EvmAnchor {
    pub const DEFAULT_MIN_CONFIRMATIONS: u32 = 12;

    pub fn new(chain: Arc<dyn EvmChain>, min_confirmations: u32) -> Self {
        let id = format!("evm:{}", chain.chain_id());
        Self { chain, min_confirmations, id }
    }

    pub fn with_default_confirmations(chain: Arc<dyn EvmChain>) -> Self {
        Self::new(chain, Self::DEFAULT_MIN_CONFIRMATIONS)
    }
}

impl AnchorResolver for EvmAnchor {
    /// None until the tx has `min_confirmations`. Position packs block number and tx index.
    fn resolve(&self, r: &AnchorRef) -> Option<AnchorRecord> {
        if r.anchor_id != self.id {
            return None;
        }
        let rc = self.chain.lookup(&r.reference)?;
        if rc.confirmations < self.min_confirmations || rc.tx_index >= (1 << 24) || rc.block_number >= (1 << 40) {
            return None;
        }
        Some(AnchorRecord {
            reference: r.clone(),
            root: rc.root,
            time: rc.block_time,
            position: (rc.block_number << 24) | rc.tx_index as u64,
        })
    }
}

impl Anchor for EvmAnchor {
    fn id(&self) -> String {
        self.id.clone()
    }

    fn publish(&self, root: &[u8; 32]) -> Result<AnchorRef, AnchorUnavailable> {
        Ok(AnchorRef::new(self.id.clone(), self.chain.submit_root(root)?))
    }
}

/// Placeholder transport so an app can wire an [EvmAnchor] before a chain is chosen.
pub struct UnimplementedEvmChain {
    pub chain_id: u64,
}

impl Default for UnimplementedEvmChain {
    fn default() -> Self {
        Self { chain_id: 1 }
    }
}

impl EvmChain for UnimplementedEvmChain {
    fn chain_id(&self) -> u64 {
        self.chain_id
    }
    fn submit_root(&self, _root: &[u8; 32]) -> Result<String, AnchorUnavailable> {
        Err(AnchorUnavailable("TODO: EVM transport not implemented".into()))
    }
    fn lookup(&self, _tx_hash: &str) -> Option<EvmRootReceipt> {
        None
    }
}

// ---------------------------------------------------------------------------------------------
// Anchor 2: signed root log (in memory; publication / mirroring is platform code).
// ---------------------------------------------------------------------------------------------

pub mod root_log {
    use super::*;

    pub const TAG: &str = "bitwrap.beats.rootlog.v1";

    /// keccak256(TAG | seq(be64) | time(be64) | prev | root): what the operator signs.
    pub fn digest_of(seq: u64, time: u64, prev: &[u8; 32], root: &[u8; 32]) -> [u8; 32] {
        keccak_parts(&[TAG.as_bytes(), &seq.to_be_bytes(), &time.to_be_bytes(), prev, root])
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Entry {
        pub seq: u64,
        pub time: u64,
        pub prev: [u8; 32],
        pub root: [u8; 32],
        pub signature: EthSignature,
    }

    impl Entry {
        pub fn digest(&self) -> [u8; 32] {
            digest_of(self.seq, self.time, &self.prev, &self.root)
        }

        /// keccak256(digest | signature): the next entry's `prev`.
        pub fn hash(&self) -> [u8; 32] {
            keccak_parts(&[&self.digest(), &self.signature.0])
        }

        pub fn to_json(&self) -> Value {
            json!({"seq": self.seq, "time": self.time, "prev": hex::encode0x(&self.prev),
                   "root": hex::encode0x(&self.root), "signature": self.signature.hex()})
        }

        pub fn from_json(o: &Value) -> Result<Self, Error> {
            Ok(Self {
                seq: get_u64(o, "seq")?,
                time: get_u64(o, "time")?,
                prev: hex::decode_n::<32>(get_str(o, "prev")?)?,
                root: hex::decode_n::<32>(get_str(o, "root")?)?,
                signature: EthSignature::from_hex(get_str(o, "signature")?)?,
            })
        }
    }
}

pub type DigestSigner = Box<dyn Fn(&[u8; 32]) -> EthSignature + Send + Sync>;
pub type Clock = Box<dyn Fn() -> u64 + Send + Sync>;

/// Fallback anchor: an append-only log of batch roots, each entry signed by the operator and
/// chained to the previous entry's hash. [AnchorResolver::resolve] re-verifies the chain up to
/// the entry (O(n)), so a [SignedRootLog::mirror] of the published entries is a full verifier.
/// Weaker than a chain: the operator picks `time` and can withhold or fork; chaining makes a
/// fork detectable by anyone who saw two heads, and time never goes backwards.
pub struct SignedRootLog {
    pub log_address: String,
    signer: Option<DigestSigner>,
    clock: Clock,
    entries: Mutex<Vec<root_log::Entry>>,
}

impl SignedRootLog {
    /// Operator side: signs with `signer` (a custody digest signer); pins `log_address` (EIP-55).
    pub fn operator(log_address: impl Into<String>, clock: Clock, signer: DigestSigner) -> Self {
        Self { log_address: log_address.into(), signer: Some(signer), clock, entries: Mutex::new(Vec::new()) }
    }

    /// Verifier side: entries fetched from wherever the log is published.
    pub fn mirror(log_address: impl Into<String>, entries: Vec<root_log::Entry>) -> Self {
        Self { log_address: log_address.into(), signer: None, clock: Box::new(|| 0), entries: Mutex::new(entries) }
    }

    pub fn entries(&self) -> Vec<root_log::Entry> {
        self.entries.lock().unwrap().clone()
    }

    pub fn entries_json(&self) -> Vec<Value> {
        self.entries.lock().unwrap().iter().map(root_log::Entry::to_json).collect()
    }
}

impl AnchorResolver for SignedRootLog {
    fn resolve(&self, r: &AnchorRef) -> Option<AnchorRecord> {
        if r.anchor_id != self.id() {
            return None;
        }
        let seq: u64 = r.reference.parse().ok()?;
        let entries = self.entries.lock().unwrap();
        if seq >= entries.len() as u64 {
            return None;
        }
        let mut prev_hash = [0u8; 32];
        let mut prev_time = 0u64;
        for (i, e) in entries[..=seq as usize].iter().enumerate() {
            if e.seq != i as u64 || e.prev != prev_hash || e.time < prev_time {
                return None;
            }
            let signer = key::recover_address(&e.digest(), &e.signature)?;
            if !key::is_low_s(&e.signature) || key::checksum_address(&signer) != self.log_address {
                return None;
            }
            prev_hash = e.hash();
            prev_time = e.time;
        }
        let e = &entries[seq as usize];
        Some(AnchorRecord { reference: r.clone(), root: e.root, time: e.time, position: e.seq })
    }
}

impl Anchor for SignedRootLog {
    fn id(&self) -> String {
        format!("log:{}", self.log_address)
    }

    fn publish(&self, root: &[u8; 32]) -> Result<AnchorRef, AnchorUnavailable> {
        let sign = self.signer.as_ref().ok_or_else(|| AnchorUnavailable("read-only log mirror".into()))?;
        let mut entries = self.entries.lock().unwrap();
        let seq = entries.len() as u64;
        let prev = entries.last().map(root_log::Entry::hash).unwrap_or([0u8; 32]);
        let time = (self.clock)().max(entries.last().map_or(0, |e| e.time)); // never go backwards
        let digest = root_log::digest_of(seq, time, &prev, root);
        entries.push(root_log::Entry { seq, time, prev, root: *root, signature: sign(&digest) });
        Ok(AnchorRef::new(self.id(), seq.to_string()))
    }
}

// ---------------------------------------------------------------------------------------------
// Commit-reveal, anchored claims, verification and ranking.
// ---------------------------------------------------------------------------------------------

/// commitment = keccak256(TAG | signerDid | 0x00 | claimCredentialCid | 0x00 | salt).
/// Binds the signer and the claim, so a copied commitment is useless to anyone else.
pub mod commitment {
    use super::*;

    pub const TAG: &str = "bitwrap.beats.claim-commit.v1";

    pub fn of(signer_did: &str, claim_cid: &str, salt: &[u8]) -> Result<[u8; 32], Error> {
        if salt.len() < 16 {
            return Err(Error::invalid("salt too short"));
        }
        Ok(keccak_parts(&[TAG.as_bytes(), signer_did.as_bytes(), &[0], claim_cid.as_bytes(), &[0], salt]))
    }

    pub fn new_salt() -> Result<[u8; 32], Error> {
        let mut s = [0u8; 32];
        getrandom::getrandom(&mut s).map_err(|e| Error::invalid(format!("no randomness: {e}")))?;
        Ok(s)
    }
}

/// The reveal half: the salt, and where the commitment was anchored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitRef {
    pub salt: Vec<u8>,
    pub anchor: AnchorRef,
    pub inclusion: InclusionProof,
}

impl CommitRef {
    pub fn to_json(&self) -> Value {
        json!({"salt": hex::encode0x(&self.salt), "anchor": self.anchor.to_json(), "inclusion": self.inclusion.to_json()})
    }

    pub fn from_json(o: &Value) -> Result<Self, Error> {
        Ok(Self {
            salt: hex::decode(get_str(o, "salt")?)?,
            anchor: AnchorRef::from_json(o.get("anchor").ok_or_else(|| Error::invalid("missing anchor"))?)?,
            inclusion: InclusionProof::from_json(o.get("inclusion").ok_or_else(|| Error::invalid("missing inclusion"))?)?,
        })
    }
}

/// A discovery claim once its batch is anchored. The snapshot travels beside it.
#[derive(Clone, Debug, PartialEq)]
pub struct AnchoredClaim {
    pub credential: Credential,
    pub anchor: AnchorRef,
    pub inclusion: InclusionProof,
    pub commit: Option<CommitRef>,
}

impl AnchoredClaim {
    pub fn to_json(&self) -> Value {
        let mut o = json!({"@type": "AnchoredClaim", "v": 1, "credential": self.credential.to_json(),
                           "anchor": self.anchor.to_json(), "inclusion": self.inclusion.to_json()});
        if let Some(c) = &self.commit {
            o["commit"] = c.to_json();
        }
        o
    }

    pub fn from_json(v: &Value) -> Result<Self, Error> {
        if v.get("@type").and_then(Value::as_str) != Some("AnchoredClaim") {
            return Err(Error::invalid("not an AnchoredClaim"));
        }
        let req = |k: &str| v.get(k).ok_or_else(|| Error::invalid(format!("missing {k}")));
        Ok(Self {
            credential: Credential::from_json(req("credential")?)?,
            anchor: AnchorRef::from_json(req("anchor")?)?,
            inclusion: InclusionProof::from_json(req("inclusion")?)?,
            commit: v.get("commit").map(CommitRef::from_json).transpose()?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnchoredOk {
    pub priority: ClaimPriority,
    pub signer_did: String,
    pub snapshot_cid: String,
    /// The priority came from a valid, timely commitment rather than the claim's own batch.
    pub via_commit: bool,
}

pub type AnchoredResult = Result<AnchoredOk, Failure>;

pub const DEFAULT_REVEAL_WINDOW_SEC: u64 = 3600;

pub mod anchored {
    use super::*;

    /// Verifies an [AnchoredClaim] and computes its priority. `iat` only has to be plausible;
    /// order comes from what the resolver says about the batch and the index the proof binds.
    pub fn verify(
        claim: &AnchoredClaim,
        snapshot_doc: &Value,
        resolver: &dyn AnchorResolver,
        now: u64,
        skew: u64,
        reveal_window: u64,
    ) -> AnchoredResult {
        let env = &claim.credential;
        let v = claims::check(env, now, skew)?;
        if let Some(r) = snapshot::validate(snapshot_doc) {
            return Err(Failure::with(Reason::SnapshotInvalid, &r));
        }
        if snapshot::cid(snapshot_doc).ok().as_deref() != Some(claims::snapshot_cid(env).unwrap_or_default()) {
            return Err(Failure::with(Reason::SnapshotInvalid, "CID mismatch"));
        }

        let rec = resolver.resolve(&claim.anchor).ok_or(Failure::new(Reason::NotAnchored))?;
        if rec.reference.anchor_id != claim.anchor.anchor_id {
            return Err(Failure::with(Reason::NotAnchored, "anchor id mismatch"));
        }
        let env_cid = env.cid();
        if !batch_tree::verify(&rec.root, &batch_tree::claim_leaf(&env_cid), &claim.inclusion) {
            return Err(Failure::new(Reason::InclusionInvalid));
        }
        let own = ClaimPriority {
            time: rec.time,
            anchor_id: rec.reference.anchor_id.clone(),
            position: rec.position,
            index: claim.inclusion.index,
        };
        let ok = |priority, via_commit| AnchoredOk {
            priority,
            signer_did: v.signer_did.clone(),
            snapshot_cid: claims::snapshot_cid(env).unwrap_or_default().to_string(),
            via_commit,
        };
        let Some(c) = &claim.commit else { return Ok(ok(own, false)) };

        let commitment = commitment::of(&v.signer_did, &env_cid, &c.salt)
            .map_err(|_| Failure::with(Reason::CommitInvalid, "salt too short"))?;
        let crec = resolver.resolve(&c.anchor).ok_or(Failure::with(Reason::CommitInvalid, "commit not anchored"))?;
        if !batch_tree::verify(&crec.root, &batch_tree::commit_leaf(&commitment), &c.inclusion) {
            return Err(Failure::with(Reason::CommitInvalid, "commitment not in batch"));
        }
        let cprio = ClaimPriority {
            time: crec.time,
            anchor_id: crec.reference.anchor_id.clone(),
            position: crec.position,
            index: c.inclusion.index,
        };
        // anchored strictly before the claim's own batch, and revealed within the window
        let earlier = ClaimPriority { index: 0, ..cprio.clone() } < ClaimPriority { index: 0, ..own.clone() };
        let in_window = rec.time.saturating_sub(crec.time) <= reveal_window;
        Ok(if earlier && in_window { ok(cprio, true) } else { ok(own, false) })
    }
}

/// A claim as presented to the ranker; `anchored` is None while its batch is not anchored.
#[derive(Clone, Debug)]
pub struct Presented {
    pub credential: Credential,
    pub anchored: Option<AnchoredClaim>,
}

#[derive(Clone, Debug)]
pub struct Ranked {
    pub claim: AnchoredClaim,
    pub result: AnchoredOk,
}

pub mod ranking {
    use super::*;

    /// The verified, anchored claim with the smallest priority. Unanchored, unresolvable or
    /// mismatched claims never rank.
    pub fn winner(
        snapshot_doc: &Value,
        presented: &[Presented],
        resolver: &dyn AnchorResolver,
        now: u64,
        skew: u64,
        reveal_window: u64,
    ) -> Option<Ranked> {
        let snapshot_cid = snapshot::cid(snapshot_doc).ok()?;
        let mut best: Option<Ranked> = None;
        for p in presented {
            let Some(a) = &p.anchored else { continue };
            if a.credential.cid() != p.credential.cid() {
                continue; // proof for a different credential
            }
            let Ok(r) = anchored::verify(a, snapshot_doc, resolver, now, skew, reveal_window) else { continue };
            if r.snapshot_cid != snapshot_cid {
                continue;
            }
            if best.as_ref().map_or(true, |b| r.priority < b.result.priority) {
                best = Some(Ranked { claim: a.clone(), result: r });
            }
        }
        best
    }
}

// ---------------------------------------------------------------------------------------------
// Reference registry (single process, in memory).
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub struct RegistryConfig {
    pub skew_sec: u64,
    /// Plausibility only: refuses replays of old signatures.
    pub max_age_sec: u64,
    pub batch_interval_sec: u64,
    pub reveal_window_sec: u64,
}

impl Default for RegistryConfig {
    fn default() -> Self {
        Self { skew_sec: SKEW_SEC, max_age_sec: 300, batch_interval_sec: 600, reveal_window_sec: DEFAULT_REVEAL_WINDOW_SEC }
    }
}

#[derive(Clone, Debug)]
pub struct Reveal {
    pub salt: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub seq: u64,
    pub received_at: u64,
    pub discoverer_did: String,
    pub credential: Credential,
    pub credential_cid: String,
    pub snapshot_cid: String,
    pub reveal: Option<Reveal>,
}

#[derive(Clone, Debug)]
pub struct Batch {
    pub no: usize,
    pub leaves: Vec<Vec<u8>>,
    pub root: [u8; 32],
    pub reference: AnchorRef,
    pub sealed_at: u64,
}

#[derive(Clone, Debug)]
pub enum SubmitResult {
    /// Queued at receipt sequence `seq`; priority comes when its batch is anchored.
    Queued(Entry),
    Duplicate(Entry),
    Rejected(Failure),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommitResult {
    Queued(u64),
    Duplicate(u64),
}

#[derive(Clone, Copy)]
struct Where {
    batch: usize,
    index: usize,
}

/// Collects, batches and ranks discovery claims. A claim has no priority until its batch is
/// sealed and the anchor reports it final; [ClaimRegistry::winner] only looks at settled batches.
pub struct ClaimRegistry {
    anchor: Arc<dyn Anchor + Send + Sync>,
    clock: Clock,
    cfg: RegistryConfig,
    entries: Vec<Entry>,
    by_cid: HashMap<String, usize>,
    snapshots: HashMap<String, Value>,
    open: Vec<Vec<u8>>,
    batches: Vec<Batch>,
    placed: HashMap<Vec<u8>, Where>,
    queued_leaves: HashMap<Vec<u8>, u64>,
    seq: u64,
    last_seal_at: u64,
}

impl ClaimRegistry {
    pub fn new(anchor: Arc<dyn Anchor + Send + Sync>, clock: Clock) -> Self {
        Self::with_config(anchor, clock, RegistryConfig::default())
    }

    pub fn with_config(anchor: Arc<dyn Anchor + Send + Sync>, clock: Clock, cfg: RegistryConfig) -> Self {
        let last_seal_at = clock();
        Self {
            anchor,
            clock,
            cfg,
            entries: Vec::new(),
            by_cid: HashMap::new(),
            snapshots: HashMap::new(),
            open: Vec::new(),
            batches: Vec::new(),
            placed: HashMap::new(),
            queued_leaves: HashMap::new(),
            seq: 0,
            last_seal_at,
        }
    }

    pub fn submit(&mut self, env: &Credential, snapshot_doc: &Value, reveal: Option<Reveal>) -> SubmitResult {
        let now = (self.clock)();
        let v = match claims::check(env, now, self.cfg.skew_sec) {
            Ok(v) => v,
            Err(f) => return SubmitResult::Rejected(f),
        };
        let snapshot_cid = claims::snapshot_cid(env).unwrap_or_default().to_string();
        if let Some(r) = snapshot::validate(snapshot_doc) {
            return SubmitResult::Rejected(Failure::with(Reason::SnapshotInvalid, &r));
        }
        if snapshot::cid(snapshot_doc).ok().as_deref() != Some(snapshot_cid.as_str()) {
            return SubmitResult::Rejected(Failure::with(Reason::SnapshotInvalid, "CID mismatch"));
        }
        if env.valid_from() < now.saturating_sub(self.cfg.max_age_sec) {
            return SubmitResult::Rejected(Failure::new(Reason::Stale));
        }
        let cid = env.cid();
        if let Some(&i) = self.by_cid.get(&cid) {
            return SubmitResult::Duplicate(self.entries[i].clone());
        }
        if let Some(r) = &reveal {
            let sealed = commitment::of(&v.signer_did, &cid, &r.salt)
                .map(|c| self.placed.contains_key(&batch_tree::commit_leaf(&c)))
                .unwrap_or(false);
            if !sealed {
                return SubmitResult::Rejected(Failure::with(Reason::CommitInvalid, "no sealed commitment"));
            }
        }
        self.seq += 1;
        let e = Entry {
            seq: self.seq,
            received_at: now,
            discoverer_did: v.signer_did,
            credential: env.clone(),
            credential_cid: cid.clone(),
            snapshot_cid: snapshot_cid.clone(),
            reveal,
        };
        self.by_cid.insert(cid.clone(), self.entries.len());
        self.entries.push(e.clone());
        self.snapshots.insert(snapshot_cid, snapshot_doc.clone());
        let leaf = batch_tree::claim_leaf(&cid);
        self.queued_leaves.insert(leaf.clone(), e.seq);
        self.open.push(leaf);
        SubmitResult::Queued(e)
    }

    /// Queues a commitment. Opaque to the registry; spam control is TODO.
    pub fn submit_commit(&mut self, commitment: &[u8; 32]) -> CommitResult {
        let leaf = batch_tree::commit_leaf(commitment);
        if let Some(&s) = self.queued_leaves.get(&leaf) {
            return CommitResult::Duplicate(s);
        }
        if self.placed.contains_key(&leaf) {
            return CommitResult::Duplicate(0);
        }
        self.seq += 1;
        self.queued_leaves.insert(leaf.clone(), self.seq);
        self.open.push(leaf);
        CommitResult::Queued(self.seq)
    }

    /// Seals the open queue into one batch and publishes its root; Ok(None) if nothing is queued.
    /// If the anchor fails, nothing changes and the queue stays open.
    pub fn seal(&mut self) -> Result<Option<Batch>, AnchorUnavailable> {
        if self.open.is_empty() {
            return Ok(None);
        }
        let leaves = self.open.clone();
        let root = batch_tree::root(&leaves).expect("non-empty batch");
        let reference = self.anchor.publish(&root)?;
        let b = Batch { no: self.batches.len(), leaves, root, reference, sealed_at: (self.clock)() };
        for (i, l) in b.leaves.iter().enumerate() {
            self.placed.insert(l.clone(), Where { batch: b.no, index: i });
        }
        self.open.clear();
        self.queued_leaves.clear();
        self.last_seal_at = b.sealed_at;
        self.batches.push(b.clone());
        Ok(Some(b))
    }

    /// For a periodic caller: seals when the batch interval has passed since the last seal.
    pub fn seal_if_due(&mut self) -> Result<Option<Batch>, AnchorUnavailable> {
        if !self.open.is_empty() && (self.clock)().saturating_sub(self.last_seal_at) >= self.cfg.batch_interval_sec {
            self.seal()
        } else {
            Ok(None)
        }
    }

    /// The claim with its anchor reference and inclusion proof, once its batch is sealed.
    pub fn proof(&self, credential_cid: &str) -> Option<AnchoredClaim> {
        let e = &self.entries[*self.by_cid.get(credential_cid)?];
        let w = *self.placed.get(&batch_tree::claim_leaf(credential_cid))?;
        let b = &self.batches[w.batch];
        let commit = match &e.reveal {
            None => None,
            Some(r) => {
                let c = commitment::of(&e.discoverer_did, credential_cid, &r.salt).ok()?;
                let cw = *self.placed.get(&batch_tree::commit_leaf(&c))?;
                let cb = &self.batches[cw.batch];
                Some(CommitRef {
                    salt: r.salt.clone(),
                    anchor: cb.reference.clone(),
                    inclusion: batch_tree::prove(&cb.leaves, cw.index).ok()?,
                })
            }
        };
        Some(AnchoredClaim {
            credential: e.credential.clone(),
            anchor: b.reference.clone(),
            inclusion: batch_tree::prove(&b.leaves, w.index).ok()?,
            commit,
        })
    }

    /// Proof for a sealed commitment, for its owner to keep until reveal.
    pub fn commit_proof(&self, commitment: &[u8; 32]) -> Option<(AnchorRef, InclusionProof)> {
        let w = *self.placed.get(&batch_tree::commit_leaf(commitment))?;
        let b = &self.batches[w.batch];
        Some((b.reference.clone(), batch_tree::prove(&b.leaves, w.index).ok()?))
    }

    /// The discoverer of `snapshot_cid` among *settled* batches (this one and every earlier one
    /// resolve), so a reported winner never changes. TODO: re-publish a dropped anchor tx.
    pub fn winner(&self, snapshot_cid: &str) -> Option<Ranked> {
        let snapshot_doc = self.snapshots.get(snapshot_cid)?;
        let settled = self.batches.iter().take_while(|b| self.anchor.resolve(&b.reference).is_some()).count();
        let presented: Vec<Presented> = self
            .entries
            .iter()
            .filter(|e| e.snapshot_cid == snapshot_cid)
            .map(|e| {
                let w = self.placed.get(&batch_tree::claim_leaf(&e.credential_cid));
                Presented {
                    credential: e.credential.clone(),
                    anchored: w.filter(|w| w.batch < settled).and_then(|_| self.proof(&e.credential_cid)),
                }
            })
            .collect();
        ranking::winner(
            snapshot_doc,
            &presented,
            &|r: &AnchorRef| self.anchor.resolve(r),
            (self.clock)(),
            self.cfg.skew_sec,
            self.cfg.reveal_window_sec,
        )
    }

    pub fn batch(&self, no: usize) -> Option<&Batch> {
        self.batches.get(no)
    }

    pub fn pending_count(&self) -> usize {
        self.open.len()
    }
}
