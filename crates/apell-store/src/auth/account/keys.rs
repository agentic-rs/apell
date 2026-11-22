use super::{invalid_identity, read_json, unavailable, visible_ascii, CACHE_LIFETIME};
use crate::Result;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use jsonwebtoken::{
  jwk::{
    AlgorithmParameters, EllipticCurve, Jwk, JwkSet, KeyAlgorithm, KeyOperations, PublicKeyUse,
  },
  Algorithm, DecodingKey,
};
use reqwest::Client;
use std::{
  collections::HashMap,
  time::{Duration, Instant},
};
use url::Url;

const REFRESH_COOLDOWN: Duration = Duration::from_secs(10);

#[derive(Default)]
pub(in crate::auth) struct Cache {
  endpoint: Option<Url>,
  keys: HashMap<String, (Algorithm, DecodingKey)>,
  fetched_at: Option<Instant>,
  last_attempt: Option<Instant>,
}

impl Cache {
  pub(in crate::auth) async fn get(
    &mut self,
    client: &Client,
    endpoint: &Url,
    kid: &str,
    algorithm: Algorithm,
  ) -> Result<DecodingKey> {
    if self.endpoint.as_ref() != Some(endpoint) {
      *self = Self {
        endpoint: Some(endpoint.clone()),
        ..Self::default()
      };
    }
    if self
      .fetched_at
      .is_some_and(|time| time.elapsed() < CACHE_LIFETIME)
    {
      if let Some(key) = self.lookup(kid, algorithm) {
        return Ok(key);
      }
    }
    if self
      .last_attempt
      .is_some_and(|time| time.elapsed() < REFRESH_COOLDOWN)
    {
      return Err(invalid_identity());
    }
    self.last_attempt = Some(Instant::now());
    let jwks: JwkSet = read_json(client.get(endpoint.clone())).await?;
    self.keys = signing_keys(jwks)?;
    self.fetched_at = Some(Instant::now());
    self.lookup(kid, algorithm).ok_or_else(invalid_identity)
  }

  fn lookup(&self, kid: &str, algorithm: Algorithm) -> Option<DecodingKey> {
    self
      .keys
      .get(kid)
      .filter(|(key_algorithm, _)| *key_algorithm == algorithm)
      .map(|(_, key)| key.clone())
  }
}

fn signing_keys(jwks: JwkSet) -> Result<HashMap<String, (Algorithm, DecodingKey)>> {
  if jwks.keys.is_empty() || jwks.keys.len() > 16 {
    return Err(unavailable());
  }
  let mut keys = HashMap::new();
  for jwk in jwks.keys {
    let Some(algorithm) = signing_algorithm(&jwk) else {
      continue;
    };
    let key = DecodingKey::from_jwk(&jwk).map_err(|_| unavailable())?;
    let kid = jwk
      .common
      .key_id
      .filter(|value| visible_ascii(value, 128))
      .ok_or_else(unavailable)?;
    if keys.insert(kid, (algorithm, key)).is_some() {
      return Err(unavailable());
    }
  }
  if keys.is_empty() {
    return Err(unavailable());
  }
  Ok(keys)
}

fn signing_algorithm(jwk: &Jwk) -> Option<Algorithm> {
  if jwk
    .common
    .public_key_use
    .as_ref()
    .is_some_and(|usage| *usage != PublicKeyUse::Signature)
    || jwk
      .common
      .key_operations
      .as_ref()
      .is_some_and(|operations| !operations.contains(&KeyOperations::Verify))
  {
    return None;
  }
  let (algorithm, key_algorithm) = match &jwk.algorithm {
    AlgorithmParameters::RSA(rsa) if rsa.e == "AQAB" => {
      let modulus = URL_SAFE_NO_PAD.decode(&rsa.n).ok()?;
      if !(256..=512).contains(&modulus.len()) || modulus[0] < 128 {
        return None;
      }
      (Algorithm::RS256, KeyAlgorithm::RS256)
    }
    AlgorithmParameters::EllipticCurve(ec) => match ec.curve {
      EllipticCurve::P256 if ec.x.len() == 43 && ec.y.len() == 43 => {
        (Algorithm::ES256, KeyAlgorithm::ES256)
      }
      EllipticCurve::P384 if ec.x.len() == 64 && ec.y.len() == 64 => {
        (Algorithm::ES384, KeyAlgorithm::ES384)
      }
      _ => return None,
    },
    _ => return None,
  };
  (jwk.common.key_algorithm == Some(key_algorithm)).then_some(algorithm)
}

#[cfg(test)]
mod tests {
  use super::*;
  use jsonwebtoken::{decode, encode, EncodingKey, Header, Validation};
  use serde_json::json;

  #[test]
  fn elliptic_keys_bind_the_header_algorithm_to_the_signing_curve() {
    // PKCS#8 wraps a SEC1 key with the id-ecPublicKey and secp384r1 OIDs.
    // Generate a fresh 48-byte scalar in range rather than checking in a key.
    let mut der = vec![
      0x30, 0x4e, 0x02, 0x01, 0x00, 0x30, 0x10, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02,
      0x01, 0x06, 0x05, 0x2b, 0x81, 0x04, 0x00, 0x22, 0x04, 0x37, 0x30, 0x35, 0x02, 0x01, 0x01,
      0x04, 0x30,
    ];
    let mut scalar = [0; 48];
    getrandom::fill(&mut scalar).unwrap();
    scalar[0] = 1;
    der.extend_from_slice(&scalar);
    let signer = EncodingKey::from_ec_der(&der);
    let mut jwk = Jwk::from_encoding_key(&signer, Algorithm::ES384).unwrap();
    jwk.common.key_id = Some("ec-key".into());
    jwk.common.public_key_use = Some(PublicKeyUse::Signature);
    let keys = signing_keys(JwkSet {
      keys: vec![jwk.clone()],
    })
    .unwrap();
    let cache = Cache {
      keys,
      ..Cache::default()
    };
    let verifier = cache.lookup("ec-key", Algorithm::ES384).unwrap();
    let mut header = Header::new(Algorithm::ES384);
    header.kid = Some("ec-key".into());
    let claims = json!({ "sub": "fixture-user", "exp": super::super::now() + 300 });
    let jwt = encode(&header, &claims, &signer).unwrap();
    let signed =
      decode::<serde_json::Value>(&jwt, &verifier, &Validation::new(Algorithm::ES384)).unwrap();
    assert_eq!(signed.claims, claims);
    assert!(cache.lookup("ec-key", Algorithm::ES256).is_none());
    assert!(cache.lookup("ec-key", Algorithm::RS256).is_none());
    for algorithm in [Some(KeyAlgorithm::ES256), None] {
      jwk.common.key_algorithm = algorithm;
      assert!(signing_keys(JwkSet {
        keys: vec![jwk.clone()]
      })
      .is_err());
    }
  }
}
