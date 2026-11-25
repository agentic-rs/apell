use super::*;
use crate::key_relay::{database as legacy, Envelope, Vault};

const VAULT: &str = "8eff66e5-d2b4-4eb4-a518-ffac33278bb8";
const PAIR: &str = "a3068f26-2a3a-41dd-8c8a-6f7af0b8912c";

fn owner(subject: &str) -> Owner {
  Owner {
    issuer: "https://account.example.invalid/oidc".into(),
    subject: subject.into(),
  }
}

fn approval(key: &Key, summary: &PairSummary) -> Key {
  let proof_key = derive_pairing_proof_key(key, VAULT);
  let sign = hmac::Key::new(hmac::HMAC_SHA256, proof_key.as_ref());
  let message = format!(
    "apell-key-relay-v2\napprove\n{VAULT}\n{}\n{}",
    summary.pair_id, summary.approval_nonce
  );
  Zeroizing::new(
    hmac::sign(&sign, message.as_bytes())
      .as_ref()
      .try_into()
      .unwrap(),
  )
}

#[test]
fn managed_pairing_accepts_app_proof_vectors() {
  let vault_id = "00112233-4455-4677-8899-aabbccddeeff";
  let root = Zeroizing::new(std::array::from_fn(|index| index as u8));
  let list = decode_key("lwFvvozDsCpFztU4CVt25Ti5AoU67HP07aio1Mqkx7k").unwrap();
  verify_proof(&root, list.as_ref(), "list", vault_id, "", None).unwrap();

  let approval = decode_key("GqsUeUMDshZ7kH_rIE5f0gFqCWyPTXiSmhOIuLa2PtQ").unwrap();
  let nonce = URL_SAFE_NO_PAD.encode([0; 16]);
  verify_proof(
    &root,
    approval.as_ref(),
    "approve",
    vault_id,
    PAIR,
    Some(&nonce),
  )
  .unwrap();
  assert!(verify_proof(
    &root,
    approval.as_ref(),
    "approve",
    VAULT,
    PAIR,
    Some(&nonce)
  )
  .is_err());
}

#[test]
fn enrollment_keeps_the_original_vault_and_requires_the_migration_path() {
  let directory = tempfile::tempdir().unwrap();
  let mut db = legacy::open(directory.path()).unwrap();
  let owner = owner("fixture-subject");
  let master = Zeroizing::new([1; 32]);
  let key = Zeroizing::new([2; 32]);
  let old = Vault {
    vault_id: VAULT.into(),
    recovery: Envelope {
      nonce: URL_SAFE_NO_PAD.encode([4; 12]),
      ciphertext: URL_SAFE_NO_PAD.encode([5; 48]),
    },
  };
  legacy::create_vault(&mut db, &owner, &old).unwrap();
  assert_eq!(
    database::status(&db, &owner).unwrap().vault_id.as_deref(),
    Some(VAULT)
  );
  assert!(!database::status(&db, &owner).unwrap().managed);
  assert!(database::create(&mut db, &master, &owner, VAULT, &key, false).is_err());
  database::create(&mut db, &master, &owner, VAULT, &key, true).unwrap();
  database::create(&mut db, &master, &owner, VAULT, &key, true).unwrap();
  assert!(database::status(&db, &owner).unwrap().managed);
  assert!(legacy::vault(&db, &owner)
    .unwrap()
    .is_some_and(|saved| saved == old));
  assert_eq!(
    &*database::load_key(&db, &master, &owner, VAULT).unwrap(),
    &*key
  );
  assert!(database::load_key(&db, &Zeroizing::new([7; 32]), &owner, VAULT).is_err());
  let stored: Vec<u8> = db
    .query_row("SELECT key_ciphertext FROM managed_key_vaults", [], |row| {
      row.get(0)
    })
    .unwrap();
  assert_eq!(stored.len(), 48);
  assert!(!stored
    .windows(key.len())
    .any(|window| window == key.as_ref()));
}

#[test]
fn pairing_needs_both_secrets_and_does_not_replay_old_approval() {
  let directory = tempfile::tempdir().unwrap();
  let mut db = legacy::open(directory.path()).unwrap();
  let account = owner("fixture-account");
  let other = owner("other-fixture-account");
  let master = Zeroizing::new([6; 32]);
  let key = Zeroizing::new([7; 32]);
  let request_digest = [8; 32];

  database::create(&mut db, &master, &account, VAULT, &key, false).unwrap();
  let first = database::start_pair(&mut db, &account, VAULT, PAIR, &request_digest, 1000).unwrap();
  let retry = database::start_pair(&mut db, &account, VAULT, PAIR, &request_digest, 1050).unwrap();
  assert_eq!(first.code, retry.code);
  assert_eq!(first.approval_nonce, retry.approval_nonce);
  assert_eq!(first.expires_at, 1300);
  assert_eq!(first.expires_at, retry.expires_at);
  assert!(database::start_pair(&mut db, &account, VAULT, PAIR, &[0; 32], 1050).is_err());
  assert!(
    database::finish_pair(&db, &master, &account, PAIR, &request_digest, 1050)
      .unwrap()
      .is_none()
  );

  let proof = approval(&key, &first);
  assert!(database::approve_pair(&mut db, &master, &other, VAULT, PAIR, &proof, 1050).is_err());
  assert!(database::approve_pair(
    &mut db,
    &master,
    &account,
    VAULT,
    PAIR,
    &Zeroizing::new([0; 32]),
    1050,
  )
  .is_err());
  database::approve_pair(&mut db, &master, &account, VAULT, PAIR, &proof, 1050).unwrap();
  assert!(database::finish_pair(&db, &master, &account, PAIR, &[0; 32], 1050).is_err());
  assert_eq!(
    &*database::finish_pair(&db, &master, &account, PAIR, &request_digest, 1050)
      .unwrap()
      .unwrap(),
    &*key
  );
  assert!(database::consume_pair(&mut db, &account, PAIR, &[0; 32], 1050).is_err());
  database::consume_pair(&mut db, &account, PAIR, &request_digest, 1050).unwrap();
  database::consume_pair(&mut db, &account, PAIR, &request_digest, 1050).unwrap();
  assert!(database::finish_pair(&db, &master, &account, PAIR, &request_digest, 1050).is_err());
  assert!(database::start_pair(&mut db, &account, VAULT, PAIR, &request_digest, 1050).is_err());

  let replacement =
    database::start_pair(&mut db, &account, VAULT, PAIR, &request_digest, 1300).unwrap();
  assert_ne!(replacement.approval_nonce, first.approval_nonce);
  assert!(database::approve_pair(&mut db, &master, &account, VAULT, PAIR, &proof, 1300).is_err());
}
