// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! The Windows setup: the app's exe given its icon, version and manifest, and the per-user NSIS
//! setup of it that `makensis` builds, as spec.md's Windows section gives it.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::{fs, io};

use toml::Value;

use crate::bad;
use crate::config::Config;
use crate::image::{ICO, Image, group_icon, ico, icons};
use crate::pe::{self, LANG, RT_GROUP_ICON, RT_ICON, RT_MANIFEST, RT_VERSION, Tree};
use crate::tool::{Temp, run};

/// The archs a setup is made for: Rust's name, Windows' in the file name, and the exe's machine.
const ARCHS: [(&str, &str, u16); 2] =
  [("x86_64", "x64", pe::AMD64), ("aarch64", "arm64", pe::ARM64)];
const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const APPROVED: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
const UNINSTALL: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall";

/// The exe's manifest: asInvoker, Common Controls 6, and Windows 10 and 11, which share one
/// supportedOS id. The app sets its DPI awareness itself.
const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0"
        processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/>
    </dependentAssembly>
  </dependency>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
    </application>
  </compatibility>
</assembly>
"#;

/// `<Name>-dev-windows-<x64|arm64>-setup.exe` in `out_dir`: the per-user setup of `binary`, the
/// app's exe for `arch` (`x86_64` or `aarch64`), built by `makensis`, which apt's nsis installs.
pub fn setup(cfg: &Config, binary: &Path, arch: &str, out_dir: &Path) -> io::Result<PathBuf> {
  build(cfg, binary, arch, out_dir, OsStr::new("makensis"))
}

/// [setup] by the program `makensis`.
fn build(
  cfg: &Config,
  binary: &Path,
  arch: &str,
  out_dir: &Path,
  makensis: &OsStr,
) -> io::Result<PathBuf> {
  let &(_, win, machine) = of(arch)?;
  file_name(&cfg.name)?;

  let at = |e: io::Error| io::Error::new(e.kind(), format!("{}: {e}", binary.display()));
  let exe = fs::read(binary).map_err(at)?;
  let pngs = icons(&Image::read(&cfg.icon)?, &ICO, true)?;
  let pngs = pngs.iter().map(|(&n, image)| Ok((n, image.encode()?)));
  let pngs = pngs.collect::<io::Result<BTreeMap<_, _>>>()?;
  let exe = pe::with_resources(&exe, machine, resources(cfg, &pngs)?).map_err(at)?;

  // The files by fixed names beside the script, where makensis reads them: no path in it.
  let tmp = Temp::new()?;
  fs::write(tmp.join("app.exe"), &exe)?;
  fs::write(tmp.join("app.ico"), ico(&pngs)?)?;
  let license = cfg.root.join("LICENSE");
  let licensed = license.is_file();
  // Settings › Apps' size: the installed files', not the icon's.
  let mut size = exe.len() as u64;
  if licensed {
    size += fs::copy(&license, tmp.join("LICENSE.txt"))?;
  }

  let script = nsi(cfg, arch, licensed, size.div_ceil(1024))?;
  fs::write(tmp.join("setup.nsi"), script)?;
  // makensis prints its warnings to stdout, which `run` drops: `-O` logs them with its errors.
  let log = tmp.join("makensis.log");
  let mut opt = OsString::from("-O");
  opt.push(&log);
  let mut cmd = Command::new(makensis);
  cmd.args(["-V2", "-WX", "-INPUTCHARSET", "UTF8"]).arg(opt).arg(tmp.join("setup.nsi"));
  if let Err(e) = run(&mut cmd) {
    if e.kind() == io::ErrorKind::NotFound {
      let tool = makensis.to_string_lossy();
      let msg = format!("{tool} not found: the setup needs it, `apt-get install nsis`");
      return Err(io::Error::new(e.kind(), msg));
    }

    let said = fs::read_to_string(&log).unwrap_or_default();
    let said = said.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with("Note: "));
    let said: Vec<_> = said.collect();
    return Err(match said.is_empty() {
      true => e,
      false => io::Error::other(format!("{e}: {}", said.join("\n"))),
    });
  }

  fs::create_dir_all(out_dir)?;
  let out = out_dir.join(format!("{}-dev-windows-{win}-setup.exe", cfg.name));
  fs::copy(tmp.join("out.exe"), &out)?;
  Ok(out)
}

/// [ARCHS]' entry of `arch`.
fn of(arch: &str) -> io::Result<&'static (&'static str, &'static str, u16)> {
  let found = ARCHS.iter().find(|(name, ..)| *name == arch);
  found.ok_or_else(|| bad(format!("no Windows setup for {arch}, only for x86_64 and aarch64")))
}

/// Checks that `name`, the app's, makes a Windows file name: the exe's, its folder's, the
/// shortcut's and the setup's.
fn file_name(name: &str) -> io::Result<()> {
  let stem = name.split('.').next().unwrap_or_default().to_ascii_uppercase();
  let numbered = stem.len() == 4 && matches!(stem.as_bytes()[3], b'1'..=b'9');
  let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
    || numbered && (stem.starts_with("COM") || stem.starts_with("LPT"));
  let chars = name.chars().any(|c| c.is_control() || r#"<>:"/\|?*"#.contains(c));
  match name.is_empty() || chars || device || name.ends_with(['.', ' ']) {
    true => Err(bad(format!(
      "the name {name:?} makes no Windows file name: none of <>:\"/\\|?*, no control character, \
       no . or space at its end, no device's name"
    ))),
    false => Ok(()),
  }
}

// MARK: The exe's resources

/// The exe's resources: its icon, the PNGs by size as `RT_ICON`s 1, 2, …, its version, and
/// [MANIFEST].
fn resources(cfg: &Config, pngs: &BTreeMap<usize, Vec<u8>>) -> io::Result<Tree> {
  let one = |data: Vec<u8>| BTreeMap::from([(1, BTreeMap::from([(LANG, data)]))]);
  let icons = (1..).zip(pngs.values()).map(|(id, png)| (id, BTreeMap::from([(LANG, png.clone())])));
  Ok(Tree::from([
    (RT_ICON, icons.collect()),
    (RT_GROUP_ICON, one(group_icon(pngs, 1)?)),
    (RT_VERSION, one(version_info(cfg)?)),
    (RT_MANIFEST, one(MANIFEST.as_bytes().to_vec())),
  ]))
}

/// A VS_VERSIONINFO block's value.
enum Val<'a> {
  Text(&'a str),
  Bin(&'a [u8]),
  Empty,
}

/// The exe's RT_VERSION: the version as numbers ([quad]) and [version_strings], in en-US and
/// Unicode as the translation says.
fn version_info(cfg: &Config) -> io::Result<Vec<u8>> {
  let [a, b, c, d] = quad(&cfg.version)?.map(u32::from);
  let (ms, ls) = (a << 16 | b, c << 16 | d);
  // VS_FIXEDFILEINFO: its signature and version, the file's and the product's version, no flags
  // of the 0x3F mask, VOS_NT_WINDOWS32, VFT_APP, no subtype and no date.
  let fixed = [0xFEEF04BD, 0x10000, ms, ls, ms, ls, 0x3F, 0, 0x40004, 1, 0, 0, 0u32];
  let fixed: Vec<u8> = fixed.iter().flat_map(|x| x.to_le_bytes()).collect();
  let strings = version_strings(cfg)?;
  let strings: Vec<_> = strings.iter().map(|(k, v)| node(k, Val::Text(v), &[])).collect();
  let table = node(&format!("{LANG:04X}04B0"), Val::Empty, &strings);
  let translation: Vec<u8> = [LANG, 1200].iter().flat_map(|x| x.to_le_bytes()).collect();
  let var = node("Translation", Val::Bin(&translation), &[]);
  let children =
    [node("StringFileInfo", Val::Empty, &[table]), node("VarFileInfo", Val::Empty, &[var])];
  Ok(node("VS_VERSION_INFO", Val::Bin(&fixed), &children))
}

/// One VS_VERSIONINFO block (verrsrc.h): its length, its value's and its type, the key, the value
/// and the children, each at 4 bytes. A text value's length is in characters, its NUL among them,
/// and its type 1, as is that of a block with no value.
fn node(key: &str, value: Val, children: &[Vec<u8>]) -> Vec<u8> {
  let (text, value) = match value {
    Val::Text(s) => (true, utf16z(s)),
    Val::Bin(b) => (false, b.to_vec()),
    Val::Empty => (true, vec![]),
  };

  let mut body = vec![0; 6];
  body.extend(utf16z(key));
  pad4(&mut body);
  body.extend(&value);
  for child in children {
    pad4(&mut body);
    body.extend(child);
  }

  let len = if text { value.len() / 2 } else { value.len() };
  let size = body.len() as u16;
  body[..2].copy_from_slice(&size.to_le_bytes());
  body[2..4].copy_from_slice(&(len as u16).to_le_bytes());
  body[4..6].copy_from_slice(&(text as u16).to_le_bytes());
  body
}

fn utf16z(s: &str) -> Vec<u8> {
  s.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect()
}

fn pad4(data: &mut Vec<u8>) {
  data.resize(data.len().next_multiple_of(4), 0);
}

/// The exe's version strings: the app's name its FileDescription, which Task Manager shows,
/// `windows.copyright` its LegalCopyright, else "© <publisher>".
fn version_strings(cfg: &Config) -> io::Result<[(&'static str, String); 8]> {
  let copyright = match cfg.windows.get("copyright") {
    None => format!("© {}", publisher(cfg)),
    Some(Value::String(s)) => s.clone(),
    Some(_) => return Err(bad("[package.metadata.shipo.windows] copyright is not a string")),
  };

  Ok([
    ("CompanyName", publisher(cfg)),
    ("FileDescription", cfg.name.clone()),
    ("FileVersion", cfg.version.clone()),
    ("InternalName", cfg.bin.clone()),
    ("LegalCopyright", copyright),
    ("OriginalFilename", format!("{}.exe", cfg.name)),
    ("ProductName", cfg.name.clone()),
    ("ProductVersion", cfg.version.clone()),
  ])
}

/// Who Settings › Apps names: the config's publisher, else the id's owner (`me.vladkens.x`).
fn publisher(cfg: &Config) -> String {
  if !cfg.publisher.is_empty() {
    return cfg.publisher.clone();
  }

  let parts: Vec<_> = cfg.id.split('.').collect();
  parts[if parts.len() > 2 { 1 } else { 0 }].to_string()
}

/// The version as Windows' four numbers: its leading ones, up to four, then zeros (1.2.3.0 of
/// 1.2.3-beta.1).
fn quad(version: &str) -> io::Result<[u16; 4]> {
  let mut nums = vec![];
  for part in version.split('.') {
    let digits = part.len() - part.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if digits == 0 || nums.len() == 4 {
      break;
    }

    let n = part[..digits].parse::<u16>();
    nums.push(n.map_err(|_| bad(format!("the version {version}: Windows' numbers end at 65535")))?);
    if digits < part.len() {
      break;
    }
  }

  if nums.is_empty() {
    return Err(bad(format!("the version {version} starts with no number")));
  }

  nums.resize(4, 0);
  Ok([nums[0], nums[1], nums[2], nums[3]])
}

// MARK: The setup's script

/// `s` in an NSIS string: `$` doubled (a define's `${` too), a quote `$\"`, line breaks a space.
fn nsis(s: &str) -> String {
  let s = s.replace('$', "$$").replace('"', r#"$\""#);
  s.split(['\r', '\n']).filter(|line| !line.is_empty()).collect::<Vec<_>>().join(" ").trim().into()
}

/// The setup's AppId, the same in every version, so a newer setup replaces the install:
/// `windows.app-id`, else the UUID v5 of the app's id in the DNS namespace. Never changed.
fn app_id(cfg: &Config) -> io::Result<String> {
  let hex = match cfg.windows.get("app-id") {
    None => uuid5(&cfg.id),
    Some(Value::String(s)) => {
      let given = s.strip_prefix('{').and_then(|s| s.strip_suffix('}')).unwrap_or(s);
      let dashed =
        given.len() == 36 && [8, 13, 18, 23].iter().all(|&i| given.as_bytes()[i] == b'-');
      let hex: String = given.chars().filter(|&c| !dashed || c != '-').collect();
      match hex.len() == 32 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        true => hex.to_ascii_lowercase(),
        false => {
          return Err(bad(format!("[package.metadata.shipo.windows] app-id {s} is no GUID")));
        }
      }
    }
    Some(_) => return Err(bad("[package.metadata.shipo.windows] app-id is not a string")),
  };

  let part = |a: usize, b: usize| &hex[a..b];
  Ok(format!(
    "{{{}-{}-{}-{}-{}}}",
    part(0, 8),
    part(8, 12),
    part(12, 16),
    part(16, 20),
    part(20, 32)
  ))
}

/// The UUID v5 (RFC 9562) of `name` in the DNS namespace, in 32 hex digits.
fn uuid5(name: &str) -> String {
  let mut data = vec![0x6B, 0xA7, 0xB8, 0x10, 0x9D, 0xAD, 0x11, 0xD1, 0x80, 0xB4, 0x00, 0xC0];
  data.extend([0x4F, 0xD4, 0x30, 0xC8]);
  data.extend(name.as_bytes());
  let mut id = sha1(&data);
  id[6] = id[6] & 0x0F | 0x50;
  id[8] = id[8] & 0x3F | 0x80;
  id[..16].iter().map(|b| format!("{b:02x}")).collect()
}

/// SHA-1 (RFC 3174), for [uuid5] alone.
fn sha1(data: &[u8]) -> [u8; 20] {
  let mut msg = data.to_vec();
  msg.push(0x80);
  msg.resize((msg.len() + 8).next_multiple_of(64) - 8, 0);
  msg.extend((data.len() as u64 * 8).to_be_bytes());

  let mut h = [0x67452301u32, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
  for block in msg.chunks(64) {
    let mut w = [0u32; 80];
    for (i, word) in block.chunks(4).enumerate() {
      w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
    }

    for i in 16..80 {
      w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
    }

    let [mut a, mut b, mut c, mut d, mut e] = h;
    for (i, &x) in w.iter().enumerate() {
      let (f, k) = match i {
        0..20 => (b & c | !b & d, 0x5A827999),
        20..40 => (b ^ c ^ d, 0x6ED9EBA1),
        40..60 => (b & c | b & d | c & d, 0x8F1BBCDC),
        _ => (b ^ c ^ d, 0xCA62C1D6),
      };

      let t = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(x);
      (e, d, c, b, a) = (d, c, b.rotate_left(30), a, t);
    }

    for (h, x) in h.iter_mut().zip([a, b, c, d, e]) {
      *h = h.wrapping_add(x);
    }
  }

  let mut out = [0; 20];
  for (chunk, x) in out.chunks_mut(4).zip(h) {
    chunk.copy_from_slice(&x.to_be_bytes());
  }

  out
}

/// The setup's script. It packs `app.exe`, `app.ico` and, with `license`, `LICENSE.txt` from its
/// folder and writes the setup there as `out.exe`: an ASCII name, which makensis opens in any
/// locale, and never `setup.exe`, which makensis -WX refuses (warning 9000). Its `.onInit` exits 2
/// on a Windows of another arch; the stub is x86, so that runs on both, and an x64 setup runs on
/// arm64 too. Settings › Apps shows `kib`. The uninstaller removes only what is its own, the files
/// a setup writes and the updater's staging, which its installer couldn't remove, then the folder
/// if empty, as a `/D=` may share it; then the shortcut and the launch at login, which would start
/// a removed exe. It keeps the app's data.
fn nsi(cfg: &Config, arch: &str, license: bool, kib: u64) -> io::Result<String> {
  let &(_, win, _) = of(arch)?;
  let name = nsis(&cfg.name);
  let exe = format!(r"$INSTDIR\{name}.exe");
  let key = format!(r"{UNINSTALL}\{}", app_id(cfg)?);
  let id = format!(r#""{}""#, nsis(&cfg.id));
  let version = quad(&cfg.version)?.map(|n| n.to_string()).join(".");
  let check: &[&str] = match arch {
    "x86_64" => &["${IfNot} ${IsNativeAMD64}", "${AndIfNot} ${IsNativeARM64}"],
    _ => &["${IfNot} ${IsNativeARM64}"],
  };

  let mut strings = version_strings(cfg)?;
  for (key, value) in &mut strings {
    if *key == "FileDescription" {
      *value = format!("{} Setup", cfg.name);
    }
  }

  let keys = ["ProductName", "ProductVersion", "FileVersion", "FileDescription"];
  let keys = keys.into_iter().chain(["CompanyName", "LegalCopyright"]);
  let strings = keys.map(|k| {
    let value = strings.iter().find(|(key, _)| *key == k).map_or("", |(_, v)| v);
    format!(r#"VIAddVersionKey /LANG=${{LANG_ENGLISH}} "{k}" "{}""#, nsis(value))
  });

  let entries = [
    ("DisplayName", name.clone()),
    ("DisplayVersion", nsis(&cfg.version)),
    ("Publisher", nsis(&publisher(cfg))),
    ("DisplayIcon", exe.clone()),
    ("InstallLocation", "$INSTDIR".into()),
    ("UninstallString", r#"$\"$INSTDIR\Uninstall.exe$\""#.into()),
    ("QuietUninstallString", r#"$\"$INSTDIR\Uninstall.exe$\" /S"#.into()),
  ];

  let mut lines = vec![
    "Unicode true".to_string(),
    "ManifestDPIAware true".into(),
    "RequestExecutionLevel user".into(),
    "SetCompressor /SOLID lzma".into(),
    format!(r#"Name "{name}" "{}""#, name.replace('&', "&&")),
    r#"OutFile "out.exe""#.into(),
    format!(r#"InstallDir "$LOCALAPPDATA\Programs\{name}""#),
    // An update, `/S` with no `/D=`, goes where the app is.
    format!(r#"InstallDirRegKey HKCU "{key}" "InstallLocation""#),
    // A file it can't write fails the setup, never skipped with an exit code of 0.
    "AllowSkipFiles off".into(),
    String::new(),
    "!include MUI2.nsh".into(),
    "!include x64.nsh".into(),
    String::new(),
    r#"!define MUI_ICON "app.ico""#.into(),
    r#"!define MUI_UNICON "app.ico""#.into(),
    // The finish page's "Run": a silent install, the updater's, shows no page and starts nothing.
    format!(r#"!define MUI_FINISHPAGE_RUN "{exe}""#),
    "!insertmacro MUI_PAGE_INSTFILES".into(),
    "!insertmacro MUI_PAGE_FINISH".into(),
    "!insertmacro MUI_UNPAGE_CONFIRM".into(),
    "!insertmacro MUI_UNPAGE_INSTFILES".into(),
    r#"!insertmacro MUI_LANGUAGE "English""#.into(),
    String::new(),
    format!("VIProductVersion {version}"),
    format!("VIFileVersion {version}"),
  ];

  lines.extend(strings);
  lines.extend([String::new(), "Function .onInit".into()]);
  lines.extend(check.iter().map(|line| format!("  {line}")));
  lines.extend([
    format!(r#"    MessageBox MB_ICONSTOP "This setup of {name} is for {win} Windows." /SD IDOK"#),
    "    SetErrorLevel 2".into(),
    "    Abort".into(),
    "  ${EndIf}".into(),
    "FunctionEnd".into(),
    String::new(),
    "Section".into(),
    r#"  SetOutPath "$INSTDIR""#.into(),
  ]);

  // The app that just quit may hold its exe a moment longer: tried for 5 s, then the setup fails.
  // `SetOverwrite try` sets the error flag where `on` would ask or abort.
  lines.extend([
    "  StrCpy $R0 0".to_string(),
    "  SetOverwrite try".into(),
    "  ${Do}".into(),
    "    ClearErrors".into(),
    format!(r#"    File "/oname={name}.exe" "app.exe""#),
    "    ${IfNot} ${Errors}".into(),
    "      ${ExitDo}".into(),
    "    ${EndIf}".into(),
    "    IntOp $R0 $R0 + 1".into(),
    "    ${If} $R0 >= 10".into(),
    format!(r#"      MessageBox MB_ICONSTOP "{exe} is in use: close {name} and run this setup again." /SD IDOK"#),
    "      SetErrorLevel 2".into(),
    "      Abort".into(),
    "    ${EndIf}".into(),
    "    Sleep 500".into(),
    "  ${Loop}".into(),
    "  SetOverwrite on".into(),
  ]);

  if license {
    lines.push(r#"  File "/oname=LICENSE.txt" "LICENSE.txt""#.into());
  }

  lines.push(r#"  WriteUninstaller "$INSTDIR\Uninstall.exe""#.into());
  lines.push(format!(
    r#"  CreateShortcut "$SMPROGRAMS\{name}.lnk" "{exe}" "" "{exe}" 0 SW_SHOWNORMAL "" "{}""#,
    nsis(&cfg.comment)
  ));

  lines.extend(entries.iter().map(|(k, v)| format!(r#"  WriteRegStr HKCU "{key}" "{k}" "{v}""#)));
  lines.extend([
    format!(r#"  WriteRegDWORD HKCU "{key}" "NoModify" 1"#),
    format!(r#"  WriteRegDWORD HKCU "{key}" "NoRepair" 1"#),
    format!(r#"  WriteRegDWORD HKCU "{key}" "EstimatedSize" {kib}"#),
    "SectionEnd".into(),
    String::new(),
    r#"Section "Uninstall""#.into(),
  ]);

  // LICENSE.txt whether or not this setup has one: an older one may have written it.
  lines.extend([
    format!(r#"  Delete "{exe}""#),
    r#"  Delete "$INSTDIR\LICENSE.txt""#.into(),
    r#"  Delete "$INSTDIR\Uninstall.exe""#.into(),
    r#"  FindFirst $0 $1 "$INSTDIR\.shipo-update-*""#.into(),
    r#"  ${DoWhile} $1 != """#.into(),
    r#"    RMDir /r "$INSTDIR\$1""#.into(),
    "    FindNext $0 $1".into(),
    "  ${Loop}".into(),
    "  FindClose $0".into(),
    r#"  RMDir "$INSTDIR""#.into(),
    format!(r#"  Delete "$SMPROGRAMS\{name}.lnk""#),
    format!(r#"  DeleteRegValue HKCU "{RUN}" {id}"#),
    format!(r#"  DeleteRegValue HKCU "{APPROVED}" {id}"#),
    format!(r#"  DeleteRegKey HKCU "{key}""#),
    "SectionEnd".into(),
  ]);

  Ok(lines.join("\n") + "\n")
}

#[cfg(test)]
mod tests {
  use toml::Table;

  use super::*;
  use crate::pe::tests::{consistent, exe};

  /// A config in `dir` with the `[windows]` table `windows`.
  fn config(dir: &Path, windows: &str) -> Config {
    Config {
      id: "com.example.hello".into(),
      name: "Hello".into(),
      icon: dir.join("icon.png"),
      comment: "Says hello".into(),
      version: "1.2.0-beta.1".into(),
      bin: "hello".into(),
      macos: Table::new(),
      windows: toml::from_str(windows).unwrap(),
      linux: Table::new(),
      publisher: String::new(),
      authors: vec![],
      repository: String::new(),
      root: dir.to_path_buf(),
      workspace: dir.to_path_buf(),
      target: dir.join("target"),
    }
  }

  const NSI: &str = r#"Unicode true
ManifestDPIAware true
RequestExecutionLevel user
SetCompressor /SOLID lzma
Name "Hello" "Hello"
OutFile "out.exe"
InstallDir "$LOCALAPPDATA\Programs\Hello"
InstallDirRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\{a90596b0-07fa-55a1-9740-d4470c5cef1c}" "InstallLocation"
AllowSkipFiles off

!include MUI2.nsh
!include x64.nsh

!define MUI_ICON "app.ico"
!define MUI_UNICON "app.ico"
!define MUI_FINISHPAGE_RUN "$INSTDIR\Hello.exe"
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

VIProductVersion 1.2.0.0
VIFileVersion 1.2.0.0
VIAddVersionKey /LANG=${LANG_ENGLISH} "ProductName" "Hello"
VIAddVersionKey /LANG=${LANG_ENGLISH} "ProductVersion" "1.2.0-beta.1"
VIAddVersionKey /LANG=${LANG_ENGLISH} "FileVersion" "1.2.0-beta.1"
VIAddVersionKey /LANG=${LANG_ENGLISH} "FileDescription" "Hello Setup"
VIAddVersionKey /LANG=${LANG_ENGLISH} "CompanyName" "example"
VIAddVersionKey /LANG=${LANG_ENGLISH} "LegalCopyright" "© example"

Function .onInit
  ${IfNot} ${IsNativeAMD64}
  ${AndIfNot} ${IsNativeARM64}
    MessageBox MB_ICONSTOP "This setup of Hello is for x64 Windows." /SD IDOK
    SetErrorLevel 2
    Abort
  ${EndIf}
FunctionEnd

Section
  SetOutPath "$INSTDIR"
  StrCpy $R0 0
  SetOverwrite try
  ${Do}
    ClearErrors
    File "/oname=Hello.exe" "app.exe"
    ${IfNot} ${Errors}
      ${ExitDo}
    ${EndIf}
    IntOp $R0 $R0 + 1
    ${If} $R0 >= 10
      MessageBox MB_ICONSTOP "$INSTDIR\Hello.exe is in use: close Hello and run this setup again." /SD IDOK
      SetErrorLevel 2
      Abort
    ${EndIf}
    Sleep 500
  ${Loop}
  SetOverwrite on
  File "/oname=LICENSE.txt" "LICENSE.txt"
  WriteUninstaller "$INSTDIR\Uninstall.exe"
  CreateShortcut "$SMPROGRAMS\Hello.lnk" "$INSTDIR\Hello.exe" "" "$INSTDIR\Hello.exe" 0 SW_SHOWNORMAL "" "Says hello"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\{a90596b0-07fa-55a1-9740-d4470c5cef1c}" "DisplayName" "Hello"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\{a90596b0-07fa-55a1-9740-d4470c5cef1c}" "DisplayVersion" "1.2.0-beta.1"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\{a90596b0-07fa-55a1-9740-d4470c5cef1c}" "Publisher" "example"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\{a90596b0-07fa-55a1-9740-d4470c5cef1c}" "DisplayIcon" "$INSTDIR\Hello.exe"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\{a90596b0-07fa-55a1-9740-d4470c5cef1c}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\{a90596b0-07fa-55a1-9740-d4470c5cef1c}" "UninstallString" "$\"$INSTDIR\Uninstall.exe$\""
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\{a90596b0-07fa-55a1-9740-d4470c5cef1c}" "QuietUninstallString" "$\"$INSTDIR\Uninstall.exe$\" /S"
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\{a90596b0-07fa-55a1-9740-d4470c5cef1c}" "NoModify" 1
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\{a90596b0-07fa-55a1-9740-d4470c5cef1c}" "NoRepair" 1
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\{a90596b0-07fa-55a1-9740-d4470c5cef1c}" "EstimatedSize" 42
SectionEnd

Section "Uninstall"
  Delete "$INSTDIR\Hello.exe"
  Delete "$INSTDIR\LICENSE.txt"
  Delete "$INSTDIR\Uninstall.exe"
  FindFirst $0 $1 "$INSTDIR\.shipo-update-*"
  ${DoWhile} $1 != ""
    RMDir /r "$INSTDIR\$1"
    FindNext $0 $1
  ${Loop}
  FindClose $0
  RMDir "$INSTDIR"
  Delete "$SMPROGRAMS\Hello.lnk"
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "com.example.hello"
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run" "com.example.hello"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\{a90596b0-07fa-55a1-9740-d4470c5cef1c}"
SectionEnd
"#;

  #[test]
  fn script() {
    let cfg = config(Path::new("/app"), "");
    assert_eq!(nsi(&cfg, "x86_64", true, 42).unwrap(), NSI);

    let arm = nsi(&cfg, "aarch64", false, 42).unwrap();
    assert!(arm.contains("  ${IfNot} ${IsNativeARM64}\n    MessageBox"), "{arm}");
    assert!(arm.contains("is for arm64 Windows."), "{arm}");
    assert!(!arm.contains(r#"File "/oname=LICENSE.txt""#), "{arm}");
    assert!(arm.contains(r#"Delete "$INSTDIR\LICENSE.txt""#), "{arm}");
  }

  #[test]
  fn script_escaped() {
    let mut cfg = config(Path::new("/app"), "copyright = \"(c) ${A} \\\"B\\\"\"");
    cfg.name = "My $App & Co".into();
    cfg.id = "com.example.$x".into();
    cfg.publisher = "Ann \"A\" $B".into();
    cfg.comment = "Says\r\nhello \"there\"\n".into();
    let text = nsi(&cfg, "x86_64", false, 1).unwrap();
    let want = [
      r#"Name "My $$App & Co" "My $$App && Co""#,
      r#"InstallDir "$LOCALAPPDATA\Programs\My $$App & Co""#,
      r#"File "/oname=My $$App & Co.exe" "app.exe""#,
      r#"Delete "$INSTDIR\My $$App & Co.exe""#,
      r#""" "Says hello $\"there$\"""#,
      r#""Publisher" "Ann $\"A$\" $$B""#,
      r#""LegalCopyright" "(c) $${A} $\"B$\"""#,
      r#"\Run" "com.example.$$x""#,
      r#"Delete "$SMPROGRAMS\My $$App & Co.lnk""#,
    ];

    for line in want {
      assert!(text.contains(line), "{line} in\n{text}");
    }
  }

  #[test]
  fn escaping() {
    let cases = [
      ("plain", "plain"),
      (r#"a "$b" ${c}"#, r#"a $\"$$b$\" $${c}"#),
      ("one\ntwo\r\nthree\n", "one two three"),
      ("$\\", "$$\\"),
    ];

    for (s, want) in cases {
      assert_eq!(nsis(s), want, "{s}");
    }
  }

  #[test]
  fn ids() {
    assert_eq!(sha1(b"abc")[..4], [0xA9, 0x99, 0x3E, 0x36]);
    let long = sha1(&[b'a'; 1000]);
    assert_eq!(
      long.iter().map(|b| format!("{b:02x}")).collect::<String>(),
      "291e9a6c66994949b57ba5e650361e98fc36b1ba"
    );

    let mut cfg = config(Path::new("/app"), "");
    cfg.id = "me.vladkens.wavo".into();
    assert_eq!(app_id(&cfg).unwrap(), "{a754b18f-b91c-5def-b91e-a32c2017c75f}");
    let given = [
      "314a56a8-8a8b-4c18-954d-b930203f086d",
      "{314A56A8-8A8B-4C18-954D-B930203F086D}",
      "314a56a88a8b4c18954db930203f086d",
    ];

    for id in given {
      let cfg = config(Path::new("/app"), &format!("app-id = {id:?}"));
      assert_eq!(app_id(&cfg).unwrap(), "{314a56a8-8a8b-4c18-954d-b930203f086d}", "{id}");
    }

    for id in
      ["314a56a8", "314a56a8-8a8b-4c18-954d-b930203f086g", "{314a56a8-8a8b4c18-954d-b930203f0-86d}"]
    {
      let cfg = config(Path::new("/app"), &format!("app-id = {id:?}"));
      assert!(app_id(&cfg).unwrap_err().to_string().ends_with("is no GUID"), "{id}");
    }

    let cfg = config(Path::new("/app"), "app-id = 1");
    assert!(app_id(&cfg).is_err());
  }

  #[test]
  fn names() {
    let mut cfg = config(Path::new("/app"), "");
    assert_eq!(publisher(&cfg), "example");
    cfg.id = "hello".into();
    assert_eq!(publisher(&cfg), "hello");
    cfg.publisher = "Ann".into();
    assert_eq!(publisher(&cfg), "Ann");

    for ok in ["Hello", "My App 2", "Ünï $&'", "COM0", "Console"] {
      assert!(file_name(ok).is_ok(), "{ok}");
    }

    for no in ["", "a/b", "a:b", "a\"b", "a\nb", "dot.", "space ", "CON", "nul.app", "Lpt3"] {
      assert!(file_name(no).is_err(), "{no}");
    }

    let cases = [
      ("1.2.3-beta.1", [1, 2, 3, 0]),
      ("2", [2, 0, 0, 0]),
      ("1.2.3.4.5", [1, 2, 3, 4]),
      ("10.0.65535", [10, 0, 65535, 0]),
      ("1.2.", [1, 2, 0, 0]),
      ("3rc", [3, 0, 0, 0]),
    ];

    for (version, want) in cases {
      assert_eq!(quad(version).unwrap(), want, "{version}");
    }

    assert_eq!(
      quad("1.65536").unwrap_err().to_string(),
      "the version 1.65536: Windows' numbers end at 65535"
    );
    assert_eq!(quad("v1").unwrap_err().to_string(), "the version v1 starts with no number");
  }

  /// A VS_VERSIONINFO block read back: its key, type, value and children, each checked to fill
  /// its length, at 4 bytes.
  #[derive(Debug)]
  struct Node {
    key: String,
    text: bool,
    value: Vec<u8>,
    children: Vec<Node>,
  }

  fn parse(data: &[u8]) -> Node {
    let u16_at = |at: usize| u16::from_le_bytes([data[at], data[at + 1]]) as usize;
    let (len, value_len, text) = (u16_at(0), u16_at(2), u16_at(4) == 1);
    assert!(len <= data.len());
    let mut at = 6;
    let mut key = vec![];
    while u16_at(at) != 0 {
      key.push(u16_at(at) as u16);
      at += 2;
    }

    at = (at + 2).next_multiple_of(4);
    let size = if text { value_len * 2 } else { value_len };
    let value = data[at..at + size].to_vec();
    at += size;
    let mut children = vec![];
    while at.next_multiple_of(4) < len {
      at = at.next_multiple_of(4);
      let child = parse(&data[at..len]);
      at += u16_at(at);
      children.push(child);
    }

    assert_eq!(at, len);
    Node { key: String::from_utf16(&key).unwrap(), text, value, children }
  }

  fn text(value: &[u8]) -> String {
    let units: Vec<u16> = value.chunks(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect();
    String::from_utf16(units.strip_suffix(&[0]).unwrap()).unwrap()
  }

  #[test]
  fn version() {
    let cfg = config(Path::new("/app"), "");
    let info = parse(&version_info(&cfg).unwrap());
    assert_eq!((info.key.as_str(), info.text, info.value.len()), ("VS_VERSION_INFO", false, 52));
    let fixed: Vec<u32> =
      info.value.chunks(4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
    assert_eq!(fixed[..6], [0xFEEF04BD, 0x10000, 0x10002, 0, 0x10002, 0]);
    assert_eq!(fixed[8..10], [0x40004, 1]);

    let [strings, var] = &info.children[..] else { panic!("{info:?}") };
    assert_eq!(
      (strings.key.as_str(), strings.text, var.key.as_str()),
      ("StringFileInfo", true, "VarFileInfo")
    );
    let [table] = &strings.children[..] else { panic!("{strings:?}") };
    assert_eq!(table.key, "040904B0");
    let got: Vec<_> = table.children.iter().map(|n| (n.key.as_str(), text(&n.value))).collect();
    let want = version_strings(&cfg).unwrap();
    let want: Vec<_> = want.iter().map(|(k, v)| (*k, v.clone())).collect();
    assert_eq!(got, want);
    assert!(table.children.iter().all(|n| n.text));
    assert_eq!(want[4], ("LegalCopyright", "© example".into()));

    let [translation] = &var.children[..] else { panic!("{var:?}") };
    assert_eq!((translation.key.as_str(), translation.text), ("Translation", false));
    assert_eq!(translation.value, [0x09, 0x04, 0xB0, 0x04]);
  }

  /// A 512 px icon at `path`, a 400 px tile with a margin.
  fn icon(path: &Path) {
    let px = (0..512 * 512).flat_map(|i: usize| {
      let (x, y) = (i % 512, i / 512);
      let inside = (56..456).contains(&x) && (56..456).contains(&y);
      [x as u8, y as u8, 128, if inside { 255 } else { 0 }]
    });

    fs::write(path, Image { w: 512, h: 512, px: px.collect() }.encode().unwrap()).unwrap();
  }

  #[test]
  fn exe_resources() {
    let tmp = Temp::new().unwrap();
    icon(&tmp.join("icon.png"));
    let cfg = config(&tmp, "");
    let pngs = icons(&Image::read(&cfg.icon).unwrap(), &ICO, true).unwrap();
    let pngs: BTreeMap<_, _> =
      pngs.iter().map(|(&n, image)| (n, image.encode().unwrap())).collect();
    let out =
      pe::with_resources(&exe(pe::AMD64), pe::AMD64, resources(&cfg, &pngs).unwrap()).unwrap();
    consistent(&out);

    let tree = pe::resources(&out).unwrap();
    assert_eq!(
      tree.keys().copied().collect::<Vec<_>>(),
      [RT_ICON, RT_GROUP_ICON, RT_VERSION, RT_MANIFEST]
    );
    assert!(tree.values().flat_map(|ids| ids.values()).all(|l| l.keys().eq([&LANG])));
    let icons: Vec<_> = tree[&RT_ICON].iter().map(|(&id, l)| (id, l[&LANG].clone())).collect();
    assert_eq!(icons, (1..).zip(pngs.values().cloned()).collect::<Vec<_>>());
    assert!(icons.iter().all(|(_, png)| png.starts_with(b"\x89PNG")));

    // The group: 6 entries, each its size, 32 bits, its PNG's length and its RT_ICON's id.
    let group = &tree[&RT_GROUP_ICON][&1][&LANG];
    assert_eq!(group[..6], [0, 0, 1, 0, 6, 0]);
    for (i, (&n, png)) in pngs.iter().enumerate() {
      let e = &group[6 + 14 * i..][..14];
      assert_eq!(e[0], (n % 256) as u8);
      assert_eq!(u16::from_le_bytes([e[6], e[7]]), 32);
      assert_eq!(u32::from_le_bytes([e[8], e[9], e[10], e[11]]) as usize, png.len());
      assert_eq!(u16::from_le_bytes([e[12], e[13]]), icons[i].0);
    }

    assert_eq!(tree[&RT_VERSION][&1][&LANG], version_info(&cfg).unwrap());
    assert_eq!(tree[&RT_MANIFEST][&1][&LANG], MANIFEST.as_bytes());
  }

  #[test]
  fn setup_built() {
    let tmp = Temp::new().unwrap();
    icon(&tmp.join("icon.png"));
    fs::write(tmp.join("LICENSE"), "MIT").unwrap();
    let mut cfg = config(&tmp, "");
    let bin = tmp.join("hello.exe");
    fs::write(&bin, exe(pe::AMD64)).unwrap();
    let out = tmp.join("out");

    let e = build(&cfg, &bin, "x86_64", &out, OsStr::new("shipo-no-makensis")).unwrap_err();
    assert_eq!(
      e.to_string(),
      "shipo-no-makensis not found: the setup needs it, `apt-get install nsis`"
    );
    assert!(!out.exists());
    #[cfg(unix)]
    {
      // A makensis that fails as the real one does on a warning, its log as `-O` names it.
      let fake = tmp.join("fake-makensis");
      let log = "warning 6001: Variable \"x\" not referenced\nError: warning treated as error\n\n\
                 Note: you may have one or two (large) stale temporary file(s) left\n";
      let script = format!(
        "#!/bin/sh\nfor a; do case $a in -O*) printf '{log}' > \"${{a#-O}}\";; esac; done\nexit 1\n"
      );
      fs::write(&fake, script).unwrap();
      crate::tool::set_mode(&fake, 0o755).unwrap();
      let e = build(&cfg, &bin, "x86_64", &out, fake.as_os_str()).unwrap_err().to_string();
      let want = "failed (exit status: 1): warning 6001: Variable \"x\" not referenced\nError: warning treated as error";
      assert!(e.ends_with(want), "{e}");
    }

    let e = setup(&cfg, &bin, "aarch64", &out).unwrap_err();
    assert_eq!(e.to_string(), format!("{}: the exe is for x64, not arm64", bin.display()));
    let e = setup(&cfg, &bin, "x86", &out).unwrap_err();
    assert_eq!(e.to_string(), "no Windows setup for x86, only for x86_64 and aarch64");
    cfg.name = "A/B".into();
    assert!(
      setup(&cfg, &bin, "x86_64", &out).unwrap_err().to_string().contains("no Windows file name")
    );
    cfg.name = "Hello".into();

    // The setup itself where makensis is, as on the release host.
    let found = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
      .any(|d| d.join("makensis").is_file());
    if !found {
      return;
    }

    let made = setup(&cfg, &bin, "x86_64", &out).unwrap();
    assert_eq!(made, out.join("Hello-dev-windows-x64-setup.exe"));
    let data = fs::read(&made).unwrap();
    assert!(data.starts_with(b"MZ") && data.len() > 30_000, "{}", data.len());
    assert_eq!(fs::read_dir(&out).unwrap().count(), 1);
  }
}
