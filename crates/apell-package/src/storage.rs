use std::{fs, io::Write, path::Path};

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
  let parent = path.parent().ok_or("Invalid storage location.")?;
  fs::create_dir_all(parent).map_err(|_| "Could not create storage directory.")?;
  let temporary = parent.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
  let result = (|| {
    let mut file = fs::OpenOptions::new()
      .write(true)
      .create_new(true)
      .open(&temporary)
      .map_err(|_| "Could not stage data.")?;
    file
      .write_all(bytes)
      .and_then(|_| file.sync_all())
      .map_err(|_| "Could not persist data.")?;
    drop(file);
    fs::rename(&temporary, path).map_err(|_| "Could not activate data.")?;
    sync_directory(parent)
  })();
  let _ = fs::remove_file(temporary);
  result
}

pub fn sync_directory(path: &Path) -> Result<(), String> {
  #[cfg(unix)]
  fs::File::open(path)
    .and_then(|directory| directory.sync_all())
    .map_err(|_| "Could not persist storage directory.".to_string())?;
  #[cfg(not(unix))]
  let _ = path;
  Ok(())
}
