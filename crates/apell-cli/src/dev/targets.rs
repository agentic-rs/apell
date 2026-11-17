use serde::Deserialize;
use std::{
  collections::BTreeMap,
  fmt, io,
  io::{IsTerminal, Write},
  process::Command,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Kind {
  Simulator,
  Device,
}

impl fmt::Display for Kind {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(match self {
      Self::Simulator => "simulator",
      Self::Device => "device",
    })
  }
}

#[derive(Clone, Debug)]
pub(crate) struct Target {
  pub kind: Kind,
  pub name: String,
  pub udid: String,
  pub booted: bool,
  os_version: String,
}

impl fmt::Display for Target {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{}  {}  iOS {}", self.kind, self.name, self.os_version)?;
    if self.booted {
      write!(f, "  booted")?;
    }
    write!(f, "  {}", self.udid)
  }
}

pub(crate) fn discover(filter: Option<Kind>) -> Result<Vec<Target>, String> {
  let mut targets = Vec::new();
  let mut failures = Vec::new();
  for kind in [Kind::Simulator, Kind::Device] {
    if filter.is_some_and(|filter| filter != kind) {
      continue;
    }
    let result = match kind {
      Kind::Simulator => xcrun(&["simctl", "list", "devices", "available", "--json"])
        .and_then(|bytes| simulators(&bytes)),
      Kind::Device => {
        xcrun(&["xcdevice", "list", "--timeout", "5"]).and_then(|bytes| devices(&bytes))
      }
    };
    match result {
      Ok(found) => targets.extend(found),
      Err(error) => failures.push(error),
    }
  }
  if targets.is_empty() && !failures.is_empty() {
    return Err(failures.join("\n"));
  }
  for failure in failures {
    eprintln!("{failure}");
  }
  targets.sort_by(|left, right| {
    (
      left.kind != Kind::Device,
      !left.booted,
      &left.name,
      &left.os_version,
      &left.udid,
    )
      .cmp(&(
        right.kind != Kind::Device,
        !right.booted,
        &right.name,
        &right.os_version,
        &right.udid,
      ))
  });
  targets.dedup_by(|left, right| left.kind == right.kind && left.udid == right.udid);
  Ok(targets)
}

fn xcrun(args: &[&str]) -> Result<Vec<u8>, String> {
  let result = Command::new("xcrun").args(args).output().map_err(|error| {
    format!(
      "Could not run xcrun {}: {error}. Install and select Xcode.",
      args[0]
    )
  })?;
  if !result.status.success() {
    return Err(format!(
      "Xcode could not list {} targets: {}",
      args[0],
      String::from_utf8_lossy(&result.stderr).trim()
    ));
  }
  Ok(result.stdout)
}

#[derive(Deserialize)]
struct SimulatorList {
  devices: BTreeMap<String, Vec<Simulator>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Simulator {
  device_type_identifier: Option<String>,
  is_available: bool,
  name: String,
  udid: String,
  state: String,
}

fn simulators(bytes: &[u8]) -> Result<Vec<Target>, String> {
  let list: SimulatorList = serde_json::from_slice(bytes)
    .map_err(|error| format!("Could not read simulator list from Xcode: {error}"))?;
  let mut targets = Vec::new();
  for (runtime, devices) in list.devices {
    let Some(version) = runtime.strip_prefix("com.apple.CoreSimulator.SimRuntime.iOS-") else {
      continue;
    };
    for device in devices {
      if device.is_available
        && device
          .device_type_identifier
          .as_deref()
          .is_some_and(|id| id.starts_with("com.apple.CoreSimulator.SimDeviceType.iPhone-"))
      {
        targets.push(Target {
          kind: Kind::Simulator,
          booted: device.state == "Booted",
          os_version: version.replace('-', "."),
          name: display_name(device.name),
          udid: device.udid,
        });
      }
    }
  }
  Ok(targets)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct XcodeDevice {
  available: bool,
  simulator: bool,
  platform: String,
  model_code: Option<String>,
  name: String,
  identifier: String,
  operating_system_version: Option<String>,
}

fn devices(bytes: &[u8]) -> Result<Vec<Target>, String> {
  let devices: Vec<XcodeDevice> = serde_json::from_slice(bytes)
    .map_err(|error| format!("Could not read device list from Xcode: {error}"))?;
  Ok(
    devices
      .into_iter()
      .filter(|device| {
        device.available
          && !device.simulator
          && device.platform == "com.apple.platform.iphoneos"
          && device
            .model_code
            .as_deref()
            .is_some_and(|model| model.starts_with("iPhone"))
      })
      .map(|device| Target {
        kind: Kind::Device,
        name: display_name(device.name),
        // xcdevice returns the hardware UDID, accepted by xcodebuild and devicectl.
        udid: device.identifier,
        booted: false,
        os_version: device
          .operating_system_version
          .unwrap_or_else(|| "unknown".into()),
      })
      .collect(),
  )
}

fn display_name(value: String) -> String {
  value.chars().filter(|c| !c.is_control()).collect()
}

pub(crate) fn list(targets: &[Target]) {
  if targets.is_empty() {
    println!("No iPhone targets found.");
  }
  for target in targets {
    println!("{target}");
  }
}

pub(crate) fn select(targets: &[Target], selector: Option<&str>) -> Result<Target, String> {
  let matches: Vec<_> = targets
    .iter()
    .filter(|target| match selector {
      None => true,
      Some("simulator") => target.kind == Kind::Simulator,
      Some("device") => target.kind == Kind::Device,
      Some(id) => target.udid.eq_ignore_ascii_case(id) || target.name.eq_ignore_ascii_case(id),
    })
    .collect();
  match matches.as_slice() {
    [] if targets.is_empty() => return Err("No matching iPhone targets found. Install an iOS Simulator runtime in Xcode, or connect and pair an iPhone with Developer Mode enabled.".into()),
    [] => return Err("No iPhone target matches that selection. Run apell dev --list to see available names and IDs.".into()),
    [only] => return Ok((*only).clone()),
    _ => (),
  }
  if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
    return Err("Multiple iPhone targets match. Run apell dev --list, then pass a unique target ID or name with --target.".into());
  }
  eprintln!("Choose an iPhone target:");
  for (index, target) in matches.iter().enumerate() {
    eprintln!("  {}. {target}", index + 1);
  }
  loop {
    eprint!("Target [1–{}]: ", matches.len());
    io::stderr().flush().map_err(|error| error.to_string())?;
    let mut input = String::new();
    let read = io::stdin()
      .read_line(&mut input)
      .map_err(|error| error.to_string())?;
    if read == 0 || input.trim().is_empty() {
      return Err("Target selection cancelled.".into());
    }
    if let Ok(number) = input.trim().parse::<usize>() {
      if let Some(target) = number.checked_sub(1).and_then(|index| matches.get(index)) {
        return Ok((*target).clone());
      }
    }
    eprintln!("Enter one of the listed numbers.");
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use serde_json::json;

  #[test]
  fn simulators_use_hardware_type_and_runtime_even_after_renaming() {
    let iphone = json!({"name":"Demo", "udid":"sim-a", "isAvailable":true, "state":"Booted","deviceTypeIdentifier":"com.apple.CoreSimulator.SimDeviceType.iPhone-16"});
    let mut ipad = iphone.clone();
    ipad["deviceTypeIdentifier"] = json!("com.apple.CoreSimulator.SimDeviceType.iPad-Pro");
    ipad["name"] = json!("iPhone");
    let mut unavailable = iphone.clone();
    unavailable["isAvailable"] = json!(false);
    let sample = json!({"devices":{
      "com.apple.CoreSimulator.SimRuntime.iOS-18-4":[iphone, ipad, unavailable],
      "com.apple.CoreSimulator.SimRuntime.tvOS-18-4":[iphone]
    }});
    let targets = simulators(&serde_json::to_vec(&sample).unwrap()).unwrap();
    assert_eq!(targets.len(), 1);
    assert!(targets[0].booted);
    assert_eq!(targets[0].os_version, "18.4");
    assert_eq!(select(&targets, Some("demo")).unwrap().udid, "sim-a");
  }

  #[test]
  fn physical_targets_must_be_available_iphones_and_keep_the_hardware_udid() {
    let iphone = json!({"simulator":false,"available":true,"platform":"com.apple.platform.iphoneos","modelCode":"iPhone15,4","identifier":"device-a","name":"Demo"});
    let mut offline = iphone.clone();
    offline["available"] = json!(false);
    let mut ipad = iphone.clone();
    ipad["modelCode"] = json!("iPad14,3");
    let mut sim = iphone.clone();
    sim["simulator"] = json!(true);
    let mut second = iphone.clone();
    second["identifier"] = json!("device-b");
    let targets =
      devices(&serde_json::to_vec(&json!([iphone, offline, ipad, sim, second])).unwrap()).unwrap();
    assert_eq!(targets.len(), 2);
    assert_eq!(select(&targets, Some("DEVICE-B")).unwrap().udid, "device-b");
    assert!(select(&targets, Some("simulator")).is_err());
  }
}
