// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! The macOS app: its Info.plist from the config and the minimum macOS its binary was built for,
//! and the `.app` bundle with its icon, signed as [crate::sign] does it. Property lists are the
//! `plist` crate's; the app's own keys come from TOML.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use std::{fs, io};

pub use plist::Value as Plist;
use toml::value::{Datetime, Offset};

use crate::config::Config;
use crate::image::{ICNS, Image, icns, icons};
use crate::tool::get;
use crate::{at, bad, sign, tool};

// MARK: Info.plist

/// The Info.plist's keys `macos.plist` can't set, and where each comes from: the updater takes a
/// release by its bundle id and starts it by its executable's name.
const RESERVED: [(&str, &str); 4] = [
  ("CFBundleExecutable", "the binary's name"),
  ("CFBundleIdentifier", "the config's `id`"),
  ("CFBundleIconFile", "the config's `name`, the icon's file"),
  ("CFBundlePackageType", "APPL, an app's"),
];

/// The Info.plist of the app, which needs macOS `minos`: the bundle from the config, then the
/// `macos.plist` keys over it, all sorted.
fn info(cfg: &Config, minos: &str) -> io::Result<Plist> {
  // Both versions take numbers and dots only: 1.2.0 of 1.2.0-beta.1.
  let version = cfg.numeric()?;
  let mut keys = BTreeMap::from([
    ("CFBundleIdentifier", Plist::from(cfg.id.as_str())),
    ("CFBundleName", cfg.name.as_str().into()),
    ("CFBundleDisplayName", cfg.name.as_str().into()),
    ("CFBundleExecutable", cfg.bin.as_str().into()),
    ("CFBundleIconFile", cfg.name.as_str().into()),
    ("CFBundlePackageType", "APPL".into()),
    ("CFBundleShortVersionString", version.into()),
    ("CFBundleVersion", version.into()),
    ("CFBundleInfoDictionaryVersion", "6.0".into()),
    ("CFBundleDevelopmentRegion", "en".into()),
    ("LSMinimumSystemVersion", minos.into()),
    ("NSHighResolutionCapable", true.into()),
  ]);

  if cfg.macos.accessory {
    keys.insert("LSUIElement", true.into());
    keys.insert("NSSupportsAutomaticTermination", false.into());
    keys.insert("NSSupportsSuddenTermination", false.into());
  }

  let mut keys: BTreeMap<String, Plist> = keys.into_iter().map(|(k, v)| (k.into(), v)).collect();
  for (key, value) in &cfg.macos.plist {
    let what = format!("[package.metadata.shipo.macos] plist.{key}");
    if let Some((_, from)) = RESERVED.iter().find(|(k, _)| k == key) {
      return Err(bad(format!("{what} can't be set: it is {from}")));
    }

    keys.insert(key.clone(), toml_plist(value, &what)?);
  }

  Ok(Plist::Dictionary(keys.into_iter().collect()))
}

// MARK: Mach-O

/// The macOS the Mach-O binary `data` needs, as `14.0` or `10.13.4`: its `LC_BUILD_VERSION`'s minos
/// for macOS, else its `LC_VERSION_MIN_MACOSX`'s version (xnu's `loader.h`). A universal binary
/// needs the lowest of its 64-bit slices': a Mac runs only the slice of its own arch, and Apple
/// silicon, whose slice asks macOS 11 at least, comes with that.
fn minos(data: &[u8]) -> io::Result<String> {
  let version = match data.get(..4) {
    Some([0xCA, 0xFE, 0xBA, wide @ (0xBE | 0xBF)]) => {
      let wide = *wide == 0xBF;
      let count = be32(data, 4)? as usize;
      let mut lowest = None;
      for i in 0..count {
        // fat_arch: cputype, cpusubtype, offset, size, align; fat_arch_64's offset and size are
        // 64-bit, then a reserved word.
        let (offset, size) = match wide {
          true => (be64(data, 8 + i * 32 + 8)?, be64(data, 8 + i * 32 + 16)?),
          false => (be32(data, 8 + i * 20 + 8)? as u64, be32(data, 8 + i * 20 + 12)? as u64),
        };

        let end = offset.checked_add(size).and_then(|end| usize::try_from(end).ok());
        let slice = usize::try_from(offset).ok().zip(end);
        let slice = slice.and_then(|(a, b)| data.get(a..b)).ok_or_else(truncated)?;
        if slice.get(..4) != Some(&MH_MAGIC_64) {
          continue;
        }

        let version = thin(slice)?;
        lowest = Some(lowest.map_or(version, |v: u32| v.min(version)));
      }

      lowest.ok_or_else(|| bad("a universal binary with no 64-bit slice"))?
    }
    Some(magic) if magic == MH_MAGIC_64 => thin(data)?,
    _ => return Err(bad("not a 64-bit Mach-O binary")),
  };

  let (major, minor, patch) = (version >> 16, version >> 8 & 0xFF, version & 0xFF);
  Ok(match patch {
    0 => format!("{major}.{minor}"),
    _ => format!("{major}.{minor}.{patch}"),
  })
}

/// `MH_MAGIC_64` as a little-endian file holds it.
const MH_MAGIC_64: [u8; 4] = [0xCF, 0xFA, 0xED, 0xFE];

/// The minos of a thin 64-bit Mach-O, as `loader.h` packs it: `xxxx.yy.zz` in nibbles.
fn thin(data: &[u8]) -> io::Result<u32> {
  const LC_VERSION_MIN_MACOSX: u32 = 0x24;
  const LC_BUILD_VERSION: u32 = 0x32;
  const PLATFORM_MACOS: u32 = 1;

  let (mut at, mut older) = (32, None);
  for _ in 0..le32(data, 16)? {
    let (cmd, size) = (le32(data, at)?, le32(data, at + 4)? as usize);
    if size < 8 {
      return Err(bad("a Mach-O load command of less than 8 bytes"));
    }

    if cmd == LC_BUILD_VERSION && le32(data, at + 8)? == PLATFORM_MACOS {
      return le32(data, at + 12);
    }

    if cmd == LC_VERSION_MIN_MACOSX {
      older = Some(le32(data, at + 8)?);
    }

    at += size;
  }

  older.ok_or_else(|| bad("no LC_BUILD_VERSION or LC_VERSION_MIN_MACOSX for macOS"))
}

fn le32(data: &[u8], at: usize) -> io::Result<u32> {
  get(data, at).map(u32::from_le_bytes).ok_or_else(truncated)
}

fn be32(data: &[u8], at: usize) -> io::Result<u32> {
  get(data, at).map(u32::from_be_bytes).ok_or_else(truncated)
}

fn be64(data: &[u8], at: usize) -> io::Result<u64> {
  get(data, at).map(u64::from_be_bytes).ok_or_else(truncated)
}

fn truncated() -> io::Error {
  bad("a truncated Mach-O binary")
}

// MARK: Bundle

/// `<target>/<Name>.app` of the binary `bin`, signed with the hardened runtime and
/// `macos.entitlements` as [sign::kept] keeps it: on a Mac by the keychain's identity, elsewhere by
/// rcodesign with `APPLE_CERTIFICATE`, else ad hoc, which a release (a `tag`) refuses. Gives the
/// bundle and who signed it.
pub fn app(cfg: &Config, bin: &Path, tag: Option<&str>) -> io::Result<(PathBuf, String)> {
  let entitlements = cfg.macos.entitlements.as_deref();
  if let Some(path) = entitlements {
    allowed(path)?;
  }

  let signed = !cfg!(target_os = "macos") && sign::secrets(&sign::CERT, tag)?;
  sign::kept(bundle(cfg, bin)?, |bundle| match cfg!(target_os = "macos") {
    true => {
      let id = sign::identity(cfg, tag)?;
      sign::codesign(bundle, &sign::codesign_args(&id.id, id.developer, entitlements))?;
      Ok(id.name)
    }
    false => sign::sign_elsewhere(cfg, bundle, signed, entitlements),
  })
}

/// Checks the entitlements' plist at `path`: the App Sandbox is refused, as a sandboxed app can't
/// replace its own bundle, so the updater couldn't install a release.
fn allowed(path: &Path) -> io::Result<()> {
  let what = format!("macos.entitlements: {}", path.display());
  if !path.is_file() {
    return Err(bad(format!("{what}: no file")));
  }

  let plist = Plist::from_file(path).map_err(|e| bad(format!("{what}: {e}")))?;
  let sandbox = plist.as_dictionary().and_then(|d| d.get("com.apple.security.app-sandbox"));
  match sandbox.and_then(Plist::as_boolean) {
    Some(true) => Err(bad(format!(
      "{what}: com.apple.security.app-sandbox is on, and the updater can't replace a sandboxed app"
    ))),
    _ => Ok(()),
  }
}

/// `<target>/<Name>.app` of the binary `bin`, unsigned, in place of one there: the binary
/// (0755, whatever its mode: GitHub's artifacts lose it), the `.icns`, `PkgInfo` and the
/// Info.plist. All is made before the old bundle goes.
pub fn bundle(cfg: &Config, bin: &Path) -> io::Result<PathBuf> {
  let exe = tool::read(bin)?;
  let plist = xml(&info(cfg, &minos(&exe).map_err(at(bin))?)?)?;
  let sizes: Vec<usize> = ICNS.iter().map(|&(_, n)| n).collect();
  let icon = icns(&icons(&Image::read(&cfg.icon)?, &sizes, false)?)?;

  let bundle = cfg.target.join(format!("{}.app", cfg.name));
  match fs::remove_dir_all(&bundle) {
    Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
    _ => {}
  }

  let contents = bundle.join("Contents");
  fs::create_dir_all(contents.join("MacOS"))?;
  fs::create_dir_all(contents.join("Resources"))?;
  let exe_path = contents.join("MacOS").join(&cfg.bin);
  fs::write(&exe_path, exe)?;
  tool::set_mode(&exe_path, 0o755)?;
  fs::write(contents.join("Resources").join(format!("{}.icns", cfg.name)), icon)?;
  fs::write(contents.join("PkgInfo"), "APPL????")?;
  fs::write(contents.join("Info.plist"), plist)?;
  Ok(bundle)
}

// MARK: Property lists

/// A dict of `items`, sorted by key.
pub fn dict<const N: usize>(items: [(&str, Plist); N]) -> Plist {
  let items = BTreeMap::from(items);
  Plist::Dictionary(items.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

/// `value` as an XML plist.
pub fn xml(value: &Plist) -> io::Result<Vec<u8>> {
  let mut out = vec![];
  value.to_writer_xml(&mut out).map_err(io::Error::other)?;
  out.push(b'\n');
  Ok(out)
}

/// `value` as a binary plist, `bplist00`.
pub fn binary(value: &Plist) -> io::Result<Vec<u8>> {
  let mut out = vec![];
  value.to_writer_binary(&mut out).map_err(io::Error::other)?;
  Ok(out)
}

/// The plist of a TOML `value`, `what` naming it in the errors. A datetime with an offset is a
/// date, in whole seconds; one without (a local datetime, date or time) is no instant, so it is its
/// TOML text as a string. A float is finite, and a string or key holds no character a plist's XML
/// can't.
fn toml_plist(value: &toml::Value, what: &str) -> io::Result<Plist> {
  use toml::Value as V;

  Ok(match value {
    V::String(s) => Plist::String(text(s, what)?),
    V::Integer(n) => Plist::from(*n),
    V::Float(x) if x.is_finite() => Plist::Real(*x),
    V::Float(x) => return Err(bad(format!("{what} is {x}, which a plist has no real for"))),
    V::Boolean(b) => Plist::Boolean(*b),
    V::Datetime(dt) => {
      instant(dt).map_or_else(|| Plist::String(dt.to_string()), |t| Plist::Date(t.into()))
    }
    V::Array(items) => {
      let items = items.iter().enumerate().map(|(i, x)| toml_plist(x, &format!("{what}[{i}]")));
      Plist::Array(items.collect::<io::Result<_>>()?)
    }
    V::Table(table) => {
      let items =
        table.iter().map(|(k, x)| Ok((text(k, what)?, toml_plist(x, &format!("{what}.{k}"))?)));
      // Sorted whatever order `toml` keeps (`preserve_order` keeps the file's).
      let items: BTreeMap<_, _> = items.collect::<io::Result<_>>()?;
      Plist::Dictionary(items.into_iter().collect())
    }
  })
}

/// `s`; an error naming `what` if it holds a character a plist's XML can't: a control character
/// other than tab, LF and CR, U+FFFE or U+FFFF.
fn text(s: &str, what: &str) -> io::Result<String> {
  let odd = |c: char| c < ' ' && !"\t\n\r".contains(c) || c == '\u{FFFE}' || c == '\u{FFFF}';
  match s.chars().find(|&c| odd(c)) {
    Some(c) => Err(bad(format!("{what} holds {c:?}, which a plist's XML can't"))),
    None => Ok(s.to_string()),
  }
}

/// The instant of a TOML datetime with a date, a time and an offset, in whole seconds.
fn instant(dt: &Datetime) -> Option<SystemTime> {
  let (Some(date), Some(time), Some(offset)) = (dt.date, dt.time, dt.offset) else {
    return None;
  };

  let minutes = match offset {
    Offset::Z => 0,
    Offset::Custom { minutes } => minutes as i64,
  };

  let days = days(date.year as i64, date.month as i64, date.day as i64);
  let secs = time.hour as i64 * 3600 + time.minute as i64 * 60 + time.second.unwrap_or(0) as i64;
  let secs = days * 86400 + secs - minutes * 60;
  let epoch = SystemTime::UNIX_EPOCH;
  match u64::try_from(secs) {
    Ok(secs) => epoch.checked_add(Duration::from_secs(secs)),
    Err(_) => epoch.checked_sub(Duration::from_secs(secs.unsigned_abs())),
  }
}

/// The days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's `days_from_civil`).
fn days(y: i64, m: i64, d: i64) -> i64 {
  let y = if m <= 2 { y - 1 } else { y };
  let era = y.div_euclid(400);
  let yoe = y - era * 400;
  let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
  let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
  era * 146_097 + doe - 719_468
}

#[cfg(test)]
pub(crate) mod tests {
  #[cfg(target_os = "macos")]
  use std::env;
  #[cfg(target_os = "macos")]
  use std::process::Command;

  use super::*;
  use crate::tool::Temp;
  #[cfg(target_os = "macos")]
  use crate::tool::run;

  /// A config in `dir` with the `[macos]` table `macos`.
  pub fn config(dir: &Path, macos: &str) -> Config {
    let mut macos: crate::config::Macos = toml::from_str(macos).unwrap();
    macos.entitlements = macos.entitlements.map(|path| dir.join(path));
    Config {
      id: "com.example.hello".into(),
      name: "Hello".into(),
      icon: dir.join("icon.png"),
      comment: String::new(),
      version: "1.2.0-beta.1".into(),
      bin: "hello".into(),
      macos,
      windows: Default::default(),
      linux: Default::default(),
      publisher: String::new(),
      authors: vec![],
      repository: String::new(),
      root: dir.to_path_buf(),
      workspace: dir.to_path_buf(),
      target: dir.join("target"),
    }
  }

  const MACOS: &str = r#"
accessory = true
entitlements = "hello.entitlements"
[plist]
NSMicrophoneUsageDescription = "Hello hears you."
CFBundleName = "Hello!"
LSEnvironment = { HELLO_MODE = "quiet" }
"#;

  const INFO: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleDevelopmentRegion</key>
	<string>en</string>
	<key>CFBundleDisplayName</key>
	<string>Hello</string>
	<key>CFBundleExecutable</key>
	<string>hello</string>
	<key>CFBundleIconFile</key>
	<string>Hello</string>
	<key>CFBundleIdentifier</key>
	<string>com.example.hello</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleName</key>
	<string>Hello!</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>1.2.0</string>
	<key>CFBundleVersion</key>
	<string>1.2.0</string>
	<key>LSEnvironment</key>
	<dict>
		<key>HELLO_MODE</key>
		<string>quiet</string>
	</dict>
	<key>LSMinimumSystemVersion</key>
	<string>11.0</string>
	<key>LSUIElement</key>
	<true/>
	<key>NSHighResolutionCapable</key>
	<true/>
	<key>NSMicrophoneUsageDescription</key>
	<string>Hello hears you.</string>
	<key>NSSupportsAutomaticTermination</key>
	<false/>
	<key>NSSupportsSuddenTermination</key>
	<false/>
</dict>
</plist>
"#;

  #[test]
  fn info_plist() {
    let cfg = config(Path::new("/app"), MACOS);
    assert_eq!(String::from_utf8(xml(&info(&cfg, "11.0").unwrap()).unwrap()).unwrap(), INFO);

    let plain = info(&config(Path::new("/app"), ""), "11.0").unwrap();
    let keys = plain.as_dictionary().unwrap();
    assert_eq!(keys.len(), 12);
    assert!(!keys.contains_key("LSUIElement"));
  }

  #[test]
  fn info_errors() {
    let e = info(&config(Path::new("/app"), "plist.X = [nan]"), "11.0").unwrap_err();
    let want = "[package.metadata.shipo.macos] plist.X[0] is NaN, which a plist has no real for";
    assert_eq!(e.to_string(), want);

    for key in
      ["CFBundleExecutable", "CFBundleIdentifier", "CFBundleIconFile", "CFBundlePackageType"]
    {
      let e =
        info(&config(Path::new("/app"), &format!("plist.{key} = \"x\"")), "11.0").unwrap_err();
      let e = e.to_string();
      assert!(
        e.starts_with(&format!("[package.metadata.shipo.macos] plist.{key} can't be set: it is ")),
        "{e}"
      );
    }

    let mut cfg = config(Path::new("/app"), "");
    cfg.version = "v1.0".into();
    assert_eq!(
      info(&cfg, "11.0").unwrap_err().to_string(),
      "the version v1.0 starts with no number"
    );
  }

  /// A thin 64-bit Mach-O of the load commands `cmds`, each `(cmd, its words after cmdsize)`.
  fn macho(cmds: &[(u32, &[u32])]) -> Vec<u8> {
    let body: Vec<u8> = cmds
      .iter()
      .flat_map(|(cmd, words)| {
        let mut out = vec![*cmd, 8 + 4 * words.len() as u32];
        out.extend_from_slice(words);
        out.into_iter().flat_map(u32::to_le_bytes)
      })
      .collect();

    let head = [0xFEEDFACF, 0x0100000C, 0, 2, cmds.len() as u32, body.len() as u32, 0, 0];
    head.into_iter().flat_map(u32::to_le_bytes).chain(body).collect()
  }

  /// A universal binary of `slices`, with 64-bit offsets if `wide`.
  fn fat(slices: &[Vec<u8>], wide: bool) -> Vec<u8> {
    let entry = if wide { 32 } else { 20 };
    let mut out = [0xCA, 0xFE, 0xBA, if wide { 0xBF } else { 0xBE }].to_vec();
    out.extend_from_slice(&(slices.len() as u32).to_be_bytes());
    let mut at = 8 + entry * slices.len();
    for slice in slices {
      out.extend_from_slice(&[0; 8]);
      match wide {
        true => [at as u64, slice.len() as u64].iter().for_each(|n| out.extend(n.to_be_bytes())),
        false => [at as u32, slice.len() as u32].iter().for_each(|n| out.extend(n.to_be_bytes())),
      }

      out.extend_from_slice(&[0; 4]);
      if wide {
        out.extend_from_slice(&[0; 4]);
      }

      at += slice.len();
    }

    slices.iter().for_each(|s| out.extend_from_slice(s));
    out
  }

  #[test]
  fn minos_headers() {
    let build = |platform, version| (0x32, vec![platform, version, 0, 0]);
    let (ios, mac14, mac10) = (build(2, 0x11_00_00), build(1, 0x0E_00_00), build(1, 0x0A_0D_04));
    let older = (0x24, vec![0x0A_0C_00, 0]);
    let segment = (0x19, vec![0; 16]);
    let of = |cmds: &[&(u32, Vec<u32>)]| {
      macho(&cmds.iter().map(|(c, w)| (*c, w.as_slice())).collect::<Vec<_>>())
    };

    let cases = [
      (of(&[&segment, &mac14]), "14.0"),
      (of(&[&mac10]), "10.13.4"),
      (of(&[&ios, &older]), "10.12"),
      (of(&[&older, &mac14]), "14.0"),
      (fat(&[of(&[&mac14]), of(&[&mac10])], false), "10.13.4"),
      (fat(&[of(&[&mac10]), of(&[&mac14])], true), "10.13.4"),
      (fat(&[b"\xce\xfa\xed\xfe".to_vec(), of(&[&mac14])], false), "14.0"),
    ];

    for (i, (data, want)) in cases.iter().enumerate() {
      assert_eq!(minos(data).unwrap(), *want, "case {i}");
    }

    let mut cut = of(&[&mac14]);
    cut.truncate(cut.len() - 10);
    // A load command whose cmdsize says 4.
    let mut short = of(&[&segment]);
    short[36] = 4;
    let errors = [
      (b"\x7fELF\x02\x01\x01\0".to_vec(), "not a 64-bit Mach-O binary"),
      (of(&[&ios]), "no LC_BUILD_VERSION or LC_VERSION_MIN_MACOSX for macOS"),
      (cut, "a truncated Mach-O binary"),
      (short, "a Mach-O load command of less than 8 bytes"),
      (fat(&[vec![0; 40]], false), "a universal binary with no 64-bit slice"),
      (fat(&[of(&[&mac14])], false)[..30].to_vec(), "a truncated Mach-O binary"),
    ];

    for (data, want) in errors {
      assert_eq!(minos(&data).unwrap_err().to_string(), want);
    }
  }

  /// The lowest `minos` `otool -l` gives for `path`, of each slice.
  #[cfg(target_os = "macos")]
  fn otool(path: &Path) -> String {
    let out = run(Command::new("otool").args(["-l", "-arch", "all"]).arg(path)).unwrap();
    let out = String::from_utf8(out).unwrap();
    let parts = |v: &str| v.split('.').map(|p| p.parse::<u32>().unwrap()).collect::<Vec<_>>();
    let all = out.lines().filter_map(|ln| ln.trim().strip_prefix("minos ")).map(str::to_string);
    all.min_by_key(|v| parts(v)).unwrap()
  }

  #[cfg(target_os = "macos")]
  #[test]
  fn minos_binaries() {
    for path in [env::current_exe().unwrap(), PathBuf::from("/bin/ls")] {
      assert_eq!(minos(&fs::read(&path).unwrap()).unwrap(), otool(&path), "{}", path.display());
    }
  }

  /// A 1024 px icon at `path`.
  fn icon(path: &Path) {
    let px = (0..1024 * 1024).flat_map(|i| [(i % 256) as u8, (i / 4096) as u8, 128, 255]).collect();
    fs::write(path, Image { w: 1024, h: 1024, px }.encode().unwrap()).unwrap();
  }

  /// A test app in `dir`: its icon and entitlements, and the config of [MACOS].
  pub fn test_app(dir: &Path) -> Config {
    icon(&dir.join("icon.png"));
    let entitlements = dict([("com.apple.security.device.audio-input", true.into())]);
    fs::write(dir.join("hello.entitlements"), xml(&entitlements).unwrap()).unwrap();
    config(dir, MACOS)
  }

  #[test]
  fn bundle_files() {
    let tmp = Temp::new().unwrap();
    let cfg = test_app(&tmp);
    let bin = tmp.join("hello");
    fs::write(&bin, macho(&[(0x32, &[1, 0x0B_00_00, 0, 0])])).unwrap();
    // An old bundle goes.
    fs::create_dir_all(tmp.join("target/Hello.app/Contents/old")).unwrap();

    let app = bundle(&cfg, &bin).unwrap();
    assert_eq!(app, tmp.join("target/Hello.app"));
    let mut files: Vec<_> =
      walk(&app).into_iter().map(|p| p.strip_prefix(&app).unwrap().to_path_buf()).collect();
    files.sort();
    let want = [
      "Contents/Info.plist",
      "Contents/MacOS/hello",
      "Contents/PkgInfo",
      "Contents/Resources/Hello.icns",
    ];
    assert_eq!(files, want.map(PathBuf::from));
    assert_eq!(fs::read_to_string(app.join("Contents/Info.plist")).unwrap(), INFO);
    assert_eq!(fs::read(app.join("Contents/PkgInfo")).unwrap(), b"APPL????");
    #[cfg(unix)]
    {
      use std::os::unix::fs::PermissionsExt;
      let mode = fs::metadata(app.join("Contents/MacOS/hello")).unwrap().permissions().mode();
      assert_eq!(mode & 0o777, 0o755);
    }

    assert_eq!(&fs::read(app.join("Contents/Resources/Hello.icns")).unwrap()[..4], b"icns");

    let e = bundle(&cfg, &tmp.join("hello.entitlements")).unwrap_err().to_string();
    assert!(e.ends_with("hello.entitlements: not a 64-bit Mach-O binary"), "{e}");
  }

  #[test]
  fn a_sandboxed_app_is_refused() {
    let tmp = Temp::new().unwrap();
    let cfg = test_app(&tmp);
    let path = tmp.join("hello.entitlements");
    let what = format!("macos.entitlements: {}", path.display());
    let write = |on: bool| {
      let plist = dict([("com.apple.security.app-sandbox", on.into())]);
      fs::write(&path, xml(&plist).unwrap()).unwrap();
    };

    allowed(&path).unwrap();
    write(false);
    allowed(&path).unwrap();
    write(true);
    let e = app(&cfg, &tmp.join("hello"), None).unwrap_err().to_string();
    let why = "com.apple.security.app-sandbox is on, and the updater can't replace a sandboxed app";
    assert_eq!(e, format!("{what}: {why}"));

    fs::write(&path, "not a plist").unwrap();
    assert!(allowed(&path).unwrap_err().to_string().starts_with(&what));
    fs::remove_file(&path).unwrap();
    let e = app(&cfg, &tmp.join("hello"), None).unwrap_err().to_string();
    assert_eq!(e, format!("{what}: no file"));
  }

  /// The files under `dir`, at any depth.
  fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    for entry in fs::read_dir(dir).unwrap() {
      let path = entry.unwrap().path();
      match path.is_dir() {
        true => out.extend(walk(&path)),
        false => out.push(path),
      }
    }

    out
  }

  /// A plist of every kind, and the Python literal `plistlib` reads as the same, its date naive in
  /// UTC as any Python 3 reads it.
  fn sample() -> (Plist, &'static str) {
    let toml: toml::Table = toml::from_str(concat!(
      "s = \"a & <b> é 𝄞\"\nn = 7\nneg = -2\nbig = 70000\nx = 0.5\nt = true\nf = false\n",
      "when = 2026-10-30T12:00:00+02:00\nday = 2026-10-30\nlist = [1, \"a\", 1, []]\n",
      "[sub]\nn = 7\nempty = {}\n",
    ))
    .unwrap();

    let plist = toml_plist(&toml::Value::Table(toml), "sample").unwrap();
    let py = concat!(
      "{'s': 'a & <b> é 𝄞', 'n': 7, 'neg': -2, 'big': 70000, 'x': 0.5, 't': True, 'f': False,",
      " 'when': datetime.datetime(2026, 10, 30, 10, 0), 'day': '2026-10-30',",
      " 'list': [1, 'a', 1, []], 'sub': {'n': 7, 'empty': {}}}",
    );

    (plist, py)
  }

  /// The instant `secs` after 1970.
  fn at_secs(secs: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
  }

  #[test]
  fn from_toml() {
    let (plist, _) = sample();
    let d = plist.as_dictionary().unwrap();
    assert_eq!(d.get("when"), Some(&Plist::Date(at_secs(1_793_354_400).into())));
    assert_eq!(d.get("day"), Some(&Plist::from("2026-10-30")));
    let list = Plist::Array(vec![1.into(), "a".into(), 1.into(), Plist::Array(vec![])]);
    assert_eq!(d.get("list"), Some(&list));

    let inf = toml::Value::Float(f64::INFINITY);
    assert_eq!(
      toml_plist(&inf, "k").unwrap_err().to_string(),
      "k is inf, which a plist has no real for"
    );
    let nested: toml::Table = toml::from_str("a = [{ b = nan }]").unwrap();
    let e = toml_plist(&toml::Value::Table(nested), "p").unwrap_err().to_string();
    assert_eq!(e, "p.a[0].b is NaN, which a plist has no real for");
    let odd: toml::Table = toml::from_str("k = \"a\\u0001\"").unwrap();
    let e = toml_plist(&toml::Value::Table(odd), "p").unwrap_err().to_string();
    assert_eq!(e, "p.k holds '\\u{1}', which a plist's XML can't");

    let unsorted: toml::Table = toml::from_str("[t]\nz = 1\na = { y = 2, b = 3 }").unwrap();
    let plist = toml_plist(&toml::Value::Table(unsorted), "p").unwrap();
    let t = plist.as_dictionary().unwrap().get("t").unwrap().as_dictionary().unwrap();
    assert_eq!(t.keys().collect::<Vec<_>>(), ["a", "z"]);
    let a = t.get("a").unwrap().as_dictionary().unwrap();
    assert_eq!(a.keys().collect::<Vec<_>>(), ["b", "y"]);
  }

  #[test]
  fn dict_sorted() {
    let d = dict([("b", 1.into()), ("C", 2.into()), ("a", 3.into())]);
    assert_eq!(d.as_dictionary().unwrap().keys().collect::<Vec<_>>(), ["C", "a", "b"]);
  }

  #[test]
  fn dates() {
    let dt: Datetime = "2026-01-01T00:30:00-01:00".parse().unwrap();
    assert_eq!(instant(&dt), Some(at_secs(1_767_231_000)));
    let dt: Datetime = "1969-12-31T23:59:59Z".parse().unwrap();
    assert_eq!(instant(&dt), SystemTime::UNIX_EPOCH.checked_sub(Duration::from_secs(1)));
    let dt: Datetime = "2000-02-29T00:00:00Z".parse().unwrap();
    assert_eq!(instant(&dt), Some(at_secs(951_782_400)));
    let dt: Datetime = "2026-01-01T00:30:00".parse().unwrap();
    assert_eq!(instant(&dt), None);
  }

  /// Runs `python3` with `code`, `data` on its stdin; its stdout.
  #[cfg(target_os = "macos")]
  fn python(code: &str, data: &[u8]) -> String {
    use std::io::Write;
    use std::process::Stdio;

    let mut cmd = Command::new("python3");
    cmd.args(["-I", "-c", code]).stdin(Stdio::piped()).stdout(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    child.stdin.take().unwrap().write_all(data).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap()
  }

  /// Both forms read back by Python's `plistlib` as the value written, and accepted by
  /// `plutil -lint`.
  #[cfg(target_os = "macos")]
  #[test]
  fn read_back() {
    let (plist, py) = sample();
    let code = |load: &str| {
      format!("import sys, plistlib, datetime\nv = {py}\nd = sys.stdin.buffer.read()\n{load}")
    };

    let same = "print(plistlib.loads(d) == v)";
    let tmp = Temp::new().unwrap();
    for (name, data) in [("a.plist", xml(&plist).unwrap()), ("b.plist", binary(&plist).unwrap())] {
      assert_eq!(python(&code(same), &data), "True\n", "{name}");
      fs::write(tmp.join(name), data).unwrap();
      run(Command::new("plutil").arg("-lint").arg(tmp.join(name))).unwrap();
    }
  }
}
