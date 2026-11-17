use serde_json::Value;

pub(crate) struct RateLimit {
  pub message: String,
  pub retryable: bool,
}

pub(crate) fn is_zai(provider_id: &str) -> bool {
  matches!(provider_id, "zai" | "zai-coding-plan")
}

// The provider message can contain private context. Display only known codes
// and fixed explanations from https://docs.z.ai/api-reference/api-code.
pub(crate) fn rate_limit(provider_id: &str, response: &Value) -> Option<RateLimit> {
  if !is_zai(provider_id) {
    return None;
  }
  let code = &response["error"]["code"];
  let code = code.as_u64().or_else(|| code.as_str()?.parse().ok())?;
  let (message, retryable) = match code {
    1113 if provider_id == "zai-coding-plan" => (
      "Z.ai reports insufficient balance or no applicable plan. Check the key type and plan status in the Z.ai console.",
      false,
    ),
    1113 => (
      "Z.ai reports insufficient balance or no applicable API usage package. Check billing in the Z.ai console.",
      false,
    ),
    1302 => ("Z.ai's request rate limit was reached. Wait briefly and try again.", true),
    1305 => ("Z.ai is temporarily overloaded. Try again later.", true),
    1308 | 1310 => (
      "Z.ai Coding Plan quota is exhausted. Check the reset time in your Z.ai subscription.",
      false,
    ),
    1309 => ("Your Z.ai Coding Plan has expired. Check your subscription.", false),
    1311 => ("Your Z.ai plan does not include this model. Select a model your plan supports.", false),
    1313 => (
      "Z.ai limited this account under its Fair Usage Policy. Check your account or contact Z.ai support.",
      false,
    ),
    1314 => ("Your Z.ai enterprise plan has expired. Contact your administrator.", false),
    1315 => (
      "Z.ai restricted this key to enterprise coding use. Check the key type in the Z.ai console.",
      false,
    ),
    1316 | 1317 => (
      "Z.ai Coding Plan quota is exhausted and extra usage has insufficient balance. Check billing and the quota reset time.",
      false,
    ),
    1318..=1321 => (
      "Z.ai Coding Plan quota is exhausted and extra usage reached the monthly spending limit. Check billing and the quota reset time.",
      false,
    ),
    _ => return None,
  };
  Some(RateLimit {
    message: format!("{message} (429 / {code})"),
    retryable,
  })
}
