// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! The update's calls on Windows: the file fetched by WinHTTP, the installer started apart from
//! the app and waiting for it to end, the release's per-user setup run silently, the installed exe
//! started.

use std::env;
use std::ffi::{OsString, c_void};
use std::fs::File;
use std::io::{self, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::ptr::{null, null_mut};
use std::time::Duration;

use windows_sys::Win32::Foundation::{
  ERROR_ACCESS_DENIED, ERROR_DIR_NOT_EMPTY, ERROR_LOCK_VIOLATION, ERROR_SHARING_VIOLATION,
  WAIT_OBJECT_0,
};
use windows_sys::Win32::Networking::WinHttp::{
  URL_COMPONENTS, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE,
  WINHTTP_INTERNET_SCHEME_HTTPS, WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
  WinHttpCloseHandle, WinHttpConnect, WinHttpCrackUrl, WinHttpOpen, WinHttpOpenRequest,
  WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest,
};
use windows_sys::Win32::System::Threading::{
  CREATE_BREAKAWAY_FROM_JOB, CREATE_NO_WINDOW, DETACHED_PROCESS, OpenProcess, PROCESS_SYNCHRONIZE,
  WaitForSingleObject,
};
use windows_sys::core::BOOL;

use crate::installer::NOTE;

// MARK: HTTP

/// A WinHTTP handle, closed on drop.
struct Handle(*mut c_void);

impl Drop for Handle {
  fn drop(&mut self) {
    // SAFETY: a handle WinHTTP gave, closed once.
    unsafe { WinHttpCloseHandle(self.0) };
  }
}

fn handle(h: *mut c_void) -> io::Result<Handle> {
  if h.is_null() { Err(io::Error::last_os_error()) } else { Ok(Handle(h)) }
}

fn ok(done: BOOL) -> io::Result<()> {
  if done == 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
}

/// `s` as a NUL-terminated UTF-16 string, for the `W` calls.
fn wide(s: &str) -> Vec<u16> {
  s.encode_utf16().chain([0]).collect()
}

/// The body of `url` into `to` by WinHTTP with the system's proxy
/// (`WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY`), which follows redirects but not from https to http.
pub fn get(url: &str, to: &Path, max: u64, progress: &mut dyn FnMut(u64)) -> io::Result<()> {
  let w = wide(url);
  let mut parts = URL_COMPONENTS {
    dwStructSize: size_of::<URL_COMPONENTS>() as u32,
    dwHostNameLength: u32::MAX,
    dwUrlPathLength: u32::MAX,
    dwExtraInfoLength: u32::MAX,
    ..Default::default()
  };
  // SAFETY: a NUL-terminated URL; the parts point into it, the query right after the path.
  ok(unsafe { WinHttpCrackUrl(w.as_ptr(), 0, 0, &mut parts) })?;
  let part = |p: *const u16, n: u32| {
    // SAFETY: a part WinHttpCrackUrl found in `w`, null when missing.
    let s =
      if p.is_null() { &[][..] } else { unsafe { std::slice::from_raw_parts(p, n as usize) } };
    s.iter().copied().chain([0]).collect::<Vec<u16>>()
  };
  let host = part(parts.lpszHostName, parts.dwHostNameLength);
  let path = part(parts.lpszUrlPath, parts.dwUrlPathLength + parts.dwExtraInfoLength);
  let secure = if parts.nScheme == WINHTTP_INTERNET_SCHEME_HTTPS { WINHTTP_FLAG_SECURE } else { 0 };

  let (agent, verb, proxy) = (wide("shipo"), wide("GET"), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY);
  let (mut code, mut len) = (0u32, 4u32);
  // SAFETY: NUL-terminated strings, null for the defaults; a handle dropped after its children
  // (locals drop in reverse); the status into a u32 of its length.
  unsafe {
    let session = handle(WinHttpOpen(agent.as_ptr(), proxy, null(), null(), 0))?;
    let connect = handle(WinHttpConnect(session.0, host.as_ptr(), parts.nPort, 0))?;
    let req =
      WinHttpOpenRequest(connect.0, verb.as_ptr(), path.as_ptr(), null(), null(), null(), secure);
    let req = handle(req)?;
    ok(WinHttpSendRequest(req.0, null(), 0, null(), 0, 0, 0))?;
    ok(WinHttpReceiveResponse(req.0, null_mut()))?;
    let query = WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER;
    ok(WinHttpQueryHeaders(req.0, query, null(), (&raw mut code).cast(), &mut len, null_mut()))?;
    super::status(url, code)?;
    read(&req, to, url, max, progress)
  }
}

/// The body of the request `req` for `url` into a new file at `to`, up to `max` bytes, `tell`ing
/// the bytes so far.
fn read(req: &Handle, to: &Path, url: &str, max: u64, tell: &mut dyn FnMut(u64)) -> io::Result<()> {
  let (mut file, mut buf, mut left) = (File::create(to)?, vec![0u8; 1 << 16], max);
  loop {
    let mut n = 0;
    // SAFETY: a buffer of its length.
    ok(unsafe { WinHttpReadData(req.0, buf.as_mut_ptr().cast(), buf.len() as u32, &mut n) })?;
    if n == 0 {
      return file.sync_all();
    }

    left = left.checked_sub(n as u64).ok_or_else(|| super::too_big(url, max))?;
    file.write_all(&buf[..n as usize])?;
    tell(max - left);
  }
}

// MARK: Install

/// The setup downloaded as `file`, run as it came: the new app, for any target.
pub fn unpack(file: &Path, _: &Path) -> io::Result<PathBuf> {
  Ok(file.to_owned())
}

/// Runs the per-user NSIS `exe` silently (`/S`, into `%LOCALAPPDATA%\Programs\<name>`) with
/// no window, to its end; its exit code (NSIS': 0 installed, 1 cancelled, 2 aborted by the script).
pub fn setup(exe: &Path) -> io::Result<i32> {
  let done = Command::new(exe).arg("/S").creation_flags(CREATE_NO_WINDOW).status()?;
  Ok(done.code().unwrap_or(-1))
}

/// Whether the process `pid` runs.
pub fn alive(pid: u32) -> bool {
  !wait_exit(pid)(Duration::ZERO)
}

/// The wait for the process `pid` to end, its handle opened now so that a pid reused later can't
/// fool it: whether it ended within the time, its handle signalled, or there was none to open
/// (gone, or another user's).
pub fn wait_exit(pid: u32) -> impl Fn(Duration) -> bool {
  // SAFETY: a handle of ours, closed on drop; a pid that is gone gives none.
  let process = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
  let process = (!process.is_null()).then(|| unsafe { OwnedHandle::from_raw_handle(process) });
  move |cap| {
    let ms = u32::try_from(cap.as_millis()).unwrap_or(u32::MAX - 1);
    // SAFETY: the handle is open.
    let ended = |p: &OwnedHandle| unsafe { WaitForSingleObject(p.as_raw_handle(), ms) };
    process.as_ref().is_none_or(|p| ended(p) == WAIT_OBJECT_0)
  }
}

/// Spawns `cmd` with no console, out of the app's job where the job lets it, so the app's end
/// doesn't end it too.
pub fn detach(cmd: &mut Command) -> io::Result<Child> {
  let flags = DETACHED_PROCESS | CREATE_NO_WINDOW;
  match cmd.creation_flags(flags | CREATE_BREAKAWAY_FROM_JOB).spawn() {
    Err(_) => cmd.creation_flags(flags).spawn(),
    child => child,
  }
}

/// Starts the app's folder `app` by its exe, named as this one's, with `args` and `note` in its
/// [NOTE] (none inherited), not waiting for it. The installer has no console to share, so a console
/// app would get a window of its own, in Windows Terminal or conhost: it gets a console with none
/// (a windowed app is unaffected).
pub fn start(app: &Path, args: &[OsString], note: Option<&str>) -> io::Result<()> {
  let exe = app.join(env::current_exe()?.file_name().unwrap_or_default());
  let mut cmd = Command::new(exe);
  cmd.env_remove(NOTE).envs(note.map(|n| (NOTE, n))).args(args).creation_flags(CREATE_NO_WINDOW);
  cmd.spawn().map(drop)
}

/// A file another process has open (a sharing or lock violation) or is deleting (access denied),
/// a folder not empty yet: the app that just quit may hold its files a moment after it ended.
pub fn busy(e: &io::Error) -> bool {
  let codes =
    [ERROR_SHARING_VIOLATION, ERROR_LOCK_VIOLATION, ERROR_ACCESS_DENIED, ERROR_DIR_NOT_EMPTY];
  e.raw_os_error().is_some_and(|c| codes.contains(&(c as u32)))
}
