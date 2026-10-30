// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! The app's Uninstall key on Windows as the setup writes it and the library finds the app by it:
//! where it is, the folders it may name and how two of them compare. The setup's stub builds this
//! file too, so the two can't differ.

use std::fs;
use std::path::{Path, PathBuf};

/// The parent of the app's Uninstall key in HKCU, `{<AppId>}`, where an install before this one,
/// an NSIS setup's too, left `InstallLocation`.
#[cfg_attr(not(windows), allow(dead_code))]
pub const UNINSTALL: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall";
/// The uninstaller's name in the install folder, as NSIS setups name theirs, so an install over
/// one of those replaces it.
pub const UNINSTALLER: &str = "Uninstall.exe";

/// The key's `UninstallString` for the app in `dir`, as the setup and v0.1's NSIS one write it.
pub fn command(dir: &str) -> String {
  format!(r#""{dir}\{UNINSTALLER}""#)
}

/// `dir` without a trailing separator if it is an absolute folder below a drive's root (`C:\…`,
/// `\\?\C:\…`) or a share's (`\\server\share\…`, `\\?\UNC\server\share\…`), never the root
/// itself, which an uninstall would empty: no part of it empty, `.`, `..`, or dots and spaces
/// alone, which Windows drops or climbs by. Its `/` made `\`, so the key holds it as Windows does.
pub fn folder(dir: &str) -> Option<PathBuf> {
  let dir = dir.replace('/', "\\");
  let dir = dir.trim_end_matches('\\');
  let prefixed = |p: &str| dir.get(..p.len()).is_some_and(|h| h.eq_ignore_ascii_case(p));
  let (parts, least) = match [r"\\?\UNC\", r"\\.\UNC\"].into_iter().find(|p| prefixed(p)) {
    Some(p) => (&dir[p.len()..], 3),
    None if prefixed(r"\\?\") || prefixed(r"\\.\") => (drive(&dir[4..])?, 1),
    None => match dir.strip_prefix(r"\\") {
      Some(share) => (share, 3),
      None => (drive(dir)?, 1),
    },
  };

  let parts: Vec<_> = parts.split('\\').collect();
  let named = |p: &&str| !p.trim_matches(['.', ' ']).is_empty();
  (parts.len() >= least && parts.iter().all(named)).then(|| dir.into())
}

/// The folders of `path` after its drive, `C:\`.
fn drive(path: &str) -> Option<&str> {
  let b = path.as_bytes();
  let drive =
    b.len() > 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'\\' | b'/');
  drive.then(|| &path[3..])
}

/// Whether the folder `l`, as the Uninstall key holds it, is `dir` by their text: case, `/`, a
/// trailing separator and `\\?\` aside. Never touches the disk.
pub fn alike(l: &str, dir: &Path) -> bool {
  let plain = |s: &str| {
    let s = s.replace('/', "\\");
    let s = s.trim_end_matches('\\');
    let prefixed = |p: &str| s.get(..p.len()).is_some_and(|h| h.eq_ignore_ascii_case(p));
    if prefixed(r"\\?\UNC\") || prefixed(r"\\.\UNC\") {
      format!(r"\\{}", &s[8..])
    } else if prefixed(r"\\?\") || prefixed(r"\\.\") {
      s[4..].to_string()
    } else {
      s.to_string()
    }
  };

  plain(l).eq_ignore_ascii_case(&plain(&dir.to_string_lossy()))
}

/// Whether the folder `l`, as the Uninstall key holds it, is `dir`: [alike], else as the file
/// system resolves them (a mapped drive, 8.3), which may wait on a share that doesn't answer.
pub fn same(l: &str, dir: &Path) -> bool {
  // Only an absolute folder resolves: a relative one would by the current folder.
  let real = |p: &Path| fs::canonicalize(p).ok();
  alike(l, dir) || folder(l).and_then(|l| real(&l)).is_some_and(|l| real(dir) == Some(l))
}

/// A registry string's text: up to its first NUL, as a value may hold more after it.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn text(units: &[u16]) -> Option<String> {
  let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
  String::from_utf16(&units[..end]).ok()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn folders() {
    let oks = [
      (r"C:\Apps\", r"C:\Apps"),
      (r"C:\Apps\Hello\\", r"C:\Apps\Hello"),
      (r"\\?\C:\Apps", r"\\?\C:\Apps"),
      (r"\\?\UNC\server\share\Apps\", r"\\?\UNC\server\share\Apps"),
      (r"\\.\unc\server\share\Apps", r"\\.\unc\server\share\Apps"),
      (r"C:\.Apps\a.b", r"C:\.Apps\a.b"),
      ("C:/Apps/Hello/", r"C:\Apps\Hello"),
    ];
    for (dir, want) in oks {
      assert_eq!(folder(dir), Some(PathBuf::from(want)), "{dir}");
    }

    // Roots by another name, and parts Windows drops or climbs by.
    let roots = [r"\\?\UNC\server\share", r"\\.\UNC\server\share", r"\\?\C:\", r"\\?\UNC\"];
    let dots = [r"C:\.", r"C:\..", r"C:\Apps\..", r"C:\ .", r"C:\Apps\. \x", r"\\server\share\."];
    let other = [r"\\?\Volume{x}\Apps", r"\\.\pipe\x", r"\\?\Apps", r"C:\Apps\\x", r"\\server\\x"];
    for dir in roots.into_iter().chain(dots).chain(other) {
      assert_eq!(folder(dir), None, "{dir}");
    }
  }

  #[test]
  fn same_folders() {
    let dir = Path::new(r"C:\Apps\Hello");
    assert!(same(r"C:\Apps\Hello", dir) && same(r"c:\apps\hello\", dir));
    assert!(same("C:/Apps/Hello/", dir) && same(r"\\?\C:\Apps\Hello", dir));
    assert!(same(r"\\?\UNC\srv\share\x", Path::new(r"\\srv\share\x")));
    assert!(same(r"\\.\C:\Apps\Hello", dir));
    assert!(same(r"\\.\UNC\srv\share\x", Path::new(r"\\srv\share\x")));
    // A relative folder never matches by the current folder.
    let here = std::env::current_dir().unwrap();
    assert!(!same(".", &here) && !same("", &here));
    assert!(!same(r"C:\Users\Me\AppData\Local\Temp", dir) && !same(r"C:\Apps", dir));
    assert!(!alike(r"C:\Apps\Hello\sub", dir) && !alike("Hello", dir));
  }

  #[test]
  fn text_up_to_its_nul() {
    let w = |s: &str| s.encode_utf16().collect::<Vec<_>>();
    assert_eq!(text(&w("C:\\Apps\0")).as_deref(), Some(r"C:\Apps"));
    assert_eq!(text(&w("C:\\Apps\0\0junk")).as_deref(), Some(r"C:\Apps"), "the rest dropped");
    assert_eq!(text(&w("C:\\Apps")).as_deref(), Some(r"C:\Apps"), "none stored");
    assert_eq!(text(&w("\0")).as_deref(), Some(""));
    assert_eq!(text(&[0xD800, 0]), None, "not UTF-16");
  }
}
