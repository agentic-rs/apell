use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, CHACHA20_POLY1305};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use zeroize::Zeroizing;

pub(super) type Secret = Zeroizing<[u8; 32]>;
pub(super) const MAX_API_KEY: usize = 4096;

const CANNOT_OPEN: &str =
  "Could not unlock the encrypted data. Check the code or try another device.";
#[cfg(test)]
const RECOVERY_PREFIX: &str = "apell-recovery-v1.";
#[cfg(test)]
const PAIR_PREFIX: &str = "apell-pair-v1.";

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Sealed {
  pub nonce: String,
  pub ciphertext: String,
}

#[cfg(test)]
pub(super) struct PairCode {
  pub id: Uuid,
  pub secret: Secret,
}

#[cfg(test)]
impl PairCode {
  pub fn new() -> Result<Self, String> {
    Ok(Self {
      id: Uuid::new_v4(),
      secret: random()?,
    })
  }

  pub fn encode(&self) -> String {
    format!(
      "{PAIR_PREFIX}{}.{}",
      self.id,
      URL_SAFE_NO_PAD.encode(self.secret.as_ref())
    )
  }

  pub fn decode(value: &str) -> Result<Self, String> {
    let (id, secret) = value
      .trim()
      .strip_prefix(PAIR_PREFIX)
      .and_then(|value| value.split_once('.'))
      .ok_or("Enter the pairing code shown on the new device.")?;
    let id = Uuid::parse_str(id)
      .ok()
      .filter(|parsed| parsed.get_version_num() == 4 && parsed.to_string() == id)
      .ok_or("Enter the pairing code shown on the new device.")?;
    Ok(Self {
      id,
      secret: decode_key(secret).ok_or("Enter the pairing code shown on the new device.")?,
    })
  }
}

pub(super) fn random() -> Result<Secret, String> {
  let mut key = Zeroizing::new([0; 32]);
  getrandom::fill(key.as_mut()).map_err(|_| "Could not generate a secure encryption key.")?;
  Ok(key)
}

#[cfg(test)]
pub(super) fn recovery_code(key: &[u8; 32]) -> String {
  format!("{RECOVERY_PREFIX}{}", URL_SAFE_NO_PAD.encode(key))
}

#[cfg(test)]
pub(super) fn recovery_key(value: &str) -> Result<Secret, String> {
  value
    .trim()
    .strip_prefix(RECOVERY_PREFIX)
    .and_then(decode_key)
    .ok_or_else(|| "Enter the saved Apell recovery key.".into())
}

pub(super) fn decode_key(value: &str) -> Option<Secret> {
  if value.len() != 43 {
    return None;
  }
  let mut bytes = Zeroizing::new([0; 32]);
  let count = URL_SAFE_NO_PAD.decode_slice(value, bytes.as_mut()).ok()?;
  (count == bytes.len()).then_some(bytes)
}

pub(super) fn context(
  purpose: &str,
  account: &str,
  vault_id: Uuid,
  destination: &[&str],
) -> Result<Vec<u8>, String> {
  serde_json::to_vec(&("apell-key-sync-v1", purpose, account, vault_id, destination))
    .map_err(|_| "Could not encode the encrypted record context.".into())
}

fn cipher(secret: &[u8; 32]) -> LessSafeKey {
  LessSafeKey::new(UnboundKey::new(&CHACHA20_POLY1305, secret).expect("a 32-byte ChaCha20 key"))
}

pub(super) fn seal(secret: &[u8; 32], context: &[u8], value: &[u8]) -> Result<Sealed, String> {
  let mut nonce = [0; 12];
  getrandom::fill(&mut nonce).map_err(|_| "Could not generate encryption randomness.")?;
  let mut payload = Zeroizing::new(value.to_vec());
  cipher(secret)
    .seal_in_place_append_tag(
      Nonce::assume_unique_for_key(nonce),
      Aad::from(context),
      &mut *payload,
    )
    .map_err(|_| "Could not encrypt this record.")?;
  Ok(Sealed {
    nonce: URL_SAFE_NO_PAD.encode(nonce),
    ciphertext: URL_SAFE_NO_PAD.encode(payload.as_slice()),
  })
}

pub(super) fn open(
  secret: &[u8; 32],
  context: &[u8],
  record: &Sealed,
  max: usize,
) -> Result<Zeroizing<Vec<u8>>, String> {
  if record.nonce.len() != 16 || record.ciphertext.len() > (max + 16).div_ceil(3) * 4 {
    return Err(CANNOT_OPEN.into());
  }
  let nonce: [u8; 12] = URL_SAFE_NO_PAD
    .decode(&record.nonce)
    .ok()
    .and_then(|nonce| nonce.try_into().ok())
    .ok_or(CANNOT_OPEN)?;
  let mut plaintext = Zeroizing::new(
    URL_SAFE_NO_PAD
      .decode(&record.ciphertext)
      .map_err(|_| CANNOT_OPEN)?,
  );
  let size = cipher(secret)
    .open_in_place(
      Nonce::assume_unique_for_key(nonce),
      Aad::from(context),
      &mut plaintext,
    )
    .map_err(|_| CANNOT_OPEN)?
    .len();
  if size == 0 || size > max {
    return Err(CANNOT_OPEN.into());
  }
  plaintext.truncate(size);
  Ok(plaintext)
}

#[cfg(test)]
pub(super) fn open_vault(
  secret: &Secret,
  context: &[u8],
  record: &Sealed,
) -> Result<Secret, String> {
  let plaintext = open(secret, context, record, 32)?;
  let mut key = Zeroizing::new([0; 32]);
  if plaintext.len() != key.len() {
    return Err(CANNOT_OPEN.into());
  }
  key.copy_from_slice(&plaintext);
  Ok(key)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn vault_codes_round_trip_and_cannot_be_substituted() {
    let vault = random().unwrap();
    let recovery = random().unwrap();
    let code = recovery_code(&recovery);
    assert_eq!(*recovery_key(&code).unwrap(), *recovery);
    let id = Uuid::new_v4();
    let recovery_context = context("recovery", "account-a", id, &[]).unwrap();
    let envelope = seal(&recovery, &recovery_context, vault.as_ref()).unwrap();
    assert_eq!(
      *open_vault(&recovery, &recovery_context, &envelope).unwrap(),
      *vault
    );
    assert!(open_vault(&random().unwrap(), &recovery_context, &envelope).is_err());

    let pair = PairCode::new().unwrap();
    let decoded = PairCode::decode(&pair.encode()).unwrap();
    assert_eq!(pair.id, decoded.id);
    assert_eq!(*pair.secret, *decoded.secret);
    let pair_context = context("pair", "account-a", id, &[&pair.id.to_string()]).unwrap();
    assert!(open_vault(&pair.secret, &pair_context, &envelope).is_err());
  }

  #[test]
  fn encrypted_api_key_is_bound_to_its_account_and_endpoint() {
    let key = random().unwrap();
    let id = Uuid::new_v4();
    let destination = [
      "connection-a",
      "provider-a",
      "https://provider.example.invalid/v1",
    ];
    let aad = context("api-key", "account-a", id, &destination).unwrap();
    let sealed = seal(&key, &aad, b"fixture-api-key").unwrap();
    assert_eq!(
      open(&key, &aad, &sealed, MAX_API_KEY).unwrap().as_slice(),
      b"fixture-api-key"
    );
    let another_account = context("api-key", "account-b", id, &destination).unwrap();
    assert!(open(&key, &another_account, &sealed, MAX_API_KEY).is_err());
    let another_destination = context(
      "api-key",
      "account-a",
      id,
      &[
        "connection-a",
        "provider-a",
        "https://other.example.invalid/v1",
      ],
    )
    .unwrap();
    assert!(open(&key, &another_destination, &sealed, MAX_API_KEY).is_err());
  }
}
