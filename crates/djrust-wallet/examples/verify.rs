//! Verifies a credential or a disclosure offline: `cargo run --example verify -- file.json [now]`.

use djrust_wallet::credential::{self, DISCLOSURE_TYPE};
use djrust_wallet::{cid, jcs, schema, Credential, Disclosure, SKEW_SEC};

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: verify <file.json> [unix-now]");
    let now: u64 = args.next().map(|s| s.parse().expect("now")).unwrap_or_else(|| {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
    });
    let doc = jcs::parse(&std::fs::read_to_string(&path).expect("read")).expect("JSON");
    if doc.get("type").and_then(|t| t.as_str()) == Some(DISCLOSURE_TYPE) {
        let d = Disclosure::from_json(&doc).expect("disclosure");
        report("disclosure", credential::verify_disclosure(&d, now, SKEW_SEC).map(|v| v.signer_did));
        for f in &d.fields {
            println!("  disclosed {} = {}", f.pointer, f.value);
        }
        return;
    }
    let c = Credential::from_json(&doc).expect("a credential");
    let name = schema::by_id(c.schema_id()).map(|s| s.name).unwrap_or("unknown schema");
    let subject = cid::of_json(c.subject()).unwrap_or_default();
    println!("credential {}\n  schema  {name}\n  subject {subject}", c.cid());
    report("credential", credential::verify(&c, now, SKEW_SEC).map(|v| v.signer_did));
}

fn report(what: &str, v: Result<String, djrust_wallet::Failure>) {
    match v {
        Ok(did) => println!("  OK: {what} issued by {did}"),
        Err(f) => {
            println!("  FAILED: {f}");
            std::process::exit(1);
        }
    }
}
