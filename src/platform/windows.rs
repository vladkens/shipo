// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! The update's calls on Windows: the file fetched by WinHTTP, the installer started apart from
//! the app and waiting for it to end, the release's per-user setup run silently, the installed exe
//! started, the installs its Uninstall keys name.

use std::ffi::{OsString, c_void};
use std::fs::File;
use std::io::{self, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::ptr::{null, null_mut};
use std::time::Duration;
use std::{env, slice};

use windows_sys::Win32::Foundation::{
  ERROR_ACCESS_DENIED, ERROR_DIR_NOT_EMPTY, ERROR_LOCK_VIOLATION, ERROR_SHARING_VIOLATION,
  GetLastError, WAIT_OBJECT_0,
};
use windows_sys::Win32::Networking::WinHttp::{
  URL_COMPONENTS, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE,
  WINHTTP_INTERNET_SCHEME_HTTPS, WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
  WinHttpCloseHandle, WinHttpConnect, WinHttpCrackUrl, WinHttpOpen, WinHttpOpenRequest,
  WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest,
};
use windows_sys::Win32::System::Diagnostics::Debug;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Registry::{
  HKEY_CURRENT_USER, KEY_READ, RRF_RT_REG_SZ, RegCloseKey, RegEnumKeyExW, RegGetValueW,
  RegOpenKeyExW,
};
use windows_sys::Win32::System::Threading::{
  CREATE_BREAKAWAY_FROM_JOB, CREATE_NO_WINDOW, DETACHED_PROCESS, OpenProcess, PROCESS_SYNCHRONIZE,
  WaitForSingleObject,
};
use windows_sys::core::BOOL;

use super::{Big, Progress};
use crate::installer::NOTE;
use crate::uninstall::{UNINSTALL, text};
use crate::{Error, Key, Result};

// MARK: HTTP

/// A WinHTTP handle, closed on drop.
struct Handle(*mut c_void);

impl Drop for Handle {
  fn drop(&mut self) {
    // SAFETY: a handle WinHTTP gave, closed once.
    unsafe { WinHttpCloseHandle(self.0) };
  }
}

/// A handle a WinHTTP call for `url` gave; its error if none.
fn handle(url: &str, h: *mut c_void) -> Result<Handle> {
  if h.is_null() { Err(failed(url)) } else { Ok(Handle(h)) }
}

/// A WinHTTP call for `url` that returns whether it succeeded; its error if not.
fn ok(url: &str, done: BOOL) -> Result<()> {
  if done == 0 { Err(failed(url)) } else { Ok(()) }
}

/// The last WinHTTP call's error, as `url`'s. WinHTTP's own codes (12000-12999) have their
/// messages in winhttp.dll, where std doesn't look: "OS Error 12007 (FormatMessageW() returned
/// error 317)"; any other code isn't there and keeps std's.
fn failed(url: &str) -> Error {
  let flags = Debug::FORMAT_MESSAGE_FROM_HMODULE | Debug::FORMAT_MESSAGE_IGNORE_INSERTS;
  // SAFETY: no arguments; the same code as `e`'s, as a u32.
  let (e, mut buf, code) = (io::Error::last_os_error(), [0u16; 512], unsafe { GetLastError() });
  // SAFETY: a NUL-terminated name; loaded by the call that failed.
  let dll = unsafe { GetModuleHandleW(wide("winhttp.dll").as_ptr()) };
  // SAFETY: a module or null (no message), a buffer of its length.
  let n = unsafe { Debug::FormatMessageW(flags, dll, code, 0, buf.as_mut_ptr(), 512, null()) };
  let s = String::from_utf16_lossy(&buf[..n as usize]).trim_end().to_owned();
  super::http(url, if n == 0 { e.to_string() } else { format!("{s} (os error {code})") })
}

/// `s` as a NUL-terminated UTF-16 string, for the `W` calls.
fn wide(s: &str) -> Vec<u16> {
  s.encode_utf16().chain([0]).collect()
}

/// The body of `url` into `to` by WinHTTP with the system's proxy
/// (`WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY`), which follows redirects but not from https to http;
/// `big` the error past `max` bytes.
pub fn get(url: &str, to: &Path, max: u64, big: Big, progress: Progress) -> Result<()> {
  let w = wide(url);
  let mut parts = URL_COMPONENTS {
    dwStructSize: size_of::<URL_COMPONENTS>() as u32,
    dwHostNameLength: u32::MAX,
    dwUrlPathLength: u32::MAX,
    dwExtraInfoLength: u32::MAX,
    ..Default::default()
  };
  // SAFETY: a NUL-terminated URL; the parts point into it, the query right after the path.
  ok(url, unsafe { WinHttpCrackUrl(w.as_ptr(), 0, 0, &mut parts) })?;
  let part = |p: *const u16, n: u32| {
    // SAFETY: a part WinHttpCrackUrl found in `w`, null when missing.
    let s = if p.is_null() { &[][..] } else { unsafe { slice::from_raw_parts(p, n as usize) } };
    s.iter().copied().chain([0]).collect::<Vec<u16>>()
  };
  let host = part(parts.lpszHostName, parts.dwHostNameLength);
  let path = part(parts.lpszUrlPath, parts.dwUrlPathLength + parts.dwExtraInfoLength);
  let secure = if parts.nScheme == WINHTTP_INTERNET_SCHEME_HTTPS { WINHTTP_FLAG_SECURE } else { 0 };

  let (agent, proxy) = (wide("shipo"), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY);
  let (mut code, mut n) = (0u32, 4u32);
  // SAFETY: NUL-terminated strings, null for the defaults (the verb GET); a handle dropped after
  // its children (locals drop in reverse); the status into a u32 of its length.
  unsafe {
    let session = handle(url, WinHttpOpen(agent.as_ptr(), proxy, null(), null(), 0))?;
    let connect = handle(url, WinHttpConnect(session.0, host.as_ptr(), parts.nPort, 0))?;
    let req = WinHttpOpenRequest(connect.0, null(), path.as_ptr(), null(), null(), null(), secure);
    let req = handle(url, req)?;
    ok(url, WinHttpSendRequest(req.0, null(), 0, null(), 0, 0, 0))?;
    ok(url, WinHttpReceiveResponse(req.0, null_mut()))?;
    let query = WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER;
    ok(url, WinHttpQueryHeaders(req.0, query, null(), (&raw mut code).cast(), &mut n, null_mut()))?;
    super::status(url, code)?;
    read(&req, to, url, max, big, progress)
  }
}

/// The body of the request `req` for `url` into a new file at `to`, up to `max` bytes (`big`'s
/// error past them), `tell`ing the bytes so far.
fn read(req: &Handle, to: &Path, url: &str, max: u64, big: Big, tell: Progress) -> Result<()> {
  let (mut file, mut buf, mut left) = (File::create(to)?, vec![0u8; 1 << 16], max);
  loop {
    let mut n = 0;
    // SAFETY: a buffer of its length.
    ok(url, unsafe { WinHttpReadData(req.0, buf.as_mut_ptr().cast(), buf.len() as u32, &mut n) })?;
    if n == 0 {
      return Ok(file.sync_all()?);
    }

    left = left.checked_sub(n as u64).ok_or_else(big)?;
    file.write_all(&buf[..n as usize])?;
    tell(max - left);
  }
}

// MARK: Install

/// The setup downloaded as `file`, run as it came: the new app, for any target.
pub fn unpack(file: &Path, _: &Path) -> Result<PathBuf> {
  Ok(file.to_owned())
}

/// Runs the per-user setup `exe` silently (`/S`) into the folder `to`, the running app's, with no
/// window, to its end; its exit code (0 installed, 2 not). `/D=` goes last and unquoted, its spaces
/// kept, as the setup reads it.
pub fn setup(exe: &Path, to: &Path) -> io::Result<i32> {
  let mut cmd = Command::new(exe);
  cmd.arg("/S").raw_arg(format!("/D={}", to.display())).creation_flags(CREATE_NO_WINDOW);
  Ok(cmd.status()?.code().unwrap_or(-1))
}

/// The `{…}` keys under HKCU's Uninstall, as the setup names the app's (v0.1's NSIS one too): the
/// library doesn't know the app's id, so the exe and its folder find its key.
pub fn installs() -> Vec<Key> {
  let path = wide(UNINSTALL);
  let (mut root, mut found, mut buf) = (null_mut(), vec![], vec![0u16; 1 << 15]);
  // SAFETY: a NUL-terminated path; the key closed below.
  if unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, path.as_ptr(), 0, KEY_READ, &mut root) } != 0 {
    return found;
  }

  for i in 0.. {
    let (mut key, mut n) = ([0u16; 256], 256u32);
    let at = key.as_mut_ptr();
    // SAFETY: a buffer of its length in chars, the longest a key's name is; its NUL kept.
    let next =
      unsafe { RegEnumKeyExW(root, i, at, &mut n, null(), null_mut(), null_mut(), null_mut()) };
    if next != 0 {
      break;
    }

    let name = String::from_utf16_lossy(&key[..n as usize]);
    if !name.starts_with('{') || !name.ends_with('}') {
      continue;
    }

    // The value's text, up to the longest path Windows takes.
    let mut get = |value: &str| {
      let (value, data, mut len) = (wide(value), buf.as_mut_ptr().cast(), 1u32 << 16);
      // SAFETY: NUL-terminated names, a buffer of its length in bytes.
      let got = unsafe {
        RegGetValueW(root, key.as_ptr(), value.as_ptr(), RRF_RT_REG_SZ, null_mut(), data, &mut len)
      };
      (got == 0).then(|| text(&buf[..len as usize / 2])).flatten()
    };
    let (location, icon, uninstall) =
      (get("InstallLocation"), get("DisplayIcon"), get("UninstallString"));
    if let (Some(location), Some(icon), Some(uninstall)) = (location, icon, uninstall) {
      found.push(Key { location, icon, uninstall });
    }
  }

  // SAFETY: the key opened above.
  unsafe { RegCloseKey(root) };
  found
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
  let child = cmd.creation_flags(flags | CREATE_BREAKAWAY_FROM_JOB).spawn();
  child.or_else(|_| cmd.creation_flags(flags).spawn())
}

/// Starts the app's folder `app` by its exe, named as this one's, with `args` and `note` in its
/// [NOTE] (none inherited), not waiting for it. The installer has no console to share, so a console
/// app would get a window of its own, in Windows Terminal or conhost: it gets a console with none
/// (a windowed app is unaffected).
pub fn start(app: &Path, args: &[OsString], note: Option<&str>) -> io::Result<()> {
  let mut cmd = Command::new(app.join(env::current_exe()?.file_name().unwrap_or_default()));
  cmd.env_remove(NOTE).envs(note.map(|n| (NOTE, n))).args(args).creation_flags(CREATE_NO_WINDOW);
  cmd.spawn().map(drop)
}

/// A file another process has open (a sharing or lock violation) or is deleting (access denied),
/// a folder not empty yet: the app that just quit may hold its files a moment after it ended.
pub fn busy(e: &io::Error) -> bool {
  let c = [ERROR_SHARING_VIOLATION, ERROR_LOCK_VIOLATION, ERROR_ACCESS_DENIED, ERROR_DIR_NOT_EMPTY];
  e.raw_os_error().is_some_and(|x| c.contains(&(x as u32)))
}

#[cfg(test)]
mod tests {
  use windows_sys::Win32::Foundation::SetLastError;

  use super::*;

  #[test]
  fn winhttp_message() {
    // SAFETY: a null handle fails harmlessly, loading winhttp.dll as the app's own calls do; then
    // the last error a failed name lookup leaves (ERROR_WINHTTP_NAME_NOT_RESOLVED).
    unsafe {
      WinHttpCloseHandle(null_mut());
      SetLastError(12007);
    }
    let e = failed("https://example.invalid").to_string();
    assert!(e.contains("name") || e.contains("resolved"), "{e}");
  }
}
