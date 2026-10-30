// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! In-place updates for desktop apps from a signed feed, as Sparkle does: [check] fetches the feed
//! the shipo packer writes and gives the newer [Release] for this platform, [download] fetches its
//! file beside the running app, verifies and stages it, [install] hands it to an installer that
//! puts it in once the app has quit, and [finish], first in `main`, is that installer. They block:
//! an app runs them on a thread of its own and posts the result back.
//!
//! The library has no UI and no state of its own: the app draws the menu item, the popup, the
//! progress and the setting, and keeps the check time and a skipped version. It never shows a
//! window and never takes the focus. `spec.md` is the contract with the packer: the feed, the
//! release files, where each OS's app is and how it is replaced.
//!
//! ```no_run
//! use std::io;
//!
//! /// The feed and the public keys `shipo keygen` printed, the working one and the spare.
//! const FEED: shipo::Feed = shipo::Feed::new(
//!   "https://github.com/me/app/releases/latest/download/update.txt",
//!   &[
//!     "6bf026c9174d547c1d68f3ec8267273426167628161897f3661a4677a6c24b6f",
//!     "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a",
//!   ],
//! );
//!
//! /// Run on a thread of the app's own when the user asks; the app quits after it, within 30 s.
//! fn update() -> io::Result<()> {
//!   let Some(release) = shipo::check(&FEED, env!("CARGO_PKG_VERSION"))? else { return Ok(()) };
//!   let staged = shipo::download(&release, |done, total| println!("{done} of {total}"))?;
//!   shipo::install(staged)
//! }
//!
//! fn main() {
//!   // First, before the arguments: the installer runs here and exits.
//!   if let Some(e) = shipo::finish() {
//!     eprintln!("the update failed: {e}");
//!   }
//!
//!   // The app.
//! }
//! ```
//!
//! The file of a release is the one people download for the first install: the notarized `.dmg`,
//! the per-user setup, the AppImage. On macOS the grants (microphone, Accessibility) survive an
//! update only when every release is signed with one stable identity: an ad-hoc signature's
//! requirement is its own build, so [download] refuses the next one.

mod feed;
mod installer;
mod platform;
#[cfg(test)]
mod testing;

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use std::{env, fmt, fs, io};

pub use feed::{Feed, Release};
use installer::{Folder, INSTALL, NOTE, READY, home};
use platform::imp;

// MARK: Public API

/// A release's app downloaded, verified and staged beside the running one, for [install]; its
/// folder is removed on drop, unless [install] handed it to the installer.
#[derive(Debug)]
pub struct Staged {
  /// The new app: the `.app` bundle copied from the image, the setup, the AppImage.
  new: PathBuf,
  /// The app it replaces ([target]).
  target: PathBuf,
  dir: Folder,
}

// What an app's thread sends back to its main one.
const _: () = {
  const fn send<T: Send>() {}
  send::<Staged>();
  send::<Release>();
};

/// The release `feed` offers this platform when it is newer than `current`: the feed fetched over
/// the OS's HTTP, then [Feed::read]; `None` when it is up to date. Nothing is fetched for a
/// pre-release `current` (a dev build, `0.1.0-5-gabc`: `Unsupported`) or a feed of no key or a
/// malformed one (`InvalidInput`).
pub fn check(feed: &Feed, current: &str) -> io::Result<Option<Release>> {
  if current.contains('-') {
    let what = format!("{current} is a dev build, which takes no update");
    return Err(error(io::ErrorKind::Unsupported, what));
  }

  if !feed::secure(feed.url) {
    return Err(bad(format!("{} is not https", feed.url)));
  }

  feed.public_keys()?;
  let dir = Folder::new(&env::temp_dir())?;
  let to = dir.0.join("update.txt");
  imp::get(feed.url, &to, feed::MAX, &mut |_| {})?;
  feed.read(&fs::read(to)?, current)
}

/// Whether the running app can update in place, checked with no network, so an app can hide its
/// update up front: not without an installed app to replace (`Unsupported`: a build, a Windows exe
/// outside `%LOCALAPPDATA%\Programs`, a Linux one not run as an AppImage, as a .deb's in /usr) or
/// one whose folder the user can't write (`PermissionDenied`: `/Applications` for a standard user,
/// `/opt`, a translocated app).
pub fn supported() -> io::Result<()> {
  installed().map(drop)
}

/// The app an update replaces ([target]), its [home] writable: a staging folder made there.
fn installed() -> io::Result<PathBuf> {
  let target = target(OS, &env::current_exe()?, &|k| env::var_os(k))?;
  Folder::new(home(OS, &target))?;
  Ok(target)
}

/// Downloads `release`'s file beside the running app, checks its SHA-512 (`InvalidData` if it
/// differs) and stages it: on macOS the one `.app` on the image (`InvalidData` for more or none),
/// held to the running bundle's id and designated requirement. Before a byte is fetched it fails
/// as [supported] does and removes what an installer stopped midway left.
///
/// `progress` gets the bytes done and the total, `release.size`, on the calling thread: at most
/// about 4 times a second while the file comes, then once with the total when it is verified.
pub fn download(release: &Release, mut progress: impl FnMut(u64, u64)) -> io::Result<Staged> {
  let target = installed()?;
  installer::tidy(OS, &target, &imp::alive);
  let (dir, file) = fetch(release, home(OS, &target), &mut progress)?;
  let new = imp::unpack(&file, &target)?;
  Ok(Staged { new, target, dir })
}

/// Starts the [Staged] app's installer, the new version's [finish] (on Windows this one's), and
/// returns once it runs; the app must then quit within 30 s. The installer waits for this process
/// to end, puts the new version in, starts it with this process's arguments and removes what is
/// left; an error of those comes from [finish] in the app it starts. `TimedOut` when the installer
/// gives no sign in 10 s, nothing swapped. The staging folder is the installer's from here on: on
/// an error the next [download] removes it.
pub fn install(staged: Staged) -> io::Result<()> {
  let Staged { new, target, dir } = staged;
  let (dir, exe) = (dir.keep(), env::current_exe()?);
  let name = exe.file_name().unwrap_or_default();
  let installer = match OS {
    Os::Mac => new.join("Contents/MacOS").join(name),
    // The setup replaces the exe: the installer runs from a copy of it, named as it is.
    Os::Windows => {
      let copy = dir.join(name);
      fs::copy(&exe, &copy)?;
      copy
    }
    Os::Linux => new.clone(),
  };

  let mut cmd = Command::new(installer);
  cmd.arg(INSTALL).arg(std::process::id().to_string()).arg(&new).arg(target).arg("--");
  cmd.args(env::args_os().skip(1)).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
  installer::handshake(&dir, READY, || {
    let mut child = imp::detach(&mut cmd)?;
    Ok(move || drop((child.kill(), child.wait())))
  })
}

/// The update's step at an app's launch, called first in `main`: before its arguments are parsed
/// and before any window. Started by [install] (`--shipo-install` the first argument), it is the
/// installer and never returns: it waits for the old app to end, puts the new one in, starts it
/// and exits (status 0, 1 on a failure, 2 for malformed arguments). Otherwise the error of the
/// installer that started this app, in `SHIPO_UPDATE_ERROR`, the first time it is called and
/// `None` after; the variable stays in the environment, so an app this one starts would show it
/// too.
pub fn finish() -> Option<io::Error> {
  static DONE: AtomicBool = AtomicBool::new(false);
  match installer::job(OS, &env::args_os().skip(1).collect::<Vec<_>>()) {
    Some(Some(job)) => installer::run(&job),
    Some(None) => std::process::exit(2),
    None => {}
  }

  if DONE.swap(true, Ordering::Relaxed) {
    return None;
  }

  // Windows' installer can't remove the folder it runs from: the app does, once it has ended.
  #[cfg(windows)]
  std::thread::spawn(|| {
    std::thread::sleep(Duration::from_secs(5));
    let found = env::current_exe().and_then(|exe| target(OS, &exe, &|k| env::var_os(k)));
    if let Ok(target) = found {
      installer::tidy(OS, &target, &imp::alive);
    }
  });

  env::var(NOTE).ok().map(io::Error::other)
}

// MARK: The installed app

/// The OS whose rules a path follows: [OS] in use, another in tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Os {
  Mac,
  Windows,
  Linux,
}

const OS: Os = match () {
  _ if cfg!(target_os = "macos") => Os::Mac,
  _ if cfg!(windows) => Os::Windows,
  _ => Os::Linux,
};

/// An environment variable by its name: [env::var_os], or a test's.
type Env<'a> = &'a dyn Fn(&str) -> Option<OsString>;

/// The app an update replaces, of the running `exe`: `Unsupported` for none (a build),
/// `PermissionDenied` for a bundle macOS translocated (run from a read-only copy, as it does for
/// one downloaded and not moved).
fn target(os: Os, exe: &Path, env: Env) -> io::Result<PathBuf> {
  let target = match os {
    // The `.app` bundle, by its layout: `<name>.app/Contents/MacOS/<exe>`.
    Os::Mac => {
      let macos = exe.parent().filter(|p| p.ends_with("Contents/MacOS"));
      let app = macos.and_then(|p| p.parent()?.parent());
      app.filter(|a| a.extension().is_some_and(|e| e == "app")).map(Path::to_owned)
    }
    // The AppImage, whose runtime sets both; one inherited from another AppImage (a terminal that
    // is one) is not where this runs.
    Os::Linux => match (env("APPIMAGE"), env("APPDIR")) {
      (Some(image), Some(mount)) if exe.starts_with(&mount) => {
        Some(PathBuf::from(image)).filter(|i| i.is_file())
      }
      _ => None,
    },
    // The exe's folder under `%LOCALAPPDATA%\Programs`, where the per-user setup installs,
    // whatever the case.
    Os::Windows => {
      let folded = |p: &Path| -> Vec<_> {
        p.components().map(|c| c.as_os_str().to_ascii_lowercase()).collect()
      };
      let programs = env("LOCALAPPDATA").map(|l| folded(&Path::new(&l).join("Programs")));
      let under = |f: &Path| f.ancestors().skip(1).any(|a| Some(folded(a)) == programs);
      exe.parent().filter(|f| under(f)).map(Path::to_owned)
    }
  };

  let what = format!("{} is not an installed app", exe.display());
  match target {
    None => Err(error(io::ErrorKind::Unsupported, what)),
    Some(t) if t.to_string_lossy().contains("/AppTranslocation/") => {
      Err(error(io::ErrorKind::PermissionDenied, format!("{what}: it runs translocated")))
    }
    Some(t) => Ok(t),
  }
}

// MARK: Download

/// [download]'s progress: the bytes done, the total.
type Progress<'a> = &'a mut dyn FnMut(u64, u64);

/// How often [download]'s progress is told at most.
const TICK: Duration = Duration::from_millis(250);

/// The release's file fetched over https (or loopback) and verified into a fresh folder under
/// `parent`, gone again on an error; one the user can't write fails first. `progress` as
/// [download]'s.
fn fetch(release: &Release, parent: &Path, progress: Progress) -> io::Result<(Folder, PathBuf)> {
  if !feed::secure(&release.url) {
    return Err(bad(format!("{} is not https", release.url)));
  }

  let exe = env::current_exe()?;
  let name = stage_name(OS, &release.url, exe.file_name().unwrap_or_default())?;
  let dir = Folder::new(parent)?;
  let file = dir.0.join(name);
  let (total, mut told) = (release.size, None::<Instant>);
  let mut tell = |done| {
    if 0 < done && done < total && told.is_none_or(|t| t.elapsed() >= TICK) {
      told = Some(Instant::now());
      progress(done, total);
    }
  };
  imp::get(&release.url, &file, total, &mut tell)?;
  release.verify(&file)?;
  progress(total, total);
  Ok((dir, file))
}

/// The name the release's file at `url` is staged by. On Windows its own, the last part of the
/// URL's path: a plain `<name>.exe` other than the running `exe`'s, whose copy, the installer,
/// goes beside it (`InvalidData` else). Windows loads compatibility shims into an exe named
/// `setup.exe` (makensis' warning 9000), and the packer names none so.
fn stage_name<'a>(os: Os, url: &'a str, exe: &OsStr) -> io::Result<&'a str> {
  match os {
    Os::Mac => Ok("update.dmg"),
    Os::Linux => Ok("app.AppImage"),
    Os::Windows => {
      let path = url.split(['?', '#']).next().unwrap_or_default();
      let name = path.rsplit('/').next().unwrap_or_default();
      let plain = !name.contains(|c: char| c.is_control() || r#"\:*?"<>|"#.contains(c));
      let ext = Path::new(name).extension().is_some_and(|e| e.eq_ignore_ascii_case("exe"));
      let own = OsStr::new(name).eq_ignore_ascii_case(exe);
      let ok = plain && ext && !own;
      ok.then_some(name).ok_or_else(|| bad(format!("{url} names no setup to stage")))
    }
  }
}

// MARK: Errors

/// An `InvalidData` error of the update.
fn bad(what: impl fmt::Display) -> io::Error {
  error(io::ErrorKind::InvalidData, what)
}

/// An error of the update of the kind `kind`.
fn error(kind: io::ErrorKind, what: impl fmt::Display) -> io::Error {
  io::Error::new(kind, format!("update: {what}"))
}

#[cfg(test)]
mod tests {
  use std::io::{Read, Write};
  use std::net::TcpListener;

  use super::*;
  use crate::testing::{ABC, FEED, KEY, RFC_KEY, dir, sign};

  /// A server on loopback, its URL: `/update.txt` a 302 to `/feed/update.txt`, which with
  /// `/abc.exe` it serves; `/bad.txt` a feed changed after it was signed; `/spare.txt` a feed of
  /// this platform's `abc.exe` and notes, signed by [RFC_KEY]; anything else a 404.
  fn serve() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let mut bad = FEED.as_bytes().to_vec();
    bad[20] ^= 1;
    let spare = format!("version 2\nnotes {url}/v2\n{} 3 {ABC} {url}/abc.exe\n", feed::platform());
    let spare = format!("{spare}sig {}\n", sign(spare.as_bytes()));
    std::thread::spawn(move || {
      for mut s in listener.incoming().flatten() {
        let (mut req, mut buf) = (Vec::new(), [0; 1024]);
        while !req.ends_with(b"\r\n\r\n") {
          match s.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => req.extend_from_slice(&buf[..n]),
          }
        }

        let req = String::from_utf8_lossy(&req);
        let (status, body): (_, &[u8]) = match req.split(' ').nth(1).unwrap_or_default() {
          "/update.txt" => ("302 Found\r\nLocation: /feed/update.txt", b""),
          "/feed/update.txt" => ("200 OK", FEED.as_bytes()),
          "/bad.txt" => ("200 OK", bad.as_slice()),
          "/spare.txt" => ("200 OK", spare.as_bytes()),
          "/abc.exe" => ("200 OK", b"abc"),
          _ => ("404 Not Found", b"none"),
        };

        let head = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\n\r\n", body.len());
        let _ = s.write_all(head.as_bytes()).and_then(|_| s.write_all(body));
      }
    });

    url
  }

  #[test]
  fn the_target_is_the_installed_app() {
    let none = |_: &str| None;
    let target = |os, exe: &str| target(os, Path::new(exe), &none);
    let app = target(Os::Mac, "/Applications/A.app/Contents/MacOS/a").unwrap();
    assert_eq!(app, Path::new("/Applications/A.app"));
    let moved = "/private/var/folders/x/T/AppTranslocation/1-2/d/A.app/Contents/MacOS/a";
    assert_eq!(target(Os::Mac, moved).unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    let windows = "C:/Users/Me/AppData/Local/Programs/W/a.exe";
    for (os, exe) in [(Os::Mac, "/Users/me/a/target/debug/a"), (Os::Linux, "/usr/bin/a")] {
      assert_eq!(target(os, exe).unwrap_err().kind(), io::ErrorKind::Unsupported);
    }

    assert_eq!(target(Os::Windows, windows).unwrap_err().kind(), io::ErrorKind::Unsupported);
  }

  #[test]
  fn a_windows_app_is_one_the_setup_installed() {
    let local = |k: &str| (k == "LOCALAPPDATA").then(|| "C:/Users/Me/AppData/Local".into());
    let target = |exe: &str| target(Os::Windows, Path::new(exe), &local);
    let programs = "C:/Users/Me/AppData/Local/Programs";
    assert_eq!(target(&format!("{programs}/W/a.exe")).unwrap(), Path::new(programs).join("W"));
    let case = "c:/users/me/appdata/local/PROGRAMS/W";
    assert_eq!(target(&format!("{case}/a.exe")).unwrap(), Path::new(case), "whatever the case");
    assert!(target(&format!("{programs}/V/W/a.exe")).is_ok(), "a folder deeper");
    for exe in [
      format!("{programs}/a.exe"),
      format!("{programs}X/W/a.exe"),
      "C:/Users/Me/src/w/target/release/a.exe".into(),
      "C:/Program Files/W/a.exe".into(),
    ] {
      assert_eq!(target(&exe).unwrap_err().kind(), io::ErrorKind::Unsupported, "{exe}");
    }
  }

  #[test]
  fn the_app_is_the_bundle_or_the_appimage() {
    let none = |_: &str| None;
    let app = |os, exe: &str, env: Env| target(os, Path::new(exe), env).ok();
    let mac = |exe: &str| app(Os::Mac, exe, &none);
    assert_eq!(mac("/Applications/A.app/Contents/MacOS/a"), Some("/Applications/A.app".into()));
    assert_eq!(mac("/Applications/A/Contents/MacOS/a"), None);
    assert_eq!(mac("/Users/me/a/target/debug/a"), None);
    assert_eq!(mac("/A.app/Contents/Resources/a"), None);
    assert_eq!(app(Os::Windows, "C:/A/a.exe", &none), None);

    let dir = dir("app-of");
    let image = dir.join("A.AppImage");
    fs::write(&image, "").unwrap();
    let linux = |exe: &str, image: &Path| {
      let env = |k: &str| match k {
        "APPIMAGE" => Some(image.into()),
        "APPDIR" => Some("/tmp/.mount_A1".into()),
        _ => None,
      };
      app(Os::Linux, exe, &env)
    };
    assert_eq!(linux("/tmp/.mount_A1/usr/bin/a", &image), Some(image.clone()));
    // Another AppImage's variables, inherited; one that is gone.
    assert_eq!(linux("/home/me/bin/a", &image), None);
    assert_eq!(linux("/tmp/.mount_A1/usr/bin/a", &dir.join("gone")), None);
    assert_eq!(app(Os::Linux, "/tmp/.mount_A1/usr/bin/a", &none), None);
    fs::remove_dir_all(&dir).unwrap();
  }

  #[cfg(unix)]
  #[test]
  fn a_read_only_folder_is_refused_before_downloading() {
    use std::os::unix::fs::PermissionsExt;
    let root = dir("read-only");
    fs::set_permissions(&root, fs::Permissions::from_mode(0o555)).unwrap();
    let r = Release { url: "http://127.0.0.1:9/a".into(), size: 1, ..Release::test("2") };
    let e = fetch(&r, &root, &mut |_, _| {}).unwrap_err();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(e.kind(), io::ErrorKind::PermissionDenied, "{e}");
    fs::remove_dir(root).unwrap();
  }

  #[test]
  fn get_follows_a_redirect_and_fails_on_a_404() {
    let (url, root) = (serve(), dir("get"));
    let to = root.join("update.txt");
    let mut seen = Vec::new();
    imp::get(&format!("{url}/update.txt"), &to, FEED.len() as u64, &mut |n| seen.push(n)).unwrap();
    assert_eq!(fs::read_to_string(&to).unwrap(), FEED);
    assert!(seen.is_sorted() && seen.last() == Some(&(FEED.len() as u64)), "{seen:?}");
    let none = &mut |_| {};
    let missing = imp::get(&format!("{url}/none"), &to, 1 << 10, none).unwrap_err();
    assert_eq!(missing.kind(), io::ErrorKind::NotFound, "{missing}");
    let big = imp::get(&format!("{url}/update.txt"), &to, FEED.len() as u64 - 1, none);
    let big = big.unwrap_err();
    assert_eq!(big.kind(), io::ErrorKind::InvalidData, "{big}");
    fs::remove_dir_all(root).unwrap();
  }

  #[test]
  fn check_and_download_over_http() {
    let url = serve();
    let at = |path: &str| format!("{url}/{path}");
    let checked = |url: &str, current| check(&Feed::new(url, &[KEY]), current);
    let offered = checked(&at("update.txt"), "1.0").unwrap();
    assert_eq!(offered, feed::release(FEED, "1.0", &feed::platform()).unwrap());
    assert_eq!(checked(&at("update.txt"), "1.2.0").unwrap(), None);
    let kind = |url: &str| checked(url, "1.0").unwrap_err().kind();
    assert_eq!(kind(&at("none.txt")), io::ErrorKind::NotFound);
    assert_eq!(kind("http://example.com/update.txt"), io::ErrorKind::InvalidData, "not https");
    assert_eq!(kind(&at("bad.txt")), io::ErrorKind::InvalidData, "changed after it was signed");
    assert_eq!(kind(&at("spare.txt")), io::ErrorKind::InvalidData, "signed by another key");

    // The spare's feed, taken by an app that holds both keys; no keys fail before a fetch.
    let both = |keys| check(&Feed::new(&at("spare.txt"), keys), "1.0");
    let r = both(&[KEY, RFC_KEY]).unwrap().unwrap();
    assert_eq!((r.version.as_str(), r.notes), ("2", Some(at("v2"))));
    let zero = "00".repeat(32);
    for keys in [&[][..], &[KEY, "AAAA"], &[KEY, &zero]] {
      let gone = check(&Feed::new(&at("none.txt"), keys), "1.0");
      assert_eq!(gone.unwrap_err().kind(), io::ErrorKind::InvalidInput, "{keys:?}");
    }

    let parent = dir("parent");
    let sha512 = feed::hex(ABC).unwrap();
    let r = Release { url: at("abc.exe"), size: 3, sha512, ..Release::test("2") };
    let mut seen = Vec::new();
    let (staged, file) = fetch(&r, &parent, &mut |done, total| seen.push((done, total))).unwrap();
    assert_eq!(fs::read(&file).unwrap(), b"abc");
    assert_eq!(seen, [(3, 3)], "only the end within a tick");
    #[cfg(unix)]
    {
      use std::os::unix::fs::PermissionsExt;
      let mode = fs::metadata(&staged.0).unwrap().permissions().mode();
      assert_eq!(mode & 0o777, 0o700);
    }

    drop(staged);
    // Shorter than the feed says, longer, another file, not https.
    let changes: [fn(&mut Release); 4] = [
      |r| r.size = 4,
      |r| r.size = 2,
      |r| r.sha512[0] ^= 1,
      |r| r.url = "http://example.com/abc.exe".into(),
    ];
    for change in changes {
      let mut r = r.clone();
      change(&mut r);
      let e = fetch(&r, &parent, &mut |_, _| {}).unwrap_err();
      assert_eq!(e.kind(), io::ErrorKind::InvalidData, "{r:?}");
    }

    let gone = Release { url: at("none.exe"), ..r };
    assert_eq!(fetch(&gone, &parent, &mut |_, _| {}).unwrap_err().kind(), io::ErrorKind::NotFound);
    assert_eq!(fs::read_dir(&parent).unwrap().count(), 0, "no folder is left");
    fs::remove_dir(parent).unwrap();
  }

  #[test]
  fn a_setup_is_staged_by_its_own_name() {
    let (url, exe) = ("https://h/d/v2/A-v2-windows-x64-setup.exe?x=1#y", OsStr::new("A.exe"));
    assert_eq!(stage_name(Os::Windows, url, exe).unwrap(), "A-v2-windows-x64-setup.exe");
    assert_eq!(stage_name(Os::Windows, "https://h/B.EXE", exe).unwrap(), "B.EXE");
    assert_eq!(stage_name(Os::Mac, url, exe).unwrap(), "update.dmg");
    assert_eq!(stage_name(Os::Linux, url, exe).unwrap(), "app.AppImage");
    // Not an exe, the app's own name (its installer's copy), a name Windows refuses, none.
    let bad = ["a.zip", "a.exe/", ".exe", "a.exe.", "a.EXE", "a:b.exe", "a\\b.exe", "a\n.exe"];
    for name in bad {
      let url = format!("https://h/{name}");
      let e = stage_name(Os::Windows, &url, OsStr::new("a.exe")).unwrap_err();
      assert_eq!(e.kind(), io::ErrorKind::InvalidData, "{name:?}");
    }
  }

  #[test]
  fn a_dev_build_is_unsupported() {
    // A feed that fails once fetched: nothing is.
    let feed = Feed::new("http://example.com/update.txt", &[KEY]);
    for dev in ["0.1.0-5-gabc", "1.2.0-rc.1"] {
      let e = check(&feed, dev).unwrap_err();
      assert_eq!(e.kind(), io::ErrorKind::Unsupported, "{dev}");
      assert!(e.to_string().contains("dev build"), "{e}");
    }
  }

  #[test]
  fn a_test_release_is_refused() {
    let (r, parent) = (Release::test("2.0"), dir("test-release"));
    assert_eq!((r.version.as_str(), r.size, &r.notes), ("2.0", 0, &None));
    let e = fetch(&r, &parent, &mut |_, _| {}).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::InvalidData, "{e}");
    assert!(download(&r, |_, _| {}).is_err(), "a build, and no file");
    assert_eq!(fs::read_dir(&parent).unwrap().count(), 0, "nothing fetched");
    fs::remove_dir(parent).unwrap();
  }

  #[test]
  fn a_build_is_unsupported() {
    assert_eq!(supported().unwrap_err().kind(), io::ErrorKind::Unsupported);
  }
}
