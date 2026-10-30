// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! In-place updates for desktop apps from a signed feed, as Sparkle does: [check] fetches the feed
//! the shipo packer writes and gives the newer [Release] for this platform, [download] fetches its
//! file beside the running app, verifies and stages it, [install] hands it to an installer that
//! puts it in once the app has quit, and [finish], first in `main`, is that installer. They block:
//! an app runs them on a thread of its own and posts the result back.
//!
//! The library has no UI and no state of its own: the app draws the menu item, the popup, the
//! progress and the setting, and keeps the check time and a skipped version. It never shows a
//! window and never takes the focus. An app on an older version of this crate takes the updates a
//! newer packer makes; the feed's format is in the readme.
//!
//! ```no_run
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
//! fn update() -> shipo::Result<()> {
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

mod error;
mod fake;
mod feed;
mod installer;
mod platform;
#[cfg(test)]
mod testing;
mod uninstall;

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use std::{env, fs};

pub use error::{Error, Result};
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
  /// `None` of a fake update's.
  dir: Option<Folder>,
}

// What an app's thread sends back to its main one.
const _: () = {
  const fn send<T: Send>() {}
  send::<Staged>();
  send::<Release>();
};

/// The release `feed` offers this platform when it is newer than `current`: the feed fetched over
/// the OS's HTTP, then [Feed::read]; `None` when it is up to date. Nothing is fetched for a
/// pre-release `current` (a dev build, `0.1.0-5-gabc`: [Error::Unsupported]), a feed URL not https
/// ([Error::Feed]) or a feed of no key or a malformed one ([Error::Keys]). A [Feed::fakeable]
/// feed's is faked while [faking].
pub fn check(feed: &Feed, current: &str) -> Result<Option<Release>> {
  check_or_fake(feed, current, fake::var().as_deref())
}

/// [check], faked by `SHIPO_FAKE_UPDATE`'s value `fake` for a fakeable feed.
fn check_or_fake(feed: &Feed, current: &str, fake: Option<&str>) -> Result<Option<Release>> {
  if let Some(value) = fake.filter(|_| feed.fakeable) {
    return fake::check(feed, value);
  }

  if current.contains('-') {
    return Err(Error::Unsupported(format!("{current} is a dev build, which takes no update")));
  }

  if !feed::secure(feed.url) {
    return Err(Error::Feed(format!("{} is not https", feed.url)));
  }

  feed.public_keys()?;
  let dir = Folder::new(&env::temp_dir())?;
  let to = dir.0.join("update.txt");
  let big = || Error::Feed(format!("more than {} bytes", feed::MAX));
  imp::get(feed.url, &to, feed::MAX, &big, &mut |_| {})?;
  feed.read(&fs::read(to)?, current)
}

/// Whether the running app can update in place, checked with no network, so an app can hide its
/// update up front: not without an installed app to replace ([Error::Unsupported]: a build, a
/// Windows exe outside `%LOCALAPPDATA%\Programs` and its setup's folder, a Linux one not run as an
/// AppImage, as a .deb's in /usr, a translocated app), nor one it can't replace
/// ([Error::Unsupported]: a bundle the user can't write or on a volume without APFS's swap, an
/// AppImage the user can't write), nor one whose folder the user can't write ([Error::Io],
/// `PermissionDenied`: `/Applications` for a standard user, `/opt`). `Ok` while [faking], whatever
/// the feed: it knows none.
pub fn supported() -> Result<()> {
  supported_or_fake(faking())
}

fn supported_or_fake(faking: bool) -> Result<()> {
  if faking {
    return Ok(());
  }

  installed().map(drop)
}

/// The app an update replaces ([target]), [replaceable], its [home] writable: a staging folder
/// made there.
fn installed() -> Result<PathBuf> {
  let target = target(OS, &env::current_exe()?, &|k| env::var_os(k), &installs)?;
  #[cfg(unix)]
  replaceable(OS, &target, imp::access(&target))?;
  Folder::new(home(OS, &target))?;
  Ok(target)
}

/// Downloads `release`'s file beside the running app, checks its size and SHA-512
/// ([Error::Mismatch] if either differs) and stages it: on macOS the one `.app` on the image, held
/// to the running bundle's id and designated requirement ([Error::Bundle]). Before a byte is
/// fetched it fails as [supported] does and removes what an installer stopped midway left.
///
/// `progress` gets the bytes done and the total, `release.size`, on the calling thread: at most
/// about 4 times a second while the file comes, then once with the total when it is verified.
///
/// A fake release, of a fakeable feed while [faking], is not fetched: its progress goes to the end
/// in about 5 s, or it fails as `SHIPO_FAKE_UPDATE` asks.
pub fn download(release: &Release, mut progress: impl FnMut(u64, u64)) -> Result<Staged> {
  if let Some(fake) = &release.fake {
    return fake::download(fake, release.size, &mut progress);
  }

  let target = installed()?;
  installer::tidy(OS, &target, &imp::alive);
  let (dir, file) = fetch(release, home(OS, &target), &mut progress)?;
  let new = imp::unpack(&file, &target)?;
  Ok(Staged { new, target, dir: Some(dir) })
}

/// Starts the [Staged] app's installer, the new version's [finish] (on Windows this one's), and
/// returns once it runs; the app must then quit within 30 s. On macOS the installer is the new
/// bundle's executable of this one's name, so every release keeps that name. The installer waits
/// for this process to end, puts the new version in, starts it with this process's arguments and
/// removes what is left; an error of those comes from [finish] in the app it starts.
/// [Error::Install] when the installer gives no sign in 10 s, nothing swapped. The staging folder
/// is the installer's from here on: on an error the next [download] removes it. A fake update's is
/// [Error::Install], nothing installed.
pub fn install(staged: Staged) -> Result<()> {
  let Staged { new, target, dir } = staged;
  let Some(dir) = dir else { return fake::install() };
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
/// installer that started this app, its `SHIPO_UPDATE_ERROR` as [Error::Install], the first time
/// it is called and `None` after; the variable stays in the environment, so an app this one starts
/// would show it too.
pub fn finish() -> Option<Error> {
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
    let var = |k: &str| env::var_os(k);
    let found = env::current_exe().ok().and_then(|exe| target(OS, &exe, &var, &installs).ok());
    if let Some(target) = found {
      installer::tidy(OS, &target, &imp::alive);
    }
  });

  env::var(NOTE).ok().map(Error::Install)
}

/// Whether `SHIPO_FAKE_UPDATE` is set (and not empty), so a [Feed::fakeable] feed's updates are
/// faked: for an app's own gates of its update, such as a build's.
pub fn faking() -> bool {
  fake::var().is_some()
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

/// The values of an HKCU `{…}` Uninstall key the app is found by: `InstallLocation`,
/// `DisplayIcon`, `UninstallString`.
#[derive(Clone)]
struct Key {
  location: String,
  icon: String,
  uninstall: String,
}

/// The Uninstall keys, Windows' only.
fn installs() -> Vec<Key> {
  #[cfg(windows)]
  return imp::installs();
  #[cfg(not(windows))]
  Vec::new()
}

/// The app an update replaces, of the running `exe`: [Error::Unsupported] for none (a build) or a
/// bundle macOS translocated (run from a read-only copy, as it does for one downloaded and not
/// moved). `keys`, the Uninstall keys, read only when the exe isn't in the default folder.
fn target(os: Os, exe: &Path, env: Env, keys: &dyn Fn() -> Vec<Key>) -> Result<PathBuf> {
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
    // The exe's folder under `%LOCALAPPDATA%\Programs`, where the per-user setup installs by
    // default, whatever the case, or the one its key names with this exe, picked in its wizard or
    // by `/D=`.
    Os::Windows => {
      let folded = |p: &Path| -> Vec<_> {
        p.components().map(|c| c.as_os_str().to_ascii_lowercase()).collect()
      };
      let programs = env("LOCALAPPDATA").map(|l| folded(&Path::new(&l).join("Programs")));
      let under = |f: &Path| f.ancestors().skip(1).any(|a| Some(folded(a)) == programs);
      match exe.parent() {
        Some(dir) if under(dir) => Some(dir.to_owned()),
        dir => dir.and_then(|dir| keyed(dir, exe, &keys())),
      }
    }
  };

  let what = format!("{} is not an installed app", exe.display());
  match target {
    None => Err(Error::Unsupported(what)),
    Some(t) if t.to_string_lossy().contains("/AppTranslocation/") => {
      Err(Error::Unsupported(format!("{what}: it runs translocated")))
    }
    Some(t) => Ok(t),
  }
}

/// The folder of the key the setup (or v0.1's NSIS one) wrote for the running `exe` in `dir`, as
/// the key holds it, so `/D=` keeps it so: a [folder](uninstall::folder) with this exe in it and
/// its uninstaller, which an MSI's or another app's key isn't. By the text first; only a key of
/// this exe is resolved on disk, where a share gone may block.
fn keyed(dir: &Path, exe: &Path, keys: &[Key]) -> Option<PathBuf> {
  let name = exe.file_name()?.to_str()?;
  let ours: Vec<_> = keys
    .iter()
    .filter(|k| {
      uninstall::folder(&k.location).is_some()
        && k.icon.eq_ignore_ascii_case(&format!(r"{}\{name}", k.location))
        && k.uninstall == uninstall::command(&k.location)
    })
    .collect();

  let found = ours.iter().find(|k| uninstall::alike(&k.location, dir));
  let found = found.or_else(|| ours.iter().find(|k| uninstall::same(&k.location, dir)));
  found.map(|k| PathBuf::from(&k.location))
}

/// What the user may do to the target, as the OS tells it; Windows' setup replaces its own.
#[cfg_attr(windows, allow(dead_code))]
struct Access {
  writable: bool,
  /// Whether its volume exchanges two paths in one step (`VOL_CAP_INT_RENAME_SWAP`); macOS only.
  swap: bool,
}

/// Whether the installer can put a new app in place of `target`, [Error::Unsupported] with the
/// reason if not. The macOS swap needs a volume that has it (APFS; not HFS+ or exFAT) and the
/// bundle writable even for its owner, as moving a folder to another parent rewrites its `..`;
/// Linux's hard link of the old copy needs the AppImage writable (`protected_hardlinks`), or owned:
/// an AppImage of no write bit is refused, though its owner could update it.
#[cfg_attr(windows, allow(dead_code))]
fn replaceable(os: Os, target: &Path, access: Access) -> Result<()> {
  let why = match os {
    Os::Mac if !access.swap => "its volume can't swap two folders in one step (not APFS)",
    Os::Mac | Os::Linux if !access.writable => "this user can't write it",
    _ => return Ok(()),
  };

  Err(Error::Unsupported(format!("{}: {why}", target.display())))
}

// MARK: Download

/// [download]'s progress: the bytes done, the total.
type Progress<'a> = &'a mut dyn FnMut(u64, u64);

/// How often [download]'s progress is told at most.
const TICK: Duration = Duration::from_millis(250);

/// The release's file fetched over https (or loopback) and verified into a fresh folder under
/// `parent`, gone again on an error; one the user can't write fails first. `progress` as
/// [download]'s.
fn fetch(release: &Release, parent: &Path, progress: Progress) -> Result<(Folder, PathBuf)> {
  if !feed::secure(&release.url) {
    return Err(Error::Feed(format!("{:?} is not https", release.url)));
  }

  let name = stage_name(OS, &release.url, env::current_exe()?.file_name().unwrap_or_default())?;
  let dir = Folder::new(parent)?;
  let file = dir.0.join(name);
  let (total, mut told) = (release.size, None::<Instant>);
  let mut tell = |done| {
    if 0 < done && done < total && told.is_none_or(|t| t.elapsed() >= TICK) {
      told = Some(Instant::now());
      progress(done, total);
    }
  };
  let big = || Error::Mismatch(format!("{} sent more than {total} bytes", release.url));
  imp::get(&release.url, &file, total, &big, &mut tell)?;
  release.verify(&file)?;
  progress(total, total);
  Ok((dir, file))
}

/// The name the release's file at `url` is staged by. On Windows its own, the last part of the
/// URL's path: a plain `<name>.exe` other than the running `exe`'s, whose copy, the installer,
/// goes beside it ([Error::Feed] else). Windows loads compatibility shims into an exe named
/// `setup.exe` (makensis' warning 9000 says so), and the packer names none so.
fn stage_name<'a>(os: Os, url: &'a str, exe: &OsStr) -> Result<&'a str> {
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
      ok.then_some(name).ok_or_else(|| Error::Feed(format!("{url} names no setup to stage")))
    }
  }
}

#[cfg(test)]
mod tests {
  use std::io::{Read, Write};
  use std::net::{Shutdown, TcpListener};
  use std::time::Duration;

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

        // One request a connection, said: a client that kept it for the redirect could send on
        // it as it closes, and WinHTTP takes the reset or empty reply as an invalid response.
        let len = body.len();
        let head =
          format!("HTTP/1.1 {status}\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n");
        let _ = s.write_all(head.as_bytes()).and_then(|_| s.write_all(body));
        // Closed once the client has, so no unread byte turns the close into a reset that drops
        // the reply in flight.
        let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
        let _ = s.shutdown(Shutdown::Write).and_then(|_| s.read_to_end(&mut Vec::new()));
      }
    });

    url
  }

  #[test]
  fn the_target_is_the_installed_app() {
    let none = |_: &str| None;
    let target = |os, exe: &str| target(os, Path::new(exe), &none, &Vec::new);
    let app = target(Os::Mac, "/Applications/A.app/Contents/MacOS/a").unwrap();
    assert_eq!(app, Path::new("/Applications/A.app"));
    let moved = "/private/var/folders/x/T/AppTranslocation/1-2/d/A.app/Contents/MacOS/a";
    let windows = "C:/Users/Me/AppData/Local/Programs/W/a.exe";
    for (os, exe) in [
      (Os::Mac, moved),
      (Os::Mac, "/Users/me/a/target/debug/a"),
      (Os::Linux, "/usr/bin/a"),
      (Os::Windows, windows),
    ] {
      let e = target(os, exe).unwrap_err();
      assert!(matches!(e, Error::Unsupported(_)), "{exe}: {e}");
    }
  }

  #[test]
  fn a_windows_app_is_one_the_setup_installed() {
    let local = |k: &str| (k == "LOCALAPPDATA").then(|| "C:/Users/Me/AppData/Local".into());
    let target = |exe: &str| target(Os::Windows, Path::new(exe), &local, &Vec::new);
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
      assert!(matches!(target(&exe).unwrap_err(), Error::Unsupported(_)), "{exe}");
    }
  }

  /// The folder picked in the setup's wizard or by `/D=` is the one its key names, as the setup
  /// (and v0.1's NSIS one) writes it with the exe; never another app's folder or a parent of it,
  /// nor a key another installer wrote.
  #[test]
  fn a_windows_app_in_its_keys_folder() {
    let local = |k: &str| (k == "LOCALAPPDATA").then(|| "C:/Users/Me/AppData/Local".into());
    let key = |l: &str, name: &str| Key {
      location: l.into(),
      icon: format!(r"{l}\{name}"),
      uninstall: format!(r#""{l}\Uninstall.exe""#),
    };
    let hello = key(r"C:\Apps\Hello", "Hello.exe");
    let found =
      |exe: &str, keys: &[Key]| target(Os::Windows, Path::new(exe), &local, &|| keys.to_vec()).ok();
    let keyed = [key(r"D:\Tools", "T.exe"), hello.clone()];
    let at = |exe: &str| found(exe, &keyed);
    assert_eq!(at("C:/Apps/Hello/Hello.exe"), Some(r"C:\Apps\Hello".into()));
    let case = at("c:/apps/HELLO/hello.EXE");
    assert_eq!(case, Some(r"C:\Apps\Hello".into()), "the key's folder, whatever the case");
    assert_eq!(at("C:/Apps/Hello/sub/Hello.exe"), None, "not a folder deeper");
    assert_eq!(at("C:/Apps/Hello.exe"), None);
    assert_eq!(at("C:/Apps/Hellos/Hello.exe"), None);
    assert_eq!(at("C:/Apps/Hello/Other.exe"), None, "another app's exe in its folder");
    assert_eq!(at("D:/Tools/Hello.exe"), None);

    // The default folder reads no key.
    let programs = "C:/Users/Me/AppData/Local/Programs/Hello/Hello.exe";
    let unread = || -> Vec<Key> { panic!("keys read") };
    assert!(target(Os::Windows, Path::new(programs), &local, &unread).is_ok());

    // Not the setup's key: an MSI's, an icon of its own, a root or a folder Windows climbs by.
    let msi = Key { uninstall: "MsiExec.exe /X{x}".into(), ..hello.clone() };
    let icon = Key { icon: r#""C:\Apps\Hello\Hello.exe",0"#.into(), ..hello.clone() };
    for k in [msi, icon] {
      assert_eq!(found("C:/Apps/Hello/Hello.exe", &[k]), None);
    }

    let odd = [
      ("C:/Hello.exe", r"C:\"),
      ("C:/Apps/../Hello/Hello.exe", r"C:\Apps\..\Hello"),
      ("C:/Apps/. /Hello.exe", r"C:\Apps\. "),
      ("//?/Volume{x}/Apps/Hello.exe", r"\\?\Volume{x}\Apps"),
    ];
    for (exe, l) in odd {
      assert!(uninstall::alike(l, Path::new(exe).parent().unwrap()), "{l}");
      assert_eq!(found(exe, &[key(l, "Hello.exe")]), None, "{l}");
    }
  }

  #[test]
  fn the_app_is_the_bundle_or_the_appimage() {
    let none = |_: &str| None;
    let app = |os, exe: &str, env: Env| target(os, Path::new(exe), env, &Vec::new).ok();
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
    let denied = std::io::ErrorKind::PermissionDenied;
    assert!(matches!(&e, Error::Io(e) if e.kind() == denied), "{e}");
    fs::remove_dir(root).unwrap();
  }

  #[test]
  fn get_follows_a_redirect_and_fails_on_a_404() {
    let (url, root) = (serve(), dir("get"));
    let to = root.join("update.txt");
    let (mut seen, big) = (Vec::new(), &|| Error::Feed("big".into()));
    let len = FEED.len() as u64;
    imp::get(&format!("{url}/update.txt"), &to, len, big, &mut |n| seen.push(n)).unwrap();
    assert_eq!(fs::read_to_string(&to).unwrap(), FEED);
    assert!(seen.is_sorted() && seen.last() == Some(&len), "{seen:?}");
    let none = &mut |_| {};
    let missing = imp::get(&format!("{url}/none"), &to, 1 << 10, big, none).unwrap_err();
    assert!(matches!(&missing, Error::Http { status: Some(404), .. }), "{missing}");
    assert!(missing.to_string().ends_with("/none: HTTP 404"), "{missing}");
    let past = imp::get(&format!("{url}/update.txt"), &to, len - 1, big, none).unwrap_err();
    assert_eq!(past.to_string(), "invalid feed: big");
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
    let error = |url: &str| checked(url, "1.0").unwrap_err();
    assert!(matches!(error(&at("none.txt")), Error::Http { status: Some(404), .. }));
    assert!(matches!(error("http://example.com/update.txt"), Error::Feed(_)), "not https");
    let bad = error(&at("bad.txt"));
    assert!(matches!(bad, Error::Signature(_)), "changed after it was signed: {bad}");
    assert!(matches!(error(&at("spare.txt")), Error::Signature(_)), "signed by another key");

    // The spare's feed, taken by an app that holds both keys; no keys fail before a fetch.
    let both = |keys| check(&Feed::new(&at("spare.txt"), keys), "1.0");
    let r = both(&[KEY, RFC_KEY]).unwrap().unwrap();
    assert_eq!((r.version.as_str(), r.notes), ("2", Some(at("v2"))));
    let zero = "00".repeat(32);
    for keys in [&[][..], &[KEY, "AAAA"], &[KEY, &zero]] {
      let gone = check(&Feed::new(&at("none.txt"), keys), "1.0");
      assert!(matches!(gone.unwrap_err(), Error::Keys), "{keys:?}");
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
    for (i, change) in changes.into_iter().enumerate() {
      let mut r = r.clone();
      change(&mut r);
      let e = fetch(&r, &parent, &mut |_, _| {}).unwrap_err();
      let mismatch = matches!(e, Error::Mismatch(_));
      assert!(if i < 3 { mismatch } else { matches!(e, Error::Feed(_)) }, "{r:?}: {e}");
    }

    let gone = Release { url: at("none.exe"), ..r };
    let e = fetch(&gone, &parent, &mut |_, _| {}).unwrap_err();
    assert!(matches!(e, Error::Http { status: Some(404), .. }), "{e}");
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
      assert!(matches!(e, Error::Feed(_)), "{name:?}");
    }
  }

  #[test]
  fn a_dev_build_is_unsupported() {
    // A feed that fails once fetched: nothing is.
    let feed = Feed::new("http://example.com/update.txt", &[KEY]);
    for dev in ["0.1.0-5-gabc", "1.2.0-rc.1"] {
      let e = check(&feed, dev).unwrap_err();
      assert!(matches!(e, Error::Unsupported(_)), "{dev}");
      assert!(e.to_string().contains("dev build"), "{e}");
    }
  }

  #[test]
  fn a_test_release_is_refused() {
    let (r, parent) = (Release::test("2.0"), dir("test-release"));
    assert_eq!((r.version.as_str(), r.size, &r.notes), ("2.0", 0, &None));
    let e = fetch(&r, &parent, &mut |_, _| {}).unwrap_err();
    assert!(matches!(e, Error::Feed(_)), "{e}");
    assert!(download(&r, |_, _| {}).is_err(), "a build, and no file");
    assert_eq!(fs::read_dir(&parent).unwrap().count(), 0, "nothing fetched");
    fs::remove_dir(parent).unwrap();
  }

  #[test]
  fn a_fakeable_feed_is_faked_to_the_install() {
    let url = "http://example.com/releases/latest/download/update.txt";
    let (real, fake) = (Feed::new(url, &[KEY]), Feed::new(url, &[KEY]).fakeable());
    let e = check_or_fake(&real, "1.0", Some("2.0")).unwrap_err();
    assert!(matches!(e, Error::Feed(m) if m.contains("not https")), "a real check");
    let r = check_or_fake(&fake, "1.0-dev", Some("2.0")).unwrap().unwrap();
    assert_eq!(r.notes.as_deref(), Some("http://example.com/releases/tag/v2.0"), "a dev build's");
    assert!(supported_or_fake(true).is_ok());

    let mut seen = Vec::new();
    let staged = download(&r, |done, total| seen.push((done, total))).unwrap();
    assert_eq!((seen.len(), seen[0], seen[19]), (20, (2_500_000, r.size), (r.size, r.size)));
    assert!(seen.is_sorted());
    let e = install(staged).unwrap_err();
    assert!(matches!(&e, Error::Install(m) if m.contains("fake")), "{e}");

    let fails = check_or_fake(&fake, "1.0", Some("2.0:fail-download")).unwrap().unwrap();
    let mut seen = Vec::new();
    let e = download(&fails, |done, _| seen.push(done)).unwrap_err();
    assert!(matches!(&e, Error::Mismatch(m) if m.contains("fake")), "{e}");
    assert_eq!(seen.len(), 8, "midway");

    let huge = Release { size: u64::MAX, ..r.clone() };
    let mut last = 0;
    download(&huge, |done, _| last = done).unwrap();
    assert_eq!(last, u64::MAX, "no overflow");

    // A real release changed to the fake's file stays real.
    let real = Release { url: r.url.clone(), size: r.size, ..Release::test("2") };
    assert!(download(&real, |_, _| {}).is_err(), "a build, and no fake");
  }

  #[test]
  fn an_app_the_installer_cant_replace_is_unsupported() {
    let access = |writable, swap| Access { writable, swap };
    let cases = [
      (Os::Mac, access(true, true), true),
      (Os::Mac, access(true, false), false),
      (Os::Mac, access(false, true), false),
      (Os::Linux, access(true, false), true),
      (Os::Linux, access(false, false), false),
      (Os::Windows, access(false, false), true),
    ];
    for (i, (os, access, ok)) in cases.into_iter().enumerate() {
      let r = replaceable(os, Path::new("/a/A.app"), access);
      assert_eq!(r.is_ok(), ok, "case {i}");
      assert!(r.is_ok() || matches!(r, Err(Error::Unsupported(_))));
    }
  }

  #[cfg(unix)]
  #[test]
  fn the_access_of_a_folder_of_ours() {
    use std::os::unix::fs::PermissionsExt;
    let root = dir("access");
    let access = imp::access(&root);
    assert!(access.writable);
    assert!(cfg!(target_os = "linux") || access.swap, "the temp folder is on APFS");
    fs::set_permissions(&root, fs::Permissions::from_mode(0o555)).unwrap();
    let access = imp::access(&root);
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!access.writable);
    fs::remove_dir(root).unwrap();
  }

  #[test]
  fn a_build_is_unsupported() {
    assert!(matches!(supported_or_fake(false).unwrap_err(), Error::Unsupported(_)));
  }
}
