//! Purpose-specific encryption and authentication keys for an Apell vault.

use ring::hkdf;
use zeroize::Zeroizing;

const SALT: &[u8] = b"apell-vault-hkdf-sha256-v1";

/// Derive the API-key encryption key. The vault ID is a canonical UUID string.
pub fn derive_api_key_encryption_key(vault_root: &[u8; 32], vault_id: &str) -> Zeroizing<[u8; 32]> {
  derive(vault_root, vault_id, b"api-key-encryption")
}

/// Derive the key shared by the device and Account relay for pairing proofs.
pub fn derive_pairing_proof_key(vault_root: &[u8; 32], vault_id: &str) -> Zeroizing<[u8; 32]> {
  derive(vault_root, vault_id, b"pairing-proof")
}

/// Derive the key for comparing saved credentials in the sync ledger.
pub fn derive_sync_fingerprint_key(vault_root: &[u8; 32], vault_id: &str) -> Zeroizing<[u8; 32]> {
  derive(vault_root, vault_id, b"sync-fingerprint")
}

fn derive(vault_root: &[u8; 32], vault_id: &str, purpose: &[u8]) -> Zeroizing<[u8; 32]> {
  let root = hkdf::Salt::new(hkdf::HKDF_SHA256, SALT).extract(vault_root);
  let info = [purpose, b"\0", vault_id.as_bytes()];
  let mut key = Zeroizing::new([0; 32]);
  root
    .expand(&info, hkdf::HKDF_SHA256)
    .expect("SHA-256 HKDF accepts a 32-byte output")
    .fill(key.as_mut())
    .expect("the output buffer has the HKDF key length");
  key
}

#[cfg(test)]
mod tests {
  use super::*;

  const VAULT: &str = "00112233-4455-4677-8899-aabbccddeeff";

  #[test]
  fn derives_three_distinct_keys_from_the_vault_root() {
    let root = std::array::from_fn(|index| index as u8);
    // Fixed outputs independently generated with Node's HKDF implementation.
    let vectors = [
      (
        derive_api_key_encryption_key(&root, VAULT),
        "cfe2015f9db319aaa6ddcee137a1e60fd725b8971463734f5a83ddc246d2544a",
      ),
      (
        derive_pairing_proof_key(&root, VAULT),
        "9152bf6ca4912d61459efce75e35cf0498d8b71b75e7c76d0491b8b2da40cda3",
      ),
      (
        derive_sync_fingerprint_key(&root, VAULT),
        "af219745977366b915dcc462e84f3f90f11b108891a2a80492059756d2763732",
      ),
    ];
    for (derived, expected) in vectors {
      let hex: String = derived.iter().map(|byte| format!("{byte:02x}")).collect();
      assert_eq!(hex, expected);
    }
  }

  #[test]
  fn binds_derived_keys_to_the_vault_id() {
    let root = [35; 32];
    let other_vault = "00112233-4455-4677-8899-aabbccddeefe";
    assert_ne!(
      *derive_pairing_proof_key(&root, VAULT),
      *derive_pairing_proof_key(&root, other_vault)
    );
    assert_ne!(
      *derive_api_key_encryption_key(&root, VAULT),
      *derive_api_key_encryption_key(&[36; 32], VAULT)
    );
  }
}
