// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! The installer, which [crate::finish] is when [crate::install] starts it with [INSTALL]: its job
//! read from its arguments, the old app's end awaited, the new app put in and started, the swap
//! undone on a failure; and the staging folder it works in, the app's handshake with it, and the
//! cleanup of what one stopped midway left.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use std::{env, fs, io, thread};

use crate::platform::imp;
use crate::{Error, OS, Os, Result};

/// The argument that makes [crate::finish] the installer.
pub const INSTALL: &str = "--shipo-install";
/// The environment variable that carries an installer's error to the app it starts.
pub const NOTE: &str = "SHIPO_UPDATE_ERROR";
/// How long [crate::install] waits for the installer's [MARK].
pub const READY: Duration = Duration::from_secs(10);
/// How long the installer waits for the old app to end.
const QUIT: Duration = Duration::from_secs(30);
/// The installer's mark in the staging folder, its pid: it runs.
const MARK: &str = "ready";
/// A staging folder's name before `<pid>-<nanos>`, the pid of the app that made it.
const STAGING: &str = ".shipo-update-";

/// Where `target`'s staging folders go: on Windows in the app's folder, which the setup writes
/// into file by file and whose `.shipo-update-*` folders the uninstaller removes, so no other
/// app's are reached; elsewhere beside the target, which the swap renames (the target itself when
/// it has no parent).
pub fn home(os: Os, target: &Path) -> &Path {
  match os {
    Os::Windows => target,
    _ => target.parent().unwrap_or(target),
  }
}

// MARK: Job

/// What an installer does: waits for `pid` to end, puts `new`, in the staging folder `dir`, in
/// `target`'s place and starts it with `args`.
#[derive(Debug, PartialEq)]
pub struct Job {
  pid: u32,
  new: PathBuf,
  target: PathBuf,
  dir: PathBuf,
  args: Vec<OsString>,
}

/// The installer's job `args` (the program's own skipped) ask: `None` for an app's own arguments,
/// `Some(None)` for malformed ones. They are [INSTALL] `<pid> <new> <target> -- <the app's>`,
/// `target` absolute, `new` inside a staging folder in its [home].
pub fn job(os: Os, args: &[OsString]) -> Option<Option<Job>> {
  let [flag, rest @ ..] = args else { return None };
  if flag != INSTALL {
    return None;
  }

  let [pid, new, target, dash, args @ ..] = rest else { return Some(None) };
  let pid = pid.to_str().and_then(|p| p.parse().ok());
  let (new, target) = (PathBuf::from(new), PathBuf::from(target));
  let home = target.is_absolute().then(|| home(os, &target));
  let inside = home.and_then(|p| new.strip_prefix(p).ok());
  let inside = inside.filter(|r| r.components().count() > 1);
  let inside = inside.filter(|r| r.components().all(|c| matches!(c, Component::Normal(_))));
  let dir =
    inside.and_then(|r| r.iter().next()).filter(|d| d.to_string_lossy().starts_with(STAGING));
  let dir = home.zip(dir).map(|(h, d)| h.join(d));
  Some(match (pid, dir, dash == "--") {
    (Some(pid), Some(dir), true) => Some(Job { pid, new, target, dir, args: args.to_vec() }),
    _ => None,
  })
}

// MARK: Install

/// Runs the installer's `job` with the OS's calls and exits: 0 once the new app runs, 1 on a
/// failure.
pub fn run(job: &Job) -> ! {
  // Windows' wait holds the old process's handle from here, before the mark lets it quit.
  let wait = imp::wait_exit(job.pid);
  #[cfg(target_os = "macos")]
  let exchange = imp::exchange;
  #[cfg(not(target_os = "macos"))]
  let exchange = |_: &Path, _: &Path| -> io::Result<()> { Err(io::ErrorKind::Unsupported.into()) };
  #[cfg(windows)]
  let setup = imp::setup;
  #[cfg(not(windows))]
  let setup = |_: &Path, _: &Path| -> io::Result<i32> { Err(io::ErrorKind::Unsupported.into()) };
  let rename = |a: &Path, b: &Path| fs::rename(a, b);
  let (wait, start) = (&wait, &imp::start);
  let os = Seams { wait, exchange: &exchange, rename: &rename, setup: &setup, start };
  std::process::exit(apply(OS, job, &os).map_or(1, |()| 0))
}

/// What the installer does to the system: the OS's, or a test's.
struct Seams<'a> {
  /// Whether the old app ended within the time ([imp::wait_exit]).
  wait: &'a dyn Fn(Duration) -> bool,
  /// Two paths exchanged in one step: macOS' alone.
  exchange: Rename<'a>,
  rename: Rename<'a>,
  /// Windows' setup run to its end into a folder, its exit code.
  setup: &'a dyn Fn(&Path, &Path) -> io::Result<i32>,
  /// The app at a path started with arguments and an error for its note ([imp::start]).
  start: Start<'a>,
}

/// An app started with arguments and an error for its note.
type Start<'a> = &'a dyn Fn(&Path, &[OsString], Option<&str>) -> io::Result<()>;

/// The installer's steps: its [MARK] written, the old app's end awaited (`TimedOut` past [QUIT],
/// nothing changed), the new app put in and started; on a failure the swap undone and the old app
/// started again with the error in its note. What is left is removed either way.
fn apply(os: Os, job: &Job, s: &Seams) -> io::Result<()> {
  fs::write(job.dir.join(MARK), std::process::id().to_string())?;
  let done = match (s.wait)(QUIT) {
    false => {
      Err(io::Error::new(io::ErrorKind::TimedOut, format!("the app didn't quit in {QUIT:?}")))
    }
    true => put(os, job, s).map_err(|e| {
      // The note, which the app's `finish` gives as `Error::Install`.
      let e = io::Error::new(e.kind(), format!("{}: {e}", job.target.display()));
      let _ = (s.start)(&job.target, &job.args, Some(&e.to_string()));
      e
    }),
  };

  if os != Os::Windows {
    let _ = (remove(&old(&job.target)), remove(&job.dir));
    return done;
  }

  // On Windows the installer runs from the staging folder, which can't go while it does: all but
  // its exe goes, and the next download removes that.
  let exe = env::current_exe().ok().and_then(|e| e.file_name().map(OsString::from));
  for e in fs::read_dir(&job.dir).into_iter().flatten().flatten() {
    if Some(e.file_name()) != exe {
      let _ = remove(&e.path());
    }
  }

  done
}

/// The new app put in the target's place and started: Windows' setup run, an exit code other
/// than 0 an error; else swapped in, the swap undone if it doesn't start.
fn put(os: Os, job: &Job, s: &Seams) -> io::Result<()> {
  if os == Os::Windows {
    return match (s.setup)(&job.new, &job.target)? {
      0 => (s.start)(&job.target, &job.args, None),
      code => Err(io::Error::other(format!("{} exited with {code}", job.new.display()))),
    };
  }

  let (undo, from, to) = swap(os, &job.new, &job.target, s)?;
  (s.start)(&job.target, &job.args, None).map_err(|e| also(e, retry(|| undo(&from, &to))))
}

/// Swaps the app `new` in for `target` whole: on macOS in one exchange, which left `new` holding
/// the old app; on Linux `target` linked to its [old] name and `new` renamed over it in one step,
/// tried again while it may pass. Failing, the target is as it was (Linux' link is removed with
/// what is left). What undoes it: the exchange again, or the [old] copy renamed back.
fn swap<'a>(
  os: Os,
  new: &Path,
  target: &Path,
  s: &Seams<'a>,
) -> io::Result<(Rename<'a>, PathBuf, PathBuf)> {
  if os == Os::Mac {
    (s.exchange)(new, target)?;
    return Ok((s.exchange, new.to_owned(), target.to_owned()));
  }

  let old = old(target);
  remove(&old)?;
  fs::hard_link(target, &old)?;
  retry(|| (s.rename)(new, target))?;
  Ok((s.rename, old, target.to_owned()))
}

/// A rename, [fs::rename] or one that fails in tests.
type Rename<'a> = &'a dyn Fn(&Path, &Path) -> io::Result<()>;

/// The error `e`, saying so when undoing it failed too.
fn also(e: io::Error, undone: io::Result<()>) -> io::Error {
  match undone {
    Ok(()) => e,
    Err(u) => io::Error::new(e.kind(), format!("{e}, and undoing it failed: {u}")),
  }
}

// MARK: Handshake

/// Starts the installer with `spawn`, which gives what kills it, and waits up to `cap` for its
/// [MARK] in `dir`; [Error::Install], the installer killed, when none comes.
pub fn handshake<K: FnOnce()>(
  dir: &Path,
  cap: Duration,
  spawn: impl FnOnce() -> io::Result<K>,
) -> Result<()> {
  let (kill, deadline) = (spawn()?, Instant::now() + cap);
  while !dir.join(MARK).exists() {
    if Instant::now() > deadline {
      kill();
      return Err(Error::Install(format!("the installer gave no sign in {cap:?}")));
    }

    thread::sleep(Duration::from_millis(20));
  }

  Ok(())
}

// MARK: Cleanup

/// Removes what an installer stopped midway left of `target`: its [old] copy (none on Windows),
/// and the staging folders in its [home] whose app and installer (the pid in its [MARK]) both
/// ended (by `alive`).
pub fn tidy(os: Os, target: &Path, alive: &dyn Fn(u32) -> bool) {
  let all = fs::read_dir(home(os, target)).into_iter().flatten().flatten();
  for e in all {
    let name = e.file_name();
    let pid = name.to_str().and_then(|n| n.strip_prefix(STAGING)?.split('-').next()?.parse().ok());
    let installer = fs::read_to_string(e.path().join(MARK)).ok().and_then(|p| p.parse().ok());
    if pid.is_some_and(|p| !alive(p)) && !installer.is_some_and(alive) {
      let _ = remove(&e.path());
    }
  }

  if os != Os::Windows {
    let _ = remove(&old(target));
  }
}

/// Where a swap moves what it replaces: `.<name>.shipo-old` beside it, a suffix only shipo makes,
/// so cleanup removes no user's `.old`.
fn old(target: &Path) -> PathBuf {
  let mut name = OsString::from(".");
  name.push(target.file_name().unwrap_or_default());
  name.push(".shipo-old");
  target.with_file_name(name)
}

// MARK: Files

/// A new staging folder in `parent` that only the user reaches (0700 on Unix), removed with what
/// it holds on drop. Made with `create_dir`, so one already there (a link planted in a shared
/// `/tmp`) is an error.
#[derive(Debug)]
pub struct Folder(pub PathBuf);

impl Folder {
  pub fn new(parent: &Path) -> io::Result<Self> {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let dir = parent.join(format!("{STAGING}{}-{nanos}", std::process::id()));
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut made = fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut made, 0o700);
    let at = |e: io::Error| io::Error::new(e.kind(), format!("{}: {e}", parent.display()));
    made.create(&dir).map_err(at)?;
    Ok(Self(dir))
  }

  /// Its path, the folder kept: the empty path left behind can't be removed.
  pub fn keep(mut self) -> PathBuf {
    std::mem::take(&mut self.0)
  }
}

impl Drop for Folder {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.0);
  }
}

/// Deletes `path`, a folder with all in it or a file; `Ok` when it's not there. Tried again while
/// the error may pass ([retry]).
fn remove(path: &Path) -> io::Result<()> {
  let removed = retry(|| match path.is_dir() {
    true => fs::remove_dir_all(path),
    false => fs::remove_file(path),
  });

  match removed {
    Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
    done => done,
  }
}

/// `step` tried again for up to 5 s while its error may pass ([imp::busy]): macOS can leave
/// entries out of a folder's listing while it is emptied ("not empty"); on Windows the app that
/// just quit may hold its files open a moment after it ended. Its last result.
fn retry(mut step: impl FnMut() -> io::Result<()>) -> io::Result<()> {
  let deadline = Instant::now() + Duration::from_secs(5);
  loop {
    match step() {
      Err(e) if imp::busy(&e) && Instant::now() < deadline => {
        thread::sleep(Duration::from_millis(50))
      }
      done => return done,
    }
  }
}

#[cfg(test)]
mod tests {
  use std::cell::{Cell, RefCell};

  use super::*;
  use crate::testing::{dir, write};

  /// Whether each `(path, text)` under `root` holds its text, `""` for one that is not there.
  fn holds(root: &Path, files: &[(&str, &str)], when: &str) {
    for (path, text) in files {
      assert_eq!(fs::read_to_string(root.join(path)).unwrap_or_default(), *text, "{path} {when}");
    }
  }

  /// macOS' exchange as three renames, through a name of its own.
  fn exchange(a: &Path, b: &Path) -> io::Result<()> {
    let via = a.with_extension("via");
    fs::rename(a, &via).and_then(|()| fs::rename(b, a)).and_then(|()| fs::rename(&via, b))
  }

  fn rename(a: &Path, b: &Path) -> io::Result<()> {
    fs::rename(a, b)
  }

  /// The OS's renames, the exchange, an app that quits, a setup that installs, an app that starts.
  const SEAMS: Seams = Seams {
    wait: &|_| true,
    exchange: &exchange,
    rename: &rename,
    setup: &|_, _| Ok(0),
    start: &|_, _, _| Ok(()),
  };

  /// What a start was given: the app, its arguments, the note.
  type Started = RefCell<Vec<(PathBuf, Vec<OsString>, Option<String>)>>;

  /// A start that records what it was given in `started`, failing the first time if `fail`.
  fn starts(
    started: &Started,
    fail: bool,
  ) -> impl Fn(&Path, &[OsString], Option<&str>) -> io::Result<()> {
    move |app, args, note| {
      started.borrow_mut().push((app.to_owned(), args.to_vec(), note.map(String::from)));
      match fail && started.borrow().len() == 1 {
        true => Err(io::Error::other("no start")),
        false => Ok(()),
      }
    }
  }

  /// The installer's arguments of `pid`, `new` and `target` under `root`, the app's `-x`.
  fn args(root: &Path, pid: &str, new: &str, target: &str) -> Vec<OsString> {
    let (new, target) = (root.join(new).into_os_string(), root.join(target).into_os_string());
    vec![INSTALL.into(), pid.into(), new, target, "--".into(), "-x".into()]
  }

  /// `os`' installer's job of `new` and `target` under `root`.
  fn install_job(root: &Path, os: Os, new: &str, target: &str) -> Job {
    job(os, &args(root, "7", new, target)).unwrap().unwrap()
  }

  /// A macOS and a Linux app under `root` and their new versions in a staging folder: a bundle
  /// holding a file `gone` that the new version drops, and an AppImage.
  fn apps(root: &Path) -> [(Os, &'static str, &'static str); 2] {
    write(root, &[("A.app/Contents/MacOS/a", "1"), ("A.app/gone", "1"), ("A.AppImage", "1")]);
    write(
      root,
      &[(".shipo-update-9/new/A.app/Contents/MacOS/a", "2"), (".shipo-update-9/app", "2")],
    );
    [
      (Os::Mac, ".shipo-update-9/new/A.app", "A.app"),
      (Os::Linux, ".shipo-update-9/app", "A.AppImage"),
    ]
  }

  /// Whether `os`'s app under `root` is the new version (`new`) or the old.
  fn installed(root: &Path, os: Os, new: bool, when: &str) {
    let (v, gone) = if new { ("2", "") } else { ("1", "1") };
    let files = match os {
      Os::Linux => vec![("A.AppImage", v)],
      _ => vec![("A.app/Contents/MacOS/a", v), ("A.app/gone", gone)],
    };

    holds(root, &files, &format!("{os:?} {when}"));
  }

  #[test]
  fn each_os_swaps_the_whole_app() {
    let root = dir("swap");
    for (os, new, target) in apps(&root) {
      swap(os, &root.join(new), &root.join(target), &SEAMS).unwrap();
      installed(&root, os, true, "swapped");
    }

    let files = [
      (".shipo-update-9/new/A.app/gone", "1"),
      (".A.AppImage.shipo-old", "1"),
      (".shipo-update-9/app", ""),
    ];
    holds(&root, &files, "the old apps aside");
    fs::remove_dir_all(root).unwrap();
  }

  #[test]
  fn a_failing_swap_leaves_the_target() {
    let root = dir("swap-fails");
    let busy = |_: &Path, _: &Path| Err(io::Error::other("busy"));
    for (os, new, target) in apps(&root) {
      let seams = Seams { exchange: &busy, rename: &busy, ..SEAMS };
      let e = swap(os, &root.join(new), &root.join(target), &seams).map(drop).unwrap_err();
      assert_eq!(e.to_string(), "busy", "{os:?}");
      installed(&root, os, false, "unchanged");
    }

    holds(&root, &[(".shipo-update-9/app", "2")], "");
    fs::remove_dir_all(root).unwrap();
  }

  #[test]
  fn a_failing_start_undoes_the_swap_and_starts_the_old_app() {
    for i in 0..2 {
      let root = dir(&format!("start-{i}"));
      let (os, new, target) = apps(&root)[i];
      let started = Started::default();
      let job = install_job(&root, os, new, target);
      let e = apply(os, &job, &Seams { start: &starts(&started, true), ..SEAMS }).unwrap_err();
      assert!(e.to_string().contains("no start"), "{e}");
      installed(&root, os, false, "undone");
      let start = |note| (job.target.clone(), vec![OsString::from("-x")], note);
      assert_eq!(*started.borrow(), [start(None), start(Some(e.to_string()))], "the note");
      assert!(!old(&job.target).exists() && !job.dir.exists(), "{os:?}: what is left");
      fs::remove_dir_all(root).unwrap();
    }
  }

  #[cfg(unix)]
  #[test]
  fn an_app_in_a_read_only_folder_still_gets_its_error() {
    use std::os::unix::fs::PermissionsExt;
    let root = dir("read-only-install");
    for (os, new, target) in apps(&root) {
      let (started, job) = (Started::default(), install_job(&root, os, new, target));
      fs::set_permissions(&root, fs::Permissions::from_mode(0o555)).unwrap();
      let e = apply(os, &job, &Seams { start: &starts(&started, false), ..SEAMS }).unwrap_err();
      fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
      assert_eq!(e.kind(), io::ErrorKind::PermissionDenied, "{os:?}: {e}");
      assert_eq!(started.borrow()[0].2, Some(e.to_string()), "{os:?}");
      installed(&root, os, false, "unchanged");
    }

    fs::remove_dir_all(root).unwrap();
  }

  #[test]
  fn an_install_waits_for_the_app_then_swaps_and_starts_it() {
    for i in 0..2 {
      let root = dir(&format!("install-{i}"));
      let (os, new, target) = apps(&root)[i];
      let (job, waited, started) =
        (install_job(&root, os, new, target), Cell::new(None), Started::default());
      let wait = |cap| {
        let mark = fs::read_to_string(job.dir.join(MARK)).unwrap_or_default();
        waited.set(Some((cap, mark == std::process::id().to_string())));
        true
      };
      apply(os, &job, &Seams { wait: &wait, start: &starts(&started, false), ..SEAMS }).unwrap();
      assert_eq!(waited.get(), Some((QUIT, true)), "the mark, the installer's pid, first");
      installed(&root, os, true, "installed");
      assert_eq!(*started.borrow(), [(job.target.clone(), vec![OsString::from("-x")], None)]);
      assert!(!old(&job.target).exists() && !job.dir.exists(), "{os:?}: what is left");
      fs::remove_dir_all(root).unwrap();
    }
  }

  #[test]
  fn windows_runs_the_setup_then_starts_what_it_left() {
    // The installer's exe, which stays for the next download.
    let exe = env::current_exe().unwrap().file_name().unwrap().to_owned();
    for (i, code) in [Some(0), Some(1), Some(2), None].into_iter().enumerate() {
      let root = dir(&format!("setup-{i}"));
      write(
        &root,
        &[
          ("W/a.exe", "1"),
          ("W/.shipo-update-9/A-v2-windows-x64-setup.exe", "2"),
          ("W/.shipo-update-9/ready", ""),
        ],
      );
      fs::write(root.join("W/.shipo-update-9").join(&exe), "1").unwrap();
      let (job, ran, started) = (
        install_job(&root, Os::Windows, "W/.shipo-update-9/A-v2-windows-x64-setup.exe", "W"),
        RefCell::new(Vec::new()),
        Started::default(),
      );
      let setup = |exe: &Path, to: &Path| {
        ran.borrow_mut().push((exe.to_owned(), to.to_owned()));
        code.ok_or_else(|| io::Error::other("no setup"))
      };
      let done = apply(
        Os::Windows,
        &job,
        &Seams { setup: &setup, start: &starts(&started, false), ..SEAMS },
      );
      assert_eq!(*ran.borrow(), [(job.new.clone(), job.target.clone())]);
      let note = done.err().map(|e| e.to_string());
      let why = [
        "",
        "A-v2-windows-x64-setup.exe exited with 1",
        "A-v2-windows-x64-setup.exe exited with 2",
        "no setup",
      ][i];
      assert!(note.as_ref().map_or(i == 0, |n| i > 0 && n.contains(why)), "{note:?}");
      assert_eq!(*started.borrow(), [(job.target.clone(), vec![OsString::from("-x")], note)]);
      let left =
        fs::read_dir(&job.dir).unwrap().flatten().map(|e| e.file_name()).collect::<Vec<_>>();
      assert_eq!(left, std::slice::from_ref(&exe), "all but the installer's exe goes");
      fs::remove_dir_all(root).unwrap();
    }
  }

  #[test]
  fn an_app_that_doesnt_quit_changes_nothing() {
    let root = dir("stays");
    let [(os, new, target), _] = apps(&root);
    let (job, started) = (install_job(&root, os, new, target), Started::default());
    let seams = Seams { wait: &|_| false, start: &starts(&started, false), ..SEAMS };
    assert_eq!(apply(os, &job, &seams).unwrap_err().kind(), io::ErrorKind::TimedOut);
    installed(&root, os, false, "after the wait");
    assert!(started.borrow().is_empty() && !job.dir.exists());
    fs::remove_dir_all(root).unwrap();
  }

  #[test]
  fn the_installers_arguments() {
    let root = env::temp_dir();
    let job = install_job(&root, Os::Mac, ".shipo-update-1-2/new/A.app", "A.app");
    assert_eq!((job.pid, &job.new), (7, &root.join(".shipo-update-1-2/new/A.app")));
    assert_eq!((&job.dir, &job.args[..]), (&root.join(".shipo-update-1-2"), &["-x".into()][..]));
    let job = install_job(&root, Os::Windows, "W/.shipo-update-1-2/s.exe", "W");
    assert_eq!(job.dir, root.join("W/.shipo-update-1-2"), "Windows' inside the app's folder");
    assert_eq!(super::job(Os::Mac, &["-x".into()]), None, "an app's own");
    assert_eq!(super::job(Os::Mac, &[]), None);
    let new = ".shipo-update-1/new/A.app";
    let mut no_dash = args(&root, "7", new, "A.app");
    no_dash[4] = "-x".into();
    let relative = [INSTALL, "7", new, "A.app", "--"].map(OsString::from).to_vec();
    for args in [
      args(&root, "x7", new, "A.app"),
      args(&root, "-7", new, "A.app"),
      args(&root, "7", "other/new/A.app", "A.app"),
      args(&root, "7", ".shipo-update-1", "A.app"),
      args(&root, "7", ".shipo-update-1/../A.app", "A.app"),
      args(&root, "7", new, "A.app")[..4].to_vec(),
      no_dash,
      relative,
    ] {
      assert_eq!(super::job(Os::Mac, &args), Some(None), "{args:?}");
    }

    let beside = args(&root, "7", ".shipo-update-1/s.exe", "W");
    assert_eq!(super::job(Os::Windows, &beside), Some(None), "Windows' beside the folder");
  }

  #[test]
  fn download_removes_what_an_ended_installer_left() {
    let root = dir("tidy");
    write(&root, &[(".shipo-update-1-5/x", ""), (".shipo-update-2-5/x", "")]);
    // The app ended, its installer runs (2) or ended (3).
    write(&root, &[(".shipo-update-1-6/ready", "2"), (".shipo-update-1-7/ready", "3")]);
    write(&root, &[(".A.app.shipo-old/x", ""), ("other/x", "")]);
    tidy(Os::Mac, &root.join("A.app"), &|pid| pid == 2);
    let names =
      [".shipo-update-1-5", ".shipo-update-2-5", ".shipo-update-1-6", ".shipo-update-1-7"];
    assert_eq!(names.map(|n| root.join(n).exists()), [false, true, true, false]);
    assert_eq!([".A.app.shipo-old", "other"].map(|n| root.join(n).exists()), [false, true]);

    // Windows' in the app's folder alone: another app's beside it stays.
    write(&root, &[("W/.shipo-update-1-5/x", ""), (".shipo-update-1-5/x", "")]);
    tidy(Os::Windows, &root.join("W"), &|pid| pid == 2);
    let names = ["W/.shipo-update-1-5", ".shipo-update-1-5", ".shipo-update-2-5"];
    assert_eq!(names.map(|n| root.join(n).exists()), [false, true, true]);
    fs::remove_dir_all(root).unwrap();
  }

  #[test]
  fn install_waits_for_the_installers_mark() {
    let (root, cap, killed) = (dir("mark"), Duration::from_millis(100), Cell::new(false));
    let e = handshake(&root, cap, || Ok(|| killed.set(true))).unwrap_err();
    assert!(matches!(e, Error::Install(_)), "{e}");
    assert!(killed.get() && root.exists(), "killed, the staging folder kept");
    killed.set(false);
    let mark = root.join(MARK);
    handshake(&root, READY, || {
      thread::spawn(move || fs::write(mark, ""));
      Ok(|| killed.set(true))
    })
    .unwrap();
    assert!(!killed.get(), "a running installer is left alone");
    fs::remove_dir_all(root).unwrap();
  }

  #[test]
  fn remove_takes_a_tree_a_file_and_nothing() {
    let root = dir("remove");
    write(&root, &[("a/b/c/d.txt", "d"), ("a/e.txt", "e"), ("f.txt", "f")]);
    remove(&root.join("f.txt")).unwrap();
    assert!(!root.join("f.txt").exists());
    remove(&root.join("a")).unwrap();
    assert!(!root.join("a").exists());
    remove(&root.join("a")).unwrap();
    remove(&root).unwrap();
    assert!(!root.exists());
  }
}
