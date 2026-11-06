//! Manual smoke test: cargo run -p apell-speech --example transcribe -- [--download] <model-directory> <audio-file>
use apell_speech::SpeechService;
use std::{
  path::PathBuf,
  sync::{atomic::AtomicBool, Arc},
};

#[tokio::main]
async fn main() -> Result<(), String> {
  let mut args = std::env::args_os().skip(1).peekable();
  let download = args.peek().is_some_and(|value| value == "--download");
  if download {
    args.next();
  }
  let root = args
    .next()
    .ok_or("Provide the model directory and audio file.")?;
  let audio = args.next().ok_or("Provide the audio file.")?;
  let service = SpeechService::new(PathBuf::from(root))?;
  if download {
    service
      .download_model(Arc::new(AtomicBool::new(false)), |_, _| {})
      .await?;
  }
  let result = tokio::task::spawn_blocking(move || {
    service.transcribe(
      &PathBuf::from(audio),
      None,
      Arc::new(AtomicBool::new(false)),
      |_| {},
    )
  })
  .await
  .map_err(|_| "Transcription worker failed.")??;
  println!("Language: {}\n{}", result.language, result.text);
  Ok(())
}
