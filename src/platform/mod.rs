// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! The OS's side of the update, one module per OS picked by `target_os` as `imp`; the rest of the
//! crate is portable and reaches the OS only through it. Each provides:
//!
//! - `get(url: &str, to: &Path, max: u64, progress: &mut dyn FnMut(u64)) -> io::Result<()>`: the
//!   body of `url` into a new file at `to` over the OS's HTTP (macOS `NSURLSession`, Windows
//!   WinHTTP, Linux libcurl loaded at run time), following redirects, with the bytes so far told;
//!   `NotFound` for a 404, another status an error ([status]), `InvalidData` past `max`
//!   ([too_big]).
//! - `unpack(file: &Path, app: &Path) -> io::Result<PathBuf>`: the new app of the release's file
//!   downloaded for `app` (macOS the image's one bundle, copied out and checked; Windows the setup
//!   as it came; Linux the AppImage made executable).
//! - `detach(&mut Command) -> io::Result<Child>`: the installer spawned apart from the app.
//! - `wait_exit`: whether the process `pid` ended within a time (Unix `wait_exit(pid: u32, cap:
//!   Duration) -> bool`, this process's parent; Windows `wait_exit(pid: u32) -> impl Fn(u32,
//!   Duration) -> bool`, its handle opened at the call, before the installer's mark).
//! - `alive(pid: u32) -> bool`, for the staging folders of ended installers.
//! - `start(app: &Path, args: &[OsString], note: Option<&str>) -> io::Result<()>`: the installed
//!   app started with `args` and `note` in its `SHIPO_UPDATE_ERROR` (none inherited), without
//!   waiting for it and without the focus.
//! - `busy(e: &io::Error) -> bool`: an error of a rename or removal that may pass if tried again.
//!
//! macOS also `accepts` (the new bundle may replace the running one) and `exchange` (two paths
//! swapped in one step); Windows also `setup` (the per-user setup run silent, its exit code). On
//! Unix `detach`, `wait_exit` and `alive` are [unix]'s.

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

#[cfg(unix)]
mod unix;

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
