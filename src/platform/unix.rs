// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! The update's processes on every Unix: whether one runs, the installer's wait for the app that
//! started it, and the installer started apart from that app.

use std::os::unix::process::{CommandExt, parent_id};
use std::process::{Child, Command};
use std::thread;
use std::time::{Duration, Instant};

/// Whether the process `pid` runs: `kill(pid, 0)`, which sends nothing. Another user's is not ours.
pub fn alive(pid: u32) -> bool {
  unsafe extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
  }

  // SAFETY: signal 0 only checks; a pid of 0 or past i32 would name a group, never asked.
  i32::try_from(pid).is_ok_and(|pid| pid > 0 && unsafe { kill(pid, 0) } == 0)
}

/// Whether the process `pid`, this one's parent, ended within `cap`: this one is reparented then,
/// and no other process can become its parent, so a reused pid can't fool the wait.
pub fn wait_exit(pid: u32, cap: Duration) -> bool {
  let deadline = Instant::now() + cap;
  while parent_id() == pid && Instant::now() < deadline {
    thread::sleep(Duration::from_millis(20));
  }

  parent_id() != pid
}

/// Spawns `cmd` in a process group of its own, so the terminal's hangup that ends the app doesn't
/// reach it; on Linux with nothing inherited past stderr ([closed]).
pub fn detach(cmd: &mut Command) -> std::io::Result<Child> {
  closed(cmd.process_group(0)).spawn()
}

/// `cmd` started with no descriptor past stderr: an AppImage's runtime leaves its FUSE daemon's
/// pipe open in the app, and the app started by the update would keep the old one mounted.
#[cfg(target_os = "linux")]
pub fn closed(cmd: &mut Command) -> &mut Command {
  unsafe extern "C" {
    fn close_range(first: u32, last: u32, flags: i32) -> i32;
    fn close(fd: i32) -> i32;
  }

  // SAFETY: only `close`, async-signal-safe, between fork and exec; close_range is glibc 2.34's,
  // the loop the older kernels' way.
  unsafe {
    cmd.pre_exec(|| {
      if close_range(3, u32::MAX, 0) != 0 {
        (3..1024).for_each(|fd| _ = close(fd));
      }

      Ok(())
    })
  }
}

#[cfg(not(target_os = "linux"))]
pub fn closed(cmd: &mut Command) -> &mut Command {
  cmd
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_wait_ends_with_the_parent_or_the_cap() {
    let start = Instant::now();
    assert!(wait_exit(parent_id().wrapping_add(1), Duration::from_secs(5)), "not the parent");
    assert!(start.elapsed() < Duration::from_secs(1));
    assert!(!wait_exit(parent_id(), Duration::from_millis(60)), "the parent runs");
    assert!(start.elapsed() >= Duration::from_millis(60));
    assert!(alive(std::process::id()) && !alive(0));
  }
}
