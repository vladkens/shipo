// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! The Windows setup stub: it installs the app its payload holds for the current user, or, as the
//! uninstaller (a payload of no files), removes it. The packer stamps its icon, version and
//! manifest and appends the payload ([payload], the packer's `src/cli/payload.rs`). A package of
//! its own, so the `shipo` crate's binary stays the packer.
//! This file is what holds on any host: the arguments, the files written and removed, the Uninstall
//! key's values; `windows.rs` the registry, the shortcuts and the run, `wizard.rs` the wizard.
//!
//! The idea of a stub with a placeholder the packer patches is Velopack's (MIT,
//! <https://github.com/velopack/velopack/blob/develop/src/bins/src/setup.rs>); the code is ours.

#![cfg_attr(windows, windows_subsystem = "windows")]
// Off Windows only the tests run what is here.
#![cfg_attr(not(windows), allow(dead_code))]

#[path = "../../src/cli/payload.rs"]
mod payload;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
mod wizard;

use std::ffi::OsString;
#[cfg(not(windows))]
use std::fs::{remove_file as delete_file, rename as move_file};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use std::{fs, io, thread};

use payload::{Files, Info, PLACEHOLDER};
#[cfg(windows)]
use windows::{delete_file, move_file};

/// Where the payload is: patched by the packer, read as it is in the file ([placeholder]).
#[used]
static PLACEHOLDER_DATA: [u8; PLACEHOLDER] = payload::placeholder();

/// The uninstaller's name in the install folder, as NSIS setups name theirs, so an install over
/// one of those replaces it.
const UNINSTALLER: &str = "Uninstall.exe";
/// The parent of the app's Uninstall key in HKCU, `{<AppId>}`, where an install before this one,
/// an NSIS setup's too, left `InstallLocation`.
const UNINSTALL: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall";
/// The setup's exit codes besides 0, installed, which an updater reads as a failure.
const CANCELLED: u8 = 1;
const FAILED: u8 = 2;
/// The licence's name in the install folder.
const LICENSE: &str = "LICENSE.txt";
/// How long a file in use is tried again: the app that just quit may hold its exe a moment.
const RETRY: Duration = Duration::from_secs(5);
/// How long the setup's own file is tried again: an antivirus may hold it a moment after download.
const RETRY_OWN: Duration = Duration::from_millis(1500);

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
  /// `/D=<folder>`: the rest of the raw command line, unquoted, its spaces kept, as NSIS setups
  /// take it.
  dir: Option<String>,
}

impl Args {
  /// The arguments `args`, the program's not among them, and `/D=` from the command line `raw`.
  fn parse(args: impl IntoIterator<Item = OsString>, raw: &str) -> Self {
    let silent = args.into_iter().any(|a| a.to_string_lossy().eq_ignore_ascii_case("/S"));
    let at = raw.match_indices("/D=").map(|(i, _)| i);
    let at = at.filter(|&i| i == 0 || raw[..i].ends_with([' ', '\t'])).last();
    let dir = at.map(|i| raw[i + 3..].trim_end().to_string());
    Self { silent, dir }
  }

  /// The folder of `/D=`, which must be a [folder].
  fn folder(&self) -> io::Result<Option<PathBuf>> {
    let Some(dir) = &self.dir else { return Ok(None) };
    let e = || io::Error::other(format!("/D= takes an absolute folder, not {dir:?}"));
    folder(dir).map(Some).ok_or_else(e)
  }
}

/// `dir` without a trailing separator if it is an absolute folder below a drive's root (`C:\…`,
/// `\\?\C:\…`) or a share's (`\\server\share\…`, `\\?\UNC\server\share\…`), never the root
/// itself, which an uninstall would empty: no part of it empty, `.`, `..`, or dots and spaces
/// alone, which Windows drops or climbs by. Its `/` made `\`, so the key holds it as Windows does.
fn folder(dir: &str) -> Option<PathBuf> {
  let dir = dir.replace('/', "\\");
  let dir = dir.trim_end_matches('\\');
  let prefixed = |p: &str| dir.get(..p.len()).is_some_and(|h| h.eq_ignore_ascii_case(p));
  let (parts, least) = match [r"\\?\UNC\", r"\\.\UNC\"].into_iter().find(|p| prefixed(p)) {
    Some(p) => (&dir[p.len()..], 3),
    None if prefixed(r"\\?\") || prefixed(r"\\.\") => (drive(&dir[4..])?, 1),
    None => match dir.strip_prefix(r"\\") {
      Some(share) => (share, 3),
      None => (drive(dir)?, 1),
    },
  };

  let parts: Vec<_> = parts.split('\\').collect();
  let named = |p: &&str| !p.trim_matches(['.', ' ']).is_empty();
  (parts.len() >= least && parts.iter().all(named)).then(|| dir.into())
}

/// Where the setup installs, and whether it updates the app there: `/D=`'s folder (`given`), else
/// the app's own, the Uninstall key's `InstallLocation` (`location`) when it is a [folder], else
/// `<local>\Programs\<name>`, `local` being `%LOCALAPPDATA%`.
fn destination(
  given: Option<PathBuf>,
  location: Option<&str>,
  local: Option<OsString>,
  name: &str,
) -> io::Result<(PathBuf, bool)> {
  match (given, location.and_then(folder)) {
    (Some(dir), _) => Ok((dir, false)),
    (None, Some(dir)) => Ok((dir, true)),
    (None, None) => {
      let local = local.ok_or_else(|| io::Error::other("no LOCALAPPDATA"))?;
      Ok((Path::new(&local).join("Programs").join(name), false))
    }
  }
}

/// The folders of `path` after its drive, `C:\`.
fn drive(path: &str) -> Option<&str> {
  let b = path.as_bytes();
  let drive =
    b.len() > 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'\\' | b'/');
  drive.then(|| &path[3..])
}

/// Whether `id` is a GUID in braces, as the packer writes the Uninstall key's name.
fn guid(id: &str) -> bool {
  let Some(hex) = id.strip_prefix('{').and_then(|s| s.strip_suffix('}')) else { return false };
  let parts: Vec<_> = hex.split('-').map(str::len).collect();
  parts == [8, 4, 4, 4, 12] && hex.bytes().all(|b| b == b'-' || b.is_ascii_hexdigit())
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
/// payload of `info` alone. A signature the setup had covers bytes it no longer has, so its PE
/// security directory is zeroed and the checksum set again.
fn uninstaller(stub: &[u8], at: u64, info: &Info) -> io::Result<Vec<u8>> {
  let data = payload::pack(info, &[])?;
  let mut out = stub.to_vec();
  let end = usize::try_from(at).ok().and_then(|at| at.checked_add(PLACEHOLDER));
  let Some(at) = end.filter(|&end| end <= out.len()).map(|end| end - PLACEHOLDER) else {
    return Err(io::Error::other("this setup's placeholder is outside it"));
  };

  payload::patch(&mut out, at, stub.len() as u64, data.len() as u64);
  out.extend(data);
  unsigned(&mut out);
  Ok(out)
}

/// `exe`, a PE32+ file, with no security directory and its checksum set again; anything else as it
/// is.
fn unsigned(exe: &mut [u8]) {
  let u32_at =
    |exe: &[u8], at: usize| exe.get(at..at + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
  let Some(opt) = u32_at(exe, 0x3C).map(|pe| pe as usize + 24) else { return };
  let pe32 =
    exe.get(opt - 24..opt - 20) == Some(b"PE\0\0") && exe.get(opt..opt + 2) == Some(&[0x0B, 0x02]);
  // The checksum at 64, the security directory, the fifth, at 112 + 8 * 4.
  if !pe32 || exe.len() < opt + 152 || !exe.starts_with(b"MZ") {
    return;
  }

  exe[opt + 144..opt + 152].fill(0);
  exe[opt + 64..opt + 68].fill(0);
  let mut sum = 0u32;
  for w in exe.chunks(2) {
    sum += u16::from_le_bytes([w[0], w.get(1).copied().unwrap_or(0)]) as u32;
    sum = (sum & 0xFFFF) + (sum >> 16);
  }

  let sum = ((sum + (sum >> 16)) & 0xFFFF).wrapping_add(exe.len() as u32);
  exe[opt + 64..opt + 68].copy_from_slice(&sum.to_le_bytes());
}

/// `n` bytes as NSIS shows a size: KB, MB, GB or TB, one decimal.
fn bytes(n: u64) -> String {
  let mut size = n as f64 / 1024.0;
  let mut unit = 0;
  while size >= 1024.0 && unit < 3 {
    size /= 1024.0;
    unit += 1;
  }

  format!("{size:.1} {}", ["KB", "MB", "GB", "TB"][unit])
}

/// `op` until it succeeds, tried again for `within`; its last error then.
fn retry<T>(within: Duration, mut op: impl FnMut() -> io::Result<T>) -> io::Result<T> {
  let end = Instant::now() + within;
  loop {
    match op() {
      Err(e) if e.kind() != io::ErrorKind::NotFound && Instant::now() < end => {
        thread::sleep(Duration::from_millis(250))
      }
      done => return done,
    }
  }
}

/// What the install tells as it goes: the file it writes and the share of all bytes written, 0 to
/// 1.
type Step<'a> = &'a dyn Fn(&str, f32);

/// Writes `files` into `dir`, made if missing, the app's exe first: each written beside as a new
/// `.<name>.shipo-new-<pid>` and renamed over the old one, tried again while it is in use. Gives
/// their size.
fn put(dir: &Path, files: &[(String, Vec<u8>)], step: Step) -> io::Result<u64> {
  fs::create_dir_all(dir)?;
  let total = files.iter().map(|(_, d)| d.len() as u64).sum::<u64>().max(1) as f32;
  let mut size = 0;
  for (name, data) in files {
    let part = dir.join(format!(".{name}.shipo-new-{}", std::process::id()));
    let path = dir.join(name);
    step(name, size as f32 / total);
    // By the MiB, so a large exe moves the bar.
    let written = fs::File::create_new(&part).and_then(|mut f| {
      for chunk in data.chunks(1 << 20) {
        f.write_all(chunk)?;
        size += chunk.len() as u64;
        step(name, size as f32 / total);
      }

      Ok(())
    });

    if let Err(e) = written {
      let _ = fs::remove_file(&part);
      return Err(e);
    }

    if let Err(e) = retry(RETRY, || move_file(&part, &path)) {
      let _ = fs::remove_file(&part);
      return Err(in_use(&path, e));
    }
  }

  Ok(size)
}

/// Whether the install in `dir` before this one of `info` may have written `LICENSE.txt`; none
/// when there was none. Its uninstaller tells: ours of this app (its payload's [Info], found by
/// `magic`, the placeholder's start), or another's (an NSIS setup's) where the Uninstall key put
/// this app (`location`). One of another app, or another's elsewhere, is an error: `dir` is
/// shared, and its `Uninstall.exe` not ours to replace.
fn previous(
  dir: &Path,
  info: &Info,
  location: Option<&str>,
  magic: &[u8],
) -> io::Result<Option<bool>> {
  let path = dir.join(UNINSTALLER);
  let data = match fs::read(&path) {
    Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
    data => data?,
  };

  let at = data.windows(magic.len()).position(|w| w == magic);
  let theirs = at.and_then(|at| {
    let found = data.get(at..at.checked_add(PLACEHOLDER)?)?.try_into().ok()?;
    own(&data, payload::fields(found)).ok().map(|(info, ..)| info)
  });

  let ours = |l: &str| same(l, dir);
  match theirs {
    Some(theirs) if theirs.app_id == info.app_id => Ok(Some(theirs.license)),
    None if location.is_some_and(ours) => Ok(Some(true)),
    Some(theirs) => Err(io::Error::other(format!("{} is {}'s", path.display(), theirs.name))),
    None => Err(io::Error::other(format!("{} is another app's", path.display()))),
  }
}

/// Writes `files` into `dir` by [put], `LICENSE.txt` removed when the install before wrote one
/// (`licensed`) and this one doesn't; then the Uninstall key by `key` of their size: an install
/// that fails before it leaves no key, one with its files always has one. The shortcut last, by
/// `shortcut`, its failure (a folder past MAX_PATH) not the install's. `step` as [put] has it.
fn install(
  dir: &Path,
  files: &[(String, Vec<u8>)],
  licensed: bool,
  key: impl FnOnce(u64) -> io::Result<()>,
  shortcut: impl FnOnce() -> io::Result<()>,
  step: Step,
) -> io::Result<()> {
  let size = put(dir, files, step)?;
  let license = dir.join(LICENSE);
  if licensed && !files.iter().any(|(n, _)| n == LICENSE) && license.is_file() {
    retry(RETRY, || delete_file(&license))?;
  }

  key(size)?;
  let _ = shortcut();
  Ok(())
}

/// Removes from `dir` the files the setup of `info` wrote, the app's exe first, an error then
/// leaving all as it was; `LICENSE.txt` if it wrote one; the updater's staging folders; then `dir`
/// if it is empty (a `/D=` may share it), each tried however the others went, the first error
/// given. The uninstaller, running, is moved out after the exe, if `moved` can; else it stays, and
/// so does the folder. A named file is only ever a file: a folder of that name stays.
fn remove(dir: &Path, info: &Info, moved: impl FnOnce() -> bool) -> io::Result<()> {
  let exe = dir.join(format!("{}.exe", info.name));
  match retry(RETRY, || delete_file(&exe)) {
    Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(in_use(&exe, e)),
    _ => {}
  }

  let mut files = vec![];
  if moved() {
    files.push(dir.join(UNINSTALLER));
  }

  if info.license {
    files.push(dir.join(LICENSE));
  }

  let mut folders = vec![];
  for entry in fs::read_dir(dir)?.flatten() {
    let staging = entry.file_name().to_string_lossy().starts_with(".shipo-update-");
    match entry.file_type() {
      Ok(t) if staging && t.is_dir() => folders.push(entry.path()),
      Ok(_) if staging => files.push(entry.path()),
      _ => {}
    }
  }

  // A folder of a named file's name is not the setup's.
  files.retain(|f| !fs::symlink_metadata(f).is_ok_and(|m| m.is_dir()));
  let removed = files.iter().map(|f| retry(RETRY, || delete_file(f)));
  let removed = removed.chain(folders.iter().map(|f| retry(RETRY, || fs::remove_dir_all(f))));
  let mut done = Ok(());
  for result in removed {
    match result {
      Err(e) if e.kind() != io::ErrorKind::NotFound => done = done.and(Err(e)),
      _ => {}
    }
  }

  let _ = fs::remove_dir(dir);
  done
}

/// Whether the folder `l`, as the Uninstall key holds it, is `dir`: by their text, case, `/`, a
/// trailing separator and `\\?\` aside, else as the file system resolves them (a mapped drive,
/// 8.3).
fn same(l: &str, dir: &Path) -> bool {
  let plain = |s: &str| {
    let s = s.replace('/', "\\");
    let s = s.trim_end_matches('\\');
    let prefixed = |p: &str| s.get(..p.len()).is_some_and(|h| h.eq_ignore_ascii_case(p));
    if prefixed(r"\\?\UNC\") || prefixed(r"\\.\UNC\") {
      format!(r"\\{}", &s[8..])
    } else if prefixed(r"\\?\") || prefixed(r"\\.\") {
      s[4..].to_string()
    } else {
      s.to_string()
    }
  };

  // Only an absolute folder resolves: a relative one would by the current folder.
  let real = |p: &Path| fs::canonicalize(p).ok();
  plain(l).eq_ignore_ascii_case(&plain(&dir.to_string_lossy()))
    || folder(l).and_then(|l| real(&l)).is_some_and(|l| real(dir) == Some(l))
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
    let parse = |args: &[&str], raw: &str| Args::parse(args.iter().map(OsString::from), raw);
    assert_eq!(parse(&[], r"C:\x\setup.exe"), Args::default());
    assert_eq!(parse(&["/S"], "setup.exe /S"), Args { silent: true, dir: None });
    assert_eq!(parse(&["/s", "/x"], "setup.exe /s /x"), Args { silent: true, dir: None });

    // The rest of the raw line, two spaces kept, however the arguments split it.
    let raw = r#""C:\a b\setup.exe" /S /D=C:\My  Apps\Hello "#;
    let dir = Some(r"C:\My  Apps\Hello".to_string());
    assert_eq!(parse(&["/S", r"/D=C:\My", r"Apps\Hello"], raw), Args { silent: true, dir });
    assert_eq!(parse(&[], r"C:\x/D=y\setup.exe").dir, None);

    let folder = |dir: &str| Args { dir: Some(dir.into()), ..Args::default() }.folder();
    for ok in [r"C:\Apps", "d:/apps", r"\\server\share\Apps"] {
      assert_eq!(folder(ok).unwrap(), Some(PathBuf::from(ok.replace('/', "\\"))), "{ok}");
    }

    // A root never: an uninstall would empty it.
    let roots = [r"C:\", "d:/", r"C:\\", r"\\server\share", r"\\server\share\"];
    for no in ["", "Hello", r"Apps\Hello", "C:", "C:Apps", r"\Apps", r"\\"].into_iter().chain(roots)
    {
      let e = folder(no).unwrap_err().to_string();
      assert_eq!(e, format!("/D= takes an absolute folder, not {no:?}"));
    }
  }

  /// What a newer setup takes from older versions: the key and its `InstallLocation` an install
  /// left, v0.1's NSIS setup's too, where `/S` alone (v0.1's updater) installs; `/S /D=<the app's
  /// folder>`, as later updaters run it, the program's path quoted; and the exit codes they read.
  #[test]
  fn compat_older_installs_and_updaters() {
    assert_eq!(UNINSTALL, r"Software\Microsoft\Windows\CurrentVersion\Uninstall");
    let dir = r"C:\Users\Me\AppData\Local\Programs\My  App";
    let location = ("InstallLocation", Value::Text(dir.into()));
    assert!(values(&info(), Path::new(dir), 1).contains(&location));
    assert_eq!(folder(dir), Some(dir.into()));

    let exe = format!(r#""{dir}\.shipo-update-41-17\Hello-v2-windows-x64-setup.exe""#);
    let parse = |args: &[&str], raw: &str| Args::parse(args.iter().map(OsString::from), raw);
    assert_eq!(parse(&["/S"], &format!("{exe} /S")), Args { silent: true, dir: None });
    let args = parse(
      &["/S", r"/D=C:\Users\Me\AppData\Local\Programs\My", "App"],
      &format!("{exe} /S /D={dir}"),
    );
    assert_eq!(args, Args { silent: true, dir: Some(dir.into()) });

    // `/S` alone installs where the key says, a folder an NSIS setup chose too, as an update.
    let nsis = r"D:\Tools\My App";
    let local = Some(OsString::from(r"C:\Users\Me\AppData\Local"));
    let to = |given, location| destination(given, location, local.clone(), "Hello").unwrap();
    assert_eq!(to(None, Some(nsis)), (PathBuf::from(nsis), true));
    assert_eq!(to(Some(dir.into()), Some(nsis)), (PathBuf::from(dir), false));
    let default = Path::new(r"C:\Users\Me\AppData\Local").join("Programs").join("Hello");
    assert_eq!(to(None, None), (default.clone(), false));
    assert_eq!(to(None, Some("relative")), (default, false));

    // The updater takes 0 as installed, anything else as a failure.
    assert_eq!((CANCELLED, FAILED), (1, 2));
  }

  #[test]
  fn folders() {
    let oks = [
      (r"C:\Apps\", r"C:\Apps"),
      (r"C:\Apps\Hello\\", r"C:\Apps\Hello"),
      (r"\\?\C:\Apps", r"\\?\C:\Apps"),
      (r"\\?\UNC\server\share\Apps\", r"\\?\UNC\server\share\Apps"),
      (r"\\.\unc\server\share\Apps", r"\\.\unc\server\share\Apps"),
      (r"C:\.Apps\a.b", r"C:\.Apps\a.b"),
      ("C:/Apps/Hello/", r"C:\Apps\Hello"),
    ];
    for (dir, want) in oks {
      assert_eq!(folder(dir), Some(PathBuf::from(want)), "{dir}");
    }

    // Roots by another name, and parts Windows drops or climbs by.
    let roots = [r"\\?\UNC\server\share", r"\\.\UNC\server\share", r"\\?\C:\", r"\\?\UNC\"];
    let dots = [r"C:\.", r"C:\..", r"C:\Apps\..", r"C:\ .", r"C:\Apps\. \x", r"\\server\share\."];
    let other = [r"\\?\Volume{x}\Apps", r"\\.\pipe\x", r"\\?\Apps", r"C:\Apps\\x", r"\\server\\x"];
    for dir in roots.into_iter().chain(dots).chain(other) {
      assert_eq!(folder(dir), None, "{dir}");
    }
  }

  #[test]
  fn same_folders() {
    let dir = Path::new(r"C:\Apps\Hello");
    assert!(same(r"C:\Apps\Hello", dir) && same(r"c:\apps\hello\", dir));
    assert!(same("C:/Apps/Hello/", dir) && same(r"\\?\C:\Apps\Hello", dir));
    assert!(same(r"\\?\UNC\srv\share\x", Path::new(r"\\srv\share\x")));
    assert!(same(r"\\.\C:\Apps\Hello", dir));
    assert!(same(r"\\.\UNC\srv\share\x", Path::new(r"\\srv\share\x")));
    // A relative folder never matches by the current folder.
    let here = std::env::current_dir().unwrap();
    assert!(!same(".", &here) && !same("", &here));
    assert!(!same(r"C:\Users\Me\AppData\Local\Temp", dir) && !same(r"C:\Apps", dir));
  }

  #[test]
  fn sizes() {
    let cases = [(0, "0.0 KB"), (1536, "1.5 KB"), (12 << 20, "12.0 MB"), (5 << 40, "5.0 TB")];
    for (n, want) in cases {
      assert_eq!(bytes(n), want);
    }

    assert_eq!(bytes(1023 << 20), "1023.0 MB");
    assert_eq!(bytes(3 << 30), "3.0 GB");
  }

  #[test]
  fn guids() {
    assert!(guid("{a90596b0-07fa-55a1-9740-d4470c5cef1c}"));
    for no in [
      "",
      "{}",
      "a90596b0-07fa-55a1-9740-d4470c5cef1c",
      "{a90596b0-07fa-55a1-9740-d4470c5cef1g}",
      r"{a90596b0-07fa-55a1-9740-d4470c5c\..}",
    ] {
      assert!(!guid(no), "{no}");
    }
  }

  #[test]
  fn uninstaller_before() {
    let dir = tmp();
    let magic = &payload::placeholder()[..payload::MAGIC.len()];
    let licensed = Info { license: true, ..info() };
    let other = Info {
      name: "Other".into(),
      app_id: "{00000000-07fa-55a1-9740-d4470c5cef1c}".into(),
      ..info()
    };
    let ours = |info: &Info| {
      let stub = [b"MZ".to_vec(), vec![0; 20], payload::placeholder().to_vec()].concat();
      uninstaller(&stub, 22, info).unwrap()
    };

    let location = dir.display().to_string();
    let before = |data: Option<Vec<u8>>, location: Option<&str>| {
      let _ = fs::remove_file(dir.join(UNINSTALLER));
      if let Some(data) = data {
        fs::write(dir.join(UNINSTALLER), data).unwrap();
      }

      previous(&dir, &info(), location, magic).map_err(|e| e.to_string())
    };

    assert_eq!(before(None, None), Ok(None));
    assert_eq!(before(Some(ours(&licensed)), None), Ok(Some(true)));
    assert_eq!(before(Some(ours(&info())), None), Ok(Some(false)));
    assert!(before(Some(ours(&other)), Some(&location)).unwrap_err().ends_with("is Other's"));
    // An NSIS setup's, where the key put this app, a trailing slash or another case its own.
    let nsis = Some(b"MZ nsis".to_vec());
    assert_eq!(
      before(nsis.clone(), Some(&format!("{}\\", location.to_uppercase()))),
      Ok(Some(true))
    );
    assert!(before(nsis.clone(), Some(r"C:\Elsewhere")).unwrap_err().ends_with("is another app's"));
    assert!(before(nsis, None).is_err());
  }

  #[test]
  fn install_order() {
    let dir = tmp().join("Hello");
    let files = vec![("Hello.exe".to_string(), b"v1".to_vec())];
    let steps = std::cell::RefCell::new(vec![]);
    let key = |size| {
      steps.borrow_mut().push(format!("key {size} {}", dir.join("Hello.exe").exists()));
      Ok(())
    };

    // A shortcut that fails leaves the install whole.
    let lnk = || {
      steps.borrow_mut().push("shortcut".into());
      Err(io::Error::other("past MAX_PATH"))
    };

    install(&dir, &files, false, key, lnk, &|_, _| {}).unwrap();
    assert_eq!(*steps.borrow(), ["key 2 true", "shortcut"]);

    // An install before wrote a licence this one doesn't: it goes; a folder of its name stays.
    fs::write(dir.join(LICENSE), "MIT").unwrap();
    install(&dir, &files, true, |_| Ok(()), || Ok(()), &|_, _| {}).unwrap();
    assert_eq!(listed(&dir), ["Hello.exe"]);
    fs::create_dir(dir.join(LICENSE)).unwrap();
    install(&dir, &files, true, |_| Ok(()), || Ok(()), &|_, _| {}).unwrap();
    assert_eq!(listed(&dir), ["Hello.exe", LICENSE]);

    // A key that fails fails the install, before the shortcut.
    steps.borrow_mut().clear();
    let no_key = |_| Err(io::Error::other("no key"));
    let e = install(&dir, &files, false, no_key, || panic!("shortcut"), &|_, _| {});
    assert_eq!(e.unwrap_err().to_string(), "no key");

    // Files that can't be written make no key.
    let file = tmp().join("file");
    fs::write(&file, "").unwrap();
    let e =
      install(&file.join("x"), &files, false, |_| panic!("key"), || panic!("lnk"), &|_, _| {});
    assert!(e.is_err());
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
    assert!(uninstaller(stub, u64::MAX, &info()).is_err());
  }

  #[test]
  fn uninstaller_unsigned() {
    // A PE32+ header at 0x40, its optional header at 0x58, a security directory and a checksum.
    let mut stub = vec![0; 0x200];
    stub[..2].copy_from_slice(b"MZ");
    stub[0x3C] = 0x40;
    stub[0x40..0x44].copy_from_slice(b"PE\0\0");
    stub[0x58..0x5A].copy_from_slice(&[0x0B, 0x02]);
    stub[0x58 + 144..0x58 + 152].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
    stub[0x58 + 64..0x58 + 68].copy_from_slice(&[9; 4]);
    stub[0x100..0x100 + PLACEHOLDER].copy_from_slice(&payload::placeholder());

    let un = uninstaller(&stub, 0x100, &info()).unwrap();
    assert_eq!(un[0x58 + 144..0x58 + 152], [0; 8]);
    let mut zeroed = un.clone();
    zeroed[0x58 + 64..0x58 + 68].fill(0);
    let sum = u32::from_le_bytes(un[0x58 + 64..0x58 + 68].try_into().unwrap());
    let mut again = zeroed.clone();
    unsigned(&mut again);
    assert_eq!(u32::from_le_bytes(again[0x58 + 64..0x58 + 68].try_into().unwrap()), sum);
    assert_ne!(sum, 0x0909_0909);

    // Anything but a PE32+ file stays as it is.
    let mut other = b"MZ not a pe".to_vec();
    unsigned(&mut other);
    assert_eq!(other, b"MZ not a pe");
  }

  #[test]
  fn files_put_and_removed() {
    let dir = tmp().join("Hello");
    let licensed = Info { license: true, ..info() };
    let files = [("Hello.exe", "v1"), (LICENSE, "MIT"), (UNINSTALLER, "un")];
    let files: Vec<_> = files.iter().map(|(n, d)| (n.to_string(), d.as_bytes().to_vec())).collect();
    let steps = std::cell::RefCell::new(vec![]);
    let step = |name: &str, part: f32| steps.borrow_mut().push((name.to_string(), part * 7.0));
    assert_eq!(put(&dir, &files, &step).unwrap(), 7);
    // Each file as it starts and as each MiB of it is written, by the share of all bytes.
    let want = [("Hello.exe", 0.0), ("Hello.exe", 2.0), (LICENSE, 2.0), (LICENSE, 5.0)];
    let want = want.into_iter().chain([(UNINSTALLER, 5.0), (UNINSTALLER, 7.0)]);
    assert_eq!(*steps.borrow(), want.map(|(n, p)| (n.to_string(), p)).collect::<Vec<_>>());
    assert_eq!(listed(&dir), ["Hello.exe", LICENSE, UNINSTALLER]);
    assert_eq!(put(&dir, &[("Hello.exe".into(), b"v2".to_vec())], &|_, _| {}).unwrap(), 2);
    assert_eq!(fs::read(dir.join("Hello.exe")).unwrap(), b"v2");

    // The staging folders go, the app's data and what a `/D=` shares stay.
    fs::create_dir_all(dir.join(".shipo-update-1-2/x")).unwrap();
    fs::write(dir.join("data.txt"), "kept").unwrap();
    remove(&dir, &licensed, || true).unwrap();
    assert_eq!(listed(&dir), ["data.txt"]);

    // An uninstaller that can't move out stays, with its folder.
    fs::remove_file(dir.join("data.txt")).unwrap();
    put(&dir, &files, &|_, _| {}).unwrap();
    remove(&dir, &licensed, || false).unwrap();
    assert_eq!(listed(&dir), [UNINSTALLER]);
    remove(&dir, &licensed, || true).unwrap();
    assert!(!dir.exists());

    // A folder shared by `/D=`: another app's licence and uninstaller, and a folder named as a
    // file, stay; a staging file goes as a file.
    put(&dir, &files[..1], &|_, _| {}).unwrap();
    fs::write(dir.join(LICENSE), "theirs").unwrap();
    fs::create_dir_all(dir.join("Uninstall.exe/kept")).unwrap();
    fs::write(dir.join(".shipo-update-file"), "").unwrap();
    remove(&dir, &info(), || true).unwrap();
    assert_eq!(listed(&dir), [LICENSE, UNINSTALLER]);
    fs::remove_dir_all(&dir).unwrap();

    // An exe in use stops it before anything changed (a folder stands for one here).
    put(&dir, &files, &|_, _| {}).unwrap();
    fs::remove_file(dir.join("Hello.exe")).unwrap();
    fs::create_dir_all(dir.join("Hello.exe/x")).unwrap();
    assert!(remove(&dir, &licensed, || panic!("moved")).is_err());
    assert_eq!(listed(&dir), ["Hello.exe", LICENSE, UNINSTALLER]);
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
