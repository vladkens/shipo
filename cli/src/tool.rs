// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! The programs the packer runs: a command whose failure is an error with what it printed, the
//! pinned tools it fetches by the system's `curl`, checked by their SHA-256 and unpacked by our own
//! tar reader, and the temporary folders and private files their secrets go through.

use std::fmt::Write;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use crate::{bad, tar};

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
    io::ErrorKind::NotFound => io::Error::new(e.kind(), format!("{} not found", program(cmd))),
    _ => io::Error::new(e.kind(), format!("{}: {e}", program(cmd))),
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
/// time. One of another hash is removed and an error; a download goes to `<path>.part` first, so a
/// broken one never stays at `path`. A stalled connection fails, and any error is tried again
/// three times.
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

  let part = path.with_file_name(format!("{name}.part"));
  let _ = fs::remove_file(&part);
  let mut curl = Command::new("curl");
  curl.args(["-fsSL", "--connect-timeout", "30", "--speed-limit", "1024", "--speed-time", "60"]);
  curl.args(["--retry", "3", "--retry-all-errors", "-o"]);
  let got = run(curl.arg(&part).arg(url));
  let got = got.and_then(|_| check(&part, sha256, &name));
  if let Err(e) = got {
    let _ = fs::remove_file(&part);
    return Err(io::Error::new(e.kind(), format!("{url}: {e}")));
  }

  fs::rename(&part, path)?;
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
pub fn sha256(path: &Path) -> io::Result<String> {
  let mut file = File::open(path)?;
  let (mut hash, mut buf) = (hmac_sha256::Hash::new(), vec![0; 1 << 16]);
  loop {
    let n = file.read(&mut buf)?;
    if n == 0 {
      break;
    }

    hash.update(&buf[..n]);
  }

  let mut hex = String::new();
  for b in hash.finalize() {
    let _ = write!(hex, "{b:02x}");
  }

  Ok(hex)
}

/// The file `member` of the gzipped tarball `tarball` at `path` (0755): written beside it and
/// renamed over it, so a copy that runs is never rewritten.
pub fn untar(tarball: &Path, member: &str, path: &Path) -> io::Result<PathBuf> {
  let at = |e: io::Error| io::Error::new(e.kind(), format!("{}: {e}", tarball.display()));
  let files = tar::files(&fs::read(tarball).map_err(at)?).map_err(at)?;
  let Some((_, data)) = files.into_iter().find(|(name, _)| name == member) else {
    return Err(bad(format!("{}: no {member}", tarball.display())));
  };

  let name = path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
  let part = path.with_file_name(format!("{name}.part"));
  let _ = fs::remove_file(&part);
  fs::write(&part, data)?;
  set_mode(&part, 0o755)?;
  fs::rename(&part, path)?;
  Ok(path.to_path_buf())
}

// MARK: Files

/// A new folder in the temp dir only its owner reads, removed with all it holds on drop.
pub struct Temp(PathBuf);

impl Temp {
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

/// `data` in a new file at `path` only its owner reads: a key, a certificate.
pub fn private(path: &Path, data: &[u8]) -> io::Result<PathBuf> {
  let mut opts = OpenOptions::new();
  opts.write(true).create_new(true);
  #[cfg(unix)]
  std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
  io::Write::write_all(&mut opts.open(path)?, data)?;
  Ok(path.to_path_buf())
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

#[cfg(test)]
mod tests {
  use super::*;

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
