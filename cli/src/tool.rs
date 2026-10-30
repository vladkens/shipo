// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! The programs the packer runs: a command whose failure is an error with what it printed, the
//! pinned tools it fetches by the system's `curl`, checked by their SHA-256 and unpacked from their
//! tarballs; and the files the packer writes: put in place whole, private for secrets, in
//! temporary folders.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use flate2::read::MultiGzDecoder;

use crate::{at, bad, wrap};

/// The secrets a release job holds, kept from every program the packer runs: one that needs a
/// value gets it under a name of its own.
const SECRETS: [&str; 6] = [
  "APPLE_CERTIFICATE",
  "APPLE_CERTIFICATE_PASSWORD",
  "APPLE_API_KEY",
  "APPLE_API_KEY_ID",
  "APPLE_API_ISSUER",
  "SHIPO_UPDATE_KEY",
];

/// `cmd` with its stdin null and without [SECRETS].
fn quiet(cmd: &mut Command) -> &mut Command {
  for name in SECRETS {
    cmd.env_remove(name);
  }

  cmd.stdin(Stdio::null())
}

/// Runs `cmd` to its end, its stdin null and without [SECRETS], and gives its stdout; an error
/// names the program and holds what it wrote to stderr.
pub fn run(cmd: &mut Command) -> io::Result<Vec<u8>> {
  let out = quiet(cmd).output().map_err(|e| spawn_error(cmd, e))?;
  match out.status.success() {
    true => Ok(out.stdout),
    false => Err(failed(cmd, &out.status.to_string(), &out.stderr)),
  }
}

/// [run] that kills `cmd` after `limit`: a keychain prompt nobody answers stops `codesign`.
pub fn run_until(cmd: &mut Command, limit: Duration) -> io::Result<Vec<u8>> {
  let cmd = quiet(cmd).stdout(Stdio::piped()).stderr(Stdio::piped());
  let mut child = cmd.spawn().map_err(|e| spawn_error(cmd, e))?;
  // Read apart, so a full pipe never blocks the program.
  let (out, err) = (child.stdout.take().map(drain), child.stderr.take().map(drain));
  let end = Instant::now() + limit;
  let status = loop {
    if let Some(status) = child.try_wait()? {
      break Some(status);
    }

    if Instant::now() >= end {
      child.kill()?;
      child.wait()?;
      break None;
    }

    thread::sleep(Duration::from_millis(20));
  };

  let joined = |h: Option<thread::JoinHandle<_>>| h.and_then(|h| h.join().ok()).unwrap_or_default();
  let (out, err) = (joined(out), joined(err));
  match status {
    Some(s) if s.success() => Ok(out),
    Some(s) => Err(failed(cmd, &s.to_string(), &err)),
    None => {
      let after = match limit.as_secs() {
        0 => format!("{} ms", limit.as_millis()),
        secs => format!("{secs} s"),
      };

      Err(failed(cmd, &format!("killed after {after}"), &err))
    }
  }
}

/// A thread reading `stream` to its end.
fn drain(mut stream: impl Read + Send + 'static) -> thread::JoinHandle<Vec<u8>> {
  thread::spawn(move || {
    let mut buf = Vec::new();
    let _ = stream.read_to_end(&mut buf);
    buf
  })
}

/// The program's name in `cmd`, for errors.
fn program(cmd: &Command) -> String {
  cmd.get_program().to_string_lossy().into_owned()
}

fn spawn_error(cmd: &Command, e: io::Error) -> io::Error {
  match e.kind() {
    io::ErrorKind::NotFound => wrap(&e, format!("{} not found", program(cmd))),
    _ => wrap(&e, format!("{}: {e}", program(cmd))),
  }
}

fn failed(cmd: &Command, status: &str, stderr: &[u8]) -> io::Error {
  let said = String::from_utf8_lossy(stderr);
  let said = said.trim();
  let mut msg = format!("{} failed ({status})", program(cmd));
  if !said.is_empty() {
    msg += &format!(": {said}");
  }

  io::Error::other(msg)
}

// MARK: Pinned tools

/// `url` into `path`, downloaded once by `curl`, the file checked against `sha256` (hex) each
/// time. One of another hash is removed and an error; a download is [place]d, so a broken one
/// never stays at `path`. A stalled connection fails, and any error is tried again three times.
pub fn fetch(url: &str, sha256: &str, path: &Path) -> io::Result<PathBuf> {
  let name = path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
  if path.exists() {
    if let Err(e) = check(path, sha256, &name) {
      fs::remove_file(path)?;
      return Err(e);
    }

    return Ok(path.to_path_buf());
  }

  if let Some(dir) = path.parent() {
    fs::create_dir_all(dir)?;
  }

  let mut curl = Command::new("curl");
  curl.args(["-fsSL", "--connect-timeout", "30", "--speed-limit", "1024", "--speed-time", "60"]);
  curl.args(["--retry", "3", "--retry-all-errors", "-o"]);
  let got = place(path, |part| {
    run(curl.arg(part).arg(url))?;
    check(part, sha256, &name)
  });

  got.map_err(|e| wrap(&e, format!("{url}: {e}")))?;
  Ok(path.to_path_buf())
}

/// Checks that the file at `path`, `name` in the error, has the SHA-256 `want` (hex).
fn check(path: &Path, want: &str, name: &str) -> io::Result<()> {
  let got = sha256(path)?;
  match got.eq_ignore_ascii_case(want) {
    true => Ok(()),
    false => Err(bad(format!("{name}: SHA-256 {got}, pinned {want}"))),
  }
}

/// The SHA-256 of the file at `path`, in hex.
fn sha256(path: &Path) -> io::Result<String> {
  Ok(hex(&hmac_sha256::Hash::hash(&read(path)?)))
}

/// The file `member` of the gzipped tarball `tarball` at `path` (0755), [place]d: renamed over
/// it, so a copy that runs is never rewritten.
pub fn untar(tarball: &Path, member: &str, path: &Path) -> io::Result<PathBuf> {
  let files = tar_files(&read(tarball)?).map_err(at(tarball))?;
  let Some((_, data)) = files.into_iter().find(|(name, _)| name == member) else {
    return Err(bad(format!("{}: no {member}", tarball.display())));
  };

  place(path, |part| {
    File::create_new(part)?.write_all(&data)?;
    set_mode(part, 0o755)
  })?;
  Ok(path.to_path_buf())
}

/// The regular files of the gzipped tarball `gz`, each by its path without a leading `./`; a path
/// that is absolute or holds `..` is an error, whatever its type, and so is a cut archive.
fn tar_files(gz: &[u8]) -> io::Result<Vec<(String, Vec<u8>)>> {
  let mut out = vec![];
  for entry in tar::Archive::new(MultiGzDecoder::new(gz)).entries()? {
    let mut entry = entry?;
    let name = String::from_utf8_lossy(&entry.path_bytes()).into_owned();
    let name = in_folder(&name)?;
    // `7` is a contiguous file, read as a regular one.
    if !matches!(entry.header().entry_type(), tar::EntryType::Regular | tar::EntryType::Continuous)
    {
      continue;
    }

    // The crate reads a cut body short without an error.
    let mut data = vec![];
    entry.read_to_end(&mut data)?;
    if data.len() as u64 != entry.size() {
      return Err(bad("the tarball is cut short"));
    }

    out.push((name, data));
  }

  Ok(out)
}

/// `name` without its leading `./`; an error when it is absolute or holds `..`.
fn in_folder(name: &str) -> io::Result<String> {
  let mut rest = name;
  while let Some(r) = rest.strip_prefix("./") {
    rest = r;
  }

  match rest.starts_with('/') || rest.split('/').any(|part| part == "..") {
    true => Err(bad(format!("the tarball holds {name:?}, outside its folder"))),
    false => Ok(rest.to_string()),
  }
}

// MARK: Files

/// A new folder in the temp dir only its owner reads, removed with all it holds on drop.
pub struct Temp(PathBuf);

impl Temp {
  /// A new folder of `files`, each `(path, text)`, its folders made.
  #[cfg(test)]
  pub fn files(files: &[(&str, &str)]) -> Self {
    let dir = Self::new().unwrap();
    for (path, text) in files {
      let path = dir.join(path);
      fs::create_dir_all(path.parent().unwrap()).unwrap();
      fs::write(path, text).unwrap();
    }

    dir
  }

  pub fn new() -> io::Result<Self> {
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
    let n = COUNT.fetch_add(1, Ordering::Relaxed);
    let name = format!("shipo-{}-{}-{n}", std::process::id(), nanos.as_nanos());
    let path = std::env::temp_dir().join(name);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut fs::DirBuilder::new(), 0o700).create(&path)?;
    #[cfg(not(unix))]
    fs::create_dir(&path)?;
    Ok(Self(path))
  }
}

impl std::ops::Deref for Temp {
  type Target = Path;
  fn deref(&self) -> &Path {
    &self.0
  }
}

impl Drop for Temp {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.0);
  }
}

/// `data` in a new file at `path` only its owner reads: a key, a certificate. A file there is an
/// error, never written over; one this fails to write is removed.
pub fn private(path: &Path, data: &[u8]) -> io::Result<PathBuf> {
  let mut opts = OpenOptions::new();
  opts.write(true).create_new(true);
  #[cfg(unix)]
  std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
  let mut file = opts.open(path)?;
  if let Err(e) = file.write_all(data) {
    let _ = fs::remove_file(path);
    return Err(at(path)(e));
  }

  Ok(path.to_path_buf())
}

/// The file `out`, made by `make` at `<out>.part` and renamed into place. A part there before, or
/// a link there, is removed first, so `make` makes a new file (`create_new`), never one written
/// through; on any error the part is removed, so no folder a release uploads or the apps fetch
/// from holds half a file.
pub fn place(out: &Path, make: impl FnOnce(&Path) -> io::Result<()>) -> io::Result<()> {
  let mut name = out.file_name().unwrap_or_default().to_os_string();
  name.push(".part");
  let part = out.with_file_name(name);
  match fs::remove_file(&part) {
    Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(at(&part)(e)),
    _ => {}
  }

  let made = make(&part).and_then(|()| fs::rename(&part, out).map_err(at(out)));
  if made.is_err() {
    let _ = fs::remove_file(&part);
  }

  made
}

/// The bytes of the file at `path`, an error naming it.
pub fn read(path: &Path) -> io::Result<Vec<u8>> {
  fs::read(path).map_err(at(path))
}

/// Sets the Unix mode of `path`; nothing elsewhere.
pub fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
  }

  #[cfg(not(unix))]
  {
    let _ = (path, mode);
    Ok(())
  }
}

// MARK: Bytes

/// `bytes` in lowercase hex.
pub fn hex(bytes: &[u8]) -> String {
  bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The `N` bytes of `data` at `at`, if it holds them; a number's `from_le_bytes` or
/// `from_be_bytes` reads them.
pub fn get<const N: usize>(data: &[u8], at: usize) -> Option<[u8; N]> {
  data.get(at..at.checked_add(N)?)?.try_into().ok()
}

#[cfg(test)]
mod tests {
  use flate2::Compression;
  use flate2::write::GzEncoder;

  use super::*;

  /// A ustar entry of the raw name `name` and the type `kind` holding `body`, padded to a block.
  fn entry(name: &str, kind: u8, body: &[u8]) -> Vec<u8> {
    let mut head = tar::Header::new_ustar();
    head.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
    head.set_mode(0o755);
    head.set_size(body.len() as u64);
    head.set_entry_type(tar::EntryType::new(kind));
    head.set_cksum();

    let mut out = head.as_bytes().to_vec();
    out.extend(body);
    out.resize(out.len().next_multiple_of(512), 0);
    out
  }

  fn gz(data: &[u8]) -> Vec<u8> {
    let mut enc = GzEncoder::new(vec![], Compression::default());
    enc.write_all(data).unwrap();
    enc.finish().unwrap()
  }

  #[test]
  fn tarball() {
    let long = format!("{}/tool", "d".repeat(120));
    let entries = [
      entry("./d/", b'5', b""),
      entry("./d/tool", b'0', b"tool"),
      entry("d/link", b'2', b""),
      entry("././@LongLink", b'L', long.as_bytes()),
      entry("cut", b'0', b"long"),
      entry("PaxHeader", b'x', b"17 path=pax/tool\n"),
      entry("cut", b'0', b"pax"),
      entry("d/empty", 0, b""),
      entry("d/contiguous", b'7', b"c"),
      vec![0; 1024],
    ];

    let want = [
      ("d/tool", "tool"),
      (long.as_str(), "long"),
      ("pax/tool", "pax"),
      ("d/empty", ""),
      ("d/contiguous", "c"),
    ];
    let want: Vec<_> = want.map(|(n, d)| (n.to_string(), d.as_bytes().to_vec())).into();
    assert_eq!(tar_files(&gz(&entries.concat())).unwrap(), want);

    for name in ["/etc/passwd", "d/../../x", "..", "./../x"] {
      let e = tar_files(&gz(&entry(name, b'0', b"x"))).unwrap_err().to_string();
      assert_eq!(e, format!("the tarball holds {name:?}, outside its folder"));
    }

    // A link out of the folder is refused too, though only regular files are taken.
    assert!(tar_files(&gz(&entry("../link", b'2', b""))).is_err());

    let mut wrong = entry("x", b'0', b"x");
    wrong[0] = b'y';
    assert!(tar_files(&gz(&wrong)).is_err());

    // Cut inside a body, inside its padding, and inside the next header.
    let full = [entry("x", b'0', &[1; 600]), entry("y", b'0', b"y")].concat();
    for at in [512 + 100, 512 + 700, 1536 + 100] {
      assert!(tar_files(&gz(&full[..at])).is_err(), "cut at {at}");
    }

    assert!(tar_files(b"not gzip").is_err());
  }

  #[test]
  fn hash() {
    let tmp = Temp::new().unwrap();
    let path = tmp.join("abc");
    fs::write(&path, "abc").unwrap();
    let want = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    assert_eq!(sha256(&path).unwrap(), want);
  }

  #[cfg(unix)]
  #[test]
  fn fetched() {
    let tmp = Temp::new().unwrap();
    let src = tmp.join("src.bin");
    fs::write(&src, "pinned").unwrap();
    let url = format!("file://{}", src.display());
    let good = sha256(&src).unwrap();
    let tools = tmp.join("tools");
    let listed = || fs::read_dir(&tools).unwrap().count();

    let bad = "00".repeat(32);
    let e = fetch(&url, &bad, &tools.join("tool")).unwrap_err().to_string();
    assert!(e.contains(&format!("tool: SHA-256 {good}, pinned {bad}")), "{e}");
    assert_eq!(listed(), 0);

    let e = fetch(&format!("{url}.none"), &good, &tools.join("tool")).unwrap_err().to_string();
    assert!(e.contains("curl failed"), "{e}");
    assert_eq!(listed(), 0);

    let path = fetch(&url, &good.to_uppercase(), &tools.join("tool")).unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"pinned");
    assert_eq!(listed(), 1);

    // Fetched once: a changed source is not read again, a changed file is refused and removed.
    fs::write(&src, "other").unwrap();
    assert_eq!(fetch(&url, &good, &path).unwrap(), path);
    fs::write(&path, "tampered").unwrap();
    assert!(fetch(&url, &good, &path).unwrap_err().to_string().contains("pinned"));
    assert_eq!(listed(), 0);
  }

  #[cfg(unix)]
  #[test]
  fn commands() {
    assert_eq!(run(Command::new("sh").args(["-c", "printf hi"])).unwrap(), b"hi");
    let e = run(Command::new("sh").args(["-c", "echo no >&2; exit 3"])).unwrap_err();
    assert_eq!(e.to_string(), "sh failed (exit status: 3): no");
    let e = run(&mut Command::new("shipo-no-such-tool")).unwrap_err();
    assert_eq!(e.to_string(), "shipo-no-such-tool not found");

    let long = Duration::from_secs(5);
    let big = "head -c 300000 /dev/zero; echo said >&2";
    assert_eq!(run_until(Command::new("sh").args(["-c", big]), long).unwrap().len(), 300_000);
    let e = run_until(Command::new("sh").args(["-c", "exit 1"]), long).unwrap_err();
    assert_eq!(e.to_string(), "sh failed (exit status: 1)");
    let e = run_until(Command::new("sleep").arg("10"), Duration::from_millis(100)).unwrap_err();
    assert_eq!(e.to_string(), "sleep failed (killed after 100 ms)");

    // The secrets never reach a child, even set on its command.
    let mut env = Command::new("sh");
    env.args(["-c", "echo ${APPLE_CERTIFICATE-none} ${SHIPO_UPDATE_KEY-none} $SHIPO_P12_PASSWORD"]);
    env.env("APPLE_CERTIFICATE", "c").env("SHIPO_UPDATE_KEY", "k").env("SHIPO_P12_PASSWORD", "p");
    assert_eq!(run(&mut env).unwrap(), b"none none p\n");
    assert_eq!(run_until(&mut env, long).unwrap(), b"none none p\n");
  }

  #[test]
  fn placed() {
    let tmp = Temp::new().unwrap();
    let out = tmp.join("App.deb");
    let listed = || {
      let mut names: Vec<_> = fs::read_dir(&*tmp)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
      names.sort();
      names
    };

    // Half written, then failed: no part is left, nor the file.
    let e = place(&out, |part| {
      fs::write(part, "half")?;
      Err(io::Error::other("disk full"))
    });
    assert_eq!(e.unwrap_err().to_string(), "disk full");
    assert!(listed().is_empty());

    place(&out, |part| fs::write(part, "whole")).unwrap();
    assert_eq!(listed(), ["App.deb"]);
    assert_eq!(fs::read_to_string(&out).unwrap(), "whole");

    // A failed one leaves the one before as it was.
    assert!(place(&out, |part| fs::write(part, "half").and(Err(io::Error::other("no")))).is_err());
    assert_eq!(listed(), ["App.deb"]);
    assert_eq!(fs::read_to_string(&out).unwrap(), "whole");
  }

  #[cfg(unix)]
  #[test]
  fn files() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = Temp::new().unwrap();
    let path = tmp.to_path_buf();
    assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o700);
    let key = private(&tmp.join("key"), b"k").unwrap();
    assert_eq!(fs::metadata(&key).unwrap().permissions().mode() & 0o777, 0o600);
    assert!(private(&key, b"again").is_err());

    let tar = tmp.join("t.tar.gz");
    fs::create_dir(tmp.join("d")).unwrap();
    fs::write(tmp.join("d/tool"), "x").unwrap();
    run(Command::new("tar").arg("-czf").arg(&tar).arg("-C").arg(&*tmp).arg("d")).unwrap();
    let out = untar(&tar, "d/tool", &tmp.join("tool")).unwrap();
    assert_eq!(fs::read(&out).unwrap(), b"x");
    assert_eq!(fs::metadata(&out).unwrap().permissions().mode() & 0o777, 0o755);
    assert!(untar(&tar, "d/none", &tmp.join("none")).is_err());

    // Put in place by a rename: a copy open before keeps its own file.
    let before = fs::File::open(&out).unwrap();
    fs::write(tmp.join("d/tool"), "y").unwrap();
    run(Command::new("tar").arg("-czf").arg(&tar).arg("-C").arg(&*tmp).arg("d")).unwrap();
    untar(&tar, "d/tool", &out).unwrap();
    assert_eq!(io::read_to_string(before).unwrap(), "x");
    assert_eq!(fs::read(&out).unwrap(), b"y");
    assert!(!tmp.join("tool.part").exists());

    drop(tmp);
    assert!(!path.exists());
  }
}
