//! Port of the Kotlin `AnchoringTest`: batch-tree inclusion proofs, anchors (signed root log,
//! EVM with a fake chain), ordering, unanchored claims, verification and commit-reveal.
//! Deterministic: every key and salt is keccak of a label.

use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use djrust_wallet::anchor::batch_tree::{self, InclusionProof};
use djrust_wallet::anchor::*;
use djrust_wallet::encoding::{hex, keccak, keccak_parts};
use djrust_wallet::credential::{self, Credential};
use djrust_wallet::key::{self, did};
use djrust_wallet::merkle::{Proof, Scheme, Step};
use djrust_wallet::{claims, jcs, schema, snapshot, Reason, SKEW_SEC};

const T0: u64 = 1_800_000_000;
const WINDOW: u64 = DEFAULT_REVEAL_WINDOW_SEC;

fn snap() -> Value {
    snapshot::of("techno", 12345, "classic").unwrap()
}
fn snap_cid() -> String {
    snapshot::cid(&snap()).unwrap()
}

struct Person {
    key: [u8; 32],
    did: String,
}

fn person(label: &str) -> Person {
    let key = keccak(label.as_bytes());
    Person { key, did: did::pkh(&key::address(&key).unwrap()) }
}

fn claim_by_on(p: &Person, iat: u64, s: &Value) -> (Credential, Value) {
    let scid = snapshot::cid(s).unwrap();
    let data = claims::data(&scid, &p.did);
    let salt = keccak(format!("doc-salt {} {iat}", p.did).as_bytes());
    let sealed = credential::seal(schema::BEATS_DISCOVERY_CLAIM_V1.id(), &data, &p.did, iat, 0, &salt).unwrap();
    (credential::sign(&sealed, &p.key).unwrap(), data)
}

fn claim_by(p: &Person, iat: u64) -> (Credential, Value) {
    claim_by_on(p, iat, &snap())
}

#[derive(Clone)]
struct Clk(Arc<AtomicU64>);
impl Clk {
    fn new() -> Self {
        Self(Arc::new(AtomicU64::new(T0)))
    }
    fn now(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
    fn add(&self, d: u64) {
        self.0.fetch_add(d, Ordering::SeqCst);
    }
    fn boxed(&self) -> Clock {
        let c = self.0.clone();
        Box::new(move || c.load(Ordering::SeqCst))
    }
}

fn test_log(clk: &Clk, label: &str) -> SignedRootLog {
    let priv_ = keccak(label.as_bytes());
    SignedRootLog::operator(key::address(&priv_).unwrap(), clk.boxed(), Box::new(move |d| key::sign(d, &priv_).unwrap()))
}

struct Rig {
    clk: Clk,
    log: Arc<SignedRootLog>,
    reg: ClaimRegistry,
}

impl Rig {
    fn new() -> Self {
        Self::with_window(WINDOW)
    }
    fn with_window(window: u64) -> Self {
        let clk = Clk::new();
        let log = Arc::new(test_log(&clk, "log-operator"));
        let cfg = RegistryConfig { reveal_window_sec: window, ..Default::default() };
        let reg = ClaimRegistry::with_config(log.clone(), clk.boxed(), cfg);
        Self { clk, log, reg }
    }
    fn now(&self) -> u64 {
        self.clk.now()
    }
    fn queue(&mut self, p: &Person, iat: u64) -> (Credential, Value) {
        let (e, d) = claim_by(p, iat);
        assert!(matches!(self.reg.submit(&e, &snap(), None), SubmitResult::Queued(_)));
        (e, d)
    }
    fn queue_now(&mut self, p: &Person) -> (Credential, Value) {
        let n = self.now();
        self.queue(p, n)
    }
    fn seal(&mut self) {
        self.reg.seal().unwrap();
    }
    fn winner_did(&self) -> Option<String> {
        self.reg.winner(&snap_cid()).map(|w| w.result.signer_did)
    }
    fn verify(&self, c: &AnchoredClaim) -> AnchoredResult {
        anchored::verify(c, &snap(), self.log.as_ref(), self.now(), SKEW_SEC, WINDOW)
    }
}

fn reason(r: AnchoredResult) -> Reason {
    r.expect_err("expected a failure").reason
}

// ---------- batch tree: inclusion proofs ----------

fn leaves(n: usize) -> Vec<Vec<u8>> {
    (0..n).map(|i| batch_tree::claim_leaf(&format!("cid-{i}"))).collect()
}

#[test]
fn inclusion_proof_every_leaf_every_size_and_index_is_bound() {
    for n in 1..=40 {
        let ls = leaves(n);
        let root = batch_tree::root(&ls).unwrap();
        for i in 0..n {
            let p = batch_tree::prove(&ls, i).unwrap();
            assert!(batch_tree::verify(&root, &ls[i], &p), "n={n} i={i}");
            assert_eq!(Some(i), batch_tree::scheme().index_of(n, &p.path));
            assert!(!batch_tree::verify(&root, &batch_tree::claim_leaf("other"), &p));
            if n > 1 {
                let j = (i + 1) % n;
                assert!(!batch_tree::verify(&root, &ls[i], &InclusionProof { count: n, index: j, path: p.path.clone() }));
            }
        }
    }
}

#[test]
fn inclusion_proof_rejects_wrong_count_wrong_root_and_tampering() {
    let ls = leaves(9);
    let root = batch_tree::root(&ls).unwrap();
    let p = batch_tree::prove(&ls, 4).unwrap();
    let ip = |count, index, path: Proof| InclusionProof { count, index, path };
    assert!(!batch_tree::verify(&root, &ls[4], &ip(10, 4, p.path.clone())));
    assert!(!batch_tree::verify(&root, &ls[4], &ip(8, 4, p.path.clone())));
    assert!(!batch_tree::verify(&batch_tree::root(&leaves(8)).unwrap(), &ls[4], &p));
    let mut flipped = p.path.steps.clone();
    flipped[0].sibling[0] ^= 1;
    assert!(!batch_tree::verify(&root, &ls[4], &ip(9, 4, Proof { steps: flipped })));
    let swapped = p.path.steps.iter().map(|s| Step { sibling: s.sibling, sibling_on_left: !s.sibling_on_left }).collect();
    assert!(!batch_tree::verify(&root, &ls[4], &ip(9, 4, Proof { steps: swapped })));
    assert!(!batch_tree::verify(&root, &ls[4], &ip(9, 4, Proof { steps: p.path.steps[1..].to_vec() })));
    assert!(!batch_tree::verify(&root, &ls[4], &ip(0, 0, p.path.clone())));
}

#[test]
fn anchoring_and_disclosure_trees_are_domain_separated() {
    let ls = leaves(5);
    let plain = Scheme::plain();
    assert_ne!(plain.root(&ls), batch_tree::scheme().root(&ls));
    let plain_root = plain.root(&ls);
    let plain_proof = plain.prove(&ls, 2).unwrap();
    assert!(plain.verify(&plain_root, &ls[2], &plain_proof));
    assert!(!batch_tree::verify(&plain_root, &ls[2], &InclusionProof { count: 5, index: 2, path: plain_proof }));
    let batch_proof = batch_tree::prove(&ls, 2).unwrap();
    assert!(!plain.verify(&batch_tree::root(&ls).unwrap(), &ls[2], &batch_proof.path));
    assert!(!plain.verify(&batch_tree::scheme().root(&ls), &ls[2], &batch_proof.path));
    assert_eq!(Scheme::new("").unwrap().root(&ls), plain.root(&ls));
    let sealed = credential::seal(schema::BEATS_TRACK_V1.id(), &jcs::parse(r#"{"a":1,"b":2}"#).unwrap(), &person("x").did, 1, 0, &[7u8; 32]).unwrap();
    assert!(!batch_tree::verify(&sealed.unsigned.disclosure_root, &ls[0], &batch_proof));
}

#[test]
fn batch_layout_matches_the_documented_bytes() {
    // keccak256(TAG | 0x03 | count be32 | treeRoot), leaves typed by a kind byte
    let ls = leaves(3);
    let expect = keccak_parts(&[batch_tree::TAG.as_bytes(), &[3], &3u32.to_be_bytes(), &batch_tree::scheme().root(&ls)]);
    assert_eq!(expect, batch_tree::root(&ls).unwrap());
    assert_eq!(batch_tree::claim_leaf("z1")[..], [1u8, b'z', b'1']);
    assert_eq!(batch_tree::commit_leaf(&[9; 32])[0], 2);
    assert!(batch_tree::root(&[]).is_err());
}

// ---------- anchors ----------

#[test]
fn signed_root_log_resolves_published_roots_and_rejects_tampering() {
    let clk = Clk::new();
    let priv_ = keccak(b"log-1");
    let addr = key::address(&priv_).unwrap();
    let log = test_log(&clk, "log-1");
    let (r1, r2) = ([1u8; 32], [2u8; 32]);
    let ref1 = log.publish(&r1).unwrap();
    clk.add(10);
    let ref2 = log.publish(&r2).unwrap();
    let rec2 = log.resolve(&ref2).unwrap();
    assert_eq!(r2, rec2.root);
    assert_eq!(clk.now(), rec2.time);
    assert_eq!(1, rec2.position);
    assert!(log.resolve(&ref1).unwrap().position < rec2.position);
    assert!(log.resolve(&AnchorRef::new(log.id(), "7")).is_none());
    assert!(log.resolve(&AnchorRef::new("log:0xother", "0")).is_none());

    let published: Vec<_> = log.entries_json().iter().map(|j| root_log::Entry::from_json(j).unwrap()).collect();
    assert_eq!(r2, SignedRootLog::mirror(&addr, published.clone()).resolve(&ref2).unwrap().root);
    let (e0, e1) = (published[0].clone(), published[1].clone());
    let edited_root = root_log::Entry { root: [9; 32], ..e1.clone() };
    assert!(SignedRootLog::mirror(&addr, vec![e0.clone(), edited_root]).resolve(&ref2).is_none());
    let edited_time = root_log::Entry { time: e1.time - 1000, ..e1.clone() };
    assert!(SignedRootLog::mirror(&addr, vec![e0.clone(), edited_time]).resolve(&ref2).is_none());
    assert!(SignedRootLog::mirror(&addr, vec![e1.clone()]).resolve(&AnchorRef::new(log.id(), "1")).is_none());
    let mallory = keccak(b"mallory-log");
    let now = clk.now();
    let forged = root_log::Entry {
        seq: 0,
        time: now,
        prev: [0; 32],
        root: r1,
        signature: key::sign(&root_log::digest_of(0, now, &[0; 32], &r1), &mallory).unwrap(),
    };
    assert!(SignedRootLog::mirror(&addr, vec![forged]).resolve(&AnchorRef::new(log.id(), "0")).is_none());
    // a mirror cannot publish
    assert!(SignedRootLog::mirror(&addr, vec![]).publish(&r1).is_err());
}

#[derive(Default)]
struct FakeState {
    txs: HashMap<String, EvmRootReceipt>,
    confirmations: u32,
    unmined: HashSet<String>,
    block: u64,
    time: u64,
}

#[derive(Default)]
struct FakeEvm(Mutex<FakeState>);

impl FakeEvm {
    fn new() -> Arc<Self> {
        Arc::new(Self(Mutex::new(FakeState { block: 100, ..Default::default() })))
    }
    fn st(&self) -> std::sync::MutexGuard<'_, FakeState> {
        self.0.lock().unwrap()
    }
}

impl EvmChain for FakeEvm {
    fn chain_id(&self) -> u64 {
        1
    }
    fn submit_root(&self, root: &[u8; 32]) -> Result<String, AnchorUnavailable> {
        let mut s = self.st();
        let h = hex::encode0x(&keccak_parts(&[root, &[s.txs.len() as u8]]));
        let rc = EvmRootReceipt { root: *root, block_number: s.block, tx_index: 0, block_time: s.time, confirmations: 0 };
        s.txs.insert(h.clone(), rc);
        Ok(h)
    }
    fn lookup(&self, tx: &str) -> Option<EvmRootReceipt> {
        let s = self.st();
        if s.unmined.contains(tx) {
            return None;
        }
        s.txs.get(tx).map(|r| EvmRootReceipt { confirmations: s.confirmations, ..r.clone() })
    }
}

#[test]
fn evm_anchor_resolves_only_after_confirmations_and_uses_chain_time() {
    let clk = Clk::new();
    let chain = FakeEvm::new();
    chain.st().time = 1_800_000_500;
    let anchor = Arc::new(EvmAnchor::new(chain.clone(), 3));
    let mut reg = ClaimRegistry::new(anchor.clone(), clk.boxed());
    let (e, _d) = claim_by(&person("p1"), clk.now());
    reg.submit(&e, &snap(), None);
    reg.seal().unwrap();
    assert!(reg.winner(&snap_cid()).is_none());
    assert!(reg.proof(&e.cid()).is_some());
    chain.st().confirmations = 3;
    let w = reg.winner(&snap_cid()).unwrap();
    assert_eq!(1_800_000_500, w.result.priority.time);
    assert_eq!(100u64 << 24, w.result.priority.position);
    assert!(EvmAnchor::with_default_confirmations(chain.clone()).resolve(&w.claim.anchor).is_none());
    assert!(anchor.resolve(&AnchorRef::new("evm:5", w.claim.anchor.reference.clone())).is_none());
}

#[test]
fn an_unconfirmed_earlier_batch_blocks_a_later_batch_from_deciding() {
    let clk = Clk::new();
    let chain = FakeEvm::new();
    let mut reg = ClaimRegistry::new(Arc::new(EvmAnchor::new(chain.clone(), 1)), clk.boxed());
    let (alice, bob) = (person("alice"), person("bob"));
    let (e, _d) = claim_by(&alice, clk.now());
    reg.submit(&e, &snap(), None);
    chain.st().block = 100;
    reg.seal().unwrap();
    let (e, _d) = claim_by(&bob, clk.now());
    reg.submit(&e, &snap(), None);
    chain.st().block = 101;
    reg.seal().unwrap();
    let r0 = reg.batch(0).unwrap().reference.reference.clone();
    {
        let mut s = chain.st();
        s.unmined.insert(r0);
        s.confirmations = 1;
    }
    assert!(reg.winner(&snap_cid()).is_none());
    chain.st().unmined.clear();
    assert_eq!(alice.did, reg.winner(&snap_cid()).unwrap().result.signer_did);
}

#[test]
fn unimplemented_evm_transport_fails_closed_and_leaves_the_queue_intact() {
    let clk = Clk::new();
    let mut reg = ClaimRegistry::new(
        Arc::new(EvmAnchor::with_default_confirmations(Arc::new(UnimplementedEvmChain::default()))),
        clk.boxed(),
    );
    let (e, _d) = claim_by(&person("p1"), clk.now());
    reg.submit(&e, &snap(), None);
    assert!(reg.seal().is_err());
    assert_eq!(1, reg.pending_count());
}

// ---------- ordering ----------

#[test]
fn within_a_batch_receipt_order_decides_not_iat() {
    let mut r = Rig::new();
    let (alice, bob) = (person("alice"), person("bob"));
    let (b_env, _b_data) = claim_by(&bob, r.now() - 120);
    r.queue_now(&alice);
    assert!(matches!(r.reg.submit(&b_env, &snap(), None), SubmitResult::Queued(_)));
    r.seal();
    let w = r.reg.winner(&snap_cid()).unwrap();
    assert_eq!(alice.did, w.result.signer_did);
    assert_eq!(0, w.result.priority.index);
    assert_eq!(1, r.reg.proof(&b_env.cid()).unwrap().inclusion.index);
}

#[test]
fn across_batches_earlier_anchor_wins_whatever_the_iat_or_index() {
    let mut r = Rig::new();
    let (alice, bob) = (person("alice"), person("bob"));
    r.queue_now(&bob);
    r.clk.add(60);
    r.seal();
    r.clk.add(60);
    let (a_env, _) = r.queue(&alice, r.now() - 100);
    r.seal();
    assert_eq!(Some(bob.did.clone()), r.winner_did());
    let a_proof = r.reg.proof(&a_env.cid()).unwrap();
    assert_eq!(0, a_proof.inclusion.index);
    assert!(r.log.resolve(&a_proof.anchor).unwrap().time > r.reg.winner(&snap_cid()).unwrap().result.priority.time);
}

#[test]
fn backdated_iat_buys_nothing() {
    let mut r = Rig::new();
    let (alice, mallory) = (person("alice"), person("mallory"));
    r.queue_now(&alice);
    r.clk.add(30);
    r.seal();
    let (e, _d) = claim_by(&mallory, r.now() - 299);
    assert!(matches!(r.reg.submit(&e, &snap(), None), SubmitResult::Queued(_)));
    r.clk.add(30);
    r.seal();
    assert_eq!(Some(alice.did), r.winner_did());
}

#[test]
fn stale_claims_are_refused() {
    let mut r = Rig::new();
    let (e, _d) = claim_by(&person("late"), r.now() - 301);
    match r.reg.submit(&e, &snap(), None) {
        SubmitResult::Rejected(f) => assert_eq!(Reason::Stale, f.reason),
        other => panic!("{other:?}"),
    }
    // resubmitting a queued envelope is an idempotent no-op
    let (e, _d) = r.queue_now(&person("p"));
    assert!(matches!(r.reg.submit(&e, &snap(), None), SubmitResult::Duplicate(_)));
}

#[test]
fn same_second_batches_order_by_anchor_position() {
    let mut r = Rig::new();
    let (alice, bob) = (person("alice"), person("bob"));
    r.queue_now(&alice);
    r.seal();
    r.queue_now(&bob);
    r.seal();
    let pa = r.log.resolve(&r.reg.batch(0).unwrap().reference).unwrap();
    let pb = r.log.resolve(&r.reg.batch(1).unwrap().reference).unwrap();
    assert_eq!(pa.time, pb.time);
    assert!(pa.position < pb.position);
    assert_eq!(Some(alice.did), r.winner_did());
}

#[test]
fn priority_compares_time_then_position_then_index() {
    let p = |t, pos, i| ClaimPriority { time: t, anchor_id: "a".into(), position: pos, index: i };
    assert!(p(1, 9, 9) < p(2, 0, 0));
    assert!(p(2, 1, 9) < p(2, 2, 0));
    assert!(p(2, 2, 0) < p(2, 2, 1));
    let q = |id: &str, pos| ClaimPriority { time: 2, anchor_id: id.into(), position: pos, index: 0 };
    assert!(q("a", 5) < q("b", 0));
}

// ---------- unanchored claims lose ----------

#[test]
fn pending_claim_loses_to_anchored_one_and_still_loses_once_sealed_later() {
    let mut r = Rig::new();
    let (bob, alice) = (person("bob"), person("alice"));
    r.queue_now(&bob);
    r.seal();
    let (a_env, _) = r.queue(&alice, r.now() - 250);
    assert_eq!(Some(bob.did.clone()), r.winner_did());
    assert!(r.reg.proof(&a_env.cid()).is_none());
    r.clk.add(60);
    r.seal();
    assert_eq!(Some(bob.did), r.winner_did());
}

#[test]
fn ranking_ignores_claims_without_anchor_or_with_a_forged_one() {
    let mut r = Rig::new();
    let (bob, alice, eve) = (person("bob"), person("alice"), person("eve"));
    let (b_env, _b_data) = r.queue_now(&bob);
    r.seal();
    let b_anchored = r.reg.proof(&b_env.cid()).unwrap();
    let (a_env, _a_data) = claim_by(&alice, r.now() - 200);
    let (e_env, _e_data) = claim_by(&eve, r.now());
    let forged = AnchoredClaim { credential: e_env.clone(), commit: None, ..b_anchored.clone() };
    let presented = vec![
        Presented { credential: a_env.clone(), anchored: None },
        Presented { credential: e_env, anchored: Some(forged) },
        Presented { credential: b_env, anchored: Some(b_anchored.clone()) },
    ];
    let w = |ps: &[Presented]| ranking::winner(&snap(), ps, r.log.as_ref(), r.now(), SKEW_SEC, WINDOW);
    assert_eq!(bob.did, w(&presented).unwrap().result.signer_did);
    assert!(w(&presented[..2]).is_none());
    assert!(w(&[Presented { credential: a_env, anchored: Some(b_anchored) }]).is_none());
}

// ---------- verification ----------

fn anchored_claim() -> (Rig, AnchoredClaim, Value) {
    let mut r = Rig::new();
    let (e, d) = r.queue_now(&person("v1"));
    r.queue_now(&person("v2"));
    r.queue_now(&person("v3"));
    r.seal();
    let c = r.reg.proof(&e.cid()).unwrap();
    (r, c, d)
}

#[test]
fn verify_accepts_an_anchored_claim_and_survives_a_json_round_trip() {
    let (r, c, _d) = anchored_claim();
    let ok = r.verify(&c).unwrap();
    assert_eq!(0, ok.priority.index);
    assert!(!ok.via_commit);
    let back = AnchoredClaim::from_json(&jcs::parse(&jcs::canonicalize(&c.to_json()).unwrap()).unwrap()).unwrap();
    assert!(r.verify(&back).is_ok());
    assert_eq!(c.credential.cid(), back.credential.cid());
    assert_eq!(c, back);
}

#[test]
fn verify_rejects_wrong_root() {
    let (r, c, _d) = anchored_claim();
    let log = r.log.clone();
    let liar = move |x: &AnchorRef| log.resolve(x).map(|rec| AnchorRecord { root: [7; 32], ..rec });
    assert_eq!(Reason::InclusionInvalid, reason(anchored::verify(&c, &snap(), &liar, r.now(), SKEW_SEC, WINDOW)));
}

#[test]
fn verify_rejects_tampered_proof_forged_index_and_swapped_envelope() {
    let (r, c, _d) = anchored_claim();
    let mut bad = c.inclusion.path.steps.clone();
    bad[0].sibling = [3; 32];
    let tampered = AnchoredClaim { inclusion: InclusionProof { path: Proof { steps: bad }, ..c.inclusion.clone() }, ..c.clone() };
    assert_eq!(Reason::InclusionInvalid, reason(r.verify(&tampered)));
    let forged_idx = AnchoredClaim { inclusion: InclusionProof { index: c.inclusion.index + 1, ..c.inclusion.clone() }, ..c.clone() };
    assert_eq!(Reason::InclusionInvalid, reason(r.verify(&forged_idx)));
    let (other, _other_data) = claim_by(&person("other"), r.now());
    assert_eq!(Reason::InclusionInvalid, reason(r.verify(&AnchoredClaim { credential: other, ..c.clone() })));
}

#[test]
fn verify_rejects_unknown_or_foreign_anchor_and_otherwise_bad_claims() {
    let (r, c, _d) = anchored_claim();
    let unknown = AnchoredClaim { anchor: AnchorRef::new(c.anchor.anchor_id.clone(), "99"), ..c.clone() };
    assert_eq!(Reason::NotAnchored, reason(r.verify(&unknown)));
    let foreign = AnchoredClaim { anchor: AnchorRef::new("evm:1", "0xabc"), ..c.clone() };
    let set = AnchorSet::of(&[r.log.as_ref()]);
    assert_eq!(Reason::NotAnchored, reason(anchored::verify(&foreign, &snap(), &set, r.now(), SKEW_SEC, WINDOW)));
    assert!(set.resolve(&c.anchor).is_some());
    let house = snapshot::of("house", 1, "classic").unwrap();
    assert_eq!(Reason::SnapshotInvalid, reason(anchored::verify(&c, &house, r.log.as_ref(), r.now(), SKEW_SEC, WINDOW)));
    // the statement travels inside the credential: changing it breaks the signature
    let mut t = c.credential.to_json();
    t["credentialSubject"]["snapshotCID"] = serde_json::json!(snapshot::cid(&house).unwrap());
    let tampered = AnchoredClaim { credential: Credential::from_json(&t).unwrap(), ..c.clone() };
    assert_eq!(Reason::IssuerMismatch, reason(r.verify(&tampered)));
}

// ---------- commit-reveal ----------

struct Committed {
    env: Credential,
    salt: Vec<u8>,
    commitment: [u8; 32],
}

fn commit(r: &mut Rig, p: &Person) -> Committed {
    let (env, _) = claim_by(p, r.now());
    let salt = keccak(format!("commit-salt {}", p.did).as_bytes()).to_vec();
    let c = commitment::of(&p.did, &env.cid(), &salt).unwrap();
    assert!(matches!(r.reg.submit_commit(&c), CommitResult::Queued(_)));
    Committed { env, salt, commitment: c }
}

#[test]
fn commit_reveal_commitment_beats_a_direct_claim_anchored_before_the_reveal() {
    let mut r = Rig::new();
    let (alice, mallory) = (person("alice"), person("mallory"));
    let c = commit(&mut r, &alice);
    r.seal();
    assert!(r.reg.winner(&snap_cid()).is_none());
    r.clk.add(60);
    r.queue_now(&mallory);
    r.seal();
    r.clk.add(60);
    // alice signed 120 s before revealing: under the 300 s max age, so still accepted
    let res = r.reg.submit(&c.env, &snap(), Some(Reveal { salt: c.salt.clone() }));
    assert!(matches!(res, SubmitResult::Queued(_)), "{res:?}");
    r.seal();
    let w = r.reg.winner(&snap_cid()).unwrap();
    assert_eq!(alice.did, w.result.signer_did);
    assert!(w.result.via_commit);
    assert_eq!(0, w.result.priority.position);
    assert!(r.verify(&w.claim).unwrap().via_commit);
    assert!(r.reg.commit_proof(&c.commitment).is_some());
}

#[test]
fn without_a_commit_the_later_revealer_loses() {
    let mut r = Rig::new();
    let (alice, mallory) = (person("alice"), person("mallory"));
    r.queue_now(&mallory);
    r.seal();
    r.clk.add(60);
    r.queue_now(&alice);
    r.seal();
    assert_eq!(Some(mallory.did), r.winner_did());
}

#[test]
fn commit_reveal_a_copied_or_foreign_commitment_is_useless() {
    let mut r = Rig::new();
    let (alice, mallory) = (person("alice"), person("mallory"));
    let c = commit(&mut r, &alice);
    assert!(matches!(r.reg.submit_commit(&c.commitment), CommitResult::Duplicate(_)));
    r.seal();
    assert!(matches!(r.reg.submit_commit(&c.commitment), CommitResult::Duplicate(_)));
    r.clk.add(60);
    let (m_env, _m_data) = claim_by(&mallory, r.now());
    match r.reg.submit(&m_env, &snap(), Some(Reveal { salt: c.salt.clone() })) {
        SubmitResult::Rejected(f) => assert_eq!(Reason::CommitInvalid, f.reason),
        other => panic!("{other:?}"),
    }
    let mut fresh = Rig::new();
    let (b_env, _b_data) = claim_by(&person("b"), fresh.now());
    let res = fresh.reg.submit(&b_env, &snap(), Some(Reveal { salt: keccak(b"never committed").to_vec() }));
    assert!(matches!(res, SubmitResult::Rejected(_)));
}

#[test]
fn commit_reveal_verifier_rejects_a_bad_salt_or_unanchored_commitment() {
    let mut r = Rig::new();
    let c = commit(&mut r, &person("alice"));
    r.seal();
    r.clk.add(60);
    r.reg.submit(&c.env, &snap(), Some(Reveal { salt: c.salt.clone() }));
    r.seal();
    let good = r.reg.proof(&c.env.cid()).unwrap();
    let gc = good.commit.clone().unwrap();
    let bad_salt = AnchoredClaim { commit: Some(CommitRef { salt: vec![5; 32], ..gc.clone() }), ..good.clone() };
    assert_eq!(Reason::CommitInvalid, reason(r.verify(&bad_salt)));
    let no_anchor = AnchoredClaim {
        commit: Some(CommitRef { anchor: AnchorRef::new(good.anchor.anchor_id.clone(), "42"), ..gc.clone() }),
        ..good.clone()
    };
    assert_eq!(Reason::CommitInvalid, reason(r.verify(&no_anchor)));
    let short = AnchoredClaim { commit: Some(CommitRef { salt: vec![1; 8], ..gc }), ..good.clone() };
    assert_eq!(Reason::CommitInvalid, reason(r.verify(&short)));
    let without = AnchoredClaim { commit: None, ..good };
    assert!(!r.verify(&without).unwrap().via_commit);
}

#[test]
fn commit_reveal_reveal_outside_the_window_earns_only_the_claims_own_priority() {
    let mut r = Rig::with_window(100);
    let (alice, mallory) = (person("alice"), person("mallory"));
    let c = commit(&mut r, &alice);
    r.seal();
    r.clk.add(50);
    r.queue_now(&mallory);
    r.seal();
    r.clk.add(100);
    assert!(matches!(r.reg.submit(&c.env, &snap(), Some(Reveal { salt: c.salt.clone() })), SubmitResult::Queued(_)));
    r.seal();
    let w = r.reg.winner(&snap_cid()).unwrap();
    assert_eq!(mallory.did, w.result.signer_did);
    let a = r.reg.proof(&c.env.cid()).unwrap();
    let v = |win| anchored::verify(&a, &snap(), r.log.as_ref(), r.now(), SKEW_SEC, win).unwrap().via_commit;
    assert!(v(1000));
    assert!(!v(100));
}

#[test]
fn commitment_layout_and_salt_length() {
    let salt = [4u8; 16];
    let expect = keccak_parts(&[commitment::TAG.as_bytes(), b"did:x", &[0], b"zC", &[0], &salt]);
    assert_eq!(expect, commitment::of("did:x", "zC", &salt).unwrap());
    assert!(commitment::of("did:x", "zC", &[0; 15]).is_err());
    assert_ne!(commitment::new_salt().unwrap(), [0; 32]);
}

#[test]
fn seal_if_due_waits_for_the_interval() {
    let mut r = Rig::new();
    r.queue_now(&person("p"));
    assert!(r.reg.seal_if_due().unwrap().is_none());
    r.clk.add(600);
    // the queued claim is now 600 s old but it was accepted when fresh; sealing is fine
    assert!(r.reg.seal_if_due().unwrap().is_some());
    assert_eq!(0, r.reg.pending_count());
    assert!(r.reg.seal().unwrap().is_none());
}

#[test]
fn a_claim_for_another_snapshot_does_not_rank_here() {
    let r = Rig::new();
    let other = snapshot::of("house", 7, "classic").unwrap();
    let (e, _d) = claim_by_on(&person("p"), r.now(), &other);
    let mut reg_r = r;
    match reg_r.reg.submit(&e, &snap(), None) {
        SubmitResult::Rejected(f) => assert_eq!(Reason::SnapshotInvalid, f.reason),
        other => panic!("{other:?}"),
    }
}
