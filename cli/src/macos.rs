// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! The macOS app: its Info.plist from the config and the minimum macOS its binary was built for,
//! the `.app` bundle with its icon, and its signature, by `codesign` on a Mac and by rcodesign
//! elsewhere.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use std::{env, fs, io};

use toml::{Table, Value};

use crate::bad;
use crate::config::Config;
use crate::image::{ICNS, Image, icns, icons};
use crate::plist::Plist;
use crate::tool::{self, Temp, private, run, run_until};

/// The `macos` table's values, their types checked.
struct Options<'a> {
  /// A tray app: no Dock icon, never ended by macOS on its own.
  accessory: bool,
  /// The entitlements' plist, from the manifest's folder.
  entitlements: Option<PathBuf>,
  /// Keys over the Info.plist's, usage strings among them.
  plist: Option<&'a Table>,
}

impl<'a> Options<'a> {
  fn of(cfg: &'a Config) -> io::Result<Self> {
    let what =
      |key: &str, kind: &str| bad(format!("[package.metadata.shipo.macos] {key} is {kind}"));
    let accessory = match cfg.macos.get("accessory") {
      None => false,
      Some(Value::Boolean(b)) => *b,
      Some(_) => return Err(what("accessory", "not true or false")),
    };

    let entitlements = match cfg.macos.get("entitlements") {
      None => None,
      Some(Value::String(path)) => Some(cfg.root.join(path)),
      Some(_) => return Err(what("entitlements", "not a string, the plist's path")),
    };

    let plist = match cfg.macos.get("plist") {
      None => None,
      Some(Value::Table(t)) => Some(t),
      Some(_) => return Err(what("plist", "not a table")),
    };

    Ok(Self { accessory, entitlements, plist })
  }
}

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
/// `macos.plist` keys over it.
pub fn info(cfg: &Config, minos: &str) -> io::Result<Plist> {
  let opts = Options::of(cfg)?;
  // Both versions take numbers and dots only: 1.2.0 of 1.2.0-beta.1.
  let Some(version) = numeric(&cfg.version) else {
    return Err(bad(format!("the version {} starts with no number", cfg.version)));
  };

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

  if opts.accessory {
    keys.insert("LSUIElement", true.into());
    keys.insert("NSSupportsAutomaticTermination", false.into());
    keys.insert("NSSupportsSuddenTermination", false.into());
  }

  let mut keys: BTreeMap<String, Plist> = keys.into_iter().map(|(k, v)| (k.into(), v)).collect();
  for (key, value) in opts.plist.into_iter().flatten() {
    let what = format!("[package.metadata.shipo.macos] plist.{key}");
    if let Some((_, from)) = RESERVED.iter().find(|(k, _)| k == key) {
      return Err(bad(format!("{what} can't be set: it is {from}")));
    }

    keys.insert(key.clone(), Plist::toml(value, &what)?);
  }

  Ok(Plist::Dict(keys))
}

/// The leading `\d+(\.\d+)*` of `version`.
fn numeric(version: &str) -> Option<&str> {
  let b = version.as_bytes();
  let digits =
    |at: usize| b.get(at..).map_or(0, |s| s.iter().take_while(|c| c.is_ascii_digit()).count());
  let mut end = digits(0);
  if end == 0 {
    return None;
  }

  while b.get(end) == Some(&b'.') && digits(end + 1) > 0 {
    end += 1 + digits(end + 1);
  }

  Some(&version[..end])
}

// MARK: Mach-O

/// The macOS the Mach-O binary `data` needs, as `14.0` or `10.13.4`: its `LC_BUILD_VERSION`'s minos
/// for macOS, else its `LC_VERSION_MIN_MACOSX`'s version (xnu's `loader.h`). A universal binary
/// needs the lowest of its 64-bit slices': a Mac runs only the slice of its own arch, and Apple
/// silicon, whose slice asks macOS 11 at least, comes with that.
pub fn minos(data: &[u8]) -> io::Result<String> {
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
  let bytes = data.get(at..at + 4).ok_or_else(truncated)?;
  Ok(u32::from_le_bytes(bytes.try_into().unwrap_or_default()))
}

fn be32(data: &[u8], at: usize) -> io::Result<u32> {
  let bytes = data.get(at..at + 4).ok_or_else(truncated)?;
  Ok(u32::from_be_bytes(bytes.try_into().unwrap_or_default()))
}

fn be64(data: &[u8], at: usize) -> io::Result<u64> {
  let bytes = data.get(at..at + 8).ok_or_else(truncated)?;
  Ok(u64::from_be_bytes(bytes.try_into().unwrap_or_default()))
}

fn truncated() -> io::Error {
  bad("a truncated Mach-O binary")
}

// MARK: Bundle

/// `<target>/<Name>.app` of the binary `bin`, signed with the hardened runtime and
/// `macos.entitlements`: on a Mac by [identity], elsewhere by rcodesign with `APPLE_CERTIFICATE`,
/// else ad hoc, which a release (a `tag`) refuses. A signature that fails leaves no bundle, never
/// one signed ad hoc on a Mac, whose grants would last that build only. Gives the bundle and who
/// signed it.
pub fn app(cfg: &Config, bin: &Path, tag: Option<&str>) -> io::Result<(PathBuf, String)> {
  let entitlements = Options::of(cfg)?.entitlements;
  if let Some(path) = &entitlements
    && !path.is_file()
  {
    return Err(bad(format!("macos.entitlements: no file {}", path.display())));
  }

  let signed = !cfg!(target_os = "macos") && secrets(&CERT, tag)?;
  let entitlements = entitlements.as_deref();
  keep_signed(bundle(cfg, bin)?, |bundle| match cfg!(target_os = "macos") {
    true => {
      let id = identity(cfg, tag)?;
      codesign(bundle, &codesign_args(&id.id, id.developer, entitlements))?;
      Ok(id.name)
    }
    false => sign_elsewhere(cfg, bundle, signed, entitlements),
  })
}

/// `bundle` once `sign` signed it, which gives who did; removed when it fails.
fn keep_signed(
  bundle: PathBuf,
  sign: impl FnOnce(&Path) -> io::Result<String>,
) -> io::Result<(PathBuf, String)> {
  match sign(&bundle) {
    Ok(name) => Ok((bundle, name)),
    Err(e) => {
      let _ = fs::remove_dir_all(&bundle);
      Err(io::Error::new(e.kind(), format!("signing {}: {e}; no bundle", bundle.display())))
    }
  }
}

/// `<target>/<Name>.app` of the binary `bin`, unsigned, in place of one there: the binary
/// (0755, whatever its mode: GitHub's artifacts lose it), the `.icns`, `PkgInfo` and the
/// Info.plist. All is made before the old bundle goes.
fn bundle(cfg: &Config, bin: &Path) -> io::Result<PathBuf> {
  let at = |e: io::Error| io::Error::new(e.kind(), format!("{}: {e}", bin.display()));
  let exe = fs::read(bin).map_err(at)?;
  let plist = info(cfg, &minos(&exe).map_err(at)?)?.xml()?;
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

// MARK: Signing on a Mac

/// What signs the app on a Mac, as [choose] says, "<Name> Local Signing" made when it must be.
fn identity(cfg: &Config, tag: Option<&str>) -> io::Result<Identity> {
  let find = |valid: bool| -> io::Result<String> {
    let mut cmd = Command::new("security");
    cmd.arg("find-identity");
    if valid {
      cmd.arg("-v");
    }

    Ok(String::from_utf8_lossy(&run(cmd.args(["-p", "codesigning"]))?).into_owned())
  };

  let local = format!("{} Local Signing", cfg.name);
  let id = choose(&find(true)?, &find(false)?, &local, tag)?;
  if id.make {
    make_identity(&local)?;
    if choose("", &find(false)?, &local, None)?.make {
      return Err(io::Error::other(format!(
        "made \"{local}\", but `security find-identity` does not list it"
      )));
    }
  }

  Ok(id)
}

/// A code signing identity in the keychain.
#[derive(Debug, PartialEq)]
struct Identity {
  /// What `codesign --sign` takes.
  id: String,
  /// How it is said.
  name: String,
  /// A Developer ID, whose signature takes Apple's secure timestamp.
  developer: bool,
  /// Not in the keychain yet: [make_identity] makes it.
  make: bool,
}

/// What signs the app, of `security find-identity -p codesigning`'s outputs, `valid` with `-v` and
/// `all` without: the first Developer ID, by its hash (two of one name, a renewal, are ambiguous by
/// name); else `local`, the self-signed identity, by its name, which a release (a `tag`) refuses.
/// The local one's designated requirement names the bundle id and the certificate, not the code's
/// hash, so the grants macOS keeps survive rebuilds.
fn choose(valid: &str, all: &str, local: &str, tag: Option<&str>) -> io::Result<Identity> {
  let quoted = |out: &str| -> Vec<(String, String)> {
    let lines = out.lines().filter_map(|ln| {
      let hash = ln.split_whitespace().nth(1)?;
      let name = ln.split('"').nth(1)?;
      Some((hash.to_string(), name.to_string()))
    });

    lines.collect()
  };

  let developer =
    quoted(valid).into_iter().find(|(_, n)| n.starts_with("Developer ID Application:"));
  if let Some((id, name)) = developer {
    return Ok(Identity { id, name, developer: true, make: false });
  }

  if let Some(tag) = tag {
    return Err(bad(format!(
      "{tag} is a release, signed by a Developer ID: the keychain has none"
    )));
  }

  let make = !quoted(all).iter().any(|(_, n)| n == local);
  Ok(Identity { id: local.into(), name: local.into(), developer: false, make })
}

/// A self-signed code signing certificate named `name` in the login keychain, codesign allowed
/// to use its key.
fn make_identity(name: &str) -> io::Result<()> {
  let tmp = Temp::new()?;
  let (key, cert, p12) = (tmp.join("key.pem"), tmp.join("cert.pem"), tmp.join("identity.p12"));
  // macOS's own openssl (LibreSSL): `security import` reads its PKCS#12, not OpenSSL 3's.
  let openssl = || Command::new("/usr/bin/openssl");
  let subject = format!("/CN={name}");
  let mut new = openssl();
  new.args(["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "3650", "-subj", &subject]);
  new.args(["-addext", "keyUsage=critical,digitalSignature"]);
  new.args(["-addext", "extendedKeyUsage=critical,codeSigning"]);
  run(new.arg("-keyout").arg(&key).arg("-out").arg(&cert))?;
  let mut export = openssl();
  export.args(["pkcs12", "-export", "-inkey"]).arg(&key).arg("-in").arg(&cert);
  run(export.arg("-out").arg(&p12).args(["-passout", "pass:shipo"]))?;

  let home = env::var_os("HOME").ok_or_else(|| bad("no $HOME, the login keychain's"))?;
  let keychain = Path::new(&home).join("Library/Keychains/login.keychain-db");
  let mut import = Command::new("security");
  import.arg("import").arg(&p12).arg("-k").arg(keychain);
  run(import.args(["-P", "shipo", "-T", "/usr/bin/codesign"]))?;
  eprintln!("shipo: made the code signing identity \"{name}\" in the login keychain");
  Ok(())
}

/// `codesign`'s arguments to sign with `id` (`-` is ad hoc), the hardened runtime and
/// `entitlements`: a `developer` ID's signature takes Apple's secure timestamp, which notarization
/// needs; the local one's none, so it signs offline.
fn codesign_args(id: &str, developer: bool, entitlements: Option<&Path>) -> Vec<OsString> {
  let stamp = if developer { "--timestamp" } else { "--timestamp=none" };
  let mut args: Vec<OsString> =
    ["--force", "--sign", id, "--options", "runtime", stamp].map(OsString::from).into();
  if let Some(path) = entitlements {
    args.extend(["--entitlements".into(), path.into()]);
  }

  args
}

/// Signs `bundle` by `codesign` with `args`, then verifies it. Its extended attributes go first:
/// codesign refuses a bundle with Finder info. A keychain prompt nobody answers ends it after a
/// minute.
fn codesign(bundle: &Path, args: &[OsString]) -> io::Result<()> {
  run(Command::new("xattr").arg("-cr").arg(bundle))?;
  run_until(Command::new("codesign").args(args).arg(bundle), Duration::from_secs(60))?;
  run(Command::new("codesign").args(["--verify", "--strict"]).arg(bundle))?;
  Ok(())
}

// MARK: Signing elsewhere

/// The secrets of the Developer ID certificate: a base64 `.p12` and its password.
pub const CERT: [&str; 2] = ["APPLE_CERTIFICATE", "APPLE_CERTIFICATE_PASSWORD"];

/// rcodesign (apple-codesign 0.29.0, 2024-11-29) by the triple it runs on, and its tarball's
/// SHA-256 as the release's SHA256SUMS gives it.
const RCODESIGN: &str = concat!(
  "https://github.com/indygreg/apple-platform-rs/releases/download/apple-codesign/0.29.0/",
  "apple-codesign-0.29.0-{triple}.tar.gz",
);
const RCODESIGN_SHA256: [(&str, &str); 3] = [
  ("x86_64-unknown-linux-musl", "dbe85cedd8ee4217b64e9a0e4c2aef92ab8bcaaa41f20bde99781ff02e600002"),
  (
    "aarch64-unknown-linux-musl",
    "4af92c87ddf52f5f2d1258a3b4e56c7dcb8f1b2468df744976c5f139e031961f",
  ),
  ("aarch64-apple-darwin", "d1a532150adaf90048260d76359261aa716abafc45c53c5dc18845029184334a"),
];

/// Whether each of `names` is set in the environment; with a release's `tag`, one missing is an
/// error.
pub fn secrets(names: &[&str], tag: Option<&str>) -> io::Result<bool> {
  let missing = names.iter().copied().filter(|n| env::var_os(n).is_none_or(|v| v.is_empty()));
  let missing: Vec<_> = missing.collect();
  if let Some(tag) = tag
    && !missing.is_empty()
  {
    return Err(bad(format!(
      "{tag} is a release, signed and notarized: set {}",
      missing.join(", ")
    )));
  }

  Ok(missing.is_empty())
}

/// rcodesign for this host, its tarball fetched into `tools` once, pinned by SHA-256.
pub fn rcodesign(tools: &Path) -> io::Result<PathBuf> {
  let os = match env::consts::OS {
    "macos" => "apple-darwin",
    _ => "unknown-linux-musl",
  };

  let triple = format!("{}-{os}", env::consts::ARCH);
  let Some(&(_, sha256)) = RCODESIGN_SHA256.iter().find(|(t, _)| *t == triple) else {
    return Err(bad(format!("no rcodesign is pinned for {triple}")));
  };

  let url = RCODESIGN.replace("{triple}", &triple);
  let name = url.rsplit('/').next().unwrap_or_default();
  let tarball = tool::fetch(&url, sha256, &tools.join(name))?;
  let member = format!("apple-codesign-0.29.0-{triple}/rcodesign");
  tool::untar(&tarball, &member, &tools.join("rcodesign"))
}

/// Signs `bundle` by rcodesign with the hardened runtime and `entitlements`: `signed` by the
/// Developer ID of [CERT] for notarization, else ad hoc. Gives who signed it.
fn sign_elsewhere(
  cfg: &Config,
  bundle: &Path,
  signed: bool,
  entitlements: Option<&Path>,
) -> io::Result<String> {
  let rcodesign = rcodesign(&cfg.target.join("tools"))?;
  let tmp = Temp::new()?;
  let pem = match signed {
    true => {
      let var = |name| env::var(name).map_err(|e| bad(format!("{name}: {e}")));
      Some(certificate(&tmp, &var(CERT[0])?, &var(CERT[1])?)?)
    }
    false => None,
  };

  rsign(&rcodesign, bundle, pem.as_deref(), &rcodesign_args(signed, entitlements))?;
  Ok(if signed { "Developer ID" } else { "ad hoc" }.into())
}

/// rcodesign's arguments to sign the app with the hardened runtime and `entitlements`, checked
/// for notarization when `signed` by a Developer ID.
fn rcodesign_args(signed: bool, entitlements: Option<&Path>) -> Vec<OsString> {
  let mut args: Vec<OsString> = vec!["--code-signature-flags".into(), "runtime".into()];
  if signed {
    args.push("--for-notarization".into());
  }

  if let Some(path) = entitlements {
    args.extend(["--entitlements-xml-file".into(), path.into()]);
  }

  args
}

/// Signs `path` in place by `rcodesign` with `args`: by the key and certificate in `pem`, else ad
/// hoc.
pub fn rsign(
  rcodesign: &Path,
  path: &Path,
  pem: Option<&Path>,
  args: &[OsString],
) -> io::Result<()> {
  let mut cmd = Command::new(rcodesign);
  cmd.arg("sign");
  if let Some(pem) = pem {
    cmd.arg("--pem-file").arg(pem);
  }

  run(cmd.args(args).arg(path))?;
  Ok(())
}

/// The key and certificate of the `.p12` in `p12` (base64), opened by `password`, as a PEM in the
/// folder `dir`, only its owner reading either: rcodesign rejects a .p12 in OpenSSL 3's defaults
/// (rcodesign #315), and OpenSSL 3 reads an older one's RC2 only with `-legacy`.
pub fn certificate(dir: &Path, p12: &str, password: &str) -> io::Result<PathBuf> {
  let data = base64(p12).ok_or_else(|| bad(format!("{} is not base64", CERT[0])))?;
  let p12 = private(&dir.join("id.p12"), &data)?;
  let mut last = None;
  for legacy in [false, true] {
    let mut cmd = Command::new("openssl");
    cmd.args(["pkcs12", "-nodes", "-passin", "env:SHIPO_P12_PASSWORD", "-in"]).arg(&p12);
    cmd.env("SHIPO_P12_PASSWORD", password);
    if legacy {
      cmd.arg("-legacy");
    }

    match run(&mut cmd) {
      Ok(pem) => return private(&dir.join("id.pem"), &pem),
      Err(e) if e.kind() == io::ErrorKind::NotFound => {
        return Err(io::Error::new(e.kind(), "openssl not found: `apt-get install openssl`"));
      }
      Err(e) => last = Some(e),
    }
  }

  let last = last.map_or_else(String::new, |e| e.to_string());
  Err(bad(format!("{}: openssl reads no .p12: {last}", CERT[0])))
}

/// The bytes of standard base64 `text`, whitespace skipped, ending at its first `=`.
fn base64(text: &str) -> Option<Vec<u8>> {
  let (mut out, mut acc, mut bits) = (vec![], 0u32, 0);
  for c in text.bytes().filter(|c| !c.is_ascii_whitespace()) {
    let v = match c {
      b'A'..=b'Z' => c - b'A',
      b'a'..=b'z' => c - b'a' + 26,
      b'0'..=b'9' => c - b'0' + 52,
      b'+' => 62,
      b'/' => 63,
      b'=' => break,
      _ => return None,
    };

    acc = (acc << 6 | v as u32) & 0xFFFF;
    bits += 6;
    if bits >= 8 {
      bits -= 8;
      out.push((acc >> bits) as u8);
    }
  }

  Some(out)
}

#[cfg(test)]
mod tests {
  use super::*;

  /// A config in `dir` with the `[macos]` table `macos`.
  fn config(dir: &Path, macos: &str) -> Config {
    Config {
      id: "com.example.hello".into(),
      name: "Hello".into(),
      icon: dir.join("icon.png"),
      comment: String::new(),
      version: "1.2.0-beta.1".into(),
      bin: "hello".into(),
      macos: toml::from_str(macos).unwrap(),
      windows: Table::new(),
      linux: Table::new(),
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
    assert_eq!(info(&cfg, "11.0").unwrap().xml().unwrap(), INFO);

    let plain = info(&config(Path::new("/app"), ""), "11.0").unwrap();
    let Plist::Dict(keys) = plain else { panic!() };
    assert_eq!(keys.len(), 12);
    assert!(!keys.contains_key("LSUIElement"));
  }

  #[test]
  fn info_errors() {
    let cases = [
      ("accessory = 1", "[package.metadata.shipo.macos] accessory is not true or false"),
      (
        "entitlements = true",
        "[package.metadata.shipo.macos] entitlements is not a string, the plist's path",
      ),
      ("plist = \"x\"", "[package.metadata.shipo.macos] plist is not a table"),
      (
        "plist.X = [nan]",
        "[package.metadata.shipo.macos] plist.X[0] is NaN, which a plist has no real for",
      ),
    ];

    for (macos, want) in cases {
      let e = info(&config(Path::new("/app"), macos), "11.0").unwrap_err();
      assert_eq!(e.to_string(), want, "{macos}");
    }

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

  #[test]
  fn versions() {
    let cases = [
      ("1.2.0", Some("1.2.0")),
      ("1.2.0-beta.1", Some("1.2.0")),
      ("10.4+build.5", Some("10.4")),
      ("1..2", Some("1")),
      ("1.2.", Some("1.2")),
      ("7", Some("7")),
      ("x1", None),
      ("", None),
    ];

    for (version, want) in cases {
      assert_eq!(numeric(version), want, "{version}");
    }
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

  #[test]
  fn identities() {
    let valid = r#"  1) 7D917A56955A0E81554606196D2673E00F9E4A9A "Hello Local Signing"
  2) E00087BDD71F8F29D6E4213BE04F8CF15C0DB969 "Developer ID Application: Jane Doe (ABCDE12345)"
     2 valid identities found
"#;
    let none = "     0 valid identities found\n";
    let untrusted = r#"  1) 7D917A56955A0E81554606196D2673E00F9E4A9A "Hello Local Signing" (CSSMERR_TP_NOT_TRUSTED)
     1 identities found
"#;

    let local = "Hello Local Signing";
    let dev = (
      "E00087BDD71F8F29D6E4213BE04F8CF15C0DB969",
      "Developer ID Application: Jane Doe (ABCDE12345)",
    );
    let other = untrusted.replace("Hello", "Other");
    let id = |id: &str, name: &str, developer, make| {
      Ok(Identity { id: id.into(), name: name.into(), developer, make })
    };

    let got = |valid, all, tag| choose(valid, all, local, tag).map_err(|e| e.to_string());
    assert_eq!(got(valid, valid, None), id(dev.0, dev.1, true, false));
    assert_eq!(got(valid, valid, Some("v1.2.0")), id(dev.0, dev.1, true, false));
    assert_eq!(got(none, untrusted, None), id(local, local, false, false));
    assert_eq!(got(none, none, None), id(local, local, false, true));
    assert_eq!(got(untrusted, &other, None), id(local, local, false, true));
    let refused = "v1.2.0 is a release, signed by a Developer ID: the keychain has none";
    assert_eq!(got(untrusted, untrusted, Some("v1.2.0")), Err(refused.into()));
  }

  #[test]
  fn signing_args() {
    let args = |v: Vec<OsString>| {
      v.iter().map(|a| a.to_string_lossy().into_owned()).collect::<Vec<_>>().join(" ")
    };
    let ent = Path::new("/app/hello.entitlements");
    let cases = [
      (
        codesign_args("ABC", true, Some(ent)),
        "--force --sign ABC --options runtime --timestamp --entitlements /app/hello.entitlements",
      ),
      (
        codesign_args("Hello Local Signing", false, None),
        "--force --sign Hello Local Signing --options runtime --timestamp=none",
      ),
      (
        rcodesign_args(true, Some(ent)),
        "--code-signature-flags runtime --for-notarization --entitlements-xml-file /app/hello.entitlements",
      ),
      (rcodesign_args(false, None), "--code-signature-flags runtime"),
    ];

    for (got, want) in cases {
      assert_eq!(args(got), want);
    }
  }

  #[test]
  fn base64s() {
    let cases = [
      ("", Some(&b""[..])),
      ("Zg==", Some(b"f")),
      ("Zm8=", Some(b"fo")),
      ("Zm9v", Some(b"foo")),
      ("Zm9v\nYmFy\r\n", Some(b"foobar")),
      ("+/8=", Some(&[0xFB, 0xFF][..])),
      ("Zm9v!", None),
    ];

    for (text, want) in cases {
      assert_eq!(base64(text).as_deref(), want, "{text:?}");
    }
  }

  #[test]
  fn secret_names() {
    let names = ["SHIPO_TEST_UNSET_A", "SHIPO_TEST_UNSET_B"];
    assert!(!secrets(&names, None).unwrap());
    let e = secrets(&names, Some("v1.2.0")).unwrap_err().to_string();
    assert_eq!(
      e,
      "v1.2.0 is a release, signed and notarized: set SHIPO_TEST_UNSET_A, SHIPO_TEST_UNSET_B"
    );
    assert!(secrets(&["PATH"], Some("v1.2.0")).unwrap());
  }

  /// A 1024 px icon at `path`.
  fn icon(path: &Path) {
    let px = (0..1024 * 1024).flat_map(|i| [(i % 256) as u8, (i / 4096) as u8, 128, 255]).collect();
    fs::write(path, Image { w: 1024, h: 1024, px }.encode().unwrap()).unwrap();
  }

  /// A test app in `dir`: its icon and entitlements, and the config of [MACOS].
  fn test_app(dir: &Path) -> Config {
    icon(&dir.join("icon.png"));
    let entitlements = Plist::dict([("com.apple.security.device.audio-input", true.into())]);
    fs::write(dir.join("hello.entitlements"), entitlements.xml().unwrap()).unwrap();
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

  #[test]
  fn removed_unsigned() {
    let tmp = Temp::new().unwrap();
    let app = tmp.join("Hello.app");
    fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
    let (kept, name) = keep_signed(app.clone(), |_| Ok("ad hoc".into())).unwrap();
    assert_eq!((kept.as_path(), name.as_str()), (app.as_path(), "ad hoc"));
    assert!(app.exists());

    let e = keep_signed(app.clone(), |_| Err(io::Error::other("no key"))).unwrap_err();
    assert_eq!(e.to_string(), format!("signing {}: no key; no bundle", app.display()));
    assert!(!app.exists());
  }

  /// codesign with an identity the keychain lacks fails, and the bundle goes.
  #[cfg(target_os = "macos")]
  #[test]
  fn removed_by_codesign() {
    let tmp = Temp::new().unwrap();
    let cfg = test_app(&tmp);
    let app = bundle(&cfg, &env::current_exe().unwrap()).unwrap();
    let args = codesign_args(&"0".repeat(40), false, None);
    let e = keep_signed(app.clone(), |b| codesign(b, &args).map(|_| String::new())).unwrap_err();
    assert!(e.to_string().contains("codesign failed"), "{e}");
    assert!(!app.exists());
  }

  /// This test binary as the app, signed ad hoc by codesign with the entitlements, the hardened
  /// runtime on, and read back by codesign and plutil.
  #[cfg(target_os = "macos")]
  #[test]
  fn signed_ad_hoc() {
    let tmp = Temp::new().unwrap();
    let cfg = test_app(&tmp);
    let app = bundle(&cfg, &env::current_exe().unwrap()).unwrap();
    codesign(&app, &codesign_args("-", false, Some(&tmp.join("hello.entitlements")))).unwrap();

    run(Command::new("plutil").arg("-lint").arg(app.join("Contents/Info.plist"))).unwrap();
    let out = Command::new("codesign").arg("-dv").arg(&app).output().unwrap();
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(said.contains("Identifier=com.example.hello"), "{said}");
    assert!(said.contains("(adhoc,runtime)"), "{said}");
    let out = run(Command::new("codesign").args(["-d", "--entitlements", "-", "--xml"]).arg(&app));
    let out = String::from_utf8(out.unwrap()).unwrap();
    assert!(out.contains("com.apple.security.device.audio-input"), "{out}");
  }

  /// A self-signed certificate's `.p12`, base64, of the password `pw`, made in `dir`.
  #[cfg(unix)]
  fn p12(dir: &Path, pw: &str) -> String {
    let (key, cert, p12) = (dir.join("k.pem"), dir.join("c.pem"), dir.join("c.p12"));
    let mut req = Command::new("openssl");
    req.args("req -x509 -newkey rsa:2048 -nodes -days 1".split(' ')).args(["-subj", "/CN=shipo"]);
    req.args(["-addext", "keyUsage=critical,digitalSignature"]);
    req.args(["-addext", "extendedKeyUsage=critical,codeSigning"]);
    run(req.arg("-keyout").arg(&key).arg("-out").arg(&cert)).unwrap();
    let mut export = Command::new("openssl");
    export.args(["pkcs12", "-export", "-inkey"]).arg(&key).arg("-in").arg(&cert);
    run(export.arg("-out").arg(&p12).args(["-passout", &format!("pass:{pw}")])).unwrap();
    let out = run(Command::new("openssl").args(["base64", "-in"]).arg(&p12)).unwrap();
    String::from_utf8(out).unwrap()
  }

  #[cfg(unix)]
  #[test]
  fn certificate_pem() {
    let tmp = Temp::new().unwrap();
    let b64 = p12(&tmp, "secret pw");
    let out = Temp::new().unwrap();
    let pem = fs::read_to_string(certificate(&out, &b64, "secret pw").unwrap()).unwrap();
    assert!(pem.contains("-----BEGIN PRIVATE KEY-----"), "{pem}");
    assert!(pem.contains("-----BEGIN CERTIFICATE-----"), "{pem}");

    let out = Temp::new().unwrap();
    let e = certificate(&out, &b64, "wrong").unwrap_err().to_string();
    assert!(e.starts_with("APPLE_CERTIFICATE: openssl reads no .p12: openssl failed"), "{e}");
    let e = certificate(&out, "not base64!", "x").unwrap_err().to_string();
    assert_eq!(e, "APPLE_CERTIFICATE is not base64");
  }

  /// rcodesign fetched into the target folder's `tools`, signing the app as it does off macOS:
  /// ad hoc, then by a self-signed certificate; on a Mac, codesign verifies both. The binary is
  /// `$SHIPO_TEST_MACHO`, else this test's own.
  #[cfg(unix)]
  #[test]
  #[ignore = "fetches rcodesign from GitHub"]
  fn rcodesign_signs() {
    let tmp = Temp::new().unwrap();
    let cfg = test_app(&tmp);
    let bin =
      env::var_os("SHIPO_TEST_MACHO").map_or_else(|| env::current_exe().unwrap(), PathBuf::from);
    let entitlements = tmp.join("hello.entitlements");
    let check = |app: &Path| {
      if cfg!(target_os = "macos") {
        run(Command::new("codesign").args(["--verify", "--strict"]).arg(app)).unwrap();
      }
    };

    // The target folder of this test's `<target>/debug/deps/<test>`.
    let exe = env::current_exe().unwrap();
    let tools = exe.ancestors().nth(3).unwrap().join("tools");
    let rcodesign = rcodesign(&tools).unwrap();

    let app = bundle(&cfg, &bin).unwrap();
    let args = rcodesign_args(false, Some(&entitlements));
    rsign(&rcodesign, &app, None, &args).unwrap();
    check(&app);

    let app = bundle(&cfg, &bin).unwrap();
    let pem = certificate(&tmp, &p12(&tmp, "pw"), "pw").unwrap();
    rsign(&rcodesign, &app, Some(&pem), &args).unwrap();
    check(&app);

    // A release's certificate is Apple's: rcodesign refuses this one for notarization.
    let notarized = rcodesign_args(true, Some(&entitlements));
    let e = rsign(&rcodesign, &app, Some(&pem), &notarized).unwrap_err().to_string();
    assert!(e.contains("--for-notarization requires use of a Developer ID signing certificate"));
  }
}
