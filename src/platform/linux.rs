// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! The update's calls on Linux, which unpacks nothing: the file fetched by libcurl, loaded at run
//! time (`dlopen`, no build-time C and no binding crate); the AppImage downloaded made executable,
//! then started in the old one's place.

use std::any::Any;
use std::ffi::{CStr, CString, OsString, c_char, c_int, c_long, c_void};
use std::fs::{self, File};
use std::io::{self, Write};
use std::mem::transmute_copy;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::OnceLock;

pub use super::unix::{access, alive, busy, wait_exit};
use super::{Big, Progress};
use crate::installer::NOTE;
use crate::{Error, Result};

// MARK: libcurl

type P = *mut c_void;

// glibc (libc.so.6 since 2.34; Rust's std links it), <dlfcn.h>.
unsafe extern "C" {
  fn dlopen(name: *const c_char, flags: c_int) -> P;
  fn dlsym(handle: P, name: *const c_char) -> P;
}

const RTLD_NOW: c_int = 2;

// <curl/curl.h>: options are a type's base (long 0, object 10000, function 20000) plus a number.
const URL: c_int = 10002;
const WRITEDATA: c_int = 10001;
const WRITEFUNCTION: c_int = 20011;
const FOLLOWLOCATION: c_int = 52;
const MAXREDIRS: c_int = 68;
/// Since 7.85; before it the bit mask of `REDIR_PROTOCOLS` (Ubuntu 22.04's 7.81).
const REDIR_PROTOCOLS_STR: c_int = 10319;
const REDIR_PROTOCOLS: c_int = 182;
const NOSIGNAL: c_int = 99;
/// Below a byte a second for a minute, the transfer stops.
const LOW_SPEED_LIMIT: c_int = 19;
const LOW_SPEED_TIME: c_int = 20;
/// `CURLINFO_LONG + 2`.
const RESPONSE_CODE: c_int = 0x200002;

/// libcurl's easy API, its functions found by name in the library, never closed.
struct Curl {
  global_init: unsafe extern "C" fn(c_long) -> c_int,
  init: unsafe extern "C" fn() -> P,
  perform: unsafe extern "C" fn(P) -> c_int,
  cleanup: unsafe extern "C" fn(P),
  strerror: unsafe extern "C" fn(c_int) -> *const c_char,
  setopt: unsafe extern "C" fn(P, c_int, ...) -> c_int,
  getinfo: unsafe extern "C" fn(P, c_int, ...) -> c_int,
}

/// libcurl loaded once (`libcurl.so.4`, else Debian's GnuTLS build) and initialized; `None` when
/// it is not installed.
fn curl() -> Option<&'static Curl> {
  static CURL: OnceLock<Option<Curl>> = OnceLock::new();
  let load = || {
    let names = [c"libcurl.so.4", c"libcurl-gnutls.so.4"];
    // SAFETY: a soname, loaded once and never closed: its functions live as long as the process.
    let lib =
      names.iter().map(|n| unsafe { dlopen(n.as_ptr(), RTLD_NOW) }).find(|h| !h.is_null())?;
    // SAFETY: each field's type is the curl.h declaration of the symbol it is found by.
    let curl = unsafe {
      Curl {
        global_init: sym(lib, c"curl_global_init")?,
        init: sym(lib, c"curl_easy_init")?,
        perform: sym(lib, c"curl_easy_perform")?,
        cleanup: sym(lib, c"curl_easy_cleanup")?,
        strerror: sym(lib, c"curl_easy_strerror")?,
        setopt: sym(lib, c"curl_easy_setopt")?,
        getinfo: sym(lib, c"curl_easy_getinfo")?,
      }
    };

    // SAFETY: once, before any handle; `CURL_GLOBAL_DEFAULT`.
    (unsafe { (curl.global_init)(3) } == 0).then_some(curl)
  };
  CURL.get_or_init(load).as_ref()
}

/// The address of `name` in the open library `lib`, as the function pointer type `T`.
///
/// # Safety
/// `T` must be the symbol's real signature, a pointer-sized `extern "C" fn`.
unsafe fn sym<T: Copy>(lib: P, name: &CStr) -> Option<T> {
  const { assert!(size_of::<T>() == size_of::<P>()) };
  // SAFETY: a library `dlopen` gave, never closed.
  let p = unsafe { dlsym(lib, name.as_ptr()) };
  // SAFETY: the caller names the symbol's type; a fn pointer is a non-null pointer.
  (!p.is_null()).then(|| unsafe { transmute_copy::<P, T>(&p) })
}

/// What a transfer's write callback, [fn@write], works with.
struct Out<'a> {
  file: File,
  /// The error that stopped it: a bare `InvalidData` past the bytes it may take.
  err: Option<io::Error>,
  /// The bytes it may still take.
  left: u64,
  /// Told the bytes left after each write.
  tell: &'a mut dyn FnMut(u64),
  /// A panic of `tell`, resumed once curl returns.
  panicked: Option<Box<dyn Any + Send>>,
}

/// curl's `CURLOPT_WRITEFUNCTION`: `size × n` bytes at `data`, all taken or the transfer stops.
unsafe extern "C" fn write(data: *const u8, size: usize, n: usize, out: P) -> usize {
  // SAFETY: curl passes the `Out` set as `CURLOPT_WRITEDATA` and that many bytes.
  let (out, data) =
    unsafe { (&mut *out.cast::<Out>(), std::slice::from_raw_parts(data, size * n)) };
  let fits = out.left.checked_sub(data.len() as u64).ok_or(io::ErrorKind::InvalidData.into());
  // Called no more once it took nothing.
  out.err = fits.and_then(|l| out.file.write_all(data).map(|()| out.left = l)).err();
  // A panic can't unwind through curl: it stops the transfer and goes on past it.
  out.panicked = panic::catch_unwind(AssertUnwindSafe(|| (out.tell)(out.left))).err();
  if out.err.is_some() || out.panicked.is_some() { 0 } else { size * n }
}

/// The body of `url` into `to` by libcurl's easy API, its write callback into the file, redirects
/// only to https (http too from loopback), `big` the error past `max` bytes
/// ([libcurl-easy](https://curl.se/libcurl/c/libcurl-easy.html), as of 2026-10).
pub fn get(url: &str, to: &Path, max: u64, big: Big, progress: Progress) -> Result<()> {
  let none = || Error::Unsupported("libcurl.so.4 is not installed".into());
  let c = curl().ok_or_else(none)?;
  let c_url = CString::new(url).map_err(|_| super::http(url, "not a URL"))?;
  let tell = &mut |left| progress(max - left);
  let mut out = Out { file: File::create(to)?, err: None, left: max, tell, panicked: None };
  let mut code: c_long = 0;
  // SAFETY: a handle of its own, its options of their documented types, cleaned up before `out`
  // and the URL go.
  let done = unsafe {
    let h = (c.init)();
    if h.is_null() {
      return Err(super::http(url, "curl_easy_init failed"));
    }

    (c.setopt)(h, URL, c_url.as_ptr());
    (c.setopt)(h, FOLLOWLOCATION, 1 as c_long);
    (c.setopt)(h, MAXREDIRS, 10 as c_long);
    // `CURLPROTO_HTTPS` 2, with `CURLPROTO_HTTP` 1 from loopback.
    let http = crate::feed::loopback(url);
    let names = if http { c"http,https" } else { c"https" };
    if (c.setopt)(h, REDIR_PROTOCOLS_STR, names.as_ptr()) != 0 {
      (c.setopt)(h, REDIR_PROTOCOLS, 2 | http as c_long);
    }

    (c.setopt)(h, NOSIGNAL, 1 as c_long);
    (c.setopt)(h, LOW_SPEED_LIMIT, 1 as c_long);
    (c.setopt)(h, LOW_SPEED_TIME, 60 as c_long);
    (c.setopt)(h, WRITEFUNCTION, write as *const c_void);
    (c.setopt)(h, WRITEDATA, (&raw mut out).cast::<c_void>());
    let done = (c.perform)(h);
    (c.getinfo)(h, RESPONSE_CODE, &raw mut code);
    (c.cleanup)(h);
    done
  };

  if let Some(p) = out.panicked {
    panic::resume_unwind(p);
  }

  // A 404's page may be past `max`: the status tells first.
  if code != 0 {
    super::status(url, code as u32)?;
  }

  let too_big = out.err.as_ref().is_some_and(|e| e.kind() == io::ErrorKind::InvalidData);
  out.err.map_or(Ok(()), |e| Err(if too_big { big() } else { e.into() }))?;

  if done != 0 {
    // SAFETY: curl's static message for its code.
    let why = unsafe { CStr::from_ptr((c.strerror)(done)) };
    return Err(super::http(url, why.to_string_lossy()));
  }

  super::status(url, code as u32)?;
  Ok(out.file.sync_all()?)
}

// MARK: Install

/// Makes the AppImage downloaded as `file` executable: the new app, for any target.
pub fn unpack(file: &Path, _: &Path) -> Result<PathBuf> {
  fs::set_permissions(file, fs::Permissions::from_mode(0o755))?;
  Ok(file.to_owned())
}

/// Starts the AppImage `app` with `args` and `note` in its [NOTE] (none inherited), not waiting
/// for it, without the installer's AppImage variables, which the runtime sets again for its own.
pub fn start(app: &Path, args: &[OsString], note: Option<&str>) -> io::Result<()> {
  let (mut cmd, vars) = (Command::new(app), ["APPIMAGE", "APPDIR", "ARGV0", "OWD", NOTE]);
  let cmd = vars.iter().fold(&mut cmd, |c, k| c.env_remove(k));
  closed(cmd).envs(note.map(|n| (NOTE, n))).args(args).spawn().map(drop)
}

/// Spawns `cmd` as every Unix does ([super::unix::detach]), with nothing inherited past stderr
/// ([closed]).
pub fn detach(cmd: &mut Command) -> io::Result<Child> {
  super::unix::detach(closed(cmd))
}

/// `cmd` started with no descriptor past stderr: an AppImage's runtime leaves its FUSE daemon's
/// pipe open in the app, and the app started by the update would keep the old one mounted.
fn closed(cmd: &mut Command) -> &mut Command {
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

#[cfg(test)]
mod tests {
  use super::*;
  use crate::testing::{dir, write};

  #[test]
  fn an_appimage_is_made_executable() {
    let root = dir("appimage");
    write(&root, &[("app.AppImage", "2")]);
    let new = unpack(&root.join("app.AppImage"), &root).unwrap();
    assert_eq!(fs::metadata(new).unwrap().permissions().mode() & 0o777, 0o755);
    fs::remove_dir_all(root).unwrap();
  }
}
