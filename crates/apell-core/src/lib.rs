pub mod bundles;
mod http;
pub mod llm;
pub mod resources;
pub mod sqlite;

pub fn valid_name(value: &str) -> bool {
  !value.is_empty()
    && value.len() <= 80
    && value
      .bytes()
      .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'_')
}
