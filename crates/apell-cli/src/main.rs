mod account;
mod pack;
mod publish;

use std::{ffi::OsStr, process::ExitCode};

fn main() -> ExitCode {
  let arguments: Vec<_> = std::env::args_os().skip(1).collect();
  let result = match arguments.first().map(|value| value.as_os_str()) {
    Some(command) if command == OsStr::new("pack") => pack::run(arguments),
    Some(command) if command == OsStr::new("publish") => publish::run(&arguments[1..]),
    Some(command)
      if ["login", "whoami", "logout", "package", "invitations"]
        .iter()
        .any(|value| command == OsStr::new(value)) =>
    {
      account::run(&arguments)
    }
    Some(command) if command == OsStr::new("--help") || command == OsStr::new("-h") => {
      println!("{}\n{}\n{}", pack::USAGE, publish::USAGE, account::USAGE);
      Ok(())
    }
    _ => Err(format!(
      "{}\n{}\n{}",
      pack::USAGE,
      publish::USAGE,
      account::USAGE
    )),
  };
  match result {
    Ok(()) => ExitCode::SUCCESS,
    Err(error) => {
      eprintln!("{error}");
      ExitCode::FAILURE
    }
  }
}
