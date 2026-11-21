mod account;
mod auth;
mod dev;
mod pack;
mod publish;

use std::{ffi::OsStr, process::ExitCode};

pub fn run() -> ExitCode {
  let arguments: Vec<_> = std::env::args_os().skip(1).collect();
  let result = match arguments.first().map(|value| value.as_os_str()) {
    Some(command) if command == OsStr::new("pack") => pack::run(arguments),
    Some(command) if command == OsStr::new("dev") => dev::run(&arguments[1..]),
    Some(command) if command == OsStr::new("publish") => publish::run(&arguments[1..]),
    Some(command) if command == OsStr::new("auth") => auth::run(&arguments[1..]),
    Some(command)
      if ["login", "whoami", "logout", "package", "invitations"]
        .iter()
        .any(|value| command == OsStr::new(value)) =>
    {
      account::run(&arguments)
    }
    Some(command) if command == OsStr::new("--help") || command == OsStr::new("-h") => {
      println!("{}", usage());
      Ok(())
    }
    _ => Err(usage()),
  };
  match result {
    Ok(()) => ExitCode::SUCCESS,
    Err(error) => {
      eprintln!("{error}");
      ExitCode::FAILURE
    }
  }
}

fn usage() -> String {
  [
    dev::USAGE,
    pack::USAGE,
    publish::USAGE,
    account::USAGE,
    auth::USAGE,
  ]
  .join("\n")
}
