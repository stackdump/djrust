# djrust-wallet

**The bitwrap identity wallet core.** It gives a user a self-held identity and signs what they
share (a track, a whole DJ snapshot, an invite, a "charted by" claim) as a W3C Verifiable
Credential that anyone can check offline. The same code runs in the browser (wasm), on Android
and iOS (FFI), on the desktop and in the storage service, so every platform signs and verifies
the same bytes.

- **Identity.** A secp256k1 key, named `did:pkh:eip155:1:<address>`. A profile host can publish a
  `did:web` document for the same key.
- **Credentials.** W3C VC Data Model 2.0 with a Data Integrity proof. The proof is hashed as in
  the W3C `ecdsa-jcs-2019` suite (RFC 8785 JCS, SHA-256) and signed with secp256k1 over EIP-712,
  so a browser wallet can sign too. [docs/STANDARDS.md](docs/STANDARDS.md) maps every piece to
  its standard and lists the gaps, starting with this one: the cryptosuite
  `bitwrap-eip712-jcs-2026` is not a W3C suite.
- **Statements:**
  - invites and their accepts (`invites`);
  - self-revocation (`revocation`);
  - charted-by claims (`chart`);
  - discovery claims and posts (`claims`, `posts`);
  - whole-session snapshot shares, by the snapshot's CID (`snapshot_share`);
  - track shares, through generic `credential::seal`.
- **Also:**
  - salted selective disclosure;
  - CID-pinned JSON Schemas (`schema/` at the repo root);
  - Merkle batch trees and a signed root log for anchoring;
  - beats.bitwrap.io's legacy EIP-191 share signing.

No I/O, no network and no key storage. Key custody belongs to each platform (Android Keystore,
the browser's WebCrypto, the Secure Enclave) and stays out of this crate. The app either hands
the key in for one signature (`credential::sign`) or signs `Unsigned::digest()` itself and calls
`Unsigned::with_signature`.

The engine crate (`djrust`) does not depend on this one, and this one does not depend on it.

```sh
cargo test -p djrust-wallet
cargo run -p djrust-wallet --example verify -- credential.json   # check a credential offline
```

`vectors/wallet.v1.json` is the contract every binding must reproduce. Regenerate it, after a
deliberate format change only, with
`DJRUST_WALLET_REGEN=1 cargo test -p djrust-wallet --test vectors regenerate -- --ignored`.

MIT, like the rest of djrust.
