// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! The Windows setup stub: it installs the app its payload holds for the current user, or, as the
//! uninstaller (a payload of no files), removes it, as spec.md's Windows section gives it. The
//! packer stamps its icon, version and manifest and appends the payload ([payload]). This file is
//! what holds on any host: the arguments, the files written and removed, the Uninstall key's
//! values; `windows.rs` the registry, the shortcut and the run.
//!
//! The idea of a stub with a placeholder the packer patches is Velopack's (MIT,
//! <https://github.com/velopack/velopack/blob/develop/src/bins/src/setup.rs>); the code is ours.

#![cfg_attr(windows, windows_subsystem = "windows")]
// Off Windows only the tests run what is here.
#![cfg_attr(not(windows), allow(dead_code))]

mod payload;
#[cfg(windows)]
mod windows;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use std::{fs, io, thread};

use payload::{Files, Info, PLACEHOLDER};

/// Where the payload is: patched by the packer, read as it is in the file ([placeholder]).
#[used]
static PLACEHOLDER_DATA: [u8; PLACEHOLDER] = payload::placeholder();

/// The uninstaller's name in the install folder.
const UNINSTALLER: &str = "Uninstall.exe";
/// The licence's name in the install folder; the uninstaller removes it whether its setup wrote
/// one or not, as an older one may have.
const LICENSE: &str = "LICENSE.txt";
/// How long a file in use is tried again: the app that just quit may hold its exe a moment.
const RETRY: Duration = Duration::from_secs(5);

fn main() -> std::process::ExitCode {
  #[cfg(windows)]
  return windows::main();

  #[cfg(not(windows))]
  {
    eprintln!("shipo-setup: the setup runs on Windows");
    std::process::ExitCode::from(2)
  }
}

/// The placeholder as the packer left it in this exe; read through a volatile load, so the
/// compiler never takes its zeros for constants.
fn placeholder() -> [u8; PLACEHOLDER] {
  // SAFETY: a static's own bytes.
  unsafe { std::ptr::read_volatile(&raw const PLACEHOLDER_DATA) }
}

/// What the command line asks.
#[derive(Debug, PartialEq, Default)]
struct Args {
  /// `/S`: no message, the app not started.
  silent: bool,
  /// `/D=<folder>`, the last argument, its spaces kept unquoted, as NSIS setups take it.
  dir: Option<PathBuf>,
}

impl Args {
  fn parse(args: impl IntoIterator<Item = OsString>) -> Self {
    let mut out = Self::default();
    let mut args = args.into_iter().map(|a| a.to_string_lossy().into_owned());
    while let Some(arg) = args.next() {
      if arg.eq_ignore_ascii_case("/S") {
        out.silent = true;
      } else if let Some(dir) = arg.strip_prefix("/D=") {
        let rest: Vec<_> = [dir.to_string()].into_iter().chain(args.by_ref()).collect();
        out.dir = Some(rest.join(" ").into());
      }
    }

    out
  }
}

/// This exe's payload by the placeholder's fields: its [Info], its files, and the stub's bytes
/// before it.
fn own(exe: &[u8], fields: [u64; 3]) -> io::Result<(Info, Files, &[u8])> {
  let [_, offset, len] = fields;
  let end = offset.checked_add(len).and_then(|end| usize::try_from(end).ok());
  let Some(payload) = end.filter(|&end| offset > 0 && end <= exe.len()).map(|end| &exe[..end])
  else {
    return Err(io::Error::other("this setup holds no app: the packer appends one"));
  };

  let (info, files) = payload::unpack(&payload[offset as usize..])?;
  Ok((info, files, &exe[..offset as usize]))
}

/// The uninstaller of `info`: the stub's bytes `stub`, its placeholder at `at` pointing at a
/// payload of `info` alone.
fn uninstaller(stub: &[u8], at: u64, info: &Info) -> io::Result<Vec<u8>> {
  let data = payload::pack(info, &[])?;
  let mut out = stub.to_vec();
  let at = usize::try_from(at).ok().filter(|&at| at + PLACEHOLDER <= out.len());
  let at = at.ok_or_else(|| io::Error::other("this setup's placeholder is outside it"))?;
  payload::patch(&mut out, at, stub.len() as u64, data.len() as u64);
  out.extend(data);
  Ok(out)
}

/// `op` until it succeeds, tried again for [RETRY]; its last error then.
fn retry<T>(mut op: impl FnMut() -> io::Result<T>) -> io::Result<T> {
  let end = Instant::now() + RETRY;
  loop {
    match op() {
      Err(e) if e.kind() != io::ErrorKind::NotFound && Instant::now() < end => {
        thread::sleep(Duration::from_millis(250))
      }
      done => return done,
    }
  }
}

/// Writes `files` into `dir`, made if missing, the app's exe first: each written beside as
/// `.<name>.shipo-new` and renamed over the old one, tried again while it is in use. Gives their
/// size.
fn put(dir: &Path, files: &[(String, Vec<u8>)]) -> io::Result<u64> {
  fs::create_dir_all(dir)?;
  let mut size = 0;
  for (name, data) in files {
    let (path, part) = (dir.join(name), dir.join(format!(".{name}.shipo-new")));
    fs::write(&part, data)?;
    if let Err(e) = retry(|| fs::rename(&part, &path)) {
      let _ = fs::remove_file(&part);
      return Err(in_use(&path, e));
    }

    size += data.len() as u64;
  }

  Ok(size)
}

/// Removes the files the setups of `name` write from `dir`, the app's exe first, the updater's
/// staging folders, then `dir` if it is empty (a `/D=` may share it). The uninstaller, running,
/// is moved out before, if `moved` can; else it stays, and so does the folder.
fn remove(dir: &Path, name: &str, moved: impl FnOnce() -> bool) -> io::Result<()> {
  let exe = dir.join(format!("{name}.exe"));
  match retry(|| fs::remove_file(&exe)) {
    Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(in_use(&exe, e)),
    _ => {}
  }

  let names: &[&str] = if moved() { &[LICENSE, UNINSTALLER] } else { &[LICENSE] };
  for name in names {
    match retry(|| fs::remove_file(dir.join(name))) {
      Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
      _ => {}
    }
  }

  for entry in fs::read_dir(dir)? {
    let entry = entry?;
    if entry.file_name().to_string_lossy().starts_with(".shipo-update-") {
      retry(|| fs::remove_dir_all(entry.path()))?;
    }
  }

  let _ = fs::remove_dir(dir);
  Ok(())
}

fn in_use(path: &Path, e: io::Error) -> io::Error {
  io::Error::new(e.kind(), format!("{} is in use or can't be written: {e}", path.display()))
}

/// A value of the Uninstall key.
#[derive(Debug, PartialEq)]
enum Value {
  Text(String),
  Number(u32),
}

/// The Uninstall key's values for Settings › Apps of `info` installed in `dir`, `kib` its size.
fn values(info: &Info, dir: &Path, kib: u64) -> Vec<(&'static str, Value)> {
  let dir = dir.display().to_string();
  let uninstall = format!(r#""{dir}\{UNINSTALLER}""#);
  let text = |s: &str| Value::Text(s.into());
  vec![
    ("DisplayName", text(&info.name)),
    ("DisplayVersion", text(&info.version)),
    ("Publisher", text(&info.publisher)),
    ("DisplayIcon", Value::Text(format!(r"{dir}\{}.exe", info.name))),
    ("InstallLocation", text(&dir)),
    ("QuietUninstallString", Value::Text(format!("{uninstall} /S"))),
    ("UninstallString", Value::Text(uninstall)),
    ("NoModify", Value::Number(1)),
    ("NoRepair", Value::Number(1)),
    ("EstimatedSize", Value::Number(u32::try_from(kib).unwrap_or(u32::MAX))),
  ]
}

#[cfg(test)]
mod tests {
  use std::sync::atomic::{AtomicU32, Ordering};

  use super::*;

  fn tmp() -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("shipo-setup-{}-{n}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
  }

  fn info() -> Info {
    let app_id = "{a90596b0-07fa-55a1-9740-d4470c5cef1c}".into();
    Info { name: "Hello".into(), id: "com.example.hello".into(), app_id, ..Default::default() }
  }

  fn listed(dir: &Path) -> Vec<String> {
    let mut names: Vec<_> =
      fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
    names.sort();
    names
  }

  #[test]
  fn args() {
    let parse = |args: &[&str]| Args::parse(args.iter().map(OsString::from));
    assert_eq!(parse(&[]), Args::default());
    assert_eq!(parse(&["/S"]), Args { silent: true, dir: None });
    assert_eq!(parse(&["/s", "/x"]), Args { silent: true, dir: None });
    let dir = Some(PathBuf::from(r"C:\My Apps\Hello"));
    assert_eq!(parse(&["/S", r"/D=C:\My", r"Apps\Hello"]), Args { silent: true, dir });
  }

  #[test]
  fn own_payload() {
    let files = vec![("Hello.exe".to_string(), b"exe".to_vec())];
    let data = payload::pack(&info(), &files).unwrap();
    let mut exe =
      [b"MZ".to_vec(), vec![0; 20], payload::placeholder().to_vec(), vec![0; 8]].concat();
    let stub_len = exe.len() as u64;
    payload::patch(&mut exe, 22, stub_len, data.len() as u64);
    exe.extend(&data);
    let found: [u8; PLACEHOLDER] = exe[22..22 + PLACEHOLDER].try_into().unwrap();
    let fields = payload::fields(&found);
    let (got, got_files, stub) = own(&exe, fields).unwrap();
    assert_eq!((got, got_files, stub.len() as u64), (info(), files, stub_len));

    // The uninstaller: the same stub, its payload the info alone.
    let un = uninstaller(stub, fields[0], &info()).unwrap();
    let found: [u8; PLACEHOLDER] = un[22..22 + PLACEHOLDER].try_into().unwrap();
    let (got, got_files, again) = own(&un, payload::fields(&found)).unwrap();
    assert_eq!((got, got_files.len(), again.len() as u64), (info(), 0, stub_len));

    let e = own(&exe, [0, 0, 0]).unwrap_err().to_string();
    assert_eq!(e, "this setup holds no app: the packer appends one");
    assert!(own(&exe, [22, stub_len, data.len() as u64 + 1]).is_err());
    assert!(own(&exe, [22, u64::MAX, 2]).is_err());
    assert!(uninstaller(stub, stub_len, &info()).is_err());
  }

  #[test]
  fn files_put_and_removed() {
    let dir = tmp().join("Hello");
    let files = [("Hello.exe", "v1"), (LICENSE, "MIT"), (UNINSTALLER, "un")];
    let files: Vec<_> = files.iter().map(|(n, d)| (n.to_string(), d.as_bytes().to_vec())).collect();
    assert_eq!(put(&dir, &files).unwrap(), 7);
    assert_eq!(listed(&dir), ["Hello.exe", LICENSE, UNINSTALLER]);
    assert_eq!(put(&dir, &[("Hello.exe".into(), b"v2".to_vec())]).unwrap(), 2);
    assert_eq!(fs::read(dir.join("Hello.exe")).unwrap(), b"v2");

    // The staging folders go, the app's data and what a `/D=` shares stay.
    fs::create_dir_all(dir.join(".shipo-update-1-2/x")).unwrap();
    fs::write(dir.join("data.txt"), "kept").unwrap();
    remove(&dir, "Hello", || true).unwrap();
    assert_eq!(listed(&dir), ["data.txt"]);

    // An uninstaller that can't move out stays, with its folder.
    fs::remove_file(dir.join("data.txt")).unwrap();
    put(&dir, &files).unwrap();
    remove(&dir, "Hello", || false).unwrap();
    assert_eq!(listed(&dir), [UNINSTALLER]);
    remove(&dir, "Hello", || true).unwrap();
    assert!(!dir.exists());
  }

  #[test]
  fn uninstall_key() {
    let info = Info { version: "1.2.0".into(), publisher: "example".into(), ..info() };
    let values = values(&info, Path::new(r"C:\Users\Me\AppData\Local\Programs\Hello"), 42);
    let text = |k: &str| match values.iter().find(|(key, _)| *key == k) {
      Some((_, Value::Text(s))) => s.clone(),
      v => panic!("{k}: {v:?}"),
    };

    assert_eq!(text("DisplayIcon"), r"C:\Users\Me\AppData\Local\Programs\Hello\Hello.exe");
    let un = r#""C:\Users\Me\AppData\Local\Programs\Hello\Uninstall.exe""#;
    assert_eq!(text("UninstallString"), un);
    assert_eq!(text("QuietUninstallString"), format!("{un} /S"));
    assert_eq!(values.len(), 10);
    assert!(values.contains(&("EstimatedSize", Value::Number(42))));
  }
}
