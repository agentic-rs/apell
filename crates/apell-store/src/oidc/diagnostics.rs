//! Opt-in operator diagnostics contain only fixed categories, never request values.
use super::unauthorized;
use crate::StoreError;
use jsonwebtoken::errors::ErrorKind;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Rejection {
  Authorization,
  HeaderMalformed,
  HeaderAlgorithm,
  HeaderType,
  HeaderKeySource,
  HeaderCritical,
  HeaderEncryption,
  HeaderUnknownFields,
  HeaderKeyId,
  KeyUnknown,
  KeyUnavailable,
  TokenMalformed,
  TokenSignature,
  TokenExpired,
  TokenNotYetValid,
  TokenIssuer,
  TokenAudience,
  TokenMissingClaim,
  TokenClaimFormat,
  TokenAlgorithm,
  TokenInvalid,
  ClaimIssuer,
  ClaimAudience,
  ClaimSubject,
  ClaimTokenId,
  ClaimExpired,
  ClaimLifetime,
  ClaimIssuedInFuture,
  ClaimNotYetValid,
  ClaimTimeOrder,
  ClaimRunner,
  ClaimReusableWorkflow,
  ClaimEvent,
  ClaimRef,
  ClaimRepository,
  ClaimRepositoryId,
  ClaimOwnerId,
  PolicyApplet,
  PolicyRepositoryId,
  PolicyOwnerId,
  PolicyWorkflow,
  PolicyRef,
  PolicyEvent,
  PolicyEnvironment,
  CredentialExpired,
  TokenReplayed,
  CredentialCapacity,
  CredentialUnknown,
  PackageChecksum,
  PackageIdentity,
}

impl Rejection {
  pub fn code(self) -> &'static str {
    match self {
      Self::Authorization => "authorization",
      Self::HeaderMalformed => "header_malformed",
      Self::HeaderAlgorithm => "header_algorithm",
      Self::HeaderType => "header_type",
      Self::HeaderKeySource => "header_key_source",
      Self::HeaderCritical => "header_critical",
      Self::HeaderEncryption => "header_encryption",
      Self::HeaderUnknownFields => "header_unknown_fields",
      Self::HeaderKeyId => "header_key_id",
      Self::KeyUnknown => "key_unknown",
      Self::KeyUnavailable => "key_unavailable",
      Self::TokenMalformed => "token_malformed",
      Self::TokenSignature => "token_signature",
      Self::TokenExpired => "token_expired",
      Self::TokenNotYetValid => "token_not_yet_valid",
      Self::TokenIssuer => "token_issuer",
      Self::TokenAudience => "token_audience",
      Self::TokenMissingClaim => "token_missing_claim",
      Self::TokenClaimFormat => "token_claim_format",
      Self::TokenAlgorithm => "token_algorithm",
      Self::TokenInvalid => "token_invalid",
      Self::ClaimIssuer => "claim_issuer",
      Self::ClaimAudience => "claim_audience",
      Self::ClaimSubject => "claim_subject",
      Self::ClaimTokenId => "claim_token_id",
      Self::ClaimExpired => "claim_expired",
      Self::ClaimLifetime => "claim_lifetime",
      Self::ClaimIssuedInFuture => "claim_issued_in_future",
      Self::ClaimNotYetValid => "claim_not_yet_valid",
      Self::ClaimTimeOrder => "claim_time_order",
      Self::ClaimRunner => "claim_runner",
      Self::ClaimReusableWorkflow => "claim_reusable_workflow",
      Self::ClaimEvent => "claim_event",
      Self::ClaimRef => "claim_ref",
      Self::ClaimRepository => "claim_repository",
      Self::ClaimRepositoryId => "claim_repository_id",
      Self::ClaimOwnerId => "claim_owner_id",
      Self::PolicyApplet => "policy_applet",
      Self::PolicyRepositoryId => "policy_repository_id",
      Self::PolicyOwnerId => "policy_owner_id",
      Self::PolicyWorkflow => "policy_workflow",
      Self::PolicyRef => "policy_ref",
      Self::PolicyEvent => "policy_event",
      Self::PolicyEnvironment => "policy_environment",
      Self::CredentialExpired => "credential_expired",
      Self::TokenReplayed => "token_replayed",
      Self::CredentialCapacity => "credential_capacity",
      Self::CredentialUnknown => "credential_unknown",
      Self::PackageChecksum => "package_checksum",
      Self::PackageIdentity => "package_identity",
    }
  }
}

pub(super) fn emit(reason: Rejection) {
  if std::env::var_os("APELL_STORE_OIDC_DIAGNOSTICS").is_some_and(|value| value == "1") {
    eprintln!("OIDC publishing rejected: {}", reason.code());
  }
}

pub(super) fn reject(reason: Rejection) -> StoreError {
  emit(reason);
  unauthorized()
}

pub(super) fn decode_reason(error: &ErrorKind) -> Rejection {
  match error {
    ErrorKind::InvalidToken | ErrorKind::Base64(_) | ErrorKind::Utf8(_) => {
      Rejection::TokenMalformed
    }
    ErrorKind::InvalidSignature => Rejection::TokenSignature,
    ErrorKind::ExpiredSignature => Rejection::TokenExpired,
    ErrorKind::ImmatureSignature => Rejection::TokenNotYetValid,
    ErrorKind::InvalidIssuer => Rejection::TokenIssuer,
    ErrorKind::InvalidAudience => Rejection::TokenAudience,
    ErrorKind::MissingRequiredClaim(_) => Rejection::TokenMissingClaim,
    ErrorKind::Json(_) | ErrorKind::InvalidClaimFormat(_) => Rejection::TokenClaimFormat,
    ErrorKind::InvalidAlgorithm | ErrorKind::InvalidAlgorithmName | ErrorKind::MissingAlgorithm => {
      Rejection::TokenAlgorithm
    }
    _ => Rejection::TokenInvalid,
  }
}
