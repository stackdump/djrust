//! Bearer invite codes (`bitwrap-invite.v1`) and their accepts: built here, checked here, for
//! every implementation.

use djrust_wallet::credential::{self, Credential};
use djrust_wallet::invites::{self, SecuredIn};
use djrust_wallet::key::{self, did};
use djrust_wallet::{jcs, keccak, schema, Reason};
use serde_json::Value;

const NOW: u64 = 1_790_000_000;

fn identity(seed: &str) -> ([u8; 32], String) {
    let p = keccak(seed.as_bytes());
    (p, did::pkh(&key::address(&p).unwrap()))
}

fn issue(sch: &schema::SchemaDoc, p: &[u8; 32], subject: &Value) -> Credential {
    let issuer = did::pkh(&key::address(p).unwrap());
    let sealed = credential::seal(sch.id(), subject, &issuer, NOW, 0, &[7u8; 16]).unwrap();
    credential::sign(&sealed, p).unwrap()
}

fn sign(p: &[u8; 32], data: &Value) -> Credential {
    issue(&schema::BITWRAP_INVITE_V1, p, data)
}

fn sign_accept(p: &[u8; 32], data: &Value) -> Credential {
    issue(&schema::BITWRAP_INVITE_ACCEPT_V1, p, data)
}

#[test]
fn an_invite_checks_with_and_without_securedin() {
    let (p, me) = identity("inviter");
    let plain = invites::data(&me, &invites::new_code().unwrap(), None).unwrap();
    assert_eq!(invites::check(&sign(&p, &plain), NOW, 300).unwrap().signer_did, me);
    let at = SecuredIn { height: 42, hash: keccak(b"row 42") };
    let secured = invites::data(&me, &[1u8; 16], Some(&at)).unwrap();
    assert!(invites::check(&sign(&p, &secured), NOW, 300).is_ok());
    let (code, got) = invites::parts(&secured).unwrap();
    assert_eq!(code, "0x01010101010101010101010101010101");
    assert_eq!(got, Some(at));
    // the shape is exact: integers stay integers, keys are the schema's
    let text = jcs::canonicalize(&secured).unwrap();
    assert!(text.contains(r#""securedIn":{"hash":"0x"#) && text.contains(r#""height":42}"#), "{text}");
}

#[test]
fn codes_are_fresh() {
    assert_ne!(invites::new_code().unwrap(), invites::new_code().unwrap());
}

#[test]
fn the_inviter_must_be_the_issuer() {
    let (p, _) = identity("inviter");
    let (_, other) = identity("someone else");
    let forged = invites::data(&other, &[2u8; 16], None).unwrap();
    assert_eq!(invites::check(&sign(&p, &forged), NOW, 300).unwrap_err().reason, Reason::ClaimMismatch);
}

#[test]
fn malformed_invites_are_refused() {
    let (p, me) = identity("inviter");
    let bad = |d: Value| invites::check(&sign(&p, &d), NOW, 300).unwrap_err().reason;
    let base = invites::data(&me, &[3u8; 16], None).unwrap();
    let mut extra = base.clone();
    extra["invitee"] = Value::from("0xabc");
    assert_eq!(bad(extra), Reason::MalformedData);
    let mut short = base.clone();
    short["code"] = Value::from("0x0102");
    assert_eq!(bad(short), Reason::MalformedData);
    let mut upper = base.clone();
    upper["code"] = Value::from("0xABABABABABABABABABABABABABABABAB");
    assert_eq!(bad(upper), Reason::MalformedData);
    let mut half = base.clone();
    half["securedIn"] = serde_json::json!({"height": 1});
    assert_eq!(bad(half), Reason::MalformedData);
    // a different schema is not an invite
    let c = issue(&schema::BEATS_TRACK_V1, &p, &base);
    assert_eq!(invites::check(&c, NOW, 300).unwrap_err().reason, Reason::WrongSchema);
}

#[test]
fn an_invite_fits_a_qr_code() {
    let (p, me) = identity("inviter");
    let d = invites::data(&me, &[4u8; 16], Some(&SecuredIn { height: 9_007_199_254_740_991, hash: [0xff; 32] })).unwrap();
    let n = jcs::bytes(&sign(&p, &d).to_json()).unwrap().len();
    assert!(n <= 1536, "{n} bytes");
}

/// An invite by `inviter`, and the accept `member` signs for it.
fn pair(inviter: &str, member: &str) -> (Credential, Credential, String) {
    let (ip, idid) = identity(inviter);
    let (mp, mdid) = identity(member);
    let inv = sign(&ip, &invites::data(&idid, &[5u8; 16], None).unwrap());
    let acc = sign_accept(&mp, &invites::accept_data(&inv.cid(), &idid, &mdid).unwrap());
    (inv, acc, mdid)
}

#[test]
fn an_accept_checks_with_its_invite() {
    let (inv, acc, member) = pair("inviter", "newcomer");
    assert_eq!(invites::check_accept(&acc, &inv, NOW, 300).unwrap().signer_did, member);
}

#[test]
fn an_accept_is_bound_to_its_invite() {
    let (inv, acc, _) = pair("inviter", "newcomer");
    // the same accept against another invite by the same inviter
    let (ip, idid) = identity("inviter");
    let other = sign(&ip, &invites::data(&idid, &[6u8; 16], None).unwrap());
    assert_eq!(invites::check_accept(&acc, &other, NOW, 300).unwrap_err().reason, Reason::ClaimMismatch);
    // a forged member field
    let (_, someone) = identity("someone else");
    let mut forged = acc.subject().clone();
    forged["member"] = Value::from(someone);
    let (mp, _) = identity("newcomer");
    assert_eq!(invites::check_accept(&sign_accept(&mp, &forged), &inv, NOW, 300).unwrap_err().reason, Reason::ClaimMismatch);
}

#[test]
fn no_self_referral() {
    let (_, me) = identity("inviter");
    assert!(invites::accept_data("z4EBG9j5XDDYDUM2AnJ5z6c7EKWDpxZvJ4qTCoDbGr7ynjdDqXu", &me, &me).is_err());
    // even signed by hand, the check refuses it
    let (ip, idid) = identity("inviter");
    let inv = sign(&ip, &invites::data(&idid, &[7u8; 16], None).unwrap());
    let acc = serde_json::json!({"@type": "BitwrapInviteAccept", "v": 1, "invite": inv.cid(), "inviter": idid, "member": idid});
    assert_eq!(invites::check_accept(&sign_accept(&ip, &acc), &inv, NOW, 300).unwrap_err().reason, Reason::ClaimMismatch);
}

#[test]
fn a_tampered_invite_fails_the_accept() {
    let (inv, acc, _) = pair("inviter", "newcomer");
    let mut t = inv.to_json();
    t["credentialSubject"]["code"] = Value::from("0x00000000000000000000000000000000"); // no longer what was signed
    let tampered = Credential::from_json(&t).unwrap();
    assert!(invites::check_accept(&acc, &tampered, NOW, 300).is_err());
}
