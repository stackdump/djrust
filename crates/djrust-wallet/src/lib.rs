//! djrust-wallet: the platform-neutral core of the bitwrap identity wallet.
//!
//! - Identities are `did:pkh:eip155:1:<EIP-55 address>` (secp256k1). A user's profile host can
//!   serve a did:web document for the same key ([key::did::web_document]).
//! - Signed statements are W3C Verifiable Credentials (Data Model 2.0) secured with a Data
//!   Integrity proof whose hashing is the W3C `ecdsa-jcs-2019` suite's, signed with secp256k1
//!   over EIP-712 ([credential]).
//! - Also here: salted selective disclosure, `$id`-addressed schemas with pinned CIDs, the
//!   statements (invites, revocations, charted-by claims, snapshot shares, posts), beats legacy
//!   signing and Merkle-batch anchoring logic.
//!
//! No I/O, no network and no key storage. Each platform's custody (Android Keystore, browser
//! WebCrypto) holds the key and hands it in for one signature, or signs the 32-byte digest
//! itself. docs/STANDARDS.md maps all of it to the standards; `vectors/wallet.v1.json` is the
//! contract every binding must reproduce.

pub mod anchor;
pub mod cid;
pub mod chart;
pub mod claims;
pub mod credential;
pub mod eip712;
pub mod encoding;
pub mod invites;
pub mod jcs;
pub mod key;
pub mod legacy;
pub mod merkle;
pub mod posts;
pub mod revocation;
pub mod schema;
pub mod snapshot;
pub mod snapshot_share;
pub mod text_policy;
pub mod time;

pub use encoding::{base58, hex, keccak};
pub use credential::{Credential, Disclosure, Sealed, Unsigned, Verified};
pub use key::EthSignature;

/// Default clock skew for verification, seconds.
pub const SKEW_SEC: u64 = 300;

/// An input that cannot be used (bad hex, bad JSON, out-of-range number, invalid key, ...).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error(pub String);

impl Error {
    pub fn invalid(msg: impl Into<String>) -> Self {
        Self(msg.into())
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

/// Why a verification failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Reason {
    BadSignature,
    HighS,
    SignerUnrecoverable,
    NotYetValid,
    Expired,
    BadWindow,
    SubjectMismatch,
    SchemaCidMismatch,
    DataCidMismatch,
    RootMismatch,
    ProofInvalid,
    BadCid,
    WrongSchema,
    MalformedData,
    PolicyViolation,
    ClaimMismatch,
    SnapshotInvalid,
    Stale,
    NotAnchored,
    InclusionInvalid,
    CommitInvalid,
    IssuerMismatch,
}

impl Reason {
    /// SCREAMING_SNAKE name, as the bindings spell it.
    pub fn name(self) -> &'static str {
        use Reason::*;
        match self {
            BadSignature => "BAD_SIGNATURE",
            HighS => "HIGH_S",
            SignerUnrecoverable => "SIGNER_UNRECOVERABLE",
            NotYetValid => "NOT_YET_VALID",
            Expired => "EXPIRED",
            BadWindow => "BAD_WINDOW",
            SubjectMismatch => "SUBJECT_MISMATCH",
            SchemaCidMismatch => "SCHEMA_CID_MISMATCH",
            DataCidMismatch => "DATA_CID_MISMATCH",
            RootMismatch => "ROOT_MISMATCH",
            ProofInvalid => "PROOF_INVALID",
            BadCid => "BAD_CID",
            WrongSchema => "WRONG_SCHEMA",
            MalformedData => "MALFORMED_DATA",
            PolicyViolation => "POLICY_VIOLATION",
            ClaimMismatch => "CLAIM_MISMATCH",
            SnapshotInvalid => "SNAPSHOT_INVALID",
            Stale => "STALE",
            NotAnchored => "NOT_ANCHORED",
            InclusionInvalid => "INCLUSION_INVALID",
            CommitInvalid => "COMMIT_INVALID",
            IssuerMismatch => "ISSUER_MISMATCH",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub reason: Reason,
    pub detail: String,
}

impl Failure {
    pub fn new(reason: Reason) -> Self {
        Self { reason, detail: String::new() }
    }
    pub fn with(reason: Reason, detail: &str) -> Self {
        Self { reason, detail: detail.into() }
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.detail.is_empty() {
            f.write_str(self.reason.name())
        } else {
            write!(f, "{}: {}", self.reason.name(), self.detail)
        }
    }
}
