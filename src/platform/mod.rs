// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! The OS's side of the update, one module per OS picked by `target_os` as `imp`; the rest of the
//! crate is portable and reaches the OS only through it. Each provides:
//!
//! - `get`: a URL's body into a file over the OS's HTTP, its errors by [status] and [too_big].
//! - `unpack`: the new app of the release's downloaded file.
//! - `detach`: the installer spawned apart from the app.
//! - `wait_exit`: the installer's wait for the old app to end, which a reused pid can't fool.
//! - `alive`: whether a process runs, for the staging folders of ended installers.
//! - `start`: the installed app started with the installer's error, not waiting, without the focus.
//! - `busy`: an error of a rename or removal that may pass if tried again.
//!
//! macOS also `accepts` and `exchange`, Windows `setup`. On Unix `alive`, `busy`, `detach` and
//! `wait_exit` are the `unix` module's below, Linux's `detach` wrapped to close what the app
//! inherited.

#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "macos")]
pub use macos as imp;

#[cfg(windows)]
pub mod windows;
#[cfg(windows)]
pub use windows as imp;

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "linux")]
pub use linux as imp;

#[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
compile_error!("shipo runs on macOS, Windows and Linux");

use std::io;

/// `get`'s error for a body of more than `max` bytes.
fn too_big(url: &str, max: u64) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidData, format!("{url}: more than {max} bytes"))
}

/// `get`'s answer to the HTTP status `code` of `url`: 2xx is `Ok`, 404 `NotFound`.
fn status(url: &str, code: u32) -> io::Result<()> {
  match code {
    200..300 => Ok(()),
    404 => Err(io::Error::new(io::ErrorKind::NotFound, format!("{url}: HTTP 404"))),
    _ => Err(io::Error::other(format!("{url}: HTTP {code}"))),
  }
}

// MARK: Unix

/// The update's processes and files on every Unix: whether a process runs, the installer's wait
/// for the app that started it, the installer started apart from that app, and the errors a
/// removal tries again.
#[cfg(unix)]
mod unix {
  use std::os::unix::process::{CommandExt, parent_id};
  use std::process::{Child, Command};
  use std::time::{Duration, Instant};
  use std::{io, thread};

  /// Whether the process `pid` runs: `kill(pid, 0)`, which sends nothing. Another user's is not
  /// ours.
  pub fn alive(pid: u32) -> bool {
    unsafe extern "C" {
      fn kill(pid: i32, sig: i32) -> i32;
    }

    // SAFETY: signal 0 only checks; a pid of 0 or past i32 would name a group, never asked.
    i32::try_from(pid).is_ok_and(|pid| pid > 0 && unsafe { kill(pid, 0) } == 0)
  }

  /// The wait for the process `pid`, this one's parent, to end: whether it ended within the time.
  /// This one is reparented then, and no other process can become its parent, so a reused pid can't
  /// fool the wait.
  pub fn wait_exit(pid: u32) -> impl Fn(Duration) -> bool {
    move |cap| {
      let deadline = Instant::now() + cap;
      while parent_id() == pid && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
      }

      parent_id() != pid
    }
  }

  /// Spawns `cmd` in a process group of its own, so the terminal's hangup that ends the app doesn't
  /// reach it.
  pub fn detach(cmd: &mut Command) -> io::Result<Child> {
    cmd.process_group(0).spawn()
  }

  /// A folder not empty yet: macOS can leave entries out of a folder's listing while it is emptied
  /// (a delete once stopped "not empty" with 51 folders never visited); on Linux a file came into
  /// it while it was emptied.
  pub fn busy(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::DirectoryNotEmpty
  }

  #[cfg(test)]
  mod tests {
    use super::*;

    #[test]
    fn the_wait_ends_with_the_parent_or_the_cap() {
      let start = Instant::now();
      assert!(wait_exit(parent_id().wrapping_add(1))(Duration::from_secs(5)), "not the parent");
      assert!(start.elapsed() < Duration::from_secs(1));
      assert!(!wait_exit(parent_id())(Duration::from_millis(60)), "the parent runs");
      assert!(start.elapsed() >= Duration::from_millis(60));
      assert!(alive(std::process::id()) && !alive(0));
    }
  }
}
