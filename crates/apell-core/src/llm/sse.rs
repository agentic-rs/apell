use super::{Failure, MAX_OUTPUT_BYTES};

#[derive(Default)]
pub(super) struct SseParser {
  line: Vec<u8>,
  data: Vec<String>,
  data_bytes: usize,
  previous_cr: bool,
}

impl SseParser {
  pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>, Failure> {
    let mut events = Vec::new();
    for &byte in bytes {
      if self.previous_cr {
        self.previous_cr = false;
        if byte == b'\n' {
          continue;
        }
      }
      if matches!(byte, b'\r' | b'\n') {
        self.process_line(&mut events)?;
        self.previous_cr = byte == b'\r';
      } else {
        if self.line.len() >= MAX_OUTPUT_BYTES {
          return Err(Failure::new(
            "response_limit",
            "The LLM stream event exceeded the size limit.",
          ));
        }
        self.line.push(byte);
      }
    }
    Ok(events)
  }

  fn process_line(&mut self, events: &mut Vec<String>) -> Result<(), Failure> {
    if self.line.is_empty() {
      if !self.data.is_empty() {
        events.push(self.data.join("\n"));
        self.data.clear();
        self.data_bytes = 0;
      }
      return Ok(());
    }
    let line = std::str::from_utf8(&self.line)
      .map_err(|_| Failure::new("invalid_response", "The LLM stream contains invalid UTF-8."))?;
    let (field, value) = line.split_once(':').unwrap_or((line, ""));
    if field == "data" {
      let value = value.strip_prefix(' ').unwrap_or(value);
      self.data_bytes = self.data_bytes.saturating_add(value.len() + 1);
      if self.data_bytes > MAX_OUTPUT_BYTES {
        return Err(Failure::new(
          "response_limit",
          "The LLM stream event exceeded the size limit.",
        ));
      }
      self.data.push(value.into());
    }
    self.line.clear();
    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn every_unicode_chunk_boundary_crlf_and_multiline() {
    let input = ":keepalive\r\ndata: 你好 🌍\r\ndata: next\r\n\r\ndata: [DONE]\r\n\r\n";
    for boundary in 0..=input.len() {
      let mut parser = SseParser::default();
      let mut events = parser.push(&input.as_bytes()[..boundary]).unwrap();
      events.extend(parser.push(&input.as_bytes()[boundary..]).unwrap());
      assert_eq!(events, ["你好 🌍\nnext", "[DONE]"]);
    }
    let mut parser = SseParser::default();
    assert_eq!(
      parser.push(b"data: one\r\rdata: two\r\r").unwrap(),
      ["one", "two"]
    );
    assert!(parser.push(b"data: incomplete").unwrap().is_empty());
    assert!(SseParser::default()
      .push(&vec![b'x'; MAX_OUTPUT_BYTES + 1])
      .is_err());
  }
}
