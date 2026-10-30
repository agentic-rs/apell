use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct Conversation {
  pub conversation_id: String,
  pub title: String,
  pub updated_at: String,
}

#[derive(Serialize, Deserialize)]
pub struct Message {
  pub message_id: String,
  pub role: String,
  pub content: String,
  pub status: String,
}

pub fn title_from_message(content: &str) -> String {
  let single_line = content.split_whitespace().collect::<Vec<_>>().join(" ");
  let mut title: String = single_line.chars().take(52).collect();
  if single_line.chars().count() > 52 {
    title.push('…');
  }
  title
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn titles_are_single_line_and_do_not_split_unicode() {
    assert_eq!(title_from_message("hello\n world"), "hello world");
    assert_eq!(
      title_from_message(&"読".repeat(53)),
      format!("{}…", "読".repeat(52))
    );
  }
}
