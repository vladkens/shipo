// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! The Windows setup: the app's exe given its icon, version and manifest, and the per-user NSIS
//! setup of it that `makensis` builds, as spec.md's Windows section gives it.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::Config;
use crate::image::{ICO, Image, group_icon, ico, icons};
use crate::tool::{self, Temp, get, run};
use crate::{Arch, at, bad, wrap};

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

/// `<Name>-<label>-windows-<x64|arm64>-setup.exe` in `out_dir`, [named](crate::out) by `tag`: the
/// per-user setup of `binary`, the app's exe for `arch`, built by `makensis`, which apt's nsis
/// installs.
pub fn setup(
  cfg: &Config,
  binary: &Path,
  arch: Arch,
  tag: Option<&str>,
  out_dir: &Path,
) -> io::Result<PathBuf> {
  build(cfg, binary, arch, tag, out_dir, OsStr::new("makensis"))
}

/// [setup] by the program `makensis`.
fn build(
  cfg: &Config,
  binary: &Path,
  arch: Arch,
  tag: Option<&str>,
  out_dir: &Path,
  makensis: &OsStr,
) -> io::Result<PathBuf> {
  file_name(&cfg.name)?;

  let exe = tool::read(binary)?;
  let pngs = icons(&Image::read(&cfg.icon)?, &ICO, true)?;
  let pngs = pngs.iter().map(|(&n, image)| Ok((n, image.encode()?)));
  let pngs = pngs.collect::<io::Result<BTreeMap<_, _>>>()?;
  let exe = with_resources(&exe, arch.machine(), resources(cfg, &pngs)?).map_err(at(binary))?;

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
      return Err(wrap(
        &e,
        format!("{tool} not found: the setup needs it, `apt-get install nsis`"),
      ));
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
  let out = crate::out(out_dir, &cfg.name, tag, &format!("windows-{}-setup.exe", arch.windows()));
  let setup = tool::read(&tmp.join("out.exe"))?;
  tool::place(&out, |part| File::create_new(part)?.write_all(&setup))?;
  Ok(out)
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
fn resources(cfg: &Config, pngs: &BTreeMap<usize, Vec<u8>>) -> io::Result<Resources> {
  let one = |data: Vec<u8>| BTreeMap::from([(1, BTreeMap::from([(LANG, data)]))]);
  let icons = (1..).zip(pngs.values()).map(|(id, png)| (id, BTreeMap::from([(LANG, png.clone())])));
  Ok(Resources::from([
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
  let [a, b, c, d] = quad(cfg)?.map(u32::from);
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
  let copyright = cfg.windows.copyright.clone();
  let copyright = copyright.unwrap_or_else(|| format!("© {}", cfg.publisher));
  Ok([
    ("CompanyName", cfg.publisher.clone()),
    ("FileDescription", cfg.name.clone()),
    ("FileVersion", cfg.version.clone()),
    ("InternalName", cfg.bin.clone()),
    ("LegalCopyright", copyright),
    ("OriginalFilename", format!("{}.exe", cfg.name)),
    ("ProductName", cfg.name.clone()),
    ("ProductVersion", cfg.version.clone()),
  ])
}

/// The version as Windows' four numbers: its leading ones, up to four, then zeros (1.2.3.0 of
/// 1.2.3-beta.1).
fn quad(cfg: &Config) -> io::Result<[u16; 4]> {
  let mut nums = [0; 4];
  for (n, part) in nums.iter_mut().zip(cfg.numeric()?.split('.')) {
    let max = || bad(format!("the version {}: Windows' numbers end at 65535", cfg.version));
    *n = part.parse().map_err(|_| max())?;
  }

  Ok(nums)
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
  let hex = match &cfg.windows.app_id {
    None => uuid5(&cfg.id),
    Some(s) => {
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
  tool::hex(&id[..16])
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
fn nsi(cfg: &Config, arch: Arch, license: bool, kib: u64) -> io::Result<String> {
  let win = arch.windows();
  let name = nsis(&cfg.name);
  let exe = format!(r"$INSTDIR\{name}.exe");
  let key = format!(r"{UNINSTALL}\{}", app_id(cfg)?);
  let id = format!(r#""{}""#, nsis(&cfg.id));
  let version = quad(cfg)?.map(|n| n.to_string()).join(".");
  let check: &[&str] = match arch {
    Arch::X86_64 => &["${IfNot} ${IsNativeAMD64}", "${AndIfNot} ${IsNativeARM64}"],
    Arch::Aarch64 => &["${IfNot} ${IsNativeARM64}"],
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
    ("Publisher", nsis(&cfg.publisher)),
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

// MARK: PE resources

const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
const RT_VERSION: u16 = 16;
const RT_MANIFEST: u16 = 24;
/// en-US, rc.exe's language.
const LANG: u16 = 0x409;
/// The COFF header's machines of x64 and arm64.
pub const AMD64: u16 = 0x8664;
pub const ARM64: u16 = 0xAA64;

/// Resources by type, id and language, all numbered: the packer names none by a string.
type Resources = BTreeMap<u16, BTreeMap<u16, BTreeMap<u16, Vec<u8>>>>;

/// A section header's size.
const SECTION: usize = 40;
/// The data directories' offset in a PE32+ optional header; the resource directory is the third,
/// the certificate table the fifth.
const DIRS: usize = 112;
/// IMAGE_SCN_CNT_INITIALIZED_DATA | IMAGE_SCN_MEM_READ.
const RSRC: u32 = 0x4000_0040;
/// A resource directory entry's offset with this bit is a subdirectory's.
const SUBDIR: u32 = 1 << 31;

/// The headers of a PE32+ file that the edit reads; offsets are into the file.
struct Pe {
  /// The COFF file header's, after `PE\0\0`.
  coff: usize,
  /// The optional header's.
  opt: usize,
  /// The section table's.
  table: usize,
  machine: u16,
  sections: Vec<Section>,
  section_align: u32,
  file_align: u32,
  image_size: u32,
  headers_size: u32,
}

struct Section {
  va: u32,
  vsize: u32,
  raw_size: u32,
  raw_at: u32,
}

impl Pe {
  fn read(data: &[u8]) -> io::Result<Self> {
    if data.get(..2) != Some(b"MZ") {
      return Err(bad("not an exe: no MZ header"));
    }

    let sig = u32_at(data, 0x3C)? as usize;
    if data.get(sig..sig + 4) != Some(b"PE\0\0") {
      return Err(bad("not an exe: no PE header"));
    }

    let coff = sig + 4;
    let (machine, count, opt_size) =
      (u16_at(data, coff)?, u16_at(data, coff + 2)?, u16_at(data, coff + 16)?);
    let opt = coff + 20;
    if u16_at(data, opt)? != 0x20B {
      return Err(bad("not a 64-bit exe (PE32+)"));
    }

    if (opt_size as usize) < DIRS + 8 * 5 || u32_at(data, opt + DIRS - 4)? < 5 {
      return Err(bad("its optional header has no resource or certificate directory"));
    }

    let table = opt + opt_size as usize;
    let section = |i: usize| {
      let at = table + SECTION * i;
      Ok(Section {
        vsize: u32_at(data, at + 8)?,
        va: u32_at(data, at + 12)?,
        raw_size: u32_at(data, at + 16)?,
        raw_at: u32_at(data, at + 20)?,
      })
    };

    let sections: Vec<Section> = (0..count as usize).map(section).collect::<io::Result<_>>()?;
    if sections.iter().any(|s| s.raw_at as u64 + s.raw_size as u64 > data.len() as u64) {
      return Err(bad("the exe ends inside its sections"));
    }

    let (section_align, file_align) = (u32_at(data, opt + 32)?, u32_at(data, opt + 36)?);
    // Windows' ranges for an exe; a driver's smaller section alignment is not one.
    let file_ok = file_align.is_power_of_two() && (512..=0x10000).contains(&file_align);
    if !file_ok || !section_align.is_power_of_two() || section_align < 4096 {
      return Err(bad(format!(
        "its file alignment {file_align:#x} or section alignment {section_align:#x} is not an \
         exe's: a power of two, 512 to 64K and at least 4K"
      )));
    }

    let (image_size, headers_size) = (u32_at(data, opt + 56)?, u32_at(data, opt + 60)?);
    Ok(Self {
      coff,
      opt,
      table,
      machine,
      sections,
      section_align,
      file_align,
      image_size,
      headers_size,
    })
  }

  /// The `len` bytes at `rva` in the file, all in one section's data.
  fn slice<'a>(&self, data: &'a [u8], rva: u32, len: u32) -> io::Result<&'a [u8]> {
    let (rva, len) = (rva as u64, len as u64);
    for s in &self.sections {
      let (va, size) = (s.va as u64, s.vsize.min(s.raw_size) as u64);
      if va <= rva && rva + len <= va + size {
        let at = (s.raw_at as u64 + rva - va) as usize;
        return data.get(at..at + len as usize).ok_or_else(short);
      }
    }

    Err(bad(format!("its resources point outside its sections' data, at {rva:#x}")))
  }

  /// The resources the resource directory points at but those of the types `skip` gives; none
  /// without one.
  fn resources(&self, data: &[u8], skip: impl Fn(u16) -> bool) -> io::Result<Resources> {
    let (rva, size) = (u32_at(data, self.opt + DIRS + 16)?, u32_at(data, self.opt + DIRS + 20)?);
    if rva == 0 || size == 0 {
      return Ok(Resources::new());
    }

    let dir = self.slice(data, rva, size)?;
    walk(dir, skip, data.len(), |rva, len| self.slice(data, rva, len))
  }
}

/// The resources of the resource directory `dir` but those of the types `skip` gives, their data
/// by `read` (an RVA and a length). A directory pointing into itself is an error, never an endless
/// walk: it visits at most as many entries as `dir` holds and `limit` bytes of data.
fn walk<'a>(
  dir: &[u8],
  skip: impl Fn(u16) -> bool,
  limit: usize,
  read: impl Fn(u32, u32) -> io::Result<&'a [u8]>,
) -> io::Result<Resources> {
  let (mut budget, mut bytes) = (dir.len() / 8, 0);
  let mut tree = Resources::new();
  for (kind, at) in entries(dir, 0, true, &mut budget)? {
    if skip(kind) {
      continue;
    }

    for (id, at) in entries(dir, at, true, &mut budget)? {
      for (lang, at) in entries(dir, at, false, &mut budget)? {
        let found = read(u32_at(dir, at)?, u32_at(dir, at + 4)?)?;
        bytes += found.len();
        if bytes > limit {
          return Err(bad("its resource directory points into itself"));
        }

        tree.entry(kind).or_default().entry(id).or_default().insert(lang, found.to_vec());
      }
    }
  }

  Ok(tree)
}

/// The ids in the resource directory at `at` in `dir` and where each points, a subdirectory if
/// `sub`, else a data entry; each entry takes one of `budget`.
fn entries(dir: &[u8], at: usize, sub: bool, budget: &mut usize) -> io::Result<Vec<(u16, usize)>> {
  if u16_at(dir, at + 12)? > 0 {
    return Err(bad("a resource named by a string, which the packer can't keep"));
  }

  let count = u16_at(dir, at + 14)? as usize;
  *budget =
    budget.checked_sub(count).ok_or_else(|| bad("its resource directory points into itself"))?;

  let entry = |i: usize| {
    let (id, to) = (u32_at(dir, at + 16 + 8 * i)?, u32_at(dir, at + 20 + 8 * i)?);
    match u16::try_from(id) {
      Ok(id) if (to & SUBDIR != 0) == sub => Ok((id, (to & !SUBDIR) as usize)),
      _ => Err(bad("its resource directory is malformed")),
    }
  };

  (0..count).map(entry).collect()
}

/// `data`, a PE32+ exe for `machine`, with a new last section, `.rsrc`, of the resources `ours`
/// and the exe's own of the other types, the resource directory pointing at it; the old ones stay
/// in the file, unused. Its checksum is set again, though Windows checks only a driver's.
fn with_resources(data: &[u8], machine: u16, ours: Resources) -> io::Result<Vec<u8>> {
  let pe = Pe::read(data)?;
  if pe.machine != machine {
    return Err(bad(format!("the exe is for {}, not {}", arch(pe.machine), arch(machine))));
  }

  // A new section breaks a signature: it covers the headers.
  if u32_at(data, pe.opt + DIRS + 36)? != 0 {
    return Err(bad("the exe is signed: its resources go in before its signature"));
  }

  // Only the types kept are read: one replaced may hold what the packer can't read, as
  // embed-resource's icon named `APP`.
  let mut tree = pe.resources(data, |kind| ours.contains_key(&kind))?;
  tree.extend(ours);

  // The new section header must fit before the first section's data, over zeros only.
  let header = pe.table + SECTION * pe.sections.len();
  let first = pe.sections.iter().map(|s| s.raw_at).filter(|&at| at > 0).min();
  let room = first.unwrap_or(u32::MAX).min(pe.headers_size) as usize;
  let free = data.get(header..header + SECTION).is_some_and(|b| b.iter().all(|&x| x == 0));
  if header + SECTION > room || !free {
    return Err(bad("its headers have no room for one more section header"));
  }

  let end = pe.sections.iter().map(|s| s.va as u64 + s.vsize as u64).max().unwrap_or(0);
  let va = end.max(pe.image_size as u64).next_multiple_of(pe.section_align as u64);
  let blob = rsrc(&tree, va as u32);
  let (at, raw) = (
    (data.len() as u64).next_multiple_of(pe.file_align as u64),
    (blob.len() as u64).next_multiple_of(pe.file_align as u64),
  );
  let image = (va + blob.len() as u64).next_multiple_of(pe.section_align as u64);
  if image > u32::MAX as u64 || at + raw > u32::MAX as u64 {
    return Err(bad("the exe with its resources passes 4 GiB"));
  }

  let mut out = data.to_vec();
  out.resize(at as usize, 0);
  let fields = [blob.len() as u32, va as u32, raw as u32, at as u32, 0, 0, 0, RSRC];
  put(&mut out, header, b".rsrc\0\0\0");
  for (i, x) in fields.iter().enumerate() {
    // NumberOfRelocations and NumberOfLinenumbers are the 16-bit halves of the seventh field.
    put(&mut out, header + 8 + 4 * i, &x.to_le_bytes());
  }

  out.extend_from_slice(&blob);
  out.resize((at + raw) as usize, 0);
  put(&mut out, pe.coff + 2, &(pe.sections.len() as u16 + 1).to_le_bytes());
  let initialized = u32_at(&out, pe.opt + 8)?.saturating_add(raw as u32);
  put(&mut out, pe.opt + 8, &initialized.to_le_bytes());
  put(&mut out, pe.opt + 56, &(image as u32).to_le_bytes());
  put(&mut out, pe.opt + DIRS + 16, &(va as u32).to_le_bytes());
  put(&mut out, pe.opt + DIRS + 20, &(blob.len() as u32).to_le_bytes());
  put(&mut out, pe.opt + 64, &[0; 4]);
  let sum = checksum(&out);
  put(&mut out, pe.opt + 64, &sum.to_le_bytes());
  Ok(out)
}

/// The resource section of `tree` at `rva`: the root, a directory per type, one per id (its
/// languages), the data entries, then the data, each at 8 bytes.
fn rsrc(tree: &Resources, rva: u32) -> Vec<u8> {
  let dir = |n: usize| 16 + 8 * n;
  let ids = tree.values();
  let names: usize = ids.clone().map(|ids| dir(ids.len())).sum();
  let langs: usize = ids.clone().flat_map(|ids| ids.values()).map(|l| dir(l.len())).sum();
  let leaves = ids.flat_map(|ids| ids.values()).map(|l| l.len()).sum::<usize>();
  let entries = dir(tree.len()) + names + langs;
  let mut out = vec![0; (entries + 16 * leaves).next_multiple_of(8)];

  let (mut sub, mut lang_dir, mut leaf) = (dir(tree.len()), dir(tree.len()) + names, 0);
  put(&mut out, 14, &(tree.len() as u16).to_le_bytes());
  for (i, (&kind, ids)) in tree.iter().enumerate() {
    entry(&mut out, 16 + 8 * i, kind, sub as u32 | SUBDIR);
    put(&mut out, sub + 14, &(ids.len() as u16).to_le_bytes());
    for (j, (&id, langs)) in ids.iter().enumerate() {
      entry(&mut out, sub + 16 + 8 * j, id, lang_dir as u32 | SUBDIR);
      put(&mut out, lang_dir + 14, &(langs.len() as u16).to_le_bytes());
      for (k, (&lang, data)) in langs.iter().enumerate() {
        let at = entries + 16 * leaf;
        entry(&mut out, lang_dir + 16 + 8 * k, lang, at as u32);
        let to = rva + out.len() as u32;
        put(&mut out, at, &to.to_le_bytes());
        put(&mut out, at + 4, &(data.len() as u32).to_le_bytes());
        out.extend_from_slice(data);
        out.resize(out.len().next_multiple_of(8), 0);
        leaf += 1;
      }

      lang_dir += dir(langs.len());
    }

    sub += dir(ids.len());
  }

  out
}

/// A directory entry at `at`: the id and where it points.
fn entry(out: &mut [u8], at: usize, id: u16, to: u32) {
  put(out, at, &(id as u32).to_le_bytes());
  put(out, at + 4, &to.to_le_bytes());
}

/// The PE checksum of `data`, whose own field is zero: the 16-bit words' sum, its carries folded
/// in, plus the file's length.
fn checksum(data: &[u8]) -> u32 {
  let mut sum = 0u32;
  for w in data.chunks(2) {
    sum += u16::from_le_bytes([w[0], w.get(1).copied().unwrap_or(0)]) as u32;
    sum = (sum & 0xFFFF) + (sum >> 16);
  }

  ((sum + (sum >> 16)) & 0xFFFF).wrapping_add(data.len() as u32)
}

fn arch(machine: u16) -> String {
  match machine {
    AMD64 => "x64".into(),
    ARM64 => "arm64".into(),
    0x14C => "x86".into(),
    _ => format!("machine {machine:#06x}"),
  }
}

fn put(out: &mut [u8], at: usize, bytes: &[u8]) {
  out[at..at + bytes.len()].copy_from_slice(bytes);
}

fn short() -> io::Error {
  bad("the exe ends inside its headers")
}

fn u16_at(data: &[u8], at: usize) -> io::Result<u16> {
  get(data, at).map(u16::from_le_bytes).ok_or_else(short)
}

fn u32_at(data: &[u8], at: usize) -> io::Result<u32> {
  get(data, at).map(u32::from_le_bytes).ok_or_else(short)
}

#[cfg(test)]
pub(crate) mod tests {

  use super::*;

  /// The resources of the PE32+ file `data`.
  fn read_resources(data: &[u8]) -> io::Result<Resources> {
    let pe = Pe::read(data)?;
    pe.resources(data, |_| false)
  }

  /// A config in `dir` with the `[windows]` table `windows`.
  fn config(dir: &Path, windows: &str) -> Config {
    Config {
      id: "com.example.hello".into(),
      name: "Hello".into(),
      icon: dir.join("icon.png"),
      comment: "Says hello".into(),
      version: "1.2.0-beta.1".into(),
      bin: "hello".into(),
      macos: Default::default(),
      windows: toml::from_str(windows).unwrap(),
      linux: Default::default(),
      publisher: "example".into(),
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
    assert_eq!(nsi(&cfg, Arch::X86_64, true, 42).unwrap(), NSI);

    let arm = nsi(&cfg, Arch::Aarch64, false, 42).unwrap();
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
    let text = nsi(&cfg, Arch::X86_64, false, 1).unwrap();
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
    assert_eq!(tool::hex(&long), "291e9a6c66994949b57ba5e650361e98fc36b1ba");

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
  }

  #[test]
  fn names() {
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

    let mut cfg = config(Path::new("/app"), "");
    let mut quad = |version: &str| {
      cfg.version = version.into();
      quad(&cfg).map_err(|e| e.to_string())
    };

    for (version, want) in cases {
      assert_eq!(quad(version), Ok(want), "{version}");
    }

    let max = "the version 1.65536: Windows' numbers end at 65535";
    assert_eq!(quad("1.65536"), Err(max.into()));
    assert_eq!(quad("v1"), Err("the version v1 starts with no number".into()));
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
    let out = with_resources(&exe(AMD64), AMD64, resources(&cfg, &pngs).unwrap()).unwrap();
    consistent(&out);

    let tree = read_resources(&out).unwrap();
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
    fs::write(&bin, exe(AMD64)).unwrap();
    let out = tmp.join("out");

    let e =
      build(&cfg, &bin, Arch::X86_64, None, &out, OsStr::new("shipo-no-makensis")).unwrap_err();
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
      let e =
        build(&cfg, &bin, Arch::X86_64, None, &out, fake.as_os_str()).unwrap_err().to_string();
      let want = "failed (exit status: 1): warning 6001: Variable \"x\" not referenced\nError: warning treated as error";
      assert!(e.ends_with(want), "{e}");
    }

    let e = setup(&cfg, &bin, Arch::Aarch64, None, &out).unwrap_err();
    assert_eq!(e.to_string(), format!("{}: the exe is for x64, not arm64", bin.display()));
    cfg.name = "A/B".into();
    assert!(
      setup(&cfg, &bin, Arch::X86_64, None, &out)
        .unwrap_err()
        .to_string()
        .contains("no Windows file name")
    );
    cfg.name = "Hello".into();

    // The setup itself where makensis is, as on the release host.
    let found = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
      .any(|d| d.join("makensis").is_file());
    if !found {
      return;
    }

    let made = setup(&cfg, &bin, Arch::X86_64, Some("v1.2.0"), &out).unwrap();
    assert_eq!(made, out.join("Hello-v1.2.0-windows-x64-setup.exe"));
    let data = fs::read(&made).unwrap();
    assert!(data.starts_with(b"MZ") && data.len() > 30_000, "{}", data.len());
    assert_eq!(fs::read_dir(&out).unwrap().count(), 1);
  }

  /// A PE32+ exe for `machine` as a linker lays it out: 0x400 bytes, the headers in the first
  /// 0x200, one `.text` section at 0x1000 in the second; its last 5 bytes are `ret` 4 times
  /// after a `nop`, so it is not all zeros.
  pub fn exe(machine: u16) -> Vec<u8> {
    let mut pe = vec![0; 0x400];
    put(&mut pe, 0, b"MZ");
    put(&mut pe, 0x3C, &0x40u32.to_le_bytes());
    put(&mut pe, 0x40, b"PE\0\0");
    put(&mut pe, 0x44, &machine.to_le_bytes());
    put(&mut pe, 0x46, &1u16.to_le_bytes());
    put(&mut pe, 0x54, &240u16.to_le_bytes());
    put(&mut pe, 0x56, &0x22u16.to_le_bytes());
    let opt = [(0, 0x20Bu32, 2), (16, 0x1000, 4), (32, 0x1000, 4), (36, 0x200, 4)];
    let opt = opt.into_iter().chain([(56, 0x2000, 4), (60, 0x200, 4), (108, 16, 4)]);
    for (at, x, n) in opt {
      put(&mut pe, 0x58 + at, &x.to_le_bytes()[..n]);
    }

    put(&mut pe, 0x148, b".text\0\0\0");
    for (i, x) in [16u32, 0x1000, 0x200, 0x200, 0, 0, 0, 0x6000_0020].iter().enumerate() {
      put(&mut pe, 0x150 + 4 * i, &x.to_le_bytes());
    }

    put(&mut pe, 0x200, &[0x90, 0xC3, 0xC3, 0xC3, 0xC3]);
    pe
  }

  /// Checks that the headers of `data` agree with each other: the sections aligned, in order and
  /// apart in memory and in the file, SizeOfImage their end, the resource directory in one, and
  /// the checksum the file's.
  pub fn consistent(data: &[u8]) {
    let pe = Pe::read(data).unwrap();
    let (fa, sa) = (pe.file_align, pe.section_align);
    let mut end = (pe.headers_size.next_multiple_of(fa), pe.headers_size);
    for s in &pe.sections {
      assert_eq!((s.va % sa, s.raw_at % fa, s.raw_size % fa), (0, 0, 0));
      assert!(s.va >= end.1.next_multiple_of(sa), "{:#x} overlaps", s.va);
      assert!(s.raw_at >= end.0 && (s.raw_at + s.raw_size) as usize <= data.len());
      end = (s.raw_at + s.raw_size, s.va + s.vsize);
    }

    assert_eq!(pe.image_size, end.1.next_multiple_of(sa));
    let dir = [16, 20].map(|at| u32_at(data, pe.opt + DIRS + at).unwrap());
    pe.slice(data, dir[0], dir[1]).unwrap();
    let mut zeroed = data.to_vec();
    put(&mut zeroed, pe.opt + 64, &[0; 4]);
    assert_eq!(u32_at(data, pe.opt + 64).unwrap(), checksum(&zeroed));
  }

  /// A tree of one resource per `(type, id, data)`, each in [LANG].
  fn tree<const N: usize>(items: [(u16, u16, &[u8]); N]) -> Resources {
    let mut tree = Resources::new();
    for (kind, id, data) in items {
      tree.entry(kind).or_default().entry(id).or_default().insert(LANG, data.to_vec());
    }

    tree
  }

  #[test]
  fn section_added() {
    let ours = tree([(RT_MANIFEST, 1, b"<m/>"), (RT_ICON, 1, b"a"), (RT_ICON, 2, b"bc")]);
    let out = with_resources(&exe(AMD64), AMD64, ours.clone()).unwrap();
    consistent(&out);
    assert_eq!(read_resources(&out).unwrap(), ours);

    let pe = Pe::read(&out).unwrap();
    assert_eq!(pe.sections.len(), 2);
    assert_eq!(&out[0x170..0x178], b".rsrc\0\0\0");
    let rsrc = &pe.sections[1];
    assert_eq!(
      (rsrc.va, rsrc.raw_at, rsrc.raw_size, pe.image_size),
      (0x2000, 0x400, 0x200, 0x3000)
    );
    assert_eq!(u32_at(&out, 0x170 + 36).unwrap(), RSRC);
    assert_eq!(u32_at(&out, pe.opt + DIRS + 16).unwrap(), 0x2000);
    assert_eq!(u32_at(&out, pe.opt + DIRS + 20).unwrap(), rsrc.vsize);
    assert_eq!(u32_at(&out, pe.opt + 8).unwrap(), 0x200);
    assert_eq!(out.len(), 0x600);
    assert_eq!(out[0x200..0x400], exe(AMD64)[0x200..0x400]);
  }

  #[test]
  fn directory_layout() {
    // The root, a directory per type and per id, sorted by number, then the data at 8 bytes.
    let data =
      rsrc(&tree([(RT_MANIFEST, 1, b"<m/>"), (RT_ICON, 2, b"bc"), (RT_ICON, 1, b"a")]), 0x5000);
    let ids = |at: usize| entries(&data, at, true, &mut 100).unwrap();
    assert_eq!(ids(0), [(RT_ICON, 32), (RT_MANIFEST, 64)]);
    assert_eq!(ids(32), [(1, 88), (2, 112)]);
    assert_eq!(ids(64), [(1, 136)]);
    assert_eq!(entries(&data, 88, false, &mut 100).unwrap(), [(LANG, 160)]);
    assert_eq!(entries(&data, 136, false, &mut 100).unwrap(), [(LANG, 192)]);
    let (rva, size) = (u32_at(&data, 192).unwrap(), u32_at(&data, 196).unwrap());
    assert_eq!((rva, size), (0x5000 + 224, 4));
    assert_eq!(&data[208..209], b"a");
    assert_eq!(&data[224..], b"<m/>\0\0\0\0");
  }

  #[test]
  fn others_kept() {
    // A first edit's resources stand for the linker's: the types written again are replaced.
    let first = tree([(6, 1, b"strings"), (RT_MANIFEST, 1, b"old"), (RT_MANIFEST, 2, b"old")]);
    let exe = with_resources(&exe(ARM64), ARM64, first).unwrap();
    let out = with_resources(&exe, ARM64, tree([(RT_MANIFEST, 1, b"new")])).unwrap();
    consistent(&out);
    assert_eq!(read_resources(&out).unwrap(), tree([(6, 1, b"strings"), (RT_MANIFEST, 1, b"new")]));
    assert_eq!(Pe::read(&out).unwrap().sections.len(), 3);
  }

  #[test]
  fn refused() {
    let ok = tree([(RT_MANIFEST, 1, b"<m/>")]);
    let err =
      |data: &[u8], machine| with_resources(data, machine, ok.clone()).unwrap_err().to_string();
    assert_eq!(err(&exe(AMD64), ARM64), "the exe is for x64, not arm64");
    assert_eq!(err(b"ELF", AMD64), "not an exe: no MZ header");

    let mut pe32 = exe(AMD64);
    put(&mut pe32, 0x58, &0x10Bu16.to_le_bytes());
    assert_eq!(err(&pe32, AMD64), "not a 64-bit exe (PE32+)");

    let mut signed = exe(AMD64);
    put(&mut signed, 0x58 + DIRS + 36, &8u32.to_le_bytes());
    assert_eq!(err(&signed, AMD64), "the exe is signed: its resources go in before its signature");

    let mut full = exe(AMD64);
    put(&mut full, 0x58 + 60, &0x170u32.to_le_bytes());
    assert_eq!(err(&full, AMD64), "its headers have no room for one more section header");
    let mut taken = exe(AMD64);
    put(&mut taken, 0x170, b".x");
    assert_eq!(err(&taken, AMD64), "its headers have no room for one more section header");

    let mut named = with_resources(&exe(AMD64), AMD64, ok.clone()).unwrap();
    put(&mut named, 0x400 + 12, &1u16.to_le_bytes());
    assert_eq!(err(&named, AMD64), "a resource named by a string, which the packer can't keep");

    let mut small = exe(AMD64);
    put(&mut small, 0x58 + 36, &0x100u32.to_le_bytes());
    let want = "its file alignment 0x100 or section alignment 0x1000 is not an exe's: a power of \
                two, 512 to 64K and at least 4K";
    assert_eq!(err(&small, AMD64), want);
    let mut big = exe(AMD64);
    put(&mut big, 0x58 + 36, &0x20000u32.to_le_bytes());
    assert!(err(&big, AMD64).starts_with("its file alignment 0x20000 or"));
    let mut driver = exe(AMD64);
    put(&mut driver, 0x58 + 32, &0x200u32.to_le_bytes());
    assert!(err(&driver, AMD64).contains("section alignment 0x200 is not"));

    // Cut anywhere, it is an error, never a panic.
    let whole = with_resources(&exe(AMD64), AMD64, ok.clone()).unwrap();
    for n in 0..0x420 {
      assert!(with_resources(&whole[..n], AMD64, ok.clone()).is_err(), "{n}");
    }
  }

  #[test]
  fn named_replaced() {
    // An icon group named by a string, as embed-resource's `APP ICON`: fine when replaced.
    let first = tree([(6, 1, b"strings"), (RT_GROUP_ICON, 1, b"group")]);
    let mut exe = with_resources(&exe(AMD64), AMD64, first).unwrap();
    // The root holds 2 types, so the group's directory follows type 6's, at 32 + 24.
    assert_eq!(entries(&exe[0x400..], 0, true, &mut 100).unwrap()[1], (RT_GROUP_ICON, 56));
    put(&mut exe, 0x400 + 56 + 12, &1u16.to_le_bytes());
    put(&mut exe, 0x400 + 56 + 14, &0u16.to_le_bytes());
    put(&mut exe, 0x400 + 56 + 16, &(SUBDIR | 0x100).to_le_bytes());

    let out = with_resources(&exe, AMD64, tree([(RT_GROUP_ICON, 1, b"ours")])).unwrap();
    assert_eq!(
      read_resources(&out).unwrap(),
      tree([(6, 1, b"strings"), (RT_GROUP_ICON, 1, b"ours")])
    );
    let e = with_resources(&exe, AMD64, tree([(RT_MANIFEST, 1, b"<m/>")])).unwrap_err();
    assert_eq!(e.to_string(), "a resource named by a string, which the packer can't keep");
  }

  #[test]
  fn loops_end() {
    let start = std::time::Instant::now();
    let read = |_, _| Ok(&b"x"[..]);
    let looped = "its resource directory points into itself";

    // The root points at itself as each type's directory, so each id's too.
    let mut dir = vec![0; 16 + 8 * 1000];
    put(&mut dir, 14, &1000u16.to_le_bytes());
    for i in 0..1000 {
      entry(&mut dir, 16 + 8 * i, i as u16 + 1, SUBDIR);
    }

    let e = walk(&dir, |_| false, 1 << 20, read).unwrap_err().to_string();
    assert!(e == looped || e == "its resource directory is malformed", "{e}");

    // Three levels of 1000 entries, each pointing at the next level's one directory: a billion
    // leaves, all at one data entry.
    let level = 16 + 8 * 1000;
    let mut dir = vec![0; 3 * level + 16];
    for (n, sub) in [(0, SUBDIR), (1, SUBDIR), (2, 0)] {
      put(&mut dir, n * level + 14, &1000u16.to_le_bytes());
      for i in 0..1000 {
        let to = if sub == 0 { 3 * level } else { (n + 1) * level } as u32;
        entry(&mut dir, n * level + 16 + 8 * i, i as u16, to | sub);
      }
    }

    put(&mut dir, 3 * level + 4, &1u32.to_le_bytes());
    assert_eq!(walk(&dir, |_| false, 1 << 20, read).unwrap_err().to_string(), looped);
    assert!(start.elapsed() < std::time::Duration::from_secs(1));
  }

  #[test]
  fn sum() {
    assert_eq!(checksum(&[1, 0, 2, 0]), 3 + 4);
    assert_eq!(checksum(&[0xFF, 0xFF, 2, 0, 7]), 2 + 7 + 5);
    assert_eq!(checksum(&[]), 0);
  }
}
