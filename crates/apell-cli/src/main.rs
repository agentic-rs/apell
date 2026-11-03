use apell_core::bundles::{atomic_write, digest, pack_directory};
use std::{
  ffi::OsStr,
  path::{Component, Path, PathBuf},
  process::ExitCode,
};

fn main() -> ExitCode {
  match run(std::env::args_os().skip(1).collect()) {
    Ok(()) => ExitCode::SUCCESS,
    Err(error) => {
      eprintln!("{error}");
      ExitCode::FAILURE
    }
  }
}

fn run(arguments: Vec<std::ffi::OsString>) -> Result<(), String> {
  let [command, directory, flag, output] = arguments.as_slice() else {
    return Err("Usage: apell pack <directory> --output <file.apel>".into());
  };
  if command != "pack"
    || flag != "--output"
    || Path::new(output).extension() != Some(OsStr::new("apel"))
  {
    return Err("Usage: apell pack <directory> --output <file.apel>".into());
  }
  let directory =
    std::fs::canonicalize(directory).map_err(|_| "Could not open the package directory.")?;
  let path = output_path(&directory, Path::new(output))?;
  let bytes = pack_directory(&directory)?;
  atomic_write(&path, &bytes)?;
  println!("Packed {} bytes. SHA-256 {}", bytes.len(), digest(&bytes));
  Ok(())
}

fn output_path(directory: &Path, output: &Path) -> Result<PathBuf, String> {
  let parent = output
    .parent()
    .filter(|path| !path.as_os_str().is_empty())
    .unwrap_or(Path::new("."));
  let parent = std::path::absolute(parent).map_err(|_| "Could not resolve output directory.")?;
  let mut resolved = PathBuf::new();
  // Resolve existing aliases before processing `..`, including aliases above
  // an output directory that does not yet exist. This check creates no files.
  for component in parent.components() {
    match component {
      Component::CurDir => continue,
      Component::ParentDir => {
        resolved.pop();
      }
      _ => resolved.push(component.as_os_str()),
    }
    match std::fs::canonicalize(&resolved) {
      Ok(path) => resolved = path,
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
      Err(_) => return Err("Could not resolve output directory.".into()),
    }
  }
  if resolved.starts_with(directory) {
    return Err("Choose an output file outside the package directory.".into());
  }
  Ok(resolved.join(output.file_name().ok_or("Choose an output file.")?))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn nested_output_is_rejected_before_it_can_change_the_package_input() {
    let root = std::env::temp_dir().join(format!(
      "apell-cli-{}-{}",
      std::process::id(),
      std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
    ));
    std::fs::create_dir(&root).unwrap();
    let input = root.join("package");
    std::fs::create_dir(&input).unwrap();
    std::fs::write(input.join("manifest.json"), br#"{"schema_version":1,"api_version":2,"applet_id":"fixture","name":"Fixture","version":"1.0.0","permissions":[]}"#).unwrap();
    std::fs::write(input.join("index.html"), b"<p>fixture</p>").unwrap();
    let pack = |output: &Path| {
      run(vec![
        "pack".into(),
        input.as_os_str().into(),
        "--output".into(),
        output.as_os_str().into(),
      ])
    };
    assert!(pack(&input.join("nested/fixture.apel"))
      .unwrap_err()
      .contains("outside"));
    assert!(!input.join("nested").exists());
    #[cfg(unix)]
    {
      let alias = root.join("alias");
      std::os::unix::fs::symlink(&input, &alias).unwrap();
      assert!(pack(&alias.join("nested/fixture.apel"))
        .unwrap_err()
        .contains("outside"));
      assert!(!input.join("nested").exists());
    }
    let output = root.join("artifacts/fixture.apel");
    pack(&output).unwrap();
    let first = std::fs::read(&output).unwrap();
    pack(&output).unwrap();
    assert_eq!(std::fs::read(&output).unwrap(), first);
    std::fs::remove_dir_all(root).unwrap();
  }
}
