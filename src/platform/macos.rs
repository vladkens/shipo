// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! The update's calls on macOS: the file fetched by an `NSURLSession` download task; the release's
//! `.dmg` attached by `hdiutil` and its one bundle copied out with `ditto`, held to the running
//! one's id and designated requirement by Security.framework; the two exchanged in one step; the
//! new one opened by LaunchServices in the old one's place.

use std::ffi::{CString, OsString, c_char};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::ptr::{self, NonNull};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;
use std::{fs, io, iter, thread};

use block2::RcBlock;
use objc2_core_foundation::{CFRetained, CFURL, Type};
use objc2_foundation::{
  NSBundle, NSError, NSHTTPURLResponse, NSNumber, NSString, NSURL, NSURLRequest,
  NSURLRequestCachePolicy, NSURLResponse, NSURLSession, NSURLVolumeSupportsSwapRenamingKey,
};
use objc2_security::{SecCSFlags, SecCode, SecStaticCode, kSecCSCheckAllArchitectures};

pub use super::unix::{alive, busy, detach, wait_exit};
use super::{Big, Progress};
use crate::installer::NOTE;
use crate::{Access, Error, Result};

// MARK: HTTP

/// The body of `url` into `to` by a download task, its file moved into place by the completion
/// handler, which runs on the session's queue while this waits; `big` the error past `max` bytes.
/// The shared session follows redirects by its defaults, to http too: the feed's signature and the
/// file's hash guard what comes.
pub fn get(url: &str, to: &Path, max: u64, big: Big, progress: Progress) -> Result<()> {
  let u = NSURL::URLWithString(&NSString::from_str(url));
  let u = u.ok_or_else(|| super::http(url, "not a URL"))?;
  let policy = NSURLRequestCachePolicy::ReloadIgnoringLocalCacheData;
  let request = NSURLRequest::requestWithURL_cachePolicy_timeoutInterval(&u, policy, 60.0);
  let (tx, rx) = mpsc::channel();
  let (link, into) = (url.to_owned(), to.to_owned());
  let done = RcBlock::new(move |file: *mut NSURL, res: *mut NSURLResponse, err: *mut NSError| {
    // SAFETY: each is null or an object that lives through the call.
    let (file, res, err) = unsafe { (file.as_ref(), res.as_ref(), err.as_ref()) };
    let code =
      res.and_then(|r| r.downcast_ref::<NSHTTPURLResponse>()).map_or(0, |r| r.statusCode());
    let _ = tx.send(match (err, file.and_then(|f| f.path())) {
      (Some(e), _) => Err(super::http(&link, e.localizedDescription())),
      (None, None) => Err(super::http(&link, "no file")),
      // The task's file is removed once the handler returns; it may be on another volume. Its
      // size is checked once it is in place.
      (None, Some(from)) => super::status(&link, code as u32).and_then(|()| {
        let from = from.to_string();
        Ok(fs::rename(&from, &into).or_else(|_| fs::copy(&from, &into).map(drop))?)
      }),
    });
  });

  // SAFETY: the handler only owns sendable values (a channel's sender, two strings).
  let task = unsafe {
    NSURLSession::sharedSession().downloadTaskWithRequest_completionHandler(&request, &done)
  };
  task.resume();
  // The bytes so far told and stopped once past `max`; the file it got checked too.
  let never = || super::http(url, "the task never ended");
  loop {
    match (rx.recv_timeout(Duration::from_millis(50)), task.countOfBytesReceived() as u64) {
      (Ok(r), n) => {
        let fits = r.and_then(|()| Ok(fs::metadata(to)?.len() <= max))?;
        return fits.then(|| progress(n)).ok_or_else(big);
      }
      (Err(RecvTimeoutError::Timeout), n) if n <= max => progress(n),
      (Err(RecvTimeoutError::Timeout), _) => break task.cancel(),
      (Err(RecvTimeoutError::Disconnected), _) => return Err(never()),
    }
  }

  Err(big())
}

// MARK: Image and signature

/// Copies the one bundle at the top of the disk image `file` into `file`'s folder, as
/// `<folder>/new/<name>.app`, once it [accepts] it in place of `app`: the image attached read-only
/// at `<folder>/volume`, unseen by Finder, up to five tries a second apart, and detached again,
/// forced if it must be.
/// [Error::Bundle] for an image with more than one `.app` at its top, or none.
pub fn unpack(file: &Path, app: &Path) -> Result<PathBuf> {
  let dir = file.parent().unwrap_or(file);
  let (volume, hdiutil) = (dir.join("volume"), || Command::new("/usr/bin/hdiutil"));
  let attach = ["attach", "-nobrowse", "-noautoopen", "-readonly", "-mountpoint"];
  let mut pauses = iter::repeat_n(Duration::from_secs(1), 4);
  while let Err(e) = run(hdiutil().args(attach).arg(&volume).arg(file)) {
    thread::sleep(pauses.next().filter(|_| super::attach_again(&e)).ok_or(e)?);
  }

  let new = copy(&volume, dir, app);
  let detach = |force: &[&str]| run(hdiutil().arg("detach").arg(&volume).args(force));
  let detached = detach(&[]).or_else(|_| detach(&["-force"]));
  let new = new?;
  detached?;
  Ok(new)
}

/// The one bundle at the top of `volume`, accepted for `app` and copied into `dir` by `ditto`.
fn copy(volume: &Path, dir: &Path, app: &Path) -> Result<PathBuf> {
  let all = fs::read_dir(volume)?.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_dir()));
  let mut apps = all.map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "app"));
  let (Some(one), None) = (apps.next(), apps.next()) else {
    return Err(Error::Bundle("the image holds more than one app, or none".into()));
  };

  accepts(&one, app)?;
  let new = dir.join("new").join(one.file_name().unwrap_or_default());
  run(Command::new("/usr/bin/ditto").arg(&one).arg(&new))?;
  Ok(new)
}

/// Whether the bundle `new` may replace the bundle `app`: the same bundle id, and signed so that
/// it meets `app`'s designated requirement, checked for every architecture as Sparkle's
/// `SUCodeSigningVerifier` does (an ad-hoc app's is its own cdhash, which no other build meets);
/// [Error::Bundle] with Security's reason if not.
pub fn accepts(new: &Path, app: &Path) -> Result<()> {
  let id = |p: &Path| {
    let bundle = NSBundle::bundleWithPath(&NSString::from_str(&p.to_string_lossy()));
    bundle?.bundleIdentifier().map(|id| id.to_string())
  };
  if id(new).is_none() || id(new) != id(app) {
    return Err(Error::Bundle(format!("{} is not the app of {}", new.display(), app.display())));
  }

  let (code, mut req, mut err) = (static_code(app)?, ptr::null_mut(), ptr::null_mut());
  // SAFETY: the code lives through the call, `req` an out pointer it sets on success.
  let status = unsafe {
    SecCode::copy_designated_requirement(&code, SecCSFlags::DefaultFlags, NonNull::from(&mut req))
  };
  let req = owned(app, status, req)?;
  let flags = SecCSFlags(kSecCSCheckAllArchitectures);
  // SAFETY: the code and requirement live through the call, `err` an out pointer.
  let status = unsafe { static_code(new)?.check_validity_with_errors(flags, Some(&req), &mut err) };
  if status == 0 {
    return Ok(());
  }

  // SAFETY: an error the call made, ours to release.
  let why = NonNull::new(err).and_then(|e| unsafe { CFRetained::from_raw(e) }.description());
  let why = why.map_or(format!("status {status}"), |d| d.to_string());
  let (new, app) = (new.display(), app.display());
  Err(Error::Bundle(format!("{new} doesn't meet {app}'s requirement: {why}")))
}

/// The signed code of the bundle at `path`.
fn static_code(path: &Path) -> Result<CFRetained<SecStaticCode>> {
  let url = CFURL::from_directory_path(path);
  let url = url.ok_or_else(|| Error::Bundle(format!("{} is not a path", path.display())))?;
  let mut code = ptr::null();
  // SAFETY: the URL lives through the call, `code` an out pointer it sets on success.
  let status = unsafe {
    SecStaticCode::create_with_path(&url, SecCSFlags::DefaultFlags, NonNull::from(&mut code))
  };
  owned(path, status, code.cast_mut())
}

/// What a Security call of `status` made at `made`, ours to release; [Error::Bundle] if it failed.
fn owned<T: Type>(path: &Path, status: i32, made: *mut T) -> Result<CFRetained<T>> {
  let made = NonNull::new(made).filter(|_| status == 0);
  // SAFETY: an object the call made with a retain count of one.
  let made = made.map(|m| unsafe { CFRetained::from_raw(m) });
  made.ok_or_else(|| Error::Bundle(format!("{}: Security's status {status}", path.display())))
}

// MARK: Install

/// What the user may do to the bundle `app`, its volume's swap Foundation's
/// `NSURLVolumeSupportsSwapRenamingKey`, the file system's own `VOL_CAP_INT_RENAME_SWAP`: asked of
/// the volume rather than guessed from its type's name, and of any path on it, where `getattrlist`
/// takes only the volume's root.
pub fn access(app: &Path) -> Access {
  let (url, mut swap) = (NSURL::fileURLWithPath(&NSString::from_str(&app.to_string_lossy())), None);
  // SAFETY: one of `NSURL`'s keys, whose value is an `NSNumber`; `swap` an out slot.
  let got =
    unsafe { url.getResourceValue_forKey_error(&mut swap, NSURLVolumeSupportsSwapRenamingKey) };
  let swap = got.ok().and(swap).and_then(|v| v.downcast::<NSNumber>().ok());
  Access { swap: swap.is_some_and(|n| n.boolValue()), ..super::unix::access(app) }
}

/// Exchanges the paths `a` and `b` in one step, `renamex_np` with `RENAME_SWAP`, as Sparkle's
/// `SUFileManager` does on APFS.
pub fn exchange(a: &Path, b: &Path) -> io::Result<()> {
  unsafe extern "C" {
    fn renamex_np(from: *const c_char, to: *const c_char, flags: u32) -> i32;
  }

  const RENAME_SWAP: u32 = 2;
  let path = |p: &Path| CString::new(p.as_os_str().as_bytes()).map_err(io::Error::from);
  let (a, b) = (path(a)?, path(b)?);
  // SAFETY: two C strings that live through the call.
  let done = unsafe { renamex_np(a.as_ptr(), b.as_ptr(), RENAME_SWAP) };
  if done == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}

/// Opens the bundle `app` as a new instance with `args` and `note` in its [NOTE] without taking
/// the focus, by `open -n -g`: started by launchd, it is its own responsible process for TCC and
/// gets launchd's environment, not this process's, and `/` as its working directory.
/// `open`'s error if LaunchServices fails.
pub fn start(app: &Path, args: &[OsString], note: Option<&str>) -> io::Result<()> {
  let env = note.into_iter().flat_map(|n| ["--env".into(), format!("{NOTE}={n}")]);
  run(Command::new("/usr/bin/open").args(["-n", "-g"]).args(env).arg(app).arg("--args").args(args))
}

/// Runs `cmd` to its end; an error with its stderr if it failed.
fn run(cmd: &mut Command) -> io::Result<()> {
  let out = cmd.output()?;
  let program = cmd.get_program().to_string_lossy();
  let why = || format!("{program}: {}", String::from_utf8_lossy(&out.stderr).trim());
  out.status.success().then_some(()).ok_or_else(|| io::Error::other(why()))
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::testing::{dir, write};

  /// A bundle of the id `id` at `app`, signed ad hoc; `build` tells two builds apart.
  fn bundle(app: &Path, id: &str, build: &str) {
    let plist = format!(
      "<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>CFBundleIdentifier</key>\
       <string>{id}</string><key>CFBundleExecutable</key><string>a</string>\
       <key>CFBundleVersion</key><string>{build}</string></dict></plist>"
    );
    write(app, &[("Contents/Info.plist", &plist)]);
    fs::create_dir(app.join("Contents/MacOS")).unwrap();
    fs::copy("/usr/bin/true", app.join("Contents/MacOS/a")).unwrap();
    let mut sign = Command::new("/usr/bin/codesign");
    assert!(sign.args(["-s", "-", "-f"]).arg(app).output().unwrap().status.success());
  }

  #[test]
  fn a_bundle_must_be_the_app_signed_alike() {
    let root = dir("bundle");
    let app = root.join("A.app");
    bundle(&app, "com.example.a", "1");
    // Another build signed ad hoc meets no requirement but its own; another app is refused.
    bundle(&root.join("v2/A.app"), "com.example.a", "2");
    bundle(&root.join("b/A.app"), "com.example.b", "1");
    for other in ["v2/A.app", "b/A.app"] {
      let e = accepts(&root.join(other), &app).unwrap_err();
      assert!(matches!(e, Error::Bundle(_)), "{other}: {e}");
    }

    // The exchange on the temp dir's APFS: each path holds the other's bundle.
    let plist = |p: &str| fs::read_to_string(root.join(p).join("Contents/Info.plist")).unwrap();
    let (v1, v2) = (plist("A.app"), plist("v2/A.app"));
    exchange(&app, &root.join("v2/A.app")).unwrap();
    assert_eq!((plist("A.app"), plist("v2/A.app")), (v2, v1));
    fs::remove_dir_all(root).unwrap();
  }

  #[test]
  fn a_disk_image_gives_its_one_app() {
    let root = dir("dmg");
    let app = root.join("A.app");
    bundle(&app, "com.example.a", "1");
    // A staging folder `name` with `update.dmg` made of `apps`, copies of the app, and a file.
    let image = |name: &str, apps: &[&str]| {
      let src = root.join(name).join("src");
      write(&src, &[("readme", "")]);
      for a in apps {
        assert!(
          Command::new("/usr/bin/ditto").arg(&app).arg(src.join(a)).status().unwrap().success()
        );
      }

      let dmg = root.join(name).join("update.dmg");
      let mut make = Command::new("/usr/bin/hdiutil");
      make.args(["create", "-quiet", "-format", "UDZO", "-srcfolder"]).arg(src).arg(&dmg);
      assert!(make.status().unwrap().success());
      dmg
    };

    let new = unpack(&image("one", &["A.app"]), &app).unwrap();
    assert_eq!(new, root.join("one/new/A.app"));
    let plist = |p: &Path| fs::read(p.join("Contents/Info.plist")).unwrap();
    assert_eq!(plist(&new), plist(&app), "copied out");
    accepts(&new, &app).unwrap();
    for (name, apps) in [("one", &[][..]), ("two", &["A.app", "B.app"]), ("none", &[])] {
      if name != "one" {
        let e = unpack(&image(name, apps), &app).unwrap_err();
        assert!(matches!(e, Error::Bundle(_)), "{name}: {e}");
      }

      assert!(!root.join(name).join("volume/readme").exists(), "{name}: detached");
    }

    let not_image = unpack(&app.join("Contents/Info.plist"), &app).unwrap_err();
    assert!(not_image.to_string().contains("hdiutil"), "{not_image}");
    fs::remove_dir_all(root).unwrap();
  }
}
