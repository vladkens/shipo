// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! The setup's run on Windows: the arch checked, the app installed or uninstalled, the Uninstall
//! key and the launch-at-login values in the registry, the Start menu shortcut by `IShellLinkW`,
//! a message box for an error unless `/S`.

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::ptr::{null, null_mut};
use std::{env, fs, io};

use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, HANDLE};
use windows_sys::Win32::System::Com::{
  CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
};
use windows_sys::Win32::System::Registry::{
  HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ,
  RRF_RT_REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteKeyValueW, RegDeleteTreeW, RegGetValueW,
  RegSetValueExW,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, IsWow64Process2};
use windows_sys::Win32::UI::Shell::{FOLDERID_Programs, SHGetKnownFolderPath, ShellLink};
use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
use windows_sys::core::{GUID, HRESULT, PWSTR};

use super::payload::{self, Info};
use super::{Args, UNINSTALLER, Value, own, placeholder, put, remove, uninstaller, values};

const UNINSTALL: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall";
const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const APPROVED: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";

/// Runs the setup: 0 when it installed or uninstalled, 2 when not, with a message unless `/S`.
pub fn main() -> ExitCode {
  let args = Args::parse(env::args_os().skip(1));
  let mut title = "Setup".to_string();
  let Err(e) = run(&args, &mut title) else { return ExitCode::SUCCESS };
  if !args.silent {
    let (text, title) = (wide(&e.to_string()), wide(&title));
    // SAFETY: NUL-terminated strings, no owner window.
    unsafe { MessageBoxW(null_mut(), text.as_ptr(), title.as_ptr(), MB_ICONERROR | MB_OK) };
  }

  ExitCode::from(2)
}

/// The payload of this exe installed, or uninstalled when it holds no files; `title` the
/// message's, once the app's name is known.
fn run(args: &Args, title: &mut String) -> io::Result<()> {
  let path = env::current_exe()?;
  let exe = fs::read(&path)?;
  let fields = payload::fields(&placeholder());
  let (info, mut files, stub) = own(&exe, fields)?;
  let uninstall = files.is_empty();
  *title = format!("{} {}", info.name, if uninstall { "Uninstall" } else { "Setup" });
  if uninstall {
    let dir = path.parent().ok_or_else(|| io::Error::other("the uninstaller is in no folder"))?;
    return uninstall_from(dir, &path, &info);
  }

  let arch = if cfg!(target_arch = "aarch64") { "arm64" } else { "x64" };
  if !fits() {
    return Err(io::Error::other(format!("This setup of {} is for {arch} Windows.", info.name)));
  }

  // An update, `/S` with no `/D=`, goes where the app is.
  let key = format!(r"{UNINSTALL}\{}", info.app_id);
  let dir = match (&args.dir, text(&key, "InstallLocation")) {
    (Some(dir), _) => dir.clone(),
    (None, Some(dir)) if !dir.is_empty() => dir.into(),
    (None, _) => {
      let local = env::var_os("LOCALAPPDATA").ok_or_else(|| io::Error::other("no LOCALAPPDATA"))?;
      Path::new(&local).join("Programs").join(&info.name)
    }
  };

  files.push((UNINSTALLER.into(), uninstaller(stub, fields[0], &info)?));
  let size = put(&dir, &files)?;
  let exe = dir.join(format!("{}.exe", info.name));
  shortcut(&programs()?.join(format!("{}.lnk", info.name)), &exe, &dir, &info.comment)?;
  set(&key, &values(&info, &dir, size.div_ceil(1024)))?;
  if !args.silent {
    Command::new(&exe).current_dir(&dir).spawn()?;
  }

  Ok(())
}

/// Removes the app of `info` from `dir`, the uninstaller at `path` moved out first, where it can be
/// while it runs, so the folder can go: to the temp folder, or the install folder's parent when
/// that is on another volume; then its shortcut, its launch at login and its key. The app's data
/// stays.
fn uninstall_from(dir: &Path, path: &Path, info: &Info) -> io::Result<()> {
  let name = format!("{}-uninstall-{}.exe", info.name, std::process::id());
  let away = [env::temp_dir(), dir.parent().unwrap_or(dir).to_path_buf()];
  // Never over a file there: std's rename on Windows replaces one.
  let moved = |d: &PathBuf| !d.join(&name).exists() && fs::rename(path, d.join(&name)).is_ok();
  remove(dir, &info.name, || away.iter().any(moved))?;
  match fs::remove_file(programs()?.join(format!("{}.lnk", info.name))) {
    Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
    _ => {}
  }

  for key in [RUN, APPROVED] {
    let (key, id) = (wide(key), wide(&info.id));
    // SAFETY: NUL-terminated strings.
    gone(unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), id.as_ptr()) })?;
  }

  let key = wide(&format!(r"{UNINSTALL}\{}", info.app_id));
  // SAFETY: a NUL-terminated string.
  gone(unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, key.as_ptr()) })
}

/// Whether this exe's arch runs natively, or an x64 one on arm64, which emulates it.
fn fits() -> bool {
  const AMD64: u16 = 0x8664;
  const ARM64: u16 = 0xAA64;
  let (mut process, mut native) = (0, 0);
  // SAFETY: the current process's pseudo handle, two u16 to fill.
  if unsafe { IsWow64Process2(GetCurrentProcess(), &mut process, &mut native) } == 0 {
    return true;
  }

  match cfg!(target_arch = "aarch64") {
    true => native == ARM64,
    false => native == AMD64 || native == ARM64,
  }
}

// MARK: Registry

/// The key `key` under HKCU made if missing and its `values` set.
fn set(key: &str, values: &[(&str, Value)]) -> io::Result<()> {
  let mut h: HKEY = null_mut();
  let path = wide(key);
  // SAFETY: a NUL-terminated name; the handle into `h`, closed below.
  ok(unsafe {
    let opts = REG_OPTION_NON_VOLATILE;
    RegCreateKeyExW(
      HKEY_CURRENT_USER,
      path.as_ptr(),
      0,
      null(),
      opts,
      KEY_SET_VALUE,
      null(),
      &mut h,
      null_mut(),
    )
  })?;

  let done = values.iter().try_for_each(|(name, value)| {
    let (kind, data) = match value {
      Value::Text(s) => (REG_SZ, wide(s).iter().flat_map(|u| u.to_le_bytes()).collect()),
      Value::Number(n) => (REG_DWORD, n.to_le_bytes().to_vec()),
    };

    let name = wide(name);
    // SAFETY: an open key, a NUL-terminated name, the data of its length.
    ok(unsafe { RegSetValueExW(h, name.as_ptr(), 0, kind, data.as_ptr(), data.len() as u32) })
  });

  // SAFETY: the key opened above.
  unsafe { RegCloseKey(h) };
  done
}

/// The string `name` of the key `key` under HKCU, if it has one.
fn text(key: &str, name: &str) -> Option<String> {
  let (key, name) = (wide(key), wide(name));
  let mut len = 0u32;
  // SAFETY: NUL-terminated strings; the size first, then the data into a buffer of it.
  unsafe {
    let get = |data: *mut c_void, len: &mut u32| {
      RegGetValueW(
        HKEY_CURRENT_USER,
        key.as_ptr(),
        name.as_ptr(),
        RRF_RT_REG_SZ,
        null_mut(),
        data,
        len,
      )
    };

    ok(get(null_mut(), &mut len)).ok()?;
    let mut buf = vec![0u16; (len as usize).div_ceil(2)];
    ok(get(buf.as_mut_ptr().cast(), &mut len)).ok()?;
    let end = buf.iter().position(|&u| u == 0).unwrap_or(buf.len());
    String::from_utf16(&buf[..end]).ok()
  }
}

fn ok(code: u32) -> io::Result<()> {
  match code {
    0 => Ok(()),
    code => Err(io::Error::from_raw_os_error(code as i32)),
  }
}

/// [ok], a value or key that is not there being gone already.
fn gone(code: u32) -> io::Result<()> {
  if code == ERROR_FILE_NOT_FOUND { Ok(()) } else { ok(code) }
}

// MARK: Shortcut

/// The current user's Start menu programs folder.
fn programs() -> io::Result<PathBuf> {
  let mut path: PWSTR = null_mut();
  // SAFETY: the path the shell allocates, freed once read.
  unsafe {
    hr(SHGetKnownFolderPath(&FOLDERID_Programs, 0, 0 as HANDLE, &mut path))?;
    let len = (0..).take_while(|&i| *path.add(i) != 0).count();
    let dir = String::from_utf16_lossy(std::slice::from_raw_parts(path, len));
    CoTaskMemFree(path as *const c_void);
    Ok(dir.into())
  }
}

const IID_SHELL_LINK: GUID = GUID::from_u128(0x000214F9_0000_0000_C000_000000000046);
const IID_PERSIST_FILE: GUID = GUID::from_u128(0x0000010B_0000_0000_C000_000000000046);

/// The shortcut `lnk` to `target`, started in `dir`, its icon the target's, `comment` its tip;
/// by the shell's own `IShellLinkW` and `IPersistFile`, called through their vtables.
fn shortcut(lnk: &Path, target: &Path, dir: &Path, comment: &str) -> io::Result<()> {
  type Unary = unsafe extern "system" fn(*mut c_void) -> u32;
  type Text = unsafe extern "system" fn(*mut c_void, *const u16) -> HRESULT;
  type Icon = unsafe extern "system" fn(*mut c_void, *const u16, i32) -> HRESULT;
  type Query = unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT;
  type Save = unsafe extern "system" fn(*mut c_void, *const u16, i32) -> HRESULT;

  /// The method at `index` of the COM object `obj`'s vtable, as `F`.
  unsafe fn method<F: Copy>(obj: *mut c_void, index: usize) -> F {
    // SAFETY: a live object, whose first field is its vtable; F is the method's type.
    unsafe { std::mem::transmute_copy(&*(*(obj as *const *const usize)).add(index)) }
  }

  let (target, dir, comment, lnk) =
    (wide_path(target), wide_path(dir), wide(comment), wide_path(lnk));
  let (mut link, mut file) = (null_mut(), null_mut());
  // SAFETY: COM on this thread; IShellLinkW's methods 7 SetDescription, 9 SetWorkingDirectory, 17
  // SetIconLocation, 20 SetPath, IUnknown's 0 QueryInterface and 2 Release, IPersistFile's 6
  // Save; each object released once.
  unsafe {
    CoInitializeEx(null(), COINIT_APARTMENTTHREADED as u32);
    hr(CoCreateInstance(&ShellLink, null_mut(), CLSCTX_INPROC_SERVER, &IID_SHELL_LINK, &mut link))?;
    let done = (|| {
      hr(method::<Text>(link, 20)(link, target.as_ptr()))?;
      hr(method::<Text>(link, 9)(link, dir.as_ptr()))?;
      hr(method::<Text>(link, 7)(link, comment.as_ptr()))?;
      hr(method::<Icon>(link, 17)(link, target.as_ptr(), 0))?;
      hr(method::<Query>(link, 0)(link, &IID_PERSIST_FILE, &mut file))?;
      let saved = hr(method::<Save>(file, 6)(file, lnk.as_ptr(), 1));
      method::<Unary>(file, 2)(file);
      saved
    })();

    method::<Unary>(link, 2)(link);
    done
  }
}

fn hr(code: HRESULT) -> io::Result<()> {
  match code {
    0.. => Ok(()),
    code => Err(io::Error::from_raw_os_error(code)),
  }
}

fn wide(s: &str) -> Vec<u16> {
  s.encode_utf16().chain([0]).collect()
}

fn wide_path(path: &Path) -> Vec<u16> {
  use std::os::windows::ffi::OsStrExt;
  path.as_os_str().encode_wide().chain([0]).collect()
}
