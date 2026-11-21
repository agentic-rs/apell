use serde::Serialize;
use std::{
  fs,
  io::Write,
  path::{Path, PathBuf},
};

pub(super) struct OutputFile {
  path: PathBuf,
  temporary: tempfile::NamedTempFile,
}

impl OutputFile {
  pub fn prepare(path: &Path) -> Result<Self, String> {
    match fs::symlink_metadata(path) {
      Ok(_) => return Err("The output file already exists. Choose a new private file.".into()),
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
      Err(_) => return Err("Could not inspect the output file.".into()),
    }
    let parent = path
      .parent()
      .filter(|value| !value.as_os_str().is_empty())
      .unwrap_or(Path::new("."));
    let temporary = tempfile::NamedTempFile::new_in(parent)
      .map_err(|_| "Could not prepare private output. Choose an existing writable directory.")?;
    Ok(Self {
      path: path.to_owned(),
      temporary,
    })
  }

  pub fn save(self, value: &impl Serialize) -> Result<(), String> {
    let mut temporary = self.temporary;
    serde_json::to_writer_pretty(temporary.as_file_mut(), value)
      .map_err(|_| "Could not encode the client configuration.")?;
    temporary
      .write_all(b"\n")
      .map_err(|_| "Could not write the private client configuration.")?;
    temporary
      .as_file()
      .sync_all()
      .map_err(|_| "Could not sync the private client configuration.")?;
    temporary.persist_noclobber(self.path)
      .map_err(|_| "Could not save the private client configuration. The destination may have appeared during registration.")?;
    Ok(())
  }
}
