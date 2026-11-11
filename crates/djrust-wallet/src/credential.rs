//! Signed statements as W3C Verifiable Credentials (VC Data Model 2.0) secured with a Data
//! Integrity proof. docs/STANDARDS.md maps every piece to its standard and lists the gaps.
//!
//! ```text
//! {"@context":["https://www.w3.org/ns/credentials/v2","https://bitwrap.io/ns/credentials/v1"],
//!  "type":["VerifiableCredential","BitwrapCredential"],
//!  "issuer":"did:pkh:eip155:1:0x…",
//!  "validFrom":"2026-11-11T10:00:00Z", "validUntil"?:"…",
//!  "credentialSchema":{"id":"https://bitwrap.io/schema/<name>.schema.json","type":"JsonSchema"},
//!  "credentialSubject":{…the statement, checked by that schema…},
//!  "proof":{"type":"DataIntegrityProof","cryptosuite":"bitwrap-eip712-jcs-2026",
//!           "created":"…","expires"?:"…","verificationMethod":"did:pkh:…#blockchainAccountId",
//!           "proofPurpose":"assertionMethod","disclosureRoot":"0x…","proofValue":"z…"}}
//! ```
//!
//! The cryptosuite `bitwrap-eip712-jcs-2026` is this crate's, not a W3C one. Its transformation
//! and hashing are those of the W3C `ecdsa-jcs-2019` suite:
//!
//! - the proof configuration is the proof without `proofValue`, plus the document's `@context`;
//! - the document is the credential without `proof`;
//! - each is canonicalized with RFC 8785 (JCS) and hashed with SHA-256.
//!
//! Only the signature differs. It is secp256k1 (the curve of the did:pkh / Ethereum identity),
//! over the EIP-712 digest of `DataIntegrity(bytes32 proofConfig, bytes32 document)` in the
//! domain `{name:"bitwrap.io", version:"1", chainId:1}`, so a browser wallet's
//! `eth_signTypedData_v4` produces the same signature as the app's own key. `proofValue` is
//! the 65-byte r|s|v signature, multibase base58btc. The verification method is the issuer's
//! did:pkh `#blockchainAccountId`: the signature recovers an address, and it must be the
//! issuer's.
//!
//! `created` and `expires` repeat `validFrom` and `validUntil`, and `disclosureRoot` commits to
//! the salted leaves of `credentialSubject` (selective disclosure, [disclose]). All three sit in
//! the signed proof configuration, so a [Disclosure], which carries the proof and the document
//! hash but not the document, still binds the issuer, the validity window and the revealed
//! fields.

use crate::eip712::{Field, TypedData, Val};
use crate::encoding::{base58, hex};
use crate::key::{self, did, EthSignature};
use crate::merkle::{disclosure, Proof, Scheme};
use crate::{cid, jcs, schema, time, Error, Failure, Reason};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub const VC_CONTEXT: &str = "https://www.w3.org/ns/credentials/v2";
/// The terms this crate adds (`BitwrapCredential`, `disclosureRoot`, the subject vocabularies):
/// `schema/bitwrap-credentials.v1.context.jsonld`, to be served at this URL.
pub const BITWRAP_CONTEXT: &str = "https://bitwrap.io/ns/credentials/v1";
pub const CREDENTIAL_TYPE: &str = "BitwrapCredential";
pub const SCHEMA_TYPE: &str = "JsonSchema";
pub const PROOF_TYPE: &str = "DataIntegrityProof";
pub const CRYPTOSUITE: &str = "bitwrap-eip712-jcs-2026";
pub const PROOF_PURPOSE: &str = "assertionMethod";
pub const DOMAIN_NAME: &str = "bitwrap.io";
pub const DOMAIN_VERSION: &str = "1";
pub const CHAIN_ID: u64 = 1;
pub const PRIMARY_TYPE: &str = "DataIntegrity";

fn contexts() -> Value {
    json!([VC_CONTEXT, BITWRAP_CONTEXT])
}

fn sha256(v: &Value) -> Result<[u8; 32], Error> {
    Ok(Sha256::digest(jcs::bytes(v)?).into())
}

fn get_str<'a>(o: &'a Value, k: &str) -> Result<&'a str, Error> {
    o.get(k).and_then(Value::as_str).ok_or_else(|| Error::invalid(format!("missing string {k}")))
}

/// A credential up to, not including, its signature: what is hashed and signed.
#[derive(Clone, Debug, PartialEq)]
pub struct Unsigned {
    /// The issuer's did:pkh.
    pub issuer: String,
    /// Unix seconds.
    pub valid_from: u64,
    /// Unix seconds; 0 = no expiry.
    pub valid_until: u64,
    /// The `$id` of the schema `credentialSubject` follows.
    pub schema_id: String,
    pub subject: Value,
    pub disclosure_root: [u8; 32],
}

impl Unsigned {
    /// The unsecured document: the credential without `proof`.
    pub fn document(&self) -> Result<Value, Error> {
        let mut d = json!({
            "@context": contexts(),
            "type": [ "VerifiableCredential", CREDENTIAL_TYPE ],
            "issuer": self.issuer,
            "validFrom": time::format(self.valid_from)?,
            "credentialSchema": { "id": self.schema_id, "type": SCHEMA_TYPE },
            "credentialSubject": self.subject,
        });
        if self.valid_until != 0 {
            d["validUntil"] = json!(time::format(self.valid_until)?);
        }
        Ok(d)
    }

    /// The proof options: the proof without `proofValue`.
    pub fn proof_options(&self) -> Result<Value, Error> {
        let mut p = json!({
            "type": PROOF_TYPE,
            "cryptosuite": CRYPTOSUITE,
            "created": time::format(self.valid_from)?,
            "verificationMethod": did::verification_method(&self.issuer),
            "proofPurpose": PROOF_PURPOSE,
            "disclosureRoot": hex::encode0x(&self.disclosure_root),
        });
        if self.valid_until != 0 {
            p["expires"] = json!(time::format(self.valid_until)?);
        }
        Ok(p)
    }

    /// SHA-256 of the JCS document.
    pub fn document_hash(&self) -> Result<[u8; 32], Error> {
        sha256(&self.document()?)
    }

    /// The EIP-712 digest that is signed.
    pub fn digest(&self) -> Result<[u8; 32], Error> {
        signing_digest(&self.proof_options()?, &self.document_hash()?)
    }

    /// The same typed data as `eth_signTypedData_v4` JSON, for an external (browser) signer.
    pub fn typed_data_json(&self) -> Result<Value, Error> {
        let pc = proof_config_hash(&self.proof_options()?)?;
        Ok(json!({
            "types": {
                "EIP712Domain": [
                    {"name": "name", "type": "string"},
                    {"name": "version", "type": "string"},
                    {"name": "chainId", "type": "uint256"}
                ],
                PRIMARY_TYPE: fields().iter().map(|f| json!({"name": f.name, "type": f.ty})).collect::<Vec<_>>()
            },
            "primaryType": PRIMARY_TYPE,
            "domain": {"name": DOMAIN_NAME, "version": DOMAIN_VERSION, "chainId": CHAIN_ID},
            "message": {"proofConfig": hex::encode0x(&pc), "document": hex::encode0x(&self.document_hash()?)}
        }))
    }

    /// The credential JSON with the proof's `proofValue` left out (what an external signer gets).
    pub fn to_json(&self) -> Result<Value, Error> {
        let mut d = self.document()?;
        d["proof"] = self.proof_options()?;
        Ok(d)
    }

    pub fn from_json(v: &Value) -> Result<Self, Error> {
        let u = parse(v, false)?.0;
        if &u.to_json()? != v {
            return Err(Error::invalid("not a credential in this crate's form"));
        }
        Ok(u)
    }

    pub fn with_signature(self, signature: EthSignature) -> Credential {
        Credential { unsigned: self, signature }
    }
}

fn fields() -> Vec<Field> {
    vec![Field::new("proofConfig", "bytes32"), Field::new("document", "bytes32")]
}

fn proof_config_hash(proof_options: &Value) -> Result<[u8; 32], Error> {
    let mut pc = proof_options.clone();
    pc.as_object_mut().ok_or_else(|| Error::invalid("proof must be an object"))?.remove("proofValue");
    pc["@context"] = contexts();
    sha256(&pc)
}

/// keccak256(0x19 0x01 | domainSeparator | hashStruct(DataIntegrity{proofConfig, document})).
fn signing_digest(proof: &Value, document_hash: &[u8; 32]) -> Result<[u8; 32], Error> {
    let pc = proof_config_hash(proof)?;
    let td = TypedData {
        types: BTreeMap::from([(PRIMARY_TYPE.to_string(), fields())]),
        primary_type: PRIMARY_TYPE.into(),
        domain: BTreeMap::from([
            ("name".to_string(), Val::Str(DOMAIN_NAME.into())),
            ("version".to_string(), Val::Str(DOMAIN_VERSION.into())),
            ("chainId".to_string(), Val::Uint(CHAIN_ID as u128)),
        ]),
        message: BTreeMap::from([
            ("proofConfig".to_string(), Val::Str(hex::encode0x(&pc))),
            ("document".to_string(), Val::Str(hex::encode0x(document_hash))),
        ]),
    };
    td.signing_hash()
}

/// A signed credential.
#[derive(Clone, Debug, PartialEq)]
pub struct Credential {
    pub unsigned: Unsigned,
    pub signature: EthSignature,
}

impl Credential {
    pub fn issuer(&self) -> &str {
        &self.unsigned.issuer
    }
    pub fn schema_id(&self) -> &str {
        &self.unsigned.schema_id
    }
    pub fn subject(&self) -> &Value {
        &self.unsigned.subject
    }
    pub fn valid_from(&self) -> u64 {
        self.unsigned.valid_from
    }
    pub fn valid_until(&self) -> u64 {
        self.unsigned.valid_until
    }

    pub fn proof_value(&self) -> String {
        format!("z{}", base58::encode(&self.signature.0))
    }

    pub fn to_json(&self) -> Value {
        let mut v = self.unsigned.to_json().expect("a parsed or sealed credential always serializes");
        v["proof"]["proofValue"] = json!(self.proof_value());
        v
    }

    /// Content address of the whole credential, proof included.
    pub fn cid(&self) -> String {
        cid::of_json(&self.to_json()).expect("a credential always canonicalizes")
    }

    /// Strict: the input must be exactly what [Credential::to_json] writes for what it says.
    pub fn from_json(v: &Value) -> Result<Self, Error> {
        let (unsigned, sig) = parse(v, true)?;
        let c = unsigned.with_signature(sig.expect("parse(.., true) returns the signature"));
        if &c.to_json() != v {
            return Err(Error::invalid("not a credential in this crate's form"));
        }
        Ok(c)
    }
}

fn parse_proof_value(s: &str) -> Result<EthSignature, Error> {
    let raw = base58::decode(s.strip_prefix('z').ok_or_else(|| Error::invalid("proofValue must be multibase base58btc"))?)?;
    EthSignature::from_slice(&raw)
}

fn parse(v: &Value, signed: bool) -> Result<(Unsigned, Option<EthSignature>), Error> {
    let o = v.as_object().ok_or_else(|| Error::invalid("a credential is an object"))?;
    if o.get("@context") != Some(&contexts()) {
        return Err(Error::invalid("unsupported @context"));
    }
    let p = o.get("proof").ok_or_else(|| Error::invalid("missing proof"))?;
    if get_str(p, "type")? != PROOF_TYPE || get_str(p, "cryptosuite")? != CRYPTOSUITE {
        return Err(Error::invalid("unsupported proof type or cryptosuite"));
    }
    let schema = o.get("credentialSchema").ok_or_else(|| Error::invalid("missing credentialSchema"))?;
    let until = match o.get("validUntil") {
        None => 0,
        Some(t) => time::parse(t.as_str().ok_or_else(|| Error::invalid("validUntil must be a string"))?)?,
    };
    let unsigned = Unsigned {
        issuer: get_str(v, "issuer")?.into(),
        valid_from: time::parse(get_str(v, "validFrom")?)?,
        valid_until: until,
        schema_id: get_str(schema, "id")?.into(),
        subject: o.get("credentialSubject").cloned().ok_or_else(|| Error::invalid("missing credentialSubject"))?,
        disclosure_root: hex::decode_n::<32>(get_str(p, "disclosureRoot")?)?,
    };
    let sig = if signed { Some(parse_proof_value(get_str(p, "proofValue")?)?) } else { None };
    Ok((unsigned, sig))
}

/// A credential ready to be signed, plus the holder secret for later disclosure.
#[derive(Clone, Debug)]
pub struct Sealed {
    pub doc_salt: Vec<u8>,
    pub unsigned: Unsigned,
}

impl Sealed {
    /// The statement: `credentialSubject`.
    pub fn data(&self) -> &Value {
        &self.unsigned.subject
    }
}

/// Fresh 32-byte holder salt from the OS CSPRNG.
pub fn new_doc_salt() -> Result<Vec<u8>, Error> {
    let mut s = vec![0u8; 32];
    getrandom::getrandom(&mut s).map_err(|e| Error::invalid(format!("no randomness: {e}")))?;
    Ok(s)
}

/// Builds the credential for `subject` under the schema whose `$id` is `schema_id`, issued by
/// `issuer` (a did:pkh) from `valid_from` (Unix seconds) until `valid_until` (0 = no expiry).
pub fn seal(schema_id: &str, subject: &Value, issuer: &str, valid_from: u64, valid_until: u64, doc_salt: &[u8]) -> Result<Sealed, Error> {
    match schema::by_id(schema_id) {
        None => return Err(Error::invalid(format!("unknown schema {schema_id}"))),
        Some(s) if !s.signable() => return Err(Error::invalid(format!("{} is unsigned data; sign a statement that names it by CID", s.name))),
        Some(_) => {}
    }
    if did::parse(issuer).is_none() {
        return Err(Error::invalid("issuer is not a did:pkh:eip155 DID"));
    }
    if !subject.is_object() {
        return Err(Error::invalid("credentialSubject must be an object"));
    }
    if doc_salt.len() < 16 {
        return Err(Error::invalid("docSalt too short"));
    }
    if valid_until != 0 && valid_until <= valid_from {
        return Err(Error::invalid("validUntil must be after validFrom"));
    }
    let unsigned = Unsigned {
        issuer: issuer.into(),
        valid_from,
        valid_until,
        schema_id: schema_id.into(),
        subject: subject.clone(),
        disclosure_root: disclosure::root(subject, doc_salt)?,
    };
    unsigned.to_json()?;
    Ok(Sealed { doc_salt: doc_salt.to_vec(), unsigned })
}

/// Software-key signing (tests, ephemeral keys). Real keys sign through platform custody: they
/// sign `sealed.unsigned.digest()` and the app calls [Unsigned::with_signature].
pub fn sign(sealed: &Sealed, priv32: &[u8]) -> Result<Credential, Error> {
    let signer = did::pkh(&key::address(priv32)?);
    if signer != sealed.unsigned.issuer {
        return Err(Error::invalid("the key is not the issuer's"));
    }
    Ok(sealed.unsigned.clone().with_signature(key::sign(&sealed.unsigned.digest()?, priv32)?))
}

/// A successful verification: the issuer, whose key made the signature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verified {
    pub signer_address: String,
    pub signer_did: String,
    pub valid_from: u64,
    pub valid_until: u64,
}

pub type Verification = Result<Verified, Failure>;

fn check_window(from: u64, until: u64, now: u64, skew: u64) -> Result<(), Failure> {
    if until != 0 && until <= from {
        return Err(Failure::new(Reason::BadWindow));
    }
    if from > now.saturating_add(skew) {
        return Err(Failure::new(Reason::NotYetValid));
    }
    if until != 0 && now >= until {
        return Err(Failure::new(Reason::Expired));
    }
    Ok(())
}

/// Recovers the signer, checks it is the issuer, and checks the validity window.
fn check_signature(proof: &Value, document_hash: &[u8; 32], sig: &EthSignature) -> Result<(String, String), Failure> {
    if !key::is_low_s(sig) {
        return Err(Failure::new(Reason::HighS));
    }
    let digest = signing_digest(proof, document_hash).map_err(|e| Failure::with(Reason::MalformedData, &e.0))?;
    let addr = key::recover_address(&digest, sig).ok_or(Failure::new(Reason::SignerUnrecoverable))?;
    let checksum = key::checksum_address(&addr);
    let signer = did::pkh(&checksum);
    let vm = proof.get("verificationMethod").and_then(Value::as_str).unwrap_or_default();
    if vm != did::verification_method(&signer) {
        return Err(Failure::with(Reason::IssuerMismatch, "the signature is not the verification method's"));
    }
    Ok((checksum, signer))
}

/// Signature, issuer and validity window (`now` is injected). The statement's own rules are the
/// schema-specific checks' (invites, claims, posts, ...).
pub fn verify(c: &Credential, now: u64, skew: u64) -> Verification {
    let u = &c.unsigned;
    if did::parse(&u.issuer).is_none() {
        return Err(Failure::with(Reason::IssuerMismatch, "issuer is not a did:pkh"));
    }
    let proof = u.proof_options().map_err(|e| Failure::with(Reason::MalformedData, &e.0))?;
    let dh = u.document_hash().map_err(|e| Failure::with(Reason::MalformedData, &e.0))?;
    let (checksum, signer) = check_signature(&proof, &dh, &c.signature)?;
    if signer != u.issuer {
        return Err(Failure::with(Reason::IssuerMismatch, &u.issuer));
    }
    check_window(u.valid_from, u.valid_until, now, skew)?;
    Ok(Verified { signer_address: checksum, signer_did: signer, valid_from: u.valid_from, valid_until: u.valid_until })
}

/// The credential follows a signable schema this crate ships, and `schema_doc` (if given) is that
/// schema.
pub fn check_schema(c: &Credential, schema_doc: Option<&Value>) -> Result<&'static schema::SchemaDoc, Failure> {
    let s = schema::by_id(&c.unsigned.schema_id).filter(|s| s.signable()).ok_or(Failure::new(Reason::WrongSchema))?;
    match schema_doc {
        Some(d) if cid::of_json(d).ok().as_deref() != Some(s.cid()) => Err(Failure::new(Reason::SchemaCidMismatch)),
        _ => Ok(s),
    }
}

/// With the holder's salt, the disclosure root is the one of `credentialSubject`.
pub fn check_root(c: &Credential, doc_salt: &[u8]) -> Result<(), Failure> {
    if disclosure::root(&c.unsigned.subject, doc_salt).ok() != Some(c.unsigned.disclosure_root) {
        return Err(Failure::new(Reason::RootMismatch));
    }
    Ok(())
}

/// One revealed field of `credentialSubject` with its proof against `disclosureRoot`.
#[derive(Clone, Debug, PartialEq)]
pub struct DisclosedField {
    pub pointer: String,
    pub value: Value,
    pub salt: [u8; 16],
    pub proof: Proof,
}

impl DisclosedField {
    pub fn to_json(&self) -> Value {
        json!({"pointer": self.pointer, "value": self.value, "salt": hex::encode(&self.salt), "proof": self.proof.to_json()})
    }

    pub fn from_json(o: &Value) -> Result<Self, Error> {
        Ok(Self {
            pointer: get_str(o, "pointer")?.into(),
            value: o.get("value").cloned().ok_or_else(|| Error::invalid("missing value"))?,
            salt: hex::decode_n::<16>(get_str(o, "salt")?)?,
            proof: Proof::from_json(o.get("proof").ok_or_else(|| Error::invalid("missing proof"))?)?,
        })
    }
}

/// Every leaf whose pointer equals, or lies under, one of `pointers`.
pub fn disclose(sealed: &Sealed, pointers: &[&str]) -> Result<Vec<DisclosedField>, Error> {
    let leaves = disclosure::flatten(sealed.data());
    let pre = disclosure::leaf_preimages(&leaves, &sealed.doc_salt)?;
    let scheme = Scheme::plain();
    leaves
        .iter()
        .enumerate()
        .filter(|(_, l)| pointers.iter().any(|p| l.pointer == *p || l.pointer.starts_with(&format!("{p}/"))))
        .map(|(i, l)| {
            Ok(DisclosedField {
                pointer: l.pointer.clone(),
                value: l.value.clone(),
                salt: disclosure::salt(&sealed.doc_salt, &l.pointer),
                proof: scheme.prove(&pre, i)?,
            })
        })
        .collect()
}

/// What a holder shows instead of the whole credential: its proof, the hash of its document,
/// and the fields of `credentialSubject` it chooses to reveal. Not a W3C Verifiable
/// Presentation (docs/STANDARDS.md, "Gaps").
#[derive(Clone, Debug, PartialEq)]
pub struct Disclosure {
    /// The credential's proof, `proofValue` included.
    pub proof: Value,
    pub document_hash: [u8; 32],
    pub fields: Vec<DisclosedField>,
}

pub const DISCLOSURE_TYPE: &str = "BitwrapDisclosure";

impl Disclosure {
    pub fn of(c: &Credential, sealed: &Sealed, pointers: &[&str]) -> Result<Self, Error> {
        if sealed.unsigned != c.unsigned {
            return Err(Error::invalid("the sealed document is not this credential's"));
        }
        Ok(Self {
            proof: c.to_json()["proof"].clone(),
            document_hash: c.unsigned.document_hash()?,
            fields: disclose(sealed, pointers)?,
        })
    }

    pub fn to_json(&self) -> Value {
        json!({"@context": contexts(), "type": DISCLOSURE_TYPE, "proof": self.proof,
               "documentHash": hex::encode0x(&self.document_hash),
               "disclosed": self.fields.iter().map(DisclosedField::to_json).collect::<Vec<_>>()})
    }

    pub fn from_json(v: &Value) -> Result<Self, Error> {
        if v.get("type").and_then(Value::as_str) != Some(DISCLOSURE_TYPE) || v.get("@context") != Some(&contexts()) {
            return Err(Error::invalid("not a BitwrapDisclosure"));
        }
        let fields = v
            .get("disclosed")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::invalid("missing disclosed"))?
            .iter()
            .map(DisclosedField::from_json)
            .collect::<Result<_, _>>()?;
        let proof = v.get("proof").filter(|p| p.is_object()).cloned().ok_or_else(|| Error::invalid("missing proof"))?;
        Ok(Self { proof, document_hash: hex::decode_n::<32>(get_str(v, "documentHash")?)?, fields })
    }
}

/// The issuer signed a credential with this document hash and proof; its window holds; every
/// revealed field proves into the signed `disclosureRoot`.
pub fn verify_disclosure(d: &Disclosure, now: u64, skew: u64) -> Verification {
    let malformed = |m: &str| Failure::with(Reason::MalformedData, m);
    let p: &Map<String, Value> = d.proof.as_object().ok_or_else(|| malformed("proof"))?;
    let s = |k: &str| p.get(k).and_then(Value::as_str);
    if s("type") != Some(PROOF_TYPE) || s("cryptosuite") != Some(CRYPTOSUITE) || s("proofPurpose") != Some(PROOF_PURPOSE) {
        return Err(malformed("unsupported proof"));
    }
    const KEYS: [&str; 8] = ["type", "cryptosuite", "created", "expires", "verificationMethod", "proofPurpose", "disclosureRoot", "proofValue"];
    if p.keys().any(|k| !KEYS.contains(&k.as_str())) {
        return Err(malformed("unknown proof member"));
    }
    let sig = s("proofValue").and_then(|v| parse_proof_value(v).ok()).ok_or_else(|| malformed("proofValue"))?;
    let from = s("created").and_then(|t| time::parse(t).ok()).ok_or_else(|| malformed("created"))?;
    let until = match p.get("expires") {
        None => 0,
        Some(t) => t.as_str().and_then(|t| time::parse(t).ok()).ok_or_else(|| malformed("expires"))?,
    };
    let root = s("disclosureRoot").and_then(|r| hex::decode_n::<32>(r).ok()).ok_or_else(|| malformed("disclosureRoot"))?;
    let (checksum, signer) = check_signature(&d.proof, &d.document_hash, &sig)?;
    check_window(from, until, now, skew)?;
    let scheme = Scheme::plain();
    for f in &d.fields {
        let ok = disclosure::preimage(&f.pointer, &f.value, &f.salt).map(|pre| scheme.verify(&root, &pre, &f.proof)).unwrap_or(false);
        if !ok {
            return Err(Failure::with(Reason::ProofInvalid, &f.pointer));
        }
    }
    Ok(Verified { signer_address: checksum, signer_did: signer, valid_from: from, valid_until: until })
}

/// The checks every statement module starts with: right schema, then [verify].
pub fn verify_as(c: &Credential, expected: &schema::SchemaDoc, now: u64, skew: u64) -> Verification {
    if c.unsigned.schema_id != expected.id() {
        return Err(Failure::new(Reason::WrongSchema));
    }
    verify(c, now, skew)
}

/// `credentialSubject` as an object whose sorted keys are one of `shapes`, with `@type` / `v`.
pub fn subject_shape<'a>(
    c: &'a Credential,
    at_type: &str,
    shapes: &[&[&str]],
) -> Result<&'a Map<String, Value>, Failure> {
    let malformed = || Failure::new(Reason::MalformedData);
    let o = c.unsigned.subject.as_object().ok_or_else(malformed)?;
    let mut keys: Vec<&str> = o.keys().map(String::as_str).collect();
    keys.sort();
    if !shapes.iter().any(|s| *s == keys.as_slice())
        || o.get("@type").and_then(Value::as_str) != Some(at_type)
        || o.get("v").and_then(Value::as_i64) != Some(1)
    {
        return Err(malformed());
    }
    Ok(o)
}
