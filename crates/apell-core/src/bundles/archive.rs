use super::{
  validate_manifest, Bundle, Manifest, MAX_BUNDLE_BYTES, MAX_ENTRIES, MAX_EXPANDED_BYTES,
};
use std::{
  collections::{BTreeMap, HashMap, HashSet},
  io::{Cursor, Read, Write},
  path::Path,
};
use unicase::UniCase;
use unicode_normalization::UnicodeNormalization;
use zip::{write::SimpleFileOptions, CompressionMethod, ZipArchive, ZipWriter};

const ZIP_ERROR: &str = "Invalid or unsupported .apel ZIP archive.";
const EXPANDED_ERROR: &str = "Package exceeds the 64 MiB expanded size limit.";

// Use package paths directly as asset identifiers. Never normalize a caller's
// traversal, encoded separator, URI query, or platform-specific path into one.
fn validate_path(path: &str) -> Result<(), String> {
  if path.is_empty()
    || path.len() > 1024
    || path.split('/').count() > 32
    || path
      .chars()
      .any(|c| c.is_control() || "\\:%?#\"<>|*".contains(c))
    || path.split('/').any(|part| {
      part.is_empty()
        || part == "."
        || part == ".."
        || part.len() > 255
        || part.ends_with([' ', '.'])
    })
  {
    return Err("Package contains an unsafe or unsupported path.".into());
  }
  Ok(())
}

#[derive(Default)]
struct Paths {
  explicit: HashSet<String>,
  // Include implicit parent directories, so assets/Foo/x and assets/foo/y
  // cannot disagree about the same directory's spelling on a mobile device.
  components: HashMap<UniCase<String>, (String, bool)>,
}

impl Paths {
  fn add(&mut self, name: &str) -> Result<(), String> {
    let path = name.strip_suffix('/').unwrap_or(name);
    validate_path(path)?;
    if !self.explicit.insert(path.to_owned()) {
      return Err("Duplicate package entry.".into());
    }
    let mut prefix = String::new();
    let mut parts = path.split('/').peekable();
    while let Some(part) = parts.next() {
      if !prefix.is_empty() {
        prefix.push('/');
      }
      prefix.push_str(part);
      let directory = parts.peek().is_some() || name.ends_with('/');
      let record = self
        .components
        .entry(UniCase::new(prefix.nfc().collect()))
        .or_insert_with(|| (prefix.clone(), directory));
      if record.0 != prefix || record.1 != directory {
        return Err("Package paths collide.".into());
      }
      if self.components.len() > MAX_ENTRIES * 2 {
        return Err("Package contains too many nested directories.".into());
      }
    }
    Ok(())
  }
}

fn word(bytes: &[u8], start: usize) -> Result<usize, String> {
  let data = bytes.get(start..start + 2).ok_or(ZIP_ERROR)?;
  Ok(u16::from_le_bytes([data[0], data[1]]) as usize)
}
fn dword(bytes: &[u8], start: usize) -> Result<usize, String> {
  let data = bytes.get(start..start + 4).ok_or(ZIP_ERROR)?;
  Ok(u32::from_le_bytes(data.try_into().unwrap()) as usize)
}

// zip-rs indexes by name and collapses exact duplicates. Check the physical
// directory first and bound its count before letting the decoder allocate.
// ZIP64 and multi-disk ZIPs are unnecessary under the .apel size limits.
fn zip_directory(bytes: &[u8]) -> Result<(usize, Vec<&str>), String> {
  if !bytes.starts_with(b"PK\x03\x04") || bytes.len() < 22 {
    return Err(ZIP_ERROR.into());
  }
  let first = bytes.len().saturating_sub(22 + 65_535);
  let end = (first..=bytes.len() - 22)
    .rev()
    .find(|&i| {
      bytes[i..].starts_with(b"PK\x05\x06")
        && i + 22 + word(bytes, i + 20).unwrap_or(0) == bytes.len()
    })
    .ok_or(ZIP_ERROR)?;
  let count = word(bytes, end + 10)?;
  let start = dword(bytes, end + 16)?;
  if word(bytes, end + 4)? != 0
    || word(bytes, end + 6)? != 0
    || word(bytes, end + 8)? != count
    || count == 0
    || count > MAX_ENTRIES
    || start.checked_add(dword(bytes, end + 12)?) != Some(end)
  {
    return Err(ZIP_ERROR.into());
  }
  let mut paths = Paths::default();
  let mut names = Vec::with_capacity(count);
  let mut cursor = start;
  for _ in 0..count {
    let header = bytes.get(cursor..end).ok_or(ZIP_ERROR)?;
    if header.len() < 46 || !header.starts_with(b"PK\x01\x02") {
      return Err(ZIP_ERROR.into());
    }
    let flags = word(header, 8)?;
    if flags & (1 | 64 | 8192) != 0 {
      return Err("Encrypted packages are unsupported.".into());
    }
    let method = word(header, 10)?;
    if !matches!(method, 0 | 8) || word(header, 34)? != 0 {
      return Err(ZIP_ERROR.into());
    }
    let length = word(header, 28)?;
    let raw = header.get(46..46 + length).ok_or(ZIP_ERROR)?;
    let name = std::str::from_utf8(raw).map_err(|_| "Package paths must use UTF-8.")?;
    paths.add(name)?;
    let local_start = dword(header, 42)?;
    let local = bytes.get(local_start..start).ok_or(ZIP_ERROR)?;
    if !local.starts_with(b"PK\x03\x04")
      || local.len() < 30
      || word(local, 6)? != flags
      || word(local, 8)? != method
      || word(local, 26)? != length
      || local.get(30..30 + length) != Some(raw)
    {
      return Err("Package local and central entries disagree.".into());
    }
    let data_offset = 30 + length + word(local, 28)?;
    if data_offset
      .checked_add(dword(header, 20)?)
      .is_none_or(|n| n > local.len())
    {
      return Err(ZIP_ERROR.into());
    }
    names.push(name);
    cursor += 46 + length + word(header, 30)? + word(header, 32)?;
    if cursor > end {
      return Err(ZIP_ERROR.into());
    }
  }
  if cursor != end {
    return Err(ZIP_ERROR.into());
  }
  Ok((start, names))
}

pub fn parse(bytes: &[u8]) -> Result<Bundle, String> {
  if bytes.len() > MAX_BUNDLE_BYTES {
    return Err("Package exceeds the 24 MiB archive limit.".into());
  }
  let (directory, names) = zip_directory(bytes)?;
  let mut archive = ZipArchive::new(Cursor::new(bytes)).map_err(|_| ZIP_ERROR)?;
  if archive.len() != names.len()
    || archive.central_directory_start() != directory as u64
    || archive.offset() != 0
    || archive.has_overlapping_files().map_err(|_| ZIP_ERROR)?
  {
    return Err(ZIP_ERROR.into());
  }
  let mut files = BTreeMap::new();
  let mut remaining = MAX_EXPANDED_BYTES;
  for (i, name) in names.into_iter().enumerate() {
    let entry = archive.by_index(i).map_err(|_| ZIP_ERROR)?;
    let mode = entry.unix_mode().unwrap_or(0) & 0o170000;
    if entry.encrypted()
      || entry.name() != name
      || entry.name_raw() != name.as_bytes()
      || !matches!(mode, 0 | 0o040000 | 0o100000)
      || mode == 0o040000 && !entry.is_dir()
      || mode == 0o100000 && entry.is_dir()
    {
      return Err("Package contains an unsafe or ambiguous entry.".into());
    }
    if entry.size() > remaining as u64 {
      return Err(EXPANDED_ERROR.into());
    }
    let declared = entry.size();
    let directory = entry.is_dir();
    // Enforce the bytes actually emitted by the decoder, even for dishonest
    // central size fields. Read through EOF so CRC failures cannot be skipped.
    let mut data = Vec::new();
    entry
      .take(remaining as u64 + 1)
      .read_to_end(&mut data)
      .map_err(|_| ZIP_ERROR)?;
    if data.len() > remaining {
      return Err(EXPANDED_ERROR.into());
    }
    if data.len() as u64 != declared || directory && !data.is_empty() {
      return Err(ZIP_ERROR.into());
    }
    remaining -= data.len();
    if !directory {
      files.insert(name.to_owned(), data);
    }
  }
  bundle(files)
}

fn bundle(files: BTreeMap<String, Vec<u8>>) -> Result<Bundle, String> {
  let manifest_bytes = files
    .get("manifest.json")
    .ok_or("Package is missing manifest.json.")?;
  if manifest_bytes.len() > 64 * 1024 {
    return Err("Package manifest exceeds 64 KiB.".into());
  }
  let manifest: Manifest =
    serde_json::from_slice(manifest_bytes).map_err(|_| "Invalid package manifest.")?;
  validate_manifest(&manifest, false)?;
  let html = std::str::from_utf8(
    files
      .get("index.html")
      .ok_or("Package is missing index.html.")?,
  )
  .map_err(|_| "index.html must use UTF-8.")?;
  if html.trim().is_empty() {
    return Err("Applet entry is empty.".into());
  }
  if let Some(path) = &manifest.lib {
    validate_path(path)?;
    if !path.starts_with("lib/")
      || !path.ends_with(".wasm")
      || !files
        .get(path)
        .is_some_and(|bytes| bytes.starts_with(b"\0asm\x01\x00\x00\x00"))
    {
      return Err("The declared backend must be a WebAssembly module inside lib/.".into());
    }
  }
  Ok(Bundle {
    manifest,
    html: html.to_owned(),
    files,
  })
}

/// Pack only files under the supplied directory. Ordering, timestamps, modes,
/// and compression settings are fixed so identical input has an identical digest.
pub fn pack_directory(directory: &Path) -> Result<Vec<u8>, String> {
  if !std::fs::symlink_metadata(directory).is_ok_and(|m| m.file_type().is_dir()) {
    return Err("Select a regular package directory.".into());
  }
  let mut files = BTreeMap::new();
  let mut paths = Paths::default();
  let mut remaining = MAX_EXPANDED_BYTES;
  let mut directories = vec![String::new()];
  while let Some(prefix) = directories.pop() {
    let entries = std::fs::read_dir(directory.join(&prefix))
      .map_err(|_| "Could not read package directory.")?;
    for entry in entries {
      let entry = entry.map_err(|_| "Could not read package entry.")?;
      let part = entry
        .file_name()
        .into_string()
        .map_err(|_| "Package paths must use UTF-8.")?;
      let name = if prefix.is_empty() {
        part
      } else {
        format!("{prefix}/{part}")
      };
      let kind = entry
        .file_type()
        .map_err(|_| "Could not inspect package entry.")?;
      if kind.is_dir() {
        paths.add(&format!("{name}/"))?;
        directories.push(name);
      } else if kind.is_file() {
        paths.add(&name)?;
        let file = std::fs::File::open(entry.path()).map_err(|_| "Could not read package file.")?;
        if file
          .metadata()
          .map_err(|_| "Could not inspect package file.")?
          .len()
          > remaining as u64
        {
          return Err(EXPANDED_ERROR.into());
        }
        let mut data = Vec::new();
        file
          .take(remaining as u64 + 1)
          .read_to_end(&mut data)
          .map_err(|_| "Could not read package file.")?;
        remaining = remaining.checked_sub(data.len()).ok_or(EXPANDED_ERROR)?;
        files.insert(name, data);
      } else {
        return Err("Packages cannot contain symlinks or special files.".into());
      }
      if paths.explicit.len() > MAX_ENTRIES {
        return Err("Package exceeds 1024 entries.".into());
      }
    }
  }
  let files = bundle(files)?.files;
  let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
  let options = SimpleFileOptions::default()
    .compression_method(CompressionMethod::Deflated)
    .compression_level(Some(6))
    .last_modified_time(zip::DateTime::default())
    .unix_permissions(0o644);
  for (name, data) in files {
    writer
      .start_file(name, options)
      .map_err(|_| "Could not pack package entry.")?;
    writer
      .write_all(&data)
      .map_err(|_| "Could not pack package entry.")?;
  }
  let bytes = writer
    .finish()
    .map_err(|_| "Could not finish ZIP package.")?
    .into_inner();
  if bytes.len() > MAX_BUNDLE_BYTES {
    return Err("Package exceeds the 24 MiB archive limit.".into());
  }
  Ok(bytes)
}
