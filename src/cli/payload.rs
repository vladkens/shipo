// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! The Windows setup's payload: what the packer appends to the setup stub and the stub installs,
//! and the placeholder in the stub that says where it is. Shared by the packer, which writes it,
//! and the stub (`setup/`, which takes this file by its path), which reads it; each uses its half.
//!
//! The placeholder is [PLACEHOLDER] bytes in the stub's data: [MAGIC], then three u64 LE, its own
//! offset in the file, the payload's and the payload's length, zeros until the packer patches
//! them. The payload is one zlib stream of items, each a u32 LE name length, the name in UTF-8, a
//! u64 LE data length and the data: first [INFO], the setup's `key=value` lines, then the files to
//! install by their names in the install folder. An uninstaller is the stub with [INFO] alone.

#![allow(dead_code)]

use std::io::{self, Read, Write};

use flate2::Compression;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;

/// Where the placeholder starts; never anywhere else in the stub, so the packer finds it.
pub const MAGIC: [u8; 16] = *b"shipo-setup-v1\0\0";
pub const PLACEHOLDER: usize = MAGIC.len() + 24;
/// The first item's name: the setup's [Info].
pub const INFO: &str = ".setup";

/// The placeholder as the stub is built with it: [MAGIC], then zeros.
pub const fn placeholder() -> [u8; PLACEHOLDER] {
  let mut out = [0; PLACEHOLDER];
  let mut i = 0;
  while i < MAGIC.len() {
    out[i] = MAGIC[i];
    i += 1;
  }

  out
}

/// The files to install, each a name and its bytes.
pub type Files = Vec<(String, Vec<u8>)>;

/// What the stub needs besides the files: the app as Settings › Apps and the Start menu show it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Info {
  /// The app's name: its folder's, its exe's (`<name>.exe`) and its shortcut's.
  pub name: String,
  pub version: String,
  pub publisher: String,
  /// The app's id, the name of its launch-at-login values.
  pub id: String,
  /// The Uninstall key's name, a GUID in braces.
  pub app_id: String,
  /// The shortcut's description.
  pub comment: String,
  /// Whether the setup writes `LICENSE.txt`, which its uninstaller then removes.
  pub license: bool,
}

impl Info {
  const KEYS: [&str; 6] = ["name", "version", "publisher", "id", "app-id", "comment"];

  fn fields(&self) -> [&String; 6] {
    [&self.name, &self.version, &self.publisher, &self.id, &self.app_id, &self.comment]
  }

  /// Its `key=value` lines, a line break in a value a space.
  fn encode(&self) -> Vec<u8> {
    let lines = Self::KEYS.iter().zip(self.fields()).map(|(k, v)| {
      let v: Vec<_> = v.split(['\r', '\n']).filter(|s| !s.is_empty()).collect();
      format!("{k}={}\n", v.join(" "))
    });

    let license = if self.license { "license=1\n" } else { "" };
    (lines.collect::<String>() + license).into_bytes()
  }

  fn decode(data: &[u8]) -> io::Result<Self> {
    let text = std::str::from_utf8(data).map_err(|_| bad("its info is not UTF-8"))?;
    let mut info = Self::default();
    for line in text.lines() {
      let (k, v) = line.split_once('=').ok_or_else(|| bad("its info has a line of no key"))?;
      let field = match k {
        "name" => &mut info.name,
        "version" => &mut info.version,
        "publisher" => &mut info.publisher,
        "id" => &mut info.id,
        "app-id" => &mut info.app_id,
        "comment" => &mut info.comment,
        "license" => {
          info.license = v == "1";
          continue;
        }
        _ => continue,
      };

      *field = v.into();
    }

    match info.name.is_empty() || info.app_id.is_empty() || info.id.is_empty() {
      true => Err(bad("its info has no name, id or app-id")),
      false => Ok(info),
    }
  }
}

/// The payload of `info` and `files`, each a name and its bytes.
pub fn pack(info: &Info, files: &[(String, Vec<u8>)]) -> io::Result<Vec<u8>> {
  let mut z = ZlibEncoder::new(vec![], Compression::best());
  let head = info.encode();
  for (name, data) in [(INFO, &head)].into_iter().chain(files.iter().map(|(n, d)| (n.as_str(), d)))
  {
    z.write_all(&(name.len() as u32).to_le_bytes())?;
    z.write_all(name.as_bytes())?;
    z.write_all(&(data.len() as u64).to_le_bytes())?;
    z.write_all(data)?;
  }

  z.finish()
}

/// The [Info] and the files of `payload`. A file's name is a plain one, of no folder.
pub fn unpack(payload: &[u8]) -> io::Result<(Info, Files)> {
  let mut data = vec![];
  ZlibDecoder::new(payload).read_to_end(&mut data)?;
  let mut items = vec![];
  let mut rest = &data[..];
  while !rest.is_empty() {
    let (name, r) = take(rest, 4, |n| u32::from_le_bytes(n.try_into().unwrap()) as u64)?;
    let (name, r) = take(r, name, |b| String::from_utf8(b.to_vec()))?;
    let name = name.map_err(|_| bad("a name not in UTF-8"))?;
    let (len, r) = take(r, 8, |n| u64::from_le_bytes(n.try_into().unwrap()))?;
    let (body, r) = take(r, len, <[u8]>::to_vec)?;
    items.push((name, body));
    rest = r;
  }

  let mut items = items.into_iter();
  let info = match items.next() {
    Some((name, data)) if name == INFO => Info::decode(&data)?,
    _ => return Err(bad("it starts with no info")),
  };

  let files: Vec<_> = items.collect();
  for (name, _) in &files {
    let plain = !name.is_empty() && name != "." && name != "..";
    if !plain || name.contains(['/', '\\', ':']) || name.eq_ignore_ascii_case(INFO) {
      return Err(bad(format!("a file named {name:?}, not a plain name")));
    }
  }

  Ok((info, files))
}

/// `f` of the first `n` bytes of `data`, and the rest.
fn take<T>(data: &[u8], n: u64, f: impl FnOnce(&[u8]) -> T) -> io::Result<(T, &[u8])> {
  match usize::try_from(n).ok().filter(|&n| n <= data.len()) {
    Some(n) => Ok((f(&data[..n]), &data[n..])),
    None => Err(bad("it is cut short")),
  }
}

/// The placeholder's offset in `stub`, the one place [MAGIC] is.
pub fn find(stub: &[u8]) -> io::Result<usize> {
  let mut found = stub.windows(MAGIC.len()).enumerate().filter(|(_, w)| *w == MAGIC);
  match (found.next(), found.next()) {
    (Some((at, _)), None) if at + PLACEHOLDER <= stub.len() => Ok(at),
    (None, _) => Err(bad("the setup stub has no placeholder")),
    _ => Err(bad("the setup stub has more than one placeholder")),
  }
}

/// Writes into the placeholder at `at` in `exe` where it is, and where the payload is.
pub fn patch(exe: &mut [u8], at: usize, offset: u64, len: u64) {
  let fields = [at as u64, offset, len].map(u64::to_le_bytes).concat();
  exe[at + MAGIC.len()..at + PLACEHOLDER].copy_from_slice(&fields);
}

/// The placeholder's own offset, the payload's and its length.
pub fn fields(placeholder: &[u8; PLACEHOLDER]) -> [u64; 3] {
  let at = |i: usize| {
    let from = MAGIC.len() + 8 * i;
    u64::from_le_bytes(placeholder[from..from + 8].try_into().unwrap())
  };

  [at(0), at(1), at(2)]
}

fn bad(msg: impl Into<String>) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidData, format!("the setup's payload: {}", msg.into()))
}

#[cfg(test)]
mod tests {
  use super::*;

  fn info() -> Info {
    Info {
      name: "Hello".into(),
      version: "1.2.0".into(),
      publisher: "Ann = B".into(),
      id: "com.example.hello".into(),
      app_id: "{a90596b0-07fa-55a1-9740-d4470c5cef1c}".into(),
      comment: "Says\r\nhello".into(),
      license: true,
    }
  }

  #[test]
  fn round_trip() {
    let files = vec![("Hello.exe".to_string(), vec![7; 5000]), ("LICENSE.txt".into(), vec![])];
    let data = pack(&info(), &files).unwrap();
    assert!(data.len() < 220, "{}", data.len());

    let (got, got_files) = unpack(&data).unwrap();
    assert_eq!(got, Info { comment: "Says hello".into(), ..info() });
    assert_eq!(got_files, files);
    assert_eq!(unpack(&pack(&info(), &[]).unwrap()).unwrap().1, []);
  }

  #[test]
  fn refused() {
    for name in ["", "..", "a/b", "a\\b", "C:x", ".SETUP"] {
      let data = pack(&info(), &[(name.into(), vec![])]).unwrap();
      assert!(unpack(&data).unwrap_err().to_string().contains("not a plain name"), "{name}");
    }

    let no_info = Info { app_id: String::new(), ..info() };
    let e = unpack(&pack(&no_info, &[]).unwrap()).unwrap_err().to_string();
    assert_eq!(e, "the setup's payload: its info has no name, id or app-id");

    // Cut anywhere inside the items, it is an error, never a panic.
    let raw = b"\x06\0\0\0.setup\x05\0\0\0\0\0\0\0name=";
    for n in 1..=raw.len() {
      let mut z = ZlibEncoder::new(vec![], Compression::fast());
      z.write_all(&raw[..n]).unwrap();
      assert!(unpack(&z.finish().unwrap()).is_err(), "{n}");
    }

    assert!(unpack(b"not zlib").is_err());
  }

  #[test]
  fn placeholder_patched() {
    let mut stub = [vec![1; 100], placeholder().to_vec(), vec![2; 10]].concat();
    assert_eq!(find(&stub).unwrap(), 100);
    patch(&mut stub, 100, 5000, 42);
    let found: [u8; PLACEHOLDER] = stub[100..100 + PLACEHOLDER].try_into().unwrap();
    assert_eq!(fields(&found), [100, 5000, 42]);
    assert_eq!(fields(&placeholder()), [0, 0, 0]);

    assert_eq!(
      find(&[0; 64]).unwrap_err().to_string(),
      "the setup's payload: the setup stub has no placeholder"
    );
    let twice = [placeholder().to_vec(), placeholder().to_vec()].concat();
    assert!(find(&twice).unwrap_err().to_string().contains("more than one"));
    assert!(find(&MAGIC).is_err());
  }
}
