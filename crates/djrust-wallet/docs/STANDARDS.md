# djrust-wallet and the W3C decentralized identity standards

This maps each piece of the wallet to the standard it follows and says plainly where it departs
from one. The last section lists the gaps.

## Summary

| piece | standard | status |
|---|---|---|
| identifier | `did:pkh` (W3C CCG), CAIP-10 account id on `eip155` | follows the method spec |
| profile name | `did:web` (W3C CCG) | document builder and checker; resolution is the app's |
| data model | W3C Verifiable Credentials Data Model 2.0 (Recommendation) | follows it for the members used |
| schema | W3C VC JSON Schema (`credentialSchema`, type `JsonSchema`) | partial: the schema constrains `credentialSubject`, not the whole credential |
| proof container | W3C Data Integrity 1.0 (`DataIntegrityProof`) | follows it |
| cryptosuite | `bitwrap-eip712-jcs-2026` | **not W3C**: this crate's own, described below |
| canonicalization | RFC 8785 (JCS), as in the W3C `ecdsa-jcs-2019` suite | follows it |
| signature | secp256k1 ECDSA, recoverable, over an EIP-712 digest | **community** (Ethereum), not a W3C suite |
| verification method | `EcdsaSecp256k1RecoveryMethod2020` (W3C CCG), as did:pkh defines it | community |
| selective disclosure | salted Merkle tree over `credentialSubject` | **this crate's own**, not `ecdsa-sd-2023` or SD-JWT |
| content ids | CIDv1, dag-json, sha2-256, base58btc | Multiformats (IETF drafts), not W3C |

## Identifiers

**did:pkh.** An identity is a secp256k1 key held in each platform's custody. Its DID is
`did:pkh:eip155:1:<EIP-55 address>`. The method-specific part is the CAIP-10 account id
`eip155:1:<address>`. `key::did::pkh_document` writes the DID document the did:pkh method
generates from the DID. It holds one verification method:

- `<did>#blockchainAccountId`;
- type `EcdsaSecp256k1RecoveryMethod2020`;
- `blockchainAccountId` set to the CAIP-10 id;
- listed under `authentication` and `assertionMethod`.

The document has no public key, because did:pkh publishes none: a verifier recovers the key from
the signature and compares addresses.

**did:web for a profile.** A user's subdomain (for example `alice.bitwrap.io`) can serve
`https://alice.bitwrap.io/.well-known/did.json`, the DID document of `did:web:alice.bitwrap.io`.
`key::did::web_document(host, pkh)` writes that document:

- `alsoKnownAs` is `[<the did:pkh>]`, and `controller` is the did:pkh;
- it holds the same `EcdsaSecp256k1RecoveryMethod2020` method with the same `blockchainAccountId`.

`check_web_document` accepts a fetched document only if it is exactly that, and returns the
did:pkh. Credentials are still **issued by the did:pkh**. The did:web is a resolvable, readable
name that points at it, not a second key. See the gaps for why.

## Credentials

Every signed statement is one VC Data Model 2.0 credential:

```json
{
  "@context": ["https://www.w3.org/ns/credentials/v2", "https://bitwrap.io/ns/credentials/v1"],
  "type": ["VerifiableCredential", "BitwrapCredential"],
  "issuer": "did:pkh:eip155:1:0x…",
  "validFrom": "2026-11-11T10:00:00Z",
  "credentialSchema": {"id": "https://bitwrap.io/schema/bitwrap-invite.v1.schema.json", "type": "JsonSchema"},
  "credentialSubject": {"@type": "BitwrapInvite", "v": 1, "inviter": "did:pkh:…", "code": "0x…"},
  "proof": {
    "type": "DataIntegrityProof",
    "cryptosuite": "bitwrap-eip712-jcs-2026",
    "created": "2026-11-11T10:00:00Z",
    "verificationMethod": "did:pkh:eip155:1:0x…#blockchainAccountId",
    "proofPurpose": "assertionMethod",
    "disclosureRoot": "0x…",
    "proofValue": "z…"
  }
}
```

- `validUntil` (and `proof.expires`) appear only when the credential expires.
- Times are written in one form only, `YYYY-MM-DDTHH:MM:SSZ`, so the bytes depend only on the instant.
- The credential has no `id`. It is named by its CID: JCS bytes of the whole credential, proof
  included, then sha2-256, then CIDv1 dag-json, base58btc. An invite accept names its invite this way.
- Parsing is strict: a credential must be exactly what this crate writes for what it says. Extra
  members, other contexts and other suites are refused rather than ignored.

The statements and their schemas:

| statement | schema `$id` (https://bitwrap.io/schema/…) | `credentialSubject` | issuer |
|---|---|---|---|
| invite | `bitwrap-invite.v1` | `BitwrapInvite`: `inviter`, `code`, `securedIn`? | the inviter |
| invite accept | `bitwrap-invite-accept.v1` | `BitwrapInviteAccept`: `invite` (CID of the invite credential), `inviter`, `member` | the member |
| revocation | `bitwrap-revocation.v1` | `BitwrapRevocation`: `id` = the revoked DID | the revoked DID itself |
| charted by | `bitwrap-chart-claim.v1` | `BitwrapChartClaim`: `id` = `ipfs://<track CID>`, `chartedBy`, `member`, `render` | the charter |
| snapshot share | `bitwrap-snapshot-share.v1` | `BitwrapSnapshotShare`: `id` = `ipfs://<DjSnapshot CID>` | whoever shares it |
| track share, discovery claim, post | `beats-track.v1`, `beats-discovery-claim.v1`, `beats-post.v1` (beats.bitwrap.io) | the payload | the signer |

A subject that is a CID-addressed document uses `ipfs://<cid>` as its `id`, so it is a URL as
VC 2.0 requires.

Two documents are unsigned data by definition and are never a credential's subject: a track's
`beats-snapshot.v1` and a whole session's `dj-snapshot.v1` (`DjSnapshot`). A statement names them
by CID instead (a chart claim, a snapshot share), and the verifier fetches the document and
checks it hashes to that CID. `credential::seal` refuses those two schemas, and `check_schema`
refuses a credential that claims one. This keeps a session's size out of the credential: a share
of a mix with eight vocoders is the same size as a share of an empty one. `schema/bitwrap-credentials.v1.context.jsonld` defines every term the statements
use, and is to be served at `https://bitwrap.io/ns/credentials/v1`.

## The proof: `bitwrap-eip712-jcs-2026`

This is the one place the wallet uses a suite of its own. The transformation and hashing are
those of W3C `ecdsa-jcs-2019`. Only the signature step differs.

1. **Proof configuration:**
   - take the proof without `proofValue`;
   - set its `@context` to the document's;
   - JCS it, then SHA-256: `proofConfig`.
2. **Document:** the credential without `proof`, JCS, then SHA-256: `document`.
3. **Digest:** the EIP-712 hash of `DataIntegrity(bytes32 proofConfig, bytes32 document)` in
   the domain `{name: "bitwrap.io", version: "1", chainId: 1}`.
4. **Signature:**
   - secp256k1 ECDSA over that digest: RFC 6979 deterministic, low-S (EIP-2), `v` = 27 or 28;
   - `proofValue` is `z` + base58btc of the 65 bytes `r | s | v`.
5. **Verification:**
   - recompute both hashes and the digest, and recover the address;
   - `verificationMethod` must be that address's did:pkh `#blockchainAccountId`, and the issuer
     must be the same did:pkh;
   - then check the window, `validFrom` ≤ now + skew < `validUntil`.

Steps 1 and 2 are exactly `ecdsa-jcs-2019`, so its hashing can be reused as is. Where the two part:

- **Curve.** `ecdsa-jcs-2019` is P-256 and P-384 only. The identity is an Ethereum key, which is
  secp256k1.
- **What is signed.** `ecdsa-jcs-2019` signs `proofConfigHash ‖ documentHash` directly. Here the
  two hashes are wrapped in EIP-712 so a browser wallet can produce the identical signature with
  `eth_signTypedData_v4` (`Unsigned::typed_data_json`).
- **Signature form.** The signature is recoverable (`v` byte), and the verification method
  carries an account id, not a public key.

EIP-712 and `EcdsaSecp256k1RecoveryMethod2020` are community specifications: Ethereum's, and the
W3C Credentials Community Group's. No W3C Recommendation defines a secp256k1 Data Integrity
suite, so a generic VC verifier will not know `bitwrap-eip712-jcs-2026` unless it is taught it.
The vectors file includes the typed data, both hashes and the digest of every credential, so
another implementation can check each step.

`created`, `expires` and `disclosureRoot` are in the proof configuration, so they are signed.
`created` and `expires` repeat `validFrom` and `validUntil`, which lets a disclosure (below) show
the validity window without the document.

## Selective disclosure

At sealing, each leaf of `credentialSubject` (its RFC 6901 pointer, JCS value and a salt derived
from a 32-byte holder secret) becomes a keccak Merkle leaf. The root is `proof.disclosureRoot`.
A `BitwrapDisclosure` carries:

- the proof, `proofValue` included;
- the document hash;
- the revealed fields, each with its salt and inclusion proof.

The verifier recomputes the proof configuration hash, checks the signature over the given
document hash, and checks each field against the signed root. It learns the issuer, the window
and those fields, and nothing else of the subject.

## Gaps

1. **No W3C cryptosuite covers secp256k1.**
   - The W3C suites are `ecdsa-rdfc-2019`, `ecdsa-jcs-2019` and `ecdsa-sd-2023` (P-256, P-384),
     and `eddsa-rdfc-2022` / `eddsa-jcs-2022` (Ed25519).
   - Interoperability with generic verifiers needs one of three things:
     - teaching them `bitwrap-eip712-jcs-2026`;
     - issuing a second proof (Data Integrity allows a proof set) with an Ed25519 or P-256 key
       the identity also controls, which needs a DID method that can publish that key (did:web
       below);
     - securing with VC-JOSE-COSE and `ES256K`.
2. **EIP-712 is not a W3C mechanism.** The wrapping exists so that browser wallets can sign. A
   pure `ecdsa-jcs-2019`-style secp256k1 signature over `proofConfigHash ‖ documentHash` would be
   closer to the standard, but no browser wallet would produce it.
3. **did:web is a name, not an issuer.**
   - Issuing as did:web would need a resolver (network) at verification, and the profile host's
     operator could repoint it. So credentials stay issued by the did:pkh, and the did:web
     document points to it with `alsoKnownAs`.
   - The reverse link, from did:pkh to the profile, is not made yet. A did:pkh has no document
     to edit. The options are a self-issued credential naming the did:web, or DIF Well Known DID
     Configuration on the host.
4. **The schemas constrain `credentialSubject`.** VC JSON Schema applies a schema to the whole
   credential. A credential-level wrapper schema (`allOf` the VC 2.0 shape plus
   `properties.credentialSubject: {$ref: …}`) would close this.
5. **No schema digest in the credential.** The schema is named by its `$id`. Its bytes are pinned
   by CID in this crate's registry and vectors, not in the credential. VC 2.0 `relatedResource`
   with `digestMultibase` would put the pin in-band, at about 150 bytes per credential.
6. **No JSON Schema validation in the core.** Each statement module checks its subject's exact
   shape in code. The schema documents are not executed.
7. **The disclosure is not a Verifiable Presentation.**
   - A `BitwrapDisclosure` is not signed by the holder, so it carries no holder binding,
     challenge or domain.
   - The standard selective-disclosure routes are `ecdsa-sd-2023` (P-256, RDF canonicalization)
     and SD-JWT VC (IETF). Neither works with a recoverable secp256k1 did:pkh as is.
8. **JSON-LD is not processed.** Verification is JCS-only; no context is fetched or expanded. The
   bitwrap context is `@protected`, and `credentialSubject` members such as `v` are defined in it,
   but RDF-based tools have not been run over it. The context is not served yet.
9. **No credential status.**
   - Revocation is a self-issued `BitwrapRevocation` that the storage service logs. It takes
     effect at its position in the log, not at its `validFrom`.
   - It is not a `credentialStatus` entry (Bitstring Status List). Third parties learn of it from
     the service's log, not from the credential.
10. **Content ids.**
    - `ipfs://<cid>` names a dag-json CID that is served by bitwrap's CDN, not necessarily on IPFS.
    - CIDs and multibase are Multiformats (IETF drafts), not W3C.
11. **Size.**
    - Every statement credential is about 900 to 1,200 bytes (vectors: `statements.*.bytes`),
      under the storage service's 1,536-byte blob limit, which the vectors test enforces.
    - The VC 2.0 shape costs about 800 bytes of that (contexts, schema, issuer and the proof).
      That is why a whole-session snapshot is shared by reference: embedded, a two-deck snapshot
      came to about 1,600 bytes.
    - A snapshot share proves who shared which CID, not that the document is still served: a
      verifier needs the document from a host (cdn.bitwrap.io) or the sharer.
12. **Not reviewed.** This mapping is the author's reading of the specifications. No conformance
    suite has been run against it.
