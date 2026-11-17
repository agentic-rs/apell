mod pack;
mod publish;

use std::{ffi::OsStr, process::ExitCode};

fn main() -> ExitCode {
  let arguments: Vec<_> = std::env::args_os().skip(1).collect();
  let result = match arguments.first().map(|value| value.as_os_str()) {
    Some(command) if command == OsStr::new("pack") => pack::run(arguments),
    Some(command) if command == OsStr::new("publish") => publish::run(&arguments[1..]),
    _ => Err(format!("{}\n{}", pack::USAGE, publish::USAGE)),
  };
  match result {
    Ok(()) => ExitCode::SUCCESS,
    Err(error) => {
      eprintln!("{error}");
      ExitCode::FAILURE
    }
  }
}
